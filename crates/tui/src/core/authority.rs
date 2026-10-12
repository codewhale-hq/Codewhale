//! Turn authority and mode/posture policy projections.
//!
//! Keep mode, approval, shell, sandbox, trust, and input provenance decisions
//! in one place so prompt metadata, tool catalogs, and runtime gates cannot
//! drift independently.

pub mod auto_review;
pub mod grants;
pub mod risk;

pub use risk::{RiskLevel, classify_risk};

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::sandbox::SandboxPolicy;
use crate::tools::canonical_action::canonical_action_alias;
use crate::tools::spec::{ApprovalRequirement, normalize_path};
use crate::worker_profile::ShellPolicy;
use codewhale_config::AppMode;
use codewhale_execpolicy::ApprovalMode;
use serde_json::Value;

use super::ops::UserInputProvenance;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOrigin {
    Interactive,
    Headless,
    Background,
}

impl RunOrigin {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Headless => "headless",
            Self::Background => "background",
        }
    }
}

/// Durable Agent-era permission baseline that Plan/YOLO restore to (#3386).
///
/// Mode cycling used to be tangled with permission policy: each mode mutated
/// `allow_shell`/`trust_mode`/`approval_mode` directly and ad-hoc snapshots
/// tried to put things back on exit. Instead, keep one canonical baseline: the
/// permission surface the user has chosen for Agent mode.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ModeSessionPrefs {
    pub(crate) agent_allow_shell: bool,
    pub(crate) agent_trust_mode: bool,
    pub(crate) agent_approval_mode: ApprovalMode,
}

/// The permission policy a given [`AppMode`] resolves to (#3386).
#[derive(Debug, Clone, Copy)]
pub(crate) struct EffectiveModePolicy {
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) mode: AppMode,
    pub(crate) allow_shell: bool,
    pub(crate) trust_mode: bool,
    pub(crate) approval_mode: ApprovalMode,
}

/// Resolve a mode's effective permission policy from the durable Agent baseline.
///
/// This is the single source of truth for the mode/permission table:
/// - `Plan`   -> read-only: no shell, no trust, `Suggest` approvals.
/// - `Agent`  -> the user's durable baseline (`prefs`).
/// - `Operate` -> Agent baseline plus orchestration capabilities in the runtime.
///
/// The legacy YOLO spelling resolves to Agent plus a `Bypass` approval
/// posture before it reaches this table; modes no longer carry permission.
#[must_use]
pub(crate) fn base_policy_for_mode(mode: AppMode, prefs: &ModeSessionPrefs) -> EffectiveModePolicy {
    match mode {
        AppMode::Plan => EffectiveModePolicy {
            mode,
            allow_shell: false,
            trust_mode: false,
            approval_mode: ApprovalMode::Suggest,
        },
        AppMode::Agent | AppMode::Operate => EffectiveModePolicy {
            mode,
            allow_shell: prefs.agent_allow_shell,
            trust_mode: prefs.agent_trust_mode,
            approval_mode: prefs.agent_approval_mode,
        },
    }
}

/// Why runtime policy narrowed the authority a turn was asked to run with.
///
/// One variant per narrowing site. Adding a site means adding a variant, which
/// is the mechanism that makes "no silent effective mode change" enforceable
/// rather than aspirational (#3947).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyNarrowingReason {
    /// Input arrived from a provenance that cannot inherit standing
    /// auto-approval authority (sub-agent handoffs, restored checkpoints).
    NonAuthoritativeProvenance,
}

impl PolicyNarrowingReason {
    /// Stable machine-readable identifier. Shared by the model-visible
    /// metadata line and doctor output so the two cannot drift.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NonAuthoritativeProvenance => "non_authoritative_provenance",
        }
    }
}

/// A structured record of one authority narrowing.
///
/// Before this existed, narrowing produced only a free-text UI status line:
/// the model saw the narrowed posture but never learned it had been narrowed
/// or why, and doctor could not report it at all. Every consumer now renders
/// from this one value, so the UI status, the `<turn_meta>` line, and doctor
/// necessarily agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PolicyNarrowingEvent {
    reason: PolicyNarrowingReason,
    /// Mode before narrowing, and after, as setting strings.
    from_mode: &'static str,
    to_mode: &'static str,
    /// Permission posture before narrowing, and after.
    from_approval: ApprovalMode,
    to_approval: ApprovalMode,
    /// Human-readable cause, e.g. the provenance that could not inherit.
    detail: String,
}

impl PolicyNarrowingEvent {
    pub(crate) fn reason(&self) -> PolicyNarrowingReason {
        self.reason
    }

    /// The single user-facing sentence. The TUI status line renders exactly
    /// this, and the model-visible metadata carries the same string.
    pub(crate) fn message(&self) -> String {
        match self.reason {
            PolicyNarrowingReason::NonAuthoritativeProvenance => format!(
                "Input provenance '{}' cannot inherit standing auto-approval authority; continuing with approvals required.",
                self.detail
            ),
        }
    }

    /// Compact `from -> to` summary for doctor and debug surfaces.
    pub(crate) fn transition(&self) -> String {
        format!(
            "{} ({}) -> {} ({})",
            self.from_mode,
            self.from_approval.permission_chip_label(),
            self.to_mode,
            self.to_approval.permission_chip_label(),
        )
    }
}

/// Effective authority for one engine turn after provenance narrowing.
#[derive(Debug, Clone)]
pub(crate) struct TurnAuthority {
    pub(crate) mode: AppMode,
    pub(crate) allow_shell: bool,
    pub(crate) trust_mode: bool,
    pub(crate) auto_approve: bool,
    pub(crate) approval_mode: ApprovalMode,
    pub(crate) dynamic_active_tools: Vec<&'static str>,
    /// Structured record of any narrowing applied to this turn (#3947). The
    /// UI status line, `<turn_meta>`, and doctor all render from here, so a
    /// narrowing that reaches one surface reaches all of them.
    pub(crate) narrowing: Option<PolicyNarrowingEvent>,
}

impl TurnAuthority {
    /// The user-facing status sentence for this turn's narrowing, if any.
    pub(crate) fn status(&self) -> Option<String> {
        self.narrowing.as_ref().map(PolicyNarrowingEvent::message)
    }

    #[must_use]
    pub(crate) fn from_effective_fields(
        mode: AppMode,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
    ) -> Self {
        Self {
            mode,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
            dynamic_active_tools: Vec::new(),
            narrowing: None,
        }
    }

    #[must_use]
    pub(crate) fn approval_mode_for_session(&self) -> ApprovalMode {
        agent_approval_mode_for_turn(self.auto_approve, self.approval_mode)
    }

    /// Authority for the per-tool approval gate, folded from the legacy
    /// session `auto_approve` bit so [`resolve_tool_permission`] observes the
    /// same effective posture the old boolean helpers encoded: a set bit is
    /// the Full Access posture (Bypass), a cleared bit is an ordinary Ask
    /// turn. The engine's `Never` denial deliberately stays at the UI layer,
    /// so this constructor never produces a `Never` posture.
    #[must_use]
    pub(crate) fn for_tool_approval_decision(auto_approve: bool) -> Self {
        Self::from_effective_fields(
            AppMode::Agent,
            true,
            false,
            auto_approve,
            posture_from_auto_approve(auto_approve),
        )
    }

    #[must_use]
    pub(crate) fn shell_policy(&self) -> ShellPolicy {
        shell_policy_for_mode(self.mode, self.allow_shell)
    }

    #[must_use]
    pub(crate) fn sandbox_policy(
        &self,
        workspace: &Path,
        configured_mode: Option<&str>,
        network_access: SandboxNetworkAccess,
    ) -> SandboxPolicy {
        sandbox_policy_for_turn(
            self.mode,
            self.approval_mode_for_session(),
            configured_mode,
            workspace,
            network_access,
        )
    }
}

/// The posture a legacy `auto_approve` bit stands for: a set bit is Full
/// Access, a cleared bit is Ask.
///
/// Every surface that carries the bit without a session posture folds it here —
/// the per-tool approval gate, a tool context built with only the bit, a
/// scheduled automation — so none of them can disagree about what it means.
#[must_use]
pub(crate) fn posture_from_auto_approve(auto_approve: bool) -> ApprovalMode {
    if auto_approve {
        ApprovalMode::Bypass
    } else {
        ApprovalMode::Suggest
    }
}

#[must_use]
pub(crate) fn effective_input_policy(
    provenance: UserInputProvenance,
    requested_mode: AppMode,
    _content: &str,
    allow_shell: bool,
    trust_mode: bool,
    auto_approve: bool,
    approval_mode: ApprovalMode,
) -> TurnAuthority {
    let mode = requested_mode;
    let mut trust_mode = trust_mode;
    let mut auto_approve = auto_approve;
    let mut approval_mode = approval_mode;
    let mut narrowing = None;

    if !provenance_can_inherit_standing_auto_authority(provenance) {
        let from_mode = mode;
        let from_approval = approval_mode;
        let had_auto_authority =
            trust_mode || auto_approve || matches!(approval_mode, ApprovalMode::Bypass);
        trust_mode = false;
        auto_approve = false;
        if matches!(approval_mode, ApprovalMode::Auto | ApprovalMode::Bypass) {
            approval_mode = ApprovalMode::Suggest;
        }
        if had_auto_authority {
            // Record the transition, not just a sentence about it: the same
            // value drives the UI status, `<turn_meta>`, and doctor (#3947).
            narrowing = Some(PolicyNarrowingEvent {
                reason: PolicyNarrowingReason::NonAuthoritativeProvenance,
                from_mode: from_mode.as_setting(),
                to_mode: mode.as_setting(),
                from_approval,
                to_approval: approval_mode,
                detail: provenance.as_str().to_string(),
            });
        }
    }

    // The named permission posture is authoritative. Normalize legacy or
    // host inputs that carry `Bypass` with a stale false auto-approve bit so
    // every engine surface observes the same Full Access contract.
    if approval_mode == ApprovalMode::Bypass {
        auto_approve = true;
    }

    TurnAuthority {
        mode,
        allow_shell,
        trust_mode,
        auto_approve,
        approval_mode,
        dynamic_active_tools: Vec::new(),
        narrowing,
    }
}

#[must_use]
pub(crate) fn provenance_can_inherit_standing_auto_authority(
    provenance: UserInputProvenance,
) -> bool {
    matches!(
        provenance,
        UserInputProvenance::ExternalUser
            | UserInputProvenance::Runtime
            | UserInputProvenance::SubAgentHandoff
    )
}

#[must_use]
pub(crate) fn agent_approval_mode_for_turn(
    auto_approve: bool,
    approval_mode: ApprovalMode,
) -> ApprovalMode {
    if auto_approve {
        ApprovalMode::Bypass
    } else {
        approval_mode
    }
}

/// Resolve the filesystem boundary for one turn.
///
/// Permission posture and filesystem scope are separate controls, but the
/// named Full Access posture must have a truthful default: outside Plan it
/// disables Codewhale's own sandbox, matching the product meaning of the
/// name. An explicit effective sandbox setting may still *tighten* that
/// default. It can never loosen Plan, Ask, or Auto-Review.
#[must_use]
pub(crate) fn sandbox_policy_for_turn(
    mode: AppMode,
    approval_mode: ApprovalMode,
    configured_mode: Option<&str>,
    workspace: &Path,
    network_access: SandboxNetworkAccess,
) -> SandboxPolicy {
    let default = if mode == AppMode::Plan {
        SandboxPolicy::ReadOnly
    } else if approval_mode == ApprovalMode::Bypass {
        SandboxPolicy::DangerFullAccess
    } else {
        workspace_write_policy(workspace, network_access)
    };

    // The effective Config has already applied managed/project precedence.
    // Only stricter scopes clamp the posture-derived default: a configured
    // danger-full-access value must not silently loosen Ask or Auto-Review.
    match (default, configured_mode) {
        (SandboxPolicy::ReadOnly, _) | (_, Some("read-only")) => SandboxPolicy::ReadOnly,
        (SandboxPolicy::DangerFullAccess, Some("workspace-write")) => {
            workspace_write_policy(workspace, network_access)
        }
        (SandboxPolicy::DangerFullAccess, Some("external-sandbox")) => {
            SandboxPolicy::ExternalSandbox {
                network_access: network_access.is_allowed(),
            }
        }
        (policy, _) => policy,
    }
}

/// Whether a sandboxed turn may open outbound connections.
///
/// Typed rather than a bare `bool` so the two call-site meanings — "the user
/// asked for network" and "some caller passed true" — cannot be transposed
/// silently, and so the default is spelled at the type instead of at each of
/// the seven resolver call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SandboxNetworkAccess {
    /// No outbound network inside the sandbox. Editing the workspace does not
    /// imply reaching the internet.
    #[default]
    Restricted,
    /// Outbound network explicitly granted by config, policy, or an approved
    /// elevation.
    Allowed,
}

impl SandboxNetworkAccess {
    #[must_use]
    pub(crate) fn from_config(configured: Option<bool>) -> Self {
        if configured.unwrap_or(false) {
            Self::Allowed
        } else {
            Self::Restricted
        }
    }

    #[must_use]
    pub(crate) fn is_allowed(self) -> bool {
        matches!(self, Self::Allowed)
    }
}

fn workspace_write_policy(workspace: &Path, network_access: SandboxNetworkAccess) -> SandboxPolicy {
    SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![workspace.to_path_buf()],
        network_access: network_access.is_allowed(),
        exclude_tmpdir: false,
        exclude_slash_tmp: false,
    }
}

/// Resolve the effective shell policy for a turn from legacy shell opt-in plus mode.
#[must_use]
pub(crate) fn shell_policy_for_mode(mode: AppMode, allow_shell: bool) -> ShellPolicy {
    if !allow_shell {
        return ShellPolicy::None;
    }
    match mode {
        AppMode::Plan => ShellPolicy::None,
        AppMode::Agent | AppMode::Operate => ShellPolicy::Full,
    }
}

/// Per-tool permission decision from the unified resolver (#4412).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolPermission {
    /// Tool executes without any approval prompt.
    Allow,
    /// Tool requires user approval before execution.
    Prompt,
    /// Tool is denied without a prompt (approval_mode=Never).
    Deny,
}

/// Unified per-tool permission resolver (#4412).
///
/// Consolidates the approval decision that was previously scattered across
/// `registered_tool_approval_required` (turn_loop), `app_auto_approve_enabled`
/// (ui.rs), and the `Never` short-circuit. One call site, one answer.
///
/// The truth table mirrors the legacy helpers exactly:
/// - `Auto` tools always run — even under `Never`, which stays read-only
///   rather than dead.
/// - `Never` denies any tool that would otherwise prompt, but only when the
///   authority is not full-access shaped: a Bypass-shaped authority carrying
///   a stale `Never` enum still auto-approves, matching the legacy UI order
///   in which the full-access shortcut ran before the `Never` check.
/// - `Suggest` and `Required` are both bypassable by auto-approve authority
///   unless the tool is on the typed non-bypassable hold list
///   (`is_non_bypassable`), which always prompts. A generic `Required` tool
///   remains auto-approved in Full Access (#3866).
#[must_use]
pub(crate) fn resolve_tool_permission(
    authority: &TurnAuthority,
    requirement: ApprovalRequirement,
    is_non_bypassable: bool,
) -> ToolPermission {
    if authority.approval_mode == ApprovalMode::Never
        && requirement != ApprovalRequirement::Auto
        && !authority.auto_approve
    {
        return ToolPermission::Deny;
    }
    match requirement {
        ApprovalRequirement::Auto => ToolPermission::Allow,
        ApprovalRequirement::Suggest | ApprovalRequirement::Required => {
            if is_non_bypassable {
                // Full Access already grants everything these calls can do —
                // shell included — so a hold that cannot open its own
                // approval modal auto-approves instead of stranding the call.
                // #3866 blocked here through v0.9.6; reversed 2026-08-10.
                return if authority.auto_approve {
                    ToolPermission::Allow
                } else {
                    ToolPermission::Prompt
                };
            }
            if authority.auto_approve || authority.approval_mode == ApprovalMode::Bypass {
                ToolPermission::Allow
            } else {
                ToolPermission::Prompt
            }
        }
    }
}

/// Whether the session posture is the one the in-workspace write carve-out
/// (#5185) relaxes: the default Ask posture (`Suggest` approvals, no
/// auto-approve) in an Agent-family mode.
///
/// Every other posture keeps its exact prior meaning: Full Access already
/// runs these calls, `Never` still denies them, Auto-Review still fails
/// unresolved holds closed, and Plan is read-only by mode.
#[must_use]
pub(crate) fn write_carve_out_posture(
    mode: AppMode,
    approval_mode: ApprovalMode,
    auto_approve: bool,
) -> bool {
    !auto_approve
        && matches!(mode, AppMode::Agent | AppMode::Operate)
        && approval_mode == ApprovalMode::Suggest
}

/// Whether every target path of a file-write call qualifies for the
/// in-workspace write carve-out (#5185): the workspace is a git work tree,
/// each path resolves inside it, and none touches `.git` internals, runtime
/// state, or a sensitive file.
///
/// The git work-tree marker is deliberate (the same shape as kimi-code's
/// `git-cwd-write-approve` policy): the carve-out exists because
/// version-controlled edits stay reviewable and recoverable, so a workspace
/// without git keeps the modal.
#[must_use]
pub(crate) fn paths_within_workspace_write_carve_out(workspace: &Path, paths: &[String]) -> bool {
    if paths.is_empty() || !workspace_is_git_work_tree(workspace) {
        return false;
    }
    let Ok(workspace_canonical) = workspace.canonicalize() else {
        return false;
    };
    paths
        .iter()
        .all(|raw| carve_out_target_allowed(workspace, &workspace_canonical, raw))
}

/// The git work-tree marker the carve-out keys on. `.git` may be a directory
/// (normal checkout) or a file (worktree or submodule); either counts.
fn workspace_is_git_work_tree(workspace: &Path) -> bool {
    workspace.join(".git").symlink_metadata().is_ok()
}

/// Whether a file-write approval card should say that the folder has no git
/// repository: the posture is the one the carve-out (#5185) relaxes, the call
/// is a canonical file-write tool, and the workspace has no git work tree, so
/// the same edit would run without a card in a repository.
///
/// Presentation only. The engine has already decided to ask; this never
/// opens, skips or answers a card.
#[must_use]
pub(crate) fn file_write_asks_without_git(
    mode: AppMode,
    approval_mode: ApprovalMode,
    workspace: &Path,
    tool_name: &str,
    input: &Value,
) -> bool {
    write_carve_out_posture(mode, approval_mode, false)
        && matches!(
            canonical_action_alias(tool_name, input),
            "write_file" | "edit_file" | "apply_patch"
        )
        && !workspace_is_git_work_tree(workspace)
}

fn carve_out_target_allowed(workspace: &Path, workspace_canonical: &Path, raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return false;
    }
    let raw_path = Path::new(raw);
    let candidate = if raw_path.is_absolute() {
        raw_path.to_path_buf()
    } else {
        workspace.join(raw_path)
    };
    // Lexical containment first: `..` escapes and absolute out-of-tree paths
    // fail here without touching the filesystem.
    let lexical = normalize_path(&candidate);
    let workspace_lexical = normalize_path(workspace);
    let workspace_canonical_lexical = normalize_path(workspace_canonical);
    let Ok(relative) = lexical
        .strip_prefix(&workspace_lexical)
        .or_else(|_| lexical.strip_prefix(&workspace_canonical_lexical))
    else {
        return false;
    };
    if !carve_out_relative_path_allowed(relative) {
        return false;
    }
    // Then symlink reality: resolve the deepest existing ancestor and
    // require the real path to stay inside the real workspace and off the
    // same exclusions (a symlink hop into `.git` or out of the tree fails).
    let Some(resolved) = resolve_deepest_existing(&candidate) else {
        return false;
    };
    let Ok(resolved_relative) = resolved.strip_prefix(workspace_canonical) else {
        return false;
    };
    carve_out_relative_path_allowed(resolved_relative)
}

/// Canonicalize the deepest existing ancestor of `candidate` and re-append
/// the not-yet-existing tail, so write targets that do not exist yet still
/// get a real-path check.
fn resolve_deepest_existing(candidate: &Path) -> Option<PathBuf> {
    let mut ancestor = candidate;
    let mut suffix: Vec<&OsStr> = Vec::new();
    loop {
        if let Ok(canonical) = ancestor.canonicalize() {
            let mut resolved = canonical;
            for part in suffix.iter().rev() {
                resolved.push(part);
            }
            return Some(resolved);
        }
        suffix.push(ancestor.file_name()?);
        ancestor = ancestor.parent()?;
    }
}

fn carve_out_relative_path_allowed(relative: &Path) -> bool {
    relative.components().all(|component| {
        let Component::Normal(part) = component else {
            return true;
        };
        !is_carve_out_excluded_name(&part.to_string_lossy().to_ascii_lowercase())
    })
}

/// Names the carve-out never auto-allows, matched per path component:
/// `.git` internals, runtime/project state, credential-bearing directories
/// and files, and key material.
fn is_carve_out_excluded_name(name: &str) -> bool {
    // The shared `.git` rule, so a Windows alias (`.git.`, `GIT~1`) is
    // excluded here exactly as the workspace file routes exclude it.
    if crate::snapshot::is_git_metadata_name(OsStr::new(name)) {
        return true;
    }
    // Runtime/project state and credential-bearing directories. `.codewhale`
    // holds session state plus MCP/hook configuration — editing it changes
    // what runs, so it keeps the modal.
    if matches!(
        name,
        ".codewhale" | ".ssh" | ".aws" | ".gnupg" | ".kube" | ".docker"
    ) {
        return true;
    }
    // Environment files and well-known credential stores.
    if name.starts_with(".env")
        || name == ".netrc"
        || name == ".npmrc"
        || name == ".pypirc"
        || name == ".git-credentials"
        || name == "credentials"
        || name.starts_with("credentials.")
    {
        return true;
    }
    // SSH private (and public) key material.
    if name.starts_with("id_rsa")
        || name.starts_with("id_dsa")
        || name.starts_with("id_ecdsa")
        || name.starts_with("id_ed25519")
    {
        return true;
    }
    // Key/certificate containers by extension.
    matches!(
        Path::new(name).extension().and_then(|ext| ext.to_str()),
        Some("pem" | "key" | "p12" | "pfx" | "jks" | "keystore")
    )
}

/// Disposition for an approval request that reached the UI (#4412).
///
/// The engine emits `ApprovalRequired` whenever its resolver answer was
/// `Prompt`; the UI then disposes of that request — honoring session caches
/// and posture races — through this single decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApprovalRequestDisposition {
    /// Session grant or full-access posture: approve without a modal.
    AutoApprove,
    /// The user already denied this approval key this session (#360).
    AutoDenySessionDenied,
    /// A forced (non-bypassable) policy hold arrived under a full-access
    /// posture that opens no modal: fail closed.
    AutoDenyFullAccessPolicyHold,
    /// Auto-Review is autonomous: unresolved holds fail closed instead of
    /// opening a user-approval modal.
    AutoDenyAutoReview,
    /// approval_mode=Never: deny without a modal.
    AutoDenyNeverPosture,
    /// Open the approval modal.
    Prompt,
}

/// Resolve how the UI disposes of one incoming approval request.
///
/// `session_approved` / `session_denied` are the caller's lookups into the
/// session approval caches (grouping key or tool name / exact approval key).
/// The branch order: session denial, then the Auto-Review hold, then the
/// full-access policy-hold denial, then the `Never` denial — the live posture
/// wins over any remembered grant (approvals J) — then auto-approval (full
/// access or a session grant), and only finally a modal. Extension-origin
/// forced calls reach that modal even under Full Access; no remembered grant
/// can satisfy them. `extension_origin` comes from Rust-minted approval keys,
/// never an extension label or a claim supplied by the host.
#[must_use]
pub(crate) fn resolve_approval_request_disposition(
    authority: &TurnAuthority,
    session_approved: bool,
    session_denied: bool,
    approval_force_prompt: bool,
    extension_origin: bool,
) -> ApprovalRequestDisposition {
    if session_denied {
        return ApprovalRequestDisposition::AutoDenySessionDenied;
    }
    if authority.approval_mode_for_session() == ApprovalMode::Auto {
        return ApprovalRequestDisposition::AutoDenyAutoReview;
    }
    // The request exists, so the engine already resolved Prompt for the tool
    // itself. What remains is the posture question: how does this authority
    // treat an ordinary promptable tool?
    let posture = resolve_tool_permission(authority, ApprovalRequirement::Suggest, false);
    if approval_force_prompt && !extension_origin && posture == ToolPermission::Allow {
        return ApprovalRequestDisposition::AutoDenyFullAccessPolicyHold;
    }
    // The live posture wins over any remembered grant: a conversation grant
    // given under Ask never outlives a later switch to Never (approvals J).
    if posture == ToolPermission::Deny {
        return ApprovalRequestDisposition::AutoDenyNeverPosture;
    }
    if !approval_force_prompt && (posture == ToolPermission::Allow || session_approved) {
        return ApprovalRequestDisposition::AutoApprove;
    }
    ApprovalRequestDisposition::Prompt
}

/// Categorizes tools by cost/risk level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    /// Free, read-only operations (`list_dir`, `read_file`, todo_*)
    Safe,
    /// File modifications (`write_file`, `edit_file`)
    FileWrite,
    /// Shell execution (`exec_shell`)
    Shell,
    /// Network-oriented built-in tools
    Network,
    /// Read-only MCP discovery and resource access
    McpRead,
    /// MCP actions that may change remote state
    McpAction,
    /// Sub-agent lifecycle (`agent` start/status/peek/cancel); the child's
    /// own tool gates govern what it may actually do.
    Agent,
    /// Unknown or unclassified tool surface
    Unknown,
}

/// Get the category for a tool by name.
pub fn get_tool_category(name: &str) -> ToolCategory {
    if name == "agent" || name == "workflow" {
        // Workflow is multi-agent orchestration; reuse Agent stakes/routing
        // and specialize the impact card via build_impact_summary (#4126).
        ToolCategory::Agent
    } else if matches!(
        name,
        "write" | "edit" | "write_file" | "edit_file" | "apply_patch"
    ) {
        ToolCategory::FileWrite
    } else if matches!(
        name,
        "web_run" | "web_search" | "fetch_url" | "wait_for_dev_server" | "registry_sync"
    ) {
        ToolCategory::Network
    } else if matches!(
        name,
        "bash"
            | "Bash"
            | "exec_shell"
            | "task_shell_start"
            | "task_shell_wait"
            | "exec_shell_wait"
            | "exec_shell_interact"
            | "exec_shell_cancel"
            | "exec_wait"
            | "exec_interact"
    ) {
        ToolCategory::Shell
    } else if name.starts_with("list_mcp_")
        || name.starts_with("read_mcp_")
        || name.starts_with("get_mcp_")
    {
        ToolCategory::McpRead
    } else if name.starts_with("mcp_") {
        ToolCategory::McpAction
    } else if matches!(
        name,
        "read"
            | "read_file"
            | "list_dir"
            | "work_update"
            | "todo_write"
            | "todo_read"
            | "checklist_write"
            | "note"
            | "update_plan"
            | "search"
            | "file_search"
            | "grep_files"
            | "git_status"
            | "git_diff"
            | "git_log"
            | "git_show"
            | "git_blame"
            | "git_commit_plan"
            | "project"
            | "diagnostics"
    ) || name.starts_with("read_")
        || name.starts_with("list_")
        || name.starts_with("get_")
    {
        ToolCategory::Safe
    } else if matches!(name, "start_mcp_server" | "start_registry_mcp_server") {
        // Starting an MCP server spawns child processes or opens network
        // connections — classify as McpAction to trigger appropriate
        // approval prompts.
        ToolCategory::McpAction
    } else {
        ToolCategory::Unknown
    }
}

/// Categorize a concrete call after resolving an action-based canonical tool.
#[must_use]
pub fn get_tool_category_for_call(name: &str, params: &Value) -> ToolCategory {
    get_tool_category(canonical_action_alias(name, params))
}

pub fn permission_label_for(
    locale: codewhale_localization::Locale,
    mode: AppMode,
    approval: ApprovalMode,
) -> std::borrow::Cow<'static, str> {
    use codewhale_localization::{MessageId, tr};
    if mode == AppMode::Plan {
        return tr(locale, MessageId::ChipPermissionReadOnly);
    }
    match approval {
        ApprovalMode::Suggest => tr(locale, MessageId::ChipPermissionAsk),
        ApprovalMode::Auto => tr(locale, MessageId::ChipPermissionAuto),
        ApprovalMode::Bypass => tr(locale, MessageId::ChipPermissionFullAccess),
        ApprovalMode::Never => tr(locale, MessageId::ChipPermissionNever),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority(mode: AppMode, auto_approve: bool, approval_mode: ApprovalMode) -> TurnAuthority {
        TurnAuthority::from_effective_fields(mode, true, false, auto_approve, approval_mode)
    }

    #[test]
    fn write_carve_out_posture_is_exactly_the_default_ask_posture() {
        assert!(write_carve_out_posture(
            AppMode::Agent,
            ApprovalMode::Suggest,
            false
        ));
        assert!(write_carve_out_posture(
            AppMode::Operate,
            ApprovalMode::Suggest,
            false
        ));
        // Full Access already runs these calls; the carve-out must not be
        // what allows them.
        assert!(!write_carve_out_posture(
            AppMode::Agent,
            ApprovalMode::Bypass,
            true
        ));
        // Never still denies; Auto-Review still fails unresolved holds closed;
        // Plan is read-only by mode.
        assert!(!write_carve_out_posture(
            AppMode::Agent,
            ApprovalMode::Never,
            false
        ));
        assert!(!write_carve_out_posture(
            AppMode::Agent,
            ApprovalMode::Auto,
            false
        ));
        assert!(!write_carve_out_posture(
            AppMode::Plan,
            ApprovalMode::Suggest,
            false
        ));
    }

    #[test]
    fn file_write_card_names_the_missing_repository_only_where_git_changes_the_answer() {
        let plain = tempfile::tempdir().expect("tempdir");
        let repo = carve_out_workspace();
        let edit = serde_json::json!({ "path": "notes.txt", "search": "a", "replace": "b" });
        let asks = |mode, approval_mode, workspace: &Path, tool: &str| {
            file_write_asks_without_git(mode, approval_mode, workspace, tool, &edit)
        };

        // The lesson-1 case: default Ask posture, plain folder, a file edit.
        for tool in ["edit_file", "write_file", "apply_patch"] {
            assert!(
                asks(AppMode::Agent, ApprovalMode::Suggest, plain.path(), tool),
                "{tool}"
            );
        }
        // In a repository the same call runs without a card, so a card that
        // does open there is asking for some other reason.
        assert!(!asks(
            AppMode::Agent,
            ApprovalMode::Suggest,
            repo.path(),
            "edit_file"
        ));
        assert!(paths_within_workspace_write_carve_out(
            repo.path(),
            &["notes.txt".to_string()]
        ));
        assert!(!paths_within_workspace_write_carve_out(
            plain.path(),
            &["notes.txt".to_string()]
        ));
        // Postures the carve-out never relaxes ask with or without git.
        assert!(!asks(
            AppMode::Agent,
            ApprovalMode::Auto,
            plain.path(),
            "edit_file"
        ));
        assert!(!asks(
            AppMode::Plan,
            ApprovalMode::Suggest,
            plain.path(),
            "edit_file"
        ));
        // Git presence never changes whether a command asks.
        assert!(!asks(
            AppMode::Agent,
            ApprovalMode::Suggest,
            plain.path(),
            "exec_shell"
        ));
    }

    fn carve_out_workspace() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("git marker");
        std::fs::create_dir_all(tmp.path().join("src")).expect("src dir");
        std::fs::write(tmp.path().join("src/main.rs"), "fn main() {}\n").expect("source file");
        tmp
    }

    #[test]
    fn carve_out_allows_in_workspace_write_targets() {
        let tmp = carve_out_workspace();
        let workspace = tmp.path();
        for paths in [
            vec!["src/main.rs".to_string()],
            vec!["src/new_file.rs".to_string()],
            vec!["deeply/nested/not-yet-created.rs".to_string()],
            vec!["./src/main.rs".to_string()],
            vec![workspace.join("src/main.rs").to_string_lossy().into_owned()],
            vec!["src/main.rs".to_string(), "src/other.rs".to_string()],
        ] {
            assert!(
                paths_within_workspace_write_carve_out(workspace, &paths),
                "{paths:?} should qualify"
            );
        }
    }

    #[test]
    fn carve_out_rejects_out_of_tree_sensitive_and_git_paths() {
        let tmp = carve_out_workspace();
        let workspace = tmp.path();
        for paths in [
            vec!["../outside.rs".to_string()],
            vec!["src/../../outside.rs".to_string()],
            vec!["/etc/passwd".to_string()],
            vec![".git/config".to_string()],
            vec!["nested/.git/hooks/pre-commit".to_string()],
            vec![".env".to_string()],
            vec!["config/.env.production".to_string()],
            vec![".ssh/config".to_string()],
            vec!["deploy/id_rsa".to_string()],
            vec!["certs/server.pem".to_string()],
            vec![".codewhale/mcp.json".to_string()],
            vec!["aws/credentials".to_string()],
            // One bad target poisons the whole call.
            vec!["src/main.rs".to_string(), ".env".to_string()],
        ] {
            assert!(
                !paths_within_workspace_write_carve_out(workspace, &paths),
                "{paths:?} must keep the modal"
            );
        }
    }

    #[test]
    fn carve_out_excludes_git_metadata_by_the_shared_rule() {
        let tmp = carve_out_workspace();
        let workspace = tmp.path();
        let mut refused = vec![".GIT/config", "nested/.Git/hooks/pre-commit"];
        if cfg!(windows) {
            refused.extend([".git./config", ".git /config", "GIT~1/config"]);
        }
        for path in refused {
            assert!(
                !paths_within_workspace_write_carve_out(workspace, &[path.to_string()]),
                "{path} must keep the modal"
            );
        }
        assert!(paths_within_workspace_write_carve_out(
            workspace,
            &[".github/workflows/ci.yml".to_string()]
        ));
    }

    #[test]
    fn carve_out_requires_a_git_work_tree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(!paths_within_workspace_write_carve_out(
            tmp.path(),
            &["src/main.rs".to_string()]
        ));
    }

    #[test]
    fn carve_out_rejects_empty_target_list() {
        let tmp = carve_out_workspace();
        assert!(!paths_within_workspace_write_carve_out(tmp.path(), &[]));
    }

    #[cfg(unix)]
    #[test]
    fn carve_out_rejects_symlink_escapes() {
        let tmp = carve_out_workspace();
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::os::unix::fs::symlink(outside.path(), tmp.path().join("link")).expect("symlink");
        assert!(!paths_within_workspace_write_carve_out(
            tmp.path(),
            &["link/evil.rs".to_string()]
        ));
        // A symlink that stays inside the workspace is fine.
        std::os::unix::fs::symlink(tmp.path().join("src"), tmp.path().join("src-link"))
            .expect("inner symlink");
        assert!(paths_within_workspace_write_carve_out(
            tmp.path(),
            &["src-link/main.rs".to_string()]
        ));
    }

    #[test]
    fn full_access_is_unsandboxed_unless_effective_config_is_stricter() {
        let workspace = Path::new("/work");
        let full_access = authority(AppMode::Agent, true, ApprovalMode::Bypass);

        assert_eq!(
            full_access.sandbox_policy(workspace, None, SandboxNetworkAccess::Restricted),
            SandboxPolicy::DangerFullAccess
        );
        // Clamping full-access down to workspace-write must land on the same
        // restricted posture an ordinary Agent turn gets, not on a wider one.
        assert!(matches!(
            full_access.sandbox_policy(
                workspace,
                Some("workspace-write"),
                SandboxNetworkAccess::Restricted
            ),
            SandboxPolicy::WorkspaceWrite { writable_roots, network_access, .. }
                if writable_roots == vec![workspace.to_path_buf()] && !network_access
        ));
        assert_eq!(
            full_access.sandbox_policy(
                workspace,
                Some("read-only"),
                SandboxNetworkAccess::Restricted
            ),
            SandboxPolicy::ReadOnly
        );
        // The external sandbox no longer claims network unconditionally; it
        // reports what was actually granted.
        assert!(matches!(
            full_access.sandbox_policy(
                workspace,
                Some("external-sandbox"),
                SandboxNetworkAccess::Restricted
            ),
            SandboxPolicy::ExternalSandbox {
                network_access: false
            }
        ));
        assert!(matches!(
            full_access.sandbox_policy(
                workspace,
                Some("external-sandbox"),
                SandboxNetworkAccess::Allowed
            ),
            SandboxPolicy::ExternalSandbox {
                network_access: true
            }
        ));
    }

    #[test]
    fn workspace_write_never_grants_network_without_an_explicit_opt_in() {
        let workspace = Path::new("/work");
        // Every non-Yolo posture, with and without a configured sandbox mode.
        for approval_mode in [
            ApprovalMode::Suggest,
            ApprovalMode::Auto,
            ApprovalMode::Never,
        ] {
            for configured in [None, Some("workspace-write"), Some("danger-full-access")] {
                let auth = authority(AppMode::Agent, false, approval_mode);
                let policy =
                    auth.sandbox_policy(workspace, configured, SandboxNetworkAccess::Restricted);
                assert!(
                    !policy.has_network_access(),
                    "{approval_mode:?}/{configured:?} leaked network: {policy:?}"
                );
            }
        }

        // The Bypass posture is deliberately unsandboxed and keeps its
        // semantics: DangerFullAccess reports network regardless of this key,
        // because it applies no sandbox at all.
        let bypass = authority(AppMode::Agent, true, ApprovalMode::Bypass);
        let policy = bypass.sandbox_policy(workspace, None, SandboxNetworkAccess::Restricted);
        assert_eq!(policy, SandboxPolicy::DangerFullAccess);
        assert!(policy.has_network_access());

        // Plan is read-only and denies network under either setting.
        let plan = authority(AppMode::Plan, false, ApprovalMode::Suggest);
        for access in [
            SandboxNetworkAccess::Restricted,
            SandboxNetworkAccess::Allowed,
        ] {
            assert!(
                !plan
                    .sandbox_policy(workspace, None, access)
                    .has_network_access()
            );
        }
    }

    #[test]
    fn sandbox_network_access_defaults_to_restricted() {
        assert_eq!(
            SandboxNetworkAccess::default(),
            SandboxNetworkAccess::Restricted
        );
        assert_eq!(
            SandboxNetworkAccess::from_config(None),
            SandboxNetworkAccess::Restricted
        );
        assert_eq!(
            SandboxNetworkAccess::from_config(Some(false)),
            SandboxNetworkAccess::Restricted
        );
        assert_eq!(
            SandboxNetworkAccess::from_config(Some(true)),
            SandboxNetworkAccess::Allowed
        );
    }

    #[test]
    fn plan_ask_and_auto_review_cannot_be_loosened_by_sandbox_config() {
        let workspace = Path::new("/work");
        for approval_mode in [ApprovalMode::Suggest, ApprovalMode::Auto] {
            let authority = authority(AppMode::Agent, false, approval_mode);
            assert!(matches!(
                authority.sandbox_policy(
                    workspace,
                    Some("danger-full-access"),
                    SandboxNetworkAccess::Restricted
                ),
                SandboxPolicy::WorkspaceWrite { .. }
            ));
        }

        let plan = authority(AppMode::Plan, true, ApprovalMode::Bypass);
        assert_eq!(
            plan.sandbox_policy(
                workspace,
                Some("danger-full-access"),
                SandboxNetworkAccess::Restricted
            ),
            SandboxPolicy::ReadOnly
        );
    }

    #[test]
    fn outbound_web_payloads_require_a_session_decision_even_for_allowed_hosts() {
        use crate::tools::{
            fetch_url::FetchUrlTool, spec::ToolSpec, web_run::WebRunTool,
            web_search::WebSearchTool, web_tool::WebTool,
        };
        let request = serde_json::json!({"action": "fetch", "url": "https://example.com/collect?data=synthetic-secret"});
        for requirement in [
            FetchUrlTool.approval_requirement_for(&request),
            WebTool::new("Web").approval_requirement_for(&request),
            WebSearchTool.approval_requirement(),
            WebRunTool.approval_requirement(),
        ] {
            for approval in [ApprovalMode::Suggest, ApprovalMode::Auto] {
                let ask = authority(AppMode::Agent, false, approval);
                assert_eq!(
                    resolve_tool_permission(&ask, requirement, false),
                    ToolPermission::Prompt
                );
            }
            let never = authority(AppMode::Agent, false, ApprovalMode::Never);
            assert_eq!(
                resolve_tool_permission(&never, requirement, false),
                ToolPermission::Deny
            );
            let granted = authority(AppMode::Agent, true, ApprovalMode::Bypass);
            assert_eq!(
                resolve_tool_permission(&granted, requirement, false),
                ToolPermission::Allow
            );
        }
        let local_read = crate::tools::file::ReadFileTool.approval_requirement();
        assert_eq!(
            resolve_tool_permission(
                &authority(AppMode::Agent, false, ApprovalMode::Suggest),
                local_read,
                false
            ),
            ToolPermission::Allow
        );
    }

    #[test]
    fn auto_requirement_always_allows() {
        for (mode, auto_approve, approval_mode) in [
            (AppMode::Agent, false, ApprovalMode::Suggest),
            (AppMode::Agent, false, ApprovalMode::Auto),
            (AppMode::Agent, false, ApprovalMode::Never),
            (AppMode::Agent, true, ApprovalMode::Bypass),
            (AppMode::Plan, false, ApprovalMode::Suggest),
        ] {
            let auth = authority(mode, auto_approve, approval_mode);
            for non_bypassable in [false, true] {
                assert_eq!(
                    resolve_tool_permission(&auth, ApprovalRequirement::Auto, non_bypassable),
                    ToolPermission::Allow,
                    "{mode:?}/{auto_approve}/{approval_mode:?}/nb={non_bypassable}"
                );
            }
        }
    }

    #[test]
    fn ask_posture_prompts_for_non_auto_tools() {
        let auth = authority(AppMode::Agent, false, ApprovalMode::Suggest);
        for requirement in [ApprovalRequirement::Suggest, ApprovalRequirement::Required] {
            assert_eq!(
                resolve_tool_permission(&auth, requirement, false),
                ToolPermission::Prompt
            );
            assert_eq!(
                resolve_tool_permission(&auth, requirement, true),
                ToolPermission::Prompt
            );
        }
    }

    #[test]
    fn full_access_allows_bypassable_but_prompts_for_non_bypassable() {
        for auth in [
            authority(AppMode::Agent, true, ApprovalMode::Bypass),
            TurnAuthority::for_tool_approval_decision(true),
        ] {
            for requirement in [ApprovalRequirement::Suggest, ApprovalRequirement::Required] {
                assert_eq!(
                    resolve_tool_permission(&auth, requirement, false),
                    ToolPermission::Allow,
                    "generic {requirement:?} tool stays auto-approved in Full Access"
                );
                assert_eq!(
                    resolve_tool_permission(&auth, requirement, true),
                    ToolPermission::Allow,
                    "non-bypassable {requirement:?} tool auto-approves in Full Access (#3866 reversed)"
                );
            }
        }

        // Ask (the default suggest posture without auto-approve) can open the
        // modal, so the hold still prompts there.
        let ask = authority(AppMode::Agent, false, ApprovalMode::Suggest);
        for requirement in [ApprovalRequirement::Suggest, ApprovalRequirement::Required] {
            assert_eq!(
                resolve_tool_permission(&ask, requirement, true),
                ToolPermission::Prompt,
                "non-bypassable {requirement:?} tool still prompts in Ask"
            );
        }
    }

    #[test]
    fn never_denies_promptable_tools_but_not_reads_or_full_access_shapes() {
        let never = authority(AppMode::Agent, false, ApprovalMode::Never);
        assert_eq!(
            resolve_tool_permission(&never, ApprovalRequirement::Suggest, false),
            ToolPermission::Deny
        );
        assert_eq!(
            resolve_tool_permission(&never, ApprovalRequirement::Required, true),
            ToolPermission::Deny
        );
        assert_eq!(
            resolve_tool_permission(&never, ApprovalRequirement::Auto, false),
            ToolPermission::Allow,
            "Never remains read-only rather than dead"
        );

        // Legacy host shape: a full-access bit with a stale Never enum still
        // auto-approves — the UI's full-access shortcut ran before its Never
        // check.
        let stale = authority(AppMode::Agent, true, ApprovalMode::Never);
        assert_eq!(
            resolve_tool_permission(&stale, ApprovalRequirement::Suggest, false),
            ToolPermission::Allow
        );
    }

    #[test]
    fn approval_request_disposition_preserves_legacy_branch_order() {
        let ask = authority(AppMode::Agent, false, ApprovalMode::Suggest);
        let auto = authority(AppMode::Agent, false, ApprovalMode::Auto);
        let full_access = authority(AppMode::Agent, true, ApprovalMode::Bypass);
        let never = authority(AppMode::Agent, false, ApprovalMode::Never);

        // Session denial wins over everything, including full access.
        assert_eq!(
            resolve_approval_request_disposition(&full_access, true, true, false, false),
            ApprovalRequestDisposition::AutoDenySessionDenied
        );
        // Forced hold under full access fails closed instead of auto-approving.
        assert_eq!(
            resolve_approval_request_disposition(&full_access, true, false, true, false),
            ApprovalRequestDisposition::AutoDenyFullAccessPolicyHold
        );
        // Full access and session grants auto-approve ordinary requests.
        assert_eq!(
            resolve_approval_request_disposition(&full_access, false, false, false, false),
            ApprovalRequestDisposition::AutoApprove
        );
        assert_eq!(
            resolve_approval_request_disposition(&ask, true, false, false, false),
            ApprovalRequestDisposition::AutoApprove
        );
        // The live Never posture wins over a remembered session grant
        // (approvals J, CURRENT_DECISIONS §21), and denies everything else
        // promptable.
        assert_eq!(
            resolve_approval_request_disposition(&never, true, false, false, false),
            ApprovalRequestDisposition::AutoDenyNeverPosture
        );
        assert_eq!(
            resolve_approval_request_disposition(&never, false, false, false, false),
            ApprovalRequestDisposition::AutoDenyNeverPosture
        );
        for force_prompt in [false, true] {
            assert_eq!(
                resolve_approval_request_disposition(&auto, false, false, force_prompt, false),
                ApprovalRequestDisposition::AutoDenyAutoReview
            );
        }
        // Ask posture with no grant opens the modal.
        assert_eq!(
            resolve_approval_request_disposition(&ask, false, false, false, false),
            ApprovalRequestDisposition::Prompt
        );
    }
}

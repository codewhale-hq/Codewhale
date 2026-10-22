//! Tool specification traits for the CodeWhale agent system.
//!
//! This module defines the core abstractions for tools:
//! - `ToolSpec`: The main trait that all tools must implement
//! - `ToolContext`: Execution context passed to tools
//! - `ToolResult`: Unified result type for tool execution
//! - `ToolCapability`: Capabilities and requirements of tools

use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

use codewhale_execpolicy::ApprovalMode;

use crate::features::Features;
use crate::lsp::LspManager;
use crate::network_policy::NetworkPolicyDecider;
use crate::rlm::session::SessionObjectSnapshot;
use crate::rlm::session::{SharedRlmSessionStore, new_shared_rlm_session_store};
use crate::sandbox::backend::SandboxBackend;
use crate::tools::handle::{SharedHandleStore, new_shared_handle_store};
use crate::tools::shell::{SharedShellManager, new_shared_shell_manager};
use crate::worker_profile::ShellPolicy;
#[allow(unused_imports)]
pub use codewhale_tools::{
    ApprovalRequirement, PreparedToolCall, ResourceClaim, ToolCapability, ToolError,
    ToolExecutionOutcome, ToolResult, ToolResultContentBlock, ToolTerminalStatus, optional_bool,
    optional_bool_opt, optional_str, optional_u64, required_str, required_u64,
    schedule_non_conflicting, type_mismatch,
};

/// Text plus provider-neutral rich blocks at the conversation boundary.
#[derive(Debug, Clone)]
pub(crate) struct RichToolResult {
    pub result: ToolResult,
    pub content_blocks: Vec<ToolResultContentBlock>,
}

impl RichToolResult {
    #[must_use]
    pub fn plain(result: ToolResult) -> Self {
        Self {
            result,
            content_blocks: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_content_blocks(
        result: ToolResult,
        content_blocks: Vec<ToolResultContentBlock>,
    ) -> Self {
        Self {
            result,
            content_blocks,
        }
    }

    #[must_use]
    pub fn into_result(self) -> ToolResult {
        self.result
    }
}

impl std::ops::Deref for RichToolResult {
    type Target = ToolResult;

    fn deref(&self) -> &Self::Target {
        &self.result
    }
}

#[async_trait]
pub trait DynamicToolExecutor: Send + Sync {
    async fn execute_dynamic_tool(
        &self,
        thread_id: Option<String>,
        namespace: Option<String>,
        name: String,
        input: Value,
    ) -> Result<ToolResult, ToolError>;
}

/// Optional durable runtime services made available to model-visible tools.
///
/// These are intentionally optional so existing unit tests and one-off tool
/// contexts keep working. Tools that need durable task/automation state fail
/// closed with a clear "not available" error when the relevant service is not
/// attached.
#[derive(Clone)]
pub struct RuntimeToolServices {
    pub shell_manager: Option<SharedShellManager>,
    /// True only for the real headless exec host after it has established the
    /// explicit authority required to transfer `persist:true` services.
    pub persist_services_enabled: bool,
    pub task_manager: Option<crate::task_manager::SharedTaskManager>,
    pub automations: Option<crate::automation_manager::SharedAutomationManager>,
    pub task_data_dir: Option<PathBuf>,
    pub active_task_id: Option<String>,
    pub active_thread_id: Option<String>,
    pub dynamic_tool_executor: Option<Arc<dyn DynamicToolExecutor>>,
    /// Active-session Work Graph authority plus its legacy Plan/To-do views.
    pub work: Option<crate::work_graph::SharedWorkRuntime>,
    /// Hook executor for `shell_env` injection (#456) and any future
    /// tool-side hook events. `None` outside the live engine — test
    /// contexts that don't care about hooks get a no-op.
    pub hook_executor: Option<std::sync::Arc<crate::hooks::HookExecutor>>,
    /// Per-session backing store for `var_handle` payloads. Cloned tool
    /// contexts share this Arc so handles survive across turns.
    pub handle_store: SharedHandleStore,
    /// Bounded host-computed source observations, isolated from writable
    /// var_handles. Children clone the Arc, not delivery state or authority.
    pub code_observations: SharedHandleStore,
    /// Per-session persistent RLM kernels, keyed by caller-chosen context name.
    pub rlm_sessions: SharedRlmSessionStore,
    /// Directory for `read_media`'s content-addressed store of
    /// pre-compression image originals. `None` (tests and one-off contexts)
    /// disables persistence so unit tests never touch the real state dir;
    /// production wiring points it at `<codewhale home>/media-originals`.
    pub media_originals_dir: Option<PathBuf>,
}

impl Default for RuntimeToolServices {
    fn default() -> Self {
        Self {
            shell_manager: None,
            persist_services_enabled: false,
            task_manager: None,
            automations: None,
            task_data_dir: None,
            active_task_id: None,
            active_thread_id: None,
            dynamic_tool_executor: None,
            work: None,
            hook_executor: None,
            handle_store: new_shared_handle_store(),
            code_observations: new_shared_handle_store(),
            rlm_sessions: new_shared_rlm_session_store(),
            media_originals_dir: None,
        }
    }
}

impl std::fmt::Debug for RuntimeToolServices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeToolServices")
            .field("shell_manager", &self.shell_manager.is_some())
            .field("persist_services_enabled", &self.persist_services_enabled)
            .field("task_manager", &self.task_manager.is_some())
            .field("automations", &self.automations.is_some())
            .field("task_data_dir", &self.task_data_dir)
            .field("active_task_id", &self.active_task_id)
            .field("active_thread_id", &self.active_thread_id)
            .field(
                "dynamic_tool_executor",
                &self.dynamic_tool_executor.is_some(),
            )
            .field("work", &self.work.is_some())
            .field("hook_executor", &self.hook_executor.is_some())
            .field("handle_store", &true)
            .field("code_observations", &true)
            .field("rlm_sessions", &true)
            .field("media_originals_dir", &self.media_originals_dir)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileReadSnapshot {
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Debug, Default)]
pub struct FileReadTracker {
    reads: HashMap<PathBuf, FileReadSnapshot>,
}

pub type SharedFileReadTracker = Arc<Mutex<FileReadTracker>>;

pub(crate) fn new_shared_file_read_tracker() -> SharedFileReadTracker {
    Arc::new(Mutex::new(FileReadTracker::default()))
}

fn file_read_snapshot(path: &Path) -> Result<FileReadSnapshot, ToolError> {
    let metadata = fs::metadata(path).map_err(|e| {
        ToolError::execution_failed(format!("Failed to inspect {}: {e}", path.display()))
    })?;
    Ok(FileReadSnapshot {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// Sandbox policy for command execution.
#[derive(Debug, Clone, Default)]
pub enum SandboxPolicy {
    /// No sandboxing (dangerous but sometimes needed)
    #[default]
    None,
}

/// Machine-readable mutation boundary for a headless worker process.
///
/// Fleet serializes this envelope onto the exact `codewhale exec` argv. The
/// child installs it before constructing its engine, and every ToolContext in
/// that process inherits the same outer cap. Nested agents may narrow this
/// boundary, but cannot remove or expand it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolAuthorityEnvelope {
    pub schema_version: u32,
    pub owner: String,
    pub authority: ToolMutationAuthority,
    /// Optional outer network cap for headless workers. `None` preserves the
    /// behavior of v1 envelopes written before this field existed; new Fleet
    /// launches always carry the resolved worker permission explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_access: Option<bool>,
    /// Explicit shell cap for a headless worker. Older v1 envelopes omit this
    /// field and therefore remain shell-less; mutation authority is never
    /// treated as an implicit shell grant.
    #[serde(default, skip_serializing_if = "ToolShellAuthority::is_none")]
    pub shell: ToolShellAuthority,
    /// Narrow process-start authority for the built-in verification surface.
    /// This is separate from both mutation and shell authority: a verifier may
    /// run classifier-bounded workspace checks, but that never grants Bash or
    /// an operator-supplied command line.
    #[serde(default, skip_serializing_if = "ToolVerificationAuthority::is_none")]
    pub verification: ToolVerificationAuthority,
    #[serde(default)]
    pub writable_roots: Vec<String>,
    #[serde(default)]
    pub writable_files: Vec<String>,
    #[serde(default)]
    pub coordination_contracts: Vec<String>,
}

/// Shell authority carried across the Fleet subprocess boundary.
///
/// Full/arbitrary shell is intentionally not representable here. Fleet can
/// opt a Scout/Reviewer worker into the classifier-proven read subset, while every
/// other headless role keeps the historical shell-less posture.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolShellAuthority {
    #[default]
    None,
    ReadOnly,
}

impl ToolShellAuthority {
    #[must_use]
    pub const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    #[must_use]
    const fn shell_policy(self) -> ShellPolicy {
        match self {
            Self::None => ShellPolicy::None,
            Self::ReadOnly => ShellPolicy::ReadOnly,
        }
    }
}

/// Process authority for Fleet's dedicated verifier role.
///
/// Arbitrary execution is intentionally not representable. The registry and
/// dispatch boundary admit only calls that the shared verification classifier
/// proves are default workspace checks or pure test selection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolVerificationAuthority {
    #[default]
    None,
    Bounded,
}

impl ToolVerificationAuthority {
    #[must_use]
    pub const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

/// Whether a headless Fleet process should register the one read-only Bash
/// surface after intersecting the transported cap with explicit tool denies.
#[must_use]
pub(crate) fn fleet_exec_shell_enabled(
    fleet_authority_active: bool,
    shell_authority: ToolShellAuthority,
    disallowed_tools: Option<&[String]>,
) -> bool {
    fleet_authority_active
        && shell_authority == ToolShellAuthority::ReadOnly
        && !disallowed_tools.is_some_and(|rules| {
            rules.iter().any(|rule| {
                let rule = rule.trim().to_ascii_lowercase();
                rule.strip_suffix('*')
                    .map_or_else(|| rule == "bash", |prefix| "bash".starts_with(prefix))
            })
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolMutationAuthority {
    ReadOnly,
    ScopedWrite,
}

static PROCESS_TOOL_AUTHORITY: OnceLock<Arc<ToolAuthorityEnvelope>> = OnceLock::new();

impl ToolAuthorityEnvelope {
    pub fn normalized(mut self) -> Result<Self, String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported tool authority schema version {}",
                self.schema_version
            ));
        }
        self.owner = bounded_authority_value("owner", &self.owner, 128)?;
        self.writable_roots = normalize_authority_paths(&self.writable_roots, "writable_roots")?;
        self.writable_files = normalize_authority_paths(&self.writable_files, "writable_files")?;
        self.coordination_contracts = normalize_authority_values(
            &self.coordination_contracts,
            "coordination_contracts",
            16,
            128,
        )?;
        if self.authority == ToolMutationAuthority::ScopedWrite
            && self.writable_roots.is_empty()
            && self.writable_files.is_empty()
            && self.coordination_contracts.is_empty()
        {
            return Err(
                "scoped_write authority requires a writable root, exact file, or coordination contract"
                    .to_string(),
            );
        }
        if self.authority == ToolMutationAuthority::ReadOnly
            && (!self.writable_roots.is_empty()
                || !self.writable_files.is_empty()
                || !self.coordination_contracts.is_empty())
        {
            return Err("read_only authority cannot carry mutation scope".to_string());
        }
        if self.verification == ToolVerificationAuthority::Bounded
            && (self.authority != ToolMutationAuthority::ReadOnly
                || self.shell != ToolShellAuthority::None)
        {
            return Err(
                "bounded verification requires read_only mutation authority and no Bash authority"
                    .to_string(),
            );
        }
        Ok(self)
    }

    pub fn from_json(raw: &str) -> Result<Self, String> {
        serde_json::from_str::<Self>(raw)
            .map_err(|error| format!("invalid tool authority envelope: {error}"))?
            .normalized()
    }

    #[cfg(test)]
    fn is_within(&self, outer: &Self) -> bool {
        if self.shell > outer.shell
            || self.verification > outer.verification
            || (outer.network_access == Some(false) && self.network_access != Some(false))
        {
            return false;
        }
        if self.authority == ToolMutationAuthority::ReadOnly {
            return true;
        }
        if outer.authority != ToolMutationAuthority::ScopedWrite {
            return false;
        }
        self.writable_roots.iter().all(|path| {
            outer
                .writable_roots
                .iter()
                .any(|root| authority_path_is_within_root(path, root))
        }) && self.writable_files.iter().all(|path| {
            outer.writable_files.contains(path)
                || outer
                    .writable_roots
                    .iter()
                    .any(|root| authority_path_is_within_root(path, root))
        }) && self
            .coordination_contracts
            .iter()
            .all(|contract| outer.coordination_contracts.contains(contract))
    }

    pub fn permits_mutation_path(
        &self,
        context: &ToolContext,
        raw_path: &str,
    ) -> Result<bool, ToolError> {
        if self.authority == ToolMutationAuthority::ReadOnly {
            return Ok(false);
        }
        let target = resolve_strict_authority_path(context, raw_path)?;
        for file in &self.writable_files {
            if resolve_strict_authority_path(context, file)? == target {
                return Ok(true);
            }
        }
        for root in &self.writable_roots {
            if target.starts_with(resolve_strict_authority_path(context, root)?) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
fn authority_path_is_within_root(path: &str, root: &str) -> bool {
    root == "."
        || path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub fn install_process_tool_authority(envelope: ToolAuthorityEnvelope) -> Result<(), String> {
    let envelope = Arc::new(envelope.normalized()?);
    if let Some(existing) = PROCESS_TOOL_AUTHORITY.get() {
        return if existing.as_ref() == envelope.as_ref() {
            Ok(())
        } else {
            Err("tool authority envelope was already installed for this process".to_string())
        };
    }
    PROCESS_TOOL_AUTHORITY
        .set(envelope)
        .map_err(|_| "tool authority envelope was already installed for this process".to_string())
}

fn process_tool_authority() -> Option<Arc<ToolAuthorityEnvelope>> {
    PROCESS_TOOL_AUTHORITY.get().cloned()
}

fn bounded_authority_value(field: &str, value: &str, max_chars: usize) -> Result<String, String> {
    let value = value.trim().nfc().collect::<String>();
    if value.is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
    {
        return Err(format!(
            "tool authority {field} must be one non-empty line of at most {max_chars} characters"
        ));
    }
    Ok(value)
}

fn normalize_authority_paths(values: &[String], field: &str) -> Result<Vec<String>, String> {
    if values.len() > 32 {
        return Err(format!("tool authority {field} accepts at most 32 entries"));
    }
    let mut normalized = Vec::new();
    for raw in values {
        let raw = bounded_authority_value(field, raw, 512)?.replace('\\', "/");
        let windows_drive = raw.as_bytes().get(1) == Some(&b':')
            && raw.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
        if raw.starts_with('/') || raw.starts_with("//") || windows_drive {
            return Err(format!(
                "tool authority {field} entries must be repo-relative"
            ));
        }
        let mut segments = Vec::new();
        for segment in raw.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    return Err(format!(
                        "tool authority {field} cannot contain parent traversal"
                    ));
                }
                value => segments.push(value),
            }
        }
        let path = if segments.is_empty() {
            ".".to_string()
        } else {
            segments.join("/")
        };
        if !normalized.contains(&path) {
            normalized.push(path);
        }
    }
    Ok(normalized)
}

fn normalize_authority_values(
    values: &[String],
    field: &str,
    max_entries: usize,
    max_chars: usize,
) -> Result<Vec<String>, String> {
    if values.len() > max_entries {
        return Err(format!(
            "tool authority {field} accepts at most {max_entries} entries"
        ));
    }
    let mut normalized = Vec::new();
    for value in values {
        let value = bounded_authority_value(field, value, max_chars)?;
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    Ok(normalized)
}

pub(crate) fn resolve_strict_authority_path(
    context: &ToolContext,
    raw_path: &str,
) -> Result<PathBuf, ToolError> {
    let normalized = normalize_authority_paths(&[raw_path.to_string()], "mutation_path")
        .map_err(ToolError::permission_denied)?
        .into_iter()
        .next()
        .ok_or_else(|| ToolError::permission_denied("mutation path cannot be empty"))?;
    let workspace = context.workspace.canonicalize().map_err(|error| {
        ToolError::execution_failed(format!(
            "Failed to canonicalize authority workspace {}: {error}",
            context.workspace.display()
        ))
    })?;
    let mut current = workspace.clone();
    if normalized != "." {
        for segment in normalized.split('/') {
            current.push(segment);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(ToolError::permission_denied(format!(
                        "machine-readable authority paths must not traverse symlinks: {}",
                        current.display()
                    )));
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(ToolError::execution_failed(format!(
                        "Failed to inspect authority path {}: {error}",
                        current.display()
                    )));
                }
            }
        }
    }
    if !current.starts_with(&workspace) {
        return Err(ToolError::permission_denied(format!(
            "machine-readable authority path escapes workspace: {}",
            current.display()
        )));
    }
    Ok(current)
}

/// Context passed to tools during execution.
#[derive(Clone)]
pub struct ToolContext {
    /// The workspace root directory
    pub workspace: PathBuf,
    /// Per-turn policy and attached services. Kept behind one owned group so
    /// cloning a context preserves the historical value semantics while the
    /// top-level context remains small and stable as services evolve.
    pub execution: Box<ToolExecutionState>,
}

/// Policy and service state attached to one tool-execution context.
///
/// `ToolContext` dereferences to this group for source compatibility with
/// existing tools. New code can use `context.execution` when the grouping is
/// useful, without growing the top-level context by another field per feature.
#[derive(Clone)]
pub struct ToolExecutionState {
    /// Actual Core caller admission for per-round Python RPCs. Kernels do not
    /// retain it; unattached tool contexts cannot make model calls.
    pub(crate) rlm_caller: Option<Arc<crate::core::engine::rlm_host::CapturedRlmCaller>>,
    /// Effective session/ancestor tool ceiling, carried to MCP dispatch and runtime registration.
    pub(crate) disallowed_tools: Vec<String>,
    /// Trusted local host narrowing; inherited by every alias/nested context.
    pub(crate) acp_host: Option<codewhale_config::AppMode>,
    /// Captured child grant; retained through aliases and context overrides.
    pub(crate) child_host: Option<Arc<crate::tools::subagent::engine::ChildAuthority>>,
    /// Set only on the context of one call a person approved on a card.
    pub(crate) human_decision: Option<crate::core::engine::HumanDecision>,
    /// Shared shell manager for background tasks and streaming IO.
    pub shell_manager: SharedShellManager,
    /// Per-session snapshots for files successfully observed by `read_file`.
    /// Mutation tools use this to reject narrow edits against unread or stale
    /// content.
    pub file_read_tracker: SharedFileReadTracker,
    /// Sub-agent that owns tool work started through this context. Root user
    /// turns leave this unset; child contexts stamp it so long-running shell
    /// jobs can be attributed in UI surfaces.
    pub owner_agent_id: Option<String>,
    pub owner_agent_name: Option<String>,
    /// Tool call and engine turn that created long-running work through this
    /// context. Hosts use these stable identities to reconcile later updates
    /// with the originating transcript position.
    pub(crate) origin_tool_call_id: Option<String>,
    pub(crate) origin_turn_id: Option<String>,
    /// Outer process authority cap installed by Fleet/headless dispatch.
    /// `None` for ordinary interactive/root sessions.
    pub(crate) tool_authority: Option<Arc<ToolAuthorityEnvelope>>,
    /// Whether to allow paths outside workspace
    pub trust_mode: bool,
    /// Current sandbox policy
    #[expect(dead_code)]
    pub sandbox_policy: SandboxPolicy,
    /// Path for notes file
    pub notes_path: PathBuf,
    /// MCP configuration path
    #[expect(dead_code)]
    pub mcp_config_path: PathBuf,
    /// Explicit skills directory used for model-visible skill discovery.
    pub skills_dir: Option<PathBuf>,
    /// Restrict skill discovery to CodeWhale-owned roots plus `skills_dir`.
    pub skills_discovery_mode: crate::skills::SkillDiscoveryMode,
    /// Immutable registry snapshot for this workspace/engine context.
    pub plugin_registry: Option<Arc<crate::plugins::PluginRegistry>>,
    /// Elevated sandbox policy override (used when retrying after sandbox denial).
    /// This overrides the default sandbox behavior for shell commands.
    pub elevated_sandbox_policy: Option<crate::sandbox::SandboxPolicy>,
    /// Whether the enclosing host is the real headless `codewhale exec`
    /// process. `persist:true` background services are only permitted there;
    /// interactive TUI, desktop/app-server, and hosted runtime-thread engines
    /// leave this false so the feature fails closed.
    pub persist_services_enabled: bool,
    /// Optional user-facing hint for shell commands that fail because the
    /// active sandbox policy intentionally denies outbound network access.
    pub shell_network_denied_hint: Option<String>,
    /// Whether tools should auto-approve without safety checks (YOLO mode).
    /// When true, command safety analysis is skipped for shell execution.
    pub auto_approve: bool,
    /// Effective approval posture for this execution context. A turn stamps the
    /// posture it resolved here; a context built from the legacy bit alone
    /// folds it with [`crate::core::authority::posture_from_auto_approve`].
    /// Tools that create work of their own (a durable task) pin it, so the work
    /// inherits the authority the caller was granted rather than re-deriving
    /// one from a legacy bit.
    pub approval_mode: ApprovalMode,
    /// Effective shell policy for this execution context.
    pub shell_policy: ShellPolicy,
    /// Effective feature flag set for the running session.
    pub features: Features,
    /// Namespace for tool state that should be scoped to the current session/thread.
    pub state_namespace: String,
    /// Effective context window for the active provider/model route. Web tools
    /// use this to keep inline page content below three percent of the route.
    pub route_context_window: Option<u32>,
    /// User-trusted external paths the agent may read/write even when they
    /// fall outside `workspace`. Loaded from `~/.deepseek/workspace-trust.json`
    /// and refreshed when the user runs `/trust add <path>`. Distinct from
    /// `trust_mode`, which is the all-or-nothing legacy switch (#29).
    pub trusted_external_paths: Vec<PathBuf>,
    /// Whether to follow symbolic links during file discovery and tool
    /// operations. When `true`, symlinked directories are traversed and
    /// symlinked paths that resolve outside the workspace are still allowed
    /// (the symlink itself must be inside the workspace). Mirrors the
    /// `workspace_follow_symlinks` setting.
    pub follow_symlinks: bool,
    /// Per-domain network policy (#135). When `None`, network tools fall back
    /// to a permissive default that mirrors pre-v0.7.0 behavior so tests and
    /// other contexts that don't construct a real policy keep working.
    pub network_policy: Option<NetworkPolicyDecider>,
    /// Durable runtime services for task, gate, PR-attempt, GitHub evidence,
    /// and automation tools.
    pub runtime: RuntimeToolServices,
    /// Snapshot of the active prompt/session/history exposed as symbolic RLM
    /// objects. Tools only receive compact cards unless explicitly opening a
    /// bounded object through `rlm_open`.
    pub session_objects: Option<SessionObjectSnapshot>,
    /// Cancellation token for the active engine turn. Tools that may wait on
    /// external work should observe this so UI cancel can interrupt them.
    pub cancel_token: Option<CancellationToken>,
    /// Absolute deadline inherited from the active Engine turn after approval.
    /// Nested model/code work may narrow this bound but must never reset it.
    pub(crate) turn_deadline: Option<tokio::time::Instant>,
    /// Optional external sandbox backend for shell execution.
    /// When set, exec_shell routes commands through this instead of spawning
    /// a local process.
    pub sandbox_backend: Option<std::sync::Arc<dyn SandboxBackend>>,
    /// Path to the user memory file. `None` when the user-memory feature
    /// (#489) is disabled — tools that read or write the file should
    /// short-circuit on `None` rather than fall back to a workspace-local
    /// default.
    pub memory_path: Option<PathBuf>,
    /// LSP manager for post-edit diagnostics injection (#428). `None` when
    /// LSP is disabled or the context is constructed in a test that does not
    /// need diagnostics. Edit tools append a `<diagnostics>` block to their
    /// result when this is present and the manager is enabled.
    pub lsp_manager: Option<Arc<LspManager>>,

    /// Adaptive evidence router (#4619). Consulted only when
    /// `CODEWHALE_ADAPTIVE_OUTPUT_ROUTING` opts the process in; under the
    /// default classic lane this field is inert and bounding happens at the
    /// engine/subagent completion boundary. `None` in sub-agents and test
    /// contexts.
    pub large_output_router: Option<crate::tools::large_output_router::LargeOutputRouter>,

    /// Which search backend `web_search` should use. Default: Firecrawl. Set via
    /// `[search] provider` in config.toml.
    pub search_provider: crate::config::SearchProvider,
    /// Optional Firecrawl key, or required key for other API search providers.
    /// Metaso also falls back to the `METASO_API_KEY` env var.
    /// Baidu also falls back to `BAIDU_SEARCH_API_KEY`.
    pub search_api_key: Option<String>,
    /// Optional DuckDuckGo-compatible HTML endpoint override for `web_search`.
    pub search_base_url: Option<String>,
    /// Opaque client for the active route's documented first-party search
    /// tool. It owns provider authentication internally and is attached only
    /// when the exact route capability says server-side search is supported.
    pub(crate) provider_native_search: Option<crate::client::ProviderNativeSearchClient>,
    /// Exact active route capability facts. Unknown stays fail-closed.
    pub(crate) route_capabilities: codewhale_config::route::RouteCapabilities,
    /// Engine-served gate for calls nested inside an `execute_tools` program.
    /// Set only on the context of one `execute_tools` call by the turn loop;
    /// every nested call is planned and approved through the same gate a
    /// direct call gets. `None` everywhere else (sub-agents, tests, exec
    /// hosts without an engine turn), where code mode keeps its read-only,
    /// auto-approved profile.
    pub(crate) nested_call_gate: Option<crate::tools::codemode::NestedCallGate>,
    /// Where the session's live permission posture lives. Set by the engine;
    /// every agent call re-reads it (`None` keeps the posture above as is).
    pub(crate) live_posture: Option<crate::core::engine::LivePosture>,
    pub(crate) browser_tool: Option<String>,
}

impl std::ops::Deref for ToolContext {
    type Target = ToolExecutionState;

    fn deref(&self) -> &Self::Target {
        &self.execution
    }
}

impl std::ops::DerefMut for ToolContext {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.execution
    }
}

impl ToolContext {
    /// Create an inert context for a registry that intentionally has no tools.
    ///
    /// Empty paths are deliberate: isolated Runtime Chat must not retain the
    /// host workspace or derive project notes and MCP paths. Isolation comes
    /// from callers pairing this context with an empty registry and allow-list
    /// plus a zero tool-call budget; this constructor is not a security
    /// boundary by itself.
    #[must_use]
    pub(crate) fn for_empty_registry() -> Self {
        Self::with_options(PathBuf::new(), false, PathBuf::new(), PathBuf::new())
    }

    /// Create a new `ToolContext` with default settings.
    #[must_use]
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace = workspace.into();
        // Prefer .codewhale, fall back to .deepseek for project-local state
        let notes_path = codewhale_config::resolve_project_state_dir(&workspace, "notes.md")
            .expect("hardcoded project notes state path is valid")
            .1;
        let mcp_config_path = codewhale_config::resolve_project_state_dir(&workspace, "mcp.json")
            .expect("hardcoded project MCP state path is valid")
            .1;
        Self::with_options(workspace, false, notes_path, mcp_config_path)
    }

    /// Create a `ToolContext` with all settings specified.
    pub fn with_options(
        workspace: impl Into<PathBuf>,
        trust_mode: bool,
        notes_path: impl Into<PathBuf>,
        mcp_config_path: impl Into<PathBuf>,
    ) -> Self {
        let workspace = workspace.into();
        let shell_manager = new_shared_shell_manager(workspace.clone());
        let tool_authority = process_tool_authority();
        let shell_policy = match tool_authority.as_deref() {
            Some(cap) => cap.shell.shell_policy(),
            None => ShellPolicy::Full,
        };
        Self {
            workspace,
            execution: Box::new(ToolExecutionState {
                rlm_caller: None,
                disallowed_tools: Vec::new(),
                acp_host: None,
                child_host: None,
                human_decision: None,
                shell_manager,
                file_read_tracker: new_shared_file_read_tracker(),
                owner_agent_id: None,
                owner_agent_name: None,
                origin_tool_call_id: None,
                origin_turn_id: None,
                tool_authority,
                trust_mode,
                sandbox_policy: SandboxPolicy::None,
                notes_path: notes_path.into(),
                mcp_config_path: mcp_config_path.into(),
                skills_dir: None,
                skills_discovery_mode: crate::skills::SkillDiscoveryMode::Compatible,
                plugin_registry: None,
                elevated_sandbox_policy: None,
                persist_services_enabled: false,
                shell_network_denied_hint: None,
                auto_approve: false,
                approval_mode: ApprovalMode::Suggest,
                shell_policy,
                features: Features::with_defaults(),
                state_namespace: "workspace".to_string(),
                route_context_window: None,
                trusted_external_paths: Vec::new(),
                follow_symlinks: false,
                network_policy: None,
                runtime: RuntimeToolServices::default(),
                session_objects: None,
                cancel_token: None,
                turn_deadline: None,
                sandbox_backend: None,
                memory_path: None,
                lsp_manager: None,
                large_output_router: None,
                search_provider: crate::config::SearchProvider::default(),
                search_api_key: None,
                search_base_url: None,
                provider_native_search: None,
                route_capabilities: codewhale_config::route::RouteCapabilities::default(),
                nested_call_gate: None,
                live_posture: None,
                browser_tool: None,
            }),
        }
    }

    /// Create a `ToolContext` with auto-approve mode (YOLO).
    pub fn with_auto_approve(
        workspace: impl Into<PathBuf>,
        trust_mode: bool,
        notes_path: impl Into<PathBuf>,
        mcp_config_path: impl Into<PathBuf>,
        auto_approve: bool,
    ) -> Self {
        let mut context = Self::with_options(workspace, trust_mode, notes_path, mcp_config_path);
        context.auto_approve = auto_approve;
        // The bit stands for a posture, so fold it here rather than leaving the
        // two fields to disagree. A turn-level builder overwrites this with the
        // session's resolved posture, which is the authority that counts.
        context.approval_mode = crate::core::authority::posture_from_auto_approve(auto_approve);
        context
    }

    /// Attach a per-domain network policy to this context (#135).
    #[must_use]
    pub fn with_network_policy(mut self, policy: NetworkPolicyDecider) -> Self {
        self.network_policy = Some(policy);
        self
    }

    /// Attach durable runtime services to tools.
    #[must_use]
    pub fn with_runtime_services(mut self, runtime: RuntimeToolServices) -> Self {
        self.runtime = runtime;
        self
    }

    /// Re-read the session's live permission posture (see `live_posture`) and
    /// return what was read; `None` when this context has no live source.
    pub(crate) fn refresh_live_posture(
        &mut self,
    ) -> Option<crate::core::engine::LiveRuntimeAuthority> {
        let live = self.live_posture.clone()?;
        let authority = live.apply(self);
        if let Some(child) = self.child_host.as_ref() {
            self.shell_policy = self.shell_policy.min_with(child.grant.shell_policy());
        }
        Some(authority)
    }

    /// Stamp tool work with the sub-agent that owns it.
    #[must_use]
    pub fn with_owner_agent(
        mut self,
        agent_id: impl Into<String>,
        agent_name: impl Into<String>,
    ) -> Self {
        let agent_id = agent_id.into();
        let agent_name = agent_name.into();
        self.owner_agent_id = (!agent_id.trim().is_empty()).then_some(agent_id);
        self.owner_agent_name = (!agent_name.trim().is_empty()).then_some(agent_name);
        self
    }

    /// Bind long-running work to the engine turn that created it.
    #[must_use]
    pub(crate) fn with_origin_turn_id(mut self, turn_id: impl Into<String>) -> Self {
        self.origin_turn_id = Some(turn_id.into());
        self
    }

    /// Bind long-running work to the tool call that created it.
    #[must_use]
    pub(crate) fn with_origin_tool_call_id(mut self, tool_call_id: impl Into<String>) -> Self {
        self.origin_tool_call_id = Some(tool_call_id.into());
        self
    }

    #[cfg(test)]
    pub(crate) fn with_tool_authority(
        mut self,
        envelope: ToolAuthorityEnvelope,
    ) -> Result<Self, String> {
        let envelope = envelope.normalized()?;
        if let Some(outer) = self.tool_authority.as_ref()
            && !envelope.is_within(outer)
        {
            return Err(
                "nested tool authority cannot expand its process authority cap".to_string(),
            );
        }
        self.tool_authority = Some(Arc::new(envelope));
        self.shell_policy = self.authority_clamped_shell_policy(self.shell_policy);
        Ok(self)
    }

    /// Attach skill discovery settings for tools that need to resolve
    /// model-visible skills by name.
    #[must_use]
    pub fn with_skills_config(
        mut self,
        skills_dir: impl Into<PathBuf>,
        discovery_mode: crate::skills::SkillDiscoveryMode,
    ) -> Self {
        self.skills_dir = Some(skills_dir.into());
        self.skills_discovery_mode = discovery_mode;
        self
    }

    #[must_use]
    pub fn with_plugin_registry(mut self, registry: Arc<crate::plugins::PluginRegistry>) -> Self {
        self.plugin_registry = Some(registry);
        self
    }

    /// Attach active prompt/history/session symbolic objects for RLM tools.
    #[must_use]
    pub fn with_session_objects(mut self, snapshot: SessionObjectSnapshot) -> Self {
        self.session_objects = Some(snapshot);
        self
    }

    /// Attach the active engine cancellation token.
    #[must_use]
    pub fn with_cancel_token(mut self, cancel_token: CancellationToken) -> Self {
        self.cancel_token = Some(cancel_token);
        self
    }

    /// Attach the effective shell policy for this turn.
    #[must_use]
    pub fn with_shell_policy(mut self, policy: ShellPolicy) -> Self {
        self.shell_policy = self.authority_clamped_shell_policy(policy);
        self
    }

    /// Replace the turn shell policy while retaining the process authority as
    /// an outer ceiling. Live mode changes rebuild this value on every tool
    /// call, so the clamp belongs here rather than only at engine startup.
    pub(crate) fn set_shell_policy(&mut self, policy: ShellPolicy) {
        self.shell_policy = self.authority_clamped_shell_policy(policy);
    }

    fn authority_clamped_shell_policy(&self, policy: ShellPolicy) -> ShellPolicy {
        let policy = match self.tool_authority.as_deref() {
            Some(cap) => policy.min_with(cap.shell.shell_policy()),
            None => policy,
        };
        self.child_host
            .as_ref()
            .map_or(policy, |child| policy.min_with(child.grant.shell_policy()))
    }

    /// Attach an external sandbox backend for remote shell execution.
    #[must_use]
    pub fn with_sandbox_backend(mut self, backend: std::sync::Arc<dyn SandboxBackend>) -> Self {
        self.sandbox_backend = Some(backend);
        self
    }

    /// Set the user's trusted external paths (loaded from
    /// `~/.deepseek/workspace-trust.json`). See [`Self::resolve_path`] for
    /// how the list is consulted.
    #[must_use]
    pub fn with_trusted_external_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.trusted_external_paths = paths;
        self
    }

    /// Set whether tools should follow symbolic links. When `true`,
    /// `resolve_path` allows symlinked paths that resolve outside the
    /// workspace, and walk-based tools traverse symlinked directories.
    /// Mirrors the `workspace_follow_symlinks` setting.
    #[must_use]
    pub fn with_follow_symlinks(mut self, follow: bool) -> Self {
        self.follow_symlinks = follow;
        self
    }

    /// Attach an LSP manager so that edit tools can auto-inject diagnostics
    /// into their results after a successful file modification (#428).
    #[must_use]
    #[cfg(test)]
    pub fn with_lsp_manager(mut self, manager: Arc<LspManager>) -> Self {
        self.lsp_manager = Some(manager);
        self
    }

    /// Remember that the caller has observed the current on-disk state of a
    /// file. This is intentionally best-effort so successful reads/writes do
    /// not fail after completing only because a post-operation metadata lookup
    /// raced with filesystem changes.
    pub fn note_file_read(&self, path: &Path) {
        let Ok(snapshot) = file_read_snapshot(path) else {
            return;
        };
        let Ok(mut tracker) = self.file_read_tracker.lock() else {
            return;
        };
        tracker.reads.insert(path.to_path_buf(), snapshot);
    }

    /// Distinct files read under `dir` so far this session. Echolocation's
    /// hunt-phase sensor: 2+ tracked reads means the model is working this
    /// pod and the next read there enters the terminal buzz. Lock
    /// contention degrades to zero (sparse sounding), never an error.
    pub(crate) fn pod_visit_count(&self, dir: &Path) -> usize {
        self.file_read_tracker
            .lock()
            .map(|tracker| {
                tracker
                    .reads
                    .keys()
                    .filter(|path| path.parent() == Some(dir))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Require a successful, still-fresh `read_file` snapshot before a narrow
    /// in-place edit. This catches model edits made against guessed or stale
    /// content while leaving transactional patch preflight separate.
    pub fn require_fresh_file_read(
        &self,
        path: &Path,
        requested_path: &str,
    ) -> Result<(), ToolError> {
        self.require_fresh_read_for(
            path,
            requested_path,
            "File action=\"edit\"",
            "File with action=\"read\"",
            "File action=\"read\"",
        )
    }

    pub fn require_fresh_read_before_write(
        &self,
        path: &Path,
        requested_path: &str,
    ) -> Result<(), ToolError> {
        self.require_fresh_read_for(path, requested_path, "write", "read", "read")
    }

    fn require_fresh_read_for(
        &self,
        path: &Path,
        requested_path: &str,
        mutation: &str,
        read_call: &str,
        read: &str,
    ) -> Result<(), ToolError> {
        let prior = {
            let tracker = self.file_read_tracker.lock().map_err(|_| {
                ToolError::execution_failed(
                    "Failed to check read-before-edit state: tracker lock poisoned".to_string(),
                )
            })?;
            tracker.reads.get(path).cloned()
        };

        let Some(prior) = prior else {
            return Err(ToolError::execution_failed(format!(
                "Refusing {mutation} for {} because it has not been read in this session. \
                 Recovery: call {read_call} path=\"{requested_path}\" to inspect the current contents, \
                 then retry {mutation}.",
                path.display()
            )));
        };

        let current = file_read_snapshot(path).map_err(|e| {
            ToolError::execution_failed(format!(
                "Refusing {mutation} for {} because the file could not be checked for staleness ({e}). \
                 Recovery: call {read_call} path=\"{requested_path}\" again, then retry {mutation}.",
                path.display()
            ))
        })?;

        if current != prior {
            return Err(ToolError::execution_failed(format!(
                "Refusing {mutation} for {} because it changed since the last {read} call. \
                 Recovery: call {read_call} path=\"{requested_path}\" again and retry with the current contents.",
                path.display()
            )));
        }

        Ok(())
    }

    /// Cap the authority a tool call asks for on work it hands off (a durable
    /// task or a scheduled automation) at what this session holds. Requested
    /// `allow_shell`, `trust_mode` and `auto_approve` bits are declarations
    /// from the model; each survives only when this session already has that
    /// authority, so delegated work never runs with more than its creator.
    pub(crate) fn cap_delegated_authority(
        &self,
        allow_shell: Option<bool>,
        trust_mode: Option<bool>,
        auto_approve: Option<bool>,
    ) -> (Option<bool>, Option<bool>, Option<bool>) {
        let holds_shell = self.shell_policy == ShellPolicy::Full;
        (
            // An omitted flag falls back to the host's configured default, so
            // a session without full shell must say "no" for it rather than
            // leave it unset.
            match allow_shell {
                Some(requested) => Some(requested && holds_shell),
                None if holds_shell => None,
                None => Some(false),
            },
            trust_mode.map(|requested| requested && self.trust_mode),
            auto_approve.map(|requested| requested && self.approval_mode == ApprovalMode::Bypass),
        )
    }

    /// Resolve a path relative to workspace, validating it doesn't escape.
    ///
    /// This handles both existing files (using canonicalize) and non-existent files
    /// (for write operations) by canonicalizing the parent directory and appending
    /// the filename.
    /// Resolve a path relative to workspace, validating it doesn't escape.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// # use crate::tools::spec::ToolContext;
    /// let ctx = ToolContext::new(".");
    /// let path = ctx.resolve_path("README.md")?;
    /// # Ok::<(), crate::tools::spec::ToolError>(())
    /// ```
    pub fn resolve_path(&self, raw: &str) -> Result<PathBuf, ToolError> {
        let candidate = if let Some(home_path) = resolve_home_path(raw)? {
            home_path
        } else if std::path::Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            self.workspace.join(raw)
        };

        // In trust mode, allow any path without validation
        if self.trust_mode {
            // Still try to canonicalize for consistency, but don't require it
            return Ok(candidate.canonicalize().unwrap_or(candidate));
        }

        // Try to canonicalize the workspace
        let workspace_canonical = self
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| self.workspace.clone());

        // When follow_symlinks is enabled, check the non-canonical (symlink)
        // path against the workspace first. A symlink inside the workspace
        // that resolves outside is allowed — the symlink itself is the gate.
        if self.follow_symlinks {
            let candidate_normalized = normalize_path(&candidate);
            let workspace_normalized = normalize_path(&self.workspace);
            let workspace_canonical_normalized = normalize_path(&workspace_canonical);

            if candidate_normalized.starts_with(&workspace_normalized)
                || candidate_normalized.starts_with(&workspace_canonical_normalized)
            {
                // The symlink (or plain path) is inside the workspace.
                // Return the canonicalized target so file I/O works correctly.
                if candidate.exists() {
                    return Ok(candidate.canonicalize().unwrap_or(candidate));
                }
                // Non-existent path: canonicalize the deepest existing ancestor
                return self.resolve_nonexistent_path(candidate, &workspace_canonical);
            }

            // Path is outside workspace even before resolving symlinks.
            // Fall through to the standard escape check.
        }

        // For the initial check, also try to canonicalize the candidate if possible
        // This handles symlinks like /var -> /private/var on macOS
        let candidate_canonical = candidate
            .canonicalize()
            .unwrap_or_else(|_| normalize_path(&candidate));
        let workspace_normalized = normalize_path(&workspace_canonical);

        // Check if the candidate is under the workspace (comparing canonical paths)
        if !candidate_canonical.starts_with(&workspace_normalized) {
            // Also try with non-canonical workspace for cases where workspace itself
            // hasn't been canonicalized yet
            let workspace_plain = normalize_path(&self.workspace);
            let candidate_normalized = normalize_path(&candidate);
            if !candidate_normalized.starts_with(&workspace_plain)
                && !self.is_trusted_external_path(&candidate_canonical)
                && !self.is_trusted_external_path(&candidate_normalized)
            {
                return Err(ToolError::PathEscape {
                    path: candidate_canonical,
                });
            }
        }

        // For existing paths, use canonicalize directly
        if candidate.exists() {
            let canonical = candidate.canonicalize().map_err(|e| {
                ToolError::execution_failed(format!(
                    "Failed to canonicalize {}: {}",
                    candidate.display(),
                    e
                ))
            })?;

            if !canonical.starts_with(&workspace_canonical)
                && !self.is_trusted_external_path(&canonical)
            {
                return Err(ToolError::PathEscape { path: canonical });
            }

            return Ok(canonical);
        }

        self.resolve_nonexistent_path(candidate, &workspace_canonical)
    }

    /// Resolve `raw` against the workspace and require an existing directory.
    ///
    /// Tools that scope execution to a subdirectory (Run `cwd`) resolve
    /// through here so containment, existence, and the refusal wording have
    /// one owner. A workspace escape keeps the typed `PathEscape`; a missing
    /// or non-directory path names the fallback (drop the field to run in
    /// the workspace root).
    pub fn resolve_existing_dir(&self, raw: &str, field: &str) -> Result<PathBuf, ToolError> {
        let resolved = self.resolve_path(raw)?;
        if resolved.is_dir() {
            Ok(resolved)
        } else {
            Err(ToolError::invalid_input(format!(
                "{field} '{raw}' is not an existing directory inside the workspace; drop `{field}` to run in the workspace root"
            )))
        }
    }

    /// Resolve a non-existent path by canonicalizing its deepest existing
    /// ancestor and validating the result is under the workspace or a
    /// trusted external path.
    fn resolve_nonexistent_path(
        &self,
        candidate: PathBuf,
        workspace_canonical: &Path,
    ) -> Result<PathBuf, ToolError> {
        let workspace_normalized = normalize_path(workspace_canonical);
        let workspace_plain = normalize_path(&self.workspace);
        let mut existing_ancestor = candidate.clone();
        let mut suffix_parts: Vec<std::ffi::OsString> = Vec::new();

        while !existing_ancestor.exists() {
            if let Some(file_name) = existing_ancestor.file_name() {
                suffix_parts.push(file_name.to_owned());
            }
            match existing_ancestor.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => {
                    existing_ancestor = parent.to_path_buf();
                }
                _ => {
                    // No existing parent found; fall back to simple check
                    break;
                }
            }
        }
        let ancestor_normalized = normalize_path(&existing_ancestor);

        let canonical_ancestor = if existing_ancestor.exists() {
            existing_ancestor
                .canonicalize()
                .unwrap_or(existing_ancestor)
        } else {
            existing_ancestor
        };

        // Rebuild the full path from canonicalized ancestor
        let mut canonical = canonical_ancestor;
        for part in suffix_parts.into_iter().rev() {
            canonical.push(part);
        }
        let canonical = normalize_path(&canonical);

        if self.follow_symlinks
            && (ancestor_normalized.starts_with(&workspace_plain)
                || ancestor_normalized.starts_with(&workspace_normalized))
        {
            return Ok(canonical);
        }

        // Validate it's under workspace, OR is under a user-trusted external
        // path (`/trust add <path>` from the slash command, persisted in
        // `~/.deepseek/workspace-trust.json`).
        if !canonical.starts_with(workspace_canonical)
            && !canonical.starts_with(&workspace_normalized)
            && !self.is_trusted_external_path(&canonical)
        {
            return Err(ToolError::PathEscape { path: canonical });
        }

        Ok(canonical)
    }

    /// Whether `path` is under any of the user-trusted external roots. The
    /// caller should pass an already-canonicalized (or normalized) path.
    fn is_trusted_external_path(&self, path: &Path) -> bool {
        self.trusted_external_paths
            .iter()
            .any(|trusted| path.starts_with(trusted))
    }

    /// Set the trust mode.
    #[cfg(test)]
    pub fn with_trust_mode(mut self, trust: bool) -> Self {
        self.trust_mode = trust;
        self
    }

    /// Set feature flags for tool execution.
    pub fn with_features(mut self, features: Features) -> Self {
        self.features = features;
        self
    }

    /// Override the shared shell manager.
    pub fn with_shell_manager(mut self, shell_manager: SharedShellManager) -> Self {
        self.shell_manager = shell_manager;
        self
    }

    /// Reuse the engine's session-scoped read snapshots across tool-context
    /// rebuilds. A fresh context is assembled for each turn, but successful
    /// reads must remain authoritative until the observed file changes.
    pub fn with_file_read_tracker(mut self, tracker: SharedFileReadTracker) -> Self {
        self.file_read_tracker = tracker;
        self
    }

    /// Set the elevated sandbox policy override.
    ///
    /// This is used when retrying a tool after a sandbox denial, to run
    /// with elevated permissions.
    pub fn with_elevated_sandbox_policy(mut self, policy: crate::sandbox::SandboxPolicy) -> Self {
        self.elevated_sandbox_policy = Some(policy);
        self
    }

    /// Carry a person's card decision to the one call it approved.
    #[must_use]
    pub(crate) fn with_human_decision(
        mut self,
        decision: crate::core::engine::HumanDecision,
    ) -> Self {
        self.human_decision = Some(decision);
        self
    }

    /// Set the shell network-denial hint used by network-restricted modes.
    pub fn with_shell_network_denied_hint(mut self, hint: impl Into<String>) -> Self {
        self.shell_network_denied_hint = Some(hint.into());
        self
    }

    /// Set the namespace used for session-scoped tool state.
    pub fn with_state_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.state_namespace = namespace.into();
        self
    }

    /// Attach the active route's effective context window.
    #[must_use]
    pub fn with_route_context_window(mut self, context_window: u32) -> Self {
        self.route_context_window = (context_window > 0).then_some(context_window);
        self
    }

    /// Attach the adaptive evidence router (#4619). Consulted only under the
    /// `CODEWHALE_ADAPTIVE_OUTPUT_ROUTING` opt-in.
    #[must_use]
    pub fn with_large_output_router(
        mut self,
        router: crate::tools::large_output_router::LargeOutputRouter,
    ) -> Self {
        self.large_output_router = Some(router);
        self
    }
}

/// Gather LSP diagnostics for `paths` using the manager stored in `context`,
/// and return the rendered `<diagnostics …>` blocks joined by newlines.
///
/// Returns an empty string when:
/// - `context.lsp_manager` is `None`
/// - the manager's `enabled` flag is `false`
/// - none of the files produce diagnostics (e.g. all clean, or language unknown)
///
/// This function is non-blocking by design: every failure mode (missing LSP
/// binary, timeout, unknown language) degrades to an empty string rather than
/// propagating an error to the caller.
pub async fn lsp_diagnostics_for_paths(context: &ToolContext, paths: &[PathBuf]) -> String {
    use crate::lsp::render_blocks;

    let manager = match context.lsp_manager.as_ref() {
        Some(m) if m.config().enabled => m,
        _ => return String::new(),
    };

    let mut blocks = Vec::new();
    for (idx, path) in paths.iter().enumerate() {
        if let Some(block) = manager.diagnostics_for(path, idx as u64).await {
            blocks.push(block);
        }
    }

    render_blocks(&blocks)
}

pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    let mut prefix: Option<std::ffi::OsString> = None;
    let mut is_root = false;
    let mut stack: Vec<std::ffi::OsString> = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix_component) => {
                prefix = Some(prefix_component.as_os_str().to_owned());
            }
            Component::RootDir => {
                is_root = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let parent = Component::ParentDir.as_os_str();
                if let Some(last) = stack.pop() {
                    if last == parent {
                        stack.push(last);
                        stack.push(parent.to_owned());
                    }
                } else if !is_root {
                    stack.push(parent.to_owned());
                }
            }
            Component::Normal(part) => {
                stack.push(part.to_owned());
            }
        }
    }

    let mut normalized = PathBuf::new();
    if let Some(prefix) = prefix {
        normalized.push(prefix);
    }
    if is_root {
        normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR));
    }
    for part in stack {
        normalized.push(part);
    }
    normalized
}

/// Resolve an exact `~` or `~/` path prefix to the current user's home directory.
///
/// Only exact `~` and `~/` (or `~\` on Windows) prefixes are resolved. Prefixes like
/// `~otheruser`, shell variables (`$VAR`), command substitution, and globs are not
/// expanded. Literal paths like `./~/file` stay literal.
///
/// Returns:
/// - `Ok(Some(path))` if `raw` has an exact home prefix and home was determined.
/// - `Ok(None)` if `raw` does not have an exact home prefix.
/// - `Err(ToolError)` if `raw` has an exact home prefix but user home could not be determined.
pub(crate) fn resolve_home_path(raw: &str) -> Result<Option<PathBuf>, ToolError> {
    resolve_home_path_with(raw, crate::config::effective_home_dir)
}

pub(crate) fn resolve_home_path_with(
    raw: &str,
    home_lookup: impl FnOnce() -> Option<PathBuf>,
) -> Result<Option<PathBuf>, ToolError> {
    let suffix = if raw == "~" {
        ""
    } else if let Some(rest) = raw.strip_prefix("~/") {
        rest.trim_start_matches(|c| c == '/' || (cfg!(windows) && c == '\\'))
    } else {
        #[cfg(windows)]
        if let Some(rest) = raw.strip_prefix(r"~\") {
            rest.trim_start_matches(['/', '\\'])
        } else {
            return Ok(None);
        }
        #[cfg(not(windows))]
        return Ok(None);
    };

    // A drive prefix is not a home-relative suffix. `PathBuf::join` would
    // otherwise replace the home on Windows (for example `~/C:\file`).
    #[cfg(windows)]
    if Path::new(suffix)
        .components()
        .any(|part| matches!(part, std::path::Component::Prefix(_)))
    {
        return Err(ToolError::invalid_input(
            "a home-relative path cannot contain a drive prefix",
        ));
    }
    let home = home_lookup().ok_or_else(|| {
        ToolError::execution_failed(format!(
            "Failed to resolve path '{raw}': user home directory could not be determined"
        ))
    })?;

    if suffix.is_empty() {
        Ok(Some(home))
    } else {
        Ok(Some(home.join(suffix)))
    }
}

/// The core trait that all tools must implement.
#[async_trait]
pub trait ToolSpec: Send + Sync {
    /// Returns the unique name of this tool (used in API calls).
    fn name(&self) -> &str;

    /// Identifies the implementation in local registration diagnostics only.
    /// Adapters should use their existing source identity, never credentials,
    /// command arguments, descriptions, or other execution payloads.
    fn registration_origin(&self) -> std::borrow::Cow<'_, str> {
        std::any::type_name::<Self>().into()
    }

    /// Returns a human-readable description of what this tool does.
    fn description(&self) -> &str;

    /// Returns the JSON Schema for the tool's input parameters.
    fn input_schema(&self) -> Value;

    /// Returns the capabilities this tool has.
    fn capabilities(&self) -> Vec<ToolCapability>;

    /// Returns the approval requirement for this tool.
    fn approval_requirement(&self) -> ApprovalRequirement {
        let caps = self.capabilities();
        if caps.contains(&ToolCapability::ExecutesCode) {
            ApprovalRequirement::Required
        } else if caps.contains(&ToolCapability::WritesFiles) {
            ApprovalRequirement::Suggest
        } else {
            ApprovalRequirement::Auto
        }
    }

    /// Returns the approval requirement for this concrete tool input.
    fn approval_requirement_for(&self, _input: &Value) -> ApprovalRequirement {
        self.approval_requirement()
    }

    /// Returns whether this tool is sandboxable.
    #[cfg(test)]
    fn is_sandboxable(&self) -> bool {
        self.capabilities().contains(&ToolCapability::Sandboxable)
    }

    /// Returns whether this tool is read-only.
    fn is_read_only(&self) -> bool {
        let caps = self.capabilities();
        caps.contains(&ToolCapability::ReadOnly)
            && !caps.contains(&ToolCapability::WritesFiles)
            && !caps.contains(&ToolCapability::ExecutesCode)
    }

    /// Returns whether this concrete tool input is read-only.
    fn is_read_only_for(&self, _input: &Value) -> bool {
        self.is_read_only()
    }

    /// Returns whether this tool can be executed in parallel with others.
    fn supports_parallel(&self) -> bool {
        false
    }

    /// Returns whether this concrete tool input can run in parallel.
    fn supports_parallel_for(&self, _input: &Value) -> bool {
        self.supports_parallel()
    }

    /// Returns whether this input starts durable/detached work and returns
    /// immediately. Detached starts are not read-only, but in auto-approved
    /// turns they do not need to block neighboring read-only inspections.
    fn starts_detached_for(&self, _input: &Value) -> bool {
        false
    }

    fn refusal_before_approval(
        &self,
        _input: &Value,
        _context: &ToolContext,
    ) -> Option<ToolResult> {
        None
    }

    /// Resolve input-specific policy without performing external side effects.
    ///
    /// Resource claims deliberately default to global exclusivity until a
    /// first-party tool opts into narrower, canonicalized claims. The initial
    /// seam records this decision but leaves the existing scheduler unchanged.
    fn prepare(&self, input: Value, _context: &ToolContext) -> Result<PreparedToolCall, ToolError> {
        Ok(PreparedToolCall {
            name: self.name().to_string(),
            description: self.description().to_string(),
            read_only: self.is_read_only_for(&input),
            supports_parallel: self.supports_parallel_for(&input),
            starts_detached: self.starts_detached_for(&input),
            approval: self.approval_requirement_for(&input),
            resources: vec![ResourceClaim::GlobalExclusive],
            input,
        })
    }

    /// The approval-grant scope this tool's calls are keyed under instead of
    /// the name-derived key families, if it has one. `None` (every built-in,
    /// script and MCP tool) keeps [`crate::tools::approval_cache`]'s keys.
    ///
    /// Extension tools return `ext:<plugin_id>@<content_hash>`, so a session
    /// grant covers one reviewed plugin build: an updated plugin, or another
    /// plugin that later registers the same name, is asked again.
    fn approval_scope(&self) -> Option<String> {
        None
    }

    /// Who this tool is, if it is an extension tool: composed by Rust from its
    /// registration. `Some` makes the turn loop serve a permission gate for
    /// the tool's call, through which its `core/call`s are planned and
    /// approved like a model's, and makes the tool unreachable from any other
    /// extension's `core/call` (no recursion). `None` (every built-in, script
    /// and MCP tool) changes nothing.
    fn extension_caller(&self) -> Option<crate::tools::codemode::ExtensionCaller> {
        None
    }

    /// Returns whether this tool should be excluded from the model-visible
    /// tool catalog (deferred loading). Tools marked `true` are registered
    /// but not sent to the model until explicitly activated via tool search.
    fn defer_loading(&self) -> bool {
        false
    }

    /// Returns whether this tool should be advertised in the model-facing
    /// catalog. Hidden compatibility tools remain registered and executable
    /// by name so saved transcripts can replay without teaching new sessions
    /// the deprecated spelling.
    fn model_visible(&self) -> bool {
        true
    }

    /// Execute the tool with the given input and context.
    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError>;

    /// Execute with rich result blocks. Existing tools inherit text-only
    /// behavior; tools such as lowercase `read` can opt in without changing
    /// the published `ToolResult` struct.
    async fn execute_rich(
        &self,
        input: Value,
        context: &ToolContext,
    ) -> Result<RichToolResult, ToolError> {
        self.execute(input, context)
            .await
            .map(RichToolResult::plain)
    }
}

#[cfg(test)]
mod tests;

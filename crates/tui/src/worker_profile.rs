//! Worker runtime profile — the per-role capability contract for a CodeWhale
//! worker (#3217, #3211, #3213, and the child-permission-intersection issues
//! #414 / #426 / #1186).
//!
//! This is the **Workflow substrate**: every detached worker — whether launched
//! as an `agent` sub-agent or a Fleet worker — should run under a profile
//! that bounds what it may do (permissions, shell access, tool scope, model
//! route, recursion budget, foreground/background). A child profile is always
//! **derived** from its parent and can never escalate beyond it.
//!
//! Scope: this module defines the contract and the parent→child derivation with
//! tests. `agent` and Fleet worker records now build and persist these
//! profiles so parent-visible worker projections have a single capability
//! contract. Runtime enforcement of every declared field remains incremental
//! follow-up work (#3217).

use crate::fleet::role::FleetRole;
use serde::{Deserialize, Serialize};

/// Coarse capability classes a worker may exercise, beyond read access (reads
/// are always permitted). A child may only ever hold a *subset* of its parent's
/// capabilities.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionSet {
    /// May modify the workspace (`write_file` / `edit_file` / `apply_patch`).
    pub write: bool,
    /// May use network-capable tools (web search/fetch, networked MCP servers).
    pub network: bool,
}

impl PermissionSet {
    /// Full capabilities (write + network).
    pub const fn full() -> Self {
        Self {
            write: true,
            network: true,
        }
    }

    /// Read-only: no write, no network.
    pub const fn read_only() -> Self {
        Self {
            write: false,
            network: false,
        }
    }

    /// Read-only inspection: read-only on the workspace, but network-capable.
    ///
    /// The read-only investigator posture (scout/reviewer): it must not
    /// mutate the workspace, but real read-only inspection needs
    /// `git`/`gh`/web reach — the old `read_only()` default left such lanes
    /// with no way to run any command or reach any remote, which made default
    /// scout lanes useless for the inspection they exist for.
    pub const fn read_only_with_network() -> Self {
        Self {
            write: false,
            network: true,
        }
    }

    /// Intersection: a capability is granted only if **both** sets grant it.
    /// This is the core non-escalation primitive — `parent.intersect(child)`
    /// can never produce a capability the parent lacks.
    #[must_use]
    pub fn intersect(self, other: Self) -> Self {
        Self {
            write: self.write && other.write,
            network: self.network && other.network,
        }
    }
}

/// Shell access policy — the replacement for the legacy per-worker shell boolean
/// (#3217). Ordered from most to least restrictive so `min` yields the safer of
/// two policies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ShellPolicy {
    /// No shell access.
    None,
    /// Read-only / non-mutating commands only (the policy enforcement lives in
    /// the exec/sandbox layer; this is the declared intent).
    ReadOnly,
    /// Full shell access.
    Full,
}

impl ShellPolicy {
    /// Convert the legacy top-level shell opt-in into the typed shell policy.
    #[must_use]
    pub const fn from_legacy_allow_shell(allow_shell: bool) -> Self {
        if allow_shell { Self::Full } else { Self::None }
    }

    /// Whether any shell tools should be exposed under this policy.
    #[must_use]
    pub const fn allows_shell(self) -> bool {
        !matches!(self, Self::None)
    }

    /// The more restrictive (safer) of two policies. A child can never exceed
    /// its parent's shell policy.
    #[must_use]
    pub fn min_with(self, other: Self) -> Self {
        if self <= other { self } else { other }
    }
}

/// Which tools a worker may call. Mirrors the existing `AgentWorkerToolProfile`
/// (`Inherited` / `Explicit`) so the two can be reconciled when this is wired in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolScope {
    /// Inherit the parent's tool surface.
    Inherit,
    /// Only the explicitly listed tool names.
    Explicit(Vec<String>),
}

/// File-system authority axis of a [`ChildGrant`]. Ordered least to most
/// permissive so `Ord::min` is the non-escalation primitive.
///
/// `None` is not "no writes" — it is "no file access of any kind", which is
/// what a child with an empty tool surface experiences. `Read` permits
/// evidence collection only; `Write` permits mutation inside the session's
/// write policy (workspace boundary, exact-file claims, approval posture).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileGrant {
    None,
    Read,
    Write,
}

/// Process authority axis of a [`ChildGrant`]. Ordered least to most
/// permissive so `Ord::min` is the non-escalation primitive.
///
/// `Inspect` is the read-only shell: canonical `bash` only, and only calls
/// the agent read-only classifier proves mutation-free (the explore /
/// reviewer / planner posture). `Verify` is the bounded built-in verification
/// surface — default workspace checks, pure test selection, and bounded Git
/// fetch/merge-tree — with no shell grammar (the test/verifier posture). A
/// child asking for a surface its parent does not hold degrades to the
/// narrower side (`Inspect ∩ Verify = Inspect`): bounded inspection is a
/// subset of bounded process authority, never a widening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShellGrant {
    None,
    Inspect,
    Verify,
    Full,
}

/// The named tool surface a grant exposes, before the explicit scope narrows
/// it. This is the role-preset axis of `tools` — distinct from
/// [`ChildGrant::scope`], which is the caller's own allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSurface {
    /// Every registered tool the grant's other axes admit.
    Inherited,
    /// The read-only evidence surface — `readonly_evidence_tool` plus
    /// `agent` (delegation). The explore / reviewer preset.
    Evidence,
}

/// The single authority object a delegated child runs under (#5633).
///
/// Roles are *presets over this object*, never a second permission system:
/// [`ChildGrant::for_role`] gives the widest grant a role can hold, and
/// [`ChildGrant::resolve`] intersects it with the effective parent-derived
/// profile and the caller's explicit scope. One projection serves every
/// consumer — the child's tool catalog, its dispatch refusals, and its
/// capability envelope all read the same fields, so a tool that is visible
/// is callable and a tool that is denied never appears.
///
/// This is deliberately a *projection*, not a second persisted record:
/// [`WorkerRuntimeProfile`] remains the wire/persistence shape and
/// [`ChildAuthority`](crate::fleet::role::ChildAuthority) /
/// [`ToolAuthorityEnvelope`](crate::tools::spec::ToolAuthorityEnvelope)
/// remain the transports that carry the same authority across the durable
/// Fleet and `codewhale exec` subprocess boundaries. What this object retires
/// is the *semantic* duplication — posture sentinel entries read back off a
/// deny list, and role-keyed re-derivation inside the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildGrant {
    /// File-system authority.
    pub files: FileGrant,
    /// Process authority.
    pub shell: ShellGrant,
    /// May reach the network through any surface.
    pub network: bool,
    /// May drive the operator's machine (computer-use / desktop tools).
    /// Children never hold this today; the field is declared so every
    /// surface reports the same denial rather than implying a grant.
    pub desktop: bool,
    /// The named tool surface before explicit scope narrowing.
    pub surface: ToolSurface,
    /// The caller's explicit allowlist, already intersected with the parent's
    /// scope at spawn. `Some(vec![])` — the `tools = false` posture — permits
    /// nothing at all.
    pub scope: Option<Vec<String>>,
    /// May delegate children of its own (remaining spawn depth).
    pub spawn: bool,
}

impl ChildGrant {
    /// The widest grant a role can ever hold — the role preset.
    ///
    /// This is the role-to-authority table, expressed once: everything else
    /// only narrows it. Read-only roles stay read-only on the workspace by
    /// intent; network reach is a read, and a worker cut off from it for no
    /// role reason cannot do its job. `desktop` is never in any preset.
    #[must_use]
    pub fn for_role(role: &FleetRole) -> Self {
        let (files, shell, surface) = match role {
            // Read-only investigators: evidence collection only, with the
            // classifier-bounded inspection shell.
            FleetRole::Scout | FleetRole::Reviewer => {
                (FileGrant::Read, ShellGrant::Inspect, ToolSurface::Evidence)
            }
            // Planner: analysis only, same bounded shell, broader read
            // surface — a plan may consult any read the session offers.
            FleetRole::Planner => (FileGrant::Read, ShellGrant::Inspect, ToolSurface::Inherited),
            // Counsel only: reads to ground advice, never acts — no process
            // surface at all (#4752).
            FleetRole::Consultant => (FileGrant::Read, ShellGrant::None, ToolSurface::Inherited),
            // Verifier: bounded workspace checks and bounded Git reference
            // reads, no shell grammar.
            FleetRole::Verifier => (FileGrant::Read, ShellGrant::Verify, ToolSurface::Inherited),
            // Doers, and Custom: the preset asks for everything; the parent
            // profile and explicit scope do all the narrowing.
            FleetRole::Builder | FleetRole::Worker | FleetRole::Custom => {
                (FileGrant::Write, ShellGrant::Full, ToolSurface::Inherited)
            }
        };
        Self {
            files,
            shell,
            network: true,
            desktop: false,
            surface,
            scope: None,
            spawn: true,
        }
    }

    /// Resolve the grant a child actually runs under: the role preset
    /// intersected with the effective parent-derived profile, carrying the
    /// caller's explicit scope and the remaining delegation depth.
    ///
    /// Every field takes the more restrictive side, so the result can never
    /// name authority the parent lacked. This is the in-process counterpart
    /// of [`crate::fleet::role::ChildAuthority::clamp`].
    #[must_use]
    pub fn resolve(
        role: &FleetRole,
        profile: &WorkerRuntimeProfile,
        scope: Option<Vec<String>>,
        can_spawn: bool,
    ) -> Self {
        let preset = Self::for_role(role);
        let profile_shell = match profile.shell {
            ShellPolicy::None => ShellGrant::None,
            ShellPolicy::ReadOnly => ShellGrant::Inspect,
            ShellPolicy::Full => ShellGrant::Full,
        };
        // The posture deny lists are the transport the ceiling clamps install;
        // the grant folds their sentinels into the semantic axes so the two
        // can never disagree. `fetch_url` stands for "no network"; `run_tests`
        // stands for "no shell authority at all" — it removes process-start
        // grants (`Verify`/`Full` → `None`) while `Inspect` survives, because
        // classifier-bounded evidence reads were never shell authority.
        // `exec_shell` (raw shell gone) is deliberately *not* folded: a
        // write-denied verifier installs it while keeping shell authority.
        let denied = |name: &str| {
            crate::core::engine::tool_catalog::tool_denied(
                Some(profile.denied_tools.as_slice()),
                name,
            )
        };
        let resolved_shell = preset.shell.min(profile_shell);
        let shell = if denied(crate::fleet::role::SHELL_AUTHORITY_SENTINEL) {
            match resolved_shell {
                ShellGrant::Inspect => ShellGrant::Inspect,
                _ => ShellGrant::None,
            }
        } else {
            resolved_shell
        };
        let no_tools = matches!(scope.as_deref(), Some([]));
        Self {
            files: if no_tools {
                FileGrant::None
            } else {
                preset.files.min(if profile.permissions.write {
                    FileGrant::Write
                } else {
                    FileGrant::Read
                })
            },
            shell,
            network: preset.network
                && profile.permissions.network
                && !denied(crate::fleet::role::NETWORK_DENIAL_SENTINEL),
            desktop: false,
            surface: preset.surface,
            scope,
            spawn: can_spawn,
        }
    }

    /// The transport shell policy the registered surface and `BashTool`
    /// enforce for this grant.
    ///
    /// `Verify` maps to `ShellPolicy::Full`: the bounded verification surface
    /// is a process-start authority the profile transports as Full, while the
    /// grant itself — not the transport — decides that raw shell names never
    /// reach the catalog. Keeping the transport at Full also preserves the
    /// ceiling a verifier may pass to its own children.
    #[must_use]
    pub const fn shell_policy(&self) -> ShellPolicy {
        match self.shell {
            ShellGrant::None => ShellPolicy::None,
            ShellGrant::Inspect => ShellPolicy::ReadOnly,
            ShellGrant::Verify | ShellGrant::Full => ShellPolicy::Full,
        }
    }
}

/// How a worker's model is selected. New model-facing spawns default to the
/// parent/session model; a child only takes a smaller/faster family sibling when
/// the parent explicitly asks for that route.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelRoute {
    /// Same model as the parent / session.
    Inherit,
    /// Explicitly request a smaller/faster same-family sibling when known.
    Faster,
    /// Legacy persisted route from the old hidden auto-router. New spawns do
    /// not emit this; runtime treats it like `Faster` for compatibility.
    Auto,
    /// An explicit model id, validated against the active provider at spawn time.
    Fixed(String),
}

/// The capability contract a single worker runs under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerRuntimeProfile {
    pub role: FleetRole,
    pub permissions: PermissionSet,
    pub shell: ShellPolicy,
    pub tools: ToolScope,
    pub model: ModelRoute,
    /// Explicit provider override; `None` inherits the parent/session provider.
    pub provider: Option<String>,
    /// Explicit reasoning/thinking tier; `None` inherits the parent/session tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Tool deny-list inherited from the parent session's `--disallowed-tools`
    /// (#4042). Deny always wins over allow, even over the explicit allowlist
    /// and the role posture. Entries support wildcard matching: an exact name
    /// (`exec_shell`) or a `prefix*` glob (`mcp_*`), compared case-insensitively.
    ///
    /// A child can only ever *add* entries — `derive_child()` takes the union of
    /// the parent's and the child's deny lists, so a descendant can never drop a
    /// restriction an ancestor imposed. The legacy `inherit_disallowed_tools:
    /// false` input remains accepted but cannot remove this ceiling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied_tools: Vec<String>,
    /// Absolute recursion ceiling. Older profiles stored a remaining allowance;
    /// interpreting that smaller value as absolute fails closed on recovery.
    /// `spawn_depth` records this worker's position on the same axis.
    pub max_spawn_depth: u32,
    #[serde(default)]
    pub spawn_depth: u32,
    /// Whole-run wall time, including queued, model and tool work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_time_secs: Option<u64>,
    /// Persisted wall-clock deadline; continuations cannot restart the clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_deadline_ms: Option<u64>,
    /// Optional model-turn cap. Zero means unbounded, matching the normal
    /// Codex and GrokBuild agent loop; an operator may still set a cap.
    #[serde(default = "default_general_max_steps")]
    pub max_steps: u32,
    /// Whether the worker runs detached (background) or inline (foreground).
    pub background: bool,
}

impl WorkerRuntimeProfile {
    /// Default model turns for every role: unbounded unless explicitly capped.
    pub const READ_ONLY_MAX_STEPS: u32 = 0;
    pub const GENERAL_MAX_STEPS: u32 = 0;

    /// Return the default model-turn cap for this role (zero = unbounded).
    #[must_use]
    pub const fn default_max_steps(role: FleetRole) -> u32 {
        match role {
            FleetRole::Scout
            | FleetRole::Reviewer
            | FleetRole::Planner
            | FleetRole::Verifier
            | FleetRole::Consultant => Self::READ_ONLY_MAX_STEPS,
            FleetRole::Builder | FleetRole::Worker | FleetRole::Custom => Self::GENERAL_MAX_STEPS,
        }
    }

    /// The default profile for a role — the per-role posture. Mirrors the role
    /// stances documented in `docs/SUBAGENTS.md` (explore/plan/review are
    /// read-only; verifier runs tests; implementer/general write).
    #[must_use]
    pub fn for_role(role: FleetRole) -> Self {
        // A role's default is what the role *intends*, expressed as the widest
        // posture the role can be given; the parent's effective posture is the
        // ceiling (`derive_child` intersects, never widens). Read-only roles
        // stay read-only on the workspace by intent. Nothing else is taken
        // away by default: network reach is a read, and a worker cut off from
        // the network or from shell for no role reason cannot do its job.
        let (permissions, shell) = match role {
            // Read-only investigators: no workspace writes, but network reach
            // and the bounded verification surface so a scout/reviewer lane
            // can run git/gh/web inspection. Raw shell stays denied by the
            // registry clamp (read-only classifier), so this widens capability
            // without widening mutation authority.
            FleetRole::Scout | FleetRole::Reviewer => {
                (PermissionSet::read_only_with_network(), ShellPolicy::Full)
            }
            // Planner: analysis only. Reads the workspace and the web and may
            // run read-only shell probes (`git log`, `rg`) under the read-only
            // classifier; never mutates.
            FleetRole::Planner => (
                PermissionSet::read_only_with_network(),
                ShellPolicy::ReadOnly,
            ),
            // Consultant: counsel only. Reads (workspace and web) to ground
            // its advice; never acts on the workspace, so no shell (#4752).
            FleetRole::Consultant => (PermissionSet::read_only_with_network(), ShellPolicy::None),
            // Verifier: doesn't modify code, but runs the bounded built-in
            // verification surface (test/check selections) under a full shell
            // ceiling clamped by ChildAuthority: writes are denied and
            // unbounded shell forms are refused (#5186). The old wording
            // promised "runs the test suite" without saying the surface is
            // bounded. See the roster description and VERIFIER_AGENT_INTRO.
            FleetRole::Verifier => (PermissionSet::read_only_with_network(), ShellPolicy::Full),
            // Doers, and Custom: inherit the parent's effective posture. A
            // custom worker is narrowed by its explicit tool list and by the
            // spawning call, not by a silent locked-down default.
            FleetRole::Builder | FleetRole::Worker | FleetRole::Custom => {
                (PermissionSet::full(), ShellPolicy::Full)
            }
        };
        Self {
            role: role.clone(),
            permissions,
            shell,
            tools: ToolScope::Inherit,
            model: ModelRoute::Inherit,
            provider: None,
            // A Consultant is asked for judgement, so it defaults to the highest
            // reasoning tier rather than inheriting the session's (#4752).
            // Still only a default: an explicit spawn-time or profile value
            // wins via `derive_child`, same as every other role.
            reasoning_effort: matches!(role, FleetRole::Consultant).then(|| "high".to_string()),
            denied_tools: Vec::new(),
            max_spawn_depth: codewhale_config::DEFAULT_SPAWN_DEPTH,
            spawn_depth: 0,
            wall_time_secs: None,
            wall_deadline_ms: None,
            max_steps: Self::default_max_steps(role.clone()),
            background: true,
        }
    }

    /// Derive a child profile from this (parent) profile and a `requested` child
    /// profile. The result is the **intersection** of the two — it can never
    /// grant the child something the parent lacks (#414 / #426 / #1186):
    ///
    /// - permissions are AND-ed,
    /// - shell takes the more restrictive policy,
    /// - an explicit parent tool set bounds the child's tool set,
    /// - the spawn-depth budget decrements by one level and clamps to the ceiling,
    /// - the tool deny-list is the **union** of the two — a child may add
    ///   restrictions but never drop one an ancestor imposed (#4042).
    ///
    /// The child keeps its own requested role, model route, and
    /// foreground/background preference (these don't grant capability), but its
    /// provider falls back to the parent's when unset.
    #[must_use]
    pub fn derive_child(&self, requested: &WorkerRuntimeProfile) -> WorkerRuntimeProfile {
        let permissions = self.permissions.intersect(requested.permissions);
        let shell = self.shell.min_with(requested.shell);
        // Deny-lists union: a child can never drop a restriction an ancestor
        // imposed. Wildcard entries are merged verbatim (no expansion).
        let mut denied_tools = self.denied_tools.clone();
        for rule in &requested.denied_tools {
            if !denied_tools.contains(rule) {
                denied_tools.push(rule.clone());
            }
        }
        let tools = match (&self.tools, &requested.tools) {
            // Parent restricts to a set → the child can only narrow within it.
            (ToolScope::Explicit(parent), ToolScope::Explicit(child)) => ToolScope::Explicit(
                child
                    .iter()
                    .filter(|name| parent.contains(name))
                    .cloned()
                    .collect(),
            ),
            (ToolScope::Explicit(parent), ToolScope::Inherit) => {
                ToolScope::Explicit(parent.clone())
            }
            // Parent inherits the full surface → the child's request stands.
            (ToolScope::Inherit, child) => child.clone(),
        };
        // Depth stays absolute; only the current position increments. Every
        // authority projection compares the position against this same ceiling.
        let max_spawn_depth = requested
            .max_spawn_depth
            .min(self.max_spawn_depth)
            .min(codewhale_config::MAX_SPAWN_DEPTH_CEILING);
        WorkerRuntimeProfile {
            role: requested.role.clone(),
            permissions,
            shell,
            tools,
            model: requested.model.clone(),
            provider: requested.provider.clone().or_else(|| self.provider.clone()),
            reasoning_effort: requested
                .reasoning_effort
                .clone()
                .or_else(|| self.reasoning_effort.clone()),
            denied_tools,
            max_spawn_depth,
            spawn_depth: self.spawn_depth.saturating_add(1),
            wall_time_secs: narrow_optional_limit(self.wall_time_secs, requested.wall_time_secs),
            wall_deadline_ms: narrow_optional_limit(
                self.wall_deadline_ms,
                requested.wall_deadline_ms,
            ),
            max_steps: narrow_model_steps(self.max_steps, requested.max_steps),
            background: requested.background,
        }
    }

    /// Remaining generations, projected from the one absolute ceiling.
    #[must_use]
    pub fn remaining_spawn_depth(&self) -> u32 {
        self.max_spawn_depth.saturating_sub(self.spawn_depth)
    }

    #[must_use]
    pub fn can_spawn_child(&self) -> bool {
        self.spawn_depth < self.max_spawn_depth
    }
}

/// Omission inherits; neither a child request nor a replay can widen a cap.
pub(crate) fn narrow_optional_limit<T: Ord>(
    inherited: Option<T>,
    requested: Option<T>,
) -> Option<T> {
    match (inherited, requested) {
        (Some(inherited), Some(requested)) => Some(inherited.min(requested)),
        (inherited, requested) => inherited.or(requested),
    }
}

/// Zero is the operator/internal unbounded sentinel, never a widening request.
pub(crate) fn narrow_model_steps(inherited: u32, requested: u32) -> u32 {
    narrow_optional_limit(
        (inherited > 0).then_some(inherited),
        (requested > 0).then_some(requested),
    )
    .unwrap_or(0)
}

const fn default_general_max_steps() -> u32 {
    WorkerRuntimeProfile::GENERAL_MAX_STEPS
}

impl Default for WorkerRuntimeProfile {
    fn default() -> Self {
        Self::for_role(FleetRole::Worker)
    }
}

/// Unified pre-launch manifest for a child agent (#414).
///
/// Everything needed to provision, launch, and resume a child — prompt, role,
/// model, tools, permissions, workspace boundary, budget, and identity — comes
/// from this single persisted record. No field is derived ad-hoc at launch time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChildLaunchManifest {
    pub owner_session: String,
    pub child_id: String,
    pub profile: WorkerRuntimeProfile,
    pub prompt: String,
    pub cwd: Option<String>,
    pub worktree: bool,
    pub writable_roots: Vec<String>,
    #[serde(default)]
    pub writable_files: Vec<String>,
    #[serde(default)]
    pub coordination_contracts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliverables: Vec<String>,
    pub resume_identity: Option<String>,
    #[serde(default)]
    pub generation: u32,
    /// Agent id this child was resumed from via `resume_from`, if any.
    /// Carries provenance across continuation chains so receipts can trace
    /// the lineage without inspecting the transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from_agent_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_intersection_never_escalates() {
        let parent = PermissionSet::read_only();
        let greedy_child = PermissionSet::full();
        // Even though the child asks for everything, the read-only parent wins.
        let got = parent.intersect(greedy_child);
        assert_eq!(got, PermissionSet::read_only());
    }

    #[test]
    fn shell_policy_min_takes_the_safer() {
        assert_eq!(
            ShellPolicy::ReadOnly.min_with(ShellPolicy::Full),
            ShellPolicy::ReadOnly
        );
        assert_eq!(
            ShellPolicy::None.min_with(ShellPolicy::ReadOnly),
            ShellPolicy::None
        );
        assert_eq!(
            ShellPolicy::Full.min_with(ShellPolicy::Full),
            ShellPolicy::Full
        );
    }

    #[test]
    fn for_role_postures_match_role_stances() {
        let explore = WorkerRuntimeProfile::for_role(FleetRole::Scout);
        assert!(!explore.permissions.write, "explore must not write");
        assert!(
            explore.permissions.network,
            "explore/read-only inspection lanes keep network reach"
        );
        assert_eq!(
            explore.shell,
            ShellPolicy::Full,
            "explore/read-only inspection lanes hold shell authority so the bounded              verification surface survives the clamp (raw shell still              requires write)"
        );
        assert_eq!(
            explore.model,
            ModelRoute::Inherit,
            "explore should not silently downgrade the child model"
        );

        let implementer = WorkerRuntimeProfile::for_role(FleetRole::Builder);
        assert!(implementer.permissions.write, "implementer writes");
        assert_eq!(implementer.shell, ShellPolicy::Full);

        let verifier = WorkerRuntimeProfile::for_role(FleetRole::Verifier);
        assert!(
            !verifier.permissions.write,
            "verifier reports, does not patch"
        );
        assert_eq!(
            verifier.shell,
            ShellPolicy::Full,
            "verifier holds shell authority for the bounded verification surface (unbounded forms are refused by the policy seam)"
        );
    }

    #[test]
    fn role_step_budgets_are_unbounded_by_default_and_profile_owned() {
        for role in [
            FleetRole::Scout,
            FleetRole::Reviewer,
            FleetRole::Planner,
            FleetRole::Verifier,
            FleetRole::Builder,
            FleetRole::Worker,
            FleetRole::Custom,
        ] {
            assert_eq!(WorkerRuntimeProfile::for_role(role.clone()).max_steps, 0);
            assert_eq!(
                WorkerRuntimeProfile::for_role(role.clone()).max_steps,
                WorkerRuntimeProfile::default_max_steps(role)
            );
        }
    }

    /// #4752: Consultant is counsel, not labour. Its posture has to be read-only
    /// and shell-less by construction, not by the caller remembering to pass
    /// `write_authority: read-only`.
    #[test]
    fn consultant_is_read_only_shell_less_and_high_reasoning_by_default() {
        let consultant = WorkerRuntimeProfile::for_role(FleetRole::Consultant);

        assert!(
            !consultant.permissions.write,
            "a consultant advises, it never writes"
        );
        assert_eq!(
            consultant.shell,
            ShellPolicy::None,
            "a consultant has no reason to run commands"
        );
        assert_eq!(
            consultant.reasoning_effort.as_deref(),
            Some("high"),
            "the point of asking a consultant is the reasoning tier"
        );
        assert_eq!(
            consultant.model,
            ModelRoute::Inherit,
            "tier is a reasoning-effort default, not a hardcoded model"
        );
        assert_eq!(
            consultant.max_steps,
            WorkerRuntimeProfile::READ_ONLY_MAX_STEPS,
            "consultants are unbounded by default like every other role"
        );
    }

    /// The reasoning default must not become a ceiling: an explicit request
    /// still wins, exactly as it does for every other role.
    #[test]
    fn an_explicit_reasoning_tier_overrides_the_consultant_default() {
        let parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        let mut requested = WorkerRuntimeProfile::for_role(FleetRole::Consultant);
        requested.reasoning_effort = Some("max".to_string());

        let child = parent.derive_child(&requested);

        assert_eq!(child.reasoning_effort.as_deref(), Some("max"));
        assert!(!child.permissions.write, "still read-only");
    }

    #[test]
    fn child_cannot_escalate_beyond_a_readonly_parent() {
        // Scout now carries the read-only inspection posture: no writes, but network reach
        // and full shell authority (bounded verification surface; raw shell
        // still requires write at the clamp).
        let parent = WorkerRuntimeProfile::for_role(FleetRole::Scout); // read-only inspection
        let greedy = WorkerRuntimeProfile::for_role(FleetRole::Builder); // wants write + full shell
        let child = parent.derive_child(&greedy);
        assert!(
            !child.permissions.write,
            "a read-only parent cannot bear a writing child"
        );
        assert!(
            child.permissions.network,
            "child inherits the read-only inspection parent's network reach"
        );
        assert_eq!(
            child.shell,
            ShellPolicy::Full,
            "child shell clamped to parent's read-only inspection posture"
        );
    }

    #[test]
    fn child_explicit_tools_are_bounded_by_parent() {
        let mut parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        parent.tools = ToolScope::Explicit(vec!["read_file".into(), "grep_files".into()]);
        let mut requested = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        requested.tools = ToolScope::Explicit(vec!["read_file".into(), "write_file".into()]);
        let child = parent.derive_child(&requested);
        match child.tools {
            ToolScope::Explicit(names) => {
                assert_eq!(
                    names,
                    vec!["read_file".to_string()],
                    "write_file not in parent set is dropped"
                );
            }
            ToolScope::Inherit => panic!("expected explicit tool scope"),
        }
    }

    #[test]
    fn absolute_spawn_depth_is_preserved_and_position_increments() {
        let mut parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        parent.max_spawn_depth = 2;
        let mut requested = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        requested.max_spawn_depth = 99; // tries to grab more than the parent has
        let child = parent.derive_child(&requested);
        assert_eq!(
            child.max_spawn_depth, 2,
            "the absolute ceiling is unchanged, never the requested 99"
        );
        assert_eq!(child.spawn_depth, 1);
        assert!(child.can_spawn_child());

        let mut leaf_parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        leaf_parent.max_spawn_depth = 1;
        let grandchild = leaf_parent.derive_child(&requested);
        assert_eq!(grandchild.max_spawn_depth, 1);
        assert_eq!(grandchild.spawn_depth, 1);
        assert!(
            !grandchild.can_spawn_child(),
            "budget exhausted at the leaf"
        );
    }

    #[test]
    fn child_provider_falls_back_to_parent() {
        let mut parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        parent.provider = Some("moonshot".to_string());
        let requested = WorkerRuntimeProfile::for_role(FleetRole::Scout); // provider None
        let child = parent.derive_child(&requested);
        assert_eq!(child.provider.as_deref(), Some("moonshot"));
    }

    #[test]
    fn child_reasoning_effort_uses_requested_then_parent() {
        let mut parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        parent.reasoning_effort = Some("low".to_string());

        let requested = WorkerRuntimeProfile::for_role(FleetRole::Scout);
        let inherited = parent.derive_child(&requested);
        assert_eq!(inherited.reasoning_effort.as_deref(), Some("low"));

        let mut requested = WorkerRuntimeProfile::for_role(FleetRole::Scout);
        requested.reasoning_effort = Some("max".to_string());
        let overridden = parent.derive_child(&requested);
        assert_eq!(overridden.reasoning_effort.as_deref(), Some("max"));
    }

    #[test]
    fn child_denied_tools_union_never_drops_parent_restriction() {
        // A child may only *add* deny entries; it can never drop a restriction
        // an ancestor imposed (#4042 non-escalation invariant).
        let mut parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        parent.denied_tools = vec!["exec_shell".into(), "mcp_*".into()];

        // Child asks for its own deny list and (tryingly) tries to omit the
        // parent's exec_shell — the union keeps both.
        let mut requested = WorkerRuntimeProfile::for_role(FleetRole::Builder);
        requested.denied_tools = vec!["write_file".into()];

        let child = parent.derive_child(&requested);
        assert!(child.denied_tools.contains(&"exec_shell".to_string()));
        assert!(child.denied_tools.contains(&"mcp_*".to_string()));
        assert!(child.denied_tools.contains(&"write_file".to_string()));
    }

    /// Every built-in role keeps network reads by default: a worker cut off
    /// from the network for no role reason cannot do its job. Only workspace
    /// mutation is a role intent.
    #[test]
    fn every_role_default_keeps_network_reads_and_only_read_only_roles_withhold_writes() {
        for role in [
            FleetRole::Scout,
            FleetRole::Reviewer,
            FleetRole::Planner,
            FleetRole::Verifier,
            FleetRole::Consultant,
            FleetRole::Builder,
            FleetRole::Worker,
            FleetRole::Custom,
        ] {
            let profile = WorkerRuntimeProfile::for_role(role.clone());
            assert!(
                profile.permissions.network,
                "{role:?} must keep network reads"
            );
            let read_only_by_intent = matches!(
                role,
                FleetRole::Scout
                    | FleetRole::Reviewer
                    | FleetRole::Planner
                    | FleetRole::Verifier
                    | FleetRole::Consultant
            );
            assert_eq!(
                profile.permissions.write, !read_only_by_intent,
                "{role:?} write default"
            );
        }
        // Custom inherits (full ceiling); Planner probes read-only shell;
        // Consultant never acts on the workspace.
        assert_eq!(
            WorkerRuntimeProfile::for_role(FleetRole::Custom).shell,
            ShellPolicy::Full
        );
        assert_eq!(
            WorkerRuntimeProfile::for_role(FleetRole::Planner).shell,
            ShellPolicy::ReadOnly
        );
        assert_eq!(
            WorkerRuntimeProfile::for_role(FleetRole::Consultant).shell,
            ShellPolicy::None
        );
    }

    /// The parent's effective posture is the ceiling: a full-default child role
    /// under a read-only, no-network, read-only-shell parent inherits exactly
    /// that, never more.
    #[test]
    fn derive_child_inherits_the_parent_ceiling_and_never_widens() {
        let mut parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        parent.permissions = PermissionSet::read_only();
        parent.shell = ShellPolicy::ReadOnly;
        for role in [FleetRole::Custom, FleetRole::Builder, FleetRole::Worker] {
            let child = parent.derive_child(&WorkerRuntimeProfile::for_role(role.clone()));
            assert!(!child.permissions.write, "{role:?} widened write");
            assert!(!child.permissions.network, "{role:?} widened network");
            assert_eq!(child.shell, ShellPolicy::ReadOnly, "{role:?} widened shell");
        }
        // And a full parent hands a doer its full posture.
        let full = WorkerRuntimeProfile::for_role(FleetRole::Worker)
            .derive_child(&WorkerRuntimeProfile::for_role(FleetRole::Custom));
        assert!(full.permissions.write && full.permissions.network);
        assert_eq!(full.shell, ShellPolicy::Full);
    }
}

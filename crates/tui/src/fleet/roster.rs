//! Fleet roster — the persistent, inspectable party of named agent roles.
//!
//! The roster merges four layers into one config-backed lineup shared by
//! model-spawned sub-agents and fleet dispatch (#fleet-roster cutover
//! (v0.8.67)):
//!
//! - built-in members (the default party, always available; every canonical
//!   dispatch posture — worker/scout/planner/reviewer/builder/verifier/
//!   consultant/custom — is seeded here, #5285),
//! - `[fleet.profiles]` entries from config.toml,
//! - personal `$CODEWHALE_HOME/agents/*.toml` profile files,
//! - workspace `.codewhale/agents/*.toml` profile files,
//! - Claude Code agent files (`.claude/agents/*.md`, then `~/.claude/agents`),
//!   which only fill ids no other layer defines.
//!
//! Precedence is Workspace > Personal > Config > Plugin > BuiltIn > Claude Code, merged by id. Loading never
//! fails the session: an unreadable workspace profile dir degrades to the
//! built-in + config layers with a log line.
//!
//! Two guardrails (#5098):
//!
//! - Shadowing is recorded, not silent: when a higher layer displaces a
//!   lower-precedence file for the same id, the roster keeps a
//!   [`ShadowedProfile`] receipt (logged at load, badged in the roster view)
//!   so an edit in the losing layer is visibly ignored rather than dropped.
//! - Project-scope profiles (`.codewhale/agents/*.toml`) join the roster only
//!   when project-level config is trusted for the launch; `--no-project-config`
//!   opts the whole layer out, same as `.codewhale/config.toml` (#485).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use codewhale_config::{
    FleetConfigToml, FleetDelegationHints, FleetLoadout, FleetProfile, FleetProfilePermissions,
    FleetRole, FleetSlot,
};

use super::profile::{
    AgentProfile, AgentProfileLoadIssue, CLAUDE_AGENT_DIR, claude_user_agent_dir,
    load_agent_profiles_from_dir_tolerant, load_claude_agent_profiles_from_dir_in,
    load_plugin_agent_profiles_from_component, load_workspace_agent_profiles_tolerant,
    personal_agent_profile_dir,
};

/// Which layer a roster member came from. Higher layers override lower ones
/// by id (Workspace > Personal > Config > Plugin > BuiltIn > ClaudeCode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileOrigin {
    BuiltIn,
    Plugin,
    Config,
    Personal,
    Workspace,
    /// A Claude Code agent file (`.claude/agents/*.md` or `~/.claude/agents`).
    /// Lowest precedence: it never displaces a Codewhale definition.
    ClaudeCode,
}

impl std::fmt::Display for ProfileOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::BuiltIn => "built-in",
            Self::Plugin => "plugin",
            Self::Config => "config",
            Self::Personal => "personal",
            Self::Workspace => "project",
            Self::ClaudeCode => "claude",
        })
    }
}

/// The merged fleet roster. Think RPG saved party / K8s runconfig: a stable,
/// named lineup of agent roles the session can inspect and dispatch against.
#[derive(Debug, Clone)]
pub struct FleetRoster {
    members: Vec<AgentProfile>,
    /// True when `members` is the exact roster from one explicitly selected
    /// v2 Fleet rather than the compatibility merge of built-ins and profile
    /// layers. Exact rosters require every dispatched task to resolve one
    /// deterministic member.
    exact_selection: bool,
    /// Lower-precedence profiles displaced by a higher layer for the same id
    /// (#5098). Shadowing is normal precedence, but it must be VISIBLE: a
    /// personal edit that loses to a stale project copy otherwise changes
    /// nothing anywhere with no signal why.
    shadowed: Vec<ShadowedProfile>,
    /// An explicitly selected v2 Fleet could not be loaded. Consumers retain
    /// this error instead of silently substituting the legacy roster.
    load_error: Option<String>,
    profile_load_issues: Vec<AgentProfileLoadIssue>,
}

/// A lower-precedence profile displaced by a higher layer for the same id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowedProfile {
    pub id: String,
    pub shadowed_origin: ProfileOrigin,
    pub shadowed_source: PathBuf,
    pub winner_origin: ProfileOrigin,
    pub winner_source: PathBuf,
}

/// One observed definition of a profile id, including whether it won the merge.
///
/// Built from the winning member plus every [`ShadowedProfile`] for that id so
/// the roster view, detail pane, and doctor can list the full stack without
/// changing merge precedence (#5098 visibility).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileLayer {
    pub origin: ProfileOrigin,
    pub source: PathBuf,
    pub wins: bool,
}

/// A profile id that exists in more than one roster layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiLayerProfile {
    pub id: String,
    pub effective: ProfileOrigin,
    pub effective_path: PathBuf,
    pub layers: Vec<ProfileLayer>,
}

fn origin_precedence(origin: ProfileOrigin) -> u8 {
    match origin {
        ProfileOrigin::Workspace => 5,
        ProfileOrigin::Personal => 4,
        ProfileOrigin::Config => 3,
        ProfileOrigin::Plugin => 2,
        ProfileOrigin::BuiltIn => 1,
        ProfileOrigin::ClaudeCode => 0,
    }
}

/// Process-launch decision: whether project-scope agent profiles
/// (`.codewhale/agents/*.toml`) may join the dispatch roster (#5098). Set
/// once from `--no-project-config` at launch so every roster re-read (spawn
/// refresh, dispatch, views) honors the same trust decision other
/// project-level config already has (#485). Defaults to enabled, matching
/// project config itself.
static PROJECT_AGENT_PROFILES_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Record the launch-time trust decision for project-scope agent profiles.
pub fn set_project_agent_profiles_enabled(enabled: bool) {
    PROJECT_AGENT_PROFILES_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Whether project-scope agent profiles join the roster in this process.
#[must_use]
pub fn project_agent_profiles_enabled() -> bool {
    PROJECT_AGENT_PROFILES_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

impl FleetRoster {
    /// Roster containing only the built-in party. Used as the runtime default
    /// before config/workspace layers are wired in.
    #[must_use]
    pub fn built_ins_only() -> Self {
        Self {
            members: Self::built_in_members(),
            exact_selection: false,
            shadowed: Vec::new(),
            load_error: None,
            profile_load_issues: Vec::new(),
        }
    }

    /// A roster built from an explicit member list.
    ///
    /// Used for run-scoped rosters that are not a merge of the config layers —
    /// notably an exact named Fleet, whose members are frozen at Workflow
    /// start and must not pick up built-in or workspace profiles by name.
    #[must_use]
    pub fn from_members(members: Vec<AgentProfile>) -> Self {
        Self {
            members,
            exact_selection: true,
            shadowed: Vec::new(),
            load_error: None,
            profile_load_issues: Vec::new(),
        }
    }

    /// An unusable explicitly selected Fleet. It deliberately contains no
    /// fallback members: running a different team would hide the selection
    /// failure.
    #[must_use]
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            members: Vec::new(),
            exact_selection: true,
            shadowed: Vec::new(),
            load_error: Some(error.into()),
            profile_load_issues: Vec::new(),
        }
    }

    /// Load and merge the full roster for a workspace.
    ///
    /// Config members come from `[fleet.profiles]` (id = map key). Personal
    /// members come from `$CODEWHALE_HOME/agents/*.toml`, and workspace members
    /// come from `.codewhale/agents/*.toml`. A load failure is logged and
    /// skipped so one broken profile layer cannot take down the session.
    #[must_use]
    pub fn load(fleet_config: &FleetConfigToml, workspace: &Path) -> Self {
        let personal_dir = personal_agent_profile_dir().ok();
        Self::load_with_personal_dir_and_plugins(
            fleet_config,
            workspace,
            personal_dir.as_deref(),
            claude_user_agent_dir().as_deref(),
            project_agent_profiles_enabled(),
            None,
        )
    }

    /// Load the ordinary roster plus trusted, enabled plugin Agent profiles.
    #[must_use]
    pub fn load_with_plugins(
        fleet_config: &FleetConfigToml,
        workspace: &Path,
        plugins: &crate::plugins::PluginRegistry,
    ) -> Self {
        let personal_dir = personal_agent_profile_dir().ok();
        Self::load_with_personal_dir_and_plugins(
            fleet_config,
            workspace,
            personal_dir.as_deref(),
            claude_user_agent_dir().as_deref(),
            project_agent_profiles_enabled(),
            Some(plugins),
        )
    }

    #[cfg(test)]
    fn load_with_personal_dir(
        fleet_config: &FleetConfigToml,
        workspace: &Path,
        personal_dir: Option<&Path>,
        include_workspace_profiles: bool,
    ) -> Self {
        Self::load_with_personal_dir_and_plugins(
            fleet_config,
            workspace,
            personal_dir,
            None,
            include_workspace_profiles,
            None,
        )
    }

    fn load_with_personal_dir_and_plugins(
        fleet_config: &FleetConfigToml,
        workspace: &Path,
        personal_dir: Option<&Path>,
        claude_user_dir: Option<&Path>,
        include_workspace_profiles: bool,
        plugins: Option<&crate::plugins::PluginRegistry>,
    ) -> Self {
        let mut built_ins = Self::built_in_members();
        let mut extras: Vec<AgentProfile> = Vec::new();
        let mut shadowed: Vec<ShadowedProfile> = Vec::new();
        let mut profile_load_issues = Vec::new();

        if let Some(plugins) = plugins {
            for (preset, authority) in crate::extension_host::native_presets_for_plugins(plugins) {
                // An ordinary roster member backed by the existing live owner
                // receipt. Selection is not authority: preparation checks it.
                let identity = crate::hashing::sha256_hex(
                    format!("{}:{}", preset.plugin_id, preset.entry.path).as_bytes(),
                );
                let metadata = crate::plugins::native_presets::metadata(plugins, &preset);
                let id = format!(
                    "{}-native-{}",
                    authority.plugin_name.chars().take(32).collect::<String>(),
                    &identity[..16]
                );
                let mut profile = FleetProfile::default();
                profile.role.name = "general".into();
                profile.role.description = metadata
                    .as_ref()
                    .and_then(|data| data.description.clone())
                    .or_else(|| {
                        Some(format!(
                            "Reviewed Native composition from {}",
                            authority.plugin_name
                        ))
                    });
                let member = AgentProfile {
                    id,
                    display_name: Some(
                        metadata
                            .as_ref()
                            .map(|data| data.name.clone().unwrap_or_else(|| data.id.clone()))
                            .unwrap_or_else(|| authority.plugin_name.clone()),
                    ),
                    description: profile.role.description.clone(),
                    requires: Vec::new(),
                    profile,
                    source: PathBuf::from(&preset.entry.path),
                    origin: ProfileOrigin::Plugin,
                    plugin_authority: Some(authority),
                    native_preset: Some(preset),
                };
                record_shadow(
                    merge_member(&mut built_ins, &mut extras, member),
                    &mut shadowed,
                );
            }
            let (sources, errors) = crate::plugins::runtime::active_component_sources(
                plugins,
                crate::plugins::activation::PluginActivationCapability::Agents,
            );
            for error in errors {
                tracing::warn!("fleet roster: {error}");
            }
            for source in sources {
                match load_plugin_agent_profiles_from_component(&source.path, &source.authority) {
                    Ok((profiles, issues)) => {
                        for issue in &issues {
                            tracing::warn!(
                                plugin = %source.plugin_name,
                                "fleet roster: skipping invalid plugin Agent profile: {issue}"
                            );
                        }
                        profile_load_issues.extend(issues);
                        for member in profiles {
                            record_shadow(
                                merge_member(&mut built_ins, &mut extras, member),
                                &mut shadowed,
                            );
                        }
                    }
                    Err(error) => tracing::warn!(
                        plugin = %source.plugin_name,
                        "fleet roster: failed to load plugin Agent profiles: {error:#}"
                    ),
                }
            }
        }

        for (id, profile) in &fleet_config.profiles {
            let mut profile = profile.clone();
            profile.role.name = super::profile::canonical_public_role_name(&profile.role.name);
            profile.slot = FleetSlot::from_name(&profile.role.name);
            let member = AgentProfile {
                native_preset: None,
                id: id.clone(),
                display_name: None,
                description: profile.role.description.clone(),
                requires: Vec::new(),
                profile,
                source: PathBuf::from("config.toml"),
                origin: ProfileOrigin::Config,
                plugin_authority: None,
            };
            record_shadow(
                merge_member(&mut built_ins, &mut extras, member),
                &mut shadowed,
            );
        }

        if let Some(personal_dir) = personal_dir {
            match load_agent_profiles_from_dir_tolerant(personal_dir, ProfileOrigin::Personal) {
                Ok((profiles, issues)) => {
                    for issue in &issues {
                        tracing::warn!(
                            "fleet roster: skipping invalid personal agent profile: {issue}"
                        );
                    }
                    profile_load_issues.extend(issues);
                    for member in profiles {
                        record_shadow(
                            merge_member(&mut built_ins, &mut extras, member),
                            &mut shadowed,
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!("fleet roster: skipping personal agent profiles: {err:#}");
                }
            }
        }

        // #5098: project-scope profiles join the dispatch roster only when the
        // launch trusted project-level config (`--no-project-config` opts the
        // whole layer out, same as `.codewhale/config.toml`).
        if include_workspace_profiles {
            match load_workspace_agent_profiles_tolerant(workspace) {
                Ok((profiles, issues)) => {
                    for issue in &issues {
                        tracing::warn!(
                            workspace = %workspace.display(),
                            "fleet roster: skipping invalid workspace agent profile: {issue}"
                        );
                    }
                    profile_load_issues.extend(issues);
                    for member in profiles {
                        record_shadow(
                            merge_member(&mut built_ins, &mut extras, member),
                            &mut shadowed,
                        );
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        workspace = %workspace.display(),
                        "fleet roster: skipping workspace agent profiles: {err:#}"
                    );
                }
            }
        }

        // Claude Code agent files come last and only fill ids nobody else
        // defined. The project copy (trusted project config only) is read
        // before `~/.claude/agents`, matching Claude Code's own precedence.
        // Only the project copy is workspace content, so only it is confined
        // to the workspace; `~/.claude/agents` is the user's own directory.
        let claude_dirs = include_workspace_profiles
            .then(|| (workspace.join(CLAUDE_AGENT_DIR), Some(workspace)))
            .into_iter()
            .chain(claude_user_dir.map(|dir| (dir.to_path_buf(), None)));
        for (dir, confine_to) in claude_dirs {
            match load_claude_agent_profiles_from_dir_in(confine_to, &dir) {
                Ok((profiles, issues)) => {
                    for issue in &issues {
                        tracing::warn!(
                            dir = %dir.display(),
                            "fleet roster: skipping Claude agent file: {issue}"
                        );
                    }
                    profile_load_issues.extend(issues);
                    for member in profiles {
                        record_shadow(fill_member(&built_ins, &mut extras, member), &mut shadowed);
                    }
                }
                Err(err) => tracing::warn!(
                    dir = %dir.display(),
                    "fleet roster: skipping Claude agent files: {err:#}"
                ),
            }
        }

        for shadow in &shadowed {
            // Overriding a built-in is the intended customization path —
            // keep it quiet. A file layer (config/personal) losing to another
            // file layer is the #5098 footgun: the edit changes nothing
            // anywhere and must be visible.
            if shadow.shadowed_origin == ProfileOrigin::BuiltIn {
                tracing::debug!(
                    "fleet roster: '{}' {} copy at {} overrides the built-in default",
                    shadow.id,
                    shadow.winner_origin,
                    shadow.winner_source.display()
                );
            } else {
                tracing::warn!(
                    "fleet roster: '{}' {} copy at {} shadows the {} copy at {} (ignored)",
                    shadow.id,
                    shadow.winner_origin,
                    shadow.winner_source.display(),
                    shadow.shadowed_origin,
                    shadow.shadowed_source.display()
                );
            }
        }

        // Built-ins keep their canonical slot order (overrides included);
        // config/workspace-only extras follow alphabetically.
        extras.sort_by_key(|a| a.id.to_lowercase());
        let mut members = built_ins;
        members.extend(extras);
        Self {
            members,
            exact_selection: false,
            shadowed,
            load_error: None,
            profile_load_issues,
        }
    }

    /// The default party. Built-ins carry no permission grants (permissions
    /// stay at the [`FleetProfilePermissions::default`] floor); behavior comes
    /// from the role posture / system prompts plus the role `instructions`
    /// below, which encode the coordination hierarchy: the **operator** (the
    /// session's `/model` selection) directs the work and assigns managers
    /// to workflows; a **manager** is the middle manager of one workflow.
    #[must_use]
    pub fn built_in_members() -> Vec<AgentProfile> {
        [
            (
                "manager",
                FleetSlot::Manager,
                FleetLoadout::Inherit,
                "Middle manager for one workflow: decomposes it into bounded tasks, dispatches workers, integrates results, and reports to the operator.",
                Some(
                    "You lead exactly one workflow. Decompose it into bounded tasks, dispatch them to the right roles, keep work-in-progress small, integrate the results, and report a concise receipt (what was done, evidence, gaps) upward. Do not take on work outside your workflow.",
                ),
            ),
            (
                "operator",
                FleetSlot::Operator,
                FleetLoadout::Inherit,
                "The helm of the session — the session's /model selection. Assigns managers to Workflows, routes work between them, arbitrates conflicts, and reviews what comes back.",
                Some(
                    "You direct the overall work, not individual Workflow steps. Assign a manager per Workflow, route work and context between them, arbitrate conflicts and priorities, review the receipts that come back, and decide what runs next. Delegate execution; keep judgment.",
                ),
            ),
            (
                "scout",
                FleetSlot::Scout,
                FleetLoadout::Inherit,
                "Read-only scouting: find files, map code, gather evidence.",
                None,
            ),
            (
                "builder",
                FleetSlot::Implementer,
                FleetLoadout::Inherit,
                "Writes code: implements bounded tasks with write and shell access.",
                None,
            ),
            (
                "reviewer",
                FleetSlot::Reviewer,
                FleetLoadout::Inherit,
                "Adversarial code review: assumes the change is broken and tries to prove it — regressions, missing tests, unhandled cases. Read-only.",
                Some(
                    "Be adversarial: assume the change is wrong until the evidence proves otherwise. Actively try to refute the claims made about the work — hunt regressions, missing tests, unhandled edge cases, and quiet behavior changes. Report severity-scored findings with file:line evidence; if nothing survives your attack, say so plainly. Never patch.",
                ),
            ),
            (
                "verifier",
                FleetSlot::Verifier,
                FleetLoadout::Inherit,
                "Runs builds and tests to verify claims; reports evidence, does not patch.",
                None,
            ),
            (
                "consultant",
                FleetSlot::Custom("consultant".to_string()),
                FleetLoadout::Inherit,
                "Short-lived, high-reasoning, read-only counsel for difficult decisions and overlooked risks.",
                Some(
                    "Give the operator a direct second opinion grounded in what you can read. Surface the decisive tradeoff, overlooked failure mode, and your recommendation. Advise only: do not edit files or run commands.",
                ),
            ),
            (
                "synthesizer",
                FleetSlot::Summarizer,
                FleetLoadout::Inherit,
                "Read-only synthesis: merge findings into one coherent report.",
                None,
            ),
            // The eight canonical dispatch postures are seeded roster members
            // (#5285). Every `type`/`role` token the Agent tool accepts maps
            // 1:1 to a named roster profile, so dispatch always resolves
            // through the roster instead of a parallel hidden enum. `worker`,
            // `planner`, and `custom` complete the set the roster previously
            // could not see (scout/builder/reviewer/verifier/consultant were
            // already seeded).
            (
                "worker",
                FleetSlot::General,
                FleetLoadout::Inherit,
                "General-purpose worker: full tool access for multi-step tasks. The unnamed dispatch default.",
                None,
            ),
            (
                "planner",
                FleetSlot::Planner,
                FleetLoadout::Inherit,
                "Planning: grounded strategy; read-only workspace, network reads, read-only shell probes.",
                None,
            ),
            (
                "custom",
                FleetSlot::Custom("custom".to_string()),
                FleetLoadout::Inherit,
                "Custom tool access: inherits the parent's write/network/shell posture; narrowed by allowed_tools.",
                None,
            ),
        ]
        .into_iter()
        .map(|(id, slot, loadout, description, instructions)| AgentProfile {
            native_preset: None,
            id: id.to_string(),
            display_name: None,
            description: Some(description.to_string()),
            requires: Vec::new(),
            profile: FleetProfile {
                slot,
                role: FleetRole {
                    name: id.to_string(),
                    description: Some(description.to_string()),
                    instructions: instructions.map(str::to_string),
                },
                loadout,
                model: None,
                provider: None,
                reasoning_effort: (id == "consultant").then(|| "high".to_string()),
                permissions: FleetProfilePermissions::default(),
                delegation: FleetDelegationHints::default(),
            },
            source: PathBuf::from("built-in"),
            origin: ProfileOrigin::BuiltIn,
            plugin_authority: None,
        })
        .collect()
    }

    /// Look up a member by id (trimmed, case-insensitive).
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&AgentProfile> {
        let id = id.trim();
        self.members
            .iter()
            .find(|member| member.id.trim().eq_ignore_ascii_case(id))
    }

    /// All members in stable order: built-in canonical order first (an
    /// overridden built-in keeps its slot but shows its overriding origin),
    /// then extra config/workspace-only members alphabetically.
    #[must_use]
    pub fn members(&self) -> &[AgentProfile] {
        &self.members
    }

    /// Error from an explicitly selected Fleet, if loading it failed.
    #[must_use]
    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn profile_load_issues(&self) -> &[AgentProfileLoadIssue] {
        &self.profile_load_issues
    }

    /// A named broken override is not permission to use a built-in or an older
    /// route. Other members remain usable, and a valid higher layer still wins.
    pub fn resolve_member(
        &self,
        selector: &str,
    ) -> Result<Option<&AgentProfile>, super::identity::FleetSelectorError> {
        use super::identity::{
            FleetSelectorError, bounded_identity_field, resolve_member_in_profiles,
        };
        let member = resolve_member_in_profiles(&self.members, selector)?;
        let requested_id = selector
            .trim()
            .split_once(':')
            .filter(|(kind, _)| matches!(kind.to_ascii_lowercase().as_str(), "member" | "id"))
            .map_or(selector.trim(), |(_, id)| id.trim());
        let issue = self
            .profile_load_issues
            .iter()
            .filter(|issue| {
                let same_id = issue.id.eq_ignore_ascii_case(requested_id)
                    || member.is_some_and(|member| {
                        super::role::public_role_label(&issue.id)
                            .eq_ignore_ascii_case(&super::role::public_role_label(&member.id))
                    });
                same_id
                    && member.is_none_or(|member| {
                        origin_precedence(issue.origin) >= origin_precedence(member.origin)
                    })
            })
            .max_by_key(|issue| origin_precedence(issue.origin));
        if let Some(issue) = issue {
            return Err(FleetSelectorError::Unavailable {
                profile: bounded_identity_field(&issue.id),
                origin: issue.origin.to_string(),
                path: bounded_identity_field(&issue.source.to_string_lossy()),
            });
        }
        Ok(member)
    }

    /// Whether this roster came from one explicitly selected v2 Fleet.
    #[must_use]
    pub fn is_exact_selection(&self) -> bool {
        self.exact_selection
    }

    /// Per-member explicit model pins, keyed by lowercased member id.
    /// Feeds the sub-agent `role_models` lookup; explicit `[subagents]`
    /// overrides are merged on top by the engine and win.
    ///
    /// Members that also pin a provider are deliberately excluded. Their
    /// complete saved profiles bind provider/model together through the
    /// profile spawn path; copying a bare model here would discard route
    /// identity. Provider-less saved defaults share the typed override map
    /// with explicit subagent configuration, without a second lookup table.
    #[must_use]
    pub fn model_overrides(&self) -> HashMap<String, crate::config::SubagentModelOverride> {
        self.members
            .iter()
            .filter_map(|member| {
                if member
                    .profile
                    .provider
                    .as_deref()
                    .is_some_and(|provider| !provider.trim().is_empty())
                {
                    return None;
                }
                let model = member.profile.model.as_deref()?.trim();
                (!model.is_empty()).then(|| (member.id.to_lowercase(), model.into()))
            })
            .collect()
    }
    /// Lower-precedence profiles displaced by higher layers (#5098). Empty
    /// for `built_ins_only` / `from_members` rosters.
    #[must_use]
    pub fn shadowed(&self) -> &[ShadowedProfile] {
        &self.shadowed
    }

    /// Shadow records for one member id (trimmed, case-insensitive).
    #[cfg(test)]
    pub fn shadowed_for<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a ShadowedProfile> {
        let id = id.trim().to_lowercase();
        self.shadowed
            .iter()
            .filter(move |shadow| shadow.id.trim().eq_ignore_ascii_case(&id))
    }

    /// Every layer that defined `id`, winner first, then remaining layers
    /// from highest remaining precedence to lowest.
    #[must_use]
    pub fn layers_for(&self, id: &str) -> Vec<ProfileLayer> {
        let Some(member) = self.get(id) else {
            return Vec::new();
        };
        layers_from_parts(member, &self.shadowed)
    }

    /// Profile ids defined in more than one layer (sorted), with the winning
    /// layer and every losing path. Empty when nothing is shadowed.
    #[must_use]
    pub fn multi_layer_report(&self) -> Vec<MultiLayerProfile> {
        let mut ids: Vec<String> = self
            .members
            .iter()
            .filter(|member| self.layers_for(&member.id).len() > 1)
            .map(|member| member.id.clone())
            .collect();
        ids.sort_by_key(|id| id.to_lowercase());
        ids.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        ids.into_iter()
            .filter_map(|id| {
                let layers = self.layers_for(&id);
                let winner = layers.iter().find(|layer| layer.wins)?;
                Some(MultiLayerProfile {
                    id,
                    effective: winner.origin,
                    effective_path: winner.source.clone(),
                    layers,
                })
            })
            .collect()
    }

    /// Human doctor lines for multi-layer profile ids: effective layer plus
    /// every observed path. Empty when no id is defined in more than one layer.
    #[must_use]
    pub fn doctor_layer_lines(&self) -> Vec<String> {
        let report = self.multi_layer_report();
        if report.is_empty() {
            return Vec::new();
        }
        let mut lines = Vec::new();
        for entry in report {
            lines.push(format!(
                "{}: effective={} · {}",
                entry.id,
                entry.effective,
                crate::utils::display_path(&entry.effective_path)
            ));
            for layer in &entry.layers {
                let mark = if layer.wins { "wins" } else { "ignored" };
                lines.push(format!(
                    "  {} · {} ({mark})",
                    layer.origin,
                    crate::utils::display_path(&layer.source)
                ));
            }
        }
        lines
    }
}

/// Reconstruct the full layer stack for a member from the winning copy plus
/// every recorded displacement. Used by the roster view (which snapshots
/// members + shadows) and by [`FleetRoster::layers_for`].
#[must_use]
pub fn layers_from_parts(member: &AgentProfile, shadowed: &[ShadowedProfile]) -> Vec<ProfileLayer> {
    let mut layers = vec![ProfileLayer {
        origin: member.origin,
        source: member.source.clone(),
        wins: true,
    }];
    for shadow in shadowed
        .iter()
        .filter(|shadow| shadow.id.trim().eq_ignore_ascii_case(member.id.trim()))
    {
        let already = layers.iter().any(|layer| {
            layer.origin == shadow.shadowed_origin && layer.source == shadow.shadowed_source
        });
        if !already {
            layers.push(ProfileLayer {
                origin: shadow.shadowed_origin,
                source: shadow.shadowed_source.clone(),
                wins: false,
            });
        }
    }
    layers.sort_by(|a, b| match (a.wins, b.wins) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => origin_precedence(b.origin).cmp(&origin_precedence(a.origin)),
    });
    layers
}

/// Fold a displaced layer (if any) into the shadow log.
fn record_shadow(displaced: Option<ShadowedProfile>, shadowed: &mut Vec<ShadowedProfile>) {
    if let Some(shadow) = displaced {
        shadowed.push(shadow);
    }
}

/// Add a lowest-precedence `member` only when no layer already defines its id.
/// A collision keeps the existing member and records the ignored copy.
fn fill_member(
    built_ins: &[AgentProfile],
    extras: &mut Vec<AgentProfile>,
    member: AgentProfile,
) -> Option<ShadowedProfile> {
    let existing = built_ins
        .iter()
        .chain(extras.iter())
        .find(|existing| existing.id.trim().eq_ignore_ascii_case(member.id.trim()));
    match existing {
        Some(existing) => Some(ShadowedProfile {
            id: existing.id.clone(),
            shadowed_origin: member.origin,
            shadowed_source: member.source,
            winner_origin: existing.origin,
            winner_source: existing.source.clone(),
        }),
        None => {
            extras.push(member);
            None
        }
    }
}

/// Overlay `member` onto the roster layers: replace an existing member with
/// the same id (case-insensitive) in place, otherwise collect it as an extra.
/// Returns a shadow record when a lower-precedence layer was displaced so the
/// load can log it and the roster can surface it (#5098).
fn merge_member(
    built_ins: &mut [AgentProfile],
    extras: &mut Vec<AgentProfile>,
    member: AgentProfile,
) -> Option<ShadowedProfile> {
    let matches =
        |existing: &AgentProfile| existing.id.trim().eq_ignore_ascii_case(member.id.trim());
    let slot = built_ins
        .iter_mut()
        .find(|existing| matches(existing))
        .or_else(|| extras.iter_mut().find(|existing| matches(existing)));
    match slot {
        Some(existing) => {
            let shadow = ShadowedProfile {
                id: existing.id.clone(),
                shadowed_origin: existing.origin,
                shadowed_source: existing.source.clone(),
                winner_origin: member.origin,
                winner_source: member.source.clone(),
            };
            *existing = member;
            Some(shadow)
        }
        None => {
            extras.push(member);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    fn config_with_profiles(profiles: BTreeMap<String, FleetProfile>) -> FleetConfigToml {
        FleetConfigToml {
            profiles,
            ..FleetConfigToml::default()
        }
    }

    fn config_profile(role: &str, model: Option<&str>) -> FleetProfile {
        FleetProfile {
            slot: FleetSlot::from_name(role),
            role: FleetRole {
                name: role.to_string(),
                description: Some(format!("{role} from config")),
                instructions: None,
            },
            loadout: FleetLoadout::Inherit,
            model: model.map(str::to_string),
            provider: None,
            reasoning_effort: None,
            permissions: FleetProfilePermissions::default(),
            delegation: FleetDelegationHints::default(),
        }
    }

    fn write_workspace_profile(workspace: &Path, filename: &str, contents: &str) {
        let dir = workspace.join(super::super::profile::WORKSPACE_AGENT_PROFILE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(filename), contents).unwrap();
    }

    /// Removing the built-in `general` member must not make the name stop
    /// resolving. #5888 deliberately kept `general` dispatchable because Agent
    /// tool type tokens, saved configs and replayed transcripts name it; that
    /// contract still holds, now through the selector rather than through a
    /// second member — which is what lets the duplicate go (#6244).
    #[test]
    fn general_still_resolves_to_the_worker_member_without_its_own_built_in() {
        use crate::fleet::identity::resolve_member_in_profiles;
        let members = FleetRoster::built_in_members();
        assert!(
            !members.iter().any(|m| m.id == "general"),
            "this test is only meaningful while `general` has no built-in member"
        );
        for selector in ["general", "member:general", "role:general", "default"] {
            let resolved = resolve_member_in_profiles(&members, selector)
                .unwrap_or_else(|error| panic!("`{selector}` must resolve, got {error:?}"))
                .unwrap_or_else(|| panic!("`{selector}` resolved to no member"));
            assert_eq!(
                resolved.id, "worker",
                "`{selector}` must land on the worker posture"
            );
        }
    }

    /// Two built-ins that collapse to the same canonical role make
    /// `role:<name>` permanently unresolvable: the selector canonicalizes both
    /// sides, matches both members, and raises `Ambiguous` forever. `general`
    /// and `worker` both canonicalized to `general`, which is what blocked
    /// plain agent spawns in production (#6244).
    #[test]
    fn built_in_members_have_one_member_per_canonical_role() {
        use crate::fleet::role::public_role_label;
        let mut seen: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for member in FleetRoster::built_in_members() {
            let role = public_role_label(&member.profile.role.name).to_string();
            if let Some(existing) = seen.insert(role.clone(), member.id.clone()) {
                panic!(
                    "built-ins `{existing}` and `{}` both canonicalize to role `{role}`; \
                     a `role:{role}` selector can never resolve",
                    member.id
                );
            }
        }
    }

    #[test]
    fn built_in_party_is_complete_with_floor_permissions() {
        let members = FleetRoster::built_in_members();
        let ids: Vec<&str> = members.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "manager",
                "operator",
                "scout",
                "builder",
                "reviewer",
                "verifier",
                "consultant",
                "synthesizer",
                "worker",
                "planner",
                "custom"
            ]
        );
        for member in &members {
            assert_eq!(member.origin, ProfileOrigin::BuiltIn, "{}", member.id);
            assert_eq!(
                member.profile.permissions,
                FleetProfilePermissions::default(),
                "built-in {} must stay at the permission floor",
                member.id
            );
            assert_eq!(
                member.profile.delegation,
                FleetDelegationHints::default(),
                "{}",
                member.id
            );
            assert!(member.profile.model.is_none(), "{}", member.id);
            assert_eq!(
                member.profile.reasoning_effort.as_deref(),
                (member.id == "consultant").then_some("high"),
                "built-in {} reasoning",
                member.id
            );
            // The coordination hierarchy (operator/manager) and the
            // adversarial reviewer carry role doctrine; the remaining
            // built-ins get behavior from posture / system prompts alone.
            let carries_doctrine = matches!(
                member.id.as_str(),
                "manager" | "operator" | "reviewer" | "consultant"
            );
            assert_eq!(
                member.profile.role.instructions.is_some(),
                carries_doctrine,
                "built-in {} instructions presence",
                member.id
            );
            assert!(member.description.is_some(), "{}", member.id);
        }
        assert_eq!(members[0].profile.slot, FleetSlot::Manager);
        assert_eq!(members[1].profile.slot, FleetSlot::Operator);
        assert_eq!(members[2].profile.loadout, FleetLoadout::Inherit);
        assert_eq!(members[6].profile.slot.as_str(), "consultant");
        assert_eq!(members[7].profile.slot, FleetSlot::Summarizer);
        assert_eq!(members[7].profile.loadout, FleetLoadout::Inherit);
    }

    /// #5285: there is no dispatch posture the roster cannot see. Every
    /// canonical `type` value the Agent tool accepts resolves to a seeded
    /// roster member, so sub-agent dispatch always has a profile to resolve
    /// through (posture, route, overlay, delegation from one place).
    #[test]
    fn every_canonical_dispatch_posture_is_a_seeded_roster_member() {
        let roster = FleetRoster::built_ins_only();
        for (posture, expected_slot) in [
            ("worker", FleetSlot::General),
            ("scout", FleetSlot::Scout),
            ("planner", FleetSlot::Planner),
            ("reviewer", FleetSlot::Reviewer),
            ("builder", FleetSlot::Implementer),
            ("verifier", FleetSlot::Verifier),
            ("consultant", FleetSlot::Custom("consultant".to_string())),
            ("custom", FleetSlot::Custom("custom".to_string())),
        ] {
            let member = roster.get(posture).unwrap_or_else(|| {
                panic!("dispatch posture {posture:?} must be a seeded roster member")
            });
            assert_eq!(
                member.profile.slot, expected_slot,
                "seeded posture {posture:?} slot"
            );
            assert_eq!(member.origin, ProfileOrigin::BuiltIn, "{posture}");
            // Seeded postures must not carry a pinned route: they inherit the
            // session route exactly like the unnamed default so legacy
            // type-only dispatches keep their model route (#5285).
            assert!(member.profile.model.is_none(), "{posture}");
            assert!(member.profile.provider.is_none(), "{posture}");
            assert_eq!(member.profile.loadout, FleetLoadout::Inherit, "{posture}");
        }
    }

    #[test]
    fn config_member_overrides_built_in_and_extras_sort_alphabetically() {
        let _env_lock = crate::test_support::lock_test_env();
        let home = TempDir::new().unwrap();
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let tmp = TempDir::new().unwrap();
        let config = config_with_profiles(BTreeMap::from([
            (
                "reviewer".to_string(),
                config_profile("reviewer", Some("deepseek-v4-pro")),
            ),
            ("zeta".to_string(), config_profile("scout", None)),
            ("alpha".to_string(), config_profile("builder", None)),
        ]));

        // Isolate from ambient personal agent profiles on developer machines.
        let roster = FleetRoster::load_with_personal_dir(&config, tmp.path(), None, true);

        let ids: Vec<&str> = roster.members().iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "manager",
                "operator",
                "scout",
                "builder",
                "reviewer",
                "verifier",
                "consultant",
                "synthesizer",
                "worker",
                "planner",
                "custom",
                "alpha",
                "zeta"
            ],
            "overridden built-in keeps its slot; extras follow alphabetically"
        );
        let reviewer = roster.get("reviewer").unwrap();
        assert_eq!(reviewer.origin, ProfileOrigin::Config);
        assert_eq!(reviewer.profile.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(reviewer.source, PathBuf::from("config.toml"));
    }

    #[test]
    fn workspace_member_wins_over_config_and_built_in() {
        let tmp = TempDir::new().unwrap();
        write_workspace_profile(
            tmp.path(),
            "reviewer.toml",
            "id = \"reviewer\"\nrole_hint = \"reviewer\"\nmodel = \"glm-5.2\"\n",
        );
        let config = config_with_profiles(BTreeMap::from([(
            "reviewer".to_string(),
            config_profile("reviewer", Some("deepseek-v4-pro")),
        )]));

        let roster = FleetRoster::load(&config, tmp.path());

        let reviewer = roster.get("reviewer").unwrap();
        assert_eq!(reviewer.origin, ProfileOrigin::Workspace);
        assert_eq!(reviewer.profile.model.as_deref(), Some("glm-5.2"));
        // Precedence must not duplicate the member.
        assert_eq!(
            roster
                .members()
                .iter()
                .filter(|m| m.id == "reviewer")
                .count(),
            1
        );
    }

    #[test]
    fn personal_member_applies_across_projects_but_project_still_wins() {
        let tmp = TempDir::new().unwrap();
        let personal_dir = tmp.path().join("personal-agents");
        std::fs::create_dir_all(&personal_dir).unwrap();
        std::fs::write(
            personal_dir.join("reviewer.toml"),
            "id = \"reviewer\"\nrole_hint = \"reviewer\"\nmodel = \"deepseek-v4-flash\"\n",
        )
        .unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let personal = FleetRoster::load_with_personal_dir(
            &FleetConfigToml::default(),
            &workspace,
            Some(&personal_dir),
            true,
        );
        let reviewer = personal.get("reviewer").unwrap();
        assert_eq!(reviewer.origin, ProfileOrigin::Personal);
        assert_eq!(reviewer.profile.model.as_deref(), Some("deepseek-v4-flash"));

        write_workspace_profile(
            &workspace,
            "reviewer.toml",
            "id = \"reviewer\"\nrole_hint = \"reviewer\"\nmodel = \"glm-5.2\"\n",
        );
        let project = FleetRoster::load_with_personal_dir(
            &FleetConfigToml::default(),
            &workspace,
            Some(&personal_dir),
            true,
        );
        let reviewer = project.get("reviewer").unwrap();
        assert_eq!(reviewer.origin, ProfileOrigin::Workspace);
        assert_eq!(reviewer.profile.model.as_deref(), Some("glm-5.2"));
    }

    #[test]
    fn personal_setup_target_round_trips_through_the_runtime_roster() {
        let _env_lock = crate::test_support::lock_test_env();
        let home = TempDir::new().unwrap();
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let workspace = TempDir::new().unwrap();
        let personal_dir = super::super::profile::agent_profile_dir_for_scope(
            super::super::profile::FleetProfileScope::Personal,
            workspace.path(),
        )
        .expect("personal profile directory");
        assert_eq!(personal_dir, home.path().join("agents"));

        let target = personal_dir.join("reviewer.toml");
        let mut transaction = codewhale_config::persistence::SetupTransaction::new();
        transaction.stage(
            target.clone(),
            b"id = \"reviewer\"\nrole_hint = \"reviewer\"\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-flash\"\n"
                .to_vec(),
        );
        transaction.commit().expect("atomic personal save");
        assert!(target.is_file(), "save must land under CODEWHALE_HOME");

        let roster = FleetRoster::load(&FleetConfigToml::default(), workspace.path());
        let reviewer = roster
            .get("reviewer")
            .expect("saved personal profile must be loaded");
        assert_eq!(reviewer.origin, ProfileOrigin::Personal);
        assert_eq!(reviewer.source, target);
        assert_eq!(reviewer.profile.provider.as_deref(), Some("deepseek"));
        assert_eq!(reviewer.profile.model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[test]
    fn broken_workspace_dir_degrades_to_built_ins_and_config() {
        // `load` reads the real personal agent dir under CODEWHALE_HOME; a
        // developer's own extra profiles must not change this assertion.
        let _env_lock = crate::test_support::lock_test_env();
        let isolated_home = TempDir::new().unwrap();
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", isolated_home.path());
        let tmp = TempDir::new().unwrap();
        // A malformed provider token is still a load failure (#4093 / #3965):
        // profile pins may name built-ins or simple custom ids like
        // `lm-studio`, but whitespace/punctuation is rejected so a broken
        // workspace dir still degrades to built-ins + config.
        write_workspace_profile(
            tmp.path(),
            "broken.toml",
            "provider = \"not a real provider\"\n",
        );
        let config = config_with_profiles(BTreeMap::from([(
            "extra".to_string(),
            config_profile("scout", None),
        )]));

        let roster = FleetRoster::load(&config, tmp.path());

        assert!(roster.get("extra").is_some());
        assert_eq!(
            roster.members().len(),
            FleetRoster::built_in_members().len() + 1
        );
    }

    #[test]
    fn invalid_legacy_profile_does_not_hide_valid_scout_neighbor() {
        let _env_lock = crate::test_support::lock_test_env();
        let home = TempDir::new().unwrap();
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let tmp = TempDir::new().unwrap();
        write_workspace_profile(
            tmp.path(),
            "reviewer.toml",
            "id = \"reviewer\"\nmodel_class_hint = \"heavy\"\n",
        );
        write_workspace_profile(
            tmp.path(),
            "scout.toml",
            "id = \"scout\"\nrole_hint = \"scout\"\nprovider = \"deepseek\"\nmodel = \"deepseek-v4-flash\"\n",
        );

        // Isolate from ambient personal agent profiles on developer machines.
        let roster = FleetRoster::load_with_personal_dir(
            &FleetConfigToml::default(),
            tmp.path(),
            None,
            true,
        );

        let scout = roster.get("scout").expect("valid scout remains visible");
        assert_eq!(scout.origin, ProfileOrigin::Workspace);
        assert_eq!(scout.profile.provider.as_deref(), Some("deepseek"));
        assert_eq!(scout.profile.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(
            roster.get("reviewer").unwrap().origin,
            ProfileOrigin::BuiltIn,
            "invalid legacy override must fall back to the safe built-in"
        );
    }

    #[test]
    fn invalid_profile_selection_rejects_fallback_and_respects_layer_precedence() {
        let root = TempDir::new().unwrap();
        let personal = root.path().join("personal");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&personal).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            personal.join("scout.toml"),
            "allow_shell = false\ntrust = false\n",
        )
        .unwrap();
        let load = || {
            FleetRoster::load_with_personal_dir(
                &FleetConfigToml::default(),
                &workspace,
                Some(&personal),
                true,
            )
        };
        let roster = load();
        for selector in [
            "scout",
            "SCOUT",
            "member:scout",
            "id:scout",
            "explore",
            "role:explore",
        ] {
            let error = roster.resolve_member(selector).unwrap_err().to_string();
            assert!(
                error.contains("invalid or unreadable"),
                "{selector}: {error}"
            );
            assert!(error.contains("scout.toml"), "{error}");
        }
        assert!(roster.resolve_member("reviewer").unwrap().is_some());
        assert!(roster.resolve_member("missing").unwrap().is_none());
        write_workspace_profile(&workspace, "scout.toml", "model = \"deepseek-v4-pro\"\n");
        let roster = load();
        let winner = roster.resolve_member("scout").unwrap().unwrap();
        assert_eq!(winner.origin, ProfileOrigin::Workspace);
        assert_eq!(winner.profile.model.as_deref(), Some("deepseek-v4-pro"));
        std::fs::write(
            workspace.join(".codewhale/agents/scout.toml"),
            "broken = [\n",
        )
        .unwrap();
        assert!(load().resolve_member("scout").is_err());
    }

    #[test]
    fn duplicate_and_renamed_invalid_profiles_keep_identity_without_parser_source() {
        let root = TempDir::new().unwrap();
        let dir = root.path().join("personal");
        std::fs::create_dir_all(&dir).unwrap();
        for file in ["first.toml", "second.toml"] {
            std::fs::write(dir.join(file), "id = \"reviewer\"\n").unwrap();
        }
        std::fs::write(
            dir.join("custom.toml"),
            "id = \"renamed\"\nsecret = \"FIXTURE_SECRET_MUST_STAY_IN_LOG\"\n",
        )
        .unwrap();
        let roster = FleetRoster::load_with_personal_dir(
            &FleetConfigToml::default(),
            root.path(),
            Some(&dir),
            false,
        );
        assert!(roster.resolve_member("reviewer").is_err());
        assert!(
            roster
                .resolve_member("renamed")
                .unwrap_err()
                .to_string()
                .contains("custom.toml")
        );
        assert_eq!(roster.profile_load_issues().len(), 3);
        let serialized = serde_json::to_string(roster.profile_load_issues()).unwrap();
        assert!(!serialized.contains("FIXTURE_SECRET_MUST_STAY_IN_LOG"));
        assert!(!serialized.contains("detail"));
        assert!(roster.resolve_member("scout").unwrap().is_some());
    }

    #[test]
    fn model_overrides_use_lowercased_ids_and_only_explicit_models() {
        let _env_lock = crate::test_support::lock_test_env();
        let home = TempDir::new().unwrap();
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        // Isolate personal `$CODEWHALE_HOME/agents` so ambient developer
        // profiles cannot pin built-ins like manager during unit tests.
        let tmp = TempDir::new().unwrap();
        let config = config_with_profiles(BTreeMap::from([
            (
                "Reviewer".to_string(),
                config_profile("reviewer", Some("deepseek-v4-pro")),
            ),
            ("scout".to_string(), config_profile("scout", None)),
        ]));

        let roster = FleetRoster::load(&config, tmp.path());
        let overrides = roster.model_overrides();

        assert_eq!(
            overrides,
            HashMap::from([(
                "reviewer".to_string(),
                crate::config::SubagentModelOverride {
                    provider: None,
                    model: "deepseek-v4-pro".to_string(),
                }
            )]),
            "only members with explicit models are pinned, keyed lowercased"
        );
    }

    /// A profile that pins BOTH a provider and a model must not contribute to
    /// the provider-less `role_models` map. That map is applied against the
    /// session provider's client, so exporting `deepseek-v4-flash` from a
    /// `provider = "deepseek"` scout profile sent a bare DeepSeek id onto an
    /// Alibaba Model Studio session (a pass-through provider, so nothing
    /// downstream rejected it) and the scout died on the provider's denial —
    /// the Model Studio Token Plan roster serves `deepseek-v4-flash-0731`, not
    /// `deepseek-v4-flash`. Provider-pinned profiles keep their model via the
    /// profile spawn path, which builds a client for the pinned provider.
    #[test]
    fn model_overrides_skip_provider_pinned_profiles() {
        let _env_lock = crate::test_support::lock_test_env();
        let home = TempDir::new().unwrap();
        let _codewhale_home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let tmp = TempDir::new().unwrap();

        let mut pinned = config_profile("scout", Some("deepseek-v4-flash"));
        pinned.provider = Some("deepseek".to_string());
        let mut blank_provider = config_profile("builder", Some("deepseek-v4-pro"));
        blank_provider.provider = Some("   ".to_string());

        let config = config_with_profiles(BTreeMap::from([
            ("scout".to_string(), pinned),
            ("builder".to_string(), blank_provider),
        ]));

        let roster = FleetRoster::load(&config, tmp.path());
        let overrides = roster.model_overrides();

        assert!(
            !overrides.contains_key("scout"),
            "a provider-pinned profile must not leak its model into the \
             provider-less role_models map: {overrides:?}"
        );
        assert_eq!(
            overrides.get("builder"),
            Some(&crate::config::SubagentModelOverride {
                provider: None,
                model: "deepseek-v4-pro".to_string(),
            }),
            "a blank provider pin is still provider-less: {overrides:?}"
        );
        // The pin itself survives on the member for the profile spawn path.
        let scout = roster.get("scout").expect("scout member");
        assert_eq!(scout.profile.provider.as_deref(), Some("deepseek"));
        assert_eq!(scout.profile.model.as_deref(), Some("deepseek-v4-flash"));
    }

    #[test]
    fn get_is_trimmed_and_case_insensitive() {
        let roster = FleetRoster::built_ins_only();
        assert!(roster.get("  Reviewer ").is_some());
        assert!(roster.get("SYNTHESIZER").is_some());
        assert!(roster.get("nonexistent").is_none());
    }

    fn write_claude_agent(dir: &Path, filename: &str, contents: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(filename), contents).unwrap();
    }

    #[test]
    fn claude_agents_fill_gaps_at_lowest_precedence() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let project_claude = workspace.join(".claude/agents");
        let user_claude = tmp.path().join("home/.claude/agents");
        write_claude_agent(
            &project_claude,
            "code-reviewer.md",
            "---\nname: code-reviewer\ndescription: Reviews diffs\ntools: Read, Grep, Glob\nmodel: sonnet\ncolor: blue\n---\nYou review code.\n",
        );
        write_claude_agent(
            &project_claude,
            "reviewer.md",
            "---\nname: reviewer\ndescription: would replace the built-in\n---\nNope.\n",
        );
        write_claude_agent(
            &project_claude,
            "bypass.md",
            "---\nname: bypass\ndescription: x\npermissionMode: bypassPermissions\n---\nbody\n",
        );
        write_claude_agent(
            &user_claude,
            "code-reviewer.md",
            "---\nname: code-reviewer\ndescription: personal copy\n---\nPersonal.\n",
        );
        write_claude_agent(
            &user_claude,
            "test-writer.md",
            "---\nname: test-writer\ndescription: Writes tests\ntools: Read, Write, Bash\n---\nWrite tests.\n",
        );
        write_workspace_profile(
            &workspace,
            "writer.toml",
            "id = \"writer\"\nbase_role = \"implement\"\n",
        );

        let roster = FleetRoster::load_with_personal_dir_and_plugins(
            &FleetConfigToml::default(),
            &workspace,
            None,
            Some(&user_claude),
            true,
            None,
        );

        let reviewer = roster.get("code-reviewer").expect("project Claude agent");
        assert_eq!(reviewer.origin, ProfileOrigin::ClaudeCode);
        assert_eq!(reviewer.source, project_claude.join("code-reviewer.md"));
        assert_eq!(reviewer.description.as_deref(), Some("Reviews diffs"));
        assert_eq!(reviewer.profile.role.name, "explore");
        assert_eq!(
            reviewer.profile.role.instructions.as_deref(),
            Some("You review code.")
        );
        assert_eq!(reviewer.profile.model, None, "Claude model aliases inherit");
        assert_eq!(
            reviewer.profile.permissions,
            FleetProfilePermissions::default()
        );

        let writer = roster.get("test-writer").expect("user Claude agent");
        assert_eq!(writer.profile.role.name, "implement");
        assert!(!writer.profile.permissions.allow_shell);
        assert!(!writer.profile.permissions.trust);

        // Built-ins and Codewhale files always win; the Claude copy is logged.
        let builtin = roster.get("reviewer").unwrap();
        assert_eq!(builtin.origin, ProfileOrigin::BuiltIn);
        assert!(
            roster
                .shadowed()
                .iter()
                .any(|shadow| shadow.id == "reviewer"
                    && shadow.shadowed_origin == ProfileOrigin::ClaudeCode
                    && shadow.winner_origin == ProfileOrigin::BuiltIn)
        );
        assert!(
            roster
                .shadowed()
                .iter()
                .any(|shadow| shadow.id == "code-reviewer"
                    && shadow.shadowed_source == user_claude.join("code-reviewer.md"))
        );
        assert_eq!(
            roster.get("writer").unwrap().origin,
            ProfileOrigin::Workspace
        );

        // Unmapped frontmatter is a visible load issue, not a silent load.
        assert!(roster.get("bypass").is_none());
        assert!(roster.resolve_member("bypass").is_err());
        assert!(
            roster
                .profile_load_issues()
                .iter()
                .any(|issue| issue.id == "bypass" && issue.detail.contains("permissionmode"))
        );
        // A lowest-precedence failure never blocks a higher layer.
        assert!(roster.resolve_member("reviewer").unwrap().is_some());
    }

    #[test]
    fn untrusted_project_skips_project_claude_agents_only() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("ws");
        let user_claude = tmp.path().join("home/.claude/agents");
        write_claude_agent(
            &workspace.join(".claude/agents"),
            "project-agent.md",
            "---\nname: project-agent\ndescription: p\n---\nbody\n",
        );
        write_claude_agent(
            &user_claude,
            "user-agent.md",
            "---\nname: user-agent\ndescription: u\n---\nbody\n",
        );
        let roster = FleetRoster::load_with_personal_dir_and_plugins(
            &FleetConfigToml::default(),
            &workspace,
            None,
            Some(&user_claude),
            false,
            None,
        );
        assert!(roster.get("project-agent").is_none());
        assert!(roster.get("user-agent").is_some());
    }

    #[test]
    fn origin_labels_are_stable() {
        assert_eq!(ProfileOrigin::ClaudeCode.to_string(), "claude");
        assert_eq!(ProfileOrigin::BuiltIn.to_string(), "built-in");
        assert_eq!(ProfileOrigin::Config.to_string(), "config");
        assert_eq!(ProfileOrigin::Personal.to_string(), "personal");
        assert_eq!(ProfileOrigin::Workspace.to_string(), "project");
    }
}

#[cfg(test)]
#[path = "tests/roster_shadow_and_trust.rs"]
mod shadow_and_trust_tests;

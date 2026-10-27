//! Fleet worker runtime — bridges Fleet task specs to headless sub-agent execution.
//!
//! This module makes Fleet workers real: instead of simulating task completion,
//! each Fleet worker spawns a headless sub-agent that runs the task instructions
//! and streams progress back into the Fleet ledger.
//!
//! Architecture:
//! - `FleetTaskSpec` + `FleetWorkerSpec` → `AgentWorkerSpec`
//! - `SubAgentManager::register_worker()` tracks the worker
//! - Sub-agent spawn happens through the existing `agent` machinery
//! - Mailbox events stream into the Fleet ledger as `FleetWorkerEventPayload`
//! - `FleetWorkerInspection` reads both ledger state and sub-agent worker records

#![allow(dead_code)]

use std::borrow::Cow;
use std::path::PathBuf;

use anyhow::{Result, bail};
use codewhale_protocol::fleet::{
    FleetEffectivePermissions, FleetResolvedRoute, FleetTaskSpec, FleetTaskWorkerProfile,
    FleetWorkerSpec,
};
use serde::{Deserialize, Serialize};

use super::identity::{FleetSelectorError, resolve_member_in_profiles};
use super::profile::FleetRole as FleetProfileRole;
use super::profile::{
    AgentProfile, FleetDelegationHints, FleetLoadout, FleetProfile, FleetProfilePermissions,
    FleetSlot, ProfileOrigin, canonical_public_role_name,
};
use crate::route_runtime::resolve_route_candidate;
use crate::tools::subagent::{AgentWorkerSpec, AgentWorkerToolProfile};
use codewhale_agent_contract::role::FleetRole;
use codewhale_agent_contract::role::runtime_role_for_member;
use codewhale_agent_contract::worker_profile::{
    ChildLaunchManifest, ModelRoute, ToolScope, WorkerRuntimeProfile,
};
use codewhale_config::ProviderKind;
use codewhale_runtime::config::Config;

/// Reserved durable task metadata written after author input is validated.
///
/// Keeping the selected identity snapshot inside the already-durable task
/// record prevents a queued run or retry from silently changing member/model
/// when a profile file is edited after run creation.
pub(crate) const FROZEN_FLEET_MEMBER_METADATA_KEY: &str = "_codewhale.frozen_fleet_member.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct FrozenFleetMember {
    schema_version: u32,
    id: String,
    display_name: Option<String>,
    description: Option<String>,
    requires: Vec<String>,
    slot: String,
    role: String,
    role_description: Option<String>,
    role_instructions: Option<String>,
    loadout: String,
    provider: Option<String>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    max_spawn_depth: Option<u32>,
    origin: ProfileOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    native_preset: Option<crate::extension_host::composition_scope::NativePresetRef>,
}

impl FrozenFleetMember {
    fn from_profile(profile: &AgentProfile) -> Self {
        Self {
            schema_version: 1,
            id: profile.id.clone(),
            display_name: profile.display_name.clone(),
            description: profile.description.clone(),
            requires: profile.requires.clone(),
            slot: profile.profile.slot.as_str().to_string(),
            role: canonical_public_role_name(&profile.profile.role.name),
            role_description: profile.profile.role.description.clone(),
            role_instructions: profile.profile.role.instructions.clone(),
            loadout: profile.profile.loadout.as_str().to_string(),
            provider: profile.profile.provider.clone(),
            model: profile.profile.model.clone(),
            reasoning_effort: profile.profile.reasoning_effort.clone(),
            max_spawn_depth: profile.profile.delegation.max_spawn_depth,
            origin: profile.origin,
            native_preset: profile.native_preset.clone(),
        }
    }

    fn into_profile(self) -> Result<AgentProfile> {
        if self.schema_version != 1 || self.id.trim().is_empty() || self.role.trim().is_empty() {
            bail!("invalid frozen Fleet member snapshot");
        }
        Ok(AgentProfile {
            id: self.id,
            native_preset: self.native_preset,
            display_name: self.display_name,
            description: self.description,
            requires: self.requires,
            profile: FleetProfile {
                slot: FleetSlot::from_name(&self.slot),
                role: FleetProfileRole {
                    name: self.role,
                    description: self.role_description,
                    instructions: self.role_instructions,
                },
                loadout: FleetLoadout::from_name(&self.loadout),
                model: self.model,
                provider: self.provider,
                reasoning_effort: self.reasoning_effort,
                // Authority is always derived from live Runtime policy. A
                // Fleet identity snapshot cannot persist or grant it.
                permissions: FleetProfilePermissions::default(),
                delegation: FleetDelegationHints {
                    max_spawn_depth: self.max_spawn_depth,
                    max_concurrency: None,
                },
            },
            source: PathBuf::from("<durable-fleet-member>"),
            origin: self.origin,
            plugin_authority: None,
        })
    }
}

/// Validate that every task referencing a workspace agent profile can resolve it.
///
/// This is intended to run at Fleet run creation time, before leasing any
/// worker or appending lifecycle events.
pub fn validate_task_agent_profiles(
    tasks: &[FleetTaskSpec],
    agent_profiles: &[AgentProfile],
) -> Result<()> {
    for task in tasks {
        resolve_task_agent_profile(task, agent_profiles)?;
    }
    Ok(())
}

/// Resolve and freeze every deterministic member selection before persistence.
///
/// Author-facing selectors may be ids, names, semantic roles, model labels, or
/// exact routes. The durable task stores the selected member's canonical id and
/// the identity/route inputs used for launch. Legacy `worker.role` remains a
/// posture when it does not name exactly one member; explicit
/// `worker.agent_profile` selectors fail closed when unknown or ambiguous, and
/// when the task's `worker.role` names a posture other than the selected
/// member's role.
pub(crate) fn freeze_fleet_task_members(
    tasks: &mut [FleetTaskSpec],
    agent_profiles: &[AgentProfile],
    require_exact_member: bool,
) -> Result<()> {
    for task in tasks {
        let Some(worker) = task.worker.as_ref() else {
            if require_exact_member {
                bail!(
                    "Fleet task {} must name one member from the explicitly selected Fleet",
                    task.id
                );
            }
            continue;
        };
        let explicit_selector = worker
            .agent_profile
            .as_deref()
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
            .map(str::to_string);
        let legacy_role_selector = worker
            .role
            .as_deref()
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
            .map(str::to_string);

        let selected = if let Some(selector) = explicit_selector.as_deref() {
            let selected = resolve_member_in_profiles(agent_profiles, selector).map_err(|error| {
                anyhow::anyhow!(
                    "Fleet task {} has invalid worker.agent_profile selector {selector:?}: {error}",
                    task.id
                )
            })?;
            Some(selected.ok_or_else(|| {
                anyhow::anyhow!(
                    "Fleet task {} references unknown agent profile selector {selector:?}",
                    task.id
                )
            })?)
        } else if let Some(selector) = legacy_role_selector.as_deref() {
            // `role` was historically only a posture. Preserve that contract
            // when no exact Fleet is selected and a roster lookup is absent.
            // Ambiguity is never a posture: silently dropping it would make a
            // selected team launch an anonymous session-route worker.
            match resolve_member_in_profiles(agent_profiles, selector).map_err(|error| {
                anyhow::anyhow!(
                    "Fleet task {} has invalid worker.role member selector {selector:?}: {error}",
                    task.id
                )
            })? {
                Some(profile) => Some(profile),
                None if require_exact_member => {
                    bail!(
                        "Fleet task {} worker.role selector {selector:?} does not name a member in the explicitly selected Fleet",
                        task.id
                    )
                }
                None => None,
            }
        } else if require_exact_member {
            bail!(
                "Fleet task {} must name one member from the explicitly selected Fleet",
                task.id
            );
        } else {
            None
        };

        if let Some(profile) = selected {
            validate_selected_member_model(task, profile)?;
            let snapshot = FrozenFleetMember::from_profile(profile);
            // A task carries exactly one posture. When an explicit member
            // selector is present, `worker.role` may only restate that
            // member's role (any casing or legacy alias); naming a different
            // posture is an authoring error, not a tie to arbitrate later.
            // Failing closed here is what keeps the launch-time resolver
            // honest: a read-only label can never widen to a member's write
            // authority, and a member's read-only slot can never be widened by
            // a write-capable label (#5945).
            if explicit_selector.is_some()
                && let Some(label) = legacy_role_selector.as_deref()
            {
                let label = canonical_public_role_name(label);
                if label != snapshot.role {
                    bail!(
                        "Fleet task {} selects member {:?} whose role is {:?}, but worker.role names a different posture {:?}; a task has one posture — drop worker.role or select a member with that role",
                        task.id,
                        profile.id,
                        snapshot.role,
                        label
                    );
                }
            }
            task.metadata.insert(
                FROZEN_FLEET_MEMBER_METADATA_KEY.to_string(),
                serde_json::to_value(&snapshot)?,
            );
            let worker = task.worker.as_mut().expect("worker checked above");
            worker.agent_profile = Some(format!("member:{}", profile.id));
            if explicit_selector.is_none() {
                worker.role = Some(snapshot.role.clone());
            } else if let Some(role) = worker.role.as_mut() {
                *role = canonical_public_role_name(role.trim());
            }
        } else if let Some(role) = task.worker.as_mut().and_then(|worker| worker.role.as_mut()) {
            *role = canonical_public_role_name(role.trim());
        }
    }
    Ok(())
}

/// Validate that every task's pinned model route actually resolves before any
/// worker is leased (#4866).
///
/// Catches the "provider-less model pin" failure mode: a profile that pins a
/// concrete model without an explicit provider resolves against the
/// session/default provider, which may not carry that model — causing a silent
/// launch failure (e.g. selecting `gpt-5.6-luna` as a Fleet Builder model with
/// no provider, when Luna lives on a different configured provider). The
/// runtime never infers a provider from a model's spelling (#4093/#2608), so a
/// pinned model that does not resolve is rejected here with a clear error
/// instead of failing silently inside the worker. Every task must have either
/// the resolved session config or an explicit profile provider; inherited
/// session/run models are validated within that already-authorized scope.
pub fn validate_fleet_task_routes(
    tasks: &[FleetTaskSpec],
    agent_profiles: &[AgentProfile],
    session_model: Option<&str>,
    config: Option<&Config>,
) -> Result<()> {
    let run_model = session_model.unwrap_or("auto");
    for task in tasks {
        let agent_profile = resolve_task_agent_profile(task, agent_profiles)?;
        let agent_profile = agent_profile.as_deref();
        let (model, source) =
            effective_fleet_model_with_source(run_model, task.worker.as_ref(), agent_profile);
        let pinned_model = matches!(source, "task.model" | "agent_profile.model");
        let explicit_provider = explicit_fleet_provider_id(agent_profile);
        if config.is_none() && explicit_provider.is_none() {
            bail!(
                "Fleet task `{}` has no provider authority for model `{model}` (source={source}); attach the resolved route config or set the agent profile provider explicitly",
                task.id,
            );
        }
        if config.is_none()
            && let Some(provider_id) = explicit_provider.as_deref()
            && ProviderKind::parse(provider_id)
                .is_none_or(|provider| provider == ProviderKind::Custom)
        {
            bail!(
                "Fleet task `{}` names custom provider=`{provider_id}`, but a provider name alone does not prove its endpoint or model; attach the live route config before creating the run",
                task.id,
            );
        }
        if pinned_model && explicit_provider.is_none() {
            let config = config.expect("provider authority checked above");
            let identity = config
                .active_provider_identity()
                .map_err(anyhow::Error::msg)?;
            let (provider, base_url) = (identity.provider, config.base_url_for_route(&identity));
            if let Err(reason) =
                crate::route_runtime::validate_unpinned_model_provider(provider, &model, &base_url)
            {
                bail!("Fleet task `{}`: {reason} (source={source})", task.id);
            }
        }

        let route = resolve_fleet_route_with_config(task, agent_profiles, session_model, config);
        let provider = explicit_provider
            .map(|provider| format!("provider=`{provider}`"))
            .unwrap_or_else(|| {
                "no explicit provider (resolves against the session/default provider)".to_string()
            });
        if route.is_none() {
            if pinned_model {
                bail!(
                    "Fleet task `{}` pins model `{}` with {} (source={source}), but that route does not \
                     resolve to a real model on any configured provider, so the worker cannot launch. \
                     The runtime never infers a provider from a model's spelling — set an explicit \
                     provider for this model in the profile, or switch the role to `inherit`.",
                    task.id,
                    model,
                    provider
                );
            }
            bail!(
                "Fleet task `{}` cannot resolve its inherited model `{model}` with {provider} \
                 (source={source}); attach the live route config for custom providers before \
                 creating the run",
                task.id,
            );
        }
        validate_fleet_reasoning_effort(task, agent_profiles, session_model, config)?;
    }
    Ok(())
}

/// Reject an explicit Fleet thinking tier when the exact resolved route does
/// not advertise reasoning support. `inherit`, `auto`, and `off` are valid on
/// every route because they do not force a reasoning payload. This check is
/// deliberately performed at run creation, after the same route resolver used
/// for launch, so the UI cannot save a profile that will silently downgrade or
/// fail at spawn time (#4866).
fn validate_fleet_reasoning_effort(
    task: &FleetTaskSpec,
    agent_profiles: &[AgentProfile],
    session_model: Option<&str>,
    config: Option<&Config>,
) -> Result<()> {
    let agent_profile = resolve_task_agent_profile(task, agent_profiles)?;
    let agent_profile = agent_profile.as_deref();
    let Some(effort) =
        effective_fleet_reasoning_effort_for_role(task.worker.as_ref(), agent_profile)
    else {
        return Ok(());
    };
    if matches!(effort.as_str(), "inherit" | "auto" | "off") {
        return Ok(());
    }
    let Some(route) = resolve_fleet_route_with_config(task, agent_profiles, session_model, config)
    else {
        // The model-route validator owns unresolved-route errors and produces
        // the more useful provider/model diagnosis.
        return Ok(());
    };
    let provider = ProviderKind::parse(&route.provider_kind).unwrap_or(ProviderKind::Custom);
    let capability = crate::provider_lake::provider_capability(provider, &route.wire_model_id);
    if capability.thinking_supported {
        return Ok(());
    }
    bail!(
        "Fleet task `{}` requests thinking tier `{effort}` for `{}` / `{}`, but that exact model route does not support thinking; choose inherit, auto, or off, or select a reasoning-capable model",
        task.id,
        route.provider_id,
        route.wire_model_id,
    );
}

/// Build a sub-agent worker spec after resolving workspace Fleet profile input.
///
/// This keeps Fleet and sub-agents on the same runtime substrate: profile files
/// and task-level role/loadout intent are composed into the existing
/// `AgentWorkerSpec` / `WorkerRuntimeProfile` pair, then optionally intersected
/// with a parent profile when the caller has one.
/// A worker workspace is isolated when it is a linked git worktree that sits
/// outside the coordinating manager's workspace (and does not contain it):
/// its mutations cannot overlap the shared checkout, so its launch manifest
/// must not claim the shared-workspace coordination scope (#5036).
fn worker_workspace_is_isolated(
    coordination_workspace: &std::path::Path,
    worker_workspace: &std::path::Path,
) -> bool {
    let canonical =
        |path: &std::path::Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let manager = canonical(coordination_workspace);
    let worker = canonical(worker_workspace);
    if worker == manager || worker.starts_with(&manager) || manager.starts_with(&worker) {
        return false;
    }
    worker.join(".git").is_file()
}

#[allow(clippy::too_many_arguments)]
pub fn fleet_task_to_worker_spec_with_profiles(
    worker_id: &str,
    run_id: &str,
    task_spec: &FleetTaskSpec,
    worker_spec: &FleetWorkerSpec,
    model: &str,
    workspace: &std::path::Path,
    coordination_workspace: &std::path::Path,
    agent_profiles: &[AgentProfile],
    parent_runtime_profile: Option<&WorkerRuntimeProfile>,
) -> Result<AgentWorkerSpec> {
    let agent_profile = resolve_task_agent_profile(task_spec, agent_profiles)?;
    let agent_profile = agent_profile.as_deref();
    let worker_profile = task_spec.worker.as_ref();
    let role = effective_fleet_role(worker_profile, agent_profile);
    let agent_type = runtime_role_for_member(role.as_deref().unwrap_or_default());
    let tool_profile = fleet_tool_profile(worker_profile);
    super::placement::require_match(task_spec, worker_spec)?;
    let mut objective = fleet_task_prompt_with_profile(task_spec, agent_profile);
    if worker_spec.environment.is_some()
        || task_spec
            .workspace
            .as_ref()
            .is_some_and(|workspace| workspace.placement.is_some())
        || matches!(
            worker_spec.host,
            codewhale_protocol::fleet::FleetHostSpec::Tailscale { .. }
        )
    {
        objective.push_str("\n\nExecution environment (operator-declared context; runtime permissions still apply):\n");
        objective.push_str(&serde_json::to_string(&super::placement::environment(
            worker_spec,
        ))?);
    }
    let max_spawn_depth = codewhale_config::FleetExecConfig::default().max_spawn_depth;
    let loadout = effective_fleet_loadout(worker_profile, agent_profile);
    let (effective_model, model_source) =
        effective_fleet_model_with_source(model, worker_profile, agent_profile);
    let mut requested_runtime = fleet_worker_runtime_profile_for_loadout(
        &agent_type,
        &tool_profile,
        &effective_model,
        0,
        max_spawn_depth,
        &loadout,
        model_source,
    );
    requested_runtime.provider = explicit_fleet_provider_id(agent_profile);
    if let Some(reasoning_effort) = effective_fleet_reasoning_effort(agent_profile) {
        requested_runtime.reasoning_effort = Some(reasoning_effort);
    }
    if let Some(agent_profile) = agent_profile
        && let Some(profile_depth) = agent_profile.profile.delegation.max_spawn_depth
    {
        requested_runtime.max_spawn_depth = requested_runtime.max_spawn_depth.min(profile_depth);
    }
    let runtime_profile = parent_runtime_profile
        .map(|parent| parent.derive_child(&requested_runtime))
        .unwrap_or(requested_runtime);
    let writable_roots = fleet_write_roots(task_spec)?;
    let coordination_contracts = fleet_coordination_contracts(task_spec)?;
    if runtime_profile.permissions.write
        && writable_roots.is_empty()
        && coordination_contracts.is_empty()
    {
        bail!(
            "Fleet task '{}' is write-capable but declares no workspace.writable_paths or metadata.coordination_contracts",
            task_spec.id
        );
    }
    let session_name = format!("fleet-{}-{}", worker_id, task_spec.id);
    let launch_manifest = ChildLaunchManifest {
        owner_session: run_id.to_string(),
        child_id: worker_id.to_string(),
        profile: runtime_profile.clone(),
        prompt: objective.clone(),
        cwd: Some(workspace.display().to_string()),
        worktree: worker_workspace_is_isolated(coordination_workspace, workspace),
        writable_roots,
        writable_files: Vec::new(),
        coordination_contracts,
        expected_artifact: None,
        deliverables: Vec::new(),
        resume_identity: Some(session_name.clone()),
        generation: 1,
        resume_from_agent_id: None,
    };

    let max_steps = task_spec
        .budget
        .as_ref()
        .and_then(|budget| budget.max_steps)
        .unwrap_or(0);

    Ok(AgentWorkerSpec {
        worker_id: worker_id.to_string(),
        run_id: run_id.to_string(),
        parent_run_id: None,
        workflow_run_id: None,
        session_name: Some(session_name),
        objective,
        role,
        agent_type,
        model: effective_model,
        workspace: workspace.to_path_buf(),
        git_branch: None,
        context_mode: "fresh".to_string(),
        fork_context: false,
        tool_profile,
        runtime_profile: runtime_profile.clone(),
        max_steps,
        spawn_depth: runtime_profile.spawn_depth,
        max_spawn_depth: runtime_profile.max_spawn_depth,
        child_route: None,
        launch_manifest: Some(launch_manifest),
    })
}

pub(crate) fn fleet_write_roots(task_spec: &FleetTaskSpec) -> Result<Vec<String>> {
    let task_root = normalize_fleet_relative_path(
        task_spec
            .workspace
            .as_ref()
            .and_then(|workspace| workspace.root.as_deref())
            .unwrap_or_else(|| std::path::Path::new(".")),
        &task_spec.id,
        "workspace.root",
    )?;
    let mut roots = Vec::new();
    for runtime_root in fleet_runtime_write_roots(task_spec)? {
        let claim_root = match (task_root.as_str(), runtime_root.as_str()) {
            (".", path) | (path, ".") => path.to_string(),
            (root, path) => format!("{root}/{path}"),
        };
        if !roots.contains(&claim_root) {
            roots.push(claim_root);
        }
    }
    Ok(roots)
}

pub(crate) fn fleet_runtime_write_roots(task_spec: &FleetTaskSpec) -> Result<Vec<String>> {
    let mut roots = Vec::new();
    for path in task_spec
        .workspace
        .as_ref()
        .into_iter()
        .flat_map(|workspace| &workspace.writable_paths)
    {
        let normalized =
            normalize_fleet_relative_path(path, &task_spec.id, "workspace.writable_paths")?;
        if !roots.contains(&normalized) {
            roots.push(normalized);
        }
    }
    Ok(roots)
}

fn normalize_fleet_relative_path(
    path: &std::path::Path,
    task_id: &str,
    field: &str,
) -> Result<String> {
    let raw = path.to_string_lossy().replace('\\', "/");
    if raw.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        bail!(
            "Fleet task '{task_id}' {field} path '{}' must be one repo-relative line and cannot escape the workspace",
            path.display()
        );
    }
    let mut segments = Vec::new();
    for segment in raw.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                bail!(
                    "Fleet task '{task_id}' {field} path '{}' cannot contain parent traversal",
                    path.display()
                );
            }
            value => segments.push(value),
        }
    }
    Ok(if segments.is_empty() {
        ".".to_string()
    } else {
        segments.join("/")
    })
}

fn fleet_coordination_contracts(task_spec: &FleetTaskSpec) -> Result<Vec<String>> {
    let Some(value) = task_spec.metadata.get("coordination_contracts") else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        bail!(
            "Fleet task '{}' metadata.coordination_contracts must be an array of strings",
            task_spec.id
        );
    };
    if values.len() > 16 {
        bail!(
            "Fleet task '{}' metadata.coordination_contracts accepts at most 16 entries",
            task_spec.id
        );
    }
    let mut contracts = Vec::new();
    for value in values {
        let Some(value) = value.as_str() else {
            bail!(
                "Fleet task '{}' metadata.coordination_contracts must contain only strings",
                task_spec.id
            );
        };
        let value = value.trim();
        if value.is_empty()
            || value.chars().count() > 128
            || value.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n'))
        {
            bail!(
                "Fleet task '{}' coordination contracts must be one non-empty line of at most 128 characters",
                task_spec.id
            );
        }
        if !contracts.iter().any(|contract| contract == value) {
            contracts.push(value.to_string());
        }
    }
    Ok(contracts)
}

/// Mint a [`FleetResolvedRoute`] snapshot for a fleet task (#3154).
///
/// This calls the existing hermetic resolver bridge
/// ([`resolve_route_candidate`]) so the persisted route reflects the same
/// resolution semantics the runtime would use, then records only non-sensitive
/// shape (provider id/kind, model ids, protocol) combined with the already
/// computed effective role/loadout/model-class intent. `source` is
/// `"resolver"`.
///
/// Honesty rules:
/// - `canonical_model` stays `None` when the resolver could not pin one.
/// - The provider comes from the resolved agent profile's own explicit
///   `provider` field when it has one (#4093) — a Fleet worker profile can be
///   pinned to a route independent of the parent/current session provider.
///   Absent an explicit pin, the worker profile carries no provider authority;
///   callers must supply the resolved live [`Config`]. The provider is NEVER
///   inferred by sniffing a substring/prefix out of `model` (EPIC #2608:
///   explicit config only). A task-level `model` selector is forwarded as the
///   model selector. No reasoning/pricing fields are fabricated.
///
/// Returns `None` (never a fabricated route) when resolution fails, so callers
/// degrade gracefully without inventing detail.
pub(crate) fn resolve_fleet_route(
    task_spec: &FleetTaskSpec,
    agent_profiles: &[AgentProfile],
    session_model: Option<&str>,
) -> Option<FleetResolvedRoute> {
    resolve_fleet_route_with_config(task_spec, agent_profiles, session_model, None)
}

/// Resolve a Fleet receipt from the same live Config used to launch workers.
/// Named custom identities are emitted only through this proof-bearing path;
/// the hermetic fallback above cannot truthfully validate arbitrary ids.
pub(crate) fn resolve_fleet_route_with_config(
    task_spec: &FleetTaskSpec,
    agent_profiles: &[AgentProfile],
    session_model: Option<&str>,
    config: Option<&Config>,
) -> Option<FleetResolvedRoute> {
    let agent_profile = resolve_task_agent_profile(task_spec, agent_profiles)
        .ok()
        .flatten();
    let agent_profile = agent_profile.as_deref();
    let worker_profile = task_spec.worker.as_ref();
    let (role, role_source) = effective_fleet_role_with_source(worker_profile, agent_profile);
    let (loadout, loadout_source) =
        effective_fleet_loadout_with_source(worker_profile, agent_profile);
    let (model_class, model_class_source) = task_model_class_with_source(worker_profile);

    // Task/profile model pins are visible route intent; next the session
    // route (the operator's model) applies as the run-level fallback; only
    // then does the resolver pick the provider default.
    let (model_selector, model_source) =
        fleet_route_model_selector_with_source(worker_profile, agent_profile, session_model);
    let model_selector = model_selector.as_deref();

    let explicit_provider_id = explicit_fleet_provider_id(agent_profile);
    let (candidate, provider_id, provider_exact_id, route_source) = if let Some(config) = config {
        let identity = match explicit_provider_id.as_deref() {
            Some(provider_id) => config.resolve_provider_identity(provider_id).ok()?,
            None => config.active_provider_identity().ok()?,
        };
        let route = crate::route_runtime::resolve_runtime_route_for_identity(
            config,
            &identity,
            model_selector,
        )
        .ok()?
        .validate()
        .ok()?;
        let provider_exact_id = route.identity.persisted_id().map(str::to_string);
        (
            route.candidate,
            route.identity.key.to_string(),
            provider_exact_id,
            "runtime_route",
        )
    } else {
        let provider_id = explicit_provider_id.as_deref()?;
        let metadata = codewhale_config::descriptors::compatibility_for_selector(provider_id)?;
        let provider = metadata.kind;
        if provider == ProviderKind::Custom {
            return None;
        }
        let candidate = resolve_route_candidate(
            provider,
            model_selector,
            Some(metadata.default_model),
            Some(metadata.base_url.to_owned()),
            None,
            None,
        )
        .ok()?;
        let provider_id = candidate.provider_id().as_str().to_string();
        (candidate, provider_id, None, "resolver")
    };

    Some(FleetResolvedRoute {
        provider_id,
        provider_exact_id,
        provider_kind: candidate.provider_kind().as_str().to_string(),
        canonical_model: candidate
            .canonical_model()
            .as_ref()
            .map(|model| model.as_str().to_string()),
        wire_model_id: candidate.wire_model_id().as_str().to_string(),
        protocol: route_protocol_label(candidate.protocol()).to_string(),
        role,
        loadout: loadout_intent_label(&loadout),
        model_class,
        model_route: Some(
            model_route_label(&fleet_model_route_for_loadout(
                model_selector.unwrap_or("auto"),
                &loadout,
            ))
            .to_string(),
        ),
        reasoning_effort: effective_fleet_reasoning_effort_for_role(worker_profile, agent_profile),
        role_source: role_source.map(str::to_string),
        loadout_source: loadout_source.map(str::to_string),
        model_class_source: model_class_source.map(str::to_string),
        model_source: Some(model_source.to_string()),
        source: route_source.to_string(),
    })
}

/// Build the receipt route from route identity reported by the worker itself.
///
/// Provider/model fields in this path are process-boundary evidence, not a
/// second resolution attempt in the manager's potentially different config.
/// Fleet task/profile fields remain intent metadata and are safe to derive
/// locally. Protocol and canonical model stay explicitly unreported because
/// the current exec terminal envelope does not carry them.
pub(crate) fn resolve_fleet_route_from_worker_report(
    task_spec: &FleetTaskSpec,
    agent_profiles: &[AgentProfile],
    session_model: Option<&str>,
    provider: &str,
    provider_exact_id: Option<&str>,
    model: &str,
) -> Option<FleetResolvedRoute> {
    let provider = non_empty_trimmed(provider)?;
    let model = non_empty_trimmed(model)?;
    let provider_exact_id = match provider_exact_id {
        Some(provider_exact_id) => Some(non_empty_trimmed(provider_exact_id)?),
        None => None,
    };
    let provider_kind = ProviderKind::parse(provider)?;
    if provider_exact_id.is_some() && provider_kind != ProviderKind::Custom {
        return None;
    }
    let provider_id = provider_exact_id.unwrap_or(provider);
    let agent_profile = resolve_task_agent_profile(task_spec, agent_profiles)
        .ok()
        .flatten();
    let agent_profile = agent_profile.as_deref();
    let worker_profile = task_spec.worker.as_ref();
    let (role, role_source) = effective_fleet_role_with_source(worker_profile, agent_profile);
    let (loadout, loadout_source) =
        effective_fleet_loadout_with_source(worker_profile, agent_profile);
    let (model_class, model_class_source) = task_model_class_with_source(worker_profile);
    let (model_selector, model_source) =
        fleet_route_model_selector_with_source(worker_profile, agent_profile, session_model);
    Some(FleetResolvedRoute {
        provider_id: provider_id.to_string(),
        provider_exact_id: provider_exact_id.map(str::to_string),
        provider_kind: provider_kind.as_str().to_string(),
        canonical_model: None,
        wire_model_id: model.to_string(),
        protocol: "unreported".to_string(),
        role,
        loadout: loadout_intent_label(&loadout),
        model_class,
        model_route: Some(
            model_route_label(&fleet_model_route_for_loadout(
                model_selector.as_deref().unwrap_or("auto"),
                &loadout,
            ))
            .to_string(),
        ),
        reasoning_effort: effective_fleet_reasoning_effort_for_role(worker_profile, agent_profile),
        role_source: role_source.map(str::to_string),
        loadout_source: loadout_source.map(str::to_string),
        model_class_source: model_class_source.map(str::to_string),
        model_source: Some(model_source.to_string()),
        source: "worker_terminal_metadata".to_string(),
    })
}

/// Plain-string label for a resolved wire protocol (no config type leaks).
fn route_protocol_label(protocol: codewhale_config::route::RequestProtocol) -> &'static str {
    use codewhale_config::route::RequestProtocol;
    match protocol {
        RequestProtocol::ChatCompletions => "chat_completions",
        RequestProtocol::Responses => "responses",
        RequestProtocol::AnthropicMessages => "anthropic_messages",
    }
}

/// Collapse an `inherit` (no-op) loadout to `None` for the receipt.
fn loadout_intent_label(loadout: &codewhale_config::FleetLoadout) -> Option<String> {
    if *loadout == codewhale_config::FleetLoadout::Inherit {
        None
    } else {
        Some(loadout.as_str().to_string())
    }
}

fn model_route_label(route: &ModelRoute) -> &'static str {
    match route {
        ModelRoute::Inherit => "inherit",
        ModelRoute::Faster => "faster",
        ModelRoute::Auto => "auto",
        ModelRoute::Fixed(_) => "fixed",
    }
}

pub(crate) fn fleet_task_prompt(task_spec: &FleetTaskSpec) -> String {
    fleet_task_prompt_with_profile(task_spec, None)
}

pub(crate) fn fleet_task_prompt_with_profiles(
    task_spec: &FleetTaskSpec,
    agent_profiles: &[AgentProfile],
) -> Result<String> {
    let agent_profile = resolve_task_agent_profile(task_spec, agent_profiles)?;
    Ok(fleet_task_prompt_with_profile(
        task_spec,
        agent_profile.as_deref(),
    ))
}

fn fleet_task_prompt_with_profile(
    task_spec: &FleetTaskSpec,
    agent_profile: Option<&AgentProfile>,
) -> String {
    let role = effective_fleet_role(task_spec.worker.as_ref(), agent_profile)
        .unwrap_or_else(|| "general".to_string());
    let mut prompt = String::new();
    prompt.push_str("You have been summoned as a Codewhale Fleet member (");
    prompt.push_str(&role);
    prompt.push_str(") by the Fleet orchestrator.\n\n");
    prompt.push_str("Fleet operating contract:\n");
    prompt.push_str("- Work only the assigned slice; keep sibling or topology assumptions out of your answer.\n");
    prompt.push_str("- Use the policy-gated tools available in this headless worker run.\n");
    prompt.push_str("- Treat the active provider/model route as inherited unless this task or profile pins a model.\n");
    prompt.push_str(
        "- Return concise evidence, gaps, and next actions; the orchestrator will integrate and verify.\n\n",
    );
    prompt.push_str("Fleet task: ");
    prompt.push_str(&task_spec.name);

    if let Some(objective) = task_spec.objective.as_deref() {
        prompt.push_str("\n\nObjective:\n");
        prompt.push_str(objective);
    } else if let Some(description) = task_spec.description.as_deref() {
        prompt.push_str("\n\nObjective:\n");
        prompt.push_str(description);
    }

    prompt.push_str("\n\nInstructions:\n");
    prompt.push_str(&task_spec.instructions);

    if !task_spec.context.is_empty() {
        prompt.push_str("\n\nContext:\n");
        for item in &task_spec.context {
            prompt.push_str("- ");
            prompt.push_str(item);
            prompt.push('\n');
        }
    }

    if !task_spec.input_files.is_empty() {
        prompt.push_str("\nInput files:\n");
        for path in &task_spec.input_files {
            prompt.push_str("- ");
            prompt.push_str(&path.display().to_string());
            prompt.push('\n');
        }
    }

    if let Some(profile) = agent_profile {
        append_agent_profile_prompt(&mut prompt, profile);
    }

    prompt
}

/// Shared saved-profile instructions for direct and durable Fleet children.
pub(crate) fn append_agent_profile_prompt(prompt: &mut String, agent_profile: &AgentProfile) {
    prompt.push_str("\nFleet profile: ");
    prompt.push_str(&agent_profile.id);
    if let Some(display_name) = agent_profile.display_name.as_deref() {
        prompt.push_str(" (");
        prompt.push_str(display_name);
        prompt.push(')');
    }
    if let Some(description) = agent_profile.description.as_deref() {
        prompt.push_str("\nProfile description:\n");
        prompt.push_str(description);
    }
    if let Some(instructions) = agent_profile.profile.role.instructions.as_deref() {
        prompt.push_str("\nProfile instructions:\n");
        prompt.push_str(instructions);
    }
}

/// Find a saved role pin without letting a member id shadow a second member
/// with the same semantic role. Built-in inherited postures are not pins.
pub(crate) fn resolve_pinned_role_profile(
    agent_profiles: &[AgentProfile],
    role: &str,
) -> Result<Option<AgentProfile>, FleetSelectorError> {
    let pinned = agent_profiles
        .iter()
        .filter(|profile| {
            profile.origin != ProfileOrigin::BuiltIn
                && profile
                    .profile
                    .model
                    .as_deref()
                    .and_then(non_empty_trimmed)
                    .is_some_and(|model| !model.eq_ignore_ascii_case("auto"))
        })
        .cloned()
        .collect::<Vec<_>>();
    resolve_member_in_profiles(
        &pinned,
        &format!("role:{}", canonical_public_role_name(role)),
    )
    .map(|member| member.cloned())
}

/// Compare only the known route pair; never infer a provider from a wire id's
/// namespace. A qualified task selector may restate that same exact pair.
pub(crate) fn requested_model_matches_pin(
    requested: &str,
    model: &str,
    provider: Option<&str>,
) -> bool {
    let requested = requested.trim();
    let model = model.trim();
    requested == model
        || provider
            .and_then(non_empty_trimmed)
            .and_then(|provider| requested.strip_prefix(&format!("{provider}/")))
            .is_some_and(|requested_model| requested_model == model)
}

fn resolve_task_agent_profile<'a>(
    task_spec: &FleetTaskSpec,
    agent_profiles: &'a [AgentProfile],
) -> Result<Option<Cow<'a, AgentProfile>>> {
    if let Some(snapshot) = task_spec.metadata.get(FROZEN_FLEET_MEMBER_METADATA_KEY) {
        let snapshot: FrozenFleetMember =
            serde_json::from_value(snapshot.clone()).map_err(|error| {
                anyhow::anyhow!(
                    "Fleet task {} has an invalid durable member snapshot: {error}",
                    task_spec.id
                )
            })?;
        return Ok(Some(Cow::Owned(snapshot.into_profile()?)));
    }
    let Some(worker) = task_spec.worker.as_ref() else {
        return Ok(None);
    };
    if let Some(selector) = worker
        .agent_profile
        .as_deref()
        .map(str::trim)
        .filter(|selector| !selector.is_empty())
    {
        let profile = resolve_member_in_profiles(agent_profiles, selector).map_err(|error| {
            anyhow::anyhow!(
                "Fleet task {} has invalid worker.agent_profile selector {selector:?}: {error}",
                task_spec.id
            )
        })?;
        let Some(profile) = profile else {
            bail!(
                "Fleet task {} references unknown agent profile selector {selector:?}",
                task_spec.id
            );
        };
        validate_selected_member_model(task_spec, profile)?;
        return Ok(Some(Cow::Borrowed(profile)));
    }

    let Some(selector) = worker
        .role
        .as_deref()
        .map(str::trim)
        .filter(|selector| !selector.is_empty())
    else {
        return Ok(None);
    };
    // `worker.role` predates human member selectors. Resolve it only when one
    // member is deterministic; an ambiguous/missing roster match remains the
    // historical Runtime posture instead of breaking an existing task.
    let profile = resolve_member_in_profiles(agent_profiles, selector).map_err(|error| {
        anyhow::anyhow!(
            "Fleet task {} has invalid worker.role member selector {selector:?}: {error}",
            task_spec.id
        )
    })?;
    if let Some(profile) = profile {
        validate_selected_member_model(task_spec, profile)?;
    }
    Ok(profile.map(Cow::Borrowed))
}

fn validate_selected_member_model(task_spec: &FleetTaskSpec, profile: &AgentProfile) -> Result<()> {
    let Some(task_model) = task_spec
        .worker
        .as_ref()
        .and_then(|worker| worker.model.as_deref())
        .and_then(non_empty_trimmed)
    else {
        return Ok(());
    };
    let Some(profile_model) = profile.profile.model.as_deref().and_then(non_empty_trimmed) else {
        return Ok(());
    };
    if profile_model.eq_ignore_ascii_case("auto") {
        return Ok(());
    }
    let profile_provider = profile
        .profile
        .provider
        .as_deref()
        .and_then(non_empty_trimmed);
    if !requested_model_matches_pin(task_model, profile_model, profile_provider) {
        bail!(
            "Fleet task {} selects member {:?} with pinned model {} on {}; worker.model {:?} conflicts with that member route",
            task_spec.id,
            profile.id,
            profile_model,
            profile_provider.unwrap_or("the session provider"),
            task_model
        );
    }
    Ok(())
}

fn effective_fleet_role(
    worker_profile: Option<&FleetTaskWorkerProfile>,
    agent_profile: Option<&AgentProfile>,
) -> Option<String> {
    effective_fleet_role_with_source(worker_profile, agent_profile).0
}

fn effective_fleet_role_with_source(
    worker_profile: Option<&FleetTaskWorkerProfile>,
    agent_profile: Option<&AgentProfile>,
) -> (Option<String>, Option<&'static str>) {
    // A resolved roster member is authoritative for the runtime posture: its
    // canonical slot (reviewer/builder/...) defines shell/write/network
    // authority. The resolved `AgentProfile` always carries a canonical
    // `role.name` — a display name, model label, or route selector is already
    // collapsed onto the member during profile resolution, never surfaced as a
    // raw posture string here.
    //
    // Preferring the member over legacy `worker.role` is what makes a task
    // whose role label is "manager" but whose agent_profile selects
    // `member:reviewer` actually run with reviewer authority. Previously the
    // first branch required `worker.agent_profile` to be empty, so a present
    // agent_profile fell through to `worker.role` and silently discarded the
    // member's slot (fleet-e12f3160: task stayed a manager-coordinator and was
    // never leased).
    //
    // This is not where a conflict gets arbitrated. `freeze_fleet_task_members`
    // rejects a spec whose `worker.role` names a posture other than the
    // selected member's role before the task is persisted, so by the time a
    // member reaches this function its role and the task label agree (#5945).
    if let Some(profile) = agent_profile {
        return (
            Some(canonical_public_role_name(&profile.profile.role.name)),
            Some("agent_profile.role"),
        );
    }
    // No member resolved (no agent_profile selector, no frozen snapshot, and
    // worker.role was not a deterministic member selector). Keep the legacy
    // role label so existing v1 tasks retain their historical posture. A
    // receipt whose `role_source` reads "task.role" therefore means exactly
    // that: no roster member was resolved for the task at all.
    worker_profile
        .and_then(|worker| worker.role.as_deref())
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(canonical_public_role_name)
        .map(|role| (Some(role), Some("task.role")))
        .unwrap_or((None, None))
}

fn effective_fleet_loadout(
    worker_profile: Option<&FleetTaskWorkerProfile>,
    agent_profile: Option<&AgentProfile>,
) -> codewhale_config::FleetLoadout {
    effective_fleet_loadout_with_source(worker_profile, agent_profile).0
}

fn effective_fleet_loadout_with_source(
    worker_profile: Option<&FleetTaskWorkerProfile>,
    agent_profile: Option<&AgentProfile>,
) -> (codewhale_config::FleetLoadout, Option<&'static str>) {
    if let Some(model_class) = worker_profile
        .and_then(|worker| worker.model_class.as_deref())
        .and_then(non_empty_trimmed)
    {
        return (
            codewhale_config::FleetLoadout::from_name(model_class),
            Some("task.model_class"),
        );
    }
    if let Some(loadout) = worker_profile
        .and_then(|worker| worker.loadout.as_deref())
        .and_then(non_empty_trimmed)
    {
        return (
            codewhale_config::FleetLoadout::from_name(loadout),
            Some("task.loadout"),
        );
    }
    if let Some(loadout) = agent_profile
        .map(|profile| profile.profile.loadout.clone())
        .filter(|loadout| *loadout != codewhale_config::FleetLoadout::Inherit)
    {
        return (loadout, Some("agent_profile.loadout"));
    }
    (codewhale_config::FleetLoadout::Inherit, None)
}

fn effective_fleet_model(
    run_model: &str,
    worker_profile: Option<&FleetTaskWorkerProfile>,
    agent_profile: Option<&AgentProfile>,
) -> String {
    effective_fleet_model_with_source(run_model, worker_profile, agent_profile).0
}

fn effective_fleet_model_with_source(
    run_model: &str,
    worker_profile: Option<&FleetTaskWorkerProfile>,
    agent_profile: Option<&AgentProfile>,
) -> (String, &'static str) {
    if let Some(model) = agent_profile
        .and_then(|profile| profile.profile.model.as_deref())
        .and_then(non_empty_trimmed)
        .filter(|model| !model.eq_ignore_ascii_case("auto"))
    {
        return (model.to_string(), "agent_profile.model");
    }
    if let Some(model) = worker_profile
        .and_then(|worker| worker.model.as_deref())
        .and_then(non_empty_trimmed)
    {
        return (model.to_string(), "task.model");
    }
    if let Some(model) = agent_profile
        .and_then(|profile| profile.profile.model.as_deref())
        .and_then(non_empty_trimmed)
    {
        return (model.to_string(), "agent_profile.model");
    }
    (run_model.to_string(), "run.model")
}

/// The provider id a resolved agent profile EXPLICITLY pins, if any (#4093).
///
/// This preserves user-named OpenAI-compatible custom providers such as
/// `lm-studio` instead of collapsing them through [`ProviderKind`]. Runtime
/// launch paths can set `Config.provider` to this exact id so the normal config
/// resolver finds `[providers.<id>]` (#3965).
///
/// Returns `None` when no profile names a provider — never invents a DeepSeek
/// default — so launch paths can omit `--provider` and leave profile-less
/// workers on their own session default. EPIC #2608: never inferred from
/// `model`.
pub(crate) fn explicit_fleet_provider_id(agent_profile: Option<&AgentProfile>) -> Option<String> {
    agent_profile
        .and_then(|profile| profile.profile.provider.as_deref())
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string)
}

pub(crate) fn effective_fleet_reasoning_effort(
    agent_profile: Option<&AgentProfile>,
) -> Option<String> {
    agent_profile
        .and_then(|profile| profile.profile.reasoning_effort.as_deref())
        .map(str::trim)
        .filter(|effort| !effort.is_empty())
        .map(str::to_string)
}

fn effective_fleet_reasoning_effort_for_role(
    worker_profile: Option<&FleetTaskWorkerProfile>,
    agent_profile: Option<&AgentProfile>,
) -> Option<String> {
    effective_fleet_reasoning_effort(agent_profile).or_else(|| {
        let role = effective_fleet_role(worker_profile, agent_profile);
        WorkerRuntimeProfile::for_role(runtime_role_for_member(role.as_deref().unwrap_or_default()))
            .reasoning_effort
    })
}

/// The effective reasoning/thinking tier a Fleet worker should launch with.
///
/// This is the launch-side twin of the receipt/runtime-profile field: an
/// explicit resolved AgentProfile tier wins, otherwise the selected role's
/// documented default applies. Task model overrides do not invent a tier.
pub(crate) fn fleet_worker_launch_reasoning_effort(
    task_spec: &FleetTaskSpec,
    agent_profiles: &[AgentProfile],
) -> Option<String> {
    let agent_profile = resolve_task_agent_profile(task_spec, agent_profiles)
        .ok()
        .flatten();
    effective_fleet_reasoning_effort_for_role(task_spec.worker.as_ref(), agent_profile.as_deref())
}

/// The route (model selector + optional explicit provider id) that a fleet
/// worker's actual `codewhale exec` subprocess should launch on (#4093 AC #4).
///
/// This is the launch-side twin of [`resolve_fleet_route`] (the receipt): both
/// read the worker's model from the same task/profile/run precedence
/// ([`effective_fleet_model`]) and the provider from the same explicit-only
/// source ([`explicit_fleet_provider_id`]), so a worker whose profile is pinned
/// to provider B launches on provider B even when the parent session is on
/// provider A.
///
/// - `model`: never empty in practice — falls back to `run_model` when neither
///   the task nor the profile pins a model, matching pre-#4093 dispatch.
/// - `provider`: `Some(provider_id)` ONLY when the resolved agent profile
///   explicitly pins a provider. `None` means "no provider authority" — the
///   caller omits `--provider` and the worker keeps its own session default,
///   preserving today's behavior for profile-less workers. Built-ins use their
///   canonical ids; user-named custom providers preserve the profile's id so
///   `codewhale exec --provider <id>` can resolve `[providers.<id>]`.
pub(crate) fn fleet_worker_launch_route(
    task_spec: &FleetTaskSpec,
    agent_profiles: &[AgentProfile],
    run_model: &str,
) -> (String, Option<String>) {
    let agent_profile = resolve_task_agent_profile(task_spec, agent_profiles)
        .ok()
        .flatten();
    let agent_profile = agent_profile.as_deref();
    let worker_profile = task_spec.worker.as_ref();
    let model = effective_fleet_model(run_model, worker_profile, agent_profile);
    let provider = explicit_fleet_provider_id(agent_profile);
    (model, provider)
}

fn task_model_class_with_source(
    worker_profile: Option<&FleetTaskWorkerProfile>,
) -> (Option<String>, Option<&'static str>) {
    worker_profile
        .and_then(|worker| worker.model_class.as_deref())
        .and_then(non_empty_trimmed)
        .map(|model_class| (Some(model_class.to_string()), Some("task.model_class")))
        .unwrap_or((None, None))
}

fn fleet_route_model_selector_with_source(
    worker_profile: Option<&FleetTaskWorkerProfile>,
    agent_profile: Option<&AgentProfile>,
    session_model: Option<&str>,
) -> (Option<String>, &'static str) {
    // The session route (operator model) is the run-level fallback, matching
    // the dispatch path where FleetManager::run_model() feeds
    // `effective_fleet_model_with_source`. Empty/"auto" stays resolver-default.
    let run_model = session_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or("auto");
    let (model, source) =
        effective_fleet_model_with_source(run_model, worker_profile, agent_profile);
    if model.trim().is_empty() || model.eq_ignore_ascii_case("auto") {
        (None, "resolver.default")
    } else {
        (Some(model), source)
    }
}

/// Runtime agent type for a roster member: role name first, falling back to
/// the org-chart slot name when the role name is empty (#fleet-roster cutover
/// (v0.8.67)).
pub(crate) fn roster_member_agent_type(member: &AgentProfile) -> FleetRole {
    let role_name = member.profile.role.name.trim();
    if role_name.is_empty() {
        runtime_role_for_member(member.profile.slot.as_str())
    } else {
        runtime_role_for_member(role_name)
    }
}

/// Convert a fleet worker profile's tool list into an `AgentWorkerToolProfile`.
fn fleet_tool_profile(profile: Option<&FleetTaskWorkerProfile>) -> AgentWorkerToolProfile {
    match profile {
        Some(p) if !p.tools.is_empty() => AgentWorkerToolProfile::Explicit(p.tools.clone()),
        _ => AgentWorkerToolProfile::Inherited,
    }
}

fn fleet_worker_runtime_profile(
    agent_type: &FleetRole,
    tool_profile: &AgentWorkerToolProfile,
    model: &str,
    spawn_depth: u32,
    max_spawn_depth: u32,
) -> WorkerRuntimeProfile {
    let mut profile = WorkerRuntimeProfile::for_role(agent_type.clone());
    profile.tools = match tool_profile {
        AgentWorkerToolProfile::Inherited => ToolScope::Inherit,
        AgentWorkerToolProfile::Explicit(tools) => ToolScope::Explicit(tools.clone()),
    };
    profile.model = if model == "auto" {
        ModelRoute::Auto
    } else {
        ModelRoute::Fixed(model.to_string())
    };
    profile.max_spawn_depth = max_spawn_depth;
    profile.spawn_depth = spawn_depth;
    profile.background = true;
    profile
}

fn fleet_worker_runtime_profile_for_loadout(
    agent_type: &FleetRole,
    tool_profile: &AgentWorkerToolProfile,
    model: &str,
    spawn_depth: u32,
    max_spawn_depth: u32,
    loadout: &codewhale_config::FleetLoadout,
    model_source: &'static str,
) -> WorkerRuntimeProfile {
    let mut profile = fleet_worker_runtime_profile(
        agent_type,
        tool_profile,
        model,
        spawn_depth,
        max_spawn_depth,
    );
    profile.model = if matches!(model_source, "task.model" | "agent_profile.model") {
        fleet_model_route_for_loadout(model, &codewhale_config::FleetLoadout::Inherit)
    } else {
        fleet_model_route_for_loadout("auto", loadout)
    };
    profile
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

pub(crate) fn fleet_model_route_for_loadout(
    model: &str,
    loadout: &codewhale_config::FleetLoadout,
) -> ModelRoute {
    let model = model.trim();
    if !model.is_empty() && !model.eq_ignore_ascii_case("auto") {
        return ModelRoute::Fixed(model.to_string());
    }
    match loadout {
        codewhale_config::FleetLoadout::Inherit => ModelRoute::Inherit,
        // `Fast` used to mean "cheap sibling" — silently route the child to a
        // different, cheaper model than the parent turn. That is a routing
        // decision the operator never made and cannot see: the child's model
        // is not reported in exec's structured output, so a parent running a
        // specifically-priced route (e.g. `muse-spark-1.2-contributor`) would
        // spawn a scout billed as something else, and the only place it
        // surfaced was the invoice.
        //
        // A loadout is a statement about how much work a role should do, not
        // authority to re-price it. `Fast` now inherits the parent's route
        // like every other default; a genuinely different model stays
        // available, but only when someone pins it explicitly.
        codewhale_config::FleetLoadout::Fast => ModelRoute::Inherit,
        codewhale_config::FleetLoadout::Custom(_) => ModelRoute::Auto,
    }
}

/// Apply exec hardening to a worker spec from fleet config (#3027).
///
/// Filters tools against allowed/disallowed lists and caps max_steps to
/// config's max_turns. `append_system_prompt` is deliberately not folded into
/// the objective: the worker command delivers it once, as system prompt text,
/// via `--append-system-prompt`, and the objective must stay identical to the
/// persisted launch manifest prompt.
pub fn apply_exec_hardening(
    mut spec: AgentWorkerSpec,
    exec: &codewhale_config::FleetExecConfig,
) -> AgentWorkerSpec {
    // Cap max_steps to config max_turns (0 means no cap).
    if exec.max_turns > 0 {
        spec.max_steps = if spec.max_steps == 0 {
            exec.max_turns
        } else {
            spec.max_steps.min(exec.max_turns)
        };
    }
    spec.max_spawn_depth = spec
        .max_spawn_depth
        .min(spec.runtime_profile.max_spawn_depth)
        .min(exec.max_spawn_depth);
    spec.runtime_profile.max_spawn_depth = spec.max_spawn_depth;
    spec.runtime_profile.spawn_depth = spec.spawn_depth;

    // Apply tool filtering
    if !exec.allowed_tools.is_empty() || !exec.disallowed_tools.is_empty() {
        spec.tool_profile = filter_tool_profile(&spec.tool_profile, exec);
        spec.runtime_profile.tools = match &spec.tool_profile {
            AgentWorkerToolProfile::Inherited => ToolScope::Inherit,
            AgentWorkerToolProfile::Explicit(tools) => ToolScope::Explicit(tools.clone()),
        };
    }
    // #4042: thread `FleetExecConfig.disallowed_tools` into the runtime profile's
    // deny-list so it is enforced at run time even for `Inherited` tool profiles,
    // which `filter_tool_profile` cannot narrow at spec time. Union with any
    // already-inherited entries (deny never relaxes). The subprocess Fleet exec
    // path separately passes `--disallowed-tools` on the CLI.
    for rule in &exec.disallowed_tools {
        if !spec.runtime_profile.denied_tools.contains(rule) {
            spec.runtime_profile.denied_tools.push(rule.clone());
        }
    }

    spec
}

pub(crate) fn fleet_effective_permissions_for_task(
    task_spec: &FleetTaskSpec,
    agent_profiles: &[AgentProfile],
    spec: &AgentWorkerSpec,
) -> FleetEffectivePermissions {
    let agent_profile = resolve_task_agent_profile(task_spec, agent_profiles)
        .ok()
        .flatten();
    codewhale_agent_contract::role::fleet_effective_permissions(
        &spec.agent_type,
        &spec.runtime_profile,
        agent_profile.as_ref().map(|profile| profile.id.as_str()),
        agent_profile
            .as_ref()
            .map(|profile| profile_origin_label(profile.origin)),
    )
}

/// Return a truthful dispatch warning when a brief asks for network-backed
/// verification but the selected Fleet role cannot use the network.
pub(crate) fn network_posture_warning_for_task(
    task: &FleetTaskSpec,
    agent_profiles: &[AgentProfile],
    session_model: Option<&str>,
) -> Option<String> {
    let brief = format!(
        "{}\n{}\n{}",
        task.name,
        task.objective.as_deref().unwrap_or_default(),
        task.instructions
    );
    let lower = brief.to_ascii_lowercase();
    let asks_for_network = [
        "gh ",
        "gh\n",
        "github",
        "curl ",
        "wget ",
        "http://",
        "https://",
        "network",
        "web search",
        "check ci",
        "check the pr",
        "check the issue",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if !asks_for_network {
        return None;
    }

    let agent_profile = resolve_task_agent_profile(task, agent_profiles)
        .ok()
        .flatten();
    let agent_profile = agent_profile.as_deref();
    let worker_profile = task.worker.as_ref();
    let role = effective_fleet_role(worker_profile, agent_profile);
    let agent_type = runtime_role_for_member(role.as_deref().unwrap_or_default());
    let tool_profile = fleet_tool_profile(worker_profile);
    let (model, model_source) = effective_fleet_model_with_source(
        session_model.unwrap_or("auto"),
        worker_profile,
        agent_profile,
    );
    let loadout = effective_fleet_loadout(worker_profile, agent_profile);
    let runtime = fleet_worker_runtime_profile_for_loadout(
        &agent_type,
        &tool_profile,
        &model,
        0,
        codewhale_config::FleetExecConfig::default().max_spawn_depth,
        &loadout,
        model_source,
    );
    if runtime.permissions.network {
        return None;
    }

    Some(format!(
        "Fleet task `{}` mentions network-backed verification, but role `{}` has network=off and shell={}. Dispatch a `worker` role with shell `read_only` for gh/curl evidence, or revise the brief.",
        task.id,
        role.as_deref().unwrap_or("worker"),
        shell_policy_label(runtime.shell),
    ))
}

fn profile_origin_label(origin: crate::fleet::roster::ProfileOrigin) -> &'static str {
    match origin {
        crate::fleet::roster::ProfileOrigin::BuiltIn => "built_in",
        crate::fleet::roster::ProfileOrigin::Plugin => "plugin",
        crate::fleet::roster::ProfileOrigin::Config => "config",
        crate::fleet::roster::ProfileOrigin::Personal => "personal",
        crate::fleet::roster::ProfileOrigin::Workspace => "workspace",
        crate::fleet::roster::ProfileOrigin::ClaudeCode => "claude",
    }
}

fn shell_policy_label(
    shell: codewhale_agent_contract::worker_profile::ShellPolicy,
) -> &'static str {
    match shell {
        codewhale_agent_contract::worker_profile::ShellPolicy::None => "none",
        codewhale_agent_contract::worker_profile::ShellPolicy::ReadOnly => "read_only",
        codewhale_agent_contract::worker_profile::ShellPolicy::Full => "full",
    }
}

/// Filter a tool profile against allowed/disallowed lists.
fn filter_tool_profile(
    profile: &AgentWorkerToolProfile,
    exec: &codewhale_config::FleetExecConfig,
) -> AgentWorkerToolProfile {
    match profile {
        AgentWorkerToolProfile::Explicit(tools) => {
            let filtered: Vec<String> = tools
                .iter()
                .filter(|t| {
                    // If allowed_tools is non-empty, only keep tools in the list
                    if !exec.allowed_tools.is_empty() && !exec.allowed_tools.contains(t) {
                        return false;
                    }
                    // Disallowed tools always win
                    !exec.disallowed_tools.contains(t)
                })
                .cloned()
                .collect();
            AgentWorkerToolProfile::Explicit(filtered)
        }
        AgentWorkerToolProfile::Inherited => {
            // Inherited profiles can't be filtered at spec time;
            // the sub-agent spawn path applies tool filtering.
            AgentWorkerToolProfile::Inherited
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_protocol::fleet::{FleetHostSpec, FleetTaskBudget, FleetWorkspaceRequirements};
    use std::path::{Path, PathBuf};

    fn explicit_deepseek_config() -> Config {
        {
            let __cw_config_init_0_0 = Some("deepseek".to_string());
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value
        }
        .with_legacy_root(Some("test-key".to_string()), None)
    }

    #[test]
    fn read_only_roles_report_the_narrowed_shell_they_actually_run_under() {
        use codewhale_agent_contract::role;
        use codewhale_agent_contract::role::FleetRole;
        use codewhale_agent_contract::worker_profile::ShellPolicy;
        let mut requested = WorkerRuntimeProfile {
            shell: ShellPolicy::Full,
            ..WorkerRuntimeProfile::default()
        };

        for role in [FleetRole::Scout, FleetRole::Reviewer, FleetRole::Planner] {
            let effective = role::effective_runtime_profile_for_role(&role, &requested);
            assert_eq!(effective.shell, ShellPolicy::ReadOnly, "{role:?}");
            assert_eq!(
                role::fleet_effective_permissions(&role, &requested, None, None).shell,
                "read_only",
                "{role:?}"
            );
        }
        let worker = role::effective_runtime_profile_for_role(&FleetRole::Worker, &requested);
        assert_eq!(worker.shell, ShellPolicy::Full);

        // A role that was already narrower than read-only keeps its posture.
        requested.shell = ShellPolicy::None;
        assert_eq!(
            role::effective_runtime_profile_for_role(&FleetRole::Scout, &requested).shell,
            ShellPolicy::None
        );
    }

    #[test]
    fn worker_workspace_isolation_requires_linked_worktree_outside_manager() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = tmp.path().join("manager");
        std::fs::create_dir_all(manager.join("sub")).expect("manager dirs");
        let worktree = tmp.path().join("worktrees").join("wt-1");
        std::fs::create_dir_all(&worktree).expect("worktree dir");

        assert!(!worker_workspace_is_isolated(&manager, &manager));
        assert!(!worker_workspace_is_isolated(
            &manager,
            &manager.join("sub")
        ));
        // An external directory without a worktree gitfile stays shared.
        assert!(!worker_workspace_is_isolated(&manager, &worktree));

        std::fs::write(
            worktree.join(".git"),
            "gitdir: /elsewhere/.git/worktrees/wt-1\n",
        )
        .expect("gitfile");
        assert!(worker_workspace_is_isolated(&manager, &worktree));
    }

    fn fleet_task(id: &str, worker: Option<FleetTaskWorkerProfile>) -> FleetTaskSpec {
        FleetTaskSpec {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            objective: Some(format!("Complete {id}")),
            instructions: format!("do {id}"),
            worker,
            workspace: Some(FleetWorkspaceRequirements {
                placement: None,
                root: Some(PathBuf::from(".")),
                required_files: Vec::new(),
                writable_paths: vec![PathBuf::from(".")],
                environment: None,
            }),
            input_files: Vec::new(),
            context: Vec::new(),
            budget: None,
            tags: Vec::new(),
            expected_artifacts: Vec::new(),
            scorer: None,
            retry_policy: None,
            alert_policy: None,
            timeout_seconds: None,
            metadata: Default::default(),
        }
    }

    #[test]
    fn write_capable_fleet_worker_requires_and_persists_a_bounded_claim() {
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };
        let mut unscoped = fleet_task("write", None);
        unscoped.workspace = None;
        let error = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &unscoped,
            &worker,
            "auto",
            Path::new("/tmp"),
            Path::new("/tmp"),
            &[],
            None,
        )
        .expect_err("unscoped Fleet writer must fail before registration");
        assert!(error.to_string().contains("declares no"), "{error:#}");

        let scoped = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &fleet_task("write", None),
            &worker,
            "auto",
            Path::new("/tmp"),
            Path::new("/tmp"),
            &[],
            None,
        )
        .expect("bounded Fleet writer");
        let manifest = scoped.launch_manifest.expect("launch manifest");
        assert_eq!(manifest.child_id, "worker-1");
        assert_eq!(manifest.writable_roots, ["."]);
        assert_eq!(manifest.prompt, scoped.objective);
    }

    #[test]
    fn fleet_claim_roots_share_one_manager_workspace_namespace() {
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };
        let mut nested = fleet_task("nested", None);
        nested.workspace = Some(FleetWorkspaceRequirements {
            placement: None,
            root: Some(PathBuf::from("pkg-a")),
            writable_paths: vec![PathBuf::from("src")],
            ..FleetWorkspaceRequirements::default()
        });
        let mut root = fleet_task("root", None);
        root.workspace = Some(FleetWorkspaceRequirements {
            placement: None,
            root: Some(PathBuf::from(".")),
            writable_paths: vec![PathBuf::from("pkg-a/src")],
            ..FleetWorkspaceRequirements::default()
        });

        let nested_spec = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &nested,
            &worker,
            "auto",
            Path::new("/repo/pkg-a"),
            Path::new("/repo/pkg-a"),
            &[],
            None,
        )
        .unwrap();
        let root_spec = fleet_task_to_worker_spec_with_profiles(
            "worker-2",
            "run-1",
            &root,
            &worker,
            "auto",
            Path::new("/repo"),
            Path::new("/repo"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(
            nested_spec.launch_manifest.unwrap().writable_roots,
            ["pkg-a/src"]
        );
        assert_eq!(
            root_spec.launch_manifest.unwrap().writable_roots,
            ["pkg-a/src"]
        );
        assert_eq!(fleet_runtime_write_roots(&nested).unwrap(), ["src"]);
    }

    #[test]
    fn fleet_manifest_rejects_control_characters_before_lease() {
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };
        let mut bad_contract = fleet_task("bad-contract", None);
        bad_contract.metadata.insert(
            "coordination_contracts".to_string(),
            serde_json::json!(["api\ncontract"]),
        );
        assert!(
            fleet_task_to_worker_spec_with_profiles(
                "worker-1",
                "run-1",
                &bad_contract,
                &worker,
                "auto",
                Path::new("/repo"),
                Path::new("/repo"),
                &[],
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("one non-empty line")
        );

        let mut bad_path = fleet_task("bad-path", None);
        bad_path.workspace.as_mut().unwrap().writable_paths = vec![PathBuf::from("src\nother")];
        assert!(
            fleet_task_to_worker_spec_with_profiles(
                "worker-1",
                "run-1",
                &bad_path,
                &worker,
                "auto",
                Path::new("/repo"),
                Path::new("/repo"),
                &[],
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("one repo-relative line")
        );
    }

    fn worker_profile(
        agent_profile: Option<&str>,
        role: Option<&str>,
        loadout: Option<&str>,
        model_class: Option<&str>,
        model: Option<&str>,
        tools: Vec<&str>,
    ) -> FleetTaskWorkerProfile {
        FleetTaskWorkerProfile {
            agent_profile: agent_profile.map(str::to_string),
            role: role.map(str::to_string),
            loadout: loadout.map(str::to_string),
            model_class: model_class.map(str::to_string),
            model: model.map(str::to_string),
            tool_profile: None,
            tools: tools.into_iter().map(str::to_string).collect(),
            capabilities: Vec::new(),
        }
    }

    fn agent_profile(
        id: &str,
        role: &str,
        instructions: Option<&str>,
        loadout: codewhale_config::FleetLoadout,
    ) -> AgentProfile {
        AgentProfile {
            native_preset: None,
            id: id.to_string(),
            display_name: Some(format!("{role} profile")),
            description: Some(format!("{role} description")),
            requires: Vec::new(),
            profile: codewhale_config::FleetProfile {
                slot: codewhale_config::FleetSlot::from_name(role),
                role: codewhale_config::FleetRole {
                    name: role.to_string(),
                    description: Some(format!("{role} role")),
                    instructions: instructions.map(str::to_string),
                },
                loadout,
                model: None,
                provider: None,
                reasoning_effort: None,
                permissions: codewhale_config::FleetProfilePermissions::default(),
                delegation: codewhale_config::FleetDelegationHints::default(),
            },
            source: std::path::PathBuf::from(format!("{id}.toml")),
            origin: crate::fleet::roster::ProfileOrigin::Workspace,
            plugin_authority: None,
        }
    }

    #[test]
    fn fleet_role_smoke_runner_maps_to_verifier() {
        assert_eq!(runtime_role_for_member("smoke-runner"), FleetRole::Verifier);
    }

    #[test]
    fn fleet_role_read_only_maps_to_explore() {
        assert_eq!(runtime_role_for_member("read-only"), FleetRole::Scout);
    }

    #[test]
    fn fleet_role_reviewer_maps_to_review() {
        assert_eq!(runtime_role_for_member("reviewer"), FleetRole::Reviewer);
    }

    #[test]
    fn fleet_role_builder_maps_to_implementer() {
        assert_eq!(runtime_role_for_member("builder"), FleetRole::Builder);
    }

    /// An *absent* role is not an *unknown* role: a Fleet task with no `role`
    /// field has always run on the documented general default, and #5575's
    /// fail-closed rule is about labels nobody declared, not about the
    /// unspecified case.
    #[test]
    fn an_unspecified_role_still_maps_to_general() {
        assert_eq!(runtime_role_for_member(""), FleetRole::Worker);
        assert_eq!(runtime_role_for_member("   "), FleetRole::Worker);
    }

    #[test]
    fn fleet_role_manager_and_coordinator_map_to_general() {
        assert_eq!(runtime_role_for_member("manager"), FleetRole::Worker);
        assert_eq!(runtime_role_for_member("coordinator"), FleetRole::Worker);
    }

    #[test]
    fn fleet_role_operator_maps_to_general_explicitly() {
        // The operator coordinates the overall work (assigns managers to
        // workflows), so it needs the full General surface — by an explicit
        // match arm, not the unknown-role fall-through.
        assert_eq!(runtime_role_for_member("operator"), FleetRole::Worker);
    }

    #[test]
    fn agent_profile_member_slot_overrides_legacy_role_label() {
        // Regression (fleet-e12f3160): a task whose legacy role label is
        // "manager" but whose agent_profile selects `member:reviewer` must run
        // with reviewer authority, not fall through to the "manager" label and
        // get stuck as a write-capable worker that never leases.
        let reviewer = agent_profile(
            "reviewer",
            "reviewer",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        let task = fleet_task(
            "conflict",
            Some(worker_profile(
                Some("member:reviewer"),
                Some("manager"),
                None,
                None,
                None,
                vec!["read_file"],
            )),
        );
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };
        let spec = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &task,
            &worker,
            "auto",
            Path::new("/tmp"),
            Path::new("/tmp"),
            &[reviewer],
            None,
        )
        .expect("member selector resolves to the reviewer roster profile");

        assert_eq!(
            spec.role.as_deref(),
            Some("reviewer"),
            "the selected member's slot must win over the legacy role label"
        );
        assert_eq!(
            spec.agent_type,
            FleetRole::Reviewer,
            "reviewer authority must not be silently widened to a write-capable worker"
        );
    }

    #[test]
    fn legacy_role_label_never_widens_into_a_member_write_slot() {
        // Mirror of the regression above (#5945 review): member `alice` sits
        // in the write-capable `implement` slot while the task's legacy label
        // says `reviewer`. Letting the member win unconditionally would turn a
        // read-only task into a write-capable one; letting the label win would
        // re-open the original bug. Neither is a posture — the spec is
        // rejected before anything is persisted.
        let alice = agent_profile(
            "alice",
            "implement",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        let mut task = fleet_task(
            "mirror-conflict",
            Some(worker_profile(
                Some("member:alice"),
                Some("reviewer"),
                None,
                None,
                None,
                vec!["read_file"],
            )),
        );

        let error = freeze_fleet_task_members(std::slice::from_mut(&mut task), &[alice], false)
            .expect_err("a read-only label must not become a write-capable member slot");
        assert!(error.to_string().contains("\"implement\""), "{error:#}");
        assert!(error.to_string().contains("\"reviewer\""), "{error:#}");
        assert!(
            !task.metadata.contains_key(FROZEN_FLEET_MEMBER_METADATA_KEY),
            "a rejected spec must not persist a member snapshot"
        );
        assert_eq!(
            task.worker.as_ref().unwrap().role.as_deref(),
            Some("reviewer"),
            "the rejected task keeps its authored label untouched"
        );
    }

    #[test]
    fn conflicting_member_and_role_label_is_rejected_at_freeze_naming_both_postures() {
        // The original fleet-e12f3160 spec: `member:reviewer` plus a
        // `manager` label. It is an authoring error, and the message must name
        // both postures so the author can see which one to drop.
        let reviewer = agent_profile(
            "reviewer",
            "reviewer",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        let mut task = fleet_task(
            "conflict",
            Some(worker_profile(
                Some("member:reviewer"),
                Some("manager"),
                None,
                None,
                None,
                vec!["read_file"],
            )),
        );

        let error = freeze_fleet_task_members(std::slice::from_mut(&mut task), &[reviewer], true)
            .expect_err("a task cannot carry two postures");
        let message = error.to_string();
        assert!(message.contains("selects member \"reviewer\""), "{error:#}");
        assert!(message.contains("whose role is \"reviewer\""), "{error:#}");
        assert!(
            message.contains("worker.role names a different posture \"manager\""),
            "{error:#}"
        );
    }

    #[test]
    fn role_label_alias_of_the_selected_member_role_is_not_a_conflict() {
        // Casing and legacy aliases are spelling, not posture: `Code-Review`
        // canonicalizes to `reviewer`, which is exactly the member's role.
        let reviewer = agent_profile(
            "reviewer",
            "reviewer",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        let mut task = fleet_task(
            "alias",
            Some(worker_profile(
                Some("member:reviewer"),
                Some("Code-Review"),
                None,
                None,
                None,
                vec!["read_file"],
            )),
        );

        let profiles = [reviewer];
        freeze_fleet_task_members(std::slice::from_mut(&mut task), &profiles, true)
            .expect("an alias of the member's own role must freeze cleanly");
        let worker = task.worker.as_ref().unwrap();
        assert_eq!(worker.agent_profile.as_deref(), Some("member:reviewer"));
        assert_eq!(worker.role.as_deref(), Some("reviewer"));
        assert!(task.metadata.contains_key(FROZEN_FLEET_MEMBER_METADATA_KEY));

        let resolved = resolve_task_agent_profile(&task, &profiles)
            .unwrap()
            .expect("frozen member");
        assert_eq!(
            effective_fleet_role_with_source(task.worker.as_ref(), Some(&resolved)),
            (Some("reviewer".to_string()), Some("agent_profile.role"))
        );
    }

    #[test]
    fn consultant_and_legacy_advisory_aliases_share_the_consultant_posture() {
        for role in ["consultant", "oracle", "advisor"] {
            assert_eq!(
                runtime_role_for_member(role),
                FleetRole::Consultant,
                "role {role}"
            );
        }
    }

    #[test]
    fn fleet_role_synthesizer_family_maps_to_read_only_plan() {
        // A synthesizer must never fall through to General's full-write
        // posture; Planner is read-only (reads plus read-only shell probes).
        for role in ["synthesizer", "summarizer", "reducer"] {
            assert_eq!(
                runtime_role_for_member(role),
                FleetRole::Planner,
                "role {role}"
            );
        }
    }

    #[test]
    fn roster_member_agent_type_uses_role_then_slot() {
        let member = agent_profile(
            "synthesizer",
            "synthesizer",
            None,
            codewhale_config::FleetLoadout::Fast,
        );
        assert_eq!(roster_member_agent_type(&member), FleetRole::Planner);

        let mut slot_only = agent_profile(
            "custom-summarizer",
            "summarizer",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        slot_only.profile.role.name = String::new();
        assert_eq!(
            slot_only.profile.slot,
            codewhale_config::FleetSlot::Summarizer
        );
        assert_eq!(roster_member_agent_type(&slot_only), FleetRole::Planner);
    }

    /// #5285: every seeded dispatch posture maps back to exactly its own
    /// runtime role, so the roster listing and the dispatch enum cannot drift
    /// into a parallel taxonomy.
    #[test]
    fn seeded_posture_members_map_1to1_to_their_fleet_role() {
        let roster = crate::fleet::roster::FleetRoster::built_ins_only();
        for (id, expected) in [
            ("worker", FleetRole::Worker),
            ("scout", FleetRole::Scout),
            ("planner", FleetRole::Planner),
            ("reviewer", FleetRole::Reviewer),
            ("builder", FleetRole::Builder),
            ("verifier", FleetRole::Verifier),
            ("consultant", FleetRole::Consultant),
            ("custom", FleetRole::Custom),
        ] {
            let member = roster
                .get(id)
                .unwrap_or_else(|| panic!("seeded posture {id:?} must be a roster member"));
            assert_eq!(
                roster_member_agent_type(member),
                expected,
                "roster member {id:?} must resolve to its own dispatch posture"
            );
        }
    }

    /// #5575: an undeclared role name must not be able to hand a worker write
    /// authority. Before this fix the durable driver answered `Worker` here and
    /// the exact driver answered `Custom` — two different tables, both
    /// write-capable and full-shell, for a string nobody declared.
    #[test]
    fn unknown_role_fails_closed_to_the_read_only_explore_posture() {
        for unknown in ["nonexistent-role", "audit-lead", "release-checker"] {
            assert_eq!(
                runtime_role_for_member(unknown),
                FleetRole::Scout,
                "unknown role {unknown:?} must fail closed, never to a write-capable posture"
            );
            assert!(
                !WorkerRuntimeProfile::for_role(runtime_role_for_member(unknown))
                    .permissions
                    .write,
                "unknown role {unknown:?} must never carry write authority"
            );
        }

        // The escape hatch is a *declared* role, not a typo: an operator who
        // wants "inherit whatever the parent has" spells it `custom`.
        assert_eq!(runtime_role_for_member("custom"), FleetRole::Custom);
    }

    /// #5575: both Fleet drivers resolve names through the same mapper, so the
    /// aliases the durable driver used to own privately now resolve to the same
    /// posture on the exact/named-Fleet driver — which previously dropped every
    /// one of them into write-capable `custom`.
    #[test]
    fn the_two_fleet_drivers_agree_on_every_member_role_alias() {
        let session = codewhale_protocol::fleet::PermissionCeiling {
            write: true,
            network_tool: true,
            shell: codewhale_protocol::fleet::ShellCeiling::Full,
            delegation_depth: 2,
            tools: true,
        };
        for (role, expected, expected_write) in [
            ("smoke-runner", FleetRole::Verifier, "read_only"),
            ("read-only", FleetRole::Scout, "read_only"),
            ("synthesizer", FleetRole::Planner, "read_only"),
            ("summarizer", FleetRole::Planner, "read_only"),
            ("reducer", FleetRole::Planner, "read_only"),
            ("manager", FleetRole::Worker, "workspace_write"),
            ("coordinator", FleetRole::Worker, "workspace_write"),
            ("operator", FleetRole::Worker, "workspace_write"),
        ] {
            assert_eq!(runtime_role_for_member(role), expected, "role {role}");
            // The exact driver's authority comes from the same mapper.
            assert_eq!(
                codewhale_agent_contract::role::ChildAuthority::from_runtime_role(role, session)
                    .write_authority,
                expected_write,
                "role {role} must resolve the same authority on the exact driver"
            );
        }
    }

    #[test]
    fn resolved_config_mints_secret_free_fleet_route_snapshot() {
        let task = fleet_task(
            "route-1",
            Some(worker_profile(
                None,
                Some("builder"),
                Some("fast"),
                None,
                None,
                vec!["read_file"],
            )),
        );
        let config = explicit_deepseek_config();
        let route = resolve_fleet_route_with_config(&task, &[], None, Some(&config))
            .expect("explicit default route should resolve offline");

        // Honest, non-empty route shape from the resolver.
        assert!(!route.provider_id.is_empty());
        assert!(!route.provider_kind.is_empty());
        assert!(!route.wire_model_id.is_empty());
        // DeepSeek Flash rides Responses since a1c1741afa (see bundled_offerings):
        // the default route follows the shipped transport, not the old pin.
        assert_eq!(route.protocol, "responses");
        assert_eq!(route.role.as_deref(), Some("implement"));
        assert_eq!(route.loadout.as_deref(), Some("fast"));
        assert_eq!(route.model_class, None);
        assert_eq!(route.model_route.as_deref(), Some("inherit"));
        assert_eq!(route.reasoning_effort, None);
        assert_eq!(route.role_source.as_deref(), Some("task.role"));
        assert_eq!(route.loadout_source.as_deref(), Some("task.loadout"));
        assert_eq!(route.model_class_source, None);
        assert_eq!(route.model_source.as_deref(), Some("resolver.default"));
        assert_eq!(route.source, "runtime_route");

        // No-secrets: the serialized snapshot carries no credential markers.
        let json = serde_json::to_string(&route).unwrap();
        let haystack = json.to_ascii_lowercase();
        for needle in [
            "api_key",
            "apikey",
            "api-key",
            "authorization",
            "bearer ",
            "auth_token",
            "auth-token",
            "password",
            "credential",
            "sk-ant-",
            "sk-proj-",
            "sk-or-",
            "secret",
        ] {
            assert!(
                !haystack.contains(needle),
                "resolved-route JSON must not contain secret marker {needle:?}: {json}"
            );
        }
    }

    #[test]
    fn resolve_fleet_route_omits_inherit_loadout() {
        // No loadout/model_class intent → `inherit` collapses to None, never an
        // "inherit" string on the receipt.
        let task = fleet_task(
            "route-2",
            Some(worker_profile(
                None,
                Some("scout"),
                None,
                None,
                None,
                vec!["read_file"],
            )),
        );
        let config = explicit_deepseek_config();
        let route = resolve_fleet_route_with_config(&task, &[], None, Some(&config))
            .expect("route should resolve");
        assert_eq!(route.role.as_deref(), Some("explore"));
        assert!(route.loadout.is_none());
        assert_eq!(route.loadout_source, None);
        assert_eq!(route.model_route.as_deref(), Some("inherit"));
        assert_eq!(route.model_source.as_deref(), Some("resolver.default"));
    }

    #[test]
    fn advisory_task_aliases_emit_advisor_in_prompts_and_route_receipts() {
        for alias in ["oracle", "advisor"] {
            let task = fleet_task(
                &format!("legacy-{alias}"),
                Some(worker_profile(
                    None,
                    Some(alias),
                    None,
                    None,
                    None,
                    vec!["read_file"],
                )),
            );

            let prompt = fleet_task_prompt(&task);
            assert!(
                prompt.contains("Fleet member (advisor)"),
                "prompt must canonicalize {alias}: {prompt}"
            );
            if alias != "advisor" {
                assert!(
                    !prompt.contains(&format!("Fleet member ({alias})")),
                    "prompt must not emit compatibility alias {alias}: {prompt}"
                );
            }

            let config = explicit_deepseek_config();
            let resolved = resolve_fleet_route_with_config(&task, &[], None, Some(&config))
                .expect("compatibility role should resolve a receipt route");
            assert_eq!(resolved.role.as_deref(), Some("advisor"));
            assert_eq!(resolved.role_source.as_deref(), Some("task.role"));

            let reported = resolve_fleet_route_from_worker_report(
                &task,
                &[],
                None,
                "deepseek",
                None,
                "deepseek-v4-pro",
            )
            .expect("worker-reported route should retain canonical role metadata");
            assert_eq!(reported.role.as_deref(), Some("advisor"));
            assert_eq!(reported.role_source.as_deref(), Some("task.role"));
        }
    }

    #[test]
    fn resolve_fleet_route_records_model_class_and_profile_sources() {
        let mut profile = agent_profile(
            "audit",
            "reviewer",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("deepseek-v4-flash".to_string());
        let task = fleet_task(
            "route-profile",
            Some(worker_profile(
                Some("audit"),
                None,
                None,
                Some("balanced"),
                None,
                vec!["read_file"],
            )),
        );
        let config = explicit_deepseek_config();
        let route = resolve_fleet_route_with_config(&task, &[profile], None, Some(&config))
            .expect("profile route should resolve");

        assert_eq!(route.role.as_deref(), Some("reviewer"));
        assert_eq!(route.role_source.as_deref(), Some("agent_profile.role"));
        assert_eq!(route.loadout.as_deref(), Some("balanced"));
        assert_eq!(route.loadout_source.as_deref(), Some("task.model_class"));
        assert_eq!(route.model_class.as_deref(), Some("balanced"));
        assert_eq!(
            route.model_class_source.as_deref(),
            Some("task.model_class")
        );
        assert_eq!(route.model_source.as_deref(), Some("agent_profile.model"));
        assert_eq!(route.model_route.as_deref(), Some("fixed"));
        assert_eq!(route.wire_model_id, "deepseek-v4-flash");
        assert_eq!(route.reasoning_effort, None);
    }

    #[test]
    fn fleet_tool_profile_empty_uses_inherited() {
        let profile = FleetTaskWorkerProfile {
            agent_profile: None,
            role: None,
            loadout: None,
            model_class: None,
            model: None,
            tool_profile: None,
            tools: vec![],
            capabilities: vec![],
        };
        assert_eq!(
            fleet_tool_profile(Some(&profile)),
            AgentWorkerToolProfile::Inherited
        );
    }

    #[test]
    fn fleet_tool_profile_explicit_passes_tools() {
        let profile = FleetTaskWorkerProfile {
            agent_profile: None,
            role: None,
            loadout: None,
            model_class: None,
            model: None,
            tool_profile: None,
            tools: vec!["cargo".to_string(), "git".to_string()],
            capabilities: vec![],
        };
        assert_eq!(
            fleet_tool_profile(Some(&profile)),
            AgentWorkerToolProfile::Explicit(vec!["cargo".to_string(), "git".to_string()])
        );
    }

    #[test]
    fn network_brief_does_not_warn_for_built_in_roles_that_keep_network_reads() {
        let reviewer = fleet_task(
            "triage",
            Some(worker_profile(
                None,
                Some("reviewer"),
                None,
                None,
                None,
                vec!["read_file"],
            )),
        );
        let mut reviewer = reviewer;
        reviewer.instructions = "Use gh to check the PR and report CI evidence.".to_string();
        // Scout/reviewer lanes now ship the read-only inspection posture (network reach,
        // bounded verification surface; see worker_profile::for_role), so a
        // network-dependent reviewer brief no longer warns by default.
        assert!(
            network_posture_warning_for_task(&reviewer, &[], None).is_none(),
            "reviewer read-only inspection posture must not warn for a gh brief"
        );

        // Every built-in role now keeps network reach (a read); read-only
        // roles stay read-only on the workspace by intent. A planner brief that
        // needs gh/curl therefore no longer warns either.
        let mut planner = reviewer.clone();
        planner.worker.as_mut().unwrap().role = Some("planner".to_string());
        assert!(
            network_posture_warning_for_task(&planner, &[], None).is_none(),
            "planner keeps network reads by default"
        );

        let mut worker = reviewer.clone();
        worker.worker.as_mut().unwrap().role = Some("worker".to_string());
        assert!(network_posture_warning_for_task(&worker, &[], None).is_none());
    }

    #[test]
    fn non_network_brief_does_not_warn_for_networkless_role() {
        let task = fleet_task(
            "local-review",
            Some(worker_profile(
                None,
                Some("reviewer"),
                None,
                None,
                None,
                vec!["read_file"],
            )),
        );
        assert!(network_posture_warning_for_task(&task, &[], None).is_none());
    }

    #[test]
    fn fleet_task_prompt_includes_instructions_context_and_input_files() {
        let task = FleetTaskSpec {
            id: "review".to_string(),
            name: "Review protocol".to_string(),
            description: None,
            objective: Some("Find protocol regressions".to_string()),
            instructions: "Read the fleet protocol and report issues.".to_string(),
            worker: None,
            workspace: None,
            input_files: vec![std::path::PathBuf::from("crates/protocol/src/fleet.rs")],
            context: vec!["Keep the report concise.".to_string()],
            budget: None,
            tags: vec![],
            expected_artifacts: vec![],
            scorer: None,
            retry_policy: None,
            alert_policy: None,
            timeout_seconds: None,
            metadata: Default::default(),
        };

        let prompt = fleet_task_prompt(&task);

        assert!(prompt.contains("summoned as a Codewhale Fleet member (general)"));
        assert!(prompt.contains("Fleet operating contract:"));
        assert!(prompt.contains("keep sibling or topology assumptions out of your answer"));
        assert!(prompt.contains("Review protocol"));
        assert!(prompt.contains("Find protocol regressions"));
        assert!(prompt.contains("Read the fleet protocol and report issues."));
        assert!(prompt.contains("Keep the report concise."));
        assert!(prompt.contains("crates/protocol/src/fleet.rs"));
    }

    #[test]
    fn fleet_worker_spec_resolves_agent_profile_role_prompt_and_loadout() {
        let profile = agent_profile(
            "reviewer",
            "reviewer",
            Some("Focus on regressions and missing tests."),
            codewhale_config::FleetLoadout::Custom("balanced".to_string()),
        );
        let task = fleet_task(
            "review",
            Some(worker_profile(
                Some("reviewer"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };

        let profiles = vec![profile];
        let spec = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &task,
            &worker,
            "auto",
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &profiles,
            None,
        )
        .unwrap();

        assert_eq!(spec.role.as_deref(), Some("reviewer"));
        assert_eq!(spec.agent_type, FleetRole::Reviewer);
        assert!(
            spec.objective
                .contains("summoned as a Codewhale Fleet member (reviewer)")
        );
        assert!(spec.objective.contains("Fleet profile: reviewer"));
        assert!(
            spec.objective
                .contains("Focus on regressions and missing tests.")
        );
        assert_eq!(spec.runtime_profile.role, FleetRole::Reviewer);
        assert_eq!(spec.runtime_profile.model, ModelRoute::Auto);

        let permissions = fleet_effective_permissions_for_task(&task, &profiles, &spec);
        assert_eq!(permissions.profile_id.as_deref(), Some("reviewer"));
        assert_eq!(permissions.profile_origin.as_deref(), Some("workspace"));
        assert_eq!(permissions.source, "worker_runtime_profile");
    }

    #[test]
    fn role_only_consultant_aliases_keep_high_reasoning_and_locked_posture() {
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };

        for parent_effort in [None, Some("low")] {
            for role in ["consultant", "oracle", "advisor"] {
                let task = fleet_task(
                    &format!("advice-{role}"),
                    Some(worker_profile(None, Some(role), None, None, None, vec![])),
                );
                let mut parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
                parent.reasoning_effort = parent_effort.map(str::to_string);
                let spec = fleet_task_to_worker_spec_with_profiles(
                    "worker-1",
                    "run-1",
                    &task,
                    &worker,
                    "deepseek-v4-pro",
                    std::path::Path::new("/tmp"),
                    std::path::Path::new("/tmp"),
                    &[],
                    Some(&parent),
                )
                .expect("role-only consultant should produce a worker spec");

                assert_eq!(spec.role.as_deref(), Some("advisor"));
                assert_eq!(spec.agent_type, FleetRole::Consultant);
                assert_eq!(spec.model, "deepseek-v4-pro", "session model is inherited");
                assert_eq!(spec.runtime_profile.model, ModelRoute::Inherit);
                assert_eq!(
                    spec.runtime_profile.provider, None,
                    "provider is not invented"
                );
                assert_eq!(
                    spec.runtime_profile.reasoning_effort.as_deref(),
                    Some("high"),
                    "role={role}, parent={parent_effort:?}"
                );
                assert!(!spec.runtime_profile.permissions.write);
                assert!(
                    spec.runtime_profile.permissions.network,
                    "counsel reads the web; only workspace mutation is withheld"
                );
                assert_eq!(
                    spec.runtime_profile.shell,
                    codewhale_agent_contract::worker_profile::ShellPolicy::None
                );
                assert_eq!(
                    fleet_worker_launch_reasoning_effort(&task, &[]).as_deref(),
                    Some("high")
                );
                let config = explicit_deepseek_config();
                let route = resolve_fleet_route_with_config(
                    &task,
                    &[],
                    Some("deepseek-v4-pro"),
                    Some(&config),
                )
                .expect("receipt route resolves");
                assert_eq!(route.role.as_deref(), Some("advisor"));
                assert_eq!(route.reasoning_effort.as_deref(), Some("high"));
            }
        }
    }

    #[test]
    fn fleet_worker_spec_inherits_session_run_model_when_unpinned() {
        // No task-level model, no roster profile model: the run model (the
        // session route — the operator's model) must flow through to the
        // worker spec, so the model picked in /model is the model that runs.
        let task = fleet_task("build", None);
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };

        let spec = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &task,
            &worker,
            "deepseek-v4-flash",
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(spec.model, "deepseek-v4-flash");

        // Legacy headless callers with no session still get the auto sentinel.
        let legacy = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &task,
            &worker,
            "auto",
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(legacy.model, "auto");
    }

    #[test]
    fn resolve_fleet_route_uses_session_model_as_run_fallback() {
        // Route receipts must agree with dispatch: when neither the task nor
        // a roster profile pins a model, the session route is the run-level
        // fallback and the receipt records it came from `run.model`. A model
        // name alone is not provider authority, so the config-less helper
        // refuses to invent a route.
        let task = fleet_task("route-session", None);
        assert!(resolve_fleet_route(&task, &[], Some("deepseek-v4-flash")).is_none());
        let config = explicit_deepseek_config();
        let route =
            resolve_fleet_route_with_config(&task, &[], Some("deepseek-v4-flash"), Some(&config))
                .expect("resolved config should authorize the session-model route");
        assert_eq!(route.model_source.as_deref(), Some("run.model"));
        assert_eq!(route.wire_model_id, "deepseek-v4-flash");

        // Task/profile pins still win over the session route.
        let mut profile = agent_profile(
            "audit",
            "reviewer",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("deepseek-v4-pro".to_string());
        let pinned_task = fleet_task(
            "route-pinned",
            Some(worker_profile(
                Some("audit"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        let pinned = resolve_fleet_route_with_config(
            &pinned_task,
            &[profile],
            Some("deepseek-v4-flash"),
            Some(&config),
        )
        .expect("pinned route should resolve under the configured provider");
        assert_eq!(pinned.model_source.as_deref(), Some("agent_profile.model"));
        assert_eq!(pinned.wire_model_id, "deepseek-v4-pro");
    }

    #[test]
    fn validate_fleet_task_routes_rejects_unresolvable_providerless_pin() {
        // #4866 Luna failure mode: a profile pins a concrete model with no
        // explicit provider. The runtime never infers a provider from spelling,
        // so a model that does not resolve on the default provider must be
        // rejected at run creation with a clear error, not fail silently later.
        let mut profile = agent_profile(
            "builder-luna",
            "builder",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("gpt-5.6-luna".to_string());
        let task = fleet_task(
            "luna-build",
            Some(worker_profile(
                Some("builder-luna"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        let err = validate_fleet_task_routes(&[task], &[profile], None, None)
            .expect_err("provider-less unresolvable pin must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("gpt-5.6-luna"), "error names the model: {msg}");
        assert!(
            msg.contains("provider") || msg.contains("inherit"),
            "error tells the user how to fix it: {msg}"
        );
    }

    #[test]
    fn configless_fleet_rejects_providerless_deepseek_named_work() {
        let mut profile = agent_profile(
            "unscoped",
            "builder",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("deepseek-v4-flash".to_string());
        let task = fleet_task(
            "unscoped-build",
            Some(worker_profile(
                Some("unscoped"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        let error = validate_fleet_task_routes(
            std::slice::from_ref(&task),
            std::slice::from_ref(&profile),
            Some("deepseek-v4-flash"),
            None,
        )
        .expect_err("model spelling must not select a provider");
        let message = error.to_string();
        assert!(message.contains("no provider authority"), "{message}");
        assert!(
            message.contains("set the agent profile provider"),
            "{message}"
        );
        assert!(resolve_fleet_route(&task, &[profile], Some("deepseek-v4-flash")).is_none());
    }

    #[test]
    fn configless_fleet_keeps_explicit_non_deepseek_provider_authority() {
        let mut profile = agent_profile(
            "grok-builder",
            "builder",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.provider = Some("xai".to_string());
        let task = fleet_task(
            "grok-build",
            Some(worker_profile(
                Some("grok-builder"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        validate_fleet_task_routes(
            std::slice::from_ref(&task),
            std::slice::from_ref(&profile),
            None,
            None,
        )
        .expect("an explicit built-in provider is route authority");
        let route = resolve_fleet_route(&task, &[profile], None)
            .expect("explicit xAI route should resolve without borrowing session config");
        assert_eq!(route.provider_id, "xai");
        assert_eq!(route.provider_kind, "xai");
        assert_eq!(route.wire_model_id, "grok-4.6");
        assert!(
            !serde_json::to_string(&route)
                .expect("route json")
                .to_ascii_lowercase()
                .contains("credential")
        );
    }

    #[test]
    fn configless_fleet_rejects_explicit_custom_provider_without_live_route_config() {
        let mut profile = agent_profile(
            "private-gateway-builder",
            "builder",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.provider = Some("private-gateway".to_string());
        let task = fleet_task(
            "private-build",
            Some(worker_profile(
                Some("private-gateway-builder"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        let error = validate_fleet_task_routes(
            std::slice::from_ref(&task),
            std::slice::from_ref(&profile),
            None,
            None,
        )
        .expect_err("a custom provider name is not endpoint or model authority");
        let message = error.to_string();
        assert!(message.contains("provider=`private-gateway`"), "{message}");
        assert!(
            message.contains("attach the live route config"),
            "{message}"
        );
        assert!(resolve_fleet_route(&task, &[profile], None).is_none());
    }

    #[test]
    fn validate_fleet_task_routes_rejects_known_foreign_providerless_pin() {
        let mut providers = codewhale_runtime::config::ProvidersConfig::default();
        providers.moonshot.api_key = Some("test-key".to_string());
        let config = {
            let __cw_config_init_1_0 = Some("moonshot".to_string());
            let __cw_config_init_1_1 = Some(providers);
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        };
        let mut profile = agent_profile(
            "moonshot-builder",
            "builder",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("deepseek-v4-pro".to_string());
        let task = fleet_task(
            "foreign-model",
            Some(worker_profile(
                Some("moonshot-builder"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        let err = validate_fleet_task_routes(
            std::slice::from_ref(&task),
            std::slice::from_ref(&profile),
            None,
            Some(&config),
        )
        .expect_err("known foreign model must fail before Fleet dispatch");
        let msg = err.to_string();
        assert!(msg.contains("deepseek-v4-pro"), "names model: {msg}");
        assert!(msg.contains("moonshot"), "names resolved route: {msg}");
        assert!(msg.contains("deepseek"), "names catalog owner: {msg}");

        profile.profile.provider = Some("moonshot".to_string());
        validate_fleet_task_routes(&[task], &[profile], None, Some(&config))
            .expect("an explicit provider+model pair remains deliberate route intent");
    }

    #[test]
    fn validate_fleet_task_routes_accepts_resolvable_pin_and_inherit() {
        // A real default-provider model resolves fine without an explicit pin.
        let mut good = agent_profile(
            "builder-ds",
            "builder",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        good.profile.model = Some("deepseek-v4-flash".to_string());
        good.profile.provider = Some("deepseek".to_string());
        let good_task = fleet_task(
            "ds-build",
            Some(worker_profile(
                Some("builder-ds"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        validate_fleet_task_routes(&[good_task], &[good], None, None)
            .expect("resolvable default-provider model must pass");

        // Inherit (no model pin) is never rejected.
        let inherit = agent_profile(
            "inherit-role",
            "builder",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        let inherit_task = fleet_task(
            "inherit-build",
            Some(worker_profile(
                Some("inherit-role"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        validate_fleet_task_routes(
            &[inherit_task],
            &[inherit],
            None,
            Some(&explicit_deepseek_config()),
        )
        .expect("inherit (no model pin) must pass");
    }

    #[test]
    fn validate_fleet_task_routes_rejects_unsupported_thinking_tier() {
        let mut profile = agent_profile(
            "preview-builder",
            "builder",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("trinity-large-preview".to_string());
        profile.profile.provider = Some("arcee".to_string());
        profile.profile.reasoning_effort = Some("high".to_string());
        let task = fleet_task(
            "preview-build",
            Some(worker_profile(
                Some("preview-builder"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        let err = validate_fleet_task_routes(&[task], &[profile], Some("deepseek-v4-flash"), None)
            .expect_err("unsupported thinking tier must fail before leasing");
        let message = err.to_string();
        assert!(message.contains("does not support thinking"), "{message}");
        assert!(message.contains("trinity-large-preview"), "{message}");
    }

    #[test]
    fn validate_fleet_task_routes_accepts_thinking_capable_route() {
        let mut profile = agent_profile(
            "deep-builder",
            "builder",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("deepseek-v4-flash".to_string());
        profile.profile.provider = Some("deepseek".to_string());
        profile.profile.reasoning_effort = Some("high".to_string());
        let task = fleet_task(
            "deep-build",
            Some(worker_profile(
                Some("deep-builder"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        validate_fleet_task_routes(&[task], &[profile], Some("deepseek-v4-flash"), None)
            .expect("thinking-capable route must pass");
    }

    #[test]
    fn validate_fleet_task_routes_keeps_non_explicit_thinking_modes_route_agnostic() {
        for effort in ["inherit", "auto", "off"] {
            let mut profile = agent_profile(
                "preview-builder",
                "builder",
                None,
                codewhale_config::FleetLoadout::Inherit,
            );
            profile.profile.model = Some("trinity-large-preview".to_string());
            profile.profile.provider = Some("arcee".to_string());
            profile.profile.reasoning_effort = Some(effort.to_string());
            let task = fleet_task(
                "preview-build",
                Some(worker_profile(
                    Some("preview-builder"),
                    None,
                    None,
                    None,
                    None,
                    vec![],
                )),
            );

            validate_fleet_task_routes(&[task], &[profile], Some("deepseek-v4-flash"), None)
                .unwrap_or_else(|error| panic!("{effort} must remain valid: {error}"));
        }
    }

    #[test]
    fn resolve_fleet_route_honors_explicit_profile_provider_not_the_default() {
        // EPIC #2608 / #4093: the resolved provider must come ONLY from the
        // profile's explicit `provider` field — never inferred from a
        // provider-shaped substring in `model`, and never the parent/session
        // route's provider. `deepseek-v4-flash` is deliberately DeepSeek-shaped
        // while the profile pins `openrouter`.
        let mut profile = agent_profile(
            "cross-provider",
            "scout",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("deepseek-v4-flash".to_string());
        profile.profile.provider = Some("openrouter".to_string());
        profile.profile.reasoning_effort = Some("max".to_string());
        let task = fleet_task(
            "route-cross-provider",
            Some(worker_profile(
                Some("cross-provider"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        // The "parent"/session route is a completely different provider's
        // model, proving the resolved route does not fall back to it.
        let route = resolve_fleet_route(&task, &[profile], Some("deepseek-v4-pro"))
            .expect("cross-provider profile route should resolve");

        assert_eq!(route.model_source.as_deref(), Some("agent_profile.model"));

        // Resolving `openrouter` directly with the same selector is the
        // ground truth for what this route SHOULD produce — comparing
        // against it (rather than hardcoding a wire id) proves the profile's
        // provider actually drove resolution, whatever wire id/aggregator
        // mapping the resolver's catalog assigns.
        let openrouter_candidate = resolve_route_candidate(
            ProviderKind::Openrouter,
            Some("deepseek-v4-flash"),
            None,
            None,
            None,
            None,
        )
        .expect("openrouter should resolve the pinned model directly");
        assert_eq!(
            route.wire_model_id,
            openrouter_candidate.wire_model_id().as_str()
        );
        assert_eq!(
            route.provider_id,
            openrouter_candidate.provider_id().as_str()
        );
        assert_eq!(
            route.provider_kind,
            openrouter_candidate.provider_kind().as_str()
        );
        assert_eq!(route.reasoning_effort.as_deref(), Some("max"));
        // Differs from DeepSeek — the pre-#4093 hardcoded default AND the
        // parent/session's provider.
        assert_ne!(route.provider_id, "deepseek");
    }

    #[test]
    fn cross_provider_profile_saves_reloads_and_resolves_to_its_own_provider() {
        // Required cross-provider save/load/launch coverage for #4093: create
        // a Fleet profile whose provider differs from the parent/session
        // provider, save it to a real TOML file, reload it from disk through
        // the same loader Fleet uses, then resolve its route and confirm the
        // resolved provider+model are the SAVED ones — never the parent's.
        let draft = crate::fleet::profile::FleetProfileDraft {
            id: "scout-openrouter".to_string(),
            display_name: Some("Scout".to_string()),
            description: Some("Cross-provider scout profile.".to_string()),
            role_hint: "scout".to_string(),
            model_class_hint: None,
            model: Some("deepseek-v4-flash".to_string()),
            provider: Some("openrouter".to_string()),
            reasoning_effort: Some("max".to_string()),
            instructions: None,
        };

        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(draft.file_name()), draft.render_toml()).unwrap();
        let profiles = crate::fleet::profile::load_agent_profiles_from_dir(dir.path())
            .expect("rendered profile TOML loads");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].profile.provider.as_deref(), Some("openrouter"));
        assert_eq!(profiles[0].profile.reasoning_effort.as_deref(), Some("max"));
        assert_eq!(
            profiles[0].profile.model.as_deref(),
            Some("deepseek-v4-flash")
        );

        let task = fleet_task(
            "route-saved-profile",
            Some(worker_profile(
                Some("scout-openrouter"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        // "Parent"/session route: a different provider's model entirely, so a
        // fallback to it would be an obvious, loud test failure.
        let route = resolve_fleet_route(&task, &profiles, Some("deepseek-v4-pro"))
            .expect("saved cross-provider profile route should resolve");

        let openrouter_candidate = resolve_route_candidate(
            ProviderKind::Openrouter,
            Some("deepseek-v4-flash"),
            None,
            None,
            None,
            None,
        )
        .expect("openrouter should resolve the saved model directly");
        assert_eq!(
            route.wire_model_id,
            openrouter_candidate.wire_model_id().as_str()
        );
        assert_eq!(
            route.provider_id,
            openrouter_candidate.provider_id().as_str()
        );
        assert_eq!(route.reasoning_effort.as_deref(), Some("max"));
        assert_ne!(route.provider_id, "deepseek");
    }

    #[test]
    fn resolve_fleet_route_preserves_exact_named_custom_provider_without_secrets() {
        let mut profile = agent_profile(
            "local",
            "scout",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("qwen-2.5-7b".to_string());
        profile.profile.provider = Some("lm-studio".to_string());
        let task = fleet_task(
            "custom-receipt",
            Some(worker_profile(
                Some("local"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        assert!(
            resolve_fleet_route(&task, &[profile.clone()], Some("deepseek-v4-pro")).is_none(),
            "a profile string alone is not proof that a named custom route exists"
        );
        let config = {
            let __cw_config_init_2_0 = Some("lm-studio".to_string());
            let __cw_config_init_2_1 = Some(codewhale_runtime::config::ProvidersConfig {
                custom: std::collections::HashMap::from([(
                    "lm-studio".to_string(),
                    codewhale_runtime::config::ProviderConfig {
                        kind: Some("openai-compatible".to_string()),
                        base_url: Some("http://127.0.0.1:1234/v1".to_string()),
                        model: Some("qwen-2.5-7b".to_string()),
                        api_key: Some("receipt-must-redact-this".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            });
            let mut __cw_config_init_2_value: Config = Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        };
        let route = resolve_fleet_route_with_config(
            &task,
            &[profile],
            Some("deepseek-v4-pro"),
            Some(&config),
        )
        .expect("live config should prove the named custom route");

        assert_eq!(route.provider_id, "lm-studio");
        assert_eq!(route.provider_exact_id.as_deref(), Some("lm-studio"));
        assert_eq!(route.provider_kind, "custom");
        assert_eq!(route.wire_model_id, "qwen-2.5-7b");
        assert_eq!(route.protocol, "chat_completions");
        assert_eq!(route.model_source.as_deref(), Some("agent_profile.model"));
        assert_eq!(route.source, "runtime_route");

        // The exact identity and wire model are durable, while endpoint/auth
        // config remains outside the receipt. The generic Custom descriptor's
        // placeholder endpoint is never serialized either.
        let json = serde_json::to_string(&route).unwrap();
        let haystack = json.to_ascii_lowercase();
        assert!(haystack.contains("lm-studio"));
        assert!(!haystack.contains("base_url"));
        assert!(!haystack.contains("http://"));
        assert!(!haystack.contains("https://"));
        for needle in [
            "api_key",
            "apikey",
            "api-key",
            "authorization",
            "bearer ",
            "auth_token",
            "auth-token",
            "password",
            "credential",
            "sk-ant-",
            "sk-proj-",
            "sk-or-",
            "secret",
            "receipt-must-redact-this",
        ] {
            assert!(
                !haystack.contains(needle),
                "named-custom route JSON must not contain secret marker {needle:?}: {json}"
            );
        }
    }

    #[test]
    fn fleet_receipt_prefers_live_case_colliding_custom_identity() {
        let mut profile = agent_profile(
            "case-local",
            "scout",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("case-model".to_string());
        profile.profile.provider = Some("CUSTOM".to_string());
        let task = fleet_task(
            "case-custom-receipt",
            Some(worker_profile(
                Some("case-local"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        let config = {
            let __cw_config_init_3_0 = Some("CUSTOM".to_string());
            let __cw_config_init_3_1 = Some(codewhale_runtime::config::ProvidersConfig {
                custom: std::collections::HashMap::from([(
                    "CUSTOM".to_string(),
                    codewhale_runtime::config::ProviderConfig {
                        kind: Some("openai-compatible".to_string()),
                        base_url: Some("http://127.0.0.1:5678/v1".to_string()),
                        model: Some("case-model".to_string()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            });
            let mut __cw_config_init_3_value: Config = Default::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value.providers = __cw_config_init_3_1;
            __cw_config_init_3_value
        };

        let route = resolve_fleet_route_with_config(
            &task,
            &[profile],
            Some("deepseek-v4-pro"),
            Some(&config),
        )
        .expect("live config route proof");
        assert_eq!(route.provider_id, "CUSTOM");
        assert_eq!(route.provider_exact_id.as_deref(), Some("CUSTOM"));
        assert_eq!(route.provider_kind, "custom");
        assert_eq!(route.source, "runtime_route");
    }

    #[test]
    fn worker_report_route_preserves_literal_custom_vs_idless_root_without_local_resolution() {
        let task = fleet_task("reported-custom", None);
        let literal = resolve_fleet_route_from_worker_report(
            &task,
            &[],
            Some("manager-model-y"),
            "custom",
            Some("custom"),
            "worker-model-x",
        )
        .expect("literal custom worker report");
        let root = resolve_fleet_route_from_worker_report(
            &task,
            &[],
            Some("manager-model-y"),
            "custom",
            None,
            "worker-model-root",
        )
        .expect("idless root custom worker report");

        assert_eq!(literal.provider_id, "custom");
        assert_eq!(literal.provider_exact_id.as_deref(), Some("custom"));
        assert_eq!(literal.wire_model_id, "worker-model-x");
        assert_eq!(root.provider_id, "custom");
        assert_eq!(root.provider_exact_id, None);
        assert_eq!(root.wire_model_id, "worker-model-root");
        assert_eq!(literal.source, "worker_terminal_metadata");
        assert_eq!(root.source, "worker_terminal_metadata");

        let literal_json = serde_json::to_value(&literal).unwrap();
        let root_json = serde_json::to_value(&root).unwrap();
        assert_eq!(literal_json["provider_exact_id"], "custom");
        assert!(root_json.get("provider_exact_id").is_none());
        assert_ne!(literal, root);
    }

    #[test]
    fn worker_report_builtin_route_does_not_become_custom_exact_route() {
        let task = fleet_task("reported-built-in", None);
        let route = resolve_fleet_route_from_worker_report(
            &task,
            &[],
            None,
            "deepseek",
            None,
            "deepseek-v4-pro",
        )
        .expect("built-in worker report");

        assert_eq!(route.provider_id, "deepseek");
        assert_eq!(route.provider_exact_id, None);
        assert_eq!(route.provider_kind, "deepseek");

        assert!(
            resolve_fleet_route_from_worker_report(
                &task,
                &[],
                None,
                "deepseek",
                Some("custom-x"),
                "deepseek-v4-pro",
            )
            .is_none(),
            "built-in kind plus custom exact id is contradictory provenance"
        );
        assert!(
            resolve_fleet_route_from_worker_report(
                &task,
                &[],
                None,
                "custom",
                Some("   "),
                "root-model",
            )
            .is_none(),
            "present-empty exact id must not collapse to idless custom root"
        );
    }

    #[test]
    fn fleet_worker_launch_route_is_explicit_provider_only() {
        // The LAUNCH resolver (twin of the receipt) must emit a provider ONLY
        // when the profile explicitly pins one, and NEVER infer it from a
        // provider-shaped model id (EPIC #2608).

        // 1) Explicit cross-provider pin: model + provider both come from the
        //    profile, not the parent/session model.
        let mut pinned = agent_profile(
            "cross",
            "scout",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        pinned.profile.model = Some("glm-5.2".to_string());
        pinned.profile.provider = Some("openrouter".to_string());
        pinned.profile.reasoning_effort = Some("high".to_string());
        let pinned_task = fleet_task(
            "launch-pinned",
            Some(worker_profile(
                Some("cross"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        let pinned_profiles = vec![pinned];
        let (model, provider) =
            fleet_worker_launch_route(&pinned_task, &pinned_profiles, "deepseek-v4-pro");
        assert_eq!(model, "glm-5.2");
        assert_eq!(provider.as_deref(), Some("openrouter"));
        assert_eq!(
            fleet_worker_launch_reasoning_effort(&pinned_task, &pinned_profiles).as_deref(),
            Some("high")
        );

        // 1b) User-named OpenAI-compatible providers are launchable too: keep
        //     the exact provider id so `codewhale exec --provider lm-studio`
        //     can resolve `[providers.lm-studio]` from config (#3965).
        let mut custom = agent_profile(
            "local",
            "scout",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        custom.profile.model = Some("qwen-2.5-7b".to_string());
        custom.profile.provider = Some("lm-studio".to_string());
        let custom_task = fleet_task(
            "launch-custom",
            Some(worker_profile(
                Some("local"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        let custom_profiles = vec![custom];
        let (model, provider) =
            fleet_worker_launch_route(&custom_task, &custom_profiles, "deepseek-v4-pro");
        assert_eq!(model, "qwen-2.5-7b");
        assert_eq!(provider.as_deref(), Some("lm-studio"));

        // 2) A DeepSeek-shaped model with NO explicit provider must NOT infer a
        //    provider — provider stays None so the worker keeps its own session
        //    default, and no `--provider` is emitted.
        let mut model_only = agent_profile(
            "modelonly",
            "scout",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        model_only.profile.model = Some("deepseek-v4-flash".to_string());
        let model_only_task = fleet_task(
            "launch-model-only",
            Some(worker_profile(
                Some("modelonly"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        let model_only_profiles = vec![model_only];
        let (model, provider) =
            fleet_worker_launch_route(&model_only_task, &model_only_profiles, "deepseek-v4-pro");
        assert_eq!(model, "deepseek-v4-flash");
        assert_eq!(provider, None);
        assert_eq!(
            fleet_worker_launch_reasoning_effort(&model_only_task, &model_only_profiles),
            None
        );

        // 3) No profile at all: run-level model, no provider (unchanged).
        let bare = fleet_task("launch-bare", None);
        let (model, provider) = fleet_worker_launch_route(&bare, &[], "deepseek-v4-pro");
        assert_eq!(model, "deepseek-v4-pro");
        assert_eq!(provider, None);
    }

    #[test]
    fn fleet_worker_spec_rejects_unknown_agent_profile_before_spawn() {
        let task = fleet_task(
            "review",
            Some(worker_profile(
                Some("missing"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        let err = validate_task_agent_profiles(&[task], &[])
            .expect_err("unknown agent profile must fail validation");

        assert!(
            err.to_string()
                .contains("references unknown agent profile selector \"missing\"")
        );
    }

    #[test]
    fn fleet_task_member_selector_accepts_display_model_and_keeps_member_posture() {
        let mut profile = agent_profile(
            "flash-scout",
            "scout",
            Some("Inspect the requested surface."),
            codewhale_config::FleetLoadout::Fast,
        );
        profile.display_name = Some("Scout One".to_string());
        profile.profile.provider = Some("deepseek".to_string());
        profile.profile.model = Some("deepseek-v4-flash".to_string());
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };

        for task in [
            fleet_task(
                "profile-selector",
                Some(worker_profile(
                    Some("DeepSeek V4 Flash"),
                    None,
                    None,
                    None,
                    None,
                    vec![],
                )),
            ),
            fleet_task(
                "legacy-role-selector",
                Some(worker_profile(
                    None,
                    Some("DeepSeek V4 Flash"),
                    None,
                    None,
                    None,
                    vec![],
                )),
            ),
        ] {
            let spec = fleet_task_to_worker_spec_with_profiles(
                "worker-1",
                "run-1",
                &task,
                &worker,
                "auto",
                Path::new("/tmp"),
                Path::new("/tmp"),
                &[profile.clone()],
                None,
            )
            .expect("display-model selector should resolve");
            assert_eq!(spec.model, "deepseek-v4-flash");
            assert_eq!(spec.role.as_deref(), Some("explore"));
            assert_eq!(spec.agent_type, FleetRole::Scout);
            assert!(spec.objective.contains("Fleet profile: flash-scout"));
        }
    }

    #[test]
    fn fleet_task_member_selector_reports_ambiguity() {
        let mut first = agent_profile(
            "scout-a",
            "scout",
            None,
            codewhale_config::FleetLoadout::Fast,
        );
        first.profile.provider = Some("deepseek".to_string());
        first.profile.model = Some("deepseek-v4-flash".to_string());
        let mut second = agent_profile(
            "scout-b",
            "reviewer",
            None,
            codewhale_config::FleetLoadout::Fast,
        );
        second.profile.provider = Some("deepseek".to_string());
        second.profile.model = Some("deepseek-v4-flash".to_string());
        let task = fleet_task(
            "ambiguous",
            Some(worker_profile(
                Some("DeepSeek V4 Flash"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );

        let error = validate_task_agent_profiles(&[task], &[first, second])
            .expect_err("ambiguous selector must fail before lease");
        assert!(error.to_string().contains("is ambiguous"), "{error:#}");
        assert!(error.to_string().contains("scout-a"), "{error:#}");
        assert!(error.to_string().contains("scout-b"), "{error:#}");
    }

    #[test]
    fn unmatched_legacy_worker_role_remains_a_runtime_posture() {
        let task = fleet_task(
            "legacy-role",
            Some(worker_profile(
                None,
                Some("reviewer"),
                None,
                None,
                None,
                vec![],
            )),
        );
        assert!(resolve_task_agent_profile(&task, &[]).unwrap().is_none());
        assert_eq!(
            effective_fleet_role(task.worker.as_ref(), None).as_deref(),
            Some("reviewer")
        );
        assert_eq!(
            runtime_role_for_member(
                effective_fleet_role(task.worker.as_ref(), None)
                    .as_deref()
                    .unwrap_or_default()
            ),
            FleetRole::Reviewer
        );
    }

    #[test]
    fn ambiguous_legacy_role_never_falls_back_to_anonymous_posture() {
        let first = agent_profile(
            "scout-a",
            "scout",
            None,
            codewhale_config::FleetLoadout::Fast,
        );
        let second = agent_profile(
            "scout-b",
            "scout",
            None,
            codewhale_config::FleetLoadout::Fast,
        );
        let mut task = fleet_task(
            "ambiguous-role",
            Some(worker_profile(
                None,
                Some("scout"),
                None,
                None,
                None,
                vec![],
            )),
        );

        let error =
            freeze_fleet_task_members(std::slice::from_mut(&mut task), &[first, second], true)
                .expect_err("an ambiguous role must fail before persistence");
        assert!(error.to_string().contains("is ambiguous"), "{error:#}");
        assert!(error.to_string().contains("scout-a"), "{error:#}");
        assert!(error.to_string().contains("scout-b"), "{error:#}");
    }

    #[test]
    fn exact_roster_requires_every_task_to_name_one_member() {
        let profile = agent_profile(
            "flash-scout",
            "scout",
            None,
            codewhale_config::FleetLoadout::Fast,
        );
        let mut missing = fleet_task(
            "missing-member",
            Some(worker_profile(
                None,
                Some("reviewer"),
                None,
                None,
                None,
                vec![],
            )),
        );
        let error = freeze_fleet_task_members(
            std::slice::from_mut(&mut missing),
            std::slice::from_ref(&profile),
            true,
        )
        .expect_err("an exact Fleet cannot silently fall back to a posture");
        assert!(
            error.to_string().contains("does not name a member"),
            "{error:#}"
        );

        let mut unspecified = fleet_task("unspecified-member", None);
        let error =
            freeze_fleet_task_members(std::slice::from_mut(&mut unspecified), &[profile], true)
                .expect_err("an exact Fleet task must name a member");
        assert!(
            error.to_string().contains("must name one member"),
            "{error:#}"
        );
    }

    #[test]
    fn frozen_member_selection_survives_roster_edits_and_task_round_trip() {
        let mut original = agent_profile(
            "flash-scout",
            "scout",
            Some("Inspect the selected surface."),
            codewhale_config::FleetLoadout::Fast,
        );
        original.display_name = Some("Scout One".to_string());
        original.profile.provider = Some("deepseek".to_string());
        original.profile.model = Some("deepseek-v4-flash".to_string());
        original.profile.reasoning_effort = Some("medium".to_string());
        original.profile.delegation.max_spawn_depth = Some(2);

        let mut task = fleet_task(
            "durable-selection",
            Some(worker_profile(
                Some("DeepSeek V4 Flash"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        freeze_fleet_task_members(std::slice::from_mut(&mut task), &[original], true)
            .expect("selection should freeze");
        assert_eq!(
            task.worker.as_ref().unwrap().agent_profile.as_deref(),
            Some("member:flash-scout")
        );

        // Exercise the actual durable representation, then present a live
        // roster whose same id now points somewhere else. Launch must keep the
        // run-creation snapshot rather than re-resolving the edited profile.
        let task: FleetTaskSpec =
            serde_json::from_value(serde_json::to_value(task).unwrap()).unwrap();
        let mut edited = agent_profile(
            "flash-scout",
            "reviewer",
            Some("This edit happened after queueing."),
            codewhale_config::FleetLoadout::Inherit,
        );
        edited.profile.provider = Some("openrouter".to_string());
        edited.profile.model = Some("gpt-5.6".to_string());

        let edited_profiles = [edited];
        let resolved = resolve_task_agent_profile(&task, &edited_profiles)
            .unwrap()
            .expect("frozen member");
        assert_eq!(resolved.profile.role.name, "explore");
        assert_eq!(resolved.profile.provider.as_deref(), Some("deepseek"));
        assert_eq!(resolved.profile.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(resolved.profile.delegation.max_spawn_depth, Some(2));
    }

    #[test]
    fn legacy_advisory_member_id_is_selected_before_role_canonicalization() {
        let profile = agent_profile(
            "advisor",
            "reviewer",
            None,
            codewhale_config::FleetLoadout::Inherit,
        );
        let mut task = fleet_task(
            "advisor-selection",
            Some(worker_profile(
                None,
                Some("advisor"),
                None,
                None,
                None,
                vec![],
            )),
        );

        freeze_fleet_task_members(std::slice::from_mut(&mut task), &[profile], true)
            .expect("exact member id should win");
        let worker = task.worker.as_ref().unwrap();
        assert_eq!(worker.agent_profile.as_deref(), Some("member:advisor"));
        assert_eq!(worker.role.as_deref(), Some("reviewer"));
    }

    #[test]
    fn selected_exact_member_route_rejects_conflicting_task_model() {
        let mut profile = agent_profile(
            "flash-scout",
            "scout",
            None,
            codewhale_config::FleetLoadout::Fast,
        );
        profile.profile.provider = Some("deepseek".to_string());
        profile.profile.model = Some("deepseek-v4-flash".to_string());
        let task = fleet_task(
            "conflict",
            Some(worker_profile(
                Some("flash-scout"),
                None,
                None,
                None,
                Some("deepseek-v4-pro"),
                vec![],
            )),
        );

        let error = validate_task_agent_profiles(&[task], &[profile])
            .expect_err("conflicting task model must fail before lease");
        assert!(error.to_string().contains("pinned model"), "{error:#}");
        assert!(error.to_string().contains("worker.model"), "{error:#}");
    }

    #[test]
    fn pinned_role_lookup_ignores_builtins_and_rejects_semantic_ambiguity() {
        let mut builtin = agent_profile("reviewer", "reviewer", None, FleetLoadout::Inherit);
        builtin.origin = ProfileOrigin::BuiltIn;
        builtin.profile.model = Some("builtin-default".into());
        let mut first = agent_profile("review-choice", "reviewer", None, FleetLoadout::Inherit);
        first.profile.provider = Some("openrouter".into());
        first.profile.model = Some("qwen/qwen3.7-plus".into());
        let selected = resolve_pinned_role_profile(&[builtin.clone(), first.clone()], "review")
            .unwrap()
            .expect("the unique saved role wins over the builtin posture");
        assert_eq!(selected.id, "review-choice");
        assert_eq!(selected.profile.provider.as_deref(), Some("openrouter"));
        assert_eq!(selected.profile.model.as_deref(), Some("qwen/qwen3.7-plus"));

        let mut second = first.clone();
        second.id = "reviewer".into();
        second.profile.provider = Some("TeamA".into());
        second.profile.model = Some("private-reviewer".into());
        for profiles in [
            vec![builtin.clone(), first.clone(), second.clone()],
            vec![second, first, builtin],
        ] {
            let error = resolve_pinned_role_profile(&profiles, "reviewer")
                .expect_err("an exact member id must not hide a second same-role pin");
            assert!(error.to_string().contains("ambiguous"), "{error:#}");
            assert!(error.to_string().contains("review-choice"), "{error:#}");
        }
    }

    #[test]
    fn pinned_model_comparison_preserves_exact_provider_identity_and_wire_namespace() {
        assert!(requested_model_matches_pin(
            "model-x",
            "model-x",
            Some("TeamA")
        ));
        assert!(requested_model_matches_pin(
            "TeamA/model-x",
            "model-x",
            Some("TeamA")
        ));
        assert!(!requested_model_matches_pin(
            "TeamA/MODEL-X",
            "model-x",
            Some("TeamA")
        ));
        assert!(!requested_model_matches_pin(
            "teama/model-x",
            "model-x",
            Some("TeamA")
        ));
        assert!(!requested_model_matches_pin(
            "TeamA/model-x",
            "model-x",
            Some("teama")
        ));
        assert!(requested_model_matches_pin(
            "org/model-x",
            "org/model-x",
            Some("TeamA")
        ));
        assert!(requested_model_matches_pin(
            "TeamA/org/model-x",
            "org/model-x",
            Some("TeamA")
        ));
        assert!(!requested_model_matches_pin(
            "Other/org/model-x",
            "org/model-x",
            Some("TeamA")
        ));
        assert!(!requested_model_matches_pin(
            "TeamA/model-x",
            "model-x",
            None
        ));
    }

    #[test]
    fn saved_case_distinct_model_conflict_is_rejected_before_freeze() {
        for provider in [None, Some("TeamA")] {
            let mut profile =
                agent_profile("review-choice", "reviewer", None, FleetLoadout::Inherit);
            profile.profile.model = Some("Preview-fixture".into());
            profile.profile.provider = provider.map(str::to_string);
            for requested in ["Preview-fixture", "preview-fixture"] {
                let mut selectors = vec![requested.to_string()];
                if let Some(provider) = provider {
                    selectors.push(format!("{provider}/{requested}"));
                }
                for selector in selectors {
                    let mut task = fleet_task(
                        "review",
                        Some(worker_profile(
                            Some("review-choice"),
                            None,
                            None,
                            None,
                            Some(&selector),
                            vec![],
                        )),
                    );
                    let result = freeze_fleet_task_members(
                        std::slice::from_mut(&mut task),
                        &[profile.clone()],
                        false,
                    );
                    if requested == "Preview-fixture" {
                        result.unwrap();
                        assert!(task.metadata.contains_key(FROZEN_FLEET_MEMBER_METADATA_KEY));
                    } else {
                        assert!(result.unwrap_err().to_string().contains("conflicts"));
                        assert!(!task.metadata.contains_key(FROZEN_FLEET_MEMBER_METADATA_KEY));
                    }
                    assert_eq!(profile.profile.model.as_deref(), Some("Preview-fixture"));
                }
            }
        }
    }

    #[test]
    fn providerless_saved_profile_pin_refuses_conflicts_before_freeze() {
        let mut profile = agent_profile("review-choice", "reviewer", None, FleetLoadout::Inherit);
        profile.profile.model = Some("deepseek-v4-flash".into());
        let mut conflict = fleet_task(
            "review",
            Some(worker_profile(
                Some("review-choice"),
                None,
                None,
                None,
                Some("deepseek-v4-pro"),
                vec![],
            )),
        );
        let error = freeze_fleet_task_members(
            std::slice::from_mut(&mut conflict),
            &[profile.clone()],
            false,
        )
        .expect_err("providerless saved models are still explicit profile pins");
        assert!(error.to_string().contains("conflicts"), "{error:#}");
        assert!(
            !conflict
                .metadata
                .contains_key(FROZEN_FLEET_MEMBER_METADATA_KEY)
        );

        let mut agreeing = fleet_task(
            "review",
            Some(worker_profile(
                Some("review-choice"),
                None,
                None,
                None,
                Some("deepseek-v4-flash"),
                vec![],
            )),
        );
        freeze_fleet_task_members(
            std::slice::from_mut(&mut agreeing),
            &[profile.clone()],
            false,
        )
        .unwrap();
        assert!(
            agreeing
                .metadata
                .contains_key(FROZEN_FLEET_MEMBER_METADATA_KEY)
        );
        assert_eq!(
            fleet_worker_launch_route(&agreeing, &[profile], "deepseek-v4-pro"),
            ("deepseek-v4-flash".into(), None),
            "freezing a model-only pin never fabricates provider authority"
        );
    }

    #[test]
    fn fleet_task_max_steps_zero_or_omitted_is_unbounded_and_positive_is_enforced() {
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };
        for (max_steps, expected) in [(None, 0), (Some(0), 0), (Some(7), 7)] {
            let mut task = fleet_task("budget", None);
            task.budget = Some(FleetTaskBudget {
                max_tokens: None,
                max_steps,
                max_tool_calls: Some(99),
                max_seconds: None,
            });
            let spec = fleet_task_to_worker_spec_with_profiles(
                "worker-1",
                "run-1",
                &task,
                &worker,
                "auto",
                Path::new("/tmp"),
                Path::new("/tmp"),
                &[],
                None,
            )
            .expect("budget should build");
            assert_eq!(spec.max_steps, expected, "max_steps={max_steps:?}");
        }
    }

    #[test]
    fn fleet_worker_spec_keeps_profile_pins_and_unpinned_task_choices() {
        let mut profile = agent_profile(
            "reviewer",
            "reviewer",
            Some("Focus on regressions and missing tests."),
            codewhale_config::FleetLoadout::Inherit,
        );
        profile.profile.model = Some("glm-5.2".to_string());
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };

        let profile_model_spec = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &fleet_task(
                "review",
                Some(worker_profile(
                    Some("reviewer"),
                    None,
                    None,
                    None,
                    None,
                    vec![],
                )),
            ),
            &worker,
            "auto",
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[profile.clone()],
            None,
        )
        .unwrap();

        assert_eq!(profile_model_spec.model, "glm-5.2");
        assert_eq!(
            profile_model_spec.runtime_profile.model,
            ModelRoute::Fixed("glm-5.2".to_string())
        );

        let conflicting_task = fleet_task(
            "review",
            Some(worker_profile(
                Some("reviewer"),
                None,
                None,
                None,
                Some("deepseek-v4-pro"),
                vec![],
            )),
        );
        let error = fleet_task_to_worker_spec_with_profiles(
            "worker-2",
            "run-1",
            &conflicting_task,
            &worker,
            "auto",
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[profile.clone()],
            None,
        )
        .expect_err("a task cannot replace a providerless saved profile pin");
        assert!(error.to_string().contains("conflicts"), "{error:#}");

        profile.profile.model = None;
        let task_model_spec = fleet_task_to_worker_spec_with_profiles(
            "worker-2",
            "run-1",
            &fleet_task(
                "review",
                Some(worker_profile(
                    Some("reviewer"),
                    None,
                    None,
                    None,
                    Some("deepseek-v4-pro"),
                    vec![],
                )),
            ),
            &worker,
            "auto",
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[profile],
            None,
        )
        .unwrap();

        assert_eq!(task_model_spec.model, "deepseek-v4-pro");
        assert_eq!(
            task_model_spec.runtime_profile.model,
            ModelRoute::Fixed("deepseek-v4-pro".to_string())
        );
    }

    #[test]
    fn fleet_worker_spec_carries_agent_profile_provider_through_runtime_contract() {
        let mut profile = agent_profile(
            "scout-openrouter",
            "scout",
            Some("Use the OpenRouter scout route."),
            codewhale_config::FleetLoadout::Fast,
        );
        profile.profile.model = Some("deepseek-v4-flash".to_string());
        profile.profile.provider = Some("openrouter".to_string());
        profile.profile.reasoning_effort = Some("max".to_string());
        let task = fleet_task(
            "scout",
            Some(worker_profile(
                Some("scout-openrouter"),
                None,
                None,
                None,
                None,
                vec![],
            )),
        );
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };
        let mut parent = WorkerRuntimeProfile::for_role(FleetRole::Worker);
        parent.provider = Some("deepseek".to_string());
        parent.reasoning_effort = Some("low".to_string());
        parent.max_spawn_depth = 3;

        let spec = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &task,
            &worker,
            "deepseek-v4-pro",
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[profile],
            Some(&parent),
        )
        .unwrap();

        assert_eq!(spec.model, "deepseek-v4-flash");
        assert_eq!(
            spec.runtime_profile.model,
            ModelRoute::Fixed("deepseek-v4-flash".to_string())
        );
        assert_eq!(spec.runtime_profile.provider.as_deref(), Some("openrouter"));
        assert_eq!(
            spec.runtime_profile.reasoning_effort.as_deref(),
            Some("max")
        );
        assert_eq!(spec.runtime_profile.max_spawn_depth, 3);
        assert_eq!(spec.runtime_profile.spawn_depth, 1);
    }

    #[test]
    fn fleet_worker_spec_model_route_precedence_is_profile_unpinned_task_then_session() {
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };
        let run_model = "deepseek-v4-pro";

        let mut profile =
            agent_profile("scout", "scout", None, codewhale_config::FleetLoadout::Fast);
        profile.profile.model = Some("deepseek-v4-flash".to_string());
        let mut unpinned_profile = profile.clone();
        unpinned_profile.profile.model = None;

        let task_model = fleet_task_to_worker_spec_with_profiles(
            "worker-task",
            "run-1",
            &fleet_task(
                "task-model",
                Some(worker_profile(
                    Some("scout"),
                    None,
                    None,
                    None,
                    Some("deepseek-v4.1"),
                    vec![],
                )),
            ),
            &worker,
            run_model,
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[unpinned_profile],
            None,
        )
        .unwrap();
        assert_eq!(task_model.model, "deepseek-v4.1");
        assert_eq!(
            task_model.runtime_profile.model,
            ModelRoute::Fixed("deepseek-v4.1".to_string())
        );

        let profile_model = fleet_task_to_worker_spec_with_profiles(
            "worker-profile",
            "run-1",
            &fleet_task(
                "profile-model",
                Some(worker_profile(
                    Some("scout"),
                    None,
                    None,
                    None,
                    None,
                    vec![],
                )),
            ),
            &worker,
            run_model,
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[profile],
            None,
        )
        .unwrap();
        assert_eq!(profile_model.model, "deepseek-v4-flash");
        assert_eq!(
            profile_model.runtime_profile.model,
            ModelRoute::Fixed("deepseek-v4-flash".to_string())
        );

        let role_default = fleet_task_to_worker_spec_with_profiles(
            "worker-role",
            "run-1",
            &fleet_task(
                "role-default",
                Some(worker_profile(
                    None,
                    Some("scout"),
                    Some("fast"),
                    None,
                    None,
                    vec![],
                )),
            ),
            &worker,
            run_model,
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(role_default.model, run_model);
        assert_eq!(role_default.runtime_profile.model, ModelRoute::Inherit);

        let inherited = fleet_task_to_worker_spec_with_profiles(
            "worker-inherit",
            "run-1",
            &fleet_task("inherit", None),
            &worker,
            run_model,
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[],
            None,
        )
        .unwrap();
        assert_eq!(inherited.model, run_model);
        assert_eq!(inherited.runtime_profile.model, ModelRoute::Inherit);
    }

    #[test]
    fn fleet_worker_spec_intersects_task_tools_with_parent_runtime_profile() {
        let task = fleet_task(
            "build",
            Some(worker_profile(
                None,
                Some("builder"),
                None,
                Some("fast"),
                None,
                vec!["read_file", "apply_patch"],
            )),
        );
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };
        let mut parent = WorkerRuntimeProfile::for_role(FleetRole::Scout);
        parent.tools = ToolScope::Explicit(vec!["read_file".to_string()]);
        parent.max_spawn_depth = 2;

        let spec = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &task,
            &worker,
            "auto",
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[],
            Some(&parent),
        )
        .unwrap();

        assert_eq!(spec.agent_type, FleetRole::Builder);
        assert!(!spec.runtime_profile.permissions.write);
        assert!(
            spec.runtime_profile.permissions.network,
            "read-only inspection lanes keep network reach"
        );
        assert_eq!(
            spec.runtime_profile.shell,
            codewhale_agent_contract::worker_profile::ShellPolicy::Full
        );
        assert_eq!(
            spec.runtime_profile.tools,
            ToolScope::Explicit(vec!["read_file".to_string()])
        );
        assert_eq!(spec.runtime_profile.model, ModelRoute::Inherit);
        assert_eq!(spec.max_spawn_depth, 2);
        assert_eq!(spec.spawn_depth, 1);

        let permissions = codewhale_agent_contract::role::fleet_effective_permissions(
            &spec.agent_type,
            &spec.runtime_profile,
            None,
            None,
        );
        assert!(!permissions.write);
        assert!(
            permissions.network,
            "read-only inspection lanes keep network reach"
        );
        assert_eq!(permissions.shell, "full");
        assert_eq!(permissions.tool_scope, "explicit");
        assert_eq!(permissions.tools, vec!["read_file".to_string()]);
        assert!(permissions.background);
        assert_eq!(permissions.max_spawn_depth, 2);
        assert_eq!(permissions.source, "worker_runtime_profile");
    }

    #[test]
    fn fleet_worker_spec_defaults_to_shared_subagent_depth() {
        let task = FleetTaskSpec {
            id: "task-1".to_string(),
            name: "Task".to_string(),
            description: None,
            objective: None,
            instructions: "Do the task.".to_string(),
            worker: Some(FleetTaskWorkerProfile {
                agent_profile: None,
                role: Some("reviewer".to_string()),
                loadout: None,
                model_class: None,
                model: None,
                tool_profile: Some("read-only".to_string()),
                tools: Vec::new(),
                capabilities: Vec::new(),
            }),
            workspace: None,
            input_files: vec![],
            context: vec![],
            budget: None,
            tags: vec![],
            expected_artifacts: vec![],
            scorer: None,
            retry_policy: None,
            alert_policy: None,
            timeout_seconds: None,
            metadata: Default::default(),
        };
        let worker = FleetWorkerSpec {
            environment: None,
            id: "worker-1".to_string(),
            name: "Worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };

        let spec = fleet_task_to_worker_spec_with_profiles(
            "worker-1",
            "run-1",
            &task,
            &worker,
            "auto",
            std::path::Path::new("/tmp"),
            std::path::Path::new("/tmp"),
            &[],
            None,
        )
        .expect("worker spec with empty profiles");

        assert_eq!(spec.spawn_depth, 0);
        assert_eq!(
            spec.max_spawn_depth,
            codewhale_protocol::fleet::DEFAULT_SPAWN_DEPTH
        );
        assert_eq!(spec.max_spawn_depth, u32::MAX);

        let hardened = apply_exec_hardening(spec, &codewhale_config::FleetExecConfig::default());
        let would_exceed = |spawn_depth: u32| spawn_depth + 1 > hardened.max_spawn_depth;
        assert!(
            !would_exceed(0),
            "root (depth 0) must spawn a child at depth 1"
        );
        assert!(!would_exceed(1), "depth-1 child must spawn to depth 2");
        assert!(!would_exceed(2), "depth-2 child must spawn to depth 3");
        assert!(
            !would_exceed(8),
            "default delegation continues beyond eight levels"
        );
    }

    #[test]
    fn fleet_fanout_role_loadouts_keep_distinct_child_models() {
        let worker = FleetWorkerSpec {
            environment: None,
            id: "local-worker".to_string(),
            name: "Local worker".to_string(),
            host: FleetHostSpec::Local,
            trust_level: None,
            labels: Default::default(),
            capabilities: vec![],
            max_concurrent_tasks: None,
        };

        let cases = [
            (
                "scout",
                "deepseek-v4-flash",
                FleetRole::Scout,
                AgentWorkerToolProfile::Explicit(vec![
                    "read_file".to_string(),
                    "grep_files".to_string(),
                ]),
            ),
            (
                "builder",
                "deepseek-v4-pro",
                FleetRole::Builder,
                AgentWorkerToolProfile::Explicit(vec![
                    "read_file".to_string(),
                    "apply_patch".to_string(),
                ]),
            ),
            (
                "verifier",
                "deepseek-v4-pro",
                FleetRole::Verifier,
                AgentWorkerToolProfile::Explicit(vec![
                    "exec_shell".to_string(),
                    "read_file".to_string(),
                ]),
            ),
        ];

        let parent_model = "parent-session-model";
        let mut child_models = std::collections::BTreeSet::new();
        for (role, model, expected_type, expected_tools) in cases {
            let task = FleetTaskSpec {
                id: format!("{role}-task"),
                name: format!("{role} task"),
                description: None,
                objective: Some(format!("{role} objective")),
                instructions: "Complete the assigned fanout lane.".to_string(),
                worker: Some(FleetTaskWorkerProfile {
                    agent_profile: None,
                    role: Some(role.to_string()),
                    loadout: None,
                    model_class: None,
                    model: None,
                    tool_profile: None,
                    tools: match &expected_tools {
                        AgentWorkerToolProfile::Explicit(tools) => tools.clone(),
                        AgentWorkerToolProfile::Inherited => Vec::new(),
                    },
                    capabilities: vec![],
                }),
                workspace: matches!(&expected_type, FleetRole::Builder).then(|| {
                    FleetWorkspaceRequirements {
                        placement: None,
                        root: Some(PathBuf::from(".")),
                        required_files: Vec::new(),
                        writable_paths: vec![PathBuf::from(".")],
                        environment: None,
                    }
                }),
                input_files: vec![],
                context: vec![],
                budget: None,
                tags: vec![],
                expected_artifacts: vec![],
                scorer: None,
                retry_policy: None,
                alert_policy: None,
                timeout_seconds: None,
                metadata: Default::default(),
            };

            let spec = fleet_task_to_worker_spec_with_profiles(
                &format!("{role}-worker"),
                "run-3289",
                &task,
                &worker,
                model,
                std::path::Path::new("/tmp"),
                std::path::Path::new("/tmp"),
                &[],
                None,
            )
            .expect("worker spec with empty profiles");

            let public_role = codewhale_agent_contract::role::public_role_label(role);
            assert_eq!(spec.role.as_deref(), Some(public_role.as_str()));
            assert_eq!(spec.agent_type, expected_type, "role {role}");
            assert_eq!(spec.tool_profile, expected_tools, "role {role}");
            assert_eq!(spec.model, model, "role {role}");
            assert_ne!(
                spec.model, parent_model,
                "Fleet fanout child {role} must use its resolved loadout, not blindly inherit"
            );
            assert_eq!(
                spec.runtime_profile.model,
                ModelRoute::Inherit,
                "role {role}"
            );
            assert_eq!(spec.runtime_profile.role, expected_type, "role {role}");
            child_models.insert(spec.model.clone());
        }
        assert_eq!(
            child_models,
            std::collections::BTreeSet::from([
                "deepseek-v4-flash".to_string(),
                "deepseek-v4-pro".to_string(),
            ]),
            "Fleet fanout should preserve a mixed scout/builder/verifier loadout"
        );
    }

    #[test]
    fn fleet_route_parity_uses_shared_router_candidates() {
        use crate::model_routing::{RouterCandidates, provider_router_candidates};
        use codewhale_config::ProviderKind;

        // Fleet emits the SAME `ModelRoute` seam the sub-agent assignment path
        // consumes. `Fast` no longer re-prices the child onto a cheaper
        // sibling: a loadout says how much work a role should do, not which
        // model it is billed as, so it inherits like every other default.
        assert_eq!(
            fleet_model_route_for_loadout("auto", &codewhale_config::FleetLoadout::Fast),
            ModelRoute::Inherit,
        );
        assert_eq!(
            fleet_model_route_for_loadout("auto", &codewhale_config::FleetLoadout::Inherit),
            ModelRoute::Inherit,
        );
        assert_eq!(
            fleet_model_route_for_loadout(
                "auto",
                &codewhale_config::FleetLoadout::Custom("strong".to_string())
            ),
            ModelRoute::Auto,
        );
        // An explicit model always pins to a Fixed route, regardless of loadout.
        assert_eq!(
            fleet_model_route_for_loadout(
                "deepseek-v4-flash",
                &codewhale_config::FleetLoadout::Custom("strong".to_string())
            ),
            ModelRoute::Fixed("deepseek-v4-flash".to_string()),
        );

        // The sub-agent runtime resolves a `ModelRoute` to a concrete model via
        // `provider_router_candidates` (see `worker_profile_subagent_assignment_route`):
        //   Fixed(m)      -> m
        //   Faster | Auto -> candidates.cheap (else parent)
        //   Inherit       -> parent
        // A fleet worker hands its `ModelRoute` to that same resolution. A
        // fleet "fast" loadout no longer lands on the provider's cheap
        // sibling — it inherits the parent route, so the child is billed as
        // the model the operator actually chose. `Auto` still resolves to the
        // cheap sibling, which is the remaining way to opt into one.
        let parent = "deepseek-v4-pro";
        let resolve = |route: &ModelRoute, candidates: &RouterCandidates| match route {
            ModelRoute::Fixed(model) => model.clone(),
            ModelRoute::Faster | ModelRoute::Auto => candidates
                .cheap
                .clone()
                .unwrap_or_else(|| parent.to_string()),
            ModelRoute::Inherit => parent.to_string(),
        };

        let deepseek = provider_router_candidates(ProviderKind::Deepseek, parent);
        assert_eq!(
            resolve(
                &fleet_model_route_for_loadout("auto", &codewhale_config::FleetLoadout::Fast),
                &deepseek,
            ),
            parent,
            "fleet fast loadout resolves to the provider cheap sibling via the shared router",
        );

        // A provider with no known fast sibling must keep children on the parent
        // model rather than fabricating a cloud id (#3166 route assertion).
        let no_sibling = provider_router_candidates(ProviderKind::Anthropic, parent);
        assert_eq!(no_sibling.cheap, None);
        assert_eq!(
            resolve(
                &fleet_model_route_for_loadout("auto", &codewhale_config::FleetLoadout::Fast),
                &no_sibling,
            ),
            parent,
            "fast with no provider sibling stays on the parent/default model",
        );
    }

    #[test]
    fn exec_hardening_caps_max_steps_to_max_turns() {
        let spec = AgentWorkerSpec {
            worker_id: "w1".to_string(),
            run_id: "r1".to_string(),
            parent_run_id: None,
            workflow_run_id: None,
            session_name: None,
            objective: "test".to_string(),
            role: None,
            agent_type: FleetRole::Worker,
            model: "auto".to_string(),
            workspace: std::path::PathBuf::from("/tmp"),
            git_branch: None,
            context_mode: "fresh".to_string(),
            fork_context: false,
            tool_profile: AgentWorkerToolProfile::Inherited,
            runtime_profile: WorkerRuntimeProfile::for_role(FleetRole::Worker),
            max_steps: 1000,
            spawn_depth: 0,
            max_spawn_depth: 0,
            child_route: None,
            launch_manifest: None,
        };
        let exec = codewhale_config::FleetExecConfig {
            max_turns: 50,
            ..Default::default()
        };
        let hardened = apply_exec_hardening(spec, &exec);
        assert_eq!(hardened.max_steps, 50);
    }

    #[test]
    fn exec_hardening_applies_explicit_spawn_depth_without_a_product_cap() {
        let spec = AgentWorkerSpec {
            worker_id: "w1".to_string(),
            run_id: "r1".to_string(),
            parent_run_id: None,
            workflow_run_id: None,
            session_name: None,
            objective: "test".to_string(),
            role: None,
            agent_type: FleetRole::Worker,
            model: "auto".to_string(),
            workspace: std::path::PathBuf::from("/tmp"),
            git_branch: None,
            context_mode: "fresh".to_string(),
            fork_context: false,
            tool_profile: AgentWorkerToolProfile::Inherited,
            runtime_profile: WorkerRuntimeProfile {
                max_spawn_depth: codewhale_config::MAX_SPAWN_DEPTH_CEILING,
                ..WorkerRuntimeProfile::for_role(FleetRole::Worker)
            },
            max_steps: 1000,
            spawn_depth: 0,
            max_spawn_depth: codewhale_config::MAX_SPAWN_DEPTH_CEILING,
            child_route: None,
            launch_manifest: None,
        };

        let exec = codewhale_config::FleetExecConfig {
            max_spawn_depth: 2,
            ..Default::default()
        };
        let hardened = apply_exec_hardening(spec.clone(), &exec);
        assert_eq!(hardened.max_spawn_depth, 2);

        let exec = codewhale_config::FleetExecConfig {
            max_spawn_depth: 99,
            ..Default::default()
        };
        let hardened = apply_exec_hardening(spec.clone(), &exec);
        assert_eq!(hardened.max_spawn_depth, 99);

        let exec = codewhale_config::FleetExecConfig {
            max_spawn_depth: 0,
            ..Default::default()
        };
        let hardened = apply_exec_hardening(spec, &exec);
        assert_eq!(hardened.max_spawn_depth, 0);
    }

    #[test]
    fn exec_hardening_filters_disallowed_tools() {
        let profile = AgentWorkerToolProfile::Explicit(vec![
            "read_file".to_string(),
            "exec_shell".to_string(),
            "git_diff".to_string(),
        ]);
        let exec = codewhale_config::FleetExecConfig {
            disallowed_tools: vec!["exec_shell".to_string()],
            ..Default::default()
        };
        let filtered = filter_tool_profile(&profile, &exec);
        assert_eq!(
            filtered,
            AgentWorkerToolProfile::Explicit(
                vec!["read_file".to_string(), "git_diff".to_string(),]
            )
        );
    }

    #[test]
    fn exec_hardening_allowed_tools_acts_as_allowlist() {
        let profile = AgentWorkerToolProfile::Explicit(vec![
            "read_file".to_string(),
            "exec_shell".to_string(),
            "git_diff".to_string(),
        ]);
        let exec = codewhale_config::FleetExecConfig {
            allowed_tools: vec!["read_file".to_string(), "git_diff".to_string()],
            ..Default::default()
        };
        let filtered = filter_tool_profile(&profile, &exec);
        assert_eq!(
            filtered,
            AgentWorkerToolProfile::Explicit(
                vec!["read_file".to_string(), "git_diff".to_string(),]
            )
        );
    }

    #[test]
    fn exec_hardening_allowed_plus_disallowed_disallowed_wins() {
        let profile = AgentWorkerToolProfile::Explicit(vec![
            "read_file".to_string(),
            "exec_shell".to_string(),
        ]);
        let exec = codewhale_config::FleetExecConfig {
            allowed_tools: vec!["read_file".to_string(), "exec_shell".to_string()],
            disallowed_tools: vec!["exec_shell".to_string()],
            ..Default::default()
        };
        let filtered = filter_tool_profile(&profile, &exec);
        assert_eq!(
            filtered,
            AgentWorkerToolProfile::Explicit(vec!["read_file".to_string(),])
        );
    }

    #[test]
    fn exec_hardening_leaves_policy_prompt_out_of_the_objective() {
        let spec = AgentWorkerSpec {
            worker_id: "w1".to_string(),
            run_id: "r1".to_string(),
            parent_run_id: None,
            workflow_run_id: None,
            session_name: None,
            objective: "do the thing".to_string(),
            role: None,
            agent_type: FleetRole::Worker,
            model: "auto".to_string(),
            workspace: std::path::PathBuf::from("/tmp"),
            git_branch: None,
            context_mode: "fresh".to_string(),
            fork_context: false,
            tool_profile: AgentWorkerToolProfile::Inherited,
            runtime_profile: WorkerRuntimeProfile::for_role(FleetRole::Worker),
            max_steps: 100,
            spawn_depth: 0,
            max_spawn_depth: 0,
            child_route: None,
            launch_manifest: None,
        };
        let exec = codewhale_config::FleetExecConfig {
            append_system_prompt: "never push to main".to_string(),
            ..Default::default()
        };
        let hardened = apply_exec_hardening(spec, &exec);
        // The policy travels as system prompt text on the worker command
        // (`--append-system-prompt`), never duplicated into the task prompt.
        assert_eq!(hardened.objective, "do the thing");
    }
}

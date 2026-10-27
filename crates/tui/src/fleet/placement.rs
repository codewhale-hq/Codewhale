use std::collections::BTreeSet;

use anyhow::{Result, ensure};
use codewhale_protocol::fleet::{FleetHostSpec, FleetTaskSpec, FleetWorkerSpec};
use codewhale_protocol::placement::{
    EnvironmentKind, EnvironmentResources, ExecutionEnvironment, PlacementCandidate,
    PlacementDecision, PlacementRefusal, PlacementRefusalCode, PlacementRequirements,
};

pub fn environment(worker: &FleetWorkerSpec) -> ExecutionEnvironment {
    worker
        .environment
        .clone()
        .unwrap_or_else(|| ExecutionEnvironment {
            id: match &worker.host {
                FleetHostSpec::Local => "local".to_string(),
                FleetHostSpec::Ssh { host, .. } => format!("ssh:{host}"),
                FleetHostSpec::Tailscale { node_id, .. } => format!("tailscale:{node_id}"),
                FleetHostSpec::Docker { image, .. } => format!("docker:{image}"),
            },
            kind: match worker.host {
                FleetHostSpec::Local | FleetHostSpec::Docker { .. } => EnvironmentKind::Local,
                _ => EnvironmentKind::Remote,
            },
            resources: EnvironmentResources::default(),
            capabilities: worker.capabilities.clone(),
            data: Vec::new(),
            persistent: None,
            cost_per_hour_microusd: None,
        })
}

pub fn requirements(task: &FleetTaskSpec) -> PlacementRequirements {
    task.workspace
        .as_ref()
        .and_then(|workspace| workspace.placement.clone())
        .unwrap_or_default()
}

pub fn validate(task: &FleetTaskSpec) -> Result<()> {
    let requirements = requirements(task);
    validate_tokens(&requirements.environments)?;
    validate_tokens(&requirements.capabilities)?;
    validate_tokens(&requirements.data)?;
    validate_tokens(&requirements.prefer)?;
    ensure!(requirements.kinds.len() <= 3, "too many environment kinds");
    Ok(())
}

pub fn validate_worker(worker: &FleetWorkerSpec) -> Result<()> {
    let environment = environment(worker);
    validate_tokens(std::slice::from_ref(&environment.id))?;
    validate_tokens(&environment.capabilities)?;
    validate_tokens(&environment.data)?;
    Ok(())
}

fn validate_tokens(values: &[String]) -> Result<()> {
    ensure!(
        values.len() <= 64,
        "environment lists cannot exceed 64 entries"
    );
    for value in values {
        ensure!(
            !value.is_empty()
                && value.trim() == value
                && value.len() <= 256
                && value.chars().all(|c| !c.is_control()),
            "environment identifiers must contain 1 to 256 printable characters"
        );
    }
    Ok(())
}

pub fn decide(
    task: &FleetTaskSpec,
    workers: &[FleetWorkerSpec],
    unavailable: &BTreeSet<String>,
) -> PlacementDecision {
    let required = requirements(task);
    let mut candidates: Vec<_> = workers
        .iter()
        .map(|worker| {
            let environment = environment(worker);
            let mut refusals = Vec::new();
            let mut refuse = |code, detail| refusals.push(PlacementRefusal { code, detail });
            if unavailable.contains(&worker.id) {
                refuse(
                    PlacementRefusalCode::WorkerBusy,
                    "Worker is already occupied or unavailable".to_string(),
                );
            }
            if matches!(worker.host, FleetHostSpec::Docker { .. }) {
                refuse(
                    PlacementRefusalCode::UnsupportedHost,
                    "Container execution is not available".to_string(),
                );
            }
            if !required.environments.is_empty() && !required.environments.contains(&environment.id)
            {
                refuse(
                    PlacementRefusalCode::EnvironmentNotAllowed,
                    "Environment is outside the task's allowed set".to_string(),
                );
            }
            if !required.kinds.is_empty() && !required.kinds.contains(&environment.kind) {
                refuse(
                    PlacementRefusalCode::KindNotAllowed,
                    "Environment kind does not match the task".to_string(),
                );
            }
            for capability in &required.capabilities {
                if !environment.capabilities.contains(capability) {
                    refuse(PlacementRefusalCode::MissingCapability, capability.clone());
                }
            }
            for data in &required.data {
                if !environment.data.contains(data) {
                    refuse(PlacementRefusalCode::MissingData, data.clone());
                }
            }
            for (name, minimum, available) in [
                (
                    "cpu_cores",
                    required.resources.cpu_cores.map(u64::from),
                    environment.resources.cpu_cores.map(u64::from),
                ),
                (
                    "memory_mib",
                    required.resources.memory_mib,
                    environment.resources.memory_mib,
                ),
                (
                    "gpus",
                    required.resources.gpus.map(u64::from),
                    environment.resources.gpus.map(u64::from),
                ),
            ] {
                if let Some(minimum) = minimum
                    && available.is_none_or(|available| available < minimum)
                {
                    refuse(
                        PlacementRefusalCode::InsufficientResources,
                        format!(
                            "{name}: requires {minimum}, declared {}",
                            available
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "unknown".to_string())
                        ),
                    );
                }
            }
            if required
                .persistent
                .is_some_and(|persistent| Some(persistent) != environment.persistent)
            {
                refuse(
                    PlacementRefusalCode::PersistenceMismatch,
                    "Environment persistence does not match the task".to_string(),
                );
            }
            if let Some(limit) = required.max_cost_per_hour_microusd {
                match environment.cost_per_hour_microusd {
                    None => refuse(
                        PlacementRefusalCode::CostUnknown,
                        "No declared compute rate".to_string(),
                    ),
                    Some(cost) if cost > limit => refuse(
                        PlacementRefusalCode::CostLimit,
                        format!("Declared compute rate {cost} exceeds {limit} micro-USD/hour"),
                    ),
                    _ => {}
                }
            }
            PlacementCandidate {
                worker_id: worker.id.clone(),
                environment,
                eligible: refusals.is_empty(),
                refusals,
            }
        })
        .collect();
    candidates.sort_by_key(|candidate| {
        (
            !candidate.eligible,
            required
                .prefer
                .iter()
                .position(|id| id == &candidate.environment.id)
                .unwrap_or(usize::MAX),
        )
    });
    let selected = candidates.iter().find(|candidate| candidate.eligible);
    PlacementDecision {
        task_id: task.id.clone(),
        worker_id: selected.map(|candidate| candidate.worker_id.clone()),
        environment_id: selected.map(|candidate| candidate.environment.id.clone()),
        reason: match selected {
            Some(candidate) if required.prefer.contains(&candidate.environment.id) => {
                "Highest preferred available environment satisfying every declared requirement"
                    .to_string()
            }
            Some(_) => {
                "First available environment satisfying every declared requirement".to_string()
            }
            None => "No available environment satisfies every declared requirement".to_string(),
        },
        candidates,
    }
}

pub fn require_match(task: &FleetTaskSpec, worker: &FleetWorkerSpec) -> Result<PlacementDecision> {
    let decision = decide(task, std::slice::from_ref(worker), &BTreeSet::new());
    ensure!(
        decision.worker_id.is_some(),
        "Task {} cannot run on worker {}: {}",
        task.id,
        worker.id,
        serde_json::to_string(&decision.candidates[0].refusals)?
    );
    Ok(decision)
}

pub fn write_receipt(
    workspace: &std::path::Path,
    run_id: &codewhale_protocol::fleet::FleetRunId,
    attempt: u32,
    decision: &PlacementDecision,
) -> Result<codewhale_protocol::fleet::FleetArtifactRef> {
    use codewhale_protocol::fleet::FleetArtifactKind;
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "kind": "environment_placement",
        "run_id": run_id,
        "attempt": attempt,
        "evidence": "operator_declared",
        "execution_authority": "existing_runtime_policy",
        "decision": decision,
    }))?;
    let checksum = codewhale_runtime::hashing::sha256_hex(&bytes);
    super::task_spec::write_fleet_artifact_ref(
        workspace,
        run_id,
        &decision.task_id,
        decision.worker_id.as_deref().unwrap_or("unplaced"),
        FleetArtifactKind::Receipt,
        &format!("placement-{attempt}-{checksum}.json"),
        &bytes,
        Some("application/json"),
    )
}

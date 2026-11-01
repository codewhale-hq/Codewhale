//! Shared Fleet control-plane surface (#1888, #4022).
//!
//! `codewhale fleet …` and the `/fleet …` slash command (and therefore its
//! hotbar action) run the *same* verbs against the *same* durable ledger and
//! render the *same* [`ControlReceipt`]. Nothing here formats twice: the CLI's
//! `print_status` / `print_inspection` delegate to the renderers below.
//!
//! Vocabulary: Fleet = who, Workflow = order, Lane = one running Workflow,
//! Runtime = where/how. Auto-Review is a permission posture and never appears
//! here as a role.

use std::path::{Path, PathBuf};

use codewhale_lane::control::{
    Availability, ControlContext, ControlDomain, ControlFailure, ControlFailureKind,
    ControlOperation, ControlReceipt, ControlSurface, DEFAULT_RUN_LIST_LIMIT, Known, RunListPage,
    RunRouteDto, RunSummaryDto, RunUsageDto, UnknownReason, parse_target, redact_path,
    sanitize_line,
};
use codewhale_protocol::fleet::{
    FleetArtifactKind, FleetReceipt, FleetRun, FleetRunId, FleetRunStatus, FleetWorkerEventPayload,
    FleetWorkerStatus,
};

use super::ledger::FleetLedgerState;
use super::manager::{FleetControlError, FleetManager, FleetStatusSnapshot, FleetWorkerInspection};

/// Maximum worker rows rendered in one durable status payload.
pub const MAX_STATUS_WORKER_ROWS: usize = 24;
/// Maximum artifact rows rendered in one inspection payload.
pub const MAX_INSPECTION_ARTIFACT_ROWS: usize = 24;

/// The durable Fleet ledger for `workspace`, without creating it.
#[must_use]
pub fn fleet_ledger_path(workspace: &Path) -> PathBuf {
    workspace.join(".codewhale").join("fleet.jsonl")
}

/// Read-only availability probe for the Fleet domain.
///
/// [`FleetManager::open`] creates the ledger as a side effect, so a status
/// surface must probe first; otherwise "this workspace has no Fleet ledger"
/// silently becomes "here is an empty Fleet ledger I just made".
#[must_use]
pub fn fleet_control_context(workspace: &Path) -> ControlContext {
    ControlContext::probe(None, Some(&fleet_ledger_path(workspace)))
}

// ---------------------------------------------------------------------------
// Labels and renderers (single source for CLI and TUI)
// ---------------------------------------------------------------------------

#[must_use]
pub fn worker_status_label(status: &FleetWorkerStatus) -> &'static str {
    match status {
        FleetWorkerStatus::Unknown => "unknown",
        FleetWorkerStatus::Online => "online",
        FleetWorkerStatus::Busy => "busy",
        FleetWorkerStatus::Offline => "offline",
        FleetWorkerStatus::Unhealthy => "unhealthy",
        FleetWorkerStatus::Draining => "draining",
        FleetWorkerStatus::Retired => "retired",
    }
}

#[must_use]
pub fn run_status_label(status: &FleetRunStatus) -> &'static str {
    match status {
        FleetRunStatus::Pending => "pending",
        FleetRunStatus::Queued => "queued",
        FleetRunStatus::Running => "running",
        FleetRunStatus::Paused => "paused",
        FleetRunStatus::Completed => "completed",
        FleetRunStatus::Failed => "failed",
        FleetRunStatus::Cancelled => "cancelled",
    }
}

#[must_use]
pub fn artifact_kind_label(kind: &FleetArtifactKind) -> String {
    match kind {
        FleetArtifactKind::Log => "log".to_string(),
        FleetArtifactKind::Patch => "patch".to_string(),
        FleetArtifactKind::TestResult => "test_result".to_string(),
        FleetArtifactKind::Report => "report".to_string(),
        FleetArtifactKind::Checkpoint => "checkpoint".to_string(),
        FleetArtifactKind::Receipt => "receipt".to_string(),
        FleetArtifactKind::Other(value) => value.clone(),
    }
}

#[must_use]
pub fn event_label(payload: &FleetWorkerEventPayload) -> String {
    match payload {
        FleetWorkerEventPayload::Queued => "queued".to_string(),
        FleetWorkerEventPayload::Leased { .. } => "leased".to_string(),
        FleetWorkerEventPayload::Starting => "starting".to_string(),
        FleetWorkerEventPayload::Running => "running".to_string(),
        FleetWorkerEventPayload::ModelWait { model } => model
            .as_ref()
            .map(|model| format!("model_wait model={model}"))
            .unwrap_or_else(|| "model_wait".to_string()),
        FleetWorkerEventPayload::RunningTool { tool, call_id } => call_id
            .as_ref()
            .map(|call_id| format!("running_tool tool={tool} call_id={call_id}"))
            .unwrap_or_else(|| format!("running_tool tool={tool}")),
        FleetWorkerEventPayload::WorkflowEvent {
            workflow_run_id,
            event,
        } => event
            .get("type")
            .and_then(serde_json::Value::as_str)
            .map(|kind| format!("workflow_event run_id={workflow_run_id} type={kind}"))
            .unwrap_or_else(|| format!("workflow_event run_id={workflow_run_id}")),
        FleetWorkerEventPayload::Heartbeat { .. } => "heartbeat".to_string(),
        FleetWorkerEventPayload::UsageReport {
            input_tokens,
            output_tokens,
        } => format!("usage_report input={input_tokens} output={output_tokens}"),
        FleetWorkerEventPayload::Artifact(artifact) => {
            format!("artifact kind={}", artifact_kind_label(&artifact.kind))
        }
        FleetWorkerEventPayload::Completed { exit_code, summary } => match (exit_code, summary) {
            (Some(code), Some(summary)) => format!("completed exit_code={code} {summary}"),
            (Some(code), None) => format!("completed exit_code={code}"),
            (None, Some(summary)) => format!("completed {summary}"),
            (None, None) => "completed".to_string(),
        },
        FleetWorkerEventPayload::Failed {
            reason,
            recoverable,
        } => format!("failed recoverable={recoverable} reason={reason}"),
        FleetWorkerEventPayload::Cancelled { cancelled_by } => cancelled_by
            .as_ref()
            .map(|by| format!("cancelled by={by}"))
            .unwrap_or_else(|| "cancelled".to_string()),
        FleetWorkerEventPayload::Interrupted { signal } => signal
            .as_ref()
            .map(|signal| format!("interrupted signal={signal}"))
            .unwrap_or_else(|| "interrupted".to_string()),
        FleetWorkerEventPayload::Stale { last_heartbeat_at } => last_heartbeat_at
            .as_ref()
            .map(|ts| format!("stale last_heartbeat_at={ts}"))
            .unwrap_or_else(|| "stale".to_string()),
        FleetWorkerEventPayload::Restarted { restart_count } => {
            format!("restarted count={restart_count}")
        }
        FleetWorkerEventPayload::Escalated { channel, alert_id } => alert_id
            .as_ref()
            .map(|alert_id| format!("escalated channel={channel} alert_id={alert_id}"))
            .unwrap_or_else(|| format!("escalated channel={channel}")),
    }
}

/// Durable status snapshot as bounded Fleet receipt lines.
///
/// The command and slash surfaces call the customer-facing concept a Fleet, but
/// these strings are nested in the shared [`ControlReceipt`] detail contract.
/// Keep the established `fleet:` prefix so existing receipt consumers and
/// scripts do not need to parse a presentation rename.
#[must_use]
pub fn status_lines(status: &FleetStatusSnapshot) -> Vec<String> {
    let mut lines = vec![format!(
        "fleet: runs={} queued={} running={} completed={} partial={} failed={} restarted={} \
         escalated={} transport_failed={} task_failed={} verifier_failed={} cancelled={} stale={}",
        status.runs,
        status.queued,
        status.running,
        status.completed,
        status.partial,
        status.failed,
        status.restarted,
        status.escalated,
        status.transport_failed,
        status.task_failed,
        status.verifier_failed,
        status.cancelled,
        status.stale
    )];
    if !status.workers.is_empty() {
        lines.push("workers:".to_string());
        for (worker_id, worker_status) in status.workers.iter().take(MAX_STATUS_WORKER_ROWS) {
            lines.push(format!(
                "  {worker_id} {}",
                worker_status_label(worker_status)
            ));
        }
        let omitted = status.workers.len().saturating_sub(MAX_STATUS_WORKER_ROWS);
        if omitted > 0 {
            lines.push(format!(
                "  [{omitted} more worker(s) omitted by the {MAX_STATUS_WORKER_ROWS}-row bound]"
            ));
        }
    }
    lines
}

/// Compatibility renderer shared by `codewhale fleet status` and `/fleet status`.
///
/// The invocation names are public Fleet wording; the returned detail stays in
/// the durable Fleet receipt spelling by way of [`status_lines`].
#[must_use]
pub fn render_fleet_status_snapshot(status: &FleetStatusSnapshot) -> String {
    status_lines(status).join("\n")
}

/// Durable worker inspection as bounded lines.
#[must_use]
pub fn inspection_lines(inspection: &FleetWorkerInspection) -> Vec<String> {
    let mut lines = vec![
        format!("worker: {}", inspection.worker_id),
        format!("status: {}", worker_status_label(&inspection.status)),
    ];
    if let Some(run_id) = &inspection.current_run_id {
        lines.push(format!("run: {}", run_id.0));
    }
    if let Some(task_id) = &inspection.current_task_id {
        lines.push(format!("task: {task_id}"));
    }
    if let Some(objective) = &inspection.objective {
        lines.push(format!("objective: {objective}"));
    }
    if let Some(role) = &inspection.role {
        lines.push(format!("role: {role}"));
    }
    if let Some(host) = &inspection.host {
        lines.push(format!("host: {host}"));
    }
    if let Some(heartbeat) = &inspection.latest_heartbeat_at {
        lines.push(format!("heartbeat: {heartbeat}"));
    }
    if let Some(event) = &inspection.latest_event {
        lines.push(format!(
            "latest_event: seq={} {}",
            event.seq,
            event_label(&event.payload)
        ));
    }
    if !inspection.artifacts.is_empty() {
        lines.push("artifacts:".to_string());
        for artifact in inspection
            .artifacts
            .iter()
            .take(MAX_INSPECTION_ARTIFACT_ROWS)
        {
            lines.push(format!(
                "  {} {}",
                artifact_kind_label(&artifact.kind),
                artifact.path.display()
            ));
        }
        let omitted = inspection
            .artifacts
            .len()
            .saturating_sub(MAX_INSPECTION_ARTIFACT_ROWS);
        if omitted > 0 {
            lines.push(format!("  [{omitted} more artifact(s) omitted]"));
        }
    }
    if let Some(receipt) = &inspection.receipt_summary {
        lines.push(format!("receipt: {receipt}"));
    }
    if let Some(error) = &inspection.last_error {
        lines.push(format!("last_error: {error}"));
    }
    if let Some(alert) = &inspection.alert_state {
        lines.push(format!("alert: {alert}"));
    }
    lines
}

#[must_use]
pub fn render_inspection(inspection: &FleetWorkerInspection) -> String {
    inspection_lines(inspection).join("\n")
}

/// Artifact listing lines for `codewhale fleet artifacts`.
#[must_use]
pub fn artifact_lines(inspection: &FleetWorkerInspection) -> Vec<String> {
    if inspection.artifacts.is_empty() {
        return vec!["artifacts: none".to_string()];
    }
    let mut lines = vec!["artifacts:".to_string()];
    for artifact in inspection
        .artifacts
        .iter()
        .take(MAX_INSPECTION_ARTIFACT_ROWS)
    {
        let size = artifact
            .size_bytes
            .map(|size| format!(" size={size}"))
            .unwrap_or_default();
        let mime = artifact
            .mime_type
            .as_ref()
            .map(|mime| format!(" mime={mime}"))
            .unwrap_or_default();
        lines.push(format!(
            "  {} {}{}{}",
            artifact_kind_label(&artifact.kind),
            artifact.path.display(),
            size,
            mime
        ));
    }
    let omitted = inspection
        .artifacts
        .len()
        .saturating_sub(MAX_INSPECTION_ARTIFACT_ROWS);
    if omitted > 0 {
        lines.push(format!("  [{omitted} more artifact(s) omitted]"));
    }
    lines
}

#[must_use]
pub fn render_artifacts(inspection: &FleetWorkerInspection) -> String {
    artifact_lines(inspection).join("\n")
}

// ---------------------------------------------------------------------------
// Fleet run DTOs
// ---------------------------------------------------------------------------

fn label(run: &FleetRun, key: &str) -> Option<String> {
    run.labels.get(key).cloned()
}

/// Project the most recent receipt's resolved route onto the shared DTO.
///
/// Everything the receipt records exactly becomes `Known`; everything it does
/// not record stays typed-unknown. In particular the Fleet receipt persists
/// the *effective* reasoning tier only, so `requested_reasoning` is
/// `not_recorded` rather than being back-filled from the effective value.
fn route_dto(receipt: Option<&FleetReceipt>) -> RunRouteDto {
    let Some(route) = receipt.and_then(|receipt| receipt.resolved_route.as_ref()) else {
        return RunRouteDto::all_unknown(UnknownReason::NotRecorded);
    };
    RunRouteDto {
        provider_id: Known::Known(route.provider_id.clone()),
        provider_exact_id: Known::from_option(route.provider_exact_id.clone()),
        model: Known::Known(
            route
                .canonical_model
                .clone()
                .unwrap_or_else(|| route.wire_model_id.clone()),
        ),
        requested_reasoning: Known::unknown(),
        effective_reasoning: Known::from_option(route.reasoning_effort.clone()),
        route_source: Known::Known(route.source.clone()),
    }
}

/// Project one durable Fleet run into the shared run DTO.
#[must_use]
pub fn fleet_run_summary(run: &FleetRun, receipt: Option<&FleetReceipt>) -> RunSummaryDto {
    RunSummaryDto {
        domain: ControlDomain::Fleet,
        run_id: run.id.0.clone(),
        status: run_status_label(&run.status).to_string(),
        // The Fleet ledger fences lifecycle per task, not per run.
        lifecycle_seq: Known::not_applicable(),
        runtime: Known::from_option(label(run, "runtime")),
        workflow: Known::from_option(label(run, "workflow")),
        fleet: Known::from_option(
            label(run, "fleet").or_else(|| (!run.name.trim().is_empty()).then(|| run.name.clone())),
        ),
        issue: Known::from_option(label(run, "issue")),
        goal: Known::from_option(label(run, "goal").or_else(|| label(run, "objective"))),
        started_at: Known::Known(run.created_at.clone()),
        stopped_at: Known::from_option(run.completed_at.clone()),
        // Fleet runs are workspace-scoped; there is no per-run worktree, and
        // the Lane-shaped Runtime handles do not apply to a Fleet run.
        location: Known::not_applicable(),
        branch: Known::not_applicable(),
        runtime_session: Known::not_applicable(),
        runtime_socket: Known::not_applicable(),
        attach: Known::not_applicable(),
        log: Known::not_applicable(),
        route: route_dto(receipt),
        // The ledger does not persist token counts or wall-clock duration.
        usage: RunUsageDto::all_unknown(UnknownReason::NotRecorded),
    }
}

/// Bounded page of durable Fleet runs, newest first.
#[must_use]
pub fn fleet_run_page(state: &FleetLedgerState, limit: usize) -> RunListPage {
    let mut summaries: Vec<RunSummaryDto> = state
        .runs
        .values()
        .map(|run| {
            let status = state
                .run_status_overrides
                .get(&run.id.0)
                .cloned()
                .unwrap_or_else(|| run.status.clone());
            let receipt = state
                .receipts
                .values()
                .filter(|receipt| receipt.run_id.0 == run.id.0)
                .max_by(|a, b| a.completed_at.cmp(&b.completed_at));
            let mut summary = fleet_run_summary(run, receipt);
            summary.status = run_status_label(&status).to_string();
            summary
        })
        .collect();
    // Newest first, matching `lane list`. Sort on the parsed UTC instant, not
    // the rendered text: two timestamps written at different offsets order
    // wrongly under a string compare. Unparseable timestamps sort last rather
    // than being silently interleaved, and the exact id breaks every tie so
    // the order is total and stable.
    summaries.sort_by(|a, b| {
        instant_of(&a.started_at)
            .cmp(&instant_of(&b.started_at))
            .reverse()
            .then_with(|| a.run_id.cmp(&b.run_id))
    });
    RunListPage::bounded(summaries, limit)
}

/// Parse a recorded timestamp into a comparable UTC instant.
///
/// `None` for unknown or unparseable values, which `Option`'s ordering places
/// before every real instant — and therefore last under the reversed
/// newest-first sort.
fn instant_of(value: &Known<String>) -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = value.as_known()?;
    chrono::DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|parsed| parsed.with_timezone(&chrono::Utc))
}

// ---------------------------------------------------------------------------
// Executor — the one code path behind `codewhale fleet …` and `/fleet …`
// ---------------------------------------------------------------------------

/// Run a Fleet control verb against the durable workspace ledger, using a
/// default manager.
///
/// The slash command and hotbar use this. The CLI uses
/// [`execute_fleet_control_with`] so its configured manager (exec config,
/// stale-after window, session model, route config) still applies — same code
/// path, same receipt, caller-owned policy.
#[must_use]
pub fn execute_fleet_control(
    surface: ControlSurface,
    workspace: &Path,
    operation: ControlOperation,
    raw_target: Option<&str>,
) -> ControlReceipt {
    let descriptor = operation.descriptor();
    let availability = descriptor.availability(surface, fleet_control_context(workspace));
    if !availability.is_available() {
        return ControlReceipt::unavailable(descriptor, surface, availability);
    }
    match FleetManager::open(workspace) {
        Ok(manager) => execute_fleet_control_with(
            surface,
            workspace,
            fleet_control_context(workspace),
            &manager,
            operation,
            raw_target,
        ),
        Err(err) => ControlReceipt::failed(
            descriptor,
            surface,
            None,
            ControlFailure::backend(format!("{err:#}")),
        ),
    }
}

/// Run a Fleet control verb against a caller-configured [`FleetManager`].
///
/// The CLI and the slash command both land here, so availability, target
/// selection, lifecycle outcome, retryability, and the sanitized failure are
/// decided once. `fleet.restart` is declared `SurfaceLimited` to the CLI
/// because it drives the manager loop to completion; this function reports
/// that as a typed unavailability on other surfaces rather than quietly doing
/// a different, smaller thing.
#[must_use]
pub fn execute_fleet_control_with(
    surface: ControlSurface,
    workspace: &Path,
    ctx: ControlContext,
    manager: &FleetManager,
    operation: ControlOperation,
    raw_target: Option<&str>,
) -> ControlReceipt {
    let descriptor = operation.descriptor();
    if descriptor.domain != ControlDomain::Fleet {
        return ControlReceipt::rejected(
            descriptor,
            surface,
            None,
            ControlFailure::new(
                ControlFailureKind::InvalidTarget,
                format!("{} is not a Fleet verb", descriptor.id),
            ),
        );
    }

    let availability = descriptor.availability(surface, ctx);
    if !availability.is_available() {
        return ControlReceipt::unavailable(descriptor, surface, availability);
    }

    let target = match parse_target(descriptor, raw_target) {
        Ok(target) => target,
        Err(failure) => return ControlReceipt::rejected(descriptor, surface, None, failure),
    };

    match operation {
        ControlOperation::FleetList => match manager.rebuild_state() {
            Ok(state) => ControlReceipt::inspected(descriptor, surface, None)
                .with_runs(fleet_run_page(&state, DEFAULT_RUN_LIST_LIMIT))
                .with_detail([format!(
                    "ledger: {}",
                    redact_path(&fleet_ledger_path(workspace))
                )]),
            Err(err) => ControlReceipt::failed(
                descriptor,
                surface,
                None,
                ControlFailure::backend(format!("{err:#}")),
            ),
        },
        ControlOperation::FleetStatus => match manager.status() {
            Ok(status) => ControlReceipt::inspected(descriptor, surface, None).with_detail(
                status_lines(&status).into_iter().chain([format!(
                    "ledger: {}",
                    redact_path(&fleet_ledger_path(workspace))
                )]),
            ),
            Err(err) => ControlReceipt::failed(
                descriptor,
                surface,
                None,
                ControlFailure::backend(format!("{err:#}")),
            ),
        },
        ControlOperation::FleetInterrupt => {
            let Some(target) = target else {
                return ControlReceipt::rejected(
                    descriptor,
                    surface,
                    None,
                    ControlFailure::invalid_target(format!(
                        "{} needs an exact worker id",
                        descriptor.id
                    )),
                );
            };
            // Exact identity against the real ledger before any mutation: a
            // worker id that was never seen in this workspace is `not_found`,
            // which is a different fact from "known worker, nothing leased"
            // (a conflict). Checking here keeps a typo from reaching the
            // mutation path at all.
            match manager.rebuild_state() {
                Ok(state) => {
                    if !state.workers.contains_key(&target.id) {
                        return ControlReceipt::rejected(
                            descriptor,
                            surface,
                            Some(target.clone()),
                            ControlFailure::not_found(format!(
                                "no Fleet worker with id {} in this workspace's ledger",
                                target.id
                            )),
                        );
                    }
                }
                Err(err) => {
                    return ControlReceipt::failed(
                        descriptor,
                        surface,
                        Some(target),
                        ControlFailure::backend(format!("{err:#}")),
                    );
                }
            }
            // Bind before matching so the borrow of `target.id` is over
            // before the arms move `target` into the receipt.
            let interrupted = manager.interrupt_worker(&target.id);
            match interrupted {
                Ok(inspection) => ControlReceipt::transitioned(descriptor, surface, Some(target))
                    .with_detail(inspection_lines(&inspection)),
                Err(err) => {
                    // The manager refuses when the exact worker has no active
                    // task. That is a state conflict, not a transient backend
                    // fault: retrying it will keep failing until work is
                    // leased again. Classified by type, not by message text.
                    let message = format!("{err:#}");
                    let failure = match err.downcast_ref::<FleetControlError>() {
                        Some(FleetControlError::NoActiveTask { .. }) => {
                            ControlFailure::conflict(message)
                        }
                        Some(FleetControlError::UnknownRun { .. }) => {
                            ControlFailure::not_found(message)
                        }
                        None => ControlFailure::backend(message),
                    };
                    ControlReceipt::failed(descriptor, surface, Some(target), failure)
                }
            }
        }
        ControlOperation::FleetResume => {
            let Some(target) = target else {
                return ControlReceipt::rejected(
                    descriptor,
                    surface,
                    None,
                    ControlFailure::invalid_target(format!(
                        "{} needs an exact run id",
                        descriptor.id
                    )),
                );
            };
            // Exact identity first. `resume_run` reconciles by run id and, for
            // an id that is not in the ledger, would still write a run-status
            // record keyed by whatever string the caller typed — durable
            // pollution from a typo, reported as a benign no-op. Refuse before
            // any write happens (#4022).
            match manager.rebuild_state() {
                Ok(state) => {
                    if !state.runs.contains_key(&target.id) {
                        return ControlReceipt::rejected(
                            descriptor,
                            surface,
                            Some(target.clone()),
                            ControlFailure::not_found(
                                FleetControlError::UnknownRun {
                                    run_id: target.id.clone(),
                                }
                                .to_string(),
                            ),
                        );
                    }
                }
                Err(err) => {
                    return ControlReceipt::failed(
                        descriptor,
                        surface,
                        Some(target),
                        ControlFailure::backend(format!("{err:#}")),
                    );
                }
            }
            let resumed = manager.resume_run(&FleetRunId::from(target.id.clone()));
            match resumed {
                Ok(report) => {
                    let reconciled = report.reclaimed_stale
                        + report.restarted
                        + report.failed
                        + report.escalated;
                    let detail = [format!(
                        "fleet resume: {} reclaimed_stale={} restarted={} failed={} escalated={}",
                        report.run_id.0,
                        report.reclaimed_stale,
                        report.restarted,
                        report.failed,
                        report.escalated
                    )]
                    .into_iter()
                    .chain(status_lines(&report.status));
                    if reconciled == 0 {
                        ControlReceipt::no_change(descriptor, surface, Some(target))
                            .with_detail(detail)
                    } else {
                        ControlReceipt::transitioned(descriptor, surface, Some(target))
                            .with_detail(detail)
                    }
                }
                Err(err) => ControlReceipt::failed(
                    descriptor,
                    surface,
                    Some(target),
                    ControlFailure::backend(format!("{err:#}")),
                ),
            }
        }
        // `fleet.restart` is CLI-only (it drives the manager loop). The
        // availability gate above already rejected it elsewhere; this arm
        // keeps the refusal explicit if the table ever changes.
        _ => ControlReceipt::unavailable(
            descriptor,
            surface,
            Availability::Unavailable {
                reason: codewhale_lane::UnavailableReason::SurfaceNotSupported,
                hint: sanitize_line(descriptor.cli_invocation),
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::ledger::FleetLedger;
    use codewhale_lane::{ControlAuthority, LifecycleOutcome, PersistenceScope, UnavailableReason};
    use codewhale_protocol::fleet::{FleetResolvedRoute, FleetTaskResult};
    use std::collections::BTreeMap;

    fn run(id: &str) -> FleetRun {
        FleetRun {
            id: FleetRunId::from(id.to_string()),
            name: "stopship".to_string(),
            status: FleetRunStatus::Running,
            target: None,
            workflow: None,
            roles: Vec::new(),
            max_workers: Some(2),
            usage_ceiling: None,
            task_specs: Vec::new(),
            worker_specs: Vec::new(),
            labels: BTreeMap::new(),
            security_policy: None,
            created_at: "2026-07-26T00:00:00Z".to_string(),
            updated_at: None,
            completed_at: None,
        }
    }

    fn receipt_with_route(run_id: &str) -> FleetReceipt {
        FleetReceipt {
            run_id: FleetRunId::from(run_id.to_string()),
            task_id: "task-1".to_string(),
            worker_id: "worker-1".to_string(),
            attempt: Some(1),
            terminal_seq: Some(9),
            completed_at: "2026-07-26T00:01:00Z".to_string(),
            result: FleetTaskResult::Pass,
            failure_kind: None,
            artifacts: Vec::new(),
            score: None,
            resolved_route: Some(FleetResolvedRoute {
                provider_id: "deepseek".to_string(),
                provider_exact_id: Some("custom".to_string()),
                provider_kind: "deepseek".to_string(),
                canonical_model: Some("deepseek-v3".to_string()),
                wire_model_id: "deepseek-chat".to_string(),
                protocol: "chat_completions".to_string(),
                role: Some("implementer".to_string()),
                loadout: None,
                model_class: None,
                model_route: None,
                reasoning_effort: Some("high".to_string()),
                role_source: None,
                loadout_source: None,
                model_class_source: None,
                model_source: None,
                source: "resolver".to_string(),
            }),
            saved_session_id: None,
            effective_permissions: None,
        }
    }

    #[test]
    fn fleet_run_dto_uses_exact_route_and_types_what_the_ledger_omits() {
        let run = run("run-1");
        let receipt = receipt_with_route("run-1");
        let summary = fleet_run_summary(&run, Some(&receipt));

        assert_eq!(summary.domain, ControlDomain::Fleet);
        assert_eq!(summary.run_id, "run-1");
        assert_eq!(summary.status, "running");
        assert_eq!(
            summary.route.provider_id,
            Known::Known("deepseek".to_string())
        );
        assert_eq!(
            summary.route.provider_exact_id,
            Known::Known("custom".to_string()),
            "the exact provider-table id must not collapse into the generic id"
        );
        assert_eq!(summary.route.model, Known::Known("deepseek-v3".to_string()));
        assert_eq!(
            summary.route.effective_reasoning,
            Known::Known("high".to_string())
        );
        assert_eq!(
            summary.route.requested_reasoning.unknown_reason(),
            Some(UnknownReason::NotRecorded),
            "the ledger records the effective tier only; do not invent the request"
        );
        assert_eq!(summary.route.reasoning_downgraded(), None);
        assert_eq!(
            summary.route.route_source,
            Known::Known("resolver".to_string())
        );
        assert_eq!(
            summary.usage.total_tokens.unknown_reason(),
            Some(UnknownReason::NotRecorded)
        );
        assert_eq!(
            summary.lifecycle_seq.unknown_reason(),
            Some(UnknownReason::NotApplicable)
        );
        let detail = summary.render_detail();
        assert!(detail.starts_with("fleet:      run-1\n"), "{detail}");
        assert!(detail.contains("\nfleet:     stopship\n"), "{detail}");
        assert!(!detail.contains("\npod:"), "{detail}");
        let wire = serde_json::to_value(&summary).expect("serialize stable run DTO");
        assert!(wire.get("fleet").is_some(), "{wire}");
    }

    #[test]
    fn a_run_without_a_receipt_reports_every_route_field_unknown() {
        let summary = fleet_run_summary(&run("run-2"), None);
        for reason in [
            summary.route.provider_id.unknown_reason(),
            summary.route.model.unknown_reason(),
            summary.route.effective_reasoning.unknown_reason(),
            summary.route.route_source.unknown_reason(),
        ] {
            assert_eq!(reason, Some(UnknownReason::NotRecorded));
        }
    }

    #[test]
    fn fleet_run_pages_are_bounded() {
        let mut state = FleetLedgerState::default();
        for index in 0..10 {
            let id = format!("run-{index:03}");
            state.runs.insert(id.clone(), run(&id));
        }
        let page = fleet_run_page(&state, 3);
        assert_eq!(page.total, 10);
        assert_eq!(page.runs.len(), 3);
        assert_eq!(page.truncated, 7);
    }

    #[test]
    fn an_absent_ledger_is_reported_and_not_created() {
        let dir = tempfile::tempdir().unwrap();
        for surface in ControlSurface::ALL {
            let receipt =
                execute_fleet_control(*surface, dir.path(), ControlOperation::FleetStatus, None);
            assert_eq!(
                receipt.availability.reason(),
                Some(UnavailableReason::NoFleetLedger),
                "{surface}"
            );
            assert_eq!(receipt.outcome, LifecycleOutcome::Rejected);
        }
        assert!(
            !fleet_ledger_path(dir.path()).exists(),
            "a read verb must not create the durable ledger"
        );
    }

    #[test]
    fn durable_fleet_status_is_identical_on_every_surface() {
        let dir = tempfile::tempdir().unwrap();
        // Creating the manager is what makes the ledger exist.
        FleetManager::open(dir.path()).unwrap();
        assert_eq!(
            ControlOperation::FleetStatus.descriptor().cli_invocation,
            "codewhale fleet status",
            "fleet remains the canonical public command"
        );
        let mut rendered = std::collections::BTreeSet::new();
        for surface in ControlSurface::ALL {
            let receipt =
                execute_fleet_control(*surface, dir.path(), ControlOperation::FleetStatus, None);
            assert_eq!(receipt.operation_id, "fleet.status");
            assert_eq!(receipt.authority, ControlAuthority::Read);
            assert_eq!(receipt.persistence, PersistenceScope::FleetLedger);
            assert_eq!(receipt.outcome, LifecycleOutcome::Inspected);
            assert!(
                receipt
                    .detail
                    .iter()
                    .any(|line| line.starts_with("fleet: runs=")),
                "the durable ledger snapshot must keep its receipt prefix"
            );
            let mut normalized = receipt.clone();
            normalized.surface = ControlSurface::Cli;
            rendered.insert(normalized.render());
        }
        assert_eq!(rendered.len(), 1, "surfaces rendered different results");
    }

    #[test]
    fn fleet_resume_receipt_preserves_established_fleet_detail_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = FleetLedger::open(dir.path()).unwrap();
        ledger.create_run(&run("run-1")).unwrap();
        let manager = FleetManager::open(dir.path()).unwrap();

        let receipt = execute_fleet_control_with(
            ControlSurface::Cli,
            dir.path(),
            fleet_control_context(dir.path()),
            &manager,
            ControlOperation::FleetResume,
            Some("run-1"),
        );

        assert_eq!(receipt.operation_id, "fleet.resume");
        assert_eq!(receipt.outcome, LifecycleOutcome::NoChange);
        assert_eq!(
            receipt.detail.first().map(String::as_str),
            Some("fleet resume: run-1 reclaimed_stale=0 restarted=0 failed=0 escalated=0")
        );
        assert_eq!(
            receipt.detail.get(1).map(String::as_str),
            Some(
                "fleet: runs=1 queued=0 running=0 completed=0 partial=0 failed=0 restarted=0 \
                 escalated=0 transport_failed=0 task_failed=0 verifier_failed=0 cancelled=0 stale=0"
            )
        );
    }

    #[test]
    fn fleet_restart_is_cli_only_and_says_so_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        FleetManager::open(dir.path()).unwrap();
        {
            let surface = ControlSurface::Slash;
            let receipt = execute_fleet_control(
                surface,
                dir.path(),
                ControlOperation::FleetRestart,
                Some("worker-1"),
            );
            assert_eq!(receipt.outcome, LifecycleOutcome::Rejected);
            assert_eq!(
                receipt.availability.reason(),
                Some(UnavailableReason::SurfaceNotSupported)
            );
            assert!(
                receipt
                    .availability
                    .hint()
                    .is_some_and(|hint| hint.contains("codewhale fleet restart"))
            );
        }
    }

    #[test]
    fn interrupt_requires_an_exact_worker_id() {
        let dir = tempfile::tempdir().unwrap();
        FleetManager::open(dir.path()).unwrap();
        for bad in [None, Some(""), Some("worker one"), Some("../escape")] {
            let receipt = execute_fleet_control(
                ControlSurface::Slash,
                dir.path(),
                ControlOperation::FleetInterrupt,
                bad,
            );
            assert_eq!(
                receipt.failure.as_ref().map(|failure| failure.kind),
                Some(ControlFailureKind::InvalidTarget),
                "{bad:?}"
            );
        }
    }

    /// #4022: an id that is not in the ledger must be refused as `not_found`
    /// *before* any durable write. Resuming a typo used to reconcile nothing,
    /// write a run-status record under the typed id, and report `no_change`.
    #[test]
    fn resuming_an_unknown_run_is_not_found_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        FleetManager::open(dir.path()).unwrap();
        let ledger = fleet_ledger_path(dir.path());
        let before = std::fs::read(&ledger).unwrap();

        for surface in ControlSurface::ALL {
            let receipt = execute_fleet_control(
                *surface,
                dir.path(),
                ControlOperation::FleetResume,
                Some("run-does-not-exist"),
            );
            assert_eq!(receipt.outcome, LifecycleOutcome::Rejected, "{surface}");
            assert_eq!(
                receipt.failure.as_ref().map(|failure| failure.kind),
                Some(ControlFailureKind::NotFound),
                "{surface}"
            );
        }

        assert_eq!(
            std::fs::read(&ledger).unwrap(),
            before,
            "a refused resume must not append to the durable ledger"
        );
        let state = FleetManager::open(dir.path())
            .unwrap()
            .rebuild_state()
            .unwrap();
        assert!(
            !state
                .run_status_overrides
                .contains_key("run-does-not-exist"),
            "a caller-supplied id must never become a durable ledger key"
        );
    }

    /// #4022: newest-first ordering is computed on parsed instants, so a run
    /// recorded at a non-UTC offset still sorts by when it actually happened.
    #[test]
    fn runs_sort_by_utc_instant_not_by_rendered_text() {
        let mut state = FleetLedgerState::default();
        // 2026-07-26T00:30:00+02:00 == 2026-07-25T22:30:00Z, i.e. *earlier*
        // than the UTC-stamped run even though its text sorts later.
        let mut earlier = run("run-offset");
        earlier.created_at = "2026-07-26T00:30:00+02:00".to_string();
        let mut later = run("run-utc");
        later.created_at = "2026-07-25T23:00:00Z".to_string();
        state.runs.insert("run-offset".to_string(), earlier);
        state.runs.insert("run-utc".to_string(), later);

        let page = fleet_run_page(&state, DEFAULT_RUN_LIST_LIMIT);
        assert_eq!(
            page.runs
                .iter()
                .map(|run| run.run_id.as_str())
                .collect::<Vec<_>>(),
            vec!["run-utc", "run-offset"],
            "a string compare would have put run-offset first"
        );
    }

    #[test]
    fn status_and_inspection_rendering_stay_bounded() {
        let mut snapshot = FleetStatusSnapshot::default();
        for index in 0..(MAX_STATUS_WORKER_ROWS + 5) {
            snapshot
                .workers
                .insert(format!("worker-{index:03}"), FleetWorkerStatus::Online);
        }
        let lines = status_lines(&snapshot);
        // summary + "workers:" + capped rows + the omission notice
        assert_eq!(lines.len(), 1 + 1 + MAX_STATUS_WORKER_ROWS + 1);
        assert_eq!(
            lines.first().map(String::as_str),
            Some(
                "fleet: runs=0 queued=0 running=0 completed=0 partial=0 failed=0 restarted=0 \
                 escalated=0 transport_failed=0 task_failed=0 verifier_failed=0 cancelled=0 stale=0"
            )
        );
        assert!(lines.last().unwrap().contains("5 more worker(s) omitted"));
    }
}

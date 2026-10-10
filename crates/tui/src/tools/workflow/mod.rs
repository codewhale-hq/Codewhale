//! Model-facing Workflow runner over the live sub-agent runtime.
//!
//! The JS VM stays in `codewhale-workflow-js`; this module supplies the TUI
//! driver that turns each `task(...)` call into a real `SubAgentManager` spawn.

mod plan_schema;
#[cfg(test)]
mod shortlist_tests;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use codewhale_workflow::{
    AgentType, BranchResult, BranchSpec, BudgetSpec, ControlNodeKind, ControlNodeResult,
    FleetRoleMap, GateKind, GateOn, GateOutcome, GateSpec, GateState, GateStatusLine,
    HandoffArtifact, LaneGateBoard, LeafResult, LeafSpec, ReduceSpec, SequenceSpec, TaskMode,
    WorkflowExecution as IrWorkflowExecution, WorkflowFleetLimits, WorkflowMemoUsage, WorkflowNode,
    WorkflowRunStatus as IrWorkflowRunStatus, WorkflowSpec, WorkflowUsage,
    compile_javascript_workflow, compile_typescript_workflow, leaf_wants_worktree,
    resolve_workflow_agent,
};
use codewhale_workflow_js::{
    BudgetSnapshot, DriverError, ProgressEvent, SCHEMA_RAW_PREVIEW_CHARS, SpawnedTask,
    TaskCompletion, TaskRequest, WORKFLOW_LIFETIME_CAP, WORKFLOW_MAX_CONCURRENT, WorkflowDriver,
    WorkflowRunCancel, WorkflowVm,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::core::events::Event;
use crate::fleet::role::{FleetRole, public_role_label};
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_bool, optional_str, optional_u64,
};
use crate::tools::subagent::{
    SharedSubAgentManager, SubAgentCompletion, SubAgentManager, SubAgentResult, SubAgentRuntime,
    SubAgentStatus, WorkflowTaskSpawnIdentity, WorkflowTaskSpawnMetadata, send_terminal_event,
    spawn_workflow_task,
};
use crate::tools::verifier::run_workflow_completion_gates;
use crate::tools::workflow_plan_approval::{
    WorkflowPlanApprovalReceipt, analyze_workflow_plan_approval_with_config, analyze_workflow_spec,
    workflow_approval_requirement_for,
};
use crate::utils::spawn_supervised;
use crate::work_graph::{
    CancelOutcome, EvidenceKind, EvidenceRef, OperationIntent, OperationObservation,
    OperationOwnerSnapshot, OwnerState, SharedWorkRuntime,
};

/// Keep promoted artifacts compact without clipping ordinary evidence reports.
/// A 900-character cap cut six-line source receipts in half during live Fleet
/// acceptance, so downstream roles could not evaluate evidence the host had
/// already approved.
const WORKFLOW_HANDOFF_MAX_CHARS: usize = 4_000;

/// Model-facing run-record payloads carry only the newest events; the full
/// stream persists per-event in `.codewhale/workflow-runs.jsonl` (#2974).
const WORKFLOW_RESULT_EVENTS_TAIL: usize = 50;
/// Bounded tail for free-form progress lines in model-facing payloads.
const WORKFLOW_RESULT_PROGRESS_TAIL: usize = 20;
/// Bounded tail for rejected child dispatches in the model-facing payload.
/// The durable run journal retains the complete failure ledger.
const WORKFLOW_RESULT_DISPATCH_FAILURES_TAIL: usize = 12;
/// Per-field cap for one model-facing dispatch-failure receipt.
const WORKFLOW_RESULT_DISPATCH_FAILURE_FIELD_MAX_CHARS: usize = 320;
/// Char cap for the VM `result` / `verification` values in model-facing
/// payloads (matches the handoff compaction budget); oversized values
/// collapse to a preview plus a journal pointer.
const WORKFLOW_RESULT_VALUE_MAX_CHARS: usize = 4_000;
/// Char cap per leaf output preview inside the model-facing execution
/// receipt; full child output stays retrievable via the worker ledger and
/// the journal.
const WORKFLOW_RESULT_LEAF_OUTPUT_MAX_CHARS: usize = 500;
const WORKFLOW_COMPLETION_MAX_BYTES: usize = 8 * 1024;
/// Stated upper bound for a bounded model-facing run-record payload; the
/// payload tests assert every `start`/`run`/`status` result stays below it.
const WORKFLOW_RESULT_MAX_CHARS: usize = 24_000;
/// In-memory (and snapshot) event retention per run: only the newest events
/// are kept; older ones remain in the per-event journal lines (#2974).
const WORKFLOW_RUN_EVENTS_MAX_RETAINED: usize = 1_000;
/// In-memory progress retention per run. Progress is journaled line-by-line,
/// so the owner record only needs a bounded newest tail for status/history.
const WORKFLOW_RUN_PROGRESS_MAX_RETAINED: usize = 1_000;
/// In-memory structured dispatch-failure retention per run. The exact count
/// is stored separately and each rejection remains durable as a typed event.
const WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED: usize = WORKFLOW_RESULT_DISPATCH_FAILURES_TAIL;
/// Progress lines the host detail projection keeps for the run manager's
/// detail pane — the newest few, matching what a human scans at a glance.
const HOST_RUN_PROGRESS_TAIL: usize = 3;

#[derive(Clone)]
pub struct WorkflowTool {
    manager: SharedSubAgentManager,
    runtime: SubAgentRuntime,
    approval_decision: &'static str,
}

impl WorkflowTool {
    #[must_use]
    pub fn new(manager: SharedSubAgentManager, runtime: SubAgentRuntime) -> Self {
        Self {
            manager,
            runtime,
            approval_decision: "approved",
        }
    }

    /// Mark execution as approved by the user's explicit `workflow run`
    /// command rather than by an Engine tool-call approval gate.
    #[must_use]
    pub(crate) fn with_explicit_cli_approval(mut self) -> Self {
        self.approval_decision = "approved_explicit_cli_command";
        self
    }
}

type SharedWorkflowRuns = Arc<Mutex<HashMap<String, WorkflowRunRecord>>>;
type SharedWorkflowControllers = Arc<Mutex<HashMap<String, Arc<WorkflowRunController>>>>;
type SharedWorkflowLifecycles = Arc<Mutex<HashMap<String, WorkflowWorkLifecycle>>>;

#[derive(Clone)]
struct WorkflowWorkLifecycle {
    work: SharedWorkRuntime,
    session_id: String,
    external: String,
}

impl WorkflowWorkLifecycle {
    fn register(
        context: &ToolContext,
        run_id: &str,
        title: &str,
    ) -> Result<Option<Self>, ToolError> {
        let Some(work) = context.runtime.work.clone() else {
            return Ok(None);
        };
        let lifecycle = Self {
            work,
            session_id: context.state_namespace.clone(),
            external: format!("workflow:{run_id}"),
        };
        // Work-graph registration is observability bookkeeping: a transiently
        // busy To-do/Plan state must not veto the run. Unbound workflows keep
        // every later reconcile skipped (`if let Some(lifecycle)` at the call
        // sites, and `attach_bound_workflow_lifecycles` binds by lookup).
        if let Err(err) = lifecycle.work.register_operation(
            &lifecycle.session_id,
            OperationIntent::new(
                lifecycle.external.clone(),
                title,
                true,
                "workflow",
                format!("workflow:{run_id}:start"),
            ),
        ) {
            tracing::warn!(
                run_id = %run_id,
                error = %err,
                "workflow work-graph registration skipped; running unbound"
            );
            return Ok(None);
        }
        Ok(Some(lifecycle))
    }

    fn for_bound(context: &ToolContext, run_id: &str) -> Option<Self> {
        let work = context.runtime.work.clone()?;
        let external = format!("workflow:{run_id}");
        work.has_operation_binding(Some(&context.state_namespace), &external)
            .then(|| Self {
                work,
                session_id: context.state_namespace.clone(),
                external,
            })
    }

    fn reconcile_record(&self, record: &WorkflowRunRecord) -> Result<bool, String> {
        let output = record.result.as_ref().and_then(|result| {
            serde_json::to_vec(result).ok().and_then(|bytes| {
                EvidenceRef::new(
                    EvidenceKind::Receipt {
                        owner: "workflow".to_string(),
                    },
                    format!("workflow:{}:result", record.run_id),
                    Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
                    false,
                )
                .ok()
            })
        });
        let state = owner_state_for_run_status(record.status);
        let mut snapshot = OperationOwnerSnapshot::new(
            self.external.clone(),
            state,
            record.lifecycle_seq,
            i64::try_from(record.completed_at_ms.unwrap_or(record.started_at_ms))
                .unwrap_or(i64::MAX),
        );
        if let Some(output) = output {
            snapshot = snapshot.with_output(output);
        }
        self.work.reconcile_operation(&self.session_id, snapshot)
    }

    fn reconcile_cancel(&self, outcome: CancelOutcome) -> Result<bool, String> {
        self.work.reconcile_observation(
            &self.session_id,
            &self.external,
            OperationObservation::CancelUpdate {
                outcome,
                at: i64::try_from(now_ms()).unwrap_or(i64::MAX),
            },
        )
    }

    fn reconcile_spawn_failure(&self) {
        let _ = self.work.reconcile_operation(
            &self.session_id,
            OperationOwnerSnapshot::new(
                self.external.clone(),
                OwnerState::Failed,
                1,
                i64::try_from(now_ms()).unwrap_or(i64::MAX),
            ),
        );
    }

    fn reconcile_missing(&self) {
        let _ = self.work.reconcile_observation(
            &self.session_id,
            &self.external,
            OperationObservation::OwnerMissing {
                checked_at: i64::try_from(now_ms()).unwrap_or(i64::MAX),
            },
        );
    }
}

/// Owner-snapshot projection of a workflow run status. Total and lossless
/// for terminal truth: Degraded stays Degraded — collapsing it into
/// Completed let dashboards and automation read a partial workflow as an
/// ordinary success (#5582). The run record and report keep the per-slot
/// detail either way.
fn owner_state_for_run_status(status: WorkflowRunStatus) -> OwnerState {
    match status {
        WorkflowRunStatus::Running => OwnerState::Running,
        WorkflowRunStatus::Completed => OwnerState::Completed,
        WorkflowRunStatus::Degraded => OwnerState::Degraded,
        WorkflowRunStatus::Failed => OwnerState::Failed,
        WorkflowRunStatus::Cancelled => OwnerState::Cancelled,
    }
}

struct WorkflowRunController {
    driver: Arc<SubAgentWorkflowDriver>,
    vm_cancel: WorkflowRunCancel,
    run_handle: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Only detached starts wake the parent; `run` already returns the result.
    parent_completion_tx: Option<mpsc::Sender<SubAgentCompletion>>,
}

impl WorkflowRunController {
    fn new(driver: Arc<SubAgentWorkflowDriver>, vm_cancel: WorkflowRunCancel) -> Self {
        Self {
            driver,
            vm_cancel,
            run_handle: Mutex::new(None),
            parent_completion_tx: None,
        }
    }

    fn with_parent_completion(mut self, detached: bool) -> Self {
        if detached {
            self.parent_completion_tx = self.driver.runtime.parent_completion_tx.clone();
        }
        self
    }

    fn set_run_handle(&self, handle: tokio::task::JoinHandle<()>) {
        if let Ok(mut guard) = self.run_handle.lock() {
            *guard = Some(handle);
        }
    }

    fn cancel(&self) {
        self.vm_cancel.cancel();
        self.driver.finalize_running_tasks_cancelled();
        self.driver.force_cancel_all();
        if let Ok(mut guard) = self.run_handle.lock()
            && let Some(handle) = guard.take()
        {
            handle.abort();
        }
    }
}

/// Queue the terminal receipt before releasing the live controller. The
/// headless settlement probe observes this same map, so it cannot see an idle
/// Workflow before the receipt is in the existing Engine completion inbox.
fn finish_workflow_controller(state: &WorkflowWorkspaceState, record: &WorkflowRunRecord) {
    let mut controllers = state
        .controllers
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if let Some(controller) = controllers.get(&record.run_id)
        && let Some(tx) = &controller.parent_completion_tx
    {
        // A racing explicit cancel is authoritative even if the VM captured
        // its successful snapshot just before the cancellation was recorded.
        let cancelled = state
            .runs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&record.run_id)
            .filter(|current| current.status == WorkflowRunStatus::Cancelled)
            .cloned();
        let record = cancelled.as_ref().unwrap_or(record);
        let mut receipt = json!({
            "event": if record.status == WorkflowRunStatus::Completed {
                "workflow.completed"
            } else {
                "workflow.failed"
            },
            "agent_id": truncate_chars(&record.run_id, 64),
            "agent_type": "workflow",
            "run_id": truncate_chars(&record.run_id, 64),
            "status": record.status,
            "child_count": record.child_ids.len(),
            "detail": { "tool": "workflow", "action": "status", "run_id": truncate_chars(&record.run_id, 64) },
        });
        let report = written_run_report(&controller.driver.workspace, &record.run_id);
        if let Some(report) = &report {
            receipt["report"] = json!(report);
        }
        let summary = workflow_receipt_summary(
            record,
            &controller.driver.task_records_snapshot(),
            report.as_deref(),
        );
        let payload =
            format!("{summary}\n<codewhale:subagent.done>{receipt}</codewhale:subagent.done>");
        debug_assert!(payload.len() <= WORKFLOW_COMPLETION_MAX_BYTES);
        send_terminal_event(
            tx,
            SubAgentCompletion {
                owner_session_id: controller.driver.owner_session_id.clone(),
                agent_id: record.run_id.clone(),
                payload,
            },
        );
    }
    controllers.remove(&record.run_id);
}

/// The plain-language half of a workflow's parent receipt: what ended, how
/// many agents finished and why the others stopped, where the report is, and
/// a bounded result preview. Event names and the JSON receipt carry the
/// machine-readable status; this is for the reader. `tasks` is the driver's
/// per-task ledger, which counts the agents once the run's retained event
/// tail has been trimmed.
fn workflow_receipt_summary(
    record: &WorkflowRunRecord,
    tasks: &[RuntimeTaskRecord],
    report: Option<&str>,
) -> String {
    let name = record
        .workflow_goal
        .as_deref()
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
        .map(|goal| format!("Workflow \"{}\"", truncate_chars(goal, 160)))
        .unwrap_or_else(|| format!("Workflow {}", truncate_chars(&record.run_id, 64)));
    // Counts come from the driver's ledger; the retained events (at most
    // WORKFLOW_RUN_EVENTS_MAX_RETAINED) only name the agents that stopped.
    let mut tally = WorkflowTally::from_events(&record.events);
    tally.count_from_ledger(
        tasks.iter().map(|task| task.status),
        record.dispatch_failure_count,
    );
    let mut summary = format!(
        "{name} {}: {}.",
        view::run_outcome_phrase(record.status),
        // Bounded so the whole receipt stays under the completion cap.
        truncate_chars(&tally.agents_sentence(), 600)
    );
    if let Some(error) = &record.error {
        summary.push_str(&format!("\nError: {}", truncate_chars(error, 512)));
    }
    match report {
        Some(report) => summary.push_str(&format!("\nReport: {report}")),
        None => summary.push_str(&format!(
            "\nNo report file was written; `workflow status {}` has the run record.",
            truncate_chars(&record.run_id, 64)
        )),
    }
    if let Some(result) = &record.result {
        summary.push_str(&format!(
            "\nResult preview: {}",
            truncate_chars(&result.to_string(), 512)
        ));
    }
    summary.push_str(
        "\nRead the report or the recorded result before summarizing the outcome and remaining work.",
    );
    summary
}

/// Active controllers, including gaps between child phases. This reads only
/// existing live state and never hydrates a journal or creates a workspace.
pub(crate) fn live_workflow_count(workspace: &Path, owner_session_id: &str) -> usize {
    let Some(state) = peek_shared_workflow_state(workspace) else {
        return 0;
    };
    state
        .controllers
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .values()
        .filter(|controller| controller.driver.owner_session_id == owner_session_id)
        .count()
}

#[derive(Debug, Clone, Serialize)]
struct WorkflowRunSummary {
    run_id: String,
    status: WorkflowRunStatus,
    lifecycle_seq: u64,
    started_at_ms: u64,
    completed_at_ms: Option<u64>,
    source_path: Option<PathBuf>,
    workflow_id: Option<String>,
    workflow_goal: Option<String>,
    token_budget: Option<u64>,
    child_count: usize,
    schema_error_count: usize,
    /// Failed `responseSchema` decodes a bounded repair followed (#5583),
    /// including succeeded ones.
    #[serde(default)]
    schema_repair_count: u64,
    dispatch_failure_count: u64,
    progress_count: u64,
    last_progress: Option<String>,
    event_count: usize,
    last_event_type: Option<String>,
    leaf_count: usize,
    branch_count: usize,
    control_count: usize,
    execution_status: Option<IrWorkflowRunStatus>,
    gate_count: usize,
    blocked_gate_count: usize,
    gate_status: Vec<GateStatusLine>,
    error: Option<String>,
    /// Run-wide usage totals reconciled from per-task telemetry (#2974).
    usage: Option<WorkflowRunUsage>,
    /// Events evicted from the retained tail; full stream in the journal.
    events_dropped: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowSchemaError {
    task_id: String,
    message: String,
    /// Which decode stage failed (#5583): `json_parse` or
    /// `schema_validation`. Journals written before the split existed
    /// default to the validation bucket — the common failure shape.
    #[serde(default = "legacy_schema_failure_kind")]
    kind: String,
    /// 1-based attempt that failed terminally (1 = no repair was tried).
    #[serde(default = "legacy_schema_failure_attempt")]
    attempt: u32,
    /// Bounded preview of the raw reply; the full text lives in `artifact`
    /// when one was written.
    #[serde(default)]
    raw_preview: String,
    /// True when the carried raw text was capped at the carry limit.
    #[serde(default)]
    raw_truncated: bool,
    /// Path of the durable raw-output artifact, when the reply exceeded the
    /// preview bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact: Option<String>,
}

/// One failed `responseSchema` decode that a bounded repair followed
/// (#5583). Recorded even when the repair succeeds, so a repaired run shows
/// exactly what was repaired and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowSchemaRepairAttempt {
    task_id: String,
    #[serde(default = "legacy_schema_failure_kind")]
    kind: String,
    /// 1-based number of the attempt that failed.
    attempt: u32,
    message: String,
    #[serde(default)]
    raw_preview: String,
    #[serde(default)]
    raw_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact: Option<String>,
}

fn legacy_schema_failure_kind() -> String {
    "schema_validation".to_string()
}

fn legacy_schema_failure_attempt() -> u32 {
    1
}

/// One `task()` dispatch the driver rejected before any child agent existed.
/// Inside `parallel()` the JS throw collapses into a `null` slot, so without
/// this ledger a run whose fan-out never dispatched anything still reads as
/// successful orchestration (#5035).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowDispatchFailure {
    at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
    message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowUiEvent {
    at_ms: u64,
    /// Conversation that owns this event. Legacy journal entries omit it and
    /// therefore fail closed at every session-facing projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_session_id: Option<String>,
    #[serde(flatten)]
    kind: WorkflowUiEventKind,
}

impl WorkflowUiEvent {
    fn new(owner_session_id: &str, kind: WorkflowUiEventKind) -> Self {
        Self {
            at_ms: now_ms(),
            owner_session_id: Some(owner_session_id.to_string()),
            kind,
        }
    }

    fn at(at_ms: u64, owner_session_id: &str, kind: WorkflowUiEventKind) -> Self {
        Self {
            at_ms,
            owner_session_id: Some(owner_session_id.to_string()),
            kind,
        }
    }

    fn event_type(&self) -> &'static str {
        self.kind.event_type()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WorkflowUiEventKind {
    RunStarted {
        workflow_id: Option<String>,
        workflow_goal: Option<String>,
        source_path: Option<PathBuf>,
        token_budget: Option<u64>,
        /// Agents this run may have live at once. Absent on journals written
        /// before the field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_concurrent: Option<u32>,
    },
    RunCompleted {
        status: WorkflowRunStatus,
        error: Option<String>,
        /// Run-wide usage totals reconciled from per-task telemetry (#2974).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<WorkflowRunUsage>,
        /// One readable line naming what the run returned, so the transcript's
        /// finish line can state the result without the full record.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_preview: Option<String>,
    },
    RunCancelled {
        reason: String,
    },
    PhaseStarted {
        title: String,
    },
    /// A `task()` is waiting for one of the run's concurrent slots. The
    /// matching `task_started` carries the same `queue_ticket`.
    TaskQueued {
        ticket: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    TaskStarted(Box<WorkflowTaskStartedEvent>),
    TaskCompleted {
        task_id: String,
        status: IrWorkflowRunStatus,
        /// One bounded line saying why the agent did not finish. Absent when
        /// it finished, and on journals written before the field existed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<WorkflowTaskStopKind>,
        /// Per-worker telemetry captured at terminal delivery (#2974).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<WorkflowTaskUsage>,
    },
    /// A settled `parallel()`/`pipeline()` slot resolved to `null`. The
    /// per-slot log line carries the message.
    SlotDropped {
        construct: String,
        index: u32,
        kind: String,
    },
    GateUpdated {
        gate_id: String,
        role: String,
        gate: String,
        state: String,
        blocked_role: Option<String>,
        blocked_reason: Option<String>,
    },
    HandoffPromoted {
        artifact_id: String,
        gate_id: String,
        kind: String,
        from_role: String,
        to_role: String,
        producer_task_id: String,
    },
    HandoffConsumed {
        artifact_id: String,
        kind: String,
        from_role: String,
        to_role: String,
        consumer_task_id: String,
    },
    TaskSchemaValidationFailed {
        task_id: String,
        message: String,
    },
    TaskDispatchFailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
        message: String,
        /// Ticket of the `task_queued` wait this refusal ends, when the task
        /// was refused after waiting for a slot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queue_ticket: Option<u64>,
    },
    BudgetUpdated {
        total: Option<u64>,
        spent: u64,
        remaining: Option<u64>,
    },
    Log {
        message: String,
    },
}

mod usage;
mod view;

use usage::{WorkflowRunUsage, WorkflowTaskUsage, WorkflowTokenSource, sum_optional_usage};
use view::{WorkflowTally, WorkflowTaskStopKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowTaskStartedEvent {
    task_id: String,
    label: Option<String>,
    /// Fleet role declared on the step, if any (#4177).
    role: Option<String>,
    profile: Option<String>,
    model: Option<String>,
    strength: Option<String>,
    thinking: Option<String>,
    /// Reasoning the task requested, verbatim (`inherit`/`auto`/effort) (#4039).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    requested_reasoning: Option<String>,
    /// Reasoning the child runtime was actually installed with (#4039). Absent
    /// when the resolved route carries no reasoning control; consumers render
    /// that as unknown rather than inventing an effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effective_reasoning: Option<String>,
    /// Resolved fleet role after roster lookup (#4177).
    resolved_role: Option<String>,
    /// Resolved AgentProfile id after fleet resolution (#4177).
    resolved_profile: Option<String>,
    resolved_provider: String,
    resolved_model: String,
    route_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    child_route: Option<crate::tools::subagent::ChildRouteReceipt>,
    worktree: bool,
    workspace: Option<PathBuf>,
    git_branch: Option<String>,
    parent_task_id: Option<String>,
    depth: u32,
    /// Workflow run that admitted this child (#4119).
    workflow_run_id: Option<String>,
    /// Phase title/id active (or declared on the task) at spawn (#4119).
    workflow_phase_id: Option<String>,
    /// Typed task label — UI must prefer this over prompt text (#4119).
    workflow_task_label: Option<String>,
    /// 0-based admission order among children of this run (#4119).
    workflow_child_index: Option<u32>,
    /// Ticket of the `task_queued` event this start ends, when the task had
    /// to wait for a slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    queue_ticket: Option<u64>,
    /// Durable exact-Fleet routing receipt: the fixed member identity, its
    /// exact provider/model, the requested vs. selector vs. provider-effective
    /// reasoning, where the decision came from, and the Router's exact identity
    /// when a Router made it. `default` keeps events written before this field
    /// existed — and every legacy/non-fleet task — readable unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fleet_receipt: Option<codewhale_workflow::FleetTaskReceipt>,
}

impl WorkflowUiEventKind {
    fn event_type(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "run_started",
            Self::RunCompleted { .. } => "run_completed",
            Self::RunCancelled { .. } => "run_cancelled",
            Self::PhaseStarted { .. } => "phase_started",
            Self::TaskQueued { .. } => "task_queued",
            Self::TaskStarted(_) => "task_started",
            Self::TaskCompleted { .. } => "task_completed",
            Self::SlotDropped { .. } => "slot_dropped",
            Self::GateUpdated { .. } => "gate_updated",
            Self::HandoffPromoted { .. } => "handoff_promoted",
            Self::HandoffConsumed { .. } => "handoff_consumed",
            Self::TaskSchemaValidationFailed { .. } => "task_schema_validation_failed",
            Self::TaskDispatchFailed { .. } => "task_dispatch_failed",
            Self::BudgetUpdated { .. } => "budget_updated",
            Self::Log { .. } => "log",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkflowRunRecord {
    run_id: String,
    /// Conversation that created the run. `None` means a legacy journal entry
    /// with unknown ownership and is intentionally invisible/non-mutable from
    /// every session-facing control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner_session_id: Option<String>,
    status: WorkflowRunStatus,
    #[serde(default)]
    lifecycle_seq: u64,
    started_at_ms: u64,
    completed_at_ms: Option<u64>,
    source_path: Option<PathBuf>,
    workflow_id: Option<String>,
    workflow_goal: Option<String>,
    token_budget: Option<u64>,
    child_ids: Vec<String>,
    /// Exact progress-line count, including entries older than `progress`'s
    /// bounded in-memory tail. Legacy snapshots are repaired on hydration.
    #[serde(default)]
    progress_count: u64,
    progress: Vec<String>,
    #[serde(default)]
    events: Vec<WorkflowUiEvent>,
    schema_errors: Vec<WorkflowSchemaError>,
    /// Failed `responseSchema` decodes that a bounded repair followed
    /// (#5583), including repairs that succeeded. Structured tail of a
    /// monotonic `schema_repair_count`.
    #[serde(default)]
    schema_repairs: Vec<WorkflowSchemaRepairAttempt>,
    #[serde(default)]
    schema_repair_count: u64,
    /// Task dispatches the driver rejected before any child ran (#5035).
    /// This is the exact, saturating total; `dispatch_failures` is only the
    /// newest structured tail.
    #[serde(default)]
    dispatch_failure_count: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dispatch_failures: Vec<WorkflowDispatchFailure>,
    result: Option<Value>,
    execution: Option<IrWorkflowExecution>,
    error: Option<String>,
    #[serde(default)]
    verify_on_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verification: Option<Value>,
    /// Durable elevated-plan approval receipt for audit (#4126).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_approval: Option<WorkflowPlanApprovalReceipt>,
    /// Compact lane gate state for status / panel surfaces (#4179).
    #[serde(default)]
    gate_status: Vec<GateStatusLine>,
    /// Run-wide usage totals reconciled at completion (#2974).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<WorkflowRunUsage>,
    /// Total events recorded for this run (monotonic; survives the bounded
    /// `events` tail retention) (#2974).
    #[serde(default)]
    events_total: u64,
    /// Events evicted from the in-memory tail; available in the journal.
    #[serde(default)]
    events_dropped: u64,
}

impl WorkflowRunRecord {
    fn new(
        run_id: String,
        owner_session_id: Option<String>,
        source_path: Option<PathBuf>,
        token_budget: Option<u64>,
        spec: Option<&WorkflowSpec>,
    ) -> Self {
        let gate_status = spec
            .map(|spec| initial_gate_status(&run_id, &spec.gates))
            .unwrap_or_default();
        Self {
            run_id,
            owner_session_id,
            status: WorkflowRunStatus::Running,
            lifecycle_seq: 1,
            started_at_ms: now_ms(),
            completed_at_ms: None,
            source_path,
            workflow_id: spec.and_then(|spec| spec.id.clone()),
            workflow_goal: spec.map(|spec| spec.goal.clone()),
            token_budget,
            child_ids: Vec::new(),
            progress_count: 0,
            progress: Vec::new(),
            events: Vec::new(),
            schema_errors: Vec::new(),
            schema_repairs: Vec::new(),
            schema_repair_count: 0,
            dispatch_failure_count: 0,
            dispatch_failures: Vec::new(),
            result: None,
            execution: None,
            error: None,
            verify_on_complete: false,
            verification: None,
            plan_approval: None,
            gate_status,
            usage: None,
            events_total: 0,
            events_dropped: 0,
        }
    }

    /// Record one event, bounding retention to the newest
    /// `WORKFLOW_RUN_EVENTS_MAX_RETAINED` entries (#2974). Every event is
    /// journaled per-line at record time, so evicted entries remain
    /// available in `.codewhale/workflow-runs.jsonl`.
    fn push_event(&mut self, event: WorkflowUiEvent) {
        self.events_total = self.events_total.saturating_add(1);
        self.events.push(event);
        if self.events.len() > WORKFLOW_RUN_EVENTS_MAX_RETAINED {
            let overflow = self.events.len() - WORKFLOW_RUN_EVENTS_MAX_RETAINED;
            self.events.drain(..overflow);
            self.events_dropped = self
                .events_dropped
                .saturating_add(u64::try_from(overflow).unwrap_or(u64::MAX));
        }
    }

    /// Retain only the newest progress lines while preserving an exact,
    /// saturating total for summaries and payload truncation receipts.
    fn push_progress(&mut self, message: String) {
        self.progress_count = self.progress_count.saturating_add(1);
        self.progress.push(message);
        if self.progress.len() > WORKFLOW_RUN_PROGRESS_MAX_RETAINED {
            let overflow = self.progress.len() - WORKFLOW_RUN_PROGRESS_MAX_RETAINED;
            self.progress.drain(..overflow);
        }
    }

    /// Record one rejected task slot without allowing a malformed workflow's
    /// rejection loop to grow the owner record without bound.
    fn push_dispatch_failure(&mut self, failure: WorkflowDispatchFailure) {
        self.dispatch_failure_count = self.dispatch_failure_count.saturating_add(1);
        self.dispatch_failures.push(failure);
        if self.dispatch_failures.len() > WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED {
            let overflow =
                self.dispatch_failures.len() - WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED;
            self.dispatch_failures.drain(..overflow);
        }
    }

    /// Repair legacy or malformed snapshot counters before exposing them.
    /// A declared count may exceed the retained tail, but never trail it.
    fn normalize_bounded_ledgers(&mut self) {
        self.progress_count = self
            .progress_count
            .max(u64::try_from(self.progress.len()).unwrap_or(u64::MAX));
        if self.progress.len() > WORKFLOW_RUN_PROGRESS_MAX_RETAINED {
            let overflow = self.progress.len() - WORKFLOW_RUN_PROGRESS_MAX_RETAINED;
            self.progress.drain(..overflow);
        }

        self.dispatch_failure_count = self
            .dispatch_failure_count
            .max(u64::try_from(self.dispatch_failures.len()).unwrap_or(u64::MAX));
        if self.dispatch_failures.len() > WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED {
            let overflow =
                self.dispatch_failures.len() - WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED;
            self.dispatch_failures.drain(..overflow);
        }
    }

    fn summary(&self) -> WorkflowRunSummary {
        WorkflowRunSummary {
            run_id: self.run_id.clone(),
            status: self.status,
            lifecycle_seq: self.lifecycle_seq,
            started_at_ms: self.started_at_ms,
            completed_at_ms: self.completed_at_ms,
            source_path: self.source_path.clone(),
            workflow_id: self.workflow_id.clone(),
            workflow_goal: self.workflow_goal.clone(),
            token_budget: self.token_budget,
            child_count: self.child_ids.len(),
            schema_error_count: self.schema_errors.len(),
            schema_repair_count: self.schema_repairs.len() as u64,
            dispatch_failure_count: self.dispatch_failure_count,
            progress_count: self.progress_count,
            last_progress: self.progress.last().cloned(),
            event_count: usize::try_from(self.events_total.max(self.events.len() as u64))
                .unwrap_or(usize::MAX),
            last_event_type: self
                .events
                .last()
                .map(|event| event.event_type().to_string()),
            leaf_count: self
                .execution
                .as_ref()
                .map(|execution| execution.leaf_results.len())
                .unwrap_or_default(),
            branch_count: self
                .execution
                .as_ref()
                .map(|execution| execution.branch_results.len())
                .unwrap_or_default(),
            control_count: self
                .execution
                .as_ref()
                .map(|execution| execution.control_node_results.len())
                .unwrap_or_default(),
            execution_status: self.execution.as_ref().map(|execution| execution.status),
            gate_count: self.gate_status.len(),
            blocked_gate_count: self
                .gate_status
                .iter()
                .filter(|line| line.blocked_reason.is_some())
                .count(),
            gate_status: self.gate_status.clone(),
            error: self.error.clone(),
            usage: self.usage.clone(),
            events_dropped: self.events_dropped,
        }
    }
}

fn initial_gate_status(run_id: &str, gates: &[GateSpec]) -> Vec<GateStatusLine> {
    if gates.is_empty() {
        return Vec::new();
    }
    let mut board = LaneGateBoard::new(run_id);
    board.install_gates(gates);
    board.status_summary()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorkflowRunStatus {
    Running,
    Completed,
    /// The script returned a value, but at least one requested task slot
    /// failed or was rejected without the script declaring a partial-failure
    /// contract. The output is preserved; the status refuses to call a run
    /// with dropped slots a plain success (receipt honesty, morning-report
    /// issue #2).
    Degraded,
    Failed,
    Cancelled,
}

/// The per-slot outcome ledger a finished run is classified against.
///
/// A workflow script can return a perfectly good-looking value while every
/// task it fanned out died — `parallel()`'s settled default resolves a failed
/// slot to `null`, so the script never sees a throw. Terminal status is
/// therefore decided from what actually ran, not from the script's return.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SlotLedger {
    /// Task records the driver holds for this run (children who were admitted).
    tasks: usize,
    /// How many of those records ended in a failed terminal state — a
    /// subagent failure, a budget-exhausted stop, or a replay divergence.
    /// A budget-dead child produced no result either; counting only plain
    /// `Failed` let an all-budget fan-out classify as a clean completion.
    failed_tasks: usize,
    /// Requested slots refused before a child existed — driver admission
    /// refusals plus `ProgressEvent::TaskRejected`.
    rejected: u64,
    /// Fan-outs (`parallel()`/`pipeline()`) that resolved with every slot
    /// failed and nothing surviving, script throws included. Those leave no
    /// task record at all, so only the structured
    /// `ProgressEvent::FanoutAllSlotsFailed` count makes them visible here.
    dead_fanouts: u32,
    /// Individual slots a settled fan-out dropped to null while other slots
    /// survived (`ProgressEvent::FanoutSlotDropped`). A partial loss with
    /// surviving work: Degraded, never a clean Completed.
    dropped_slots: u32,
    /// Whether any child agent was ever admitted for this run.
    any_child_ran: bool,
    /// First retained dispatch-failure message, for the all-rejected receipt.
    retained_detail: Option<String>,
}

impl SlotLedger {
    /// The terminal status a `Completed` script run should actually record,
    /// with the receipt line, or `None` when every slot came back clean.
    fn classify(&self) -> Option<(WorkflowRunStatus, String)> {
        let dead_fanouts = if self.dead_fanouts > 0 {
            format!(
                " and {} fan-out(s) lost every slot (no work survived them)",
                self.dead_fanouts
            )
        } else {
            String::new()
        };
        if !self.any_child_ran && self.rejected > 0 {
            // #5035: every dispatch was rejected before a child ran.
            let retained = self
                .retained_detail
                .as_ref()
                .map(|message| format!("; retained detail: {message}"))
                .unwrap_or_default();
            return Some((
                WorkflowRunStatus::Failed,
                format!(
                    "no child agents ran: all {} task dispatch(es) were rejected{retained}{dead_fanouts}",
                    self.rejected
                ),
            ));
        }
        // R9: a run whose every task failed produced nothing. Reporting that
        // as a partial success let a workflow that lost all its work read as
        // "completed with caveats"; it is a failure with a preserved output.
        // `failed_tasks` counts every failure-ish terminal state, so a
        // fan-out that died entirely of budget exhaustion lands here too.
        if self.tasks > 0 && self.failed_tasks == self.tasks {
            let rejected = if self.rejected > 0 {
                format!(" and {} dispatch(es) were rejected", self.rejected)
            } else {
                String::new()
            };
            return Some((
                WorkflowRunStatus::Failed,
                format!(
                    "no task produced a result: all {} task(s) failed{rejected}{dead_fanouts}; \
                     the recorded result reflects no completed work",
                    self.tasks
                ),
            ));
        }
        // R9: a fan-out can die without a single task record — thunks that
        // throw before (or instead of) calling `task()` resolve to null slots
        // and leave only the dead-fan-out count behind. When no child
        // anywhere survived, that run also produced nothing.
        if self.dead_fanouts > 0 && self.tasks == 0 {
            let rejected = if self.rejected > 0 {
                format!(" and {} dispatch(es) were rejected", self.rejected)
            } else {
                String::new()
            };
            return Some((
                WorkflowRunStatus::Failed,
                format!(
                    "no work survived: {} fan-out(s) lost every slot{rejected}; \
                     the recorded result reflects no completed work",
                    self.dead_fanouts
                ),
            ));
        }
        if self.failed_tasks > 0
            || self.rejected > 0
            || self.dead_fanouts > 0
            || self.dropped_slots > 0
        {
            let mut parts = Vec::new();
            if self.failed_tasks > 0 {
                parts.push(format!(
                    "{} of {} task(s) failed",
                    self.failed_tasks, self.tasks
                ));
            }
            if self.rejected > 0 {
                parts.push(format!("{} dispatch(es) were rejected", self.rejected));
            }
            if self.dead_fanouts > 0 {
                parts.push(format!("{} fan-out(s) lost every slot", self.dead_fanouts));
            }
            if self.dropped_slots > 0 {
                parts.push(format!(
                    "{} fan-out slot(s) dropped while others survived",
                    self.dropped_slots
                ));
            }
            return Some((
                WorkflowRunStatus::Degraded,
                format!(
                    "completed with dropped slots: {}; the recorded result may be partial",
                    parts.join(" and ")
                ),
            ));
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowAction {
    Start,
    Run,
    Status,
    Cancel,
}

fn parse_workflow_action(input: &Value) -> Result<WorkflowAction, ToolError> {
    let Some(action) = optional_str(input, "action")? else {
        return Ok(WorkflowAction::Start);
    };
    match action.trim().to_ascii_lowercase().as_str() {
        "" | "start" | "spawn" => Ok(WorkflowAction::Start),
        "run" | "wait" => Ok(WorkflowAction::Run),
        "status" | "list" | "inspect" => Ok(WorkflowAction::Status),
        "cancel" | "stop" | "abort" => Ok(WorkflowAction::Cancel),
        other => Err(ToolError::invalid_input(format!(
            "Invalid workflow action '{other}'. Use start, run, status, or cancel."
        ))),
    }
}

#[async_trait]
impl ToolSpec for WorkflowTool {
    fn name(&self) -> &'static str {
        "workflow"
    }

    fn description(&self) -> &'static str {
        concat!(
            "Run named steps through the existing sub-agents with a structured plan, ordered phases, shared budgets and result handoffs. Fleet configures those same sub-agents and roles. ",
            "A workflow is for broad or staged work: several steps with an order, phases, gates, or a fan-in of results. ",
            "agent(action=\"roster\") lists the saved models, role/profile assignments, and route availability that steps can be assigned. ",
            "plan is the structured input form for multi-step work; saved or advanced workflows can use script/source_path; exactly one input form is accepted. ",
            "action=start runs the orchestration detached and action=status with run_id reports its progress. action=run waits and returns the final result."
        )
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "run", "status", "cancel"],
                    "description": "start (default) launches a Workflow in the background. run waits for completion. status lists runs or inspects run_id. cancel stops a run and its child agents."
                },
                "run_id": {
                    "type": "string",
                    "description": "Workflow run id for action=status or action=cancel."
                },
                "script": {
                    "type": "string",
                    "description": "Workflow JS source. The runtime provides args, task(...), parallel(thunks), pipeline(thunks), log(...), phase(...), and budget. Fan-out syntax: await parallel([() => task({...}), () => task({...})]). parallel() requires one array of zero-argument thunks, not variadic task promises."
                },
                "source_path": {
                    "type": "string",
                    "description": "Path to a .workflow.js script inside the workspace. Use instead of script for checked-in workflows."
                },
                "fleet": {
                    "type": "string",
                    "description": "Named Fleet from $CODEWHALE_HOME/fleets/, <workspace>/.codewhale/fleets/, or checked-in <workspace>/fleets/; qualified origin/name accepted (codewhale_home, workspace, workspace_root). Exact Fleets freeze member identity, route, and reasoning. Runtime derives authority from role and live parent; per-task route/authority overrides are rejected."
                },
                "plan": plan_schema::structured_plan_schema(),
                "args": {
                    "anyOf": [
                        { "type": "null" },
                        { "type": "boolean" },
                        { "type": "integer" },
                        { "type": "number" },
                        { "type": "string" },
                        { "type": "array" },
                        {
                            "type": "object",
                            "additionalProperties": {}
                        }
                    ],
                    "description": "JSON value exposed to the script as args. Defaults to null."
                },
                "token_budget": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional shared Workflow admission cap; omit for no cap. Usage is reconciled when children report completion; already-running parallel children can take aggregate spent past the cap, while later and descendant spawns are rejected once exhausted."
                },
                "wait": {
                    "type": "boolean",
                    "description": "For action=start, wait for completion instead of returning immediately."
                },
                "verify": {
                    "type": "boolean",
                    "default": false,
                    "description": "After a successful workflow completion, run quick workspace verifier gates (auto/quick profile)."
                }
            },
            "required": [],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ExecutesCode,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        // Default posture: elevated starts require approval. Concrete inputs
        // refine this via `approval_requirement_for` (#4126).
        ApprovalRequirement::Required
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        // The session's `[workflow]` table decides read-only auto-start and
        // write approval; product defaults apply only when the runtime never
        // threaded a config. YOLO/bypass still short-circuit upstream.
        let config = workflow_config_for(&self.runtime);
        workflow_approval_requirement_for(input, &config)
    }

    fn starts_detached_for(&self, input: &Value) -> bool {
        // A scheduling hint, not an authority decision, and this trait method
        // cannot report an error. A malformed `wait` reads as "not detached"
        // so the call stays in the foreground; `execute` then refuses it with
        // the named-parameter error rather than running anything.
        matches!(parse_workflow_action(input), Ok(WorkflowAction::Start))
            && !optional_bool(input, "wait", false).unwrap_or(true)
    }

    fn supports_parallel_for(&self, input: &Value) -> bool {
        matches!(parse_workflow_action(input), Ok(WorkflowAction::Status))
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        matches!(parse_workflow_action(input), Ok(WorkflowAction::Status))
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let state = shared_workflow_state(&context.workspace);
        attach_bound_workflow_lifecycles(context, &state)?;
        // Keyed off the parsed `WorkflowAction` discriminant, never off
        // `input["action"]`. The JSON Schema published to the model is a
        // declaration, not a guard: the real parse also accepts `spawn`,
        // `wait`, `list`, `inspect`, `stop`, and `abort`, and its reject arm
        // embeds the model's string verbatim.
        let action = parse_workflow_action(&input)?;
        codewhale_telemetry::session_counters().bump(codewhale_telemetry::Counter::WorkflowRun);
        match action {
            WorkflowAction::Start => {
                let wait = optional_bool(&input, "wait", false)?;
                start_workflow(
                    input,
                    context,
                    self.manager.clone(),
                    self.runtime.clone(),
                    state,
                    wait,
                    self.approval_decision,
                )
                .await
            }
            WorkflowAction::Run => {
                start_workflow(
                    input,
                    context,
                    self.manager.clone(),
                    self.runtime.clone(),
                    state,
                    true,
                    self.approval_decision,
                )
                .await
            }
            WorkflowAction::Status => status_workflow(input, state, &context.state_namespace),
            WorkflowAction::Cancel => cancel_workflow(input, state, &context.state_namespace).await,
        }
    }
}

fn attach_bound_workflow_lifecycles(
    context: &ToolContext,
    state: &Arc<WorkflowWorkspaceState>,
) -> Result<(), ToolError> {
    let records = lock_mutex(&state.runs)?
        .values()
        .filter(|record| {
            record.owner_session_id.as_deref() == Some(context.state_namespace.as_str())
        })
        .cloned()
        .collect::<Vec<_>>();
    for record in records {
        if let Some(lifecycle) = WorkflowWorkLifecycle::for_bound(context, &record.run_id) {
            state.attach_lifecycle(&record.run_id, lifecycle);
            state.reconcile_snapshot(&record);
        }
    }
    Ok(())
}

fn fail_workflow_start(state: &Arc<WorkflowWorkspaceState>, run_id: &str, message: String) {
    let snapshot = state.runs.lock().ok().and_then(|mut runs| {
        let record = runs.get_mut(run_id)?;
        record.status = WorkflowRunStatus::Failed;
        record.lifecycle_seq = record.lifecycle_seq.saturating_add(1);
        record.completed_at_ms = Some(now_ms());
        record.error = Some(message);
        Some(record.clone())
    });
    let Some(snapshot) = snapshot else {
        state.mark_owner_missing(run_id);
        return;
    };
    if state.try_record_snapshot(&snapshot).is_ok() {
        state.reconcile_snapshot(&snapshot);
    } else {
        state.mark_owner_missing(run_id);
    }
}

fn fail_workflow_after_controller_registration(
    state: &Arc<WorkflowWorkspaceState>,
    run_id: &str,
    controller: &Arc<WorkflowRunController>,
    message: String,
) {
    controller.cancel();
    if let Ok(mut controllers) = state.controllers.lock() {
        controllers.remove(run_id);
    }
    fail_workflow_start(state, run_id, message);
}

#[allow(clippy::too_many_arguments)]
async fn start_workflow(
    input: Value,
    context: &ToolContext,
    manager: SharedSubAgentManager,
    runtime: SubAgentRuntime,
    state: Arc<WorkflowWorkspaceState>,
    wait: bool,
    approval_decision: &str,
) -> Result<ToolResult, ToolError> {
    if runtime.cancel_token.is_cancelled() {
        return Err(ToolError::execution_failed(
            "Workflow parent session is cancelled",
        ));
    }
    let mut source = workflow_source(&input, context)?;
    let workflow_cfg = workflow_config_for(&runtime);
    for (name, value) in [
        ("max_children", workflow_cfg.max_children),
        ("max_depth", workflow_cfg.max_depth),
        ("max_concurrent", workflow_cfg.max_concurrent),
    ] {
        if value == 0 {
            return Err(ToolError::invalid_input(format!(
                "workflow.{name} must be greater than zero"
            )));
        }
    }
    if let Some(spec) = source.spec.as_mut() {
        validate_runtime_gates(&mut spec.gates)?;
        spec.validate_for_fleet_with_limits(WorkflowFleetLimits {
            max_total_agents: (workflow_cfg.max_children as usize)
                .min(codewhale_workflow::DEFAULT_FLEET_WORKFLOW_MAX_AGENTS),
            max_depth: (workflow_cfg.max_depth as usize)
                .min(codewhale_workflow::DEFAULT_FLEET_WORKFLOW_MAX_DEPTH),
        })
        .map_err(|err| ToolError::invalid_input(format!(
            "Workflow plan exceeds workflow.max_children or workflow.max_depth, or is invalid: {err}"
        )))?;
    }
    let args = input.get("args").cloned().unwrap_or(Value::Null);
    let token_budget = optional_u64(&input, "token_budget", 0)?;
    let token_budget = [
        (token_budget > 0).then_some(token_budget),
        source.spec.as_ref().and_then(|spec| spec.budget.max_tokens),
    ]
    .into_iter()
    .flatten()
    .min()
    .or((workflow_cfg.default_token_budget > 0).then_some(workflow_cfg.default_token_budget));
    let verify_on_complete = optional_bool(&input, "verify", false)?;
    let fleet = if let Some(name) = workflow_fleet_name(&input)? {
        let workspace = context.workspace.clone();
        let api_config = runtime.api_config.clone();
        tokio::task::spawn_blocking(move || {
            workflow_fleet_binding(&name, &workspace, api_config.as_deref())
        })
        .await
        .map_err(|error| ToolError::execution_failed(format!("Fleet loading failed: {error}")))??
    } else {
        WorkflowFleetBinding::None
    };
    let run_id = format!("workflow_{}", &Uuid::new_v4().to_string()[..8]);
    let gate_specs = source
        .spec
        .as_ref()
        .map(|spec| spec.gates.clone())
        .unwrap_or_default();

    // Capture the approved plan envelope for audit/receipt (#4126). Reaching
    // execute means the approval gate already passed (or YOLO/auto-start).
    let summary = source
        .spec
        .as_ref()
        .map(|spec| analyze_workflow_spec(spec, token_budget, &workflow_cfg))
        .unwrap_or_else(|| analyze_workflow_plan_approval_with_config(&input, &workflow_cfg));
    let approval_decision = if summary.is_read_only_envelope() {
        "auto_read_only"
    } else {
        approval_decision
    };
    let plan_approval = summary.to_receipt(approval_decision, now_ms());
    let workflow_title = source
        .spec
        .as_ref()
        .map(|spec| spec.goal.as_str())
        .or_else(|| {
            source
                .path
                .as_ref()
                .and_then(|path| path.file_name()?.to_str())
        })
        .unwrap_or("Workflow run");
    let lifecycle = WorkflowWorkLifecycle::register(context, &run_id, workflow_title)?;

    {
        let mut runs_guard = match lock_mutex(&state.runs) {
            Ok(guard) => guard,
            Err(err) => {
                if let Some(lifecycle) = lifecycle.as_ref() {
                    lifecycle.reconcile_spawn_failure();
                }
                return Err(err);
            }
        };
        let mut record = WorkflowRunRecord::new(
            run_id.clone(),
            Some(context.state_namespace.clone()),
            source.path.clone(),
            token_budget,
            source.spec.as_ref(),
        );
        record.verify_on_complete = verify_on_complete;
        record.plan_approval = Some(plan_approval.clone());
        let started = WorkflowUiEvent::at(
            record.started_at_ms,
            &context.state_namespace,
            WorkflowUiEventKind::RunStarted {
                workflow_id: record.workflow_id.clone(),
                workflow_goal: record.workflow_goal.clone(),
                source_path: record.source_path.clone(),
                token_budget: record.token_budget,
                max_concurrent: Some(
                    workflow_cfg
                        .max_concurrent
                        .min(WORKFLOW_MAX_CONCURRENT as u32),
                ),
            },
        );
        record.push_event(started.clone());
        runs_guard.insert(run_id.clone(), record.clone());
        if let Err(err) = state.try_record_snapshot(&record) {
            runs_guard.remove(&run_id);
            if let Some(lifecycle) = lifecycle.as_ref() {
                lifecycle.reconcile_spawn_failure();
            }
            return Err(ToolError::execution_failed(format!(
                "workflow journal snapshot failed before launch: {err}"
            )));
        }
        // #4122: emit RunStarted immediately so the panel + history card open
        // before the first task/phase (including wait:false fire-and-forget).
        if let Some(tx) = runtime.event_tx.as_ref()
            && let Ok(mut value) = serde_json::to_value(&started)
        {
            if let Some(obj) = value.as_object_mut() {
                obj.insert("run_id".to_string(), json!(run_id));
            }
            let _ = tx.try_send(Event::WorkflowUi {
                owner_session_id: context.state_namespace.clone(),
                run_id: run_id.clone(),
                event: value,
            });
        }
    }
    if let Some(lifecycle) = lifecycle {
        state.attach_lifecycle(&run_id, lifecycle);
    }

    // Children share the spawn runtime; the driver keeps the exact Fleet
    // binding frozen for this run, including native model selectors.
    let driver = SubAgentWorkflowDriver::new(
        run_id.clone(),
        context.state_namespace.clone(),
        manager,
        runtime,
        state.clone(),
        token_budget,
        fleet,
        gate_specs,
        context.workspace.clone(),
    );
    let vm_cancel = WorkflowRunCancel::new();
    let controller = Arc::new(
        WorkflowRunController::new(driver.clone(), vm_cancel.clone()).with_parent_completion(!wait),
    );
    if let Err(err) = lock_mutex(&state.controllers).map(|mut controllers_guard| {
        controllers_guard.insert(run_id.clone(), controller.clone());
    }) {
        fail_workflow_start(&state, &run_id, err.to_string());
        return Err(err);
    }
    let running_snapshot = {
        let mut runs_guard = match lock_mutex(&state.runs) {
            Ok(guard) => guard,
            Err(err) => {
                fail_workflow_after_controller_registration(
                    &state,
                    &run_id,
                    &controller,
                    err.to_string(),
                );
                return Err(err);
            }
        };
        let Some(record) = runs_guard.get_mut(&run_id) else {
            drop(runs_guard);
            fail_workflow_after_controller_registration(
                &state,
                &run_id,
                &controller,
                "workflow owner record disappeared before launch".to_string(),
            );
            return Err(ToolError::execution_failed(
                "workflow owner record disappeared before launch",
            ));
        };
        record.lifecycle_seq = record.lifecycle_seq.saturating_add(1);
        record.clone()
    };
    if let Err(err) = state.try_record_snapshot(&running_snapshot) {
        fail_workflow_after_controller_registration(
            &state,
            &run_id,
            &controller,
            format!("workflow journal failed while activating owner: {err}"),
        );
        return Err(ToolError::execution_failed(format!(
            "workflow journal failed while activating owner: {err}"
        )));
    }
    state.reconcile_snapshot(&running_snapshot);

    let run = run_workflow_vm(
        run_id.clone(),
        source.source,
        source.spec,
        args,
        driver,
        state.clone(),
        context.clone(),
        vm_cancel,
    );
    if wait {
        run.await;
    } else {
        let handle = spawn_supervised("workflow-run", std::panic::Location::caller(), run);
        controller.set_run_handle(handle);
    }

    workflow_result_for(&run_id, state, &context.state_namespace)
}

fn status_workflow(
    input: Value,
    state: Arc<WorkflowWorkspaceState>,
    owner_session_id: &str,
) -> Result<ToolResult, ToolError> {
    if let Some(run_id) = optional_str(&input, "run_id")? {
        return workflow_result_for(run_id, state, owner_session_id);
    }
    let mut summaries = {
        let runs_guard = lock_mutex(&state.runs)?;
        runs_guard
            .values()
            .filter(|record| record.owner_session_id.as_deref() == Some(owner_session_id))
            .map(WorkflowRunRecord::summary)
            .collect::<Vec<_>>()
    };
    summaries.sort_by_key(|record| record.started_at_ms);
    ToolResult::json(&json!({
        "action": "status",
        "count": summaries.len(),
        "runs": summaries,
    }))
    .map_err(|err| ToolError::execution_failed(err.to_string()))
}

async fn cancel_workflow(
    input: Value,
    state: Arc<WorkflowWorkspaceState>,
    owner_session_id: &str,
) -> Result<ToolResult, ToolError> {
    let run_id =
        optional_str(&input, "run_id")?.ok_or_else(|| ToolError::missing_field("run_id"))?;
    cancel_workflow_run(run_id, state, owner_session_id)
}

/// Synchronous cancellation core shared by the model-facing tool action and
/// the host `/workflow cancel` command. When a live controller exists this
/// signals the VM, aborts the run task, journals the terminal snapshot, and
/// streams the cancelled event. When the journal has a running line but no
/// controller (typical after a restart), the journal is still marked
/// cancelled with an honest nothing-live receipt. Nothing here waits on the
/// network.
fn cancel_workflow_run(
    run_id: &str,
    state: Arc<WorkflowWorkspaceState>,
    owner_session_id: &str,
) -> Result<ToolResult, ToolError> {
    // Resolve ownership before touching the controller map or disclosing
    // status. Foreign and legacy-ownerless ids are indistinguishable from an
    // unknown run.
    {
        let runs_guard = lock_mutex(&state.runs)?;
        let record = runs_guard
            .get(run_id)
            .filter(|record| record.owner_session_id.as_deref() == Some(owner_session_id));
        record.ok_or_else(|| {
            ToolError::invalid_input(format!("Unknown workflow run_id '{run_id}'"))
        })?;
    };
    // Terminal publication and a cancel claim share this lock. If cancellation
    // claims a live controller first, no success receipt can pass that claim.
    let controllers_guard = lock_mutex(&state.controllers)?;
    let controller = controllers_guard.get(run_id).cloned();
    let current_status = lock_mutex(&state.runs)?
        .get(run_id)
        .map(|record| record.status)
        .ok_or_else(|| ToolError::invalid_input(format!("Unknown workflow run_id '{run_id}'")))?;
    if current_status != WorkflowRunStatus::Running && controller.is_none() {
        drop(controllers_guard);
        state.reconcile_cancel(run_id, CancelOutcome::AlreadyFinished);
        if let Ok(runs_guard) = state.runs.lock()
            && let Some(record) = runs_guard.get(run_id)
        {
            state.reconcile_snapshot(record);
        }
        return workflow_result_for(run_id, state, owner_session_id);
    }
    let live = controller.is_some();
    let reason = if live {
        "cancelled by workflow tool"
    } else {
        "cancelled; no live process to stop"
    }
    .to_string();
    let cancelled_event = WorkflowUiEvent::new(
        owner_session_id,
        WorkflowUiEventKind::RunCancelled {
            reason: reason.clone(),
        },
    );
    let snapshot = {
        let mut runs_guard = lock_mutex(&state.runs)?;
        let record = runs_guard.get_mut(run_id).ok_or_else(|| {
            ToolError::invalid_input(format!("Unknown workflow run_id '{run_id}'"))
        })?;
        record.status = WorkflowRunStatus::Cancelled;
        record.lifecycle_seq = record.lifecycle_seq.saturating_add(1);
        record.completed_at_ms = Some(now_ms());
        record.error = Some(reason);
        record.push_event(cancelled_event.clone());
        record.clone()
    };
    drop(controllers_guard);
    state.reconcile_cancel(
        run_id,
        if live {
            CancelOutcome::Requested
        } else {
            CancelOutcome::Acknowledged
        },
    );
    if let Some(controller) = controller.as_ref() {
        controller.cancel();
    }
    if let Err(err) = state.try_record_snapshot(&snapshot) {
        state.mark_owner_missing(run_id);
        let mut failed = snapshot;
        failed.error = Some(format!("workflow cancellation journal failed: {err}"));
        if let Some(controller) = controller.as_ref() {
            write_run_report_artifact(&controller.driver.workspace, &failed);
        }
        finish_workflow_controller(&state, &failed);
        return Err(ToolError::execution_failed(format!(
            "workflow cancellation journal failed: {err}"
        )));
    }
    state.reconcile_snapshot(&snapshot);
    // The VM may publish its terminal `run_completed` event while cancellation
    // is racing it. Always stream the authoritative cancellation afterward so
    // the live panel finalizes running rows and cannot remain visually failed.
    if let Some(controller) = controller {
        controller.driver.emit_ui_event(&cancelled_event);
        // The receipt links the report, so write it first; the VM's own
        // terminal path may rewrite it moments later.
        write_run_report_artifact(&controller.driver.workspace, &snapshot);
    }
    finish_workflow_controller(&state, &snapshot);
    workflow_result_for(run_id, state, owner_session_id)
}

fn workflow_fleet_name(input: &Value) -> Result<Option<String>, ToolError> {
    let named = match optional_str(input, "fleet")? {
        Some(name) => Some(name),
        None => match input.get("args") {
            Some(args) => optional_str(args, "fleet")?,
            None => None,
        },
    };
    Ok(named
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string))
}

/// How a Workflow run is bound to a named Fleet.
///
/// The two saved forms share one store and one `fleet: "<name>"` option. Legacy
/// role maps keep their exact previous behavior; an exact fleet is frozen into
/// an immutable Workflow snapshot at start and drives every task launch from
/// that snapshot.
#[derive(Debug, Clone, Default)]
enum WorkflowFleetBinding {
    #[default]
    None,
    Legacy {
        name: String,
        roles: FleetRoleMap,
    },
    Exact(Arc<crate::fleet::exact::ExactFleetWorkflow>),
}

impl WorkflowFleetBinding {
    fn name(&self) -> Option<String> {
        match self {
            Self::None => None,
            Self::Legacy { name, .. } => Some(name.clone()),
            Self::Exact(operation) => Some(operation.snapshot().fleet().qualified()),
        }
    }

    fn legacy_roles(&self) -> Option<&FleetRoleMap> {
        match self {
            Self::Legacy { roles, .. } => Some(roles),
            Self::None | Self::Exact(_) => None,
        }
    }

    fn exact(&self) -> Option<&Arc<crate::fleet::exact::ExactFleetWorkflow>> {
        match self {
            Self::Exact(operation) => Some(operation),
            Self::None | Self::Legacy { .. } => None,
        }
    }
}

// Reads Fleet/profile files; the runtime caller must use spawn_blocking.
fn workflow_fleet_binding(
    name: &str,
    workspace: &std::path::Path,
    api_config: Option<&crate::config::Config>,
) -> Result<WorkflowFleetBinding, ToolError> {
    let roots = crate::fleet::exact::fleet_search_roots(workspace);
    let (document, id) = crate::fleet::exact::load_fleet_document(name, workspace, api_config)
        .map_err(|err| {
            ToolError::invalid_input(format!(
                "Failed to load workflow Fleet '{name}' from {}: {err}",
                roots
                    .iter()
                    .map(|root| format!("{}/{}", root.origin, root.root.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;

    if let Some(legacy) = document.legacy() {
        let roles = FleetRoleMap::from_pairs(
            legacy
                .roles
                .iter()
                .map(|(role, profile)| (role.as_str(), profile.as_str())),
        )
        .map_err(|err| ToolError::invalid_input(err.to_string()))?;
        return Ok(WorkflowFleetBinding::Legacy {
            name: name.to_owned(),
            roles,
        });
    }

    // Exact: freeze the definition now. Everything the run launches afterwards
    // comes from this value, so editing the file mid-run cannot move a route.
    // The same labelled roots resolve the Fleet *and* the Reasoning Router
    // profile it references, so a Router is qualified (`workspace/luna-low`)
    // exactly the way a Fleet is and cannot be resolved by shadowing.
    let operation = crate::fleet::exact::ExactFleetWorkflow::capture(
        &document,
        id,
        chrono::Utc::now().to_rfc3339(),
        api_config,
        &roots,
    )
    .map_err(ToolError::invalid_input)?;
    Ok(WorkflowFleetBinding::Exact(Arc::new(operation)))
}

fn apply_named_fleet_to_task_request(
    fleet_roles: Option<&FleetRoleMap>,
    request: &mut TaskRequest,
) -> Result<(), DriverError> {
    let Some(fleet_roles) = fleet_roles else {
        return Ok(());
    };
    let resolved = resolve_workflow_agent(
        request.role.as_deref(),
        request.profile.as_deref(),
        fleet_roles,
        true,
    )
    .map_err(|err| DriverError::Rejected(err.to_string()))?;
    request.role = resolved.resolved_role.as_deref().map(public_role_label);
    request.profile = Some(resolved.resolved_profile);
    Ok(())
}

/// **Phase one** of an exact-Fleet task: resolve the member and stamp its
/// Runtime-derived authority onto the request, contacting nobody.
///
/// This runs *before* gate evaluation and before a concurrency slot is taken,
/// which is what makes it safe: a task that is about to be rejected or queued
/// must not have spent a router call or disclosed a summary to another
/// provider. Everything that can cost money lives in
/// [`route_admitted_exact_task`].
fn bind_exact_fleet_task_request(
    operation: &crate::fleet::exact::ExactFleetWorkflow,
    session: codewhale_workflow::PermissionCeiling,
    request: &mut TaskRequest,
) -> Result<crate::fleet::exact::ExactMemberBinding, DriverError> {
    let fleet = operation.snapshot().fleet().qualified();

    // The exact Fleet owns member identity, route, and reasoning. Runtime owns
    // authority: after selection it derives the closed role posture and
    // intersects it with the live parent. Tasks may narrow that authority, as
    // native read-only plans do, but cannot replace identity or widen posture.
    for (field, present) in [
        ("model", request.model.is_some()),
        ("model_strength", request.model_strength.is_some()),
        ("thinking", request.thinking.is_some()),
        ("subagent_type", request.subagent_type.is_some()),
    ] {
        if present {
            return Err(DriverError::Rejected(format!(
                "Fleet `{fleet}` is an exact Fleet: task option `{field}` is not allowed. Every \
                 member's identity, provider, model, and reasoning are fixed by the saved Fleet, \
                 while Runtime derives authority from its role and the live parent. Switch \
                 Fleets/edit the Fleet for identity or route changes; do not override either \
                 contract per task."
            )));
        }
    }

    let mut binding = operation
        .bind_member(request.profile.as_deref(), request.role.as_deref(), session)
        .map_err(|err| DriverError::Rejected(format!("Fleet `{fleet}`: {err}")))?;
    if request.write_authority.as_deref() == Some("worktree_write") && !request.worktree {
        return Err(DriverError::Rejected(format!(
            "Fleet `{fleet}`: worktree_write requires worktree isolation"
        )));
    }
    binding
        .narrow_for_task(
            request.write_authority.as_deref(),
            request.allowed_tools.as_deref(),
            &request.disallowed_tools,
            request.max_depth,
        )
        .map_err(|err| DriverError::Rejected(format!("Fleet `{fleet}`: {err}")))?;

    // Id and role are stamped **separately and semantically**. The member id
    // addresses the run-scoped roster profile projected from the snapshot,
    // which carries the exact provider pin and canonical wire model. The role
    // stays the Fleet's semantic role, because that is what gates, handoffs,
    // and records key on — overwriting it with the profile id (as an earlier
    // pass did) silently broke every gate whose member id differs from its
    // role.
    request.profile = Some(binding.member_id.clone());
    request.role = Some(binding.member_role.clone());

    // `subagent_type` is cleared rather than defaulted so the selected roster
    // profile's Runtime role is what picks the child posture.
    request.subagent_type = None;
    // The Runtime/parent intersection becomes an actual tool policy the child
    // enforces, not a Fleet identity label.
    request.allowed_tools = binding.authority.allowed_tools.clone();
    request.disallowed_tools = binding.authority.disallowed_tools.clone();
    request.write_authority = Some(binding.authority.write_authority.to_string());
    request.max_depth = Some(
        request
            .max_depth
            .map_or(binding.authority.max_depth, |asked| {
                asked.min(binding.authority.max_depth)
            }),
    );

    // A member that resolves read-only under the live posture cannot hold a
    // write claim, so any scope the script declared is dropped here rather
    // than refused: the script cannot predict every member's resolved
    // posture, and `validate_spawn_write_contract` would otherwise refuse the
    // task after the Router had been paid. A write-capable member with no
    // declared scope is left alone; the spawn boundary defaults it to the
    // workspace root exactly as it does for a plain Agent spawn.
    if binding.authority.write_authority == "read_only" {
        request.write_roots.clear();
        request.exact_files.clear();
        request.coordination_contracts.clear();
    }
    Ok(binding)
}

/// Which role a `task_started` event displays.
///
/// An exact-Fleet receipt wins over the spawn metadata, because the metadata's
/// role is the roster profile's **Runtime posture** — the closed role policy
/// selected after identity resolution — and rendering that where the member's role belongs
/// renames the operator's `auditor` to `scout` in the panel, the history card,
/// and the journal. The posture is not lost: it rides the same receipt in its
/// own field. Non-Fleet tasks keep the previous metadata-then-request order
/// exactly.
fn displayed_resolved_role(
    fleet_receipt: Option<&codewhale_workflow::FleetTaskReceipt>,
    metadata_role: Option<&str>,
    request_role: Option<&str>,
) -> Option<String> {
    fleet_receipt
        .map(|receipt| receipt.member_role.clone())
        .or_else(|| metadata_role.map(str::to_string))
        .or_else(|| request_role.map(str::to_string))
}

/// The visible line for a routing decision whose spawn then failed.
///
/// Kept separate from the recorder so the wording is testable without a live
/// driver, and so the receipt's own content-free `line()` stays the single
/// source of what a receipt may say.
fn orphaned_fleet_receipt_line(
    receipt: &codewhale_workflow::FleetTaskReceipt,
    error: &str,
) -> String {
    format!(
        "Fleet route {} spawn_failed=true reason={}",
        receipt.line(),
        error.replace('\n', " ")
    )
}

/// **Phase two**: route an already admitted task.
///
/// Only reachable once the task has passed its gates and holds a concurrency
/// slot, so this is the one place a reasoning router call — and any
/// cross-provider disclosure — can happen.
async fn route_admitted_exact_task(
    operation: &crate::fleet::exact::ExactFleetWorkflow,
    binding: &crate::fleet::exact::ExactMemberBinding,
    request: &mut TaskRequest,
) -> Result<codewhale_workflow::FleetTaskReceipt, DriverError> {
    let fleet = operation.snapshot().fleet().qualified();
    let launch = operation
        .route_admitted_task(binding, &request.description)
        .await
        .map_err(|err| DriverError::Rejected(format!("Fleet `{fleet}`: {err}")))?;

    request.thinking = Some(launch.thinking.clone());
    // **The launch authority is what the child runs under.** Binding stamped a
    // provisional copy so the write-scope contract could be checked for free;
    // this re-stamps from the value `route_admitted_task` recomputed and
    // verified, so the request that reaches the spawn boundary carries the
    // launched envelope and not an older one. Without this the launch's
    // `authority` was computed, put on a struct, and never read — a ceiling
    // that existed only as a field.
    apply_launch_authority(&fleet, &launch, request)?;
    Ok(launch.receipt)
}

/// Stamp the launched authority onto the request and refuse any drift.
///
/// Two things happen here and both are load-bearing. The envelope fields are
/// overwritten from `launch.authority`, so the spawn input is built from the
/// launched value rather than the admitted one. And `max_depth` is intersected
/// rather than replaced, because a task may legitimately ask for *less* nesting
/// than its ceiling allows — but never more.
fn apply_launch_authority(
    fleet: &str,
    launch: &crate::fleet::exact::ExactMemberLaunch,
    request: &mut TaskRequest,
) -> Result<(), DriverError> {
    let authority = &launch.authority;

    // Identity first: an envelope stamped onto the wrong member's request is a
    // widening as surely as a wider envelope would be.
    if request.profile.as_deref() != Some(launch.member_id.as_str())
        || request.role.as_deref() != Some(launch.member_role.as_str())
    {
        return Err(DriverError::Rejected(format!(
            "fleet `{fleet}`: task identity drifted between admission and launch (request \
             profile={:?} role={:?}, launch member `{}` role `{}`); the launch is refused rather \
             than run under an envelope resolved for a different member.",
            request.profile, request.role, launch.member_id, launch.member_role,
        )));
    }

    request.allowed_tools = authority.allowed_tools.clone();
    request.disallowed_tools = authority.disallowed_tools.clone();
    request.write_authority = Some(authority.write_authority.to_string());
    request.subagent_type = None;
    request.max_depth = Some(
        request
            .max_depth
            .map_or(authority.max_depth, |asked| asked.min(authority.max_depth)),
    );

    // The receipt records the fingerprint; the request now carries the envelope
    // it names. Recomputing the fingerprint from what was just stamped is the
    // check that the two describe each other — a mismatch here means a field
    // was added to the envelope and not to the stamping, which is exactly the
    // silent-gap failure this whole seam exists to prevent.
    let expected = authority.fingerprint();
    if launch.receipt.authority_fingerprint.as_deref() != Some(expected.as_str()) {
        return Err(DriverError::Rejected(format!(
            "fleet `{fleet}`: member `{}` produced a receipt whose authority fingerprint does not \
             match the envelope being installed (receipt={:?} envelope={expected}). Failing \
             closed.",
            launch.member_id, launch.receipt.authority_fingerprint,
        )));
    }
    Ok(())
}

// Pre-existing spawn signature that grew `vm_cancel` for the cancel-interrupt
// wiring; the args mirror one workflow run's context and are consumed once.
#[allow(clippy::too_many_arguments)]
async fn run_workflow_vm(
    run_id: String,
    source: String,
    spec: Option<WorkflowSpec>,
    args: Value,
    driver: Arc<SubAgentWorkflowDriver>,
    state: Arc<WorkflowWorkspaceState>,
    context: ToolContext,
    vm_cancel: WorkflowRunCancel,
) {
    let vm = WorkflowVm::new();
    let run = vm.run_script_with_cancel(&source, args, driver.clone(), vm_cancel.clone());
    tokio::pin!(run);
    // The VM closes its local driver during normal teardown too. Observe the
    // original caller token here, so that teardown cannot masquerade as a stop.
    let result = tokio::select! {
        biased;
        () = driver.parent_cancel_token.cancelled() => {
            vm_cancel.cancel();
            driver.force_cancel_all();
            // Dropping the run future signals its existing guard too. Do not
            // wait behind another VM's admission permit after an explicit stop.
            Err(codewhale_workflow_js::WorkflowJsError::Cancelled)
        }
        result = &mut run => result,
    };
    let mut status = WorkflowRunStatus::Completed;
    let mut output = None;
    let mut error = None;
    match result {
        Ok(value) => {
            if let Some(gate_error) = driver.terminal_gate_failure() {
                status = WorkflowRunStatus::Failed;
                error = Some(gate_error);
            } else {
                output = Some(value);
            }
        }
        Err(err) => {
            status = WorkflowRunStatus::Failed;
            error = Some(err.to_string());
        }
    }
    if driver.parent_cancel_token.is_cancelled() {
        driver.finalize_running_tasks_cancelled();
        status = WorkflowRunStatus::Cancelled;
        output = None;
        error = Some("Workflow cancelled by its parent session".to_string());
    }
    let snapshot = {
        let mut runs_guard = match state.runs.lock() {
            Ok(guard) => guard,
            Err(_) => {
                state.mark_owner_missing(&run_id);
                return;
            }
        };
        let Some(record) = runs_guard.get_mut(&run_id) else {
            state.mark_owner_missing(&run_id);
            return;
        };
        if record.status != WorkflowRunStatus::Cancelled {
            // Receipt honesty: a script that returns a value has not
            // necessarily orchestrated anything. Classify against the slot
            // ledger — every requested task either became a child with a
            // terminal record or landed in `dispatch_failures` (driver
            // rejections and, via `ProgressEvent::TaskRejected`, VM-level
            // rejections that previously vanished into null slots), and a
            // fan-out whose every slot failed — script throws included —
            // arrives via the dead-fan-out counter.
            if status == WorkflowRunStatus::Completed {
                let task_records = driver.task_records_snapshot();
                let ledger = SlotLedger {
                    tasks: task_records.len(),
                    failed_tasks: task_records
                        .iter()
                        .filter(|task| task.failed_for_ledger())
                        .count(),
                    rejected: record.dispatch_failure_count,
                    dead_fanouts: driver.dead_fanout_count(),
                    dropped_slots: driver.dropped_slot_count(),
                    any_child_ran: !record.child_ids.is_empty(),
                    retained_detail: record
                        .dispatch_failures
                        .first()
                        .map(|failure| failure.message.clone()),
                };
                if let Some((slot_status, slot_error)) = ledger.classify() {
                    status = slot_status;
                    error = Some(slot_error);
                }
            }
            record.status = status;
            record.result = output;
            record.error = error.clone();
            if status == WorkflowRunStatus::Cancelled {
                let cancelled = WorkflowUiEvent::new(
                    &driver.owner_session_id,
                    WorkflowUiEventKind::RunCancelled {
                        reason: error
                            .clone()
                            .unwrap_or_else(|| "Workflow cancelled".to_string()),
                    },
                );
                record.push_event(cancelled.clone());
                driver.emit_ui_event(&cancelled);
            }
            record.execution = spec.as_ref().map(|spec| {
                execution_from_declarative_spec(spec, driver.task_records_snapshot(), status)
            });
            record.completed_at_ms = Some(now_ms());
        }
        record.clone()
    };
    let verify_on_complete = state
        .runs
        .lock()
        .ok()
        .and_then(|guard| guard.get(&run_id).map(|record| record.verify_on_complete))
        .unwrap_or(false);
    if snapshot.status == WorkflowRunStatus::Completed && verify_on_complete {
        let verification = tokio::select! {
            biased;
            () = driver.parent_cancel_token.cancelled() => {
                if let Ok(mut runs_guard) = state.runs.lock()
                    && let Some(record) = runs_guard.get_mut(&run_id)
                {
                    record.status = WorkflowRunStatus::Cancelled;
                    record.error = Some("Workflow cancelled during completion verification".to_string());
                }
                None
            }
            verification = run_workflow_completion_gates(&context) => Some(verification),
        };
        match verification {
            Some(Ok(verification)) => {
                if let Ok(mut runs_guard) = state.runs.lock()
                    && let Some(record) = runs_guard.get_mut(&run_id)
                    && record.status != WorkflowRunStatus::Cancelled
                {
                    record.verification = Some(verification);
                }
            }
            Some(Err(err)) => {
                if let Ok(mut runs_guard) = state.runs.lock()
                    && let Some(record) = runs_guard.get_mut(&run_id)
                    && record.status != WorkflowRunStatus::Cancelled
                {
                    record.status = WorkflowRunStatus::Failed;
                    record.error = Some(format!("verification gates failed: {err}"));
                }
            }
            None => {}
        }
    }
    let final_budget = driver.current_budget_snapshot();
    // Reconcile run-wide usage totals from per-task telemetry (#2974).
    let run_usage = run_usage_totals(&driver.task_records_snapshot());
    let snapshot = state
        .runs
        .lock()
        .ok()
        .and_then(|mut guard| {
            let record = guard.get_mut(&run_id)?;
            if record.status != WorkflowRunStatus::Cancelled {
                record.lifecycle_seq = record.lifecycle_seq.saturating_add(1);
                if run_usage.is_some() {
                    record.usage = run_usage.clone();
                }
                let budget_event =
                    WorkflowUiEvent::new(&driver.owner_session_id, budget_event_kind(final_budget));
                let completed = WorkflowUiEvent::new(
                    &driver.owner_session_id,
                    WorkflowUiEventKind::RunCompleted {
                        status: record.status,
                        error: record.error.clone(),
                        usage: run_usage.clone(),
                        result_preview: record.result.as_ref().and_then(workflow_result_preview),
                    },
                );
                record.push_event(budget_event.clone());
                record.push_event(completed.clone());
                // Live stream terminal events even when recorded outside the
                // driver helper (completion path).
                driver.emit_ui_event(&budget_event);
                driver.emit_ui_event(&completed);
            }
            Some(record.clone())
        })
        .unwrap_or(snapshot);
    let mut terminal = snapshot;
    if let Err(err) = state.try_record_snapshot(&terminal) {
        // Status reads and the parent receipt must agree when the final
        // journal append fails. Keep completed child output as evidence and
        // preserve any cancellation recorded while the append was attempted.
        terminal = {
            let mut runs = state
                .runs
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let record = runs.entry(run_id.clone()).or_insert(terminal);
            if record.status != WorkflowRunStatus::Cancelled {
                record.status = WorkflowRunStatus::Failed;
            }
            let persistence_error =
                format!("workflow terminal journal snapshot could not be persisted: {err}");
            let error = match record.error.take() {
                Some(error) => format!("{persistence_error}; prior outcome: {error}"),
                None => persistence_error,
            };
            record.error = Some(error.clone());
            record.lifecycle_seq = record.lifecycle_seq.saturating_add(1);
            if let Some(execution) = record.execution.as_mut() {
                if record.status == WorkflowRunStatus::Cancelled {
                    execution.mark_cancelled();
                } else {
                    execution.mark_failed();
                }
            }
            let event = WorkflowUiEvent::new(
                &driver.owner_session_id,
                if record.status == WorkflowRunStatus::Cancelled {
                    WorkflowUiEventKind::RunCancelled { reason: error }
                } else {
                    WorkflowUiEventKind::RunCompleted {
                        status: record.status,
                        error: record.error.clone(),
                        usage: record.usage.clone(),
                        result_preview: record.result.as_ref().and_then(workflow_result_preview),
                    }
                },
            );
            record.push_event(event.clone());
            driver.emit_ui_event(&event);
            record.clone()
        };
        state.mark_owner_missing(&run_id);
    } else {
        state.reconcile_snapshot(&terminal);
    }
    write_run_report_artifact(&context.workspace, &terminal);
    finish_workflow_controller(&state, &terminal);
}

mod report;

use report::{
    bounded_raw_preview, write_run_report_artifact, write_schema_raw_artifact, written_run_report,
};

fn session_workflow_config_store()
-> &'static Mutex<HashMap<PathBuf, codewhale_config::WorkflowConfigToml>> {
    static STORE: OnceLock<Mutex<HashMap<PathBuf, codewhale_config::WorkflowConfigToml>>> =
        OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn workflow_session_key(workspace: &Path) -> PathBuf {
    workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
}

/// Install the session `[workflow]` table after a config.toml reload (or a
/// test mutation). `/workflow settings` and the workflow tool both read this
/// so a refresh cannot leave the two surfaces disagreeing.
pub(crate) fn set_session_workflow_config(
    workspace: &Path,
    config: codewhale_config::WorkflowConfigToml,
) {
    session_workflow_config_store()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(workflow_session_key(workspace), config);
}

/// The refreshed session `[workflow]` table, if a reload (or test) installed
/// one for this workspace.
pub(crate) fn session_workflow_config(
    workspace: &Path,
) -> Option<codewhale_config::WorkflowConfigToml> {
    session_workflow_config_store()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&workflow_session_key(workspace))
        .cloned()
}

/// The effective `[workflow]` table: the refreshed session table when one has
/// been installed, otherwise the runtime snapshot, otherwise product defaults.
fn workflow_config_for(runtime: &SubAgentRuntime) -> codewhale_config::WorkflowConfigToml {
    session_workflow_config(&runtime.context.workspace).unwrap_or_else(|| {
        runtime
            .api_config
            .as_deref()
            .map(crate::config::Config::workflow_config)
            .unwrap_or_default()
    })
}

fn workflow_result_for(
    run_id: &str,
    state: Arc<WorkflowWorkspaceState>,
    owner_session_id: &str,
) -> Result<ToolResult, ToolError> {
    let record = {
        let runs_guard = lock_mutex(&state.runs)?;
        runs_guard
            .get(run_id)
            .filter(|record| record.owner_session_id.as_deref() == Some(owner_session_id))
            .cloned()
            .ok_or_else(|| {
                ToolError::invalid_input(format!("Unknown workflow run_id '{run_id}'"))
            })?
    };
    let journal_path = state.journal_path().to_path_buf();
    let (payload, bounds) = bounded_run_record_value(&record, &journal_path);
    let mut result =
        ToolResult::json(&payload).map_err(|err| ToolError::execution_failed(err.to_string()))?;
    let summary = record.summary();
    result.metadata = Some(json!({
        "run_id": summary.run_id,
        "status": summary.status,
        "terminal": summary.status != WorkflowRunStatus::Running,
        "child_count": summary.child_count,
        "schema_error_count": summary.schema_error_count,
        "schema_repair_count": summary.schema_repair_count,
        "dispatch_failure_count": summary.dispatch_failure_count,
        "event_count": summary.event_count,
        "events_returned": bounds.events_returned,
        "events_omitted": bounds.events_omitted,
        "dispatch_failures_returned": bounds.dispatch_failures_returned,
        "dispatch_failures_omitted": bounds.dispatch_failures_omitted,
        "events_dropped": summary.events_dropped,
        "last_event_type": summary.last_event_type,
        "leaf_count": summary.leaf_count,
        "branch_count": summary.branch_count,
        "control_count": summary.control_count,
        "execution_status": summary.execution_status,
        "gate_count": summary.gate_count,
        "blocked_gate_count": summary.blocked_gate_count,
        "gate_status": summary.gate_status,
        // #2974: bounded payload; full detail stays in the durable journal.
        "truncated": bounds.truncated(),
        "payload_budget_chars": WORKFLOW_RESULT_MAX_CHARS,
        "journal_path": journal_path.display().to_string(),
        // #4126: durable plan-approval receipt for audit/receipt consumers.
        "plan_approval": record.plan_approval,
    }));
    Ok(result)
}

/// What `bounded_run_record_value` clipped out of the model-facing payload.
#[derive(Debug, Default)]
struct RunPayloadBounds {
    events_returned: usize,
    events_omitted: usize,
    progress_returned: usize,
    progress_omitted: u64,
    dispatch_failures_returned: usize,
    dispatch_failures_omitted: u64,
    dispatch_failure_fields_truncated: usize,
    result_truncated: bool,
    leaf_outputs_truncated: usize,
}

impl RunPayloadBounds {
    fn truncated(&self) -> bool {
        self.events_omitted > 0
            || self.progress_omitted > 0
            || self.dispatch_failures_omitted > 0
            || self.dispatch_failure_fields_truncated > 0
            || self.result_truncated
            || self.leaf_outputs_truncated > 0
    }
}

/// Build the model-facing view of a run record (#2974). The JSON shape is
/// identical to the full record (panel hydration and history cards keep
/// working unchanged), but the unbounded parts are clipped:
///
/// - `events`: newest `WORKFLOW_RESULT_EVENTS_TAIL` entries.
/// - `progress`: newest `WORKFLOW_RESULT_PROGRESS_TAIL` lines.
/// - `dispatch_failures`: newest `WORKFLOW_RESULT_DISPATCH_FAILURES_TAIL`
///   entries with bounded string fields.
/// - `result` / `verification`: collapsed to a preview + journal pointer
///   when the serialized value exceeds `WORKFLOW_RESULT_VALUE_MAX_CHARS`.
/// - `execution.leaf_results[*].output`: per-leaf preview capped at
///   `WORKFLOW_RESULT_LEAF_OUTPUT_MAX_CHARS`.
///
/// Full detail remains available in `.codewhale/workflow-runs.jsonl`; every
/// clip adds an explicit note/pointer so the model can fetch more on demand.
fn bounded_run_record_value(
    record: &WorkflowRunRecord,
    journal_path: &Path,
) -> (Value, RunPayloadBounds) {
    let mut bounds = RunPayloadBounds::default();
    let journal = journal_path.display().to_string();
    let mut value = serde_json::to_value(record).unwrap_or_else(|_| json!({}));
    let Some(obj) = value.as_object_mut() else {
        return (value, bounds);
    };

    // The structured failure tail below is intentionally clipped, but panel
    // summaries still need the exact saturating total to remain truthful
    // after replay.
    obj.insert(
        "dispatch_failure_count".to_string(),
        json!(record.dispatch_failure_count),
    );
    obj.insert("progress_count".to_string(), json!(record.progress_count));

    if let Some(events) = obj.get_mut("events").and_then(Value::as_array_mut) {
        if events.len() > WORKFLOW_RESULT_EVENTS_TAIL {
            let omitted = events.len() - WORKFLOW_RESULT_EVENTS_TAIL;
            events.drain(..omitted);
            bounds.events_omitted = omitted;
        }
        bounds.events_returned = events.len();
    }
    if bounds.events_omitted > 0 {
        obj.insert(
            "events_note".to_string(),
            json!(format!(
                "showing the newest {} of {} events; full stream: {journal}",
                bounds.events_returned,
                bounds.events_returned + bounds.events_omitted,
            )),
        );
    }

    if let Some(progress) = obj.get_mut("progress").and_then(Value::as_array_mut) {
        if progress.len() > WORKFLOW_RESULT_PROGRESS_TAIL {
            let omitted = progress.len() - WORKFLOW_RESULT_PROGRESS_TAIL;
            progress.drain(..omitted);
        }
        bounds.progress_returned = progress.len();
        let returned = u64::try_from(bounds.progress_returned).unwrap_or(u64::MAX);
        bounds.progress_omitted = record.progress_count.saturating_sub(returned);
    }
    if bounds.progress_omitted > 0 {
        obj.insert(
            "progress_note".to_string(),
            json!(format!(
                "showing the newest {} of {} progress lines; full log: {journal}",
                bounds.progress_returned, record.progress_count,
            )),
        );
    }

    if let Some(failures) = obj
        .get_mut("dispatch_failures")
        .and_then(Value::as_array_mut)
    {
        if failures.len() > WORKFLOW_RESULT_DISPATCH_FAILURES_TAIL {
            let omitted = failures.len() - WORKFLOW_RESULT_DISPATCH_FAILURES_TAIL;
            failures.drain(..omitted);
        }
        bounds.dispatch_failures_returned = failures.len();
        for failure in failures {
            let Some(fields) = failure.as_object_mut() else {
                continue;
            };
            for key in ["label", "phase", "message"] {
                let Some(slot) = fields.get_mut(key) else {
                    continue;
                };
                let Some(raw) = slot.as_str() else {
                    continue;
                };
                if raw.chars().count() > WORKFLOW_RESULT_DISPATCH_FAILURE_FIELD_MAX_CHARS {
                    *slot = Value::String(truncate_chars(
                        raw,
                        WORKFLOW_RESULT_DISPATCH_FAILURE_FIELD_MAX_CHARS,
                    ));
                    bounds.dispatch_failure_fields_truncated += 1;
                }
            }
        }
    }
    bounds.dispatch_failures_omitted = record
        .dispatch_failure_count
        .saturating_sub(u64::try_from(bounds.dispatch_failures_returned).unwrap_or(u64::MAX));
    if bounds.dispatch_failures_omitted > 0 || bounds.dispatch_failure_fields_truncated > 0 {
        obj.insert(
            "dispatch_failures_note".to_string(),
            json!(format!(
                "showing {} of {} dispatch failures with bounded fields; full record: {journal}",
                bounds.dispatch_failures_returned, record.dispatch_failure_count,
            )),
        );
    }

    for key in ["result", "verification"] {
        let Some(raw) = obj.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        let text = raw.to_string();
        if text.chars().count() > WORKFLOW_RESULT_VALUE_MAX_CHARS {
            obj.insert(
                key.to_string(),
                json!({
                    "truncated": true,
                    "omitted_chars": text.chars().count() - WORKFLOW_RESULT_VALUE_MAX_CHARS,
                    "preview": truncate_chars(&text, WORKFLOW_RESULT_VALUE_MAX_CHARS),
                    "full_detail": journal,
                }),
            );
            bounds.result_truncated = true;
        }
    }

    if let Some(leaves) = obj
        .get_mut("execution")
        .and_then(|execution| execution.get_mut("leaf_results"))
        .and_then(Value::as_array_mut)
    {
        for leaf in leaves {
            let too_long = leaf
                .get("output")
                .and_then(Value::as_str)
                .is_some_and(|output| {
                    output.chars().count() > WORKFLOW_RESULT_LEAF_OUTPUT_MAX_CHARS
                });
            if too_long
                && let Some(slot) = leaf.get_mut("output")
                && let Some(output) = slot.as_str()
            {
                let clipped = format!(
                    "{} [leaf output truncated to {WORKFLOW_RESULT_LEAF_OUTPUT_MAX_CHARS} chars; full text: {journal}]",
                    truncate_chars(output, WORKFLOW_RESULT_LEAF_OUTPUT_MAX_CHARS),
                );
                *slot = Value::String(clipped);
                bounds.leaf_outputs_truncated += 1;
            }
        }
    }

    (value, bounds)
}

/// One readable line for what a run returned: the text itself, an object's
/// own `summary`/`message`/`text`, or the slot names with the empty ones
/// named — never raw JSON and never a bare "N field(s)". Bounded.
pub(crate) fn workflow_result_preview(value: &Value) -> Option<String> {
    const MAX: usize = 200;
    let preview = match value {
        Value::Null => return None,
        Value::String(text) => text.split_whitespace().collect::<Vec<_>>().join(" "),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Array(items) => {
            let empty = items.iter().filter(|item| item.is_null()).count();
            let noun = if items.len() == 1 { "item" } else { "items" };
            if empty > 0 {
                format!("{} {noun}, {empty} empty", items.len())
            } else {
                format!("{} {noun}", items.len())
            }
        }
        Value::Object(map) => {
            if let Some(text) = ["summary", "message", "text"]
                .iter()
                .find_map(|key| map.get(*key).and_then(Value::as_str))
                .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|text| !text.is_empty())
            {
                text
            } else {
                let filled: Vec<&str> = map
                    .iter()
                    .filter(|(_, v)| !v.is_null())
                    .map(|(k, _)| k.as_str())
                    .collect();
                let empty: Vec<&str> = map
                    .iter()
                    .filter(|(_, v)| v.is_null())
                    .map(|(k, _)| k.as_str())
                    .collect();
                match (filled.is_empty(), empty.is_empty()) {
                    (true, true) => return None,
                    (false, true) => filled.join(", "),
                    (true, false) => format!("empty: {}", empty.join(", ")),
                    (false, false) => {
                        format!("{} · empty: {}", filled.join(", "), empty.join(", "))
                    }
                }
            }
        }
    };
    let preview = preview.trim();
    (!preview.is_empty()).then(|| truncate_chars(preview, MAX))
}

/// Char-boundary-safe truncation with an ellipsis (precedent:
/// `cargo_failure_summary::truncate_chars`).
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if let Some((idx, _)) = text.char_indices().nth(max_chars) {
        if max_chars < 3 {
            return text[..idx].to_string();
        }
        let truncate_at = text
            .char_indices()
            .nth(max_chars - 3)
            .map(|(idx, _)| idx)
            .unwrap_or(0);
        format!("{}...", &text[..truncate_at])
    } else {
        text.to_string()
    }
}

#[derive(Debug)]
struct WorkflowSource {
    source: String,
    path: Option<PathBuf>,
    spec: Option<WorkflowSpec>,
}

fn workflow_source(input: &Value, context: &ToolContext) -> Result<WorkflowSource, ToolError> {
    let script = match optional_str(input, "script")? {
        Some(script) => Some(script),
        None => optional_str(input, "source")?,
    }
    .map(str::to_string);
    let source_path = match optional_str(input, "source_path")? {
        Some(path) => Some(path),
        None => optional_str(input, "path")?,
    };
    let plan = input.get("plan").filter(|value| !value.is_null());

    let provided = [
        script.as_ref().is_some_and(|s| !s.trim().is_empty()),
        source_path.is_some(),
        plan.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if provided > 1 {
        return Err(ToolError::invalid_input(
            "Use exactly one of script, source_path, or plan",
        ));
    }

    match (script, source_path, plan) {
        (Some(source), None, None) if !source.trim().is_empty() => {
            workflow_source_from_raw(source, None, Some(&context.workspace))
        }
        (None, Some(path), None) => read_workflow_source_path(path, context),
        (None, None, Some(plan_value)) => {
            workflow_source_from_plan_in(plan_value, Some(&context.workspace))
        }
        _ => Err(ToolError::missing_field("script")),
    }
}

/// Planner-to-workflow structured launch path (#4124).
///
/// Accepts product-shaped plans (`goal` + `phases`/`children`) or IR-shaped
/// plans (`goal` + `nodes`), validates them, and lowers to imperative JS that
/// uses `parallel()` (partial success) rather than raw `Promise.all()`.
#[cfg(test)]
fn workflow_source_from_plan(plan_value: &Value) -> Result<WorkflowSource, ToolError> {
    workflow_source_from_plan_in(plan_value, None)
}

fn workflow_source_from_plan_in(
    plan_value: &Value,
    workspace: Option<&Path>,
) -> Result<WorkflowSource, ToolError> {
    let spec = structured_plan_to_workflow_spec(plan_value)?;
    let lowered = lower_declarative_workflow_to_imperative_js_in(&spec, workspace)?;
    Ok(WorkflowSource {
        source: lowered,
        path: None,
        spec: Some(spec),
    })
}

#[derive(Debug, Deserialize)]
struct StructuredWorkflowPlan {
    goal: String,
    #[serde(default)]
    risk: Option<String>,
    #[serde(default)]
    max_children: Option<usize>,
    #[serde(default)]
    token_budget: Option<u64>,
    #[serde(default)]
    phases: Vec<StructuredPlanPhase>,
    #[serde(default)]
    children: Vec<StructuredPlanChild>,
    /// Escape hatch: full Workflow IR nodes (kind/spec or JS authoring shapes).
    #[serde(default)]
    nodes: Option<Value>,
    /// Optional Workflow-owned gate specs (#4179).
    #[serde(default)]
    gates: Vec<GateSpec>,
}

#[derive(Debug, Deserialize)]
struct StructuredPlanPhase {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    parallel: Option<bool>,
    #[serde(default)]
    children: Vec<StructuredPlanChild>,
}

#[derive(Debug, Deserialize)]
struct StructuredPlanChild {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(alias = "description")]
    prompt: String,
    #[serde(default, alias = "type", alias = "agent_type")]
    agent_type: Option<String>,
    /// Fleet role name (#4177). Preferred step identity; resolved via roster.
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    /// Saved Fleet shortlist selector, resolved by the existing task binder.
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    file_scope: Vec<String>,
    /// Optional child working directory, repository-relative like
    /// `task({cwd})`. Disambiguates multi-repo workspaces (#6232).
    #[serde(default)]
    cwd: Option<String>,
}

fn structured_plan_to_workflow_spec(plan_value: &Value) -> Result<WorkflowSpec, ToolError> {
    if !plan_value.is_object() {
        return Err(ToolError::invalid_input(
            "Workflow plan must be a JSON object with goal and phases/children (or nodes)",
        ));
    }

    let plan: StructuredWorkflowPlan =
        serde_json::from_value(plan_value.clone()).map_err(|err| {
            ToolError::invalid_input(format!("Invalid structured Workflow plan: {err}"))
        })?;

    let goal = plan.goal.trim();
    if goal.is_empty() {
        return Err(ToolError::invalid_input(
            "Workflow plan goal must be a non-empty string",
        ));
    }

    // IR / declarative nodes escape hatch: re-parse as workflow({...}) object.
    if let Some(nodes) = plan.nodes.as_ref() {
        if !nodes.is_array() {
            return Err(ToolError::invalid_input(
                "Workflow plan.nodes must be an array of workflow nodes",
            ));
        }
        let mut object = plan_value.clone();
        if let Some(obj) = object.as_object_mut() {
            obj.insert("goal".to_string(), Value::String(goal.to_string()));
            if let Some(token_budget) = plan.token_budget {
                let mut budget = obj.get("budget").cloned().unwrap_or_else(|| json!({}));
                if let Some(budget_obj) = budget.as_object_mut() {
                    budget_obj.insert("max_tokens".to_string(), json!(token_budget));
                }
                obj.insert("budget".to_string(), budget);
            }
        }
        let wrapped = format!("workflow({});", object);
        return compile_javascript_workflow("<structured plan>", &wrapped).map_err(|err| {
            ToolError::invalid_input(format!("Invalid structured Workflow plan nodes: {err}"))
        });
    }

    let default_mode = plan_risk_to_mode(plan.risk.as_deref())?;
    let mut nodes = Vec::new();

    if !plan.phases.is_empty() {
        let mut prior_results = Vec::new();
        for (phase_index, phase) in plan.phases.iter().enumerate() {
            let phase_id = phase
                .id
                .as_deref()
                .or(phase.title.as_deref())
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("phase-{}", phase_index + 1));
            let mut children = plan_children_to_leaves(
                &phase.children,
                default_mode,
                plan.token_budget,
                &phase_id,
            )?;
            if children.is_empty() {
                return Err(ToolError::invalid_input(format!(
                    "Workflow plan phase '{phase_id}' must declare at least one child"
                )));
            }
            let parallel = phase.parallel.unwrap_or(children.len() > 1);
            // Phases are a data handoff as well as an ordering boundary.
            // Reuse the IR's dependency field so ordinary structured plans
            // get the same result forwarding as authored Workflow nodes.
            for child in &mut children {
                child.depends_on_results = prior_results.clone();
                if !parallel {
                    prior_results.push(child.id.clone());
                }
            }
            prior_results = children.iter().map(|child| child.id.clone()).collect();
            if parallel && children.len() > 1 {
                nodes.push(WorkflowNode::BranchSet(BranchSpec {
                    id: phase_id,
                    description: phase.title.clone(),
                    parallel: true,
                    budget: BudgetSpec {
                        max_tokens: plan.token_budget,
                        ..BudgetSpec::default()
                    },
                    permissions: Default::default(),
                    model_policy: Default::default(),
                    children: children.into_iter().map(WorkflowNode::Leaf).collect(),
                }));
            } else {
                // A one-child phase still owns a visible phase and task
                // grouping; flattening it loses the usual inspect/build/test
                // progression from the shared activity view.
                nodes.push(WorkflowNode::Sequence(SequenceSpec {
                    id: phase_id,
                    children: children.into_iter().map(WorkflowNode::Leaf).collect(),
                }));
            }
        }
    } else if !plan.children.is_empty() {
        let children =
            plan_children_to_leaves(&plan.children, default_mode, plan.token_budget, "plan")?;
        if children.len() == 1 {
            nodes.push(WorkflowNode::Leaf(
                children.into_iter().next().expect("one child"),
            ));
        } else {
            nodes.push(WorkflowNode::BranchSet(BranchSpec {
                id: "plan".to_string(),
                description: Some(goal.to_string()),
                parallel: true,
                budget: BudgetSpec {
                    max_tokens: plan.token_budget,
                    ..BudgetSpec::default()
                },
                permissions: Default::default(),
                model_policy: Default::default(),
                children: children.into_iter().map(WorkflowNode::Leaf).collect(),
            }));
        }
    } else {
        return Err(ToolError::invalid_input(
            "Workflow plan must include phases, children, or nodes",
        ));
    }

    let mut total_children = 0usize;
    count_plan_leaves(&nodes, &mut total_children);
    if let Some(max_children) = plan.max_children
        && total_children > max_children
    {
        return Err(ToolError::invalid_input(format!(
            "Workflow plan declares {total_children} children which exceeds max_children={max_children}"
        )));
    }

    let spec = WorkflowSpec {
        id: None,
        goal: goal.to_string(),
        description: plan.risk.clone(),
        budget: BudgetSpec {
            max_tokens: plan.token_budget,
            ..BudgetSpec::default()
        },
        permissions: Default::default(),
        model_policy: Default::default(),
        promotion_policy: Default::default(),
        gates: plan.gates,
        nodes,
    };
    spec.validate_for_fleet().map_err(|error| {
        ToolError::invalid_input(format!("Invalid structured Workflow plan: {error}"))
    })?;
    Ok(spec)
}

fn plan_risk_to_mode(risk: Option<&str>) -> Result<TaskMode, ToolError> {
    match risk.map(str::trim).filter(|s| !s.is_empty()) {
        None | Some("read_only") | Some("readonly") | Some("low") | Some("safe") => {
            Ok(TaskMode::ReadOnly)
        }
        Some("writes") | Some("write") | Some("read_write") | Some("readwrite")
        | Some("medium") => Ok(TaskMode::ReadWrite),
        Some("elevated") | Some("high") | Some("shell") | Some("network") => {
            // Elevated risk still launches as read_write; approval gates (#4126)
            // consume the risk string via plan description.
            Ok(TaskMode::ReadWrite)
        }
        Some(other) => Err(ToolError::invalid_input(format!(
            "Invalid plan risk '{other}'. Use read_only, writes, or elevated."
        ))),
    }
}

fn plan_children_to_leaves(
    children: &[StructuredPlanChild],
    default_mode: TaskMode,
    token_budget: Option<u64>,
    phase_id: &str,
) -> Result<Vec<LeafSpec>, ToolError> {
    if children.is_empty() {
        return Ok(Vec::new());
    }
    let mut leaves = Vec::with_capacity(children.len());
    for (index, child) in children.iter().enumerate() {
        let prompt = child.prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::invalid_input(format!(
                "Workflow plan child {} in phase '{phase_id}' must have a non-empty prompt",
                index + 1
            )));
        }
        let id = child
            .id
            .as_deref()
            .or(child.label.as_deref())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{phase_id}-child-{}", index + 1));
        let agent_type = parse_plan_agent_type(child.agent_type.as_deref())?;
        let mode = match child.mode.as_deref().map(str::trim) {
            None | Some("") => default_mode,
            Some("read_only") | Some("readonly") => TaskMode::ReadOnly,
            Some("read_write") | Some("readwrite") | Some("writes") | Some("write") => {
                TaskMode::ReadWrite
            }
            Some(other) => {
                return Err(ToolError::invalid_input(format!(
                    "Invalid plan child mode '{other}' on '{id}'. Use read_only or read_write."
                )));
            }
        };
        let role = child
            .role
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(|r| r.to_ascii_lowercase());
        let profile = child
            .profile
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(|p| p.to_ascii_lowercase());
        let model = child.model.as_deref().map(str::trim);
        if model == Some("") {
            return Err(ToolError::invalid_input(format!(
                "Workflow plan child '{id}' model must not be empty"
            )));
        }
        let cwd = child.cwd.as_deref().map(str::trim);
        if cwd == Some("") {
            return Err(ToolError::invalid_input(format!(
                "Workflow plan child '{id}' cwd must not be empty"
            )));
        }
        leaves.push(LeafSpec {
            id,
            prompt: prompt.to_string(),
            agent_type,
            role,
            profile,
            mode,
            isolation: Default::default(),
            file_scope: child.file_scope.clone(),
            cwd: cwd.map(str::to_string),
            depends_on_results: Vec::new(),
            budget: BudgetSpec {
                max_tokens: token_budget,
                ..BudgetSpec::default()
            },
            permissions: Default::default(),
            model_policy: codewhale_workflow::ModelPolicy {
                model: model.map(str::to_string),
                ..Default::default()
            },
        });
    }
    Ok(leaves)
}

/// Plan-child `type` vocabulary. Accepts the Agent tool's canonical types and
/// legacy aliases so the same option value works for direct Agent dispatch and
/// for Workflow plan children (#5035), normalized onto the workflow IR schema.
/// Rejections use the Agent tool's error contract ("Invalid sub-agent type
/// `'<value>'. Use: ...`") with field-specific guidance.
fn parse_plan_agent_type(raw: Option<&str>) -> Result<AgentType, ToolError> {
    let Some(kind) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(AgentType::General);
    };
    // `delegate` is a Workflow-only legacy alias. Every shared spelling uses
    // the same parser as direct sub-agent launches and Fleet configuration.
    let role = if kind.eq_ignore_ascii_case("delegate") {
        Some(FleetRole::Worker)
    } else {
        FleetRole::from_str(kind)
    };
    match role {
        Some(FleetRole::Worker) => Ok(AgentType::General),
        Some(FleetRole::Scout) => Ok(AgentType::Explore),
        Some(FleetRole::Planner) => Ok(AgentType::Plan),
        Some(FleetRole::Reviewer | FleetRole::Consultant) => Ok(AgentType::Review),
        Some(FleetRole::Builder) => Ok(AgentType::Implementer),
        Some(FleetRole::Verifier) => Ok(AgentType::Verifier),
        Some(FleetRole::Custom) => Err(ToolError::invalid_input(
            "Invalid sub-agent type 'custom' for a Workflow plan child: custom requires an \
             explicit allowed_tools list, which plan children cannot declare. Use role/profile \
             or another type.",
        )),
        None => Err(ToolError::invalid_input(format!(
            "Invalid sub-agent type '{kind}'. Use: worker, scout, planner, reviewer, builder, \
             verifier (legacy aliases remain accepted: general, explore/explorer, plan/awaiter, \
             review, implementer, consultant/oracle/advisor)."
        ))),
    }
}

/// Gate identity is runtime input, including for structured plans that do not
/// pass through the JavaScript author's normalization path.
fn validate_runtime_gates(gates: &mut [GateSpec]) -> Result<(), ToolError> {
    let mut ids = HashSet::new();
    for gate in gates {
        gate.id = gate.id.trim().to_string();
        if gate.id.is_empty() || !ids.insert(gate.id.clone()) {
            return Err(ToolError::invalid_input(
                "Workflow gates require non-empty, unique ids",
            ));
        }
        gate.role = gate.role.trim().to_lowercase();
        if gate.role.is_empty() {
            return Err(ToolError::invalid_input(
                "Workflow gate role must not be empty",
            ));
        }
        if let Some(role) = gate.blocks_role.as_mut() {
            *role = role.trim().to_lowercase();
            if role.is_empty() {
                return Err(ToolError::invalid_input(
                    "Workflow gate blocks_role must not be empty",
                ));
            }
        }
        if gate.on != GateOn::RoleComplete {
            return Err(ToolError::invalid_input(
                "Workflow role_start gates are not supported; use role_complete with an explicit prerequisite role",
            ));
        }
        if gate.blocks_role.as_deref() == Some(gate.role.as_str()) {
            return Err(ToolError::invalid_input(format!(
                "Workflow gate `{}` has role and blocks_role both set to `{}`; a gate cannot block \
                 the role whose completion triggers it, so set blocks_role to a later role",
                gate.id, gate.role
            )));
        }
    }
    Ok(())
}

fn count_plan_leaves(nodes: &[WorkflowNode], total: &mut usize) {
    for node in nodes {
        match node {
            WorkflowNode::Leaf(_) => *total += 1,
            WorkflowNode::BranchSet(spec) => count_plan_leaves(&spec.children, total),
            WorkflowNode::Sequence(spec) => count_plan_leaves(&spec.children, total),
            WorkflowNode::Reduce(_)
            | WorkflowNode::TeacherReview(_)
            | WorkflowNode::LoopUntil(_)
            | WorkflowNode::Cond(_)
            | WorkflowNode::Expand(_) => {}
        }
    }
}

fn read_workflow_source_path(
    path: &str,
    context: &ToolContext,
) -> Result<WorkflowSource, ToolError> {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        context.workspace.join(raw)
    };
    let canonical = joined.canonicalize().map_err(|err| {
        ToolError::invalid_input(format!(
            "Failed to resolve workflow source_path '{path}': {err}"
        ))
    })?;
    if !context.trust_mode {
        let workspace = context
            .workspace
            .canonicalize()
            .unwrap_or_else(|_| context.workspace.clone());
        // The user-global saved-workflow store is a first-class source
        // alongside the workspace: `~/.codewhale/workflows/*.workflow.js`
        // definitions surface as slash commands and must launch from any
        // workspace without trust_mode.
        let home_store = crate::config::effective_home_dir()
            .map(|home| home.join(".codewhale").join("workflows"))
            .and_then(|dir| dir.canonicalize().ok());
        let inside_home_store = home_store
            .as_deref()
            .is_some_and(|dir| canonical.starts_with(dir));
        if !canonical.starts_with(&workspace) && !inside_home_store {
            return Err(ToolError::permission_denied(format!(
                "workflow source_path must stay inside the workspace or ~/.codewhale/workflows: {}",
                canonical.display()
            )));
        }
    }
    let source = std::fs::read_to_string(&canonical).map_err(|err| {
        ToolError::execution_failed(format!(
            "Failed to read workflow source_path '{}': {err}",
            canonical.display()
        ))
    })?;
    workflow_source_from_raw(source, Some(canonical), Some(&context.workspace))
}

fn workflow_source_from_raw(
    source: String,
    path: Option<PathBuf>,
    workspace: Option<&Path>,
) -> Result<WorkflowSource, ToolError> {
    let adapted = adapt_workflow_source_in(&source, path.as_deref(), workspace)?;
    Ok(WorkflowSource {
        source: adapted.source,
        path,
        spec: adapted.spec,
    })
}

struct AdaptedWorkflowSource {
    source: String,
    spec: Option<WorkflowSpec>,
}

#[cfg(test)]
fn adapt_workflow_source(
    source: &str,
    path: Option<&Path>,
) -> Result<AdaptedWorkflowSource, ToolError> {
    adapt_workflow_source_in(source, path, None)
}

fn adapt_workflow_source_in(
    source: &str,
    path: Option<&Path>,
    workspace: Option<&Path>,
) -> Result<AdaptedWorkflowSource, ToolError> {
    if !looks_like_declarative_workflow(source) {
        return Ok(AdaptedWorkflowSource {
            source: source.to_string(),
            spec: None,
        });
    }

    let identifier = path
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<inline workflow>".to_string());
    let extension = path
        .and_then(Path::extension)
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    let spec = if extension.eq_ignore_ascii_case("ts") {
        compile_typescript_workflow(&identifier, source)
    } else {
        compile_javascript_workflow(&identifier, source)
    }
    .map_err(|err| {
        ToolError::invalid_input(format!(
            "Failed to compile declarative Workflow source '{identifier}': {err}"
        ))
    })?;

    let lowered = lower_declarative_workflow_to_imperative_js_in(&spec, workspace)?;
    Ok(AdaptedWorkflowSource {
        source: lowered,
        spec: Some(spec),
    })
}

fn looks_like_declarative_workflow(source: &str) -> bool {
    // Match a top-level `workflow(...)` / `export default workflow(...)` call on
    // any line, ignoring leading indentation, so an indented (non-column-0)
    // declarative call is still recognized rather than misrun as an imperative
    // script (#dogfood 0.8.67).
    source.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("workflow(") || trimmed.starts_with("export default workflow(")
    })
}

#[cfg(test)]
fn lower_declarative_workflow_to_imperative_js(spec: &WorkflowSpec) -> Result<String, ToolError> {
    lower_declarative_workflow_to_imperative_js_in(spec, None)
}

fn lower_declarative_workflow_to_imperative_js_in(
    spec: &WorkflowSpec,
    workspace: Option<&Path>,
) -> Result<String, ToolError> {
    let mut lowerer = DeclarativeWorkflowLowerer {
        workspace: workspace.map(Path::to_path_buf),
        ..DeclarativeWorkflowLowerer::default()
    };
    lowerer.line("\"use strict\";");
    lowerer.line("const __results = Object.create(null);");
    lowerer.line(format!(
        "phase({});",
        js_string(&format!("workflow: {}", spec.goal))
    ));
    for node in &spec.nodes {
        lowerer.lower_node(node, None)?;
    }
    lowerer.line("return __results;");
    Ok(lowerer.finish())
}

#[derive(Default)]
struct DeclarativeWorkflowLowerer {
    source: String,
    next_var: usize,
    /// Workspace root, so a leaf's absolute `cwd` inside it lowers to the
    /// repo-relative form `task()` accepts.
    workspace: Option<PathBuf>,
}

impl DeclarativeWorkflowLowerer {
    fn finish(self) -> String {
        self.source
    }

    fn line(&mut self, line: impl AsRef<str>) {
        self.source.push_str(line.as_ref());
        self.source.push('\n');
    }

    fn next_temp(&mut self, prefix: &str) -> String {
        let value = format!("__{prefix}_{}", self.next_var);
        self.next_var += 1;
        value
    }

    fn lower_node(&mut self, node: &WorkflowNode, phase: Option<&str>) -> Result<(), ToolError> {
        match node {
            WorkflowNode::Leaf(spec) => self.lower_leaf(spec, phase, /* parallel */ false),
            WorkflowNode::BranchSet(spec) => self.lower_branch(spec),
            WorkflowNode::Sequence(spec) => self.lower_sequence(spec),
            WorkflowNode::Reduce(spec) => self.lower_reduce(spec),
            WorkflowNode::TeacherReview(_) => Err(unsupported_declarative_node("teacher_review")),
            WorkflowNode::LoopUntil(_) => Err(unsupported_declarative_node("loop_until")),
            WorkflowNode::Cond(_) => Err(unsupported_declarative_node("cond")),
            WorkflowNode::Expand(_) => Err(unsupported_declarative_node("expand")),
        }
    }

    fn lower_leaf(
        &mut self,
        spec: &LeafSpec,
        phase: Option<&str>,
        parallel: bool,
    ) -> Result<(), ToolError> {
        self.line(format!(
            "__results[{}] = await task({});",
            js_string(&spec.id),
            leaf_task_options_expression(spec, phase, parallel, self.workspace.as_deref())?
        ));
        Ok(())
    }

    fn lower_branch(&mut self, spec: &BranchSpec) -> Result<(), ToolError> {
        self.line(format!("phase({});", js_string(&spec.id)));
        if spec.parallel {
            let mut leaves = Vec::new();
            for child in &spec.children {
                let WorkflowNode::Leaf(leaf) = child else {
                    return Err(ToolError::invalid_input(format!(
                        "Declarative Workflow adapter only supports leaf children inside parallel branch '{}'",
                        spec.id
                    )));
                };
                leaves.push(leaf);
            }
            // #4124: use Workflow `parallel()` (all-settled / partial success)
            // instead of raw Promise.all, which aborts siblings on first failure.
            let temp = self.next_temp("parallel");
            self.line(format!("const {temp} = await parallel(["));
            for leaf in &leaves {
                // Parallel write-capable children default to worktree isolation
                // (#4120) unless the plan explicitly sets isolation: shared.
                self.line(format!(
                    "  () => task({}),",
                    leaf_task_options_expression(
                        leaf,
                        Some(&spec.id),
                        /* parallel */ true,
                        self.workspace.as_deref(),
                    )?
                ));
            }
            self.line("]);");
            for (index, leaf) in leaves.iter().enumerate() {
                self.line(format!(
                    "__results[{}] = {temp}[{index}];",
                    js_string(&leaf.id)
                ));
            }
            return Ok(());
        }

        for child in &spec.children {
            self.lower_node(child, Some(&spec.id))?;
        }
        Ok(())
    }

    fn lower_sequence(&mut self, spec: &SequenceSpec) -> Result<(), ToolError> {
        self.line(format!("phase({});", js_string(&spec.id)));
        for child in &spec.children {
            self.lower_node(child, Some(&spec.id))?;
        }
        Ok(())
    }

    fn lower_reduce(&mut self, spec: &ReduceSpec) -> Result<(), ToolError> {
        let inputs = result_inputs_expression(&spec.inputs);
        self.line(format!(
            "__results[{}] = await task({});",
            js_string(&spec.id),
            task_options_expression(
                format!(
                    "{} + \"\\n\\nInputs:\\n\" + {inputs}",
                    js_string(&spec.prompt)
                ),
                Some("plan"),
                None,
                None,
                spec.model_policy.model.as_deref(),
                false,
                None,
                None,
                None,
                Some("read_only"),
                &[],
                &spec.id,
                Some("reduce"),
                None,
                None,
            )
        ));
        Ok(())
    }
}

fn unsupported_declarative_node(kind: &str) -> ToolError {
    ToolError::invalid_input(format!(
        "Declarative Workflow adapter does not yet support {kind} nodes"
    ))
}

fn leaf_description(spec: &LeafSpec) -> String {
    let mut description = spec.prompt.trim().to_string();
    let mut metadata = Vec::new();
    metadata.push(format!("Workflow leaf id: {}", spec.id));
    metadata.push(format!("Mode: {}", task_mode_name(spec.mode)));
    if !spec.file_scope.is_empty() {
        metadata.push(format!("File scope: {}", spec.file_scope.join(", ")));
    }
    if !spec.depends_on_results.is_empty() {
        metadata.push(format!(
            "Depends on results: {}",
            spec.depends_on_results.join(", ")
        ));
    }
    if spec.budget != BudgetSpec::default() {
        let mut budget = Vec::new();
        if let Some(max_steps) = spec.budget.max_steps {
            budget.push(format!("max_steps={max_steps}"));
        }
        if let Some(timeout_secs) = spec.budget.timeout_secs {
            budget.push(format!("timeout_secs={timeout_secs}"));
        }
        if let Some(max_parallel) = spec.budget.max_parallel {
            budget.push(format!("max_parallel={max_parallel}"));
        }
        if let Some(max_tokens) = spec.budget.max_tokens {
            budget.push(format!("max_tokens={max_tokens}"));
        }
        if !budget.is_empty() {
            metadata.push(format!("Budget: {}", budget.join(", ")));
        }
    }
    if !metadata.is_empty() {
        description.push_str("\n\nWorkflow metadata:\n");
        for item in metadata {
            description.push_str("- ");
            description.push_str(&item);
            description.push('\n');
        }
    }
    description
}

fn leaf_task_options_expression(
    spec: &LeafSpec,
    phase: Option<&str>,
    parallel: bool,
    workspace: Option<&Path>,
) -> Result<String, ToolError> {
    validate_leaf_runtime_contract(spec)?;
    // Reject invalid plans before accepting a background run, using the same
    // path policy as direct task() dispatch rather than a second validator.
    let cwd = spec
        .cwd
        .as_deref()
        .map(|cwd| codewhale_workflow_js::normalize_task_cwd_in(cwd, workspace))
        .transpose()
        .map_err(|error| {
            ToolError::invalid_input(format!("Workflow leaf '{}': {error}", spec.id))
        })?;
    let worktree = leaf_wants_worktree(spec, parallel);
    let write_authority = match spec.mode {
        TaskMode::ReadOnly => "read_only",
        TaskMode::ReadWrite if worktree => "worktree_write",
        TaskMode::ReadWrite => "workspace_write",
    };
    let write_roots = if spec.mode == TaskMode::ReadWrite {
        spec.file_scope
            .iter()
            .map(|scope| codewhale_workflow::normalize_file_scope_root(scope))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    Ok(task_options_expression(
        leaf_description_expression(spec),
        leaf_subagent_type(spec),
        spec.role.as_deref(),
        spec.profile.as_deref(),
        spec.model_policy.model.as_deref(),
        // Parallel write-capable children default to worktree isolation (#4120).
        // Explicit isolation: shared is the approved same-worktree override.
        worktree,
        spec.budget.max_tokens,
        spec.budget.max_steps,
        spec.budget.timeout_secs,
        Some(write_authority),
        &write_roots,
        &spec.id,
        phase,
        leaf_allowed_tools(spec)?,
        cwd.as_deref(),
    ))
}

fn validate_leaf_runtime_contract(spec: &LeafSpec) -> Result<(), ToolError> {
    if spec.mode == TaskMode::ReadOnly && spec.permissions.allow_write {
        return Err(ToolError::invalid_input(format!(
            "Workflow leaf '{}' is read_only but requests allow_write permissions",
            spec.id
        )));
    }
    if spec.mode == TaskMode::ReadWrite && spec.file_scope.is_empty() {
        return Err(ToolError::invalid_input(format!(
            "Workflow leaf '{}' is read_write but declares no file_scope for its bounded write claim",
            spec.id
        )));
    }
    for scope in &spec.file_scope {
        let normalized = codewhale_workflow::normalize_file_scope_root(scope);
        if normalized.is_empty() || normalized.contains('*') {
            return Err(ToolError::invalid_input(format!(
                "Workflow leaf '{}' has unsupported file_scope '{}'; use a concrete path or a trailing /* or /** directory scope",
                spec.id, scope
            )));
        }
    }
    // A Fleet role and its authority posture are independent. In particular,
    // acceptance workflows must be able to resolve the `implementer` role to
    // its saved profile while narrowing that child to the read-only tool set.
    // `leaf_allowed_tools` enforces the mode below; rejecting the combination
    // made verification-only role/gate dogfood impossible.
    if spec.mode == TaskMode::ReadWrite
        && matches!(
            spec.agent_type,
            AgentType::Explore | AgentType::Plan | AgentType::Review | AgentType::Verifier
        )
    {
        return Err(ToolError::invalid_input(format!(
            "Workflow leaf '{}' is read_write but uses read-only agent_type {}",
            spec.id,
            agent_type_name(spec.agent_type)
        )));
    }
    if spec.mode == TaskMode::ReadOnly
        && spec
            .permissions
            .allowed_tools
            .iter()
            .any(|tool| is_write_or_shell_tool(tool))
    {
        return Err(ToolError::invalid_input(format!(
            "Workflow leaf '{}' is read_only but requests write/shell allowed_tools",
            spec.id
        )));
    }
    if spec.permissions.deny_all_tools && !spec.permissions.allowed_tools.is_empty() {
        return Err(ToolError::invalid_input(format!(
            "Workflow leaf '{}' cannot combine deny_all_tools with allowed_tools",
            spec.id
        )));
    }
    Ok(())
}

fn leaf_description_expression(spec: &LeafSpec) -> String {
    let description = js_string(&leaf_description(spec));
    if spec.depends_on_results.is_empty() {
        return description;
    }
    let inputs = result_inputs_expression(&spec.depends_on_results);
    // parallel() retains null for a failed slot. A dependent worker must
    // not start with an empty stand-in for that result; reducers can still
    // inspect partial fan-out through their separate input projection.
    let required =
        serde_json::to_string(&spec.depends_on_results).expect("dependency IDs serialize to JSON");
    format!(
        "({required}.forEach(id => {{ if (__results[id] == null) throw new Error('Required Workflow result unavailable: ' + id); }}), {description} + \"\\n\\nInputs:\\n\" + {inputs})"
    )
}

fn result_inputs_expression(inputs: &[String]) -> String {
    let entries = inputs
        .iter()
        .map(|input| format!("[{}, __results[{}]]", js_string(input), js_string(input)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "[{entries}].map(([id, value]) => \"--- \" + id + \" ---\\n\" + String(value ?? \"\")).join(\"\\n\\n\")"
    )
}

fn leaf_subagent_type(spec: &LeafSpec) -> Option<&'static str> {
    // A named Fleet profile owns the child's runtime type. Emitting the IR's
    // default `general` here makes role-only leaves look like an explicit type
    // override and can conflict with the resolved roster member (for example,
    // scout -> explore). Preserve non-General types because those represent an
    // authored override and the spawn path must still validate compatibility.
    if (spec.role.is_some() || spec.profile.is_some()) && spec.agent_type == AgentType::General {
        return None;
    }
    if spec.mode == TaskMode::ReadOnly && spec.agent_type == AgentType::General {
        return Some("review");
    }
    // A read_only leaf must not *name* a write-capable type. `type` is a claim
    // about what the child can do, and claiming it while the leaf narrows the
    // child to read-only tools is the contradiction #5123 asks the spawn path
    // to reject. The leaf's role/profile already carries the identity roster
    // resolution needs, so drop the redundant type and let the role speak —
    // this is the `implementer` role narrowed to verification-only work that
    // `validate_leaf_runtime_contract` deliberately allows.
    if spec.mode == TaskMode::ReadOnly
        && spec.agent_type == AgentType::Implementer
        && (spec.role.is_some() || spec.profile.is_some())
    {
        return None;
    }
    Some(agent_type_name(spec.agent_type))
}

fn leaf_allowed_tools(spec: &LeafSpec) -> Result<Option<Vec<String>>, ToolError> {
    if spec.permissions.deny_all_tools {
        return Ok(Some(Vec::new()));
    }
    if !spec.permissions.allowed_tools.is_empty() {
        return Ok(Some(spec.permissions.allowed_tools.clone()));
    }
    // The child grant already intersects role, parent permissions and the
    // emitted writeAuthority. A second File-only default hid bounded Git/CI
    // inspection from scouts and Run from verifiers without adding safety.
    // Explicit allowlists and deny_all_tools above remain exact restrictions.
    Ok(None)
}

fn is_write_or_shell_tool(tool: &str) -> bool {
    // One list, owned by the workflow crate. This used to be a second copy
    // that drifted from `elevation.rs`'s — see `codewhale_workflow::is_write_tool`.
    codewhale_workflow::is_write_tool(tool) || codewhale_workflow::is_shell_tool(tool)
}

// Pre-existing builder that grew `allowed_tools`; each arg maps 1:1 onto one
// optional field of the generated JS options literal.
#[allow(clippy::too_many_arguments)]
fn task_options_expression(
    description_expr: String,
    subagent_type: Option<&str>,
    role: Option<&str>,
    profile: Option<&str>,
    model: Option<&str>,
    worktree: bool,
    token_budget: Option<u64>,
    max_steps: Option<u32>,
    wall_time_secs: Option<u64>,
    write_authority: Option<&str>,
    write_roots: &[String],
    label: &str,
    phase: Option<&str>,
    allowed_tools: Option<Vec<String>>,
    cwd: Option<&str>,
) -> String {
    let mut fields = vec![format!("description: {description_expr}")];
    if let Some(subagent_type) = subagent_type {
        fields.push(format!("type: {}", js_string(subagent_type)));
    }
    fields.push(format!("label: {}", js_string(label)));
    if let Some(phase) = phase {
        fields.push(format!("phase: {}", js_string(phase)));
    }
    if let Some(cwd) = cwd {
        fields.push(format!("cwd: {}", js_string(cwd)));
    }
    if let Some(role) = role {
        fields.push(format!("role: {}", js_string(role)));
    }
    if let Some(profile) = profile {
        fields.push(format!("profile: {}", js_string(profile)));
    }
    if let Some(model) = model {
        fields.push(format!("model: {}", js_string(model)));
    }
    if worktree {
        fields.push("worktree: true".to_string());
    }
    if let Some(token_budget) = token_budget {
        fields.push(format!("tokenBudget: {token_budget}"));
    }
    // Workflow's saved budget uses zero for no additional step cap. Omit
    // that sentinel at the Agent boundary, where an explicit cap must be
    // positive and may only narrow inherited authority.
    if let Some(max_steps) = max_steps.filter(|steps| *steps > 0) {
        fields.push(format!("maxSteps: {max_steps}"));
    }
    if let Some(wall_time_secs) = wall_time_secs {
        fields.push(format!("wallTimeSecs: {wall_time_secs}"));
    }
    if let Some(write_authority) = write_authority {
        fields.push(format!("writeAuthority: {}", js_string(write_authority)));
    }
    if !write_roots.is_empty() {
        fields.push(format!(
            "writeRoots: {}",
            serde_json::to_string(write_roots).expect("serializing write roots cannot fail")
        ));
    }
    if let Some(allowed_tools) = allowed_tools {
        fields.push(format!(
            "allowedTools: {}",
            serde_json::to_string(&allowed_tools).expect("serializing tool names cannot fail")
        ));
    }
    format!("{{ {} }}", fields.join(", "))
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing JS string cannot fail")
}

fn agent_type_name(agent_type: AgentType) -> &'static str {
    match agent_type {
        AgentType::General => "general",
        AgentType::Explore => "explore",
        AgentType::Plan => "plan",
        AgentType::Review => "review",
        AgentType::Implementer => "implementer",
        AgentType::Verifier => "verifier",
    }
}

fn task_mode_name(mode: TaskMode) -> &'static str {
    match mode {
        TaskMode::ReadOnly => "read_only",
        TaskMode::ReadWrite => "read_write",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExplicitGateVerdict {
    Approve,
    Reject,
}

/// Recognize only a standalone verdict token on the first non-empty line.
///
/// This deliberately does not interpret prose, Markdown bullets, or verdict
/// words later in an otherwise successful child response. Existing workflows
/// whose children return ordinary prose therefore remain pass-on-success,
/// while review-style children can fail closed with `BLOCK` or `FAIL`.
fn explicit_gate_verdict(output: Option<&str>) -> Option<ExplicitGateVerdict> {
    let first_meaningful = output?
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    if first_meaningful.eq_ignore_ascii_case("APPROVE")
        || first_meaningful.eq_ignore_ascii_case("PASS")
    {
        Some(ExplicitGateVerdict::Approve)
    } else if first_meaningful.eq_ignore_ascii_case("BLOCK")
        || first_meaningful.eq_ignore_ascii_case("FAIL")
    {
        Some(ExplicitGateVerdict::Reject)
    } else {
        None
    }
}

fn has_gate_artifact_body(output: Option<&str>) -> bool {
    let Some(output) = output else {
        return false;
    };
    let mut meaningful_lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    // A declared artifact needs both a body label and at least one concrete
    // entry after the verdict. This keeps `APPROVE\nok` from promoting a
    // placeholder while remaining format-agnostic for arbitrary artifact kinds.
    meaningful_lines.next();
    meaningful_lines.next().is_some() && meaningful_lines.next().is_some()
}

fn gate_outcome_for_completed_role(
    record: &RuntimeTaskRecord,
    require_explicit_verdict: bool,
    artifact_kind: Option<&str>,
) -> GateOutcome {
    match record.status {
        IrWorkflowRunStatus::Succeeded => match explicit_gate_verdict(record.output.as_deref()) {
            Some(ExplicitGateVerdict::Reject) => GateOutcome::Fail {
                reason: record
                    .output
                    .clone()
                    .unwrap_or_else(|| "child returned an explicit rejection verdict".into()),
            },
            Some(ExplicitGateVerdict::Approve)
                if require_explicit_verdict
                    && artifact_kind.is_some()
                    && !has_gate_artifact_body(record.output.as_deref()) =>
            {
                GateOutcome::Fail {
                    reason: format!(
                        "task {} approved without the required {} artifact body",
                        record.agent_id,
                        artifact_kind.unwrap_or("gate")
                    ),
                }
            }
            Some(ExplicitGateVerdict::Approve) => GateOutcome::Pass,
            None if require_explicit_verdict => GateOutcome::Fail {
                reason: format!(
                    "task {} completed without the required first-line gate verdict; expected exactly APPROVE, PASS, BLOCK, or FAIL",
                    record.agent_id
                ),
            },
            None => GateOutcome::Pass,
        },
        _ => GateOutcome::Fail {
            reason: record.output.clone().unwrap_or_else(|| {
                format!("task {} ended as {:?}", record.agent_id, record.status)
            }),
        },
    }
}

#[derive(Debug, Clone)]
struct RuntimeTaskRecord {
    agent_id: String,
    label: Option<String>,
    role: Option<String>,
    status: IrWorkflowRunStatus,
    output: Option<String>,
    schema_error: Option<String>,
    usage: Option<WorkflowTaskUsage>,
}

impl RuntimeTaskRecord {
    /// Whether this record ended in a terminal state that produced no usable
    /// result — the states the slot ledger counts as a failed task.
    ///
    /// `BudgetExceeded` is the named gap this exists to close: a child that
    /// dies of budget exhaustion is a failed task for the all-failed rule,
    /// not an invisible one. `Cancelled` is
    /// deliberately excluded: it is the run's own stop, not lost work, and
    /// run-level cancellation is finalized before this ledger is consulted.
    fn failed_for_ledger(&self) -> bool {
        matches!(
            self.status,
            IrWorkflowRunStatus::Failed | IrWorkflowRunStatus::BudgetExceeded
        )
    }
}

/// Bound on the workflow completion pump inbox (#6147): one completion per
/// terminated workflow child, drained by the pump task.
const WORKFLOW_COMPLETION_CHANNEL_CAPACITY: usize = 64;

struct SubAgentWorkflowDriver {
    run_id: String,
    owner_session_id: String,
    manager: SharedSubAgentManager,
    runtime: SubAgentRuntime,
    parent_cancel_token: CancellationToken,
    state: Arc<WorkflowWorkspaceState>,
    completion_tx: mpsc::Sender<SubAgentCompletion>,
    completion_state: Arc<Mutex<CompletionState>>,
    child_ids: Arc<Mutex<Vec<String>>>,
    /// Monotonic 0-based child admission counter for `workflow_child_index`.
    child_counter: AtomicU32,
    /// Tickets for `task_queued` events, so a start can name the wait it ends.
    queue_tickets: AtomicU64,
    max_children: u32,
    /// Latest `phase(...)` title observed on this run (used when a task omits
    /// an explicit `phase` option).
    current_phase: Mutex<Option<String>>,
    task_records: Arc<Mutex<HashMap<String, RuntimeTaskRecord>>>,
    /// Fan-outs that resolved with every slot failed (`ProgressEvent::FanoutAllSlotsFailed`),
    /// script throws included. Consulted by the terminal slot ledger: those
    /// fan-outs can leave no task record at all.
    dead_fanouts: AtomicU32,
    dropped_slots: AtomicU32,
    total_budget: Option<u64>,
    last_budget_event: Arc<Mutex<Option<BudgetSnapshot>>>,
    /// Workflow-owned gates installed for this run (#4179).
    gate_specs: Arc<Vec<GateSpec>>,
    /// Lane-scoped gate and handoff state keyed by run id.
    gate_board: Arc<Mutex<LaneGateBoard>>,
    /// Caps concurrently live `task()` children for this run (product: 16).
    concurrent_gate: Arc<Semaphore>,
    /// Held permits for in-flight children; released on completion/cancel.
    spawn_permits: Mutex<HashMap<String, OwnedSemaphorePermit>>,
    /// Optional named Fleet roster for resolving Workflow task roles (#4177/#4178).
    fleet_name: Option<String>,
    /// The Fleet this Workflow is bound to, frozen at start. For an exact
    /// fleet this holds the immutable snapshot every task launch reads from,
    /// which is why editing `fleets/<name>.toml` mid-run cannot move a route.
    fleet: WorkflowFleetBinding,
    /// Workspace root, for durable `responseSchema` raw-output artifacts
    /// written beside the run report (#5583).
    workspace: PathBuf,
}

/// A single-consumer handoff remains reserved during admission. If routing,
/// spawning, or cancellation drops that admission, the same board retains the
/// evidence for the next attempt.
struct WorkflowHandoffReservation {
    board: Arc<Mutex<LaneGateBoard>>,
    artifacts: Vec<HandoffArtifact>,
}

impl WorkflowHandoffReservation {
    fn commit(mut self) -> Vec<HandoffArtifact> {
        std::mem::take(&mut self.artifacts)
    }
}

impl Drop for WorkflowHandoffReservation {
    fn drop(&mut self) {
        if self.artifacts.is_empty() {
            return;
        }
        let mut board = self
            .board
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // Consumption is newest-first; restore original chronological order.
        for artifact in self.artifacts.drain(..).rev() {
            board.artifacts.push(artifact);
        }
    }
}

impl SubAgentWorkflowDriver {
    #[allow(clippy::too_many_arguments)]
    fn new(
        run_id: String,
        owner_session_id: String,
        manager: SharedSubAgentManager,
        mut runtime: SubAgentRuntime,
        state: Arc<WorkflowWorkspaceState>,
        total_budget: Option<u64>,
        fleet: WorkflowFleetBinding,
        gate_specs: Vec<GateSpec>,
        workspace: PathBuf,
    ) -> Arc<Self> {
        let fleet_name = fleet.name();
        let workflow_cfg = workflow_config_for(&runtime);
        let parent_cancel_token = runtime.cancel_token.clone();
        // Workflow cancellation owns only this token subtree, never the
        // caller's token or its unrelated direct children.
        runtime.cancel_token = runtime.cancel_token.child_token();
        runtime.context.cancel_token = Some(runtime.cancel_token.clone());
        let (completion_tx, completion_rx) = mpsc::channel(WORKFLOW_COMPLETION_CHANNEL_CAPACITY);
        let mut gate_board = LaneGateBoard::new(run_id.clone());
        gate_board.install_gates(&gate_specs);
        let driver = Arc::new(Self {
            run_id,
            owner_session_id,
            manager,
            runtime,
            parent_cancel_token,
            state,
            completion_tx,
            completion_state: Arc::new(Mutex::new(CompletionState::default())),
            child_ids: Arc::new(Mutex::new(Vec::new())),
            child_counter: AtomicU32::new(0),
            queue_tickets: AtomicU64::new(0),
            max_children: workflow_cfg.max_children.min(WORKFLOW_LIFETIME_CAP as u32),
            current_phase: Mutex::new(None),
            task_records: Arc::new(Mutex::new(HashMap::new())),
            dead_fanouts: AtomicU32::new(0),
            dropped_slots: AtomicU32::new(0),
            total_budget,
            last_budget_event: Arc::new(Mutex::new(None)),
            gate_specs: Arc::new(gate_specs),
            gate_board: Arc::new(Mutex::new(gate_board)),
            concurrent_gate: Arc::new(Semaphore::new(
                (workflow_cfg.max_concurrent as usize).min(WORKFLOW_MAX_CONCURRENT),
            )),
            spawn_permits: Mutex::new(HashMap::new()),
            fleet_name,
            fleet,
            workspace,
        });
        spawn_completion_pump(driver.clone(), completion_rx);
        driver
    }

    fn force_cancel_all(&self) {
        self.concurrent_gate.close();
        self.runtime.cancel_token.cancel();
        let ids = self
            .child_ids
            .lock()
            .map(|ids| ids.clone())
            .unwrap_or_default();
        if let Ok(mut permits) = self.spawn_permits.lock() {
            permits.clear();
        }
        cancel_child_agents(self.manager.clone(), ids);
        if let Ok(mut state) = self.completion_state.lock() {
            for (_, waiter) in state.waiters.drain() {
                let _ = waiter.send(TaskCompletion::Cancelled);
            }
        }
    }

    fn ensure_admission_open(&self) -> Result<(), DriverError> {
        if self.concurrent_gate.is_closed() || self.runtime.cancel_token.is_cancelled() {
            Err(DriverError::Rejected("workflow admission cancelled".into()))
        } else {
            Ok(())
        }
    }

    fn finalize_running_tasks_cancelled(&self) {
        let ids = self
            .child_ids
            .lock()
            .map(|ids| ids.clone())
            .unwrap_or_default();
        for id in &ids {
            self.record_task_completion(id, &TaskCompletion::Cancelled, None);
        }
    }

    fn record_child(&self, agent_id: &str) {
        if let Ok(mut ids) = self.child_ids.lock()
            && !ids.iter().any(|id| id == agent_id)
        {
            ids.push(agent_id.to_string());
        }
        if let Ok(mut runs) = self.state.runs.lock()
            && let Some(record) = runs.get_mut(&self.run_id)
            && !record.child_ids.iter().any(|id| id == agent_id)
        {
            record.child_ids.push(agent_id.to_string());
        }
    }

    fn current_budget_snapshot(&self) -> BudgetSnapshot {
        // Token-budget enforcement was removed (#6189): nothing stops a run.
        // The snapshot survives for the declared spec ceiling only; spent is
        // no longer tracked per scope.
        BudgetSnapshot {
            total: self.total_budget,
            spent: 0,
        }
    }

    /// Return the first authoritative gate failure after the VM has no more
    /// children to admit. Intermediate blocks already reject the downstream
    /// spawn; this final check gives a terminal role's BLOCK verdict the same
    /// fail-closed semantics.
    fn terminal_gate_failure(&self) -> Option<String> {
        let board = match self.gate_board.lock() {
            Ok(board) => board,
            Err(_) => {
                return Some(
                    "workflow gate board was unavailable during terminal finalization".to_string(),
                );
            }
        };
        self.gate_specs.iter().find_map(|spec| {
            let state = board.gates.get(&spec.id).unwrap_or(&GateState::Pending);
            (!matches!(state, GateState::Passed)).then(|| {
                format!(
                    "workflow gate `{}` ended {}: {}",
                    spec.id,
                    state.as_str(),
                    gate_state_reason(state)
                )
            })
        })
    }

    fn record_run_event(&self, event: WorkflowUiEvent) {
        let recorded = if let Ok(mut runs) = self.state.runs.lock()
            && let Some(record) = runs.get_mut(&self.run_id)
        {
            // A terminal run's event list is the cancel/complete receipt.
            // Racing VM and child completions must not append after that.
            if record.status != WorkflowRunStatus::Running {
                return;
            }
            record.push_event(event.clone());
            true
        } else {
            false
        };
        if recorded {
            self.state.record_event(&self.run_id, &event);
        }
        // #4122: stream typed events live into the panel + history card.
        self.emit_ui_event(&event);
    }

    /// Publish a flattened WorkflowUiEvent on the engine event bus so the TUI
    /// can hydrate the panel while the tool is still running.
    fn emit_ui_event(&self, event: &WorkflowUiEvent) {
        let Some(tx) = self.runtime.event_tx.as_ref() else {
            return;
        };
        let Ok(mut value) = serde_json::to_value(event) else {
            return;
        };
        if let Some(obj) = value.as_object_mut() {
            obj.insert("run_id".to_string(), json!(self.run_id));
        }
        let ui_event = Event::WorkflowUi {
            owner_session_id: self.owner_session_id.clone(),
            run_id: self.run_id.clone(),
            event: value,
        };
        // A lost terminal event leaves a row or a whole run looking live
        // forever; those wait for capacity. Progress stays lossy.
        if matches!(
            event.kind,
            WorkflowUiEventKind::RunCompleted { .. }
                | WorkflowUiEventKind::RunCancelled { .. }
                | WorkflowUiEventKind::TaskCompleted { .. }
        ) {
            send_terminal_event(tx, ui_event);
        } else {
            let _ = tx.try_send(ui_event);
        }
    }

    fn record_budget_snapshot(&self, snapshot: BudgetSnapshot) {
        let changed = if let Ok(mut last) = self.last_budget_event.lock() {
            if last.as_ref() == Some(&snapshot) {
                false
            } else {
                *last = Some(snapshot);
                true
            }
        } else {
            false
        };
        let event = WorkflowUiEvent::new(&self.owner_session_id, budget_event_kind(snapshot));
        if changed {
            self.record_run_event(event);
        } else {
            // The VM polls the budget before it can admit its first child.
            // Keep that live path warm even when no token value changed, but
            // do not journal an unbounded stream of identical snapshots.
            self.emit_ui_event(&event);
        }
    }

    fn prepare_request_for_gates(
        &self,
        request: &mut TaskRequest,
    ) -> Result<Vec<HandoffArtifact>, DriverError> {
        let Some(role) = request.role.as_deref().filter(|role| !role.is_empty()) else {
            return Ok(Vec::new());
        };
        if self.gate_specs.is_empty() {
            return Ok(Vec::new());
        }

        let (blocked, handoffs) = {
            let mut board = self
                .gate_board
                .lock()
                .map_err(|_| DriverError::Rejected("workflow gate board lock poisoned".into()))?;
            let blocked = board.role_is_blocked(&self.gate_specs, role).cloned();
            // Handoffs are consumed (removed from the board) as they are
            // delivered — but only when the role is actually admitted. A
            // blocked task must leave them in place for the retry after the
            // gate clears.
            let handoffs = if blocked.is_none() {
                board.consume_handoffs_for(role, 4)
            } else {
                Vec::new()
            };
            (blocked, handoffs)
        };

        if let Some(state) = blocked {
            return Err(DriverError::Rejected(format!(
                "workflow gate blocks role `{role}`: {}",
                gate_state_reason(&state)
            )));
        }

        if !handoffs.is_empty() {
            append_handoff_context(request, &handoffs);
        }
        Ok(handoffs)
    }

    fn update_gate_status(&self, status: Vec<GateStatusLine>) {
        let snapshot = if let Ok(mut runs) = self.state.runs.lock()
            && let Some(record) = runs.get_mut(&self.run_id)
        {
            record.gate_status = status;
            Some(record.clone())
        } else {
            None
        };
        if let Some(record) = snapshot {
            self.state.record_snapshot(&record);
        }
    }

    fn evaluate_gates_for_completed_role(&self, record: &RuntimeTaskRecord) {
        let Some(role) = record.role.as_deref().filter(|role| !role.is_empty()) else {
            return;
        };
        if self.gate_specs.is_empty() {
            return;
        }
        let specs = self
            .gate_specs
            .iter()
            .filter(|spec| spec.on == GateOn::RoleComplete && spec.role.eq_ignore_ascii_case(role))
            .cloned()
            .collect::<Vec<_>>();
        if specs.is_empty() {
            return;
        }

        let mut events = Vec::new();
        let mut next_status = Vec::new();
        if let Ok(mut board) = self.gate_board.lock() {
            for spec in specs {
                let outcome = gate_outcome_for_completed_role(
                    record,
                    spec.require_explicit_verdict,
                    spec.artifact_kind.as_deref(),
                );
                let mut state = match board.evaluate(&spec, outcome.clone()) {
                    Ok(state) => state,
                    Err(err) => {
                        let state = GateState::Blocked {
                            reason: err.to_string(),
                        };
                        // Evaluation errors must become authoritative board state.
                        // Otherwise the emitted receipt can say `blocked` while the
                        // admission check still sees the gate as pending.
                        board.gates.insert(spec.id.clone(), state.clone());
                        state
                    }
                };
                let mut promotion = None;
                if matches!(state, GateState::Passed)
                    && let (Some(kind), Some(to_role)) =
                        (spec.artifact_kind.as_deref(), spec.blocks_role.as_deref())
                {
                    let artifact = HandoffArtifact {
                        // Gate ids are authored input and are not guaranteed unique.
                        // Use an opaque id so every promotion has a stable, distinct
                        // identity even when a malformed workflow repeats a gate id.
                        id: format!("handoff_{}", Uuid::new_v4()),
                        lane_id: self.run_id.clone(),
                        from_role: spec.role.clone(),
                        to_role: to_role.to_string(),
                        kind: kind.to_string(),
                        payload: record.output.clone().unwrap_or_default(),
                        created_at: now_ms().to_string(),
                    };
                    match board.record_handoff(artifact.clone()) {
                        Ok(()) => {
                            promotion = Some(WorkflowUiEvent::new(
                                &self.owner_session_id,
                                WorkflowUiEventKind::HandoffPromoted {
                                    artifact_id: artifact.id,
                                    gate_id: spec.id.clone(),
                                    kind: artifact.kind,
                                    from_role: artifact.from_role,
                                    to_role: artifact.to_role,
                                    producer_task_id: record.agent_id.clone(),
                                },
                            ));
                        }
                        Err(err) => {
                            state = GateState::Blocked {
                                reason: format!(
                                    "gate passed but its handoff could not be recorded: {err}"
                                ),
                            };
                            board.gates.insert(spec.id.clone(), state.clone());
                        }
                    }
                }
                events.push(WorkflowUiEvent::new(
                    &self.owner_session_id,
                    WorkflowUiEventKind::GateUpdated {
                        gate_id: spec.id.clone(),
                        role: spec.role.clone(),
                        gate: gate_kind_label(spec.gate).to_string(),
                        state: state.as_str().to_string(),
                        blocked_role: spec.blocks_role.clone(),
                        blocked_reason: state.blocked_reason().map(str::to_string),
                    },
                ));
                if let Some(event) = promotion {
                    events.push(event);
                }
            }
            next_status = board.status_summary();
        }
        if !events.is_empty() || !next_status.is_empty() {
            self.update_gate_status(next_status);
        }
        for event in events {
            self.record_run_event(event);
        }
    }

    fn record_task_started(
        &self,
        agent_id: &str,
        request: &TaskRequest,
        metadata: &WorkflowTaskSpawnMetadata,
        result: &crate::tools::subagent::SubAgentResult,
        fleet_receipt: Option<codewhale_workflow::FleetTaskReceipt>,
        queue_ticket: Option<u64>,
    ) {
        // Prefer typed spawn metadata over request fields so panel/history never
        // need to re-derive labels from the child prompt (#4119).
        let label = metadata
            .workflow_task_label
            .clone()
            .or_else(|| request.label.clone());
        self.record_run_event(WorkflowUiEvent::new(
            &self.owner_session_id,
            WorkflowUiEventKind::TaskStarted(Box::new(WorkflowTaskStartedEvent {
                task_id: agent_id.to_string(),
                label,
                role: request.role.as_deref().map(public_role_label),
                profile: request.profile.clone(),
                model: request.model.clone(),
                strength: request.model_strength.clone(),
                thinking: request.thinking.clone(),
                // #4039: both sides of the reasoning receipt come from the
                // spawn metadata the runtime minted, never from the request or
                // from current session config.
                requested_reasoning: metadata
                    .requested_reasoning
                    .clone()
                    .or_else(|| request.thinking.clone()),
                effective_reasoning: metadata.effective_reasoning.clone(),
                // Prefer spawn metadata (fleet-resolved); fall back to request.
                //
                // An exact-Fleet receipt overrides both, because the spawn
                // metadata's role is the roster profile's **Runtime posture** —
                // the closed role policy selected after member identity — and displaying
                // that where the member's role belongs silently renames the
                // operator's `auditor` to `scout`. The posture is not lost: it
                // rides the receipt as its own field.
                resolved_role: displayed_resolved_role(
                    fleet_receipt.as_ref(),
                    metadata.resolved_role.as_deref(),
                    request.role.as_deref(),
                ),
                resolved_profile: metadata
                    .resolved_profile
                    .clone()
                    .or_else(|| request.profile.clone()),
                resolved_provider: metadata.resolved_provider.clone(),
                resolved_model: metadata.resolved_model.clone(),
                route_source: metadata.route_source.clone(),
                child_route: Some(metadata.child_route.clone()),
                worktree: request.worktree,
                workspace: result.workspace.clone(),
                git_branch: result.git_branch.clone(),
                parent_task_id: metadata.parent_task_id.clone(),
                depth: metadata.depth,
                workflow_run_id: metadata.workflow_run_id.clone(),
                workflow_phase_id: metadata.workflow_phase_id.clone(),
                workflow_task_label: metadata.workflow_task_label.clone(),
                workflow_child_index: metadata.workflow_child_index,
                queue_ticket,
                fleet_receipt: fleet_receipt.clone(),
            })),
        ));
        // Also surface the decision as a run log line, so the receipt is
        // *visible* in the panel and transcript rather than only structured on
        // an event a UI has to know to unpack.
        if let Some(receipt) = fleet_receipt {
            self.record_run_event(WorkflowUiEvent::new(
                &self.owner_session_id,
                WorkflowUiEventKind::Log {
                    message: format!("Fleet route {}", receipt.line()),
                },
            ));
        }
    }

    /// Preserve a routing receipt whose task never became a child.
    ///
    /// A receipt normally rides the `task_started` event, which a failed spawn
    /// never emits. Recording it here keeps the run's history complete: the
    /// decision happened, the tokens were spent, and — if the Router ran on
    /// another provider — a bounded summary already left the host. Silence
    /// would make all three unrecoverable.
    fn record_orphaned_fleet_receipt(
        &self,
        receipt: &codewhale_workflow::FleetTaskReceipt,
        error: &str,
    ) {
        self.record_run_event(WorkflowUiEvent::new(
            &self.owner_session_id,
            WorkflowUiEventKind::Log {
                message: orphaned_fleet_receipt_line(receipt, error),
            },
        ));
    }

    fn record_task_request(&self, agent_id: &str, request: &TaskRequest) {
        if let Ok(mut records) = self.task_records.lock() {
            records
                .entry(agent_id.to_string())
                .or_insert_with(|| RuntimeTaskRecord {
                    agent_id: agent_id.to_string(),
                    label: request.label.clone(),
                    role: request.role.clone(),
                    status: IrWorkflowRunStatus::Running,
                    output: None,
                    schema_error: None,
                    usage: None,
                });
        }
        let pending_completion = self
            .completion_state
            .lock()
            .ok()
            .and_then(|state| state.pending.get(agent_id).cloned());
        if let Some(completion) = pending_completion {
            self.record_task_completion(agent_id, &completion.completion, completion.usage);
        } else if self.runtime.cancel_token.is_cancelled() {
            self.record_task_completion(agent_id, &TaskCompletion::Cancelled, None);
        }
    }

    fn record_task_completion(
        &self,
        agent_id: &str,
        completion: &TaskCompletion,
        usage: Option<WorkflowTaskUsage>,
    ) {
        let mut terminal_event = None;
        let mut completed_record = None;
        if let Ok(mut records) = self.task_records.lock()
            && let Some(record) = records.get_mut(agent_id)
        {
            if usage.is_some() {
                record.usage = usage;
            }
            if record.status == IrWorkflowRunStatus::Running {
                let (status, output) = task_completion_status(completion);
                let (reason, kind) = view::task_stop(completion);
                record.status = status;
                record.output = output;
                terminal_event = Some(WorkflowUiEvent::new(
                    &self.owner_session_id,
                    WorkflowUiEventKind::TaskCompleted {
                        task_id: agent_id.to_string(),
                        status,
                        reason,
                        kind,
                        usage: record.usage.clone(),
                    },
                ));
                completed_record = Some(record.clone());
            }
        }
        if let Some(event) = terminal_event {
            self.record_run_event(event);
        }
        if let Some(record) = completed_record.as_ref() {
            // A role-complete gate is caused by this terminal transition, so its
            // durable task receipt must precede gate evaluation and promotion.
            self.evaluate_gates_for_completed_role(record);
        }
    }

    /// A rejected dispatch never produces a child agent, and inside
    /// `parallel()` the JS throw collapses to a `null` slot; this ledger keeps
    /// the rejection visible on the run record and result payload (#5035).
    fn record_dispatch_failure(
        &self,
        label: Option<String>,
        phase: Option<String>,
        message: String,
        queue_ticket: Option<u64>,
    ) {
        let failure = WorkflowDispatchFailure {
            at_ms: now_ms(),
            label,
            phase,
            message,
        };
        let slot = failure
            .label
            .as_deref()
            .or(failure.phase.as_deref())
            .unwrap_or("task");
        let progress_line = format!("dispatch failed for {slot}: {}", failure.message);
        let ui_event = WorkflowUiEvent::new(
            &self.owner_session_id,
            WorkflowUiEventKind::TaskDispatchFailed {
                label: failure.label.clone(),
                phase: failure.phase.clone(),
                message: failure.message.clone(),
                queue_ticket,
            },
        );
        if let Ok(mut runs) = self.state.runs.lock()
            && let Some(record) = runs.get_mut(&self.run_id)
        {
            if record.status != WorkflowRunStatus::Running {
                return;
            }
            record.push_progress(progress_line.clone());
            record.push_event(ui_event.clone());
            record.push_dispatch_failure(failure);
        }
        self.state.record_progress(&self.run_id, &progress_line);
        self.state.record_event(&self.run_id, &ui_event);
        self.emit_ui_event(&ui_event);
    }

    fn record_schema_validation_failure(&self, agent_id: &str, message: String) {
        if let Ok(mut records) = self.task_records.lock()
            && let Some(record) = records.get_mut(agent_id)
        {
            record.status = IrWorkflowRunStatus::Failed;
            record.schema_error = Some(message.clone());
            record.output = Some(message);
        }
    }

    /// Bounded preview + durable artifact for a failed `responseSchema`
    /// attempt's raw reply (#5583). The record keeps a preview; the full
    /// text goes to a `.txt` artifact beside the run report when it is
    /// larger than the preview, so the journal stays small while the
    /// evidence stays durable.
    fn schema_raw_receipt(
        &self,
        task_id: &str,
        attempt: u32,
        raw: &str,
    ) -> (String, Option<String>) {
        let preview = bounded_raw_preview(raw);
        let artifact = if raw.chars().count() > SCHEMA_RAW_PREVIEW_CHARS {
            write_schema_raw_artifact(&self.workspace, &self.run_id, task_id, attempt, raw)
        } else {
            None
        };
        (preview, artifact)
    }

    fn task_records_snapshot(&self) -> Vec<RuntimeTaskRecord> {
        self.task_records
            .lock()
            .map(|records| records.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Count a fan-out that resolved with every slot failed. The prelude's
    /// run-log breadcrumb already names it for operators; this counter is
    /// what lets the terminal slot ledger act on it.
    fn record_dead_fanout(&self) {
        self.dead_fanouts.fetch_add(1, Ordering::SeqCst);
    }

    fn dead_fanout_count(&self) -> u32 {
        self.dead_fanouts.load(Ordering::SeqCst)
    }

    /// Count one slot a settled fan-out dropped while others survived; the
    /// per-slot breadcrumb already narrates it, this counter makes the loss
    /// visible to the terminal slot ledger.
    fn record_dropped_slot(&self) {
        self.dropped_slots.fetch_add(1, Ordering::SeqCst);
    }

    fn dropped_slot_count(&self) -> u32 {
        self.dropped_slots.load(Ordering::SeqCst)
    }

    fn add_waiter_or_complete(&self, agent_id: String, waiter: oneshot::Sender<TaskCompletion>) {
        let mut state = self
            .completion_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(completion) = state.pending.remove(&agent_id) {
            drop(state);
            // The child may have completed before its permit was registered.
            // Consume that receipt and release the late-installed permit too.
            if let Ok(mut permits) = self.spawn_permits.lock() {
                permits.remove(&agent_id);
            }
            // Its receipt may also have arrived just after record_task_request
            // checked pending state. Replaying here closes that same race for
            // the durable task status; terminal transitions are idempotent.
            self.record_task_completion(&agent_id, &completion.completion, completion.usage);
            let _ = waiter.send(completion.completion);
        } else if self.runtime.cancel_token.is_cancelled() {
            if let Ok(mut permits) = self.spawn_permits.lock() {
                permits.remove(&agent_id);
            }
            let _ = waiter.send(TaskCompletion::Cancelled);
        } else {
            state.waiters.insert(agent_id, waiter);
        }
    }

    fn deliver_completion(
        &self,
        agent_id: String,
        completion: TaskCompletion,
        usage: Option<WorkflowTaskUsage>,
    ) {
        self.record_task_completion(&agent_id, &completion, usage.clone());
        if let Ok(mut permits) = self.spawn_permits.lock() {
            permits.remove(&agent_id);
        }
        let mut state = self
            .completion_state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(waiter) = state.waiters.remove(&agent_id) {
            let _ = waiter.send(completion);
        } else {
            state
                .pending
                .insert(agent_id, PendingCompletion { completion, usage });
        }
    }
}

#[derive(Clone)]
struct PendingCompletion {
    completion: TaskCompletion,
    usage: Option<WorkflowTaskUsage>,
}

#[derive(Default)]
struct CompletionState {
    waiters: HashMap<String, oneshot::Sender<TaskCompletion>>,
    pending: HashMap<String, PendingCompletion>,
}

impl SubAgentWorkflowDriver {
    /// The admission half of [`WorkflowDriver::spawn_task`]; every `Err` it
    /// returns is recorded as a dispatch failure by the trait wrapper.
    async fn spawn_task_admitted(
        &self,
        mut request: TaskRequest,
        queue_ticket: &mut Option<u64>,
    ) -> Result<SpawnedTask, DriverError> {
        self.ensure_admission_open()?;
        // Exact fleets resolve from the frozen snapshot; legacy role maps keep
        // their previous path unchanged.
        //
        // The exact path is deliberately split in two. **Binding** resolves the
        // member, its frozen route, and its clamped authority, and contacts
        // nobody. **Routing** — the half that may call the fleet's reasoning
        // router, spend the operator's tokens, and disclose a bounded summary
        // to another provider — happens only after this task has passed its
        // gates and holds a concurrency slot. A task that is rejected or
        // capacity-blocked therefore costs nothing and reveals nothing.
        let exact_binding = if let Some(operation) = self.fleet.exact() {
            // The depth budget is the other failure the spawn boundary can be
            // predicted to raise, and it does not depend on the member. Check
            // it here so an over-deep task is refused for free rather than
            // after a routing request has already been paid for.
            if self.runtime.would_exceed_depth() {
                return Err(DriverError::Rejected(format!(
                    "fleet `{}`: sub-agent depth limit reached (depth {}, max {}); this task \
                     cannot spawn a child, so it is refused before the reasoning router is asked \
                     anything.",
                    operation.snapshot().fleet().qualified(),
                    self.runtime.spawn_depth,
                    self.runtime.max_spawn_depth,
                )));
            }
            Some(bind_exact_fleet_task_request(
                operation,
                crate::tools::subagent::session_permission_ceiling(&self.runtime),
                &mut request,
            )?)
        } else {
            apply_named_fleet_to_task_request(self.fleet.legacy_roles(), &mut request).map_err(
                |err| {
                    if let Some(fleet) = self.fleet_name.as_deref() {
                        DriverError::Rejected(format!(
                            "Fleet `{fleet}` role resolution failed: {err}"
                        ))
                    } else {
                        err
                    }
                },
            )?;
            None
        };
        // Take a concurrent slot (max 16 live children per run). A task that
        // has to wait is announced first, so a surface can show it queued
        // instead of leaving it invisible until a slot frees.
        let permit = match self.concurrent_gate.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(tokio::sync::TryAcquireError::Closed) => {
                return Err(DriverError::Rejected(
                    "workflow concurrent admission closed".into(),
                ));
            }
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                let ticket = self.queue_tickets.fetch_add(1, Ordering::SeqCst);
                *queue_ticket = Some(ticket);
                self.record_run_event(WorkflowUiEvent::new(
                    &self.owner_session_id,
                    WorkflowUiEventKind::TaskQueued {
                        ticket,
                        label: request
                            .label
                            .as_deref()
                            .map(str::trim)
                            .filter(|label| !label.is_empty())
                            .map(str::to_string),
                        phase: request
                            .phase
                            .as_deref()
                            .map(str::trim)
                            .filter(|phase| !phase.is_empty())
                            .map(str::to_string)
                            .or_else(|| {
                                self.current_phase
                                    .lock()
                                    .ok()
                                    .and_then(|phase| phase.clone())
                            }),
                    },
                ));
                self.concurrent_gate
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| {
                        DriverError::Rejected("workflow concurrent admission closed".into())
                    })?
            }
        };
        self.ensure_admission_open()?;
        // A plain compare-exchange loop: `fetch_update` is deprecated from
        // Rust 1.99 and its `try_update` replacement is newer than the MSRV.
        let mut count = self.child_counter.load(Ordering::SeqCst);
        let workflow_child_index = loop {
            if count >= self.max_children {
                return Err(DriverError::Rejected(format!(
                    "workflow.max_children limit ({}) reached; reduce remaining work or start a new reviewed plan",
                    self.max_children
                )));
            }
            match self.child_counter.compare_exchange_weak(
                count,
                count + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(reserved) => break reserved,
                Err(current) => count = current,
            }
        };
        let handoffs = WorkflowHandoffReservation {
            board: self.gate_board.clone(),
            artifacts: self.prepare_request_for_gates(&mut request)?,
        };

        // Admitted. Only now may the reasoning router be consulted.
        let fleet_receipt = match (self.fleet.exact(), exact_binding.as_ref()) {
            (Some(operation), Some(binding)) => {
                match route_admitted_exact_task(operation, binding, &mut request).await {
                    Ok(receipt) => Some(receipt),
                    Err(err) => {
                        drop(permit);
                        return Err(err);
                    }
                }
            }
            _ => None,
        };
        if let Err(err) = self.ensure_admission_open() {
            if let Some(receipt) = &fleet_receipt {
                self.record_orphaned_fleet_receipt(receipt, &err.to_string());
            }
            return Err(err);
        }

        let runtime = self
            .runtime
            .clone()
            .with_parent_completion_tx(self.completion_tx.clone());
        let request_record = request.clone();
        let workflow_phase_id = request
            .phase
            .as_ref()
            .map(|phase| phase.trim())
            .filter(|phase| !phase.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.current_phase
                    .lock()
                    .ok()
                    .and_then(|phase| phase.clone())
            });
        let workflow_task_label = request
            .label
            .as_ref()
            .map(|label| label.trim())
            .filter(|label| !label.is_empty())
            .map(str::to_string);
        let identity = WorkflowTaskSpawnIdentity {
            workflow_run_id: self.run_id.clone(),
            workflow_phase_id,
            workflow_task_label,
            workflow_child_index,
            exact_fleet_binding: exact_binding.clone(),
            // The Fleet decision travels to the spawn boundary as a value that
            // boundary re-checks, rather than as trust in the caller.
            fleet_authority_fingerprint: fleet_receipt
                .as_ref()
                .and_then(|receipt| receipt.authority_fingerprint.clone()),
        };
        let result =
            match spawn_workflow_task(request, self.manager.clone(), runtime, identity).await {
                Ok(result) => result,
                Err(err) => {
                    drop(permit);
                    // The Router decision was already made and already paid
                    // for. Dropping the receipt with the failed spawn would
                    // erase the only record that a routing request was spent —
                    // and, when a bounded summary crossed to another provider,
                    // the only disclosure that it did. It survives the failure.
                    if let Some(receipt) = fleet_receipt {
                        self.record_orphaned_fleet_receipt(&receipt, &err.to_string());
                    }
                    return Err(DriverError::Rejected(err.to_string()));
                }
            };
        let task_id = result.result.agent_id.clone();
        if let Err(err) = self.ensure_admission_open() {
            let _ = self.manager.write().await.cancel_agent(&task_id);
            if let Some(receipt) = &fleet_receipt {
                self.record_orphaned_fleet_receipt(receipt, &err.to_string());
            }
            return Err(err);
        }
        if let Ok(mut permits) = self.spawn_permits.lock() {
            permits.insert(task_id.clone(), permit);
        }
        self.record_child(&task_id);
        self.record_task_started(
            &task_id,
            &request_record,
            &result.metadata,
            &result.result,
            fleet_receipt,
            // Started: a later refusal no longer ends this wait.
            queue_ticket.take(),
        );
        for artifact in handoffs.commit() {
            self.record_run_event(WorkflowUiEvent::new(
                &self.owner_session_id,
                WorkflowUiEventKind::HandoffConsumed {
                    artifact_id: artifact.id,
                    kind: artifact.kind,
                    from_role: artifact.from_role,
                    to_role: artifact.to_role,
                    consumer_task_id: task_id.clone(),
                },
            ));
        }
        self.record_task_request(&task_id, &request_record);
        let (tx, rx) = oneshot::channel();
        self.add_waiter_or_complete(task_id.clone(), tx);
        Ok(SpawnedTask {
            task_id,
            completion: rx,
        })
    }
}

#[async_trait]
impl WorkflowDriver for SubAgentWorkflowDriver {
    async fn spawn_task(&self, request: TaskRequest) -> Result<SpawnedTask, DriverError> {
        let label = request
            .label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_string);
        let phase = request
            .phase
            .as_deref()
            .map(str::trim)
            .filter(|phase| !phase.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.current_phase
                    .lock()
                    .ok()
                    .and_then(|phase| phase.clone())
            });
        let mut queue_ticket = None;
        let result = self.spawn_task_admitted(request, &mut queue_ticket).await;
        if let Err(err) = &result {
            self.record_dispatch_failure(label, phase, err.to_string(), queue_ticket);
        }
        result
    }

    fn cancel_all(&self) {
        self.force_cancel_all();
    }

    fn budget(&self) -> BudgetSnapshot {
        let snapshot = self.current_budget_snapshot();
        self.record_budget_snapshot(snapshot);
        snapshot
    }

    fn workspace_root(&self) -> Option<PathBuf> {
        Some(self.runtime.context.workspace.clone())
    }

    fn progress(&self, event: ProgressEvent) {
        let mut schema_error = None;
        let mut schema_repair = None;
        let (message, ui_event) = match event {
            // Pre-spawn rejections share the dispatch-failure ledger so the
            // completion classifier sees every requested slot, whether the VM
            // or the driver refused it.
            ProgressEvent::TaskRejected {
                label,
                phase,
                message,
            } => {
                self.record_dispatch_failure(label, phase, message, None);
                return;
            }
            // R9: a dead fan-out is a status signal, not a narrated line —
            // the prelude already logged the operator breadcrumb. Counted on
            // the driver so the terminal slot ledger can refuse to record a
            // run where nothing survived as a plain success.
            ProgressEvent::FanoutAllSlotsFailed { .. } => {
                self.record_dead_fanout();
                return;
            }
            ProgressEvent::FanoutSlotDropped {
                construct,
                kind,
                slot,
            } => {
                self.record_dropped_slot();
                // The per-slot log line already narrates the drop; this is
                // its typed twin for surfaces that show slots.
                self.record_run_event(WorkflowUiEvent::new(
                    &self.owner_session_id,
                    WorkflowUiEventKind::SlotDropped {
                        construct,
                        index: slot,
                        kind,
                    },
                ));
                return;
            }
            ProgressEvent::Log { message } => (
                format!("log: {message}"),
                WorkflowUiEvent::new(&self.owner_session_id, WorkflowUiEventKind::Log { message }),
            ),
            ProgressEvent::Phase { title } => {
                if let Ok(mut current) = self.current_phase.lock() {
                    *current = Some(title.clone());
                }
                (
                    format!("phase: {title}"),
                    WorkflowUiEvent::new(
                        &self.owner_session_id,
                        WorkflowUiEventKind::PhaseStarted { title },
                    ),
                )
            }
            ProgressEvent::TaskSchemaValidationFailed {
                task_id,
                kind,
                attempt,
                message,
                raw,
                raw_truncated,
            } => {
                self.record_schema_validation_failure(&task_id, message.clone());
                let (raw_preview, artifact) = self.schema_raw_receipt(&task_id, attempt, &raw);
                schema_error = Some(WorkflowSchemaError {
                    task_id: task_id.clone(),
                    message: message.clone(),
                    kind,
                    attempt,
                    raw_preview,
                    raw_truncated,
                    artifact,
                });
                (
                    format!(
                        "schema validation failed for {task_id} (attempt {attempt}): {message}"
                    ),
                    WorkflowUiEvent::new(
                        &self.owner_session_id,
                        WorkflowUiEventKind::TaskSchemaValidationFailed { task_id, message },
                    ),
                )
            }
            // #5583: a failed decode with a repair still to come. Receipted
            // on the schema-repair ledger (visible even when the repair
            // succeeds); the live surfaces see it as a progress line so
            // operators know a bounded re-ask is happening.
            ProgressEvent::TaskSchemaRepairAttempted {
                task_id,
                kind,
                attempt,
                message,
                raw,
                raw_truncated,
            } => {
                let (raw_preview, artifact) = self.schema_raw_receipt(&task_id, attempt, &raw);
                schema_repair = Some(WorkflowSchemaRepairAttempt {
                    task_id: task_id.clone(),
                    kind,
                    attempt,
                    message: message.clone(),
                    raw_preview,
                    raw_truncated,
                    artifact,
                });
                let note = format!(
                    "schema decode failed for {task_id} (attempt {attempt}): {message}; \
                     dispatching a bounded repair"
                );
                (
                    format!("log: {note}"),
                    WorkflowUiEvent::new(
                        &self.owner_session_id,
                        WorkflowUiEventKind::Log { message: note },
                    ),
                )
            }
        };
        if let Ok(mut runs) = self.state.runs.lock()
            && let Some(record) = runs.get_mut(&self.run_id)
        {
            if record.status != WorkflowRunStatus::Running {
                return;
            }
            record.push_progress(message.clone());
            record.push_event(ui_event.clone());
            if let Some(schema_error) = schema_error {
                record.schema_errors.push(schema_error);
            }
            if let Some(schema_repair) = schema_repair {
                record.schema_repair_count = record.schema_repair_count.saturating_add(1);
                record.schema_repairs.push(schema_repair);
            }
        }
        self.state.record_progress(&self.run_id, &message);
        self.state.record_event(&self.run_id, &ui_event);
        // #4122: phase/schema/log progress streams into the live panel path.
        self.emit_ui_event(&ui_event);
    }
}

fn budget_event_kind(snapshot: BudgetSnapshot) -> WorkflowUiEventKind {
    WorkflowUiEventKind::BudgetUpdated {
        total: snapshot.total,
        spent: snapshot.spent,
        remaining: snapshot.remaining(),
    }
}

fn gate_kind_label(kind: GateKind) -> &'static str {
    match kind {
        GateKind::Verify => "verify",
        GateKind::Review => "review",
        GateKind::Approve => "approve",
    }
}

fn gate_state_reason(state: &GateState) -> String {
    state
        .blocked_reason()
        .map(str::to_string)
        .unwrap_or_else(|| state.as_str().to_string())
}

fn append_handoff_context(request: &mut TaskRequest, handoffs: &[HandoffArtifact]) {
    request
        .description
        .push_str("\n\nWorkflow handoff artifacts available for this role:\n");
    for artifact in handoffs {
        request.description.push_str(&format!(
            "- id: {} kind: {} from: {} to: {}\n  payload: {}\n",
            artifact.id,
            artifact.kind,
            artifact.from_role,
            artifact.to_role,
            compact_handoff_payload(&artifact.payload, WORKFLOW_HANDOFF_MAX_CHARS)
        ));
    }
}

fn compact_handoff_payload(payload: &str, max_chars: usize) -> String {
    let trimmed = payload.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out = trimmed.chars().take(max_chars).collect::<String>();
    out.push_str("...");
    out
}

fn task_completion_status(completion: &TaskCompletion) -> (IrWorkflowRunStatus, Option<String>) {
    match completion {
        TaskCompletion::Completed { text } => (IrWorkflowRunStatus::Succeeded, Some(text.clone())),
        TaskCompletion::Failed { message } => (IrWorkflowRunStatus::Failed, Some(message.clone())),
        TaskCompletion::Cancelled => (IrWorkflowRunStatus::Cancelled, None),
        TaskCompletion::BudgetExhausted { message } => {
            (IrWorkflowRunStatus::BudgetExceeded, Some(message.clone()))
        }
    }
}

/// Sum per-task telemetry into run-wide totals for `run_completed` (#2974).
/// Returns `None` when no task contributed telemetry (e.g. a plan that ran
/// zero children) so the event stays byte-identical to its pre-#2974 shape.
fn run_usage_totals(records: &[RuntimeTaskRecord]) -> Option<WorkflowRunUsage> {
    let mut usages = records.iter().filter_map(|record| record.usage.as_ref());
    let mut totals = WorkflowRunUsage::from_task(usages.next()?);
    for usage in usages {
        totals.add_task(usage);
    }
    Some(totals)
}

/// Convert captured task telemetry into the shared `WorkflowUsage` aggregate
/// used by the workflow execution record (#2974).
fn workflow_usage_from_task(usage: &WorkflowTaskUsage) -> WorkflowUsage {
    WorkflowUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cost_microusd: usage.cost_microusd,
    }
}

fn execution_from_declarative_spec(
    spec: &WorkflowSpec,
    records: Vec<RuntimeTaskRecord>,
    terminal_status: WorkflowRunStatus,
) -> IrWorkflowExecution {
    let by_label = records
        .into_iter()
        .filter_map(|record| record.label.clone().map(|label| (label, record)))
        .collect::<HashMap<_, _>>();
    let mut execution = IrWorkflowExecution::default();
    for node in &spec.nodes {
        push_execution_node(node, &by_label, &mut execution);
    }
    let mut leaf_usage = execution.leaf_results.iter().map(|leaf| leaf.usage);
    execution.usage = leaf_usage
        .next()
        .map_or_else(WorkflowUsage::default, |first| {
            leaf_usage.fold(first, |mut totals, usage| {
                totals.input_tokens = sum_optional_usage(totals.input_tokens, usage.input_tokens);
                totals.output_tokens =
                    sum_optional_usage(totals.output_tokens, usage.output_tokens);
                totals.cost_microusd =
                    sum_optional_usage(totals.cost_microusd, usage.cost_microusd);
                totals
            })
        });
    match terminal_status {
        WorkflowRunStatus::Completed | WorkflowRunStatus::Degraded => {}
        WorkflowRunStatus::Failed => mark_ir_status(&mut execution, IrWorkflowRunStatus::Failed),
        WorkflowRunStatus::Cancelled => {
            mark_ir_status(&mut execution, IrWorkflowRunStatus::Cancelled);
        }
        WorkflowRunStatus::Running => {
            execution.status = IrWorkflowRunStatus::Running;
        }
    }
    execution
}

fn push_execution_node(
    node: &WorkflowNode,
    records: &HashMap<String, RuntimeTaskRecord>,
    execution: &mut IrWorkflowExecution,
) {
    match node {
        WorkflowNode::Leaf(spec) => push_leaf_execution(spec, records, execution),
        WorkflowNode::BranchSet(spec) => push_branch_execution(spec, records, execution),
        WorkflowNode::Sequence(spec) => push_sequence_execution(spec, records, execution),
        WorkflowNode::Reduce(spec) => push_control_execution(
            spec.id.as_str(),
            ControlNodeKind::Reduce,
            records.get(&spec.id),
            spec.inputs.clone(),
            Some(spec.prompt.clone()),
            execution,
        ),
        WorkflowNode::TeacherReview(spec) => push_control_execution(
            spec.id.as_str(),
            ControlNodeKind::TeacherReview,
            records.get(&spec.id),
            spec.candidates.clone(),
            Some("teacher review not lowered by the production adapter".to_string()),
            execution,
        ),
        WorkflowNode::LoopUntil(spec) => push_control_execution(
            spec.id.as_str(),
            ControlNodeKind::LoopUntil,
            records.get(&spec.id),
            spec.children.iter().map(declarative_node_id).collect(),
            Some("loop_until not lowered by the production adapter".to_string()),
            execution,
        ),
        WorkflowNode::Cond(spec) => push_control_execution(
            spec.id.as_str(),
            ControlNodeKind::Cond,
            records.get(&spec.id),
            spec.then_nodes
                .iter()
                .chain(spec.else_nodes.iter())
                .map(declarative_node_id)
                .collect(),
            Some("cond not lowered by the production adapter".to_string()),
            execution,
        ),
        WorkflowNode::Expand(spec) => push_control_execution(
            spec.id.as_str(),
            ControlNodeKind::Expand,
            records.get(&spec.id),
            Vec::new(),
            Some(format!("expand not lowered from {}", spec.source)),
            execution,
        ),
    }
}

fn push_leaf_execution(
    spec: &LeafSpec,
    records: &HashMap<String, RuntimeTaskRecord>,
    execution: &mut IrWorkflowExecution,
) {
    let record = records.get(&spec.id);
    let status = record
        .map(|record| record.status)
        .unwrap_or(IrWorkflowRunStatus::Pending);
    mark_ir_status(execution, status);
    execution.leaf_results.push(LeafResult {
        leaf_id: spec.id.clone(),
        task_id: record
            .map(|record| record.agent_id.clone())
            .unwrap_or_else(|| spec.id.clone()),
        role: spec.role.clone(),
        profile: spec.profile.clone(),
        status,
        usage: record
            .and_then(|record| record.usage.as_ref())
            .map(workflow_usage_from_task)
            .unwrap_or_default(),
        memo_usage: WorkflowMemoUsage::default(),
        output: record.and_then(|record| record.output.clone()),
        artifacts: Vec::new(),
        schema_error: record.and_then(|record| record.schema_error.clone()),
    });
}

fn push_branch_execution(
    spec: &BranchSpec,
    records: &HashMap<String, RuntimeTaskRecord>,
    execution: &mut IrWorkflowExecution,
) {
    let before = execution.leaf_results.len();
    for child in &spec.children {
        push_execution_node(child, records, execution);
    }
    let status = aggregate_ir_status(
        execution.leaf_results[before..]
            .iter()
            .map(|result| result.status),
    );
    mark_ir_status(execution, status);
    execution.branch_results.push(BranchResult {
        branch_id: spec.id.clone(),
        task_id: spec.id.clone(),
        status,
        usage: WorkflowUsage::default(),
        memo_usage: WorkflowMemoUsage::default(),
        artifacts: Vec::new(),
        notes: Some("production driver branch receipt from child task outcomes".to_string()),
    });
    execution.control_node_results.push(ControlNodeResult {
        node_id: spec.id.clone(),
        kind: ControlNodeKind::BranchSet,
        status,
        selected_children: spec.children.iter().map(declarative_node_id).collect(),
        summary: Some("branch set lowered into production child tasks".to_string()),
    });
}

fn push_sequence_execution(
    spec: &SequenceSpec,
    records: &HashMap<String, RuntimeTaskRecord>,
    execution: &mut IrWorkflowExecution,
) {
    let before_leaf = execution.leaf_results.len();
    let before_control = execution.control_node_results.len();
    for child in &spec.children {
        push_execution_node(child, records, execution);
    }
    let status = aggregate_ir_status(
        execution.leaf_results[before_leaf..]
            .iter()
            .map(|result| result.status)
            .chain(
                execution.control_node_results[before_control..]
                    .iter()
                    .map(|result| result.status),
            ),
    );
    mark_ir_status(execution, status);
    execution.control_node_results.push(ControlNodeResult {
        node_id: spec.id.clone(),
        kind: ControlNodeKind::Sequence,
        status,
        selected_children: spec.children.iter().map(declarative_node_id).collect(),
        summary: Some("sequence lowered in declaration order".to_string()),
    });
}

fn push_control_execution(
    node_id: &str,
    kind: ControlNodeKind,
    record: Option<&RuntimeTaskRecord>,
    selected_children: Vec<String>,
    fallback_summary: Option<String>,
    execution: &mut IrWorkflowExecution,
) {
    let status = record
        .map(|record| record.status)
        .unwrap_or(IrWorkflowRunStatus::Pending);
    mark_ir_status(execution, status);
    execution.control_node_results.push(ControlNodeResult {
        node_id: node_id.to_string(),
        kind,
        status,
        selected_children,
        summary: record
            .and_then(|record| record.output.clone())
            .or(fallback_summary),
    });
}

fn aggregate_ir_status(
    statuses: impl IntoIterator<Item = IrWorkflowRunStatus>,
) -> IrWorkflowRunStatus {
    let mut saw_pending = false;
    let mut saw_running = false;
    for status in statuses {
        match status {
            IrWorkflowRunStatus::BudgetExceeded => return IrWorkflowRunStatus::BudgetExceeded,
            IrWorkflowRunStatus::Cancelled => return IrWorkflowRunStatus::Cancelled,
            IrWorkflowRunStatus::Failed => return IrWorkflowRunStatus::Failed,
            IrWorkflowRunStatus::Running => saw_running = true,
            IrWorkflowRunStatus::Pending => saw_pending = true,
            IrWorkflowRunStatus::Succeeded => {}
        }
    }
    if saw_running {
        IrWorkflowRunStatus::Running
    } else if saw_pending {
        IrWorkflowRunStatus::Pending
    } else {
        IrWorkflowRunStatus::Succeeded
    }
}

fn mark_ir_status(execution: &mut IrWorkflowExecution, status: IrWorkflowRunStatus) {
    match status {
        IrWorkflowRunStatus::Failed => execution.mark_failed(),
        IrWorkflowRunStatus::Cancelled => execution.mark_cancelled(),
        IrWorkflowRunStatus::BudgetExceeded => execution.mark_budget_exceeded(),
        IrWorkflowRunStatus::Running => {
            if execution.status == IrWorkflowRunStatus::Succeeded {
                execution.status = IrWorkflowRunStatus::Running;
            }
        }
        IrWorkflowRunStatus::Pending => {
            if execution.status == IrWorkflowRunStatus::Succeeded {
                execution.status = IrWorkflowRunStatus::Pending;
            }
        }
        IrWorkflowRunStatus::Succeeded => {}
    }
}

fn declarative_node_id(node: &WorkflowNode) -> String {
    match node {
        WorkflowNode::BranchSet(spec) => spec.id.clone(),
        WorkflowNode::Leaf(spec) => spec.id.clone(),
        WorkflowNode::Sequence(spec) => spec.id.clone(),
        WorkflowNode::Reduce(spec) => spec.id.clone(),
        WorkflowNode::TeacherReview(spec) => spec.id.clone(),
        WorkflowNode::LoopUntil(spec) => spec.id.clone(),
        WorkflowNode::Cond(spec) => spec.id.clone(),
        WorkflowNode::Expand(spec) => spec.id.clone(),
    }
}

fn spawn_completion_pump(
    driver: Arc<SubAgentWorkflowDriver>,
    mut rx: mpsc::Receiver<SubAgentCompletion>,
) {
    spawn_supervised(
        "workflow-completion-pump",
        std::panic::Location::caller(),
        async move {
            loop {
                let completion = tokio::select! {
                    biased;
                    _ = driver.runtime.cancel_token.cancelled() => break,
                    completion = rx.recv() => completion,
                };
                let Some(completion) = completion else {
                    break;
                };
                let agent_id = completion.agent_id.clone();
                let (task_completion, usage) =
                    completion_from_manager(driver.manager.clone(), &agent_id, completion.payload)
                        .await;
                // A worker that ran somewhere other than its requested route
                // says so in the run itself, not only on its receipt.
                if let Some(note) = rerouted_task_note(&driver.manager, &agent_id).await {
                    driver.record_run_event(WorkflowUiEvent::new(
                        &driver.owner_session_id,
                        WorkflowUiEventKind::Log {
                            message: format!("Route fallback ({agent_id}): {note}"),
                        },
                    ));
                }
                driver.deliver_completion(agent_id, task_completion, usage);
            }
        },
    );
}

/// The fallback note of a child that ran on a replacement or parent route
/// instead of the one it requested, if it did.
async fn rerouted_task_note(manager: &SharedSubAgentManager, agent_id: &str) -> Option<String> {
    let route = manager
        .read()
        .await
        .get_result(agent_id)
        .ok()?
        .child_route?;
    matches!(
        route.route_source.as_str(),
        "session.fallback" | "role.replacement"
    )
    .then_some(route.fallback_note)
    .flatten()
}

/// Resolve a child's terminal completion from the manager, reading it exactly
/// once.
///
/// Every production path that publishes a child's completion does so inside
/// `SubAgentManager::finish_terminal_result`, which calls
/// `SubAgentTerminalDeliveryContext::deliver` (`subagent/mod.rs:2241`; the
/// `try_send` that wakes this pump is at `:2254`) and then commits the terminal
/// status through `update_from_result_with_persist` — both within the same
/// `&mut self` call, so its caller holds the write guard on this same
/// `Arc<RwLock<_>>` across the pair. See the six call sites at
/// `subagent/mod.rs:4761`, `:5883`, `:6592`, `:8025`, `:11453` (the panic path,
/// guarded at `:11442`) and `:11734` (guarded at `:11705`). A reader therefore
/// cannot acquire the lock between the wake and the commit, so the first read
/// after a completion arrives already observes the terminal status. Polling for
/// it bought nothing and cost the serial pump up to a second of head-of-line
/// blocking per child (#6211).
///
/// Known limitation: this does not cover ids the manager never records —
/// notably the workflow `run_id` that `finish_workflow_controller` sends for a
/// detached nested workflow. Those fail closed here rather than being waited
/// on.
async fn completion_from_manager(
    manager: SharedSubAgentManager,
    agent_id: &str,
    fallback_payload: String,
) -> (TaskCompletion, Option<WorkflowTaskUsage>) {
    let snapshot_and_usage = {
        let manager = manager.read().await;
        let snapshot = manager.get_result(agent_id).ok();
        let usage = snapshot
            .as_ref()
            .filter(|snapshot| snapshot.status != SubAgentStatus::Running)
            .map(|snapshot| task_usage_from_manager(&manager, agent_id, snapshot));
        let verification = manager
            .get_worker_record(agent_id)
            .map(|record| record.verification);
        (snapshot, usage, verification)
    };
    if let (Some(snapshot), usage, verification) = snapshot_and_usage
        && snapshot.status != SubAgentStatus::Running
    {
        let completion = match snapshot.status {
            SubAgentStatus::Completed
                if verification.as_ref().is_some_and(|receipt| {
                    matches!(
                        receipt.status.as_str(),
                        "deliverable_missing" | "claim_mismatch"
                    )
                }) =>
            {
                TaskCompletion::Failed {
                    message: format!(
                        "Sub-agent delivery verification failed: {}",
                        truncate_chars(
                            &verification.expect("matched failed receipt").summary,
                            1_000
                        )
                    ),
                }
            }
            SubAgentStatus::Completed => TaskCompletion::Completed {
                text: snapshot.result.clone().unwrap_or(fallback_payload),
            },
            SubAgentStatus::Failed(ref message) => TaskCompletion::Failed {
                message: message.clone(),
            },
            SubAgentStatus::Interrupted(ref message) => TaskCompletion::Failed {
                message: message.clone(),
            },
            SubAgentStatus::Cancelled => TaskCompletion::Cancelled,
            // The checkpoint reason names the limit that stopped the agent
            // (steps or wall time); the tally and panel read it from here.
            SubAgentStatus::BudgetExhausted => TaskCompletion::BudgetExhausted {
                message: snapshot
                    .checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.reason.trim())
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("agent stopped at a step or time limit")
                    .to_string(),
            },
            SubAgentStatus::Running => unreachable!("guarded above"),
        };
        return (completion, usage);
    }
    (
        TaskCompletion::Failed {
            message: format!(
                "sub-agent '{agent_id}' had no terminal manager record when its completion was delivered"
            ),
        },
        None,
    )
}

/// Capture per-worker telemetry at terminal delivery (#2974): provider-reported
/// tokens from the worker ledger, the model/tool step count and duration from
/// the agent snapshot, and a durable artifact reference for the full output.
fn task_usage_from_manager(
    manager: &SubAgentManager,
    agent_id: &str,
    snapshot: &SubAgentResult,
) -> WorkflowTaskUsage {
    let record = manager.get_worker_record(agent_id);
    let usage = record.as_ref().map(|record| &record.usage);
    let result_ref = record.as_ref().and_then(|record| {
        record
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "transcript")
            .or_else(|| record.artifacts.last())
            .map(|artifact| artifact.target.clone())
    });
    let input_tokens = usage.and_then(|usage| usage.input_tokens);
    let output_tokens = usage.and_then(|usage| usage.output_tokens);
    let total_tokens = usage.and_then(|usage| usage.total_tokens);
    // #4039: the worker ledger leaves these fields `None` until it receives a
    // typed provider usage envelope. Presence, not magnitude, is the receipt:
    // a provider-reported zero is still a real observation and must survive.
    let reported = provider_usage_was_reported(input_tokens, output_tokens, total_tokens);
    WorkflowTaskUsage {
        input_tokens: reported.then_some(input_tokens).flatten(),
        output_tokens: reported.then_some(output_tokens).flatten(),
        total_tokens: reported.then_some(total_tokens).flatten(),
        cost_microusd: usage.and_then(|usage| usage.cost_microusd),
        tool_calls: Some(snapshot.steps_taken),
        duration_ms: Some(snapshot.duration_ms),
        result_ref,
        token_source: reported.then_some(WorkflowTokenSource::ProviderReported),
    }
}

fn provider_usage_was_reported(
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
) -> bool {
    [input_tokens, output_tokens, total_tokens]
        .iter()
        .any(Option::is_some)
}

fn cancel_child_agents(manager: SharedSubAgentManager, ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    if let Ok(mut manager_guard) = manager.try_write() {
        for id in ids {
            let _ = manager_guard.cancel_agent(&id);
        }
        return;
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        spawn_supervised(
            "workflow-cancel-children",
            std::panic::Location::caller(),
            async move {
                let mut manager_guard = manager.write().await;
                for id in ids {
                    let _ = manager_guard.cancel_agent(&id);
                }
            },
        );
    }
}

fn lock_mutex<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, ToolError> {
    mutex
        .lock()
        .map_err(|_| ToolError::execution_failed("workflow state lock poisoned"))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

mod journal;

use journal::{WorkflowWorkspaceState, peek_shared_workflow_state, shared_workflow_state};

/// Bounded, read-only projection of one workflow run for the human-only
/// `/structcopy` command (#2033).
///
/// Built on the existing [`WorkflowRunSummary`] projection so retention and
/// truncation accounting (`events_total` / `events_dropped`) stay in exactly
/// one place. Two extra constraints beyond the model-facing summary:
/// `source_path` collapses to a bare file-name label so no filesystem path
/// leaves the process, and raw event/hook payloads never enter the
/// projection. Returns `None` when `run_id` is unknown to this session;
/// never creates workspace state or touches the journal.
pub(crate) fn structcopy_run_projection(
    workspace: &Path,
    run_id: &str,
    owner_session_id: Option<&str>,
) -> Option<Value> {
    let owner_session_id = owner_session_id?;
    let state = peek_shared_workflow_state(workspace)?;
    let runs = state
        .runs
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let record = runs
        .get(run_id)
        .filter(|record| record.owner_session_id.as_deref() == Some(owner_session_id))?;
    let mut value = serde_json::to_value(record.summary()).ok()?;
    if let Some(object) = value.as_object_mut() {
        let source_file = record
            .source_path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(|name| Value::String(name.to_string()))
            .unwrap_or(Value::Null);
        object.remove("source_path");
        object.insert("source_file".to_string(), source_file);
        // `WorkflowRunSummary` predates truthful unavailable-state rendering
        // and uses zero defaults when no execution projection exists. A
        // structural export must not turn that absence into measured zeros.
        if record.execution.is_none() {
            object.insert("leaf_count".to_string(), Value::Null);
            object.insert("branch_count".to_string(), Value::Null);
            object.insert("control_count".to_string(), Value::Null);
        }
    }
    Some(value)
}

/// One workflow run as the human-facing `/workflow` command reads it: a
/// bounded projection of the run record with no raw event payloads and no
/// filesystem paths beyond the source file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostWorkflowRunLine {
    pub run_id: String,
    /// Human-facing stage derived from the durable owner record and its typed
    /// event tail: queued | running | waiting | completed | degraded | failed |
    /// cancelled. This is presentation state, not a second lifecycle owner.
    pub status: &'static str,
    /// Whether the canonical owner record is still nonterminal. Controls key
    /// off this bit instead of reverse-parsing the display stage.
    pub active: bool,
    /// The run's goal, its workflow id, or the source file name.
    pub label: String,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub child_count: usize,
    pub last_progress: Option<String>,
    pub error: Option<String>,
}

fn host_workflow_stage(record: &WorkflowRunRecord) -> &'static str {
    match record.status {
        WorkflowRunStatus::Completed => return "completed",
        WorkflowRunStatus::Degraded => return "degraded",
        WorkflowRunStatus::Failed => return "failed",
        WorkflowRunStatus::Cancelled => return "cancelled",
        WorkflowRunStatus::Running => {}
    }

    // The owner journal remains the sole lifecycle source. These finer
    // nonterminal stages come only from its typed event stream: before the VM
    // reports real activity the accepted run is queued; while one or more
    // children remain open the VM is waiting on those agents; otherwise it is
    // actively running host/script work. A truncated tail may forget a child's
    // start event, in which case we safely show the coarser `running` state.
    let mut open_children = HashSet::new();
    let mut work_started = !record.progress.is_empty();
    for event in &record.events {
        match &event.kind {
            WorkflowUiEventKind::TaskStarted(started) => {
                work_started = true;
                open_children.insert(started.task_id.clone());
            }
            WorkflowUiEventKind::TaskCompleted { task_id, .. } => {
                work_started = true;
                open_children.remove(task_id);
            }
            WorkflowUiEventKind::RunStarted { .. }
            | WorkflowUiEventKind::RunCompleted { .. }
            | WorkflowUiEventKind::RunCancelled { .. } => {}
            _ => work_started = true,
        }
    }
    if !open_children.is_empty() {
        "waiting"
    } else if work_started {
        "running"
    } else {
        "queued"
    }
}

fn host_run_line(record: &WorkflowRunRecord) -> HostWorkflowRunLine {
    let summary = record.summary();
    let label = summary
        .workflow_goal
        .clone()
        .or_else(|| summary.workflow_id.clone())
        .or_else(|| {
            summary
                .source_path
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "workflow".to_string());
    HostWorkflowRunLine {
        run_id: summary.run_id,
        status: host_workflow_stage(record),
        active: summary.status == WorkflowRunStatus::Running,
        label,
        started_at_ms: summary.started_at_ms,
        completed_at_ms: summary.completed_at_ms,
        child_count: summary.child_count,
        last_progress: summary.last_progress,
        error: summary.error,
    }
}

/// Live workspace state if this process already has it, otherwise the
/// existing run journal. Never creates `.codewhale/` or the ledger.
fn host_workflow_state(workspace: &Path) -> Option<Arc<WorkflowWorkspaceState>> {
    if let Some(state) = peek_shared_workflow_state(workspace) {
        return Some(state);
    }
    // No live state yet: hydrate only if a journal already exists so
    // status/cancel can see runs from a previous process without creating
    // files in a workspace that never ran a workflow.
    workflow_journal_exists(workspace).then(|| shared_workflow_state(workspace))
}

/// Cancel hydrates an on-disk journal without the restart-orphan Failed
/// rewrite so a running line with no live controller can still be cancelled.
fn host_workflow_state_for_cancel(workspace: &Path) -> Option<Arc<WorkflowWorkspaceState>> {
    if let Some(state) = peek_shared_workflow_state(workspace) {
        return Some(state);
    }
    workflow_journal_exists(workspace)
        .then(|| WorkflowWorkspaceState::open_preserving_running(workspace))
}

fn workflow_journal_exists(workspace: &Path) -> bool {
    workspace
        .join(journal::CODEWHALE_DIR)
        .join(journal::WORKFLOW_RUNS_FILE)
        .is_file()
}

/// Every workflow run this workspace knows about (live and journaled),
/// oldest first. Read-only: never creates the journal or workspace state.
/// The `/workflow status` command reads this directly so status never costs
/// a model turn.
pub(crate) fn host_workflow_runs(
    workspace: &Path,
    owner_session_id: Option<&str>,
) -> Vec<HostWorkflowRunLine> {
    let Some(owner_session_id) = owner_session_id else {
        return Vec::new();
    };
    let Some(state) = host_workflow_state(workspace) else {
        return Vec::new();
    };
    let runs = state
        .runs
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut lines: Vec<HostWorkflowRunLine> = runs
        .values()
        .filter(|record| record.owner_session_id.as_deref() == Some(owner_session_id))
        .map(host_run_line)
        .collect();
    lines.sort_by_key(|line| line.started_at_ms);
    lines
}

/// One child-agent row of a workflow run as the host reads it: the typed
/// spawn label, the resolved role/model, the phase it was admitted under,
/// and a terminal state derived from the task-completed event (`running`
/// until one exists). Same bounded projection rules as
/// [`HostWorkflowRunLine`]: no raw event payloads, no filesystem paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostWorkflowChildRow {
    pub task_id: String,
    pub label: Option<String>,
    pub role: Option<String>,
    pub model: Option<String>,
    pub phase: Option<String>,
    pub state: &'static str,
}

/// Expanded host projection for the `/workflows` run manager: the run line
/// plus the phase order, the child-agent roster, and the retained progress
/// tail, all derived from the same run record the journal persists. Derived
/// from the bounded event tail, so a long run shows the newest phases and
/// children, not the whole history. Read-only: never creates the journal or
/// workspace state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostWorkflowRunDetail {
    pub line: HostWorkflowRunLine,
    pub phases: Vec<String>,
    pub children: Vec<HostWorkflowChildRow>,
    pub progress_tail: Vec<String>,
    pub has_result: bool,
}

fn host_task_state(status: IrWorkflowRunStatus) -> &'static str {
    match status {
        IrWorkflowRunStatus::Pending => "pending",
        IrWorkflowRunStatus::Running => "running",
        IrWorkflowRunStatus::Succeeded => "succeeded",
        IrWorkflowRunStatus::Failed => "failed",
        IrWorkflowRunStatus::Cancelled => "cancelled",
        IrWorkflowRunStatus::BudgetExceeded => "budget_exceeded",
    }
}

fn host_run_detail(record: &WorkflowRunRecord) -> HostWorkflowRunDetail {
    let line = host_run_line(record);
    let mut phases: Vec<String> = Vec::new();
    let mut children: Vec<HostWorkflowChildRow> = Vec::new();
    for event in &record.events {
        match &event.kind {
            WorkflowUiEventKind::PhaseStarted { title } => {
                if phases.last().map(String::as_str) != Some(title.as_str()) {
                    phases.push(title.clone());
                }
            }
            WorkflowUiEventKind::TaskStarted(started) => children.push(HostWorkflowChildRow {
                task_id: started.task_id.clone(),
                label: started
                    .workflow_task_label
                    .clone()
                    .or_else(|| started.label.clone()),
                role: started
                    .resolved_role
                    .clone()
                    .or_else(|| started.role.clone()),
                model: if started.resolved_model.is_empty() {
                    None
                } else {
                    Some(started.resolved_model.clone())
                },
                phase: started.workflow_phase_id.clone(),
                state: "running",
            }),
            WorkflowUiEventKind::TaskCompleted {
                task_id, status, ..
            } => {
                if let Some(row) = children.iter_mut().find(|row| row.task_id == *task_id) {
                    row.state = host_task_state(*status);
                }
            }
            _ => {}
        }
    }
    HostWorkflowRunDetail {
        line,
        phases,
        children,
        progress_tail: record
            .progress
            .iter()
            .rev()
            .take(HOST_RUN_PROGRESS_TAIL)
            .rev()
            .cloned()
            .collect(),
        has_result: record.result.is_some(),
    }
}

/// Every workflow run this workspace knows about (live and journaled) with
/// the detail the `/workflows` manager renders, oldest first. Read-only:
/// never creates the journal or workspace state.
pub(crate) fn host_workflow_run_details(
    workspace: &Path,
    owner_session_id: Option<&str>,
) -> Vec<HostWorkflowRunDetail> {
    let Some(owner_session_id) = owner_session_id else {
        return Vec::new();
    };
    let Some(state) = host_workflow_state(workspace) else {
        return Vec::new();
    };
    let runs = state
        .runs
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let mut details: Vec<HostWorkflowRunDetail> = runs
        .values()
        .filter(|record| record.owner_session_id.as_deref() == Some(owner_session_id))
        .map(host_run_detail)
        .collect();
    details.sort_by_key(|detail| detail.line.started_at_ms);
    details
}

/// Cancel a running workflow directly from the host (the `/workflow cancel`
/// command and the panel's cancel control), without a model turn. Returns
/// the run's projection after cancellation, or a plain reason when the run
/// is unknown. A run that already finished is reported as it is. After a
/// restart, a journaled running line with no live controller is cancelled
/// in the journal rather than rejected as unknown or controller-missing.
pub(crate) fn host_cancel_workflow(
    workspace: &Path,
    run_id: &str,
    owner_session_id: Option<&str>,
) -> Result<HostWorkflowRunLine, String> {
    let Some(owner_session_id) = owner_session_id else {
        return Err(format!("Unknown workflow run '{run_id}'."));
    };
    let Some(state) = host_workflow_state_for_cancel(workspace) else {
        return Err(format!("Unknown workflow run '{run_id}'."));
    };
    match cancel_workflow_run(run_id, state.clone(), owner_session_id) {
        Ok(_) => {
            let runs = state
                .runs
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            runs.get(run_id)
                .map(host_run_line)
                .ok_or_else(|| format!("Unknown workflow run '{run_id}'."))
        }
        Err(err) => Err(err.to_string()),
    }
}

/// Seed a minimal run record so `/structcopy` tests can exercise the
/// workflow projection without standing up the JS VM.
#[cfg(test)]
pub(crate) fn structcopy_test_seed_run(workspace: &Path, run_id: &str, owner_session_id: &str) {
    let state = shared_workflow_state(workspace);
    let record = WorkflowRunRecord::new(
        run_id.to_string(),
        Some(owner_session_id.to_string()),
        None,
        None,
        None,
    );
    state
        .runs
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(run_id.to_string(), record);
}

/// Reconcile workflow bindings after the journal has replayed restart
/// recovery. The journal owns lifecycle truth; the graph only receives its
/// monotonic projection.
pub(crate) fn reconcile_persisted_workflow_bindings(
    work: &SharedWorkRuntime,
    session_id: &str,
    workspace: &Path,
) -> Result<usize, String> {
    let state = shared_workflow_state(workspace);
    let records = state
        .runs
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let candidates = work
        .reconcilable_durable_bindings(Some(session_id))
        .into_iter()
        .filter(|external| external.starts_with("workflow:"))
        .collect::<std::collections::HashSet<_>>();
    let mut seen = std::collections::HashSet::new();
    let mut changed = 0usize;
    for record in records {
        if record.owner_session_id.as_deref() != Some(session_id) {
            continue;
        }
        let external = format!("workflow:{}", record.run_id);
        if !candidates.contains(&external) {
            continue;
        }
        seen.insert(external.clone());
        let lifecycle = WorkflowWorkLifecycle {
            work: work.clone(),
            session_id: session_id.to_string(),
            external,
        };
        changed += usize::from(lifecycle.reconcile_record(&record)?);
    }
    for external in candidates.difference(&seen) {
        changed += usize::from(work.reconcile_observation(
            session_id,
            external,
            OperationObservation::OwnerMissing {
                checked_at: i64::try_from(now_ms()).unwrap_or(i64::MAX),
            },
        )?);
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::CodewhaleClient;
    use crate::tools::ToolRegistryBuilder;
    use crate::tools::subagent::{SubAgentRuntime, new_shared_subagent_manager};
    use axum::{Json, Router, routing::post};
    use codewhale_workflow::{IsolationMode, leaf_is_write_capable};
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_clean_slot_ledger_leaves_the_run_completed() {
        let ledger = SlotLedger {
            tasks: 3,
            failed_tasks: 0,
            rejected: 0,
            dead_fanouts: 0,
            dropped_slots: 0,
            any_child_ran: true,
            retained_detail: None,
        };
        assert_eq!(ledger.classify(), None);
    }

    #[test]
    fn a_run_with_no_tasks_at_all_stays_completed() {
        // A script that orchestrates nothing is not a dropped-slot failure.
        assert_eq!(SlotLedger::default().classify(), None);
    }

    #[test]
    fn some_failed_slots_degrade_the_run_without_failing_it() {
        let (status, message) = SlotLedger {
            tasks: 3,
            failed_tasks: 1,
            rejected: 0,
            dead_fanouts: 0,
            dropped_slots: 0,
            any_child_ran: true,
            retained_detail: None,
        }
        .classify()
        .expect("a dropped slot must be classified");
        assert_eq!(status, WorkflowRunStatus::Degraded);
        assert!(message.contains("1 of 3 task(s) failed"), "{message}");
    }

    #[test]
    fn a_run_whose_every_task_failed_cannot_report_success() {
        // R9: `parallel()`'s settled default resolves each dead slot to
        // `null`, so the script returns cleanly. The run must not.
        let (status, message) = SlotLedger {
            tasks: 4,
            failed_tasks: 4,
            rejected: 0,
            dead_fanouts: 0,
            dropped_slots: 0,
            any_child_ran: true,
            retained_detail: None,
        }
        .classify()
        .expect("a fully-failed fan-out must be classified");
        assert_eq!(status, WorkflowRunStatus::Failed);
        assert!(
            message.contains("no task produced a result: all 4 task(s) failed"),
            "{message}"
        );
        assert_ne!(
            owner_state_for_run_status(status),
            OwnerState::Completed,
            "a run that lost every task must never project as completed"
        );
    }

    #[test]
    fn a_fully_failed_fan_out_also_names_its_rejected_dispatches() {
        let (status, message) = SlotLedger {
            tasks: 2,
            failed_tasks: 2,
            rejected: 1,
            dead_fanouts: 0,
            dropped_slots: 0,
            any_child_ran: true,
            retained_detail: Some("admission cap".to_string()),
        }
        .classify()
        .expect("a fully-failed fan-out must be classified");
        assert_eq!(status, WorkflowRunStatus::Failed);
        assert!(
            message.contains("all 2 task(s) failed")
                && message.contains("1 dispatch(es) were rejected"),
            "{message}"
        );
    }

    #[test]
    fn every_dispatch_rejected_still_fails_before_any_child_ran() {
        let (status, message) = SlotLedger {
            tasks: 0,
            failed_tasks: 0,
            rejected: 2,
            dead_fanouts: 0,
            dropped_slots: 0,
            any_child_ran: false,
            retained_detail: Some("depth ceiling".to_string()),
        }
        .classify()
        .expect("an all-rejected run must be classified");
        assert_eq!(status, WorkflowRunStatus::Failed);
        assert!(
            message.contains("no child agents ran") && message.contains("depth ceiling"),
            "{message}"
        );
    }

    #[test]
    fn budget_exhausted_children_count_as_failed_for_the_all_failed_rule() {
        // R9 blocker: a fan-out whose every child died of budget exhaustion
        // used to classify as a plain completion because `failed_tasks`
        // counted only `Failed` records. The records below are built through
        // the real completion mapping, not hand-set counters.
        let completions = [
            TaskCompletion::BudgetExhausted {
                message: "shared pool drained".to_string(),
            },
            TaskCompletion::BudgetExhausted {
                message: "shared pool drained".to_string(),
            },
        ];
        let task_records: Vec<RuntimeTaskRecord> = completions
            .iter()
            .enumerate()
            .map(|(index, completion)| {
                let (status, output) = task_completion_status(completion);
                RuntimeTaskRecord {
                    agent_id: format!("agent_{index}"),
                    label: None,
                    role: None,
                    status,
                    output,
                    schema_error: None,
                    usage: None,
                }
            })
            .collect();
        let ledger = SlotLedger {
            tasks: task_records.len(),
            failed_tasks: task_records
                .iter()
                .filter(|task| task.failed_for_ledger())
                .count(),
            rejected: 0,
            // The fan-out those children died in also reported itself dead.
            dead_fanouts: 1,
            dropped_slots: 0,
            any_child_ran: true,
            retained_detail: None,
        };
        assert_eq!(task_records[0].status, IrWorkflowRunStatus::BudgetExceeded);
        let (status, message) = ledger
            .classify()
            .expect("an all-budget-exhausted run must be classified");
        assert_eq!(status, WorkflowRunStatus::Failed);
        assert!(
            message.contains("all 2 task(s) failed")
                && message.contains("1 fan-out(s) lost every slot"),
            "{message}"
        );
        assert_ne!(
            owner_state_for_run_status(status),
            OwnerState::Completed,
            "a run that lost every child to budget exhaustion must never project as completed"
        );
    }

    #[test]
    fn a_fan_out_of_script_throws_with_no_task_records_fails_the_run() {
        // R9 blocker: thunks that throw without calling `task()` leave no
        // task record and no dispatch failure — only the structured
        // every-slot-failed count. That run produced nothing.
        let (status, message) = SlotLedger {
            tasks: 0,
            failed_tasks: 0,
            rejected: 0,
            dead_fanouts: 1,
            dropped_slots: 0,
            any_child_ran: false,
            retained_detail: None,
        }
        .classify()
        .expect("a dead script-throw fan-out must be classified");
        assert_eq!(status, WorkflowRunStatus::Failed);
        assert!(
            message.contains("no work survived") && message.contains("1 fan-out(s)"),
            "{message}"
        );
    }

    #[test]
    fn a_dead_fan_out_beside_surviving_work_degrades_instead_of_failing() {
        // Some work survived elsewhere in the run: keep the output, refuse
        // the plain-success label, but do not claim nothing completed.
        let (status, message) = SlotLedger {
            tasks: 1,
            failed_tasks: 0,
            rejected: 0,
            dead_fanouts: 1,
            dropped_slots: 0,
            any_child_ran: true,
            retained_detail: None,
        }
        .classify()
        .expect("a dead fan-out beside surviving work must be classified");
        assert_eq!(status, WorkflowRunStatus::Degraded);
        assert!(
            message.contains("1 fan-out(s) lost every slot")
                && message.contains("result may be partial"),
            "{message}"
        );
    }

    #[test]
    fn an_empty_fan_out_is_not_a_dead_fan_out() {
        // `parallel([])` declares no slots; a run that orchestrates nothing
        // stays completed (mirrors `a_run_with_no_tasks_at_all_stays_completed`).
        assert_eq!(SlotLedger::default().classify(), None);
    }

    #[test]
    fn settled_runs_leave_a_report_artifact_under_codewhale_reports() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut record = WorkflowRunRecord::new(
            "workflow_report_1".to_string(),
            Some("session-test".to_string()),
            None,
            None,
            None,
        );
        record.status = WorkflowRunStatus::Completed;
        record.workflow_goal = Some("prove the report artifact".to_string());
        record.push_progress("phase: scan".to_string());
        record.result = Some(serde_json::json!({"confirmed": 2}));

        write_run_report_artifact(tmp.path(), &record);

        let path = tmp
            .path()
            .join(".codewhale")
            .join("reports")
            .join("workflow_report_1.md");
        let body = std::fs::read_to_string(&path).expect("report written");
        assert!(body.contains("# Workflow run workflow_report_1"), "{body}");
        assert!(body.contains("status: Completed"), "{body}");
        assert!(body.contains("prove the report artifact"), "{body}");
        assert!(body.contains("phase: scan"), "{body}");
        assert!(body.contains("\"confirmed\": 2"), "{body}");
    }

    /// A workspace that ships `.codewhale/reports` as a link cannot make the
    /// run report or the raw schema reply land outside the workspace.
    #[cfg(unix)]
    #[test]
    fn report_artifacts_are_never_written_through_a_linked_reports_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::create_dir_all(tmp.path().join(".codewhale")).expect("mkdir");
        std::os::unix::fs::symlink(
            outside.path(),
            tmp.path().join(".codewhale").join("reports"),
        )
        .expect("link");
        let mut record = WorkflowRunRecord::new(
            "workflow_report_linked".to_string(),
            Some("session-test".to_string()),
            None,
            None,
            None,
        );
        record.status = WorkflowRunStatus::Completed;

        write_run_report_artifact(tmp.path(), &record);
        let schema = write_schema_raw_artifact(tmp.path(), "run-linked", "agent_0001", 1, "raw");

        assert!(schema.is_none(), "the schema artifact must be refused");
        assert_eq!(
            std::fs::read_dir(outside.path()).expect("outside").count(),
            0,
            "nothing may land behind the link"
        );
    }

    #[test]
    fn running_runs_write_no_report_artifact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let record = WorkflowRunRecord::new(
            "workflow_report_2".to_string(),
            Some("session-test".to_string()),
            None,
            None,
            None,
        );
        write_run_report_artifact(tmp.path(), &record);
        assert!(
            !tmp.path().join(".codewhale").join("reports").exists(),
            "running runs must not leave report files"
        );
    }

    #[test]
    fn source_path_accepts_the_home_workflow_store_and_rejects_elsewhere() {
        let _lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let store = home.join(".codewhale").join("workflows");
        std::fs::create_dir_all(&store).expect("store");
        let _home_guard = crate::test_support::EnvVarGuard::set("HOME", &home);
        let _userprofile_guard = crate::test_support::EnvVarGuard::set("USERPROFILE", &home);

        let saved = store.join("triage.workflow.js");
        std::fs::write(&saved, "phase('scan');\n").expect("write saved workflow");
        let elsewhere = tmp.path().join("outside.workflow.js");
        std::fs::write(&elsewhere, "phase('scan');\n").expect("write outside workflow");

        let workspace = tmp.path().join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let context = ToolContext::new(workspace);

        let resolved = read_workflow_source_path(saved.to_str().expect("utf8 path"), &context)
            .expect("home workflow store is a first-class source");
        assert!(resolved.source.contains("phase('scan')"));

        let err = read_workflow_source_path(elsewhere.to_str().expect("utf8 path"), &context)
            .expect_err("arbitrary outside paths stay denied");
        assert!(
            err.to_string()
                .contains("workspace or ~/.codewhale/workflows"),
            "{err}"
        );
    }

    #[test]
    fn restored_workflow_binding_consumes_journal_recovery() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        let record = WorkflowRunRecord::new(
            "workflow_restore".to_string(),
            Some("restored-workflow-session".to_string()),
            None,
            None,
            None,
        );
        state.record_snapshot(&record);

        let work = crate::work_graph::new_shared_work_runtime(
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        );
        work.register_operation(
            "restored-workflow-session",
            OperationIntent::new(
                "workflow:workflow_restore",
                "restored workflow",
                true,
                "workflow",
                "restore-test",
            ),
        )
        .expect("register saved workflow binding");
        work.reconcile_operation(
            "restored-workflow-session",
            OperationOwnerSnapshot::new("workflow:workflow_restore", OwnerState::Running, 1, 1),
        )
        .expect("saved running owner state");
        work.register_operation(
            "restored-workflow-session",
            OperationIntent::new(
                "workflow:workflow_absent",
                "absent workflow",
                true,
                "workflow",
                "absent-restore-test",
            ),
        )
        .expect("register absent workflow binding");
        work.reconcile_operation(
            "restored-workflow-session",
            OperationOwnerSnapshot::new("workflow:workflow_absent", OwnerState::Running, 1, 1),
        )
        .expect("saved absent owner state");

        assert_eq!(
            reconcile_persisted_workflow_bindings(&work, "restored-workflow-session", tmp.path(),),
            Ok(2)
        );
        let graph = work
            .capture(Some("restored-workflow-session"))
            .expect("capture restored workflow")
            .expect("graph")
            .graph;
        let operation = graph
            .nodes
            .iter()
            .find(|node| {
                node.binding
                    .as_ref()
                    .is_some_and(|binding| binding.external == "workflow:workflow_restore")
            })
            .expect("workflow operation");
        assert_eq!(operation.state, crate::work_graph::NodeState::Failed);
        assert_eq!(
            operation
                .binding
                .as_ref()
                .and_then(|binding| binding.last_observation.as_ref())
                .map(|observation| observation.seq),
            Some(2),
            "journal replay must advance the lost live owner before graph reconciliation"
        );
        let absent = graph
            .nodes
            .iter()
            .find(|node| {
                node.binding
                    .as_ref()
                    .is_some_and(|binding| binding.external == "workflow:workflow_absent")
            })
            .expect("absent workflow operation");
        assert_eq!(absent.state, crate::work_graph::NodeState::Stale);
        assert_eq!(
            reconcile_persisted_workflow_bindings(&work, "restored-workflow-session", tmp.path(),),
            Ok(0),
            "rechecking an already stale missing owner must be idempotent"
        );
    }

    #[tokio::test]
    async fn cancellation_without_controller_marks_the_journal_cancelled() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        let record = WorkflowRunRecord::new(
            "workflow_missing_controller".to_string(),
            Some("missing-controller-session".to_string()),
            None,
            None,
            None,
        );
        state
            .runs
            .lock()
            .expect("runs lock")
            .insert(record.run_id.clone(), record.clone());
        state.record_snapshot(&record);

        let work = crate::work_graph::new_shared_work_runtime(
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        );
        work.register_operation(
            "missing-controller-session",
            OperationIntent::new(
                "workflow:workflow_missing_controller",
                "missing controller",
                true,
                "workflow",
                "missing-controller-test",
            ),
        )
        .expect("register workflow");
        work.reconcile_operation(
            "missing-controller-session",
            OperationOwnerSnapshot::new(
                "workflow:workflow_missing_controller",
                OwnerState::Running,
                1,
                1,
            ),
        )
        .expect("running workflow");
        state.attach_lifecycle(
            "workflow_missing_controller",
            WorkflowWorkLifecycle {
                work: work.clone(),
                session_id: "missing-controller-session".to_string(),
                external: "workflow:workflow_missing_controller".to_string(),
            },
        );

        cancel_workflow(
            json!({"run_id": "workflow_missing_controller"}),
            state.clone(),
            "missing-controller-session",
        )
        .await
        .expect("controller-less cancel must still journal cancelled");
        let record = state
            .runs
            .lock()
            .expect("runs lock")
            .get("workflow_missing_controller")
            .cloned()
            .expect("workflow owner");
        assert_eq!(record.status, WorkflowRunStatus::Cancelled);
        assert_eq!(record.lifecycle_seq, 2);
        assert!(
            record
                .error
                .as_deref()
                .is_some_and(|error| error.contains("no live process")),
            "expected an honest nothing-live receipt, got {:?}",
            record.error
        );
        let operation = work
            .capture(Some("missing-controller-session"))
            .expect("capture")
            .expect("graph")
            .graph
            .nodes
            .into_iter()
            .find(|node| node.kind == crate::work_graph::NodeKind::Operation)
            .expect("workflow operation");
        assert_eq!(operation.state, crate::work_graph::NodeState::Cancelled);
    }

    #[tokio::test]
    async fn workflow_controls_are_session_owned_and_legacy_records_fail_closed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        let record = |run_id: &str, owner: Option<&str>| {
            WorkflowRunRecord::new(
                run_id.to_string(),
                owner.map(str::to_string),
                None,
                None,
                None,
            )
        };
        let legacy_record = {
            let mut value =
                serde_json::to_value(record("workflow-legacy", Some("session-from-newer-schema")))
                    .expect("serialize legacy fixture");
            value
                .as_object_mut()
                .expect("record object")
                .remove("owner_session_id");
            serde_json::from_value::<WorkflowRunRecord>(value)
                .expect("legacy ownerless journal record parses")
        };
        assert!(legacy_record.owner_session_id.is_none());
        {
            let mut runs = state.runs.lock().expect("runs");
            runs.insert(
                "workflow-a".to_string(),
                record("workflow-a", Some("session-a")),
            );
            runs.insert(
                "workflow-b".to_string(),
                record("workflow-b", Some("session-b")),
            );
            runs.insert("workflow-legacy".to_string(), legacy_record);
        }

        let listed =
            status_workflow(json!({}), state.clone(), "session-b").expect("session-owned list");
        let listed: Value = serde_json::from_str(&listed.content).expect("list json");
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["runs"][0]["run_id"], "workflow-b");

        let empty_dir = tempfile::tempdir().expect("empty tempdir");
        let empty_state = WorkflowWorkspaceState::open(empty_dir.path());
        let unknown_a = workflow_result_for("workflow-a", empty_state.clone(), "session-b")
            .expect_err("unknown A run");
        let foreign = workflow_result_for("workflow-a", state.clone(), "session-b")
            .expect_err("foreign run must be hidden");
        let unknown_legacy = workflow_result_for("workflow-legacy", empty_state, "session-b")
            .expect_err("unknown legacy-shaped id");
        let legacy = workflow_result_for("workflow-legacy", state.clone(), "session-b")
            .expect_err("legacy ownerless run must be hidden");
        assert_eq!(foreign.to_string(), unknown_a.to_string());
        assert_eq!(legacy.to_string(), unknown_legacy.to_string());

        cancel_workflow(json!({"run_id": "workflow-a"}), state.clone(), "session-b")
            .await
            .expect_err("B cannot cancel A");
        cancel_workflow(
            json!({"run_id": "workflow-legacy"}),
            state.clone(),
            "session-b",
        )
        .await
        .expect_err("B cannot cancel ownerless legacy work");
        {
            let runs = state.runs.lock().expect("runs");
            assert_eq!(runs["workflow-a"].status, WorkflowRunStatus::Running);
            assert_eq!(runs["workflow-legacy"].status, WorkflowRunStatus::Running);
        }

        cancel_workflow(json!({"run_id": "workflow-b"}), state.clone(), "session-b")
            .await
            .expect("B can cancel B");
        assert_eq!(
            state.runs.lock().expect("runs")["workflow-b"].status,
            WorkflowRunStatus::Cancelled
        );
    }

    #[test]
    fn handoff_compaction_preserves_release_sized_evidence() {
        let payload = format!("APPROVE\n{}\nterminal: RunCompleted", "e".repeat(1_500));

        assert_eq!(
            compact_handoff_payload(&payload, WORKFLOW_HANDOFF_MAX_CHARS),
            payload
        );
    }

    #[test]
    fn handoff_compaction_still_caps_oversized_artifacts() {
        let payload = "e".repeat(WORKFLOW_HANDOFF_MAX_CHARS + 1);
        let compacted = compact_handoff_payload(&payload, WORKFLOW_HANDOFF_MAX_CHARS);

        assert_eq!(compacted.chars().count(), WORKFLOW_HANDOFF_MAX_CHARS + 3);
        assert!(compacted.ends_with("..."));
    }

    #[test]
    fn declarative_detection_matches_indented_and_nonleading_workflow_calls() {
        // column-0 forms
        assert!(looks_like_declarative_workflow("workflow({ tasks: [] })"));
        assert!(looks_like_declarative_workflow(
            "export default workflow({})"
        ));
        // #dogfood 0.8.67: a leading statement/comment followed by an INDENTED
        // top-level workflow( call must still be detected as declarative.
        assert!(looks_like_declarative_workflow(
            "// build the run\n  workflow({\n    tasks: [],\n  })"
        ));
        // imperative scripts must not be misdetected as declarative
        assert!(!looks_like_declarative_workflow(
            "return await parallel([() => task({ description: \"x\" })]);"
        ));
        assert!(!looks_like_declarative_workflow("const x = myworkflow(1);"));
    }

    #[test]
    fn workflow_action_defaults_to_start() {
        assert_eq!(
            parse_workflow_action(&json!({})).unwrap(),
            WorkflowAction::Start
        );
        assert_eq!(
            parse_workflow_action(&json!({"action": "run"})).unwrap(),
            WorkflowAction::Run
        );
    }

    #[test]
    fn named_fleet_maps_workflow_role_to_profile_before_spawn() {
        let fleet = FleetRoleMap::from_pairs([
            ("scout", "scout"),
            ("implementer", "builder"),
            ("reviewer", "reviewer"),
            ("verifier", "verifier"),
            ("release_lead", "manager"),
        ])
        .expect("fleet");
        let mut request = TaskRequest {
            description: "fix it".to_string(),
            subagent_type: None,
            role: Some("implementer".to_string()),
            profile: None,
            model: None,
            model_strength: None,
            thinking: None,
            cwd: None,
            worktree: true,
            write_authority: Some("worktree_write".to_string()),
            write_roots: vec!["src".to_string()],
            exact_files: Vec::new(),
            coordination_contracts: vec!["test-contract".to_string()],
            dependencies: Vec::new(),
            acceptance: Vec::new(),
            allowed_tools: None,
            disallowed_tools: Vec::new(),
            max_depth: None,
            token_budget: None,
            max_steps: None,
            wall_time_secs: None,
            response_schema: None,
            schema_repair_attempts: None,
            label: Some("fix".to_string()),
            phase: Some("implement".to_string()),
        };

        apply_named_fleet_to_task_request(Some(&fleet), &mut request).expect("resolve");

        assert_eq!(request.role.as_deref(), Some("implement"));
        assert_eq!(request.profile.as_deref(), Some("builder"));
    }

    // ── Exact named Fleet (schema = "exact") ────────────────────────────────

    /// A Fleet that references a saved, reusable Reasoning Router service, and
    /// whose members' ids differ from their semantic roles — the case a gate
    /// keyed on a role has to keep working through.
    const EXACT_GLM_FLEET: &str = r#"
name = "glm-pair"
schema = "exact"
reasoning_router = "luna-low"

[[members]]
id = "implementer"
role = "builder"
provider = "zai"
model = "glm-5"
reasoning = "auto"
permissions = "read_write"

[[members]]
id = "auditor"
role = "reviewer"
provider = "zai"
model = "glm-5"
reasoning = "high"
permissions = "read_only"
"#;

    fn exact_task_request(role: &str) -> TaskRequest {
        TaskRequest {
            description: "land the fix".to_string(),
            subagent_type: None,
            role: Some(role.to_string()),
            profile: None,
            model: None,
            model_strength: None,
            thinking: None,
            cwd: None,
            worktree: false,
            write_authority: None,
            write_roots: Vec::new(),
            exact_files: Vec::new(),
            coordination_contracts: Vec::new(),
            dependencies: Vec::new(),
            acceptance: Vec::new(),
            allowed_tools: None,
            disallowed_tools: Vec::new(),
            max_depth: None,
            token_budget: None,
            max_steps: None,
            wall_time_secs: None,
            response_schema: None,
            schema_repair_attempts: None,
            label: None,
            phase: None,
        }
    }

    /// A task for a write-capable member. The spawn boundary refuses an
    /// unbounded write claim, so a write-capable exact task always carries a
    /// declared scope — the same contract, checked before the Router runs.
    fn exact_write_task_request(role: &str) -> TaskRequest {
        TaskRequest {
            write_roots: vec!["crates/tui".to_string()],
            ..exact_task_request(role)
        }
    }

    fn exact_session() -> codewhale_workflow::PermissionCeiling {
        codewhale_workflow::PermissionCeiling {
            write: true,
            network_tool: true,
            shell: codewhale_workflow::ShellCeiling::Full,
            delegation_depth: codewhale_config::DEFAULT_SPAWN_DEPTH,
            tools: true,
        }
    }

    fn exact_workflow_with(
        text: &str,
        router: Option<std::sync::Arc<crate::fleet::exact::StaticFleetRouter>>,
    ) -> crate::fleet::exact::ExactFleetWorkflow {
        let document = codewhale_workflow::FleetDocument::parse(text).expect("exact fleet parses");
        crate::fleet::exact::ExactFleetWorkflow::for_tests(
            &document,
            codewhale_workflow::QualifiedFleetId {
                name: "glm-pair".to_string(),
                origin: "workspace".to_string(),
            },
            router,
        )
    }

    fn exact_workflow(text: &str) -> crate::fleet::exact::ExactFleetWorkflow {
        exact_workflow_with(
            text,
            Some(crate::fleet::exact::StaticFleetRouter::new(
                r#"{"reasoning":"max"}"#,
            )),
        )
    }

    /// Binding resolves the member and Runtime authority; routing resolves reasoning.
    /// Both halves land on the request, in that order.
    #[tokio::test]
    async fn exact_fleet_task_launch_resolves_member_route_and_runtime_authority() {
        let operation = exact_workflow(EXACT_GLM_FLEET);
        let mut request = exact_write_task_request("builder");

        let binding = bind_exact_fleet_task_request(&operation, exact_session(), &mut request)
            .expect("exact member resolves");

        // Addressed by member id, so the frozen snapshot route (which carries
        // the exact provider pin and canonical wire model) is what the
        // spawn resolves…
        assert_eq!(request.profile.as_deref(), Some("implementer"));
        // …while the semantic role is preserved for gates and records.
        assert_eq!(request.role.as_deref(), Some("implement"));
        let member = operation
            .snapshot()
            .member("implementer")
            .expect("snapshot entry");
        assert_eq!(member.route.provider, "zai");
        assert_eq!(member.route.model, "glm-5");

        // Runtime's role/parent intersection reached the request before routing.
        assert_eq!(request.write_authority.as_deref(), Some("workspace_write"));
        assert_eq!(
            request.max_depth,
            Some(codewhale_config::DEFAULT_SPAWN_DEPTH)
        );
        assert!(request.thinking.is_none(), "reasoning is not decided yet");

        route_admitted_exact_task(&operation, &binding, &mut request)
            .await
            .expect("routing");
        // `auto` was resolved by the Router into a concrete tier — never the
        // literal sentinel, and never the legacy local heuristic.
        assert_eq!(request.thinking.as_deref(), Some("max"));

        // A read-only member is launched read-only, with no router call.
        let mut auditor = exact_task_request("reviewer");
        let auditor_binding =
            bind_exact_fleet_task_request(&operation, exact_session(), &mut auditor)
                .expect("auditor resolves");
        route_admitted_exact_task(&operation, &auditor_binding, &mut auditor)
            .await
            .expect("routing");
        assert_eq!(auditor.write_authority.as_deref(), Some("read_only"));
        assert_eq!(auditor.thinking.as_deref(), Some("high"));
        assert_eq!(auditor.role.as_deref(), Some("reviewer"));
        assert_eq!(auditor.profile.as_deref(), Some("auditor"));
    }

    /// The session's `[workflow]` table must decide the approval requirement;
    /// before this the tool consulted product defaults only, so a user who
    /// set `require_approval_for_writes = false` (documented in
    /// docs/AUTOMATIC_WORKFLOWS.md) still got the approval card.
    #[test]
    fn workflow_tool_honors_the_session_workflow_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let mut runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx,
            true,
            None,
            manager.clone(),
        );
        let input = json!({
            "action": "start",
            "plan": {
                "goal": "write freely",
                "risk": "writes",
                "children": [{ "prompt": "edit", "type": "implementer" }]
            }
        });
        let tool = WorkflowTool::new(Arc::clone(&manager), runtime.clone());
        assert_eq!(
            tool.approval_requirement_for(&input),
            ApprovalRequirement::Required,
            "product default: writes need approval"
        );

        let config = crate::config::Config {
            workflow: Some(codewhale_config::WorkflowConfigToml {
                require_approval_for_writes: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        runtime.api_config = Some(Arc::new(config));
        let tool = WorkflowTool::new(Arc::clone(&manager), runtime.clone());
        assert_eq!(
            tool.approval_requirement_for(&input),
            ApprovalRequirement::Auto,
            "the session config must win"
        );

        let read_only = json!({
            "action": "start",
            "plan": {
                "goal": "scout crates",
                "risk": "read_only",
                "children": [{ "prompt": "look", "type": "explore" }]
            }
        });
        let config = crate::config::Config {
            workflow: Some(codewhale_config::WorkflowConfigToml {
                auto_start_read_only: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        runtime.api_config = Some(Arc::new(config));
        let tool = WorkflowTool::new(manager, runtime);
        assert_eq!(
            tool.approval_requirement_for(&read_only),
            ApprovalRequirement::Required,
            "auto_start_read_only = false must still ask"
        );
    }

    /// The gate machinery keys on the **semantic role**. It must still fire
    /// when a member's id differs from that role — the exact failure an earlier
    /// pass introduced by stamping the profile id into `role`.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_role_keyed_gate_still_fires_when_the_member_id_differs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        );
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_exact_gate".to_string();
        // The gate blocks the semantic role `implement`, whose member id is
        // the *different* string `implementer`.
        let gates = vec![GateSpec {
            id: "explore-findings".to_string(),
            role: "explore".to_string(),
            on: GateOn::RoleComplete,
            gate: GateKind::Approve,
            on_fail: codewhale_workflow::GateOnFail::Block,
            blocks_role: Some("implement".to_string()),
            max_retries: 0,
            artifact_kind: Some("findings".to_string()),
            require_explicit_verdict: false,
        }];
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".to_string()),
                None,
                None,
                None,
            ),
        );
        let driver = SubAgentWorkflowDriver::new(
            run_id.clone(),
            "session-test".to_string(),
            manager,
            runtime,
            state.clone(),
            None,
            WorkflowFleetBinding::None,
            gates,
            tmp.path().to_path_buf(),
        );

        // The upstream explore agent fails, which puts the gate into a
        // blocking state.
        driver.evaluate_gates_for_completed_role(&RuntimeTaskRecord {
            agent_id: "explore-agent".to_string(),
            label: Some("explore".to_string()),
            role: Some("explore".to_string()),
            status: IrWorkflowRunStatus::Failed,
            output: None,
            schema_error: None,
            usage: None,
        });

        let operation = exact_workflow(EXACT_GLM_FLEET);
        let mut request = exact_write_task_request("builder");
        bind_exact_fleet_task_request(&operation, exact_session(), &mut request).expect("bind");

        assert_ne!(
            request.role.as_deref(),
            request.profile.as_deref(),
            "this fleet's ids and roles differ, which is the whole point"
        );
        assert_eq!(request.role.as_deref(), Some("implement"));

        let err = driver
            .prepare_request_for_gates(&mut request)
            .expect_err("a blocking gate on `implement` must still see `implement`");
        assert!(err.to_string().contains("implement"), "{err}");

        // The same gate does *not* block a task carrying the member id, which
        // is exactly why stamping the id into `role` silently disabled it.
        let mut by_id = exact_task_request("builder");
        by_id.role = Some("implementer".to_string());
        by_id.profile = None;
        assert!(
            driver.prepare_request_for_gates(&mut by_id).is_ok(),
            "the profile id is not the semantic role the gate keys on"
        );
    }

    /// A task whose `role` and `profile` name different members is rejected
    /// rather than resolved by precedence.
    #[test]
    fn exact_fleet_rejects_a_conflicting_task_role_and_profile() {
        let operation = exact_workflow(EXACT_GLM_FLEET);
        let mut request = exact_task_request("reviewer");
        request.profile = Some("implementer".to_string());

        let err = bind_exact_fleet_task_request(&operation, exact_session(), &mut request)
            .expect_err("conflicting identity");
        let message = format!("{err:?}");
        assert!(message.contains("different members"), "{message}");
    }

    #[test]
    fn exact_fleet_rejects_task_level_route_overrides() {
        let operation = exact_workflow(EXACT_GLM_FLEET);

        for mutate in [
            (|request: &mut TaskRequest| request.model = Some("glm-4".to_string()))
                as fn(&mut TaskRequest),
            |request: &mut TaskRequest| request.model_strength = Some("faster".to_string()),
            |request: &mut TaskRequest| request.thinking = Some("off".to_string()),
        ] {
            let mut request = exact_task_request("builder");
            mutate(&mut request);
            let err = bind_exact_fleet_task_request(&operation, exact_session(), &mut request)
                .expect_err("an exact fleet member may not be re-routed per task");
            let message = format!("{err:?}");
            assert!(
                message.contains("not allowed"),
                "override must be rejected, not ignored: {message}"
            );
        }
    }

    /// A task must not be able to replace Runtime's role/parent authority by
    /// asking for a different agent type, tool surface, or write authority.
    #[test]
    fn exact_fleet_rejects_task_level_posture_widening() {
        let operation = exact_workflow(EXACT_GLM_FLEET);

        for (field, mutate) in [
            (
                "subagent_type",
                (|request: &mut TaskRequest| {
                    request.subagent_type = Some("general".to_string());
                }) as fn(&mut TaskRequest),
            ),
            ("write_authority", |request: &mut TaskRequest| {
                request.write_authority = Some("workspace_write".to_string());
            }),
        ] {
            // The Runtime reviewer posture is read-only.
            let mut request = exact_task_request("reviewer");
            mutate(&mut request);
            let err = bind_exact_fleet_task_request(&operation, exact_session(), &mut request)
                .expect_err("Runtime authority must win over a task option");
            let message = format!("{err:?}");
            assert!(
                message.contains(field),
                "{field} must be rejected: {message}"
            );
        }

        // With no task options, Runtime's reviewer posture is what lands.
        let mut clean = exact_task_request("reviewer");
        bind_exact_fleet_task_request(&operation, exact_session(), &mut clean)
            .expect("clean launch");
        assert_eq!(clean.write_authority.as_deref(), Some("read_only"));
        assert_eq!(clean.subagent_type, None);
        // A task allowlist can only narrow; it never removes Runtime denials.
        // Asking for a tool the role denies is refused loudly (SHA-6734)
        // instead of silently launching a child with nothing usable.
        let mut narrowed = exact_task_request("reviewer");
        narrowed.allowed_tools = Some(vec!["exec_shell".to_string()]);
        let err = bind_exact_fleet_task_request(&operation, exact_session(), &mut narrowed)
            .expect_err("a requested tool the Runtime denylist removes is refused");
        let message = format!("{err:?}");
        assert!(message.contains("would start with no tools"), "{message}");
        assert!(message.contains("dropped [exec_shell]"), "{message}");
        // A request the role allows still narrows the surface.
        let mut narrowed = exact_task_request("reviewer");
        narrowed.allowed_tools = Some(vec!["read_file".to_string()]);
        bind_exact_fleet_task_request(&operation, exact_session(), &mut narrowed)
            .expect("allowlist narrows within the role");
        assert_eq!(narrowed.write_authority.as_deref(), Some("read_only"));
        assert!(
            narrowed
                .disallowed_tools
                .iter()
                .any(|tool| tool.eq_ignore_ascii_case("exec_shell"))
        );
    }

    /// Runtime's reviewer posture becomes a real tool policy, and the legacy
    /// Fleet `permissions` key cannot turn network reach off or rewrite it.
    #[test]
    fn exact_fleet_runtime_authority_reaches_the_spawn_request() {
        let operation = exact_workflow(EXACT_GLM_FLEET);
        let mut request = exact_task_request("reviewer");

        bind_exact_fleet_task_request(&operation, exact_session(), &mut request).expect("bind");

        assert_eq!(
            request.allowed_tools, None,
            "Runtime reviewer inherits the parent tool surface"
        );
        for denied in ["write_file", "apply_patch", "exec_shell"] {
            assert!(
                request.disallowed_tools.iter().any(|name| name == denied),
                "{denied} must be denied: {:?}",
                request.disallowed_tools
            );
        }
        for available in ["Bash", "Web", "web.run", "fetch_url", "mcp*"] {
            assert!(
                !request
                    .disallowed_tools
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(available)),
                "Runtime reviewer keeps {available}: {:?}",
                request.disallowed_tools
            );
        }
    }

    #[test]
    fn legacy_fleet_permissions_cannot_change_runtime_authority() {
        let narrow = exact_workflow(EXACT_GLM_FLEET);
        let legacy_full_text =
            EXACT_GLM_FLEET.replace("permissions = \"read_only\"", "permissions = \"full\"");
        let legacy_full = exact_workflow(&legacy_full_text);
        let mut narrow_request = exact_task_request("reviewer");
        let mut full_request = exact_task_request("reviewer");

        let narrow_binding =
            bind_exact_fleet_task_request(&narrow, exact_session(), &mut narrow_request)
                .expect("legacy narrow value loads");
        let full_binding =
            bind_exact_fleet_task_request(&legacy_full, exact_session(), &mut full_request)
                .expect("legacy full value loads");

        assert_eq!(narrow_binding.authority, full_binding.authority);
        assert_eq!(narrow_request.write_authority, full_request.write_authority);
        assert_eq!(
            narrow_request.disallowed_tools,
            full_request.disallowed_tools
        );
    }

    /// A Router decision is paid for before the child exists. If the spawn then
    /// fails, the receipt is the only record that tokens were spent and — for a
    /// cross-provider Router — that a bounded summary already left the host. It
    /// must survive the failure rather than being dropped with it.
    #[tokio::test]
    async fn a_routing_receipt_survives_a_failed_spawn() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        );
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_orphaned_receipt".to_string();
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".to_string()),
                None,
                None,
                None,
            ),
        );
        let driver = SubAgentWorkflowDriver::new(
            run_id.clone(),
            "session-test".to_string(),
            manager,
            runtime,
            state.clone(),
            None,
            WorkflowFleetBinding::None,
            Vec::new(),
            tmp.path().to_path_buf(),
        );

        let operation = exact_workflow(EXACT_GLM_FLEET);
        let mut request = exact_write_task_request("builder");
        let binding =
            bind_exact_fleet_task_request(&operation, exact_session(), &mut request).expect("bind");
        let receipt = route_admitted_exact_task(&operation, &binding, &mut request)
            .await
            .expect("routing");

        driver.record_orphaned_fleet_receipt(&receipt, "Sub-agent depth limit reached");

        let events = state
            .runs
            .lock()
            .expect("runs")
            .get(&run_id)
            .expect("run")
            .events
            .clone();
        let logged = events
            .iter()
            .filter_map(|event| match &event.kind {
                WorkflowUiEventKind::Log { message } => Some(message.clone()),
                _ => None,
            })
            .find(|message| message.contains("spawn_failed=true"))
            .expect("the receipt must outlive the failed spawn");

        assert!(logged.contains("member=implementer"), "{logged}");
        assert!(logged.contains("source=fleet_router"), "{logged}");
        assert!(
            logged.contains("reasoning_router:workspace/luna-low"),
            "{logged}"
        );
        assert!(logged.contains("Sub-agent depth limit reached"), "{logged}");
        // Still content-free: a failure line is no excuse to echo the task.
        assert!(!logged.contains("land the fix"), "{logged}");
    }

    /// A member's semantic role is what the operator named and what gates key
    /// on; the roster profile's role is the Runtime **posture** selected after
    /// identity resolution. The started event must show the first, not the
    /// second.
    #[tokio::test]
    async fn a_started_event_shows_the_members_role_not_its_permission_posture() {
        const AUDIT_FLEET: &str = r#"
name = "glm-pair"
schema = "exact"

[[members]]
id = "auditor"
role = "auditor"
provider = "zai"
model = "glm-5"
reasoning = "high"
permissions = "read_only"
"#;
        let operation = exact_workflow_with(AUDIT_FLEET, None);
        // The member declares `permissions = "read_only"`, and an undeclared
        // role name no longer hands the child a write-capable posture (#5575),
        // so a declared write scope would be dropped at bind time. Ask for no
        // write scope, which is what this member actually has.
        let mut request = exact_task_request("auditor");
        let binding =
            bind_exact_fleet_task_request(&operation, exact_session(), &mut request).expect("bind");
        let receipt = route_admitted_exact_task(&operation, &binding, &mut request)
            .await
            .expect("routing");

        // The binding authority — and therefore the spawn metadata — carries
        // the posture role, because that is what picked the child's tool
        // surface.
        let posture = binding.authority.posture_role.to_string();
        assert_eq!(posture, "explore");
        assert_eq!(receipt.posture_role.as_deref(), Some("explore"));

        // What the run displays is the member's role, not that posture.
        assert_eq!(
            displayed_resolved_role(Some(&receipt), Some(&posture), request.role.as_deref()),
            Some("auditor".to_string()),
            "the panel must not rename the operator's member to its posture"
        );

        // A non-Fleet task keeps the previous precedence untouched.
        assert_eq!(
            displayed_resolved_role(None, Some("builder"), Some("reviewer")),
            Some("builder".to_string())
        );
        assert_eq!(
            displayed_resolved_role(None, None, Some("reviewer")),
            Some("reviewer".to_string())
        );
    }

    /// Binding never refuses a task over write scope. A write-capable member
    /// with no declared scope binds unchanged, and the spawn boundary defaults
    /// it to the workspace root just as it does for a plain Agent spawn; a
    /// member that resolves read-only has any declared scope dropped, because
    /// it could never hold that claim and the script cannot predict every
    /// member's resolved posture.
    #[test]
    fn exact_fleet_binding_defers_write_scope_to_the_spawn_boundary() {
        let router = crate::fleet::exact::StaticFleetRouter::new(r#"{"reasoning":"max"}"#);
        let operation = exact_workflow_with(EXACT_GLM_FLEET, Some(router.clone()));

        // (a) Write-capable member, no declared scope: binds, scope untouched.
        let mut unbounded = exact_task_request("builder");
        bind_exact_fleet_task_request(&operation, exact_session(), &mut unbounded)
            .expect("an unscoped writer binds; the spawn boundary supplies '.'");
        assert_ne!(unbounded.write_authority.as_deref(), Some("read_only"));
        assert!(unbounded.write_roots.is_empty());
        assert!(unbounded.exact_files.is_empty());
        assert!(unbounded.coordination_contracts.is_empty());

        // (b) Read-only member declaring a write scope: the scope is dropped,
        // not refused, so the spawn contract cannot trip over it later.
        let mut scoped_read_only = exact_task_request("reviewer");
        scoped_read_only.write_roots = vec!["crates/tui".to_string()];
        scoped_read_only.exact_files = vec!["crates/tui/src/main.rs".to_string()];
        scoped_read_only.coordination_contracts = vec!["api".to_string()];
        bind_exact_fleet_task_request(&operation, exact_session(), &mut scoped_read_only)
            .expect("a read-only member's declared scope is dropped, not refused");
        assert_eq!(
            scoped_read_only.write_authority.as_deref(),
            Some("read_only")
        );
        assert!(scoped_read_only.write_roots.is_empty());
        assert!(scoped_read_only.exact_files.is_empty());
        assert!(scoped_read_only.coordination_contracts.is_empty());

        // A declared scope on a writer is kept as written.
        let mut bounded = exact_write_task_request("builder");
        let declared = bounded.write_roots.clone();
        bind_exact_fleet_task_request(&operation, exact_session(), &mut bounded)
            .expect("a bounded write claim is valid");
        assert_eq!(bounded.write_roots, declared);

        // Binding never contacts the router.
        assert_eq!(router.call_count(), 0);
    }

    /// The parent posture wins over the saved Fleet, in the request the child
    /// actually receives.
    #[test]
    fn a_read_only_session_narrows_a_write_capable_exact_member() {
        let operation = exact_workflow(EXACT_GLM_FLEET);
        let session = codewhale_workflow::PermissionCeiling {
            write: false,
            network_tool: false,
            shell: codewhale_workflow::ShellCeiling::ReadOnly,
            delegation_depth: 0,
            tools: true,
        };

        let mut request = exact_task_request("builder");
        bind_exact_fleet_task_request(&operation, session, &mut request).expect("bind");

        assert_eq!(
            request.write_authority.as_deref(),
            Some("read_only"),
            "a saved read_write member must not write inside a read-only session"
        );
        assert_eq!(request.max_depth, Some(0));
    }

    /// A task that never reaches admission must never reach the Router.
    #[test]
    fn a_rejected_or_unadmitted_task_spends_no_router_call() {
        let router = crate::fleet::exact::StaticFleetRouter::new(r#"{"reasoning":"max"}"#);
        let operation = exact_workflow_with(EXACT_GLM_FLEET, Some(router.clone()));

        // Rejected by an override check, before the member is even resolved.
        let mut overridden = exact_task_request("builder");
        overridden.model = Some("glm-4".to_string());
        assert!(
            bind_exact_fleet_task_request(&operation, exact_session(), &mut overridden).is_err()
        );

        // Rejected by member resolution.
        let mut unknown = exact_task_request("wizard");
        assert!(bind_exact_fleet_task_request(&operation, exact_session(), &mut unknown).is_err());

        // Admitted-shaped but never routed: the capacity-blocked case.
        let mut queued = exact_write_task_request("builder");
        bind_exact_fleet_task_request(&operation, exact_session(), &mut queued).expect("bind");

        assert_eq!(
            router.call_count(),
            0,
            "binding must never contact the reasoning router"
        );
    }

    /// The routing decision must survive the run as a durable, visible receipt
    /// that carries no task content.
    #[tokio::test]
    async fn an_exact_fleet_launch_produces_a_durable_routing_receipt() {
        let operation = exact_workflow(EXACT_GLM_FLEET);
        let mut request = exact_write_task_request("builder");

        let binding =
            bind_exact_fleet_task_request(&operation, exact_session(), &mut request).expect("bind");
        let receipt = route_admitted_exact_task(&operation, &binding, &mut request)
            .await
            .expect("routing");

        assert_eq!(receipt.fleet, "workspace/glm-pair");
        assert_eq!(receipt.member_id, "implementer");
        assert_eq!(receipt.member_role, "implement");
        assert_eq!(receipt.provider, "zai");
        assert_eq!(receipt.model, "glm-5");
        assert_eq!(receipt.requested_reasoning, "auto");
        assert_eq!(receipt.effective_reasoning, "max");
        assert_eq!(receipt.selection_source, "fleet_router");
        let router = receipt.router.as_ref().expect("router identity");
        assert_eq!(router.service_kind, "reasoning_router");
        assert_eq!(router.qualified(), "workspace/luna-low");
        let call = router.call.as_ref().expect("call disclosure");
        assert_eq!(call.requested, "low");
        assert_eq!(call.effective, "low");

        // It rides the durable task_started event, and older consumers that
        // never saw this field still deserialize.
        let event = WorkflowUiEvent::new(
            "session-test",
            WorkflowUiEventKind::TaskStarted(Box::new(WorkflowTaskStartedEvent {
                task_id: "t1".to_string(),
                label: None,
                role: request.role.clone(),
                profile: request.profile.clone(),
                model: None,
                strength: None,
                thinking: request.thinking.clone(),
                // The #4039 reasoning fields carry the same requested →
                // effective pair the receipt records, so the event and its
                // receipt cannot disagree about what the child ran at.
                requested_reasoning: Some(receipt.requested_reasoning.clone()),
                effective_reasoning: Some(receipt.effective_reasoning.clone()),
                resolved_role: None,
                resolved_profile: None,
                resolved_provider: "zai".to_string(),
                resolved_model: "glm-5".to_string(),
                route_source: "fleet".to_string(),
                child_route: None,
                worktree: false,
                workspace: None,
                git_branch: None,
                parent_task_id: None,
                depth: 0,
                workflow_run_id: None,
                workflow_phase_id: None,
                workflow_task_label: None,
                workflow_child_index: None,
                queue_ticket: None,
                fleet_receipt: Some(receipt.clone()),
            })),
        );
        let payload = serde_json::to_value(&event).expect("serialize");
        let rendered = payload.to_string();
        for expected in [
            "fleet_receipt",
            "\"selection_source\":\"fleet_router\"",
            "\"provider_effective_reasoning\"",
            "\"requested_reasoning\":\"auto\"",
            "gpt-5.6-luna",
            "\"service_kind\":\"reasoning_router\"",
        ] {
            assert!(
                rendered.contains(expected),
                "{expected} missing: {rendered}"
            );
        }
        // No absolute paths, secrets, or task text on a durable event.
        for forbidden in ["/Users/", "/home/", ".toml", "api_key", "land the fix"] {
            assert!(!rendered.contains(forbidden), "{forbidden} in {rendered}");
        }

        // The visible one-line form names every side of the decision.
        let line = receipt.line();
        for expected in [
            "requested=auto",
            "effective=max",
            "source=fleet_router",
            "reasoning_router:workspace/luna-low",
            "router_call_requested=low",
        ] {
            assert!(line.contains(expected), "{expected} missing from {line}");
        }
    }

    #[test]
    fn exact_fleet_rejects_an_unknown_member() {
        let operation = exact_workflow(EXACT_GLM_FLEET);
        let mut request = exact_task_request("wizard");

        let err = bind_exact_fleet_task_request(&operation, exact_session(), &mut request)
            .expect_err("unknown member");
        let message = format!("{err:?}");
        assert!(message.contains("wizard"), "{message}");
        assert!(message.contains("implementer"), "{message}");

        // The Reasoning Router is never dispatchable as a worker.
        let mut router_request = exact_task_request("luna-low");
        assert!(
            bind_exact_fleet_task_request(&operation, exact_session(), &mut router_request)
                .is_err(),
            "the reasoning router must not be launchable as a worker"
        );
    }

    /// One saved Router profile, referenced by two different Fleets.
    #[tokio::test]
    async fn one_reasoning_router_profile_serves_two_fleets() {
        let first = exact_workflow(EXACT_GLM_FLEET);
        let second = {
            let text = EXACT_GLM_FLEET.replace("name = \"glm-pair\"", "name = \"glm-solo\"");
            let document =
                codewhale_workflow::FleetDocument::parse(&text).expect("second fleet parses");
            crate::fleet::exact::ExactFleetWorkflow::for_tests(
                &document,
                codewhale_workflow::QualifiedFleetId {
                    name: "glm-solo".to_string(),
                    origin: "workspace".to_string(),
                },
                Some(crate::fleet::exact::StaticFleetRouter::new(
                    r#"{"reasoning":"low"}"#,
                )),
            )
        };

        assert_eq!(
            first.snapshot().router(),
            second.snapshot().router(),
            "both fleets reference the identical captured router service"
        );
        assert_ne!(
            first.snapshot().fleet().qualified(),
            second.snapshot().fleet().qualified()
        );

        // Both actually route through it, and both receipts name it.
        for (operation, expected) in [(&first, "max"), (&second, "low")] {
            let mut request = exact_write_task_request("builder");
            let binding = bind_exact_fleet_task_request(operation, exact_session(), &mut request)
                .expect("bind");
            let receipt = route_admitted_exact_task(operation, &binding, &mut request)
                .await
                .expect("routing");
            assert_eq!(receipt.effective_reasoning, expected);
            assert_eq!(
                receipt.router.as_ref().expect("router").qualified(),
                "workspace/luna-low"
            );
        }
    }

    /// Editing the saved Fleet mid-run must not move the running Workflow's
    /// routes. The snapshot owns copies; only the next Workflow sees the edit.
    #[tokio::test]
    async fn editing_the_fleet_file_after_start_does_not_move_a_running_route() {
        let tmp = tempfile::tempdir().expect("tmp");
        let fleets = tmp.path().join("fleets");
        std::fs::create_dir_all(&fleets).expect("fleets dir");
        let path = fleets.join("glm-pair.toml");
        std::fs::write(&path, EXACT_GLM_FLEET).expect("write fleet");

        let roots = vec![codewhale_workflow::FleetSearchRoot::new(
            "workspace",
            tmp.path(),
        )];
        let (document, id) =
            codewhale_workflow::FleetDocument::load_by_name("glm-pair", &roots).expect("load");
        let operation = crate::fleet::exact::ExactFleetWorkflow::for_tests(
            &document,
            id,
            Some(crate::fleet::exact::StaticFleetRouter::new(
                r#"{"reasoning":"max"}"#,
            )),
        );
        let started_hash = operation.snapshot().content_hash().to_string();

        // The operator rewrites the saved Fleet mid-run.
        std::fs::write(
            &path,
            EXACT_GLM_FLEET
                .replace(
                    "model = \"glm-5\"\nreasoning = \"auto\"",
                    "model = \"glm-4\"\nreasoning = \"off\"",
                )
                .replace("permissions = \"read_write\"", "permissions = \"full\""),
        )
        .expect("rewrite fleet");

        let mut request = exact_write_task_request("builder");
        let binding = bind_exact_fleet_task_request(&operation, exact_session(), &mut request)
            .expect("bind after the edit");
        route_admitted_exact_task(&operation, &binding, &mut request)
            .await
            .expect("launch after the edit");

        let member = operation
            .snapshot()
            .member("implementer")
            .expect("snapshot entry");
        assert_eq!(
            member.route.model, "glm-5",
            "the in-flight snapshot must keep the model it started with"
        );
        assert_eq!(request.thinking.as_deref(), Some("max"));
        assert_eq!(
            request.write_authority.as_deref(),
            Some("workspace_write"),
            "the edit must not widen the running ceiling to `full`"
        );
        assert_eq!(operation.snapshot().content_hash(), started_hash);

        // A fresh Workflow does see the edit.
        let (reloaded, reloaded_id) =
            codewhale_workflow::FleetDocument::load_by_name("glm-pair", &roots).expect("reload");
        let next = crate::fleet::exact::ExactFleetWorkflow::for_tests(
            &reloaded,
            reloaded_id,
            Some(crate::fleet::exact::StaticFleetRouter::new(
                r#"{"reasoning":"max"}"#,
            )),
        );
        assert_eq!(
            next.snapshot()
                .member("implementer")
                .expect("snapshot entry")
                .route
                .model,
            "glm-4"
        );
        assert_ne!(next.snapshot().content_hash(), started_hash);
    }

    #[test]
    fn named_fleet_rejects_unknown_workflow_role_before_spawn() {
        let fleet = FleetRoleMap::from_pairs([("scout", "scout")]).expect("fleet");
        let mut request = TaskRequest {
            description: "fix it".to_string(),
            subagent_type: None,
            role: Some("wizard".to_string()),
            profile: None,
            model: None,
            model_strength: None,
            thinking: None,
            cwd: None,
            worktree: false,
            write_authority: Some("read_only".to_string()),
            write_roots: Vec::new(),
            exact_files: Vec::new(),
            coordination_contracts: Vec::new(),
            dependencies: Vec::new(),
            acceptance: Vec::new(),
            allowed_tools: None,
            disallowed_tools: Vec::new(),
            max_depth: None,
            token_budget: None,
            max_steps: None,
            wall_time_secs: None,
            response_schema: None,
            schema_repair_attempts: None,
            label: None,
            phase: None,
        };

        let err = apply_named_fleet_to_task_request(Some(&fleet), &mut request)
            .expect_err("unknown role should fail");
        assert!(
            err.to_string().contains("unknown fleet role `wizard`"),
            "{err}"
        );
    }

    #[test]
    fn declarative_leaf_budget_reaches_task_runtime_options() {
        let source = r#"
workflow({
  "goal": "bound one child",
  "nodes": [{
    "agent": {
      "id": "bounded",
      "prompt": "Inspect bounded evidence.",
      "budget": { "max_tokens": 5000, "max_steps": 4, "timeout_secs": 90 }
    }
  }]
});
"#;

        let adapted = adapt_workflow_source(source, None).expect("lower bounded leaf");
        assert!(
            adapted.source.contains("tokenBudget: 5000"),
            "{}",
            adapted.source
        );
        assert!(adapted.source.contains("maxSteps: 4"), "{}", adapted.source);
        assert!(
            adapted.source.contains("wallTimeSecs: 90"),
            "{}",
            adapted.source
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn declarative_max_steps_zero_runs_without_a_turn_cap() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("Completed without a turn cap.").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(manager, runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"
                    workflow({
                      "goal": "prove zero means an unbounded child loop",
                      "nodes": [{
                        "agent": {
                          "id": "zero-step",
                          "prompt": "Complete this task.",
                          "budget": { "max_steps": 0, "timeout_secs": 90 }
                        }
                      }]
                    });
                    "#
                }),
                &ctx,
            )
            .await
            .expect("workflow returns its terminal receipt");
        let payload: Value = serde_json::from_str(&result.content).expect("workflow JSON");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an unbounded child must be allowed to start a model turn"
        );
        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(
            payload["execution"]["leaf_results"][0]["status"], "succeeded",
            "{payload}"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn role_only_leaf_omits_type_and_resolves_through_named_fleet() {
        let _retry_guard = workflow_test_retry_guard();
        let _env_lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let fleet_dir = tmp.path().join("fleets");
        std::fs::create_dir_all(&fleet_dir).expect("fleet dir");
        std::fs::write(
            fleet_dir.join("role-only-test.toml"),
            r#"
name = "role-only-test"

[roles]
scout = "scout"
reviewer = "reviewer"
"#,
        )
        .expect("role-only fleet");
        let source = r#"
export default workflow({
  "goal": "resolve a role-only child",
  "nodes": [
    {
      "agent": {
        "id": "scout-source",
        "prompt": "Inspect the source without editing.",
        "role": "scout",
        "mode": "read_only"
      }
    }
  ]
});
"#;

        let adapted = adapt_workflow_source(source, None).expect("lower role-only workflow");
        assert!(adapted.source.contains("role: \"scout\""));
        assert!(
            !adapted.source.contains("type:"),
            "Fleet-addressed leaves must defer runtime type to the roster:\n{}",
            adapted.source
        );
        let non_role = adapt_workflow_source(
            r#"workflow({
              "goal": "default non-role child",
              "nodes": [{ "agent": { "id": "audit", "prompt": "Audit only." } }]
            });"#,
            None,
        )
        .expect("lower non-role workflow");
        assert!(
            non_role.source.contains("type: \"review\""),
            "non-role read-only leaves retain the review default:\n{}",
            non_role.source
        );
        let explicit_type_source = r#"
workflow({
  "goal": "preserve an authored role type",
  "nodes": [{
    "agent": {
      "id": "review-source",
      "prompt": "Review the source without editing.",
      "agent_type": "review",
      "role": "reviewer",
      "mode": "read_only"
    }
  }]
});
"#;
        let explicit_type = adapt_workflow_source(explicit_type_source, None)
            .expect("lower explicitly typed Fleet role");
        assert!(
            explicit_type.source.contains("type: \"review\""),
            "an authored non-General type must remain a validated override:\n{}",
            explicit_type.source
        );

        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("scout evidence").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);
        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": source,
                    "fleet": "role-only-test"
                }),
                &ctx,
            )
            .await
            .expect("role-only workflow should resolve through its named Fleet");
        let payload: Value = serde_json::from_str(&result.content).expect("workflow JSON");

        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let started = payload["events"]
            .as_array()
            .expect("typed events")
            .iter()
            .find(|event| event["type"] == "task_started")
            .expect("task_started receipt");
        assert_eq!(started["role"], "explore");
        assert_eq!(started["profile"], "scout");
        assert_eq!(started["resolved_profile"], "scout");

        let explicit_result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": explicit_type_source,
                    "fleet": "role-only-test"
                }),
                &ctx,
            )
            .await
            .expect("matching explicit role type should remain valid");
        let explicit_payload: Value =
            serde_json::from_str(&explicit_result.content).expect("workflow JSON");
        assert_eq!(
            explicit_payload["status"], "completed",
            "{explicit_payload}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let conflicting_result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"workflow({
                      "goal": "reject a conflicting authored type",
                      "nodes": [{ "agent": {
                        "id": "bad-scout",
                        "prompt": "Review as a scout.",
                        "agent_type": "review",
                        "role": "scout",
                        "mode": "read_only"
                      } }]
                    });"#,
                    "fleet": "role-only-test"
                }),
                &ctx,
            )
            .await
            .expect("conflicting type returns a terminal workflow record");
        let conflicting_payload: Value =
            serde_json::from_str(&conflicting_result.content).expect("workflow JSON");
        assert_eq!(
            conflicting_payload["status"], "failed",
            "{conflicting_payload}"
        );
        assert!(
            conflicting_payload["error"]
                .as_str()
                .is_some_and(|error| error
                    .contains("Fleet role conflicts with the explicit legacy agent type")),
            "{conflicting_payload}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "conflicting explicit type must fail before the provider"
        );
    }

    #[test]
    fn parallel_write_children_default_to_worktree_isolation() {
        // #4120: write-capable parallel leaves get worktree: true by default.
        let source = r#"
export default workflow({
  "goal": "parallel write isolation default",
  "nodes": [
    {
      "branch": {
        "id": "implement",
        "parallel": true,
        "children": [
          {
            "agent": {
              "id": "left",
              "prompt": "Patch left lane",
              "agent_type": "implementer",
              "mode": "read_write",
              "file_scope": ["src/left.rs"]
            }
          },
          {
            "agent": {
              "id": "right",
              "prompt": "Patch right lane",
              "agent_type": "implementer",
              "mode": "read_write",
              "file_scope": ["src/right.rs"]
            }
          }
        ]
      }
    }
  ]
});
"#;
        let adapted = adapt_workflow_source(source, None).expect("lower parallel write workflow");
        let spec = adapted.spec.expect("declarative spec");
        let WorkflowNode::BranchSet(branch) = &spec.nodes[0] else {
            panic!("expected branch_set");
        };
        assert!(branch.parallel);
        for child in &branch.children {
            let WorkflowNode::Leaf(leaf) = child else {
                panic!("expected leaf");
            };
            assert!(leaf_is_write_capable(leaf));
            assert!(
                leaf_wants_worktree(leaf, true),
                "parallel write leaf {} should default to worktree",
                leaf.id
            );
            assert_eq!(leaf.isolation, IsolationMode::Auto);
        }
        assert!(
            adapted.source.contains("worktree: true"),
            "lowered JS should request worktree isolation:\n{}",
            adapted.source
        );
        // Both parallel children should carry the worktree flag.
        assert_eq!(
            adapted.source.matches("worktree: true").count(),
            2,
            "each parallel write child should get worktree: true:\n{}",
            adapted.source
        );
        assert_eq!(
            adapted
                .source
                .matches("writeAuthority: \"worktree_write\"")
                .count(),
            2,
            "each isolated writer should carry enforced worktree authority:\n{}",
            adapted.source
        );
        assert!(adapted.source.contains("writeRoots: [\"src/left.rs\"]"));
        assert!(adapted.source.contains("writeRoots: [\"src/right.rs\"]"));
    }

    #[test]
    fn parallel_write_same_worktree_requires_explicit_shared_isolation() {
        // #4120: isolation: shared is the approved same-worktree override.
        let source = r#"
export default workflow({
  "goal": "parallel write same-worktree override",
  "nodes": [
    {
      "branch": {
        "id": "implement",
        "parallel": true,
        "children": [
          {
            "agent": {
              "id": "shared-writer",
              "prompt": "Patch in the parent checkout",
              "agent_type": "implementer",
              "mode": "read_write",
              "isolation": "shared",
              "file_scope": ["src/shared.rs"]
            }
          },
          {
            "agent": {
              "id": "isolated-writer",
              "prompt": "Patch in a worktree",
              "agent_type": "implementer",
              "mode": "read_write",
              "isolation": "worktree",
              "file_scope": ["src/isolated.rs"]
            }
          }
        ]
      }
    }
  ]
});
"#;
        let adapted =
            adapt_workflow_source(source, None).expect("lower same-worktree override workflow");
        let spec = adapted.spec.expect("declarative spec");
        let WorkflowNode::BranchSet(branch) = &spec.nodes[0] else {
            panic!("expected branch_set");
        };
        let leaves: Vec<&LeafSpec> = branch
            .children
            .iter()
            .map(|child| match child {
                WorkflowNode::Leaf(leaf) => leaf,
                _ => panic!("expected leaf"),
            })
            .collect();
        assert_eq!(leaves[0].isolation, IsolationMode::Shared);
        assert!(
            !leaf_wants_worktree(leaves[0], true),
            "explicit shared should keep same-worktree"
        );
        assert_eq!(leaves[1].isolation, IsolationMode::Worktree);
        assert!(leaf_wants_worktree(leaves[1], true));

        // Only the explicit worktree child should emit worktree: true.
        assert_eq!(
            adapted.source.matches("worktree: true").count(),
            1,
            "same-worktree override must not force worktree on shared leaf:\n{}",
            adapted.source
        );
        assert!(
            adapted.source.contains("shared-writer") && adapted.source.contains("isolated-writer"),
            "both children should still be lowered:\n{}",
            adapted.source
        );
        assert!(
            adapted
                .source
                .contains("writeAuthority: \"workspace_write\"")
        );
        assert!(
            adapted
                .source
                .contains("writeAuthority: \"worktree_write\"")
        );
    }

    #[test]
    fn leaf_absolute_cwd_inside_the_workspace_lowers_repo_relative() {
        // Founder run: a plan leaf with cwd "/Volumes/VIXinSSD/CW/codewhale"
        // was refused although it named the workspace's own checkout.
        // Platform-absolute paths: `/Volumes/…` is not absolute on Windows.
        let workspace = std::env::temp_dir().join("cw-leaf-fixture");
        let leaf: LeafSpec = serde_json::from_value(json!({
            "id": "engine-readiness",
            "prompt": "Audit release readiness",
            "agent_type": "review",
            "mode": "read_only",
            "cwd": workspace.join("codewhale").to_str().unwrap()
        }))
        .expect("leaf");
        let source = leaf_task_options_expression(&leaf, None, false, Some(&workspace)).unwrap();
        assert!(source.contains("cwd: \"codewhale\""), "{source}");
        assert!(!source.contains("cw-leaf-fixture"), "{source}");

        let mut outside = leaf.clone();
        outside.cwd = Some(
            std::env::temp_dir()
                .join("cw-leaf-fixture-other")
                .to_str()
                .unwrap()
                .to_string(),
        );
        let error = leaf_task_options_expression(&outside, None, false, Some(&workspace))
            .unwrap_err()
            .to_string();
        assert!(error.contains("outside the workspace"), "{error}");
        // Without a known workspace the old refusal stands.
        let error = leaf_task_options_expression(&leaf, None, false, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("bounded repo-relative paths"), "{error}");
    }

    #[test]
    fn read_only_workflow_leaves_use_the_runtime_grant_unless_explicitly_narrowed() {
        for agent_type in ["explore", "review", "verifier", "general"] {
            let mut leaf: LeafSpec = serde_json::from_value(json!({
                "id": "inspect",
                "prompt": "Inspect source and CI evidence",
                "agent_type": agent_type,
                "mode": "read_only"
            }))
            .expect("read-only leaf");
            assert_eq!(leaf_allowed_tools(&leaf).unwrap(), None);
            let source = leaf_task_options_expression(&leaf, None, false, None).unwrap();
            assert!(source.contains("writeAuthority: \"read_only\""), "{source}");
            assert!(!source.contains("allowedTools:"), "{source}");

            leaf.permissions.deny_all_tools = true;
            assert_eq!(leaf_allowed_tools(&leaf).unwrap(), Some(Vec::new()));
            leaf.permissions.deny_all_tools = false;
            leaf.permissions.allowed_tools = vec!["File".into()];
            assert_eq!(
                leaf_allowed_tools(&leaf).unwrap(),
                Some(vec!["File".into()])
            );
            leaf.permissions.allowed_tools = vec!["bash".into()];
            assert!(validate_leaf_runtime_contract(&leaf).is_ok());
            leaf.permissions.allowed_tools = vec!["exec_shell".into()];
            assert!(validate_leaf_runtime_contract(&leaf).is_err());
        }
    }

    #[test]
    fn parallel_read_only_children_do_not_default_to_worktree() {
        let source = r#"
export default workflow({
  "goal": "parallel read-only stays shared",
  "nodes": [
    {
      "branch": {
        "id": "audit",
        "parallel": true,
        "children": [
          {
            "agent": {
              "id": "review-a",
              "prompt": "Review A",
              "agent_type": "review",
              "mode": "read_only"
            }
          },
          {
            "agent": {
              "id": "review-b",
              "prompt": "Review B",
              "agent_type": "verifier",
              "mode": "read_only"
            }
          }
        ]
      }
    }
  ]
});
"#;
        let adapted = adapt_workflow_source(source, None).expect("lower parallel read-only");
        assert!(
            !adapted.source.contains("worktree: true"),
            "read-only parallel children should not get worktree isolation:\n{}",
            adapted.source
        );
        assert_eq!(
            adapted
                .source
                .matches("writeAuthority: \"read_only\"")
                .count(),
            2,
            "read-only mode must reach the task authority contract:\n{}",
            adapted.source
        );
    }

    #[test]
    fn sequential_write_children_do_not_default_to_worktree() {
        let source = r#"
export default workflow({
  "goal": "sequential write stays shared by default",
  "nodes": [
    {
      "sequence": {
        "id": "implement",
        "children": [
          {
            "agent": {
              "id": "writer",
              "prompt": "Patch sequentially",
              "agent_type": "implementer",
              "mode": "read_write",
              "file_scope": ["src/main.rs"]
            }
          }
        ]
      }
    }
  ]
});
"#;
        let adapted = adapt_workflow_source(source, None).expect("lower sequential write");
        assert!(
            !adapted.source.contains("worktree: true"),
            "sequential writes should not default to worktree:\n{}",
            adapted.source
        );
        assert!(
            adapted
                .source
                .contains("writeAuthority: \"workspace_write\"")
        );
        assert!(adapted.source.contains("writeRoots: [\"src/main.rs\"]"));
    }

    #[test]
    fn write_scope_suffix_globs_lower_to_enforceable_roots() {
        let source = r#"workflow({
          "goal": "bounded auth patch",
          "nodes": [{ "agent": {
            "id": "writer",
            "prompt": "Patch auth",
            "agent_type": "implementer",
            "mode": "read_write",
            "file_scope": ["./src/auth/**"]
          }}]
        });"#;
        let adapted = adapt_workflow_source(source, None).expect("lower trailing glob scope");
        assert!(
            adapted.source.contains("writeRoots: [\"src/auth\"]"),
            "runtime claim must contain src/auth/login.rs:\n{}",
            adapted.source
        );

        let unsupported = r#"workflow({
          "goal": "reject ambiguous glob",
          "nodes": [{ "agent": {
            "id": "writer",
            "prompt": "Patch auth",
            "agent_type": "implementer",
            "mode": "read_write",
            "file_scope": ["src/*/auth"]
          }}]
        });"#;
        let error = match adapt_workflow_source(unsupported, None) {
            Ok(_) => panic!("internal globs cannot become literal runtime roots"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("unsupported file_scope"), "{error}");
    }

    #[test]
    fn write_leaves_require_scope_and_reduce_stays_read_only() {
        let unscoped_writer = r#"workflow({
          "goal": "reject an unbounded writer",
          "nodes": [{ "agent": {
            "id": "writer",
            "prompt": "Patch it",
            "agent_type": "implementer",
            "mode": "read_write"
          }}]
        });"#;
        let error = match adapt_workflow_source(unscoped_writer, None) {
            Ok(_) => panic!("write-capable leaves must declare file_scope"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("declares no file_scope"), "{error}");

        let reduce = r#"workflow({
          "goal": "reduce read-only evidence",
          "nodes": [{ "reduce": {
            "id": "summary",
            "inputs": [],
            "prompt": "Summarize the evidence"
          }}]
        });"#;
        let adapted = adapt_workflow_source(reduce, None).expect("lower reduce");
        assert!(adapted.source.contains("type: \"plan\""));
        assert!(adapted.source.contains("writeAuthority: \"read_only\""));
    }

    #[test]
    fn inline_script_and_source_path_are_mutually_exclusive() {
        let ctx = ToolContext::new(".");
        let err = workflow_source(
            &json!({
                "script": "return 1;",
                "source_path": "workflow.js"
            }),
            &ctx,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("exactly one of script, source_path, or plan"),
            "{err}"
        );
    }

    #[test]
    fn structured_plan_lowers_to_parallel_not_promise_all() {
        // #4124: planner plan → JS with parallel() partial-success semantics.
        let ctx = ToolContext::new(".");
        let source = workflow_source(
            &json!({
                "plan": {
                    "goal": "audit two independent scopes",
                    "risk": "read_only",
                    "max_children": 8,
                    "token_budget": 120000,
                    "phases": [{
                        "id": "scout",
                        "title": "Scout",
                        "children": [
                            {
                                "id": "left",
                                "label": "left-lane",
                                "prompt": "Inspect crates/left",
                                "type": "explore"
                            },
                            {
                                "id": "right",
                                "prompt": "Inspect crates/right",
                                "type": "explore"
                            }
                        ]
                    }]
                }
            }),
            &ctx,
        )
        .expect("structured plan should lower");

        assert!(
            source.source.contains("await parallel(["),
            "lowered JS must use parallel():\n{}",
            source.source
        );
        assert!(
            !source.source.contains("Promise.all"),
            "lowered JS must not use raw Promise.all:\n{}",
            source.source
        );
        assert!(
            source.source.contains("() => task("),
            "parallel slots should be thunks:\n{}",
            source.source
        );
        let spec = source.spec.expect("plan should produce WorkflowSpec");
        assert_eq!(spec.goal, "audit two independent scopes");
        assert_eq!(spec.budget.max_tokens, Some(120000));
        assert_eq!(spec.nodes.len(), 1);
        let WorkflowNode::BranchSet(branch) = &spec.nodes[0] else {
            panic!("expected parallel branch for multi-child phase");
        };
        assert!(branch.parallel);
        assert_eq!(branch.children.len(), 2);
    }

    #[test]
    fn structured_plan_child_cwd_lowers_to_task_cwd() {
        // #6232: plan children accept `cwd` (repository-relative, like
        // task({cwd})) so multi-repo workspaces can disambiguate the child
        // repository — including the worktree root the parallel-write default
        // (#4120) resolves. A blank value is refused, never lowered.
        let ctx = ToolContext::new(".");
        let source = workflow_source(
            &json!({
                "plan": {
                    "goal": "patch two repos",
                    "risk": "writes",
                    "phases": [{
                        "id": "build",
                        "children": [
                            {
                                "id": "a",
                                "prompt": "Patch repo A",
                                "type": "implement",
                                "file_scope": ["src/**"],
                                "cwd": "repos/a"
                            },
                            {
                                "id": "b",
                                "prompt": "Patch repo B",
                                "type": "implement",
                                "file_scope": ["src/**"],
                                "cwd": "repos/b"
                            }
                        ]
                    }]
                }
            }),
            &ctx,
        )
        .expect("structured plan with child cwd should lower");
        assert!(
            source.source.contains(r#"cwd: "repos/a""#),
            "child cwd must reach the lowered task() call:\n{}",
            source.source
        );
        assert!(
            source.source.contains(r#"cwd: "repos/b""#),
            "child cwd must reach the lowered task() call:\n{}",
            source.source
        );

        let blank = workflow_source(
            &json!({
                "plan": {
                    "goal": "blank cwd",
                    "risk": "read_only",
                    "children": [
                        {
                            "id": "blank",
                            "prompt": "Inspect",
                            "type": "explore",
                            "cwd": "  "
                        }
                    ]
                }
            }),
            &ctx,
        );
        let err = blank
            .expect_err("blank child cwd must be refused")
            .to_string();
        assert!(err.contains("cwd"), "{err}");
    }

    #[tokio::test]
    async fn structured_plan_child_cwd_rejected_before_start() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        );
        let tool = WorkflowTool::new(manager, runtime);
        for cwd in [
            "/absolute/repo",
            "../sibling",
            "repo/../sibling",
            "//server/share",
            r"C:\repo",
            "repo\nchild",
        ] {
            let error = tool
                .execute(
                    json!({
                        "action": "start",
                        "plan": {
                            "goal": "inspect a repo",
                            "children": [{
                                "id": "inspect",
                                "prompt": "Read the README",
                                "type": "explore",
                                "cwd": cwd
                            }]
                        }
                    }),
                    &ctx,
                )
                .await
                .expect_err("invalid cwd must fail the call, not create a background run");
            let error = error.to_string();
            assert!(
                error.contains("inspect") && error.contains("cwd"),
                "{cwd:?}: {error}"
            );
        }
    }

    #[test]
    fn structured_plan_child_cwd_uses_dispatch_normalization() {
        let source = workflow_source(
            &json!({
                "plan": {
                    "goal": "inspect a repo",
                    "children": [{
                        "prompt": "Read the README",
                        "type": "explore",
                        "cwd": r" ./repos\a// "
                    }]
                }
            }),
            &ToolContext::new("."),
        )
        .expect("bounded cwd should normalize before launch");
        assert!(
            source.source.contains(r#"cwd: "repos/a""#),
            "{}",
            source.source
        );
    }

    #[test]
    fn structured_plan_validation_errors_are_typed() {
        let ctx = ToolContext::new(".");
        let missing_goal = workflow_source(
            &json!({
                "plan": {
                    "goal": "   ",
                    "children": [{ "prompt": "do work" }]
                }
            }),
            &ctx,
        )
        .unwrap_err();
        assert!(missing_goal.to_string().contains("goal"), "{missing_goal}");

        let over_limit = workflow_source(
            &json!({
                "plan": {
                    "goal": "too many children",
                    "max_children": 1,
                    "children": [
                        { "id": "a", "prompt": "one" },
                        { "id": "b", "prompt": "two" }
                    ]
                }
            }),
            &ctx,
        )
        .unwrap_err();
        assert!(
            over_limit.to_string().contains("max_children"),
            "{over_limit}"
        );

        let bad_type = workflow_source(
            &json!({
                "plan": {
                    "goal": "bad type",
                    "children": [{ "prompt": "x", "type": "wizard" }]
                }
            }),
            &ctx,
        )
        .unwrap_err();
        assert!(
            bad_type
                .to_string()
                .contains("Invalid sub-agent type 'wizard'"),
            "{bad_type}"
        );

        let exclusive = workflow_source(
            &json!({
                "script": "return 1;",
                "plan": { "goal": "x", "children": [{ "prompt": "y" }] }
            }),
            &ctx,
        )
        .unwrap_err();
        assert!(
            exclusive
                .to_string()
                .contains("exactly one of script, source_path, or plan"),
            "{exclusive}"
        );
    }

    #[test]
    fn plan_child_type_shares_the_agent_tool_option_vocabulary() {
        // #5035: type values accepted by direct Agent dispatch must not be
        // rejected by Workflow plan authoring; aliases normalize onto the IR.
        for (alias, expected) in [
            ("worker", AgentType::General),
            ("delegate", AgentType::General),
            ("scout", AgentType::Explore),
            ("Explorer", AgentType::Explore),
            ("planner", AgentType::Plan),
            ("awaiter", AgentType::Plan),
            ("reviewer", AgentType::Review),
            ("consultant", AgentType::Review),
            ("oracle", AgentType::Review),
            ("advisor", AgentType::Review),
            ("builder", AgentType::Implementer),
            ("verifier", AgentType::Verifier),
        ] {
            assert_eq!(
                parse_plan_agent_type(Some(alias))
                    .unwrap_or_else(|err| panic!("{alias} rejected: {err}")),
                expected,
                "{alias}"
            );
        }

        // Typos fail with the Agent tool's error contract and the full set.
        let typo = parse_plan_agent_type(Some("wizard"))
            .unwrap_err()
            .to_string();
        assert!(typo.contains("Invalid sub-agent type 'wizard'"), "{typo}");
        assert!(
            typo.contains("worker, scout, planner, reviewer, builder"),
            "{typo}"
        );
        assert!(
            typo.contains("consultant/oracle/advisor"),
            "accepted advisory aliases must remain visible in the guidance: {typo}"
        );

        // `custom` is Agent-only; the rejection says why and what to use.
        let custom = parse_plan_agent_type(Some("custom"))
            .unwrap_err()
            .to_string();
        assert!(
            custom.contains("Invalid sub-agent type 'custom'"),
            "{custom}"
        );
        assert!(custom.contains("allowed_tools"), "{custom}");
    }

    #[test]
    fn declarative_parallel_branch_uses_parallel_helper() {
        let source = r#"
export default workflow({
  "goal": "partial success fan-out",
  "nodes": [
    {
      "branch": {
        "id": "fan",
        "parallel": true,
        "children": [
          { "agent": { "id": "a", "prompt": "A", "agent_type": "explore", "mode": "read_only" } },
          { "agent": { "id": "b", "prompt": "B", "agent_type": "explore", "mode": "read_only" } }
        ]
      }
    }
  ]
});
"#;
        let adapted = adapt_workflow_source(source, None).expect("lower declarative");
        assert!(
            adapted.source.contains("await parallel(["),
            "declarative parallel must lower via parallel():\n{}",
            adapted.source
        );
        assert!(
            !adapted.source.contains("Promise.all"),
            "must not emit raw Promise.all:\n{}",
            adapted.source
        );
    }

    #[test]
    fn source_path_must_stay_inside_workspace_without_trust_mode() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        let outside_path = outside.path().join("outside.workflow.js");
        std::fs::write(&outside_path, "return 1;").expect("outside workflow source");
        let ctx = ToolContext::new(workspace.path().to_path_buf());

        let err = workflow_source(
            &json!({
                "source_path": outside_path
            }),
            &ctx,
        )
        .expect_err("outside source_path should be denied");

        assert!(
            err.to_string().contains("must stay inside the workspace"),
            "{err}"
        );
    }

    #[test]
    fn subagent_tool_surface_registers_workflow_and_agent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        );
        let registry = ToolRegistryBuilder::new()
            .with_subagent_tools(manager, runtime)
            .build(ctx);

        assert!(registry.contains("workflow"));
        assert!(registry.contains("agent"));
        assert!(registry.contains("agents/list"));
        assert!(registry.contains("agents/message"));
        assert!(registry.contains("agents/followup"));
        assert!(registry.contains("agents/interrupt"));
        assert!(registry.contains("agents/wait"));
        assert!(
            registry
                .to_api_tools()
                .iter()
                .any(|tool| tool.name == "workflow")
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn workflow_run_dispatches_task_through_subagent_manager() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("child done").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": "phase('dispatch'); log('starting child'); const out = await task({ description: 'say done', allowedTools: [], label: 'inspect-child', model: 'deepseek-v4-flash', modelStrength: 'same', thinking: 'low' }); return { out };"
                }),
                &ctx,
            )
            .await
            .expect("workflow run should complete");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(payload["result"]["out"], "child done");
        assert_eq!(payload["child_ids"].as_array().unwrap().len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let child_id = payload["child_ids"][0].as_str().unwrap();
        let events = payload["events"].as_array().expect("events array");
        assert!(
            events
                .iter()
                .any(|event| event["type"] == "phase_started" && event["title"] == "dispatch"),
            "{events:#?}"
        );
        assert!(
            events
                .iter()
                .any(|event| event["type"] == "log" && event["message"] == "starting child"),
            "{events:#?}"
        );
        assert!(
            events.iter().any(|event| event["type"] == "budget_updated"),
            "{events:#?}"
        );
        let task_started = events
            .iter()
            .find(|event| event["type"] == "task_started")
            .expect("task_started event");
        assert_eq!(task_started["task_id"], child_id);
        assert_eq!(task_started["label"], "inspect-child");
        assert!(task_started["profile"].is_null());
        assert_eq!(task_started["model"], "deepseek-v4-flash");
        assert_eq!(task_started["strength"], "same");
        assert_eq!(task_started["thinking"], "low");
        assert_eq!(task_started["requested_reasoning"], "low");
        assert!(
            task_started["effective_reasoning"]
                .as_str()
                .is_some_and(|value| !value.is_empty()),
            "{task_started}"
        );
        assert_eq!(task_started["resolved_provider"], "deepseek");
        assert_eq!(task_started["resolved_model"], "deepseek-v4-flash");
        assert_eq!(task_started["route_source"], "task.model");
        assert_eq!(task_started["worktree"], false);
        assert!(task_started["parent_task_id"].is_null());
        assert_eq!(task_started["depth"], 1);
        // #4119: workflow identity on spawn / task_started metadata.
        assert_eq!(
            task_started["workflow_run_id"].as_str(),
            payload["run_id"].as_str()
        );
        assert_eq!(task_started["workflow_phase_id"], "dispatch");
        assert_eq!(task_started["workflow_task_label"], "inspect-child");
        assert_eq!(task_started["workflow_child_index"], 0);
        assert!(
            events.iter().any(|event| event["type"] == "task_completed"
                && event["task_id"] == child_id
                && event["status"] == "succeeded"),
            "{events:#?}"
        );
        let child = manager
            .read()
            .await
            .get_result(child_id)
            .expect("child result");
        assert_eq!(child.status, SubAgentStatus::Completed);
        assert_eq!(child.result.as_deref(), Some("child done"));

        // Full receipt chain: the spawn-minted event survives the JSONL
        // journal reload, hydrates the live projection, and round-trips into
        // history without following a later route or inventing missing usage.
        let reloaded = WorkflowWorkspaceState::open(tmp.path());
        let persisted = reloaded
            .runs
            .lock()
            .expect("reloaded workflow runs")
            .get(payload["run_id"].as_str().expect("run id"))
            .cloned()
            .expect("persisted run");
        let persisted_json = serde_json::to_value(&persisted).expect("persisted run JSON");
        let persisted_started = persisted_json["events"]
            .as_array()
            .and_then(|events| events.iter().find(|event| event["type"] == "task_started"))
            .expect("persisted task_started receipt");
        assert_eq!(persisted_started["requested_reasoning"], "low");
        assert_eq!(
            persisted_started["effective_reasoning"],
            task_started["effective_reasoning"]
        );
        assert_eq!(persisted_started["resolved_provider"], "deepseek");
        assert_eq!(persisted_started["resolved_model"], "deepseek-v4-flash");
        assert_eq!(persisted_started["route_source"], "task.model");

        let mut panel =
            crate::tui::widgets::workflow_panel::WorkflowPanel::from_run_json(&persisted_json)
                .expect("journal should hydrate workflow panel");
        let original_receipt = panel
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .find(|row| row.task_id == child_id)
            .map(crate::tui::widgets::workflow_panel::row_receipt_text)
            .expect("spawned child receipt");
        assert!(original_receipt.contains("deepseek/deepseek-v4-flash"));
        assert!(original_receipt.contains("reasoning low→"));
        assert!(original_receipt.contains("via task.model"));

        panel.apply_json_event(&json!({
            "type": "task_started",
            "at_ms": 9_000,
            "task_id": "later-route",
            "label": "later-route",
            "resolved_role": "consultant",
            "resolved_provider": "moonshot",
            "resolved_model": "kimi-k3",
            "requested_reasoning": "auto",
            "effective_reasoning": "medium",
            "route_source": "agent_profile.model",
            "worktree": false,
        }));
        panel.apply_json_event(&json!({
            "type": "task_completed",
            "at_ms": 9_100,
            "task_id": "later-route",
            "status": "succeeded"
        }));
        let unchanged = panel
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .find(|row| row.task_id == child_id)
            .map(crate::tui::widgets::workflow_panel::row_receipt_text)
            .expect("original child after later route");
        assert_eq!(unchanged, original_receipt);

        let history =
            crate::tui::widgets::workflow_panel::WorkflowPanel::from_run_json(&panel.to_run_json())
                .expect("history receipt round trip");
        let later_receipt = history
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .find(|row| row.task_id == "later-route")
            .map(crate::tui::widgets::workflow_panel::row_receipt_text)
            .expect("later route history receipt");
        assert!(later_receipt.contains("moonshot/kimi-k3"));
        assert!(later_receipt.contains("reasoning auto→medium"));
        assert!(later_receipt.contains("tokens unknown"));
        assert!(!later_receipt.contains("tokens 0"));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn named_fleet_run_emits_role_resolved_receipt_and_rejects_unknown_before_provider() {
        let _retry_guard = workflow_test_retry_guard();
        let _env_lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        std::fs::create_dir_all(tmp.path().join("fleets")).expect("fleets dir");
        std::fs::write(
            tmp.path().join("fleets/offline.toml"),
            r#"
name = "offline"
[roles]
reviewer = "reviewer"
"#,
        )
        .expect("named fleet fixture");

        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("role-resolved child").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(manager, runtime);

        let completed = tool
            .execute(
                json!({
                    "action": "run",
                    "fleet": "offline",
                    "script": "return await task({ description: 'review it', type: 'review', role: 'reviewer', label: 'offline-review' });"
                }),
                &ctx,
            )
            .await
            .expect("named fleet workflow");
        let payload: Value = serde_json::from_str(&completed.content).expect("workflow JSON");
        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(payload["result"], "role-resolved child");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let started = payload["events"]
            .as_array()
            .and_then(|events| events.iter().find(|event| event["type"] == "task_started"))
            .expect("task_started receipt");
        assert_eq!(started["role"], "reviewer");
        assert_eq!(started["profile"], "reviewer");
        assert_eq!(started["resolved_role"], "reviewer");
        assert_eq!(started["resolved_profile"], "reviewer");
        assert_eq!(started["resolved_provider"], "deepseek");
        assert_eq!(started["resolved_model"], "deepseek-v4-flash");
        assert_eq!(started["route_source"], "run.model");
        assert!(
            payload["events"]
                .as_array()
                .is_some_and(|events| events.iter().any(|event| event["type"] == "task_completed"))
        );

        let rejected = tool
            .execute(
                json!({
                    "action": "run",
                    "fleet": "offline",
                    "script": "return await task({ description: 'must not launch', type: 'review', role: 'wizard' });"
                }),
                &ctx,
            )
            .await
            .expect("rejected workflow still returns its terminal record");
        let rejected: Value = serde_json::from_str(&rejected.content).expect("rejected JSON");
        assert_eq!(rejected["status"], "failed", "{rejected}");
        assert!(
            rejected["error"]
                .as_str()
                .is_some_and(|error| error.contains("unknown fleet role `wizard`")),
            "{rejected}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "unknown role must fail before a second provider call"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn workflow_spawn_records_carry_child_index_and_phase_metadata() {
        // #4119: sequential children get monotonic workflow_child_index and
        // inherit the active phase when task options omit `phase`.
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 4);
        let (client, calls, api_config) = fake_chat_client("ok").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": "phase('alpha'); await task({ description: 'first', type: 'explore', allowedTools: [], label: 'one' }); phase('beta'); await task({ description: 'second', type: 'explore', allowedTools: [], label: 'two', phase: 'beta-explicit' }); return { ok: true };"
                }),
                &ctx,
            )
            .await
            .expect("workflow run should complete");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");
        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(payload["child_ids"].as_array().unwrap().len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let mut started: Vec<&Value> = payload["events"]
            .as_array()
            .expect("events")
            .iter()
            .filter(|event| event["type"] == "task_started")
            .collect();
        started.sort_by_key(|event| event["workflow_child_index"].as_u64().unwrap_or(u64::MAX));
        assert_eq!(started.len(), 2, "{started:#?}");

        assert_eq!(started[0]["workflow_run_id"], payload["run_id"]);
        assert_eq!(started[0]["workflow_phase_id"], "alpha");
        assert_eq!(started[0]["workflow_task_label"], "one");
        assert_eq!(started[0]["workflow_child_index"], 0);
        assert_eq!(started[0]["label"], "one");

        assert_eq!(started[1]["workflow_run_id"], payload["run_id"]);
        // Explicit task phase wins over the driver's current phase.
        assert_eq!(started[1]["workflow_phase_id"], "beta-explicit");
        assert_eq!(started[1]["workflow_task_label"], "two");
        assert_eq!(started[1]["workflow_child_index"], 1);
        assert_eq!(started[1]["label"], "two");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn declarative_parallel_spawn_failure_nulls_slot_and_continues() {
        // #4124: parallel() is all-settled — a rejected spawn becomes a null slot
        // (with a breadcrumb) instead of aborting the rest of the script the way
        // raw Promise.all would. Downstream reduce still runs on partial results.
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("reduce-with-partial").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"export default workflow({
                        "goal": "partial success fan-out",
                        "nodes": [
                            {
                                "branch": {
                                    "id": "parallel",
                                    "parallel": true,
                                    "children": [
                                        {
                                            "agent": {
                                                "id": "bad-profile",
                                                "prompt": "This child should be rejected before model execution.",
                                                "profile": "missing-profile"
                                            }
                                        }
                                    ]
                                }
                            },
                            {
                                "reduce": {
                                    "id": "summary",
                                    "inputs": ["bad-profile"],
                                    "prompt": "Summarize whatever survived the parallel fan-out."
                                }
                            }
                        ]
                    });"#
                }),
                &ctx,
            )
            .await
            .expect("partial-success workflow still returns run record");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        // Receipt honesty (morning-report issue #2): the run keeps its output
        // and the reduce still runs, but a dropped slot means the status is
        // degraded, never a plain completed.
        assert_eq!(payload["status"], "degraded", "{payload}");
        let degradation = payload["error"].as_str().expect("degradation surfaced");
        assert!(
            degradation.contains("result may be partial"),
            "{degradation}"
        );
        assert!(
            payload["result"]["bad-profile"].is_null(),
            "failed parallel slot should be null: {}",
            payload["result"]
        );
        assert_eq!(payload["result"]["summary"], "reduce-with-partial");
        let progress = payload["progress"]
            .as_array()
            .expect("progress array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            progress.contains("missing-profile") && progress.contains("dropped a failed slot"),
            "breadcrumb should surface the spawn rejection:\n{progress}"
        );
        // #5035: partial success is explicit — the rejected slot lands in the
        // run record as a structured dispatch failure, not only a log line.
        let failures = payload["dispatch_failures"]
            .as_array()
            .expect("dispatch failures surfaced on the run record");
        assert_eq!(failures.len(), 1, "{payload}");
        assert!(
            failures[0]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("missing-profile"),
            "{failures:?}"
        );
        assert_eq!(
            result.metadata.as_ref().expect("metadata")["dispatch_failure_count"],
            1
        );
        assert!(
            calls.load(Ordering::SeqCst) >= 1,
            "reduce should still run after a null parallel slot"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn parallel_slots_rejected_by_the_vm_cannot_report_plain_success() {
        // Morning-report issue #2: options that fail VM validation throw
        // before the driver ever sees a dispatch, and parallel() collapses
        // those throws into null slots. The run must classify against the
        // slot ledger instead of reporting completed with [null, ...].
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("unused").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": "return await parallel([() => task({ description: 'bad a', type: 'explore', allowedTools: [], cwd: '/absolute/a' }), () => task({ description: 'bad b', type: 'explore', allowedTools: [], cwd: '/absolute/b' })]);"
                }),
                &ctx,
            )
            .await
            .expect("vm-rejected fan-out still returns the run record");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "failed", "{payload}");
        let error = payload["error"].as_str().expect("error surfaced");
        assert!(
            error.contains("all 2 task dispatch(es) were rejected"),
            "error should name the total rejection: {error}"
        );
        let failures = payload["dispatch_failures"]
            .as_array()
            .expect("collected dispatch failures");
        assert_eq!(failures.len(), 2, "{payload}");
        for failure in failures {
            let message = failure["message"].as_str().expect("failure message");
            assert!(message.contains("bounded repo-relative paths"), "{message}");
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no provider call should be spent on vm-rejected slots"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn partially_dropped_parallel_slots_degrade_the_run() {
        // One slot completes, one is rejected before dispatch: the run keeps
        // its output but must report degraded, not completed.
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("child done").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": "return await parallel([() => task({ description: 'say done', type: 'explore', allowedTools: [] }), () => task({ description: 'bad slot', type: 'explore', allowedTools: [], cwd: '/absolute/path' })]);"
                }),
                &ctx,
            )
            .await
            .expect("partially dropped fan-out still returns the run record");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "degraded", "{payload}");
        let error = payload["error"].as_str().expect("degradation surfaced");
        assert!(
            error.contains("1 dispatch(es) were rejected"),
            "error should count the dropped slot: {error}"
        );
        assert!(
            error.contains("result may be partial"),
            "error should warn the result is partial: {error}"
        );
        let results = payload["result"].as_array().expect("run kept its output");
        assert_eq!(results.len(), 2, "{payload}");
        assert!(results[1].is_null(), "the rejected slot stays null");
        assert!(
            calls.load(Ordering::SeqCst) >= 1,
            "the healthy slot still ran"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn parallel_fan_out_with_every_dispatch_rejected_fails_the_run() {
        // #5035: when every parallel slot is rejected before dispatch, the run
        // must not report overall success that ran nothing — the collected
        // per-slot failures surface and the run fails loudly.
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("unused").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"export default workflow({
                        "goal": "total dispatch failure fan-out",
                        "nodes": [
                            {
                                "branch": {
                                    "id": "parallel",
                                    "parallel": true,
                                    "children": [
                                        {
                                            "agent": {
                                                "id": "bad-one",
                                                "prompt": "Rejected before model execution.",
                                                "profile": "missing-profile"
                                            }
                                        },
                                        {
                                            "agent": {
                                                "id": "bad-two",
                                                "prompt": "Also rejected before model execution.",
                                                "profile": "missing-profile"
                                            }
                                        }
                                    ]
                                }
                            }
                        ]
                    });"#
                }),
                &ctx,
            )
            .await
            .expect("total dispatch failure still returns the run record");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "failed", "{payload}");
        let error = payload["error"].as_str().expect("error surfaced");
        assert!(
            error.contains("all 2 task dispatch(es) were rejected"),
            "error should name the total dispatch failure: {error}"
        );
        let failures = payload["dispatch_failures"]
            .as_array()
            .expect("collected dispatch failures");
        assert_eq!(failures.len(), 2, "{payload}");
        for failure in failures {
            let message = failure["message"].as_str().expect("failure message");
            assert!(message.contains("missing-profile"), "{message}");
        }
        assert_eq!(payload["child_ids"].as_array().unwrap().len(), 0);
        assert_eq!(
            result.metadata.as_ref().expect("metadata")["dispatch_failure_count"],
            2
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no provider call should be spent on an all-rejected fan-out"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_token_budget_never_kills_a_child_nor_fails_the_run() {
        // #6189 removed token-budget enforcement: usage is tracked, never
        // enforced, and no token budget may kill a worker. This drives the
        // same end-to-end path the former budget-death test used — a 1-token
        // `tokenBudget` on both children, with the fake provider reporting
        // more tokens than that cap — and asserts the run completes with both
        // results. A regression back to enforcement (children dying of the
        // token ceiling, the run failing) fails here.
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("child done").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": "return await parallel([() => task({ description: 'spendy a', type: 'explore', allowedTools: [], tokenBudget: 1 }), () => task({ description: 'spendy b', type: 'explore', allowedTools: [], tokenBudget: 1 })]);"
                }),
                &ctx,
            )
            .await
            .expect("budgeted fan-out still returns the run record");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "completed", "{payload}");
        let slots = payload["result"].as_array().expect("run kept its output");
        assert_eq!(slots.len(), 2, "{payload}");
        assert!(
            slots.iter().all(|slot| slot == "child done"),
            "a token budget must not stop a child from reporting: {slots:?}"
        );
        assert_eq!(payload["child_ids"].as_array().unwrap().len(), 2);
        assert!(
            calls.load(Ordering::SeqCst) >= 2,
            "both children should have run and reported usage"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_fan_out_of_script_throws_cannot_report_completed() {
        // R9 blocker: thunks that throw without calling `task()` produce
        // kind `script`, drop to null, and previously left no host-visible
        // trace beyond a log line — no task records, no dispatch failures,
        // classify() = None, run recorded Completed. The structured
        // every-slot-failed signal must carry the dead fan-out to the host.
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("unused").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"
                    const results = await parallel([
                        () => { throw new Error("script slot a died"); },
                        () => { throw new Error("script slot b died"); },
                    ]);
                    return { slots: results };
                    "#
                }),
                &ctx,
            )
            .await
            .expect("all-script-throw fan-out still returns the run record");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "failed", "{payload}");
        let error = payload["error"].as_str().expect("error surfaced");
        assert!(
            error.contains("no work survived") && error.contains("1 fan-out(s) lost every slot"),
            "error should name the dead fan-out: {error}"
        );
        // The null slots survive on the record; the run refuses the success.
        assert_eq!(
            payload["result"]["slots"],
            json!([null, null]),
            "the settled array is preserved verbatim: {}",
            payload["result"]
        );
        let progress = payload["progress"]
            .as_array()
            .expect("progress array")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            progress.contains("every slot failed (2 of 2)")
                && progress.contains("no work survived this fan-out"),
            "the operator breadcrumb must stay in the run log:\n{progress}"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "no provider call should be spent on thunks that never call task()"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn a_mixed_fan_out_with_surviving_work_stays_degraded_not_failed() {
        // One slot runs a real child and survives; one thunk throws. Some
        // work survived, so the run degrades with its null preserved — it is
        // not a dead fan-out (1 of 2 failed) and must not record failed.
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("child done").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": "return await parallel([() => task({ description: 'healthy slot', type: 'explore', allowedTools: [] }), () => { throw new Error('script slot died'); }]);"
                }),
                &ctx,
            )
            .await
            .expect("mixed fan-out still returns the run record");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "degraded", "{payload}");
        let error = payload["error"].as_str().expect("degradation surfaced");
        assert!(
            error.contains("result may be partial"),
            "error should keep the partial-output caveat: {error}"
        );
        let slots = payload["result"].as_array().expect("run kept its output");
        assert_eq!(slots.len(), 2, "{payload}");
        assert!(slots[0].is_string() && slots[1].is_null(), "{slots:?}");
        assert!(
            calls.load(Ordering::SeqCst) >= 1,
            "the healthy slot still ran"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn structured_plan_phases_forward_results_and_stop_on_missing_dependencies() {
        let _retry_guard = workflow_test_retry_guard();
        for fail_upstream in [false, true] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let ctx = ToolContext::new(tmp.path().to_path_buf());
            let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
            let (client, calls, bodies, api_config) =
                fake_chat_client_capturing("upstream-output").await;
            let runtime = SubAgentRuntime::new(
                client,
                "deepseek-v4-flash".to_string(),
                ctx.clone(),
                true,
                None,
                manager,
            )
            .with_api_config(api_config);
            let tool = WorkflowTool::new(runtime.manager.clone(), runtime);
            let mut upstream = vec![json!({
                "id": "__proto__", "prompt": "Produce the upstream finding.", "role": "reviewer"
            })];
            if fail_upstream {
                upstream.push(json!({
                    "id": "missing", "prompt": "This profile does not exist.",
                    "profile": "required-profile-does-not-exist"
                }));
            }
            let result = tool.execute(json!({
                "action": "run",
                "plan": {
                    "goal": "Carry evidence through native workflow phases",
                    "risk": "read_only",
                    "token_budget": 1_000,
                    "phases": [
                        {"id": "inspect", "children": upstream},
                        {"id": "finish", "children": [{
                            "id": "second", "prompt": "Use the upstream finding.", "role": "reviewer"
                        }]}
                    ]
                }
            }), &ctx).await.expect("workflow returns its run receipt");
            let payload: Value = serde_json::from_str(&result.content).expect("json result");
            assert_eq!(payload["token_budget"], 1_000, "{payload}");
            if fail_upstream {
                assert_ne!(payload["status"], "completed", "{payload}");
                assert_eq!(
                    calls.load(Ordering::SeqCst),
                    1,
                    "dependent child must not run: {payload}"
                );
                assert!(
                    payload
                        .to_string()
                        .contains("Required Workflow result unavailable: missing"),
                    "{payload}"
                );
            } else {
                assert_eq!(payload["status"], "completed", "{payload}");
                assert_eq!(calls.load(Ordering::SeqCst), 2);
                let bodies = bodies.lock().expect("captured bodies");
                let downstream = bodies.get(1).expect("dependent provider call").to_string();
                assert!(downstream.contains("--- __proto__ ---"), "{downstream}");
                assert!(downstream.contains("upstream-output"), "{downstream}");
                let phases = payload["events"]
                    .as_array()
                    .expect("events")
                    .iter()
                    .filter(|event| event["type"] == "phase_started")
                    .filter_map(|event| event["title"].as_str())
                    .collect::<Vec<_>>();
                assert!(
                    phases.contains(&"inspect") && phases.contains(&"finish"),
                    "{phases:?}"
                );
                let task_phases = payload["events"]
                    .as_array()
                    .expect("events")
                    .iter()
                    .filter(|event| event["type"] == "task_started")
                    .filter_map(|event| event["workflow_phase_id"].as_str())
                    .collect::<Vec<_>>();
                assert_eq!(task_phases, ["inspect", "finish"]);
            }
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn declarative_dependency_results_are_forwarded_to_downstream_prompt() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, bodies, api_config) =
            fake_chat_client_capturing("upstream-output").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"export default workflow({
                        "goal": "dependency forwarding",
                        "nodes": [
                            {
                                "agent": {
                                    "id": "first",
                                    "prompt": "Produce the upstream finding.",
                                    "agent_type": "review"
                                }
                            },
                            {
                                "agent": {
                                    "id": "second",
                                    "prompt": "Use the upstream finding.",
                                    "agent_type": "review",
                                    "depends_on_results": ["first"]
                                }
                            }
                        ]
                    });"#
                }),
                &ctx,
            )
            .await
            .expect("dependency workflow should complete");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(payload["execution"]["status"], "succeeded");
        assert_eq!(
            payload["execution"]["leaf_results"][0]["output"],
            "upstream-output"
        );
        assert_eq!(
            payload["execution"]["leaf_results"][1]["output"],
            "upstream-output"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let bodies = bodies.lock().expect("captured bodies");
        let second_body = bodies.get(1).expect("second provider call").to_string();
        assert!(second_body.contains("--- first ---"), "{second_body}");
        assert!(second_body.contains("upstream-output"), "{second_body}");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn workflow_runtime_gates_promote_handoff_and_block_downstream_role() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        );
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_gate".to_string();
        let gates = vec![GateSpec {
            id: "explore-findings".to_string(),
            role: "explore".to_string(),
            on: GateOn::RoleComplete,
            gate: GateKind::Approve,
            on_fail: codewhale_workflow::GateOnFail::Block,
            blocks_role: Some("implement".to_string()),
            max_retries: 0,
            artifact_kind: Some("findings".to_string()),
            require_explicit_verdict: false,
        }];
        let spec = WorkflowSpec {
            id: Some("gate-fixture".to_string()),
            goal: "gate fixture".to_string(),
            description: None,
            budget: BudgetSpec::default(),
            permissions: Default::default(),
            model_policy: Default::default(),
            promotion_policy: Default::default(),
            gates: gates.clone(),
            nodes: Vec::new(),
        };
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".to_string()),
                None,
                None,
                Some(&spec),
            ),
        );
        let driver = SubAgentWorkflowDriver::new(
            run_id.clone(),
            "session-test".to_string(),
            manager,
            runtime,
            state.clone(),
            None,
            WorkflowFleetBinding::None,
            gates,
            tmp.path().to_path_buf(),
        );

        driver.evaluate_gates_for_completed_role(&RuntimeTaskRecord {
            agent_id: "explore-agent".to_string(),
            label: Some("explore".to_string()),
            role: Some("explore".to_string()),
            status: IrWorkflowRunStatus::Succeeded,
            output: Some("findings: inspect tui exit path".to_string()),
            schema_error: None,
            usage: None,
        });

        let mut implementer = TaskRequest {
            description: "Use the findings.".to_string(),
            subagent_type: Some("implement".to_string()),
            role: Some("implement".to_string()),
            profile: None,
            model: None,
            model_strength: None,
            thinking: None,
            cwd: None,
            worktree: false,
            write_authority: Some("workspace_write".to_string()),
            write_roots: vec!["src".to_string()],
            exact_files: Vec::new(),
            coordination_contracts: vec!["test-contract".to_string()],
            dependencies: Vec::new(),
            acceptance: Vec::new(),
            allowed_tools: Some(Vec::new()),
            disallowed_tools: Vec::new(),
            max_depth: None,
            token_budget: None,
            max_steps: None,
            wall_time_secs: None,
            response_schema: None,
            schema_repair_attempts: None,
            label: Some("fix".to_string()),
            phase: None,
        };
        let handoffs = driver
            .prepare_request_for_gates(&mut implementer)
            .expect("passed gate should admit implementer");
        assert_eq!(handoffs.len(), 1, "{handoffs:?}");
        assert_eq!(handoffs[0].kind, "findings");
        assert_eq!(handoffs[0].from_role, "explore");
        assert_eq!(handoffs[0].to_role, "implement");
        assert!(
            implementer
                .description
                .contains("Workflow handoff artifacts available"),
            "{}",
            implementer.description
        );
        assert!(
            implementer.description.contains("inspect tui exit path"),
            "{}",
            implementer.description
        );

        driver.evaluate_gates_for_completed_role(&RuntimeTaskRecord {
            agent_id: "explore-agent-2".to_string(),
            label: Some("explore".to_string()),
            role: Some("explore".to_string()),
            status: IrWorkflowRunStatus::Failed,
            output: Some("explore incomplete".to_string()),
            schema_error: None,
            usage: None,
        });
        let mut blocked = TaskRequest {
            description: "Try after block.".to_string(),
            role: Some("implement".to_string()),
            ..implementer.clone()
        };
        let err = driver
            .prepare_request_for_gates(&mut blocked)
            .expect_err("blocked gate should reject downstream role");
        assert!(err.to_string().contains("explore incomplete"), "{err}");

        let run = state
            .runs
            .lock()
            .expect("runs")
            .get(&run_id)
            .cloned()
            .expect("run");
        assert!(
            run.gate_status
                .iter()
                .any(|line| line.gate_id == "explore-findings"
                    && line.state == "blocked"
                    && line.blocked_reason.as_deref() == Some("explore incomplete")),
            "{:?}",
            run.gate_status
        );
        assert!(
            run.events
                .iter()
                .any(|event| event.event_type() == "gate_updated"),
            "{:?}",
            run.events
        );
        assert_eq!(
            run.events
                .iter()
                .filter(|event| event.event_type() == "handoff_promoted")
                .count(),
            1,
            "a later blocked gate must not publish another handoff: {:?}",
            run.events
        );
        assert!(
            run.events
                .iter()
                .all(|event| event.event_type() != "handoff_consumed"),
            "request preparation alone must not claim consumption: {:?}",
            run.events
        );
    }

    #[tokio::test]
    async fn workflow_handoff_is_delivered_to_exactly_one_task() {
        // LaneGateBoard.artifacts used to be append-only: every same-role
        // task re-received up to 4 prior handoff payloads while
        // HandoffConsumed receipts fired as if they were spent.
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        );
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_handoff_once".to_string();
        let gates = vec![GateSpec {
            id: "scout-findings".to_string(),
            role: "scout".to_string(),
            on: GateOn::RoleComplete,
            gate: GateKind::Approve,
            on_fail: codewhale_workflow::GateOnFail::Block,
            blocks_role: Some("implementer".to_string()),
            max_retries: 0,
            artifact_kind: Some("findings".to_string()),
            require_explicit_verdict: false,
        }];
        let spec = WorkflowSpec {
            id: Some("handoff-once-fixture".to_string()),
            goal: "handoff delivered once".to_string(),
            description: None,
            budget: BudgetSpec::default(),
            permissions: Default::default(),
            model_policy: Default::default(),
            promotion_policy: Default::default(),
            gates: gates.clone(),
            nodes: Vec::new(),
        };
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".to_string()),
                None,
                None,
                Some(&spec),
            ),
        );
        let driver = SubAgentWorkflowDriver::new(
            run_id.clone(),
            "session-test".to_string(),
            manager,
            runtime,
            state.clone(),
            None,
            WorkflowFleetBinding::None,
            gates,
            tmp.path().to_path_buf(),
        );

        driver.evaluate_gates_for_completed_role(&RuntimeTaskRecord {
            agent_id: "scout-agent".to_string(),
            label: Some("scout".to_string()),
            role: Some("scout".to_string()),
            status: IrWorkflowRunStatus::Succeeded,
            output: Some("findings: exactly once".to_string()),
            schema_error: None,
            usage: None,
        });

        let implementer = TaskRequest {
            description: "Use the findings.".to_string(),
            subagent_type: Some("implementer".to_string()),
            role: Some("implementer".to_string()),
            profile: None,
            model: None,
            model_strength: None,
            thinking: None,
            cwd: None,
            worktree: false,
            write_authority: Some("workspace_write".to_string()),
            write_roots: vec!["src".to_string()],
            exact_files: Vec::new(),
            coordination_contracts: Vec::new(),
            dependencies: Vec::new(),
            acceptance: Vec::new(),
            allowed_tools: Some(Vec::new()),
            disallowed_tools: Vec::new(),
            max_depth: None,
            token_budget: None,
            max_steps: None,
            wall_time_secs: None,
            response_schema: None,
            schema_repair_attempts: None,
            label: Some("fix".to_string()),
            phase: None,
        };

        let mut first = implementer.clone();
        let handoffs = driver
            .prepare_request_for_gates(&mut first)
            .expect("passed gate should admit first implementer");
        assert_eq!(handoffs.len(), 1, "{handoffs:?}");
        assert!(first.description.contains("exactly once"));

        // A second same-role task must not re-receive the spent handoff.
        let mut second = TaskRequest {
            description: "Second implementer task.".to_string(),
            ..implementer
        };
        let handoffs = driver
            .prepare_request_for_gates(&mut second)
            .expect("passed gate should admit second implementer");
        assert!(
            handoffs.is_empty(),
            "handoff already consumed must not re-deliver: {handoffs:?}"
        );
        assert!(
            !second
                .description
                .contains("Workflow handoff artifacts available"),
            "{}",
            second.description
        );
    }

    #[tokio::test]
    async fn workflow_gate_evaluation_error_persists_blocked_and_denies_target_role() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx,
            true,
            None,
            manager.clone(),
        );
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_malformed_gate".to_string();
        let gates = vec![GateSpec {
            id: String::new(),
            role: "scout".to_string(),
            on: GateOn::RoleComplete,
            gate: GateKind::Approve,
            on_fail: codewhale_workflow::GateOnFail::Block,
            blocks_role: Some("implementer".to_string()),
            max_retries: 0,
            artifact_kind: Some("findings".to_string()),
            require_explicit_verdict: false,
        }];
        let spec = WorkflowSpec {
            id: Some("malformed-gate-fixture".to_string()),
            goal: "malformed gate must fail closed".to_string(),
            description: None,
            budget: BudgetSpec::default(),
            permissions: Default::default(),
            model_policy: Default::default(),
            promotion_policy: Default::default(),
            gates: gates.clone(),
            nodes: Vec::new(),
        };
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".to_string()),
                None,
                None,
                Some(&spec),
            ),
        );
        let driver = SubAgentWorkflowDriver::new(
            run_id.clone(),
            "session-test".to_string(),
            manager,
            runtime,
            state.clone(),
            None,
            WorkflowFleetBinding::None,
            gates,
            tmp.path().to_path_buf(),
        );

        driver.evaluate_gates_for_completed_role(&RuntimeTaskRecord {
            agent_id: "scout-agent".to_string(),
            label: Some("scout".to_string()),
            role: Some("scout".to_string()),
            status: IrWorkflowRunStatus::Succeeded,
            output: Some("findings".to_string()),
            schema_error: None,
            usage: None,
        });

        let mut request = TaskRequest {
            description: "Must not be admitted.".to_string(),
            subagent_type: Some("implementer".to_string()),
            role: Some("implementer".to_string()),
            profile: None,
            model: None,
            model_strength: None,
            thinking: None,
            cwd: None,
            worktree: false,
            write_authority: Some("workspace_write".to_string()),
            write_roots: vec!["src".to_string()],
            exact_files: Vec::new(),
            coordination_contracts: vec!["test-contract".to_string()],
            dependencies: Vec::new(),
            acceptance: Vec::new(),
            allowed_tools: Some(Vec::new()),
            disallowed_tools: Vec::new(),
            max_depth: None,
            token_budget: None,
            max_steps: None,
            wall_time_secs: None,
            response_schema: None,
            schema_repair_attempts: None,
            label: Some("blocked".to_string()),
            phase: None,
        };
        let error = driver
            .prepare_request_for_gates(&mut request)
            .expect_err("malformed gate must deny its target role");
        assert!(
            error.to_string().contains("gate id must not be empty"),
            "{error}"
        );
        let board = driver.gate_board.lock().expect("gate board");
        assert!(matches!(
            board.gates.get(""),
            Some(GateState::Blocked { reason }) if reason.contains("gate id must not be empty")
        ));
        assert!(board.artifacts.is_empty(), "{:?}", board.artifacts);
        drop(board);
        let run = state
            .runs
            .lock()
            .expect("runs")
            .get(&run_id)
            .cloned()
            .expect("run");
        assert!(run.gate_status.iter().any(|line| {
            line.gate_id.is_empty()
                && line.state == "blocked"
                && line
                    .blocked_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("gate id must not be empty"))
        }));
        assert!(
            run.events
                .iter()
                .all(|event| event.event_type() != "handoff_promoted"),
            "{:?}",
            run.events
        );
    }

    #[tokio::test]
    async fn workflow_handoff_record_error_changes_pass_to_blocked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx,
            true,
            None,
            manager.clone(),
        );
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_handoff_record_error".to_string();
        let gates = vec![GateSpec {
            id: "scout-findings".to_string(),
            role: "scout".to_string(),
            on: GateOn::RoleComplete,
            gate: GateKind::Approve,
            on_fail: codewhale_workflow::GateOnFail::Block,
            blocks_role: Some("implementer".to_string()),
            max_retries: 0,
            artifact_kind: Some("findings".to_string()),
            require_explicit_verdict: false,
        }];
        let spec = WorkflowSpec {
            id: Some("handoff-record-error-fixture".to_string()),
            goal: "failed handoff recording must fail closed".to_string(),
            description: None,
            budget: BudgetSpec::default(),
            permissions: Default::default(),
            model_policy: Default::default(),
            promotion_policy: Default::default(),
            gates: gates.clone(),
            nodes: Vec::new(),
        };
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".to_string()),
                None,
                None,
                Some(&spec),
            ),
        );
        let driver = SubAgentWorkflowDriver::new(
            run_id.clone(),
            "session-test".to_string(),
            manager,
            runtime,
            state.clone(),
            None,
            WorkflowFleetBinding::None,
            gates,
            tmp.path().to_path_buf(),
        );
        driver.gate_board.lock().expect("gate board").lane_id = "wrong-lane".to_string();

        driver.evaluate_gates_for_completed_role(&RuntimeTaskRecord {
            agent_id: "scout-agent".to_string(),
            label: Some("scout".to_string()),
            role: Some("scout".to_string()),
            status: IrWorkflowRunStatus::Succeeded,
            output: Some("findings".to_string()),
            schema_error: None,
            usage: None,
        });

        let mut request = TaskRequest {
            description: "Must not be admitted.".to_string(),
            subagent_type: Some("implementer".to_string()),
            role: Some("implementer".to_string()),
            profile: None,
            model: None,
            model_strength: None,
            thinking: None,
            cwd: None,
            worktree: false,
            write_authority: Some("workspace_write".to_string()),
            write_roots: vec!["src".to_string()],
            exact_files: Vec::new(),
            coordination_contracts: vec!["test-contract".to_string()],
            dependencies: Vec::new(),
            acceptance: Vec::new(),
            allowed_tools: Some(Vec::new()),
            disallowed_tools: Vec::new(),
            max_depth: None,
            token_budget: None,
            max_steps: None,
            wall_time_secs: None,
            response_schema: None,
            schema_repair_attempts: None,
            label: Some("blocked".to_string()),
            phase: None,
        };
        let error = driver
            .prepare_request_for_gates(&mut request)
            .expect_err("unrecorded handoff must deny its target role");
        assert!(
            error.to_string().contains("handoff could not be recorded"),
            "{error}"
        );
        let board = driver.gate_board.lock().expect("gate board");
        assert!(matches!(
            board.gates.get("scout-findings"),
            Some(GateState::Blocked { reason }) if reason.contains("does not match board lane")
        ));
        assert!(board.artifacts.is_empty(), "{:?}", board.artifacts);
        drop(board);
        let run = state
            .runs
            .lock()
            .expect("runs")
            .get(&run_id)
            .cloned()
            .expect("run");
        assert!(run.events.iter().any(|event| {
            matches!(
                &event.kind,
                WorkflowUiEventKind::GateUpdated {
                    state,
                    blocked_reason: Some(reason),
                    ..
                } if state == "blocked" && reason.contains("handoff could not be recorded")
            )
        }));
        assert!(
            run.events
                .iter()
                .all(|event| event.event_type() != "handoff_promoted"),
            "{:?}",
            run.events
        );
    }

    #[test]
    fn explicit_gate_verdict_only_reads_first_standalone_token() {
        assert_eq!(
            explicit_gate_verdict(Some("\n  APPROVE  \nreview complete")),
            Some(ExplicitGateVerdict::Approve)
        );
        assert_eq!(
            explicit_gate_verdict(Some("PASS\nverification complete")),
            Some(ExplicitGateVerdict::Approve)
        );
        assert_eq!(
            explicit_gate_verdict(Some("BLOCK\nmissing receipt")),
            Some(ExplicitGateVerdict::Reject)
        );
        assert_eq!(
            explicit_gate_verdict(Some("\nFAIL\nmissing receipt")),
            Some(ExplicitGateVerdict::Reject)
        );
        assert_eq!(
            explicit_gate_verdict(Some("Review result: BLOCK")),
            None,
            "prose remains backward-compatible success output"
        );
        assert_eq!(
            explicit_gate_verdict(Some("review notes\nBLOCK")),
            None,
            "later verdict words must not override the first meaningful line"
        );
    }

    #[test]
    fn required_explicit_gate_verdict_fails_closed_when_missing_or_malformed() {
        let mut record = RuntimeTaskRecord {
            agent_id: "reviewer-malformed".to_string(),
            label: Some("reviewer".to_string()),
            role: Some("reviewer".to_string()),
            status: IrWorkflowRunStatus::Succeeded,
            output: Some("Review result: BLOCK".to_string()),
            schema_error: None,
            usage: None,
        };

        match gate_outcome_for_completed_role(&record, true, None) {
            GateOutcome::Fail { reason } => {
                assert!(
                    reason.contains("required first-line gate verdict"),
                    "{reason}"
                );
            }
            outcome => panic!("required malformed verdict must fail closed: {outcome:?}"),
        }
        assert_eq!(
            gate_outcome_for_completed_role(&record, false, None),
            GateOutcome::Pass,
            "legacy gates retain pass-on-success behavior"
        );

        record.output = None;
        assert!(matches!(
            gate_outcome_for_completed_role(&record, true, None),
            GateOutcome::Fail { .. }
        ));
    }

    #[test]
    fn required_gate_artifact_rejects_bare_or_placeholder_approval() {
        let mut record = RuntimeTaskRecord {
            agent_id: "implementer-bare".to_string(),
            label: Some("implementer".to_string()),
            role: Some("implementer".to_string()),
            status: IrWorkflowRunStatus::Succeeded,
            output: Some("APPROVE".to_string()),
            schema_error: None,
            usage: None,
        };

        match gate_outcome_for_completed_role(&record, true, Some("verification_plan")) {
            GateOutcome::Fail { reason } => {
                assert!(
                    reason.contains("verification_plan artifact body"),
                    "{reason}"
                );
            }
            outcome => panic!("bare approval must not promote an empty artifact: {outcome:?}"),
        }

        record.output = Some("APPROVE\nacceptance evidence".to_string());
        match gate_outcome_for_completed_role(&record, true, Some("verification_plan")) {
            GateOutcome::Fail { reason } => {
                assert!(
                    reason.contains("verification_plan artifact body"),
                    "{reason}"
                );
            }
            outcome => {
                panic!("one placeholder line must not count as an artifact: {outcome:?}");
            }
        }

        record.output = Some("APPROVE\nPLAN\n- verify the typed receipt".to_string());
        assert_eq!(
            gate_outcome_for_completed_role(&record, true, Some("verification_plan")),
            GateOutcome::Pass
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn terminal_blocked_gate_fails_workflow_finalization() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) =
            fake_chat_client("BLOCK\nFINAL RECEIPT\n- missing terminal evidence").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"export default workflow({
                        "goal": "fail closed on the terminal release verdict",
                        "gates": [
                            {
                                "id": "terminal-release",
                                "role": "reviewer",
                                "on": "role_complete",
                                "gate": "approve",
                                "on_fail": "block",
                                "max_retries": 0,
                                "artifact_kind": "final_receipt",
                                "require_explicit_verdict": true
                            }
                        ],
                        "nodes": [
                            {
                                "agent": {
                                    "id": "release-receipt",
                                    "prompt": "Return the terminal verdict and receipt.",
                                    "agent_type": "general",
                                    "role": "reviewer",
                                    "mode": "read_only",
                                    "permissions": { "deny_all_tools": true },
                                    "budget": { "max_steps": 1 }
                                }
                            }
                        ]
                    });"#
                }),
                &ctx,
            )
            .await
            .expect("blocked terminal gate should return its failed run record");
        let payload: Value = serde_json::from_str(&result.content).expect("workflow JSON");

        assert_eq!(calls.load(Ordering::SeqCst), 1, "{payload}");
        assert_eq!(payload["status"], "failed", "{payload}");
        assert_eq!(payload["execution"]["status"], "failed", "{payload}");
        assert!(
            payload["error"]
                .as_str()
                .is_some_and(|error| error.contains("terminal-release")
                    && error.contains("ended blocked")
                    && error.contains("missing terminal evidence")),
            "{payload}"
        );
        assert!(payload["gate_status"].as_array().is_some_and(|gates| {
            gates
                .iter()
                .any(|gate| gate["gate_id"] == "terminal-release" && gate["state"] == "blocked")
        }));
        assert!(payload["events"].as_array().is_some_and(|events| {
            events
                .iter()
                .any(|event| event["type"] == "run_completed" && event["status"] == "failed")
        }));
    }

    #[tokio::test]
    async fn workflow_runtime_gate_honors_explicit_reviewer_verdicts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx,
            true,
            None,
            manager.clone(),
        );
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_explicit_verdict".to_string();
        let gates = vec![GateSpec {
            id: "review-findings".to_string(),
            role: "reviewer".to_string(),
            on: GateOn::RoleComplete,
            gate: GateKind::Review,
            on_fail: codewhale_workflow::GateOnFail::Block,
            blocks_role: Some("verifier".to_string()),
            max_retries: 0,
            artifact_kind: Some("review_report".to_string()),
            require_explicit_verdict: true,
        }];
        let spec = WorkflowSpec {
            id: Some("explicit-verdict-fixture".to_string()),
            goal: "honor reviewer verdict".to_string(),
            description: None,
            budget: BudgetSpec::default(),
            permissions: Default::default(),
            model_policy: Default::default(),
            promotion_policy: Default::default(),
            gates: gates.clone(),
            nodes: Vec::new(),
        };
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".to_string()),
                None,
                None,
                Some(&spec),
            ),
        );
        let driver = SubAgentWorkflowDriver::new(
            run_id,
            "session-test".to_string(),
            manager,
            runtime,
            state,
            None,
            WorkflowFleetBinding::None,
            gates,
            tmp.path().to_path_buf(),
        );

        driver.evaluate_gates_for_completed_role(&RuntimeTaskRecord {
            agent_id: "reviewer-block".to_string(),
            label: Some("reviewer".to_string()),
            role: Some("reviewer".to_string()),
            status: IrWorkflowRunStatus::Succeeded,
            output: Some("\nBLOCK\nmissing terminal receipt".to_string()),
            schema_error: None,
            usage: None,
        });

        let verifier_request = || TaskRequest {
            description: "Verify the accepted review.".to_string(),
            subagent_type: Some("verifier".to_string()),
            role: Some("verifier".to_string()),
            profile: None,
            model: None,
            model_strength: None,
            thinking: None,
            cwd: None,
            worktree: false,
            write_authority: Some("read_only".to_string()),
            write_roots: Vec::new(),
            exact_files: Vec::new(),
            coordination_contracts: Vec::new(),
            dependencies: Vec::new(),
            acceptance: Vec::new(),
            allowed_tools: Some(Vec::new()),
            disallowed_tools: Vec::new(),
            max_depth: None,
            token_budget: None,
            max_steps: None,
            wall_time_secs: None,
            response_schema: None,
            schema_repair_attempts: None,
            label: Some("verify".to_string()),
            phase: None,
        };
        let mut blocked_verifier = verifier_request();
        let error = driver
            .prepare_request_for_gates(&mut blocked_verifier)
            .expect_err("successful reviewer BLOCK must not admit verifier");
        assert!(error.to_string().contains("BLOCK"), "{error}");
        {
            let board = driver.gate_board.lock().expect("gate board");
            assert!(
                board.artifacts.is_empty(),
                "rejected output must not produce a handoff: {:?}",
                board.artifacts
            );
            assert!(matches!(
                board.gates.get("review-findings"),
                Some(GateState::Blocked { .. })
            ));
        }

        driver.evaluate_gates_for_completed_role(&RuntimeTaskRecord {
            agent_id: "reviewer-approve".to_string(),
            label: Some("reviewer".to_string()),
            role: Some("reviewer".to_string()),
            status: IrWorkflowRunStatus::Succeeded,
            output: Some("APPROVE\nEVIDENCE REVIEW\n- all receipt owners confirmed".to_string()),
            schema_error: None,
            usage: None,
        });

        let mut admitted_verifier = verifier_request();
        driver
            .prepare_request_for_gates(&mut admitted_verifier)
            .expect("explicit reviewer APPROVE should admit verifier");
        assert!(
            admitted_verifier
                .description
                .contains("all receipt owners confirmed"),
            "{}",
            admitted_verifier.description
        );
        let board = driver.gate_board.lock().expect("gate board");
        assert!(
            board.artifacts.is_empty(),
            "the admitted verifier consumed the handoff; spent artifacts leave the board: {:?}",
            board.artifacts
        );
        assert!(matches!(
            board.gates.get("review-findings"),
            Some(GateState::Passed)
        ));
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn workflow_status_lists_compact_typed_receipts() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, _calls, api_config) = fake_chat_client("status-output").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let run = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"export default workflow({
                        "id": "status-fixture",
                        "goal": "status summary",
                        "nodes": [
                            {
                                "agent": {
                                    "id": "inspect",
                                    "prompt": "Inspect the code.",
                                    "agent_type": "review"
                                }
                            }
                        ]
                    });"#
                }),
                &ctx,
            )
            .await
            .expect("workflow run");
        let run_payload: Value = serde_json::from_str(&run.content).expect("run json");

        let status = tool
            .execute(json!({"action": "status"}), &ctx)
            .await
            .expect("workflow status");
        let status_payload: Value = serde_json::from_str(&status.content).expect("status json");
        let summary = &status_payload["runs"][0];

        assert_eq!(status_payload["count"], 1);
        assert_eq!(summary["run_id"], run_payload["run_id"]);
        assert_eq!(summary["workflow_id"], "status-fixture");
        assert_eq!(summary["workflow_goal"], "status summary");
        assert_eq!(summary["status"], "completed");
        assert_eq!(summary["execution_status"], "succeeded");
        assert_eq!(summary["child_count"], 1);
        assert_eq!(summary["leaf_count"], 1);
        assert_eq!(summary["branch_count"], 0);
        assert_eq!(summary["control_count"], 0);
        assert!(summary["event_count"].as_u64().unwrap_or_default() >= 3);
        assert_eq!(summary["last_event_type"], "run_completed");
        assert!(summary.get("result").is_none());
        assert!(summary.get("execution").is_none());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn workflow_status_survives_tool_rebuild_via_journal() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, _calls, api_config) = fake_chat_client("journal-output").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(manager.clone(), runtime.clone());

        let run = tool
            .execute(
                json!({
                    "action": "run",
                    "script": "return { ok: true };"
                }),
                &ctx,
            )
            .await
            .expect("workflow run");
        let run_payload: Value = serde_json::from_str(&run.content).expect("run json");
        let run_id = run_payload["run_id"].as_str().expect("run id");

        let journal_path = tmp.path().join(".codewhale/workflow-runs.jsonl");
        assert!(
            journal_path.exists(),
            "journal should be created under workspace"
        );

        let rebuilt = WorkflowTool::new(
            manager.clone(),
            SubAgentRuntime::new(
                stub_client(),
                "deepseek-v4-flash".to_string(),
                ctx.clone(),
                true,
                None,
                manager,
            ),
        );
        let status = rebuilt
            .execute(json!({"action": "status", "run_id": run_id}), &ctx)
            .await
            .expect("workflow status after rebuild");
        let status_payload: Value = serde_json::from_str(&status.content).expect("status json");
        assert_eq!(status_payload["run_id"], run_id);
        assert_eq!(status_payload["status"], "completed");
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn workflow_status_surfaces_schema_failure_instead_of_null_success() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, _calls, api_config) = fake_chat_client(r#"{"refuted":"yes"}"#).await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(manager, runtime);

        let run = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"
                    return await parallel([
                        () => task({
                            description: "Return the schema fixture.",
                            responseSchema: {
                                type: "object",
                                properties: { refuted: { type: "boolean" } },
                                required: ["refuted"],
                            },
                        }),
                    ]);
                    "#
                }),
                &ctx,
            )
            .await
            .expect("workflow run returns a record");
        let run_payload: Value = serde_json::from_str(&run.content).expect("run json");

        assert_eq!(run_payload["status"], "failed");
        assert!(run_payload["result"].is_null());
        assert!(
            run_payload["error"]
                .as_str()
                .unwrap()
                .contains("responseSchema validation")
        );
        assert!(
            run_payload["progress"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message
                    .as_str()
                    .is_some_and(|message| message.contains("schema validation failed"))),
            "schema validation error should be visible in the run receipt: {run_payload}"
        );
        assert!(
            run_payload["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["type"] == "task_schema_validation_failed"
                    && event["message"]
                        .as_str()
                        .is_some_and(|message| message.contains("responseSchema validation"))),
            "schema validation event should be visible in the typed receipt: {run_payload}"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn schema_failure_repairs_and_the_receipt_survives_journal_reload() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        // The #5583 report shape: attempt 1 wraps valid JSON in prose (a
        // parse failure), the bounded repair answers with bare corrected
        // JSON. Two model calls, no more — the fake server panics on extras.
        let (client, calls, api_config) = fake_chat_client_responses(&[
            "Sure — here it is:\n```json\n{\"refuted\": true}\n```\nDone!",
            "{\"refuted\": true}",
        ])
        .await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(manager, runtime);

        let run = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"
                    return await task({
                        description: "Return the schema fixture.",
                        responseSchema: {
                            type: "object",
                            properties: { refuted: { type: "boolean" } },
                            required: ["refuted"],
                        },
                    });
                    "#
                }),
                &ctx,
            )
            .await
            .expect("workflow run returns a record");
        let run_payload: Value = serde_json::from_str(&run.content).expect("run json");

        assert_eq!(run_payload["status"], "completed", "{run_payload}");
        assert_eq!(run_payload["result"]["refuted"], json!(true));
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one repair re-ask");
        assert_eq!(
            run.metadata
                .as_ref()
                .and_then(|metadata| metadata.get("schema_repair_count"))
                .and_then(Value::as_u64),
            Some(1),
            "the repair should be counted in the run metadata"
        );
        assert!(
            run_payload["progress"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message
                    .as_str()
                    .is_some_and(|message| message.contains("dispatching a bounded repair"))),
            "the repair should be visible in the run receipt: {run_payload}"
        );

        // The durable receipt (kind, attempt, bounded raw preview) survives a
        // journal reload — a restarted session still sees what was repaired.
        let state = WorkflowWorkspaceState::open(tmp.path());
        let record = state
            .runs
            .lock()
            .expect("runs")
            .get(run_payload["run_id"].as_str().expect("run id"))
            .cloned()
            .expect("record survives reload");
        assert_eq!(record.schema_repairs.len(), 1);
        let repair = &record.schema_repairs[0];
        assert_eq!(repair.kind, "json_parse");
        assert_eq!(repair.attempt, 1);
        assert!(repair.raw_preview.contains("Sure — here it is"));
        assert!(!repair.raw_truncated);
        assert!(repair.artifact.is_none(), "a short reply needs no artifact");
        assert!(
            record.schema_errors.is_empty(),
            "a successful repair leaves no terminal schema error"
        );
    }

    #[test]
    fn bounded_raw_preview_truncates_on_a_char_boundary() {
        let short = "plain reply";
        assert_eq!(bounded_raw_preview(short), short);
        let long = "é".repeat(SCHEMA_RAW_PREVIEW_CHARS + 5);
        let preview = bounded_raw_preview(&long);
        assert!(preview.contains("preview truncated"));
        let kept = preview.lines().next().expect("kept line");
        assert_eq!(kept.chars().count(), SCHEMA_RAW_PREVIEW_CHARS);
    }

    #[test]
    fn schema_raw_artifacts_land_beside_the_report_and_refuse_bad_ids() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write_schema_raw_artifact(
            tmp.path(),
            "run-2049",
            "agent_0007",
            2,
            "the full raw reply",
        )
        .expect("artifact path");
        let written = std::fs::read_to_string(&path).expect("artifact written");
        assert_eq!(written, "the full raw reply");
        assert!(
            path.ends_with("run-2049.schema.agent_0007.attempt2.txt"),
            "{path}"
        );
        // The slug filter sanitizes hostile ids into safe file names rather
        // than refusing: path traversal cannot survive it.
        let sanitized = write_schema_raw_artifact(tmp.path(), "../../etc", "agent_0007", 1, "x")
            .expect("sanitized path");
        assert!(
            sanitized.contains("etc.schema.agent_0007.attempt1.txt"),
            "{sanitized}"
        );
        assert!(
            !sanitized.contains(".."),
            "no parent traversal may survive: {sanitized}"
        );
        assert!(
            write_schema_raw_artifact(tmp.path(), "///", "agent_0007", 1, "x").is_none(),
            "an id with no slug characters must refuse rather than write"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn declarative_issue_audit_fixture_runs_through_subagent_driver() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let workflow_dir = tmp.path().join("workflows");
        std::fs::create_dir_all(&workflow_dir).expect("workflow dir");
        let fixture = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../workflows/issue_audit.workflow.js"),
        )
        .expect("issue audit fixture");
        std::fs::write(workflow_dir.join("issue_audit.workflow.js"), fixture)
            .expect("write fixture into workspace");

        let mut ctx = ToolContext::new(tmp.path().to_path_buf());
        ctx.runtime.work = Some(crate::work_graph::new_shared_work_runtime(
            crate::tools::todo::new_shared_todo_list(),
            crate::tools::plan::new_shared_plan_state(),
        ));
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 4);
        let (client, calls, api_config) = fake_chat_client("audited").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "source_path": "workflows/issue_audit.workflow.js"
                }),
                &ctx,
            )
            .await
            .expect("declarative workflow should complete");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(payload["result"]["code-audit"], "audited");
        assert_eq!(payload["result"]["test-audit"], "audited");
        assert_eq!(payload["result"]["docs-audit"], "audited");
        assert_eq!(payload["result"]["synthesize-release-risk"], "audited");
        assert_eq!(payload["execution"]["status"], "succeeded");
        assert_eq!(
            payload["execution"]["leaf_results"]
                .as_array()
                .expect("leaf results")
                .len(),
            3
        );
        assert_eq!(
            payload["execution"]["branch_results"][0]["branch_id"],
            "parallel-audit"
        );
        assert!(
            payload["execution"]["control_node_results"]
                .as_array()
                .expect("control results")
                .iter()
                .any(|result| result["node_id"] == "synthesize-release-risk"
                    && result["kind"] == "reduce"
                    && result["status"] == "succeeded")
        );
        assert_eq!(payload["child_ids"].as_array().unwrap().len(), 4);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert!(
            payload["progress"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message == "phase: parallel-audit")
        );

        // Operate projects Workflow fan-out/fan-in and its children through
        // the same canonical Work Graph. The Workflow remains the accountable
        // operation identity while the worker bindings stay inspectable; no
        // second plan/strategy lifecycle is created for the reduce step.
        let work = ctx.runtime.work.as_ref().expect("work runtime");
        let graph = work
            .capture(Some(&ctx.state_namespace))
            .expect("capture workflow work")
            .expect("workflow graph")
            .graph;
        let workflow_external = format!(
            "workflow:{}",
            payload["run_id"].as_str().expect("workflow run id")
        );
        let workflow_operations = graph
            .nodes
            .iter()
            .filter(|node| {
                node.binding
                    .as_ref()
                    .is_some_and(|binding| binding.external == workflow_external)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            workflow_operations.len(),
            1,
            "one accountable Workflow operation: {graph:#?}"
        );
        assert_eq!(
            workflow_operations[0].state,
            crate::work_graph::NodeState::Completed
        );
        for child_id in payload["child_ids"].as_array().expect("workflow child ids") {
            let worker_external = format!(
                "worker:{}",
                child_id.as_str().expect("workflow child id string")
            );
            assert!(
                graph.nodes.iter().any(|node| {
                    node.binding
                        .as_ref()
                        .is_some_and(|binding| binding.external == worker_external)
                }),
                "Workflow worker {worker_external} must remain inspectable in the same graph: {graph:#?}"
            );
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn stopship_acceptance_fixture_emits_role_gate_and_terminal_receipts() {
        let _retry_guard = workflow_test_retry_guard();
        let _env_lock = crate::test_support::lock_test_env();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let workflow_dir = tmp.path().join("workflows");
        let fleet_dir = tmp.path().join("fleets");
        std::fs::create_dir_all(&workflow_dir).expect("workflow dir");
        std::fs::create_dir_all(&fleet_dir).expect("fleet dir");
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::copy(
            repo_root.join("workflows/stopship.workflow.js"),
            workflow_dir.join("stopship.workflow.js"),
        )
        .expect("copy stopship acceptance fixture");
        std::fs::copy(
            repo_root.join("fleets/stopship.toml"),
            fleet_dir.join("stopship.toml"),
        )
        .expect("copy stopship fleet");

        let source = std::fs::read_to_string(workflow_dir.join("stopship.workflow.js"))
            .expect("read stopship acceptance fixture");
        let compiled =
            codewhale_workflow::compile_javascript_workflow("stopship.workflow.js", &source)
                .expect("compile stopship acceptance fixture");
        let codewhale_workflow::WorkflowNode::Sequence(sequence) = &compiled.nodes[0] else {
            panic!("stopship fixture should be one ordered role chain");
        };
        for (index, node) in sequence.children.iter().enumerate() {
            let codewhale_workflow::WorkflowNode::Leaf(leaf) = node else {
                panic!("stopship role chain must contain only leaves");
            };
            for path in &leaf.file_scope {
                assert!(
                    repo_root.join(path).is_file(),
                    "{} scopes a file that does not exist: {path}",
                    leaf.id
                );
            }
            let tools = leaf_allowed_tools(leaf).expect("lower stopship child tools");
            if index == 0 {
                // No explicit list: the scout keeps its read-only catalog, and
                // `scout_surface_keeps_tool_search_grep_files_activation_path`
                // pins the tool_search -> deferred grep_files route it uses.
                assert_eq!(tools, None, "the scout keeps its read-only catalog");
            } else {
                assert_eq!(
                    tools,
                    Some(Vec::<String>::new()),
                    "downstream handoff consumer {} must receive no tools",
                    leaf.id
                );
            }
        }

        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 8);
        let responses = [
            r#"APPROVE
SOURCE EVIDENCE
- crates/cli/src/lib.rs: load_named_fleet
- crates/workflow/src/role_resolve.rs: resolve_workflow_agent
- crates/cli/src/lib.rs: start_lane
- crates/tui/src/tools/workflow/mod.rs: record_task_started
- crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::GateUpdated
- crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::RunCompleted -> terminal_completed_receipt
- crates/lane/src/runtime.rs: process_exit_receipt -> lane_reconciled"#,
            r#"APPROVE
PLAN
- fleets/stopship.toml: name = "stopship" -> named Fleet loading
- crates/workflow/src/role_resolve.rs: resolve_workflow_agent -> role resolution
- crates/cli/src/lib.rs: start_lane -> tmux Lane launch
- crates/tui/src/tools/workflow/mod.rs: record_task_started -> typed task_started
- crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::GateUpdated -> gate promotion
- crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::RunCompleted -> terminal_completed_receipt
- crates/lane/src/runtime.rs: process_exit_receipt -> tmux Lane reconciliation"#,
            r#"APPROVE
EVIDENCE REVIEW
- fleets/stopship.toml: name = "stopship"
- crates/workflow/src/role_resolve.rs: resolve_workflow_agent
- crates/cli/src/lib.rs: start_lane
- crates/tui/src/tools/workflow/mod.rs: record_task_started
- crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::GateUpdated
- crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::RunCompleted -> terminal_completed_receipt
- crates/lane/src/runtime.rs: process_exit_receipt -> lane_reconciled"#,
            r#"APPROVE
EVIDENCE MATRIX
- fleet_load: fleets/stopship.toml: name = "stopship"
- role_resolution: crates/workflow/src/role_resolve.rs: resolve_workflow_agent
- lane_launch: crates/cli/src/lib.rs: start_lane
- task_started: crates/tui/src/tools/workflow/mod.rs: record_task_started
- gate_updated: crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::GateUpdated
- run_completed: crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::RunCompleted -> terminal_completed_receipt
- lane_exit: crates/lane/src/runtime.rs: process_exit_receipt -> lane_reconciled"#,
            r#"APPROVE
FINAL RECEIPT
- fleet_load: fleets/stopship.toml: name = "stopship"
- role_resolution: crates/workflow/src/role_resolve.rs: resolve_workflow_agent
- lane_launch: crates/cli/src/lib.rs: start_lane
- task_started: crates/tui/src/tools/workflow/mod.rs: record_task_started
- gate_updated: crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::GateUpdated
- run_completed: crates/tui/src/tools/workflow/mod.rs: WorkflowUiEventKind::RunCompleted -> terminal_completed_receipt
- lane_exit: crates/lane/src/runtime.rs: process_exit_receipt -> lane_reconciled"#,
        ];
        let (client, calls, api_config) = fake_chat_client_responses(&responses).await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "source_path": "workflows/stopship.workflow.js",
                    "fleet": "stopship",
                    "token_budget": 60_000
                }),
                &ctx,
            )
            .await
            .expect("stopship acceptance workflow returns a terminal record");
        let payload: Value = serde_json::from_str(&result.content).expect("workflow JSON");

        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(payload["execution"]["status"], "succeeded", "{payload}");
        assert_eq!(calls.load(Ordering::SeqCst), 5, "one child per Fleet role");
        let approval = &payload["plan_approval"];
        assert_eq!(approval["decision"], "auto_read_only", "{approval}");
        assert_eq!(approval["token_budget"], 60_000, "{approval}");
        assert_eq!(approval["writes"], false, "{approval}");
        assert_eq!(approval["shell"], false, "{approval}");
        assert_eq!(approval["network"], false, "{approval}");
        assert_eq!(approval["high_budget"], false, "{approval}");
        assert_eq!(approval["elevated"], false, "{approval}");
        assert!(
            approval["reasons"].as_array().is_some_and(Vec::is_empty),
            "{approval}"
        );

        let events = payload["events"].as_array().expect("typed events");
        let started = events
            .iter()
            .filter(|event| event["type"] == "task_started")
            .collect::<Vec<_>>();
        let expected_roles = [
            ("explore", "scout"),
            ("implement", "builder"),
            ("reviewer", "reviewer"),
            ("test", "verifier"),
            ("release_lead", "advisor"),
        ];
        assert_eq!(started.len(), expected_roles.len(), "{started:#?}");
        for (event, (role, profile)) in started.iter().zip(expected_roles) {
            assert_eq!(event["role"], role);
            assert_eq!(event["profile"], profile);
            assert_eq!(event["resolved_profile"], profile);
            assert_eq!(event["workflow_run_id"], payload["run_id"]);
        }

        let gates = events
            .iter()
            .filter(|event| event["type"] == "gate_updated")
            .collect::<Vec<_>>();
        assert_eq!(gates.len(), 5, "{gates:#?}");
        assert!(gates.iter().all(|event| event["state"] == "passed"));
        assert_eq!(gates[0]["role"], "explore");
        assert_eq!(gates[0]["blocked_role"], "implement");
        assert_eq!(gates[3]["role"], "test");
        assert_eq!(gates[3]["blocked_role"], "release_lead");
        assert_eq!(gates[4]["role"], "release_lead");
        assert!(gates[4]["blocked_role"].is_null());

        let promoted = events
            .iter()
            .filter(|event| event["type"] == "handoff_promoted")
            .collect::<Vec<_>>();
        let consumed = events
            .iter()
            .filter(|event| event["type"] == "handoff_consumed")
            .collect::<Vec<_>>();
        let expected_handoffs = [
            ("explore", "implement", "source_evidence"),
            ("implement", "reviewer", "verification_plan"),
            ("reviewer", "test", "review_report"),
            ("test", "release_lead", "verification_report"),
        ];
        assert_eq!(promoted.len(), expected_handoffs.len(), "{promoted:#?}");
        assert_eq!(consumed.len(), expected_handoffs.len(), "{consumed:#?}");
        let artifact_ids = promoted
            .iter()
            .map(|event| {
                event["artifact_id"]
                    .as_str()
                    .filter(|id| id.starts_with("handoff_") && id.len() > "handoff_".len())
                    .expect("opaque non-empty handoff artifact id")
            })
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            artifact_ids.len(),
            promoted.len(),
            "every promotion must have a unique artifact id: {promoted:#?}"
        );
        for (index, (from_role, to_role, kind)) in expected_handoffs.into_iter().enumerate() {
            assert_eq!(promoted[index]["from_role"], from_role);
            assert_eq!(promoted[index]["to_role"], to_role);
            assert_eq!(promoted[index]["kind"], kind);
            assert_eq!(promoted[index]["gate_id"], gates[index]["gate_id"]);
            assert_eq!(
                promoted[index]["producer_task_id"],
                started[index]["task_id"]
            );
            assert!(
                promoted[index].get("payload").is_none(),
                "{:#?}",
                promoted[index]
            );

            assert_eq!(
                consumed[index]["artifact_id"],
                promoted[index]["artifact_id"]
            );
            assert_eq!(consumed[index]["from_role"], from_role);
            assert_eq!(consumed[index]["to_role"], to_role);
            assert_eq!(consumed[index]["kind"], kind);
            assert_eq!(
                consumed[index]["consumer_task_id"],
                started[index + 1]["task_id"]
            );
            assert!(
                consumed[index].get("payload").is_none(),
                "{:#?}",
                consumed[index]
            );

            let producer_task_id = promoted[index]["producer_task_id"]
                .as_str()
                .expect("producer task id");
            let consumer_task_id = consumed[index]["consumer_task_id"]
                .as_str()
                .expect("consumer task id");
            let gate_id = promoted[index]["gate_id"].as_str().expect("gate id");
            let artifact_id = promoted[index]["artifact_id"]
                .as_str()
                .expect("artifact id");
            let task_completed_index = events
                .iter()
                .position(|event| {
                    event["type"] == "task_completed" && event["task_id"] == producer_task_id
                })
                .expect("producer completion receipt");
            let gate_updated_index = events
                .iter()
                .position(|event| event["type"] == "gate_updated" && event["gate_id"] == gate_id)
                .expect("gate update receipt");
            let promoted_index = events
                .iter()
                .position(|event| {
                    event["type"] == "handoff_promoted" && event["artifact_id"] == artifact_id
                })
                .expect("handoff promotion receipt");
            let consumer_started_index = events
                .iter()
                .position(|event| {
                    event["type"] == "task_started" && event["task_id"] == consumer_task_id
                })
                .expect("consumer start receipt");
            let consumed_index = events
                .iter()
                .position(|event| {
                    event["type"] == "handoff_consumed" && event["artifact_id"] == artifact_id
                })
                .expect("handoff consumption receipt");
            assert!(
                task_completed_index < gate_updated_index
                    && gate_updated_index < promoted_index
                    && promoted_index < consumer_started_index
                    && consumer_started_index < consumed_index,
                "causal receipt order must be task_completed -> gate_updated -> handoff_promoted -> task_started -> handoff_consumed: {events:#?}"
            );
        }
        let terminal_completed_receipt = events
            .iter()
            .any(|event| event["type"] == "run_completed" && event["status"] == "completed");
        assert!(terminal_completed_receipt, "{events:#?}");
    }

    /// The deadline is the point of this test: an id the manager never records
    /// must fail closed on the first read. With the old retry loop the body
    /// slept 50 x 20ms before answering, and this timeout fires. A plain
    /// `#[tokio::test]` is deliberate — `start_paused = true` auto-advances
    /// time and would let the polling version pass (#6211).
    #[tokio::test]
    async fn completion_from_manager_fails_closed_without_polling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);

        let (completion, usage) = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            completion_from_manager(manager, "missing_agent", "fallback".to_string()),
        )
        .await
        .expect("completion_from_manager must answer from one read, not by polling");
        assert!(usage.is_none(), "fail-closed path carries no telemetry");
        match completion {
            TaskCompletion::Failed { message } => {
                assert!(message.contains("no terminal manager record"), "{message}");
            }
            other => panic!("expected a fail-closed failure, got {other:?}"),
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn task_completed_and_run_completed_carry_usage_telemetry() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, api_config) = fake_chat_client("telemetry-output").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager,
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(runtime.manager.clone(), runtime);

        let run = tool
            .execute(
                json!({
                    "action": "run",
                    "script": r#"export default workflow({
                        "id": "telemetry-fixture",
                        "goal": "usage telemetry",
                        "nodes": [
                            {
                                "agent": {
                                    "id": "inspect",
                                    "prompt": "Inspect the code.",
                                    "agent_type": "review"
                                }
                            }
                        ]
                    });"#
                }),
                &ctx,
            )
            .await
            .expect("workflow run");
        let payload: Value = serde_json::from_str(&run.content).expect("run json");
        assert_eq!(payload["status"], "completed", "{payload}");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{payload}");

        let events = payload["events"].as_array().expect("typed events");
        let task_completed = events
            .iter()
            .find(|event| event["type"] == "task_completed")
            .expect("task_completed event");
        let usage = &task_completed["usage"];
        let input = usage["input_tokens"].as_u64().expect("worker input tokens");
        let output = usage["output_tokens"]
            .as_u64()
            .expect("worker output tokens");
        let total = usage["total_tokens"].as_u64().expect("worker total tokens");
        assert!(total >= input + output, "{task_completed}");
        assert!(
            usage["tool_calls"].as_u64().unwrap_or_default() >= 1,
            "{task_completed}"
        );
        assert!(usage["duration_ms"].is_u64(), "{task_completed}");
        // #4039: a row may only label tokens as provider-reported when the
        // worker ledger actually received them.
        assert_eq!(
            usage["token_source"], "provider_reported",
            "{task_completed}"
        );
        assert!(
            usage["result_ref"]
                .as_str()
                .is_some_and(|target| target.starts_with("agent:")),
            "{task_completed}"
        );

        // Run totals reconcile exactly with the per-task telemetry.
        let run_completed = events
            .iter()
            .find(|event| event["type"] == "run_completed")
            .expect("run_completed event");
        let run_usage = &run_completed["usage"];
        assert_eq!(run_usage["total_tokens"], total, "{run_completed}");
        assert_eq!(run_usage["input_tokens"], input, "{run_completed}");
        assert_eq!(run_usage["output_tokens"], output, "{run_completed}");
        assert_eq!(
            run_usage["tool_calls"], usage["tool_calls"],
            "{run_completed}"
        );
        assert_eq!(run_usage["tasks_reported"], 1, "{run_completed}");

        // Totals also land on the persisted record and execution receipt.
        assert_eq!(payload["usage"]["total_tokens"], total, "{payload}");
        assert_eq!(
            payload["execution"]["usage"]["input_tokens"], input,
            "{payload}"
        );
        assert_eq!(
            payload["execution"]["leaf_results"][0]["usage"]["input_tokens"], input,
            "{payload}"
        );
    }

    #[test]
    fn provider_usage_presence_preserves_a_real_zero_receipt() {
        assert!(!provider_usage_was_reported(None, None, None));
        assert!(provider_usage_was_reported(Some(0), Some(0), Some(0)));
        assert!(provider_usage_was_reported(None, Some(0), None));
    }

    #[test]
    fn run_usage_totals_reconcile_task_telemetry() {
        let task_usage = |total: u64, calls: u32| WorkflowTaskUsage {
            input_tokens: Some(total / 2),
            output_tokens: Some(total - total / 2),
            total_tokens: Some(total),
            cost_microusd: Some(total),
            tool_calls: Some(calls),
            duration_ms: Some(7),
            result_ref: None,
            token_source: Some(WorkflowTokenSource::ProviderReported),
        };
        let record = |agent_id: &str, usage: Option<WorkflowTaskUsage>| RuntimeTaskRecord {
            agent_id: agent_id.to_string(),
            label: None,
            role: None,
            status: IrWorkflowRunStatus::Succeeded,
            output: None,
            schema_error: None,
            usage,
        };
        let records = vec![
            record("a", Some(task_usage(100, 2))),
            record("b", Some(task_usage(60, 1))),
            record("c", None),
        ];
        let totals = run_usage_totals(&records).expect("totals");
        assert_eq!(totals.total_tokens, Some(160));
        assert_eq!(totals.cost_microusd, Some(160));
        assert_eq!(totals.input_tokens, Some(80));
        assert_eq!(totals.output_tokens, Some(80));
        assert_eq!(totals.tool_calls, Some(3));
        assert_eq!(totals.tasks_reported, 2);

        assert!(run_usage_totals(&[]).is_none());
        assert!(run_usage_totals(&[record("d", None)]).is_none());
    }

    #[test]
    fn run_and_ir_usage_keep_unknown_distinct_from_reported_zero() {
        let record = |agent_id: &str, usage: WorkflowTaskUsage| RuntimeTaskRecord {
            agent_id: agent_id.to_string(),
            label: Some(agent_id.to_string()),
            role: None,
            status: IrWorkflowRunStatus::Succeeded,
            output: None,
            schema_error: None,
            usage: Some(usage),
        };
        let unknown = WorkflowTaskUsage {
            tool_calls: Some(1),
            duration_ms: Some(4),
            ..WorkflowTaskUsage::default()
        };
        let reported_zero = WorkflowTaskUsage {
            input_tokens: Some(0),
            output_tokens: Some(0),
            total_tokens: Some(0),
            cost_microusd: Some(0),
            tool_calls: Some(0),
            duration_ms: Some(0),
            token_source: Some(WorkflowTokenSource::ProviderReported),
            ..WorkflowTaskUsage::default()
        };

        let unknown_totals = run_usage_totals(&[record("unknown", unknown.clone())])
            .expect("tool/duration receipt still creates run usage");
        assert_eq!(unknown_totals.input_tokens, None);
        assert_eq!(unknown_totals.output_tokens, None);
        assert_eq!(unknown_totals.total_tokens, None);
        assert_eq!(unknown_totals.tool_calls, Some(1));

        let zero_totals = run_usage_totals(&[record("zero", reported_zero.clone())])
            .expect("reported zero receipt");
        assert_eq!(zero_totals.input_tokens, Some(0));
        assert_eq!(zero_totals.output_tokens, Some(0));
        assert_eq!(zero_totals.total_tokens, Some(0));
        assert_eq!(zero_totals.cost_microusd, Some(0));
        assert_eq!(zero_totals.tool_calls, Some(0));

        let unknown_ir = workflow_usage_from_task(&unknown);
        assert_eq!(unknown_ir.input_tokens, None);
        assert_eq!(unknown_ir.output_tokens, None);
        assert_eq!(unknown_ir.cost_microusd, None);
        let zero_ir = workflow_usage_from_task(&reported_zero);
        assert_eq!(zero_ir.input_tokens, Some(0));
        assert_eq!(zero_ir.output_tokens, Some(0));
        assert_eq!(zero_ir.cost_microusd, Some(0));

        let mixed = run_usage_totals(&[record("zero", reported_zero), record("unknown", unknown)])
            .expect("mixed receipts");
        assert_eq!(
            mixed.total_tokens,
            Some(0),
            "a missing contributor keeps the observed subtotal"
        );
        assert_eq!(mixed.input_tokens, Some(0));
        assert_eq!(mixed.output_tokens, Some(0));
        assert_eq!(mixed.cost_microusd, Some(0));
        assert_eq!(mixed.tool_calls, Some(1));
    }

    #[test]
    fn workflow_ui_event_usage_telemetry_serde_round_trip() {
        let event = WorkflowUiEvent::at(
            7,
            "session-test",
            WorkflowUiEventKind::TaskCompleted {
                task_id: "child-1".to_string(),
                status: IrWorkflowRunStatus::Succeeded,
                reason: None,
                kind: None,
                usage: Some(WorkflowTaskUsage {
                    input_tokens: Some(128),
                    output_tokens: Some(32),
                    total_tokens: Some(160),
                    cost_microusd: Some(42),
                    tool_calls: Some(2),
                    duration_ms: Some(42),
                    result_ref: Some("agent:child-1".to_string()),
                    token_source: Some(WorkflowTokenSource::ProviderReported),
                }),
            },
        );
        let json = serde_json::to_value(&event).expect("serialize");
        assert_eq!(json["usage"]["total_tokens"], 160);
        let parsed: WorkflowUiEvent = serde_json::from_value(json).expect("deserialize round trip");
        match parsed.kind {
            WorkflowUiEventKind::TaskCompleted {
                usage: Some(usage), ..
            } => {
                assert_eq!(usage.total_tokens, Some(160));
                assert_eq!(usage.cost_microusd, Some(42));
                assert_eq!(usage.tool_calls, Some(2));
                assert_eq!(usage.duration_ms, Some(42));
            }
            other => panic!("expected task_completed with usage, got {other:?}"),
        }

        // Journals written before #2974 carry no usage fields; they must
        // still parse with `usage == None`.
        let legacy_task: WorkflowUiEvent = serde_json::from_str(
            r#"{"at_ms":5,"type":"task_completed","task_id":"child-1","status":"succeeded"}"#,
        )
        .expect("legacy task_completed parses");
        match legacy_task.kind {
            WorkflowUiEventKind::TaskCompleted { usage: None, .. } => {}
            other => panic!("expected legacy task_completed without usage, got {other:?}"),
        }
        let legacy_run: WorkflowUiEvent = serde_json::from_str(
            r#"{"at_ms":6,"type":"run_completed","status":"completed","error":null}"#,
        )
        .expect("legacy run_completed parses");
        match legacy_run.kind {
            WorkflowUiEventKind::RunCompleted { usage: None, .. } => {}
            other => panic!("expected legacy run_completed without usage, got {other:?}"),
        }

        // A telemetry-less event serializes without a `usage` key, so old
        // consumers see a byte-compatible shape.
        let plain = serde_json::to_value(WorkflowUiEvent::at(
            8,
            "session-test",
            WorkflowUiEventKind::RunCompleted {
                status: WorkflowRunStatus::Completed,
                error: None,
                usage: None,
                result_preview: None,
            },
        ))
        .expect("serialize plain run_completed");
        assert!(plain.get("usage").is_none(), "{plain}");
    }

    #[test]
    fn run_record_event_retention_is_bounded() {
        let mut record = WorkflowRunRecord::new(
            "workflow_tail".to_string(),
            Some("session-test".to_string()),
            None,
            None,
            None,
        );
        for index in 0..(WORKFLOW_RUN_EVENTS_MAX_RETAINED + 5) {
            record.push_event(WorkflowUiEvent::at(
                index as u64,
                "session-test",
                WorkflowUiEventKind::Log {
                    message: format!("log {index}"),
                },
            ));
        }
        assert_eq!(record.events.len(), WORKFLOW_RUN_EVENTS_MAX_RETAINED);
        assert_eq!(record.events_dropped, 5);
        assert_eq!(
            record.events_total,
            (WORKFLOW_RUN_EVENTS_MAX_RETAINED + 5) as u64
        );
        // The retained window is the newest tail.
        let first = &record.events[0];
        assert_eq!(first.at_ms, 5);
        // Summaries report the truthful total, not the retained tail length.
        let summary = record.summary();
        assert_eq!(summary.event_count, WORKFLOW_RUN_EVENTS_MAX_RETAINED + 5);
        assert_eq!(summary.events_dropped, 5);
        assert_eq!(summary.last_event_type.as_deref(), Some("log"));
    }

    #[test]
    fn run_record_progress_and_rejection_ledgers_are_bounded_with_exact_counts() {
        let mut record = WorkflowRunRecord::new(
            "workflow_rejection_loop".to_string(),
            Some("session-test".to_string()),
            None,
            None,
            None,
        );
        let progress_total = WORKFLOW_RUN_PROGRESS_MAX_RETAINED + 7;
        for index in 0..progress_total {
            record.push_progress(format!("progress {index}"));
        }
        let rejection_total = WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED + 7;
        for index in 0..rejection_total {
            record.push_dispatch_failure(WorkflowDispatchFailure {
                at_ms: index as u64,
                label: Some(format!("rejected-{index}")),
                phase: Some("fan-out".to_string()),
                message: "invalid task options".to_string(),
            });
        }

        assert_eq!(record.progress.len(), WORKFLOW_RUN_PROGRESS_MAX_RETAINED);
        assert_eq!(record.progress_count, progress_total as u64);
        assert_eq!(
            record.progress.first().map(String::as_str),
            Some("progress 7")
        );
        assert_eq!(
            record.dispatch_failures.len(),
            WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED
        );
        assert_eq!(record.dispatch_failure_count, rejection_total as u64);
        assert_eq!(record.dispatch_failures[0].at_ms, 7);

        let summary = record.summary();
        assert_eq!(summary.progress_count, progress_total as u64);
        assert_eq!(summary.dispatch_failure_count, rejection_total as u64);

        let (payload, bounds) = bounded_run_record_value(&record, Path::new("workflow-runs.jsonl"));
        assert_eq!(payload["progress_count"], progress_total as u64);
        assert_eq!(payload["dispatch_failure_count"], rejection_total as u64);
        assert_eq!(
            bounds.dispatch_failures_omitted,
            (rejection_total - WORKFLOW_RESULT_DISPATCH_FAILURES_TAIL) as u64
        );

        // Imported counters can already be saturated. Another rejection must
        // retain its newest detail without wrapping the authoritative total.
        record.dispatch_failure_count = u64::MAX;
        record.push_dispatch_failure(WorkflowDispatchFailure {
            at_ms: u64::MAX,
            label: None,
            phase: None,
            message: "malformed rejection loop".to_string(),
        });
        assert_eq!(record.dispatch_failure_count, u64::MAX);
        assert_eq!(
            record.dispatch_failures.len(),
            WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED
        );
        assert_eq!(
            record.dispatch_failures.last().map(|failure| failure.at_ms),
            Some(u64::MAX)
        );
    }

    #[test]
    fn workflow_status_payload_bounds_oversized_run_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_big_run".to_string();
        let mut record = WorkflowRunRecord::new(
            run_id.clone(),
            Some("session-test".to_string()),
            None,
            None,
            None,
        );
        record.status = WorkflowRunStatus::Completed;
        // A high-fan-out run: far more events/progress than the model needs.
        for index in 0..200u64 {
            record.push_event(WorkflowUiEvent::at(
                index,
                "session-test",
                WorkflowUiEventKind::Log {
                    message: format!("fan-out event {index}"),
                },
            ));
        }
        for index in 0..60 {
            record.push_progress(format!("progress line {index}"));
        }
        for index in 0..40u64 {
            record.push_dispatch_failure(WorkflowDispatchFailure {
                at_ms: index,
                label: Some(format!("rejected-{index}")),
                phase: Some("fan-out".to_string()),
                message: format!("dispatch rejected {index}: {}", "x".repeat(2_000)),
            });
        }
        record.result = Some(json!({ "blob": "r".repeat(10_000) }));
        record.execution = Some(IrWorkflowExecution {
            status: IrWorkflowRunStatus::Succeeded,
            usage: WorkflowUsage::default(),
            memo_usage: WorkflowMemoUsage::default(),
            leaf_results: vec![LeafResult {
                leaf_id: "inspect".to_string(),
                task_id: "agent_1".to_string(),
                role: None,
                profile: None,
                status: IrWorkflowRunStatus::Succeeded,
                usage: WorkflowUsage::default(),
                memo_usage: WorkflowMemoUsage::default(),
                output: Some("o".repeat(5_000)),
                artifacts: Vec::new(),
                schema_error: None,
            }],
            branch_results: Vec::new(),
            control_node_results: Vec::new(),
        });
        state
            .runs
            .lock()
            .expect("runs")
            .insert(run_id.clone(), record);

        let result = workflow_result_for(&run_id, state, "session-test").expect("status result");
        assert!(
            result.content.len() < WORKFLOW_RESULT_MAX_CHARS,
            "bounded payload must stay under {WORKFLOW_RESULT_MAX_CHARS} chars, got {}",
            result.content.len()
        );
        let payload: Value = serde_json::from_str(&result.content).expect("payload json");

        let events = payload["events"].as_array().expect("events");
        assert_eq!(events.len(), WORKFLOW_RESULT_EVENTS_TAIL, "{payload}");
        assert!(
            payload["events_note"]
                .as_str()
                .is_some_and(|note| note.contains("workflow-runs.jsonl")),
            "{payload}"
        );
        // The retained window is the newest events.
        assert_eq!(events[0]["at_ms"], 150);

        let progress = payload["progress"].as_array().expect("progress");
        assert_eq!(progress.len(), WORKFLOW_RESULT_PROGRESS_TAIL, "{payload}");
        assert!(payload.get("progress_note").is_some(), "{payload}");

        let dispatch_failures = payload["dispatch_failures"]
            .as_array()
            .expect("dispatch failures");
        assert_eq!(
            dispatch_failures.len(),
            WORKFLOW_RESULT_DISPATCH_FAILURES_TAIL,
            "{payload}"
        );
        assert_eq!(dispatch_failures[0]["at_ms"], 28);
        assert_eq!(
            payload["dispatch_failure_count"], 40,
            "exact count must survive the bounded failure tail"
        );
        assert!(
            dispatch_failures.iter().all(|failure| failure["message"]
                .as_str()
                .is_some_and(|message| message.chars().count()
                    <= WORKFLOW_RESULT_DISPATCH_FAILURE_FIELD_MAX_CHARS)),
            "{dispatch_failures:?}"
        );
        assert!(
            payload["dispatch_failures_note"]
                .as_str()
                .is_some_and(|note| note.contains("workflow-runs.jsonl")),
            "{payload}"
        );

        // Oversized VM result collapses to a preview with a journal pointer.
        assert_eq!(payload["result"]["truncated"], true, "{payload}");
        assert!(
            payload["result"]["preview"]
                .as_str()
                .is_some_and(|preview| preview.chars().count() <= WORKFLOW_RESULT_VALUE_MAX_CHARS),
            "{payload}"
        );
        assert!(
            payload["result"]["full_detail"]
                .as_str()
                .is_some_and(|path| path.contains("workflow-runs.jsonl")),
            "{payload}"
        );

        // Leaf outputs carry a bounded preview instead of full child text.
        let leaf_output = payload["execution"]["leaf_results"][0]["output"]
            .as_str()
            .expect("leaf output");
        assert!(
            leaf_output.contains("leaf output truncated"),
            "{leaf_output}"
        );
        assert!(leaf_output.len() < 1_000, "{leaf_output}");

        let metadata = result.metadata.expect("metadata");
        assert_eq!(metadata["events_returned"], WORKFLOW_RESULT_EVENTS_TAIL);
        assert_eq!(metadata["events_omitted"], 150);
        assert_eq!(metadata["event_count"], 200);
        assert_eq!(
            metadata["dispatch_failures_returned"],
            WORKFLOW_RESULT_DISPATCH_FAILURES_TAIL
        );
        assert_eq!(metadata["dispatch_failures_omitted"], 28);
        assert_eq!(metadata["truncated"], true);
        assert!(
            metadata["journal_path"]
                .as_str()
                .is_some_and(|path| path.contains("workflow-runs.jsonl")),
            "{metadata}"
        );
    }

    #[test]
    fn workflow_status_payload_keeps_small_records_intact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_small_run".to_string();
        let mut record = WorkflowRunRecord::new(
            run_id.clone(),
            Some("session-test".to_string()),
            None,
            None,
            None,
        );
        record.status = WorkflowRunStatus::Completed;
        record.push_event(WorkflowUiEvent::at(
            1,
            "session-test",
            WorkflowUiEventKind::PhaseStarted {
                title: "scan".to_string(),
            },
        ));
        record.result = Some(json!({ "ok": true }));
        state
            .runs
            .lock()
            .expect("runs")
            .insert(run_id.clone(), record);

        let result = workflow_result_for(&run_id, state, "session-test").expect("status result");
        let payload: Value = serde_json::from_str(&result.content).expect("payload json");
        assert_eq!(payload["events"].as_array().map(Vec::len), Some(1));
        assert_eq!(payload["result"], json!({ "ok": true }));
        assert!(payload.get("events_note").is_none(), "{payload}");
        assert!(payload.get("progress_note").is_none(), "{payload}");
        let metadata = result.metadata.expect("metadata");
        assert_eq!(metadata["truncated"], false);
        assert_eq!(metadata["events_omitted"], 0);
        assert_eq!(metadata["events_returned"], 1);
    }
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn workflow_cancel_interrupts_vm_and_blocks_further_spawns() {
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 4);
        let (client, calls, api_config) = fake_chat_client("child done").await;
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(256);
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            Some(event_tx),
            manager.clone(),
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(manager.clone(), runtime);

        let started = tool
            .execute(
                json!({
                    "action": "start",
                    "script": r#"
                        let n = 0;
                        while (n < 20) {
                            await task({ description: `task ${n}`, type: 'explore', allowedTools: [] });
                            n++;
                        }
                        return n;
                    "#
                }),
                &ctx,
            )
            .await
            .expect("workflow start");
        let run_id = started
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("run_id"))
            .and_then(Value::as_str)
            .expect("run_id metadata");

        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("workflow should spawn at least one child before cancel");
        let calls_before_cancel = calls.load(Ordering::SeqCst);
        assert!(calls_before_cancel >= 1);

        let cancelled = tool
            .execute(json!({"action": "cancel", "run_id": run_id}), &ctx)
            .await
            .expect("workflow cancel");
        let cancelled_payload: Value =
            serde_json::from_str(&cancelled.content).expect("cancel json");
        assert_eq!(cancelled_payload["status"], "cancelled");
        assert!(
            cancelled_payload["events"]
                .as_array()
                .is_some_and(|events| events.iter().any(|event| event["type"] == "run_cancelled")),
            "cancel receipt must include the authoritative terminal event: {cancelled_payload}"
        );
        let mut streamed_cancel = false;
        while let Ok(event) = event_rx.try_recv() {
            if let Event::WorkflowUi { event, .. } = event
                && event["type"] == "run_cancelled"
            {
                streamed_cancel = true;
            }
        }
        assert!(
            streamed_cancel,
            "cancel must stream a terminal UI event after any racing completion"
        );
        let first_event_count = cancelled_payload["events"]
            .as_array()
            .expect("events")
            .len();
        let first_completed_at = cancelled_payload["completed_at_ms"].clone();
        let cancelled_again = tool
            .execute(json!({"action": "cancel", "run_id": run_id}), &ctx)
            .await
            .expect("second workflow cancel is a no-op");
        let cancelled_again_payload: Value =
            serde_json::from_str(&cancelled_again.content).expect("second cancel json");
        assert_eq!(cancelled_again_payload["status"], "cancelled");
        assert_eq!(
            cancelled_again_payload["events"]
                .as_array()
                .expect("events")
                .len(),
            first_event_count,
            "second cancel must not append a duplicate terminal event"
        );
        assert_eq!(
            cancelled_again_payload["completed_at_ms"], first_completed_at,
            "second cancel must preserve the original completion time"
        );

        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        let calls_after_cancel = calls.load(Ordering::SeqCst);
        assert!(
            calls_after_cancel <= calls_before_cancel + 1,
            "cancelled workflow kept spawning children: before={calls_before_cancel} after={calls_after_cancel}"
        );
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn workflow_token_budget_is_reported_never_enforced() {
        // #6189: a declared workflow `token_budget` survives as a reported
        // ceiling in the run snapshot; it no longer seeds a manager budget
        // scope, clamps a child's provider request, or stops a run.
        let _retry_guard = workflow_test_retry_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (client, calls, bodies, api_config) = fake_chat_client_capturing("budgeted").await;
        let runtime = SubAgentRuntime::new(
            client,
            "deepseek-v4-flash".to_string(),
            ctx.clone(),
            true,
            None,
            manager.clone(),
        )
        .with_api_config(api_config);
        let tool = WorkflowTool::new(manager.clone(), runtime);

        let result = tool
            .execute(
                json!({
                    "action": "run",
                    "token_budget": 1000,
                    "script": r#"
                        await task({ description: 'budgeted work', type: 'explore', allowedTools: [] });
                        return { spent: budget.spent(), total: budget.total, remaining: budget.remaining() };
                    "#
                }),
                &ctx,
            )
            .await
            .expect("budget workflow should complete");
        let payload: Value = serde_json::from_str(&result.content).expect("json result");

        assert_eq!(payload["status"], "completed", "{payload}");
        // #6189 removed per-scope token tracking: `spent` is deliberately 0
        // and the declared ceiling survives only as a reported total.
        assert_eq!(payload["result"]["spent"], 0, "{payload}");
        assert_eq!(payload["result"]["total"], 1000);
        assert_eq!(payload["result"]["remaining"], 1000);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let bodies = bodies.lock().expect("captured first request");
        let max_tokens = bodies[0]
            .get("max_tokens")
            .or_else(|| bodies[0].get("max_completion_tokens"))
            .and_then(Value::as_u64);
        assert!(
            max_tokens.is_some_and(|tokens| tokens > 1000),
            "a declared token budget must not clamp the child's provider request (#6189): {max_tokens:?}"
        );
    }

    #[tokio::test]
    async fn identical_budget_snapshots_emit_a_live_heartbeat_without_journal_duplication() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx,
            true,
            Some(event_tx),
            manager.clone(),
        );
        let state = WorkflowWorkspaceState::open(tmp.path());
        let run_id = "workflow_budget_heartbeat".to_string();
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".to_string()),
                None,
                None,
                None,
            ),
        );
        let driver = SubAgentWorkflowDriver::new(
            run_id.clone(),
            "session-test".to_string(),
            manager,
            runtime,
            state.clone(),
            Some(1_000),
            WorkflowFleetBinding::None,
            Vec::new(),
            tmp.path().to_path_buf(),
        );
        let snapshot = BudgetSnapshot {
            total: Some(1_000),
            spent: 0,
        };

        driver.record_budget_snapshot(snapshot);
        driver.record_budget_snapshot(snapshot);

        let mut streamed = 0;
        while let Ok(Event::WorkflowUi { event, .. }) = event_rx.try_recv() {
            if event["type"] == "budget_updated" {
                streamed += 1;
            }
        }
        assert_eq!(
            streamed, 2,
            "unchanged budget still refreshes the live panel"
        );
        let recorded = state
            .runs
            .lock()
            .expect("runs")
            .get(&run_id)
            .expect("run")
            .events
            .iter()
            .filter(|event| event.event_type() == "budget_updated")
            .count();
        assert_eq!(recorded, 1, "heartbeat must not grow the durable journal");
    }

    fn event_channel_driver(
        workspace: &Path,
        capacity: usize,
    ) -> (Arc<SubAgentWorkflowDriver>, mpsc::Receiver<Event>) {
        let ctx = ToolContext::new(workspace.to_path_buf());
        let manager = new_shared_subagent_manager(workspace.to_path_buf(), 2);
        let (event_tx, event_rx) = mpsc::channel(capacity);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            ctx,
            true,
            Some(event_tx),
            manager.clone(),
        );
        let state = WorkflowWorkspaceState::open(workspace);
        let run_id = "workflow_event_truth".to_string();
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some("session-test".to_string()),
                None,
                None,
                None,
            ),
        );
        let driver = SubAgentWorkflowDriver::new(
            run_id,
            "session-test".to_string(),
            manager,
            runtime,
            state,
            None,
            WorkflowFleetBinding::None,
            Vec::new(),
            workspace.to_path_buf(),
        );
        (driver, event_rx)
    }

    #[tokio::test]
    async fn channel_full_then_run_completed_is_still_delivered() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (driver, mut event_rx) = event_channel_driver(tmp.path(), 1);
        let log = |message: &str| {
            WorkflowUiEvent::new(
                "session-test",
                WorkflowUiEventKind::Log {
                    message: message.to_string(),
                },
            )
        };
        driver.emit_ui_event(&log("fills the channel"));
        driver.emit_ui_event(&log("progress is lossy when full"));
        driver.emit_ui_event(&WorkflowUiEvent::new(
            "session-test",
            WorkflowUiEventKind::RunCompleted {
                status: WorkflowRunStatus::Completed,
                error: None,
                usage: None,
                result_preview: None,
            },
        ));
        let mut types = Vec::new();
        for _ in 0..2 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), event_rx.recv())
                .await
                .expect("terminal event waits for capacity instead of dropping")
                .expect("event channel open");
            let Event::WorkflowUi { event, .. } = event else {
                panic!("unexpected engine event");
            };
            types.push(event["type"].as_str().expect("type").to_string());
        }
        assert_eq!(types, ["log", "run_completed"]);
        assert!(
            event_rx.try_recv().is_err(),
            "the second progress line was dropped, as before"
        );
    }

    #[tokio::test]
    async fn dropped_slots_and_task_stops_are_typed_events() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (driver, mut event_rx) = event_channel_driver(tmp.path(), 32);
        for slot in [1, 3] {
            driver.progress(ProgressEvent::FanoutSlotDropped {
                construct: "parallel".to_string(),
                kind: "task".to_string(),
                slot,
            });
        }
        assert_eq!(driver.dropped_slot_count(), 2);
        let mut dropped = Vec::new();
        while let Ok(Event::WorkflowUi { event, .. }) = event_rx.try_recv() {
            if event["type"] == "slot_dropped" {
                dropped.push((event["index"].clone(), event["construct"].clone()));
            }
        }
        assert_eq!(
            dropped,
            vec![(json!(1), json!("parallel")), (json!(3), json!("parallel"))]
        );

        driver.record_task_request(
            "agent_limit",
            &TaskRequest {
                label: Some("evals".to_string()),
                ..exact_task_request("reviewer")
            },
        );
        driver.record_task_completion(
            "agent_limit",
            &TaskCompletion::BudgetExhausted {
                message: "child step budget exhausted for task execution (limit: 8; used: 8)"
                    .to_string(),
            },
            None,
        );
        let completed = loop {
            let Ok(Event::WorkflowUi { event, .. }) = event_rx.try_recv() else {
                panic!("task_completed was not streamed");
            };
            if event["type"] == "task_completed" {
                break event;
            }
        };
        assert_eq!(completed["status"], "budget_exceeded");
        assert_eq!(completed["kind"], "steps");
        assert!(
            completed["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("limit: 8")),
            "{completed}"
        );
    }

    #[test]
    fn receipt_summary_is_a_plain_sentence_with_the_report_path() {
        let mut record = WorkflowRunRecord::new(
            "workflow_d08d912f".to_string(),
            Some("session-test".to_string()),
            None,
            None,
            None,
        );
        record.status = WorkflowRunStatus::Degraded;
        record.workflow_goal = Some("Compare Cline with Codewhale".to_string());
        let started = |task_id: &str, label: &str| {
            WorkflowUiEvent::at(
                1,
                "session-test",
                WorkflowUiEventKind::TaskStarted(Box::new(WorkflowTaskStartedEvent {
                    task_id: task_id.to_string(),
                    label: Some(label.to_string()),
                    role: None,
                    profile: None,
                    model: None,
                    strength: None,
                    thinking: None,
                    requested_reasoning: None,
                    effective_reasoning: None,
                    resolved_role: None,
                    resolved_profile: None,
                    resolved_provider: "local".to_string(),
                    resolved_model: "stub".to_string(),
                    route_source: "session".to_string(),
                    child_route: None,
                    worktree: false,
                    workspace: None,
                    git_branch: None,
                    parent_task_id: None,
                    depth: 1,
                    workflow_run_id: Some("workflow_d08d912f".to_string()),
                    workflow_phase_id: Some("Survey".to_string()),
                    workflow_task_label: None,
                    workflow_child_index: Some(0),
                    queue_ticket: None,
                    fleet_receipt: None,
                })),
            )
        };
        let completed = |task_id: &str, completion: TaskCompletion| {
            let (status, _) = task_completion_status(&completion);
            let (reason, kind) = view::task_stop(&completion);
            WorkflowUiEvent::at(
                2,
                "session-test",
                WorkflowUiEventKind::TaskCompleted {
                    task_id: task_id.to_string(),
                    status,
                    reason,
                    kind,
                    usage: None,
                },
            )
        };
        record.push_event(started("a", "loop-prompt"));
        record.push_event(completed(
            "a",
            TaskCompletion::Completed {
                text: "ok".to_string(),
            },
        ));
        record.push_event(started("b", "tools-approval"));
        record.push_event(completed(
            "b",
            TaskCompletion::BudgetExhausted {
                message: "child wall-time budget exhausted during task execution".to_string(),
            },
        ));
        record.result = Some(json!({"a": "ok", "b": null}));

        let summary = workflow_receipt_summary(
            &record,
            &[],
            Some(".codewhale/reports/workflow_d08d912f.md"),
        );
        let first_line = summary.lines().next().expect("sentence");
        assert_eq!(
            first_line,
            "Workflow \"Compare Cline with Codewhale\" finished with gaps: 1 of 2 agents finished, \
             1 failed (tools-approval: stopped at the time limit)."
        );
        assert!(summary.contains("\nReport: .codewhale/reports/workflow_d08d912f.md"));
        assert!(!summary.contains("Degraded") && !summary.contains(" ended "));

        let unreported = workflow_receipt_summary(&record, &[], None);
        assert!(
            unreported.contains("No report file was written; `workflow status workflow_d08d912f`"),
            "{unreported}"
        );
    }

    #[test]
    fn receipt_counts_from_the_task_ledger_once_events_were_trimmed() {
        let mut record = WorkflowRunRecord::new(
            "workflow_trimmed".to_string(),
            Some("session-test".to_string()),
            None,
            None,
            None,
        );
        record.status = WorkflowRunStatus::Degraded;
        record.workflow_goal = Some("Survey".to_string());
        // Chatty run: logs push every task_started out of the retained tail.
        for index in 0..(WORKFLOW_RUN_EVENTS_MAX_RETAINED + 50) {
            record.push_event(WorkflowUiEvent::at(
                1,
                "session-test",
                WorkflowUiEventKind::Log {
                    message: format!("line {index}"),
                },
            ));
        }
        let budget = TaskCompletion::BudgetExhausted {
            message: "child step budget exhausted".to_string(),
        };
        let (reason, kind) = view::task_stop(&budget);
        record.push_event(WorkflowUiEvent::at(
            2,
            "session-test",
            WorkflowUiEventKind::TaskCompleted {
                task_id: "agent_mid".to_string(),
                status: IrWorkflowRunStatus::BudgetExceeded,
                reason,
                kind,
                usage: None,
            },
        ));
        record.dispatch_failure_count = 1;
        assert!(record.events_dropped > 0);

        // Events alone still count the agent they name as stopped.
        let events_only = workflow_receipt_summary(&record, &[], None);
        assert!(
            events_only.starts_with(
                "Workflow \"Survey\" finished with gaps: 0 of 1 agent finished, 1 failed \
                 (agent_mid: stopped at the step limit)."
            ),
            "{events_only}"
        );

        let task = |index: usize, status| RuntimeTaskRecord {
            agent_id: format!("agent_{index}"),
            label: None,
            role: None,
            status,
            output: None,
            schema_error: None,
            usage: None,
        };
        let mut ledger: Vec<RuntimeTaskRecord> = (0..598)
            .map(|index| task(index, IrWorkflowRunStatus::Succeeded))
            .collect();
        ledger.push(task(598, IrWorkflowRunStatus::BudgetExceeded));
        ledger.push(task(599, IrWorkflowRunStatus::Failed));
        let summary = workflow_receipt_summary(&record, &ledger, None);
        assert!(
            summary.starts_with(
                "Workflow \"Survey\" finished with gaps: 598 of 601 agents finished, 2 failed, \
                 1 could not start (agent_mid: stopped at the step limit; 2 more)."
            ),
            "{summary}"
        );
    }

    #[tokio::test]
    async fn detached_receipt_links_the_written_report_and_keeps_event_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (tool, context, mut rx) = native_lifecycle_tool(tmp.path());
        let state = shared_workflow_state(tmp.path());
        let run_id = "workflow_report_link".to_string();
        let driver = SubAgentWorkflowDriver::new(
            run_id.clone(),
            context.state_namespace.clone(),
            tool.manager,
            tool.runtime,
            state.clone(),
            None,
            WorkflowFleetBinding::None,
            Vec::new(),
            tmp.path().to_path_buf(),
        );
        state.controllers.lock().expect("controllers").insert(
            run_id.clone(),
            Arc::new(
                WorkflowRunController::new(driver, WorkflowRunCancel::new())
                    .with_parent_completion(true),
            ),
        );
        let mut record = WorkflowRunRecord::new(
            run_id,
            Some(context.state_namespace.clone()),
            None,
            None,
            None,
        );
        record.status = WorkflowRunStatus::Degraded;
        // Many long multibyte labels must not push the receipt past its cap.
        for index in 0..40 {
            let id = format!("agent_{index}");
            record.push_event(WorkflowUiEvent::at(
                1,
                "session-test",
                WorkflowUiEventKind::TaskDispatchFailed {
                    label: Some(format!("{index}{}", "🦀".repeat(200))),
                    phase: None,
                    message: format!("{id}: {}", "🦀".repeat(500)),
                    queue_ticket: None,
                },
            ));
        }
        record.error = Some("🦀".repeat(10_000));
        record.result = Some(json!({"preview": "🦀".repeat(10_000)}));
        write_run_report_artifact(tmp.path(), &record);
        finish_workflow_controller(&state, &record);
        let completion = rx.try_recv().expect("receipt enqueued");
        assert!(
            completion.payload.len() <= WORKFLOW_COMPLETION_MAX_BYTES,
            "{} bytes",
            completion.payload.len()
        );
        let receipt = terminal_workflow_receipt(&completion.payload);
        assert_eq!(receipt["event"], "workflow.failed", "event names unchanged");
        assert_eq!(receipt["status"], "degraded", "status carries the truth");
        assert_eq!(
            receipt["report"],
            ".codewhale/reports/workflow_report_link.md"
        );
        assert!(
            completion
                .payload
                .contains("\nReport: .codewhale/reports/workflow_report_link.md"),
            "{}",
            completion.payload
        );
        assert!(
            completion
                .payload
                .contains("finished with gaps: 0 of 40 agents finished, 40 could not start")
        );
    }

    fn native_lifecycle_tool(
        workspace: &Path,
    ) -> (
        WorkflowTool,
        ToolContext,
        mpsc::Receiver<SubAgentCompletion>,
    ) {
        let context = ToolContext::new(workspace.to_path_buf());
        let manager = new_shared_subagent_manager(workspace.to_path_buf(), 2);
        let (tx, rx) = mpsc::channel(16);
        let runtime = SubAgentRuntime::new(
            stub_client(),
            "deepseek-v4-flash".to_string(),
            context.clone(),
            true,
            None,
            manager.clone(),
        )
        .with_parent_completion_tx(tx);
        (WorkflowTool::new(manager, runtime), context, rx)
    }

    fn native_lifecycle_driver(
        workspace: &Path,
        gates: Vec<GateSpec>,
    ) -> Arc<SubAgentWorkflowDriver> {
        let (tool, context, _rx) = native_lifecycle_tool(workspace);
        let state = shared_workflow_state(workspace);
        let run_id = "workflow_native_fixture".to_string();
        state.runs.lock().expect("runs").insert(
            run_id.clone(),
            WorkflowRunRecord::new(
                run_id.clone(),
                Some(context.state_namespace.clone()),
                None,
                None,
                None,
            ),
        );
        SubAgentWorkflowDriver::new(
            run_id,
            context.state_namespace.clone(),
            tool.manager,
            tool.runtime,
            state,
            None,
            WorkflowFleetBinding::None,
            gates,
            workspace.to_path_buf(),
        )
    }

    fn terminal_workflow_receipt(payload: &str) -> Value {
        let (_, receipt) = payload
            .rsplit_once("<codewhale:subagent.done>")
            .expect("terminal envelope");
        serde_json::from_str(
            receipt
                .strip_suffix("</codewhale:subagent.done>")
                .expect("closing envelope"),
        )
        .expect("terminal metadata")
    }

    #[tokio::test]
    async fn native_detached_workflow_wakes_once_and_foreground_does_not() {
        for (action, script, event) in [
            ("start", "return {ok: true};", "workflow.completed"),
            (
                "start",
                "throw new Error('failed native step');",
                "workflow.failed",
            ),
            ("run", "return {ok: true};", "workflow.completed"),
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let (tool, context, mut rx) = native_lifecycle_tool(tmp.path());
            let result = tool
                .execute(json!({"action": action, "script": script}), &context)
                .await
                .expect("run receipt");
            let result: Value = serde_json::from_str(&result.content).expect("run json");
            let run_id = result["run_id"].as_str().expect("run id");
            if action == "start" {
                let completion = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                    .await
                    .expect("detached completion")
                    .expect("parent inbox open");
                assert_eq!(completion.owner_session_id, context.state_namespace);
                assert_eq!(completion.agent_id, run_id);
                assert_eq!(
                    terminal_workflow_receipt(&completion.payload)["event"],
                    event
                );
                assert!(completion.payload.len() <= WORKFLOW_COMPLETION_MAX_BYTES);
                assert_eq!(live_workflow_count(tmp.path(), &context.state_namespace), 0);
                tool.execute(json!({"action":"status", "run_id":run_id}), &context)
                    .await
                    .expect("status remains addressable");
                tool.execute(json!({"action":"cancel", "run_id":run_id}), &context)
                    .await
                    .expect("terminal cancel is idempotent");
            } else {
                assert_eq!(result["status"], "completed");
            }
            assert!(rx.try_recv().is_err(), "no duplicate or foreground fan-in");
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn native_terminal_journal_failure_agrees_across_status_event_and_parent_receipt() {
        let _retry_guard = workflow_test_retry_guard();
        for cancelled in [false, true] {
            let tmp = tempfile::tempdir().expect("tempdir");
            let requested = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let calls = Arc::new(AtomicUsize::new(0));
            let app = Router::new().route(
                "/{*path}",
                post({
                    let requested = Arc::clone(&requested);
                    let release = Arc::clone(&release);
                    let calls = Arc::clone(&calls);
                    move |Json(body): Json<Value>| {
                        let requested = Arc::clone(&requested);
                        let release = Arc::clone(&release);
                        let calls = Arc::clone(&calls);
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            requested.notify_one();
                            release.notified().await;
                            fake_chat_response(
                                body["stream"].as_bool().unwrap_or(false),
                                "chatcmpl-journal-probe".to_string(),
                                "JOURNAL_CHILD_COMPLETED",
                            )
                        }
                    }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind journal probe");
            let address = listener.local_addr().expect("journal probe address");
            let server = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });
            let config = crate::config::Config {
                ..crate::config::Config::default()
            }
            .with_legacy_root(
                Some("test-key".to_string()),
                Some(format!("http://{address}/v1")),
            );
            let context = ToolContext::new(tmp.path().to_path_buf());
            let manager = new_shared_subagent_manager(tmp.path().to_path_buf(), 2);
            let (completion_tx, mut completion_rx) = mpsc::channel(16);
            let (event_tx, mut event_rx) = mpsc::channel(128);
            let runtime = SubAgentRuntime::new(
                CodewhaleClient::new(&config).expect("journal probe client"),
                "deepseek-v4-flash".to_string(),
                context.clone(),
                true,
                Some(event_tx),
                manager.clone(),
            )
            .with_api_config(config)
            .with_parent_completion_tx(completion_tx);
            let parent_cancel = runtime.cancel_token.clone();
            let tool = WorkflowTool::new(manager, runtime);
            let started = tool
                .execute(
                    json!({
                        "action": "start",
                        "plan": {
                            "goal": "Retain truthful terminal evidence when the journal fails",
                            "phases": [{
                                "id": "journal-phase",
                                "title": "Journal probe",
                                "children": [{
                                    "id": "journal-child",
                                    "prompt": "Return the read-only probe result.",
                                    "type": "explore",
                                    "mode": "read_only"
                                }]
                            }]
                        }
                    }),
                    &context,
                )
                .await
                .expect("launch succeeds while the journal is writable");
            let started: Value = serde_json::from_str(&started.content).expect("start json");
            let run_id = started["run_id"].as_str().expect("run id");
            tokio::time::timeout(std::time::Duration::from_secs(10), requested.notified())
                .await
                .expect("real workflow child reaches the loopback provider");
            assert_eq!(live_workflow_count(tmp.path(), &context.state_namespace), 1);
            let state = shared_workflow_state(tmp.path());
            let journal = state.journal_path();
            assert!(
                std::fs::read_to_string(journal)
                    .expect("launch persisted")
                    .contains(run_id)
            );
            // A directory at the append path fails even when tests run with
            // permissions that would bypass a read-only file's mode bits.
            std::fs::rename(journal, journal.with_extension("before-failure.jsonl"))
                .expect("preserve the successful launch journal");
            std::fs::create_dir(journal).expect("inject append failure after launch");
            if cancelled {
                parent_cancel.cancel();
            }
            release.notify_one();

            let completion =
                tokio::time::timeout(std::time::Duration::from_secs(10), completion_rx.recv())
                    .await
                    .expect("workflow settles despite the failed append")
                    .expect("parent receipt");
            server.abort();
            let expected_status = if cancelled { "cancelled" } else { "failed" };
            let receipt = terminal_workflow_receipt(&completion.payload);
            assert_eq!(receipt["status"], expected_status);
            assert_eq!(receipt["event"], "workflow.failed");
            assert!(completion.payload.contains("could not be persisted"));
            assert!(completion.payload.len() <= WORKFLOW_COMPLETION_MAX_BYTES);
            assert_eq!(live_workflow_count(tmp.path(), &context.state_namespace), 0);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(completion_rx.try_recv().is_err(), "only one parent receipt");

            // Tool rebuilds share the same authoritative run cache.
            let rebuilt = WorkflowTool::new(tool.manager.clone(), tool.runtime.clone());
            let status = rebuilt
                .execute(json!({"action":"status", "run_id":run_id}), &context)
                .await
                .expect("status remains addressable after the failed append");
            let status: Value = serde_json::from_str(&status.content).expect("status json");
            assert_eq!(status["status"], expected_status);
            assert_eq!(status["execution"]["status"], expected_status);
            assert!(
                status["error"]
                    .as_str()
                    .unwrap()
                    .contains("could not be persisted")
            );
            let expected_event = if cancelled {
                "run_cancelled"
            } else {
                "run_completed"
            };
            let mut final_event = None;
            while let Ok(event) = event_rx.try_recv() {
                if let Event::WorkflowUi { event, .. } = event
                    && matches!(
                        event["type"].as_str(),
                        Some("run_completed" | "run_cancelled")
                    )
                {
                    final_event = Some(event);
                }
            }
            let final_event = final_event.expect("terminal UI event");
            assert_eq!(final_event["type"], expected_event);
            assert!(final_event.to_string().contains("could not be persisted"));
            let recorded_terminal = status["events"]
                .as_array()
                .unwrap()
                .iter()
                .rev()
                .find(|event| {
                    matches!(
                        event["type"].as_str(),
                        Some("run_completed" | "run_cancelled")
                    )
                })
                .expect("recorded terminal event");
            assert_eq!(recorded_terminal["type"], expected_event);
            assert!(
                recorded_terminal
                    .to_string()
                    .contains("could not be persisted")
            );
            let report = std::fs::read_to_string(
                tmp.path()
                    .join(".codewhale/reports")
                    .join(format!("{run_id}.md")),
            )
            .expect("best-effort report remains writable");
            assert!(report.contains(if cancelled {
                "- status: Cancelled"
            } else {
                "- status: Failed"
            }));
            assert!(report.contains("could not be persisted"));
            if cancelled {
                assert!(
                    status["error"]
                        .as_str()
                        .unwrap()
                        .contains("cancelled by its parent")
                );
            } else {
                assert_eq!(status["result"]["journal-child"], "JOURNAL_CHILD_COMPLETED");
                assert!(completion.payload.contains("JOURNAL_CHILD_COMPLETED"));
                assert!(report.contains("JOURNAL_CHILD_COMPLETED"));
                assert_eq!(final_event["status"], "failed");
                assert_eq!(
                    status["execution"]["leaf_results"][0]["status"],
                    "succeeded"
                );
            }
            assert!(
                journal.is_dir(),
                "no terminal durability is claimed after the failed append"
            );
        }
    }

    #[tokio::test]
    async fn native_completion_is_enqueued_before_live_count_zero_and_bounded() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (tool, context, mut rx) = native_lifecycle_tool(tmp.path());
        let state = shared_workflow_state(tmp.path());
        let run_id = "workflow_bounded".to_string();
        let driver = SubAgentWorkflowDriver::new(
            run_id.clone(),
            context.state_namespace.clone(),
            tool.manager,
            tool.runtime,
            state.clone(),
            None,
            WorkflowFleetBinding::None,
            Vec::new(),
            tmp.path().to_path_buf(),
        );
        state.controllers.lock().expect("controllers").insert(
            run_id.clone(),
            Arc::new(
                WorkflowRunController::new(driver, WorkflowRunCancel::new())
                    .with_parent_completion(true),
            ),
        );
        let mut record = WorkflowRunRecord::new(
            run_id,
            Some(context.state_namespace.clone()),
            None,
            None,
            None,
        );
        record.status = WorkflowRunStatus::Failed;
        record.workflow_goal = Some("🦀\u{0001}".repeat(10_000));
        record.error = Some("🦀\u{0001}".repeat(10_000));
        record.result = Some(json!({"preview":"🦀\u{0001}".repeat(10_000)}));
        assert_eq!(live_workflow_count(tmp.path(), &context.state_namespace), 1);
        assert_eq!(live_workflow_count(tmp.path(), "other-session"), 0);
        finish_workflow_controller(&state, &record);
        assert_eq!(live_workflow_count(tmp.path(), &context.state_namespace), 0);
        let completion = rx
            .try_recv()
            .expect("receipt already enqueued when idle is observable");
        assert!(
            completion.payload.len() <= WORKFLOW_COMPLETION_MAX_BYTES,
            "{} bytes",
            completion.payload.len()
        );
        assert_eq!(
            terminal_workflow_receipt(&completion.payload)["status"],
            "failed"
        );
        finish_workflow_controller(&state, &record);
        assert!(rx.try_recv().is_err(), "terminalization is exactly once");
    }

    #[tokio::test]
    async fn native_parent_cancellation_between_children_settles_the_vm() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut tool, context, mut completion_rx) = native_lifecycle_tool(tmp.path());
        let parent_cancel = tool.runtime.cancel_token.clone();
        let (event_tx, mut event_rx) = mpsc::channel(32);
        tool.runtime.event_tx = Some(event_tx);
        tool.execute(
            json!({"action":"start", "script":"phase('between-children'); while (true) {}"}),
            &context,
        )
        .await
        .expect("detached start");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Event::WorkflowUi { event, .. } =
                    event_rx.recv().await.expect("event channel")
                    && event["type"] == "phase_started"
                {
                    break;
                }
            }
        })
        .await
        .expect("VM entered a phase with no child provider in flight");
        assert_eq!(live_workflow_count(tmp.path(), &context.state_namespace), 1);
        parent_cancel.cancel();
        let completion =
            tokio::time::timeout(std::time::Duration::from_secs(5), completion_rx.recv())
                .await
                .expect("cancel stops VM bytecode")
                .expect("cancel receipt");
        assert_eq!(
            terminal_workflow_receipt(&completion.payload)["status"],
            "cancelled"
        );
        assert_eq!(live_workflow_count(tmp.path(), &context.state_namespace), 0);
        assert_eq!(tool.manager.read().await.active_count(), 0);
    }

    #[tokio::test]
    async fn explicit_cancel_receipt_links_the_report_it_wrote() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (mut tool, context, mut completion_rx) = native_lifecycle_tool(tmp.path());
        let (event_tx, mut event_rx) = mpsc::channel(32);
        tool.runtime.event_tx = Some(event_tx);
        let started = tool
            .execute(
                json!({"action":"start", "script":"phase('spin'); while (true) {}"}),
                &context,
            )
            .await
            .expect("detached start");
        let started: Value = serde_json::from_str(&started.content).expect("start json");
        let run_id = started["run_id"].as_str().expect("run id").to_string();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Event::WorkflowUi { event, .. } =
                    event_rx.recv().await.expect("event channel")
                    && event["type"] == "phase_started"
                {
                    break;
                }
            }
        })
        .await
        .expect("VM entered its phase");
        tool.execute(json!({"action":"cancel", "run_id":run_id}), &context)
            .await
            .expect("cancel");
        let completion =
            tokio::time::timeout(std::time::Duration::from_secs(5), completion_rx.recv())
                .await
                .expect("cancel receipt arrives")
                .expect("parent inbox open");
        let receipt = terminal_workflow_receipt(&completion.payload);
        assert_eq!(receipt["status"], "cancelled");
        let report = format!(".codewhale/reports/{run_id}.md");
        assert_eq!(receipt["report"], report.as_str());
        assert!(
            completion.payload.contains(&format!("\nReport: {report}")),
            "{}",
            completion.payload
        );
        assert!(!completion.payload.contains("No report file was written"));
    }

    #[tokio::test]
    async fn native_cancel_closes_queued_admission_without_cancelling_parent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        set_session_workflow_config(
            tmp.path(),
            codewhale_config::WorkflowConfigToml {
                max_concurrent: 1,
                ..Default::default()
            },
        );
        let driver = native_lifecycle_driver(tmp.path(), Vec::new());
        assert_eq!(driver.concurrent_gate.available_permits(), 1);
        let held = driver
            .concurrent_gate
            .clone()
            .acquire_owned()
            .await
            .expect("hold capacity");
        let mut queued = Box::pin(driver.spawn_task(TaskRequest {
            label: Some("reviewer-slot".to_string()),
            ..exact_task_request("reviewer")
        }));
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(queued.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        // With cap 1 held, the waiting task is announced before any start.
        let announced: Vec<Value> = shared_workflow_state(tmp.path())
            .runs
            .lock()
            .expect("runs")
            .get("workflow_native_fixture")
            .expect("run")
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.event_type(),
                    "task_queued" | "task_started" | "task_dispatch_failed"
                )
            })
            .map(|event| serde_json::to_value(event).expect("event json"))
            .collect();
        assert_eq!(announced.len(), 1, "{announced:?}");
        assert_eq!(announced[0]["type"], "task_queued");
        assert_eq!(announced[0]["ticket"], 0);
        assert_eq!(announced[0]["label"], "reviewer-slot");
        driver.force_cancel_all();
        drop(held);
        let error = match queued.await {
            Ok(_) => panic!("cancelled admission spawned"),
            Err(err) => err,
        };
        assert!(error.to_string().contains("admission closed"), "{error}");
        assert_eq!(driver.child_counter.load(Ordering::SeqCst), 0);
        assert!(driver.child_ids.lock().expect("children").is_empty());
        assert!(
            !driver.parent_cancel_token.is_cancelled(),
            "workflow stop is scoped to its subtree"
        );
    }

    #[tokio::test]
    async fn native_handoff_survives_rejected_spawn_and_pending_gates_block_completion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let gate: GateSpec = serde_json::from_value(json!({
            "id":"findings", "role":"scout", "on":"role_complete", "gate":"approve", "on_fail":"block",
            "blocks_role":"reviewer", "artifact_kind":"findings"
        })).expect("gate fixture");
        let driver = native_lifecycle_driver(tmp.path(), vec![gate]);
        assert!(
            driver.terminal_gate_failure().is_some(),
            "unevaluated prerequisite blocks terminal success"
        );
        driver.gate_board.lock().expect("board").gates.clear();
        assert!(
            driver.terminal_gate_failure().is_some(),
            "missing persisted gate also blocks success"
        );
        driver.evaluate_gates_for_completed_role(&RuntimeTaskRecord {
            agent_id: "scout-1".to_string(),
            label: None,
            role: Some("scout".to_string()),
            status: IrWorkflowRunStatus::Succeeded,
            output: Some("evidence to retain".to_string()),
            schema_error: None,
            usage: None,
        });
        assert!(
            driver.terminal_gate_failure().is_none(),
            "passed prerequisite releases the run"
        );
        let mut rejected = exact_task_request("reviewer");
        rejected.profile = Some("native-fixture-missing-profile".to_string());
        let error = match driver.spawn_task(rejected).await {
            Ok(_) => panic!("missing profile spawned"),
            Err(err) => err,
        };
        assert!(
            error.to_string().contains("native-fixture-missing-profile"),
            "{error}"
        );
        assert_eq!(driver.gate_board.lock().expect("board").artifacts.len(), 1);
        let mut retry = exact_task_request("reviewer");
        let handoffs = driver
            .prepare_request_for_gates(&mut retry)
            .expect("retry retains evidence");
        assert_eq!(handoffs.len(), 1);
        assert!(retry.description.contains("evidence to retain"));
        assert!(
            driver.state.runs.lock().expect("runs")[&driver.run_id]
                .events
                .iter()
                .all(|event| event.event_type() != "handoff_consumed")
        );
    }

    #[tokio::test]
    async fn native_configured_shape_and_admission_limits_are_enforced() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (tool, context, _rx) = native_lifecycle_tool(tmp.path());
        set_session_workflow_config(
            tmp.path(),
            codewhale_config::WorkflowConfigToml {
                max_children: 2,
                max_depth: 1,
                max_concurrent: 1,
                ..Default::default()
            },
        );
        for children in [
            json!([{"prompt":"one"},{"prompt":"two"},{"prompt":"three"}]),
            json!([{"prompt":"one"},{"prompt":"two"}]),
        ] {
            let error = tool
                .execute(
                    json!({"action":"start", "plan":{"goal":"bounded", "children":children}}),
                    &context,
                )
                .await
                .expect_err("configured total or structural depth rejects before launch");
            assert!(error.to_string().contains("workflow.max_"), "{error}");
        }
        assert!(
            shared_workflow_state(tmp.path())
                .runs
                .lock()
                .expect("runs")
                .is_empty()
        );
        let initial_spawn_depth = tool.runtime.spawn_depth;
        let accepted = tool.execute(json!({"action":"run", "plan":{
            "goal":"two sequential leaves fit structural depth one", "phases":[
                {"id":"first", "children":[{"prompt":"one", "profile":"missing-boundary-profile"}]},
                {"id":"second", "children":[{"prompt":"two", "profile":"missing-boundary-profile"}]}
            ]
        }}), &context).await.expect("sequence wrappers do not increase structural depth");
        let accepted: Value = serde_json::from_str(&accepted.content).expect("accepted run");
        assert_eq!(
            accepted["status"], "failed",
            "admitted plan, then deliberate missing-profile rejection"
        );
        assert_eq!(
            tool.runtime.spawn_depth, initial_spawn_depth,
            "plan shape never rewrites delegation depth"
        );
        set_session_workflow_config(
            tmp.path(),
            codewhale_config::WorkflowConfigToml {
                max_children: 1,
                max_concurrent: 1,
                ..Default::default()
            },
        );
        let driver = native_lifecycle_driver(tmp.path(), Vec::new());
        let mut invalid = exact_task_request("reviewer");
        invalid.profile = Some("missing-first-attempt-profile".to_string());
        let first = driver.spawn_task(invalid.clone()).await;
        assert!(first.is_err());
        let second = match driver.spawn_task(invalid).await {
            Ok(_) => panic!("cap bypassed"),
            Err(err) => err,
        };
        assert!(
            second
                .to_string()
                .contains("workflow.max_children limit (1)"),
            "{second}"
        );
        // Both attempts found the one slot free, so neither was announced as
        // waiting for it.
        assert!(
            driver.state.runs.lock().expect("runs")[&driver.run_id]
                .events
                .iter()
                .all(|event| event.event_type() != "task_queued")
        );
        assert_eq!(driver.child_counter.load(Ordering::SeqCst), 1);
        assert!(driver.child_ids.lock().expect("children").is_empty());
        for name in ["max_children", "max_concurrent", "max_depth"] {
            let mut config = serde_json::to_value(codewhale_config::WorkflowConfigToml::default())
                .expect("config");
            config[name] = json!(0);
            set_session_workflow_config(
                tmp.path(),
                serde_json::from_value(config).expect("zero config"),
            );
            let error = tool
                .execute(json!({"action":"start", "script":"return 1;"}), &context)
                .await
                .expect_err("zero ceiling fails without waiting on a semaphore");
            assert!(
                error.to_string().contains(&format!("workflow.{name}")),
                "{error}"
            );
        }
    }

    #[test]
    fn native_gate_identity_is_normalized_and_unsupported_or_duplicate_gates_reject() {
        let gate = json!({"id":" check ","role":" Scout ","on":"role_complete", "gate":"approve", "on_fail":"block", "blocks_role":" Reviewer "});
        let mut gates: Vec<GateSpec> = serde_json::from_value(json!([gate])).expect("gates");
        validate_runtime_gates(&mut gates).expect("normalize gate");
        assert_eq!(gates[0].id, "check");
        assert_eq!(gates[0].role, "scout");
        assert_eq!(gates[0].blocks_role.as_deref(), Some("reviewer"));
        let mut duplicate = vec![gates[0].clone(), gates[0].clone()];
        assert!(validate_runtime_gates(&mut duplicate).is_err());
        for (field, value) in [
            ("id", " "),
            ("role", " "),
            ("blocks_role", " "),
            ("on", "role_start"),
        ] {
            let mut candidate = serde_json::to_value(&gates[0]).expect("gate json");
            candidate[field] = json!(value);
            let mut candidate =
                [serde_json::from_value(candidate).expect("syntactically valid gate")];
            assert!(validate_runtime_gates(&mut candidate).is_err(), "{field}");
        }
    }

    #[test]
    fn native_gate_that_blocks_its_own_role_is_rejected_at_admission() {
        let mut gates: Vec<GateSpec> = serde_json::from_value(json!([{
            "id": "v1", "gate": "verify", "on": "role_complete", "role": "implement",
            "blocks_role": "implement", "on_fail": "escalate", "max_retries": 1,
            "require_explicit_verdict": true
        }]))
        .expect("gates");
        let error = validate_runtime_gates(&mut gates).expect_err("gate blocks its own role");
        let message = error.to_string();
        for needle in ["`v1`", "blocks_role", "`implement`"] {
            assert!(message.contains(needle), "{message}");
        }
        gates[0].blocks_role = Some(" Implement ".to_string());
        assert!(validate_runtime_gates(&mut gates).is_err());
        gates[0].blocks_role = Some("verify".to_string());
        validate_runtime_gates(&mut gates).expect("blocking a later role stays valid");
    }

    #[tokio::test]
    async fn native_default_shared_budget_applies_only_without_an_explicit_budget() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (tool, context, _rx) = native_lifecycle_tool(tmp.path());
        set_session_workflow_config(
            tmp.path(),
            codewhale_config::WorkflowConfigToml {
                default_token_budget: 123,
                ..Default::default()
            },
        );
        for (explicit, expected) in [(None, 123), (Some(17), 17), (Some(246), 246)] {
            let mut input = json!({"action":"run", "script":"return budget.remaining();"});
            if let Some(explicit) = explicit {
                input["token_budget"] = json!(explicit);
            }
            let receipt = tool.execute(input, &context).await.expect("budget run");
            let receipt: Value = serde_json::from_str(&receipt.content).expect("budget receipt");
            assert_eq!(receipt["token_budget"], expected);
            assert_eq!(
                receipt["result"], expected,
                "the same effective cap reaches the VM"
            );
        }
    }

    #[tokio::test]
    async fn native_cancel_preserves_completed_outputs_and_late_completion_cannot_reverse_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let driver = native_lifecycle_driver(tmp.path(), Vec::new());
        for id in ["completed-child", "running-child"] {
            driver.record_child(id);
            driver.task_records.lock().expect("records").insert(
                id.to_string(),
                RuntimeTaskRecord {
                    agent_id: id.to_string(),
                    label: None,
                    role: None,
                    status: IrWorkflowRunStatus::Running,
                    output: None,
                    schema_error: None,
                    usage: None,
                },
            );
        }
        driver.record_task_completion(
            "completed-child",
            &TaskCompletion::Completed {
                text: "completed evidence must survive".to_string(),
            },
            None,
        );
        driver.finalize_running_tasks_cancelled();
        driver.record_task_completion(
            "running-child",
            &TaskCompletion::Completed {
                text: "late completion after cancellation".to_string(),
            },
            None,
        );
        let records = driver.task_records.lock().expect("records");
        assert_eq!(
            records["completed-child"].status,
            IrWorkflowRunStatus::Succeeded
        );
        assert_eq!(
            records["completed-child"].output.as_deref(),
            Some("completed evidence must survive")
        );
        assert_eq!(
            records["running-child"].status,
            IrWorkflowRunStatus::Cancelled
        );
        assert!(records["running-child"].output.is_none());
    }

    #[tokio::test]
    async fn native_completion_before_registration_releases_capacity_and_cancelled_waiters() {
        let tmp = tempfile::tempdir().expect("tempdir");
        set_session_workflow_config(
            tmp.path(),
            codewhale_config::WorkflowConfigToml {
                max_concurrent: 1,
                ..Default::default()
            },
        );
        let driver = native_lifecycle_driver(tmp.path(), Vec::new());
        let held = driver
            .concurrent_gate
            .clone()
            .acquire_owned()
            .await
            .expect("capacity");
        driver.deliver_completion(
            "instant".to_string(),
            TaskCompletion::Completed {
                text: "ready".to_string(),
            },
            None,
        );
        driver
            .spawn_permits
            .lock()
            .expect("permits")
            .insert("instant".to_string(), held);
        driver.record_task_request("instant", &exact_task_request("reviewer"));
        let (tx, rx) = oneshot::channel();
        driver.add_waiter_or_complete("instant".to_string(), tx);
        assert!(
            matches!(rx.await.expect("early completion delivered"), TaskCompletion::Completed {text} if text == "ready")
        );
        assert_eq!(
            driver.concurrent_gate.available_permits(),
            1,
            "instant completion must not leak the sole permit"
        );
        // Reproduce the other interleaving: the completion checked records
        // before registration, but published pending after its first peek.
        driver.record_task_request("pending-after-peek", &exact_task_request("reviewer"));
        driver
            .completion_state
            .lock()
            .expect("completion state")
            .pending
            .insert(
                "pending-after-peek".to_string(),
                PendingCompletion {
                    completion: TaskCompletion::Completed {
                        text: "preserved receipt".to_string(),
                    },
                    usage: None,
                },
            );
        let (tx, rx) = oneshot::channel();
        driver.add_waiter_or_complete("pending-after-peek".to_string(), tx);
        assert!(matches!(
            rx.await.expect("completion"),
            TaskCompletion::Completed { .. }
        ));
        assert_eq!(
            driver.task_records.lock().expect("records")["pending-after-peek"]
                .output
                .as_deref(),
            Some("preserved receipt")
        );
        driver.force_cancel_all();
        driver.record_task_request("late-registration", &exact_task_request("reviewer"));
        let (tx, rx) = oneshot::channel();
        driver.add_waiter_or_complete("late-registration".to_string(), tx);
        assert!(matches!(
            rx.await.expect("late waiter released"),
            TaskCompletion::Cancelled
        ));
        assert_eq!(
            driver.task_records.lock().expect("records")["late-registration"].status,
            IrWorkflowRunStatus::Cancelled
        );
    }

    fn fake_chat_response(stream: bool, id: String, text: &str) -> axum::response::Response {
        use axum::response::IntoResponse;
        let usage = json!({"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2});
        if stream {
            let content = json!({
                "id": id,
                "model": "deepseek-v4-flash",
                "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": null}],
            });
            let terminal = json!({
                "id": id,
                "model": "deepseek-v4-flash",
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": usage,
            });
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                format!("data: {content}\n\ndata: {terminal}\n\ndata: [DONE]\n\n"),
            )
                .into_response()
        } else {
            Json(json!({
                "id": id,
                "model": "deepseek-v4-flash",
                "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
                "usage": usage,
            })).into_response()
        }
    }

    fn stub_client() -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = crate::config::Config {
            ..crate::config::Config::default()
        }
        .with_legacy_root(Some("test-key".to_string()), None);
        CodewhaleClient::new(&config).expect("stub client should construct")
    }

    async fn fake_chat_client(
        response_text: &str,
    ) -> (CodewhaleClient, Arc<AtomicUsize>, crate::config::Config) {
        let (client, calls, _, config) = fake_chat_client_capturing(response_text).await;
        (client, calls, config)
    }

    async fn fake_chat_client_responses(
        response_texts: &[&str],
    ) -> (CodewhaleClient, Arc<AtomicUsize>, crate::config::Config) {
        let (client, calls, _, config) = fake_chat_client_capturing_responses(response_texts).await;
        (client, calls, config)
    }

    pub(super) async fn fake_chat_client_capturing(
        response_text: &str,
    ) -> (
        CodewhaleClient,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<Value>>>,
        crate::config::Config,
    ) {
        fake_chat_client_capturing_responses(&[response_text]).await
    }

    async fn fake_chat_client_capturing_responses(
        response_texts: &[&str],
    ) -> (
        CodewhaleClient,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<Value>>>,
        crate::config::Config,
    ) {
        assert!(
            !response_texts.is_empty(),
            "fake chat client needs at least one response"
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let response_texts = Arc::new(
            response_texts
                .iter()
                .map(|response| (*response).to_string())
                .collect::<Vec<_>>(),
        );
        let app = Router::new().route(
            "/{*path}",
            post({
                let calls = Arc::clone(&calls);
                let bodies = Arc::clone(&bodies);
                let response_texts = Arc::clone(&response_texts);
                move |Json(body): Json<Value>| {
                    let calls = Arc::clone(&calls);
                    let bodies = Arc::clone(&bodies);
                    let response_texts = Arc::clone(&response_texts);
                    async move {
                        let stream = body["stream"].as_bool().unwrap_or(false);
                        bodies.lock().expect("capture body").push(body);
                        let attempt = calls.fetch_add(1, Ordering::SeqCst) + 1;
                        let response_text = if response_texts.len() == 1 {
                            response_texts[0].clone()
                        } else {
                            response_texts
                                .get(attempt - 1)
                                .unwrap_or_else(|| {
                                    panic!(
                                        "fake chat server received call {attempt} but only {} responses were supplied",
                                        response_texts.len()
                                    )
                                })
                                .clone()
                        };
                        fake_chat_response(
                            stream,
                            format!("chatcmpl-workflow-test-{attempt}"),
                            &response_text,
                        )
                    }
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake chat server");
        let addr = listener.local_addr().expect("fake chat server addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let config = crate::config::Config {
            ..crate::config::Config::default()
        }
        .with_legacy_root(
            Some("test-key".to_string()),
            Some(format!("http://{addr}/v1")),
        );
        (
            CodewhaleClient::new(&config).expect("fake chat client"),
            calls,
            bodies,
            config,
        )
    }

    pub(super) fn workflow_test_retry_guard() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::retry_status::test_guard();
        crate::retry_status::clear();
        crate::retry_status::clear_rate_limit();
        guard
    }
}

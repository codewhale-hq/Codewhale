//! Persistent background task manager for Codewhale agent work.
//!
//! Tasks are durable across restarts and execute with a bounded worker pool.
//! Execution uses the shared runtime provider route and links every task to
//! runtime thread/turn records for unified timelines.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use std::time::Duration as StdDuration;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, mpsc, oneshot};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::runtime_threads::{
    RUNTIME_STORE_FAILURE_EVENT, RuntimeEventRecord, RuntimeProcessOwnerLock, RuntimeTurnStatus,
    manager::CreateThreadRequest, manager::RuntimeThreadManager,
    manager::RuntimeThreadManagerConfig, manager::SharedRuntimeThreadManager,
    manager::StartTurnRequest,
};
use codewhale_runtime::config::Config;
use codewhale_util::utils::spawn_supervised;

const DEFAULT_WORKERS: usize = 2;
const MAX_WORKERS: usize = 8;
const TIMELINE_SUMMARY_LIMIT: usize = 240;
const TIMELINE_ENTRY_LIMIT: usize = 256;
const TIMELINE_HEAD_KEEP: usize = 8;
const ARTIFACT_THRESHOLD: usize = 1200;
const TASK_EVENT_CHANNEL_CAPACITY: usize = 256;
const EVENT_CURSOR_BATCH: usize = 256;
const DESTINATION_BUSY_RETRY: Duration = Duration::from_secs(2);
const EVENT_CATCHUP_POLL: Duration = Duration::from_millis(200);
// v4 binds execution to a trusted Runtime scope. Older executors must not
// ignore its eligibility or generation fence.
const CURRENT_TASK_SCHEMA_VERSION: u32 = 4;
const STORE_REFRESH_INTERVAL: Duration = Duration::from_millis(200);
/// How long a worker with nothing claimable naps between queue-fingerprint
/// checks. An in-process notify still wakes it at once; this only bounds how
/// soon it notices another process's queue write, so an idle session stats the
/// queue about once a second per worker instead of five times (#6728, #6573).
const STORE_SETTLED_NAP: Duration = Duration::from_secs(1);
/// Retry an empty claim while queue metadata is missing or too recent to
/// distinguish writes on a coarse-mtime filesystem. Once settled, idle workers
/// wait for admission notifications or queue changes instead of reloading every
/// task record on a timer (#6728).
const STORE_IDLE_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Ceiling for the retry delay after a claim fails (e.g. "Task store is busy").
/// A failed claim has already waited out `lock_store`'s 5s deadline, so work
/// written by another process can wait up to about 13s after contention; an
/// in-process submission or a queue change ends the backoff early.
const STORE_BUSY_BACKOFF_MAX: Duration = Duration::from_secs(8);
/// A read snapshot is only trusted once both stamps are at least this old, so
/// a write landing in the same coarse mtime tick as the load (1s or 2s on
/// some filesystems) cannot hide behind an unchanged stamp (#6573).
const READ_SNAPSHOT_SETTLE: Duration = Duration::from_secs(2);

/// Only lock contention at the first worker claim may acknowledge a scheduled
/// retry. Corrupt state and other lock/I/O errors still fail initialization.
#[derive(Debug)]
struct TaskStoreBusy;

impl std::fmt::Display for TaskStoreBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Task store is busy; state is unavailable")
    }
}

impl std::error::Error for TaskStoreBusy {}

const fn default_task_schema_version() -> u32 {
    CURRENT_TASK_SCHEMA_VERSION
}

/// Durable task status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Canceled,
}

/// What the manager actually did while handling a cancellation request.
///
/// This is returned from the same state-lock transaction as the task record,
/// so callers never have to infer an outcome from a stale pre-cancel read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCancelDisposition {
    Forced,
    Requested,
    AlreadyFinished,
}

#[derive(Debug, Clone)]
pub struct TaskCancellation {
    pub task: TaskRecord,
    pub disposition: TaskCancelDisposition,
}

impl TaskStatus {
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

/// Why a durable task left the running state. Stored on the task record so
/// receipts and status views can show a forced timeout separately from a
/// cooperative cancel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTerminalReason {
    Completed,
    Canceled,
    CancelTimeout,
    Shutdown,
    WallTimeout,
    IdleTimeout,
    Failed,
}

impl TaskTerminalReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::CancelTimeout => "cancel_timeout",
            Self::Shutdown => "shutdown",
            Self::WallTimeout => "wall_timeout",
            Self::IdleTimeout => "idle_timeout",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub const fn task_status(self) -> TaskStatus {
        match self {
            Self::Completed => TaskStatus::Completed,
            Self::Canceled | Self::CancelTimeout | Self::Shutdown => TaskStatus::Canceled,
            Self::WallTimeout | Self::IdleTimeout | Self::Failed => TaskStatus::Failed,
        }
    }

    #[must_use]
    pub fn receipt_message(self) -> String {
        match self {
            Self::Completed => "Task completed".to_string(),
            Self::Canceled => "Task canceled".to_string(),
            Self::CancelTimeout => {
                "Task did not terminalize after cancellation; worker released".to_string()
            }
            Self::Shutdown => "Task canceled because the task manager shut down".to_string(),
            Self::WallTimeout => {
                "Task exceeded its wall-time deadline without completing".to_string()
            }
            Self::IdleTimeout => {
                "Task made no model or tool progress before the idle deadline".to_string()
            }
            Self::Failed => "Task ended unexpectedly".to_string(),
        }
    }

    fn after_grace(self) -> Self {
        match self {
            Self::Canceled => Self::CancelTimeout,
            other => other,
        }
    }
}

/// Durable tool-call status within a task timeline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskToolStatus {
    Running,
    Success,
    Failed,
    Canceled,
}

/// Timeline entry for a task execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskTimelineEntry {
    pub timestamp: DateTime<Utc>,
    pub kind: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_path: Option<PathBuf>,
}

/// Tool call summary for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskToolCallSummary {
    pub id: String,
    pub name: String,
    pub status: TaskToolStatus,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_ref: Option<PathBuf>,
}

/// Checklist item stored on durable tasks. This is the durable form behind the
/// model-visible checklist/todo compatibility tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskChecklistItem {
    pub id: u32,
    pub content: String,
    pub status: String,
}

/// Checklist state associated with a task.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskChecklistState {
    pub items: Vec<TaskChecklistItem>,
    pub completion_pct: u8,
    pub in_progress_id: Option<u32>,
    pub updated_at: Option<DateTime<Utc>>,
}

/// Structured verification evidence attached to a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGateRecord {
    pub id: String,
    pub gate: String,
    pub command: String,
    pub cwd: PathBuf,
    pub exit_code: Option<i32>,
    pub status: String,
    pub classification: String,
    pub duration_ms: u64,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<PathBuf>,
    pub recorded_at: DateTime<Utc>,
}

/// PR-attempt metadata and artifacts attached to a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskAttemptRecord {
    pub id: String,
    pub attempt_group_id: String,
    pub attempt_index: u32,
    pub attempt_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub summary: String,
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_path: Option<PathBuf>,
    pub verification: Vec<String>,
    pub selected: bool,
    pub recorded_at: DateTime<Utc>,
}

/// Durable artifact reference produced by task-aware tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskArtifactRef {
    pub label: String,
    pub path: PathBuf,
    pub summary: String,
    pub created_at: DateTime<Utc>,
}

/// GitHub write/read evidence attached to a task timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGithubEvent {
    pub id: String,
    pub action: String,
    pub target: String,
    pub number: u64,
    pub summary: String,
    pub url: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// Durable task record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    #[serde(default = "default_task_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    pub workspace: PathBuf,
    pub mode: String,
    pub allow_shell: bool,
    pub trust_mode: bool,
    /// A record without the field (written before it existed) is not
    /// auto-approved: a missing grant never widens authority.
    #[serde(default)]
    pub auto_approve: bool,
    /// Permission posture the task's own thread starts on (`ask`,
    /// `auto_review`, `full_access`). Absent on records written before the
    /// field existed and on the in-process callers that still express authority
    /// through `auto_approve` alone; the thread request then derives the
    /// posture from that bit exactly as it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_posture: Option<String>,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_detail_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_thread_id: Option<String>,
    /// Trusted execution provenance, distinct from model-visible ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_generation: Option<String>,
    /// Durable cancellation acknowledged by the actual execution owner.
    #[serde(default)]
    pub cancel_requested_seq: u64,
    #[serde(default)]
    pub runtime_event_count: usize,
    /// Monotonic owner-lifecycle sequence used by Work Graph reconciliation.
    /// Output/progress events do not advance this counter; only lifecycle
    /// transitions do, so replay after restart is stable.
    #[serde(default)]
    pub lifecycle_seq: u64,
    #[serde(default)]
    pub checklist: TaskChecklistState,
    #[serde(default)]
    pub gates: Vec<TaskGateRecord>,
    #[serde(default)]
    pub attempts: Vec<TaskAttemptRecord>,
    #[serde(default)]
    pub artifacts: Vec<TaskArtifactRef>,
    #[serde(default)]
    pub github_events: Vec<TaskGithubEvent>,
    pub tool_calls: Vec<TaskToolCallSummary>,
    pub timeline: Vec<TaskTimelineEntry>,
}

/// Lightweight task view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: String,
    pub status: TaskStatus,
    pub prompt_summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    pub mode: String,
    pub workspace: PathBuf,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub lifecycle_seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    pub execution_binding_known: bool,
}

impl From<&TaskRecord> for TaskSummary {
    fn from(value: &TaskRecord) -> Self {
        Self {
            id: value.id.clone(),
            status: value.status,
            prompt_summary: summarize_text(&value.prompt, TIMELINE_SUMMARY_LIMIT),
            name: value.name.clone(),
            model: value.model.clone(),
            model_provider: value.model_provider.clone(),
            model_provider_id: value.model_provider_id.clone(),
            mode: value.mode.clone(),
            workspace: value.workspace.clone(),
            created_at: value.created_at,
            started_at: value.started_at,
            ended_at: value.ended_at,
            duration_ms: value.duration_ms,
            lifecycle_seq: value.lifecycle_seq,
            error: value.error.clone(),
            terminal_reason: value.terminal_reason.clone(),
            thread_id: value.thread_id.clone(),
            turn_id: value.turn_id.clone(),
            owner_session_id: value.owner_session_id.clone(),
            execution_binding_known: value.execution_scope.is_some()
                && (value.status != TaskStatus::Running || value.execution_generation.is_some()),
        }
    }
}

/// Count totals by status for task dashboards.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct TaskCounts {
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
    pub canceled: usize,
}

/// Request to enqueue a new task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewTaskRequest {
    pub prompt: String,
    /// Caller-given run name, stored as-given. Absent names stay absent —
    /// titles derived from the prompt are a presentation concern.
    #[serde(default)]
    pub name: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
    #[serde(default)]
    pub model_provider_id: Option<String>,
    pub workspace: Option<PathBuf>,
    pub mode: Option<String>,
    pub allow_shell: Option<bool>,
    pub trust_mode: Option<bool>,
    pub auto_approve: Option<bool>,
    /// Posture for the thread this task runs on. Takes precedence over
    /// `auto_approve`, which the runtime only reads when no posture is given.
    pub permission_posture: Option<String>,
    pub owner_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_thread_id: Option<String>,
}

impl NewTaskRequest {
    /// Preserve values already resolved into a staged or accepted task.
    pub fn from_task(task: &TaskRecord) -> Self {
        Self {
            prompt: task.prompt.clone(),
            name: task.name.clone(),
            model: Some(task.model.clone()),
            model_provider: task.model_provider.clone(),
            model_provider_id: task.model_provider_id.clone(),
            workspace: Some(task.workspace.clone()),
            mode: Some(task.mode.clone()),
            allow_shell: Some(task.allow_shell),
            trust_mode: Some(task.trust_mode),
            auto_approve: Some(task.auto_approve),
            permission_posture: task.permission_posture.clone(),
            owner_session_id: task.owner_session_id.clone(),
            destination_thread_id: task.destination_thread_id.clone(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn from_prompt(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            name: None,
            model: None,
            model_provider: None,
            model_provider_id: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: Some(true),
            permission_posture: None,
            owner_session_id: None,
            destination_thread_id: None,
        }
    }
}

/// Task manager startup options.
#[derive(Debug, Clone)]
pub struct TaskManagerConfig {
    pub data_dir: PathBuf,
    pub worker_count: usize,
    pub default_workspace: PathBuf,
    pub default_model: String,
    pub default_mode: String,
    pub allow_shell: bool,
    pub trust_mode: bool,
    pub execution_limits: TaskExecutionLimits,
}

/// Deadlines and persistence cadence for one durable execution.
#[derive(Debug, Clone, Copy)]
pub struct TaskExecutionLimits {
    pub wall_time: Duration,
    pub idle_progress: Duration,
    pub cancel_grace: Duration,
    pub persist_debounce: Duration,
}

impl Default for TaskExecutionLimits {
    fn default() -> Self {
        Self {
            wall_time: Duration::from_secs(30 * 60),
            idle_progress: Duration::from_secs(2 * 60),
            cancel_grace: Duration::from_secs(5),
            persist_debounce: Duration::from_millis(250),
        }
    }
}

#[cfg(test)]
impl TaskExecutionLimits {
    fn short_for_tests() -> Self {
        Self {
            wall_time: Duration::from_millis(400),
            idle_progress: Duration::from_millis(150),
            cancel_grace: Duration::from_millis(50),
            persist_debounce: Duration::from_millis(10),
        }
    }
}

/// Pure watchdog for cancel grace, wall time, and idle progress.
struct ExecutionGuard {
    started_at: Instant,
    last_progress_at: Instant,
    interrupt_at: Option<Instant>,
    interrupt_reason: Option<TaskTerminalReason>,
    limits: TaskExecutionLimits,
}

#[derive(Debug)]
enum GuardAction {
    Run { wait: Duration },
    Interrupt { reason: TaskTerminalReason },
    Terminalize { reason: TaskTerminalReason },
}

impl ExecutionGuard {
    fn new(limits: TaskExecutionLimits, now: Instant) -> Self {
        Self {
            started_at: now,
            last_progress_at: now,
            interrupt_at: None,
            interrupt_reason: None,
            limits,
        }
    }

    fn note_progress(&mut self, now: Instant) {
        self.last_progress_at = now;
    }

    fn note_interrupt(&mut self, now: Instant, reason: TaskTerminalReason) {
        if self.interrupt_at.is_none() {
            self.interrupt_at = Some(now);
            self.interrupt_reason = Some(reason);
        }
    }

    fn evaluate(&self, now: Instant, cancel: bool, shutdown: bool) -> GuardAction {
        if let Some(interrupt_at) = self.interrupt_at {
            let elapsed = now.saturating_duration_since(interrupt_at);
            if elapsed >= self.limits.cancel_grace {
                let reason = self
                    .interrupt_reason
                    .unwrap_or(TaskTerminalReason::CancelTimeout)
                    .after_grace();
                return GuardAction::Terminalize { reason };
            }
            return GuardAction::Run {
                wait: self.limits.cancel_grace.saturating_sub(elapsed),
            };
        }

        let wall_elapsed = now.saturating_duration_since(self.started_at);
        let idle_elapsed = now.saturating_duration_since(self.last_progress_at);
        // A limit whose deadline does not fit in `Instant` can never fire.
        let wall_deadline = self.started_at.checked_add(self.limits.wall_time);
        let idle_deadline = self.last_progress_at.checked_add(self.limits.idle_progress);
        let pending = if shutdown {
            Some(TaskTerminalReason::Shutdown)
        } else if cancel {
            Some(TaskTerminalReason::Canceled)
        } else {
            // Attribute the timeout to the limit that was crossed first, not
            // to the one this tick happens to check first. When the watchdog
            // is starved past both deadlines (a >=250 ms scheduler stall on a
            // loaded CI runner is enough with the test budgets), the idle
            // limit that expired earlier is still the truthful reason; a tie
            // keeps the wall limit's precedence (issue #5898).
            match (wall_deadline, idle_deadline) {
                (Some(wall), Some(idle)) if now >= wall && wall <= idle => {
                    Some(TaskTerminalReason::WallTimeout)
                }
                (_, Some(idle)) if now >= idle => Some(TaskTerminalReason::IdleTimeout),
                (Some(wall), _) if now >= wall => Some(TaskTerminalReason::WallTimeout),
                _ => None,
            }
        };
        if let Some(reason) = pending {
            return GuardAction::Interrupt { reason };
        }

        let wait = self
            .limits
            .wall_time
            .saturating_sub(wall_elapsed)
            .min(self.limits.idle_progress.saturating_sub(idle_elapsed))
            .min(EVENT_CATCHUP_POLL);
        GuardAction::Run {
            wait: wait.max(Duration::from_millis(1)),
        }
    }

    fn preserve_timeout_reason(&self, result: TaskExecutionResult) -> TaskExecutionResult {
        if result.terminal_reason != TaskTerminalReason::Canceled {
            return result;
        }
        match self.interrupt_reason {
            Some(reason @ (TaskTerminalReason::WallTimeout | TaskTerminalReason::IdleTimeout)) => {
                TaskExecutionResult::from_reason(reason, result.result_text)
            }
            _ => result,
        }
    }
}

impl TaskManagerConfig {
    #[must_use]
    pub fn from_runtime(
        config: &Config,
        workspace: PathBuf,
        default_model: Option<String>,
        worker_count: Option<usize>,
    ) -> Self {
        Self {
            data_dir: default_tasks_dir(),
            worker_count: worker_count.unwrap_or(DEFAULT_WORKERS),
            default_workspace: workspace,
            default_model: default_model.unwrap_or_else(|| config.default_model()),
            default_mode: "agent".to_string(),
            allow_shell: config.allow_shell(),
            trust_mode: false,
            execution_limits: TaskExecutionLimits::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionTask {
    id: String,
    prompt: String,
    model: String,
    model_provider: Option<String>,
    model_provider_id: Option<String>,
    workspace: PathBuf,
    mode_label: String,
    allow_shell: bool,
    trust_mode: bool,
    auto_approve: bool,
    permission_posture: Option<String>,
    destination_thread_id: Option<String>,
}

impl From<&TaskRecord> for ExecutionTask {
    fn from(task: &TaskRecord) -> Self {
        Self {
            id: task.id.clone(),
            prompt: task.prompt.clone(),
            model: task.model.clone(),
            model_provider: task.model_provider.clone(),
            model_provider_id: task.model_provider_id.clone(),
            workspace: task.workspace.clone(),
            mode_label: task.mode.clone(),
            allow_shell: task.allow_shell,
            trust_mode: task.trust_mode,
            auto_approve: task.auto_approve,
            permission_posture: task.permission_posture.clone(),
            destination_thread_id: task.destination_thread_id.clone(),
        }
    }
}

impl ExecutionTask {
    pub fn thread_request(&self) -> CreateThreadRequest {
        CreateThreadRequest {
            model: Some(self.model.clone()),
            model_provider: self.model_provider.clone(),
            model_provider_id: self.model_provider_id.clone(),
            workspace: Some(self.workspace.clone()),
            mode: Some(self.mode_label.clone()),
            allow_shell: Some(self.allow_shell),
            trust_mode: Some(self.trust_mode),
            auto_approve: Some(self.auto_approve),
            permission_posture: self.permission_posture.clone(),
            task_id: Some(self.id.clone()),
            ..Default::default()
        }
    }

    /// The turn request for this task. A task that carries a pinned posture
    /// runs its turn under that posture: the legacy `auto_approve` bit is only
    /// sent for records that predate the pinned posture, because a per-turn
    /// `auto_approve` without a posture re-derives the permission from the
    /// bit alone and would override what the thread was created with.
    pub fn turn_request(&self) -> StartTurnRequest {
        let pinned = self.permission_posture.is_some();
        StartTurnRequest {
            prompt: self.prompt.clone(),
            input_summary: Some(summarize_text(&self.prompt, TIMELINE_SUMMARY_LIMIT)),
            model: Some(self.model.clone()),
            mode: Some(self.mode_label.clone()),
            permission_posture: self.permission_posture.clone(),
            allow_shell: Some(self.allow_shell),
            trust_mode: Some(self.trust_mode),
            auto_approve: (!pinned).then_some(self.auto_approve),
            ..Default::default()
        }
    }
}

/// Event stream produced by an executor while a task runs.
#[derive(Debug, Clone)]
pub enum TaskExecutionEvent {
    ThreadLinked {
        thread_id: String,
        turn_id: String,
    },
    Status {
        message: String,
    },
    MessageDelta {
        content: String,
    },
    ToolStarted {
        id: String,
        name: String,
        input: Value,
    },
    ToolProgress {
        id: String,
        output: String,
    },
    /// Emitted while a journal-tracked tool is in flight but the journal is
    /// silent, so the worker supervisor's idle watchdog counts the window as
    /// progress instead of idle. Supervisor-side liveness signal only: never
    /// persisted and never shown on the task timeline.
    ToolHeartbeat,
    ToolCompleted {
        id: String,
        name: String,
        success: bool,
        output: String,
        metadata: Option<Value>,
    },
    Error {
        message: String,
    },
    RuntimeEvent {
        seq: u64,
        event: String,
        summary: String,
    },
}

/// Final executor result.
#[derive(Debug, Clone)]
pub struct TaskExecutionResult {
    pub status: TaskStatus,
    pub result_text: Option<String>,
    pub error: Option<String>,
    pub terminal_reason: TaskTerminalReason,
}

impl TaskExecutionResult {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            status: TaskStatus::Failed,
            result_text: None,
            error: Some(error.into()),
            terminal_reason: TaskTerminalReason::Failed,
        }
    }

    fn from_reason(reason: TaskTerminalReason, result_text: Option<String>) -> Self {
        let error = match reason {
            TaskTerminalReason::Completed | TaskTerminalReason::Canceled => None,
            _ => Some(reason.receipt_message()),
        };
        Self {
            status: reason.task_status(),
            result_text,
            error,
            terminal_reason: reason,
        }
    }
}

/// Abstraction for task execution.
#[async_trait]
pub trait TaskExecutor: Send + Sync {
    async fn execute(
        &self,
        task: ExecutionTask,
        events: mpsc::Sender<TaskExecutionEvent>,
        cancel: CancellationToken,
    ) -> TaskExecutionResult;
}

/// Executor backed by the shared runtime and its canonical provider resolver.
pub struct EngineTaskExecutor {
    runtime_threads: SharedRuntimeThreadManager,
    limits: TaskExecutionLimits,
}

impl EngineTaskExecutor {
    #[must_use]
    pub fn new(runtime_threads: SharedRuntimeThreadManager, limits: TaskExecutionLimits) -> Self {
        Self {
            runtime_threads,
            limits,
        }
    }
}

#[async_trait]
impl TaskExecutor for EngineTaskExecutor {
    async fn execute(
        &self,
        task: ExecutionTask,
        events: mpsc::Sender<TaskExecutionEvent>,
        cancel: CancellationToken,
    ) -> TaskExecutionResult {
        if cancel.is_cancelled() {
            return TaskExecutionResult::from_reason(TaskTerminalReason::Canceled, None);
        }
        let (thread_id, turn) = if let Some(destination) = task.destination_thread_id.clone() {
            let deadline = Instant::now() + self.limits.wall_time;
            let turn = loop {
                match self
                    .runtime_threads
                    .start_scheduled_turn(&destination, &task.prompt, &task.id)
                    .await
                {
                    Ok(Some(turn)) => break turn,
                    Ok(None) if Instant::now() < deadline => {
                        tokio::select! {
                            () = cancel.cancelled() => {
                                return TaskExecutionResult::from_reason(
                                    TaskTerminalReason::Canceled,
                                    None,
                                );
                            }
                            () = tokio::time::sleep(DESTINATION_BUSY_RETRY) => {}
                        }
                    }
                    Ok(None) => {
                        return TaskExecutionResult::failed(format!(
                            "Destination thread {destination} stayed busy past the task wall-time budget"
                        ));
                    }
                    Err(err) => {
                        return TaskExecutionResult::failed(format!(
                            "Destination thread {destination} cannot take this turn: {err}"
                        ));
                    }
                }
            };
            (destination, turn)
        } else {
            let thread = match self
                .runtime_threads
                .create_thread(task.thread_request())
                .await
            {
                Ok(thread) => thread,
                Err(err) => {
                    return TaskExecutionResult::failed(format!(
                        "Failed to create runtime thread: {err}"
                    ));
                }
            };

            if cancel.is_cancelled() {
                return TaskExecutionResult::from_reason(TaskTerminalReason::Canceled, None);
            }
            match self
                .runtime_threads
                .start_turn(&thread.id, task.turn_request())
                .await
            {
                Ok(turn) => (thread.id, turn),
                Err(err) => {
                    return TaskExecutionResult::failed(format!("Failed to start task: {err}"));
                }
            }
        };

        emit_task_event(
            &events,
            TaskExecutionEvent::ThreadLinked {
                thread_id: thread_id.clone(),
                turn_id: turn.id.clone(),
            },
        )
        .await;
        emit_task_event(
            &events,
            TaskExecutionEvent::Status {
                message: format!("Task {} started", task.id),
            },
        )
        .await;

        drive_engine_turn(
            self.runtime_threads.as_ref(),
            &thread_id,
            &turn.id,
            events,
            cancel,
            self.limits,
        )
        .await
    }
}

async fn drive_engine_turn(
    runtime_threads: &RuntimeThreadManager,
    thread_id: &str,
    turn_id: &str,
    events: mpsc::Sender<TaskExecutionEvent>,
    cancel: CancellationToken,
    limits: TaskExecutionLimits,
) -> TaskExecutionResult {
    let mut subscription = runtime_threads.subscribe_events();
    let mut guard = ExecutionGuard::new(limits, Instant::now());
    let mut final_text = RuntimeTaskOutput::default();
    let mut cursor = 0u64;
    let mut terminal_status: Option<RuntimeTurnStatus> = None;
    let mut terminal_error: Option<String> = None;
    // Approval requests this turn is waiting on, each with the deadline the
    // runtime bridge will resolve it by (#6118).
    let mut pending_approvals: HashMap<String, Instant> = HashMap::new();
    // Journal ids of tool items currently in flight. The journal records a
    // tool call only at start/completion — nothing in between — so the
    // idle-progress deadline would run unopposed during any silent build,
    // test suite, or MCP call and kill healthy work at the idle limit. A
    // tool that is still running IS progress; the wall-time budget remains
    // the backstop for a hung one (a genuinely stuck tool also hits its own
    // execution timeout long before a 30-minute wall budget in the common
    // case). Both lifecycle edges are keyed by the journal item id:
    // item.started also carries the engine tool-use id under "tool", but
    // the terminal events only carry "item", so keying the insert by
    // tool-use id would never drain the set.
    let mut running_tools: HashSet<String> = HashSet::new();

    loop {
        let batch = match runtime_threads
            .events_from_offset_async(thread_id, cursor, Some(EVENT_CURSOR_BATCH))
            .await
        {
            Ok((batch, next_cursor)) => {
                cursor = next_cursor;
                batch
            }
            Err(err) => {
                return TaskExecutionResult {
                    status: TaskStatus::Failed,
                    result_text: final_text.into_result(true),
                    error: Some(format!("Failed to read runtime events: {err}")),
                    terminal_reason: TaskTerminalReason::Failed,
                };
            }
        };

        let more_pending = batch.len() >= EVENT_CURSOR_BATCH;
        for event in batch {
            if event.thread_id != thread_id {
                continue;
            }
            if event
                .turn_id
                .as_deref()
                .is_some_and(|event_turn| event_turn != turn_id)
            {
                continue;
            }
            match event.event.as_str() {
                "item.started" => {
                    // Journal tool items carry a `tool` payload key;
                    // non-tool items do not. A silent context compaction is
                    // therefore invisible to this set (disclosed gap: a
                    // background task mid-compaction can still hit the idle
                    // deadline — compaction has no heartbeat of its own).
                    if event.payload.get("tool").is_some()
                        && let Some(item_id) = journal_item_id(&event)
                    {
                        running_tools.insert(item_id);
                    }
                }
                "item.completed" | "item.failed" => {
                    if let Some(item_id) = journal_item_id(&event) {
                        running_tools.remove(&item_id);
                    }
                }
                _ => {}
            }
            match event.event.as_str() {
                // An approval parks the turn on an external decision until
                // the runtime bridge answers or its own window closes; note
                // that deadline so the idle watchdog stays off it (#6118).
                "approval.required" => {
                    let approval_id = event
                        .payload
                        .get("approval_id")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                        .unwrap_or_else(|| format!("approval-{}", event.seq));
                    let until = match runtime_threads.approval_decision_timeout() {
                        Some(wait) => Instant::now() + wait,
                        // `0` waits indefinitely by configuration; the wall
                        // deadline still bounds the run.
                        None => Instant::now() + limits.wall_time,
                    };
                    pending_approvals.insert(approval_id, until);
                }
                "approval.decided" => {
                    if let Some(approval_id) =
                        event.payload.get("approval_id").and_then(Value::as_str)
                    {
                        pending_approvals.remove(approval_id);
                    }
                }
                _ => {}
            }
            if runtime_event_is_progress(&event) {
                guard.note_progress(Instant::now());
            }
            if let Some((status, error)) =
                ingest_runtime_event(&event, &mut final_text, &events).await
            {
                // The decision window closed on a pending approval: the
                // runtime already denied the tool, and an unattended run has
                // no operator to answer, so stop the turn instead of letting
                // it keep burning under a failure nobody sees (#6118).
                if event.event.as_str() == "approval.timeout" {
                    let _ = runtime_threads.interrupt_turn(thread_id, turn_id).await;
                }
                terminal_status = Some(status);
                terminal_error = error;
            }
        }

        if terminal_status.is_some() {
            break;
        }

        // While an approval is pending the turn is deliberately waiting on an
        // external decision, not drifting: keep the idle deadline from firing
        // so the bridge's own window can resolve and record it. Entries expire
        // with their window, so a decision that never arrives cannot suspend
        // the watchdog forever (#6118).
        let now = Instant::now();
        pending_approvals.retain(|_, until| now < *until);
        if !pending_approvals.is_empty() {
            guard.note_progress(now);
        }

        // Steady-state progress refresh: the journal is silent for the whole
        // execution window of an in-flight tool, so the idle check must be
        // suppressed for as long as any tool is running — not only on event
        // arrival. The loop below wakes at least every EVENT_CATCHUP_POLL,
        // so this runs throughout silent builds, test suites, and MCP calls.
        if !running_tools.is_empty() {
            guard.note_progress(now);
            // The worker supervisor (run_task) feeds its own guard from the
            // task-event channel and cannot see this journal-derived set, so
            // without a heartbeat its idle deadline would still fire on the
            // only production path that runs this loop. Publish the in-flight
            // window on that channel; the heartbeat refreshes the supervisor's
            // idle clock and is deliberately not persisted or surfaced.
            emit_task_event(&events, TaskExecutionEvent::ToolHeartbeat).await;
        }

        match guard.evaluate(now, cancel.is_cancelled(), false) {
            GuardAction::Interrupt { reason } => {
                let _ = runtime_threads.interrupt_turn(thread_id, turn_id).await;
                emit_task_event(
                    &events,
                    TaskExecutionEvent::Status {
                        message: reason.receipt_message(),
                    },
                )
                .await;
                guard.note_interrupt(Instant::now(), reason);
            }
            GuardAction::Terminalize { reason } => {
                return TaskExecutionResult::from_reason(reason, final_text.into_result(true));
            }
            GuardAction::Run { wait } => {
                if more_pending {
                    continue;
                }
                tokio::select! {
                    _ = cancel.cancelled(), if !cancel.is_cancelled() => {}
                    _ = subscription.recv() => {}
                    _ = sleep(wait) => {}
                }
            }
        }
    }

    let result = match terminal_status.unwrap_or(RuntimeTurnStatus::Failed) {
        RuntimeTurnStatus::Completed => TaskExecutionResult {
            status: TaskStatus::Completed,
            result_text: final_text.into_result(false),
            error: None,
            terminal_reason: TaskTerminalReason::Completed,
        },
        RuntimeTurnStatus::Interrupted | RuntimeTurnStatus::Canceled => TaskExecutionResult {
            status: TaskStatus::Canceled,
            result_text: final_text.into_result(true),
            error: None,
            terminal_reason: TaskTerminalReason::Canceled,
        },
        RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress | RuntimeTurnStatus::Failed => {
            TaskExecutionResult {
                status: TaskStatus::Failed,
                result_text: final_text.into_result(true),
                error: terminal_error
                    .or_else(|| Some(TaskTerminalReason::Failed.receipt_message())),
                terminal_reason: TaskTerminalReason::Failed,
            }
        }
    };
    guard.preserve_timeout_reason(result)
}

async fn emit_task_event(events: &mpsc::Sender<TaskExecutionEvent>, event: TaskExecutionEvent) {
    let _ = events.send(event).await;
}

fn optional_nonzero_text(text: String) -> Option<String> {
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

/// A task result is the last message, not concatenated progress commentary.
/// Only interrupted/failed execution may return a still-streaming message.
#[derive(Default)]
struct RuntimeTaskOutput {
    text: String,
    completed: bool,
}

impl RuntimeTaskOutput {
    fn into_result(self, allow_partial: bool) -> Option<String> {
        (self.completed || allow_partial)
            .then(|| optional_nonzero_text(self.text))
            .flatten()
    }
}

fn append_message_delta(result_text: &mut String, event: &TaskExecutionEvent) {
    if let TaskExecutionEvent::MessageDelta { content } = event {
        result_text.push_str(content);
    }
}

fn runtime_event_is_progress(event: &RuntimeEventRecord) -> bool {
    matches!(
        event.event.as_str(),
        "item.delta" | "item.started" | "item.completed" | "item.failed" | "turn.completed"
    )
}

/// The journal receipt id of the item a lifecycle event carries. Every
/// lifecycle edge (`item.started` / `item.completed` / `item.failed`) repeats
/// it under `payload.item.id`, which is what makes it a stable tracking key
/// across both edges of one tool call.
fn journal_item_id(event: &RuntimeEventRecord) -> Option<String> {
    event
        .payload
        .get("item")
        .and_then(|item| item.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

async fn ingest_runtime_event(
    event: &RuntimeEventRecord,
    final_text: &mut RuntimeTaskOutput,
    events: &mpsc::Sender<TaskExecutionEvent>,
) -> Option<(RuntimeTurnStatus, Option<String>)> {
    emit_task_event(
        events,
        TaskExecutionEvent::RuntimeEvent {
            seq: event.seq,
            event: event.event.clone(),
            summary: summarize_text(&event.payload.to_string(), TIMELINE_SUMMARY_LIMIT),
        },
    )
    .await;

    match event.event.as_str() {
        "item.delta" => {
            let kind = event
                .payload
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if kind == "agent_message" {
                if let Some(content) = event.payload.get("delta").and_then(Value::as_str) {
                    final_text.text.push_str(content);
                    final_text.completed = false;
                    emit_task_event(
                        events,
                        TaskExecutionEvent::MessageDelta {
                            content: content.to_string(),
                        },
                    )
                    .await;
                }
            } else if kind == "tool_call" {
                let output = event
                    .payload
                    .get("delta")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                emit_task_event(
                    events,
                    TaskExecutionEvent::ToolProgress {
                        id: event.item_id.clone().unwrap_or_default(),
                        output,
                    },
                )
                .await;
            }
            None
        }
        "item.started" => {
            if event.payload.pointer("/item/kind").and_then(Value::as_str) == Some("agent_message")
            {
                *final_text = RuntimeTaskOutput::default();
            }
            if let Some(tool) = event.payload.get("tool") {
                let id = tool
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = tool
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let input = tool.get("input").cloned().unwrap_or_else(|| json!({}));
                emit_task_event(events, TaskExecutionEvent::ToolStarted { id, name, input }).await;
            }
            None
        }
        "item.completed" | "item.failed" => {
            if let Some(item) = event.payload.get("item") {
                let kind = item.get("kind").and_then(Value::as_str).unwrap_or_default();
                if kind == "tool_call" || kind == "file_change" || kind == "command_execution" {
                    let metadata = item.get("metadata");
                    // Starts carry the call execution ID; item.id is Runtime's
                    // separate receipt ID. Runtime preserves the call identity
                    // in terminal metadata, including errors and redacted input.
                    let id = metadata
                        .and_then(|meta| {
                            meta.get("tool_result_for")
                                .or_else(|| meta.get("tool_use_id"))
                                .or_else(|| meta.get("tool_call_id"))
                        })
                        .and_then(Value::as_str)
                        .filter(|id| !id.is_empty())
                        .or_else(|| item.get("id").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                    let name = metadata
                        .and_then(|meta| meta.get("tool_name"))
                        .and_then(Value::as_str)
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| {
                            item.get("summary")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .split(':')
                                .next()
                                .unwrap_or("tool")
                                .trim()
                        })
                        .to_string();
                    let output = item
                        .get("detail")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    emit_task_event(
                        events,
                        TaskExecutionEvent::ToolCompleted {
                            id,
                            name,
                            success: event.event == "item.completed",
                            output,
                            metadata: metadata.cloned(),
                        },
                    )
                    .await;
                } else if kind == "agent_message" {
                    // The completed item is authoritative even when catch-up
                    // did not receive its deltas. Replacing avoids duplication.
                    final_text.text = item
                        .get("detail")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("summary").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                    final_text.completed = event.event == "item.completed";
                } else if kind == "status" {
                    let message = item
                        .get("detail")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("summary").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                    emit_task_event(events, TaskExecutionEvent::Status { message }).await;
                } else if kind == "error" {
                    let message = item
                        .get("detail")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("summary").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                    emit_task_event(events, TaskExecutionEvent::Error { message }).await;
                }
            }
            None
        }
        "turn.completed" => {
            if let Some(turn_payload) = event.payload.get("turn") {
                let status = turn_payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("failed");
                let terminal_status = match status {
                    "completed" => RuntimeTurnStatus::Completed,
                    "interrupted" => RuntimeTurnStatus::Interrupted,
                    "canceled" => RuntimeTurnStatus::Canceled,
                    _ => RuntimeTurnStatus::Failed,
                };
                let terminal_error = turn_payload
                    .get("error")
                    .and_then(Value::as_str)
                    .map(ToString::to_string);
                Some((terminal_status, terminal_error))
            } else {
                Some((RuntimeTurnStatus::Completed, None))
            }
        }
        RUNTIME_STORE_FAILURE_EVENT => {
            // The runtime's own store failed. The notice names the file and
            // the next action; `terminal` means no `turn.completed` can
            // follow, so the driver stops waiting instead of idling out (#5931).
            let message = event
                .payload
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Session runtime store failure")
                .to_string();
            emit_task_event(
                events,
                TaskExecutionEvent::Error {
                    message: message.clone(),
                },
            )
            .await;
            event
                .payload
                .get("terminal")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                .then_some((RuntimeTurnStatus::Failed, Some(message)))
        }

        "approval.timeout" => Some((
            RuntimeTurnStatus::Failed,
            Some(
                "Tool approval was not answered within the decision window; the runtime denied the tool and the run stopped."
                    .to_string(),
            ),
        )),
        _ => None,
    }
}

/// Thread-safe task manager.
pub type SharedTaskManager = Arc<TaskManager>;

pub struct TaskManagerShutdownGuard(std::sync::Weak<TaskManager>);

impl Drop for TaskManagerShutdownGuard {
    fn drop(&mut self) {
        if let Some(manager) = self.0.upgrade() {
            manager.shutdown();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    if let Err(error) = manager.shutdown_and_wait().await {
                        tracing::error!(%error, "Task service shutdown failed");
                    }
                });
            }
        }
    }
}

/// A process generation stays live as long as either the manager or actual
/// Runtime work retains it. The Runtime's existing scope lock excludes another
/// executor for that scope; this generation lock proves a particular owner died.
#[derive(Debug)]
pub struct TaskExecutionLease {
    scope: String,
    generation: String,
    _scope_owner: Arc<RuntimeProcessOwnerLock>,
    _generation_owner: RuntimeProcessOwnerLock,
}

impl TaskExecutionLease {
    fn new(
        root: &Path,
        scope: String,
        scope_owner: Arc<RuntimeProcessOwnerLock>,
    ) -> Result<Arc<Self>> {
        validate_execution_id(&scope, 64)?;
        let generation = Uuid::new_v4().simple().to_string();
        let path = execution_lease_path(root, &scope, &generation)?;
        let owner = RuntimeProcessOwnerLock::try_acquire_file(&path, true)?
            .context("Task execution generation is already owned")?;
        Ok(Arc::new(Self {
            scope,
            generation,
            _scope_owner: scope_owner,
            _generation_owner: owner,
        }))
    }
}

fn validate_execution_id(value: &str, length: usize) -> Result<()> {
    if value.len() != length || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Invalid task execution identity");
    }
    Ok(())
}

fn execution_lease_path(root: &Path, scope: &str, generation: &str) -> Result<PathBuf> {
    validate_execution_id(scope, 64)?;
    validate_execution_id(generation, 32)?;
    Ok(root
        .join("execution-owners")
        .join(format!("{scope}.{generation}.lock")))
}

#[cfg(any(test, feature = "test-support"))]
pub fn test_execution_scope(name: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(name.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub struct TaskManager {
    cfg: TaskManagerConfig,
    default_workspace: Mutex<PathBuf>,
    executor: Arc<dyn TaskExecutor>,
    /// The runtime thread store this manager drives, when it owns one.
    runtime_threads: Option<SharedRuntimeThreadManager>,
    tasks_dir: PathBuf,
    artifacts_dir: PathBuf,
    queue_path: PathBuf,
    state: Mutex<ManagerState>,
    notify: Notify,
    cancel_token: CancellationToken,
    execution_lease: Arc<TaskExecutionLease>,
    workers: Mutex<Vec<TaskWorker>>,
    shutdown_drain: Mutex<()>,
    /// Full store loads performed by this manager (tests only, #6573).
    #[cfg(any(test, feature = "test-support"))]
    store_loads: std::sync::atomic::AtomicUsize,
    /// Queue fingerprint reads performed by worker loops (tests only, #6728).
    #[cfg(any(test, feature = "test-support"))]
    fingerprint_reads: std::sync::atomic::AtomicUsize,
}

type InspectionReply = oneshot::Sender<Result<ClaimSchedule>>;

struct TaskWorker {
    inspect: mpsc::Sender<InspectionReply>,
    join: tokio::task::JoinHandle<()>,
}

/// Cheap stat-only view of the shared queue file. Everything that makes work
/// claimable rewrites `queue.json` through an atomic rename: admission writes
/// it before promoting the task record, and claims, cancels and startup
/// recovery write it too. Running tasks flush their records into `tasks/`
/// every few hundred milliseconds but leave the queue alone, so the tasks
/// directory is deliberately not part of the fingerprint. Startup still rebuilds
/// missing queue entries from durable records. Failed claims retry independently,
/// including a queue removal whose task-status write failed. Missing or recent
/// metadata keeps a fallback deadline until it can be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoreFingerprint(Option<(std::time::SystemTime, u64, u64)>);

impl StoreFingerprint {
    fn read(path: &Path) -> Self {
        Self(fs::metadata(path).ok().and_then(|meta| {
            #[cfg(unix)]
            let inode = std::os::unix::fs::MetadataExt::ino(&meta);
            #[cfg(not(unix))]
            let inode = 0;
            Some((meta.modified().ok()?, meta.len(), inode))
        }))
    }

    /// True when the stamp is old enough that a later write cannot share it.
    fn settled(&self, now: std::time::SystemTime) -> bool {
        self.0.as_ref().is_none_or(|(modified, _, _)| {
            now.duration_since(*modified)
                .is_ok_and(|age| age >= READ_SNAPSHOT_SETTLE)
        })
    }
}

/// Stat-only view of everything `load_state` reads, for read-only callers
/// (#6573). Every task and queue write is an atomic rename, which bumps the
/// tasks directory's or the queue file's mtime, so an unchanged snapshot
/// means an unchanged store. The TUI task panel lists tasks every 2.5s; with
/// this, an idle session answers from memory instead of taking the
/// cross-process store lock and re-parsing every task record each time.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadSnapshot {
    queue: StoreFingerprint,
    tasks_dir: StoreFingerprint,
}

impl ReadSnapshot {
    fn read(tasks_dir: &Path, queue_path: &Path) -> Self {
        Self {
            queue: StoreFingerprint::read(queue_path),
            tasks_dir: StoreFingerprint::read(tasks_dir),
        }
    }

    fn settled(&self, now: std::time::SystemTime) -> bool {
        self.queue.settled(now) && self.tasks_dir.settled(now)
    }
}

/// When an idle worker should next claim from the shared store (#6573).
///
/// A worker claims when it was notified in-process, when the queue
/// fingerprint changed since its last attempt, or when a retry deadline passed.
/// Settled empty claims have no deadline. Unreadable/recent metadata gets a
/// fallback and failed claims get exponential backoff ("Task store is busy"). Workers
/// keep checking the fingerprint every tick during a backoff, so a queue
/// write from another process still gets an early retry.
#[derive(Debug, Clone)]
struct ClaimSchedule {
    seen: Option<StoreFingerprint>,
    next_claim: Option<Instant>,
    failure_backoff: Duration,
}

impl ClaimSchedule {
    fn new(now: Instant) -> Self {
        Self {
            seen: None,
            next_claim: Some(now),
            failure_backoff: STORE_REFRESH_INTERVAL,
        }
    }

    fn should_claim(&self, fingerprint: &StoreFingerprint, now: Instant, woken: bool) -> bool {
        woken
            || self.seen.as_ref() != Some(fingerprint)
            || self.next_claim.is_some_and(|deadline| now >= deadline)
    }

    /// How long the worker may sleep before its next pass. Nothing claimable
    /// (a settled empty queue, no retry deadline) only needs to notice another
    /// process's queue write; pending work or a retry deadline keeps the
    /// short tick.
    fn nap(&self) -> Duration {
        if self.next_claim.is_none() {
            STORE_SETTLED_NAP
        } else {
            STORE_REFRESH_INTERVAL
        }
    }

    fn claimed_task(&mut self, now: Instant) {
        self.seen = None;
        self.next_claim = Some(now);
        self.failure_backoff = STORE_REFRESH_INTERVAL;
    }

    fn found_nothing(
        &mut self,
        fingerprint: StoreFingerprint,
        now: Instant,
        wall_now: std::time::SystemTime,
    ) {
        // Re-read a recent stamp after its coarse mtime tick has passed before
        // trusting it indefinitely. Missing metadata never proves no change.
        let settled = fingerprint.0.as_ref().is_some_and(|(modified, _, _)| {
            wall_now
                .duration_since(*modified)
                .is_ok_and(|age| age >= STORE_IDLE_POLL_INTERVAL)
        });
        self.seen = Some(fingerprint);
        self.next_claim = (!settled).then_some(now + STORE_IDLE_POLL_INTERVAL);
        self.failure_backoff = STORE_REFRESH_INTERVAL;
    }

    /// Returns the delay before the next claim unless the queue changes.
    fn claim_failed(&mut self, fingerprint: StoreFingerprint, now: Instant) -> Duration {
        let delay = self.failure_backoff;
        self.seen = Some(fingerprint);
        self.next_claim = Some(now + delay);
        self.failure_backoff = (delay * 2).min(STORE_BUSY_BACKOFF_MAX);
        delay
    }
}

/// The "store is busy" error, naming the lock holder when its record can be
/// read. Reading the record is best effort and never fails the caller (#6573).
fn store_busy_error(lock_path: &Path) -> anyhow::Error {
    match RuntimeProcessOwnerLock::read_holder(lock_path) {
        Some((pid, held_for)) => anyhow::Error::new(TaskStoreBusy).context(format!(
            "Task store is busy; state is unavailable (lock held by pid {pid} for {}s)",
            held_for.as_secs()
        )),
        None => TaskStoreBusy.into(),
    }
}

struct ManagerState {
    tasks: HashMap<String, TaskRecord>,
    queue: VecDeque<String>,
    running_cancel: HashMap<String, CancellationToken>,
    /// Uncommitted typed deltas, reapplied to a fresh record before persistence.
    pending_events: HashMap<String, Vec<TaskExecutionEvent>>,
    /// Store snapshot that `tasks` and `queue` exactly reflect, set only by a
    /// read-only refresh and cleared by every path that may change them.
    read_snapshot: Option<ReadSnapshot>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct QueueFile {
    queue: Vec<String>,
}

impl TaskManager {
    /// Start the manager with the default DeepSeek executor.
    ///
    /// Interactive callers pass an initial session id to isolate new hosts,
    /// or the saved store binding to retain the same authority across resume.
    pub async fn start(
        cfg: TaskManagerConfig,
        api_config: Config,
        plugin_registry: Arc<codewhale_plugins::PluginRegistry>,
        session_id: &str,
        binding: Option<&crate::runtime_threads::RuntimeStoreBinding>,
    ) -> Result<SharedTaskManager> {
        // Resolve the sessions root's canonical spelling off this runtime
        // once, so the saved-store confinement checks here and in later
        // `/resume` / `/load` switches are pure comparisons (#6522).
        crate::runtime_threads::prepare_canonical_sessions_root().await;
        let runtime_threads = Arc::new(RuntimeThreadManager::open_for_session(
            api_config.clone(),
            cfg.default_workspace.clone(),
            RuntimeThreadManagerConfig::for_session(cfg.data_dir.clone(), session_id),
            plugin_registry,
            binding,
        )?);
        let workspace = cfg.default_workspace.clone();
        let workers = tokio::task::spawn_blocking(move || {
            crate::tools::subagent::new_shared_worker_authority(workspace)
        })
        .await?;
        runtime_threads.attach_subagent_manager(workers).await?;
        Self::start_with_runtime_manager(cfg, api_config, runtime_threads).await
    }

    /// Start the manager with an injected runtime thread manager.
    pub async fn start_with_runtime_manager(
        cfg: TaskManagerConfig,
        _api_config: Config,
        runtime_threads: SharedRuntimeThreadManager,
    ) -> Result<SharedTaskManager> {
        let executor: Arc<dyn TaskExecutor> = Arc::new(EngineTaskExecutor::new(
            runtime_threads.clone(),
            cfg.execution_limits,
        ));
        let identity = runtime_threads.task_execution_identity();
        let manager = Self::start_with_executor_and_runtime(
            cfg,
            executor,
            Some(runtime_threads.clone()),
            identity,
        )
        .await?;
        runtime_threads.attach_task_manager(manager.clone());
        Ok(manager)
    }

    /// Start the manager with a custom executor (used for tests).
    #[cfg(any(test, feature = "test-support"))]
    pub async fn start_with_executor(
        cfg: TaskManagerConfig,
        executor: Arc<dyn TaskExecutor>,
    ) -> Result<SharedTaskManager> {
        Self::start_with_executor_in_scope(cfg, executor, "test").await
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn start_with_executor_in_scope(
        cfg: TaskManagerConfig,
        executor: Arc<dyn TaskExecutor>,
        scope: &str,
    ) -> Result<SharedTaskManager> {
        let scope = test_execution_scope(scope);
        let path = cfg
            .data_dir
            .join("execution-owners")
            .join(format!("test-{scope}.lock"));
        let owner = RuntimeProcessOwnerLock::try_acquire_file(&path, true)?
            .context("Mock Runtime execution scope is already owned")?;
        Self::start_with_executor_and_runtime(cfg, executor, None, (scope, Arc::new(owner))).await
    }

    async fn start_with_executor_and_runtime(
        cfg: TaskManagerConfig,
        executor: Arc<dyn TaskExecutor>,
        runtime_threads: Option<SharedRuntimeThreadManager>,
        identity: (String, Arc<RuntimeProcessOwnerLock>),
    ) -> Result<SharedTaskManager> {
        let workers = cfg.worker_count.clamp(1, MAX_WORKERS);
        let tasks_dir = cfg.data_dir.join("tasks");
        let artifacts_dir = cfg.data_dir.join("artifacts");
        let queue_path = cfg.data_dir.join("queue.json");
        tokio::fs::create_dir_all(&tasks_dir)
            .await
            .with_context(|| format!("Failed to create tasks dir {}", tasks_dir.display()))?;
        tokio::fs::create_dir_all(&artifacts_dir)
            .await
            .with_context(|| {
                format!(
                    "Failed to create task artifacts dir {}",
                    artifacts_dir.display()
                )
            })?;

        let execution_lease = TaskExecutionLease::new(&cfg.data_dir, identity.0, identity.1)?;
        let cancel_token = CancellationToken::new();
        let default_workspace = cfg.default_workspace.clone();
        let manager = Arc::new(Self {
            cfg,
            default_workspace: Mutex::new(default_workspace),
            executor,
            runtime_threads,
            tasks_dir,
            artifacts_dir,
            queue_path,
            state: Mutex::new(ManagerState {
                tasks: HashMap::new(),
                queue: VecDeque::new(),
                running_cancel: HashMap::new(),
                pending_events: HashMap::new(),
                read_snapshot: None,
            }),
            notify: Notify::new(),
            cancel_token: cancel_token.clone(),
            execution_lease: execution_lease.clone(),
            workers: Mutex::new(Vec::new()),
            shutdown_drain: Mutex::new(()),
            #[cfg(any(test, feature = "test-support"))]
            store_loads: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(any(test, feature = "test-support"))]
            fingerprint_reads: std::sync::atomic::AtomicUsize::new(0),
        });

        {
            let mut state = manager.state.lock().await;
            let _transaction = manager.lock_store().await?;
            manager.refresh_locked_offloaded(&mut state).await?;
            manager.recover_dead_executions_locked(&mut state)?;
            manager.persist_queue_locked(&state.queue)?;
        }
        if let Some(runtime) = &manager.runtime_threads {
            runtime.retain_task_execution_lease(execution_lease)?;
        }

        manager.initialize_workers(workers).await?;
        Ok(manager)
    }

    /// Establish each worker's first claim/retry boundary after loading state.
    async fn initialize_workers(self: &Arc<Self>, workers: usize) -> Result<Vec<ClaimSchedule>> {
        let mut startup_guard = self.shutdown_guard();
        for _ in 0..workers {
            let manager_clone = Arc::clone(self);
            let (inspect, inspections) = mpsc::channel(1);
            let worker = spawn_supervised(
                "task-manager-worker",
                std::panic::Location::caller(),
                async move {
                    manager_clone.worker_loop(inspections).await;
                },
            );
            self.workers.lock().await.push(TaskWorker {
                inspect,
                join: worker,
            });
        }

        // Spawning is not initialization completion. Each worker acknowledges
        // its first queue inspection after committing its scheduling decision,
        // before executing any claimed task. A retry remains an explicit part
        // of that result; startup does not promise a globally idle store.
        let inspected = self.inspect_workers().await;
        if inspected.is_err() {
            self.shutdown_and_wait().await?;
        }
        startup_guard.0 = std::sync::Weak::new();
        inspected
    }

    /// Inspect through every worker's normal claim path and await its reply.
    ///
    /// The reply belongs to this request, so a fast worker cannot acknowledge
    /// another worker's inspection or race ahead of a subscriber. It is sent
    /// only after the store operation and schedule update finish. This is a
    /// per-worker boundary, not a global snapshot or a task-completion promise:
    /// a claimed task executes after the acknowledgement, and another process
    /// may subsequently change the queue. No elapsed quiet period proves any
    /// of these operations complete (the former #6573 test assumed it did).
    async fn inspect_workers(&self) -> Result<Vec<ClaimSchedule>> {
        if self.cancel_token.is_cancelled() {
            bail!("Task worker inspection requested after shutdown");
        }
        let senders: Vec<_> = self
            .workers
            .lock()
            .await
            .iter()
            .map(|worker| worker.inspect.clone())
            .collect();
        let mut replies = Vec::with_capacity(senders.len());
        for sender in senders {
            let (reply, received) = oneshot::channel();
            sender
                .send(reply)
                .await
                .map_err(|_| anyhow!("Task worker stopped before queue inspection"))?;
            replies.push(received);
        }
        let mut schedules = Vec::with_capacity(replies.len());
        for reply in replies {
            schedules.push(
                reply
                    .await
                    .context("Task worker stopped during queue inspection")??,
            );
        }
        Ok(schedules)
    }

    /// Request shutdown. Ownership remains retained through actual execution.
    pub fn shutdown(&self) {
        self.cancel_token.cancel();
    }

    pub fn shutdown_guard(self: &Arc<Self>) -> TaskManagerShutdownGuard {
        TaskManagerShutdownGuard(Arc::downgrade(self))
    }

    pub async fn shutdown_and_wait(&self) -> Result<()> {
        self.shutdown();
        let _drain = self.shutdown_drain.lock().await;
        if let Some(runtime) = &self.runtime_threads {
            runtime.close_execution_admission().await;
        }
        // Waiting through the admission fence settles requests that passed
        // their admission check before shutdown was requested.
        {
            let _transaction = self.lock_store().await?;
        }
        let mut failure = None;
        {
            let mut workers = self.workers.lock().await;
            while let Some(worker) = workers.last_mut() {
                // Await by reference: canceling this caller leaves the join in
                // the manager, so a later drain still waits for actual exit.
                let result = (&mut worker.join).await;
                workers.pop();
                if let Err(error) = result {
                    failure = Some(anyhow!("Task worker shutdown failed: {error}"));
                }
            }
        }
        if let Some(runtime) = &self.runtime_threads {
            runtime.shutdown_and_wait().await?;
        }
        failure.map_or(Ok(()), Err)
    }

    pub fn execution_scope(&self) -> &str {
        &self.execution_lease.scope
    }

    /// Apply `edit` to the runtime threads' authoritative config and reload
    /// it, so runtime-chat and queued runtime turns see a setting the UI just
    /// persisted instead of their startup snapshot. No runtime manager is a
    /// no-op.
    pub async fn reload_runtime_config_with(
        &self,
        edit: impl FnOnce(&mut codewhale_runtime::config::Config),
    ) -> Result<()> {
        let Some(runtime) = &self.runtime_threads else {
            return Ok(());
        };
        let mut config = runtime.read_config().clone();
        edit(&mut config);
        runtime.reload_config(config).await.map(|_| ())
    }

    pub fn session_store_binding(&self) -> Option<crate::runtime_threads::RuntimeStoreBinding> {
        self.runtime_threads
            .as_ref()
            .map(|runtime| runtime.session_store_binding())
    }

    pub async fn set_default_workspace(&self, workspace: PathBuf) {
        let mut default_workspace = self.default_workspace.lock().await;
        *default_workspace = workspace;
    }

    pub async fn default_workspace(&self) -> PathBuf {
        self.default_workspace.lock().await.clone()
    }

    /// Enqueue a new task.
    pub async fn add_task(&self, req: NewTaskRequest) -> Result<TaskRecord> {
        self.add_task_with_id(req, Self::new_task_id()).await
    }

    /// Allocate the durable owner identity before queue insertion so callers
    /// can register graph spawn intent first.
    #[must_use]
    pub fn new_task_id() -> String {
        format!("task_{}", &Uuid::new_v4().simple().to_string()[..16])
    }

    /// Read the exact durable task binding without adopting another process's
    /// queue. Used by the automation dispatcher while it owns the store claim.
    pub fn read_bound_task(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        if task_id.is_empty()
            || !task_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            bail!("Invalid durable task id");
        }
        read_bound_task_file(&self.tasks_dir.join(format!("{task_id}.json")), task_id)
    }

    /// Recover a persisted automation admission under its cross-process
    /// dispatch lock. A promoted task is accepted work, including failed or
    /// interrupted work; returning it must never enqueue it again.
    pub async fn recover_task_admission(
        &self,
        request: NewTaskRequest,
        task_id: String,
    ) -> Result<TaskRecord> {
        validate_preallocated_task_id(&task_id)?;
        if let Some(task) = self.read_bound_task(&task_id)? {
            validate_bound_task_request(&task, &request)?;
            return Ok(task);
        }
        let staged_path = self.tasks_dir.join(format!(".{task_id}.json.pending"));
        let admission = if let Some(staged) = read_bound_task_file(&staged_path, &task_id)? {
            validate_bound_task_request(&staged, &request)?;
            if staged.status != TaskStatus::Queued
                || staged.started_at.is_some()
                || staged.thread_id.is_some()
                || staged.turn_id.is_some()
            {
                bail!("Unpromoted task stage contains execution evidence; refusing to replay it");
            }
            // The original resolved settings remain durable in this stage
            // until promotion, including if recovery itself is interrupted.
            self.admit_task_record(staged, true).await
        } else {
            self.add_task_with_id(request.clone(), task_id.clone())
                .await
        };
        match admission {
            Ok(task) => Ok(task),
            Err(error) => {
                // Preserve a task promoted before a torn response; do not
                // replace its identity or convert accepted work into a retry.
                if let Some(task) = self.read_bound_task(&task_id)? {
                    validate_bound_task_request(&task, &request)?;
                    Ok(task)
                } else {
                    Err(error)
                }
            }
        }
    }

    /// Enqueue using a preallocated id. This is crate-visible only for the
    /// model tool's register-before-work transaction.
    pub async fn add_task_with_id(
        &self,
        req: NewTaskRequest,
        task_id: String,
    ) -> Result<TaskRecord> {
        let prompt = req.prompt.trim().to_string();
        if prompt.is_empty() {
            bail!("Task prompt cannot be empty");
        }
        if (req.model_provider.is_some() || req.model_provider_id.is_some())
            && req
                .model
                .as_deref()
                .is_none_or(|model| model.trim().is_empty())
        {
            bail!("A pinned task provider requires an explicit model");
        }
        // The worker runs this same projection when it opens the task's
        // thread. Running it here as well refuses an unknown mode or posture
        // at the boundary the request crossed, instead of after the task has
        // sat in the durable queue and a worker has claimed it.
        codewhale_runtime::runtime_policy::RuntimePolicyProjection::from_request(
            req.mode
                .as_deref()
                .filter(|mode| !mode.trim().is_empty())
                .unwrap_or(&self.cfg.default_mode),
            req.permission_posture.as_deref(),
            req.auto_approve,
        )?;
        validate_preallocated_task_id(&task_id)?;

        let task = TaskRecord {
            schema_version: CURRENT_TASK_SCHEMA_VERSION,
            // 16 random hex chars (was 8; ~60 bits of entropy once UUIDv4's
            // fixed version nibble is discounted): task ids live in durable
            // state that accumulates across restarts, and a collision
            // overwrites a record while leaving a duplicate queue entry.
            // `resolve_task_id` matches by prefix, so short references still
            // work.
            id: task_id,
            prompt,
            name: req
                .name
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty()),
            model: req.model.unwrap_or_else(|| self.cfg.default_model.clone()),
            model_provider: req.model_provider,
            model_provider_id: req.model_provider_id,
            workspace: match req.workspace {
                Some(workspace) => workspace,
                None => self.default_workspace().await,
            },
            mode: req.mode.unwrap_or_else(|| self.cfg.default_mode.clone()),
            allow_shell: req.allow_shell.unwrap_or(self.cfg.allow_shell),
            trust_mode: req.trust_mode.unwrap_or(self.cfg.trust_mode),
            // Auto-approval must be opted into explicitly
            // (GHSA-72w5-pf8h-xfp4).
            auto_approve: req.auto_approve.unwrap_or(false),
            permission_posture: req.permission_posture,
            status: TaskStatus::Queued,
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
            duration_ms: None,
            result_summary: None,
            result_detail_path: None,
            error: None,
            terminal_reason: None,
            thread_id: None,
            turn_id: None,
            owner_session_id: req.owner_session_id,
            destination_thread_id: req.destination_thread_id,
            execution_scope: Some(self.execution_scope().to_string()),
            execution_generation: None,
            cancel_requested_seq: 0,
            runtime_event_count: 0,
            lifecycle_seq: 1,
            checklist: TaskChecklistState::default(),
            gates: Vec::new(),
            attempts: Vec::new(),
            artifacts: Vec::new(),
            github_events: Vec::new(),
            tool_calls: Vec::new(),
            timeline: vec![TaskTimelineEntry {
                timestamp: Utc::now(),
                kind: "queued".to_string(),
                summary: "Task queued".to_string(),
                detail_path: None,
            }],
        };

        self.admit_task_record(task, false).await
    }

    async fn admit_task_record(&self, task: TaskRecord, recover_stage: bool) -> Result<TaskRecord> {
        {
            let mut state = self.state.lock().await;
            let _transaction = self.lock_store().await?;
            self.refresh_locked_offloaded(&mut state).await?;
            if self.cancel_token.is_cancelled() {
                bail!("Task manager is shutting down; admission is closed");
            }
            if task.execution_scope.as_deref() != Some(self.execution_scope()) {
                bail!(
                    "Task execution scope is unverified or belongs to another Runtime; refusing adoption"
                );
            }
            let task_path = self.tasks_dir.join(format!("{}.json", task.id));
            // The staged extension is intentionally not `.json`, so startup
            // replay ignores an interrupted create until the queue write has
            // succeeded and this file is atomically promoted.
            let staged_task_path = self.tasks_dir.join(format!(".{}.json.pending", task.id));
            if recover_stage {
                if let Some(accepted) = self.read_bound_task(&task.id)? {
                    validate_bound_task_request(&accepted, &NewTaskRequest::from_task(&task))?;
                    return Ok(accepted);
                }
                let current = read_bound_task_file(&staged_task_path, &task.id)?
                    .context("Unaccepted task stage disappeared during recovery")?;
                if serde_json::to_value(&current)? != serde_json::to_value(&task)? {
                    bail!("Unaccepted task stage changed during recovery");
                }
            }
            if state.tasks.contains_key(&task.id)
                || task_path.exists()
                || (!recover_stage && staged_task_path.exists())
            {
                bail!("Task id already exists: {}", task.id);
            }
            let mut next_queue = state.queue.clone();
            if !next_queue.contains(&task.id) {
                next_queue.push_back(task.id.clone());
            }

            // Stage the owner record, then persist its queue membership, then
            // atomically promote it. A crash before promotion leaves either an
            // ignored staged file or a queue entry with no task (which replay
            // drops); a crash after promotion leaves the complete runnable
            // pair. In-memory scheduling is published only after all three.
            if !recover_stage {
                write_json_atomic(&staged_task_path, &task)?;
            }
            if let Err(err) = self.persist_queue_locked(&next_queue) {
                if !recover_stage
                    && let Err(cleanup_err) = tokio::fs::remove_file(&staged_task_path).await
                {
                    tracing::warn!(
                        task_id = %task.id,
                        error = %cleanup_err,
                        "failed to remove ignored staged task after queue write failure"
                    );
                }
                return Err(err);
            }
            if let Err(promote_err) = tokio::fs::rename(&staged_task_path, &task_path).await {
                let rollback_error = self.persist_queue_locked(&state.queue).err();
                let cleanup_error = if recover_stage {
                    None
                } else {
                    tokio::fs::remove_file(&staged_task_path).await.err()
                };
                let mut message =
                    format!("Failed to promote staged task {}: {promote_err}", task.id);
                if let Some(rollback_error) = rollback_error {
                    message.push_str(&format!("; queue rollback also failed: {rollback_error:#}"));
                }
                if let Some(cleanup_error) = cleanup_error {
                    message.push_str(&format!(
                        "; ignored staged-file cleanup also failed: {cleanup_error}"
                    ));
                }
                bail!(message);
            }
            state.queue = next_queue;
            state.tasks.insert(task.id.clone(), task.clone());
        }
        self.notify.notify_one();
        Ok(task)
    }

    /// List tasks, newest first.
    pub async fn list_tasks(&self, limit: Option<usize>) -> Result<Vec<TaskSummary>> {
        self.list_tasks_scoped(limit, None).await
    }

    /// List tasks, newest first, optionally scoped to a workspace.
    pub async fn list_tasks_scoped(
        &self,
        limit: Option<usize>,
        workspace: Option<&Path>,
    ) -> Result<Vec<TaskSummary>> {
        self.list_tasks_visible_to(limit, workspace, None).await
    }

    /// List tasks owned by a session, newest first, optionally scoped to a workspace.
    ///
    /// Ownerless legacy records fail closed and are not model-visible.
    pub async fn list_tasks_for_owner(
        &self,
        limit: Option<usize>,
        workspace: Option<&Path>,
        owner_session_id: &str,
    ) -> Result<Vec<TaskSummary>> {
        self.list_tasks_visible_to(limit, workspace, Some(owner_session_id))
            .await
    }

    async fn list_tasks_visible_to(
        &self,
        limit: Option<usize>,
        workspace: Option<&Path>,
        owner_session_id: Option<&str>,
    ) -> Result<Vec<TaskSummary>> {
        let mut state = self.state.lock().await;
        self.refresh_for_read(&mut state).await?;
        let mut items = state
            .tasks
            .values()
            .filter(|record| {
                workspace.is_none_or(|workspace| record.workspace.as_path() == workspace)
                    && owner_session_id.is_none_or(|owner_session_id| {
                        record.owner_session_id.as_deref() == Some(owner_session_id)
                    })
            })
            .map(TaskSummary::from)
            .collect::<Vec<_>>();
        items.sort_by_key(|i| std::cmp::Reverse(i.created_at));
        if let Some(limit) = limit {
            items.truncate(limit);
        }
        Ok(items)
    }

    /// Retrieve a task by full id or prefix.
    pub async fn get_task(&self, id_or_prefix: &str) -> Result<TaskRecord> {
        self.get_task_visible_to(id_or_prefix, None).await
    }

    /// Retrieve a session-owned task by full id or prefix.
    ///
    /// Ownership is applied before id resolution so foreign records cannot
    /// disclose their existence through exact matches or prefix ambiguity.
    pub async fn get_task_for_owner(
        &self,
        id_or_prefix: &str,
        owner_session_id: &str,
    ) -> Result<TaskRecord> {
        self.get_task_visible_to(id_or_prefix, Some(owner_session_id))
            .await
    }

    /// Retrieve a task the interactive operator can inspect by full id or prefix.
    ///
    /// Scheduled automations have no session owner because they can outlive the
    /// session that configured them. They remain operator-visible only while
    /// bound to this manager's verified Runtime execution scope. Session-owned
    /// tasks keep their existing isolation, and unscoped legacy records remain
    /// hidden.
    pub async fn get_task_for_interactive_session(
        &self,
        id_or_prefix: &str,
        owner_session_id: &str,
    ) -> Result<TaskRecord> {
        let mut state = self.state.lock().await;
        self.refresh_for_read(&mut state).await?;
        let id = resolve_task_id_visible_to_operator(
            &state.tasks,
            id_or_prefix,
            owner_session_id,
            self.execution_scope(),
        )?;
        state
            .tasks
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow!("Task not found: {id_or_prefix}"))
    }

    /// Retrieve the exact owned task stamped onto a trusted runtime thread.
    ///
    /// The runtime thread supplies a full durable id rather than model input.
    /// An ownerless task needs the current trusted execution scope. Legacy
    /// ownerless tasks without that provenance still fail closed.
    pub async fn get_task_for_active_runtime(&self, task_id: &str) -> Result<TaskRecord> {
        let mut state = self.state.lock().await;
        self.refresh_for_read(&mut state).await?;
        state
            .tasks
            .get(task_id)
            .filter(|task| match task.execution_scope.as_deref() {
                Some(scope) => scope == self.execution_scope(),
                None => task.owner_session_id.is_some(),
            })
            .cloned()
            .ok_or_else(|| anyhow!("Task not found: {task_id}"))
    }

    async fn get_task_visible_to(
        &self,
        id_or_prefix: &str,
        owner_session_id: Option<&str>,
    ) -> Result<TaskRecord> {
        let mut state = self.state.lock().await;
        self.refresh_for_read(&mut state).await?;
        let id = resolve_task_id_visible_to(&state.tasks, id_or_prefix, owner_session_id)?;
        state
            .tasks
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow!("Task not found: {id_or_prefix}"))
    }

    /// Cancel a queued or running task by id/prefix.
    pub async fn cancel_task(&self, id_or_prefix: &str) -> Result<TaskCancellation> {
        self.cancel_task_visible_to(id_or_prefix, None, None).await
    }

    /// Cancel a queued or running task owned by the given session.
    ///
    /// Ownerless legacy and foreign records fail closed.
    pub async fn cancel_task_for_owner(
        &self,
        id_or_prefix: &str,
        owner_session_id: &str,
    ) -> Result<TaskCancellation> {
        self.cancel_task_visible_to(id_or_prefix, Some(owner_session_id), None)
            .await
    }

    /// Cancel a task visible to the interactive operator.
    ///
    /// This is the cancellation counterpart to
    /// [`Self::get_task_for_interactive_session`]. It exists for human TUI
    /// actions, including the automation view's cancel button; model and child
    /// task APIs retain session-only visibility.
    pub async fn cancel_task_for_interactive_session(
        &self,
        id_or_prefix: &str,
        owner_session_id: &str,
    ) -> Result<TaskCancellation> {
        self.cancel_task_visible_to(
            id_or_prefix,
            Some(owner_session_id),
            Some(self.execution_scope()),
        )
        .await
    }

    /// Cancel the exact owned task stamped onto a trusted runtime thread.
    pub async fn cancel_task_for_active_runtime(&self, task_id: &str) -> Result<TaskCancellation> {
        self.get_task_for_active_runtime(task_id).await?;
        self.cancel_task(task_id).await
    }

    async fn cancel_task_visible_to(
        &self,
        id_or_prefix: &str,
        owner_session_id: Option<&str>,
        operator_execution_scope: Option<&str>,
    ) -> Result<TaskCancellation> {
        let mut state = self.state.lock().await;
        let _transaction = self.lock_store().await?;
        self.refresh_locked_offloaded(&mut state).await?;
        let id = match (owner_session_id, operator_execution_scope) {
            (Some(owner_session_id), Some(execution_scope)) => resolve_task_id_visible_to_operator(
                &state.tasks,
                id_or_prefix,
                owner_session_id,
                execution_scope,
            )?,
            _ => resolve_task_id_visible_to(&state.tasks, id_or_prefix, owner_session_id)?,
        };
        let now = Utc::now();

        let mut cancel_running = false;
        let disposition = {
            let task = state
                .tasks
                .get_mut(&id)
                .ok_or_else(|| anyhow!("Task not found: {id}"))?;
            match task.status {
                TaskStatus::Queued => {
                    task.status = TaskStatus::Canceled;
                    task.lifecycle_seq = task.lifecycle_seq.saturating_add(1);
                    task.ended_at = Some(now);
                    task.duration_ms = Some(0);
                    push_timeline_entry(
                        task,
                        TaskTimelineEntry {
                            timestamp: now,
                            kind: "canceled".to_string(),
                            summary: "Task canceled before execution".to_string(),
                            detail_path: None,
                        },
                    );
                    state.queue.retain(|queued_id| queued_id != &id);
                    TaskCancelDisposition::Forced
                }
                TaskStatus::Running => {
                    cancel_running = true;
                    task.lifecycle_seq = task.lifecycle_seq.saturating_add(1);
                    task.cancel_requested_seq = task.lifecycle_seq;
                    push_timeline_entry(
                        task,
                        TaskTimelineEntry {
                            timestamp: now,
                            kind: "cancel_requested".to_string(),
                            summary: "Cancellation requested".to_string(),
                            detail_path: None,
                        },
                    );
                    TaskCancelDisposition::Requested
                }
                TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Canceled => {
                    TaskCancelDisposition::AlreadyFinished
                }
            }
        };

        if cancel_running && let Some(token) = state.running_cancel.get(&id) {
            token.cancel();
        }

        self.persist_changed_task_locked(&mut state, &id)?;
        self.persist_queue_locked(&state.queue)?;
        let task = state
            .tasks
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow!("Task not found: {id}"))?;
        Ok(TaskCancellation { task, disposition })
    }

    /// Return aggregate status counters.
    pub async fn counts(&self) -> Result<TaskCounts> {
        let mut state = self.state.lock().await;
        self.refresh_for_read(&mut state).await?;
        let mut counts = TaskCounts::default();
        for task in state.tasks.values() {
            match task.status {
                TaskStatus::Queued => counts.queued += 1,
                TaskStatus::Running => counts.running += 1,
                TaskStatus::Completed => counts.completed += 1,
                TaskStatus::Failed => counts.failed += 1,
                TaskStatus::Canceled => counts.canceled += 1,
            }
        }
        Ok(counts)
    }

    /// Root directory for durable task state.
    #[must_use]
    pub fn data_dir(&self) -> PathBuf {
        self.cfg.data_dir.clone()
    }

    /// Live events from the runtime thread store this manager drives, when it
    /// owns one. The TUI taps `runtime.store_failure` here (#5931).
    #[must_use]
    pub fn subscribe_runtime_events(
        &self,
    ) -> Option<tokio::sync::broadcast::Receiver<RuntimeEventRecord>> {
        self.runtime_threads
            .as_ref()
            .map(|runtime| runtime.subscribe_events())
    }

    /// Resolve a task artifact reference to an absolute path.
    #[must_use]
    pub fn artifact_absolute_path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cfg.data_dir.join(path)
        }
    }

    /// Write a durable task artifact and return the persisted path reference.
    pub fn write_task_artifact(
        &self,
        task_id: &str,
        label: &str,
        content: &str,
    ) -> Result<PathBuf> {
        self.write_artifact(task_id, label, content)
    }

    /// Apply model-visible tool metadata to a task and persist it.
    pub async fn record_tool_metadata(
        &self,
        id_or_prefix: &str,
        metadata: &Value,
    ) -> Result<TaskRecord> {
        let mut state = self.state.lock().await;
        let _transaction = self.lock_store().await?;
        self.refresh_locked_offloaded(&mut state).await?;
        let id = resolve_task_id(&state.tasks, id_or_prefix)?;
        let updated = {
            let task = state
                .tasks
                .get_mut(&id)
                .ok_or_else(|| anyhow!("Task not found: {id}"))?;
            self.apply_task_update_metadata(task, Some(metadata))?;
            task.clone()
        };
        self.persist_changed_task_locked(&mut state, &id)?;
        Ok(updated)
    }

    async fn claim_next_task(&self) -> Result<Option<(String, ExecutionTask, CancellationToken)>> {
        let mut state = self.state.lock().await;
        let _transaction = self.lock_store().await?;
        // The worker loop runs this repeatedly; keep the directory scan and
        // JSON parsing off the async runtime threads (#6573).
        let (tasks_dir, queue_path) = (self.tasks_dir.clone(), self.queue_path.clone());
        let loaded = tokio::task::spawn_blocking(move || load_state(&tasks_dir, &queue_path))
            .await
            .context("Task store load was interrupted")??;
        self.apply_loaded_locked(&mut state, loaded)?;
        if self.cancel_token.is_cancelled() {
            return Ok(None);
        }
        let Some(id) = state
            .queue
            .iter()
            .find(|id| {
                state.tasks.get(*id).is_some_and(|task| {
                    task.status == TaskStatus::Queued
                        && task.execution_scope.as_deref() == Some(self.execution_scope())
                })
            })
            .cloned()
        else {
            return Ok(None);
        };
        state.read_snapshot = None;
        state.queue.retain(|queued| queued != &id);
        let task = state
            .tasks
            .get_mut(&id)
            .context("Claimed task is missing")?;
        let now = Utc::now();
        task.status = TaskStatus::Running;
        task.execution_generation = Some(self.execution_lease.generation.clone());
        task.lifecycle_seq = task.lifecycle_seq.saturating_add(1);
        task.started_at = Some(now);
        task.ended_at = None;
        task.duration_ms = None;
        task.error = None;
        push_timeline_entry(
            task,
            TaskTimelineEntry {
                timestamp: now,
                kind: "running".into(),
                summary: "Task started".into(),
                detail_path: None,
            },
        );
        let request = ExecutionTask::from(&*task);
        // Removing queue membership first is recoverable from the still-Queued
        // record. Executor polling requires BOTH durable writes to succeed.
        self.persist_queue_locked(&state.queue)?;
        self.persist_changed_task_locked(&mut state, &id)?;
        let cancel = CancellationToken::new();
        state.running_cancel.insert(id.clone(), cancel.clone());
        Ok(Some((id, request, cancel)))
    }

    /// Claim and run queued work.
    ///
    /// Several processes can share one data dir, and every claim takes the
    /// cross-process store lock and reloads the whole store. An idle worker
    /// therefore only claims when it was notified in-process, when the store
    /// fingerprint changed, or when unreadable/recent metadata needs a retry.
    /// A settled empty queue does no periodic full reload; failed claims still
    /// back off exponentially instead of retrying every tick (#6573, #6728).
    async fn worker_loop(self: Arc<Self>, mut inspections: mpsc::Receiver<InspectionReply>) {
        let mut schedule = ClaimSchedule::new(Instant::now());
        let mut woken = true;
        let mut initial_claim = true;
        // True while consecutive claims are failing: the first failure of an
        // episode is an error, repeats are debug until a claim succeeds (#6573).
        let mut claim_failing = false;
        // Startup supplies the first inspection request. Waiting for it avoids
        // running a recovered task before startup can observe its claim result.
        let mut reply = tokio::select! {
            _ = self.cancel_token.cancelled() => return,
            request = inspections.recv() => request,
        };
        loop {
            if self.cancel_token.is_cancelled() {
                break;
            }
            // Read before claiming so a write racing the claim changes the
            // fingerprint and is picked up on the next pass.
            #[cfg(any(test, feature = "test-support"))]
            self.fingerprint_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let fingerprint = StoreFingerprint::read(&self.queue_path);
            if schedule.should_claim(&fingerprint, Instant::now(), woken) {
                let startup_claim = std::mem::take(&mut initial_claim);
                match self.claim_next_task().await {
                    Ok(Some((id, request, cancel))) => {
                        claim_failing = false;
                        schedule.claimed_task(Instant::now());
                        if let Some(reply) = reply.take() {
                            let _ = reply.send(Ok(schedule.clone()));
                        }
                        self.run_task(id, request, cancel).await;
                        woken = true;
                        continue;
                    }
                    Ok(None) => {
                        claim_failing = false;
                        schedule.found_nothing(
                            fingerprint,
                            Instant::now(),
                            std::time::SystemTime::now(),
                        );
                    }
                    Err(error) => {
                        let retry = schedule.claim_failed(fingerprint, Instant::now());
                        let retry_ms = retry.as_millis() as u64;
                        if claim_failing {
                            tracing::debug!(
                                %error,
                                retry_ms,
                                "Task claim still unavailable; executor was not polled"
                            );
                        } else {
                            claim_failing = true;
                            tracing::error!(
                                %error,
                                retry_ms,
                                "Task claim unavailable; executor was not polled"
                            );
                        }
                        if let Some(reply) = reply.take() {
                            if startup_claim && error.is::<TaskStoreBusy>() {
                                // A committed contention retry is worker readiness,
                                // not a claim that storage is settled or idle.
                                let _ = reply.send(Ok(schedule.clone()));
                            } else {
                                let _ = reply.send(Err(error));
                            }
                        }
                    }
                }
            }
            if let Some(reply) = reply.take() {
                let _ = reply.send(Ok(schedule.clone()));
            }
            woken = false;
            tokio::select! {
                _ = self.cancel_token.cancelled() => break,
                request = inspections.recv() => {
                    let Some(request) = request else { break };
                    reply = Some(request);
                }
                _ = self.notify.notified() => woken = true,
                _ = sleep(schedule.nap()) => {},
            }
        }
    }

    fn observe_task_cancellation(&self, task_id: &str, cancel: &CancellationToken) -> Result<()> {
        let task = self
            .read_bound_task(task_id)?
            .context("Running task is missing")?;
        self.require_execution_owner(&task)?;
        if task.cancel_requested_seq > 0 {
            cancel.cancel();
        }
        Ok(())
    }

    async fn run_task(&self, task_id: String, request: ExecutionTask, cancel: CancellationToken) {
        let (event_tx, mut event_rx) = mpsc::channel(TASK_EVENT_CHANNEL_CAPACITY);
        let exec_fut = self
            .executor
            .execute(request.clone(), event_tx, cancel.clone());
        tokio::pin!(exec_fut);

        let mut guard = ExecutionGuard::new(self.cfg.execution_limits, Instant::now());
        let mut dirty = false;
        let mut accumulated_result_text = String::new();
        let persist_debounce = self.cfg.execution_limits.persist_debounce;
        let mut persist_deadline = None;

        let mut next_store_poll = Instant::now();
        let mut execution_started = false;
        let mut blocked_event: Option<TaskExecutionEvent> = None;
        let mut next_event_retry = Instant::now();
        let (mut result, manager_terminalized) = loop {
            if Instant::now() >= next_store_poll {
                if let Err(error) = self.observe_task_cancellation(&task_id, &cancel) {
                    tracing::error!(%error, "Task ownership unavailable; requesting Runtime cancellation");
                    cancel.cancel();
                }
                next_store_poll = Instant::now() + STORE_REFRESH_INTERVAL;
            }
            if !execution_started && (cancel.is_cancelled() || self.cancel_token.is_cancelled()) {
                let reason = if self.cancel_token.is_cancelled() {
                    TaskTerminalReason::Shutdown
                } else {
                    TaskTerminalReason::Canceled
                };
                break (TaskExecutionResult::from_reason(reason, None), true);
            }
            if Instant::now() >= next_event_retry {
                if let Some(event) = blocked_event.take()
                    && self
                        .process_execution_event(
                            &task_id,
                            event.clone(),
                            &mut guard,
                            &mut accumulated_result_text,
                            &mut dirty,
                        )
                        .await
                        .is_err()
                {
                    blocked_event = Some(event);
                    cancel.cancel();
                }
                next_event_retry = Instant::now() + STORE_REFRESH_INTERVAL;
            }
            let mut action = guard.evaluate(
                Instant::now(),
                cancel.is_cancelled(),
                self.cancel_token.is_cancelled(),
            );
            if matches!(
                action,
                GuardAction::Interrupt {
                    reason: TaskTerminalReason::IdleTimeout
                }
            ) {
                // Progress already accepted by the executor must win an idle
                // deadline race. Drain only the events queued at this instant
                // so a producer cannot keep the watchdog from re-evaluating
                // wall time, shutdown, or explicit cancellation indefinitely.
                let queued = event_rx.len();
                for _ in 0..queued {
                    if blocked_event.is_some() {
                        break;
                    }
                    let Ok(event) = event_rx.try_recv() else {
                        break;
                    };
                    if self
                        .process_execution_event(
                            &task_id,
                            event.clone(),
                            &mut guard,
                            &mut accumulated_result_text,
                            &mut dirty,
                        )
                        .await
                        .is_err()
                    {
                        blocked_event = Some(event);
                        cancel.cancel();
                    }
                }
                action = guard.evaluate(
                    Instant::now(),
                    cancel.is_cancelled(),
                    self.cancel_token.is_cancelled(),
                );
            }
            match action {
                GuardAction::Interrupt { reason } => {
                    cancel.cancel();
                    guard.note_interrupt(Instant::now(), reason);
                    continue;
                }
                GuardAction::Terminalize { reason } => {
                    break (TaskExecutionResult::from_reason(reason, None), true);
                }
                GuardAction::Run { wait } => {
                    execution_started = true;
                    // Keep the first dirty event's deadline across heartbeat
                    // and ownership-poll wakes. An urgent persisted event
                    // clears dirty and starts a fresh window for later deltas.
                    let persist_at = if dirty {
                        *persist_deadline.get_or_insert_with(|| Instant::now() + persist_debounce)
                    } else {
                        persist_deadline = None;
                        Instant::now()
                    };
                    tokio::select! {
                        biased;
                        exec_result = &mut exec_fut => {
                            break (guard.preserve_timeout_reason(exec_result), false);
                        }
                        // A ready event channel must not starve a due write.
                        _ = tokio::time::sleep_until(persist_at.into()), if dirty => {
                            match self.flush_task(&task_id).await {
                                Ok(()) => {
                                    dirty = false;
                                    persist_deadline = None;
                                }
                                Err(err) => {
                                    tracing::error!("Failed to debounce-persist task {task_id}: {err}");
                                    cancel.cancel();
                                    persist_deadline = Some(Instant::now() + persist_debounce);
                                }
                            }
                        }
                        maybe_event = event_rx.recv(), if blocked_event.is_none() => {
                            if let Some(event) = maybe_event
                                && self.process_execution_event(
                                    &task_id, event.clone(), &mut guard,
                                    &mut accumulated_result_text, &mut dirty,
                                ).await.is_err() {
                                blocked_event = Some(event);
                                cancel.cancel();
                            }
                        }
                        _ = self.cancel_token.cancelled(), if !self.cancel_token.is_cancelled() => {
                            cancel.cancel();
                        }
                        _ = sleep(wait.min(STORE_REFRESH_INTERVAL)) => {}
                    }
                }
            }
        };

        // Stop accepting producer events while one event cannot be retained.
        // The pending delta set and channel remain bounded during disk failure;
        // keep the execution lease until accepted events and the terminal receipt
        // have actually been persisted. A storage failure is not completion.
        event_rx.close();
        loop {
            let event = blocked_event.take().or_else(|| event_rx.try_recv().ok());
            let Some(event) = event else {
                break;
            };
            while self
                .process_execution_event(
                    &task_id,
                    event.clone(),
                    &mut guard,
                    &mut accumulated_result_text,
                    &mut dirty,
                )
                .await
                .is_err()
            {
                sleep(STORE_REFRESH_INTERVAL).await;
            }
        }
        if manager_terminalized {
            result.result_text = optional_nonzero_text(accumulated_result_text);
        }
        loop {
            match self
                .finish_task(
                    &task_id,
                    result.clone(),
                    cancel.clone(),
                    &request.mode_label,
                )
                .await
            {
                Ok(()) => break,
                Err(err) => {
                    tracing::error!(
                        "Task {task_id} terminal receipt is pending storage recovery: {err}"
                    );
                    sleep(STORE_REFRESH_INTERVAL).await;
                }
            }
        }
    }

    async fn process_execution_event(
        &self,
        task_id: &str,
        event: TaskExecutionEvent,
        guard: &mut ExecutionGuard,
        accumulated_result_text: &mut String,
        dirty: &mut bool,
    ) -> Result<()> {
        if matches!(event, TaskExecutionEvent::ToolHeartbeat) {
            // Liveness is in-memory only. In particular, leave any real event's
            // pending persistence intact; the supervisor still checks execution
            // ownership and cancellation on its regular store-poll deadline.
            guard.note_progress(Instant::now());
            return Ok(());
        }
        match self.apply_execution_event(task_id, event.clone()).await {
            Ok(outcome) => {
                if execution_event_is_progress(&event) {
                    guard.note_progress(Instant::now());
                }
                append_message_delta(accumulated_result_text, &event);
                *dirty = !outcome.persisted;
                Ok(())
            }
            Err(err) => {
                tracing::error!("Task {task_id} event is waiting for storage recovery: {err}");
                Err(err)
            }
        }
    }

    async fn apply_execution_event(
        &self,
        task_id: &str,
        event: TaskExecutionEvent,
    ) -> Result<EventApplyOutcome> {
        let urgent = execution_event_persist_urgent(&event);
        let mut state = self.state.lock().await;
        let _transaction = self.lock_store().await?;
        self.refresh_locked_offloaded(&mut state).await?;
        if state
            .pending_events
            .get(task_id)
            .is_some_and(|events| events.len() >= TASK_EVENT_CHANNEL_CAPACITY)
        {
            self.persist_changed_task_locked(&mut state, task_id)?;
        }
        let task = state
            .tasks
            .get_mut(task_id)
            .context("Event task is missing")?;
        self.require_execution_owner(task)?;
        self.apply_event_to_task(task, event.clone())?;
        let pending = state.pending_events.entry(task_id.to_string()).or_default();
        pending.push(event);
        let persist_now = urgent || pending.len() >= TASK_EVENT_CHANNEL_CAPACITY;
        let persisted = if persist_now {
            match self.persist_changed_task_locked(&mut state, task_id) {
                Ok(()) => true,
                Err(error) => {
                    // This event is already retained in the bounded delta set.
                    // Return acceptance so the caller must not append it again.
                    if let Some(cancel) = state.running_cancel.get(task_id) {
                        cancel.cancel();
                    }
                    tracing::error!(%error, "Task event retained pending storage recovery; cancellation requested");
                    false
                }
            }
        } else {
            false
        };
        Ok(EventApplyOutcome { persisted })
    }

    fn apply_event_to_task(&self, task: &mut TaskRecord, event: TaskExecutionEvent) -> Result<()> {
        let task_id = task.id.clone();
        match event {
            TaskExecutionEvent::ThreadLinked { thread_id, turn_id } => {
                task.thread_id = Some(thread_id.clone());
                task.turn_id = Some(turn_id.clone());
                push_timeline_entry(
                    task,
                    TaskTimelineEntry {
                        timestamp: Utc::now(),
                        kind: "runtime_link".to_string(),
                        summary: format!("Linked runtime thread {thread_id} turn {turn_id}"),
                        detail_path: None,
                    },
                );
            }
            TaskExecutionEvent::Status { message } => {
                push_timeline_entry(
                    task,
                    TaskTimelineEntry {
                        timestamp: Utc::now(),
                        kind: "status".to_string(),
                        summary: summarize_text(&message, TIMELINE_SUMMARY_LIMIT),
                        detail_path: None,
                    },
                );
            }
            TaskExecutionEvent::MessageDelta { content } => {
                if !content.trim().is_empty() {
                    push_timeline_entry(
                        task,
                        TaskTimelineEntry {
                            timestamp: Utc::now(),
                            kind: "message".to_string(),
                            summary: summarize_text(&content, TIMELINE_SUMMARY_LIMIT),
                            detail_path: None,
                        },
                    );
                }
            }
            TaskExecutionEvent::ToolStarted { id, name, input } => {
                let input_summary = summarize_json(&input);
                task.tool_calls.push(TaskToolCallSummary {
                    id: id.clone(),
                    name: name.clone(),
                    status: TaskToolStatus::Running,
                    started_at: Utc::now(),
                    ended_at: None,
                    duration_ms: None,
                    input_summary: input_summary.clone(),
                    output_summary: None,
                    detail_path: None,
                    patch_ref: None,
                });
                let summary = input_summary
                    .map(|s| format!("{name} started ({s})"))
                    .unwrap_or_else(|| format!("{name} started"));
                push_timeline_entry(
                    task,
                    TaskTimelineEntry {
                        timestamp: Utc::now(),
                        kind: "tool_started".to_string(),
                        summary,
                        detail_path: None,
                    },
                );
            }
            TaskExecutionEvent::ToolProgress { id, output } => {
                push_timeline_entry(
                    task,
                    TaskTimelineEntry {
                        timestamp: Utc::now(),
                        kind: "tool_progress".to_string(),
                        summary: format!(
                            "{id}: {}",
                            summarize_text(&output, TIMELINE_SUMMARY_LIMIT.saturating_sub(8))
                        ),
                        detail_path: None,
                    },
                );
            }
            // Supervisor-side liveness only: recording it would put a
            // timeline entry behind every poll tick of a silent build.
            TaskExecutionEvent::ToolHeartbeat => {}
            TaskExecutionEvent::ToolCompleted {
                id,
                name,
                success,
                output,
                metadata,
            } => {
                let now = Utc::now();
                let detail_path = self.artifact_if_large(&task_id, &name, &output)?;
                let output_summary = summarize_text(&output, TIMELINE_SUMMARY_LIMIT);
                let patch_ref = if name == "apply_patch" {
                    detail_path.clone()
                } else {
                    None
                };

                if let Some(call) = task.tool_calls.iter_mut().find(|call| call.id == id) {
                    call.status = if success {
                        TaskToolStatus::Success
                    } else {
                        TaskToolStatus::Failed
                    };
                    call.ended_at = Some(now);
                    call.duration_ms = Some(duration_ms(call.started_at, now));
                    call.output_summary = Some(output_summary.clone());
                    call.detail_path = detail_path.clone();
                    call.patch_ref = patch_ref.clone();

                    if call.duration_ms.is_none()
                        && let Some(duration) = metadata
                            .as_ref()
                            .and_then(|m| m.get("duration_ms"))
                            .and_then(Value::as_u64)
                    {
                        call.duration_ms = Some(duration);
                    }
                }

                let status = if success { "success" } else { "failed" };
                push_timeline_entry(
                    task,
                    TaskTimelineEntry {
                        timestamp: now,
                        kind: "tool_completed".to_string(),
                        summary: format!("{name} {status}: {output_summary}"),
                        detail_path: detail_path.clone(),
                    },
                );
                if let Some(patch_ref) = patch_ref {
                    push_timeline_entry(
                        task,
                        TaskTimelineEntry {
                            timestamp: now,
                            kind: "patch_ref".to_string(),
                            summary: format!("Patch artifact: {}", patch_ref.display()),
                            detail_path: Some(patch_ref),
                        },
                    );
                }

                self.apply_task_update_metadata(task, metadata.as_ref())?;
            }
            TaskExecutionEvent::Error { message } => {
                push_timeline_entry(
                    task,
                    TaskTimelineEntry {
                        timestamp: Utc::now(),
                        kind: "error".to_string(),
                        summary: summarize_text(&message, TIMELINE_SUMMARY_LIMIT),
                        detail_path: None,
                    },
                );
            }
            TaskExecutionEvent::RuntimeEvent {
                seq,
                event,
                summary,
            } => {
                task.runtime_event_count = task.runtime_event_count.saturating_add(1);
                push_timeline_entry(
                    task,
                    TaskTimelineEntry {
                        timestamp: Utc::now(),
                        kind: "runtime_event".to_string(),
                        summary: format!("#{seq} {event}: {summary}"),
                        detail_path: None,
                    },
                );
            }
        }

        Ok(())
    }

    async fn flush_task(&self, task_id: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        let _transaction = self.lock_store().await?;
        self.refresh_locked_offloaded(&mut state).await?;
        let task = state
            .tasks
            .get(task_id)
            .context("Flushed task is missing")?;
        self.require_execution_owner(task)?;
        self.persist_changed_task_locked(&mut state, task_id)
    }

    async fn finish_task(
        &self,
        task_id: &str,
        mut result: TaskExecutionResult,
        cancel: CancellationToken,
        mode_label: &str,
    ) -> Result<()> {
        let mut state = self.state.lock().await;
        let _transaction = self.lock_store().await?;
        self.refresh_locked_offloaded(&mut state).await?;
        state.running_cancel.remove(task_id);
        let task = state
            .tasks
            .get_mut(task_id)
            .context("Finished task is missing")?;
        self.require_execution_owner(task)?;

        let now = Utc::now();
        if (cancel.is_cancelled() || task.cancel_requested_seq > 0)
            && result.status == TaskStatus::Completed
        {
            result.status = TaskStatus::Canceled;
            result.result_text = None;
            result.error = None;
            result.terminal_reason = TaskTerminalReason::Canceled;
        }
        if self.cancel_token.is_cancelled()
            && result.status != TaskStatus::Completed
            && matches!(
                result.terminal_reason,
                TaskTerminalReason::Canceled | TaskTerminalReason::Failed
            )
        {
            result.status = TaskStatus::Canceled;
            result.terminal_reason = TaskTerminalReason::Shutdown;
            result.error = Some(TaskTerminalReason::Shutdown.receipt_message());
        }

        task.status = result.status;
        task.lifecycle_seq = task.lifecycle_seq.saturating_add(1);
        task.mode = mode_label.to_string();
        task.ended_at = Some(now);
        task.duration_ms = task.started_at.map(|start| duration_ms(start, now));
        task.error = result.error.clone();
        task.terminal_reason = Some(result.terminal_reason.as_str().to_string());
        let finished_summary = if matches!(result.status, TaskStatus::Queued | TaskStatus::Running)
        {
            format!("Task ended in unexpected state: {mode_label}")
        } else {
            match result.terminal_reason {
                TaskTerminalReason::Completed
                | TaskTerminalReason::Canceled
                | TaskTerminalReason::Shutdown => result.terminal_reason.receipt_message(),
                TaskTerminalReason::Failed
                | TaskTerminalReason::WallTimeout
                | TaskTerminalReason::IdleTimeout
                | TaskTerminalReason::CancelTimeout => format!(
                    "{}: {}",
                    result.terminal_reason.as_str(),
                    result
                        .error
                        .as_deref()
                        .map(|e| summarize_text(e, TIMELINE_SUMMARY_LIMIT))
                        .unwrap_or_else(|| result.terminal_reason.receipt_message())
                ),
            }
        };
        push_timeline_entry(
            task,
            TaskTimelineEntry {
                timestamp: now,
                kind: "finished".to_string(),
                summary: finished_summary,
                detail_path: None,
            },
        );

        if let Some(text) = result.result_text {
            let detail_path = self.artifact_if_large(task_id, "result", &text)?;
            task.result_summary = Some(summarize_text(&text, TIMELINE_SUMMARY_LIMIT));
            task.result_detail_path = detail_path.clone();
            if let Some(detail_path) = detail_path {
                push_timeline_entry(
                    task,
                    TaskTimelineEntry {
                        timestamp: now,
                        kind: "result_ref".to_string(),
                        summary: format!("Result artifact: {}", detail_path.display()),
                        detail_path: Some(detail_path),
                    },
                );
            }
        } else if result.status == TaskStatus::Completed {
            task.result_summary = Some("(no textual output)".to_string());
        }

        self.persist_changed_task_locked(&mut state, task_id)?;
        Ok(())
    }

    fn artifact_if_large(
        &self,
        task_id: &str,
        label: &str,
        content: &str,
    ) -> Result<Option<PathBuf>> {
        if content.len() < ARTIFACT_THRESHOLD {
            return Ok(None);
        }
        self.write_artifact(task_id, label, content).map(Some)
    }

    fn write_artifact(&self, task_id: &str, label: &str, content: &str) -> Result<PathBuf> {
        ensure_safe_storage_id("task id", task_id)?;
        let artifact_dir = self.artifacts_dir.join(task_id);
        fs::create_dir_all(&artifact_dir)
            .with_context(|| format!("Failed to create artifact dir {}", artifact_dir.display()))?;
        let stamp = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        let filename = format!("{stamp}_{}.txt", sanitize_filename(label));
        let absolute = artifact_dir.join(filename);
        fs::write(&absolute, content)
            .with_context(|| format!("Failed to write artifact {}", absolute.display()))?;
        let relative = absolute
            .strip_prefix(&self.cfg.data_dir)
            .map(PathBuf::from)
            .unwrap_or(absolute);
        Ok(relative)
    }

    fn apply_task_update_metadata(
        &self,
        task: &mut TaskRecord,
        metadata: Option<&Value>,
    ) -> Result<()> {
        let Some(updates) = metadata.and_then(|m| m.get("task_updates")) else {
            return Ok(());
        };
        let now = Utc::now();

        if let Some(value) = updates.get("checklist") {
            let mut checklist: TaskChecklistState = serde_json::from_value(value.clone())
                .context("Failed to parse checklist task update")?;
            checklist.updated_at = checklist.updated_at.or(Some(now));
            task.checklist = checklist;
            push_timeline_entry(
                task,
                TaskTimelineEntry {
                    timestamp: now,
                    kind: "checklist".to_string(),
                    summary: format!(
                        "Checklist updated: {} item(s), {}% complete",
                        task.checklist.items.len(),
                        task.checklist.completion_pct
                    ),
                    detail_path: None,
                },
            );
        }

        if let Some(value) = updates.get("gate") {
            let gate: TaskGateRecord = serde_json::from_value(value.clone())
                .context("Failed to parse gate task update")?;
            let summary = format!("Gate {} {}: {}", gate.gate, gate.status, gate.summary);
            task.gates.retain(|existing| existing.id != gate.id);
            task.gates.push(gate.clone());
            push_timeline_entry(
                task,
                TaskTimelineEntry {
                    timestamp: now,
                    kind: "gate".to_string(),
                    summary: summarize_text(&summary, TIMELINE_SUMMARY_LIMIT),
                    detail_path: gate.log_path,
                },
            );
        }

        if let Some(value) = updates.get("attempt") {
            let attempt: TaskAttemptRecord = serde_json::from_value(value.clone())
                .context("Failed to parse attempt task update")?;
            task.attempts.retain(|existing| existing.id != attempt.id);
            task.attempts.push(attempt.clone());
            push_timeline_entry(
                task,
                TaskTimelineEntry {
                    timestamp: now,
                    kind: "pr_attempt".to_string(),
                    summary: format!(
                        "Attempt {}/{} recorded for {}",
                        attempt.attempt_index, attempt.attempt_count, attempt.attempt_group_id
                    ),
                    detail_path: attempt.patch_path,
                },
            );
        }

        if let Some(value) = updates.get("artifacts")
            && let Some(items) = value.as_array()
        {
            for item in items {
                let artifact: TaskArtifactRef = serde_json::from_value(item.clone())
                    .context("Failed to parse artifact task update")?;
                push_timeline_entry(
                    task,
                    TaskTimelineEntry {
                        timestamp: now,
                        kind: "artifact".to_string(),
                        summary: format!("{}: {}", artifact.label, artifact.summary),
                        detail_path: Some(artifact.path.clone()),
                    },
                );
                task.artifacts.push(artifact);
            }
        }

        if let Some(value) = updates.get("github_event") {
            let event: TaskGithubEvent = serde_json::from_value(value.clone())
                .context("Failed to parse GitHub task update")?;
            push_timeline_entry(
                task,
                TaskTimelineEntry {
                    timestamp: now,
                    kind: "github".to_string(),
                    summary: format!(
                        "{} {}#{}: {}",
                        event.action, event.target, event.number, event.summary
                    ),
                    detail_path: None,
                },
            );
            task.github_events.push(event);
        }

        Ok(())
    }

    /// Acquire the cross-process task-store lock.
    ///
    /// Polls with exponential backoff (5ms → 50ms) instead of a flat 5ms
    /// interval: under contention the old shape woke ~200×/s for up to its
    /// whole five-second deadline (#6211 R7c). The deadline and the busy
    /// error are unchanged. What this does not do: it does not add the
    /// in-process mutex the issue also suggested — in-process contenders
    /// just back off against the same file lock.
    async fn lock_store(&self) -> Result<RuntimeProcessOwnerLock> {
        let path = self.cfg.data_dir.join("task-store.lock");
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut wait = Duration::from_millis(5);
        loop {
            if let Some(mut owner) = RuntimeProcessOwnerLock::try_acquire_file(&path, true)? {
                // Lets a contender that times out name this process (#6573).
                owner.record_holder();
                return Ok(owner);
            }
            if Instant::now() >= deadline {
                return Err(store_busy_error(&path));
            }
            sleep(wait).await;
            wait = (wait * 2).min(Duration::from_millis(50));
        }
    }

    async fn refresh_locked_offloaded(&self, state: &mut ManagerState) -> Result<()> {
        // Callers go on to change `state`; a failed persist must not leave a
        // diverged view that a read-only caller would trust.
        state.read_snapshot = None;
        let (tasks_dir, queue_path) = (self.tasks_dir.clone(), self.queue_path.clone());
        let loaded = tokio::task::spawn_blocking(move || load_state(&tasks_dir, &queue_path))
            .await
            .context("Task store load was interrupted")??;
        self.apply_loaded_locked(state, loaded)
    }

    /// Bring `state` up to date for a read-only caller (#6573).
    ///
    /// Skips the cross-process lock and the full reload while the store is
    /// provably unchanged since the last read-only refresh. Several TUIs
    /// sharing one data dir each list tasks every 2.5s on their event loop;
    /// without this, every idle session took the store lock and re-parsed
    /// every task record on each poll, contending with the others.
    async fn refresh_for_read(&self, state: &mut ManagerState) -> Result<()> {
        if let Some(snapshot) = &state.read_snapshot
            && *snapshot == ReadSnapshot::read(&self.tasks_dir, &self.queue_path)
        {
            return Ok(());
        }
        let _transaction = self.lock_store().await?;
        // Taken under the lock and before the load, so any later write
        // changes it.
        let snapshot = ReadSnapshot::read(&self.tasks_dir, &self.queue_path);
        self.refresh_locked_offloaded(state).await?;
        if snapshot.settled(std::time::SystemTime::now()) {
            state.read_snapshot = Some(snapshot);
        }
        Ok(())
    }

    fn apply_loaded_locked(&self, state: &mut ManagerState, loaded: LoadedTaskState) -> Result<()> {
        #[cfg(any(test, feature = "test-support"))]
        self.store_loads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        state.tasks = loaded.tasks;
        state.queue = loaded.queue;
        for (id, events) in &state.pending_events {
            let task = state
                .tasks
                .get_mut(id)
                .context("Pending task disappeared")?;
            self.require_execution_owner(task)?;
            for event in events {
                self.apply_event_to_task(task, event.clone())?;
            }
        }
        Ok(())
    }

    fn require_execution_owner(&self, task: &TaskRecord) -> Result<()> {
        if task.execution_scope.as_deref() != Some(self.execution_scope())
            || task.execution_generation.as_deref() != Some(&self.execution_lease.generation)
            || task.status != TaskStatus::Running
        {
            bail!("Task execution ownership changed; refusing a stale write");
        }
        Ok(())
    }

    fn persist_changed_task_locked(&self, state: &mut ManagerState, id: &str) -> Result<()> {
        let task = state.tasks.get(id).context("Changed task is missing")?;
        self.persist_task_locked(task)?;
        state.pending_events.remove(id);
        Ok(())
    }

    fn recover_dead_executions_locked(&self, state: &mut ManagerState) -> Result<()> {
        for task in state.tasks.values_mut() {
            // Unknown legacy ownership is preserved, never guessed from the
            // visibility owner, model spelling, or current process defaults.
            if task.status != TaskStatus::Running {
                continue;
            }
            let (Some(scope), Some(generation)) =
                (&task.execution_scope, &task.execution_generation)
            else {
                continue;
            };
            let path = execution_lease_path(&self.cfg.data_dir, scope, generation)?;
            let Some(_dead_owner) = RuntimeProcessOwnerLock::try_acquire_file(&path, false)? else {
                continue;
            };
            let now = Utc::now();
            let duration_ms = task.started_at.and_then(|started| {
                u64::try_from(now.signed_duration_since(started).num_milliseconds()).ok()
            });
            task.status = TaskStatus::Failed;
            task.lifecycle_seq = task.lifecycle_seq.saturating_add(1);
            task.ended_at = Some(now);
            task.duration_ms = duration_ms;
            task.terminal_reason = Some(TaskTerminalReason::Failed.as_str().to_string());
            task.error =
                Some("Interrupted by process restart; prior process is not attached".to_string());
            for tool in &mut task.tool_calls {
                if tool.status == TaskToolStatus::Running {
                    tool.status = TaskToolStatus::Failed;
                    tool.ended_at = Some(now);
                    tool.duration_ms = duration_ms.or_else(|| {
                        u64::try_from(
                            now.signed_duration_since(tool.started_at)
                                .num_milliseconds(),
                        )
                        .ok()
                    });
                }
            }
            push_timeline_entry(
                task,
                TaskTimelineEntry {
                    timestamp: now,
                    kind: "recovered".to_string(),
                    summary: "Interrupted by process restart; prior process is not attached"
                        .to_string(),
                    detail_path: None,
                },
            );

            self.persist_task_locked(task)?;
        }
        Ok(())
    }

    fn persist_queue_locked(&self, queue: &VecDeque<String>) -> Result<()> {
        write_json_atomic(
            &self.queue_path,
            &QueueFile {
                queue: queue.iter().cloned().collect(),
            },
        )
    }

    fn persist_task_locked(&self, task: &TaskRecord) -> Result<()> {
        let path = self.tasks_dir.join(format!("{}.json", task.id));
        write_json_atomic(&path, task)
    }
}

fn validate_preallocated_task_id(task_id: &str) -> Result<()> {
    if task_id.len() != 21
        || !task_id.starts_with("task_")
        || !task_id[5..].chars().all(|ch| ch.is_ascii_hexdigit())
    {
        bail!("Invalid preallocated task id: expected task_<16hex>");
    }
    Ok(())
}

fn read_bound_task_file(path: &Path, task_id: &str) -> Result<Option<TaskRecord>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read bound task"),
    };
    let task: TaskRecord = serde_json::from_slice(&bytes).context("decode bound task")?;
    if task.id != task_id || task.schema_version > CURRENT_TASK_SCHEMA_VERSION {
        bail!("Bound task identity or schema does not match its durable admission");
    }
    Ok(Some(task))
}

pub fn validate_bound_task_request(task: &TaskRecord, request: &NewTaskRequest) -> Result<()> {
    if task.prompt != request.prompt.trim()
        || task.owner_session_id != request.owner_session_id
        || task.destination_thread_id != request.destination_thread_id
        || task.model_provider != request.model_provider
        || task.model_provider_id != request.model_provider_id
        || request
            .model
            .as_ref()
            .is_some_and(|value| value != &task.model)
        || request
            .workspace
            .as_ref()
            .is_some_and(|value| value != &task.workspace)
        || request
            .mode
            .as_ref()
            .is_some_and(|value| value != &task.mode)
        || request
            .allow_shell
            .is_some_and(|value| value != task.allow_shell)
        || request
            .trust_mode
            .is_some_and(|value| value != task.trust_mode)
        || request
            .permission_posture
            .as_deref()
            .is_some_and(|value| Some(value) != task.permission_posture.as_deref())
        || task.auto_approve != request.auto_approve.unwrap_or(false)
    {
        bail!("Task admission replay does not match the bound request");
    }
    Ok(())
}

/// A read-only inventory and reconstructed queue. Execution recovery is a
/// separate transaction requiring proof that a particular generation died.
struct LoadedTaskState {
    tasks: HashMap<String, TaskRecord>,
    queue: VecDeque<String>,
}

fn load_state(tasks_dir: &Path, queue_path: &Path) -> Result<LoadedTaskState> {
    let mut tasks = HashMap::new();
    if tasks_dir.exists() {
        for entry in fs::read_dir(tasks_dir)
            .with_context(|| format!("Failed to read tasks dir {}", tasks_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let content = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read task file {}", path.display()))?;
            let task: TaskRecord = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse task file {}", path.display()))?;
            if task.schema_version > CURRENT_TASK_SCHEMA_VERSION {
                bail!(
                    "Task schema v{} is newer than supported v{}",
                    task.schema_version,
                    CURRENT_TASK_SCHEMA_VERSION
                );
            }
            ensure_safe_storage_id("task id", &task.id)?;
            if path.file_stem().and_then(|stem| stem.to_str()) != Some(task.id.as_str()) {
                bail!("Task record identity differs from its path");
            }
            if let Some(scope) = &task.execution_scope {
                validate_execution_id(scope, 64)?;
            }
            if let Some(generation) = &task.execution_generation {
                validate_execution_id(generation, 32)?;
                if task.execution_scope.is_none() {
                    bail!("Task generation has no execution scope");
                }
            }
            tasks.insert(task.id.clone(), task);
        }
    }

    let mut queue = if queue_path.exists() {
        let content = fs::read_to_string(queue_path)
            .with_context(|| format!("Failed to read queue file {}", queue_path.display()))?;
        let parsed: QueueFile = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse queue file {}", queue_path.display()))?;
        VecDeque::from(parsed.queue)
    } else {
        VecDeque::new()
    };

    queue.retain(|id| {
        tasks
            .get(id)
            .is_some_and(|task| task.status == TaskStatus::Queued)
    });

    let known = queue.iter().cloned().collect::<HashSet<_>>();
    let mut missing = tasks
        .values()
        .filter(|task| task.status == TaskStatus::Queued && !known.contains(&task.id))
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    missing.sort();
    for id in missing {
        queue.push_back(id);
    }

    Ok(LoadedTaskState { tasks, queue })
}

struct EventApplyOutcome {
    persisted: bool,
}

fn execution_event_is_progress(event: &TaskExecutionEvent) -> bool {
    matches!(
        event,
        TaskExecutionEvent::MessageDelta { .. }
            | TaskExecutionEvent::ToolStarted { .. }
            | TaskExecutionEvent::ToolProgress { .. }
            | TaskExecutionEvent::ToolHeartbeat
            | TaskExecutionEvent::ToolCompleted { .. }
    )
}

fn execution_event_persist_urgent(event: &TaskExecutionEvent) -> bool {
    !matches!(
        event,
        TaskExecutionEvent::MessageDelta { .. }
            | TaskExecutionEvent::ToolProgress { .. }
            | TaskExecutionEvent::RuntimeEvent { .. }
            // Liveness-only signal (see the variant doc): it arrives up to
            // ~5x/s throughout a silent build, and persisting it would
            // rewrite the whole task record on every tick while holding the
            // manager-wide state lock.
            | TaskExecutionEvent::ToolHeartbeat
    )
}

fn timeline_kinds_coalesce(left: &str, right: &str) -> bool {
    matches!(
        (left, right),
        ("message", "message")
            | ("tool_progress", "tool_progress")
            | ("runtime_event", "runtime_event")
    )
}

fn push_timeline_entry(task: &mut TaskRecord, entry: TaskTimelineEntry) {
    if let Some(last) = task.timeline.last_mut()
        && timeline_kinds_coalesce(last.kind.as_str(), entry.kind.as_str())
    {
        last.timestamp = entry.timestamp;
        last.summary = entry.summary;
        last.detail_path = entry.detail_path;
        return;
    }
    task.timeline.push(entry);
    trim_task_timeline(&mut task.timeline);
}

fn trim_task_timeline(entries: &mut Vec<TaskTimelineEntry>) {
    if entries.len() <= TIMELINE_ENTRY_LIMIT {
        return;
    }
    let overflow = entries.len() - TIMELINE_ENTRY_LIMIT;
    let start = TIMELINE_HEAD_KEEP.min(entries.len().saturating_sub(overflow + 1));
    let end = start + overflow;
    if start >= end || end > entries.len() {
        entries.truncate(TIMELINE_ENTRY_LIMIT);
        return;
    }
    entries.drain(start..end);
    let omitted = TaskTimelineEntry {
        timestamp: Utc::now(),
        kind: "omitted".to_string(),
        summary: format!("{overflow} earlier events omitted to bound storage"),
        detail_path: None,
    };
    if entries
        .get(start)
        .is_none_or(|entry| entry.kind != "omitted")
    {
        entries.insert(start, omitted);
    }
    if entries.len() > TIMELINE_ENTRY_LIMIT {
        let extra = entries.len() - TIMELINE_ENTRY_LIMIT;
        let drop_at = (start + 1).min(entries.len().saturating_sub(1));
        let drop_end = (drop_at + extra).min(entries.len());
        if drop_at < drop_end {
            entries.drain(drop_at..drop_end);
        } else {
            entries.truncate(TIMELINE_ENTRY_LIMIT);
        }
    }
}

fn resolve_task_id_visible_to(
    tasks: &HashMap<String, TaskRecord>,
    id_or_prefix: &str,
    owner_session_id: Option<&str>,
) -> Result<String> {
    let visible = |record: &TaskRecord| {
        owner_session_id.is_none_or(|owner_session_id| {
            record.owner_session_id.as_deref() == Some(owner_session_id)
        })
    };
    if tasks.get(id_or_prefix).is_some_and(visible) {
        return Ok(id_or_prefix.to_string());
    }
    let matches = tasks
        .iter()
        .filter(|(id, record)| id.starts_with(id_or_prefix) && visible(record))
        .map(|(id, _)| id)
        .cloned()
        .collect::<Vec<_>>();
    match matches.len() {
        0 => bail!("Task not found: {id_or_prefix}"),
        1 => Ok(matches[0].clone()),
        _ => bail!(
            "Ambiguous task prefix '{}': matches {} tasks",
            id_or_prefix,
            matches.len()
        ),
    }
}

fn resolve_task_id_visible_to_operator(
    tasks: &HashMap<String, TaskRecord>,
    id_or_prefix: &str,
    owner_session_id: &str,
    execution_scope: &str,
) -> Result<String> {
    let visible = |record: &TaskRecord| {
        record.owner_session_id.as_deref() == Some(owner_session_id)
            || record.owner_session_id.is_none()
                && !execution_scope.is_empty()
                && record.execution_scope.as_deref() == Some(execution_scope)
    };
    if tasks.get(id_or_prefix).is_some_and(visible) {
        return Ok(id_or_prefix.to_string());
    }
    let matches = tasks
        .iter()
        .filter(|(id, record)| id.starts_with(id_or_prefix) && visible(record))
        .map(|(id, _)| id)
        .cloned()
        .collect::<Vec<_>>();
    match matches.len() {
        0 => bail!("Task not found: {id_or_prefix}"),
        1 => Ok(matches[0].clone()),
        _ => bail!(
            "Ambiguous task prefix '{}': matches {} tasks",
            id_or_prefix,
            matches.len()
        ),
    }
}

fn resolve_task_id(tasks: &HashMap<String, TaskRecord>, id_or_prefix: &str) -> Result<String> {
    resolve_task_id_visible_to(tasks, id_or_prefix, None)
}

fn summarize_json(value: &Value) -> Option<String> {
    let text = serde_json::to_string(value).ok()?;
    Some(summarize_text(&text, TIMELINE_SUMMARY_LIMIT))
}

fn summarize_text(text: &str, limit: usize) -> String {
    let take = limit.saturating_sub(3);
    let mut count = 0;
    let mut out = String::new();
    for ch in text.chars() {
        if count >= take {
            out.push_str("...");
            return out;
        }
        if ch.is_control() && ch != '\n' && ch != '\t' {
            continue;
        }
        out.push(ch);
        count += 1;
    }
    out
}

fn ensure_safe_storage_id(kind: &str, value: &str) -> Result<()> {
    let mut components = Path::new(value).components();
    let Some(component) = components.next() else {
        bail!("{kind} must not be empty");
    };
    if components.next().is_some() || !matches!(component, std::path::Component::Normal(_)) {
        bail!("{kind} must be a single path component");
    }
    Ok(())
}

fn sanitize_filename(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "artifact".to_string()
    } else {
        out
    }
}

fn duration_ms(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    let millis = (end - start).num_milliseconds();
    if millis.is_negative() {
        0
    } else {
        u64::try_from(millis).unwrap_or(u64::MAX)
    }
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    let payload = serde_json::to_string_pretty(value)?;
    codewhale_util::utils::write_atomic(path, payload.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Default task manager data location (`~/.codewhale/tasks`, or legacy
/// `~/.deepseek/tasks` when only the legacy directory exists).
#[must_use]
pub fn default_tasks_dir() -> PathBuf {
    for var in ["CODEWHALE_TASKS_DIR", "DEEPSEEK_TASKS_DIR"] {
        if let Ok(path) = std::env::var(var)
            && !path.trim().is_empty()
        {
            return PathBuf::from(path);
        }
    }
    if let Some(home) = codewhale_paths::codewhale_home_override().ok().flatten() {
        return home.join("tasks");
    }
    codewhale_paths::user_home()
        .map(|home| default_tasks_dir_for_home(&home))
        .unwrap_or_else(|| PathBuf::from(".codewhale").join("tasks"))
}

fn default_tasks_dir_for_home(home: &Path) -> PathBuf {
    let primary = home.join(".codewhale").join("tasks");
    if primary.is_dir() {
        return primary;
    }
    let legacy = home.join(".deepseek").join("tasks");
    if legacy.is_dir() {
        return legacy;
    }
    primary
}

/// Wait for a task to reach a terminal status (tests and API helpers).
#[cfg(any(test, feature = "test-support"))]
pub async fn wait_for_terminal_state(
    manager: &TaskManager,
    task_id: &str,
    timeout: StdDuration,
) -> Result<TaskRecord> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let task = manager.get_task(task_id).await?;
        if task.status.is_terminal() {
            return Ok(task);
        }
        if std::time::Instant::now() >= deadline {
            bail!("Timed out waiting for task {task_id}");
        }
        sleep(StdDuration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, lock_test_env};
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::Duration;

    struct MockExecutor;

    /// Poll until the task is claimed as `Running`, or fail at `timeout`.
    ///
    /// A worker claims the task and installs its cancel token under one state
    /// lock, so observing `Running` means a cancel or shutdown now reaches a
    /// live executor rather than a still-queued record.
    async fn wait_for_running(
        manager: &TaskManager,
        task_id: &str,
        timeout: Duration,
    ) -> Result<TaskRecord> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let task = manager.get_task(task_id).await?;
            if task.status == TaskStatus::Running {
                return Ok(task);
            }
            if task.status.is_terminal() || std::time::Instant::now() >= deadline {
                bail!("task {task_id} never started running: {task:?}");
            }
            sleep(Duration::from_millis(5)).await;
        }
    }

    fn provider_default_model_cases() -> Vec<(&'static str, Config, &'static str)> {
        let deepseek = {
            let __cw_config_init_0_0 = Some("deepseek".to_string());
            let __cw_config_init_0_1 = Some("deepseek-v4-flash".to_string());
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.default_text_model = __cw_config_init_0_1;
            __cw_config_init_0_value
        };

        let zai = {
            let __cw_config_init_1_0 = Some("zai".to_string());
            // Exercise provider-aware rejection of a stale DeepSeek root default.
            let __cw_config_init_1_1 =
                Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.default_text_model = __cw_config_init_1_1;
            __cw_config_init_1_value
        };

        let mut custom_providers = codewhale_runtime::config::ProvidersConfig::default();
        custom_providers.custom.insert(
            "acme".to_string(),
            codewhale_runtime::config::ProviderConfig {
                base_url: Some("http://127.0.0.1:1/v1".to_string()),
                model: Some("acme-coder".to_string()),
                kind: Some("openai-compatible".to_string()),
                ..codewhale_runtime::config::ProviderConfig::default()
            },
        );
        let custom = {
            let __cw_config_init_2_0 = Some("acme".to_string());
            let __cw_config_init_2_1 = Some(custom_providers);
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        };

        vec![
            ("deepseek", deepseek, "deepseek-v4-flash"),
            (
                "zai",
                zai,
                codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL,
            ),
            ("custom", custom, "acme-coder"),
        ]
    }

    #[test]
    fn task_manager_config_uses_the_active_provider_default() {
        for (label, config, expected) in provider_default_model_cases() {
            let task_config =
                TaskManagerConfig::from_runtime(&config, PathBuf::from("."), None, Some(1));
            assert_eq!(
                task_config.default_model, expected,
                "{label} durable task default"
            );
        }
    }

    #[async_trait]
    impl TaskExecutor for MockExecutor {
        async fn execute(
            &self,
            task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            cancel: CancellationToken,
        ) -> TaskExecutionResult {
            let _ = events
                .send(TaskExecutionEvent::Status {
                    message: format!("running {}", task.id),
                })
                .await;
            let _ = events
                .send(TaskExecutionEvent::ThreadLinked {
                    thread_id: "thr_test".to_string(),
                    turn_id: "turn_test".to_string(),
                })
                .await;
            let _ = events
                .send(TaskExecutionEvent::ToolStarted {
                    id: "tool_1".to_string(),
                    name: "read_file".to_string(),
                    input: serde_json::json!({ "path": "README.md" }),
                })
                .await;
            sleep(Duration::from_millis(50)).await;
            if cancel.is_cancelled() {
                return TaskExecutionResult {
                    status: TaskStatus::Canceled,
                    result_text: None,
                    error: None,
                    terminal_reason: TaskTerminalReason::Canceled,
                };
            }
            let _ = events
                .send(TaskExecutionEvent::ToolCompleted {
                    id: "tool_1".to_string(),
                    name: "read_file".to_string(),
                    success: true,
                    output: "read ok".to_string(),
                    metadata: Some(serde_json::json!({
                        "duration_ms": 10,
                        "task_updates": {
                            "checklist": {
                                "items": [
                                    { "id": 1, "content": "read fixture", "status": "in_progress" }
                                ],
                                "completion_pct": 0,
                                "in_progress_id": 1,
                                "updated_at": null
                            }
                        }
                    })),
                })
                .await;
            TaskExecutionResult {
                status: TaskStatus::Completed,
                result_text: Some("done".to_string()),
                error: None,
                terminal_reason: TaskTerminalReason::Completed,
            }
        }
    }

    fn test_config(root: PathBuf) -> TaskManagerConfig {
        TaskManagerConfig {
            default_workspace: root.join("workspace"),
            data_dir: root,
            worker_count: 1,
            default_model: "deepseek-v4-flash".to_string(),
            default_mode: "agent".to_string(),
            allow_shell: false,
            trust_mode: false,
            execution_limits: TaskExecutionLimits::default(),
        }
    }

    fn short_test_config(root: PathBuf) -> TaskManagerConfig {
        TaskManagerConfig {
            execution_limits: TaskExecutionLimits::short_for_tests(),
            ..test_config(root)
        }
    }

    fn wall_timeout_test_config(root: PathBuf) -> TaskManagerConfig {
        let mut config = short_test_config(root);
        config.execution_limits.idle_progress = config
            .execution_limits
            .wall_time
            .saturating_add(config.execution_limits.cancel_grace);
        config
    }

    /// The fixture owns the queue timestamp; model an already settled file
    /// directly instead of sleeping for the filesystem's coarse-mtime window.
    async fn settle_queue_fixture(manager: &TaskManager) -> Result<()> {
        let _transaction = manager.lock_store().await?;
        fs::OpenOptions::new()
            .write(true)
            .open(&manager.queue_path)?
            .set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(946_684_800))?;
        Ok(())
    }

    async fn inspect_settled_workers(manager: &TaskManager) -> Result<()> {
        // This deadline only detects a stuck worker. Success requires a reply
        // from every worker, including the result of its scheduling decision.
        let schedules =
            tokio::time::timeout(Duration::from_secs(10), manager.inspect_workers()).await??;
        assert_eq!(
            schedules.len(),
            manager.cfg.worker_count.clamp(1, MAX_WORKERS)
        );
        for schedule in schedules {
            assert!(schedule.seen.is_some(), "worker must finish an empty claim");
            assert!(
                schedule.next_claim.is_none(),
                "settled queue must not schedule a reload"
            );
        }
        Ok(())
    }

    /// Re-enter the constructor's worker-readiness boundary after the initial
    /// state load, without racing the initial transaction or a wall-clock sleep.
    async fn stop_idle_workers_for_initialization_fixture(manager: &TaskManager) {
        let workers = std::mem::take(&mut *manager.workers.lock().await);
        for worker in workers {
            worker.join.abort();
            let _ = worker.join.await;
        }
        assert!(!manager.cancel_token.is_cancelled());
    }

    #[tokio::test]
    async fn initial_worker_contention_acknowledges_retry_and_recovers() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (started, mut executions) = mpsc::unbounded_channel();
        let manager = TaskManager::start_with_executor(
            test_config(root.path().to_path_buf()),
            Arc::new(ControlledExecutor { started }),
        )
        .await?;
        stop_idle_workers_for_initialization_fixture(&manager).await;
        let held =
            RuntimeProcessOwnerLock::try_acquire_file(&root.path().join("task-store.lock"), true)?
                .context("fixture must own the task store before the first worker claim")?;
        let schedules =
            tokio::time::timeout(Duration::from_secs(10), manager.initialize_workers(1)).await??;
        assert_eq!(schedules.len(), 1);
        assert!(
            schedules[0].next_claim.is_some(),
            "contention must schedule a retry"
        );
        assert!(
            !manager.cancel_token.is_cancelled(),
            "startup must retain the retrying worker"
        );
        drop(held);

        let task = manager
            .add_task(NewTaskRequest::from_prompt(
                "recover after startup contention",
            ))
            .await?;
        let (id, finish) = tokio::time::timeout(Duration::from_secs(10), executions.recv())
            .await?
            .context("retrying worker must execute work after the lock is released")?;
        assert_eq!(id, task.id);
        finish
            .send(())
            .map_err(|_| anyhow!("executor stopped before release"))?;
        tokio::time::timeout(Duration::from_secs(10), manager.inspect_workers()).await??;
        assert_eq!(
            manager.get_task(&task.id).await?.status,
            TaskStatus::Completed
        );
        manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test]
    async fn initial_worker_corrupt_store_is_not_acknowledged_as_ready() -> Result<()> {
        let root = tempfile::tempdir()?;
        let manager = TaskManager::start_with_executor(
            test_config(root.path().to_path_buf()),
            Arc::new(MockExecutor),
        )
        .await?;
        stop_idle_workers_for_initialization_fixture(&manager).await;
        {
            let _transaction = manager.lock_store().await?;
            fs::write(&manager.queue_path, b"invalid queue JSON")?;
        }
        let result =
            tokio::time::timeout(Duration::from_secs(10), manager.initialize_workers(1)).await?;
        let error = result
            .err()
            .context("a corrupt first claim must fail startup")?;
        assert!(
            !error.is::<TaskStoreBusy>(),
            "corruption must never be retry readiness"
        );
        assert!(format!("{error:#}").contains("queue"));
        assert!(manager.cancel_token.is_cancelled());
        manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test]
    async fn inspection_replies_cover_every_worker_and_survive_caller_cancellation() -> Result<()> {
        let root = tempfile::tempdir()?;
        let manager = TaskManager::start_with_executor(
            TaskManagerConfig {
                worker_count: 3,
                ..test_config(root.path().to_path_buf())
            },
            Arc::new(MockExecutor),
        )
        .await?;
        // One constructor load and an acknowledged initial claim per worker.
        // A reply from one fast worker cannot stand in for the other two.
        assert!(manager.store_loads.load(Ordering::Relaxed) >= 4);
        settle_queue_fixture(&manager).await?;
        inspect_settled_workers(&manager).await?;

        let sender = manager.workers.lock().await[0].inspect.clone();
        let (reply, received) = oneshot::channel();
        sender.send(reply).await?;
        drop(received);
        // The abandoned reply must not stop the worker or strand the next
        // caller. FIFO delivery makes this receipt follow the abandoned one.
        inspect_settled_workers(&manager).await?;
        manager.shutdown_and_wait().await?;
        assert!(manager.inspect_workers().await.is_err());
        let (reply, received) = oneshot::channel();
        assert!(sender.send(reply).await.is_err());
        assert!(received.await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn inspection_never_reports_a_failed_store_as_settled_and_can_recover() -> Result<()> {
        let root = tempfile::tempdir()?;
        let manager = TaskManager::start_with_executor(
            test_config(root.path().to_path_buf()),
            Arc::new(MockExecutor),
        )
        .await?;
        let queue = fs::read(&manager.queue_path)?;
        {
            let _transaction = manager.lock_store().await?;
            fs::write(&manager.queue_path, b"invalid queue JSON")?;
        }
        let inspected =
            tokio::time::timeout(Duration::from_secs(10), manager.inspect_workers()).await?;
        // If the normal timer already observed the failure, this inspection
        // reports its retry state. Otherwise the failed claim replies Err.
        // Neither outcome is an acknowledgement of settled storage.
        match inspected {
            Err(error) => assert!(format!("{error:#}").contains("queue")),
            Ok(schedules) => {
                assert_eq!(schedules.len(), 1);
                assert!(schedules[0].next_claim.is_some());
            }
        }
        {
            let _transaction = manager.lock_store().await?;
            fs::write(&manager.queue_path, queue)?;
        }
        settle_queue_fixture(&manager).await?;
        inspect_settled_workers(&manager).await?;
        manager.shutdown_and_wait().await?;
        Ok(())
    }

    /// Execution starts and ends at messages controlled by the caller, so
    /// worker-boundary tests never infer either transition from elapsed time.
    struct ControlledExecutor {
        started: mpsc::UnboundedSender<(String, oneshot::Sender<()>)>,
    }

    #[async_trait]
    impl TaskExecutor for ControlledExecutor {
        async fn execute(
            &self,
            task: ExecutionTask,
            _events: mpsc::Sender<TaskExecutionEvent>,
            cancel: CancellationToken,
        ) -> TaskExecutionResult {
            let (finish, finished) = oneshot::channel();
            if self.started.send((task.id, finish)).is_err() {
                return TaskExecutionResult::from_reason(TaskTerminalReason::Canceled, None);
            }
            let reason = tokio::select! {
                result = finished => if result.is_ok() {
                    TaskTerminalReason::Completed
                } else {
                    TaskTerminalReason::Canceled
                },
                _ = cancel.cancelled() => TaskTerminalReason::Canceled,
            };
            TaskExecutionResult::from_reason(reason, None)
        }
    }

    /// #6573: idle managers sharing one data dir must not reload the store
    /// (and take its cross-process lock) every 200ms per worker.
    #[tokio::test]
    async fn idle_managers_sharing_a_store_do_not_poll_it_continuously() -> Result<()> {
        let root = tempfile::tempdir()?;
        let config = || TaskManagerConfig {
            worker_count: 2,
            ..test_config(root.path().to_path_buf())
        };
        let (started, mut executions) = mpsc::unbounded_channel();
        let first = TaskManager::start_with_executor_in_scope(
            config(),
            Arc::new(ControlledExecutor { started }),
            "first",
        )
        .await?;
        let second =
            TaskManager::start_with_executor_in_scope(config(), Arc::new(MockExecutor), "second")
                .await?;
        settle_queue_fixture(&first).await?;
        for manager in [&first, &second] {
            inspect_settled_workers(manager).await?;
            manager.store_loads.store(0, Ordering::Relaxed);
        }

        // Each reply proves the real worker evaluated the unchanged queue.
        // The schedule assertion also rejects a future fallback reload now,
        // without hoping a two-second observation window catches it firing.
        for _ in 0..10 {
            inspect_settled_workers(&first).await?;
            inspect_settled_workers(&second).await?;
        }
        let loads =
            first.store_loads.load(Ordering::Relaxed) + second.store_loads.load(Ordering::Relaxed);
        assert_eq!(
            loads, 0,
            "acknowledged idle inspections must not reload the store"
        );

        // No inspection request triggers this claim: admission's normal Notify
        // must wake a worker. Its started message establishes that boundary.
        let task = first
            .add_task(NewTaskRequest::from_prompt("wake an idle worker"))
            .await?;
        let (id, finish) = tokio::time::timeout(Duration::from_secs(10), executions.recv())
            .await?
            .context("executor stopped before acknowledging start")?;
        assert_eq!(id, task.id);
        finish
            .send(())
            .map_err(|_| anyhow!("executor stopped before release"))?;
        // The busy worker handles this request only after persisting its task's
        // terminal result. A second worker's reply alone cannot satisfy it.
        tokio::time::timeout(Duration::from_secs(10), first.inspect_workers()).await??;
        assert_eq!(
            first.get_task(&task.id).await?.status,
            TaskStatus::Completed
        );

        first.shutdown_and_wait().await?;
        second.shutdown_and_wait().await?;
        Ok(())
    }

    /// #6573: the TUI task panel lists tasks every 2.5s. An unchanged store
    /// must be answered from memory, without the cross-process lock or a
    /// reload, while a write from another process is still seen.
    #[tokio::test]
    async fn repeated_listing_of_an_unchanged_store_does_not_reload_it() -> Result<()> {
        let root = tempfile::tempdir()?;
        let lister = TaskManager::start_with_executor_in_scope(
            test_config(root.path().to_path_buf()),
            Arc::new(MockExecutor),
            "lister",
        )
        .await?;
        let writer = TaskManager::start_with_executor_in_scope(
            test_config(root.path().to_path_buf()),
            Arc::new(MockExecutor),
            "writer",
        )
        .await?;
        let first = writer
            .add_task(NewTaskRequest::from_prompt("first"))
            .await?;
        wait_for_terminal_state(&writer, &first.id, Duration::from_secs(2)).await?;
        // Let the store's mtimes settle so a snapshot can be trusted.
        sleep(READ_SNAPSHOT_SETTLE + Duration::from_millis(300)).await;
        assert_eq!(lister.list_tasks(None).await?.len(), 1);

        lister.store_loads.store(0, Ordering::Relaxed);
        for _ in 0..20 {
            assert_eq!(lister.list_tasks(None).await?.len(), 1);
            assert_eq!(lister.counts().await?.completed, 1);
        }
        let loads = lister.store_loads.load(Ordering::Relaxed);
        // Only the idle worker's fallback poll may reload in this window.
        assert!(
            loads <= 1,
            "40 read-only calls on an unchanged store reloaded it {loads} times"
        );

        let second = writer
            .add_task(NewTaskRequest::from_prompt("second"))
            .await?;
        assert!(
            lister
                .list_tasks(None)
                .await?
                .iter()
                .any(|task| task.id == second.id),
            "a write from another process must invalidate the read snapshot"
        );

        lister.shutdown_and_wait().await?;
        writer.shutdown_and_wait().await?;
        Ok(())
    }

    /// #6573: a task running in one process rewrites its record on every
    /// persisted event. Those writes must not make idle workers in another
    /// process reload the shared store on every tick.
    #[tokio::test]
    async fn running_task_flushes_do_not_wake_idle_workers_elsewhere() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (started, mut executions) = mpsc::unbounded_channel();
        let busy = TaskManager::start_with_executor_in_scope(
            test_config(root.path().to_path_buf()),
            Arc::new(ControlledExecutor { started }),
            "busy",
        )
        .await?;
        let task = busy
            .add_task(NewTaskRequest::from_prompt("stream status"))
            .await?;
        let (id, finish) = tokio::time::timeout(Duration::from_secs(10), executions.recv())
            .await?
            .context("executor stopped before acknowledging start")?;
        assert_eq!(id, task.id);

        let idle = TaskManager::start_with_executor_in_scope(
            TaskManagerConfig {
                worker_count: 2,
                ..test_config(root.path().to_path_buf())
            },
            Arc::new(MockExecutor),
            "idle",
        )
        .await?;
        settle_queue_fixture(&idle).await?;
        inspect_settled_workers(&idle).await?;
        idle.store_loads.store(0, Ordering::Relaxed);

        for chunk in 0..10 {
            // Await the production persistence operation, then acknowledge
            // every idle worker's inspection of the resulting store. Sending
            // an executor event alone would not prove its write had finished.
            let outcome = busy
                .apply_execution_event(
                    &task.id,
                    TaskExecutionEvent::Status {
                        message: format!("step {chunk}"),
                    },
                )
                .await?;
            assert!(outcome.persisted);
            inspect_settled_workers(&idle).await?;
        }
        let persisted = busy.read_bound_task(&task.id)?.context("persisted task")?;
        assert_eq!(persisted.status, TaskStatus::Running);
        assert_eq!(persisted.timeline.last().unwrap().summary, "step 9");
        assert_eq!(
            idle.store_loads.load(Ordering::Relaxed),
            0,
            "acknowledged task-record writes must not cause idle queue reloads"
        );

        finish
            .send(())
            .map_err(|_| anyhow!("executor stopped before release"))?;
        tokio::time::timeout(Duration::from_secs(10), busy.inspect_workers()).await??;
        assert_eq!(busy.get_task(&task.id).await?.status, TaskStatus::Completed);
        busy.shutdown_and_wait().await?;
        idle.shutdown_and_wait().await?;
        Ok(())
    }

    #[test]
    fn claim_schedule_backs_off_after_failures_and_retries_on_queue_change() {
        let fingerprint = |len| StoreFingerprint(Some((std::time::SystemTime::UNIX_EPOCH, len, 1)));
        let start = Instant::now();
        let mut schedule = ClaimSchedule::new(start);
        assert!(schedule.should_claim(&fingerprint(1), start, false));

        // Consecutive failures double the retry delay up to the ceiling.
        let mut now = start;
        let mut delays = Vec::new();
        for _ in 0..8 {
            let delay = schedule.claim_failed(fingerprint(1), now);
            delays.push(delay.as_millis());
            assert!(!schedule.should_claim(&fingerprint(1), now + delay / 2, false));
            now += delay;
            assert!(schedule.should_claim(&fingerprint(1), now, false));
        }
        assert_eq!(delays, [200, 400, 800, 1600, 3200, 6400, 8000, 8000]);

        // During a backoff, a queue change or an in-process wakeup retries
        // at once.
        let delay = schedule.claim_failed(fingerprint(1), now);
        assert_eq!(delay, STORE_BUSY_BACKOFF_MAX);
        assert!(schedule.should_claim(&fingerprint(2), now, false));
        assert!(schedule.should_claim(&fingerprint(1), now, true));

        // An empty claim on a settled queue resets backoff without scheduling
        // another scan, however long the worker stays idle.
        schedule.found_nothing(
            fingerprint(2),
            now,
            std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(60),
        );
        assert!(!schedule.should_claim(&fingerprint(2), now, false));
        assert!(!schedule.should_claim(&fingerprint(2), now + Duration::from_secs(86_400), false));
        assert_eq!(
            schedule.claim_failed(fingerprint(2), now),
            STORE_REFRESH_INTERVAL
        );
    }

    #[test]
    fn claim_schedule_retries_unknown_or_recent_queue_metadata() {
        let now = Instant::now();
        let wall_now = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(60);
        for fingerprint in [
            StoreFingerprint(None),
            StoreFingerprint(Some((wall_now, 1, 1))),
        ] {
            let mut schedule = ClaimSchedule::new(now);
            schedule.found_nothing(fingerprint.clone(), now, wall_now);
            assert!(!schedule.should_claim(&fingerprint, now, false));
            assert!(schedule.should_claim(&fingerprint, now + STORE_IDLE_POLL_INTERVAL, false));
        }
    }

    #[tokio::test]
    async fn settled_idle_workers_notice_external_queue_writes_without_notify() -> Result<()> {
        let root = tempfile::tempdir()?;
        let (started, mut executions) = mpsc::unbounded_channel();
        let manager = TaskManager::start_with_executor(
            TaskManagerConfig {
                worker_count: 2,
                ..test_config(root.path().to_path_buf())
            },
            Arc::new(ControlledExecutor { started }),
        )
        .await?;
        settle_queue_fixture(&manager).await?;
        inspect_settled_workers(&manager).await?;

        let mut task = sample_task_record();
        task.status = TaskStatus::Queued;
        task.started_at = None;
        let mut second = task.clone();
        second.id = "task_0123456789abcdee".into();
        {
            // Simulate another process's durable admission, under the real
            // cross-process transaction lock. No local state or Notify changes.
            let _transaction = manager.lock_store().await?;
            manager.persist_task_locked(&task)?;
            manager.persist_task_locked(&second)?;
            manager.persist_queue_locked(&VecDeque::from([task.id.clone(), second.id.clone()]))?;
        }
        let mut started_ids = Vec::new();
        for _ in 0..2 {
            // No local Notify or inspection request drives these claims. Wait
            // for executor messages proving the normal external-write path ran.
            let (id, finish) = tokio::time::timeout(Duration::from_secs(10), executions.recv())
                .await?
                .context("external task was not acknowledged")?;
            started_ids.push(id);
            finish
                .send(())
                .map_err(|_| anyhow!("executor stopped before release"))?;
        }
        started_ids.sort();
        let mut expected_ids = vec![task.id.clone(), second.id.clone()];
        expected_ids.sort();
        assert_eq!(started_ids, expected_ids);
        tokio::time::timeout(Duration::from_secs(10), manager.inspect_workers()).await??;
        for id in [&task.id, &second.id] {
            assert_eq!(manager.get_task(id).await?.status, TaskStatus::Completed);
        }
        manager.shutdown_and_wait().await?;
        Ok(())
    }

    /// #6728: with nothing claimable, each worker rechecks the queue about once
    /// per STORE_SETTLED_NAP rather than every STORE_REFRESH_INTERVAL. The
    /// bound is an upper limit, so a slow machine only lowers the count.
    #[tokio::test]
    async fn settled_idle_workers_check_the_queue_at_the_settled_nap() -> Result<()> {
        let root = tempfile::tempdir()?;
        let workers = 2;
        let manager = TaskManager::start_with_executor(
            TaskManagerConfig {
                worker_count: workers,
                ..test_config(root.path().to_path_buf())
            },
            Arc::new(MockExecutor),
        )
        .await?;
        sleep(STORE_IDLE_POLL_INTERVAL + Duration::from_millis(300)).await;
        manager.fingerprint_reads.store(0, Ordering::Relaxed);
        manager.store_loads.store(0, Ordering::Relaxed);

        let window = Duration::from_secs(3);
        sleep(window).await;
        let reads = manager.fingerprint_reads.load(Ordering::Relaxed);
        let loads = manager.store_loads.load(Ordering::Relaxed);
        let ticks = (window.as_millis() / STORE_SETTLED_NAP.as_millis()) as usize + 1;
        assert!(
            reads <= workers * ticks,
            "{workers} idle workers read the queue fingerprint {reads} times in {window:?}; \
             expected at most {}",
            workers * ticks
        );
        assert_eq!(loads, 0, "idle workers reloaded a settled store");
        manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[test]
    fn claim_schedule_naps_long_only_when_nothing_is_claimable() {
        let now = Instant::now();
        let wall_now = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let settled = StoreFingerprint(Some((std::time::SystemTime::UNIX_EPOCH, 1, 1)));
        let mut schedule = ClaimSchedule::new(now);
        // A fresh schedule has a claim pending.
        assert_eq!(schedule.nap(), STORE_REFRESH_INTERVAL);
        schedule.found_nothing(settled.clone(), now, wall_now);
        assert_eq!(schedule.nap(), STORE_SETTLED_NAP);
        // A failed claim keeps the short tick so a queue change is seen early.
        schedule.claim_failed(settled.clone(), now);
        assert_eq!(schedule.nap(), STORE_REFRESH_INTERVAL);
        // Recent or missing metadata retries on a deadline, so it stays short.
        schedule.found_nothing(StoreFingerprint(None), now, wall_now);
        assert_eq!(schedule.nap(), STORE_REFRESH_INTERVAL);
        schedule.claimed_task(now);
        assert_eq!(schedule.nap(), STORE_REFRESH_INTERVAL);
    }

    /// #6573: a busy store names the holder; an unreadable record falls back
    /// to the bare message without failing.
    #[test]
    fn busy_store_error_names_the_lock_holder_when_known() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("task-store.lock");
        let mut holder =
            RuntimeProcessOwnerLock::try_acquire_file(&path, true)?.context("first acquire")?;
        holder.record_holder();
        assert!(RuntimeProcessOwnerLock::try_acquire_file(&path, true)?.is_none());

        let error = store_busy_error(&path);
        assert!(
            error.is::<TaskStoreBusy>(),
            "holder diagnostics must retain typed contention"
        );
        let message = error.to_string();
        assert!(
            message.contains(&format!("held by pid {}", std::process::id())),
            "{message}"
        );

        // Releasing clears the record, so a later error cannot name a stale pid.
        drop(holder);
        assert_eq!(
            store_busy_error(&path).to_string(),
            "Task store is busy; state is unavailable"
        );
        // Garbage and a missing file also fall back.
        fs::write(&path, "pid=oops since_ms=\n")?;
        assert!(RuntimeProcessOwnerLock::read_holder(&path).is_none());
        fs::remove_file(&path)?;
        assert!(RuntimeProcessOwnerLock::read_holder(&path).is_none());
        Ok(())
    }

    #[tokio::test]
    async fn running_task_observes_durable_cancel_without_a_queue_change() -> Result<()> {
        let root = tempfile::tempdir()?;
        let manager = TaskManager::start_with_executor(
            test_config(root.path().to_path_buf()),
            Arc::new(CooperativeIdleCancelExecutor),
        )
        .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("durable cancel"))
            .await?;
        wait_for_running(&manager, &task.id, Duration::from_secs(2)).await?;
        let queue_before = StoreFingerprint::read(&manager.queue_path);
        {
            let _transaction = manager.lock_store().await?;
            let mut task = manager
                .read_bound_task(&task.id)?
                .context("running fixture")?;
            task.lifecycle_seq += 1;
            task.cancel_requested_seq = task.lifecycle_seq;
            manager.persist_task_locked(&task)?;
        }
        let done = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(2)).await?;
        assert_eq!(done.status, TaskStatus::Canceled);
        assert_eq!(StoreFingerprint::read(&manager.queue_path), queue_before);
        manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test]
    async fn persists_and_recovers_task_records() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;

        let task = manager
            .add_task(NewTaskRequest {
                owner_session_id: Some("session-persist".to_string()),
                ..NewTaskRequest::from_prompt("test persistence")
            })
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Completed);
        assert_eq!(finished.thread_id.as_deref(), Some("thr_test"));
        assert_eq!(finished.turn_id.as_deref(), Some("turn_test"));
        assert_eq!(finished.checklist.items.len(), 1);
        assert_eq!(finished.checklist.in_progress_id, Some(1));
        assert!(
            finished.lifecycle_seq >= 3,
            "queued, running, and terminal owner transitions must advance the sequence"
        );

        manager.shutdown_and_wait().await?;
        drop(manager);

        let recovered =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;
        let loaded = recovered.get_task(&task.id).await?;
        assert_eq!(loaded.status, TaskStatus::Completed);
        assert_eq!(
            loaded.owner_session_id.as_deref(),
            Some("session-persist"),
            "session ownership should survive persistence and restart"
        );
        assert!(!loaded.timeline.is_empty());
        assert_eq!(loaded.checklist.items[0].content, "read fixture");
        Ok(())
    }

    struct AdmissionCountingExecutor(Arc<AtomicUsize>);

    #[async_trait]
    impl TaskExecutor for AdmissionCountingExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            _events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            self.0.fetch_add(1, Ordering::SeqCst);
            TaskExecutionResult {
                status: TaskStatus::Completed,
                result_text: Some("admission fixture completed".into()),
                error: None,
                terminal_reason: TaskTerminalReason::Completed,
            }
        }
    }

    #[tokio::test]
    async fn interrupted_task_stage_preserves_resolved_request_and_executes_once() -> Result<()> {
        for queue_was_written in [false, true] {
            let root = tempfile::tempdir()?;
            let tasks_dir = root.path().join("tasks");
            fs::create_dir_all(&tasks_dir)?;
            let mut staged = sample_task_record();
            staged.status = TaskStatus::Queued;
            staged.started_at = None;
            staged.model = "staged-model".into();
            staged.workspace = root.path().join("staged-workspace");
            fs::create_dir(&staged.workspace)?;
            let mut request = NewTaskRequest::from_task(&staged);
            request.model = None;
            request.workspace = None;
            request.mode = None;
            request.allow_shell = None;
            request.trust_mode = None;
            let staged_path = tasks_dir.join(format!(".{}.json.pending", staged.id));
            write_json_atomic(&staged_path, &staged)?;
            if queue_was_written {
                write_json_atomic(
                    &root.path().join("queue.json"),
                    &QueueFile {
                        queue: vec![staged.id.clone()],
                    },
                )?;
            }
            let executions = Arc::new(AtomicUsize::new(0));
            let mut config = test_config(root.path().to_path_buf());
            config.default_model = "new-default-model".into();
            config.default_workspace = root.path().join("new-workspace");
            config.default_mode = "plan".into();
            config.allow_shell = true;
            config.trust_mode = true;
            let manager = TaskManager::start_with_executor(
                config,
                Arc::new(AdmissionCountingExecutor(executions.clone())),
            )
            .await?;
            // Interrupt recovery while its admission is waiting for the queue
            // lock. The resolved intent must remain durable for another retry.
            let stage_before = fs::read(&staged_path)?;
            let queue_guard = manager.state.lock().await;
            let mut recovery =
                Box::pin(manager.recover_task_admission(request.clone(), staged.id.clone()));
            assert!(
                tokio::time::timeout(Duration::from_millis(25), &mut recovery)
                    .await
                    .is_err()
            );
            assert_eq!(
                fs::read(&staged_path)?,
                stage_before,
                "interrupted recovery must preserve its resolved staged intent"
            );
            assert!(manager.read_bound_task(&staged.id)?.is_none());
            assert_eq!(executions.load(Ordering::SeqCst), 0);
            drop(recovery);
            drop(queue_guard);
            let admitted = manager
                .recover_task_admission(request.clone(), staged.id.clone())
                .await?;
            assert_eq!(admitted.id, staged.id);
            assert_eq!(admitted.model, staged.model);
            assert_eq!(admitted.workspace, staged.workspace);
            assert_eq!(admitted.mode, staged.mode);
            assert_eq!(admitted.allow_shell, staged.allow_shell);
            assert_eq!(admitted.trust_mode, staged.trust_mode);
            assert!(
                !staged_path.exists(),
                "unaccepted stage recovered through TaskManager"
            );
            let completed =
                wait_for_terminal_state(&manager, &admitted.id, Duration::from_secs(5)).await?;
            assert_eq!(completed.status, TaskStatus::Completed);
            assert!(
                completed
                    .result_summary
                    .as_deref()
                    .unwrap_or_default()
                    .contains("admission fixture completed")
            );
            assert_eq!(executions.load(Ordering::SeqCst), 1);
            let replay = manager
                .recover_task_admission(request.clone(), staged.id.clone())
                .await?;
            assert_eq!(replay.status, TaskStatus::Completed);
            assert_eq!(replay.id, staged.id);
            let canonical = tasks_dir.join(format!("{}.json", staged.id));
            let before = fs::read(&canonical)?;
            let mut mismatched = request;
            mismatched.prompt = "a different operation".into();
            let error = manager
                .recover_task_admission(mismatched, staged.id)
                .await
                .expect_err("mismatched replay must be rejected");
            assert!(error.to_string().contains("does not match"));
            assert_eq!(
                fs::read(canonical)?,
                before,
                "replay cannot rewrite accepted work"
            );
            assert_eq!(executions.load(Ordering::SeqCst), 1);
            manager.shutdown();
        }
        Ok(())
    }

    #[tokio::test]
    async fn accepted_task_interrupted_by_restart_is_reconciled_without_execution() -> Result<()> {
        let root = tempfile::tempdir()?;
        let tasks_dir = root.path().join("tasks");
        fs::create_dir_all(&tasks_dir)?;
        let mut accepted = sample_task_record();
        accepted.execution_generation = Some(Uuid::new_v4().simple().to_string());
        let lease_path = execution_lease_path(
            root.path(),
            accepted.execution_scope.as_deref().unwrap(),
            accepted.execution_generation.as_deref().unwrap(),
        )?;
        drop(
            RuntimeProcessOwnerLock::try_acquire_file(&lease_path, true)?
                .context("fixture generation")?,
        );
        let request = NewTaskRequest::from_task(&accepted);
        write_json_atomic(&tasks_dir.join(format!("{}.json", accepted.id)), &accepted)?;
        write_json_atomic(
            &root.path().join("queue.json"),
            &QueueFile {
                queue: vec![accepted.id.clone()],
            },
        )?;
        let executions = Arc::new(AtomicUsize::new(0));
        let manager = TaskManager::start_with_executor(
            test_config(root.path().to_path_buf()),
            Arc::new(AdmissionCountingExecutor(executions.clone())),
        )
        .await?;
        let recovered = manager
            .recover_task_admission(request, accepted.id.clone())
            .await?;
        assert_eq!(recovered.id, accepted.id);
        assert_eq!(recovered.status, TaskStatus::Failed);
        assert!(
            recovered
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Interrupted by process restart")
        );
        assert_eq!(manager.list_tasks(None).await?.len(), 1);
        assert_eq!(
            executions.load(Ordering::SeqCst),
            0,
            "accepted work cannot be replayed after restart"
        );
        manager.shutdown();
        Ok(())
    }

    #[tokio::test]
    async fn preallocated_task_ids_are_validated_and_collision_safe() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root), Arc::new(MockExecutor)).await?;
        let request = NewTaskRequest::from_prompt("preallocated owner identity");

        let invalid = manager
            .add_task_with_id(request.clone(), "task_short".to_string())
            .await
            .expect_err("invalid preallocated id");
        assert!(invalid.to_string().contains("task_<16hex>"), "{invalid:#}");

        let id = "task_0123456789abcdef".to_string();
        let created = manager
            .add_task_with_id(request.clone(), id.clone())
            .await?;
        assert_eq!(created.id, id);
        assert_eq!(
            created.schema_version, CURRENT_TASK_SCHEMA_VERSION,
            "execution provenance requires readers that preserve the binding"
        );
        assert_eq!(created.lifecycle_seq, 1);
        let collision = manager
            .add_task_with_id(request, id)
            .await
            .expect_err("task id collision");
        assert!(
            collision.to_string().contains("already exists"),
            "{collision:#}"
        );
        assert_eq!(manager.list_tasks(None).await?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn failed_queue_write_leaves_no_replayable_task_record() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;
        std::fs::remove_file(root.join("queue.json"))?;
        std::fs::create_dir(root.join("queue.json"))?;

        let id = "task_fedcba9876543210".to_string();
        let error = manager
            .add_task_with_id(
                NewTaskRequest::from_prompt("must not resurrect"),
                id.clone(),
            )
            .await
            .expect_err("queue path directory must reject the atomic queue write");
        assert!(error.to_string().contains("queue.json"), "{error:#}");
        assert!(
            manager.list_tasks(None).await.is_err(),
            "unavailable storage cannot be reported as empty"
        );
        assert!(!root.join("tasks").join(format!("{id}.json")).exists());
        assert!(
            !root
                .join("tasks")
                .join(format!(".{id}.json.pending"))
                .exists(),
            "a failed queue write may leave no replayable or staged task record"
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_tasks_scopes_results_to_workspace_before_limit() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root), Arc::new(MockExecutor)).await?;

        manager
            .add_task(NewTaskRequest {
                prompt: "task in workspace a".to_string(),
                workspace: Some(PathBuf::from("/tmp/workspace-a")),
                ..NewTaskRequest::from_prompt("task in workspace a")
            })
            .await?;
        manager
            .add_task(NewTaskRequest {
                prompt: "task in workspace b".to_string(),
                workspace: Some(PathBuf::from("/tmp/workspace-b")),
                ..NewTaskRequest::from_prompt("task in workspace b")
            })
            .await?;

        let scoped = manager
            .list_tasks_scoped(Some(1), Some(Path::new("/tmp/workspace-a")))
            .await?;
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].workspace, PathBuf::from("/tmp/workspace-a"));
        Ok(())
    }

    #[tokio::test]
    async fn task_controls_are_session_owned_and_legacy_records_fail_closed() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root), Arc::new(MockExecutor)).await?;

        let mut session_a = sample_task_record();
        session_a.id = "task_dead000000000001".to_string();
        session_a.owner_session_id = Some("session-a".to_string());
        session_a.status = TaskStatus::Completed;
        session_a.created_at = Utc::now() - chrono::Duration::seconds(3);

        let mut session_b = sample_task_record();
        session_b.id = "task_dead000000000002".to_string();
        session_b.owner_session_id = Some("session-b".to_string());
        session_b.status = TaskStatus::Completed;
        session_b.created_at = Utc::now() - chrono::Duration::seconds(2);

        let mut session_b_newest = sample_task_record();
        session_b_newest.id = "task_beef000000000002".to_string();
        session_b_newest.owner_session_id = Some("session-b".to_string());
        session_b_newest.status = TaskStatus::Completed;
        session_b_newest.created_at = Utc::now();

        let mut legacy = sample_task_record();
        legacy.id = "task_dead000000000003".to_string();
        legacy.owner_session_id = None;
        legacy.execution_scope = None;
        legacy.status = TaskStatus::Completed;
        legacy.created_at = Utc::now() - chrono::Duration::seconds(1);

        {
            let mut state = manager.state.lock().await;
            for record in [
                session_a.clone(),
                session_b.clone(),
                session_b_newest.clone(),
                legacy.clone(),
            ] {
                manager.persist_task_locked(&record)?;
                state.tasks.insert(record.id.clone(), record);
            }
        }

        let session_b_list = manager
            .list_tasks_for_owner(Some(1), None, "session-b")
            .await?;
        assert_eq!(session_b_list.len(), 1);
        assert_eq!(session_b_list[0].id, session_b_newest.id);

        let session_b_prefix = manager.get_task_for_owner("task_dead", "session-b").await?;
        assert_eq!(session_b_prefix.id, session_b.id);
        assert!(
            manager
                .get_task_for_owner(&session_a.id, "session-b")
                .await
                .unwrap_err()
                .to_string()
                .contains("Task not found")
        );
        assert!(
            manager
                .get_task_for_owner(&legacy.id, "session-b")
                .await
                .unwrap_err()
                .to_string()
                .contains("Task not found")
        );
        assert!(
            manager
                .get_task_for_active_runtime(&legacy.id)
                .await
                .unwrap_err()
                .to_string()
                .contains("Task not found"),
            "legacy ownerless active tasks must fail closed"
        );
        assert_eq!(
            manager.get_task_for_active_runtime(&session_a.id).await?.id,
            session_a.id
        );

        assert!(
            manager
                .cancel_task_for_owner(&session_a.id, "session-b")
                .await
                .unwrap_err()
                .to_string()
                .contains("Task not found")
        );
        assert_eq!(
            manager.get_task(&session_a.id).await?.status,
            TaskStatus::Completed
        );
        assert!(
            manager
                .cancel_task_for_owner(&legacy.id, "session-b")
                .await
                .unwrap_err()
                .to_string()
                .contains("Task not found")
        );

        let own = manager
            .cancel_task_for_owner(&session_b.id, "session-b")
            .await?;
        assert_eq!(own.disposition, TaskCancelDisposition::AlreadyFinished);
        let active_own = manager
            .cancel_task_for_active_runtime(&session_a.id)
            .await?;
        assert_eq!(
            active_own.disposition,
            TaskCancelDisposition::AlreadyFinished
        );
        assert_eq!(
            manager
                .get_task_for_owner(&session_a.id, "session-a")
                .await?
                .id,
            session_a.id,
            "switching A to B and back must restore A's controls"
        );
        Ok(())
    }

    #[tokio::test]
    async fn interactive_task_controls_include_only_owned_or_same_scope_records() -> Result<()> {
        let root = tempfile::tempdir()?;
        let manager = TaskManager::start_with_executor(
            test_config(root.path().to_path_buf()),
            Arc::new(MockExecutor),
        )
        .await?;
        // Exercise persisted controls without a worker racing to execute the
        // queued fixture records. The manager retains its verified scope.
        manager.shutdown_and_wait().await?;
        let mut scheduled = sample_task_record();
        scheduled.id = "task_dead000000000001".to_string();
        scheduled.owner_session_id = None;
        scheduled.execution_scope = Some(manager.execution_scope().to_string());
        scheduled.status = TaskStatus::Queued;

        let mut owned = scheduled.clone();
        owned.id = "task_beef000000000001".to_string();
        owned.owner_session_id = Some("session-a".to_string());
        owned.status = TaskStatus::Completed;

        let mut foreign_scope = scheduled.clone();
        foreign_scope.id = "task_dead000000000002".to_string();
        foreign_scope.execution_scope = Some(test_execution_scope("other"));
        let mut other_session = scheduled.clone();
        other_session.id = "task_dead000000000003".to_string();
        other_session.owner_session_id = Some("session-b".to_string());
        let mut legacy = scheduled.clone();
        legacy.id = "task_dead000000000004".to_string();
        legacy.execution_scope = None;
        let hidden = [foreign_scope, other_session, legacy];
        {
            let mut state = manager.state.lock().await;
            for record in std::iter::once(&scheduled)
                .chain(std::iter::once(&owned))
                .chain(hidden.iter())
            {
                manager.persist_task_locked(record)?;
                state.tasks.insert(record.id.clone(), record.clone());
            }
        }

        for record in [&scheduled, &owned] {
            assert_eq!(
                manager
                    .get_task_for_interactive_session(&record.id, "session-a")
                    .await?
                    .id,
                record.id
            );
        }
        // Hidden records sharing this prefix must not make it ambiguous.
        assert_eq!(
            manager
                .get_task_for_interactive_session("task_dead", "session-a")
                .await?
                .id,
            scheduled.id
        );
        let ambiguous = manager
            .get_task_for_interactive_session("task_", "session-a")
            .await
            .unwrap_err();
        assert!(ambiguous.to_string().contains("matches 2 tasks"));
        for record in &hidden {
            assert!(
                manager
                    .get_task_for_interactive_session(&record.id, "session-a")
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("Task not found")
            );
            assert!(
                manager
                    .cancel_task_for_interactive_session(&record.id, "session-a")
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("Task not found")
            );
            assert_eq!(
                manager.get_task(&record.id).await?.status,
                TaskStatus::Queued
            );
        }
        // Model and child-session APIs do not inherit the human-only access.
        assert!(
            manager
                .get_task_for_owner(&scheduled.id, "session-a")
                .await
                .is_err()
        );
        assert!(
            manager
                .cancel_task_for_owner(&scheduled.id, "session-a")
                .await
                .is_err()
        );
        let canceled = manager
            .cancel_task_for_interactive_session("task_dead", "session-a")
            .await?;
        assert_eq!(canceled.task.id, scheduled.id);
        assert_eq!(canceled.task.status, TaskStatus::Canceled);
        assert_eq!(
            manager.get_task(&scheduled.id).await?.status,
            TaskStatus::Canceled
        );
        manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[test]
    fn interactive_task_controls_reject_an_empty_manager_scope() {
        let mut record = sample_task_record();
        record.owner_session_id = None;
        record.execution_scope = Some(String::new());
        let id = record.id.clone();
        let tasks = HashMap::from([(id.clone(), record)]);
        assert!(resolve_task_id_visible_to_operator(&tasks, &id, "session-a", "").is_err());
    }

    #[tokio::test]
    async fn boot_does_not_rewrite_non_recovered_task_files() -> Result<()> {
        // #3757 boot-persist narrowing: TaskManager::start must persist only
        // the reconciled queue and the running->failed recoveries — a
        // completed task's file must be byte-identical across a restart.
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("finish then persist"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Completed);
        manager.shutdown_and_wait().await?;
        drop(manager);

        let task_file = root.join("tasks").join(format!("{}.json", task.id));
        let before = fs::read(&task_file)?;

        let recovered =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;
        // Give start() a beat to run its (narrowed) boot persist.
        sleep(Duration::from_millis(50)).await;
        drop(recovered);

        let after = fs::read(&task_file)?;
        assert_eq!(
            before, after,
            "a completed task file must not be rewritten on boot"
        );
        Ok(())
    }

    #[test]
    fn legacy_running_tasks_are_preserved_without_assuming_owner_death() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let tasks_dir = root.join("tasks");
        fs::create_dir_all(&tasks_dir)?;
        let queue_path = root.join("queue.json");
        let task_id = "task_stale_running".to_string();
        let started_at = Utc::now() - chrono::Duration::seconds(30);
        let task = TaskRecord {
            destination_thread_id: None,
            schema_version: CURRENT_TASK_SCHEMA_VERSION,
            id: task_id.clone(),
            prompt: "long-running shell work".to_string(),
            name: None,
            model: "deepseek-v4-flash".to_string(),
            model_provider: None,
            model_provider_id: None,
            workspace: PathBuf::from("."),
            mode: "agent".to_string(),
            allow_shell: true,
            trust_mode: false,
            auto_approve: false,
            permission_posture: None,
            status: TaskStatus::Running,
            created_at: started_at,
            started_at: Some(started_at),
            ended_at: None,
            duration_ms: None,
            result_summary: None,
            result_detail_path: None,
            error: None,
            terminal_reason: None,
            thread_id: Some("thr_stale".to_string()),
            turn_id: Some("turn_stale".to_string()),
            owner_session_id: Some("session-old".to_string()),
            execution_scope: None,
            execution_generation: None,
            cancel_requested_seq: 0,
            runtime_event_count: 0,
            lifecycle_seq: 2,
            checklist: TaskChecklistState::default(),
            gates: Vec::new(),
            attempts: Vec::new(),
            artifacts: Vec::new(),
            github_events: Vec::new(),
            tool_calls: vec![TaskToolCallSummary {
                id: "tool_shell".to_string(),
                name: "task_shell_start".to_string(),
                status: TaskToolStatus::Running,
                started_at,
                ended_at: None,
                duration_ms: None,
                input_summary: Some("shell: sleep 999".to_string()),
                output_summary: None,
                detail_path: None,
                patch_ref: None,
            }],
            timeline: vec![TaskTimelineEntry {
                timestamp: started_at,
                kind: "running".to_string(),
                summary: "Task started".to_string(),
                detail_path: None,
            }],
        };
        fs::write(
            tasks_dir.join(format!("{task_id}.json")),
            serde_json::to_string_pretty(&task)?,
        )?;
        fs::write(
            &queue_path,
            serde_json::to_string_pretty(&QueueFile {
                queue: vec![task_id.clone()],
            })?,
        )?;

        let loaded = load_state(&tasks_dir, &queue_path)?;
        let queue = loaded.queue;
        let recovered = loaded.tasks.get(&task_id).expect("task loaded");

        assert!(queue.is_empty(), "stale running task must not be requeued");
        assert_eq!(recovered.status, TaskStatus::Running);
        assert!(recovered.ended_at.is_none());
        assert!(recovered.error.is_none());
        assert_eq!(recovered.tool_calls[0].status, TaskToolStatus::Running);
        assert!(!TaskSummary::from(recovered).execution_binding_known);
        assert_eq!(
            fs::read(tasks_dir.join(format!("{task_id}.json")))?,
            serde_json::to_string_pretty(&task)?.as_bytes()
        );
        Ok(())
    }

    #[tokio::test]
    async fn default_workspace_updates_for_future_tasks() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let new_workspace =
            std::env::temp_dir().join(format!("deepseek-workspace-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root), Arc::new(MockExecutor)).await?;

        manager.set_default_workspace(new_workspace.clone()).await;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("test workspace default"))
            .await?;

        assert_eq!(manager.default_workspace().await, new_workspace);
        assert_eq!(task.workspace, new_workspace);
        Ok(())
    }

    #[tokio::test]
    async fn record_tool_metadata_updates_explicit_task() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root), Arc::new(MockExecutor)).await?;

        let task = manager
            .add_task(NewTaskRequest::from_prompt("test metadata"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        let updated = manager
            .record_tool_metadata(
                &finished.id,
                &serde_json::json!({
                    "task_updates": {
                        "gate": {
                            "id": "gate_test",
                            "gate": "test",
                            "command": "cargo test -p codewhale-tui --lib",
                            "cwd": ".",
                            "exit_code": 0,
                            "status": "passed",
                            "classification": "passed",
                            "duration_ms": 1,
                            "summary": "ok",
                            "log_path": null,
                            "recorded_at": Utc::now()
                        }
                    }
                }),
            )
            .await?;

        assert_eq!(updated.gates.len(), 1);
        assert_eq!(updated.gates[0].classification, "passed");
        Ok(())
    }

    #[tokio::test]
    async fn write_task_artifact_rejects_traversal_task_id() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("tasks-root");
        let escaped = temp.path().join("escape");
        let manager =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;

        let err = manager
            .write_task_artifact("../escape", "result", "artifact body")
            .expect_err("traversal task ids must be rejected");

        assert!(err.to_string().contains("single path component"));
        assert!(!escaped.exists(), "artifact write escaped the task root");
        Ok(())
    }

    #[tokio::test]
    async fn cancel_running_task_marks_canceled() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager = TaskManager::start_with_executor(
            test_config(root),
            Arc::new(CooperativeIdleCancelExecutor),
        )
        .await?;

        let task = manager
            .add_task(NewTaskRequest::from_prompt("test cancellation"))
            .await?;

        wait_for_running(&manager, &task.id, Duration::from_secs(5)).await?;
        let cancellation = manager.cancel_task(&task.id).await?;
        assert_eq!(cancellation.disposition, TaskCancelDisposition::Requested);
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Canceled);
        Ok(())
    }

    #[tokio::test]
    async fn cancel_finished_task_returns_atomic_already_finished_outcome() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root), Arc::new(MockExecutor)).await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("finish before cancellation"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Completed);

        let cancellation = manager.cancel_task(&task.id).await?;

        assert_eq!(
            cancellation.disposition,
            TaskCancelDisposition::AlreadyFinished
        );
        assert_eq!(cancellation.task.status, TaskStatus::Completed);
        Ok(())
    }

    // GHSA-72w5-pf8h-xfp4 — regression: omitted optional fields must not
    // silently elevate the spawned task's privileges.
    #[tokio::test]
    async fn add_task_without_optional_fields_does_not_grant_shell_or_auto_approve() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;

        let req = NewTaskRequest {
            destination_thread_id: None,
            prompt: "fix TODOs and write a README".to_string(),
            name: None,
            model: None,
            model_provider: None,
            model_provider_id: None,
            workspace: None,
            mode: None,
            allow_shell: None,
            trust_mode: None,
            auto_approve: None,
            permission_posture: None,
            owner_session_id: None,
        };
        let task = manager.add_task(req).await?;

        assert!(
            !task.allow_shell,
            "model-omitted allow_shell must default to false (no silent shell grant)"
        );
        assert!(
            !task.auto_approve,
            "model-omitted auto_approve must default to false (no silent auto-approval)"
        );
        assert!(
            !task.trust_mode,
            "model-omitted trust_mode must default to false"
        );
        Ok(())
    }

    /// A task's own thread starts on the posture the request pinned, and the
    /// posture is what the thread's policy is derived from — the legacy
    /// `auto_approve` bit is only read when no posture is given.
    #[tokio::test]
    async fn add_task_pins_the_posture_its_thread_starts_on() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;

        let task = manager
            .add_task(NewTaskRequest {
                permission_posture: Some("auto_review".to_string()),
                ..NewTaskRequest::from_prompt("pin the posture")
            })
            .await?;

        assert_eq!(task.permission_posture.as_deref(), Some("auto_review"));
        let request = ExecutionTask::from(&task).thread_request();
        assert_eq!(request.permission_posture.as_deref(), Some("auto_review"));
        // `from_prompt` asks for auto-approval; the pinned posture outranks it,
        // so the thread must not silently run wider than what was requested.
        assert_eq!(request.auto_approve, Some(true));
        Ok(())
    }

    /// The worker's thread projection refuses these postures, so admission
    /// refuses them too: the request came through the Runtime API, and that
    /// is where the refusal belongs, not in a worker after the task was
    /// durably queued.
    #[tokio::test]
    async fn add_task_refuses_a_posture_the_thread_would_reject() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;

        for posture in ["sideways", "never"] {
            let error = manager
                .add_task(NewTaskRequest {
                    permission_posture: Some(posture.to_string()),
                    ..NewTaskRequest::from_prompt("refuse me")
                })
                .await
                .expect_err("a posture the thread cannot honour is refused at admission");
            assert!(error.to_string().contains("permission posture"), "{error}");
        }
        assert!(manager.list_tasks(None).await?.is_empty());
        Ok(())
    }

    /// The Runtime's `POST /v1/tasks` body may omit the posture entirely: it is
    /// optional on the wire, and absent means "derive it from the legacy bits",
    /// which is what every client that predates the field sends.
    #[test]
    fn new_task_request_accepts_a_body_without_a_posture() {
        let request: NewTaskRequest =
            serde_json::from_str(r#"{"prompt":"ship it","mode":"agent"}"#).expect("wire body");
        assert!(request.permission_posture.is_none());
        assert_eq!(request.mode.as_deref(), Some("agent"));
    }

    #[tokio::test]
    async fn rejects_newer_task_schema_on_recovery() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(MockExecutor))
                .await?;

        let task = manager
            .add_task(NewTaskRequest::from_prompt("test schema gate"))
            .await?;
        let _ = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        manager.shutdown_and_wait().await?;
        drop(manager);

        let task_path = root.join("tasks").join(format!("{}.json", task.id));
        let mut value: serde_json::Value = serde_json::from_str(&fs::read_to_string(&task_path)?)?;
        value["schema_version"] = serde_json::json!(999);
        fs::write(&task_path, serde_json::to_string_pretty(&value)?)?;

        match TaskManager::start_with_executor(test_config(root), Arc::new(MockExecutor)).await {
            Ok(_) => panic!("manager should reject newer task schema"),
            Err(err) => assert!(err.to_string().contains("newer than supported")),
        }
        Ok(())
    }

    #[test]
    fn default_tasks_dir_falls_back_to_legacy_deepseek_tasks() {
        let temp_home = tempfile::tempdir().unwrap();
        let home = temp_home.path();
        let legacy_tasks = home.join(".deepseek").join("tasks");
        std::fs::create_dir_all(&legacy_tasks).unwrap();

        assert_eq!(default_tasks_dir_for_home(home), legacy_tasks);
    }

    #[test]
    fn default_tasks_dir_prefers_existing_codewhale_tasks() {
        let temp_home = tempfile::tempdir().unwrap();
        let home = temp_home.path();
        let primary_tasks = home.join(".codewhale").join("tasks");
        let legacy_tasks = home.join(".deepseek").join("tasks");
        std::fs::create_dir_all(&primary_tasks).unwrap();
        std::fs::create_dir_all(&legacy_tasks).unwrap();

        assert_eq!(default_tasks_dir_for_home(home), primary_tasks);
    }

    #[test]
    fn default_tasks_dir_falls_back_to_legacy_when_primary_is_file() {
        let temp_home = tempfile::tempdir().unwrap();
        let home = temp_home.path();
        let primary_tasks = home.join(".codewhale").join("tasks");
        let legacy_tasks = home.join(".deepseek").join("tasks");
        std::fs::create_dir_all(primary_tasks.parent().unwrap()).unwrap();
        std::fs::write(&primary_tasks, "not a directory").unwrap();
        std::fs::create_dir_all(&legacy_tasks).unwrap();

        assert_eq!(default_tasks_dir_for_home(home), legacy_tasks);
    }

    #[test]
    fn default_tasks_dir_ignores_legacy_file_for_new_installs() {
        let temp_home = tempfile::tempdir().unwrap();
        let home = temp_home.path();
        let primary_tasks = home.join(".codewhale").join("tasks");
        let legacy_tasks = home.join(".deepseek").join("tasks");
        std::fs::create_dir_all(legacy_tasks.parent().unwrap()).unwrap();
        std::fs::write(&legacy_tasks, "not a directory").unwrap();

        assert_eq!(default_tasks_dir_for_home(home), primary_tasks);
    }

    #[test]
    fn default_tasks_dir_uses_codewhale_tasks_for_new_installs() {
        let temp_home = tempfile::tempdir().unwrap();
        let home = temp_home.path();

        assert_eq!(
            default_tasks_dir_for_home(home),
            home.join(".codewhale").join("tasks")
        );
    }

    #[test]
    fn task_and_runtime_roots_honor_explicit_codewhale_home() {
        let _lock = lock_test_env();
        let temp_root = tempfile::tempdir().unwrap();
        let ambient_home = temp_root.path().join("ambient-home");
        let explicit_home = temp_root.path().join("explicit-home");
        std::fs::create_dir_all(ambient_home.join(".deepseek").join("tasks")).unwrap();
        let _home = EnvVarGuard::set("HOME", &ambient_home);
        let _userprofile = EnvVarGuard::set("USERPROFILE", &ambient_home);
        let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &explicit_home);
        let _tasks_override = EnvVarGuard::remove("CODEWHALE_TASKS_DIR");
        let _legacy_tasks_override = EnvVarGuard::remove("DEEPSEEK_TASKS_DIR");
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

        let task_root = default_tasks_dir();
        let task_manager =
            TaskManagerConfig::from_runtime(&Config::default(), PathBuf::from("."), None, None);
        let runtime = RuntimeThreadManagerConfig::from_task_data_dir(task_manager.data_dir.clone());

        assert_eq!(task_root, explicit_home.join("tasks"));
        assert_eq!(task_manager.data_dir, task_root);
        assert_eq!(runtime.task_data_dir, task_root);
        assert_eq!(
            runtime.data_dir,
            explicit_home.join("tasks").join("runtime")
        );
    }

    #[test]
    fn whitespace_codewhale_home_keeps_ambient_legacy_task_and_runtime_fallbacks() {
        let _lock = lock_test_env();
        let temp_root = tempfile::tempdir().unwrap();
        let ambient_home = temp_root.path().join("ambient-home");
        let legacy_tasks = ambient_home.join(".deepseek").join("tasks");
        std::fs::create_dir_all(&legacy_tasks).unwrap();
        let _home = EnvVarGuard::set("HOME", &ambient_home);
        let _userprofile = EnvVarGuard::set("USERPROFILE", &ambient_home);
        let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", " \t ");
        let _tasks_override = EnvVarGuard::remove("CODEWHALE_TASKS_DIR");
        let _legacy_tasks_override = EnvVarGuard::remove("DEEPSEEK_TASKS_DIR");
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

        let task_root = default_tasks_dir();
        let task_manager =
            TaskManagerConfig::from_runtime(&Config::default(), PathBuf::from("."), None, None);
        let runtime = RuntimeThreadManagerConfig::from_task_data_dir(task_manager.data_dir.clone());

        assert_eq!(task_root, legacy_tasks);
        assert_eq!(task_manager.data_dir, task_root);
        assert_eq!(runtime.task_data_dir, task_root);
        assert_eq!(runtime.data_dir, task_root.join("runtime"));
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_codewhale_home_is_preserved_by_task_and_runtime_roots() {
        use std::os::unix::ffi::OsStringExt;

        let _lock = lock_test_env();
        let temp_root = tempfile::tempdir().unwrap();
        let explicit_home = temp_root.path().join(std::ffi::OsString::from_vec(
            b"codewhale-\xff-home".to_vec(),
        ));
        let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", &explicit_home);
        let _tasks_override = EnvVarGuard::remove("CODEWHALE_TASKS_DIR");
        let _legacy_tasks_override = EnvVarGuard::remove("DEEPSEEK_TASKS_DIR");
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");

        let task_root = default_tasks_dir();
        let task_manager =
            TaskManagerConfig::from_runtime(&Config::default(), PathBuf::from("."), None, None);
        let runtime = RuntimeThreadManagerConfig::from_task_data_dir(task_manager.data_dir.clone());

        assert_eq!(task_root, explicit_home.join("tasks"));
        assert_eq!(task_manager.data_dir, task_root);
        assert_eq!(runtime.task_data_dir, task_root);
        assert_eq!(
            runtime.data_dir,
            explicit_home.join("tasks").join("runtime")
        );
    }

    struct DeafHangExecutor;

    #[async_trait]
    impl TaskExecutor for DeafHangExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            _events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            std::future::pending().await
        }
    }

    struct PartialThenHangExecutor;

    #[async_trait]
    impl TaskExecutor for PartialThenHangExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            let _ = events
                .send(TaskExecutionEvent::MessageDelta {
                    content: "partial ".to_string(),
                })
                .await;
            let _ = events
                .send(TaskExecutionEvent::MessageDelta {
                    content: "result".to_string(),
                })
                .await;
            std::future::pending().await
        }
    }

    struct PollCountingHangExecutor {
        polls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl TaskExecutor for PollCountingHangExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            _events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            std::future::poll_fn(|_| {
                self.polls.fetch_add(1, Ordering::Relaxed);
                std::task::Poll::Pending
            })
            .await
        }
    }

    struct HeartbeatExecutor;

    #[async_trait]
    impl TaskExecutor for HeartbeatExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            loop {
                let _ = events
                    .send(TaskExecutionEvent::Status {
                        message: "heartbeat".to_string(),
                    })
                    .await;
                sleep(Duration::from_millis(10)).await;
            }
        }
    }

    struct ProgressHeartbeatExecutor;

    #[async_trait]
    impl TaskExecutor for ProgressHeartbeatExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            loop {
                let _ = events
                    .send(TaskExecutionEvent::MessageDelta {
                        content: "working".to_string(),
                    })
                    .await;
                sleep(Duration::from_millis(10)).await;
            }
        }
    }

    struct CooperativeIdleCancelExecutor;

    #[async_trait]
    impl TaskExecutor for CooperativeIdleCancelExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            _events: mpsc::Sender<TaskExecutionEvent>,
            cancel: CancellationToken,
        ) -> TaskExecutionResult {
            cancel.cancelled().await;
            TaskExecutionResult::from_reason(TaskTerminalReason::Canceled, None)
        }
    }

    struct CooperativeProgressCancelExecutor;

    #[async_trait]
    impl TaskExecutor for CooperativeProgressCancelExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            cancel: CancellationToken,
        ) -> TaskExecutionResult {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        return TaskExecutionResult::from_reason(
                            TaskTerminalReason::Canceled,
                            None,
                        );
                    }
                    _ = sleep(Duration::from_millis(10)) => {
                        let _ = events
                            .send(TaskExecutionEvent::MessageDelta {
                                content: "working".to_string(),
                            })
                            .await;
                    }
                }
            }
        }
    }

    struct PromptRouterExecutor;

    #[async_trait]
    impl TaskExecutor for PromptRouterExecutor {
        async fn execute(
            &self,
            task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            if task.prompt.starts_with("hang ") {
                std::future::pending().await
            } else {
                // The follow-up task must complete without a single await
                // point: `run_task` polls the executor future before its
                // guard can observe the (test-shortened) idle/wall budgets,
                // so an await-free future always finishes first and an
                // interrupt can never be recorded against it. The previous
                // MockExecutor delegation (`send(...).await` x4 plus a 50 ms
                // sleep) left windows where CI scheduler/storage stalls of
                // >=150 ms tripped the idle watchdog mid-flight; the executor
                // then observed the cancellation and returned `Canceled`,
                // which `preserve_timeout_reason` rewrote into the timeout
                // reason -> `Failed` (issue #5898). `try_send` keeps the
                // released worker's event pipeline exercised without
                // suspending this future.
                let _ = events.try_send(TaskExecutionEvent::Status {
                    message: format!("running after forced release {}", task.id),
                });
                TaskExecutionResult {
                    status: TaskStatus::Completed,
                    result_text: Some("done after hang".to_string()),
                    error: None,
                    terminal_reason: TaskTerminalReason::Completed,
                }
            }
        }
    }

    struct FloodExecutor;

    #[async_trait]
    impl TaskExecutor for FloodExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            _cancel: CancellationToken,
        ) -> TaskExecutionResult {
            for i in 0..400 {
                // Mirror the runtime path: each raw event is followed by its
                // derived message delta. Alternating the two non-urgent stream
                // kinds prevents timeline coalescing without turning this
                // storage-bound test into hundreds of synchronous fsyncs.
                let _ = events
                    .send(TaskExecutionEvent::RuntimeEvent {
                        seq: i,
                        event: "item.delta".to_string(),
                        summary: format!("tick {i}"),
                    })
                    .await;
                let _ = events
                    .send(TaskExecutionEvent::MessageDelta {
                        content: format!("chunk {i}"),
                    })
                    .await;
            }
            TaskExecutionResult {
                status: TaskStatus::Completed,
                result_text: Some("flooded".to_string()),
                error: None,
                terminal_reason: TaskTerminalReason::Completed,
            }
        }
    }

    struct CompleteAfterCancelExecutor;

    #[async_trait]
    impl TaskExecutor for CompleteAfterCancelExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            _events: mpsc::Sender<TaskExecutionEvent>,
            cancel: CancellationToken,
        ) -> TaskExecutionResult {
            cancel.cancelled().await;
            TaskExecutionResult {
                status: TaskStatus::Completed,
                result_text: Some("late complete".to_string()),
                error: None,
                terminal_reason: TaskTerminalReason::Completed,
            }
        }
    }

    fn sample_task_record() -> TaskRecord {
        TaskRecord {
            destination_thread_id: None,
            schema_version: CURRENT_TASK_SCHEMA_VERSION,
            id: "task_0123456789abcdef".to_string(),
            prompt: "bound timeline".to_string(),
            name: None,
            model: "deepseek-v4-flash".to_string(),
            model_provider: None,
            model_provider_id: None,
            workspace: PathBuf::from("."),
            mode: "agent".to_string(),
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            permission_posture: None,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            ended_at: None,
            duration_ms: None,
            result_summary: None,
            result_detail_path: None,
            error: None,
            terminal_reason: None,
            thread_id: None,
            turn_id: None,
            owner_session_id: None,
            execution_scope: Some(test_execution_scope("test")),
            execution_generation: None,
            cancel_requested_seq: 0,
            runtime_event_count: 0,
            lifecycle_seq: 2,
            checklist: TaskChecklistState::default(),
            gates: Vec::new(),
            attempts: Vec::new(),
            artifacts: Vec::new(),
            github_events: Vec::new(),
            tool_calls: Vec::new(),
            timeline: Vec::new(),
        }
    }

    /// Records persisted before `auto_approve` existed must load as not
    /// auto-approved, and the thread request they build must say so.
    #[test]
    fn task_record_missing_auto_approve_loads_fail_closed() {
        let mut record = sample_task_record();
        record.auto_approve = true;
        let mut value = serde_json::to_value(&record).expect("serialize");
        assert_eq!(value["auto_approve"], serde_json::json!(true));
        value
            .as_object_mut()
            .expect("object")
            .remove("auto_approve");
        let loaded: TaskRecord = serde_json::from_value(value).expect("legacy record loads");
        assert!(!loaded.auto_approve);
        assert_eq!(
            ExecutionTask::from(&loaded).thread_request().auto_approve,
            Some(false)
        );
        assert_eq!(
            ExecutionTask::from(&loaded).turn_request().auto_approve,
            Some(false)
        );
    }

    #[test]
    fn execution_guard_idle_does_not_reset_without_progress() {
        let start = Instant::now();
        let limits = TaskExecutionLimits::short_for_tests();
        let guard = ExecutionGuard::new(limits, start);
        match guard.evaluate(start + limits.idle_progress, false, false) {
            GuardAction::Interrupt { reason } => {
                assert_eq!(reason, TaskTerminalReason::IdleTimeout);
            }
            other => panic!("expected idle interrupt, got {other:?}"),
        }
    }

    #[test]
    fn execution_guard_reports_the_limit_that_expired_first_when_both_elapsed() {
        let start = Instant::now();
        let limits = TaskExecutionLimits::short_for_tests();
        let guard = ExecutionGuard::new(limits, start);
        // A starved watchdog that first ticks after both budgets ran out must
        // still report the idle limit, which expired first.
        match guard.evaluate(
            start + limits.wall_time + limits.idle_progress,
            false,
            false,
        ) {
            GuardAction::Interrupt { reason } => {
                assert_eq!(reason, TaskTerminalReason::IdleTimeout);
            }
            other => panic!("expected idle interrupt, got {other:?}"),
        }

        // Late progress pushes the idle deadline past the wall deadline, so
        // the same starved tick reports the wall limit instead.
        let mut guard = ExecutionGuard::new(limits, start);
        guard.note_progress(start + limits.wall_time - Duration::from_millis(1));
        match guard.evaluate(
            start + limits.wall_time + limits.idle_progress,
            false,
            false,
        ) {
            GuardAction::Interrupt { reason } => {
                assert_eq!(reason, TaskTerminalReason::WallTimeout);
            }
            other => panic!("expected wall interrupt, got {other:?}"),
        }
    }

    #[test]
    fn execution_guard_progress_refreshes_idle_until_wall_timeout() {
        let start = Instant::now();
        let limits = TaskExecutionLimits::short_for_tests();
        let mut guard = ExecutionGuard::new(limits, start);
        let progressed = start + (limits.idle_progress / 2);
        guard.note_progress(progressed);
        match guard.evaluate(progressed + (limits.idle_progress / 2), false, false) {
            GuardAction::Run { .. } => {}
            other => panic!("progress should keep idle from firing, got {other:?}"),
        }
        // Progress keeps arriving, so the idle deadline never expires before
        // the wall deadline does.
        guard.note_progress(start + limits.wall_time - (limits.idle_progress / 2));
        match guard.evaluate(start + limits.wall_time, false, false) {
            GuardAction::Interrupt { reason } => {
                assert_eq!(reason, TaskTerminalReason::WallTimeout);
            }
            other => panic!("expected wall interrupt, got {other:?}"),
        }
    }

    #[test]
    fn execution_guard_cancel_grace_terminalizes_stuck_work() {
        let start = Instant::now();
        let limits = TaskExecutionLimits::short_for_tests();
        let mut guard = ExecutionGuard::new(limits, start);
        match guard.evaluate(start, true, false) {
            GuardAction::Interrupt { reason } => {
                assert_eq!(reason, TaskTerminalReason::Canceled);
                guard.note_interrupt(start, reason);
            }
            other => panic!("expected cancel interrupt, got {other:?}"),
        }
        match guard.evaluate(start + limits.cancel_grace, true, false) {
            GuardAction::Terminalize { reason } => {
                assert_eq!(reason, TaskTerminalReason::CancelTimeout);
            }
            other => panic!("expected cancel timeout, got {other:?}"),
        }
    }

    #[test]
    fn consecutive_message_deltas_coalesce_on_the_timeline() {
        let mut task = sample_task_record();
        for i in 0..50 {
            push_timeline_entry(
                &mut task,
                TaskTimelineEntry {
                    timestamp: Utc::now(),
                    kind: "message".to_string(),
                    summary: format!("chunk {i}"),
                    detail_path: None,
                },
            );
        }
        assert_eq!(
            task.timeline
                .iter()
                .filter(|entry| entry.kind == "message")
                .count(),
            1
        );
        assert_eq!(
            task.timeline.last().map(|e| e.summary.as_str()),
            Some("chunk 49")
        );
    }

    #[test]
    fn timeline_trim_bounds_growth_and_keeps_a_head() {
        let mut task = sample_task_record();
        for i in 0..400 {
            push_timeline_entry(
                &mut task,
                TaskTimelineEntry {
                    timestamp: Utc::now(),
                    kind: "status".to_string(),
                    summary: format!("tick {i}"),
                    detail_path: None,
                },
            );
        }
        assert!(task.timeline.len() <= TIMELINE_ENTRY_LIMIT);
        assert_eq!(task.timeline[0].summary, "tick 0");
        assert!(
            task.timeline.iter().any(|entry| entry.kind == "omitted"),
            "bounded timeline should record omitted history: {:?}",
            task.timeline
                .iter()
                .map(|e| e.kind.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn never_terminalizing_execution_fails_with_idle_timeout() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(short_test_config(root), Arc::new(DeafHangExecutor))
                .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("never finish"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Failed);
        assert_eq!(finished.terminal_reason.as_deref(), Some("idle_timeout"));
        assert!(
            finished
                .error
                .as_deref()
                .is_some_and(|err| err.contains("idle")),
            "idle timeout must be visible on the receipt: {finished:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn forced_timeout_keeps_all_partial_message_output() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager = TaskManager::start_with_executor(
            short_test_config(root),
            Arc::new(PartialThenHangExecutor),
        )
        .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("retain partial result"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;

        assert_eq!(finished.status, TaskStatus::Failed);
        assert_eq!(finished.terminal_reason.as_deref(), Some("idle_timeout"));
        assert_eq!(finished.result_summary.as_deref(), Some("partial result"));
        Ok(())
    }

    #[tokio::test]
    async fn cooperative_cancel_after_idle_timeout_keeps_timeout_reason() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager = TaskManager::start_with_executor(
            short_test_config(root),
            Arc::new(CooperativeIdleCancelExecutor),
        )
        .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("cooperative idle timeout"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Failed);
        assert_eq!(finished.terminal_reason.as_deref(), Some("idle_timeout"));
        assert!(
            finished
                .error
                .as_deref()
                .is_some_and(|error| error.contains("idle")),
            "cooperative cancellation must retain the timeout receipt: {finished:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn heartbeat_status_does_not_refresh_idle_timeout() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(short_test_config(root), Arc::new(HeartbeatExecutor))
                .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("heartbeat only"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Failed);
        assert_eq!(finished.terminal_reason.as_deref(), Some("idle_timeout"));
        Ok(())
    }

    #[tokio::test]
    async fn active_progress_keeps_idle_alive_until_wall_timeout() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager = TaskManager::start_with_executor(
            short_test_config(root),
            Arc::new(ProgressHeartbeatExecutor),
        )
        .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("genuine progress"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Failed);
        assert_eq!(finished.terminal_reason.as_deref(), Some("wall_timeout"));
        Ok(())
    }

    #[tokio::test]
    async fn cooperative_cancel_after_wall_timeout_keeps_timeout_reason() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager = TaskManager::start_with_executor(
            wall_timeout_test_config(root),
            Arc::new(CooperativeProgressCancelExecutor),
        )
        .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("cooperative wall timeout"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Failed);
        assert_eq!(finished.terminal_reason.as_deref(), Some("wall_timeout"));
        assert!(
            finished
                .error
                .as_deref()
                .is_some_and(|error| error.contains("wall-time")),
            "cooperative cancellation must retain the timeout receipt: {finished:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_terminalizes_a_stuck_running_task() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(short_test_config(root), Arc::new(DeafHangExecutor))
                .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("stuck during shutdown"))
            .await?;
        wait_for_running(&manager, &task.id, Duration::from_secs(5)).await?;
        manager.shutdown();
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Canceled);
        assert_eq!(finished.terminal_reason.as_deref(), Some("shutdown"));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_cancel_signal_does_not_spin_during_grace() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let polls = Arc::new(AtomicUsize::new(0));
        let manager = TaskManager::start_with_executor(
            short_test_config(root),
            Arc::new(PollCountingHangExecutor {
                polls: Arc::clone(&polls),
            }),
        )
        .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("stuck during shutdown"))
            .await?;

        wait_for_running(&manager, &task.id, Duration::from_secs(5)).await?;

        manager.shutdown();
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.terminal_reason.as_deref(), Some("shutdown"));
        assert!(
            polls.load(Ordering::Relaxed) <= 10,
            "already-canceled shutdown signal repeatedly repolled the executor during grace: {} polls",
            polls.load(Ordering::Relaxed)
        );
        Ok(())
    }

    #[tokio::test]
    async fn forced_idle_timeout_releases_the_worker_for_later_tasks() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager = TaskManager::start_with_executor(
            short_test_config(root),
            Arc::new(PromptRouterExecutor),
        )
        .await?;
        let stuck = manager
            .add_task(NewTaskRequest::from_prompt("hang until idle timeout"))
            .await?;
        let finished =
            wait_for_terminal_state(&manager, &stuck.id, Duration::from_secs(10)).await?;
        assert_eq!(
            finished.terminal_reason.as_deref(),
            Some("idle_timeout"),
            "stuck task terminal record: {finished:?}"
        );

        let next = manager
            .add_task(NewTaskRequest::from_prompt("run after hang"))
            .await?;
        let completed =
            wait_for_terminal_state(&manager, &next.id, Duration::from_secs(10)).await?;
        assert_eq!(
            completed.status,
            TaskStatus::Completed,
            "follow-up task terminal record: {completed:?}"
        );
        assert_eq!(
            completed.terminal_reason.as_deref(),
            Some("completed"),
            "follow-up task terminal record: {completed:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancel_then_completed_result_is_recorded_as_canceled() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager = TaskManager::start_with_executor(
            test_config(root),
            Arc::new(CompleteAfterCancelExecutor),
        )
        .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("race complete after cancel"))
            .await?;
        wait_for_running(&manager, &task.id, Duration::from_secs(5)).await?;
        let cancellation = manager.cancel_task(&task.id).await?;
        assert_eq!(cancellation.disposition, TaskCancelDisposition::Requested);
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Canceled);
        assert_eq!(finished.terminal_reason.as_deref(), Some("canceled"));
        Ok(())
    }

    #[tokio::test]
    async fn long_stream_timeline_is_bounded() -> Result<()> {
        let root = std::env::temp_dir().join(format!("deepseek-task-test-{}", Uuid::new_v4()));
        let manager =
            TaskManager::start_with_executor(test_config(root.clone()), Arc::new(FloodExecutor))
                .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("flood the timeline"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(finished.status, TaskStatus::Completed);
        assert_eq!(finished.runtime_event_count, 400);
        assert!(
            finished.timeline.len() <= TIMELINE_ENTRY_LIMIT,
            "timeline grew to {}",
            finished.timeline.len()
        );
        assert!(
            finished
                .timeline
                .iter()
                .any(|entry| entry.kind == "omitted"),
            "long streams should drop older timeline entries: {:?}",
            finished
                .timeline
                .iter()
                .map(|e| e.kind.as_str())
                .collect::<Vec<_>>()
        );

        let persisted_path = root.join("tasks").join(format!("{}.json", task.id));
        let persisted: TaskRecord = serde_json::from_slice(&fs::read(&persisted_path)?)?;
        assert_eq!(persisted.status, TaskStatus::Completed);
        assert_eq!(persisted.runtime_event_count, 400);
        assert!(persisted.timeline.len() <= TIMELINE_ENTRY_LIMIT);
        assert!(
            persisted
                .timeline
                .iter()
                .any(|entry| entry.kind == "omitted")
        );
        Ok(())
    }

    async fn test_runtime_manager() -> Result<RuntimeThreadManager> {
        let root = tempfile::tempdir()?.keep();
        RuntimeThreadManager::open(
            Config::default(),
            PathBuf::from("."),
            RuntimeThreadManagerConfig::from_task_data_dir(root),
        )
    }

    async fn drain_task_events(mut rx: mpsc::Receiver<TaskExecutionEvent>) {
        while rx.recv().await.is_some() {}
    }

    struct RuntimeProjectionExecutor(Vec<(&'static str, Value)>);

    #[async_trait]
    impl TaskExecutor for RuntimeProjectionExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            cancel: CancellationToken,
        ) -> TaskExecutionResult {
            let runtime = test_runtime_manager().await.expect("fixture runtime");
            let thread = runtime
                .create_thread(CreateThreadRequest::default())
                .await
                .expect("fixture thread");
            for (event, payload) in &self.0 {
                runtime
                    .emit_event_for_test(
                        &thread.id,
                        Some("turn_projection"),
                        event,
                        payload.clone(),
                    )
                    .await
                    .expect("persist fixture runtime event");
            }
            drive_engine_turn(
                &runtime,
                &thread.id,
                "turn_projection",
                events,
                cancel,
                TaskExecutionLimits::default(),
            )
            .await
        }
    }

    async fn project_runtime_task(events: Vec<(&'static str, Value)>) -> Result<TaskRecord> {
        let root = tempfile::tempdir()?;
        let manager = TaskManager::start_with_executor(
            test_config(root.path().to_path_buf()),
            Arc::new(RuntimeProjectionExecutor(events)),
        )
        .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("runtime projection fixture"))
            .await?;
        wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        manager.shutdown_and_wait().await?;
        // Assert the durable task projection, not just an adapter event.
        let path = root.path().join("tasks").join(format!("{}.json", task.id));
        Ok(serde_json::from_slice(&fs::read(path)?)?)
    }

    #[tokio::test]
    async fn runtime_task_projection_preserves_provider_ids_and_terminal_tool_statuses()
    -> Result<()> {
        let mut events = Vec::new();
        for (item, provider) in [
            ("item_a", "call_a"),
            ("item_b", "call_b"),
            ("item_c", "call_c"),
        ] {
            events.push((
                "item.started",
                json!({
                    "item": { "id": item, "kind": "tool_call" },
                    "tool": { "id": provider, "name": "read", "input": {} }
                }),
            ));
        }
        // Parallel same-name calls finish out of order. Success preserves
        // tool_result_for; errors retain tool_use_id; redaction uses tool_call_id.
        for (item, provider, identity_key, terminal) in [
            ("item_c", "call_c", "tool_use_id", "item.failed"),
            ("item_b", "call_b", "tool_call_id", "item.completed"),
            ("item_a", "call_a", "tool_result_for", "item.completed"),
        ] {
            events.push((
                terminal,
                json!({ "item": {
                    "id": item, "kind": "tool_call", "summary": "redacted receipt",
                    "detail": format!("result for {provider}"),
                    "metadata": { identity_key: provider, "tool_name": "read" }
                }}),
            ));
        }
        // Old event shapes with no metadata retain their existing identity.
        events.extend([
            ("item.started", json!({ "tool": { "id": "legacy", "name": "read", "input": {} } })),
            ("item.completed", json!({ "item": { "id": "legacy", "kind": "tool_call", "summary": "read: ok", "detail": "ok" } })),
            ("turn.completed", json!({ "turn": { "status": "completed" } })),
        ]);
        let task = project_runtime_task(events).await?;
        assert_eq!(task.status, TaskStatus::Completed);
        assert_eq!(task.tool_calls.len(), 4);
        for (call, expected_id, expected_status) in [
            (&task.tool_calls[0], "call_a", TaskToolStatus::Success),
            (&task.tool_calls[1], "call_b", TaskToolStatus::Success),
            (&task.tool_calls[2], "call_c", TaskToolStatus::Failed),
            (&task.tool_calls[3], "legacy", TaskToolStatus::Success),
        ] {
            assert_eq!(call.id, expected_id);
            assert_eq!(call.status, expected_status);
            assert!(call.ended_at.is_some());
            assert!(call.duration_ms.is_some());
        }
        assert_eq!(
            task.tool_calls[0].output_summary.as_deref(),
            Some("result for call_a")
        );
        assert_eq!(
            task.tool_calls[2].output_summary.as_deref(),
            Some("result for call_c")
        );
        Ok(())
    }

    #[tokio::test]
    async fn runtime_task_projection_uses_last_completed_message_without_delta_duplication()
    -> Result<()> {
        let commentary = "Checking fixture state. ".repeat(20);
        let task = project_runtime_task(vec![
            ("item.started", json!({ "item": { "id": "commentary", "kind": "agent_message" } })),
            ("item.delta", json!({ "kind": "agent_message", "delta": commentary })),
            ("item.completed", json!({ "item": { "id": "commentary", "kind": "agent_message", "detail": commentary } })),
            ("item.started", json!({ "item": { "id": "final", "kind": "agent_message" } })),
            ("item.delta", json!({ "kind": "agent_message", "delta": "NOTHING_" })),
            ("item.completed", json!({ "item": { "id": "final", "kind": "agent_message", "detail": "NOTHING_TO_REPORT" } })),
            ("turn.completed", json!({ "turn": { "status": "completed" } })),
        ]).await?;
        assert_eq!(task.result_summary.as_deref(), Some("NOTHING_TO_REPORT"));
        assert!(task.result_detail_path.is_none());
        assert!(
            task.timeline.iter().any(|entry| entry.kind == "message"
                && entry.summary.starts_with("Checking fixture state."))
        );
        Ok(())
    }

    #[tokio::test]
    async fn runtime_task_projection_preserves_partial_output_only_for_unfinished_results()
    -> Result<()> {
        for (status, expected) in [
            ("interrupted", "partial final"),
            ("failed", "partial final"),
            ("completed", "(no textual output)"),
        ] {
            let task = project_runtime_task(vec![
                (
                    "item.completed",
                    json!({ "item": { "kind": "agent_message", "detail": "earlier commentary" } }),
                ),
                (
                    "item.started",
                    json!({ "item": { "kind": "agent_message" } }),
                ),
                (
                    "item.delta",
                    json!({ "kind": "agent_message", "delta": "partial final" }),
                ),
                ("turn.completed", json!({ "turn": { "status": status } })),
            ])
            .await?;
            assert_eq!(task.result_summary.as_deref(), Some(expected), "{status}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn engine_turn_without_terminal_event_idle_times_out() -> Result<()> {
        let runtime = test_runtime_manager().await?;
        let thread = runtime
            .create_thread(CreateThreadRequest::default())
            .await?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(drain_task_events(rx));
        let result = drive_engine_turn(
            &runtime,
            &thread.id,
            "turn_missing",
            tx,
            CancellationToken::new(),
            TaskExecutionLimits::short_for_tests(),
        )
        .await;
        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.terminal_reason, TaskTerminalReason::IdleTimeout);
        Ok(())
    }

    #[tokio::test]
    async fn engine_turn_keeps_idle_timeout_when_runtime_interrupts_during_grace() -> Result<()> {
        let runtime = Arc::new(test_runtime_manager().await?);
        let thread = runtime
            .create_thread(CreateThreadRequest::default())
            .await?;
        let (tx, mut rx) = mpsc::channel(64);
        let runtime_for_drive = Arc::clone(&runtime);
        let thread_id = thread.id.clone();
        let drive = tokio::spawn(async move {
            drive_engine_turn(
                runtime_for_drive.as_ref(),
                &thread_id,
                "turn_timeout",
                tx,
                CancellationToken::new(),
                TaskExecutionLimits {
                    wall_time: Duration::from_secs(2),
                    idle_progress: Duration::from_millis(80),
                    cancel_grace: Duration::from_millis(500),
                    persist_debounce: Duration::from_millis(10),
                },
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match rx.recv().await {
                    Some(TaskExecutionEvent::Status { message })
                        if message.contains("idle deadline") =>
                    {
                        break;
                    }
                    Some(_) => {}
                    None => panic!("engine task event stream closed before timeout interrupt"),
                }
            }
        })
        .await
        .context("engine did not request the idle-timeout interrupt")?;

        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_timeout"),
                "turn.completed",
                json!({ "turn": { "status": "interrupted" } }),
            )
            .await?;
        let result = drive.await?;
        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.terminal_reason, TaskTerminalReason::IdleTimeout);
        Ok(())
    }

    #[tokio::test]
    async fn engine_turn_uses_cursor_catchup_for_completed_event() -> Result<()> {
        let runtime = test_runtime_manager().await?;
        let thread = runtime
            .create_thread(CreateThreadRequest::default())
            .await?;
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_done"),
                "turn.completed",
                json!({ "turn": { "status": "completed" } }),
            )
            .await?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(drain_task_events(rx));
        let result = drive_engine_turn(
            &runtime,
            &thread.id,
            "turn_done",
            tx,
            CancellationToken::new(),
            TaskExecutionLimits::short_for_tests(),
        )
        .await;
        assert_eq!(result.status, TaskStatus::Completed);
        assert_eq!(result.terminal_reason, TaskTerminalReason::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn pending_approval_suspends_idle_and_timeout_denial_settles_failed() -> Result<()> {
        // #6118: a run that needs a tool approval must not die as a silent
        // idle-timeout cancel; the pending approval suspends the idle
        // watchdog, and the bridge's own deadline denial then settles the
        // run Failed with the reason recorded.
        let runtime = Arc::new(test_runtime_manager().await?);
        let thread = runtime
            .create_thread(CreateThreadRequest::default())
            .await?;
        let thread_id = thread.id.clone();
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_approval"),
                "approval.required",
                json!({
                    "approval_id": "approval_fixture_1",
                    "tool_call_id": "call_fixture_1",
                    "tool_name": "shell",
                }),
            )
            .await?;
        let (tx, mut rx) = mpsc::channel(64);
        let runtime_for_drive = Arc::clone(&runtime);
        let drive = tokio::spawn(async move {
            drive_engine_turn(
                runtime_for_drive.as_ref(),
                &thread_id,
                "turn_approval",
                tx,
                CancellationToken::new(),
                TaskExecutionLimits {
                    wall_time: Duration::from_secs(5),
                    idle_progress: Duration::from_millis(120),
                    cancel_grace: Duration::from_millis(200),
                    persist_debounce: Duration::from_millis(10),
                },
            )
            .await
        });

        // Well past the idle window, the pending approval must keep the run
        // alive; the old behavior killed it here with no receipt.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            !drive.is_finished(),
            "a pending approval must suspend the idle watchdog (#6118)"
        );
        while let Ok(event) = rx.try_recv() {
            if let TaskExecutionEvent::Status { message } = event {
                assert!(
                    !message.contains("idle deadline"),
                    "no idle interrupt may fire while an approval is pending: {message}"
                );
            }
        }

        // The decision window closes: the run settles Failed with the reason.
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_approval"),
                "approval.timeout",
                json!({ "approval_id": "approval_fixture_1", "tool_call_id": "call_fixture_1" }),
            )
            .await?;
        let result = tokio::time::timeout(Duration::from_secs(2), drive)
            .await
            .context("the decision-window denial must settle the run promptly")??;
        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.terminal_reason, TaskTerminalReason::Failed);
        assert!(
            result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Tool approval was not answered")),
            "the run must record why it stopped, got {:?}",
            result.error
        );
        Ok(())
    }

    #[tokio::test]
    async fn resolved_approval_restores_the_idle_watchdog() -> Result<()> {
        // #6118 counter-check: the suspension ends with the decision, so a
        // run that then stops making progress is idle-killed exactly as
        // before.
        let runtime = test_runtime_manager().await?;
        let thread = runtime
            .create_thread(CreateThreadRequest::default())
            .await?;
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_approval_resolved"),
                "approval.required",
                json!({
                    "approval_id": "approval_fixture_2",
                    "tool_call_id": "call_fixture_2",
                    "tool_name": "shell",
                }),
            )
            .await?;
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_approval_resolved"),
                "approval.decided",
                json!({
                    "approval_id": "approval_fixture_2",
                    "tool_call_id": "call_fixture_2",
                    "decision": "allow",
                }),
            )
            .await?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(drain_task_events(rx));
        let result = drive_engine_turn(
            &runtime,
            &thread.id,
            "turn_approval_resolved",
            tx,
            CancellationToken::new(),
            TaskExecutionLimits::short_for_tests(),
        )
        .await;
        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.terminal_reason, TaskTerminalReason::IdleTimeout);
        Ok(())
    }

    #[tokio::test]
    async fn silent_running_tool_suppresses_idle_until_it_completes() -> Result<()> {
        let runtime = Arc::new(test_runtime_manager().await?);
        let thread = runtime
            .create_thread(CreateThreadRequest::default())
            .await?;
        let (tx, mut rx) = mpsc::channel(64);
        let runtime_for_drive = Arc::clone(&runtime);
        let thread_id = thread.id.clone();
        let drive = tokio::spawn(async move {
            drive_engine_turn(
                runtime_for_drive.as_ref(),
                &thread_id,
                "turn_silent_tool",
                tx,
                CancellationToken::new(),
                TaskExecutionLimits {
                    wall_time: Duration::from_secs(5),
                    idle_progress: Duration::from_millis(80),
                    cancel_grace: Duration::from_millis(500),
                    persist_debounce: Duration::from_millis(10),
                },
            )
            .await
        });

        // Keyed by the journal item id on the started edge; the payload also
        // carries the engine tool-use id under "tool" (which must NOT become
        // the tracking key: the terminal events only repeat "item").
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_silent_tool"),
                "item.started",
                json!({
                    "item": { "id": "item_tool_silent", "status": "in_progress" },
                    "tool": { "id": "call_build", "name": "exec_command", "input": {} }
                }),
            )
            .await?;

        // A silent build produces no journal traffic for its whole window;
        // far past the idle deadline the watchdog must still not fire.
        tokio::time::sleep(Duration::from_millis(400)).await;
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            seen.push(event);
        }
        assert!(
            !seen.iter().any(|event| matches!(
                event,
                TaskExecutionEvent::Status { message }
                    if message.contains("idle deadline")
            )),
            "idle watchdog fired while a tool was still running"
        );

        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_silent_tool"),
                "item.completed",
                json!({ "item": { "id": "item_tool_silent", "status": "completed" } }),
            )
            .await?;

        // Completion drains the set; silence afterwards must let the idle
        // deadline fire again (a drain bug would surface as WallTimeout).
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match rx.recv().await {
                    Some(TaskExecutionEvent::Status { message })
                        if message.contains("idle deadline") =>
                    {
                        break;
                    }
                    Some(_) => {}
                    None => panic!("task event stream closed before idle deadline"),
                }
            }
        })
        .await
        .context("idle deadline did not fire after the tool completed")?;
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_silent_tool"),
                "turn.completed",
                json!({ "turn": { "status": "interrupted" } }),
            )
            .await?;
        let result = drive.await?;
        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.terminal_reason, TaskTerminalReason::IdleTimeout);
        Ok(())
    }

    #[tokio::test]
    async fn silent_running_tool_heartbeats_the_worker_watchdog() -> Result<()> {
        // The supervisor's guard (run_task) only sees task events, so the
        // steady-state suppression must also publish the in-flight window on
        // the event channel — otherwise the production path still idles out
        // even though this loop's own guard is satisfied.
        let runtime = Arc::new(test_runtime_manager().await?);
        let thread = runtime
            .create_thread(CreateThreadRequest::default())
            .await?;
        let (tx, mut rx) = mpsc::channel(64);
        let runtime_for_drive = Arc::clone(&runtime);
        let thread_id = thread.id.clone();
        let drive = tokio::spawn(async move {
            drive_engine_turn(
                runtime_for_drive.as_ref(),
                &thread_id,
                "turn_heartbeat",
                tx,
                CancellationToken::new(),
                TaskExecutionLimits {
                    wall_time: Duration::from_secs(5),
                    idle_progress: Duration::from_millis(80),
                    cancel_grace: Duration::from_millis(500),
                    persist_debounce: Duration::from_millis(10),
                },
            )
            .await
        });

        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_heartbeat"),
                "item.started",
                json!({
                    "item": { "id": "item_tool_hb", "status": "in_progress" },
                    "tool": { "id": "call_build", "name": "exec_command", "input": {} }
                }),
            )
            .await?;

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match rx.recv().await {
                    Some(TaskExecutionEvent::ToolHeartbeat) => break,
                    Some(_) => {}
                    None => panic!("task event stream closed before any heartbeat"),
                }
            }
        })
        .await
        .context("no ToolHeartbeat reached the worker channel while a tool was in flight")?;

        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_heartbeat"),
                "item.completed",
                json!({ "item": { "id": "item_tool_hb", "status": "completed" } }),
            )
            .await?;
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_heartbeat"),
                "turn.completed",
                json!({ "turn": { "status": "completed" } }),
            )
            .await?;
        let result = drive.await?;
        assert_eq!(result.status, TaskStatus::Completed);
        Ok(())
    }

    #[tokio::test]
    async fn hung_running_tool_still_hits_the_wall_budget() -> Result<()> {
        let runtime = Arc::new(test_runtime_manager().await?);
        let thread = runtime
            .create_thread(CreateThreadRequest::default())
            .await?;
        let (tx, mut rx) = mpsc::channel(64);
        let runtime_for_drive = Arc::clone(&runtime);
        let thread_id = thread.id.clone();
        let drive = tokio::spawn(async move {
            drive_engine_turn(
                runtime_for_drive.as_ref(),
                &thread_id,
                "turn_hung_tool",
                tx,
                CancellationToken::new(),
                TaskExecutionLimits {
                    wall_time: Duration::from_millis(300),
                    idle_progress: Duration::from_millis(80),
                    cancel_grace: Duration::from_millis(500),
                    persist_debounce: Duration::from_millis(10),
                },
            )
            .await
        });

        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_hung_tool"),
                "item.started",
                json!({
                    "item": { "id": "item_tool_hung", "status": "in_progress" },
                    "tool": { "id": "call_hang", "name": "exec_command", "input": {} }
                }),
            )
            .await?;

        // The running-tool suppression only defers the idle watchdog; the
        // wall-time budget remains the backstop for a tool that never
        // completes.
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match rx.recv().await {
                    Some(TaskExecutionEvent::Status { message })
                        if message.contains("wall-time") =>
                    {
                        break;
                    }
                    Some(_) => {}
                    None => panic!("task event stream closed before wall deadline"),
                }
            }
        })
        .await
        .context("wall budget did not fire during a hung tool")?;
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_hung_tool"),
                "turn.completed",
                json!({ "turn": { "status": "interrupted" } }),
            )
            .await?;
        let result = drive.await?;
        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.terminal_reason, TaskTerminalReason::WallTimeout);
        Ok(())
    }

    /// Mirrors the fixed runtime contract: a ToolStarted edge, then a silent
    /// window longer than the idle deadline fed only by ToolHeartbeat, then
    /// completion. Pins the supervisor side of the heartbeat chain — the
    /// worker idle watchdog must count heartbeats as progress.
    ///
    /// Margins: heartbeats every 30 ms across a 1.2 s window against a 500 ms
    /// idle deadline. The window still outlasts the deadline (so the test
    /// fails if heartbeats stop counting), while a loaded runner would need a
    /// half-second stall to starve it; the 150 ms default deadline did starve
    /// on hosted Windows.
    struct ToolHeartbeatExecutor;

    const HEARTBEAT_TEST_IDLE_PROGRESS: Duration = Duration::from_millis(500);
    const HEARTBEAT_TEST_TICKS: u32 = 40;

    #[async_trait]
    impl TaskExecutor for ToolHeartbeatExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            cancel: CancellationToken,
        ) -> TaskExecutionResult {
            let _ = events
                .send(TaskExecutionEvent::ToolStarted {
                    id: "tool_hb".to_string(),
                    name: "exec_command".to_string(),
                    input: serde_json::json!({}),
                })
                .await;
            for _ in 0..HEARTBEAT_TEST_TICKS {
                sleep(Duration::from_millis(30)).await;
                if cancel.is_cancelled() {
                    return TaskExecutionResult {
                        status: TaskStatus::Canceled,
                        result_text: None,
                        error: None,
                        terminal_reason: TaskTerminalReason::Canceled,
                    };
                }
                let _ = events.send(TaskExecutionEvent::ToolHeartbeat).await;
            }
            let _ = events
                .send(TaskExecutionEvent::ToolCompleted {
                    id: "tool_hb".to_string(),
                    name: "exec_command".to_string(),
                    success: true,
                    output: "build ok".to_string(),
                    metadata: None,
                })
                .await;
            TaskExecutionResult {
                status: TaskStatus::Completed,
                result_text: Some("done".to_string()),
                error: None,
                terminal_reason: TaskTerminalReason::Completed,
            }
        }
    }

    #[tokio::test]
    async fn worker_supervisor_honors_tool_heartbeats_during_silent_tools() -> Result<()> {
        let root = tempfile::tempdir()?;
        let mut config = short_test_config(root.path().to_path_buf());
        config.execution_limits.wall_time = Duration::from_secs(5);
        config.execution_limits.idle_progress = HEARTBEAT_TEST_IDLE_PROGRESS;
        let manager =
            TaskManager::start_with_executor(config, Arc::new(ToolHeartbeatExecutor)).await?;

        let task = manager
            .add_task(NewTaskRequest::from_prompt("silent build with heartbeats"))
            .await?;
        let finished = wait_for_terminal_state(&manager, &task.id, Duration::from_secs(10)).await?;
        assert_eq!(
            finished.status,
            TaskStatus::Completed,
            "heartbeats from a silent in-flight tool must keep the worker idle watchdog fed"
        );
        assert!(
            !finished
                .timeline
                .iter()
                .any(|entry| entry.summary.contains("heartbeat")),
            "heartbeats are supervisor-side liveness only and must not surface on the timeline"
        );
        Ok(())
    }

    struct ExposedTaskEventsExecutor {
        ready: mpsc::UnboundedSender<mpsc::Sender<TaskExecutionEvent>>,
    }

    #[async_trait]
    impl TaskExecutor for ExposedTaskEventsExecutor {
        async fn execute(
            &self,
            _task: ExecutionTask,
            events: mpsc::Sender<TaskExecutionEvent>,
            cancel: CancellationToken,
        ) -> TaskExecutionResult {
            let _ = self.ready.send(events);
            cancel.cancelled().await;
            TaskExecutionResult::from_reason(TaskTerminalReason::Canceled, None)
        }
    }

    #[tokio::test]
    async fn tool_heartbeat_is_liveness_only_and_never_persists() -> Result<()> {
        // Drive the actual supervisor beyond both persistence triggers: more
        // heartbeats than the pending-event cap, then an idle debounce window.
        let root = tempfile::tempdir()?;
        let config = test_config(root.path().to_path_buf());
        let debounce = config.execution_limits.persist_debounce;
        assert!(EVENT_CATCHUP_POLL < debounce);
        let (ready, mut receiver) = mpsc::unbounded_channel();
        let manager =
            TaskManager::start_with_executor(config, Arc::new(ExposedTaskEventsExecutor { ready }))
                .await?;
        let task = manager
            .add_task(NewTaskRequest::from_prompt("heartbeat persistence pin"))
            .await?;
        let events = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .context("task never reached its executor")?
            .context("executor did not expose its event channel")?;
        let path = manager.tasks_dir.join(format!("{}.json", task.id));
        let before_bytes = fs::read(&path)?;
        let before_fingerprint = StoreFingerprint::read(&path);
        let before_loads = manager.store_loads.load(Ordering::Relaxed);
        let running: TaskRecord = serde_json::from_slice(&before_bytes)?;
        assert_eq!(running.status, TaskStatus::Running);

        for _ in 0..TASK_EVENT_CHANNEL_CAPACITY + 8 {
            events.send(TaskExecutionEvent::ToolHeartbeat).await?;
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while events.capacity() != TASK_EVENT_CHANNEL_CAPACITY {
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .context("supervisor did not consume the heartbeat burst")?;
        sleep(debounce * 3).await;
        assert_eq!(
            manager.store_loads.load(Ordering::Relaxed),
            before_loads,
            "heartbeats must not even reload the persistent task store"
        );
        assert!(
            !manager
                .state
                .lock()
                .await
                .pending_events
                .contains_key(&task.id),
            "heartbeats must not enter the pending persistence queue"
        );
        assert_eq!(fs::read(&path)?, before_bytes);
        assert_eq!(
            StoreFingerprint::read(&path),
            before_fingerprint,
            "heartbeat traffic must not rewrite even byte-identical task records"
        );

        // Keep the production heartbeat cadence faster than the default 250ms
        // debounce until a real delta is saved. Neither the beats nor the
        // supervisor's ownership-poll wakes may restart its persistence window.
        events.try_send(TaskExecutionEvent::MessageDelta {
            content: "real progress survives".to_string(),
        })?;
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut heartbeat = tokio::time::interval(EVENT_CATCHUP_POLL);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                heartbeat.tick().await;
                events.send(TaskExecutionEvent::ToolHeartbeat).await?;
                let saved: TaskRecord = serde_json::from_slice(&fs::read(&path)?)?;
                if saved.timeline.iter().any(|entry| {
                    entry.kind == "message" && entry.summary == "real progress survives"
                }) {
                    break Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await
        .context("continuous heartbeats starved the real delta's persistence deadline")??;

        // An urgent edge flushes a newly dirty delta as well as its own data,
        // then clears that delta's outstanding deadline.
        events.try_send(TaskExecutionEvent::MessageDelta {
            content: "urgent progress survives".to_string(),
        })?;
        events.try_send(TaskExecutionEvent::ToolStarted {
            id: "item-1".into(),
            name: "bash".into(),
            input: serde_json::json!({}),
        })?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let saved: TaskRecord = serde_json::from_slice(&fs::read(&path)?)?;
                if saved
                    .tool_calls
                    .iter()
                    .any(|call| call.id == "item-1" && call.status == TaskToolStatus::Running)
                {
                    assert!(
                        saved
                            .timeline
                            .iter()
                            .any(|entry| entry.kind == "tool_started")
                    );
                    assert!(saved.timeline.iter().any(|entry| {
                        entry.kind == "message" && entry.summary == "urgent progress survives"
                    }));
                    assert!(
                        !saved
                            .timeline
                            .iter()
                            .any(|entry| entry.summary.contains("heartbeat"))
                    );
                    break Ok::<_, anyhow::Error>(());
                }
                sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .context("real tool lifecycle edge was not persisted")??;
        assert_ne!(StoreFingerprint::read(&path), before_fingerprint);
        let after_urgent = StoreFingerprint::read(&path);
        sleep(debounce * 2).await;
        assert_eq!(
            StoreFingerprint::read(&path),
            after_urgent,
            "successful urgent persistence must not leave a pending write deadline"
        );
        manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test]
    async fn engine_turn_prefers_runtime_terminal_over_cancel_grace() -> Result<()> {
        let runtime = test_runtime_manager().await?;
        let thread = runtime
            .create_thread(CreateThreadRequest::default())
            .await?;
        let cancel = CancellationToken::new();
        cancel.cancel();
        runtime
            .emit_event_for_test(
                &thread.id,
                Some("turn_done"),
                "turn.completed",
                json!({ "turn": { "status": "interrupted" } }),
            )
            .await?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(drain_task_events(rx));
        let result = drive_engine_turn(
            &runtime,
            &thread.id,
            "turn_done",
            tx,
            cancel,
            TaskExecutionLimits::short_for_tests(),
        )
        .await;
        assert_eq!(result.status, TaskStatus::Canceled);
        assert_eq!(result.terminal_reason, TaskTerminalReason::Canceled);
        Ok(())
    }

    #[tokio::test]
    async fn runtime_store_failure_event_reaches_the_task_timeline() -> Result<()> {
        // #5931: the runtime's own store fault lands in the task timeline,
        // and a terminal one stops the driver instead of idling it out.
        let (tx, mut rx) = mpsc::channel(8);
        let mut final_text = RuntimeTaskOutput::default();
        let path = "/tmp/runtime/turns/turn_store.json";
        let event = RuntimeEventRecord {
            schema_version: 1,
            seq: 7,
            timestamp: Utc::now(),
            thread_id: "thr_store".to_string(),
            turn_id: Some("turn_store".to_string()),
            item_id: None,
            event: RUNTIME_STORE_FAILURE_EVENT.to_string(),
            payload: json!({
                "operation": "read",
                "record_kind": "turn",
                "record_id": "turn_store",
                "path": path,
                "error": format!("Failed to read turn {path}: No such file"),
                "reason": "No such file",
                "next_action": format!("Move {path} aside (or delete it) and retry."),
                "message": format!(
                    "Session runtime store: turn turn_store at {path} could not be read: No such file. Move {path} aside (or delete it) and retry."
                ),
            }),
        };

        assert!(
            ingest_runtime_event(&event, &mut final_text, &tx)
                .await
                .is_none(),
            "a non-terminal store fault leaves the driver waiting"
        );
        let mut saw_error = false;
        while let Ok(received) = rx.try_recv() {
            if let TaskExecutionEvent::Error { message } = received {
                assert!(message.contains(path), "{message}");
                saw_error = true;
            }
        }
        assert!(saw_error, "store fault missing from the task timeline");

        let mut terminal = event.clone();
        terminal.payload["terminal"] = json!(true);
        let (status, error) = ingest_runtime_event(&terminal, &mut final_text, &tx)
            .await
            .expect("a terminal store fault ends the turn");
        assert_eq!(status, RuntimeTurnStatus::Failed);
        assert!(error.is_some_and(|message| message.contains(path)));
        Ok(())
    }
}

#[cfg(test)]
mod ownership_tests;

//! WorkflowPanel — one workflow run's state, reduced from its event stream.
//!
//! Issue #4121 (CODEWHALE_0_8_68 §2.4). Progress lives off the transcript: the
//! workbar under the composer (`workbar.rs`) paints one row per run from this
//! state, and `/workflows` holds the detail. Events are applied through
//! [`WorkflowPanelEvent`].
//!
//! Issue #4122 routes the same event stream into a compact history card that
//! reuses this state machine: collapsed summarizes lifecycle/children/phases/
//! failures/elapsed; expanded adds phase/child summaries, artifact links,
//! final result, and failure details. Direct sub-agent cards share helpers
//! from this module where practical.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::{Value, json};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_tui_kit::ui_text::truncate_line_to_width;

/// Maximum phase summary chips shown in the expanded body.
const MAX_PHASE_SUMMARY: usize = 6;
/// Newest rejected dispatches retained by the panel. The workflow journal is
/// the durable, unbounded source of truth; this is only a compact UI tail.
const MAX_DISPATCH_FAILURES_RETAINED: usize = 12;
/// Rejected dispatches shown at once in the live panel/history body.
const MAX_VISIBLE_DISPATCH_FAILURES: usize = 3;
/// Columns the transcript card header gives its summary
/// (`history::constants::TOOL_HEADER_SUMMARY_LIMIT`); the finish line's facts
/// are fitted to it so the header never cuts the counts.
const FINISH_FACTS_COLS: usize = 72;
/// The title keeps at least this much of itself on the finish line.
const MIN_FINISH_TITLE_COLS: usize = 16;

/// Lifecycle of the active (or most recently completed) workflow run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowPanelLifecycle {
    Pending,
    Running,
    Succeeded,
    /// The workflow returned usable output but one or more task slots failed.
    Degraded,
    Failed,
    Cancelled,
}

impl WorkflowPanelLifecycle {
    #[must_use]
    pub fn is_running(self) -> bool {
        matches!(self, Self::Running | Self::Pending)
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Degraded | Self::Failed | Self::Cancelled
        )
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "success",
            Self::Degraded => "degraded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn display_label(self, locale: Locale) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Degraded => tr(locale, MessageId::WorkflowStatusDegraded),
            other => std::borrow::Cow::Borrowed(other.label()),
        }
    }

    fn color(self) -> ratatui::style::Color {
        match self {
            Self::Pending => palette::TEXT_MUTED,
            // A healthy run is accent, not attention: warning/error colours
            // are reserved for states that need the user (#6503).
            Self::Running => palette::WHALE_ACTION,
            Self::Succeeded => palette::STATUS_SUCCESS,
            Self::Degraded => palette::STATUS_WARNING,
            Self::Failed => palette::STATUS_ERROR,
            Self::Cancelled => palette::TEXT_MUTED,
        }
    }
}

/// Per-task / per-worker row status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowRowStatus {
    Pending,
    Running,
    Waiting,
    Succeeded,
    Failed,
    Cancelled,
    SchemaFailed,
}

impl WorkflowRowStatus {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Succeeded => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::SchemaFailed => "schema",
        }
    }

    /// Localized display variant of [`Self::label`]. `label()` stays
    /// English because it doubles as the machine-readable `status` token in
    /// [`WorkflowPanel::to_run_json`]; this method is for rendered rows only.
    #[must_use]
    pub fn display_label(self, locale: Locale) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Waiting => tr(locale, MessageId::WorkflowStatusWaiting),
            other => std::borrow::Cow::Borrowed(other.label()),
        }
    }

    #[must_use]
    pub fn is_running(self) -> bool {
        matches!(self, Self::Pending | Self::Running | Self::Waiting)
    }

    #[must_use]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failed | Self::SchemaFailed)
    }

    #[must_use]
    pub fn is_cancel(self) -> bool {
        matches!(self, Self::Cancelled)
    }

    fn color(self) -> ratatui::style::Color {
        match self {
            Self::Pending => palette::TEXT_MUTED,
            // Running is the normal state of a live row, so it reads neutral
            // (#6503). Waiting covers `needs_user` / `blocked`, which do want
            // attention; failure red stays reserved for actual failure.
            Self::Running => palette::TEXT_TOOL_OUTPUT,
            Self::Waiting => palette::STATUS_WARNING,
            Self::Succeeded => palette::STATUS_SUCCESS,
            Self::Failed | Self::SchemaFailed => palette::STATUS_ERROR,
            Self::Cancelled => palette::TEXT_MUTED,
        }
    }

    fn from_ir_status(status: &str) -> Self {
        match status {
            "succeeded" | "completed" | "success" | "done" => Self::Succeeded,
            "failed" | "error" => Self::Failed,
            "cancelled" | "canceled" => Self::Cancelled,
            "budget_exceeded" => Self::Failed,
            "running" => Self::Running,
            "waiting" | "blocked" | "needs_user" => Self::Waiting,
            "pending" => Self::Pending,
            other if other.contains("schema") => Self::SchemaFailed,
            _ => Self::Failed,
        }
    }
}

/// Closed route-source vocabulary minted by the spawn resolver. Persisted
/// journals are untrusted input: an unrecognized value stays unknown rather
/// than becoming UI copy (#4039).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowRouteSource {
    TaskModel,
    TaskModelStrength,
    AgentProfileModel,
    AgentProfileLoadout,
    RoleDefault,
    RunModel,
}

impl WorkflowRouteSource {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "task.model" => Some(Self::TaskModel),
            "task.model_strength" => Some(Self::TaskModelStrength),
            "agent_profile.model" => Some(Self::AgentProfileModel),
            "agent_profile.loadout" => Some(Self::AgentProfileLoadout),
            "role.default" => Some(Self::RoleDefault),
            "run.model" => Some(Self::RunModel),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::TaskModel => "task.model",
            Self::TaskModelStrength => "task.model_strength",
            Self::AgentProfileModel => "agent_profile.model",
            Self::AgentProfileLoadout => "agent_profile.loadout",
            Self::RoleDefault => "role.default",
            Self::RunModel => "run.model",
        }
    }
}

/// Closed token provenance carried by a terminal usage receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTokenSource {
    ProviderReported,
    Estimated,
}

impl WorkflowTokenSource {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "provider_reported" => Some(Self::ProviderReported),
            "estimated" => Some(Self::Estimated),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderReported => "provider_reported",
            Self::Estimated => "estimated",
        }
    }
}

/// Immutable route captured by the task-started event (#4039, #5305).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowRowRoute {
    pub child_route: Option<codewhale_engine::tools::subagent::ChildRouteReceipt>,
    pub role: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub requested_reasoning: Option<String>,
    pub effective_reasoning: Option<String>,
    pub route_source: Option<WorkflowRouteSource>,
}

impl WorkflowRowRoute {
    fn from_json(value: &Value) -> Self {
        let child_route: Option<codewhale_engine::tools::subagent::ChildRouteReceipt> = value
            .get("child_route")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok());
        let receipt = child_route.as_ref();
        Self {
            role: receipt
                .map(|receipt| receipt.canonical_role.clone())
                .or_else(|| opt_str(value, "resolved_role"))
                .or_else(|| opt_str(value, "role")),
            provider: receipt
                .map(|receipt| receipt.provider_id.clone())
                .or_else(|| opt_str(value, "resolved_provider"))
                .or_else(|| opt_str(value, "provider")),
            model: receipt
                .map(|receipt| receipt.model_id.clone())
                .or_else(|| opt_str(value, "resolved_model")),
            requested_reasoning: receipt
                .map(|receipt| receipt.requested_reasoning.clone())
                .or_else(|| opt_str(value, "requested_reasoning"))
                .or_else(|| opt_str(value, "thinking")),
            effective_reasoning: receipt
                .and_then(|receipt| receipt.effective_reasoning.clone())
                .or_else(|| opt_str(value, "effective_reasoning")),
            route_source: receipt
                .and_then(|receipt| WorkflowRouteSource::parse(&receipt.route_source))
                .or_else(|| {
                    opt_str(value, "route_source")
                        .as_deref()
                        .and_then(WorkflowRouteSource::parse)
                }),
            child_route,
        }
    }

    fn field(value: Option<&String>, locale: Locale) -> String {
        value
            .map(String::as_str)
            .map(crate::tui::app::bound_agent_activity_text)
            .map(|value| {
                value
                    .chars()
                    .map(|ch| if ch.is_control() { ' ' } else { ch })
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| tr(locale, MessageId::WorkflowReceiptUnknown).into_owned())
    }
}

/// Optional terminal usage receipt from `task_completed` (#4039).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkflowRowUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub tool_calls: Option<u32>,
    pub duration_ms: Option<u64>,
    pub token_source: Option<WorkflowTokenSource>,
}

impl WorkflowRowUsage {
    fn token_total(&self) -> Option<u64> {
        self.total_tokens
            .or_else(|| match (self.input_tokens, self.output_tokens) {
                (Some(input), Some(output)) => Some(input.saturating_add(output)),
                _ => None,
            })
    }

    fn token_source_label(&self, locale: Locale) -> String {
        match self.token_source {
            Some(WorkflowTokenSource::ProviderReported) => {
                tr(locale, MessageId::WorkflowReceiptProviderReported).into_owned()
            }
            Some(WorkflowTokenSource::Estimated) => {
                tr(locale, MessageId::WorkflowReceiptEstimated).into_owned()
            }
            None => tr(locale, MessageId::WorkflowReceiptUnknown).into_owned(),
        }
    }
}

/// One worker/task row under a phase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPanelRow {
    pub task_id: String,
    pub label: String,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub strength: Option<String>,
    pub worktree: bool,
    pub workspace: Option<PathBuf>,
    pub status: WorkflowRowStatus,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub error: Option<String>,
    pub schema_error: Option<String>,
    pub route: WorkflowRowRoute,
    pub usage: Option<WorkflowRowUsage>,
}

/// One lane gate status line surfaced by the Workflow runtime (#4179).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPanelGateLine {
    pub gate_id: String,
    pub role: Option<String>,
    pub gate: Option<String>,
    pub state: String,
    pub blocked_role: Option<String>,
    pub blocked_reason: Option<String>,
}

/// One ordered phase group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPanelPhase {
    pub title: String,
    pub rows: Vec<WorkflowPanelRow>,
}

/// One workflow task dispatch rejected before a child agent existed.
///
/// This deliberately does not reuse [`WorkflowPanelRow`]: counting a rejected
/// launch as a child would make the panel's child/receipt totals dishonest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPanelDispatchFailure {
    pub label: Option<String>,
    pub phase: Option<String>,
    pub message: String,
    pub at_ms: u64,
}

impl WorkflowPanelDispatchFailure {
    fn bounded(label: Option<String>, phase: Option<String>, message: String, at_ms: u64) -> Self {
        let bounded = |value: String| {
            crate::tui::app::bound_agent_activity_text(&value)
                .chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        let label = label.map(bounded).filter(|value| !value.is_empty());
        let phase = phase.map(bounded).filter(|value| !value.is_empty());
        let message = bounded(message);
        Self {
            label,
            phase,
            message,
            at_ms,
        }
    }
}

impl WorkflowPanelPhase {
    fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            rows: Vec::new(),
        }
    }

    fn counts(&self) -> (usize, usize, usize, usize) {
        let mut done = 0usize;
        let mut running = 0usize;
        let mut failed = 0usize;
        let mut cancelled = 0usize;
        for row in &self.rows {
            match row.status {
                WorkflowRowStatus::Succeeded => done += 1,
                WorkflowRowStatus::Running
                | WorkflowRowStatus::Pending
                | WorkflowRowStatus::Waiting => running += 1,
                WorkflowRowStatus::Failed | WorkflowRowStatus::SchemaFailed => failed += 1,
                WorkflowRowStatus::Cancelled => cancelled += 1,
            }
        }
        (done, running, failed, cancelled)
    }

    /// Labelled non-zero counts, e.g. `4 running · 1 done` (#6503). Replaces
    /// the unlabelled `[0✓ 5… 0! 0⊘]` glyph counters.
    fn counts_text(&self, locale: Locale, separator: &str) -> String {
        let (done, running, failed, cancelled) = self.counts();
        let counts = [
            (running, MessageId::WorkflowCountRunning),
            (done, MessageId::WorkflowCountDone),
            (failed, MessageId::WorkflowCountFailed),
            (cancelled, MessageId::WorkflowCountCancelled),
        ]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, id)| count_text(locale, id, count))
        .collect::<Vec<_>>();
        if counts.is_empty() {
            tr(locale, MessageId::WorkflowNoTasksYet).into_owned()
        } else {
            counts.join(separator)
        }
    }
}

/// Events the panel understands. Mirrors the tool-side `WorkflowUiEvent`
/// shape so #4122 can forward JSON without re-encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowPanelEvent {
    RunStarted {
        run_id: String,
        workflow_id: Option<String>,
        workflow_goal: Option<String>,
        source_path: Option<PathBuf>,
        token_budget: Option<u64>,
        at_ms: u64,
    },
    RunCompleted {
        status: WorkflowPanelLifecycle,
        error: Option<String>,
        at_ms: u64,
    },
    RunCancelled {
        reason: String,
        at_ms: u64,
    },
    PhaseStarted {
        title: String,
        at_ms: u64,
    },
    TaskStarted {
        task_id: String,
        label: Option<String>,
        profile: Option<String>,
        model: Option<String>,
        strength: Option<String>,
        resolved_model: Option<String>,
        worktree: bool,
        workspace: Option<PathBuf>,
        /// Launch receipt carried by this event (#4039).
        route: Box<WorkflowRowRoute>,
        at_ms: u64,
    },
    TaskCompleted {
        task_id: String,
        status: WorkflowRowStatus,
        /// Terminal usage receipt carried by this event, if any (#4039).
        usage: Option<WorkflowRowUsage>,
        /// Why a failed or cancelled task ended (`reason` on the wire). The
        /// run's own error is often an aggregate ("all 2 task(s) failed"); the
        /// task's reason is the cause a person can act on.
        reason: Option<String>,
        at_ms: u64,
    },
    GateUpdated {
        gate_id: String,
        role: Option<String>,
        gate: Option<String>,
        state: String,
        blocked_role: Option<String>,
        blocked_reason: Option<String>,
        at_ms: u64,
    },
    TaskSchemaValidationFailed {
        task_id: String,
        message: String,
        at_ms: u64,
    },
    TaskDispatchFailed {
        label: Option<String>,
        phase: Option<String>,
        message: String,
        at_ms: u64,
    },
    BudgetUpdated {
        total: Option<u64>,
        spent: u64,
        remaining: Option<u64>,
        at_ms: u64,
    },
}

impl WorkflowPanelEvent {
    /// Parse one flattened tool UI event (`{"type":"…", …}`).
    pub fn from_json_value(value: &Value) -> Option<Self> {
        let event_type = value.get("type")?.as_str()?;
        let at_ms = value
            .get("at_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(now_ms);
        match event_type {
            "run_started" => Some(Self::RunStarted {
                run_id: value
                    .get("run_id")
                    .and_then(Value::as_str)
                    .unwrap_or("workflow")
                    .to_string(),
                workflow_id: opt_str(value, "workflow_id"),
                workflow_goal: opt_str(value, "workflow_goal"),
                source_path: opt_str(value, "source_path").map(PathBuf::from),
                token_budget: value.get("token_budget").and_then(Value::as_u64),
                at_ms,
            }),
            "run_completed" => {
                // A terminal receipt without a readable status is not
                // evidence of success; it fails closed like an unknown one.
                let status = value
                    .get("status")
                    .and_then(Value::as_str)
                    .map(lifecycle_from_status)
                    .unwrap_or(WorkflowPanelLifecycle::Failed);
                Some(Self::RunCompleted {
                    status,
                    error: opt_str(value, "error"),
                    at_ms,
                })
            }
            "run_cancelled" => Some(Self::RunCancelled {
                reason: opt_str(value, "reason").unwrap_or_else(|| "cancelled".to_string()),
                at_ms,
            }),
            "phase_started" => Some(Self::PhaseStarted {
                title: opt_str(value, "title").unwrap_or_else(|| "Phase".to_string()),
                at_ms,
            }),
            "task_started" => Some(Self::TaskStarted {
                task_id: opt_str(value, "task_id")?,
                // Prefer typed workflow metadata over generic label so rows
                // never fall back to prompt parsing (#4119).
                label: opt_str(value, "workflow_task_label").or_else(|| opt_str(value, "label")),
                profile: opt_str(value, "profile"),
                model: opt_str(value, "model").or_else(|| opt_str(value, "resolved_model")),
                strength: opt_str(value, "strength"),
                resolved_model: opt_str(value, "resolved_model"),
                worktree: value
                    .get("worktree")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                workspace: opt_str(value, "workspace").map(PathBuf::from),
                route: Box::new(WorkflowRowRoute::from_json(value)),
                at_ms,
            }),
            "task_completed" => {
                // Like `run_completed`: a missing status, or a completion
                // receipt that still says running/pending, is contradictory
                // and fails closed rather than leaving a live-looking row.
                let status = value
                    .get("status")
                    .and_then(Value::as_str)
                    .map(WorkflowRowStatus::from_ir_status)
                    .filter(|status| {
                        !matches!(
                            status,
                            WorkflowRowStatus::Running | WorkflowRowStatus::Pending
                        )
                    })
                    .unwrap_or(WorkflowRowStatus::Failed);
                Some(Self::TaskCompleted {
                    task_id: opt_str(value, "task_id")?,
                    status,
                    usage: value.get("usage").and_then(usage_from_json),
                    reason: opt_str(value, "reason").or_else(|| opt_str(value, "error")),
                    at_ms,
                })
            }
            "gate_updated" => Some(Self::GateUpdated {
                gate_id: opt_str(value, "gate_id")?,
                role: opt_str(value, "role"),
                gate: opt_str(value, "gate"),
                state: opt_str(value, "state").unwrap_or_else(|| "pending".to_string()),
                blocked_role: opt_str(value, "blocked_role"),
                blocked_reason: opt_str(value, "blocked_reason"),
                at_ms,
            }),
            "task_schema_validation_failed" => Some(Self::TaskSchemaValidationFailed {
                task_id: opt_str(value, "task_id")?,
                message: opt_str(value, "message").unwrap_or_else(|| "schema failed".to_string()),
                at_ms,
            }),
            "task_dispatch_failed" => Some(Self::TaskDispatchFailed {
                label: opt_str(value, "label"),
                phase: opt_str(value, "phase"),
                message: opt_str(value, "message").unwrap_or_default(),
                at_ms,
            }),
            "budget_updated" => Some(Self::BudgetUpdated {
                total: value.get("total").and_then(Value::as_u64),
                spent: value.get("spent").and_then(Value::as_u64).unwrap_or(0),
                remaining: value.get("remaining").and_then(Value::as_u64),
                at_ms,
            }),
            // Logs are intentionally not surfaced in the panel body — they
            // would re-flood the surface the panel exists to protect.
            "log" => None,
            _ => None,
        }
    }
}

/// Collapsible workflow activity panel.
#[derive(Debug, Clone)]
pub struct WorkflowPanel {
    pub run_id: String,
    pub label: String,
    pub lifecycle: WorkflowPanelLifecycle,
    pub phases: Vec<WorkflowPanelPhase>,
    pub selected_phase: usize,
    pub gates: Vec<WorkflowPanelGateLine>,
    /// Newest rejected launches. These are run failures, not child rows.
    pub dispatch_failures: Vec<WorkflowPanelDispatchFailure>,
    /// Monotonic count, including failures older than the retained UI tail.
    pub dispatch_failure_count: usize,
    pub budget_total: Option<u64>,
    pub budget_spent: u64,
    pub budget_remaining: Option<u64>,
    pub started_at_ms: u64,
    pub completed_at_ms: Option<u64>,
    pub error: Option<String>,
    /// Optional final result / verification summary for the history card.
    pub result_summary: Option<String>,
    /// Source script path or other durable artifact pointer.
    pub source_path: Option<PathBuf>,
    /// Spillover / full-output path when the tool result was large.
    pub spillover_path: Option<PathBuf>,
    /// UI locale for rendered copy. Defaults to English; hosts with app
    /// access set it after construction (#4057 wave 2).
    pub locale: Locale,
    /// Whether the transcript already carries this run's finish line. The live
    /// stream and the tool-complete hydration can both deliver the terminal
    /// event; the line is written once.
    pub finish_announced: bool,
    /// Direct-agent cards reuse the Workflow history layout but do not carry a
    /// Workflow launch receipt. Keep that distinction explicit so the shared
    /// renderer never invents unknown Workflow provenance for them (#4039).
    show_workflow_receipts: bool,
}

/// Extra fields the history card can show that are not part of the live panel
/// progress surface (artifact links, final result text).
#[derive(Debug, Clone, Default)]
pub struct WorkflowHistoryExtras {
    pub result_summary: Option<String>,
    pub source_path: Option<PathBuf>,
    pub spillover_path: Option<PathBuf>,
    pub verification_summary: Option<String>,
}

impl WorkflowPanel {
    #[must_use]
    pub fn new(run_id: impl Into<String>, label: impl Into<String>, at_ms: u64) -> Self {
        Self {
            run_id: run_id.into(),
            label: label.into(),
            lifecycle: WorkflowPanelLifecycle::Running,
            phases: Vec::new(),
            selected_phase: 0,
            gates: Vec::new(),
            dispatch_failures: Vec::new(),
            dispatch_failure_count: 0,
            budget_total: None,
            budget_spent: 0,
            budget_remaining: None,
            started_at_ms: at_ms,
            completed_at_ms: None,
            error: None,
            result_summary: None,
            source_path: None,
            spillover_path: None,
            locale: Locale::En,
            finish_announced: false,
            show_workflow_receipts: true,
        }
    }

    /// Hydrate panel state from a workflow tool JSON payload (run record or a
    /// snapshot produced by [`Self::to_run_json`]). Prefers the typed `events`
    /// array when present; falls back to summary + phase fields.
    #[must_use]
    pub fn from_run_json(value: &Value) -> Option<Self> {
        if value.get("action").and_then(Value::as_str) == Some("status") {
            return None;
        }
        let run_id = value
            .get("run_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?
            .to_string();
        let label = value
            .get("workflow_goal")
            .and_then(Value::as_str)
            .or_else(|| value.get("workflow_id").and_then(Value::as_str))
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(&run_id)
            .to_string();
        let at_ms = value
            .get("started_at_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let mut panel = Self::new(run_id.clone(), label.clone(), at_ms);

        if let Some(events) = value.get("events").and_then(Value::as_array) {
            for event in events {
                let mut event = event.clone();
                if let Some(obj) = event.as_object_mut() {
                    obj.insert("run_id".to_string(), Value::String(run_id.clone()));
                }
                panel.apply_json_event(&event);
            }
        } else if let Some(phases) = value.get("phases").and_then(Value::as_array) {
            for phase_val in phases {
                let title = phase_val
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Work");
                panel.phases.push(WorkflowPanelPhase::new(title));
                let phase_idx = panel.phases.len() - 1;
                if let Some(rows) = phase_val.get("rows").and_then(Value::as_array) {
                    for row in rows {
                        let task_id = row
                            .get("task_id")
                            .and_then(Value::as_str)
                            .unwrap_or("task")
                            .to_string();
                        let status = row
                            .get("status")
                            .and_then(Value::as_str)
                            .map(WorkflowRowStatus::from_ir_status)
                            .unwrap_or(WorkflowRowStatus::Pending);
                        panel.phases[phase_idx].rows.push(WorkflowPanelRow {
                            task_id: task_id.clone(),
                            label: row
                                .get("label")
                                .and_then(Value::as_str)
                                .unwrap_or(&task_id)
                                .to_string(),
                            profile: opt_str(row, "profile"),
                            model: opt_str(row, "model"),
                            strength: opt_str(row, "strength"),
                            worktree: row
                                .get("worktree")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            workspace: opt_str(row, "workspace").map(PathBuf::from),
                            status,
                            started_at_ms: row
                                .get("started_at_ms")
                                .and_then(Value::as_u64)
                                .unwrap_or(at_ms),
                            completed_at_ms: row.get("completed_at_ms").and_then(Value::as_u64),
                            error: opt_str(row, "error"),
                            schema_error: opt_str(row, "schema_error"),
                            route: WorkflowRowRoute::from_json(row),
                            usage: row.get("usage").and_then(usage_from_json),
                        });
                    }
                }
            }
            if !panel.phases.is_empty() {
                panel.selected_phase = panel.phases.len() - 1;
            }
        } else if let Some(child_count) =
            value
                .get("child_count")
                .and_then(Value::as_u64)
                .or_else(|| {
                    value
                        .get("child_ids")
                        .and_then(Value::as_array)
                        .map(|a| a.len() as u64)
                })
        {
            // Bare summary without events: synthesize a Work phase so child
            // count still surfaces on the history card.
            if child_count > 0 {
                let mut phase = WorkflowPanelPhase::new("Work");
                for i in 0..child_count {
                    let id = value
                        .get("child_ids")
                        .and_then(Value::as_array)
                        .and_then(|ids| ids.get(i as usize))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("child-{i}"));
                    phase.rows.push(WorkflowPanelRow {
                        task_id: id.clone(),
                        label: id,
                        profile: None,
                        model: None,
                        strength: None,
                        worktree: false,
                        workspace: None,
                        status: WorkflowRowStatus::Succeeded,
                        started_at_ms: at_ms,
                        completed_at_ms: value.get("completed_at_ms").and_then(Value::as_u64),
                        error: None,
                        schema_error: None,
                        // A bare child-count summary carries no receipt at all;
                        // the row must show that rather than infer one (#4039).
                        route: WorkflowRowRoute::default(),
                        usage: Some(WorkflowRowUsage::default()),
                    });
                }
                panel.phases.push(phase);
            }
        }

        panel.merge_dispatch_failures_from_run_json(value);

        if let Some(gates) = value
            .get("gate_status")
            .or_else(|| value.get("gates"))
            .and_then(Value::as_array)
        {
            for gate in gates {
                if let Some(gate_id) = opt_str(gate, "gate_id") {
                    panel.upsert_gate(WorkflowPanelGateLine {
                        gate_id,
                        role: opt_str(gate, "role"),
                        gate: opt_str(gate, "gate"),
                        state: opt_str(gate, "state").unwrap_or_else(|| "pending".to_string()),
                        blocked_role: opt_str(gate, "blocked_role"),
                        blocked_reason: opt_str(gate, "blocked_reason"),
                    });
                }
            }
        }

        if let Some(status) = value.get("status").and_then(Value::as_str) {
            let life = lifecycle_from_status(status);
            if life.is_terminal() {
                panel.lifecycle = life;
                panel.completed_at_ms = value
                    .get("completed_at_ms")
                    .and_then(Value::as_u64)
                    .or(panel.completed_at_ms);
            } else if panel.lifecycle.is_running() {
                panel.lifecycle = life;
            }
        }
        if let Some(error) = opt_str(value, "error") {
            panel.error = Some(error);
        }
        if let Some(spent) = value.get("budget_spent").and_then(Value::as_u64) {
            panel.budget_spent = spent;
        }
        if let Some(total) = value
            .get("token_budget")
            .or_else(|| value.get("budget_total"))
            .and_then(Value::as_u64)
        {
            panel.budget_total = Some(total);
        }
        if let Some(remaining) = value.get("budget_remaining").and_then(Value::as_u64) {
            panel.budget_remaining = Some(remaining);
        }
        // Apply extras after events so RunStarted reset does not wipe them.
        if panel.source_path.is_none() {
            panel.source_path = opt_str(value, "source_path").map(PathBuf::from);
        }
        if panel.result_summary.is_none() {
            panel.result_summary = value
                .get("result")
                .and_then(summarize_result_value)
                .or_else(|| opt_str(value, "result_summary"));
        }
        if let Some(verification) = value.get("verification")
            && let Some(summary) = verification.get("summary").and_then(Value::as_str)
        {
            let trimmed = summary.trim();
            if !trimmed.is_empty() {
                panel.result_summary = Some(match panel.result_summary.take() {
                    Some(existing) => format!("{existing} · verify: {trimmed}"),
                    None => format!("verify: {trimmed}"),
                });
            }
        }
        // Prefer the goal label from the payload when events used a fallback.
        if !label.is_empty() && panel.label == run_id {
            panel.label = label;
        }
        Some(panel)
    }

    /// Snapshot panel state into a JSON blob suitable for the history cell
    /// (and re-hydration via [`Self::from_run_json`]).
    #[must_use]
    pub fn to_run_json(&self) -> Value {
        let status = match self.lifecycle {
            WorkflowPanelLifecycle::Pending => "pending",
            WorkflowPanelLifecycle::Running => "running",
            WorkflowPanelLifecycle::Succeeded => "completed",
            WorkflowPanelLifecycle::Degraded => "degraded",
            WorkflowPanelLifecycle::Failed => "failed",
            WorkflowPanelLifecycle::Cancelled => "cancelled",
        };
        let (done, total) = self.done_total();
        let (failed, cancelled) = self.failure_cancel_counts();
        json!({
            "run_id": self.run_id,
            "status": status,
            "workflow_goal": self.label,
            "started_at_ms": self.started_at_ms,
            "completed_at_ms": self.completed_at_ms,
            "child_count": total,
            "done_count": done,
            "phase_count": self.phase_count(),
            "failure_count": failed,
            "cancel_count": cancelled,
            "error": self.error,
            "result_summary": self.result_summary,
            "source_path": self.source_path.as_ref().map(|p| p.display().to_string()),
            "spillover_path": self.spillover_path.as_ref().map(|p| p.display().to_string()),
            "token_budget": self.budget_total,
            "budget_spent": self.budget_spent,
            "budget_remaining": self.budget_remaining,
            "dispatch_failure_count": self.dispatch_failure_count,
            "dispatch_failures": self.dispatch_failures.iter().map(|failure| {
                json!({
                    "label": failure.label.as_deref(),
                    "phase": failure.phase.as_deref(),
                    "message": failure.message.as_str(),
                    "at_ms": failure.at_ms,
                })
            }).collect::<Vec<_>>(),
            "gates": self.gates.iter().map(|gate| {
                json!({
                    "gate_id": gate.gate_id.as_str(),
                    "role": gate.role.as_deref(),
                    "gate": gate.gate.as_deref(),
                    "state": gate.state.as_str(),
                    "blocked_role": gate.blocked_role.as_deref(),
                    "blocked_reason": gate.blocked_reason.as_deref(),
                })
            }).collect::<Vec<_>>(),
            "phases": self.phases.iter().map(workflow_phase_run_json).collect::<Vec<_>>(),
        })
    }

    /// Compact one-line history-card summary: lifecycle, settled/total
    /// children, phases, failures (only when there are any), elapsed (#4122,
    /// #6503). The free-text goal lives on the expanded body so the fixed
    /// header summary budget (≈56 cols) never drops counts.
    #[must_use]
    pub fn compact_summary_text(&self, width: usize) -> String {
        let raw = format!(
            "workflow {life} · {counts}",
            life = self.lifecycle.display_label(self.locale),
            counts = self.summary_counts_text(),
        );
        truncate_line_to_width(&raw, width.max(1))
    }

    /// `N/M done · P phases[ · F failed][ · C cancelled] · elapsed`. The
    /// settled count is left out until a child exists.
    fn summary_counts_text(&self) -> String {
        let (_, total) = self.done_total();
        let phases = self.phase_count();
        let phase_id = if phases == 1 {
            MessageId::WorkflowPhaseCountOne
        } else {
            MessageId::WorkflowPhaseCountMany
        };
        let settled = if total > 0 {
            format!("{} · ", self.settled_text())
        } else {
            String::new()
        };
        format!(
            "{settled}{phases}{problems} · {elapsed}",
            phases = count_text(self.locale, phase_id, phases),
            problems = self.problem_counts_suffix(),
            elapsed = self.elapsed_label(),
        )
    }

    /// `N/M done`, localized.
    fn settled_text(&self) -> String {
        let (done, total) = self.done_total();
        tr(self.locale, MessageId::WorkflowSettledOfTotal)
            .replace("{done}", &done.to_string())
            .replace("{total}", &total.to_string())
    }

    /// ` · N failed · N cancelled`, each only when non-zero (#6503): a
    /// healthy run does not spend header columns announcing `0 fail`.
    fn problem_counts_suffix(&self) -> String {
        let (failed, cancelled) = self.failure_cancel_counts();
        let mut out = String::new();
        if failed > 0 {
            out.push_str(" · ");
            out.push_str(&count_text(
                self.locale,
                MessageId::WorkflowCountFailed,
                failed,
            ));
        }
        if cancelled > 0 {
            out.push_str(" · ");
            out.push_str(&count_text(
                self.locale,
                MessageId::WorkflowCountCancelled,
                cancelled,
            ));
        }
        out
    }

    /// Elapsed label shared with direct sub-agent cards.
    #[must_use]
    pub fn elapsed_label(&self) -> String {
        // Guard against epoch-zero starts (bare status payloads without
        // timestamps) which would otherwise render multi-year elapsed times.
        if self.started_at_ms == 0 {
            if let Some(completed) = self.completed_at_ms {
                return codewhale_command_contract::elapsed::format_elapsed_ms(completed);
            }
            return "0s".to_string();
        }
        let end = self.completed_at_ms.unwrap_or_else(now_ms);
        codewhale_command_contract::elapsed::format_elapsed_ms(
            end.saturating_sub(self.started_at_ms),
        )
    }

    /// Expanded history-card body lines (phase/child summaries, links,
    /// result, failures). Empty when the card should stay compact.
    #[must_use]
    pub fn history_expanded_lines(
        &self,
        width: u16,
        extras: &WorkflowHistoryExtras,
    ) -> Vec<Line<'static>> {
        let content_width = usize::from(width).max(1);
        let mut lines = Vec::new();

        if !self.label.trim().is_empty() {
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(
                    &format!("goal: {}", short_label(self.label.trim(), 160)),
                    content_width,
                ),
                Style::default().fg(palette::TEXT_TOOL_OUTPUT),
            )));
        }

        // Phase summary strip (same chips as the panel body).
        if !self.phases.is_empty() {
            let mut chips = Vec::new();
            for (idx, phase) in self.phases.iter().take(MAX_PHASE_SUMMARY).enumerate() {
                let marker =
                    codewhale_tui_kit::glyphs::selection_marker(idx == self.selected_phase);
                chips.push(format!(
                    "{marker}{title} ({counts})",
                    title = short_label(&phase.title, 24),
                    counts = phase.counts_text(self.locale, ", "),
                ));
            }
            if self.phases.len() > MAX_PHASE_SUMMARY {
                chips.push(format!("+{}", self.phases.len() - MAX_PHASE_SUMMARY));
            }
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(&format!("phases: {}", chips.join("  ")), content_width),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }

        if !self.gates.is_empty() {
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(&format!("gates: {}", self.gates_summary()), content_width),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }

        // Child summary across all phases.
        let children: Vec<String> = self
            .phases
            .iter()
            .flat_map(|p| p.rows.iter())
            .take(8)
            .map(|row| {
                format!(
                    "{mark} {label} ({status})",
                    mark = role_mark(row.profile.as_deref()),
                    label = short_label(&row.label, 16),
                    status = row.status.display_label(self.locale)
                )
            })
            .collect();
        if !children.is_empty() {
            let more = self
                .phases
                .iter()
                .map(|p| p.rows.len())
                .sum::<usize>()
                .saturating_sub(children.len());
            let mut body = children.join(" · ");
            if more > 0 {
                body = format!("{body} · +{more} more");
            }
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(&format!("children: {body}"), content_width),
                Style::default().fg(palette::TEXT_TOOL_OUTPUT),
            )));
        }

        // The history variant uses the same real-data lane vocabulary as the
        // live panel. Durations are proportional within the run; gates remain
        // a separate named line because runtime events do not yet timestamp
        // them precisely enough to place them on a synthetic timeline.
        let rows = self.phases.iter().flat_map(|phase| phase.rows.iter());
        let max_elapsed = rows
            .clone()
            .map(|row| row_elapsed_ms(row, now_ms()))
            .max()
            .unwrap_or(0);
        for row in rows.take(8) {
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(
                    &format!(
                        "lane {mark} {label:<14} {track} {elapsed} {status}",
                        mark = role_mark(row.profile.as_deref()),
                        label = short_label(&row.label, 14),
                        track = lane_track(row, max_elapsed, 16, now_ms()),
                        elapsed = codewhale_command_contract::elapsed::format_elapsed_ms(
                            row_elapsed_ms(row, now_ms())
                        ),
                        status = row.status.display_label(self.locale),
                    ),
                    content_width,
                ),
                Style::default().fg(row.status.color()),
            )));
            // #4039: the history card shows the same immutable receipt as the
            // live panel, so a finished run stays auditable after the fact.
            if self.show_workflow_receipts {
                lines.extend(
                    receipt_line_strings(row, self.locale, content_width, 2)
                        .into_iter()
                        .map(|text| {
                            Line::from(Span::styled(text, Style::default().fg(palette::TEXT_MUTED)))
                        }),
                );
            }
        }

        // Rejected launches are run-level failures rather than child lanes.
        // Keep their newest bounded details visible in the completed card.
        lines.extend(self.render_dispatch_failure_lines(content_width));

        if self.lifecycle.is_terminal() {
            let (done, total) = self.done_total();
            let (failed, cancelled) = self.failure_cancel_counts();
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(
                    &tr(self.locale, MessageId::WorkflowDebrief)
                        .replace("{done}", &done.to_string())
                        .replace("{total}", &total.to_string())
                        .replace("{failed}", &failed.to_string())
                        .replace("{cancelled}", &cancelled.to_string())
                        .replace("{elapsed}", &self.elapsed_label()),
                    content_width,
                ),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }

        let result = extras
            .result_summary
            .as_deref()
            .or(self.result_summary.as_deref())
            .or(extras.verification_summary.as_deref());
        if let Some(result) = result.filter(|s| !s.trim().is_empty()) {
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(
                    &format!("result: {}", short_label(result.trim(), 160)),
                    content_width,
                ),
                Style::default().fg(palette::TEXT_TOOL_OUTPUT),
            )));
        }

        let source = extras
            .source_path
            .as_ref()
            .or(self.source_path.as_ref())
            .map(|p| p.display().to_string());
        if let Some(path) = source.filter(|s| !s.is_empty()) {
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(&format!("source: {path}"), content_width),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }
        let spill = extras
            .spillover_path
            .as_ref()
            .or(self.spillover_path.as_ref())
            .map(|p| p.display().to_string());
        if let Some(path) = spill.filter(|s| !s.is_empty()) {
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(&format!("artifact: {path}"), content_width),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        } else if self.lifecycle.is_terminal() {
            let details = crate::tui::shell_key_routing::tool_details_chord();
            let transcript_hint = tr(self.locale, MessageId::WorkflowTranscriptDetails)
                .replace("{details}", details.as_ref());
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(&transcript_hint, content_width),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }

        if let Some(error) = self.error.as_deref().filter(|s| !s.trim().is_empty()) {
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(
                    &format!("error: {}", short_label(error, 160)),
                    content_width,
                ),
                Style::default().fg(palette::STATUS_ERROR),
            )));
        }
        for row in self.phases.iter().flat_map(|p| p.rows.iter()) {
            if let Some(schema) = row.schema_error.as_deref() {
                lines.push(Line::from(Span::styled(
                    truncate_line_to_width(
                        &format!(
                            "schema {}: {}",
                            short_label(&row.task_id, 12),
                            short_label(schema, 120)
                        ),
                        content_width,
                    ),
                    Style::default().fg(palette::STATUS_ERROR),
                )));
            } else if row.status.is_failure()
                && let Some(err) = row.error.as_deref()
            {
                lines.push(Line::from(Span::styled(
                    truncate_line_to_width(
                        &format!(
                            "fail {}: {}",
                            short_label(&row.label, 14),
                            short_label(err, 120)
                        ),
                        content_width,
                    ),
                    Style::default().fg(palette::STATUS_ERROR),
                )));
            }
        }

        lines
    }

    /// Full history-card lines including a simple self-contained header so
    /// unit tests (and direct sub-agent cards) can render without history.rs.
    ///
    /// Public convergence API for #4122 — also exercised by unit tests and
    /// `DelegateCard::as_workflow_history_panel`.
    #[must_use]
    #[allow(dead_code)] // public API used by direct sub-agent projection + tests
    pub fn render_history_card(
        &self,
        width: u16,
        expanded: bool,
        extras: &WorkflowHistoryExtras,
    ) -> Vec<Line<'static>> {
        let content_width = usize::from(width).max(1);
        let mut lines = Vec::new();
        let glyph = if expanded { '▼' } else { '▶' };
        let summary = self.compact_summary_text(content_width.saturating_sub(2));
        lines.push(Line::from(Span::styled(
            truncate_line_to_width(&format!("{glyph} {summary}"), content_width),
            Style::default()
                .fg(self.lifecycle.color())
                .add_modifier(Modifier::BOLD),
        )));
        if expanded {
            lines.extend(self.history_expanded_lines(width, extras));
        }
        lines
    }

    /// Single-agent "mini workflow" view for direct sub-agent cards so they
    /// share the same lifecycle/elapsed/result concepts as workflow runs.
    #[must_use]
    #[allow(dead_code)] // public API used by DelegateCard + tests
    pub fn from_direct_subagent(
        agent_id: impl Into<String>,
        role: impl Into<String>,
        lifecycle: WorkflowPanelLifecycle,
        started_at_ms: u64,
        completed_at_ms: Option<u64>,
        summary: Option<String>,
        error: Option<String>,
    ) -> Self {
        let agent_id = agent_id.into();
        let role = role.into();
        let mut panel = Self::new(agent_id.clone(), role.clone(), started_at_ms);
        panel.lifecycle = lifecycle;
        panel.completed_at_ms = completed_at_ms;
        panel.show_workflow_receipts = false;
        panel.result_summary = summary.clone();
        panel.error = error.clone();
        let status = match lifecycle {
            WorkflowPanelLifecycle::Pending => WorkflowRowStatus::Pending,
            WorkflowPanelLifecycle::Running => WorkflowRowStatus::Running,
            WorkflowPanelLifecycle::Succeeded => WorkflowRowStatus::Succeeded,
            WorkflowPanelLifecycle::Degraded => WorkflowRowStatus::Failed,
            WorkflowPanelLifecycle::Failed => WorkflowRowStatus::Failed,
            WorkflowPanelLifecycle::Cancelled => WorkflowRowStatus::Cancelled,
        };
        let mut phase = WorkflowPanelPhase::new("Agent");
        phase.rows.push(WorkflowPanelRow {
            task_id: agent_id,
            label: role,
            profile: None,
            model: None,
            strength: None,
            worktree: false,
            workspace: None,
            status,
            started_at_ms,
            completed_at_ms,
            error,
            schema_error: None,
            // A direct sub-agent card projects a single agent, not a Workflow
            // task, so it carries no Workflow launch/usage receipt (#4039).
            route: WorkflowRowRoute::default(),
            usage: completed_at_ms.map(|_| WorkflowRowUsage::default()),
        });
        panel.phases.push(phase);
        panel
    }

    /// Apply a stream of events. `RunStarted` replaces any prior completed run.
    pub fn apply_event(&mut self, event: WorkflowPanelEvent) {
        match event {
            WorkflowPanelEvent::RunStarted {
                run_id,
                workflow_id,
                workflow_goal,
                source_path,
                token_budget,
                at_ms,
            } => {
                // A repeated or replayed start of this same run never resets
                // it: rebuilding would erase settled rows and a terminal
                // outcome (U05-01). It names the run (its goal is fixed for
                // the run) and fills only what the panel still lacks, such as
                // the start time of a placeholder opened by a late event.
                if run_id == self.run_id {
                    if let Some(label) = workflow_goal.or(workflow_id) {
                        self.label = label;
                    }
                    if self.started_at_ms == 0 {
                        self.started_at_ms = at_ms;
                    }
                    if self.budget_total.is_none() {
                        self.budget_total = token_budget;
                        self.budget_remaining = token_budget;
                    }
                    if self.source_path.is_none() {
                        self.source_path = source_path;
                    }
                    return;
                }
                // New run replaces preserved completed state.
                let locale = self.locale;
                *self = Self::new(
                    run_id,
                    workflow_goal
                        .or(workflow_id)
                        .unwrap_or_else(|| "workflow".to_string()),
                    at_ms,
                );
                self.locale = locale;
                self.budget_total = token_budget;
                self.budget_remaining = token_budget;
                self.source_path = source_path;
            }
            WorkflowPanelEvent::RunCompleted {
                status,
                error,
                at_ms,
            } => {
                // A cancel is final. Under backpressure the VM's own
                // `run_completed` can land after `run_cancelled`; it must not
                // repaint a stopped run as finished or drop the cancel reason.
                if self.lifecycle == WorkflowPanelLifecycle::Cancelled {
                    return;
                }
                // A completion receipt that still says running/pending is
                // contradictory, not a success.
                self.lifecycle = if status.is_running() {
                    WorkflowPanelLifecycle::Failed
                } else {
                    status
                };
                self.error = error;
                self.completed_at_ms = Some(at_ms);
            }
            WorkflowPanelEvent::RunCancelled { reason, at_ms } => {
                self.finalize_running_rows(WorkflowRowStatus::Cancelled, at_ms);
                self.lifecycle = WorkflowPanelLifecycle::Cancelled;
                self.error = Some(reason);
                self.completed_at_ms = Some(at_ms);
            }
            WorkflowPanelEvent::PhaseStarted { title, at_ms: _ } => {
                if self.phases.last().is_some_and(|phase| phase.title == title) {
                    return;
                }
                self.phases.push(WorkflowPanelPhase::new(title));
                self.selected_phase = self.phases.len().saturating_sub(1);
            }
            WorkflowPanelEvent::TaskStarted {
                task_id,
                label,
                profile,
                model,
                strength,
                resolved_model,
                worktree,
                workspace,
                route,
                at_ms,
            } => {
                // A settled run is final. A child launch that raced the
                // cancel (or any late same-run start) is recorded, but it
                // never reopens the run or resets a row that already settled.
                let settled = self.lifecycle.is_terminal();
                if settled && self.find_row_mut(&task_id).is_some() {
                    return;
                }
                if self.phases.is_empty() {
                    self.phases.push(WorkflowPanelPhase::new("Work"));
                    self.selected_phase = 0;
                }
                // Tasks join the runtime's newest phase. `selected_phase` is
                // the render cursor and never routes an arriving task.
                let phase_idx = self.phases.len().saturating_sub(1);
                let display_model = resolved_model.or(model);
                let mut row = WorkflowPanelRow {
                    task_id: task_id.clone(),
                    label: label
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or_else(|| task_id.clone()),
                    profile,
                    model: display_model,
                    strength,
                    worktree,
                    workspace,
                    status: WorkflowRowStatus::Running,
                    started_at_ms: at_ms,
                    completed_at_ms: None,
                    error: None,
                    schema_error: None,
                    route: *route,
                    usage: None,
                };
                if self.lifecycle == WorkflowPanelLifecycle::Cancelled {
                    // Same finalization the cancel applied to running rows.
                    row.status = WorkflowRowStatus::Cancelled;
                    row.completed_at_ms = Some(at_ms);
                    row.usage = Some(WorkflowRowUsage::default());
                }
                if let Some(existing) = self.find_row_mut(&task_id) {
                    *existing = row;
                } else if let Some(phase) = self.phases.get_mut(phase_idx) {
                    phase.rows.push(row);
                }
                if !settled {
                    self.lifecycle = WorkflowPanelLifecycle::Running;
                }
            }
            WorkflowPanelEvent::TaskCompleted {
                task_id,
                status,
                usage,
                reason,
                at_ms,
            } => {
                if let Some(row) = self.find_row_mut(&task_id) {
                    row.status = status;
                    row.completed_at_ms = Some(at_ms);
                    if (status.is_failure() || status.is_cancel())
                        && let Some(reason) = reason.filter(|text| !text.trim().is_empty())
                    {
                        row.error = Some(reason);
                    }
                    // A completed row always carries a usage receipt, even when
                    // every counter in it is unknown (#4039).
                    row.usage = Some(usage.unwrap_or_default());
                }
            }
            WorkflowPanelEvent::GateUpdated {
                gate_id,
                role,
                gate,
                state,
                blocked_role,
                blocked_reason,
                at_ms: _,
            } => {
                self.upsert_gate(WorkflowPanelGateLine {
                    gate_id,
                    role,
                    gate,
                    state,
                    blocked_role,
                    blocked_reason,
                });
            }
            WorkflowPanelEvent::TaskSchemaValidationFailed {
                task_id,
                message,
                at_ms,
            } => {
                if let Some(row) = self.find_row_mut(&task_id) {
                    row.status = WorkflowRowStatus::SchemaFailed;
                    row.schema_error = Some(message);
                    row.completed_at_ms = Some(at_ms);
                } else {
                    // Schema can fire before/without a started task.
                    if self.phases.is_empty() {
                        self.phases.push(WorkflowPanelPhase::new("Work"));
                    }
                    let phase_idx = self.phases.len().saturating_sub(1);
                    if let Some(phase) = self.phases.get_mut(phase_idx) {
                        phase.rows.push(WorkflowPanelRow {
                            task_id,
                            label: "schema".to_string(),
                            profile: None,
                            model: None,
                            strength: None,
                            worktree: false,
                            workspace: None,
                            status: WorkflowRowStatus::SchemaFailed,
                            started_at_ms: at_ms,
                            completed_at_ms: Some(at_ms),
                            error: None,
                            schema_error: Some(message),
                            // Schema failure without a task_started: nothing was
                            // received about the route, so nothing is claimed.
                            route: WorkflowRowRoute::default(),
                            usage: Some(WorkflowRowUsage::default()),
                        });
                    }
                }
            }
            WorkflowPanelEvent::TaskDispatchFailed {
                label,
                phase,
                message,
                at_ms,
            } => {
                self.record_dispatch_failure(label, phase, message, at_ms);
                // A failed launch can be only one slot in a parallel phase;
                // keep the run live so surviving siblings can still finish.
                if self.lifecycle.is_running() {
                    self.lifecycle = WorkflowPanelLifecycle::Running;
                }
            }
            WorkflowPanelEvent::BudgetUpdated {
                total,
                spent,
                remaining,
                at_ms: _,
            } => {
                if total.is_some() {
                    self.budget_total = total;
                }
                self.budget_spent = spent;
                self.budget_remaining = remaining;
            }
        }
    }

    /// Apply one event only when its explicit route identity belongs to this
    /// panel. A strictly newer `run_started` is the sole event allowed to
    /// select a different run; legacy direct callers without an id remain
    /// accepted.
    pub fn apply_json_event(&mut self, value: &Value) -> bool {
        let event_type = value.get("type").and_then(Value::as_str);
        let event_run_id = value
            .get("run_id")
            .or_else(|| value.get("workflow_run_id"))
            .and_then(Value::as_str)
            .filter(|run_id| !run_id.trim().is_empty());
        if event_type == Some("run_started")
            && event_run_id.is_some_and(|run_id| run_id != self.run_id)
            && value
                .get("at_ms")
                .and_then(Value::as_u64)
                .is_none_or(|at_ms| at_ms <= self.started_at_ms)
        {
            return false;
        }
        if event_type != Some("run_started")
            && event_run_id.is_some_and(|run_id| run_id != self.run_id)
        {
            return false;
        }
        if let Some(event) = WorkflowPanelEvent::from_json_value(value) {
            self.apply_event(event);
            return true;
        }
        false
    }

    pub fn apply_json_events(&mut self, values: &[Value]) {
        for value in values {
            self.apply_json_event(value);
        }
    }

    /// Merge the authoritative structured failure ledger carried by a run
    /// result after its retained event tail has been applied. The tail can
    /// replay events already seen live; the exact top-level count and newest
    /// bounded ledger therefore replace, rather than add to, panel state.
    pub(crate) fn merge_dispatch_failures_from_run_json(&mut self, value: &Value) {
        let fallback_at_ms = value
            .get("started_at_ms")
            .and_then(Value::as_u64)
            .unwrap_or(self.started_at_ms);
        let ledger = value
            .get("dispatch_failures")
            .and_then(Value::as_array)
            .map(|failures| {
                let start = failures
                    .len()
                    .saturating_sub(MAX_DISPATCH_FAILURES_RETAINED);
                failures[start..]
                    .iter()
                    .map(|failure| {
                        WorkflowPanelDispatchFailure::bounded(
                            opt_str(failure, "label"),
                            opt_str(failure, "phase"),
                            opt_str(failure, "message").unwrap_or_default(),
                            failure
                                .get("at_ms")
                                .and_then(Value::as_u64)
                                .unwrap_or(fallback_at_ms),
                        )
                    })
                    .collect::<Vec<_>>()
            });
        let declared_count = value
            .get("dispatch_failure_count")
            .and_then(Value::as_u64)
            .map(|count| usize::try_from(count).unwrap_or(usize::MAX));

        if let Some(ledger) = ledger {
            let returned = ledger.len();
            self.dispatch_failures = ledger;
            self.dispatch_failure_count = declared_count
                .map(|count| count.max(returned))
                .unwrap_or_else(|| self.dispatch_failure_count.max(returned));
        } else if let Some(count) = declared_count {
            self.dispatch_failure_count = count;
        }
    }

    /// Tokens the run has used so far: the live meter the engine streams
    /// (`budget_updated.spent`) or, when larger, the finished agents'
    /// receipts. `None` until something is reported — never a made-up zero.
    #[must_use]
    pub fn tokens_so_far(&self) -> Option<u64> {
        let receipts: u64 = self
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .filter_map(|row| row.usage.as_ref().and_then(WorkflowRowUsage::token_total))
            .sum();
        let tokens = self.budget_spent.max(receipts);
        (tokens > 0).then_some(tokens)
    }

    /// The transcript's finish line for a settled run: the state word, the
    /// facts (`name · 1/3 done · 2 failed · 2m 14s · ↓1.2M`), and the result
    /// or the one reason it fell short. The name is the goal's first sentence
    /// and the reason is a first sentence too; the full goal, every agent's
    /// error and the run's own error stay on the expanded card. `None` while
    /// the run is live.
    #[must_use]
    pub fn finish_line(&self) -> Option<(String, String, Option<String>)> {
        let state = match self.lifecycle {
            WorkflowPanelLifecycle::Pending | WorkflowPanelLifecycle::Running => return None,
            WorkflowPanelLifecycle::Succeeded => MessageId::WorkflowLineFinished,
            WorkflowPanelLifecycle::Degraded => MessageId::WorkflowLineFinishedWithGaps,
            WorkflowPanelLifecycle::Failed => MessageId::WorkflowLineFailed,
            WorkflowPanelLifecycle::Cancelled => MessageId::WorkflowLineStopped,
        };
        let (_, _, _, total) = self.row_outcomes();
        let mut rest = Vec::new();
        if total > 0 || self.dispatch_failure_count > 0 {
            rest.push(self.outcome_counts_text());
        }
        rest.push(self.elapsed_label());
        if let Some(tokens) = self.tokens_so_far() {
            rest.push(format!(
                "↓{}",
                crate::tui::footer_ui::format_token_count_compact(tokens)
            ));
        }
        // The title gives way, at a word boundary, so the counts and elapsed
        // are never the part the card header cuts off.
        let rest = rest.join(" · ");
        let title_cols = FINISH_FACTS_COLS
            .saturating_sub(rest.width() + 3)
            .max(MIN_FINISH_TITLE_COLS);
        let facts = [
            codewhale_tui_kit::ui_text::semantic_truncate(&self.short_title(), title_cols),
            rest,
        ];
        let detail = match self.lifecycle {
            WorkflowPanelLifecycle::Succeeded | WorkflowPanelLifecycle::Degraded => self
                .result_summary
                .clone()
                .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
                .or_else(|| self.outcome_reason())
                .or_else(|| self.error.as_deref().map(first_sentence)),
            _ => self
                .outcome_reason()
                .or_else(|| self.result_summary.as_deref().map(first_sentence)),
        }
        .filter(|text| !text.is_empty());
        Some((
            tr(self.locale, state).into_owned(),
            facts.join(" · "),
            detail,
        ))
    }

    #[must_use]
    pub fn done_total(&self) -> (usize, usize) {
        let mut done = 0usize;
        let mut total = 0usize;
        for phase in &self.phases {
            for row in &phase.rows {
                total += 1;
                if !row.status.is_running() {
                    done += 1;
                }
            }
        }
        (done, total)
    }

    #[must_use]
    pub fn phase_count(&self) -> usize {
        self.phases.len()
    }

    #[must_use]
    pub fn failure_cancel_counts(&self) -> (usize, usize) {
        let mut failed = self.dispatch_failure_count;
        let mut cancelled = 0usize;
        for phase in &self.phases {
            for row in &phase.rows {
                if row.status.is_failure() {
                    failed = failed.saturating_add(1);
                } else if row.status.is_cancel() {
                    cancelled = cancelled.saturating_add(1);
                }
            }
        }
        (failed, cancelled)
    }

    /// Rows by outcome: `(succeeded, failed, cancelled, total)`. Rejected
    /// dispatches are not rows, so they are not in these counts.
    #[must_use]
    pub fn row_outcomes(&self) -> (usize, usize, usize, usize) {
        self.phases.iter().map(WorkflowPanelPhase::counts).fold(
            (0, 0, 0, 0),
            |acc, (done, running, failed, cancelled)| {
                (
                    acc.0 + done,
                    acc.1 + failed,
                    acc.2 + cancelled,
                    acc.3 + done + running + failed + cancelled,
                )
            },
        )
    }

    /// The run's name on one-line surfaces: the goal's first sentence. The
    /// full goal stays on the expanded card, so a paragraph-long goal is not
    /// the title of every row that names the run.
    #[must_use]
    pub fn short_title(&self) -> String {
        let title = first_sentence(&self.label);
        if title.is_empty() {
            "workflow".to_string()
        } else {
            title
        }
    }

    /// `k/n done · m failed · c cancelled`: what finished, and what did not.
    /// Only the done count is always there; a failed or cancelled count shows
    /// only when it is not zero. Settled-but-failed agents never count as done.
    #[must_use]
    pub fn outcome_counts_text(&self) -> String {
        let (succeeded, _, _, total) = self.row_outcomes();
        let (failed, cancelled) = self.failure_cancel_counts();
        let mut parts = Vec::new();
        if total > 0 {
            parts.push(
                tr(self.locale, MessageId::WorkflowSettledOfTotal)
                    .replace("{done}", &succeeded.to_string())
                    .replace("{total}", &total.to_string()),
            );
        } else if failed == 0 {
            parts.push(tr(self.locale, MessageId::WorkflowNoTasksYet).into_owned());
        }
        if failed > 0 {
            parts.push(count_text(
                self.locale,
                MessageId::WorkflowCountFailed,
                failed,
            ));
        }
        if cancelled > 0 {
            parts.push(count_text(
                self.locale,
                MessageId::WorkflowCountCancelled,
                cancelled,
            ));
        }
        parts.join(" · ")
    }

    /// The one reason worth showing for a run that settled short of success,
    /// as its first sentence. When nothing succeeded, a failed agent's own
    /// reason ("Authorization failed: …") beats the run's aggregate ("no task
    /// produced a result: all 2 task(s) failed …"): it is the cause a person
    /// can act on. `None` for a live or fully successful run.
    #[must_use]
    pub fn outcome_reason(&self) -> Option<String> {
        let row_reason = || {
            self.phases
                .iter()
                .flat_map(|phase| phase.rows.iter())
                .filter(|row| row.status.is_failure())
                .find_map(|row| row.error.as_deref().or(row.schema_error.as_deref()))
                .map(str::to_string)
                .or_else(|| {
                    self.dispatch_failures
                        .first()
                        .map(|failure| failure.message.clone())
                })
        };
        let run_error = || self.error.clone();
        let reason = match self.lifecycle {
            WorkflowPanelLifecycle::Pending
            | WorkflowPanelLifecycle::Running
            | WorkflowPanelLifecycle::Succeeded => return None,
            WorkflowPanelLifecycle::Cancelled => run_error(),
            WorkflowPanelLifecycle::Degraded => row_reason().or_else(run_error),
            WorkflowPanelLifecycle::Failed => {
                let (succeeded, _, _, _) = self.row_outcomes();
                if succeeded == 0 {
                    row_reason().or_else(run_error)
                } else {
                    run_error().or_else(row_reason)
                }
            }
        };
        reason
            .map(|text| first_sentence(strip_error_tag(&text)))
            .filter(|text| !text.is_empty())
    }

    fn record_dispatch_failure(
        &mut self,
        label: Option<String>,
        phase: Option<String>,
        message: String,
        at_ms: u64,
    ) {
        let failure = WorkflowPanelDispatchFailure::bounded(label, phase, message, at_ms);
        self.dispatch_failure_count = self.dispatch_failure_count.saturating_add(1);
        self.dispatch_failures.push(failure);
        if self.dispatch_failures.len() > MAX_DISPATCH_FAILURES_RETAINED {
            let overflow = self.dispatch_failures.len() - MAX_DISPATCH_FAILURES_RETAINED;
            self.dispatch_failures.drain(..overflow);
        }
    }

    fn render_dispatch_failure_lines(&self, width: usize) -> Vec<Line<'static>> {
        let shown = self
            .dispatch_failures
            .len()
            .min(MAX_VISIBLE_DISPATCH_FAILURES);
        let start = self.dispatch_failures.len().saturating_sub(shown);
        let mut lines = Vec::with_capacity(shown.saturating_add(1));
        for failure in &self.dispatch_failures[start..] {
            let slot = match (failure.label.as_deref(), failure.phase.as_deref()) {
                (Some(label), Some(phase)) if label != phase => {
                    format!("{} [{}]", short_label(label, 28), short_label(phase, 20))
                }
                (Some(label), _) => short_label(label, 28),
                (None, Some(phase)) => short_label(phase, 28),
                (None, None) => {
                    tr(self.locale, MessageId::WorkflowDispatchFallbackTask).into_owned()
                }
            };
            let message = if failure.message.is_empty() {
                tr(self.locale, MessageId::SetupStatusFailed).into_owned()
            } else {
                short_label(&failure.message, 160)
            };
            let text = tr(self.locale, MessageId::WorkflowDispatchFailureLine)
                .replace("{slot}", &slot)
                .replace("{message}", &message);
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(&text, width.max(1)),
                Style::default().fg(palette::STATUS_ERROR),
            )));
        }
        let omitted = self.dispatch_failure_count.saturating_sub(shown);
        if omitted > 0 {
            let text = tr(self.locale, MessageId::WorkflowDispatchFailuresOmitted)
                .replace("{count}", &omitted.to_string());
            lines.push(Line::from(Span::styled(
                truncate_line_to_width(&text, width.max(1)),
                Style::default().fg(palette::TEXT_MUTED),
            )));
        }
        lines
    }

    fn find_row_mut(&mut self, task_id: &str) -> Option<&mut WorkflowPanelRow> {
        for phase in &mut self.phases {
            if let Some(row) = phase.rows.iter_mut().find(|r| r.task_id == task_id) {
                return Some(row);
            }
        }
        None
    }

    fn gates_summary(&self) -> String {
        self.gates
            .iter()
            .take(6)
            .map(|gate| {
                let target = gate
                    .blocked_role
                    .as_deref()
                    .or(gate.role.as_deref())
                    .unwrap_or("-");
                if let Some(reason) = gate.blocked_reason.as_deref() {
                    format!(
                        "{}:{}->{} ({})",
                        short_label(&gate.gate_id, 18),
                        gate.state,
                        target,
                        short_label(reason, 40)
                    )
                } else {
                    format!(
                        "{}:{}->{}",
                        short_label(&gate.gate_id, 18),
                        gate.state,
                        target
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    }

    fn upsert_gate(&mut self, gate: WorkflowPanelGateLine) {
        if let Some(existing) = self
            .gates
            .iter_mut()
            .find(|existing| existing.gate_id == gate.gate_id)
        {
            *existing = gate;
        } else {
            self.gates.push(gate);
        }
    }

    fn finalize_running_rows(&mut self, status: WorkflowRowStatus, at_ms: u64) {
        for phase in &mut self.phases {
            for row in &mut phase.rows {
                if row.status.is_running() {
                    row.status = status;
                    row.completed_at_ms = Some(at_ms);
                    // A terminal Workflow row must keep the receipt shape even
                    // when cancellation arrived before provider telemetry.
                    // Unknown counters remain unknown; they never disappear or
                    // become fabricated zeros (#4039).
                    row.usage.get_or_insert_with(WorkflowRowUsage::default);
                }
            }
        }
    }
}

fn workflow_phase_run_json(phase: &WorkflowPanelPhase) -> Value {
    json!({
        "title": phase.title,
        "rows": phase.rows.iter().map(workflow_row_run_json).collect::<Vec<_>>(),
    })
}

fn workflow_row_run_json(row: &WorkflowPanelRow) -> Value {
    let usage = row.usage.as_ref().map(|usage| {
        json!({
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.total_tokens,
            "tool_calls": usage.tool_calls,
            "duration_ms": usage.duration_ms,
            "token_source": usage.token_source.map(WorkflowTokenSource::as_str),
        })
    });
    json!({
        "task_id": row.task_id,
        "label": row.label,
        "profile": row.profile,
        "model": row.model,
        "strength": row.strength,
        "worktree": row.worktree,
        "workspace": row.workspace.as_ref().map(|p| p.display().to_string()),
        "status": row.status.label(),
        "started_at_ms": row.started_at_ms,
        "completed_at_ms": row.completed_at_ms,
        "error": row.error,
        "schema_error": row.schema_error,
        "role": row.route.role,
        "provider": row.route.provider,
        "resolved_model": row.route.model,
        "requested_reasoning": row.route.requested_reasoning,
        "effective_reasoning": row.route.effective_reasoning,
        "route_source": row.route.route_source.map(WorkflowRouteSource::as_str),
        "child_route": row.route.child_route,
        "usage": usage,
    })
}

fn lifecycle_from_status(status: &str) -> WorkflowPanelLifecycle {
    match status {
        "running" => WorkflowPanelLifecycle::Running,
        "completed" | "succeeded" | "success" => WorkflowPanelLifecycle::Succeeded,
        "degraded" => WorkflowPanelLifecycle::Degraded,
        "failed" | "error" => WorkflowPanelLifecycle::Failed,
        "cancelled" | "canceled" => WorkflowPanelLifecycle::Cancelled,
        "pending" => WorkflowPanelLifecycle::Pending,
        _ => WorkflowPanelLifecycle::Failed,
    }
}

/// Drop a leading error-taxonomy tag (`[auth] `, `[rate_limit] `): the
/// sentence after it already says what kind of failure it was, and a one-row
/// surface has no columns to spare. The full error keeps its tag.
fn strip_error_tag(text: &str) -> &str {
    let trimmed = text.trim_start();
    trimmed
        .strip_prefix('[')
        .and_then(|rest| rest.split_once("] "))
        .filter(|(tag, _)| {
            !tag.is_empty()
                && tag.len() <= 24
                && tag
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        })
        .map_or(trimmed, |(_, rest)| rest)
}

/// The first sentence of `text`, whitespace-flattened, without its closing
/// full stop. A sentence ends at `.`, `!` or `?` followed by whitespace, or at
/// `;` — so `v0.10.1.` inside a sentence and `a.rs` never split it. Nothing is
/// cut mid-word: the caller truncates, at a word boundary, only what does not
/// fit.
pub(crate) fn first_sentence(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut end = flat.len();
    let mut chars = flat.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let next_is_space = chars.peek().is_some_and(|(_, next)| *next == ' ');
        if ch == ';' || (matches!(ch, '.' | '!' | '?') && next_is_space) {
            end = if ch == '.' || ch == ';' {
                index
            } else {
                index + ch.len_utf8()
            };
            break;
        }
    }
    flat[..end].trim_end_matches(['.', ' ']).trim().to_string()
}

/// `{count} running`-style labelled count.
fn count_text(locale: Locale, id: MessageId, count: usize) -> String {
    tr(locale, id).replace("{count}", &count.to_string())
}

fn localized_field(locale: Locale, id: MessageId, value: &str) -> String {
    tr(locale, id).replace("{value}", value)
}

/// Launch route only: role, provider/model, requested→effective reasoning,
/// route source (#4039).
fn route_receipt_parts(row: &WorkflowPanelRow, locale: Locale) -> Vec<String> {
    let unknown = tr(locale, MessageId::WorkflowReceiptUnknown).into_owned();
    let role = WorkflowRowRoute::field(row.route.role.as_ref(), locale);
    let provider = WorkflowRowRoute::field(row.route.provider.as_ref(), locale);
    let model = WorkflowRowRoute::field(row.route.model.as_ref(), locale);
    let requested = WorkflowRowRoute::field(row.route.requested_reasoning.as_ref(), locale);
    let effective = WorkflowRowRoute::field(row.route.effective_reasoning.as_ref(), locale);
    let source = row
        .route
        .route_source
        .map(WorkflowRouteSource::as_str)
        .unwrap_or(unknown.as_str());
    vec![
        localized_field(locale, MessageId::WorkflowReceiptRole, &role),
        format!("{provider}/{model}"),
        localized_field(
            locale,
            MessageId::WorkflowReceiptReasoning,
            &format!("{requested}→{effective}"),
        ),
        localized_field(locale, MessageId::WorkflowReceiptVia, source),
    ]
}

/// Full receipt: launch route plus, for a settled row, what it cost.
fn receipt_parts(row: &WorkflowPanelRow, locale: Locale) -> Vec<String> {
    let unknown = tr(locale, MessageId::WorkflowReceiptUnknown).into_owned();
    let mut parts = route_receipt_parts(row, locale);
    if let Some(usage) = row.usage.as_ref() {
        let tokens = usage.token_total().map_or_else(
            || unknown.clone(),
            |total| format!("{total} ({})", usage.token_source_label(locale)),
        );
        let tools = usage
            .tool_calls
            .map_or_else(|| unknown.clone(), |calls| calls.to_string());
        let duration = usage.duration_ms.map_or_else(
            || unknown.clone(),
            codewhale_command_contract::elapsed::format_elapsed_ms,
        );
        parts.extend([
            localized_field(locale, MessageId::WorkflowReceiptTokens, &tokens),
            localized_field(locale, MessageId::WorkflowReceiptTools, &tools),
            localized_field(locale, MessageId::WorkflowReceiptDuration, &duration),
        ]);
    }
    parts
}

/// Full English receipt retained for history serialization tests and text
/// exports. Renderers use [`receipt_line_strings`] so narrow terminals wrap
/// fields instead of dropping them.
#[must_use]
#[cfg(test)]
pub fn row_receipt_text(row: &WorkflowPanelRow) -> String {
    receipt_parts(row, Locale::En).join(" · ")
}

fn receipt_line_strings(
    row: &WorkflowPanelRow,
    locale: Locale,
    width: usize,
    requested_indent: usize,
) -> Vec<String> {
    pack_receipt_lines(receipt_parts(row, locale), width, requested_indent)
}

/// Pack receipt fields into indented lines that fit `width`, wrapping whole
/// fields first and hard-wrapping only a field wider than the line.
fn pack_receipt_lines(parts: Vec<String>, width: usize, requested_indent: usize) -> Vec<String> {
    let indent = requested_indent.min(width.saturating_sub(1));
    let prefix = " ".repeat(indent);
    let available = width.saturating_sub(indent).max(1);
    let mut packed = Vec::new();
    let mut current = String::new();

    for part in parts {
        if UnicodeWidthStr::width(part.as_str()) > available {
            if !current.is_empty() {
                packed.push(std::mem::take(&mut current));
            }
            packed.extend(hard_wrap_display(&part, available));
            continue;
        }
        let combined_width = if current.is_empty() {
            UnicodeWidthStr::width(part.as_str())
        } else {
            UnicodeWidthStr::width(current.as_str()) + 3 + UnicodeWidthStr::width(part.as_str())
        };
        if !current.is_empty() && combined_width > available {
            packed.push(std::mem::take(&mut current));
        }
        if current.is_empty() {
            current = part;
        } else {
            current.push_str(" · ");
            current.push_str(&part);
        }
    }
    if !current.is_empty() {
        packed.push(current);
    }
    packed
        .into_iter()
        .map(|line| format!("{prefix}{line}"))
        .collect()
}

fn hard_wrap_display(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if !line.is_empty() && used + ch_width > width {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push(ch);
        used += ch_width;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Read a terminal usage receipt out of a `task_completed` payload (#4039).
///
/// Absent counters stay `None`. An object that carries no counter at all still
/// produces a receipt so the row can say `unknown` in every column instead of
/// silently omitting the line.
fn usage_from_json(value: &Value) -> Option<WorkflowRowUsage> {
    let object = value.as_object()?;
    let number = |key: &str| object.get(key).and_then(Value::as_u64);
    Some(WorkflowRowUsage {
        input_tokens: number("input_tokens"),
        output_tokens: number("output_tokens"),
        total_tokens: number("total_tokens"),
        tool_calls: number("tool_calls").and_then(|calls| u32::try_from(calls).ok()),
        duration_ms: number("duration_ms"),
        token_source: opt_str(value, "token_source")
            .as_deref()
            .and_then(WorkflowTokenSource::parse),
    })
}

fn opt_str(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn short_label(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.width() <= max {
        return trimmed.to_string();
    }
    truncate_line_to_width(trimmed, max)
}

/// Terminal-safe role grammar from the underwater design contract. Labels
/// remain authoritative; the marks make siblings scan as the same work kind.
fn role_mark(profile: Option<&str>) -> &'static str {
    let role = profile.unwrap_or_default().trim().to_ascii_lowercase();
    if role.contains("operator") {
        "@"
    } else if role.contains("manager") || role.contains("lead") || role.contains("coordinator") {
        "/\\"
    } else if role.contains("scout") || role.contains("research") || role.contains("explor") {
        "<>"
    } else if role.contains("build") || role.contains("implement") || role.contains("engineer") {
        "[]"
    } else if role.contains("verif") || role.contains("test") || role.contains("qa") {
        "()"
    } else if role.contains("review") || role.contains("critic") {
        "**"
    } else {
        "--"
    }
}

fn row_elapsed_ms(row: &WorkflowPanelRow, now_ms: u64) -> u64 {
    row.completed_at_ms
        .unwrap_or(now_ms)
        .saturating_sub(row.started_at_ms)
}

fn lane_track(row: &WorkflowPanelRow, max_elapsed_ms: u64, width: usize, now_ms: u64) -> String {
    let width = width.max(4);
    let elapsed = row_elapsed_ms(row, now_ms);
    let filled = if max_elapsed_ms == 0 {
        1
    } else {
        ((elapsed as u128 * width as u128) / max_elapsed_ms as u128).clamp(1, width as u128)
            as usize
    };
    let end = match row.status {
        WorkflowRowStatus::Succeeded => "OK",
        WorkflowRowStatus::Failed | WorkflowRowStatus::SchemaFailed => "!!",
        WorkflowRowStatus::Cancelled => "XX",
        WorkflowRowStatus::Waiting => "? ",
        WorkflowRowStatus::Pending => ". ",
        WorkflowRowStatus::Running => "> ",
    };
    let body_width = width.saturating_sub(2);
    let active = filled.saturating_sub(2).min(body_width);
    format!(
        "{}{}{}",
        "=".repeat(active),
        end,
        "-".repeat(body_width.saturating_sub(active))
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The run's result as one readable line — the same wording the engine puts
/// on `run_completed.result_preview`, so the transcript and the parent receipt
/// never describe one result two ways.
fn summarize_result_value(value: &Value) -> Option<String> {
    codewhale_engine::tools::workflow::workflow_result_preview(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Exactly the flattened `task_started` payload the Workflow runtime emits
    /// (`WorkflowTaskStartedEvent` + `run_id`), so the projection is tested on
    /// the production wire shape rather than a hand-shaped struct.
    fn task_started_json(task_id: &str, provider: &str, model: &str) -> Value {
        json!({
            "type": "task_started",
            "at_ms": 1_200,
            "run_id": "workflow_abc",
            "task_id": task_id,
            "label": task_id,
            "role": "implementer",
            "profile": "impl-1",
            "model": "flash",
            "strength": "same",
            "thinking": "high",
            "requested_reasoning": "high",
            "effective_reasoning": "max",
            "resolved_role": "verifier",
            "resolved_profile": "verify-1",
            "resolved_provider": provider,
            "resolved_model": model,
            "route_source": "agent_profile.model",
            "worktree": false,
            "depth": 1,
            "workflow_run_id": "workflow_abc",
            "workflow_task_label": task_id,
        })
    }

    /// Detail view: the history card's expanded body, which carries every
    /// row's full receipt (#4039) now that live rows are one line (#6503).
    fn detail(panel: &WorkflowPanel, width: u16) -> String {
        panel
            .history_expanded_lines(width, &WorkflowHistoryExtras::default())
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// #4039: a row's receipt states the exact role, provider, model,
    /// requested → effective reasoning, and route source the runtime reported
    /// — and keeps stating them after the session routes somewhere else.
    #[test]
    fn row_route_receipt_is_exact_and_survives_a_later_model_switch() {
        let mut panel = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        panel.apply_json_event(&task_started_json("t1", "deepseek", "deepseek-v4-flash"));
        let before = detail(&panel, 200);
        assert!(before.contains("role verifier"), "{before}");
        assert!(before.contains("deepseek/deepseek-v4-flash"), "{before}");
        assert!(before.contains("reasoning high→max"), "{before}");
        assert!(before.contains("via agent_profile.model"), "{before}");
        // A running row claims no totals at all.
        assert!(!before.contains("tokens"), "{before}");

        // The session now routes elsewhere: a later task lands on another
        // provider/model. The already-launched row must not follow it.
        panel.apply_json_event(&task_started_json("t2", "moonshot", "kimi-k3"));
        let after = detail(&panel, 200);
        assert!(after.contains("deepseek/deepseek-v4-flash"), "{after}");
        assert!(after.contains("moonshot/kimi-k3"), "{after}");
        let t1 = panel
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .find(|row| row.task_id == "t1")
            .expect("t1 row");
        assert_eq!(t1.route.provider.as_deref(), Some("deepseek"));
        assert_eq!(t1.route.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(t1.route.requested_reasoning.as_deref(), Some("high"));
        assert_eq!(t1.route.effective_reasoning.as_deref(), Some("max"));
        assert_eq!(
            t1.route.route_source,
            Some(WorkflowRouteSource::AgentProfileModel)
        );
    }

    /// #4039: completed rows show provider-reported totals with their
    /// provenance, and unreported telemetry stays `unknown` — never `0`.
    #[test]
    fn completed_row_usage_is_reported_or_unknown_but_never_a_fabricated_zero() {
        let mut panel = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        panel.apply_json_event(&task_started_json("t1", "deepseek", "deepseek-v4-flash"));
        panel.apply_json_event(&task_started_json("t2", "deepseek", "deepseek-v4-flash"));
        panel.apply_json_event(&json!({
            "type": "task_completed",
            "at_ms": 3_200,
            "task_id": "t1",
            "status": "succeeded",
            "usage": {
                "input_tokens": 128,
                "output_tokens": 32,
                "total_tokens": 160,
                "tool_calls": 3,
                "duration_ms": 2_000,
                "token_source": "provider_reported",
            },
        }));
        // A provider that reported nothing: the runtime omits `usage`.
        panel.apply_json_event(&json!({
            "type": "task_completed",
            "at_ms": 3_400,
            "task_id": "t2",
            "status": "succeeded",
        }));

        let text = detail(&panel, 200);
        assert!(text.contains("tokens 160 (provider-reported)"), "{text}");
        assert!(text.contains("tools 3"), "{text}");
        assert!(text.contains("tokens unknown · tools unknown"), "{text}");
        let t2 = panel
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .find(|row| row.task_id == "t2")
            .expect("t2 row");
        let usage = t2.usage.as_ref().expect("completed rows carry a receipt");
        assert_eq!(usage.total_tokens, None);
        assert_eq!(usage.tool_calls, None);
    }

    /// #4039: the projection is built once from the events already applied —
    /// the history round trip must carry the receipts rather than force a
    /// re-scan of the durable journal.
    #[test]
    fn row_receipts_survive_the_history_round_trip() {
        let mut panel = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        panel.apply_json_event(&task_started_json("t1", "deepseek", "deepseek-v4-flash"));
        panel.apply_json_event(&json!({
            "type": "task_completed",
            "at_ms": 3_200,
            "task_id": "t1",
            "status": "succeeded",
            "usage": {
                "total_tokens": 160,
                "tool_calls": 3,
                "duration_ms": 2_000,
                "token_source": "provider_reported",
            },
        }));
        let original = panel
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .map(row_receipt_text)
            .collect::<Vec<_>>();

        let rehydrated = WorkflowPanel::from_run_json(&panel.to_run_json()).expect("rehydrate");
        let round_tripped = rehydrated
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .map(row_receipt_text)
            .collect::<Vec<_>>();
        assert_eq!(original, round_tripped);

        {
            let child_id = "persisted-child";
            let mut started = task_started_json(child_id, "deepseek", "deepseek-v4-flash");
            started["requested_reasoning"] = json!("low");
            started["effective_reasoning"] = json!("low");
            started["route_source"] = json!("task.model");
            let persisted_json = json!({
                "run_id":"workflow_abc", "workflow_goal":"audit", "started_at_ms":1_000,
                "events":[
                    {"type":"run_started", "run_id":"workflow_abc", "workflow_goal":"audit", "at_ms":1_000},
                    started,
                    {"type":"task_completed", "task_id":child_id, "status":"succeeded", "at_ms":3_200}
                ]
            });
            let mut panel = WorkflowPanel::from_run_json(&persisted_json)
                .expect("journal should hydrate workflow panel");
            let original_receipt = panel
                .phases
                .iter()
                .flat_map(|phase| phase.rows.iter())
                .find(|row| row.task_id == child_id)
                .map(row_receipt_text)
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
                .map(row_receipt_text)
                .expect("original child after later route");
            assert_eq!(unchanged, original_receipt);

            let history = WorkflowPanel::from_run_json(&panel.to_run_json())
                .expect("history receipt round trip");
            let later_receipt = history
                .phases
                .iter()
                .flat_map(|phase| phase.rows.iter())
                .find(|row| row.task_id == "later-route")
                .map(row_receipt_text)
                .expect("later route history receipt");
            assert!(later_receipt.contains("moonshot/kimi-k3"));
            assert!(later_receipt.contains("reasoning auto→medium"));
            assert!(later_receipt.contains("tokens unknown"));
            assert!(!later_receipt.contains("tokens 0"));
        }
    }

    /// #4039 compatibility: journals written before the receipt fields existed
    /// still project, and every missing field reads `unknown`.
    #[test]
    fn legacy_task_events_project_as_unknown_not_as_defaults() {
        let mut panel = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        panel.apply_json_event(&json!({
            "type": "task_started",
            "at_ms": 1_200,
            "task_id": "t1",
            "label": "legacy",
            "worktree": false,
        }));
        panel.apply_json_event(&json!({
            "type": "task_completed",
            "at_ms": 1_900,
            "task_id": "t1",
            "status": "succeeded",
        }));
        let text = detail(&panel, 200);
        let unknown_route = "role unknown · unknown/unknown · reasoning unknown→unknown";
        assert!(text.contains(unknown_route), "{text}");
        assert!(text.contains("via unknown"), "{text}");
        assert!(
            text.contains("tokens unknown · tools unknown · duration unknown"),
            "{text}"
        );
    }

    #[test]
    fn requested_model_and_foreign_provenance_never_become_effective_receipts() {
        let mut panel = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        panel.apply_json_event(&json!({
            "type": "task_started",
            "at_ms": 1_200,
            "task_id": "legacy",
            "model": "requested-only",
            "thinking": "high",
            "route_source": "foreign.source",
            "worktree": false,
        }));
        let row = panel
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .find(|row| row.task_id == "legacy")
            .expect("legacy row");
        assert_eq!(row.model.as_deref(), Some("requested-only"));
        assert_eq!(row.route.model, None);
        assert_eq!(row.route.route_source, None);
        let receipt = row_receipt_text(row);
        assert!(receipt.contains("unknown/unknown"), "{receipt}");
        assert!(receipt.contains("reasoning high→unknown"), "{receipt}");
        assert!(receipt.contains("via unknown"), "{receipt}");
    }

    #[test]
    fn receipt_provenance_is_closed_and_a_reported_zero_stays_zero() {
        let mut panel = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        panel.apply_json_event(&task_started_json("t1", "deepseek", "deepseek-v4-flash"));
        panel.apply_json_event(&json!({
            "type": "task_completed",
            "at_ms": 1_300,
            "task_id": "t1",
            "status": "succeeded",
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0,
                "total_tokens": 0,
                "tool_calls": 0,
                "duration_ms": 0,
                "token_source": "foreign-source",
            },
        }));
        let text = detail(&panel, 200);
        assert!(text.contains("tokens 0 (unknown)"), "{text}");
        assert!(!text.contains("foreign-source"), "{text}");
    }

    #[test]
    fn required_receipt_fields_survive_release_terminal_sizes() {
        let mut panel = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        panel.apply_json_event(&task_started_json("t1", "deepseek", "deepseek-v4-flash"));
        panel.apply_json_event(&json!({
            "type": "task_completed",
            "at_ms": 3_200,
            "task_id": "t1",
            "status": "succeeded",
            "usage": {
                "total_tokens": 160,
                "tool_calls": 3,
                "duration_ms": 2_000,
                "token_source": "provider_reported",
            },
        }));

        for width in [40_u16, 60, 80] {
            let lines = panel.history_expanded_lines(width, &WorkflowHistoryExtras::default());
            let text = lines
                .iter()
                .map(|line| {
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            for required in [
                "role verifier",
                "deepseek/deepseek-v4-flash",
                "reasoning high→max",
                "via agent_profile.model",
                "tokens 160 (provider-reported)",
                "tools 3",
                "duration 2s",
            ] {
                assert!(text.contains(required), "{width} lost {required}: {text}");
            }
            assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
        }
    }

    #[test]
    fn receipt_fields_flatten_controls_strip_ansi_and_redact_secrets() {
        let mut panel = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        panel.apply_json_event(&json!({
            "type": "task_started",
            "at_ms": 1_200,
            "task_id": "hostile",
            "resolved_role": "verifier\r\nFORGED ROLE",
            "resolved_provider": "\u{1b}[31mdeepseek\u{1b}[0m\tFORGED PROVIDER",
            "resolved_model": "model\napi_key=sk-receipt-secret-1234567890",
            "requested_reasoning": "high\rFORGED REASONING",
            "effective_reasoning": "max\tFORGED EFFECTIVE",
            "route_source": "task.model",
            "worktree": false,
        }));
        let row = panel
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .find(|row| row.task_id == "hostile")
            .expect("hostile row");
        let receipt = row_receipt_text(row);

        assert!(receipt.contains("verifier FORGED ROLE"), "{receipt:?}");
        assert!(receipt.contains("deepseek FORGED PROVIDER"), "{receipt:?}");
        assert!(receipt.contains("api_key=[redacted]"), "{receipt:?}");
        assert!(!receipt.contains("sk-receipt-secret"), "{receipt:?}");
        assert!(!receipt.chars().any(char::is_control), "{receipt:?}");
        for line in receipt_line_strings(row, Locale::En, 18, 2) {
            assert!(!line.chars().any(char::is_control), "{line:?}");
            assert!(line.width() <= 18, "{line:?}");
        }
    }

    fn started_panel() -> WorkflowPanel {
        let mut panel = WorkflowPanel::new("workflow_abc", "ship v0.8.68", 1_000);
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Analyze".to_string(),
            at_ms: 1_100,
        });
        panel.apply_event(WorkflowPanelEvent::TaskStarted {
            task_id: "t1".to_string(),
            label: Some("scout crates".to_string()),
            profile: Some("explore".to_string()),
            model: Some("flash".to_string()),
            strength: Some("low".to_string()),
            resolved_model: Some("deepseek-v4-flash".to_string()),
            worktree: true,
            workspace: Some(PathBuf::from("/tmp/wt-1")),
            route: Box::default(),
            at_ms: 1_200,
        });
        panel
    }

    #[test]
    fn late_run_completed_keeps_a_cancelled_run_cancelled() {
        let mut panel = started_panel();
        panel.apply_event(WorkflowPanelEvent::RunCancelled {
            reason: "stopped by you".to_string(),
            at_ms: 2_000,
        });
        panel.apply_event(WorkflowPanelEvent::RunCompleted {
            status: WorkflowPanelLifecycle::Succeeded,
            error: None,
            at_ms: 2_100,
        });
        assert_eq!(panel.lifecycle, WorkflowPanelLifecycle::Cancelled);
        assert_eq!(panel.error.as_deref(), Some("stopped by you"));
        assert_eq!(panel.completed_at_ms, Some(2_000));
        assert_eq!(
            panel.find_row_mut("t1").expect("row").status,
            WorkflowRowStatus::Cancelled
        );
    }

    /// U05-01: a child launch that raced the cancel (or any late same-run
    /// `task_started`) lands after the run settled. It is recorded, but it
    /// never reopens the run, and a settled row is never reset to running.
    #[test]
    fn late_task_started_never_reopens_a_settled_run() {
        let mut panel = started_panel();
        panel.apply_json_event(&json!({
            "type": "run_cancelled", "run_id": "workflow_abc",
            "reason": "stopped by you", "at_ms": 2_000,
        }));
        panel.apply_json_event(&task_started_json("t2", "deepseek", "flash"));
        panel.apply_json_event(&task_started_json("t1", "deepseek", "flash"));
        assert_eq!(panel.lifecycle, WorkflowPanelLifecycle::Cancelled);
        assert_eq!(
            panel.find_row_mut("t1").expect("t1").status,
            WorkflowRowStatus::Cancelled
        );
        assert_eq!(
            panel
                .find_row_mut("t2")
                .expect("late t2 is recorded")
                .status,
            WorkflowRowStatus::Cancelled
        );

        let mut panel = started_panel();
        panel.apply_json_event(&json!({
            "type": "task_completed", "run_id": "workflow_abc",
            "task_id": "t1", "status": "succeeded", "at_ms": 1_500,
        }));
        panel.apply_json_event(&json!({
            "type": "run_completed", "run_id": "workflow_abc",
            "status": "completed", "at_ms": 1_600,
        }));
        panel.apply_json_event(&task_started_json("t1", "deepseek", "flash"));
        assert_eq!(panel.lifecycle, WorkflowPanelLifecycle::Succeeded);
        assert_eq!(
            panel.find_row_mut("t1").expect("t1").status,
            WorkflowRowStatus::Succeeded
        );
    }

    /// U05-m1: an arriving task joins the runtime's newest phase, never the
    /// phase the user's cursor happens to rest on.
    #[test]
    fn arriving_task_joins_the_newest_phase_not_the_selected_one() {
        let mut panel = started_panel();
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Verify".to_string(),
            at_ms: 1_300,
        });
        panel.selected_phase = 0;
        panel.apply_json_event(&task_started_json("t2", "deepseek", "flash"));
        let ids = |phase: &WorkflowPanelPhase| {
            phase
                .rows
                .iter()
                .map(|row| row.task_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&panel.phases[0]), ["t1"]);
        assert_eq!(ids(&panel.phases[1]), ["t2"]);
    }

    /// U05-02: a terminal receipt with a missing or contradictory status is
    /// not evidence of success; it fails closed like an unknown status.
    #[test]
    fn malformed_completion_status_never_reads_as_success() {
        for status in [None, Some("running"), Some("pending")] {
            let mut panel = started_panel();
            let mut completed = json!({
                "type": "run_completed", "run_id": "workflow_abc", "at_ms": 2_000,
            });
            let mut task_completed = json!({
                "type": "task_completed", "run_id": "workflow_abc",
                "task_id": "t1", "at_ms": 1_900,
            });
            if let Some(status) = status {
                completed["status"] = json!(status);
                task_completed["status"] = json!(status);
            }
            panel.apply_json_event(&task_completed);
            panel.apply_json_event(&completed);
            assert_eq!(
                panel.lifecycle,
                WorkflowPanelLifecycle::Failed,
                "{status:?}"
            );
            assert_eq!(
                panel.find_row_mut("t1").expect("row").status,
                WorkflowRowStatus::Failed
            );
        }
    }

    /// #4208: every decorative glyph the run map emits — expand marks, role
    /// marks, lane glyphs, gates, status marks across running, waiting,
    /// failed, cancelled, and completed members — must narrow to an
    /// ASCII-safe alternative.
    #[test]
    fn workflow_panel_glyphs_all_have_ascii_alternatives() {
        let mut panel = started_panel();
        for (task_id, status) in [
            ("t1", WorkflowRowStatus::Succeeded),
            ("t2", WorkflowRowStatus::Failed),
            ("t3", WorkflowRowStatus::Cancelled),
            ("t4", WorkflowRowStatus::Waiting),
        ] {
            if task_id != "t1" {
                panel.apply_event(WorkflowPanelEvent::TaskStarted {
                    task_id: task_id.to_string(),
                    label: Some(format!("lane {task_id}")),
                    profile: Some("implementer".to_string()),
                    model: None,
                    strength: None,
                    resolved_model: None,
                    worktree: false,
                    workspace: None,
                    route: Box::default(),
                    at_ms: 1_400,
                });
            }
            panel.apply_event(WorkflowPanelEvent::TaskCompleted {
                task_id: task_id.to_string(),
                status,
                usage: None,
                reason: None,
                at_ms: 2_500,
            });
        }
        panel.apply_event(WorkflowPanelEvent::GateUpdated {
            gate_id: "gate-1".to_string(),
            role: Some("verifier".to_string()),
            gate: Some("tests-green".to_string()),
            state: "blocked".to_string(),
            blocked_role: Some("implementer".to_string()),
            blocked_reason: Some("waiting on tests".to_string()),
            at_ms: 2_600,
        });

        let mut glyphs: Vec<char> = panel.compact_summary_text(120).chars().collect();
        for line in panel.render_history_card(100, true, &WorkflowHistoryExtras::default()) {
            for span in &line.spans {
                glyphs.extend(span.content.chars());
            }
        }
        for ch in glyphs.into_iter().filter(|ch| !ch.is_ascii()) {
            let mut cell = ratatui::buffer::Cell::default();
            cell.set_symbol(&ch.to_string());
            crate::tui::color_compat::adapt_cell_symbol_for_ascii(&mut cell);
            assert!(
                cell.symbol().is_ascii(),
                "workflow glyph {ch:?} (U+{:04X}) lacks an ASCII-safe alternative",
                ch as u32
            );
        }
    }

    #[test]
    fn a_repeated_start_of_the_same_run_keeps_settled_rows_and_outcome() {
        let mut panel = started_panel();
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "t1".to_string(),
            status: WorkflowRowStatus::Succeeded,
            usage: None,
            reason: None,
            at_ms: 1_400,
        });
        panel.apply_event(WorkflowPanelEvent::RunCompleted {
            status: WorkflowPanelLifecycle::Succeeded,
            error: None,
            at_ms: 1_500,
        });
        let rows = |panel: &WorkflowPanel| panel.phases.iter().map(|p| p.rows.len()).sum::<usize>();
        let settled_rows = rows(&panel);
        assert!(settled_rows > 0);
        assert!(panel.apply_json_event(&json!({
            "type": "run_started",
            "run_id": "workflow_abc",
            "at_ms": 1_600,
            "workflow_goal": "ship v0.8.68",
            "token_budget": 9_000
        })));
        assert_eq!(panel.lifecycle, WorkflowPanelLifecycle::Succeeded);
        assert_eq!(rows(&panel), settled_rows);
        assert_eq!(
            panel.budget_total,
            Some(9_000),
            "missing metadata is filled"
        );
    }

    #[test]
    fn json_events_round_trip_without_log_flood() {
        let mut panel = WorkflowPanel::new("w1", "goal", 0);
        let events = vec![
            json!({
                "type": "run_started",
                "at_ms": 10,
                "run_id": "w1",
                "workflow_goal": "demo",
                "token_budget": 5000
            }),
            json!({"type": "log", "at_ms": 11, "message": "should not appear"}),
            json!({"type": "phase_started", "at_ms": 12, "title": "Analyze"}),
            json!({
                "type": "task_started",
                "at_ms": 13,
                "task_id": "a",
                "label": "scout",
                "profile": "explore",
                "resolved_model": "flash",
                "worktree": true
            }),
            json!({
                "type": "budget_updated",
                "at_ms": 14,
                "total": 5000,
                "spent": 100,
                "remaining": 4900
            }),
            json!({
                "type": "task_completed",
                "at_ms": 15,
                "task_id": "a",
                "status": "succeeded"
            }),
            json!({
                "type": "gate_updated",
                "at_ms": 15,
                "gate_id": "reviewer-diff",
                "role": "reviewer",
                "gate": "review",
                "state": "blocked",
                "blocked_role": "verifier",
                "blocked_reason": "review found regression"
            }),
            json!({
                "type": "run_completed",
                "at_ms": 16,
                "status": "completed"
            }),
        ];
        panel.apply_json_events(&events);
        assert_eq!(panel.label, "demo");
        assert_eq!(panel.lifecycle, WorkflowPanelLifecycle::Succeeded);
        assert_eq!(panel.budget_spent, 100);
        assert_eq!(panel.budget_remaining, Some(4900));
        let joined: String = panel
            .render_history_card(100, true, &WorkflowHistoryExtras::default())
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!joined.contains("should not appear"), "{joined}");
        assert!(joined.contains("scout"), "{joined}");
        assert!(joined.contains("done"), "{joined}");
        assert!(joined.contains("reviewer-diff"), "{joined}");
        assert!(joined.contains("review found regression"), "{joined}");
    }

    #[test]
    fn failure_and_cancel_counts_roll_up_in_header() {
        let mut panel = started_panel();
        panel.apply_event(WorkflowPanelEvent::TaskStarted {
            task_id: "t2".to_string(),
            label: Some("b".to_string()),
            profile: None,
            model: None,
            strength: None,
            resolved_model: None,
            worktree: false,
            workspace: None,
            route: Box::default(),
            at_ms: 1_300,
        });
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "t1".to_string(),
            status: WorkflowRowStatus::Failed,
            usage: None,
            reason: None,
            at_ms: 1_400,
        });
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "t2".to_string(),
            status: WorkflowRowStatus::Cancelled,
            usage: None,
            reason: None,
            at_ms: 1_500,
        });
        let (failed, cancelled) = panel.failure_cancel_counts();
        assert_eq!(failed, 1);
        assert_eq!(cancelled, 1);
        let header = panel.compact_summary_text(100);
        assert!(header.contains("1 fail"), "{header}");
        assert!(header.contains("1 cancel"), "{header}");
        assert!(header.contains("2/2"), "{header}");
    }

    #[test]
    fn task_started_json_prefers_workflow_task_label_over_generic_label() {
        // #4119: panel rows use typed workflow metadata, not prompt text.
        let event = WorkflowPanelEvent::from_json_value(&json!({
            "type": "task_started",
            "task_id": "child-1",
            "label": "fallback-label",
            "workflow_task_label": "typed-label",
            "workflow_run_id": "run-xyz",
            "workflow_phase_id": "dispatch",
            "workflow_child_index": 2,
            "at_ms": 42,
        }))
        .expect("task_started parses");
        match event {
            WorkflowPanelEvent::TaskStarted { label, .. } => {
                assert_eq!(label.as_deref(), Some("typed-label"));
            }
            other => panic!("expected TaskStarted, got {other:?}"),
        }

        let mut panel = WorkflowPanel::new("run-xyz", "goal", 1);
        panel.apply_json_event(&json!({
            "type": "task_started",
            "task_id": "child-1",
            "label": "fallback-label",
            "workflow_task_label": "typed-label",
            "at_ms": 42,
        }));
        let row = panel
            .phases
            .iter()
            .flat_map(|phase| phase.rows.iter())
            .find(|row| row.task_id == "child-1")
            .expect("row recorded");
        assert_eq!(row.label, "typed-label");
    }

    #[test]
    fn explicit_event_run_id_cannot_cross_panel_run() {
        let mut panel = WorkflowPanel::new("run-b", "active run", 2_000);

        assert!(!panel.apply_json_event(&json!({
            "type": "run_started",
            "run_id": "run-a",
            "workflow_goal": "late prior run",
            "at_ms": 1_500,
        })));
        assert!(!panel.apply_json_event(&json!({
            "type": "phase_started",
            "run_id": "run-a",
            "title": "Late A phase",
            "at_ms": 2_100,
        })));
        assert!(panel.phases.is_empty());
        assert_eq!(panel.run_id, "run-b");

        assert!(panel.apply_json_event(&json!({
            "type": "phase_started",
            "run_id": "run-b",
            "title": "B phase",
            "at_ms": 2_200,
        })));
        assert_eq!(panel.phases.len(), 1);
        assert_eq!(panel.phases[0].title, "B phase");
    }

    #[test]
    fn dispatch_failure_event_surfaces_without_inventing_a_child() {
        let mut panel = started_panel();
        panel.apply_json_event(&json!({
            "type": "task_dispatch_failed",
            "label": "review docs",
            "phase": "Analyze",
            "message": "unknown agent profile reviewer",
            "at_ms": 1_250,
        }));

        assert_eq!(panel.done_total(), (0, 1), "rejected launch is not a child");
        assert_eq!(panel.failure_cancel_counts(), (1, 0));
        assert_eq!(panel.lifecycle, WorkflowPanelLifecycle::Running);
        assert!(panel.compact_summary_text(120).contains("1 fail"));

        let live = panel
            .render_history_card(120, true, &WorkflowHistoryExtras::default())
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "dispatch failed",
            "review docs",
            "Analyze",
            "unknown agent profile reviewer",
        ] {
            assert!(live.contains(expected), "missing {expected}: {live}");
        }

        let snapshot = panel.to_run_json();
        assert_eq!(snapshot["dispatch_failure_count"], 1);
        assert_eq!(
            snapshot["dispatch_failures"].as_array().map(Vec::len),
            Some(1)
        );
        let restored = WorkflowPanel::from_run_json(&snapshot).expect("panel rehydrates");
        assert_eq!(restored.done_total(), (0, 1));
        assert_eq!(restored.failure_cancel_counts(), (1, 0));
        let history = restored
            .render_history_card(120, true, &WorkflowHistoryExtras::default())
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        assert!(history.contains("dispatch failed"), "{history}");
        assert!(
            history.contains("unknown agent profile reviewer"),
            "{history}"
        );

        let mut japanese = restored.clone();
        japanese.locale = Locale::Ja;
        let localized = japanese
            .render_history_card(120, true, &WorkflowHistoryExtras::default())
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        assert!(localized.contains("ディスパッチ失敗"), "{localized}");
        assert!(!localized.contains("dispatch failed"), "{localized}");
    }

    #[test]
    fn degraded_run_preserves_partial_success_as_a_distinct_terminal_state() {
        let mut panel = started_panel();
        panel.apply_json_event(&json!({
            "type": "task_completed",
            "task_id": "t1",
            "status": "succeeded",
            "at_ms": 1_300,
        }));
        panel.apply_json_event(&json!({
            "type": "task_dispatch_failed",
            "label": "review docs",
            "message": "profile unavailable",
            "at_ms": 1_350,
        }));
        panel.apply_json_event(&json!({
            "type": "run_completed",
            "status": "degraded",
            "error": "completed with dropped slots",
            "at_ms": 1_400,
        }));

        assert_eq!(panel.lifecycle, WorkflowPanelLifecycle::Degraded);
        assert!(panel.lifecycle.is_terminal());
        assert_eq!(panel.done_total(), (1, 1));
        assert_eq!(panel.failure_cancel_counts(), (1, 0));
        assert!(panel.compact_summary_text(120).contains("degraded"));

        panel.locale = Locale::Ja;
        assert!(panel.compact_summary_text(120).contains("一部失敗"));
    }

    #[test]
    fn dispatch_failure_tail_is_bounded_and_redacted() {
        let mut panel = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        for index in 0..20 {
            panel.apply_json_event(&json!({
                "type": "task_dispatch_failed",
                "label": format!("job-{index}"),
                "message": if index == 19 {
                    "\u{1b}[31mapi_key=sk-dispatch-secret-1234567890\u{1b}[0m\nfailed"
                } else {
                    "profile unavailable"
                },
                "at_ms": 1_100 + index,
            }));
        }

        assert_eq!(panel.dispatch_failure_count, 20);
        assert_eq!(
            panel.dispatch_failures.len(),
            MAX_DISPATCH_FAILURES_RETAINED
        );
        assert_eq!(
            panel
                .dispatch_failures
                .first()
                .and_then(|failure| failure.label.as_deref()),
            Some("job-8")
        );
        let latest = panel.dispatch_failures.last().expect("latest failure");
        assert!(!latest.message.contains("sk-dispatch-secret"));
        assert!(!latest.message.chars().any(char::is_control));
        let rendered = panel
            .render_history_card(120, true, &WorkflowHistoryExtras::default())
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
            .collect::<String>();
        assert!(rendered.contains("17 earlier not shown"), "{rendered}");
        assert!(!rendered.contains("sk-dispatch-secret"), "{rendered}");
    }

    #[test]
    fn run_json_overlap_does_not_double_count_dispatch_failure_ledger() {
        let failure = json!({
            "type": "task_dispatch_failed",
            "label": "review docs",
            "phase": "Analyze",
            "message": "profile unavailable",
            "at_ms": 1_250,
        });
        let panel = WorkflowPanel::from_run_json(&json!({
            "run_id": "workflow_abc",
            "workflow_goal": "audit",
            "started_at_ms": 1_000,
            "events": [
                {
                    "type": "run_started",
                    "run_id": "workflow_abc",
                    "workflow_goal": "audit",
                    "at_ms": 1_000,
                },
                failure.clone(),
            ],
            "dispatch_failure_count": 1,
            "dispatch_failures": [{
                "label": "review docs",
                "phase": "Analyze",
                "message": "profile unavailable",
                "at_ms": 1_250,
            }],
        }))
        .expect("panel rehydrates");

        assert_eq!(panel.dispatch_failure_count, 1);
        assert_eq!(panel.dispatch_failures.len(), 1);
        assert_eq!(panel.failure_cancel_counts(), (1, 0));

        let mut live = WorkflowPanel::new("workflow_abc", "audit", 1_000);
        live.apply_json_event(&failure);
        live.apply_json_events(std::slice::from_ref(&failure));
        live.merge_dispatch_failures_from_run_json(&json!({
            "dispatch_failure_count": 1,
            "dispatch_failures": [{
                "label": "review docs",
                "phase": "Analyze",
                "message": "profile unavailable",
                "at_ms": 1_250,
            }],
        }));
        assert_eq!(
            live.dispatch_failure_count, 1,
            "authoritative completion ledger must absorb retained replay"
        );
        live.apply_json_events(&[failure.clone(), failure]);
        live.merge_dispatch_failures_from_run_json(&json!({
            "dispatch_failure_count": 2,
            "dispatch_failures": [
                {
                    "label": "review docs",
                    "phase": "Analyze",
                    "message": "profile unavailable",
                    "at_ms": 1_250,
                },
                {
                    "label": "review docs",
                    "phase": "Analyze",
                    "message": "profile unavailable",
                    "at_ms": 1_250,
                },
            ],
        }));
        assert_eq!(
            live.dispatch_failure_count, 2,
            "authoritative count must preserve two genuinely identical slots"
        );
    }

    #[test]
    fn imported_max_dispatch_count_cannot_overflow_failed_child_rollup() {
        let mut panel = started_panel();
        panel.dispatch_failure_count = usize::MAX;
        panel.find_row_mut("t1").expect("row").status = WorkflowRowStatus::Failed;
        assert_eq!(panel.failure_cancel_counts(), (usize::MAX, 0));
    }

    #[test]
    fn compact_history_card_summarizes_lifecycle_children_phases_failures_elapsed() {
        let mut panel = started_panel();
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "t1".to_string(),
            status: WorkflowRowStatus::Failed,
            usage: None,
            reason: None,
            at_ms: 2_000,
        });
        panel.apply_event(WorkflowPanelEvent::RunCompleted {
            status: WorkflowPanelLifecycle::Failed,
            error: Some("scout failed".to_string()),
            at_ms: 2_100,
        });
        let lines = panel.render_history_card(120, false, &WorkflowHistoryExtras::default());
        assert_eq!(lines.len(), 1, "compact is a single summary line");
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(joined.contains('▶'), "collapsed glyph: {joined}");
        assert!(
            joined.contains("failed") || joined.contains("fail"),
            "{joined}"
        );
        assert!(joined.contains("1/1 done"), "{joined}");
        assert!(joined.contains("1 phase"), "{joined}");
        assert!(joined.contains("1 fail"), "{joined}");
        // elapsed is present (0s or more depending on timestamps)
        assert!(
            joined.contains('s') || joined.contains('m'),
            "elapsed time expected: {joined}"
        );
        // Goal is reserved for the expanded body so compact stays under the
        // tool-header summary budget.
        assert!(
            !joined.contains("ship v0.8.68"),
            "compact must not spend budget on free-text goal: {joined}"
        );
    }

    #[test]
    fn expanded_history_card_shows_phase_child_result_links_and_failures() {
        let mut panel = started_panel();
        panel.source_path = Some(PathBuf::from("workflows/demo.workflow.js"));
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "t1".to_string(),
            status: WorkflowRowStatus::Failed,
            usage: None,
            reason: None,
            at_ms: 2_000,
        });
        if let Some(row) = panel.find_row_mut("t1") {
            row.error = Some("timeout waiting for model".to_string());
        }
        panel.apply_event(WorkflowPanelEvent::RunCompleted {
            status: WorkflowPanelLifecycle::Failed,
            error: Some("phase Analyze failed".to_string()),
            at_ms: 2_100,
        });
        let extras = WorkflowHistoryExtras {
            result_summary: Some("no ship blockers found".to_string()),
            source_path: None,
            spillover_path: Some(PathBuf::from("/tmp/workflow-out.json")),
            verification_summary: None,
        };
        let lines = panel.render_history_card(120, true, &extras);
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains('▼'), "expanded glyph: {joined}");
        assert!(joined.contains("goal:"), "{joined}");
        assert!(joined.contains("ship v0.8.68"), "{joined}");
        assert!(joined.contains("phases:"), "{joined}");
        assert!(joined.contains("Analyze"), "{joined}");
        assert!(joined.contains("children:"), "{joined}");
        assert!(joined.contains("scout crates"), "{joined}");
        assert!(joined.contains("result:"), "{joined}");
        assert!(joined.contains("no ship blockers"), "{joined}");
        assert!(
            joined.contains("source:") || joined.contains("demo.workflow"),
            "{joined}"
        );
        assert!(joined.contains("artifact:"), "{joined}");
        assert!(joined.contains("error:"), "{joined}");
        assert!(joined.contains("phase Analyze failed"), "{joined}");
        assert!(
            joined.contains("fail") || joined.contains("timeout"),
            "{joined}"
        );
    }

    #[test]
    fn direct_subagent_card_reuses_history_renderer() {
        let panel = WorkflowPanel::from_direct_subagent(
            "agent_abc",
            "explore",
            WorkflowPanelLifecycle::Succeeded,
            1_000,
            Some(4_500),
            Some("found 3 call sites".to_string()),
            None,
        );
        let compact = panel.render_history_card(100, false, &WorkflowHistoryExtras::default());
        let joined: String = compact
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            joined.contains("success") || joined.contains("explore"),
            "{joined}"
        );
        assert!(joined.contains("1/1 done"), "{joined}");
        assert!(joined.contains("3s") || joined.contains("s"), "{joined}");

        let expanded = panel.render_history_card(
            100,
            true,
            &WorkflowHistoryExtras {
                result_summary: Some("found 3 call sites".to_string()),
                ..WorkflowHistoryExtras::default()
            },
        );
        let joined: String = expanded
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("children:"), "{joined}");
        assert!(joined.contains("result:"), "{joined}");
        assert!(joined.contains("found 3 call sites"), "{joined}");
        assert!(!joined.contains("reasoning unknown"), "{joined}");
        assert!(!joined.contains("via unknown"), "{joined}");
        assert!(!joined.contains("tokens unknown"), "{joined}");
        assert!(
            joined.contains(crate::tui::shell_key_routing::tool_details_chord().as_ref()),
            "history details hint must use the platform chord: {joined}"
        );
        assert!(!joined.contains("details (v)"), "{joined}");
    }

    #[test]
    fn from_run_json_round_trips_events_into_history_card() {
        let value = json!({
            "run_id": "workflow_demo",
            "status": "completed",
            "workflow_goal": "ship it",
            "started_at_ms": 1000,
            "completed_at_ms": 5000,
            "events": [
                {
                    "type": "run_started",
                    "at_ms": 1000,
                    "run_id": "workflow_demo",
                    "workflow_goal": "ship it"
                },
                {"type": "phase_started", "at_ms": 1100, "title": "Build"},
                {
                    "type": "task_started",
                    "at_ms": 1200,
                    "task_id": "t1",
                    "label": "compile",
                    "profile": "implementer"
                },
                {
                    "type": "task_completed",
                    "at_ms": 4000,
                    "task_id": "t1",
                    "status": "succeeded"
                },
                {"type": "run_completed", "at_ms": 5000, "status": "completed"}
            ]
        });
        let panel = WorkflowPanel::from_run_json(&value).expect("hydrate");
        assert_eq!(panel.lifecycle, WorkflowPanelLifecycle::Succeeded);
        let compact = panel.compact_summary_text(120);
        assert!(compact.contains("1/1 done"), "{compact}");
        assert!(compact.contains("success"), "{compact}");
        let expanded = panel.history_expanded_lines(120, &WorkflowHistoryExtras::default());
        let joined: String = expanded
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("goal:"), "{joined}");
        assert!(joined.contains("ship it"), "{joined}");
        assert!(joined.contains("Build"), "{joined}");
        assert!(joined.contains("compile"), "{joined}");
    }

    // ── #4131 dogfood scenario projections ──────────────────────────────────

    /// WF-A1: read-only repo audit — scout phase on main workspace, labeled
    /// children, no worktree marker, synthesizer phase present.
    #[test]
    fn dogfood_read_only_repo_audit_panel() {
        let mut panel = WorkflowPanel::new("wf_a1", "read-only repo audit", 1_000);
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Scout".to_string(),
            at_ms: 1_100,
        });
        for (id, label, role) in [
            ("t1", "map crates", "explore"),
            ("t2", "scan unsafe", "explore"),
            ("t3", "scan unwrap", "explore"),
        ] {
            panel.apply_event(WorkflowPanelEvent::TaskStarted {
                task_id: id.to_string(),
                label: Some(label.to_string()),
                profile: Some(role.to_string()),
                model: Some("flash".to_string()),
                strength: Some("low".to_string()),
                resolved_model: Some("deepseek-v4-flash".to_string()),
                worktree: false,
                workspace: None,
                route: Box::default(),
                at_ms: 1_200,
            });
            panel.apply_event(WorkflowPanelEvent::TaskCompleted {
                task_id: id.to_string(),
                status: WorkflowRowStatus::Succeeded,
                usage: None,
                reason: None,
                at_ms: 1_500,
            });
        }
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Synthesize".to_string(),
            at_ms: 1_600,
        });
        panel.apply_event(WorkflowPanelEvent::TaskStarted {
            task_id: "t4".to_string(),
            label: Some("audit summary".to_string()),
            profile: Some("general".to_string()),
            model: None,
            strength: None,
            resolved_model: None,
            worktree: false,
            workspace: None,
            route: Box::default(),
            at_ms: 1_700,
        });
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "t4".to_string(),
            status: WorkflowRowStatus::Succeeded,
            usage: None,
            reason: None,
            at_ms: 2_000,
        });
        panel.apply_event(WorkflowPanelEvent::RunCompleted {
            status: WorkflowPanelLifecycle::Succeeded,
            error: None,
            at_ms: 2_100,
        });

        let header = panel.compact_summary_text(140);
        assert!(
            header.contains("success") || header.contains("completed"),
            "{header}"
        );
        assert!(!header.contains("fail"), "{header}");
        assert!(
            header.contains("4/") || header.contains("4 child") || header.contains("0/"),
            "{header}"
        );

        // Selected phase is Synthesize; scout labels live in earlier phases.
        panel.selected_phase = 0;
        let scout_body = panel.render_history_card(120, true, &WorkflowHistoryExtras::default());
        let scout_joined: String = scout_body
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(scout_joined.contains("map crates"), "{scout_joined}");
        assert!(
            !scout_joined.contains(" wt "),
            "read-only scouts stay on main: {scout_joined}"
        );

        let card = panel.render_history_card(
            120,
            true,
            &WorkflowHistoryExtras {
                result_summary: Some("no critical issues".to_string()),
                ..WorkflowHistoryExtras::default()
            },
        );
        let card_text: String = card
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            card_text.contains("Scout") || card_text.contains("Synthesize"),
            "{card_text}"
        );
        assert!(card_text.contains("no critical issues"), "{card_text}");
        assert!(
            !card_text.to_ascii_lowercase().contains("unknown child"),
            "{card_text}"
        );
    }

    /// WF-A2: staged bugfix — implementer worktree + verifier on main.
    #[test]
    fn dogfood_staged_worktree_implementer_verifier() {
        let mut panel = WorkflowPanel::new("wf_a2", "staged docs fix", 1_000);
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Implement".to_string(),
            at_ms: 1_100,
        });
        panel.apply_event(WorkflowPanelEvent::TaskStarted {
            task_id: "impl".to_string(),
            label: Some("implementer".to_string()),
            profile: Some("implementer".to_string()),
            model: Some("pro".to_string()),
            strength: None,
            resolved_model: Some("deepseek-v4-pro".to_string()),
            worktree: true,
            workspace: Some(PathBuf::from("/tmp/wt-impl")),
            route: Box::default(),
            at_ms: 1_200,
        });
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "impl".to_string(),
            status: WorkflowRowStatus::Succeeded,
            usage: None,
            reason: None,
            at_ms: 2_000,
        });
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Verify".to_string(),
            at_ms: 2_100,
        });
        panel.apply_event(WorkflowPanelEvent::TaskStarted {
            task_id: "ver".to_string(),
            label: Some("verifier".to_string()),
            profile: Some("verifier".to_string()),
            model: Some("flash".to_string()),
            strength: None,
            resolved_model: None,
            worktree: false,
            workspace: None,
            route: Box::default(),
            at_ms: 2_200,
        });
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "ver".to_string(),
            status: WorkflowRowStatus::Succeeded,
            usage: None,
            reason: None,
            at_ms: 3_000,
        });
        panel.apply_event(WorkflowPanelEvent::RunCompleted {
            status: WorkflowPanelLifecycle::Succeeded,
            error: None,
            at_ms: 3_100,
        });

        assert_eq!(panel.phases.len(), 2);
        assert_eq!(panel.phases[0].title, "Implement");
        assert_eq!(panel.phases[1].title, "Verify");

        panel.selected_phase = 0;
        let implement_body =
            panel.render_history_card(140, true, &WorkflowHistoryExtras::default());
        let impl_text: String = implement_body
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(impl_text.contains("implementer"), "{impl_text}");

        panel.selected_phase = 1;
        let verify_body = panel.render_history_card(140, true, &WorkflowHistoryExtras::default());
        let ver_text: String = verify_body
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(ver_text.contains("verifier"), "{ver_text}");
        assert!(
            !ver_text.contains("  wt"),
            "verifier stays on main: {ver_text}"
        );
    }

    /// WF-A3: partial failure + synthesis — fail count visible, summary card.
    #[test]
    fn dogfood_partial_failure_and_synthesis() {
        let mut panel = WorkflowPanel::new("wf_a3", "partial failure synthesis", 1_000);
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Parallel scouts".to_string(),
            at_ms: 1_100,
        });
        for (id, label, status) in [
            ("a", "scout-a", WorkflowRowStatus::Succeeded),
            ("b", "scout-b-fail", WorkflowRowStatus::Failed),
            ("c", "scout-c", WorkflowRowStatus::Succeeded),
        ] {
            panel.apply_event(WorkflowPanelEvent::TaskStarted {
                task_id: id.to_string(),
                label: Some(label.to_string()),
                profile: Some("explore".to_string()),
                model: None,
                strength: None,
                resolved_model: None,
                worktree: false,
                workspace: None,
                route: Box::default(),
                at_ms: 1_200,
            });
            panel.apply_event(WorkflowPanelEvent::TaskCompleted {
                task_id: id.to_string(),
                status,
                usage: None,
                reason: None,
                at_ms: 1_500,
            });
        }
        if let Some(row) = panel.find_row_mut("b") {
            row.error = Some("scout refused to produce summary".to_string());
        }
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Synthesize".to_string(),
            at_ms: 1_600,
        });
        panel.apply_event(WorkflowPanelEvent::TaskStarted {
            task_id: "syn".to_string(),
            label: Some("synthesizer".to_string()),
            profile: Some("general".to_string()),
            model: None,
            strength: None,
            resolved_model: None,
            worktree: false,
            workspace: None,
            route: Box::default(),
            at_ms: 1_700,
        });
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "syn".to_string(),
            status: WorkflowRowStatus::Succeeded,
            usage: None,
            reason: None,
            at_ms: 2_000,
        });
        // Partial success at run level: completed with surviving synthesis.
        panel.apply_event(WorkflowPanelEvent::RunCompleted {
            status: WorkflowPanelLifecycle::Succeeded,
            error: None,
            at_ms: 2_100,
        });

        let (failed, cancelled) = panel.failure_cancel_counts();
        assert_eq!(failed, 1, "exactly one parallel slot failed");
        assert_eq!(cancelled, 0);
        let header = panel.compact_summary_text(140);
        assert!(header.contains("1 fail"), "{header}");

        let card = panel.render_history_card(
            140,
            true,
            &WorkflowHistoryExtras {
                result_summary: Some("2/3 scouts ok; scout-b failed".to_string()),
                ..WorkflowHistoryExtras::default()
            },
        );
        let joined: String = card
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("scout-b-fail") || joined.contains("fail"),
            "{joined}"
        );
        assert!(joined.contains("2/3 scouts ok"), "{joined}");
    }

    /// WF-A4: cancellation mid-run — running children cancelled, done preserved.
    #[test]
    fn dogfood_cancellation_mid_run() {
        let mut panel = WorkflowPanel::new("wf_a4", "cancel mid-run", 1_000);
        panel.apply_event(WorkflowPanelEvent::PhaseStarted {
            title: "Long work".to_string(),
            at_ms: 1_100,
        });
        panel.apply_event(WorkflowPanelEvent::TaskStarted {
            task_id: "slow-1".to_string(),
            label: Some("slow-1".to_string()),
            profile: Some("explore".to_string()),
            model: None,
            strength: None,
            resolved_model: None,
            worktree: false,
            workspace: None,
            route: Box::default(),
            at_ms: 1_200,
        });
        panel.apply_event(WorkflowPanelEvent::TaskStarted {
            task_id: "slow-2".to_string(),
            label: Some("slow-2".to_string()),
            profile: Some("explore".to_string()),
            model: None,
            strength: None,
            resolved_model: None,
            worktree: false,
            workspace: None,
            route: Box::default(),
            at_ms: 1_210,
        });
        panel.apply_event(WorkflowPanelEvent::TaskCompleted {
            task_id: "slow-1".to_string(),
            status: WorkflowRowStatus::Succeeded,
            usage: None,
            reason: None,
            at_ms: 1_500,
        });

        // A confirmed host interrupt finalizes remaining runners. The widget
        // itself never claims cancellation before that runtime event.
        panel.apply_event(WorkflowPanelEvent::RunCancelled {
            reason: "interrupted".to_string(),
            at_ms: now_ms(),
        });
        assert_eq!(panel.lifecycle, WorkflowPanelLifecycle::Cancelled);

        let slow1 = panel
            .phases
            .iter()
            .flat_map(|p| p.rows.iter())
            .find(|r| r.task_id == "slow-1")
            .expect("slow-1");
        let slow2 = panel
            .phases
            .iter()
            .flat_map(|p| p.rows.iter())
            .find(|r| r.task_id == "slow-2")
            .expect("slow-2");
        assert_eq!(slow1.status, WorkflowRowStatus::Succeeded);
        assert_eq!(slow2.status, WorkflowRowStatus::Cancelled);

        let (failed, cancelled) = panel.failure_cancel_counts();
        assert_eq!(failed, 0);
        assert_eq!(cancelled, 1);
        let header = panel.compact_summary_text(120);
        assert!(
            header.contains("cancel") || header.contains("cancelled"),
            "{header}"
        );
    }
}

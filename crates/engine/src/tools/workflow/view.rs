//! One tally of a workflow run's agents.
//!
//! Every surface that says "N of M finished" should read this function, so
//! there is one count, not one per reducer. It is pure: typed run events in,
//! counts and the agents that stopped out. A failed or cancelled agent is
//! never counted as finished.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::{
    IrWorkflowRunStatus, TaskCompletion, WorkflowRunStatus, WorkflowUiEvent, WorkflowUiEventKind,
    truncate_chars,
};

/// Why a workflow agent stopped without finishing, as a closed set the UI can
/// render without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WorkflowTaskStopKind {
    /// The agent's wall-time limit ran out.
    WallTime,
    /// The agent used its last allowed step.
    Steps,
    /// The agent itself failed (error result, failed delivery check, ...).
    Agent,
    /// The agent was cancelled.
    Cancelled,
    /// The agent finished, but its reply did not match the task's schema.
    Schema,
    /// The task was refused before any agent started.
    Dispatch,
}

impl WorkflowTaskStopKind {
    fn phrase(self) -> &'static str {
        match self {
            Self::WallTime => "stopped at the time limit",
            Self::Steps => "stopped at the step limit",
            Self::Agent => "failed",
            Self::Cancelled => "cancelled",
            Self::Schema => "reply did not match the schema",
            Self::Dispatch => "could not start",
        }
    }
}

const REASON_MAX_CHARS: usize = 240;

/// Reason and kind for a terminal child. A finished child has neither.
pub(super) fn task_stop(
    completion: &TaskCompletion,
) -> (Option<String>, Option<WorkflowTaskStopKind>) {
    match completion {
        TaskCompletion::Completed { .. } => (None, None),
        TaskCompletion::Failed { message } => (
            Some(bounded_reason(message)),
            Some(WorkflowTaskStopKind::Agent),
        ),
        TaskCompletion::Cancelled => (None, Some(WorkflowTaskStopKind::Cancelled)),
        TaskCompletion::BudgetExhausted { message } => {
            // The sub-agent loop names its two limits in the checkpoint
            // reason; the same substring decides its own hand-back path.
            let kind = if message.contains("wall-time") {
                WorkflowTaskStopKind::WallTime
            } else {
                WorkflowTaskStopKind::Steps
            };
            (Some(bounded_reason(message)), Some(kind))
        }
    }
}

fn bounded_reason(message: &str) -> String {
    let first_line = message.lines().next().unwrap_or_default().trim();
    truncate_chars(first_line, REASON_MAX_CHARS)
}

/// One agent that did not finish, in the order it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoppedAgent {
    pub(super) label: String,
    pub(super) kind: WorkflowTaskStopKind,
    pub(super) reason: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct WorkflowTally {
    pub(super) queued: usize,
    pub(super) running: usize,
    pub(super) finished: usize,
    pub(super) failed: usize,
    pub(super) cancelled: usize,
    pub(super) not_started: usize,
    pub(super) stopped: Vec<StoppedAgent>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowState {
    Running,
    Finished,
    Failed,
    Cancelled,
}

impl WorkflowTally {
    pub(super) fn from_events(events: &[WorkflowUiEvent]) -> Self {
        let mut tally = Self::default();
        let mut labels: HashMap<&str, String> = HashMap::new();
        let mut rows: Vec<(&str, RowState)> = Vec::new();
        let mut queued_tickets: Vec<u64> = Vec::new();
        for event in events {
            match &event.kind {
                WorkflowUiEventKind::TaskQueued { ticket, .. } => queued_tickets.push(*ticket),
                WorkflowUiEventKind::TaskStarted(started) => {
                    if let Some(ticket) = started.queue_ticket {
                        queued_tickets.retain(|queued| *queued != ticket);
                    }
                    let label = started
                        .workflow_task_label
                        .clone()
                        .or_else(|| started.label.clone())
                        .unwrap_or_else(|| started.task_id.clone());
                    labels.insert(started.task_id.as_str(), label);
                    rows.push((started.task_id.as_str(), RowState::Running));
                }
                WorkflowUiEventKind::TaskCompleted {
                    task_id,
                    status,
                    reason,
                    kind,
                    ..
                } => {
                    let state = row_state(*status);
                    set_row(&mut rows, task_id, state);
                    if matches!(state, RowState::Failed | RowState::Cancelled) {
                        tally.stopped.push(StoppedAgent {
                            label: label_for(&labels, task_id),
                            kind: kind.unwrap_or(if state == RowState::Cancelled {
                                WorkflowTaskStopKind::Cancelled
                            } else {
                                WorkflowTaskStopKind::Agent
                            }),
                            reason: reason.clone(),
                        });
                    }
                }
                // The reply is decoded after the child settles, so a schema
                // failure turns an already-finished row into a failed one.
                WorkflowUiEventKind::TaskSchemaValidationFailed { task_id, message } => {
                    if set_row(&mut rows, task_id, RowState::Failed) {
                        tally.stopped.push(StoppedAgent {
                            label: label_for(&labels, task_id),
                            kind: WorkflowTaskStopKind::Schema,
                            reason: Some(bounded_reason(message)),
                        });
                    }
                }
                WorkflowUiEventKind::TaskDispatchFailed {
                    label,
                    message,
                    queue_ticket,
                    ..
                } => {
                    if let Some(ticket) = queue_ticket {
                        queued_tickets.retain(|queued| queued != ticket);
                    }
                    tally.not_started += 1;
                    tally.stopped.push(StoppedAgent {
                        label: label.clone().unwrap_or_else(|| "task".to_string()),
                        kind: WorkflowTaskStopKind::Dispatch,
                        reason: Some(bounded_reason(message)),
                    });
                }
                // A settled run has nothing left waiting: a wait it cut short
                // never became an agent.
                WorkflowUiEventKind::RunCompleted { .. }
                | WorkflowUiEventKind::RunCancelled { .. } => queued_tickets.clear(),
                _ => {}
            }
        }
        for (_, state) in rows {
            match state {
                RowState::Running => tally.running += 1,
                RowState::Finished => tally.finished += 1,
                RowState::Failed => tally.failed += 1,
                RowState::Cancelled => tally.cancelled += 1,
            }
        }
        tally.queued = queued_tickets.len();
        tally
    }

    /// Recount agents from the driver's own per-task ledger. A run keeps only
    /// its newest events, so once older ones were trimmed the events alone
    /// under-count; the ledger and the exact dispatch-failure total do not.
    /// `stopped` stays the named sample the retained events carry.
    pub(super) fn count_from_ledger(
        &mut self,
        statuses: impl IntoIterator<Item = IrWorkflowRunStatus>,
        dispatch_failures: u64,
    ) {
        let (mut running, mut finished, mut failed, mut cancelled) = (0, 0, 0, 0);
        for status in statuses {
            match row_state(status) {
                RowState::Running => running += 1,
                RowState::Finished => finished += 1,
                RowState::Failed => failed += 1,
                RowState::Cancelled => cancelled += 1,
            }
        }
        if running + finished + failed + cancelled == 0 {
            return;
        }
        self.running = running;
        self.finished = finished;
        self.failed = failed;
        self.cancelled = cancelled;
        self.not_started = self
            .not_started
            .max(usize::try_from(dispatch_failures).unwrap_or(usize::MAX));
    }

    /// Agents that started, plus tasks refused before they could.
    pub(super) fn total(&self) -> usize {
        self.running + self.finished + self.failed + self.cancelled + self.not_started
    }

    /// "3 of 5 agents finished, 2 failed (evals: stopped at the step limit)".
    /// Zero counts are left out.
    pub(super) fn agents_sentence(&self) -> String {
        let total = self.total();
        if total == 0 && self.queued == 0 {
            return "no agents ran".to_string();
        }
        let noun = if total == 1 { "agent" } else { "agents" };
        let mut sentence = format!("{} of {total} {noun} finished", self.finished);
        for (count, word) in [
            (self.failed, "failed"),
            (self.cancelled, "cancelled"),
            (self.not_started, "could not start"),
            (self.running, "still running"),
            (self.queued, "still waiting for a slot"),
        ] {
            if count > 0 {
                sentence.push_str(&format!(", {count} {word}"));
            }
        }
        const SHOWN: usize = 4;
        if !self.stopped.is_empty() {
            let mut details: Vec<String> = self
                .stopped
                .iter()
                .take(SHOWN)
                .map(|agent| {
                    let label = truncate_chars(&agent.label, 48);
                    match (agent.kind, agent.reason.as_deref()) {
                        (
                            WorkflowTaskStopKind::Agent | WorkflowTaskStopKind::Dispatch,
                            Some(reason),
                        ) => {
                            format!(
                                "{label}: {}: {}",
                                agent.kind.phrase(),
                                truncate_chars(reason, 80)
                            )
                        }
                        (kind, _) => format!("{label}: {}", kind.phrase()),
                    }
                })
                .collect();
            // The named list can be a sample of a trimmed run; the counts
            // are the whole run, so "more" is measured against them.
            let stopped_total = self
                .stopped
                .len()
                .max(self.failed + self.cancelled + self.not_started);
            if stopped_total > details.len() {
                details.push(format!("{} more", stopped_total - details.len()));
            }
            sentence.push_str(&format!(" ({})", details.join("; ")));
        }
        sentence
    }
}

fn row_state(status: IrWorkflowRunStatus) -> RowState {
    match status {
        IrWorkflowRunStatus::Succeeded => RowState::Finished,
        IrWorkflowRunStatus::Cancelled => RowState::Cancelled,
        IrWorkflowRunStatus::Pending | IrWorkflowRunStatus::Running => RowState::Running,
        IrWorkflowRunStatus::Failed | IrWorkflowRunStatus::BudgetExceeded => RowState::Failed,
    }
}

/// Set a row's state, adding the row when its `task_started` event was
/// trimmed from the retained tail. True when the row is new or changed.
fn set_row<'a>(rows: &mut Vec<(&'a str, RowState)>, task_id: &'a str, state: RowState) -> bool {
    match rows.iter_mut().find(|(id, _)| *id == task_id) {
        Some(row) => {
            let changed = row.1 != state;
            row.1 = state;
            changed
        }
        None => {
            rows.push((task_id, state));
            true
        }
    }
}

fn label_for(labels: &HashMap<&str, String>, task_id: &str) -> String {
    labels
        .get(task_id)
        .cloned()
        .unwrap_or_else(|| task_id.to_string())
}

/// Plain word for a run's end state. Degraded is not a failure: the script
/// returned, with gaps.
pub(super) fn run_outcome_phrase(status: WorkflowRunStatus) -> &'static str {
    match status {
        WorkflowRunStatus::Running => "is still running",
        WorkflowRunStatus::Completed => "finished",
        WorkflowRunStatus::Degraded => "finished with gaps",
        WorkflowRunStatus::Failed => "failed",
        WorkflowRunStatus::Cancelled => "was stopped",
    }
}

#[cfg(test)]
mod tests {
    use super::super::WorkflowTaskStartedEvent;
    use super::*;

    fn started(task_id: &str, label: &str, phase: &str, ticket: Option<u64>) -> WorkflowUiEvent {
        WorkflowUiEvent::at(
            1,
            "session",
            WorkflowUiEventKind::TaskStarted(Box::new(WorkflowTaskStartedEvent {
                task_id: task_id.to_string(),
                label: None,
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
                workflow_run_id: Some("workflow_view".to_string()),
                workflow_phase_id: Some(phase.to_string()),
                workflow_task_label: Some(label.to_string()),
                workflow_child_index: Some(0),
                queue_ticket: ticket,
                fleet_receipt: None,
            })),
        )
    }

    fn completed(task_id: &str, completion: TaskCompletion) -> WorkflowUiEvent {
        let status = match completion {
            TaskCompletion::Completed { .. } => IrWorkflowRunStatus::Succeeded,
            TaskCompletion::Failed { .. } => IrWorkflowRunStatus::Failed,
            TaskCompletion::Cancelled => IrWorkflowRunStatus::Cancelled,
            TaskCompletion::BudgetExhausted { .. } => IrWorkflowRunStatus::BudgetExceeded,
        };
        let (reason, kind) = task_stop(&completion);
        WorkflowUiEvent::at(
            2,
            "session",
            WorkflowUiEventKind::TaskCompleted {
                task_id: task_id.to_string(),
                status,
                reason,
                kind,
                usage: None,
            },
        )
    }

    #[test]
    fn stop_kind_names_the_limit_that_ended_the_agent() {
        let steps = TaskCompletion::BudgetExhausted {
            message: "child step budget exhausted for task execution (limit: 40; used: 40; any remaining turn is reserved for hand-back)".to_string(),
        };
        let wall = TaskCompletion::BudgetExhausted {
            message: "child wall-time budget exhausted during task execution; remaining time is reserved for hand-back.".to_string(),
        };
        assert_eq!(task_stop(&steps).1, Some(WorkflowTaskStopKind::Steps));
        assert!(task_stop(&steps).0.unwrap().contains("limit: 40"));
        assert_eq!(task_stop(&wall).1, Some(WorkflowTaskStopKind::WallTime));
        assert_eq!(
            task_stop(&TaskCompletion::Completed {
                text: "done".to_string()
            }),
            (None, None)
        );
        let long = TaskCompletion::Failed {
            message: format!("{}\nstack line", "x".repeat(1_000)),
        };
        let (reason, kind) = task_stop(&long);
        assert_eq!(kind, Some(WorkflowTaskStopKind::Agent));
        let reason = reason.unwrap();
        assert!(reason.chars().count() <= REASON_MAX_CHARS);
        assert!(!reason.contains("stack line"), "one line, never the stack");
    }

    #[test]
    fn failed_and_cancelled_agents_are_never_counted_as_finished() {
        // Real run 511c2203's shape: every agent died at its limit.
        let events: Vec<WorkflowUiEvent> = (0..4)
            .flat_map(|index| {
                let id = format!("agent_{index}");
                [
                    started(&id, &format!("slot-{index}"), "survey", None),
                    completed(
                        &id,
                        TaskCompletion::BudgetExhausted {
                            message: "child step budget exhausted".to_string(),
                        },
                    ),
                ]
            })
            .collect();
        let tally = WorkflowTally::from_events(&events);
        assert_eq!((tally.finished, tally.failed, tally.total()), (0, 4, 4));
        let sentence = tally.agents_sentence();
        assert!(
            sentence.starts_with("0 of 4 agents finished, 4 failed"),
            "{sentence}"
        );
        assert!(sentence.contains("slot-0: stopped at the step limit"));
        assert_eq!(sentence.matches("stopped at the step limit").count(), 4);
        assert!(!sentence.contains("more"), "{sentence}");
    }

    #[test]
    fn tally_reads_schema_failures_dispatch_refusals_and_queued_tasks() {
        let queued = |ticket: u64, label: &str| {
            WorkflowUiEvent::at(
                0,
                "session",
                WorkflowUiEventKind::TaskQueued {
                    ticket,
                    label: Some(label.to_string()),
                    phase: None,
                },
            )
        };
        let mut events = vec![
            queued(0, "tools-approval"),
            queued(2, "evals"),
            started("a", "loop-prompt", "survey", None),
            completed(
                "a",
                TaskCompletion::Completed {
                    text: "ok".to_string(),
                },
            ),
            started("b", "tools-approval", "survey", Some(0)),
            completed(
                "b",
                TaskCompletion::Completed {
                    text: "not json".to_string(),
                },
            ),
            WorkflowUiEvent::at(
                3,
                "session",
                WorkflowUiEventKind::TaskSchemaValidationFailed {
                    task_id: "b".to_string(),
                    message: "expected object".to_string(),
                },
            ),
            WorkflowUiEvent::at(
                4,
                "session",
                WorkflowUiEventKind::TaskDispatchFailed {
                    label: Some("evals".to_string()),
                    phase: None,
                    message: "cwd is outside the workspace".to_string(),
                    queue_ticket: Some(2),
                },
            ),
            queued(1, "surfaces"),
            started("c", "models-context", "survey", None),
            completed("c", TaskCompletion::Cancelled),
        ];
        let tally = WorkflowTally::from_events(&events);
        assert_eq!(tally.finished, 1);
        assert_eq!(tally.failed, 1);
        assert_eq!(tally.cancelled, 1);
        assert_eq!(tally.not_started, 1);
        assert_eq!(
            tally.queued, 1,
            "ticket 0 started and ticket 2 was refused; ticket 1 still waits"
        );
        assert_eq!(tally.total(), 4);
        let sentence = tally.agents_sentence();
        assert_eq!(
            sentence,
            "1 of 4 agents finished, 1 failed, 1 cancelled, 1 could not start, 1 still waiting for a slot \
             (tools-approval: reply did not match the schema; evals: could not start: cwd is outside the workspace; \
             models-context: cancelled)"
        );

        // A settled run has nothing waiting; the cut-short wait is not an agent.
        events.push(WorkflowUiEvent::at(
            6,
            "session",
            WorkflowUiEventKind::RunCancelled {
                reason: "stopped".to_string(),
            },
        ));
        let settled = WorkflowTally::from_events(&events);
        assert_eq!((settled.queued, settled.total()), (0, 4));
    }

    #[test]
    fn empty_run_says_no_agents_ran() {
        assert_eq!(
            WorkflowTally::from_events(&[]).agents_sentence(),
            "no agents ran"
        );
    }
}

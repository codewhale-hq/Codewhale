use super::{
    SharedWorkflowControllers, SharedWorkflowLifecycles, SharedWorkflowRuns,
    WorkflowDispatchFailure, WorkflowRunRecord, WorkflowRunStatus, WorkflowUiEvent,
    WorkflowUiEventKind, WorkflowWorkLifecycle,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tracing::warn;

pub(super) const CODEWHALE_DIR: &str = ".codewhale";
pub(super) const WORKFLOW_RUNS_FILE: &str = "workflow-runs.jsonl";

/// Per-workspace workflow state shared across tool-registry rebuilds.
pub(super) struct WorkflowWorkspaceState {
    pub runs: SharedWorkflowRuns,
    pub controllers: SharedWorkflowControllers,
    lifecycles: SharedWorkflowLifecycles,
    journal: WorkflowRunJournal,
}

impl WorkflowWorkspaceState {
    pub fn open(workspace: &Path) -> Arc<Self> {
        Self::open_inner(workspace, true)
    }

    /// Hydrate the journal without rewriting leftover `running` rows to
    /// `failed`. Host reads do not claim runtime ownership; host cancel can
    /// still mark a controller-less run cancelled instead of assuming a crash.
    pub fn open_preserving_running(workspace: &Path) -> Arc<Self> {
        Self::open_inner(workspace, false)
    }

    fn open_inner(workspace: &Path, recover_orphans: bool) -> Arc<Self> {
        let journal = WorkflowRunJournal::open(workspace);
        let runs = Arc::new(Mutex::new(journal.hydrate_runs(recover_orphans)));
        Arc::new(Self {
            runs,
            controllers: Arc::new(Mutex::new(HashMap::new())),
            lifecycles: Arc::new(Mutex::new(HashMap::new())),
            journal,
        })
    }

    pub fn attach_lifecycle(&self, run_id: &str, lifecycle: WorkflowWorkLifecycle) {
        self.lifecycles
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .entry(run_id.to_string())
            .or_insert(lifecycle);
    }

    pub fn reconcile_snapshot(&self, record: &WorkflowRunRecord) {
        let lifecycle = self
            .lifecycles
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(&record.run_id)
            .cloned();
        if let Some(lifecycle) = lifecycle
            && let Err(err) = lifecycle.reconcile_record(record)
        {
            warn!(
                run_id = record.run_id,
                "workflow Work reconciliation failed: {err}"
            );
        }
    }

    pub fn reconcile_cancel(&self, run_id: &str, outcome: super::CancelOutcome) {
        let lifecycle = self
            .lifecycles
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(run_id)
            .cloned();
        if let Some(lifecycle) = lifecycle
            && let Err(err) = lifecycle.reconcile_cancel(outcome)
        {
            warn!(run_id, "workflow cancellation reconciliation failed: {err}");
        }
    }

    pub fn mark_owner_missing(&self, run_id: &str) {
        let lifecycle = self
            .lifecycles
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(run_id)
            .cloned();
        if let Some(lifecycle) = lifecycle {
            lifecycle.reconcile_missing();
        }
    }

    pub fn try_record_snapshot(&self, record: &WorkflowRunRecord) -> Result<(), String> {
        self.journal
            .append_snapshot(record)
            .map_err(|err| err.to_string())
    }

    pub fn record_snapshot(&self, record: &WorkflowRunRecord) {
        if let Err(err) = self.try_record_snapshot(record) {
            warn!("workflow journal snapshot failed: {err}");
        }
    }

    pub fn record_progress(&self, run_id: &str, message: &str) {
        if let Err(err) = self.journal.append_progress(run_id, message) {
            warn!("workflow journal progress failed: {err}");
        }
    }

    pub fn record_event(&self, run_id: &str, event: &WorkflowUiEvent) {
        if let Err(err) = self.journal.append_event(run_id, event) {
            warn!("workflow journal event failed: {err}");
        }
    }

    /// Durable journal location for full-fidelity run detail (#2974).
    pub fn journal_path(&self) -> &Path {
        &self.journal.ledger_path
    }
}

fn workspace_store() -> &'static Mutex<HashMap<PathBuf, Arc<WorkflowWorkspaceState>>> {
    static STORE: OnceLock<Mutex<HashMap<PathBuf, Arc<WorkflowWorkspaceState>>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn shared_workflow_state(workspace: &Path) -> Arc<WorkflowWorkspaceState> {
    let key = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut store = workspace_store()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    store
        .entry(key)
        .or_insert_with(|| WorkflowWorkspaceState::open(workspace))
        .clone()
}

/// Read-only lookup that never creates workspace state, a journal
/// directory, or a ledger file. Used by the human-only `/structcopy`
/// command (#2033), which must stay side-effect free.
pub(super) fn peek_shared_workflow_state(workspace: &Path) -> Option<Arc<WorkflowWorkspaceState>> {
    let key = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    workspace_store()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .get(&key)
        .cloned()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WorkflowJournalRecord {
    // Boxed: a full run record dwarfs the progress variant
    // (clippy::large_enum_variant).
    Snapshot {
        run: Box<WorkflowRunRecord>,
    },
    Progress {
        run_id: String,
        message: String,
    },
    Event {
        run_id: String,
        event: Box<WorkflowUiEvent>,
    },
}

#[derive(Debug)]
struct WorkflowRunJournal {
    /// Workspace the ledger must stay inside; links below it are refused.
    root: PathBuf,
    ledger_path: PathBuf,
}

impl WorkflowRunJournal {
    /// Opening has no side effects: `.codewhale/` and the ledger appear with
    /// the first appended record, and only through the confined helpers, so a
    /// linked `.codewhale` never receives an empty `state/` or ledger.
    fn open(workspace: &Path) -> Self {
        Self {
            root: workspace.to_path_buf(),
            ledger_path: workspace.join(CODEWHALE_DIR).join(WORKFLOW_RUNS_FILE),
        }
    }

    fn hydrate_runs(&self, recover_orphans: bool) -> HashMap<String, WorkflowRunRecord> {
        let file = match codewhale_util::fs_confined::open_read(&self.root, &self.ledger_path) {
            Ok(file) => file,
            Err(err) => {
                if err.kind() != std::io::ErrorKind::NotFound {
                    warn!(
                        "workflow journal unreadable ({}): {err}",
                        self.ledger_path.display()
                    );
                }
                return HashMap::new();
            }
        };
        let mut runs = HashMap::new();
        for line in std::io::BufReader::new(file).lines() {
            let Ok(line) = line else { continue };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let record = match serde_json::from_str::<WorkflowJournalRecord>(trimmed) {
                Ok(record) => record,
                Err(err) => {
                    warn!("workflow journal skipped malformed line: {err}");
                    continue;
                }
            };
            match record {
                WorkflowJournalRecord::Snapshot { run } => {
                    let mut run = *run;
                    run.normalize_bounded_ledgers();
                    runs.insert(run.run_id.clone(), run);
                }
                WorkflowJournalRecord::Progress { run_id, message } => {
                    if let Some(run) = runs.get_mut(&run_id) {
                        run.push_progress(message);
                    }
                }
                WorkflowJournalRecord::Event { run_id, event } => {
                    if let Some(run) = runs.get_mut(&run_id) {
                        let event = *event;
                        if let WorkflowUiEventKind::TaskDispatchFailed {
                            label,
                            phase,
                            message,
                            ..
                        } = &event.kind
                        {
                            run.push_dispatch_failure(WorkflowDispatchFailure {
                                at_ms: event.at_ms,
                                label: label.clone(),
                                phase: phase.clone(),
                                message: message.clone(),
                            });
                        }
                        run.push_event(event);
                    }
                }
            }
        }
        // Journals written before #2974 have no counters; rebuild them
        // from the retained tail so summaries stay truthful.
        for run in runs.values_mut() {
            run.normalize_bounded_ledgers();
            run.events_total = run.events_total.max(run.events.len() as u64);
        }
        // A run journaled as Running belongs to a process that is gone;
        // without this it would show as live forever after a restart.
        // Host reads and cancellation skip this owner-only rewrite.
        if recover_orphans {
            let mut recovered = Vec::new();
            for run in runs.values_mut() {
                if run.status == WorkflowRunStatus::Running {
                    run.status = WorkflowRunStatus::Failed;
                    run.lifecycle_seq = run.lifecycle_seq.saturating_add(1);
                    run.completed_at_ms.get_or_insert_with(super::now_ms);
                    run.error = Some(
                        "process exited before the run completed (recovered on startup)"
                            .to_string(),
                    );
                    recovered.push(run.clone());
                }
            }
            // The recovery decision is owner truth, not a presentation-only
            // repair. Append it so another restart replays the same terminal
            // sequence instead of rediscovering and incrementing it again.
            for run in recovered {
                if let Err(err) = self.append_snapshot(&run) {
                    warn!(
                        run_id = run.run_id,
                        "workflow recovery snapshot append failed: {err}"
                    );
                }
            }
        }
        runs
    }

    fn append_record(&self, record: &WorkflowJournalRecord) -> std::io::Result<()> {
        let mut line =
            serde_json::to_string(record).map_err(|err| std::io::Error::other(err.to_string()))?;
        line.push('\n');
        let mut file = codewhale_util::fs_confined::open_append(&self.root, &self.ledger_path)?;
        file.write_all(line.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    fn append_snapshot(&self, record: &WorkflowRunRecord) -> std::io::Result<()> {
        self.append_record(&WorkflowJournalRecord::Snapshot {
            run: Box::new(record.clone()),
        })
    }

    fn append_progress(&self, run_id: &str, message: &str) -> std::io::Result<()> {
        self.append_record(&WorkflowJournalRecord::Progress {
            run_id: run_id.to_string(),
            message: message.to_string(),
        })
    }

    fn append_event(&self, run_id: &str, event: &WorkflowUiEvent) -> std::io::Result<()> {
        self.append_record(&WorkflowJournalRecord::Event {
            run_id: run_id.to_string(),
            event: Box::new(event.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED, WorkflowUiEventKind};
    use super::*;

    /// #5582: a degraded run must never project as an ordinary success
    /// to owner-level consumers.
    #[test]
    fn owner_snapshot_keeps_degraded_distinct_from_completed() {
        use crate::work_graph::OwnerState;
        assert_eq!(
            crate::tools::workflow::owner_state_for_run_status(WorkflowRunStatus::Degraded),
            OwnerState::Degraded
        );
        assert_eq!(
            crate::tools::workflow::owner_state_for_run_status(WorkflowRunStatus::Completed),
            OwnerState::Completed
        );
    }

    fn sample_record(run_id: &str, status: WorkflowRunStatus) -> WorkflowRunRecord {
        WorkflowRunRecord {
            run_id: run_id.to_string(),
            owner_session_id: Some("session-journal".to_string()),
            status,
            lifecycle_seq: 1,
            started_at_ms: 1,
            completed_at_ms: None,
            source_path: None,
            workflow_id: Some("fixture".to_string()),
            workflow_goal: Some("journal test".to_string()),
            token_budget: None,
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
            gate_status: Vec::new(),
            usage: None,
            events_total: 0,
            events_dropped: 0,
        }
    }

    /// Opening the journal creates nothing; the first record creates the
    /// ledger under a private `.codewhale/`.
    #[test]
    fn opening_the_journal_creates_nothing_until_a_record_is_appended() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        assert!(!tmp.path().join(CODEWHALE_DIR).exists());
        state.record_snapshot(&sample_record("workflow_lazy", WorkflowRunStatus::Running));
        assert!(state.journal_path().is_file());
    }

    /// A linked `.codewhale` gets no empty `state/` or ledger, and appends and
    /// reads are refused instead of writing through the link.
    #[cfg(unix)]
    #[test]
    fn journal_never_writes_through_a_linked_codewhale_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let workspace = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&workspace).expect("mkdir workspace");
        std::fs::create_dir_all(&outside).expect("mkdir outside");
        std::os::unix::fs::symlink(&outside, workspace.join(CODEWHALE_DIR)).expect("link");

        let journal = WorkflowRunJournal::open(&workspace);
        assert!(journal.hydrate_runs(true).is_empty());
        let err = journal
            .append_snapshot(&sample_record("workflow_link", WorkflowRunStatus::Running))
            .expect_err("append through a linked .codewhale must be refused");
        assert!(!err.to_string().is_empty());

        // The state-level entry points degrade to a warning, not a write.
        let state = WorkflowWorkspaceState::open(&workspace);
        state.record_progress("workflow_link", "phase: scan");
        assert_eq!(
            std::fs::read_dir(&outside).expect("read outside").count(),
            0
        );
    }

    #[test]
    fn workflow_journal_hydrates_snapshots_and_progress() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        let running = sample_record("workflow_abc", WorkflowRunStatus::Running);
        state.record_snapshot(&running);
        state.record_progress("workflow_abc", "phase: scan");
        state.record_event(
            "workflow_abc",
            &WorkflowUiEvent::at(
                5,
                "session-journal",
                WorkflowUiEventKind::PhaseStarted {
                    title: "scan".to_string(),
                },
            ),
        );

        let completed = WorkflowRunRecord {
            status: WorkflowRunStatus::Completed,
            completed_at_ms: Some(99),
            progress: vec!["phase: scan".to_string()],
            events: vec![WorkflowUiEvent::at(
                5,
                "session-journal",
                WorkflowUiEventKind::PhaseStarted {
                    title: "scan".to_string(),
                },
            )],
            ..sample_record("workflow_abc", WorkflowRunStatus::Completed)
        };
        state.record_snapshot(&completed);
        state.record_event(
            "workflow_abc",
            &WorkflowUiEvent::at(
                6,
                "session-journal",
                WorkflowUiEventKind::HandoffPromoted {
                    artifact_id: "workflow_abc:scout-1:scout-gate:findings".to_string(),
                    gate_id: "scout-gate".to_string(),
                    kind: "findings".to_string(),
                    from_role: "scout".to_string(),
                    to_role: "implementer".to_string(),
                    producer_task_id: "scout-1".to_string(),
                },
            ),
        );
        state.record_event(
            "workflow_abc",
            &WorkflowUiEvent::at(
                7,
                "session-journal",
                WorkflowUiEventKind::HandoffConsumed {
                    artifact_id: "workflow_abc:scout-1:scout-gate:findings".to_string(),
                    kind: "findings".to_string(),
                    from_role: "scout".to_string(),
                    to_role: "implementer".to_string(),
                    consumer_task_id: "implementer-1".to_string(),
                },
            ),
        );

        let reloaded = WorkflowWorkspaceState::open(tmp.path());
        let runs = reloaded
            .runs
            .lock()
            .expect("runs lock")
            .get("workflow_abc")
            .cloned()
            .expect("hydrated run");
        assert_eq!(runs.status, WorkflowRunStatus::Completed);
        assert_eq!(runs.progress, vec!["phase: scan"]);
        assert_eq!(runs.events.len(), 3);
        assert_eq!(runs.events[0].event_type(), "phase_started");
        let promoted = serde_json::to_value(&runs.events[1]).expect("promoted receipt");
        assert_eq!(promoted["type"], "handoff_promoted");
        assert_eq!(
            promoted["artifact_id"],
            "workflow_abc:scout-1:scout-gate:findings"
        );
        assert_eq!(promoted["gate_id"], "scout-gate");
        assert_eq!(promoted["producer_task_id"], "scout-1");
        assert!(promoted.get("payload").is_none(), "{promoted}");
        let consumed = serde_json::to_value(&runs.events[2]).expect("consumed receipt");
        assert_eq!(consumed["type"], "handoff_consumed");
        assert_eq!(consumed["artifact_id"], promoted["artifact_id"]);
        assert_eq!(consumed["consumer_task_id"], "implementer-1");
        assert!(consumed.get("payload").is_none(), "{consumed}");
        assert_eq!(runs.completed_at_ms, Some(99));

        // The event-line replay above must also survive compaction into a
        // final Snapshot record containing both handoff variants.
        reloaded.record_snapshot(&runs);
        let reopened = WorkflowWorkspaceState::open(tmp.path());
        let compacted = reopened
            .runs
            .lock()
            .expect("runs lock")
            .get("workflow_abc")
            .cloned()
            .expect("snapshot with handoff receipts");
        assert_eq!(
            compacted
                .events
                .iter()
                .map(WorkflowUiEvent::event_type)
                .collect::<Vec<_>>(),
            vec!["phase_started", "handoff_promoted", "handoff_consumed"]
        );
    }

    #[test]
    fn workflow_journal_rebuilds_a_bounded_exact_rejection_ledger() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        state.record_snapshot(&sample_record(
            "workflow_rejections",
            WorkflowRunStatus::Running,
        ));
        let total = WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED + 5;
        for index in 0..total {
            let message = format!("invalid task options {index}");
            state.record_progress(
                "workflow_rejections",
                &format!("dispatch failed for rejected-{index}: {message}"),
            );
            state.record_event(
                "workflow_rejections",
                &WorkflowUiEvent::at(
                    index as u64,
                    "session-journal",
                    WorkflowUiEventKind::TaskDispatchFailed {
                        label: Some(format!("rejected-{index}")),
                        phase: Some("fan-out".to_string()),
                        message,
                        queue_ticket: None,
                    },
                ),
            );
        }
        drop(state);

        let reloaded = WorkflowWorkspaceState::open(tmp.path());
        let run = reloaded
            .runs
            .lock()
            .expect("runs lock")
            .get("workflow_rejections")
            .cloned()
            .expect("hydrated rejection run");
        assert_eq!(run.progress_count, total as u64);
        assert_eq!(run.progress.len(), total);
        assert_eq!(run.dispatch_failure_count, total as u64);
        assert_eq!(
            run.dispatch_failures.len(),
            WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED
        );
        assert_eq!(
            run.dispatch_failures
                .first()
                .and_then(|failure| failure.label.as_deref()),
            Some("rejected-5")
        );
        drop(reloaded);

        // Restart recovery appends a compact snapshot. Replaying the
        // journal again must not double-count its earlier event lines.
        let reopened = WorkflowWorkspaceState::open(tmp.path());
        let run = reopened
            .runs
            .lock()
            .expect("runs lock")
            .get("workflow_rejections")
            .cloned()
            .expect("rehydrated rejection run");
        assert_eq!(run.dispatch_failure_count, total as u64);
        assert_eq!(
            run.dispatch_failures.len(),
            WORKFLOW_RUN_DISPATCH_FAILURES_MAX_RETAINED
        );
    }

    #[test]
    fn workflow_journal_marks_orphaned_running_runs_failed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        state.record_snapshot(&sample_record(
            "workflow_orphan",
            WorkflowRunStatus::Running,
        ));

        let reloaded = WorkflowWorkspaceState::open(tmp.path());
        let run = reloaded
            .runs
            .lock()
            .expect("runs lock")
            .get("workflow_orphan")
            .cloned()
            .expect("hydrated run");
        assert_eq!(run.status, WorkflowRunStatus::Failed);
        assert_eq!(
            run.lifecycle_seq, 2,
            "restart recovery is a durable owner lifecycle transition"
        );
        assert!(
            run.completed_at_ms.is_some(),
            "restart recovery must terminalize the durable owner record"
        );
        assert!(
            run.error
                .as_deref()
                .is_some_and(|error| error.contains("process exited")),
            "expected orphan recovery error, got {:?}",
            run.error
        );

        let reopened = WorkflowWorkspaceState::open(tmp.path());
        let replayed = reopened
            .runs
            .lock()
            .expect("runs lock")
            .get("workflow_orphan")
            .cloned()
            .expect("durably recovered run");
        assert_eq!(replayed.status, WorkflowRunStatus::Failed);
        assert_eq!(
            replayed.lifecycle_seq, 2,
            "reopening must replay the recovery snapshot without another transition"
        );
    }

    #[test]
    fn host_cancel_hydrates_a_journal_without_live_process_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        let record = sample_record("workflow_prior", WorkflowRunStatus::Running);
        state.record_snapshot(&record);
        drop(state);

        assert!(
            peek_shared_workflow_state(tmp.path()).is_none(),
            "writing the journal must not insert process-wide live state"
        );

        let line = super::super::host_cancel_workflow(
            tmp.path(),
            "workflow_prior",
            Some("session-journal"),
        )
        .expect("a journaled run must be visible to host cancel after restart");
        assert_eq!(line.run_id, "workflow_prior");
        assert_eq!(line.status, "cancelled");
        assert!(
            line.error
                .as_deref()
                .is_some_and(|error| error.contains("no live process")),
            "controller-less cancel must leave an honest receipt, got {:?}",
            line.error
        );

        let reopened = WorkflowWorkspaceState::open(tmp.path());
        let replayed = reopened
            .runs
            .lock()
            .expect("runs lock")
            .get("workflow_prior")
            .cloned()
            .expect("cancelled journal line");
        assert_eq!(replayed.status, WorkflowRunStatus::Cancelled);
    }

    #[test]
    fn host_stage_is_derived_from_typed_owner_events() {
        let mut record = sample_record("workflow_stage", WorkflowRunStatus::Running);
        record.push_event(WorkflowUiEvent::at(
            1,
            "session-journal",
            WorkflowUiEventKind::RunStarted {
                workflow_id: Some("fixture".to_string()),
                workflow_goal: Some("review release".to_string()),
                source_path: None,
                token_budget: None,
                max_concurrent: None,
            },
        ));
        assert_eq!(super::super::host_workflow_stage(&record), "queued");

        record.push_event(WorkflowUiEvent::at(
            2,
            "session-journal",
            WorkflowUiEventKind::PhaseStarted {
                title: "review".to_string(),
            },
        ));
        assert_eq!(super::super::host_workflow_stage(&record), "running");

        record.push_event(WorkflowUiEvent::at(
            3,
            "session-journal",
            WorkflowUiEventKind::TaskStarted(Box::new(super::super::WorkflowTaskStartedEvent {
                task_id: "reviewer-1".to_string(),
                label: Some("reviewer".to_string()),
                role: None,
                profile: None,
                model: None,
                strength: None,
                thinking: None,
                requested_reasoning: None,
                effective_reasoning: None,
                resolved_role: Some("reviewer".to_string()),
                resolved_profile: None,
                resolved_provider: "local".to_string(),
                resolved_model: "stub".to_string(),
                route_source: "session".to_string(),
                child_route: None,
                worktree: false,
                workspace: None,
                git_branch: None,
                parent_task_id: None,
                depth: 0,
                workflow_run_id: Some("workflow_stage".to_string()),
                workflow_phase_id: Some("review".to_string()),
                workflow_task_label: Some("reviewer".to_string()),
                workflow_child_index: Some(0),
                queue_ticket: None,
                fleet_receipt: None,
            })),
        ));
        assert_eq!(super::super::host_workflow_stage(&record), "waiting");

        record.push_event(WorkflowUiEvent::at(
            4,
            "session-journal",
            WorkflowUiEventKind::TaskCompleted {
                task_id: "reviewer-1".to_string(),
                status: super::super::IrWorkflowRunStatus::Succeeded,
                reason: None,
                kind: None,
                usage: None,
            },
        ));
        assert_eq!(super::super::host_workflow_stage(&record), "running");

        record.status = WorkflowRunStatus::Completed;
        assert_eq!(super::super::host_workflow_stage(&record), "completed");
        record.status = WorkflowRunStatus::Failed;
        assert_eq!(super::super::host_workflow_stage(&record), "failed");
        record.status = WorkflowRunStatus::Cancelled;
        assert_eq!(super::super::host_workflow_stage(&record), "cancelled");
    }

    #[test]
    fn host_run_details_derive_phases_and_child_states_from_the_journal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = WorkflowWorkspaceState::open(tmp.path());
        let mut record = sample_record("workflow_detail", WorkflowRunStatus::Running);
        record.workflow_goal = Some("audit provider errors".to_string());
        for message in ["phase: scan", "child slow-1 done", "child slow-2 failed"] {
            record.push_progress(message.to_string());
        }
        state.record_snapshot(&record);
        drop(state);

        let phase: WorkflowUiEvent = serde_json::from_value(serde_json::json!({
            "at_ms": 1,
            "owner_session_id": "session-journal",
            "type": "phase_started",
            "title": "scan"
        }))
        .expect("phase_started event");
        let started: WorkflowUiEvent = WorkflowUiEvent::at(
            2,
            "session-journal",
            WorkflowUiEventKind::TaskStarted(Box::new(super::super::WorkflowTaskStartedEvent {
                task_id: "child-1".to_string(),
                label: Some("slow-1".to_string()),
                role: None,
                profile: None,
                model: None,
                strength: None,
                thinking: None,
                requested_reasoning: None,
                effective_reasoning: None,
                resolved_role: Some("explore".to_string()),
                resolved_profile: None,
                resolved_provider: "deepseek".to_string(),
                resolved_model: "deepseek-v4-flash".to_string(),
                route_source: "session".to_string(),
                child_route: None,
                worktree: false,
                workspace: None,
                git_branch: None,
                parent_task_id: None,
                depth: 0,
                workflow_run_id: Some("workflow_detail".to_string()),
                workflow_phase_id: Some("scan".to_string()),
                workflow_task_label: None,
                workflow_child_index: Some(0),
                queue_ticket: None,
                fleet_receipt: None,
            })),
        );
        let completed: WorkflowUiEvent = serde_json::from_value(serde_json::json!({
            "at_ms": 3,
            "owner_session_id": "session-journal",
            "type": "task_completed",
            "task_id": "child-1",
            "status": "failed"
        }))
        .expect("task_completed event");
        let replay = WorkflowWorkspaceState::open_preserving_running(tmp.path());
        replay.record_event("workflow_detail", &phase);
        replay.record_event("workflow_detail", &started);
        replay.record_event("workflow_detail", &completed);
        drop(replay);

        let details = super::super::host_workflow_run_details(tmp.path(), Some("session-journal"));
        assert_eq!(details.len(), 1, "one journaled run");
        let detail = &details[0];
        assert_eq!(detail.line.run_id, "workflow_detail");
        assert_eq!(detail.line.status, "running");
        assert!(peek_shared_workflow_state(tmp.path()).is_none());
        assert_eq!(detail.line.label, "audit provider errors");
        assert_eq!(detail.phases, vec!["scan".to_string()]);
        assert_eq!(detail.children.len(), 1);
        let child = &detail.children[0];
        assert_eq!(child.task_id, "child-1");
        assert_eq!(child.label.as_deref(), Some("slow-1"));
        assert_eq!(child.role.as_deref(), Some("explore"));
        assert_eq!(child.model.as_deref(), Some("deepseek-v4-flash"));
        assert_eq!(child.phase.as_deref(), Some("scan"));
        assert_eq!(
            child.state, "failed",
            "terminal event must win over running"
        );
        assert_eq!(detail.progress_tail.len(), 3);
        assert!(!detail.has_result);

        // Session ownership fences the projection: a foreign session
        // sees nothing, exactly like every other host control.
        assert!(
            super::super::host_workflow_run_details(tmp.path(), Some("session-other")).is_empty()
        );
    }
}

//! Owner-facing lifecycle adapter vocabulary.
//!
//! Owners keep their existing registries, ledgers, and process handles. They
//! translate those records into this small vocabulary; `WorkRuntime`
//! persists the resulting observation through the graph reducer. No adapter
//! infers liveness from UI state.

use super::{AcceptanceRequirement, EvidenceRef, OperationObservation, OwnerState, Ts};
use crate::fleet::ledger::{FleetTaskLedgerStatus, FleetTaskState};
use crate::task_manager::TaskStatus;
use chrono::{DateTime, Utc};
use codewhale_lane::{LaneRecord, LaneStatus};

/// Spawn intent registered before an owner starts work.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationIntent {
    pub external: String,
    pub title: String,
    pub durable: bool,
    pub source: String,
    pub call_id: String,
    pub acceptance: Vec<AcceptanceRequirement>,
}

impl OperationIntent {
    #[must_use]
    pub fn new(
        external: impl Into<String>,
        title: impl Into<String>,
        durable: bool,
        source: impl Into<String>,
        call_id: impl Into<String>,
    ) -> Self {
        Self {
            external: external.into(),
            title: title.into(),
            durable,
            source: source.into(),
            call_id: call_id.into(),
            acceptance: Vec::new(),
        }
    }
}

/// One authoritative owner snapshot. `seq` must be monotonic within the
/// external binding; replaying the same `(binding, seq)` is a reducer no-op.
#[derive(Debug, Clone, PartialEq)]
pub struct OperationOwnerSnapshot {
    pub external: String,
    pub state: OwnerState,
    pub seq: u64,
    pub observed_at: Ts,
    pub output: Option<EvidenceRef>,
}

impl OperationOwnerSnapshot {
    #[must_use]
    pub fn new(external: impl Into<String>, state: OwnerState, seq: u64, observed_at: Ts) -> Self {
        Self {
            external: external.into(),
            state,
            seq,
            observed_at,
            output: None,
        }
    }

    #[must_use]
    pub fn with_output(mut self, output: EvidenceRef) -> Self {
        self.output = Some(output);
        self
    }

    #[must_use]
    pub fn into_observation(self) -> OperationObservation {
        OperationObservation::OwnerReported {
            state: self.state,
            seq: self.seq,
            at: self.observed_at,
            output: self.output,
        }
    }
}

/// Translate a durable task record or summary without duplicating lifecycle
/// semantics across the model tool, periodic TUI refresh, and engine restore.
#[must_use]
pub fn task_owner_snapshot(
    id: &str,
    status: TaskStatus,
    lifecycle_seq: u64,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
) -> OperationOwnerSnapshot {
    let state = match status {
        TaskStatus::Queued => OwnerState::Initializing,
        TaskStatus::Running => OwnerState::Running,
        TaskStatus::Completed => OwnerState::Completed,
        TaskStatus::Failed => OwnerState::Failed,
        TaskStatus::Canceled => OwnerState::Cancelled,
    };
    let observed_at = ended_at
        .or(started_at)
        .unwrap_or(created_at)
        .timestamp_millis();
    OperationOwnerSnapshot::new(format!("task:{id}"), state, lifecycle_seq, observed_at)
}

/// Translate the replayed Fleet task ledger. Live worker enrichment never
/// overrides this durable task projection.
#[must_use]
pub fn fleet_task_owner_snapshot(task: &FleetTaskState, observed_at: Ts) -> OperationOwnerSnapshot {
    let state = match task.status {
        FleetTaskLedgerStatus::Enqueued => OwnerState::Initializing,
        FleetTaskLedgerStatus::Leased => OwnerState::Running,
        FleetTaskLedgerStatus::Completed => OwnerState::Completed,
        FleetTaskLedgerStatus::Failed => OwnerState::Failed,
        FleetTaskLedgerStatus::Cancelled => OwnerState::Cancelled,
    };
    OperationOwnerSnapshot::new(
        format!("fleet:{}/{}", task.entry.run_id.0, task.entry.task_id),
        state,
        task.lifecycle_seq.max(1),
        observed_at,
    )
}

/// Translate a durable Lane registry record without inspecting backend
/// processes. Backend reconciliation must first update the registry; the
/// registry remains the owner presented to the graph.
#[must_use]
pub fn lane_owner_snapshot(record: &LaneRecord, observed_at: Ts) -> OperationOwnerSnapshot {
    let state = match record.status {
        LaneStatus::Pending => OwnerState::Initializing,
        LaneStatus::Running => OwnerState::Running,
        LaneStatus::Stopped => OwnerState::Cancelled,
        LaneStatus::Failed => OwnerState::Failed,
        LaneStatus::Completed => OwnerState::Completed,
    };
    OperationOwnerSnapshot::new(
        format!("lane:{}", record.id),
        state,
        record.lifecycle_seq.max(1),
        observed_at,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_owner_snapshot_exhaustively_maps_status_and_prefers_terminal_time() {
        let created_at = DateTime::from_timestamp_millis(10).expect("created timestamp");
        let started_at = DateTime::from_timestamp_millis(20).expect("started timestamp");
        let ended_at = DateTime::from_timestamp_millis(30).expect("ended timestamp");
        let cases = [
            (TaskStatus::Queued, OwnerState::Initializing),
            (TaskStatus::Running, OwnerState::Running),
            (TaskStatus::Completed, OwnerState::Completed),
            (TaskStatus::Failed, OwnerState::Failed),
            (TaskStatus::Canceled, OwnerState::Cancelled),
        ];

        for (status, expected) in cases {
            let snapshot = task_owner_snapshot(
                "task-id",
                status,
                7,
                created_at,
                Some(started_at),
                Some(ended_at),
            );
            assert_eq!(snapshot.external, "task:task-id");
            assert_eq!(snapshot.state, expected);
            assert_eq!(snapshot.seq, 7);
            assert_eq!(snapshot.observed_at, 30);
        }
    }

    #[test]
    fn task_owner_snapshot_falls_back_from_started_to_created_time() {
        let created_at = DateTime::from_timestamp_millis(10).expect("created timestamp");
        let started_at = DateTime::from_timestamp_millis(20).expect("started timestamp");

        let started = task_owner_snapshot(
            "started",
            TaskStatus::Running,
            2,
            created_at,
            Some(started_at),
            None,
        );
        let queued = task_owner_snapshot("queued", TaskStatus::Queued, 1, created_at, None, None);

        assert_eq!(started.observed_at, 20);
        assert_eq!(queued.observed_at, 10);
    }
}

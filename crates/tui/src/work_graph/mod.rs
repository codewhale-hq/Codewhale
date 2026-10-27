//! Work Graph — the single authoritative work ledger for a session.
//!
//! One graph carries objectives, plan steps, operations, evidence, blockers,
//! and approvals. **Invariant: one graph writes every projection; projections
//! never write each other.** Plan and todo views, work-surface rows, and the
//! inspector all derive from [`WorkGraphSnapshot`] through pure functions.
//!
//! Why a graph instead of parallel trackers: flat status lists let an agent
//! mark work "done" by assertion, lose dependency structure, and cannot say
//! what evidence backed a completion. Here completion and verification are
//! distinct states, `Verified` is unreachable without a satisfying evidence
//! path (V4, fail-closed), dependencies are first-class edges (acyclic, V1),
//! and liveness truth stays with the owning subsystems — the graph records
//! observations, it never invents them.
//!
//! Session persistence, legacy import, UI projections, and liveness adapters
//! are wired: `session_manager` stores a snapshot, `tools/shell` and
//! `tools/tasks` observe operations, and the engine records owner liveness.
#![allow(dead_code)]
#![allow(unused_imports)]

mod compat;
mod digest;
mod events;
mod ids;
mod liveness;
mod migration;
mod model;
mod reducer;
mod runtime;
mod validate;

#[cfg(test)]
mod tests;

pub use compat::{PlanProjection, TodoProjection, project_plan, project_todos};
pub use digest::{format_operation_digest, format_operation_digest_parts};
pub use events::{
    ApprovalRef, CancelOutcome, ChangeCtx, ChangeReceipt, ObservationSummary, OperationObservation,
    OwnerState, ProposedNodeUpdate, WorkGraphChange, WorkGraphProposal, WorkNodePatch,
};
pub use ids::{BindingId, ChangeId, ProposalId, WorkEdgeId, WorkNodeId};
pub use liveness::{
    OperationIntent, OperationOwnerSnapshot, fleet_task_owner_snapshot, lane_owner_snapshot,
    task_owner_snapshot,
};
pub use migration::import_legacy;
pub use model::{
    ACTIVITY_CAP, AcceptanceRequirement, BoundedSet, BoundedVec, CompatPlanMetadata,
    CompatProjectionState, CompatTodoBinding, EdgeKind, EvidenceKind, EvidenceKindTag, EvidenceRef,
    EvidenceRefError, HISTORY_CAP, IdempotencyKey, NodeKind, NodeState, OperationBinding,
    Provenance, SCHEMA_VERSION, SEEN_KEYS_CAP, Ts, WorkActivityEvent, WorkEdge, WorkGraphSnapshot,
    WorkNode, external_identity_is_well_formed,
};
pub(crate) use model::{ENDED_OPERATION_CAP, constrained_effective_reasoning_for_route};
pub use reducer::apply;
pub(crate) use reducer::evictable_operations;
pub(crate) use runtime::{ACTIVE_OPERATION_SUMMARY_END, ACTIVE_OPERATION_SUMMARY_START};
pub use runtime::{SharedWorkRuntime, WorkRuntime, WorkRuntimeSnapshot, new_shared_work_runtime};
pub use validate::{ValidationCode, ValidationReport, Violation, validate};

/// Convenience wrapper owning the current snapshot. [`WorkGraph::apply`]
/// commits the reducer's result on success and leaves the snapshot untouched
/// on rejection — the fail-closed contract, packaged.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkGraph {
    snapshot: WorkGraphSnapshot,
}

impl WorkGraph {
    #[must_use]
    pub fn new() -> Self {
        Self {
            snapshot: WorkGraphSnapshot::new(),
        }
    }

    #[must_use]
    pub fn from_snapshot(snapshot: WorkGraphSnapshot) -> Self {
        Self { snapshot }
    }

    #[must_use]
    pub fn snapshot(&self) -> &WorkGraphSnapshot {
        &self.snapshot
    }

    #[must_use]
    pub fn into_snapshot(self) -> WorkGraphSnapshot {
        self.snapshot
    }

    /// Apply one change through the reducer. On `Ok` the new snapshot is
    /// committed; on `Err` the held snapshot is unchanged.
    pub fn apply(
        &mut self,
        change: WorkGraphChange,
        ctx: ChangeCtx,
    ) -> Result<ChangeReceipt, ValidationReport> {
        let (next, receipt) = reducer::apply(&self.snapshot, change, ctx)?;
        self.snapshot = next;
        Ok(receipt)
    }
}

impl Default for WorkGraph {
    fn default() -> Self {
        Self::new()
    }
}

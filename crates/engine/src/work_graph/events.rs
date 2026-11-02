//! Changes, observations, and receipts — the reducer's entire input/output
//! vocabulary.
//!
//! The reducer never reads clocks or RNG: [`ChangeCtx`] carries the timestamp,
//! the session identity used for deterministic ID derivation, and an optional
//! idempotency key. Same snapshot + same change + same ctx ⇒ same result,
//! always.
//!
//! Spec-silent shapes chosen minimally here (documented on each type):
//! [`WorkNodePatch`], [`WorkGraphProposal`], [`ApprovalRef`], and the
//! placeholder observation types that later slices' liveness adapters will
//! feed ([`OperationObservation`], [`OwnerState`], [`CancelOutcome`]).

use serde::{Deserialize, Serialize};

use super::ids::{ChangeId, ProposalId, WorkEdgeId, WorkNodeId};
use super::model::{
    AcceptanceRequirement, CompatProjectionState, EvidenceRef, IdempotencyKey, NodeState,
    OperationBinding, Provenance, Ts, WorkActivityEvent, WorkEdge, WorkNode,
};

/// A single mutation of the work graph. The reducer is the only write path;
/// UI, tools, and runtime adapters all speak this vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
// Variants intentionally carry full payloads (a node, a proposal) rather than
// boxed indirection: changes are transient values, not stored long-term.
#[allow(clippy::large_enum_variant)]
pub enum WorkGraphChange {
    AddNode {
        node: WorkNode,
    },
    UpdateNode {
        id: WorkNodeId,
        patch: WorkNodePatch,
    },
    AddEdge {
        edge: WorkEdge,
    },
    RemoveEdge {
        id: WorkEdgeId,
    },
    BindOperation {
        node: WorkNodeId,
        binding: OperationBinding,
    },
    ReconcileOperation {
        node: WorkNodeId,
        obs: OperationObservation,
    },
    AttachEvidence {
        node: WorkNodeId,
        evidence: EvidenceRef,
    },
    ProposePlanDiff {
        proposal: WorkGraphProposal,
    },
    /// Explicitly retire a pending proposal before a replacement is
    /// proposed. This keeps repeated "Revise plan" turns reviewable without
    /// silently rewriting or accumulating stale proposals.
    WithdrawPlanDiff {
        proposal_id: ProposalId,
    },
    AcceptPlanDiff {
        proposal_id: ProposalId,
        approval: ApprovalRef,
    },
    Supersede {
        old: WorkNodeId,
        replacement: WorkNodeId,
    },
    /// Atomically replace the inputs for the legacy Plan/To-do projections.
    ReplaceCompatProjection {
        compat: CompatProjectionState,
    },
    /// Record the canonical digest of a completed legacy import.
    SetImportDigest {
        digest: String,
    },
    /// Append one bounded, configuration-only activity receipt.
    RecordActivity {
        event: WorkActivityEvent,
    },
    /// Evict the oldest ended, non-durable Operation nodes beyond `keep`
    /// (#6842). See `reducer::prune_ended_operations` for eligibility.
    PruneEndedOperations {
        keep: usize,
    },
}

impl WorkGraphChange {
    /// Stable discriminant name recorded on receipts. Names only — receipts
    /// never carry payload text.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            WorkGraphChange::AddNode { .. } => "add_node",
            WorkGraphChange::UpdateNode { .. } => "update_node",
            WorkGraphChange::AddEdge { .. } => "add_edge",
            WorkGraphChange::RemoveEdge { .. } => "remove_edge",
            WorkGraphChange::BindOperation { .. } => "bind_operation",
            WorkGraphChange::ReconcileOperation { .. } => "reconcile_operation",
            WorkGraphChange::AttachEvidence { .. } => "attach_evidence",
            WorkGraphChange::ProposePlanDiff { .. } => "propose_plan_diff",
            WorkGraphChange::WithdrawPlanDiff { .. } => "withdraw_plan_diff",
            WorkGraphChange::AcceptPlanDiff { .. } => "accept_plan_diff",
            WorkGraphChange::Supersede { .. } => "supersede",
            WorkGraphChange::ReplaceCompatProjection { .. } => "replace_compat_projection",
            WorkGraphChange::SetImportDigest { .. } => "set_import_digest",
            WorkGraphChange::RecordActivity { .. } => "record_activity",
            WorkGraphChange::PruneEndedOperations { .. } => "prune_ended_operations",
        }
    }
}

/// Partial update of a node. Spec-silent shape: `Option` per patchable field,
/// `None` meaning "leave unchanged". Identity, kind, binding, and evidence
/// are deliberately NOT patchable here — they move only through their
/// dedicated changes (`BindOperation`, `AttachEvidence`, `Supersede`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkNodePatch {
    pub title: Option<String>,
    pub state: Option<NodeState>,
    pub acceptance: Option<Vec<AcceptanceRequirement>>,
    pub provenance: Option<Provenance>,
}

/// A reviewable plan diff. Spec-silent shape: explicit added/updated/removed
/// sets rather than nested changes, so the whole delta is inspectable before
/// acceptance and applies atomically (validated as one unit — no silent
/// mutation of objectives, dependencies, acceptance, or scope).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkGraphProposal {
    pub id: ProposalId,
    #[serde(default)]
    pub added_nodes: Vec<WorkNode>,
    #[serde(default)]
    pub added_edges: Vec<WorkEdge>,
    #[serde(default)]
    pub updated_nodes: Vec<ProposedNodeUpdate>,
    #[serde(default)]
    pub removed_nodes: Vec<WorkNodeId>,
    #[serde(default)]
    pub removed_edges: Vec<WorkEdgeId>,
    /// Graph-owned inputs for the legacy Plan/To-do projections. This is part
    /// of the reviewed scope delta and is applied atomically with the graph
    /// changes. Older saved proposals deserialize with no replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_compat: Option<CompatProjectionState>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposedNodeUpdate {
    pub id: WorkNodeId,
    pub patch: WorkNodePatch,
}

/// Reference to the approval that accepted a plan diff. Spec-silent shape:
/// a reference-only string (approval receipt / user action handle), recorded
/// on the Approval node the acceptance creates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRef {
    pub reference: String,
}

/// Lifecycle state as reported by an operation's owner. Placeholder for the
/// liveness slice; present now so reducer signatures are final.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerState {
    Initializing,
    Running,
    Waiting,
    Completed,
    /// Terminal: finished and produced output, but with dropped or failed
    /// slots the operator must be able to distinguish from an ordinary
    /// success (#5582). Collapsing this into `Completed` let dashboards and
    /// automation treat a partial workflow as fully accepted.
    Degraded,
    Failed,
    Cancelled,
}

/// Typed cancellation outcomes, mirroring real owner semantics (immediate
/// abort vs teardown-wait vs already-finished vs unknown-after-restart).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancelOutcome {
    Requested,
    Acknowledged,
    Forced,
    AlreadyFinished,
    NotFound,
    StaleUnknown,
}

/// An observation about a bound operation, produced by owner adapters (later
/// slice) and consumed by the reducer. The reducer applies these purely; it
/// never queries owners itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationObservation {
    /// Owner is authoritative. Idempotency key = `(binding, seq)`.
    OwnerReported {
        state: OwnerState,
        seq: u64,
        at: Ts,
        /// Bounded logical output receipt. The reference never contains raw
        /// logs or reasoning; `raw_bytes` preserves the pre-truncation size.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<EvidenceRef>,
    },
    /// No live handle for the binding (e.g. after restart). Never maps to
    /// Active or Completed — only to Stale (fail toward honesty).
    OwnerMissing {
        checked_at: Ts,
    },
    CancelUpdate {
        outcome: CancelOutcome,
        at: Ts,
    },
}

/// Compact record of the most recent observation, stored on the binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationSummary {
    pub owner_state: OwnerState,
    pub seq: u64,
    pub observed_at: Ts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<EvidenceRef>,
}

/// Everything ambient the reducer needs, supplied by the caller so the
/// reducer itself stays pure: no clock reads, no RNG, no globals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeCtx {
    /// Session identity used for deterministic ID derivation.
    pub session_id: String,
    /// Timestamp to record for this change (milliseconds since Unix epoch).
    pub now: Ts,
    /// Present for owner-observation changes; duplicates inside the
    /// snapshot's dedup window become no-op receipts.
    pub idempotency_key: Option<IdempotencyKey>,
}

/// Receipt for an applied (or deduplicated) change. Bounded history of these
/// lives on the snapshot. Receipts carry discriminant names and identifiers
/// only — no payload text, no secrets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeReceipt {
    pub change_id: ChangeId,
    pub revision: u64,
    pub summary: String,
    pub applied_at: Ts,
    pub idempotency_key: Option<IdempotencyKey>,
    pub no_op: bool,
}

impl ChangeReceipt {
    #[must_use]
    pub fn of(change: &WorkGraphChange, revision: u64, ctx: &ChangeCtx) -> Self {
        let kind = change.kind_name();
        ChangeReceipt {
            change_id: ChangeId::derive(&ctx.session_id, &format!("change:{revision}:{kind}")),
            revision,
            summary: kind.to_string(),
            applied_at: ctx.now,
            idempotency_key: ctx.idempotency_key.clone(),
            no_op: false,
        }
    }
}

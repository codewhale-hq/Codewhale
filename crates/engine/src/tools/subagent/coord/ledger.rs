//! Durable coordination records for delegated Work (#4647).
//!
//! Split out of `coord.rs` unchanged (#5462): the decision/claim/contention
//! ledger and its receipt types are the half of that file with consumers
//! outside the tool layer — `tui::coordination_detail`, `tui::work_surface`,
//! `tui::ui::tests`, and `core::engine::tests` all name these types — while
//! the tool wrappers around them are model-surface code. Keeping both in one
//! 3.8k-line file meant every read of either started by scrolling past the
//! other.
//!
//! This is a pure move with re-exports: `coord` re-publishes every public item
//! under its original path, so no consumer needed an edited import and no
//! behavior changed. New coordination *state* belongs here; new coordination
//! *tools* belong in `coord.rs`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    COORDINATION_PROJECTION_BYTE_LIMIT, COORDINATION_PROJECTION_DECISION_LIMIT,
    COORDINATION_RECORD_LIMIT,
};
use crate::tools::subagent::normalize_claim_path;

/// Coordination records for delegated Work (#4647).
///
/// Decision records, write-scope claims, and contention detection for parallel
/// agent work. Parallel work may proceed only when scopes and contracts do not
/// collide silently.
/// Status of a coordination decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Proposed,
    Accepted,
    Superseded,
}

/// Serialized coordination state schema. Increment only with an explicit
/// migration; restart/replay must never infer a newer contract from old data.
pub const COORDINATION_SCHEMA_VERSION: u32 = 1;

pub(super) const MAX_RECONCILIATION_RETRIES: u32 = 3;

const fn coordination_schema_version() -> u32 {
    COORDINATION_SCHEMA_VERSION
}

/// A bounded coordination decision record (#4647).
///
/// Persisted with stable subject, concise constraints, one active owner,
/// applicability scope, evidence handles, and sequence/version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionRecord {
    pub decision_id: String,
    pub subject: String,
    pub status: DecisionStatus,
    pub owner: String,
    pub scope: Vec<String>,
    pub constraints: Vec<String>,
    pub evidence_handles: Vec<String>,
    pub version: u32,
    pub sequence: u64,
}

/// A write-scope claim for a write-capable child (#4647).
///
/// Declares expected repo-relative paths/trees and named contracts.
/// This is coordination metadata, not another approval system.
///
/// An `exact_files` entry beneath a declared root binds that root to the
/// files listed under it (#6278): peers may then share the root with
/// disjoint file claims, and the claim's authorized surface is what
/// [`WriteScopeClaim::contains_path`] and [`WriteScopeClaim::overlaps`]
/// both see.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteScopeClaim {
    pub owner: String,
    pub roots: Vec<String>,
    pub exact_files: Vec<String>,
    pub contracts: Vec<String>,
}

impl WriteScopeClaim {
    /// Roots that authorize their whole tree. A root with declared
    /// `exact_files` beneath it is bound to those files instead: the files
    /// are the real exclusion boundary, so peers may share one root while
    /// claiming disjoint outputs (#6278). A root carrying no declared file
    /// keeps tree-wide authority, and an exact file outside every root
    /// stands alone.
    fn open_roots(&self) -> impl Iterator<Item = &String> {
        self.roots.iter().filter(|root| {
            !self
                .exact_files
                .iter()
                .any(|file| paths_overlap_by_containment(root, file))
        })
    }

    /// Check whether this claim overlaps with another. A claim overlaps when
    /// either normalized open tree contains the other or exact files collide.
    #[must_use]
    pub fn overlaps(&self, other: &WriteScopeClaim) -> bool {
        for root_a in self.open_roots() {
            for root_b in other.open_roots() {
                if paths_overlap_by_containment(root_a, root_b)
                    || paths_overlap_by_containment(root_b, root_a)
                {
                    return true;
                }
            }
        }
        for file_a in &self.exact_files {
            if other
                .exact_files
                .iter()
                .any(|file| paths_overlap_equal(file, file_a))
                || other
                    .open_roots()
                    .any(|root| paths_overlap_by_containment(root, file_a))
            {
                return true;
            }
        }
        for file_b in &other.exact_files {
            if self
                .open_roots()
                .any(|root| paths_overlap_by_containment(root, file_b))
            {
                return true;
            }
        }
        if self
            .contracts
            .iter()
            .any(|contract| other.contracts.iter().any(|other| other == contract))
        {
            return true;
        }
        false
    }

    #[must_use]
    pub fn contains_path(&self, path: &str) -> bool {
        self.exact_files.iter().any(|file| file == path)
            || self.open_roots().any(|root| path_contains(root, path))
    }
}

fn path_contains(root: &str, candidate: &str) -> bool {
    let root = root.trim_end_matches('/');
    let candidate = candidate.trim_end_matches('/');
    root == "."
        || root == candidate
        || candidate
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn paths_overlap_equal(left: &str, right: &str) -> bool {
    left == right || left.to_lowercase() == right.to_lowercase()
}

fn paths_overlap_by_containment(root: &str, candidate: &str) -> bool {
    path_contains(root, candidate) || path_contains(&root.to_lowercase(), &candidate.to_lowercase())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedWriteClaim {
    pub claim: WriteScopeClaim,
    pub sequence: u64,
    #[serde(default)]
    pub isolated_worktree: bool,
    /// Which of this claim's roots/exact files existed on disk when it was
    /// registered (#5906).
    ///
    /// Recorded because "the claimed path is gone" and "the claimed path does
    /// not exist yet" look identical from a later `exists()` call and mean
    /// opposite things. A claim on a worktree that has since been removed
    /// describes work nobody can be doing and must stop refusing claimants; a
    /// claim on a directory its owner is about to create is exactly the
    /// forward-looking reservation the ledger exists to honor, and disarming
    /// that would let two writers into the same new tree.
    ///
    /// Empty for claims persisted before this field existed and for claims
    /// that named nothing on disk at the time — both mean "no evidence of
    /// disappearance", so neither is ever treated as stale.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub present_at_claim: Vec<String>,
}

impl PersistedWriteClaim {
    /// Whether every path this claim was recorded as actually covering has
    /// since disappeared from disk (#5906).
    ///
    /// Isolated-worktree claims are excluded because they never contend at
    /// all — the answer would be unused, and their paths are not resolved
    /// against the coordination root.
    pub fn names_only_vanished_paths<F>(&self, mut path_exists: F) -> bool
    where
        F: FnMut(&str) -> bool,
    {
        !self.isolated_worktree
            && !self.present_at_claim.is_empty()
            && !self
                .present_at_claim
                .iter()
                .any(|path| path_exists(path.as_str()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconciliationReceipt {
    pub reconciliation_id: String,
    pub subject: String,
    pub owner: String,
    pub input_decisions: Vec<String>,
    pub outcome: String,
    pub evidence_handles: Vec<String>,
    /// Preserved candidate branches, patches, or artifact handles. A fan-in
    /// receipt is not valid if either conflicting candidate was discarded.
    #[serde(default)]
    pub candidate_handles: Vec<String>,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default)]
    pub retry_limit: u32,
    #[serde(default)]
    pub reviewer_evidence_handles: Vec<String>,
    #[serde(default)]
    pub verifier_evidence_handles: Vec<String>,
    #[serde(default)]
    pub verification_outcome: String,
    pub sequence: u64,
}

/// Durable receipt for the minimal accepted-decision context projected into a
/// child. It records counts and stable ids, never the child's transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextProjectionReceipt {
    pub child_id: String,
    pub decision_ids: Vec<String>,
    pub projected_bytes: usize,
    /// Repeated constraint facts elided across otherwise distinct decisions.
    /// Decision records themselves are never collapsed by this count.
    pub deduplicated: usize,
    /// Relevant unique decisions omitted solely because the hard count or
    /// byte bound was reached. This must not be conflated with deduplication.
    #[serde(default)]
    pub omitted: usize,
    pub sequence: u64,
}

/// Admission outcome persisted with a write-contention receipt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteContentionDisposition {
    BlockedPendingIsolationOrSerialization,
    ResolvedBySuccessfulClaim,
}

impl WriteContentionDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BlockedPendingIsolationOrSerialization => {
                "blocked_pending_isolation_or_serialization"
            }
            Self::ResolvedBySuccessfulClaim => "resolved_by_successful_claim",
        }
    }

    #[must_use]
    pub const fn blocks_admission(self) -> bool {
        matches!(self, Self::BlockedPendingIsolationOrSerialization)
    }
}

/// Durable non-secret receipt emitted when two active shared-workspace claims
/// collide. Rejected scope expansion remains visible after restart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteContentionReceipt {
    pub claimant: String,
    pub conflicting_owner: String,
    pub roots: Vec<String>,
    pub exact_files: Vec<String>,
    pub contracts: Vec<String>,
    pub disposition: WriteContentionDisposition,
    /// Sequence of the later successful claim that resolved this receipt.
    /// It intentionally references that claim's sequence instead of consuming
    /// another ledger sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_sequence: Option<u64>,
    pub sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationHotPath {
    pub path: String,
    pub active_claims: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationDetailMetrics {
    pub hottest_paths: Vec<CoordinationHotPath>,
    pub package_or_module_growth: Option<Value>,
    pub route_or_cost: Option<Value>,
    pub note: String,
}

/// One bounded typed projection shared by headless inspection and the TUI.
/// It contains durable coordination facts only, never raw reasoning or a
/// delegated transcript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoordinationDetailProjection {
    pub schema_version: u32,
    pub sequence: u64,
    pub decisions: Vec<DecisionRecord>,
    pub write_claims: Vec<PersistedWriteClaim>,
    pub reconciliations: Vec<ReconciliationReceipt>,
    pub context_projections: Vec<ContextProjectionReceipt>,
    pub contentions: Vec<WriteContentionReceipt>,
    pub metrics: CoordinationDetailMetrics,
    pub bounded: bool,
    pub limit: usize,
    /// Whether this process currently holds the workspace coordination flock.
    /// When false, durable ledger writes are skipped and the UI must say so —
    /// a counter must never tick on a turn the engine has already settled.
    #[serde(default = "default_process_lock_held")]
    pub process_lock_held: bool,
    /// Human-readable reason when [`Self::process_lock_held`] is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_lock_note: Option<String>,
}

fn default_process_lock_held() -> bool {
    // Legacy projections (tests, older sessions) assume the lock is held so
    // they do not spuriously light the unavailable banner.
    true
}

/// Attribution minted from an owner-admitted directory. Serialized data never
/// admits that directory: the manager requires its current held identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinationClaimScope {
    pub canonical_root: PathBuf,
    pub platform: String,
    pub volume: u64,
    pub index: u64,
}

impl CoordinationClaimScope {
    pub fn validate_shape(&self) -> Result<(), String> {
        if !self.canonical_root.is_absolute()
            || self.canonical_root.as_os_str().len() > 32768
            || self.canonical_root.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || !matches!(self.platform.as_str(), "unix" | "windows")
        {
            return Err("coordination root attribution is not bounded and canonical".into());
        }
        Ok(())
    }
}

fn projected_claim(claim: &WriteScopeClaim, root: &Path) -> WriteScopeClaim {
    let project = |path: &String| {
        let joined = if path == "." {
            root.to_path_buf()
        } else {
            root.join(path)
        };
        joined.to_string_lossy().replace('\\', "/")
    };
    WriteScopeClaim {
        owner: claim.owner.clone(),
        roots: claim.roots.iter().map(project).collect(),
        exact_files: claim.exact_files.iter().map(project).collect(),
        contracts: claim.contracts.clone(),
    }
}

/// Durable, bounded coordination state owned by `SubAgentManager`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationLedger {
    #[serde(default = "coordination_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub decisions: Vec<DecisionRecord>,
    #[serde(default)]
    pub write_claims: Vec<PersistedWriteClaim>,
    #[serde(default)]
    pub reconciliations: Vec<ReconciliationReceipt>,
    #[serde(default)]
    pub projections: Vec<ContextProjectionReceipt>,
    #[serde(default)]
    pub contentions: Vec<WriteContentionReceipt>,
    /// Root-session provenance for records whose logical owner (`root`) is
    /// otherwise shared by every conversation in a workspace. Agent-owned
    /// records can also be stamped here, but may be resolved through their
    /// immutable `SubAgent.owner_session_id`. Missing legacy provenance is
    /// never guessed by active-session views.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub record_sessions: HashMap<u64, String>,
    /// One bounded sequence-root map for shared claims and path-qualified
    /// decisions. The historical wire name stays stable; original schema1
    /// omits it, and scope-bearing schema2 never guesses an absent map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_scopes: Option<HashMap<u64, CoordinationClaimScope>>,
}

impl Default for CoordinationLedger {
    fn default() -> Self {
        Self {
            schema_version: COORDINATION_SCHEMA_VERSION,
            sequence: 0,
            decisions: Vec::new(),
            write_claims: Vec::new(),
            reconciliations: Vec::new(),
            projections: Vec::new(),
            contentions: Vec::new(),
            record_sessions: HashMap::new(),
            claim_scopes: None,
        }
    }
}

impl CoordinationLedger {
    fn next_sequence(&mut self) -> u64 {
        self.sequence = self.sequence.saturating_add(1);
        self.sequence
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn record_decision(&mut self, decision: DecisionRecord) -> Result<DecisionRecord, String> {
        self.record_decision_in_scope(decision, None)
    }

    pub fn record_decision_in_scope(
        &mut self,
        mut decision: DecisionRecord,
        scope: Option<CoordinationClaimScope>,
    ) -> Result<DecisionRecord, String> {
        self.validate_schema()?;
        if let Some(scope) = &scope {
            scope.validate_shape()?;
            if self
                .claim_scopes
                .as_ref()
                .is_some_and(|scopes| scopes.len() >= COORDINATION_RECORD_LIMIT)
            {
                return Err("coordination root attribution capacity reached".into());
            }
        }
        decision.decision_id = decision.decision_id.trim().to_string();
        if !decision.decision_id.is_empty() {
            decision.decision_id = bounded_coordination_atom("decision id", &decision.decision_id)?;
        }
        decision.subject = bounded_coordination_atom("decision subject", &decision.subject)?;
        decision.owner = bounded_coordination_atom("decision owner", &decision.owner)?;
        decision.scope = normalize_coordination_values("decision scope", &decision.scope, 24)?;
        decision.constraints =
            normalize_coordination_values("decision constraints", &decision.constraints, 24)?;
        decision.evidence_handles = normalize_coordination_values(
            "decision evidence handles",
            &decision.evidence_handles,
            24,
        )?;
        reject_sensitive_coordination_values(&decision.constraints)?;
        reject_sensitive_coordination_values(&decision.evidence_handles)?;
        if decision.subject.trim().is_empty() || decision.owner.trim().is_empty() {
            return Err("decision subject and owner are required".to_string());
        }
        if !decision.decision_id.trim().is_empty()
            && self
                .decisions
                .iter()
                .any(|existing| existing.decision_id == decision.decision_id)
        {
            return Err(format!(
                "decision id '{}' already exists",
                decision.decision_id
            ));
        }
        if decision.status == DecisionStatus::Accepted
            && let Some(existing) = self.decisions.iter().find(|existing| {
                existing.subject == decision.subject
                    && existing.status == DecisionStatus::Accepted
                    && existing.decision_id != decision.decision_id
            })
        {
            return Err(format!(
                "subject '{}' already has accepted decision '{}' owned by '{}'; preserve both candidates and use neutral reconciliation",
                decision.subject, existing.decision_id, existing.owner
            ));
        }
        let next_version = self
            .decisions
            .iter()
            .filter(|existing| existing.subject == decision.subject)
            .map(|existing| existing.version)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        decision.version = decision.version.max(next_version);
        decision.sequence = self.next_sequence();
        if decision.decision_id.trim().is_empty() {
            decision.decision_id = format!("decision_{}", decision.sequence);
        }
        self.decisions.push(decision.clone());
        if self.decisions.len() > COORDINATION_RECORD_LIMIT {
            let referenced = self
                .reconciliations
                .iter()
                .flat_map(|receipt| receipt.input_decisions.iter())
                .cloned()
                .collect::<BTreeSet<_>>();
            if let Some(index) = self.decisions.iter().position(|existing| {
                existing.status != DecisionStatus::Accepted
                    && !referenced.contains(&existing.decision_id)
            }) {
                self.decisions.remove(index);
            } else {
                self.decisions.pop();
                return Err(
                    "coordination decision capacity is occupied by accepted or reconciled records"
                        .to_string(),
                );
            }
        }
        self.prune_claim_scopes();
        if let Some(scope) = scope {
            self.schema_version = 2;
            self.claim_scopes
                .get_or_insert_with(HashMap::new)
                .insert(decision.sequence, scope);
        }
        Ok(decision)
    }

    pub fn update_decision_status(
        &mut self,
        decision_id: &str,
        status: DecisionStatus,
        owner: &str,
        expected_version: u32,
    ) -> Result<DecisionRecord, String> {
        self.validate_schema()?;
        let Some(index) = self
            .decisions
            .iter()
            .position(|decision| decision.decision_id == decision_id)
        else {
            return Err(format!("decision '{decision_id}' not found"));
        };
        if self.decisions[index].owner != owner {
            return Err(format!(
                "decision '{decision_id}' is owned by '{}'; caller '{owner}' cannot change it",
                self.decisions[index].owner
            ));
        }
        if self.decisions[index].version != expected_version {
            return Err(format!(
                "decision '{decision_id}' version changed: expected {expected_version}, current {}",
                self.decisions[index].version
            ));
        }
        let subject = self.decisions[index].subject.clone();
        if status == DecisionStatus::Accepted
            && let Some(existing) =
                self.decisions
                    .iter()
                    .enumerate()
                    .find_map(|(other_index, existing)| {
                        (other_index != index
                            && existing.subject == subject
                            && existing.status == DecisionStatus::Accepted)
                            .then_some(existing)
                    })
        {
            return Err(format!(
                "subject '{subject}' already has accepted decision '{}' owned by '{}'; preserve both candidates and use neutral reconciliation",
                existing.decision_id, existing.owner
            ));
        }
        let previous_sequence = self.decisions[index].sequence;
        let previous_scope = self.claim_scope(previous_sequence).cloned();
        let sequence = self.next_sequence();
        let decision = &mut self.decisions[index];
        decision.status = status;
        decision.version = decision.version.saturating_add(1);
        decision.sequence = sequence;
        let result = decision.clone();
        if let Some(scopes) = &mut self.claim_scopes {
            scopes.remove(&previous_sequence);
            if let Some(scope) = previous_scope {
                scopes.insert(sequence, scope);
            }
        }
        Ok(result)
    }

    /// Register a bounded write claim.
    ///
    /// `owner_is_active` is the caller's whole answer to "may this existing
    /// claim refuse a new claimant" — liveness and, since #5906, whether the
    /// claim's recorded paths still exist. The ledger has no filesystem of its
    /// own; [`super::SubAgentManager::admissible_coordination_owners`] resolves
    /// both before calling, and stamps
    /// [`PersistedWriteClaim::present_at_claim`] on the record afterwards.
    #[cfg(any(test, feature = "test-support"))]
    pub fn register_claim<F>(
        &mut self,
        claim: WriteScopeClaim,
        isolated_worktree: bool,
        owner_is_active: F,
    ) -> Result<PersistedWriteClaim, String>
    where
        F: FnMut(&str) -> bool,
    {
        self.register_claim_in_scope(claim, isolated_worktree, None, None, owner_is_active)
    }

    pub fn claim_scope(&self, sequence: u64) -> Option<&CoordinationClaimScope> {
        self.claim_scopes
            .as_ref()
            .and_then(|scopes| scopes.get(&sequence))
    }

    pub(in crate::tools::subagent) fn prune_claim_scopes(&mut self) {
        if let Some(scopes) = &mut self.claim_scopes {
            scopes.retain(|sequence, _| {
                self.write_claims
                    .iter()
                    .any(|claim| claim.sequence == *sequence)
                    || self
                        .decisions
                        .iter()
                        .any(|decision| decision.sequence == *sequence)
            });
        }
    }

    pub fn register_claim_in_scope<F>(
        &mut self,
        mut claim: WriteScopeClaim,
        isolated_worktree: bool,
        scope: Option<CoordinationClaimScope>,
        original_root: Option<&Path>,
        mut owner_is_active: F,
    ) -> Result<PersistedWriteClaim, String>
    where
        F: FnMut(&str) -> bool,
    {
        self.validate_schema()?;
        if let Some(scope) = &scope {
            scope.validate_shape()?;
            if isolated_worktree || original_root.is_none() {
                return Err(
                    "attributed claims require the held original root and shared execution".into(),
                );
            }
        }
        if self
            .claim_scopes
            .as_ref()
            .is_some_and(|scopes| !scopes.is_empty())
            && original_root.is_none()
        {
            return Err("attributed coordination state requires its held original root".into());
        }
        claim.owner = bounded_coordination_atom("write claim owner", &claim.owner)?;
        claim.roots = normalize_claim_paths(&claim.roots)?;
        claim.exact_files = normalize_claim_paths(&claim.exact_files)?;
        claim.contracts = normalize_claim_strings(&claim.contracts, 16, 128, "contracts")?;
        if claim.roots.is_empty() && claim.exact_files.is_empty() && claim.contracts.is_empty() {
            return Err(
                "write claim requires an owner and at least one root, file, or contract"
                    .to_string(),
            );
        }
        let replacing_existing_owner = self
            .write_claims
            .iter()
            .any(|existing| existing.claim.owner == claim.owner);
        if scope.is_some()
            && self
                .claim_scopes
                .as_ref()
                .is_some_and(|scopes| scopes.len() >= COORDINATION_RECORD_LIMIT)
            && !self.write_claims.iter().any(|existing| {
                existing.claim.owner == claim.owner && self.claim_scope(existing.sequence).is_some()
            })
        {
            return Err("coordination root attribution capacity reached".into());
        }
        if !replacing_existing_owner && self.write_claims.len() >= COORDINATION_RECORD_LIMIT {
            let mut inactive = Vec::new();
            for existing in &self.write_claims {
                if !owner_is_active(&existing.claim.owner) {
                    inactive.push((existing.sequence, existing.claim.owner.clone()));
                }
            }
            inactive.sort_by_key(|(sequence, _)| *sequence);
            for (_, owner) in inactive {
                if self.write_claims.len() < COORDINATION_RECORD_LIMIT {
                    break;
                }
                self.write_claims
                    .retain(|existing| existing.claim.owner != owner);
            }
            self.prune_claim_scopes();
            if self.write_claims.len() >= COORDINATION_RECORD_LIMIT {
                return Err(format!(
                    "write-claim capacity is {COORDINATION_RECORD_LIMIT} active owners; complete, serialize, or isolate existing work before admitting another writer"
                ));
            }
        }
        if !isolated_worktree
            && let Some(existing) = self
                .write_claims
                .iter()
                .find(|existing| {
                    !existing.isolated_worktree
                        && existing.claim.owner != claim.owner
                        && owner_is_active(&existing.claim.owner)
                        && match original_root {
                            Some(original) => {
                                let existing_root = self
                                    .claim_scope(existing.sequence)
                                    .map_or(original, |scope| scope.canonical_root.as_path());
                                let candidate_root = scope
                                    .as_ref()
                                    .map_or(original, |scope| scope.canonical_root.as_path());
                                projected_claim(&existing.claim, existing_root)
                                    .overlaps(&projected_claim(&claim, candidate_root))
                            }
                            None => existing.claim.overlaps(&claim),
                        }
                })
                .cloned()
        {
            let receipt = WriteContentionReceipt {
                claimant: claim.owner.clone(),
                conflicting_owner: existing.claim.owner.clone(),
                roots: claim.roots.clone(),
                exact_files: claim.exact_files.clone(),
                contracts: claim.contracts.clone(),
                disposition: WriteContentionDisposition::BlockedPendingIsolationOrSerialization,
                resolution_sequence: None,
                sequence: self.next_sequence(),
            };
            self.contentions.push(receipt);
            trim_front(&mut self.contentions, COORDINATION_RECORD_LIMIT);
            return Err(format!(
                "write-scope contention with {}: requested roots {:?}, files {:?}, contracts {:?} overlap its writable roots {:?}, files {:?}, contracts {:?}. Claim disjoint sibling write_roots (for example tmp/scan/worker-a and tmp/scan/worker-b) or exact_files for each output. A read-only worker uses write_authority=read_only without a write claim. Otherwise serialize the writers (wait for that owner to settle, or cancel it) or use worktree isolation; a nested path inside an existing writable root still overlaps.",
                existing.claim.owner,
                claim.roots,
                claim.exact_files,
                claim.contracts,
                existing.claim.roots,
                existing.claim.exact_files,
                existing.claim.contracts
            ));
        }
        self.write_claims
            .retain(|existing| existing.claim.owner != claim.owner);
        let record = PersistedWriteClaim {
            claim,
            sequence: self.next_sequence(),
            isolated_worktree,
            // Stamped by the caller, which owns the filesystem.
            present_at_claim: Vec::new(),
        };
        for contention in &mut self.contentions {
            if contention.claimant == record.claim.owner
                && contention.disposition.blocks_admission()
            {
                contention.disposition = WriteContentionDisposition::ResolvedBySuccessfulClaim;
                contention.resolution_sequence = Some(record.sequence);
            }
        }
        self.write_claims.push(record.clone());
        self.prune_claim_scopes();
        if let Some(scope) = scope {
            self.schema_version = 2;
            self.claim_scopes
                .get_or_insert_with(HashMap::new)
                .insert(record.sequence, scope);
        }
        Ok(record)
    }

    /// Release write claims whose owner is not a live claimant (#5562).
    ///
    /// Claims are durable records and legitimately outlive the agents that
    /// registered them; that is fine for history, but a stale claim must never
    /// keep blocking a later writer. An optional owner restricts the sweep to
    /// one claim owner; live claimants are never removed. Returns the released
    /// owners (one entry per claim, in ledger order).
    pub fn release_stale_claims<F>(
        &mut self,
        owner: Option<&str>,
        mut owner_is_active: F,
    ) -> Result<Vec<String>, String>
    where
        F: FnMut(&str) -> bool,
    {
        self.validate_schema()?;
        let owner_filter = match owner {
            Some(value) => Some(bounded_coordination_atom("write claim owner", value)?),
            None => None,
        };
        let mut released = Vec::new();
        self.write_claims.retain(|record| {
            if owner_filter
                .as_deref()
                .is_some_and(|wanted| wanted != record.claim.owner)
            {
                return true;
            }
            if owner_is_active(&record.claim.owner) {
                return true;
            }
            released.push(record.claim.owner.clone());
            false
        });
        // A release is a mutation: advance the ledger sequence so receipts
        // stamped by the caller stay coherent (coordinator contract).
        if !released.is_empty() {
            self.next_sequence();
            self.prune_claim_scopes();
        }
        Ok(released)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reconcile(
        &mut self,
        subject: String,
        owner: String,
        input_decisions: Vec<String>,
        outcome: String,
        evidence_handles: Vec<String>,
        candidate_handles: Vec<String>,
        retry_count: u32,
        retry_limit: u32,
        reviewer_evidence_handles: Vec<String>,
        verifier_evidence_handles: Vec<String>,
        verification_outcome: String,
    ) -> Result<ReconciliationReceipt, String> {
        self.validate_schema()?;
        let subject = bounded_coordination_atom("reconciliation subject", &subject)?;
        let owner = bounded_coordination_atom("reconciliation owner", &owner)?;
        let outcome = bounded_coordination_atom("reconciliation outcome", &outcome)?;
        let verification_outcome = bounded_coordination_atom(
            "reconciliation verification outcome",
            &verification_outcome,
        )?;
        if input_decisions.len() < 2 {
            return Err("neutral fan-in requires at least two input decisions".to_string());
        }
        if input_decisions.iter().collect::<BTreeSet<_>>().len() != input_decisions.len() {
            return Err("neutral fan-in decision ids must be distinct".to_string());
        }
        if candidate_handles.len() < 2
            || candidate_handles
                .iter()
                .any(|handle| handle.trim().is_empty())
        {
            return Err(
                "neutral fan-in must preserve at least two candidate branch, patch, or artifact handles"
                    .to_string(),
            );
        }
        if candidate_handles.iter().collect::<BTreeSet<_>>().len() != candidate_handles.len() {
            return Err("neutral fan-in candidate handles must be distinct".to_string());
        }
        let input_decisions =
            normalize_coordination_values("input decision ids", &input_decisions, 24)?;
        let evidence_handles = normalize_coordination_values(
            "reconciliation evidence handles",
            &evidence_handles,
            24,
        )?;
        let candidate_handles =
            normalize_coordination_values("candidate handles", &candidate_handles, 24)?;
        if input_decisions.len() < 2 {
            return Err(
                "neutral fan-in requires at least two distinct normalized input decisions"
                    .to_string(),
            );
        }
        if candidate_handles.len() < 2 {
            return Err(
                "neutral fan-in must preserve at least two distinct normalized candidate handles"
                    .to_string(),
            );
        }
        let reviewer_evidence_handles = normalize_coordination_values(
            "Reviewer evidence handles",
            &reviewer_evidence_handles,
            24,
        )?;
        let verifier_evidence_handles = normalize_coordination_values(
            "Verifier evidence handles",
            &verifier_evidence_handles,
            24,
        )?;
        reject_sensitive_coordination_values(&evidence_handles)?;
        reject_sensitive_coordination_values(&candidate_handles)?;
        reject_sensitive_coordination_values(&reviewer_evidence_handles)?;
        reject_sensitive_coordination_values(&verifier_evidence_handles)?;
        if retry_limit == 0 || retry_limit > MAX_RECONCILIATION_RETRIES {
            return Err(format!(
                "reconciliation retry_limit must be between 1 and {MAX_RECONCILIATION_RETRIES}"
            ));
        }
        if retry_count > retry_limit {
            return Err("reconciliation retry_count exceeds retry_limit".to_string());
        }
        if reviewer_evidence_handles.is_empty() || verifier_evidence_handles.is_empty() {
            return Err(
                "neutral fan-in requires independent Reviewer and Verifier evidence handles"
                    .to_string(),
            );
        }
        if reviewer_evidence_handles.iter().any(|review| {
            verifier_evidence_handles
                .iter()
                .any(|verify| verify == review)
        }) {
            return Err("Reviewer and Verifier evidence handles must be independent".to_string());
        }
        if !matches!(
            verification_outcome.as_str(),
            "verified" | "failed" | "blocked"
        ) {
            return Err(
                "neutral fan-in verification_outcome must be verified, failed, or blocked"
                    .to_string(),
            );
        }
        if input_decisions.iter().any(|id| {
            !self
                .decisions
                .iter()
                .any(|decision| &decision.decision_id == id)
        }) {
            return Err("reconciliation references an unknown decision".to_string());
        }
        let inputs = input_decisions
            .iter()
            .filter_map(|id| {
                self.decisions
                    .iter()
                    .find(|decision| &decision.decision_id == id)
            })
            .collect::<Vec<_>>();
        if inputs.iter().any(|decision| decision.subject != subject) {
            return Err("reconciliation inputs must share the requested subject".to_string());
        }
        if inputs.iter().any(|decision| decision.owner == owner) {
            return Err(
                "neutral fan-in owner must differ from every input decision owner".to_string(),
            );
        }
        let sequence = self.next_sequence();
        let receipt = ReconciliationReceipt {
            reconciliation_id: format!("reconcile_{sequence}"),
            subject,
            owner,
            input_decisions,
            outcome,
            evidence_handles,
            candidate_handles,
            retry_count,
            retry_limit,
            reviewer_evidence_handles,
            verifier_evidence_handles,
            verification_outcome,
            sequence,
        };
        self.reconciliations.push(receipt.clone());
        trim_front(&mut self.reconciliations, COORDINATION_RECORD_LIMIT);
        Ok(receipt)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn project_relevant_decisions(
        &mut self,
        child_id: &str,
        claim: Option<&WriteScopeClaim>,
        capabilities: &[String],
    ) -> (String, ContextProjectionReceipt) {
        self.project_relevant_decisions_in_scope(child_id, claim, capabilities, None, None)
    }

    pub fn project_relevant_decisions_in_scope(
        &mut self,
        child_id: &str,
        claim: Option<&WriteScopeClaim>,
        capabilities: &[String],
        claim_root: Option<&Path>,
        original_root: Option<&Path>,
    ) -> (String, ContextProjectionReceipt) {
        const HEADER: &str = "Accepted coordination decisions relevant to this child (bounded):\n";
        let mut seen_constraint_facts = BTreeSet::new();
        let mut decision_ids = Vec::new();
        let mut lines = Vec::new();
        let mut projected_bytes = 0usize;
        let mut deduplicated = 0usize;
        let mut omitted = 0usize;
        for decision in self
            .decisions
            .iter()
            .rev()
            .filter(|decision| decision.status == DecisionStatus::Accepted)
            .filter(|decision| {
                decision_is_relevant(
                    decision,
                    claim,
                    capabilities,
                    claim_root,
                    self.claim_scope(decision.sequence)
                        .map(|scope| scope.canonical_root.as_path())
                        .or(original_root),
                )
            })
        {
            if decision_ids.len() >= COORDINATION_PROJECTION_DECISION_LIMIT {
                omitted = omitted.saturating_add(1);
                continue;
            }
            let constraints = decision
                .constraints
                .iter()
                .filter_map(|value| {
                    let value = bounded_utf8(value, 192);
                    if seen_constraint_facts.insert(value.clone()) {
                        Some(value)
                    } else {
                        deduplicated = deduplicated.saturating_add(1);
                        None
                    }
                })
                .take(8)
                .collect::<Vec<_>>()
                .join("; ");
            let mut line = format!(
                "- {} v{} [{}] owner={}",
                decision.subject, decision.version, decision.decision_id, decision.owner,
            );
            if !constraints.is_empty() {
                line.push_str(": ");
                line.push_str(&constraints);
            }
            let line = bounded_utf8(&line, 512);
            let added_bytes = line.len().saturating_add(1);
            if HEADER
                .len()
                .saturating_add(projected_bytes)
                .saturating_add(added_bytes)
                > COORDINATION_PROJECTION_BYTE_LIMIT
            {
                omitted = omitted.saturating_add(1);
                continue;
            }
            projected_bytes = projected_bytes.saturating_add(added_bytes);
            decision_ids.push(decision.decision_id.clone());
            lines.push(line);
        }
        let projection = if lines.is_empty() {
            String::new()
        } else {
            format!("{HEADER}{}", lines.join("\n"))
        };
        let receipt = ContextProjectionReceipt {
            child_id: child_id.to_string(),
            decision_ids,
            projected_bytes: projection.len(),
            deduplicated,
            omitted,
            sequence: self.next_sequence(),
        };
        self.projections.push(receipt.clone());
        trim_front(&mut self.projections, COORDINATION_RECORD_LIMIT);
        (projection, receipt)
    }

    pub(in crate::tools::subagent) fn validate_replay(&mut self) -> Result<(), String> {
        self.validate_schema()?;
        if self.decisions.len() > COORDINATION_RECORD_LIMIT
            || self.write_claims.len() > COORDINATION_RECORD_LIMIT
            || self.reconciliations.len() > COORDINATION_RECORD_LIMIT
            || self.projections.len() > COORDINATION_RECORD_LIMIT
            || self.contentions.len() > COORDINATION_RECORD_LIMIT
        {
            return Err("coordination record count exceeds the durable bound".to_string());
        }

        let mut sequences = BTreeSet::new();
        let mut max_sequence = 0_u64;
        let mut decision_ids = BTreeSet::new();
        let mut accepted_subjects = BTreeSet::new();
        for decision in &self.decisions {
            bounded_coordination_atom("decision id", &decision.decision_id)?;
            bounded_coordination_atom("decision subject", &decision.subject)?;
            bounded_coordination_atom("decision owner", &decision.owner)?;
            if decision.version == 0 {
                return Err(format!(
                    "decision '{}' has zero version",
                    decision.decision_id
                ));
            }
            validate_sequence(
                decision.sequence,
                "decision",
                &mut sequences,
                &mut max_sequence,
            )?;
            if !decision_ids.insert(decision.decision_id.clone()) {
                return Err(format!("duplicate decision id '{}'", decision.decision_id));
            }
            if decision.status == DecisionStatus::Accepted
                && !accepted_subjects.insert(decision.subject.clone())
            {
                return Err(format!(
                    "multiple accepted decisions own subject '{}'",
                    decision.subject
                ));
            }
            validate_normalized_coordination_values("decision scope", &decision.scope, 24)?;
            validate_normalized_coordination_values(
                "decision constraints",
                &decision.constraints,
                24,
            )?;
            validate_normalized_coordination_values(
                "decision evidence handles",
                &decision.evidence_handles,
                24,
            )?;
            reject_sensitive_coordination_values(&decision.constraints)?;
            reject_sensitive_coordination_values(&decision.evidence_handles)?;
        }

        let mut claim_owners = BTreeSet::new();
        for claim in &self.write_claims {
            validate_sequence(
                claim.sequence,
                "write claim",
                &mut sequences,
                &mut max_sequence,
            )?;
            bounded_coordination_atom("write claim owner", &claim.claim.owner)?;
            if !claim_owners.insert(claim.claim.owner.clone()) {
                return Err(format!(
                    "duplicate write claim owner '{}'",
                    claim.claim.owner
                ));
            }
            let roots = normalize_claim_paths(&claim.claim.roots)?;
            let exact_files = normalize_claim_paths(&claim.claim.exact_files)?;
            let contracts = normalize_claim_strings(&claim.claim.contracts, 16, 128, "contracts")?;
            if roots != claim.claim.roots
                || exact_files != claim.claim.exact_files
                || contracts != claim.claim.contracts
                || (roots.is_empty() && exact_files.is_empty() && contracts.is_empty())
            {
                return Err(format!(
                    "write claim for '{}' is not normalized and bounded",
                    claim.claim.owner
                ));
            }
        }

        if let Some(scopes) = &self.claim_scopes {
            if scopes.len() > COORDINATION_RECORD_LIMIT {
                return Err("coordination root attribution exceeds claim capacity".into());
            }
            for (sequence, scope) in scopes {
                scope.validate_shape()?;
                if !self
                    .write_claims
                    .iter()
                    .any(|claim| claim.sequence == *sequence && !claim.isolated_worktree)
                    && !self
                        .decisions
                        .iter()
                        .any(|decision| decision.sequence == *sequence)
                {
                    return Err(
                        "coordination root attribution has no matching shared claim or decision"
                            .into(),
                    );
                }
            }
        }

        for receipt in &self.reconciliations {
            validate_sequence(
                receipt.sequence,
                "reconciliation",
                &mut sequences,
                &mut max_sequence,
            )?;
            validate_reconciliation_receipt(receipt, &self.decisions)?;
        }
        for projection in &self.projections {
            validate_sequence(
                projection.sequence,
                "context projection",
                &mut sequences,
                &mut max_sequence,
            )?;
            bounded_coordination_atom("projection child", &projection.child_id)?;
            if projection.decision_ids.len() > COORDINATION_PROJECTION_DECISION_LIMIT
                || projection.projected_bytes > COORDINATION_PROJECTION_BYTE_LIMIT
                || projection
                    .decision_ids
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len()
                    != projection.decision_ids.len()
            {
                return Err(format!(
                    "context projection for '{}' exceeds its bounds or duplicates decisions",
                    projection.child_id
                ));
            }
        }
        for contention in &self.contentions {
            validate_sequence(
                contention.sequence,
                "contention",
                &mut sequences,
                &mut max_sequence,
            )?;
            bounded_coordination_atom("contention claimant", &contention.claimant)?;
            bounded_coordination_atom(
                "contention conflicting owner",
                &contention.conflicting_owner,
            )?;
            match (contention.disposition, contention.resolution_sequence) {
                (WriteContentionDisposition::BlockedPendingIsolationOrSerialization, None) => {}
                (WriteContentionDisposition::ResolvedBySuccessfulClaim, Some(sequence))
                    if sequence > contention.sequence && sequence <= self.sequence => {}
                (WriteContentionDisposition::BlockedPendingIsolationOrSerialization, Some(_)) => {
                    return Err(
                        "blocked contention receipt cannot carry a resolution sequence".to_string(),
                    );
                }
                (WriteContentionDisposition::ResolvedBySuccessfulClaim, _) => {
                    return Err(
                        "resolved contention receipt requires a later valid resolution sequence"
                            .to_string(),
                    );
                }
            }
            if normalize_claim_paths(&contention.roots)? != contention.roots
                || normalize_claim_paths(&contention.exact_files)? != contention.exact_files
                || normalize_claim_strings(&contention.contracts, 16, 128, "contracts")?
                    != contention.contracts
            {
                return Err("contention receipt paths/contracts are not normalized".to_string());
            }
        }
        if self.sequence < max_sequence {
            return Err(format!(
                "coordination sequence {} is behind record sequence {max_sequence}",
                self.sequence
            ));
        }
        Ok(())
    }

    fn validate_schema(&self) -> Result<(), String> {
        match (self.schema_version, self.claim_scopes.as_ref()) {
            (COORDINATION_SCHEMA_VERSION, None) | (2, Some(_)) => Ok(()),
            (2, None) => {
                Err("coordination schema 2 requires explicit root attribution state".into())
            }
            (COORDINATION_SCHEMA_VERSION, Some(_)) => {
                Err("legacy coordination schema cannot contain attributed roots".into())
            }
            (version, _) => Err(format!(
                "unsupported coordination schema {version}; expected 1 or 2"
            )),
        }
    }
}

fn normalize_claim_paths(paths: &[String]) -> Result<Vec<String>, String> {
    if paths.len() > 32 {
        return Err("write claim paths accept at most 32 entries".to_string());
    }
    let mut normalized = Vec::new();
    for path in paths {
        let path = normalize_claim_path(path)?;
        if !normalized.contains(&path) {
            normalized.push(path);
        }
    }
    Ok(normalized)
}

fn normalize_claim_strings(
    values: &[String],
    count_limit: usize,
    char_limit: usize,
    field: &str,
) -> Result<Vec<String>, String> {
    if values.len() > count_limit {
        return Err(format!(
            "write claim {field} accepts at most {count_limit} entries"
        ));
    }
    let mut normalized = Vec::new();
    for value in values {
        let value = value.trim();
        if value.is_empty()
            || value.chars().count() > char_limit
            || value.chars().any(char::is_control)
        {
            return Err(format!(
                "write claim {field} entries must be 1..={char_limit} characters"
            ));
        }
        if !normalized.iter().any(|existing| existing == value) {
            normalized.push(value.to_string());
        }
    }
    Ok(normalized)
}

fn bounded_coordination_atom(field: &str, value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().count() > 512
        || value.chars().any(|ch| matches!(ch, '\r' | '\n'))
    {
        return Err(format!(
            "{field} must be one non-empty line of at most 512 characters"
        ));
    }
    Ok(value.to_string())
}

fn normalize_coordination_values(
    field: &str,
    values: &[String],
    limit: usize,
) -> Result<Vec<String>, String> {
    if values.len() > limit {
        return Err(format!("{field} accepts at most {limit} entries"));
    }
    let mut normalized = Vec::new();
    for value in values {
        let value = bounded_coordination_atom(field, value)?;
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    Ok(normalized)
}

fn validate_normalized_coordination_values(
    field: &str,
    values: &[String],
    limit: usize,
) -> Result<(), String> {
    if normalize_coordination_values(field, values, limit)? != values {
        return Err(format!("{field} is not trimmed and deduplicated"));
    }
    Ok(())
}

fn reject_sensitive_coordination_values(values: &[String]) -> Result<(), String> {
    const SENSITIVE_MARKERS: &[&str] = &[
        "secret",
        "password",
        "api_key",
        "api-key",
        "authorization:",
        "bearer ",
        "token=",
        "sk-",
        "ghp_",
        "xoxb-",
        "<thinking",
        "chain of thought",
        "raw reasoning",
    ];
    for value in values {
        let lower = value.to_ascii_lowercase();
        if let Some(marker) = SENSITIVE_MARKERS
            .iter()
            .find(|marker| lower.contains(**marker))
        {
            return Err(format!(
                "coordination metadata rejected sensitive or raw-reasoning marker '{marker}'"
            ));
        }
    }
    Ok(())
}

fn validate_sequence(
    sequence: u64,
    kind: &str,
    sequences: &mut BTreeSet<u64>,
    max_sequence: &mut u64,
) -> Result<(), String> {
    if sequence == 0 || !sequences.insert(sequence) {
        return Err(format!(
            "{kind} has a zero or duplicate sequence {sequence}"
        ));
    }
    *max_sequence = (*max_sequence).max(sequence);
    Ok(())
}

fn validate_reconciliation_receipt(
    receipt: &ReconciliationReceipt,
    decisions: &[DecisionRecord],
) -> Result<(), String> {
    bounded_coordination_atom("reconciliation id", &receipt.reconciliation_id)?;
    bounded_coordination_atom("reconciliation subject", &receipt.subject)?;
    bounded_coordination_atom("reconciliation owner", &receipt.owner)?;
    bounded_coordination_atom("reconciliation outcome", &receipt.outcome)?;
    if receipt.input_decisions.len() < 2
        || receipt
            .input_decisions
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != receipt.input_decisions.len()
    {
        return Err("reconciliation requires at least two distinct decision ids".to_string());
    }
    let inputs = receipt
        .input_decisions
        .iter()
        .map(|id| {
            decisions
                .iter()
                .find(|decision| &decision.decision_id == id)
                .ok_or_else(|| format!("reconciliation references unknown decision '{id}'"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if inputs
        .iter()
        .any(|decision| decision.subject != receipt.subject)
    {
        return Err("reconciliation inputs must share the requested subject".to_string());
    }
    if inputs
        .iter()
        .any(|decision| decision.owner == receipt.owner)
    {
        return Err("neutral fan-in owner must differ from every candidate owner".to_string());
    }
    if receipt.candidate_handles.len() < 2
        || receipt
            .candidate_handles
            .iter()
            .collect::<BTreeSet<_>>()
            .len()
            != receipt.candidate_handles.len()
    {
        return Err("reconciliation requires at least two distinct candidate handles".to_string());
    }
    validate_normalized_coordination_values("candidate handles", &receipt.candidate_handles, 24)?;
    validate_normalized_coordination_values(
        "reconciliation evidence handles",
        &receipt.evidence_handles,
        24,
    )?;
    validate_normalized_coordination_values(
        "Reviewer evidence handles",
        &receipt.reviewer_evidence_handles,
        24,
    )?;
    validate_normalized_coordination_values(
        "Verifier evidence handles",
        &receipt.verifier_evidence_handles,
        24,
    )?;
    reject_sensitive_coordination_values(&receipt.candidate_handles)?;
    reject_sensitive_coordination_values(&receipt.evidence_handles)?;
    reject_sensitive_coordination_values(&receipt.reviewer_evidence_handles)?;
    reject_sensitive_coordination_values(&receipt.verifier_evidence_handles)?;
    if receipt.retry_limit == 0
        || receipt.retry_limit > MAX_RECONCILIATION_RETRIES
        || receipt.retry_count > receipt.retry_limit
    {
        return Err("reconciliation retry count/limit is invalid".to_string());
    }
    if receipt.reviewer_evidence_handles.is_empty()
        || receipt.verifier_evidence_handles.is_empty()
        || receipt.reviewer_evidence_handles.iter().any(|review| {
            receipt
                .verifier_evidence_handles
                .iter()
                .any(|verify| verify == review)
        })
    {
        return Err("Reviewer and Verifier evidence must be present and independent".to_string());
    }
    if !matches!(
        receipt.verification_outcome.as_str(),
        "verified" | "failed" | "blocked"
    ) {
        return Err("reconciliation verification outcome is invalid".to_string());
    }
    Ok(())
}

fn decision_is_relevant(
    decision: &DecisionRecord,
    claim: Option<&WriteScopeClaim>,
    capabilities: &[String],
    claim_root: Option<&Path>,
    decision_root: Option<&Path>,
) -> bool {
    let relevant_path = |claim: &WriteScopeClaim, value: &str| match (claim_root, decision_root) {
        (Some(claim_root), Some(decision_root)) => {
            let Ok(relative) = normalize_claim_path(value) else {
                return false;
            };
            let path = if relative == "." {
                decision_root.to_path_buf()
            } else {
                decision_root.join(relative)
            };
            let contained = path
                .strip_prefix(claim_root)
                .ok()
                .is_some_and(|relative| claim.contains_path(&relative.to_string_lossy()));
            contained
                || claim
                    .roots
                    .iter()
                    .chain(&claim.exact_files)
                    .any(|relative| claim_root.join(relative).starts_with(&path))
        }
        _ => claim_reaches_path(claim, value),
    };
    if decision.scope.is_empty() {
        return true;
    }
    decision.scope.iter().any(|raw| {
        let value = raw.trim();
        let (kind, value) = value
            .split_once(':')
            .map_or(("", value), |(kind, value)| (kind.trim(), value.trim()));
        match kind {
            "capability" => capabilities.iter().any(|capability| capability == value),
            "contract" => {
                claim.is_some_and(|claim| claim.contracts.iter().any(|contract| contract == value))
            }
            "path" => claim.is_some_and(|claim| relevant_path(claim, value)),
            _ => {
                capabilities.iter().any(|capability| capability == value)
                    || claim.is_some_and(|claim| {
                        claim.contracts.iter().any(|contract| contract == value)
                            || relevant_path(claim, value)
                    })
            }
        }
    })
}

fn claim_reaches_path(claim: &WriteScopeClaim, path: &str) -> bool {
    let Ok(path) = normalize_claim_path(path) else {
        return false;
    };
    claim.contains_path(&path)
        || claim.roots.iter().any(|root| path_contains(&path, root))
        || claim
            .exact_files
            .iter()
            .any(|file| path_contains(&path, file))
}

fn bounded_utf8(value: &str, byte_limit: usize) -> String {
    if value.len() <= byte_limit {
        return value.to_string();
    }
    let mut end = byte_limit;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].to_string()
}

fn trim_front<T>(records: &mut Vec<T>, limit: usize) {
    if records.len() > limit {
        records.drain(..records.len() - limit);
    }
}

#[cfg(test)]
mod records_tests {
    use super::*;
    use serde_json::json;

    fn scoped_root(path: impl AsRef<Path>) -> CoordinationClaimScope {
        CoordinationClaimScope {
            canonical_root: path.as_ref().to_path_buf(),
            platform: if cfg!(windows) { "windows" } else { "unix" }.into(),
            volume: 1,
            index: 1,
        }
    }

    fn root_claim(owner: &str, path: &str, contract: Option<&str>) -> WriteScopeClaim {
        WriteScopeClaim {
            owner: owner.into(),
            roots: vec![path.into()],
            exact_files: Vec::new(),
            contracts: contract.map(String::from).into_iter().collect(),
        }
    }

    #[test]
    fn attributed_claims_project_nested_and_alias_roots_and_keep_contracts_global() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let original = root.join("original");
        let mut ledger = CoordinationLedger::default();
        ledger
            .register_claim_in_scope(
                root_claim("a", "src", None),
                false,
                Some(scoped_root(root.join("shared"))),
                Some(&original),
                |_| true,
            )
            .unwrap();
        assert!(
            ledger
                .register_claim_in_scope(
                    root_claim("nested", ".", None),
                    false,
                    Some(scoped_root(root.join("shared/src"))),
                    Some(&original),
                    |_| true
                )
                .is_err()
        );
        assert!(
            ledger
                .register_claim_in_scope(
                    root_claim("alias", "src", None),
                    false,
                    Some(scoped_root(root.join("shared"))),
                    Some(&original),
                    |_| true
                )
                .is_err()
        );
        ledger
            .register_claim_in_scope(
                root_claim("disjoint", "src", Some("release")),
                false,
                Some(scoped_root(root.join("other"))),
                Some(&original),
                |_| true,
            )
            .unwrap();
        assert!(
            ledger
                .register_claim_in_scope(
                    root_claim("global-contract", "unrelated", Some("release")),
                    false,
                    None,
                    Some(&original),
                    |_| true
                )
                .is_err()
        );
    }

    #[test]
    fn attributed_schema_is_sticky_and_absent_map_or_orphan_receipt_refuses() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let original = root.join("original");
        let mut legacy = CoordinationLedger::default();
        legacy
            .register_claim(root_claim("legacy", "src", None), false, |_| false)
            .unwrap();
        let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
        assert!(
            !String::from_utf8(legacy_bytes.clone())
                .unwrap()
                .contains("claim_scopes")
        );
        let mut decoded: CoordinationLedger = serde_json::from_slice(&legacy_bytes).unwrap();
        decoded.validate_replay().unwrap();
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), legacy_bytes);
        decoded
            .register_claim_in_scope(
                root_claim("extra", "src", None),
                false,
                Some(scoped_root(root.join("other"))),
                Some(&original),
                |_| false,
            )
            .unwrap();
        assert_eq!(decoded.schema_version, 2);
        decoded.release_stale_claims(None, |_| false).unwrap();
        assert!(decoded.claim_scopes.as_ref().unwrap().is_empty());
        assert_eq!(decoded.schema_version, 2);
        decoded.validate_replay().unwrap();
        decoded.claim_scopes = None;
        assert!(
            decoded
                .validate_replay()
                .unwrap_err()
                .contains("requires explicit")
        );
        decoded.claim_scopes = Some(HashMap::from([(100, scoped_root(root.join("other")))]));
        assert!(
            decoded
                .validate_replay()
                .unwrap_err()
                .contains("matching shared claim")
        );
        decoded.schema_version = 1;
        assert!(
            decoded
                .validate_replay()
                .unwrap_err()
                .contains("legacy coordination schema")
        );
    }

    #[test]
    fn overlapping_roots_detected() {
        let a = WriteScopeClaim {
            owner: "agent-a".into(),
            roots: vec!["src/tui/".into()],
            exact_files: vec![],
            contracts: vec![],
        };
        let b = WriteScopeClaim {
            owner: "agent-b".into(),
            roots: vec!["src/tui/widgets/".into()],
            exact_files: vec![],
            contracts: vec![],
        };
        assert!(a.overlaps(&b));
    }

    #[test]
    fn disjoint_roots_no_overlap() {
        let a = WriteScopeClaim {
            owner: "agent-a".into(),
            roots: vec!["src/tui/".into()],
            exact_files: vec![],
            contracts: vec![],
        };
        let b = WriteScopeClaim {
            owner: "agent-b".into(),
            roots: vec!["src/core/".into()],
            exact_files: vec![],
            contracts: vec![],
        };
        assert!(!a.overlaps(&b));
    }

    #[test]
    fn exact_file_collision_detected() {
        let a = WriteScopeClaim {
            owner: "agent-a".into(),
            roots: vec![],
            exact_files: vec!["src/main.rs".into()],
            contracts: vec![],
        };
        let b = WriteScopeClaim {
            owner: "agent-b".into(),
            roots: vec![],
            exact_files: vec!["src/main.rs".into()],
            contracts: vec![],
        };
        assert!(a.overlaps(&b));
    }

    #[test]
    fn release_stale_claims_releases_only_inactive_owners_and_honours_an_owner_filter() {
        let mut ledger = CoordinationLedger::default();
        for (owner, root) in [
            ("live-builder", "src/live"),
            ("zombie-builder", "src/zombie"),
            ("prior-session-builder", "src/prior"),
        ] {
            ledger
                .register_claim(
                    WriteScopeClaim {
                        owner: owner.into(),
                        roots: vec![root.into()],
                        exact_files: Vec::new(),
                        contracts: Vec::new(),
                    },
                    false,
                    |candidate| candidate == "live-builder",
                )
                .expect("non-overlapping claim registers");
        }
        assert_eq!(ledger.write_claims.len(), 3);

        let released = ledger
            .release_stale_claims(None, |candidate| candidate == "live-builder")
            .expect("release sweeps stale claims");
        assert_eq!(released, vec!["zombie-builder", "prior-session-builder"]);
        assert_eq!(ledger.write_claims.len(), 1);
        assert_eq!(ledger.write_claims[0].claim.owner, "live-builder");

        // A second sweep is idempotent and never touches a live claimant.
        let released = ledger
            .release_stale_claims(None, |candidate| candidate == "live-builder")
            .expect("idempotent sweep");
        assert!(released.is_empty());
    }

    #[test]
    fn release_stale_claims_refuses_to_remove_an_owner_filter_for_a_live_claimant() {
        let mut ledger = CoordinationLedger::default();
        ledger
            .register_claim(
                WriteScopeClaim {
                    owner: "live-builder".into(),
                    roots: vec!["src/live".into()],
                    exact_files: Vec::new(),
                    contracts: Vec::new(),
                },
                false,
                |candidate| candidate == "live-builder",
            )
            .expect("claim registers");
        let released = ledger
            .release_stale_claims(Some("live-builder"), |candidate| {
                candidate == "live-builder"
            })
            .expect("a live claimant is never released");
        assert!(released.is_empty());

        // A stale owner named in the filter IS released; other stale owners are not touched.
        ledger
            .register_claim(
                WriteScopeClaim {
                    owner: "zombie-builder".into(),
                    roots: vec!["src/zombie".into()],
                    exact_files: Vec::new(),
                    contracts: Vec::new(),
                },
                false,
                |candidate| candidate == "live-builder",
            )
            .expect("claim registers");
        let released = ledger
            .release_stale_claims(Some("zombie-builder"), |candidate| {
                candidate == "live-builder"
            })
            .expect("filtered release");
        assert_eq!(released, vec!["zombie-builder"]);
        assert_eq!(ledger.write_claims.len(), 1);
    }

    #[test]
    fn path_overlap_respects_component_boundaries_and_root_coverage() {
        let root = WriteScopeClaim {
            owner: "agent-a".into(),
            roots: vec!["src".into()],
            exact_files: vec![],
            contracts: vec![],
        };
        let sibling = WriteScopeClaim {
            owner: "agent-b".into(),
            roots: vec!["src2".into()],
            exact_files: vec![],
            contracts: vec![],
        };
        let child_file = WriteScopeClaim {
            owner: "agent-c".into(),
            roots: vec![],
            exact_files: vec!["src/lib.rs".into()],
            contracts: vec![],
        };
        assert!(!root.overlaps(&sibling));
        assert!(root.overlaps(&child_file));
    }

    #[test]
    fn disjoint_exact_files_share_a_root() {
        // #6278: N workers, one results directory, one disjoint file each.
        let a = WriteScopeClaim {
            owner: "agent-a".into(),
            roots: vec!["tmp/scan".into()],
            exact_files: vec!["tmp/scan/a.txt".into()],
            contracts: vec![],
        };
        let b = WriteScopeClaim {
            owner: "agent-b".into(),
            roots: vec!["tmp/scan".into()],
            exact_files: vec!["tmp/scan/b.txt".into()],
            contracts: vec![],
        };
        assert!(!a.overlaps(&b));
        assert!(!b.overlaps(&a));
        // The bound root is the file boundary: the shared tree and the
        // peer's file are outside this claim's authorized surface.
        assert!(a.contains_path("tmp/scan/a.txt"));
        assert!(!a.contains_path("tmp/scan/b.txt"));
        assert!(!a.contains_path("tmp/scan/scratch.txt"));
    }

    #[test]
    fn file_bound_roots_still_refuse_real_overlap() {
        let a = WriteScopeClaim {
            owner: "agent-a".into(),
            roots: vec!["tmp/scan".into()],
            exact_files: vec!["tmp/scan/a.txt".into()],
            contracts: vec![],
        };
        // The same file under the shared root still contends.
        let same_file = WriteScopeClaim {
            owner: "agent-b".into(),
            roots: vec!["tmp/scan".into()],
            exact_files: vec!["tmp/scan/a.txt".into()],
            contracts: vec![],
        };
        assert!(a.overlaps(&same_file));
        // An open root — no files declared beneath it — keeps tree-wide
        // authority and still contends with a file beneath it.
        let open_root = WriteScopeClaim {
            owner: "agent-c".into(),
            roots: vec!["tmp/scan".into()],
            exact_files: vec![],
            contracts: vec![],
        };
        assert!(a.overlaps(&open_root));
        // A root whose files sit elsewhere stays open: union claims are
        // unchanged for files outside the claimed trees.
        let mixed = WriteScopeClaim {
            owner: "agent-d".into(),
            roots: vec!["src".into()],
            exact_files: vec!["README.md".into()],
            contracts: vec![],
        };
        assert!(mixed.contains_path("src/lib.rs"));
        assert!(mixed.contains_path("README.md"));
    }

    #[test]
    fn disjoint_file_claims_under_one_root_both_register() {
        let mut ledger = CoordinationLedger::default();
        for (owner, file) in [("agent-a", "tmp/scan/a.txt"), ("agent-b", "tmp/scan/b.txt")] {
            ledger
                .register_claim(
                    WriteScopeClaim {
                        owner: owner.into(),
                        roots: vec!["tmp/scan".into()],
                        exact_files: vec![file.into()],
                        contracts: vec![],
                    },
                    false,
                    |_| true,
                )
                .expect("disjoint files under a shared root register");
        }
        assert_eq!(ledger.write_claims.len(), 2);
        assert!(ledger.contentions.is_empty());
    }

    #[test]
    fn legacy_blocked_contention_wire_defaults_to_unresolved() {
        let receipt: WriteContentionReceipt = serde_json::from_value(json!({
            "claimant": "agent-b",
            "conflicting_owner": "agent-a",
            "roots": ["src"],
            "exact_files": [],
            "contracts": ["public-api"],
            "disposition": "blocked_pending_isolation_or_serialization",
            "sequence": 3
        }))
        .expect("existing blocked wire receipt remains readable");

        assert_eq!(
            receipt.disposition,
            WriteContentionDisposition::BlockedPendingIsolationOrSerialization
        );
        assert_eq!(receipt.resolution_sequence, None);
        assert!(receipt.disposition.blocks_admission());
    }

    #[test]
    fn active_shared_claims_contend_but_isolated_claims_do_not() {
        let mut ledger = CoordinationLedger::default();
        let first = WriteScopeClaim {
            owner: "agent-a".into(),
            roots: vec!["src".into()],
            exact_files: vec![],
            contracts: vec!["public-api".into()],
        };
        ledger.register_claim(first, false, |_| false).unwrap();
        let second = WriteScopeClaim {
            owner: "agent-b".into(),
            roots: vec!["docs".into()],
            exact_files: vec![],
            contracts: vec!["public-api".into()],
        };
        let err = ledger
            .register_claim(second.clone(), false, |owner| owner == "agent-a")
            .unwrap_err();
        assert!(
            err.contains("contention") && err.contains("agent-a"),
            "{err}"
        );
        assert_eq!(ledger.contentions.len(), 1);
        assert_eq!(ledger.contentions[0].claimant, "agent-b");
        assert_eq!(ledger.contentions[0].conflicting_owner, "agent-a");
        assert_eq!(
            ledger.contentions[0].disposition,
            WriteContentionDisposition::BlockedPendingIsolationOrSerialization
        );
        assert_eq!(
            serde_json::to_value(&ledger.contentions[0]).unwrap()["disposition"],
            json!("blocked_pending_isolation_or_serialization")
        );
        let resolving_claim = ledger
            .register_claim(second, true, |owner| owner == "agent-a")
            .expect("isolated claim resolves the blocked admission");
        assert_eq!(
            ledger.contentions[0].disposition,
            WriteContentionDisposition::ResolvedBySuccessfulClaim
        );
        assert_eq!(
            ledger.contentions[0].resolution_sequence,
            Some(resolving_claim.sequence)
        );
    }

    #[test]
    fn active_write_claims_are_never_evicted_by_receipt_retention() {
        let mut ledger = CoordinationLedger::default();
        for index in 0..COORDINATION_RECORD_LIMIT {
            ledger
                .register_claim(
                    WriteScopeClaim {
                        owner: format!("agent-{index:03}"),
                        roots: vec![format!("pkg-{index:03}")],
                        exact_files: vec![],
                        contracts: vec![],
                    },
                    false,
                    |_| true,
                )
                .unwrap();
        }
        let error = ledger
            .register_claim(
                WriteScopeClaim {
                    owner: "agent-over-cap".into(),
                    roots: vec!["new-package".into()],
                    exact_files: vec![],
                    contracts: vec![],
                },
                false,
                |_| true,
            )
            .expect_err("all-active capacity must fail before evicting ownership");
        assert!(error.contains("active owners"), "{error}");
        assert_eq!(ledger.write_claims.len(), COORDINATION_RECORD_LIMIT);
        assert!(
            ledger
                .write_claims
                .iter()
                .any(|record| record.claim.owner == "agent-000")
        );
    }

    #[test]
    fn accepted_decisions_require_owner_and_explicit_neutral_reconciliation() {
        let mut ledger = CoordinationLedger::default();
        let make = |id: &str, owner: &str, status| DecisionRecord {
            decision_id: id.into(),
            subject: "storage".into(),
            status,
            owner: owner.into(),
            scope: vec!["router".into()],
            constraints: vec![],
            evidence_handles: vec![format!("receipt:{id}")],
            version: 1,
            sequence: 0,
        };
        ledger
            .record_decision(make("a", "agent-a", DecisionStatus::Accepted))
            .unwrap();
        ledger
            .record_decision(make("b", "agent-b", DecisionStatus::Proposed))
            .unwrap();
        let owner_error = ledger
            .update_decision_status("b", DecisionStatus::Accepted, "root", 2)
            .unwrap_err();
        assert!(owner_error.contains("owned by 'agent-b'"), "{owner_error}");
        let stale = ledger
            .update_decision_status("b", DecisionStatus::Accepted, "agent-b", 1)
            .unwrap_err();
        assert!(stale.contains("expected 1, current 2"), "{stale}");
        let conflict = ledger
            .update_decision_status("b", DecisionStatus::Accepted, "agent-b", 2)
            .unwrap_err();
        assert!(conflict.contains("neutral reconciliation"), "{conflict}");
        ledger
            .update_decision_status("a", DecisionStatus::Superseded, "agent-a", 1)
            .unwrap();
        ledger
            .update_decision_status("b", DecisionStatus::Accepted, "agent-b", 2)
            .unwrap();
        let receipt = ledger
            .reconcile(
                "storage".into(),
                "root".into(),
                vec!["a".into(), "b".into()],
                "use bounded origin-session artifacts".into(),
                vec!["test:coord".into()],
                vec!["branch:agent-a".into(), "branch:agent-b".into()],
                1,
                3,
                vec!["review:independent".into()],
                vec!["verify:locked".into()],
                "verified".into(),
            )
            .unwrap();
        assert_eq!(receipt.input_decisions.len(), 2);
        assert!(receipt.sequence > ledger.decisions[1].sequence);
    }

    #[test]
    fn relevant_decision_projection_is_deduplicated_bounded_and_receipted() {
        let mut ledger = CoordinationLedger::default();
        for (id, subject, scope) in [
            ("file", "file-contract", "path:src"),
            ("docs", "docs-contract", "path:docs"),
            ("api", "api-contract", "contract:public-api"),
        ] {
            ledger
                .record_decision(DecisionRecord {
                    decision_id: id.into(),
                    subject: subject.into(),
                    status: DecisionStatus::Accepted,
                    owner: "planner".into(),
                    scope: vec![scope.into()],
                    constraints: vec!["bounded".into(), "bounded".into()],
                    evidence_handles: vec![format!("receipt:{id}")],
                    version: 1,
                    sequence: 0,
                })
                .unwrap();
        }
        let claim = WriteScopeClaim {
            owner: "worker".into(),
            roots: vec!["src/tui".into()],
            exact_files: vec![],
            contracts: vec!["public-api".into()],
        };
        let (projection, receipt) =
            ledger.project_relevant_decisions("worker", Some(&claim), &["File".into()]);
        assert!(projection.contains("file-contract"), "{projection}");
        assert!(projection.contains("api-contract"), "{projection}");
        assert!(!projection.contains("docs-contract"), "{projection}");
        assert!(projection.len() <= COORDINATION_PROJECTION_BYTE_LIMIT);
        assert_eq!(receipt.decision_ids, vec!["api", "file"]);
        assert_eq!(receipt.deduplicated, 1);
        assert_eq!(ledger.projections.last(), Some(&receipt));
    }

    #[test]
    fn projection_receipt_distinguishes_unique_omissions_from_deduplication() {
        let mut ledger = CoordinationLedger::default();
        for index in 0..(COORDINATION_PROJECTION_DECISION_LIMIT + 2) {
            ledger
                .record_decision(DecisionRecord {
                    decision_id: format!("decision-{index}"),
                    subject: format!("subject-{index}"),
                    status: DecisionStatus::Accepted,
                    owner: "planner".into(),
                    scope: vec!["path:src".into()],
                    constraints: vec![format!("constraint-{index}")],
                    evidence_handles: vec![format!("receipt:{index}")],
                    version: 1,
                    sequence: 0,
                })
                .unwrap();
        }
        let claim = WriteScopeClaim {
            owner: "worker".into(),
            roots: vec!["src".into()],
            exact_files: vec![],
            contracts: vec![],
        };

        let (projection, receipt) = ledger.project_relevant_decisions("worker", Some(&claim), &[]);

        assert!(projection.len() <= COORDINATION_PROJECTION_BYTE_LIMIT);
        assert_eq!(
            receipt.decision_ids.len(),
            COORDINATION_PROJECTION_DECISION_LIMIT
        );
        assert_eq!(receipt.deduplicated, 0);
        assert_eq!(receipt.omitted, 2);
    }

    #[test]
    fn coordination_schema_drift_fails_closed_before_mutation() {
        let mut ledger = CoordinationLedger {
            schema_version: COORDINATION_SCHEMA_VERSION + 2,
            ..CoordinationLedger::default()
        };
        let error = ledger
            .register_claim(
                WriteScopeClaim {
                    owner: "worker".into(),
                    roots: vec!["src".into()],
                    exact_files: vec![],
                    contracts: vec![],
                },
                false,
                |_| false,
            )
            .unwrap_err();
        assert!(error.contains("unsupported coordination schema"), "{error}");
        assert!(ledger.write_claims.is_empty());
    }
}

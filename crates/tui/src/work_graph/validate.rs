//! Invariant validation — fail closed.
//!
//! [`validate`] checks every whole-snapshot invariant (V1–V8 below, plus
//! structural well-formedness). The reducer calls it on the candidate
//! snapshot after every change and rejects the change on any violation,
//! leaving the input snapshot untouched. There is no fail-open path: if a
//! node cannot be verified, it does not become Verified — verification
//! infrastructure trouble must surface as a rejection (callers then mark the
//! node Blocked), never as silently-assumed success.
//!
//! Invariants:
//! - V1  `DependsOn` edges are acyclic.
//! - V2  every live (`Initializing`/`Active`/`Waiting`) Operation reaches an
//!   Objective/PlanStep via `Contains` ancestry — no orphaned live work.
//! - V3  `binding.is_some()` ⇒ `kind == Operation`.
//! - V4  `Verified` ⇒ acceptance non-empty ⇒ a `Verifies`-edge evidence path
//!   satisfies every requirement. Completion is never verification.
//! - V5  `Blocked` ⇒ an incoming `Blocks` edge, an unmet `DependsOn`, or a
//!   pending `RequiresApproval` path exists.
//! - V6  each binding's `external` matches exactly one identity scheme and no
//!   two operations bind the same external identity.
//! - V7  `RuntimeRef`/`LaneRef` nodes never carry liveness state — the
//!   owning subsystems are the only liveness source.
//! - V8  history is bounded and its revisions strictly increase.
//! - V9  terminal states are never overwritten except via explicit
//!   `Supersede` (enforced in the reducer, which sees the predecessor
//!   snapshot; single-snapshot validation cannot observe overwrites).
//! - V10 compat projections are pure functions of the snapshot — enforced at
//!   the type level: projection functions take `&WorkGraphSnapshot` (see
//!   `compat.rs`); nothing hands them mutable graph access.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::ids::WorkNodeId;
use super::model::{
    ACTIVITY_CAP, EdgeKind, HISTORY_CAP, NodeKind, NodeState, SCHEMA_VERSION, WorkActivityEvent,
    WorkGraphSnapshot, WorkNode, external_identity_is_well_formed,
};

/// Which rule a violation belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationCode {
    /// Basic well-formedness (unique IDs, resolvable endpoints, schema).
    Structural,
    V1,
    V2,
    V3,
    V4,
    V5,
    V6,
    V7,
    V8,
    V9,
    /// Never emitted at runtime: enforced by projection function signatures.
    V10,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    pub code: ValidationCode,
    pub message: String,
}

/// Result of a failed validation. A change producing any violation is
/// rejected wholesale; the pre-change snapshot is returned to the caller
/// untouched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub violations: Vec<Violation>,
}

impl ValidationReport {
    #[must_use]
    pub fn single(code: ValidationCode, message: impl Into<String>) -> Self {
        ValidationReport {
            violations: vec![Violation {
                code,
                message: message.into(),
            }],
        }
    }

    #[must_use]
    pub fn contains_code(&self, code: ValidationCode) -> bool {
        self.violations.iter().any(|v| v.code == code)
    }
}

impl std::fmt::Display for ValidationReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "work graph validation failed:")?;
        for v in &self.violations {
            write!(f, " [{:?}] {};", v.code, v.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationReport {}

/// Validate a whole snapshot. `Ok(())` or every violation found.
pub fn validate(snapshot: &WorkGraphSnapshot) -> Result<(), ValidationReport> {
    let mut violations = Vec::new();

    check_structural(snapshot, &mut violations);
    check_v1_depends_on_acyclic(snapshot, &mut violations);
    check_v2_live_operations_rooted(snapshot, &mut violations);
    check_v3_binding_only_on_operations(snapshot, &mut violations);
    check_v4_verified_requires_evidence(snapshot, &mut violations);
    check_v5_blocked_has_cause(snapshot, &mut violations);
    check_v6_binding_identity(snapshot, &mut violations);
    check_v7_refs_inert(snapshot, &mut violations);
    check_v8_history_bounded_monotonic(snapshot, &mut violations);

    if violations.is_empty() {
        Ok(())
    } else {
        Err(ValidationReport { violations })
    }
}

fn check_structural(snapshot: &WorkGraphSnapshot, out: &mut Vec<Violation>) {
    if snapshot.schema != SCHEMA_VERSION {
        out.push(Violation {
            code: ValidationCode::Structural,
            message: format!("unknown schema {}", snapshot.schema),
        });
    }
    let mut node_ids = HashSet::new();
    for node in &snapshot.nodes {
        if !node_ids.insert(&node.id) {
            out.push(Violation {
                code: ValidationCode::Structural,
                message: format!("duplicate node id {}", node.id),
            });
        }
        if node.evidence.is_some() && !matches!(node.kind, NodeKind::Evidence) {
            out.push(Violation {
                code: ValidationCode::Structural,
                message: format!(
                    "node {} carries evidence but is not an Evidence node",
                    node.id
                ),
            });
        }
    }
    let mut edge_ids = HashSet::new();
    for edge in &snapshot.edges {
        if !edge_ids.insert(&edge.id) {
            out.push(Violation {
                code: ValidationCode::Structural,
                message: format!("duplicate edge id {}", edge.id),
            });
        }
        for endpoint in [&edge.from, &edge.to] {
            if !node_ids.contains(endpoint) {
                out.push(Violation {
                    code: ValidationCode::Structural,
                    message: format!("edge {} references missing node {}", edge.id, endpoint),
                });
            }
        }
    }

    let mut plan_ids = HashSet::new();
    for id in &snapshot.compat.plan_order {
        if !plan_ids.insert(id) {
            out.push(Violation {
                code: ValidationCode::Structural,
                message: format!("duplicate plan projection node {id}"),
            });
        }
        match snapshot.node(id) {
            Some(node) if matches!(node.kind, NodeKind::PlanStep) => {}
            Some(_) => out.push(Violation {
                code: ValidationCode::Structural,
                message: format!("plan projection node {id} is not a PlanStep"),
            }),
            None => out.push(Violation {
                code: ValidationCode::Structural,
                message: format!("plan projection references missing node {id}"),
            }),
        }
    }

    let mut todo_ids = HashSet::new();
    let mut active_todos = 0usize;
    for binding in &snapshot.compat.todos {
        if binding.legacy_id == 0 || !todo_ids.insert(binding.legacy_id) {
            out.push(Violation {
                code: ValidationCode::Structural,
                message: format!("invalid or duplicate legacy To-do id {}", binding.legacy_id),
            });
        }
        match snapshot.node(&binding.node) {
            Some(node) => {
                if node.kind != NodeKind::PlanStep {
                    out.push(Violation {
                        code: ValidationCode::Structural,
                        message: format!(
                            "To-do projection {} node {} is not a PlanStep",
                            binding.legacy_id, binding.node
                        ),
                    });
                }
                if matches!(node.state, NodeState::Active) {
                    active_todos += 1;
                }
            }
            None => out.push(Violation {
                code: ValidationCode::Structural,
                message: format!(
                    "To-do projection {} references missing node {}",
                    binding.legacy_id, binding.node
                ),
            }),
        }
        if let Some(index) = binding.plan_index {
            let aliased = usize::try_from(index)
                .ok()
                .and_then(|index| snapshot.compat.plan_order.get(index));
            if aliased != Some(&binding.node) {
                out.push(Violation {
                    code: ValidationCode::Structural,
                    message: format!(
                        "To-do projection {} has an invalid plan alias",
                        binding.legacy_id
                    ),
                });
            }
        }
    }
    if active_todos > 1 {
        out.push(Violation {
            code: ValidationCode::Structural,
            message: "legacy To-do projection has more than one active row".to_string(),
        });
    }

    if snapshot.activities.len() > ACTIVITY_CAP {
        out.push(Violation {
            code: ValidationCode::Structural,
            message: format!(
                "activity length {} exceeds bound {ACTIVITY_CAP}",
                snapshot.activities.len()
            ),
        });
    }
    for activity in snapshot.activities.iter() {
        let (requested, effective, provider_kind, provider, endpoint_identity, model, operation) =
            match activity {
                WorkActivityEvent::ReasoningEffortChanged {
                    requested,
                    effective,
                    provider_kind,
                    provider,
                    endpoint_identity,
                    model,
                    operation,
                    ..
                } => (
                    requested,
                    effective,
                    provider_kind,
                    provider,
                    endpoint_identity,
                    model,
                    operation,
                ),
            };
        if matches!(
            requested,
            codewhale_models::reasoning_effort_tier::ReasoningEffortTier::ThinkingEnabledGranularityUnavailable
                | codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Unavailable
        ) {
            out.push(Violation {
                code: ValidationCode::Structural,
                message: "requested reasoning effort is not an operator-selectable tier"
                    .to_string(),
            });
        }
        if provider.is_empty()
            || provider.chars().count() > 128
            || provider
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            out.push(Violation {
                code: ValidationCode::Structural,
                message: "activity provider is not a bounded route identity".to_string(),
            });
        }
        let provenance_is_bounded = provider_kind.is_some()
            && endpoint_identity.as_ref().is_some_and(|endpoint| {
                !endpoint.is_empty()
                    && endpoint.chars().count() <= 512
                    && !endpoint.chars().any(char::is_control)
            })
            && model.as_ref().is_some_and(|model| {
                !model.trim().is_empty()
                    && model.chars().count() <= 256
                    && !model.chars().any(char::is_control)
            });
        if !provenance_is_bounded {
            if *effective
                != codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Unavailable
            {
                out.push(Violation {
                    code: ValidationCode::Structural,
                    message:
                        "activity without bounded route provenance must be effective unavailable"
                            .to_string(),
                });
            }
        } else {
            let api_provider = provider_kind.expect("provenance bounded above");
            if api_provider != codewhale_config::ProviderKind::Custom
                && provider != api_provider.as_str()
            {
                out.push(Violation {
                    code: ValidationCode::Structural,
                    message: "activity provider identity does not match its recorded kind"
                        .to_string(),
                });
                continue;
            }
            let constrained = match api_provider {
                codewhale_config::ProviderKind::Custom => {
                    Some(codewhale_models::reasoning_effort_tier::ReasoningEffortTier::Unavailable)
                }
                api_provider => super::model::constrained_effective_reasoning_for_route(
                    *requested,
                    api_provider,
                    endpoint_identity.as_deref().expect("bounded above"),
                    model.as_deref().expect("bounded above"),
                ),
            };
            if constrained.is_some_and(|expected| *effective != expected) {
                out.push(Violation {
                    code: ValidationCode::Structural,
                    message: "activity effective reasoning is impossible for its recorded route"
                        .to_string(),
                });
            } else if constrained.is_none()
                && matches!(
                    effective,
                    codewhale_models::reasoning_effort_tier::ReasoningEffortTier::ThinkingEnabledGranularityUnavailable
                )
            {
                out.push(Violation {
                    code: ValidationCode::Structural,
                    message: "granularity-unavailable receipt is not valid for this recorded route"
                        .to_string(),
                });
            }
        }
        if let Some(operation) = operation {
            match snapshot.node(operation) {
                Some(node) if node.kind == NodeKind::Operation => {}
                Some(_) => out.push(Violation {
                    code: ValidationCode::Structural,
                    message: format!("activity operation {operation} is not an Operation node"),
                }),
                None => out.push(Violation {
                    code: ValidationCode::Structural,
                    message: format!("activity references missing operation {operation}"),
                }),
            }
        }
    }
}

/// V1: DFS three-color cycle detection over `DependsOn` edges.
fn check_v1_depends_on_acyclic(snapshot: &WorkGraphSnapshot, out: &mut Vec<Violation>) {
    let mut adjacency: HashMap<&WorkNodeId, Vec<&WorkNodeId>> = HashMap::new();
    for edge in &snapshot.edges {
        if matches!(edge.kind, EdgeKind::DependsOn) {
            adjacency.entry(&edge.from).or_default().push(&edge.to);
        }
    }
    let mut done: HashSet<&WorkNodeId> = HashSet::new();
    let mut in_progress: HashSet<&WorkNodeId> = HashSet::new();

    fn visit<'a>(
        node: &'a WorkNodeId,
        adjacency: &HashMap<&'a WorkNodeId, Vec<&'a WorkNodeId>>,
        done: &mut HashSet<&'a WorkNodeId>,
        in_progress: &mut HashSet<&'a WorkNodeId>,
    ) -> bool {
        if done.contains(node) {
            return true;
        }
        if !in_progress.insert(node) {
            return false; // back edge → cycle
        }
        let acyclic = adjacency
            .get(node)
            .map(|next| next.iter().all(|n| visit(n, adjacency, done, in_progress)))
            .unwrap_or(true);
        in_progress.remove(node);
        done.insert(node);
        acyclic
    }

    for node in &snapshot.nodes {
        if !visit(&node.id, &adjacency, &mut done, &mut in_progress) {
            out.push(Violation {
                code: ValidationCode::V1,
                message: format!("depends_on cycle reachable from node {}", node.id),
            });
            return; // one report is enough; graph is already invalid
        }
    }
}

/// V2: every live Operation climbs `Contains` ancestry to an
/// Objective/PlanStep. `Contains` points parent → child, so we walk incoming
/// edges upward with a visited set (defensive against malformed cycles).
fn check_v2_live_operations_rooted(snapshot: &WorkGraphSnapshot, out: &mut Vec<Violation>) {
    for node in &snapshot.nodes {
        if !(matches!(node.kind, NodeKind::Operation) && node.state.is_live()) {
            continue;
        }
        let mut visited: HashSet<&WorkNodeId> = HashSet::new();
        let mut frontier: Vec<&WorkNodeId> = vec![&node.id];
        let mut rooted = false;
        while let Some(current) = frontier.pop() {
            if !visited.insert(current) {
                continue;
            }
            for edge in &snapshot.edges {
                if matches!(edge.kind, EdgeKind::Contains)
                    && &edge.to == current
                    && let Some(parent) = snapshot.node(&edge.from)
                {
                    if matches!(parent.kind, NodeKind::Objective | NodeKind::PlanStep) {
                        rooted = true;
                    }
                    frontier.push(&parent.id);
                }
            }
            if rooted {
                break;
            }
        }
        if !rooted {
            out.push(Violation {
                code: ValidationCode::V2,
                message: format!(
                    "live operation {} has no Objective/PlanStep ancestry",
                    node.id
                ),
            });
        }
    }
}

fn check_v3_binding_only_on_operations(snapshot: &WorkGraphSnapshot, out: &mut Vec<Violation>) {
    for node in &snapshot.nodes {
        if node.binding.is_some() && !matches!(node.kind, NodeKind::Operation) {
            out.push(Violation {
                code: ValidationCode::V3,
                message: format!("non-operation node {} carries a binding", node.id),
            });
        }
    }
}

/// V4: `Verified` demands non-empty acceptance and, for every requirement, at
/// least one Evidence node linked by a `Verifies` edge whose payload
/// satisfies it. There is no fail-open branch: absence of satisfying
/// evidence — for any reason, including verification infrastructure being
/// unavailable — is a rejection.
fn check_v4_verified_requires_evidence(snapshot: &WorkGraphSnapshot, out: &mut Vec<Violation>) {
    for node in &snapshot.nodes {
        if !matches!(node.state, NodeState::Verified) {
            continue;
        }
        if node.acceptance.is_empty() {
            out.push(Violation {
                code: ValidationCode::V4,
                message: format!("verified node {} has no acceptance requirements", node.id),
            });
            continue;
        }
        let evidence: Vec<&WorkNode> = snapshot
            .edges
            .iter()
            .filter(|e| matches!(e.kind, EdgeKind::Verifies) && e.to == node.id)
            .filter_map(|e| snapshot.node(&e.from))
            .filter(|n| matches!(n.kind, NodeKind::Evidence))
            .collect();
        for requirement in &node.acceptance {
            let satisfied = evidence.iter().any(|ev| {
                ev.evidence
                    .as_ref()
                    .is_some_and(|payload| requirement.is_satisfied_by(payload))
            });
            if !satisfied {
                out.push(Violation {
                    code: ValidationCode::V4,
                    message: format!(
                        "verified node {} lacks satisfying evidence for {:?}",
                        node.id, requirement
                    ),
                });
            }
        }
    }
}

/// V5: `Blocked` must have a visible cause.
fn check_v5_blocked_has_cause(snapshot: &WorkGraphSnapshot, out: &mut Vec<Violation>) {
    for node in &snapshot.nodes {
        if !matches!(node.state, NodeState::Blocked) {
            continue;
        }
        let blocked_by_edge = snapshot
            .edges
            .iter()
            .any(|e| matches!(e.kind, EdgeKind::Blocks) && e.to == node.id);
        let unmet_dependency = snapshot.edges.iter().any(|e| {
            matches!(e.kind, EdgeKind::DependsOn)
                && e.from == node.id
                && snapshot
                    .node(&e.to)
                    .is_some_and(|dep| !WorkGraphSnapshot::node_is_done(dep))
        });
        let pending_approval = snapshot.edges.iter().any(|e| {
            matches!(e.kind, EdgeKind::RequiresApproval)
                && e.from == node.id
                && snapshot
                    .node(&e.to)
                    .is_some_and(|approval| !WorkGraphSnapshot::node_is_done(approval))
        });
        if !(blocked_by_edge || unmet_dependency || pending_approval) {
            out.push(Violation {
                code: ValidationCode::V5,
                message: format!("blocked node {} has no blocking cause", node.id),
            });
        }
    }
}

/// V6: binding externals are well-formed under exactly one scheme prefix and
/// unique across operations. (Cross-checking against the owners' live
/// registries is the liveness slice's job; within the snapshot this is the
/// enforceable core.)
fn check_v6_binding_identity(snapshot: &WorkGraphSnapshot, out: &mut Vec<Violation>) {
    let mut seen: HashMap<&str, &WorkNodeId> = HashMap::new();
    for node in &snapshot.nodes {
        let Some(binding) = &node.binding else {
            continue;
        };
        if !external_identity_is_well_formed(&binding.external) {
            out.push(Violation {
                code: ValidationCode::V6,
                message: format!(
                    "node {} binding external {:?} matches no identity scheme",
                    node.id, binding.external
                ),
            });
        }
        if let Some(previous) = seen.insert(binding.external.as_str(), &node.id) {
            out.push(Violation {
                code: ValidationCode::V6,
                message: format!(
                    "external {:?} bound by both {} and {}",
                    binding.external, previous, node.id
                ),
            });
        }
    }
}

/// V7: reference nodes are inert — they never carry liveness state, because
/// the owning subsystems are the only source of liveness truth.
fn check_v7_refs_inert(snapshot: &WorkGraphSnapshot, out: &mut Vec<Violation>) {
    for node in &snapshot.nodes {
        if matches!(node.kind, NodeKind::RuntimeRef | NodeKind::LaneRef)
            && !matches!(node.state, NodeState::Ready)
        {
            out.push(Violation {
                code: ValidationCode::V7,
                message: format!(
                    "reference node {} carries liveness state {:?}",
                    node.id, node.state
                ),
            });
        }
    }
}

/// V8: bounded history with strictly increasing revisions. (The exactly-once
/// revision increment itself is a reducer property, covered by tests.)
fn check_v8_history_bounded_monotonic(snapshot: &WorkGraphSnapshot, out: &mut Vec<Violation>) {
    if snapshot.history.len() > HISTORY_CAP {
        out.push(Violation {
            code: ValidationCode::V8,
            message: format!(
                "history length {} exceeds bound {HISTORY_CAP}",
                snapshot.history.len()
            ),
        });
    }
    let mut previous: Option<u64> = None;
    for receipt in snapshot.history.iter() {
        if let Some(prev) = previous
            && receipt.revision <= prev
        {
            out.push(Violation {
                code: ValidationCode::V8,
                message: format!(
                    "history revisions not strictly increasing ({} then {})",
                    prev, receipt.revision
                ),
            });
            break;
        }
        previous = Some(receipt.revision);
    }
    if let Some(last) = snapshot.history.last() {
        // During apply, validation runs before the increment, so the newest
        // receipt may equal the current revision but never exceed it by >1.
        if last.revision > snapshot.revision.saturating_add(1) {
            out.push(Violation {
                code: ValidationCode::V8,
                message: format!(
                    "history revision {} ahead of snapshot revision {}",
                    last.revision, snapshot.revision
                ),
            });
        }
    }
}

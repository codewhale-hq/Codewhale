//! Work-graph unit tests: one test per invariant V1–V10, plus the reducer
//! contract (determinism, idempotency dedup, fail-closed rejection, bounded
//! history) and serde round-trip.

use super::compat;
use super::*;
use codewhale_models::reasoning_effort_tier::ReasoningEffortTier;

const SESSION: &str = "sess-test";

fn nid(disc: &str) -> WorkNodeId {
    WorkNodeId::derive(SESSION, disc)
}

fn eid(disc: &str) -> WorkEdgeId {
    WorkEdgeId::derive(SESSION, disc)
}

fn ctx(now: Ts) -> ChangeCtx {
    ChangeCtx {
        session_id: SESSION.to_string(),
        now,
        idempotency_key: None,
    }
}

fn ctx_keyed(now: Ts, seq: u64) -> ChangeCtx {
    ChangeCtx {
        session_id: SESSION.to_string(),
        now,
        idempotency_key: Some(IdempotencyKey {
            binding: BindingId::derive(SESSION, "binding:op"),
            seq,
        }),
    }
}

fn mk_node(disc: &str, kind: NodeKind, state: NodeState, now: Ts) -> WorkNode {
    WorkNode {
        id: nid(disc),
        kind,
        title: disc.to_string(),
        state,
        acceptance: Vec::new(),
        binding: None,
        evidence: None,
        provenance: Provenance::Import {
            source_digest: "digest".to_string(),
            ordinal: None,
        },
        created_at: now,
        updated_at: now,
    }
}

fn mk_edge(disc: &str, kind: EdgeKind, from: &str, to: &str) -> WorkEdge {
    WorkEdge {
        id: eid(disc),
        kind,
        from: nid(from),
        to: nid(to),
    }
}

fn effort_activity(
    provider: impl Into<String>,
    ts: Ts,
    operation: Option<WorkNodeId>,
) -> WorkActivityEvent {
    let provider = provider.into();
    WorkActivityEvent::ReasoningEffortChanged {
        requested: ReasoningEffortTier::Low,
        effective: ReasoningEffortTier::High,
        provider_kind: Some(
            codewhale_config::ProviderKind::parse(&provider)
                .unwrap_or(codewhale_config::ProviderKind::Custom),
        ),
        provider,
        endpoint_identity: Some("https://example.invalid/v1".to_string()),
        model: Some("test-model".to_string()),
        ts,
        operation,
    }
}

fn must(graph: &mut WorkGraph, change: WorkGraphChange, now: Ts) -> ChangeReceipt {
    graph.apply(change, ctx(now)).expect("change should apply")
}

/// Objective ─Contains→ PlanStep ─Contains→ Operation (all Ready).
fn seeded() -> WorkGraph {
    let mut graph = WorkGraph::new();
    must(
        &mut graph,
        WorkGraphChange::AddNode {
            node: mk_node("root", NodeKind::Objective, NodeState::Ready, 1),
        },
        1,
    );
    must(
        &mut graph,
        WorkGraphChange::AddNode {
            node: mk_node("step", NodeKind::PlanStep, NodeState::Ready, 2),
        },
        2,
    );
    must(
        &mut graph,
        WorkGraphChange::AddEdge {
            edge: mk_edge("c:root:step", EdgeKind::Contains, "root", "step"),
        },
        3,
    );
    must(
        &mut graph,
        WorkGraphChange::AddNode {
            node: mk_node("op", NodeKind::Operation, NodeState::Ready, 4),
        },
        4,
    );
    must(
        &mut graph,
        WorkGraphChange::AddEdge {
            edge: mk_edge("c:step:op", EdgeKind::Contains, "step", "op"),
        },
        5,
    );
    graph
}

fn set_state(graph: &mut WorkGraph, disc: &str, state: NodeState, now: Ts) {
    must(
        graph,
        WorkGraphChange::UpdateNode {
            id: nid(disc),
            patch: WorkNodePatch {
                state: Some(state),
                ..WorkNodePatch::default()
            },
        },
        now,
    );
}

fn expect_code(
    result: Result<ChangeReceipt, ValidationReport>,
    code: ValidationCode,
) -> ValidationReport {
    let report = result.expect_err("change should be rejected");
    assert!(report.contains_code(code), "expected {code:?} in {report}",);
    report
}

#[test]
fn v1_depends_on_cycles_rejected() {
    let mut graph = seeded();
    must(
        &mut graph,
        WorkGraphChange::AddNode {
            node: mk_node("step2", NodeKind::PlanStep, NodeState::Ready, 6),
        },
        6,
    );
    must(
        &mut graph,
        WorkGraphChange::AddEdge {
            edge: mk_edge("d:step:step2", EdgeKind::DependsOn, "step", "step2"),
        },
        7,
    );
    let result = graph.apply(
        WorkGraphChange::AddEdge {
            edge: mk_edge("d:step2:step", EdgeKind::DependsOn, "step2", "step"),
        },
        ctx(8),
    );
    expect_code(result, ValidationCode::V1);
}

#[test]
fn v2_live_operation_requires_plan_ancestry() {
    let mut graph = seeded();
    // Attached operation may go live.
    set_state(&mut graph, "op", NodeState::Active, 6);

    // An orphaned live operation is rejected outright.
    let result = graph.apply(
        WorkGraphChange::AddNode {
            node: mk_node("orphan-op", NodeKind::Operation, NodeState::Active, 7),
        },
        ctx(7),
    );
    expect_code(result, ValidationCode::V2);
}

#[test]
fn v3_binding_only_on_operation_nodes() {
    let mut graph = seeded();
    let result = graph.apply(
        WorkGraphChange::BindOperation {
            node: nid("step"),
            binding: OperationBinding {
                external: "task:task_0123456789abcdef".to_string(),
                durable: true,
                last_observation: None,
            },
        },
        ctx(6),
    );
    expect_code(result, ValidationCode::V3);
}

#[test]
fn v4_verified_requires_satisfying_evidence() {
    let mut graph = seeded();

    // Verified with empty acceptance: rejected.
    let result = graph.apply(
        WorkGraphChange::UpdateNode {
            id: nid("op"),
            patch: WorkNodePatch {
                state: Some(NodeState::Verified),
                ..WorkNodePatch::default()
            },
        },
        ctx(6),
    );
    expect_code(result, ValidationCode::V4);

    // Give the node an acceptance requirement; Verified without evidence is
    // still rejected — cannot-verify never fails open into done.
    must(
        &mut graph,
        WorkGraphChange::UpdateNode {
            id: nid("op"),
            patch: WorkNodePatch {
                acceptance: Some(vec![AcceptanceRequirement::EvidenceOfKind {
                    kind: EvidenceKindTag::TestSummary,
                }]),
                ..WorkNodePatch::default()
            },
        },
        7,
    );
    let result = graph.apply(
        WorkGraphChange::UpdateNode {
            id: nid("op"),
            patch: WorkNodePatch {
                state: Some(NodeState::Verified),
                ..WorkNodePatch::default()
            },
        },
        ctx(8),
    );
    expect_code(result, ValidationCode::V4);
    // The failed attempt changed nothing; the caller's recourse is Blocked,
    // never silently-verified.
    assert_eq!(
        graph.snapshot().node(&nid("op")).unwrap().state,
        NodeState::Ready
    );

    // Wrong-kind evidence does not satisfy the requirement.
    must(
        &mut graph,
        WorkGraphChange::AttachEvidence {
            node: nid("op"),
            evidence: EvidenceRef::new(EvidenceKind::ToolRun, "tool-call:1", None, false).unwrap(),
        },
        9,
    );
    let result = graph.apply(
        WorkGraphChange::UpdateNode {
            id: nid("op"),
            patch: WorkNodePatch {
                state: Some(NodeState::Verified),
                ..WorkNodePatch::default()
            },
        },
        ctx(10),
    );
    expect_code(result, ValidationCode::V4);

    // Satisfying evidence unlocks Verified.
    must(
        &mut graph,
        WorkGraphChange::AttachEvidence {
            node: nid("op"),
            evidence: EvidenceRef::new(EvidenceKind::TestSummary, "test-run:42", Some(2048), true)
                .unwrap(),
        },
        11,
    );
    set_state(&mut graph, "op", NodeState::Verified, 12);
    assert_eq!(
        graph.snapshot().node(&nid("op")).unwrap().state,
        NodeState::Verified
    );
}

#[test]
fn v5_blocked_requires_visible_cause() {
    let mut graph = seeded();
    let result = graph.apply(
        WorkGraphChange::UpdateNode {
            id: nid("op"),
            patch: WorkNodePatch {
                state: Some(NodeState::Blocked),
                ..WorkNodePatch::default()
            },
        },
        ctx(6),
    );
    expect_code(result, ValidationCode::V5);

    must(
        &mut graph,
        WorkGraphChange::AddNode {
            node: mk_node("blocker", NodeKind::Blocker, NodeState::Ready, 7),
        },
        7,
    );
    must(
        &mut graph,
        WorkGraphChange::AddEdge {
            edge: mk_edge("b:blocker:op", EdgeKind::Blocks, "blocker", "op"),
        },
        8,
    );
    set_state(&mut graph, "op", NodeState::Blocked, 9);
    assert_eq!(
        graph.snapshot().node(&nid("op")).unwrap().state,
        NodeState::Blocked
    );
}

#[test]
fn v6_binding_external_identity() {
    let mut graph = seeded();

    // Unknown scheme: rejected.
    let result = graph.apply(
        WorkGraphChange::BindOperation {
            node: nid("op"),
            binding: OperationBinding {
                external: "mystery:xyz".to_string(),
                durable: true,
                last_observation: None,
            },
        },
        ctx(6),
    );
    expect_code(result, ValidationCode::V6);

    // Well-formed externals bind; fleet's two-part form parses too.
    assert!(external_identity_is_well_formed("fleet:run_1/task_2"));
    assert!(!external_identity_is_well_formed("fleet:run-only"));
    must(
        &mut graph,
        WorkGraphChange::BindOperation {
            node: nid("op"),
            binding: OperationBinding {
                external: "shell:shell_ab12cd34".to_string(),
                durable: false,
                last_observation: None,
            },
        },
        7,
    );

    // A second operation cannot claim the same external identity.
    must(
        &mut graph,
        WorkGraphChange::AddNode {
            node: mk_node("op2", NodeKind::Operation, NodeState::Ready, 8),
        },
        8,
    );
    let result = graph.apply(
        WorkGraphChange::BindOperation {
            node: nid("op2"),
            binding: OperationBinding {
                external: "shell:shell_ab12cd34".to_string(),
                durable: false,
                last_observation: None,
            },
        },
        ctx(9),
    );
    expect_code(result, ValidationCode::V6);
}

#[test]
fn v7_reference_nodes_stay_inert() {
    let mut graph = seeded();
    let result = graph.apply(
        WorkGraphChange::AddNode {
            node: mk_node("rt", NodeKind::RuntimeRef, NodeState::Active, 6),
        },
        ctx(6),
    );
    expect_code(result, ValidationCode::V7);

    // Inert reference nodes are fine.
    must(
        &mut graph,
        WorkGraphChange::AddNode {
            node: mk_node("lane", NodeKind::LaneRef, NodeState::Ready, 7),
        },
        7,
    );
}

#[test]
fn v8_revision_and_history_monotonic() {
    // Applied changes increment the revision exactly once each and receipts
    // land in order.
    let graph = seeded();
    assert_eq!(graph.snapshot().revision, 5);
    let revisions: Vec<u64> = graph
        .snapshot()
        .history
        .iter()
        .map(|r| r.revision)
        .collect();
    assert_eq!(revisions, vec![1, 2, 3, 4, 5]);

    // A snapshot whose history revisions are out of order fails validation.
    let mut bad = graph.snapshot().clone();
    let mut receipt = bad.history.last().unwrap().clone();
    receipt.revision = 2; // duplicate/regressing revision
    bad.history.push_bounded(receipt);
    let report = validate(&bad).expect_err("out-of-order history must fail");
    assert!(report.contains_code(ValidationCode::V8));
}

#[test]
fn v9_terminal_states_only_move_via_supersede() {
    let mut graph = seeded();
    set_state(&mut graph, "op", NodeState::Cancelled, 6);

    // Patching a terminal node's state is rejected.
    let result = graph.apply(
        WorkGraphChange::UpdateNode {
            id: nid("op"),
            patch: WorkNodePatch {
                state: Some(NodeState::Active),
                ..WorkNodePatch::default()
            },
        },
        ctx(7),
    );
    expect_code(result, ValidationCode::V9);

    // ...but re-asserting the state it already holds is a no-op, not an error.
    // `work_update` replaces the whole list on every call, so a cancelled item
    // is re-sent as cancelled every time; rejecting that made the tool unusable
    // once anything was cancelled.
    let terminal_state = graph.snapshot().node(&nid("op")).expect("op node").state;
    let replay = graph.apply(
        WorkGraphChange::UpdateNode {
            id: nid("op"),
            patch: WorkNodePatch {
                state: Some(terminal_state),
                ..WorkNodePatch::default()
            },
        },
        ctx(8),
    );
    assert!(
        replay.is_ok(),
        "re-sending a terminal node's own state must be accepted: {replay:?}"
    );

    // Explicit Supersede is the sanctioned path.
    must(
        &mut graph,
        WorkGraphChange::AddNode {
            node: mk_node("op-new", NodeKind::Operation, NodeState::Ready, 8),
        },
        8,
    );
    must(
        &mut graph,
        WorkGraphChange::Supersede {
            old: nid("op"),
            replacement: nid("op-new"),
        },
        9,
    );
    let snapshot = graph.snapshot();
    assert_eq!(
        snapshot.node(&nid("op")).unwrap().state,
        NodeState::Superseded
    );
    assert!(snapshot.edges.iter().any(|e| {
        matches!(e.kind, EdgeKind::Supersedes) && e.from == nid("op-new") && e.to == nid("op")
    }));
}

#[test]
fn v10_projections_are_pure_functions_of_snapshot() {
    // Type-level enforcement: projections coerce to plain fns over an
    // immutable snapshot reference — no mutable access, no ambient state.
    let _plan: fn(&WorkGraphSnapshot) -> compat::PlanProjection = compat::project_plan;
    let _todos: fn(&WorkGraphSnapshot) -> compat::TodoProjection = compat::project_todos;
}

#[test]
fn compat_todo_binding_rejects_non_plan_step_node() {
    let mut bad = seeded().snapshot().clone();
    bad.compat.todos.push(CompatTodoBinding {
        legacy_id: 1,
        node: nid("root"),
        plan_index: None,
    });

    let report = validate(&bad).expect_err("compat To-do rows must bind to PlanStep nodes");
    assert!(report.contains_code(ValidationCode::Structural));
}

/// A representative change sequence exercising nodes, edges, bindings,
/// reconciliation, evidence, proposals, and supersede.
fn scripted_run() -> WorkGraph {
    let mut graph = seeded();
    must(
        &mut graph,
        WorkGraphChange::BindOperation {
            node: nid("op"),
            binding: OperationBinding {
                external: "task:task_0123456789abcdef".to_string(),
                durable: true,
                last_observation: None,
            },
        },
        6,
    );
    graph
        .apply(
            WorkGraphChange::ReconcileOperation {
                node: nid("op"),
                obs: OperationObservation::OwnerReported {
                    state: OwnerState::Running,
                    seq: 1,
                    at: 7,
                    output: None,
                },
            },
            ctx_keyed(7, 1),
        )
        .expect("reconcile applies");
    must(
        &mut graph,
        WorkGraphChange::UpdateNode {
            id: nid("op"),
            patch: WorkNodePatch {
                acceptance: Some(vec![AcceptanceRequirement::EvidenceOfKind {
                    kind: EvidenceKindTag::ToolRun,
                }]),
                ..WorkNodePatch::default()
            },
        },
        8,
    );
    graph
        .apply(
            WorkGraphChange::ReconcileOperation {
                node: nid("op"),
                obs: OperationObservation::OwnerReported {
                    state: OwnerState::Completed,
                    seq: 2,
                    at: 9,
                    output: None,
                },
            },
            ctx_keyed(9, 2),
        )
        .expect("reconcile applies");
    must(
        &mut graph,
        WorkGraphChange::AttachEvidence {
            node: nid("op"),
            evidence: EvidenceRef::new(EvidenceKind::ToolRun, "tool-call:99", Some(512), false)
                .unwrap(),
        },
        10,
    );
    set_state(&mut graph, "op", NodeState::Verified, 11);

    let proposal = WorkGraphProposal {
        id: ProposalId::derive(SESSION, "proposal:1"),
        added_nodes: vec![mk_node("step2", NodeKind::PlanStep, NodeState::Ready, 12)],
        added_edges: vec![mk_edge("c:root:step2", EdgeKind::Contains, "root", "step2")],
        updated_nodes: vec![ProposedNodeUpdate {
            id: nid("step"),
            patch: WorkNodePatch {
                title: Some("step (revised)".to_string()),
                ..WorkNodePatch::default()
            },
        }],
        removed_nodes: Vec::new(),
        removed_edges: Vec::new(),
        replacement_compat: None,
    };
    must(
        &mut graph,
        WorkGraphChange::ProposePlanDiff { proposal },
        12,
    );
    must(
        &mut graph,
        WorkGraphChange::AcceptPlanDiff {
            proposal_id: ProposalId::derive(SESSION, "proposal:1"),
            approval: ApprovalRef {
                reference: "user-review:1".to_string(),
            },
        },
        13,
    );
    graph
}

#[test]
fn reducer_is_deterministic_including_ids() {
    let first = scripted_run();
    let second = scripted_run();
    assert_eq!(first.snapshot(), second.snapshot());
    // Byte-identical serialization: same IDs, same ordering, same receipts.
    let a = serde_json::to_string(first.snapshot()).unwrap();
    let b = serde_json::to_string(second.snapshot()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn idempotency_key_dedup_is_a_no_op() {
    let mut graph = scripted_run();
    let before = graph.snapshot().clone();

    // Same (binding, seq) as an already-applied observation: acknowledged,
    // nothing changes — not revision, not history, not node state.
    let receipt = graph
        .apply(
            WorkGraphChange::ReconcileOperation {
                node: nid("op"),
                obs: OperationObservation::OwnerReported {
                    state: OwnerState::Failed,
                    seq: 2,
                    at: 99,
                    output: None,
                },
            },
            ctx_keyed(99, 2),
        )
        .expect("duplicate is acknowledged, not errored");
    assert!(receipt.no_op);
    assert_eq!(receipt.revision, before.revision);
    assert_eq!(graph.snapshot(), &before);
}

#[test]
fn rejected_change_leaves_snapshot_untouched() {
    let mut graph = seeded();
    let before = graph.snapshot().clone();
    let result = graph.apply(
        WorkGraphChange::UpdateNode {
            id: nid("op"),
            patch: WorkNodePatch {
                state: Some(NodeState::Verified), // V4: no acceptance, no evidence
                ..WorkNodePatch::default()
            },
        },
        ctx(6),
    );
    expect_code(result, ValidationCode::V4);
    assert_eq!(graph.snapshot(), &before);

    // Same fail-closed contract on the pure entry point.
    let report = apply(
        &before,
        WorkGraphChange::RemoveEdge {
            id: eid("does-not-exist"),
        },
        ctx(7),
    )
    .expect_err("missing edge is rejected");
    assert!(report.contains_code(ValidationCode::Structural));
}

#[test]
fn history_stays_bounded_at_cap() {
    let mut graph = seeded();
    let seeded_revision = graph.snapshot().revision;
    let extra = HISTORY_CAP as u64 + 44;
    for i in 0..extra {
        must(
            &mut graph,
            WorkGraphChange::UpdateNode {
                id: nid("step"),
                patch: WorkNodePatch {
                    title: Some(format!("step v{i}")),
                    ..WorkNodePatch::default()
                },
            },
            100 + i as Ts,
        );
    }
    let snapshot = graph.snapshot();
    assert_eq!(snapshot.revision, seeded_revision + extra);
    assert_eq!(snapshot.history.len(), HISTORY_CAP);
    // Oldest receipts were evicted FIFO; the window ends at the current
    // revision and starts exactly HISTORY_CAP entries back.
    let first = snapshot.history.iter().next().unwrap().revision;
    let last = snapshot.history.last().unwrap().revision;
    assert_eq!(last, snapshot.revision);
    assert_eq!(first, snapshot.revision - (HISTORY_CAP as u64 - 1));
}

#[test]
fn activity_receipts_reject_bad_provider_identity_and_oversized_snapshots() {
    let mut graph = seeded();
    for provider in [
        String::new(),
        "bad route".to_string(),
        "bad\nroute".to_string(),
        "x".repeat(129),
    ] {
        let before = graph.snapshot().clone();
        let result = graph.apply(
            WorkGraphChange::RecordActivity {
                event: effort_activity(provider, 10, None),
            },
            ctx(10),
        );
        expect_code(result, ValidationCode::Structural);
        assert_eq!(graph.snapshot(), &before, "rejection must be atomic");
    }

    for offset in 0..(ACTIVITY_CAP + 8) {
        let ts = 100 + offset as Ts;
        must(
            &mut graph,
            WorkGraphChange::RecordActivity {
                event: effort_activity("xiaomi-mimo", ts, None),
            },
            ts,
        );
    }
    assert_eq!(graph.snapshot().activities.len(), ACTIVITY_CAP);
    assert_eq!(
        graph.snapshot().activities.iter().next(),
        Some(&effort_activity("xiaomi-mimo", 108, None)),
        "bounded writes evict the oldest receipt"
    );

    let mut wire = serde_json::to_value(graph.snapshot()).expect("serialize graph");
    let activities = wire["activities"].as_array_mut().expect("activity array");
    activities.push(activities.last().expect("bounded receipt").clone());
    let oversized: WorkGraphSnapshot =
        serde_json::from_value(wire).expect("deserialize untrusted oversized snapshot");
    let report = validate(&oversized).expect_err("oversized activity must fail closed");
    assert!(report.contains_code(ValidationCode::Structural));
}

#[test]
fn effective_only_reasoning_states_cannot_be_requested() {
    for requested in [
        ReasoningEffortTier::ThinkingEnabledGranularityUnavailable,
        ReasoningEffortTier::Unavailable,
    ] {
        let mut graph = seeded();
        let before = graph.snapshot().clone();
        let result = graph.apply(
            WorkGraphChange::RecordActivity {
                event: WorkActivityEvent::ReasoningEffortChanged {
                    requested,
                    effective: requested,
                    provider_kind: Some(codewhale_config::ProviderKind::Zai),
                    provider: "zai".to_string(),
                    endpoint_identity: Some(
                        codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL.to_string(),
                    ),
                    model: Some(
                        codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL
                            .to_string(),
                    ),
                    ts: 10,
                    operation: None,
                },
            },
            ctx(10),
        );
        expect_code(result, ValidationCode::Structural);
        assert_eq!(graph.snapshot(), &before, "rejection must be atomic");
    }
}

#[test]
fn activity_operation_links_require_live_operations_at_write_time() {
    let mut graph = seeded();
    for operation in [nid("missing"), nid("root")] {
        let before = graph.snapshot().clone();
        let result = graph.apply(
            WorkGraphChange::RecordActivity {
                event: effort_activity("xiaomi-mimo", 10, Some(operation)),
            },
            ctx(10),
        );
        expect_code(result, ValidationCode::Structural);
        assert_eq!(graph.snapshot(), &before, "rejection must be atomic");
    }

    set_state(&mut graph, "op", NodeState::Active, 10);
    set_state(&mut graph, "op", NodeState::Completed, 11);
    let result = graph.apply(
        WorkGraphChange::RecordActivity {
            event: effort_activity("xiaomi-mimo", 12, Some(nid("op"))),
        },
        ctx(12),
    );
    expect_code(result, ValidationCode::Structural);

    for operation in [nid("missing"), nid("root")] {
        let mut malformed = graph.snapshot().clone();
        malformed
            .activities
            .push_bounded(effort_activity("xiaomi-mimo", 13, Some(operation)));
        let report = validate(&malformed).expect_err("invalid historical link must fail closed");
        assert!(report.contains_code(ValidationCode::Structural));
    }

    let mut historical = seeded();
    set_state(&mut historical, "op", NodeState::Active, 20);
    must(
        &mut historical,
        WorkGraphChange::RecordActivity {
            event: effort_activity("xiaomi-mimo", 21, Some(nid("op"))),
        },
        21,
    );
    set_state(&mut historical, "op", NodeState::Completed, 22);
    validate(historical.snapshot()).expect("a formerly-live historical link remains valid");
}

#[test]
fn snapshot_serde_round_trips() {
    let graph = scripted_run();
    let json = serde_json::to_string(graph.snapshot()).unwrap();
    let restored: WorkGraphSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(&restored, graph.snapshot());
    validate(&restored).expect("restored snapshot still satisfies invariants");
}

#[test]
fn legacy_reasoning_activity_loads_with_effective_truth_unavailable() {
    let mut wire = serde_json::to_value(effort_activity("moonshot", 10, None)).unwrap();
    let object = wire.as_object_mut().expect("activity object");
    object.remove("endpoint_identity");
    object.remove("model");

    let restored: WorkActivityEvent = serde_json::from_value(wire).expect("legacy activity loads");
    let WorkActivityEvent::ReasoningEffortChanged {
        effective,
        endpoint_identity,
        model,
        ..
    } = restored;
    assert_eq!(effective, ReasoningEffortTier::Unavailable);
    assert!(endpoint_identity.is_none());
    assert!(model.is_none());
}

#[test]
fn legacy_reasoning_activity_missing_provider_kind_downgrades_effective_truth() {
    let activity = WorkActivityEvent::ReasoningEffortChanged {
        requested: ReasoningEffortTier::Max,
        effective: ReasoningEffortTier::Max,
        provider_kind: Some(codewhale_config::ProviderKind::Openai),
        provider: "openai".to_string(),
        endpoint_identity: Some(
            codewhale_config::descriptors::defaults::DEFAULT_OPENAI_BASE_URL.to_string(),
        ),
        model: Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_MODEL.to_string()),
        ts: 10,
        operation: None,
    };
    let mut wire = serde_json::to_value(activity).expect("serialize current activity");
    wire.as_object_mut()
        .expect("activity object")
        .remove("provider_kind");

    let restored: WorkActivityEvent =
        serde_json::from_value(wire).expect("legacy activity remains loadable");
    let WorkActivityEvent::ReasoningEffortChanged {
        effective,
        provider_kind,
        endpoint_identity,
        model,
        ..
    } = restored;
    assert_eq!(provider_kind, None);
    assert_eq!(effective, ReasoningEffortTier::Unavailable);
    assert!(
        endpoint_identity.is_some(),
        "route provenance remains visible"
    );
    assert!(model.is_some(), "model provenance remains visible");
}

#[test]
fn restored_custom_builtin_slug_collisions_cannot_forge_concrete_tiers() {
    for provider in ["openai", "zai"] {
        let mut forged = seeded().into_snapshot();
        forged
            .activities
            .push_bounded(WorkActivityEvent::ReasoningEffortChanged {
                requested: ReasoningEffortTier::Max,
                effective: ReasoningEffortTier::Max,
                provider_kind: Some(codewhale_config::ProviderKind::Custom),
                provider: provider.to_string(),
                endpoint_identity: Some("https://gateway.example/v1".to_string()),
                model: Some("vendor-model-x".to_string()),
                ts: 10,
                operation: None,
            });
        let wire = serde_json::to_string(&forged).expect("serialize forged custom route");
        let restored: WorkGraphSnapshot =
            serde_json::from_str(&wire).expect("restore forged custom route");
        let report = validate(&restored)
            .expect_err("custom identity collision cannot inherit built-in reasoning tiers");
        assert!(
            report.contains_code(ValidationCode::Structural),
            "{provider}"
        );

        let mut truthful_snapshot = seeded().into_snapshot();
        truthful_snapshot
            .activities
            .push_bounded(WorkActivityEvent::ReasoningEffortChanged {
                requested: ReasoningEffortTier::Max,
                effective: ReasoningEffortTier::Unavailable,
                provider_kind: Some(codewhale_config::ProviderKind::Custom),
                provider: provider.to_string(),
                endpoint_identity: Some("https://gateway.example/v1".to_string()),
                model: Some("vendor-model-x".to_string()),
                ts: 10,
                operation: None,
            });
        let truthful_wire =
            serde_json::to_string(&truthful_snapshot).expect("serialize truthful custom route");
        let truthful: WorkGraphSnapshot =
            serde_json::from_str(&truthful_wire).expect("restore truthful custom route");
        validate(&truthful).expect("custom slug collision remains valid only as unavailable");
    }
}

#[test]
fn restored_genuine_openai_preserves_known_tier_with_exact_provenance() {
    let mut snapshot = seeded().into_snapshot();
    snapshot
        .activities
        .push_bounded(WorkActivityEvent::ReasoningEffortChanged {
            requested: ReasoningEffortTier::Max,
            effective: ReasoningEffortTier::Max,
            provider_kind: Some(codewhale_config::ProviderKind::Openai),
            provider: "openai".to_string(),
            endpoint_identity: Some(
                codewhale_config::descriptors::defaults::DEFAULT_OPENAI_BASE_URL.to_string(),
            ),
            model: Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_MODEL.to_string()),
            ts: 10,
            operation: None,
        });
    let wire = serde_json::to_string(&snapshot).expect("serialize built-in OpenAI route");
    let restored: WorkGraphSnapshot =
        serde_json::from_str(&wire).expect("restore built-in OpenAI route");

    validate(&restored).expect("kind plus exact identity and route provenance prove OpenAI");
}

#[test]
fn restored_builtin_kind_rejects_mismatched_exact_identity() {
    let mut snapshot = seeded().into_snapshot();
    snapshot
        .activities
        .push_bounded(WorkActivityEvent::ReasoningEffortChanged {
            requested: ReasoningEffortTier::Max,
            effective: ReasoningEffortTier::Max,
            provider_kind: Some(codewhale_config::ProviderKind::Openai),
            provider: "zai".to_string(),
            endpoint_identity: Some(
                codewhale_config::descriptors::defaults::DEFAULT_OPENAI_BASE_URL.to_string(),
            ),
            model: Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_MODEL.to_string()),
            ts: 10,
            operation: None,
        });

    let report = validate(&snapshot).expect_err("kind and exact identity cannot disagree");
    assert!(report.contains_code(ValidationCode::Structural));
    assert!(report.violations.iter().any(|violation| {
        violation
            .message
            .contains("provider identity does not match its recorded kind")
    }));
}

#[test]
fn restored_zai_gateway_cannot_forge_effective_max() {
    let mut snapshot = seeded().into_snapshot();
    snapshot
        .activities
        .push_bounded(WorkActivityEvent::ReasoningEffortChanged {
            requested: ReasoningEffortTier::Max,
            effective: ReasoningEffortTier::Max,
            provider_kind: Some(codewhale_config::ProviderKind::Zai),
            provider: "zai".to_string(),
            endpoint_identity: Some("https://gateway.example/v1".to_string()),
            model: Some(
                codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL.to_string(),
            ),
            ts: 10,
            operation: None,
        });
    let wire = serde_json::to_string(&snapshot).expect("serialize forged snapshot");
    let restored: WorkGraphSnapshot = serde_json::from_str(&wire).expect("restore forged snapshot");

    let report = validate(&restored).expect_err("gateway cannot prove effective max");
    assert!(report.contains_code(ValidationCode::Structural));
    assert!(report.violations.iter().any(|violation| {
        violation
            .message
            .contains("impossible for its recorded route")
    }));
}

#[test]
fn restored_zai_toggle_only_and_unknown_models_enforce_effective_truth() {
    for (model, truthful_effective) in [
        (
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_1_MODEL,
            ReasoningEffortTier::ThinkingEnabledGranularityUnavailable,
        ),
        (
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_TURBO_MODEL,
            ReasoningEffortTier::ThinkingEnabledGranularityUnavailable,
        ),
        ("glm-future-unknown", ReasoningEffortTier::Unavailable),
    ] {
        let mut forged = seeded().into_snapshot();
        forged
            .activities
            .push_bounded(WorkActivityEvent::ReasoningEffortChanged {
                requested: ReasoningEffortTier::Max,
                effective: ReasoningEffortTier::Max,
                provider_kind: Some(codewhale_config::ProviderKind::Zai),
                provider: "zai".to_string(),
                endpoint_identity: Some(
                    codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL.to_string(),
                ),
                model: Some(model.to_string()),
                ts: 10,
                operation: None,
            });
        validate(&forged).expect_err("toggle-only/unknown Z.ai route cannot prove max");

        let mut truthful = seeded().into_snapshot();
        truthful
            .activities
            .push_bounded(WorkActivityEvent::ReasoningEffortChanged {
                requested: ReasoningEffortTier::Max,
                effective: truthful_effective,
                provider_kind: Some(codewhale_config::ProviderKind::Zai),
                provider: "zai".to_string(),
                endpoint_identity: Some(
                    codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL.to_string(),
                ),
                model: Some(model.to_string()),
                ts: 10,
                operation: None,
            });
        validate(&truthful).expect("truthful Z.ai effective state restores");
    }
}

#[test]
fn restored_zai_forced_thinking_models_cannot_claim_effective_off() {
    // GLM-5.3 / GLM-5.3-Flash reject `thinking.type: "disabled"`; the wire
    // sends `off` as `reasoning_effort: "low"`, so only `Low` is a truthful
    // effective receipt for a requested `Off`. GLM-5.2 keeps the toggle.
    // Both hosts of the first-party open platform enforce the same receipt.
    let zai_activity =
        |endpoint: &str, model: &str, effective: ReasoningEffortTier| -> WorkActivityEvent {
            WorkActivityEvent::ReasoningEffortChanged {
                requested: ReasoningEffortTier::Off,
                effective,
                provider_kind: Some(codewhale_config::ProviderKind::Zai),
                provider: "zai".to_string(),
                endpoint_identity: Some(endpoint.to_string()),
                model: Some(model.to_string()),
                ts: 10,
                operation: None,
            }
        };
    for endpoint in [
        codewhale_config::descriptors::defaults::DEFAULT_ZAI_BASE_URL,
        "https://open.bigmodel.cn/api/paas/v4",
    ] {
        for model in [
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_MODEL,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_3_FLASH_MODEL,
        ] {
            let mut forged = seeded().into_snapshot();
            forged
                .activities
                .push_bounded(zai_activity(endpoint, model, ReasoningEffortTier::Off));
            validate(&forged).expect_err("forced-thinking Z.ai route cannot prove effective off");

            let mut truthful = seeded().into_snapshot();
            truthful.activities.push_bounded(zai_activity(
                endpoint,
                model,
                ReasoningEffortTier::Low,
            ));
            validate(&truthful).expect("off on a forced-thinking route restores as low");
        }

        let mut glm_5_2 = seeded().into_snapshot();
        glm_5_2.activities.push_bounded(zai_activity(
            endpoint,
            codewhale_config::catalog::reviewed::constants::ZAI_GLM_5_2_MODEL,
            ReasoningEffortTier::Off,
        ));
        validate(&glm_5_2).expect("GLM-5.2 still honours the disabled toggle");
    }
}

#[test]
fn restored_named_gateway_cannot_forge_effective_max() {
    let mut snapshot = seeded().into_snapshot();
    snapshot
        .activities
        .push_bounded(WorkActivityEvent::ReasoningEffortChanged {
            requested: ReasoningEffortTier::Max,
            effective: ReasoningEffortTier::Max,
            provider_kind: Some(codewhale_config::ProviderKind::Custom),
            provider: "my-gateway".to_string(),
            endpoint_identity: Some("https://gateway.example/v1".to_string()),
            model: Some("vendor-model-x".to_string()),
            ts: 10,
            operation: None,
        });
    let wire = serde_json::to_string(&snapshot).expect("serialize forged snapshot");
    let restored: WorkGraphSnapshot = serde_json::from_str(&wire).expect("restore forged snapshot");

    let report = validate(&restored).expect_err("unknown gateway cannot prove effective max");
    assert!(report.contains_code(ValidationCode::Structural));
    assert!(report.violations.iter().any(|violation| {
        violation
            .message
            .contains("impossible for its recorded route")
    }));
}

#[test]
fn restored_named_gateway_accepts_truthful_effective_unavailable() {
    let mut snapshot = seeded().into_snapshot();
    snapshot
        .activities
        .push_bounded(WorkActivityEvent::ReasoningEffortChanged {
            requested: ReasoningEffortTier::Max,
            effective: ReasoningEffortTier::Unavailable,
            provider_kind: Some(codewhale_config::ProviderKind::Custom),
            provider: "my-gateway".to_string(),
            endpoint_identity: Some("https://gateway.example/v1".to_string()),
            model: Some("vendor-model-x".to_string()),
            ts: 10,
            operation: None,
        });
    let wire = serde_json::to_string(&snapshot).expect("serialize truthful unknown snapshot");
    let restored: WorkGraphSnapshot =
        serde_json::from_str(&wire).expect("restore truthful unknown snapshot");

    validate(&restored).expect("unknown route remains valid when effective truth is unavailable");
}

#[test]
fn pre_wg3_proposal_shape_deserializes_with_empty_delta_extensions() {
    let proposal = WorkGraphProposal {
        id: ProposalId::derive(SESSION, "legacy-proposal"),
        added_nodes: Vec::new(),
        added_edges: Vec::new(),
        updated_nodes: Vec::new(),
        removed_nodes: vec![nid("old-step")],
        removed_edges: Vec::new(),
        replacement_compat: Some(CompatProjectionState::default()),
    };
    let mut value = serde_json::to_value(proposal).expect("serialize current proposal");
    let object = value.as_object_mut().expect("proposal object");
    object.remove("removed_nodes");
    object.remove("replacement_compat");

    let restored: WorkGraphProposal =
        serde_json::from_value(value).expect("deserialize pre-WG3 proposal");
    assert!(restored.removed_nodes.is_empty());
    assert!(restored.replacement_compat.is_none());
}

#[test]
fn evidence_ref_rejects_paths_and_key_material() {
    let err = |reference: &str| {
        EvidenceRef::new(EvidenceKind::ToolRun, reference, None, false)
            .expect_err("reference should be rejected")
    };
    assert_eq!(err(""), EvidenceRefError::EmptyReference);
    assert_eq!(err("/var/log/system.log"), EvidenceRefError::AbsolutePath);
    assert_eq!(err("\\\\server\\share"), EvidenceRefError::AbsolutePath);
    assert_eq!(err("C:\\temp\\out.txt"), EvidenceRefError::AbsolutePath);
    assert_eq!(err("~/notes.txt"), EvidenceRefError::HomeRelativePath);
    assert_eq!(
        err("two words"),
        EvidenceRefError::ContainsWhitespaceOrControl
    );
    assert_eq!(
        err("line\nbreak"),
        EvidenceRefError::ContainsWhitespaceOrControl
    );
    // Built at runtime so secret scanners never see a contiguous PEM marker
    // in source; the validator still receives the real prefix.
    let pem_marker = format!("-----{}-RSA-KEY", "BEGIN");
    assert_eq!(err(&pem_marker), EvidenceRefError::LooksLikeKeyMaterial);

    // Deserialization re-runs constructor validation — serde is not a bypass.
    let smuggled =
        r#"{"kind":"tool_run","reference":"/etc/hosts","raw_bytes":null,"truncated":false}"#;
    assert!(serde_json::from_str::<EvidenceRef>(smuggled).is_err());

    let ok = EvidenceRef::new(
        EvidenceKind::Artifact {
            digest: "sha256:abc123".to_string(),
        },
        "artifact:build-77",
        Some(4096),
        true,
    )
    .unwrap();
    assert_eq!(ok.reference(), "artifact:build-77");
    assert_eq!(ok.raw_bytes(), Some(4096));
    assert!(ok.truncated());
}

#[test]
fn web_citation_evidence_is_typed_inspectable_and_fail_closed() {
    let kind = EvidenceKind::WebCitation {
        ref_id: "web_0123456789abcdef".to_string(),
        url: "https://example.com/source".to_string(),
        retrieved_at: "2026-07-19T18:00:00Z".to_string(),
    };
    let evidence = EvidenceRef::new(kind, "web_0123456789abcdef", None, false)
        .expect("valid web citation evidence");
    assert_eq!(evidence.kind().tag(), EvidenceKindTag::WebCitation);

    let serialized = serde_json::to_value(&evidence).expect("serialize citation evidence");
    let restored: EvidenceRef =
        serde_json::from_value(serialized).expect("deserialize citation evidence");
    assert_eq!(restored, evidence);

    let mismatched = EvidenceRef::new(
        EvidenceKind::WebCitation {
            ref_id: "web_other".to_string(),
            url: "https://example.com/source".to_string(),
            retrieved_at: "2026-07-19T18:00:00Z".to_string(),
        },
        "web_0123456789abcdef",
        None,
        false,
    )
    .expect_err("mismatched ref must fail");
    assert_eq!(mismatched, EvidenceRefError::WebCitationReferenceMismatch);

    let credentialed = EvidenceRef::new(
        EvidenceKind::WebCitation {
            ref_id: "web_0123456789abcdef".to_string(),
            url: "https://user:password@example.com/source".to_string(),
            retrieved_at: "2026-07-19T18:00:00Z".to_string(),
        },
        "web_0123456789abcdef",
        None,
        false,
    )
    .expect_err("credentialed URL must fail");
    assert_eq!(credentialed, EvidenceRefError::InvalidWebCitationUrl);

    let query_credential = EvidenceRef::new(
        EvidenceKind::WebCitation {
            ref_id: "web_0123456789abcdef".to_string(),
            url: "https://example.com/source?access_token=sensitive".to_string(),
            retrieved_at: "2026-07-19T18:00:00Z".to_string(),
        },
        "web_0123456789abcdef",
        None,
        false,
    )
    .expect_err("credential-bearing query must fail");
    assert_eq!(query_credential, EvidenceRefError::InvalidWebCitationUrl);
}

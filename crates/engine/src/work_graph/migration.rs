//! Deterministic import of the pre-Work-Graph Plan and To-do session state.

use crate::tools::plan::{PlanSnapshot, PlanState, StepStatus};
use crate::tools::todo::{TodoList, TodoListSnapshot, TodoStatus};
use codewhale_runtime::hashing::sha256_hex;

use super::{
    ChangeCtx, CompatPlanMetadata, CompatProjectionState, CompatTodoBinding, EdgeKind, NodeKind,
    NodeState, Provenance, ValidationReport, WorkEdge, WorkEdgeId, WorkGraph, WorkGraphChange,
    WorkGraphSnapshot, WorkNode, WorkNodeId, WorkNodePatch,
};

const PLAN_PROVENANCE_PREFIX: &str = "\u{2063}cw-plan-step:";
const PLAN_PROVENANCE_SUFFIX: &str = "\u{2063}";

/// Import legacy state into a deterministic graph. Repeating the same import
/// produces the same IDs, digest, and serialized snapshot.
pub fn import_legacy(
    session_id: &str,
    plan: &PlanSnapshot,
    todos: &TodoListSnapshot,
) -> Result<WorkGraphSnapshot, String> {
    let plan = PlanState::from_snapshot(plan).snapshot();
    let todos = TodoList::from_snapshot(todos)?.snapshot();
    let canonical = serde_json::to_vec(&(&plan, &todos))
        .map_err(|err| format!("could not canonicalize legacy Work state: {err}"))?;
    let digest = sha256_hex(canonical);

    let mut graph = WorkGraph::new();
    let ctx = ChangeCtx {
        session_id: session_id.to_string(),
        now: 0,
        idempotency_key: None,
    };
    let objective_id = WorkNodeId::derive(session_id, "objective");
    let objective_title = plan
        .objective
        .as_deref()
        .or(plan.title.as_deref())
        .unwrap_or("Imported session work")
        .to_string();
    apply(
        &mut graph,
        WorkGraphChange::AddNode {
            node: WorkNode {
                id: objective_id.clone(),
                kind: NodeKind::Objective,
                title: objective_title,
                state: NodeState::Ready,
                acceptance: Vec::new(),
                binding: None,
                evidence: None,
                provenance: Provenance::Import {
                    source_digest: digest.clone(),
                    ordinal: None,
                },
                created_at: 0,
                updated_at: 0,
            },
        },
        &ctx,
    )?;

    let mut compat = CompatProjectionState {
        plan: CompatPlanMetadata::from_plan_snapshot(&plan),
        ..CompatProjectionState::default()
    };
    for (index, item) in plan.items.iter().enumerate() {
        let ordinal = u32::try_from(index).map_err(|_| "too many legacy plan steps")?;
        let id = WorkNodeId::derive(session_id, &format!("plan:{index}"));
        apply(
            &mut graph,
            WorkGraphChange::AddNode {
                node: WorkNode {
                    id: id.clone(),
                    kind: NodeKind::PlanStep,
                    title: item.step.trim().to_string(),
                    state: node_state_from_plan(&item.status),
                    acceptance: Vec::new(),
                    binding: None,
                    evidence: None,
                    provenance: Provenance::Import {
                        source_digest: digest.clone(),
                        ordinal: Some(ordinal),
                    },
                    created_at: 0,
                    updated_at: 0,
                },
            },
            &ctx,
        )?;
        add_contains_edge(&mut graph, session_id, &objective_id, &id, &ctx)?;
        compat.plan_order.push(id);
    }

    for item in &todos.items {
        let (clean_title, marker_index) = strip_plan_provenance(&item.content);
        let aliased = marker_index
            .and_then(|index| {
                usize::try_from(index)
                    .ok()
                    .map(|usize_index| (index, usize_index))
            })
            .and_then(|(index, usize_index)| {
                compat
                    .plan_order
                    .get(usize_index)
                    .cloned()
                    .map(|node| (index, node))
            });
        let (node, plan_index) = if let Some((index, node)) = aliased {
            let current = graph
                .snapshot()
                .node(&node)
                .map(|node| node.state)
                .ok_or_else(|| "legacy plan alias references a missing node".to_string())?;
            let desired = more_advanced_state(current, node_state_from_todo(item.status));
            if desired != current {
                apply(
                    &mut graph,
                    WorkGraphChange::UpdateNode {
                        id: node.clone(),
                        patch: WorkNodePatch {
                            state: Some(desired),
                            ..WorkNodePatch::default()
                        },
                    },
                    &ctx,
                )?;
            }
            (node, Some(index))
        } else {
            let node = WorkNodeId::derive(session_id, &format!("todo:{}", item.id));
            apply(
                &mut graph,
                WorkGraphChange::AddNode {
                    node: WorkNode {
                        id: node.clone(),
                        kind: NodeKind::PlanStep,
                        title: clean_title,
                        // Add live operations as Ready, root them, then apply
                        // their live state so V2 is true after every change.
                        state: NodeState::Ready,
                        acceptance: Vec::new(),
                        binding: None,
                        evidence: None,
                        provenance: Provenance::Import {
                            source_digest: digest.clone(),
                            ordinal: Some(item.id),
                        },
                        created_at: 0,
                        updated_at: 0,
                    },
                },
                &ctx,
            )?;
            add_contains_edge(&mut graph, session_id, &objective_id, &node, &ctx)?;
            let desired = node_state_from_todo(item.status);
            if desired != NodeState::Ready {
                apply(
                    &mut graph,
                    WorkGraphChange::UpdateNode {
                        id: node.clone(),
                        patch: WorkNodePatch {
                            state: Some(desired),
                            ..WorkNodePatch::default()
                        },
                    },
                    &ctx,
                )?;
            }
            (node, None)
        };
        compat.todos.push(CompatTodoBinding {
            legacy_id: item.id,
            node,
            plan_index,
        });
    }

    apply(
        &mut graph,
        WorkGraphChange::ReplaceCompatProjection { compat },
        &ctx,
    )?;
    apply(
        &mut graph,
        WorkGraphChange::SetImportDigest { digest },
        &ctx,
    )?;
    Ok(graph.into_snapshot())
}

fn apply(graph: &mut WorkGraph, change: WorkGraphChange, ctx: &ChangeCtx) -> Result<(), String> {
    graph
        .apply(change, ctx.clone())
        .map(|_| ())
        .map_err(|err: ValidationReport| err.to_string())
}

fn add_contains_edge(
    graph: &mut WorkGraph,
    session_id: &str,
    objective: &WorkNodeId,
    child: &WorkNodeId,
    ctx: &ChangeCtx,
) -> Result<(), String> {
    apply(
        graph,
        WorkGraphChange::AddEdge {
            edge: WorkEdge {
                id: WorkEdgeId::derive(
                    session_id,
                    &format!("contains:{}:{}", objective.as_str(), child.as_str()),
                ),
                kind: EdgeKind::Contains,
                from: objective.clone(),
                to: child.clone(),
            },
        },
        ctx,
    )
}

fn node_state_from_plan(status: &StepStatus) -> NodeState {
    match status {
        StepStatus::Pending => NodeState::Ready,
        StepStatus::InProgress => NodeState::Active,
        StepStatus::Completed => NodeState::Completed,
    }
}

fn node_state_from_todo(status: TodoStatus) -> NodeState {
    match status {
        TodoStatus::Pending => NodeState::Ready,
        TodoStatus::InProgress => NodeState::Active,
        TodoStatus::Completed => NodeState::Completed,
        TodoStatus::Cancelled => NodeState::Cancelled,
    }
}

fn more_advanced_state(left: NodeState, right: NodeState) -> NodeState {
    fn rank(state: NodeState) -> u8 {
        match state {
            NodeState::Completed | NodeState::Verified => 2,
            NodeState::Initializing | NodeState::Active => 1,
            _ => 0,
        }
    }
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

/// The only remaining parser for the retired Plan→To-do marker writer.
fn strip_plan_provenance(content: &str) -> (String, Option<u32>) {
    let Some(start) = content.find(PLAN_PROVENANCE_PREFIX) else {
        return (
            content
                .replace(PLAN_PROVENANCE_SUFFIX, "")
                .trim()
                .to_string(),
            None,
        );
    };
    let value_start = start + PLAN_PROVENANCE_PREFIX.len();
    let (marker_end, index) =
        if let Some(relative_end) = content[value_start..].find(PLAN_PROVENANCE_SUFFIX) {
            let end = value_start + relative_end;
            (
                end + PLAN_PROVENANCE_SUFFIX.len(),
                content[value_start..end].parse::<u32>().ok(),
            )
        } else {
            // A truncated retired marker was never user content. Drop the
            // unterminated suffix so hidden migration metadata cannot leak into
            // either compatibility projection.
            (content.len(), None)
        };
    let mut clean = String::with_capacity(content.len());
    clean.push_str(&content[..start]);
    clean.push_str(&content[marker_end..]);
    (
        clean.replace(PLAN_PROVENANCE_SUFFIX, "").trim().to_string(),
        index,
    )
}

#[cfg(any(test, feature = "test-support"))]
fn plan_provenance_marker(index: u32) -> String {
    format!("{PLAN_PROVENANCE_PREFIX}{index}{PLAN_PROVENANCE_SUFFIX}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::plan::PlanItemArg;
    use crate::tools::todo::TodoItem;
    use crate::work_graph::{project_plan, project_todos, validate};

    #[test]
    fn legacy_import_is_deterministic_and_projects_complete_old_views() {
        let plan = PlanSnapshot {
            objective: Some("Ship it".to_string()),
            items: vec![PlanItemArg {
                step: "Verify".to_string(),
                status: StepStatus::InProgress,
            }],
            ..PlanSnapshot::default()
        };
        let todos = TodoListSnapshot {
            items: vec![TodoItem {
                id: 4,
                content: format!("Verify{}", plan_provenance_marker(0)),
                status: TodoStatus::InProgress,
            }],
            completion_pct: 0,
            in_progress_id: Some(4),
        };
        let first = import_legacy("session-1", &plan, &todos).expect("import");
        let second = import_legacy("session-1", &plan, &todos).expect("repeat import");
        assert_eq!(first, second);
        validate(&first).expect("valid graph");
        assert_eq!(project_plan(&first), plan);
        let projected_todos = project_todos(&first);
        assert_eq!(projected_todos.items[0].content, "Verify");
        assert_eq!(projected_todos.items[0].status, TodoStatus::InProgress);
        assert_eq!(first.compat.todos[0].node, first.compat.plan_order[0]);
        assert!(!first.nodes[1].title.contains('\u{2063}'));
    }

    #[test]
    fn malformed_retired_markers_never_leak_into_old_views() {
        let plan = PlanSnapshot::default();
        let todos = TodoListSnapshot {
            items: vec![
                TodoItem {
                    id: 1,
                    content: format!(
                        "Visible{PLAN_PROVENANCE_PREFIX}not-a-number{PLAN_PROVENANCE_SUFFIX}"
                    ),
                    status: TodoStatus::Pending,
                },
                TodoItem {
                    id: 2,
                    content: format!("Also visible{PLAN_PROVENANCE_PREFIX}truncated"),
                    status: TodoStatus::Pending,
                },
            ],
            completion_pct: 0,
            in_progress_id: None,
        };
        let graph = import_legacy("malformed-markers", &plan, &todos).expect("import");
        let projected = project_todos(&graph);
        assert_eq!(projected.items[0].content, "Visible");
        assert_eq!(projected.items[1].content, "Also visible");
        assert!(
            projected
                .items
                .iter()
                .all(|item| !item.content.contains('\u{2063}'))
        );
    }
}

//! Pure compatibility projections for the pre-Work-Graph Plan and To-do wire
//! formats.
//!
//! The graph owns the ordering, metadata, titles, and states. These functions
//! only derive owned snapshots; they never receive mutable graph access (V10).

use crate::tools::plan::{PlanItemArg, PlanSnapshot, StepStatus};
use crate::tools::todo::{TodoItem, TodoListSnapshot, TodoStatus};

use super::model::{CompatPlanMetadata, NodeState, WorkGraphSnapshot, WorkNode};

pub type PlanProjection = PlanSnapshot;
pub type TodoProjection = TodoListSnapshot;

impl CompatPlanMetadata {
    #[must_use]
    pub fn from_plan_snapshot(snapshot: &PlanSnapshot) -> Self {
        Self {
            title: snapshot.title.clone(),
            objective: snapshot.objective.clone(),
            context_summary: snapshot.context_summary.clone(),
            explanation: snapshot.explanation.clone(),
            sources_used: snapshot.sources_used.clone(),
            critical_files: snapshot.critical_files.clone(),
            constraints: snapshot.constraints.clone(),
            recommended_approach: snapshot.recommended_approach.clone(),
            verification_plan: snapshot.verification_plan.clone(),
            risks_and_unknowns: snapshot.risks_and_unknowns.clone(),
            handoff_packet: snapshot.handoff_packet.clone(),
        }
    }

    #[must_use]
    fn to_plan_snapshot(&self) -> PlanSnapshot {
        PlanSnapshot {
            title: self.title.clone(),
            objective: self.objective.clone(),
            context_summary: self.context_summary.clone(),
            explanation: self.explanation.clone(),
            sources_used: self.sources_used.clone(),
            critical_files: self.critical_files.clone(),
            constraints: self.constraints.clone(),
            recommended_approach: self.recommended_approach.clone(),
            verification_plan: self.verification_plan.clone(),
            risks_and_unknowns: self.risks_and_unknowns.clone(),
            handoff_packet: self.handoff_packet.clone(),
            items: Vec::new(),
        }
    }
}

/// Derive the complete legacy Strategy/Plan snapshot.
#[must_use]
pub fn project_plan(snapshot: &WorkGraphSnapshot) -> PlanProjection {
    let mut plan = snapshot.compat.plan.to_plan_snapshot();
    plan.items = snapshot
        .compat
        .plan_order
        .iter()
        .filter_map(|id| snapshot.node(id))
        .map(|node| PlanItemArg {
            step: node.title.clone(),
            status: plan_status(node),
        })
        .collect();
    plan
}

/// Derive the complete legacy To-do snapshot. Migration provenance stays in
/// the graph; old readers receive clean, user-visible content.
#[must_use]
pub fn project_todos(snapshot: &WorkGraphSnapshot) -> TodoProjection {
    let items = snapshot
        .compat
        .todos
        .iter()
        .filter_map(|binding| {
            let node = snapshot.node(&binding.node)?;
            Some(TodoItem {
                id: binding.legacy_id,
                content: node.title.clone(),
                status: todo_status(node),
            })
        })
        .collect::<Vec<_>>();
    let settled = items.iter().filter(|item| item.status.is_settled()).count();
    let completion_pct = if items.is_empty() {
        0
    } else {
        let rounded = (settled.saturating_mul(100) + items.len() / 2) / items.len();
        u8::try_from(rounded).unwrap_or(u8::MAX)
    };
    let in_progress_id = items
        .iter()
        .find(|item| item.status == TodoStatus::InProgress)
        .map(|item| item.id);
    TodoListSnapshot {
        items,
        completion_pct,
        in_progress_id,
    }
}

fn plan_status(node: &WorkNode) -> StepStatus {
    match node.state {
        NodeState::Initializing | NodeState::Active => StepStatus::InProgress,
        NodeState::Completed | NodeState::Verified => StepStatus::Completed,
        _ => StepStatus::Pending,
    }
}

fn todo_status(node: &WorkNode) -> TodoStatus {
    match node.state {
        NodeState::Initializing | NodeState::Active => TodoStatus::InProgress,
        NodeState::Completed | NodeState::Verified => TodoStatus::Completed,
        NodeState::Cancelled | NodeState::Superseded => TodoStatus::Cancelled,
        _ => TodoStatus::Pending,
    }
}

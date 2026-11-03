//! Plain-text projection of canonical Work Graph and To-do state.

use crate::tools::todo::{TodoItem, TodoListSnapshot, TodoStatus};

use super::{NodeKind, NodeState, WorkGraphSnapshot, WorkRuntimeSnapshot};

#[must_use]
pub fn format_operation_digest(snapshot: Option<&WorkRuntimeSnapshot>) -> String {
    let Some(snapshot) = snapshot else {
        return "No active operations or to-do items.".to_string();
    };
    format_operation_digest_parts(&snapshot.graph, &snapshot.todos)
}

#[must_use]
pub fn format_operation_digest_parts(
    graph: &WorkGraphSnapshot,
    todos: &TodoListSnapshot,
) -> String {
    let mut operations = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Operation)
        .collect::<Vec<_>>();
    operations.sort_by_key(|node| (state_rank(node.state), node.updated_at));

    let mut todo_items = todos.items.iter().collect::<Vec<_>>();
    todo_items.sort_by_key(|item| (todo_rank(item), item.id));

    if operations.is_empty() && todo_items.is_empty() {
        return "No active operations or to-do items.".to_string();
    }

    let mut out = String::from("Operation digest\n");
    if !operations.is_empty() {
        out.push_str("\nOperations\n");
        for node in operations {
            let owner = node
                .binding
                .as_ref()
                .map_or("unbound", |binding| binding.external.as_str());
            out.push_str(&format!(
                "  {:<10} {} · {}\n",
                state_label(node.state),
                owner,
                one_line(&node.title)
            ));
        }
    }
    if !todo_items.is_empty() {
        out.push_str("\nTo-do\n");
        for item in todo_items {
            out.push_str(&format!(
                "  {:<10} #{} · {}\n",
                todo_label(item.status),
                item.id,
                one_line(&item.content)
            ));
        }
    }
    out.trim_end().to_string()
}

const fn state_rank(state: NodeState) -> u8 {
    match state {
        NodeState::Active | NodeState::Initializing => 0,
        NodeState::Waiting => 1,
        NodeState::Blocked | NodeState::Stale => 2,
        NodeState::Ready => 3,
        NodeState::Failed => 4,
        NodeState::Completed => 5,
        NodeState::Verified => 6,
        NodeState::Cancelled | NodeState::Superseded => 7,
    }
}

const fn state_label(state: NodeState) -> &'static str {
    match state {
        NodeState::Ready => "ready",
        NodeState::Initializing => "starting",
        NodeState::Active => "running",
        NodeState::Waiting => "waiting",
        NodeState::Blocked => "blocked",
        NodeState::Completed => "ended",
        NodeState::Verified => "verified",
        NodeState::Stale => "stale",
        NodeState::Superseded => "superseded",
        NodeState::Cancelled => "cancelled",
        NodeState::Failed => "failed",
    }
}

const fn todo_rank(item: &TodoItem) -> u8 {
    match item.status {
        TodoStatus::InProgress => 0,
        TodoStatus::Pending => 1,
        TodoStatus::Completed => 2,
        TodoStatus::Cancelled => 3,
    }
}

const fn todo_label(status: TodoStatus) -> &'static str {
    match status {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "running",
        TodoStatus::Completed => "completed",
        TodoStatus::Cancelled => "cancelled",
    }
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::todo::TodoItem;

    #[test]
    fn digest_orders_running_work_first_and_distinguishes_cancelled() {
        let todos = TodoListSnapshot {
            items: vec![
                TodoItem {
                    id: 1,
                    content: "later".into(),
                    status: TodoStatus::Pending,
                },
                TodoItem {
                    id: 2,
                    content: "now".into(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    id: 3,
                    content: "dropped".into(),
                    status: TodoStatus::Cancelled,
                },
            ],
            completion_pct: 0,
            in_progress_id: Some(2),
        };
        let text = format_operation_digest_parts(&WorkGraphSnapshot::new(), &todos);
        assert!(text.find("#2 · now").unwrap() < text.find("#1 · later").unwrap());
        assert!(text.contains("cancelled  #3 · dropped"));
        assert!(!text.contains('\u{1b}'));
    }
}

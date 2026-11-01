//! Todo list tool and supporting data structures.

use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};

// === Types ===

/// Status for a todo item.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    #[serde(alias = "canceled")]
    Cancelled,
}

impl TodoStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Cancelled => "cancelled",
        }
    }

    /// Parse a string into a todo status.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_lowercase().as_str() {
            "pending" => Some(TodoStatus::Pending),
            "in_progress" | "inprogress" | "in-progress" | "in progress" => {
                Some(TodoStatus::InProgress)
            }
            "completed" | "complete" | "done" => Some(TodoStatus::Completed),
            "cancelled" | "canceled" => Some(TodoStatus::Cancelled),
            _ => None,
        }
    }

    /// Whether this item has reached a terminal outcome. Cancellation settles
    /// work without misreporting it as successful completion.
    #[must_use]
    pub fn is_settled(self) -> bool {
        matches!(self, TodoStatus::Completed | TodoStatus::Cancelled)
    }
}

/// A single todo item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub id: u32,
    pub content: String,
    pub status: TodoStatus,
}

/// Snapshot of a todo list for display or serialization.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoListSnapshot {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<TodoItem>,
    #[serde(default)]
    pub completion_pct: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_progress_id: Option<u32>,
}

impl TodoListSnapshot {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Mutable list of todo items with helper operations.
#[derive(Debug, Clone, Default)]
pub struct TodoList {
    items: Vec<TodoItem>,
    next_id: u32,
}

impl TodoList {
    /// Create an empty todo list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            next_id: 1,
        }
    }

    /// Return a snapshot of the list with computed metrics.
    #[must_use]
    pub fn snapshot(&self) -> TodoListSnapshot {
        TodoListSnapshot {
            items: self.items.clone(),
            completion_pct: self.completion_percentage(),
            in_progress_id: self.in_progress_id(),
        }
    }

    /// Rebuild a mutable list from a persisted snapshot.
    ///
    /// Derived snapshot fields are deliberately recomputed. IDs and the
    /// single-in-progress invariant are validated before any live state is
    /// replaced, so malformed session data cannot leave a half-restored list.
    pub fn from_snapshot(snapshot: &TodoListSnapshot) -> Result<Self, String> {
        let mut seen = std::collections::HashSet::with_capacity(snapshot.items.len());
        let mut in_progress_count = 0usize;
        let mut max_id = 0u32;
        let mut items = Vec::with_capacity(snapshot.items.len());

        for item in &snapshot.items {
            if item.id == 0 {
                return Err("To-do item IDs must be greater than zero".to_string());
            }
            if !seen.insert(item.id) {
                return Err(format!("Duplicate To-do item ID {}", item.id));
            }
            if item.status == TodoStatus::InProgress {
                in_progress_count += 1;
                if in_progress_count > 1 {
                    return Err("Only one To-do item may be in progress".to_string());
                }
            }
            max_id = max_id.max(item.id);
            items.push(TodoItem {
                id: item.id,
                content: item.content.clone(),
                status: item.status,
            });
        }

        let next_id = if items.is_empty() {
            1
        } else {
            max_id
                .checked_add(1)
                .ok_or_else(|| "To-do item IDs are exhausted".to_string())?
        };
        Ok(Self { items, next_id })
    }

    /// Add a new todo item.
    pub fn add(&mut self, content: String, status: TodoStatus) -> TodoItem {
        let status = match status {
            TodoStatus::InProgress => {
                self.set_single_in_progress(None);
                TodoStatus::InProgress
            }
            other => other,
        };

        let item = TodoItem {
            id: self.next_id,
            content,
            status,
        };
        self.next_id += 1;
        self.items.push(item.clone());
        item
    }

    /// Compute completion percentage for the list.
    #[must_use]
    pub fn completion_percentage(&self) -> u8 {
        if self.items.is_empty() {
            return 0;
        }
        let total = self.items.len();
        let settled = self
            .items
            .iter()
            .filter(|item| item.status.is_settled())
            .count();
        let percent = settled.saturating_mul(100);
        let percent = (percent + total / 2) / total;
        u8::try_from(percent).unwrap_or(u8::MAX)
    }

    /// Return the id of the in-progress item, if any.
    #[must_use]
    pub fn in_progress_id(&self) -> Option<u32> {
        self.items
            .iter()
            .find(|item| item.status == TodoStatus::InProgress)
            .map(|item| item.id)
    }

    /// Clear all todo items.
    pub fn clear(&mut self) {
        self.items.clear();
        self.next_id = 1;
    }

    fn set_single_in_progress(&mut self, allow_id: Option<u32>) {
        for item in &mut self.items {
            if Some(item.id) != allow_id && item.status == TodoStatus::InProgress {
                item.status = TodoStatus::Pending;
            }
        }
    }
}

// === TodoWriteTool - ToolSpec implementation ===

/// Shared reference to a `TodoList` for use across tools
pub type SharedTodoList = Arc<Mutex<TodoList>>;

/// Create a new shared `TodoList`
pub fn new_shared_todo_list() -> SharedTodoList {
    Arc::new(Mutex::new(TodoList::new()))
}

const CANONICAL_WORK_SURFACE: &str = "work";
const CANONICAL_PROGRESS_TOOL: &str = "todo_write";
const DURABLE_WORK_OWNER: &str = "fleet_workflow_ledger";

/// Tool for writing and updating the todo list
pub struct TodoWriteTool {
    name: &'static str,
    todo_list: SharedTodoList,
}

impl TodoWriteTool {
    /// Canonical model-facing progress surface (#4132).
    pub fn new(todo_list: SharedTodoList) -> Self {
        Self {
            name: CANONICAL_PROGRESS_TOOL,
            todo_list,
        }
    }

    /// Hidden compat alias (`work_update`, `TodoWrite`, `todo`, …) — same
    /// handler, not model-visible.
    pub fn alias(name: &'static str, todo_list: SharedTodoList) -> Self {
        Self { name, todo_list }
    }
}

#[async_trait]
impl ToolSpec for TodoWriteTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn model_visible(&self) -> bool {
        self.name == CANONICAL_PROGRESS_TOOL
    }

    fn description(&self) -> &'static str {
        "Replace the To-do list the user watches. For any task with three or more steps or more than one file, write the list before you start, keep exactly one item in_progress, and mark items done as you finish them. Skip it only for a single quick answer or edit."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete list of To-do items. This replaces the existing list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {
                                "type": "string",
                                "description": "The task description"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "cancelled"],
                                "description": "Task status"
                            }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }

    fn is_read_only_for(&self, _input: &serde_json::Value) -> bool {
        // This mutates only the caller's in-memory progress list. Sub-agent
        // runtimes allocate a fresh list per child, so it cannot touch the
        // workspace, Git, remote services, or a parent/sibling's state. Treat
        // it as bounded agent-owned state for read-only authority envelopes;
        // keep WritesFiles above for legacy/UI capability grouping.
        true
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let todos = input
            .get("todos")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::invalid_input("Missing or invalid 'todos' array"))?;

        let mut list = TodoList::new();

        for item in todos {
            let content = item
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ToolError::invalid_input("Todo item missing 'content'"))?;

            let status = match item.get("status").and_then(|v| v.as_str()) {
                Some(raw) => TodoStatus::parse(raw).ok_or_else(|| {
                    // #5123-class: unknown statuses used to silently coerce to
                    // pending on the canonical progress surface.
                    ToolError::invalid_input(format!(
                        "unknown todo status '{raw}'; expected pending, in_progress, \
                         completed, or cancelled"
                    ))
                })?,
                None => TodoStatus::Pending,
            };

            list.add(content.to_string(), status);
        }

        let snapshot = publish_todo_snapshot(
            context,
            &self.todo_list,
            CANONICAL_PROGRESS_TOOL,
            list.snapshot(),
        )
        .await?;
        let result = serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| "{}".to_string());

        Ok(ToolResult::success(format!(
            "Todo list updated ({} items, {}% settled)\n{}",
            snapshot.items.len(),
            snapshot.completion_pct,
            result
        ))
        .with_metadata(work_progress_metadata(&snapshot)))
    }
}

async fn publish_todo_snapshot(
    context: &ToolContext,
    todo_list: &SharedTodoList,
    tool_name: &str,
    desired: TodoListSnapshot,
) -> Result<TodoListSnapshot, ToolError> {
    if let Some(work) = context.runtime.work.as_ref()
        && work.matches_todos(todo_list)
    {
        return work
            .apply_todo_update(&context.state_namespace, tool_name, &desired)
            .await
            .map_err(ToolError::execution_failed);
    }
    *todo_list.lock().await =
        TodoList::from_snapshot(&desired).map_err(ToolError::execution_failed)?;
    Ok(desired)
}

fn work_progress_metadata(snapshot: &TodoListSnapshot) -> serde_json::Value {
    let items = snapshot
        .items
        .iter()
        .map(|item| {
            json!({
                "id": item.id,
                "content": item.content,
                "status": item.status.as_str(),
            })
        })
        .collect::<Vec<_>>();
    json!({
        "canonical_tool": CANONICAL_PROGRESS_TOOL,
        "work_surface": {
            "canonical": CANONICAL_WORK_SURFACE,
            "model_visible": true,
            "durable_owner": DURABLE_WORK_OWNER,
            "progress_key": "task_updates.checklist"
        },
        "task_updates": {
            "checklist": {
                "items": items,
                "completion_pct": snapshot.completion_pct,
                "in_progress_id": snapshot.in_progress_id,
                "updated_at": null
            }
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn todo_write_is_bounded_agent_owned_state_for_read_only_envelopes() {
        let tool = super::TodoWriteTool::new(super::new_shared_todo_list());
        assert!(crate::tools::spec::ToolSpec::is_read_only_for(
            &tool,
            &serde_json::json!({
                "todos": [{"content": "private evidence note", "status": "pending"}]
            })
        ));
        assert!(
            crate::tools::spec::ToolSpec::capabilities(&tool)
                .contains(&crate::tools::spec::ToolCapability::WritesFiles),
            "legacy capability grouping remains intact"
        );
    }

    use super::*;

    #[test]
    fn cancelled_is_a_terminal_round_trippable_todo_state() {
        assert_eq!(TodoStatus::parse("cancelled"), Some(TodoStatus::Cancelled));
        assert_eq!(TodoStatus::parse("canceled"), Some(TodoStatus::Cancelled));

        let mut list = TodoList::new();
        list.add("abandoned approach".to_string(), TodoStatus::Cancelled);
        let snapshot = list.snapshot();
        assert_eq!(snapshot.completion_pct, 100);
        assert_eq!(snapshot.in_progress_id, None);
        assert_eq!(
            serde_json::to_value(snapshot.items[0].status).expect("serialize"),
            serde_json::json!("cancelled")
        );

        let schema = TodoWriteTool::new(new_shared_todo_list()).input_schema();
        let statuses = &schema["properties"]["todos"]["items"]["properties"]["status"]["enum"];
        assert!(statuses.as_array().is_some_and(|values| {
            values
                .iter()
                .any(|value| value.as_str() == Some("cancelled"))
        }));
    }

    #[test]
    fn persisted_snapshot_restores_ids_status_and_recomputes_metrics() {
        let snapshot = TodoListSnapshot {
            items: vec![
                TodoItem {
                    id: 4,
                    content: " inspect ".to_string(),
                    status: TodoStatus::Completed,
                },
                TodoItem {
                    id: 9,
                    content: "patch".to_string(),
                    status: TodoStatus::InProgress,
                },
            ],
            completion_pct: 0,
            in_progress_id: None,
        };

        let mut restored = TodoList::from_snapshot(&snapshot).expect("restore");
        let restored_snapshot = restored.snapshot();
        assert_eq!(restored_snapshot.items[0].id, 4);
        assert_eq!(restored_snapshot.items[0].content, " inspect ");
        assert_eq!(restored_snapshot.items[1].id, 9);
        assert_eq!(restored_snapshot.completion_pct, 50);
        assert_eq!(restored_snapshot.in_progress_id, Some(9));
        assert_eq!(
            restored.add("verify".to_string(), TodoStatus::Pending).id,
            10
        );
    }

    #[test]
    fn malformed_persisted_snapshot_is_rejected_deterministically() {
        let duplicate = TodoListSnapshot {
            items: vec![
                TodoItem {
                    id: 1,
                    content: "one".to_string(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    id: 1,
                    content: "two".to_string(),
                    status: TodoStatus::Pending,
                },
            ],
            ..TodoListSnapshot::default()
        };
        assert_eq!(
            TodoList::from_snapshot(&duplicate).unwrap_err(),
            "Duplicate To-do item ID 1"
        );

        let multiple_active = TodoListSnapshot {
            items: vec![
                TodoItem {
                    id: 1,
                    content: "one".to_string(),
                    status: TodoStatus::InProgress,
                },
                TodoItem {
                    id: 2,
                    content: "two".to_string(),
                    status: TodoStatus::InProgress,
                },
            ],
            ..TodoListSnapshot::default()
        };
        assert_eq!(
            TodoList::from_snapshot(&multiple_active).unwrap_err(),
            "Only one To-do item may be in progress"
        );
    }

    #[tokio::test]
    async fn work_update_rejects_unknown_status_instead_of_coercing_to_pending() {
        // #5123-class: statuses like "blocked" / "in-progress" used to be
        // recorded as pending with a success receipt on the canonical
        // progress surface.
        let tool = TodoWriteTool::new(new_shared_todo_list());
        let context = ToolContext::new(std::env::temp_dir());
        let err = tool
            .execute(
                json!({"todos": [{ "content": "x", "status": "blocked" }]}),
                &context,
            )
            .await
            .expect_err("unknown status must fail fast");
        assert!(format!("{err}").contains("unknown todo status"), "{err}");

        // Common near-misses resolve via the synonym table.
        assert_eq!(TodoStatus::parse("complete"), Some(TodoStatus::Completed));
        assert_eq!(
            TodoStatus::parse("in-progress"),
            Some(TodoStatus::InProgress)
        );
        assert_eq!(TodoStatus::parse("blocked"), None);
    }

    #[tokio::test]
    async fn work_update_returns_canonical_task_update_metadata() {
        let tool = TodoWriteTool::new(new_shared_todo_list());
        let context = ToolContext::new(std::env::temp_dir());
        let result = tool
            .execute(
                json!({
                    "todos": [
                        { "content": "wire durable task tools", "status": "in_progress" },
                        { "content": "run gates", "status": "pending" }
                    ]
                }),
                &context,
            )
            .await
            .expect("work_update succeeds");

        assert!(tool.model_visible());
        let metadata = result.metadata.expect("metadata");
        assert_eq!(metadata["canonical_tool"], "todo_write");
        assert_eq!(metadata["work_surface"]["canonical"], "work");
        assert_eq!(metadata["work_surface"]["model_visible"], true);
        assert_eq!(
            metadata["work_surface"]["durable_owner"],
            "fleet_workflow_ledger"
        );
        assert_eq!(
            metadata["work_surface"]["progress_key"],
            "task_updates.checklist"
        );
        assert_eq!(
            metadata["task_updates"]["checklist"]["in_progress_id"],
            json!(1)
        );
        assert_eq!(
            metadata["task_updates"]["checklist"]["items"][0]["content"],
            "wire durable task tools"
        );
    }

    #[tokio::test]
    async fn work_update_routes_through_attached_graph() {
        let todos = new_shared_todo_list();
        let plan = crate::tools::plan::new_shared_plan_state();
        let work = crate::work_graph::new_shared_work_runtime(todos.clone(), plan);
        let mut context = ToolContext::new(std::env::temp_dir());
        context.runtime.work = Some(work.clone());

        TodoWriteTool::new(todos.clone())
            .execute(
                json!({"todos": [
                    {"content": "Graph-owned", "status": "completed"},
                    {"content": "Discarded branch", "status": "cancelled"}
                ]}),
                &context,
            )
            .await
            .expect("second work_update");

        let state = work
            .capture(Some(&context.state_namespace))
            .expect("capture")
            .expect("graph state");
        assert_eq!(state.todos.items[0].status, TodoStatus::Completed);
        assert_eq!(state.todos.items[1].status, TodoStatus::Cancelled);
        assert_eq!(state.todos.completion_pct, 100);
        let node = state
            .graph
            .node(&state.graph.compat.todos[0].node)
            .expect("projected node");
        assert_eq!(node.state, crate::work_graph::NodeState::Completed);
        let cancelled_node = state
            .graph
            .node(&state.graph.compat.todos[1].node)
            .expect("cancelled projected node");
        assert_eq!(
            cancelled_node.state,
            crate::work_graph::NodeState::Cancelled
        );
        assert!(todos.lock().await.snapshot().is_empty());
        assert_eq!(work.publish_pending().await, Ok(true));
        assert_eq!(
            todos.lock().await.snapshot().items[0].status,
            TodoStatus::Completed
        );
        assert_eq!(
            todos.lock().await.snapshot().items[1].status,
            TodoStatus::Cancelled
        );
    }
}

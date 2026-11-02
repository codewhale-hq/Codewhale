//! `/v1/plan`, `/v1/todo`, `/v1/plans`, `/v1/todos`, and the per-thread pair —
//! read-only inventory over the plan/todo tool receipts the runtime store
//! already persists on turn items.
//!
//! There is no second plan store. An `update_plan` receipt carries the plan
//! payload; a `todo_write` / `work_update` receipt carries the whole
//! checklist. The newest receipt per thread is the live projection, and
//! earlier `update_plan` receipts in the same thread report
//! `superseded: true`. The projection mirrors the GPUI client
//! (`codewhale-gpui/src/plans.rs`) so both surfaces read the same receipts the
//! same way: match on `metadata.tool_name` (never on item `kind` — the store
//! files `todo_write` under `file_change` because the name contains "write")
//! and parse the args JSON out of `metadata.tool_input`, falling back to
//! `detail` for seeded history receipts that predate durable `tool_input`
//! (the same fallback restart history rebuild uses in runtime_threads.rs).
//!
//! **What this module does not do:** it does not read the live `PlanState` /
//! `SharedTodoList` — those are per-engine session state, not durable
//! receipts, and a headless Runtime may have no engine loaded for the thread.
//! To-do history is not tracked: only the newest checklist receipt projects,
//! matching the client. And the cross-thread routes pay one
//! `get_thread_detail` whole-store walk per scanned thread — the store keeps
//! no plan/todo index — so `limit` bounds how many of the newest threads are
//! scanned.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Serialize;
use serde_json::Value;

use codewhale_engine::runtime_threads::{TurnItemLifecycleStatus, TurnItemRecord};
use codewhale_engine::tools::plan::{PlanItemArg, PlanSnapshot};
use codewhale_engine::tools::todo::TodoStatus;

use super::{ApiError, RuntimeApiState, ThreadsQuery, map_thread_err, resolve_thread_filter};

/// One `update_plan` receipt projected for the API.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlanRevisionView {
    item_id: String,
    turn_id: String,
    /// Receipt lifecycle (`completed`, `failed`, …), not plan state.
    status: TurnItemLifecycleStatus,
    title: Option<String>,
    objective: Option<String>,
    explanation: Option<String>,
    recommended_approach: Option<String>,
    /// `PlanItemArg` already serializes as `{step, status}` with status in
    /// `pending|in_progress|completed`.
    steps: Vec<PlanItemArg>,
    /// True when a newer `update_plan` receipt in the same thread superseded
    /// this revision.
    superseded: bool,
}

/// A thread's current plan plus its revision history.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadPlanView {
    thread_id: String,
    /// Fields of the newest revision, flattened to the top level.
    #[serde(flatten)]
    current: PlanRevisionView,
    /// Every `update_plan` receipt, newest first; `revisions[0]` is `current`.
    revisions: Vec<PlanRevisionView>,
}

/// The newest revision of one thread's plan, for `/v1/plans` rows.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PlanEntryView {
    thread_id: String,
    #[serde(flatten)]
    revision: PlanRevisionView,
}

#[derive(Debug, Serialize)]
pub(super) struct PlansResponse {
    plans: Vec<PlanEntryView>,
}

#[derive(Debug, Serialize)]
pub(super) struct TodosResponse {
    todos: Vec<ThreadTodoView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct TodoItemView {
    content: String,
    status: TodoStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u32>,
}

/// A thread's latest checklist receipt projected for the API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ThreadTodoView {
    thread_id: String,
    item_id: Option<String>,
    turn_id: Option<String>,
    /// Receipt tool name (`todo_write`, `work_update`, …).
    tool: String,
    /// Receipt lifecycle (`completed`, `failed`, …), not list state.
    status: Option<TurnItemLifecycleStatus>,
    items: Vec<TodoItemView>,
    completion_pct: Option<u8>,
}

fn is_plan_tool(name: &str) -> bool {
    name == "update_plan"
}

/// Names a checklist receipt can persist under: the canonical `todo_write`,
/// its registered compat aliases (registry.rs `with_todo_tool`), and the
/// `todos` name the GPUI client also recognizes.
fn is_todo_tool(name: &str) -> bool {
    matches!(
        name,
        "todo_write"
            | "work_update"
            | "TodoWrite"
            | "todo"
            | "todos"
            | "checklist_write"
            | "checklist_update"
    )
}

fn tool_name(item: &TurnItemRecord) -> &str {
    item.metadata
        .as_ref()
        .and_then(|meta| meta.get("tool_name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The receipt's args JSON. Live calls persist it as a string under
/// `metadata.tool_input` at `ToolCallStarted` and carry it through completion;
/// seeded history receipts carry `tool_name` but leave the input in `detail`,
/// which is why the fallback exists only when `tool_input` is absent.
fn tool_input(item: &TurnItemRecord) -> Option<Value> {
    let raw = item
        .metadata
        .as_ref()
        .and_then(|meta| meta.get("tool_input"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| item.detail.clone())?;
    serde_json::from_str(&raw).ok()
}

/// Every `update_plan` receipt in the thread, oldest first, with `superseded`
/// set on all but the newest.
fn plan_revisions(items: &[TurnItemRecord]) -> Vec<PlanRevisionView> {
    let mut revisions = Vec::new();
    for item in items {
        if !is_plan_tool(tool_name(item)) {
            continue;
        }
        let Some(input) = tool_input(item) else {
            continue;
        };
        // The engine's canonical tolerant parser for this payload: it cleans
        // optional strings and coerces unknown/absent step status to Pending.
        let snapshot = PlanSnapshot::from_tool_input(&input);
        revisions.push(PlanRevisionView {
            item_id: item.id.clone(),
            turn_id: item.turn_id.clone(),
            status: item.status,
            title: snapshot.title,
            objective: snapshot.objective,
            explanation: snapshot.explanation,
            recommended_approach: snapshot.recommended_approach,
            steps: snapshot.items,
            superseded: false,
        });
    }
    if let Some(current_id) = revisions.last().map(|rev| rev.item_id.clone()) {
        for rev in &mut revisions {
            rev.superseded = rev.item_id != current_id;
        }
    }
    revisions
}

/// The newest checklist receipt in the thread, or `None` when it has none.
/// Each receipt replaces the whole list, so only the latest projects.
fn latest_todo(items: &[TurnItemRecord]) -> Option<ThreadTodoView> {
    let mut latest: Option<ThreadTodoView> = None;
    for item in items {
        let tool = tool_name(item);
        if !is_todo_tool(tool) {
            continue;
        }
        let Some(input) = tool_input(item) else {
            continue;
        };
        let todos = input
            .get("todos")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| {
                input
                    .pointer("/task_updates/checklist/items")
                    .and_then(Value::as_array)
                    .cloned()
            })
            .unwrap_or_default();
        let mut steps = Vec::new();
        for todo in &todos {
            let content = todo
                .get("content")
                .or_else(|| todo.get("text"))
                .or_else(|| todo.get("step"))
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default();
            if content.is_empty() {
                continue;
            }
            let status = todo
                .get("status")
                .and_then(Value::as_str)
                .and_then(TodoStatus::parse)
                .unwrap_or(TodoStatus::Pending);
            let id = todo
                .get("id")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok());
            steps.push(TodoItemView {
                content: content.to_string(),
                status,
                id,
            });
        }
        let completion_pct = input
            .get("completion_pct")
            .or_else(|| input.pointer("/task_updates/checklist/completion_pct"))
            .and_then(Value::as_u64)
            .map(|n| n.min(100) as u8)
            .or_else(|| {
                if steps.is_empty() {
                    None
                } else {
                    let settled = steps.iter().filter(|s| s.status.is_settled()).count();
                    Some(((settled * 100) / steps.len()) as u8)
                }
            });
        latest = Some(ThreadTodoView {
            thread_id: String::new(),
            item_id: Some(item.id.clone()),
            turn_id: Some(item.turn_id.clone()),
            tool: tool.to_string(),
            status: Some(item.status),
            items: steps,
            completion_pct,
        });
    }
    latest
}

/// `GET /v1/threads/{id}/plan` — the thread's latest plan and its revisions.
pub(super) async fn get_thread_plan(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<ThreadPlanView>, ApiError> {
    let detail = state
        .runtime_threads
        .get_thread_detail(&id)
        .await
        .map_err(map_thread_err)?;
    let mut revisions = plan_revisions(&detail.items);
    revisions.reverse();
    let Some(current) = revisions.first().cloned() else {
        return Err(ApiError::not_found(format!("Thread '{id}' has no plan")));
    };
    Ok(Json(ThreadPlanView {
        thread_id: id,
        current,
        revisions,
    }))
}

/// `GET /v1/threads/{id}/todo` — the thread's latest checklist.
pub(super) async fn get_thread_todo(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<ThreadTodoView>, ApiError> {
    let detail = state
        .runtime_threads
        .get_thread_detail(&id)
        .await
        .map_err(map_thread_err)?;
    let Some(mut todo) = latest_todo(&detail.items) else {
        return Err(ApiError::not_found(format!(
            "Thread '{id}' has no todo list"
        )));
    };
    todo.thread_id = id;
    Ok(Json(todo))
}

/// `GET /v1/plan` — the latest plan on the most recently updated thread that
/// has one. Threads are already newest-first; the first hit wins.
pub(super) async fn latest_plan(
    State(state): State<RuntimeApiState>,
    Query(query): Query<ThreadsQuery>,
) -> Result<Json<ThreadPlanView>, ApiError> {
    let filter = resolve_thread_filter(query.include_archived, query.archived_only);
    let threads = state
        .runtime_threads
        .list_threads(filter, query.limit)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    for thread in threads {
        let detail = state
            .runtime_threads
            .get_thread_detail(&thread.id)
            .await
            .map_err(map_thread_err)?;
        let mut revisions = plan_revisions(&detail.items);
        if revisions.is_empty() {
            continue;
        }
        revisions.reverse();
        let current = revisions[0].clone();
        return Ok(Json(ThreadPlanView {
            thread_id: thread.id,
            current,
            revisions,
        }));
    }
    Err(ApiError::not_found("No thread has a plan"))
}

/// `GET /v1/todo` — the latest checklist on the most recently updated thread
/// that has one.
pub(super) async fn latest_todo_route(
    State(state): State<RuntimeApiState>,
    Query(query): Query<ThreadsQuery>,
) -> Result<Json<ThreadTodoView>, ApiError> {
    let filter = resolve_thread_filter(query.include_archived, query.archived_only);
    let threads = state
        .runtime_threads
        .list_threads(filter, query.limit)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    for thread in threads {
        let detail = state
            .runtime_threads
            .get_thread_detail(&thread.id)
            .await
            .map_err(map_thread_err)?;
        if let Some(mut todo) = latest_todo(&detail.items) {
            todo.thread_id = thread.id;
            return Ok(Json(todo));
        }
    }
    Err(ApiError::not_found("No thread has a todo list"))
}

/// `GET /v1/plans` — every scanned thread's latest plan, newest thread first.
pub(super) async fn list_plans(
    State(state): State<RuntimeApiState>,
    Query(query): Query<ThreadsQuery>,
) -> Result<Json<PlansResponse>, ApiError> {
    let filter = resolve_thread_filter(query.include_archived, query.archived_only);
    let threads = state
        .runtime_threads
        .list_threads(filter, query.limit)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut plans = Vec::new();
    for thread in threads {
        let detail = state
            .runtime_threads
            .get_thread_detail(&thread.id)
            .await
            .map_err(map_thread_err)?;
        if let Some(revision) = plan_revisions(&detail.items).into_iter().last() {
            plans.push(PlanEntryView {
                thread_id: thread.id,
                revision,
            });
        }
    }
    Ok(Json(PlansResponse { plans }))
}

/// `GET /v1/todos` — every scanned thread's latest checklist, newest thread
/// first.
pub(super) async fn list_todos(
    State(state): State<RuntimeApiState>,
    Query(query): Query<ThreadsQuery>,
) -> Result<Json<TodosResponse>, ApiError> {
    let filter = resolve_thread_filter(query.include_archived, query.archived_only);
    let threads = state
        .runtime_threads
        .list_threads(filter, query.limit)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut todos = Vec::new();
    for thread in threads {
        let detail = state
            .runtime_threads
            .get_thread_detail(&thread.id)
            .await
            .map_err(map_thread_err)?;
        if let Some(mut todo) = latest_todo(&detail.items) {
            todo.thread_id = thread.id;
            todos.push(todo);
        }
    }
    Ok(Json(TodosResponse { todos }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use codewhale_engine::runtime_threads::TurnItemKind;
    use serde_json::json;

    fn receipt(id: &str, tool: &str, input: &Value) -> TurnItemRecord {
        let now = Utc::now();
        TurnItemRecord {
            schema_version: 2,
            id: id.to_string(),
            turn_id: "turn_1".to_string(),
            kind: TurnItemKind::ToolCall,
            status: TurnItemLifecycleStatus::Completed,
            summary: tool.to_string(),
            detail: Some("tool output".to_string()),
            metadata: Some(json!({
                "tool_use_id": format!("call_{id}"),
                "tool_name": tool,
                "tool_input": input.to_string(),
            })),
            artifact_refs: Vec::new(),
            artifacts: Vec::new(),
            started_at: Some(now),
            ended_at: Some(now),
        }
    }

    #[test]
    fn plan_revisions_mark_all_but_newest_superseded() {
        let items = vec![
            receipt(
                "p1",
                "update_plan",
                &json!({
                    "title": "First",
                    "plan": [{"step": "A", "status": "completed"}]
                }),
            ),
            receipt("other", "shell", &json!({"cmd": "pwd"})),
            receipt(
                "p2",
                "update_plan",
                &json!({
                    "objective": "Ship it",
                    "plan": [
                        {"step": "Probe", "status": "completed"},
                        {"step": "Land", "status": "in_progress"}
                    ]
                }),
            ),
        ];
        let revisions = plan_revisions(&items);
        assert_eq!(revisions.len(), 2);
        assert!(revisions[0].superseded);
        assert!(!revisions[1].superseded);
        assert_eq!(revisions[1].item_id, "p2");
        assert_eq!(revisions[1].objective.as_deref(), Some("Ship it"));
        assert_eq!(revisions[1].steps.len(), 2);
        assert_eq!(
            revisions[1].steps[1].status,
            codewhale_engine::tools::plan::StepStatus::InProgress
        );
    }

    #[test]
    fn latest_todo_keeps_only_the_newest_receipt() {
        let items = vec![
            receipt(
                "t1",
                "todo_write",
                &json!({"todos": [{"id": 1, "content": "old", "status": "pending"}]}),
            ),
            receipt(
                "t2",
                "work_update",
                &json!({
                    "todos": [
                        {"id": 1, "content": "probe", "status": "completed"},
                        {"id": 2, "content": "land", "status": "in_progress"},
                        {"id": 3, "content": "push", "status": "pending"}
                    ]
                }),
            ),
        ];
        let todo = latest_todo(&items).expect("a todo receipt exists");
        assert_eq!(todo.item_id.as_deref(), Some("t2"));
        assert_eq!(todo.tool, "work_update");
        assert_eq!(todo.items.len(), 3);
        assert_eq!(todo.items[1].status, TodoStatus::InProgress);
        assert_eq!(todo.completion_pct, Some(33));
        assert!(latest_todo(&items[..0]).is_none());
    }

    #[test]
    fn seeded_receipt_without_tool_input_reads_detail() {
        let mut item = receipt(
            "seeded",
            "update_plan",
            &json!({"title": "Seeded", "plan": [{"step": "A", "status": "pending"}]}),
        );
        // Seeded history carries the args JSON in `detail`, not
        // `metadata.tool_input` (runtime_threads.rs SeedItem::ToolUse).
        let input = item
            .metadata
            .as_ref()
            .and_then(|m| m.get("tool_input"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        item.detail = Some(input);
        item.metadata = Some(json!({"tool_use_id": "call_seeded", "tool_name": "update_plan"}));
        let revisions = plan_revisions(&[item]);
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].title.as_deref(), Some("Seeded"));
    }
}

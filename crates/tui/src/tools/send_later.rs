//! Model-callable one-shot delayed continuation tool (`send_later`).
//!
//! Lets the agent schedule a future message into the current workspace.  When
//! the trigger fires the scheduler enqueues a normal durable task with the
//! specified message, just like an automation run.
//!
//! # Actions
//!
//! | action     | description                                               |
//! |------------|-----------------------------------------------------------|
//! | `schedule` | Create a pending trigger; returns `trigger_id`+`fire_at` |
//! | `list`     | List recent triggers (default: 50 newest)                 |
//! | `read`     | Read a single trigger by `trigger_id`                     |
//! | `cancel`   | Cancel a pending trigger before it fires                  |

use std::path::PathBuf;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use crate::automation_manager::{
    CreateDelayedTriggerRequest, DelayedTriggerStatus, SharedAutomationManager,
};
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, optional_u64,
};

const ALL_ACTIONS: &[&str] = &["schedule", "list", "read", "cancel"];
const READ_ACTIONS: &[&str] = &["list", "read"];

/// One-shot delayed-continuation tool.
pub struct SendLaterTool {
    name: &'static str,
    read_only: bool,
}

impl SendLaterTool {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            read_only: false,
        }
    }

    /// Plan-mode variant: only the read-only actions are advertised.
    pub const fn read_only(name: &'static str) -> Self {
        Self {
            name,
            read_only: true,
        }
    }

    fn allowed_actions(&self) -> &'static [&'static str] {
        if self.read_only {
            READ_ACTIONS
        } else {
            ALL_ACTIONS
        }
    }

    fn resolve_action<'a>(&self, input: &'a Value) -> Result<&'a str, ToolError> {
        let action = input.get("action").and_then(Value::as_str).ok_or_else(|| {
            ToolError::invalid_input(format!(
                "send_later: missing `action` (one of: {})",
                self.allowed_actions().join(", ")
            ))
        })?;
        if self.allowed_actions().contains(&action) {
            Ok(action)
        } else {
            Err(ToolError::invalid_input(format!(
                "send_later: invalid action `{action}` (one of: {})",
                self.allowed_actions().join(", ")
            )))
        }
    }

    fn automations_from_context(
        context: &ToolContext,
    ) -> Result<SharedAutomationManager, ToolError> {
        context
            .runtime
            .automations
            .as_ref()
            .cloned()
            .ok_or_else(|| ToolError::not_available("send_later: automation manager not available"))
    }
}

#[async_trait]
impl ToolSpec for SendLaterTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        if self.read_only {
            "Inspect pending one-shot delayed continuations. Actions: \"list\" (newest triggers), \"read\" (one trigger by trigger_id)."
        } else {
            "Schedule a one-shot delayed message. By default it arrives as a new turn in this thread. \
Actions: \"schedule\" (create a pending trigger; requires approval), \
\"list\" (recent triggers), \"read\" (one trigger by trigger_id), \
\"cancel\" (cancel a pending trigger before it fires; requires approval). \
Use delay_minutes or fire_at (ISO 8601 UTC) — not both. \
Returns trigger_id and resolved fire_at."
        }
    }

    fn input_schema(&self) -> Value {
        let actions: Vec<&str> = self.allowed_actions().to_vec();
        let mut properties = serde_json::Map::new();
        properties.insert(
            "action".to_string(),
            json!({
                "type": "string",
                "enum": actions,
                "description": "Action to perform."
            }),
        );
        if !self.read_only {
            properties.insert(
                "delay_minutes".to_string(),
                json!({
                    "type": "integer",
                    "minimum": 1,
                    "description": "Minutes from now to fire. Mutually exclusive with fire_at. (action=schedule)"
                }),
            );
            properties.insert(
                "fire_at".to_string(),
                json!({
                    "type": "string",
                    "description": "Absolute UTC fire time as an ISO 8601 string, e.g. \"2026-07-08T00:43:00Z\". Mutually exclusive with delay_minutes. (action=schedule)"
                }),
            );
            properties.insert(
                "message".to_string(),
                json!({
                    "type": "string",
                    "description": "The message to inject as a new task when the trigger fires. (action=schedule)"
                }),
            );
            properties.insert(
                "workspace".to_string(),
                json!({
                    "type": "string",
                    "description": "Optional working directory for the fired task; defaults to the current workspace. (action=schedule)"
                }),
            );
            properties.insert(
                "thread_id".to_string(),
                json!({
                    "type": "string",
                    "description": "Thread the message is delivered into as a new turn. Defaults to the calling thread, so the reminder comes back to this conversation; \"current\" names it explicitly and \"\" starts a new background task thread instead. (action=schedule)"
                }),
            );
            properties.insert(
                "parent_trigger_id".to_string(),
                json!({
                    "type": "string",
                    "description": "Optional id of the trigger that re-armed this one, for lineage tracking. (action=schedule)"
                }),
            );
        }
        properties.insert(
            "trigger_id".to_string(),
            json!({
                "type": "string",
                "description": "Target trigger id. (action=read/cancel)"
            }),
        );
        properties.insert(
            "limit".to_string(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 200,
                "default": 50,
                "description": "Maximum number of results to return. (action=list)"
            }),
        );
        properties.insert(
            "status".to_string(),
            json!({
                "type": "string",
                "enum": ["pending", "dispatching", "fired", "canceled", "failed"],
                "description": "Filter by trigger status. (action=list)"
            }),
        );
        json!({
            "type": "object",
            "properties": properties,
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        if self.read_only {
            vec![ToolCapability::ReadOnly]
        } else {
            vec![
                ToolCapability::ExecutesCode,
                ToolCapability::RequiresApproval,
            ]
        }
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        if self.read_only {
            ApprovalRequirement::Auto
        } else {
            ApprovalRequirement::Required
        }
    }

    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        match input.get("action").and_then(Value::as_str) {
            Some("list") | Some("read") => ApprovalRequirement::Auto,
            _ if self.read_only => ApprovalRequirement::Auto,
            _ => ApprovalRequirement::Required,
        }
    }

    fn is_read_only_for(&self, input: &Value) -> bool {
        match input.get("action").and_then(Value::as_str) {
            Some("list") | Some("read") => true,
            _ => self.read_only,
        }
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let action = self.resolve_action(&input)?;

        match action {
            "schedule" => execute_schedule(&input, context).await,
            "list" => execute_list(&input, context).await,
            "read" => execute_read(&input, context).await,
            "cancel" => execute_cancel(&input, context).await,
            _ => Err(ToolError::invalid_input(format!(
                "send_later: unhandled action `{action}`"
            ))),
        }
    }
}

// ── Action handlers ────────────────────────────────────────────────────────

async fn execute_schedule(input: &Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
    let automations = SendLaterTool::automations_from_context(context)?;

    let delay_minutes = input.get("delay_minutes").and_then(Value::as_u64);
    let fire_at_str = optional_str(input, "fire_at")?;
    let message = input
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::invalid_input("send_later schedule: `message` is required"))?;

    if message.trim().is_empty() {
        return Err(ToolError::invalid_input(
            "send_later schedule: `message` must not be empty",
        ));
    }

    // Validate mutual exclusivity and resolve fire_at.
    let fire_at: DateTime<Utc> = match (delay_minutes, fire_at_str) {
        (Some(_), Some(_)) => {
            return Err(ToolError::invalid_input(
                "send_later schedule: `delay_minutes` and `fire_at` are mutually exclusive",
            ));
        }
        (None, None) => {
            return Err(ToolError::invalid_input(
                "send_later schedule: one of `delay_minutes` or `fire_at` is required",
            ));
        }
        (Some(minutes), None) => {
            if minutes == 0 {
                return Err(ToolError::invalid_input(
                    "send_later schedule: `delay_minutes` must be >= 1",
                ));
            }
            let minutes_i64 = i64::try_from(minutes).map_err(|_| {
                ToolError::invalid_input("send_later schedule: `delay_minutes` is too large")
            })?;
            Utc::now() + Duration::minutes(minutes_i64)
        }
        (None, Some(fire_at_s)) => fire_at_s.parse::<DateTime<Utc>>().map_err(|err| {
            ToolError::invalid_input(format!(
                "send_later schedule: invalid `fire_at` — expected ISO 8601 UTC, e.g. \
                     \"2026-07-08T00:43:00Z\": {err}"
            ))
        })?,
    };

    let workspace: Option<PathBuf> = optional_str(input, "workspace")?
        .map(PathBuf::from)
        .or_else(|| {
            let ws = &context.workspace;
            if ws == std::path::Path::new(".") {
                None
            } else {
                Some(ws.clone())
            }
        });

    let parent_trigger_id = optional_str(input, "parent_trigger_id")?.map(String::from);

    let destination_thread_id = crate::tools::automation::destination_thread(
        input,
        context,
        context.runtime.active_thread_id.clone(),
    )?;

    let req = CreateDelayedTriggerRequest {
        destination_thread_id,
        fire_at,
        message: message.to_string(),
        workspace,
        owner_session_id: Some(context.state_namespace.clone()),
        parent_trigger_id,
    };

    let record = {
        let manager = automations.lock().await;
        // A pending trigger is fired only by the scope that owns it
        // (`fire_due_triggers`), and a trigger has no paused state to fall
        // back to, so an unbound host must refuse rather than store a delayed
        // message that never arrives.
        crate::tools::automation::require_dispatch_owner(&manager, "schedule a delayed message")?;
        manager.create_trigger(req).map_err(|err| {
            ToolError::execution_failed(format!("send_later schedule failed: {err}"))
        })?
    };

    Ok(ToolResult::success(
        serde_json::to_string_pretty(&json!({
            "trigger_id": record.trigger_id,
            "fire_at": record.fire_at.to_rfc3339(),
            "status": "pending",
            "thread_id": record.destination_thread_id,
        }))
        .unwrap_or_default(),
    ))
}

async fn execute_list(input: &Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
    let automations = SendLaterTool::automations_from_context(context)?;

    let limit = Some(optional_u64(input, "limit", 50)? as usize);
    let status_filter = optional_str(input, "status")?
        .map(parse_trigger_status)
        .transpose()
        .map_err(ToolError::invalid_input)?;

    let records = {
        let manager = automations.lock().await;
        manager
            .list_triggers_for_owner(status_filter, limit, &context.state_namespace)
            .map_err(|err| ToolError::execution_failed(format!("send_later list failed: {err}")))?
    };

    let items: Vec<Value> = records
        .iter()
        .map(|r| {
            json!({
                "trigger_id": r.trigger_id,
                "fire_at": r.fire_at.to_rfc3339(),
                "status": trigger_status_str(r.status),
                "created_at": r.created_at.to_rfc3339(),
                "message_preview": r.message.chars().take(120).collect::<String>(),
                "workspace": r.workspace,
                "parent_trigger_id": r.parent_trigger_id,
                "destination_thread_id": r.destination_thread_id,
                "task_id": r.task_id,
                "error": r.error,
            })
        })
        .collect();

    let count = items.len();
    Ok(ToolResult::success(
        serde_json::to_string_pretty(&json!({ "triggers": items, "count": count }))
            .unwrap_or_default(),
    ))
}

async fn execute_read(input: &Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
    let automations = SendLaterTool::automations_from_context(context)?;

    let trigger_id = input
        .get("trigger_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::invalid_input("send_later read: `trigger_id` is required"))?;

    let record = {
        let manager = automations.lock().await;
        manager
            .get_trigger_for_owner(trigger_id, &context.state_namespace)
            .map_err(|err| ToolError::execution_failed(format!("send_later read failed: {err}")))?
    };

    Ok(ToolResult::success(
        serde_json::to_string_pretty(&json!({
            "trigger_id": record.trigger_id,
            "fire_at": record.fire_at.to_rfc3339(),
            "status": trigger_status_str(record.status),
            "created_at": record.created_at.to_rfc3339(),
            "fired_at": record.fired_at.map(|t| t.to_rfc3339()),
            "message": record.message,
            "workspace": record.workspace,
            "parent_trigger_id": record.parent_trigger_id,
            "destination_thread_id": record.destination_thread_id,
            "task_id": record.task_id,
            "thread_id": record.thread_id,
            "error": record.error,
        }))
        .unwrap_or_default(),
    ))
}

async fn execute_cancel(input: &Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
    let automations = SendLaterTool::automations_from_context(context)?;

    let trigger_id = input
        .get("trigger_id")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::invalid_input("send_later cancel: `trigger_id` is required"))?;

    let record = {
        let manager = automations.lock().await;
        manager
            .cancel_trigger_for_owner(trigger_id, &context.state_namespace)
            .map_err(|err| {
                ToolError::execution_failed(format!("send_later cancel failed: {err}"))
            })?
    };

    Ok(ToolResult::success(
        serde_json::to_string_pretty(&json!({
            "trigger_id": record.trigger_id,
            "status": "canceled",
            "fire_at": record.fire_at.to_rfc3339(),
        }))
        .unwrap_or_default(),
    ))
}

fn parse_trigger_status(s: &str) -> Result<DelayedTriggerStatus, String> {
    match s {
        "pending" => Ok(DelayedTriggerStatus::Pending),
        "dispatching" => Ok(DelayedTriggerStatus::Dispatching),
        "fired" => Ok(DelayedTriggerStatus::Fired),
        "canceled" => Ok(DelayedTriggerStatus::Canceled),
        "failed" => Ok(DelayedTriggerStatus::Failed),
        other => Err(format!(
            "unknown trigger status '{other}'; expected one of: pending, dispatching, fired, canceled, failed"
        )),
    }
}

fn trigger_status_str(status: DelayedTriggerStatus) -> &'static str {
    match status {
        DelayedTriggerStatus::Pending => "pending",
        DelayedTriggerStatus::Dispatching => "dispatching",
        DelayedTriggerStatus::Fired => "fired",
        DelayedTriggerStatus::Canceled => "canceled",
        DelayedTriggerStatus::Failed => "failed",
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Duration;
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    use crate::automation_manager::{AutomationManager, DelayedTriggerStatus};
    use crate::tools::spec::{RuntimeToolServices, ToolContext, ToolSpec};

    use super::SendLaterTool;

    fn make_context(tmp: &TempDir) -> ToolContext {
        make_context_for_session(tmp, "test-session")
    }

    fn make_context_for_session(tmp: &TempDir, session_id: &str) -> ToolContext {
        let manager = AutomationManager::open_for_test(tmp.path().to_path_buf()).unwrap();
        let shared = Arc::new(Mutex::new(manager));
        ToolContext::new(".")
            .with_state_namespace(session_id)
            .with_runtime_services(RuntimeToolServices {
                automations: Some(shared),
                ..Default::default()
            })
    }

    #[tokio::test]
    async fn schedule_with_delay_minutes_succeeds() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        let result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 60,
                    "message": "Check CI status on open PRs."
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.content.contains("trigger_id"));
        assert!(result.content.contains("fire_at"));
        assert!(result.content.contains("pending"));
    }

    #[tokio::test]
    async fn schedule_with_absolute_fire_at_succeeds() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        let fire_at = (chrono::Utc::now() + Duration::minutes(30)).to_rfc3339();
        let result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "fire_at": fire_at,
                    "message": "Check PR mergeability."
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.content.contains("trig_"));
    }

    #[tokio::test]
    async fn schedule_rejects_mutually_exclusive_inputs() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        let fire_at = (chrono::Utc::now() + Duration::minutes(30)).to_rfc3339();
        let err = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 60,
                    "fire_at": fire_at,
                    "message": "Should fail."
                }),
                &ctx,
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[tokio::test]
    async fn schedule_rejects_missing_timing_input() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        let err = tool
            .execute(
                json!({
                    "action": "schedule",
                    "message": "No timing provided."
                }),
                &ctx,
            )
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("required"),
            "expected required-timing error, got: {err}"
        );
    }

    #[tokio::test]
    async fn schedule_rejects_past_fire_at() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        let fire_at = (chrono::Utc::now() - Duration::minutes(5)).to_rfc3339();
        let err = tool
            .execute(
                json!({
                    "action": "schedule",
                    "fire_at": fire_at,
                    "message": "This is in the past."
                }),
                &ctx,
            )
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("future"),
            "expected future-time error, got: {err}"
        );
    }

    #[tokio::test]
    async fn schedule_rejects_malformed_fire_at() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        let err = tool
            .execute(
                json!({
                    "action": "schedule",
                    "fire_at": "not-a-timestamp",
                    "message": "Bad time."
                }),
                &ctx,
            )
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("invalid `fire_at`"),
            "expected parse error, got: {err}"
        );
    }

    #[tokio::test]
    async fn list_and_read_round_trip() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        // Schedule a trigger.
        let sched_result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 15,
                    "message": "PR watcher check-in."
                }),
                &ctx,
            )
            .await
            .unwrap();

        let sched_val: serde_json::Value = serde_json::from_str(&sched_result.content).unwrap();
        let trigger_id = sched_val["trigger_id"].as_str().unwrap();

        // List should include it.
        let list_result = tool
            .execute(json!({ "action": "list" }), &ctx)
            .await
            .unwrap();
        assert!(list_result.content.contains(trigger_id));

        // Read should return full detail.
        let read_result = tool
            .execute(json!({ "action": "read", "trigger_id": trigger_id }), &ctx)
            .await
            .unwrap();
        assert!(read_result.content.contains("PR watcher check-in."));
    }

    #[tokio::test]
    async fn cancel_pending_trigger() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        let sched_result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 30,
                    "message": "Will be canceled."
                }),
                &ctx,
            )
            .await
            .unwrap();

        let trigger_id = serde_json::from_str::<serde_json::Value>(&sched_result.content).unwrap()
            ["trigger_id"]
            .as_str()
            .unwrap()
            .to_string();

        let cancel_result = tool
            .execute(
                json!({ "action": "cancel", "trigger_id": &trigger_id }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(cancel_result.content.contains("canceled"));

        // Canceling again should fail.
        let err = tool
            .execute(
                json!({ "action": "cancel", "trigger_id": &trigger_id }),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("canceled"));
    }

    #[tokio::test]
    async fn list_with_status_filter() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        // Schedule one trigger.
        let sched_result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 10,
                    "message": "Pending trigger."
                }),
                &ctx,
            )
            .await
            .unwrap();
        let trigger_id = serde_json::from_str::<serde_json::Value>(&sched_result.content).unwrap()
            ["trigger_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Cancel it.
        tool.execute(
            json!({ "action": "cancel", "trigger_id": &trigger_id }),
            &ctx,
        )
        .await
        .unwrap();

        // List pending: should be empty.
        let pending_list = tool
            .execute(json!({ "action": "list", "status": "pending" }), &ctx)
            .await
            .unwrap();
        let pending_val: serde_json::Value = serde_json::from_str(&pending_list.content).unwrap();
        assert_eq!(pending_val["count"].as_u64().unwrap(), 0);

        // List canceled: should contain our trigger.
        let canceled_list = tool
            .execute(json!({ "action": "list", "status": "canceled" }), &ctx)
            .await
            .unwrap();
        assert!(canceled_list.content.contains(&trigger_id));
    }

    #[tokio::test]
    async fn parent_trigger_id_is_preserved() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        // First trigger (the "parent").
        let parent_result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 60,
                    "message": "First check-in."
                }),
                &ctx,
            )
            .await
            .unwrap();
        let parent_id = serde_json::from_str::<serde_json::Value>(&parent_result.content).unwrap()
            ["trigger_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Re-armed trigger referencing the parent.
        let child_result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 60,
                    "message": "Second check-in (re-arm).",
                    "parent_trigger_id": &parent_id,
                }),
                &ctx,
            )
            .await
            .unwrap();
        let child_id =
            serde_json::from_str::<serde_json::Value>(&child_result.content).unwrap()["trigger_id"]
                .as_str()
                .unwrap()
                .to_string();

        let read_result = tool
            .execute(json!({ "action": "read", "trigger_id": &child_id }), &ctx)
            .await
            .unwrap();
        assert!(read_result.content.contains(&parent_id));
    }

    #[tokio::test]
    async fn persistence_survives_manager_reload() {
        let tmp = TempDir::new().unwrap();
        let ctx = make_context(&tmp);
        let tool = SendLaterTool::new("send_later");

        let sched_result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 45,
                    "message": "Persisted trigger."
                }),
                &ctx,
            )
            .await
            .unwrap();
        let trigger_id = serde_json::from_str::<serde_json::Value>(&sched_result.content).unwrap()
            ["trigger_id"]
            .as_str()
            .unwrap()
            .to_string();

        // Open a fresh manager over the same directory to simulate restart.
        let ctx2 = make_context(&tmp);
        let read_result = tool
            .execute(
                json!({ "action": "read", "trigger_id": &trigger_id }),
                &ctx2,
            )
            .await
            .unwrap();
        assert!(read_result.content.contains("Persisted trigger."));
    }

    #[tokio::test]
    async fn trigger_controls_are_session_owned_and_legacy_records_fail_closed() {
        let tmp = TempDir::new().unwrap();
        let session_a = make_context_for_session(&tmp, "session-a");
        let session_b = make_context_for_session(&tmp, "session-b");
        let tool = SendLaterTool::new("send_later");

        let session_b_result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 30,
                    "message": "session B continuation"
                }),
                &session_b,
            )
            .await
            .unwrap();
        let session_b_id = serde_json::from_str::<serde_json::Value>(&session_b_result.content)
            .unwrap()["trigger_id"]
            .as_str()
            .unwrap()
            .to_string();

        let session_a_result = tool
            .execute(
                json!({
                    "action": "schedule",
                    "delay_minutes": 30,
                    "message": "session A continuation"
                }),
                &session_a,
            )
            .await
            .unwrap();
        let session_a_id = serde_json::from_str::<serde_json::Value>(&session_a_result.content)
            .unwrap()["trigger_id"]
            .as_str()
            .unwrap()
            .to_string();

        let manager = AutomationManager::open_for_test(tmp.path().to_path_buf()).unwrap();
        let mut legacy = manager
            .create_trigger(crate::automation_manager::CreateDelayedTriggerRequest {
                destination_thread_id: None,
                fire_at: chrono::Utc::now() + Duration::hours(1),
                message: "ownerless legacy continuation".to_string(),
                workspace: None,
                owner_session_id: None,
                parent_trigger_id: None,
            })
            .unwrap();

        let session_b_list = tool
            .execute(json!({ "action": "list", "limit": 1 }), &session_b)
            .await
            .unwrap();
        assert!(session_b_list.content.contains(&session_b_id));
        assert!(!session_b_list.content.contains(&session_a_id));
        assert!(!session_b_list.content.contains(&legacy.trigger_id));

        for action in ["read", "cancel"] {
            let error = tool
                .execute(
                    json!({ "action": action, "trigger_id": &session_a_id }),
                    &session_b,
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("not found"), "{error}");
        }
        let still_pending = manager.get_trigger(&session_a_id).unwrap();
        assert_eq!(still_pending.status, DelayedTriggerStatus::Pending);

        let legacy_error = tool
            .execute(
                json!({ "action": "read", "trigger_id": &legacy.trigger_id }),
                &session_a,
            )
            .await
            .unwrap_err();
        assert!(legacy_error.to_string().contains("not found"));

        let restored_a = tool
            .execute(
                json!({ "action": "read", "trigger_id": &session_a_id }),
                &session_a,
            )
            .await
            .unwrap();
        assert!(
            restored_a.content.contains("session A continuation"),
            "switching A to B and back must restore A's controls"
        );

        let own_cancel = tool
            .execute(
                json!({ "action": "cancel", "trigger_id": &session_b_id }),
                &session_b,
            )
            .await
            .unwrap();
        assert!(own_cancel.content.contains("canceled"));

        legacy.fire_at = chrono::Utc::now() - Duration::minutes(1);
        manager.save_trigger(&legacy).unwrap();
        assert!(
            manager
                .collect_due_triggers(chrono::Utc::now())
                .unwrap()
                .iter()
                .all(|record| record.trigger_id != legacy.trigger_id),
            "ownerless legacy triggers must never fire"
        );
    }

    #[tokio::test]
    async fn collect_due_triggers_returns_past_pending() {
        let tmp = TempDir::new().unwrap();
        let manager = AutomationManager::open_for_test(tmp.path().to_path_buf()).unwrap();

        // Create a trigger with fire_at one hour from now — not due yet.
        let req = crate::automation_manager::CreateDelayedTriggerRequest {
            destination_thread_id: None,
            fire_at: chrono::Utc::now() + Duration::hours(1),
            message: "Not due yet.".to_string(),
            workspace: None,
            owner_session_id: Some("session-a".to_string()),
            parent_trigger_id: None,
        };
        let record = manager.create_trigger(req).unwrap();
        let due = manager.collect_due_triggers(chrono::Utc::now()).unwrap();
        assert!(due.is_empty(), "should not fire a future trigger");

        // Back-date the fire_at to the past and re-save.
        let mut past_record = record;
        past_record.fire_at = chrono::Utc::now() - Duration::minutes(5);
        manager.save_trigger(&past_record).unwrap();

        let due = manager.collect_due_triggers(chrono::Utc::now()).unwrap();
        assert_eq!(due.len(), 1, "should fire a past-due trigger");
    }

    /// A pending trigger is fired only by the scope that owns it, and a
    /// trigger has no paused state, so a one-shot host that attaches the
    /// store for inspection must refuse to schedule rather than store a
    /// delayed message that never arrives.
    #[tokio::test]
    async fn scheduling_requires_a_dispatch_owner() {
        let tmp = TempDir::new().unwrap();
        let manager = AutomationManager::open(tmp.path().to_path_buf()).unwrap();
        assert!(
            manager.execution_scope().is_none(),
            "fixture must model the unbound one-shot host"
        );
        let ctx = ToolContext::new(".").with_runtime_services(RuntimeToolServices {
            automations: Some(Arc::new(Mutex::new(manager))),
            ..Default::default()
        });
        let tool = SendLaterTool::new("send_later");

        let err = tool
            .execute(
                json!({"action": "schedule", "delay_minutes": 60, "message": "Check CI."}),
                &ctx,
            )
            .await
            .expect_err("must refuse without a dispatch owner");
        assert!(
            err.to_string().contains("persistent execution owner"),
            "{err}"
        );

        // Inspection still works against the attached store, and nothing was
        // persisted by the refused schedule.
        let listed = tool
            .execute(json!({"action": "list"}), &ctx)
            .await
            .expect("list must serve an attached store");
        assert!(
            AutomationManager::open(tmp.path().to_path_buf())
                .unwrap()
                .list_triggers(None, None)
                .unwrap()
                .is_empty(),
            "a refused schedule must not leave a trigger behind: {}",
            listed.content
        );
    }
}

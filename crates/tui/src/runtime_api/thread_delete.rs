use axum::Json;
use axum::extract::{Path, State};
use serde_json::{Value, json};

use codewhale_engine::automation_manager::{AutomationStatus, DelayedTriggerStatus};
use codewhale_engine::runtime_threads::manager::ThreadListFilter;
use codewhale_engine::session_manager::SessionManager;

use super::sessions::map_session_err;
use super::{ApiError, RuntimeApiState, map_thread_err};

pub(super) async fn delete_thread(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    {
        let automations = state.automations.lock().await;
        let mut scheduled: Vec<String> = automations
            .list_automations()
            .map_err(|e| ApiError::internal(format!("Failed to list automations: {e}")))?
            .into_iter()
            .filter(|a| {
                a.status == AutomationStatus::Active
                    && a.destination_thread_id.as_deref() == Some(id.as_str())
            })
            .map(|a| format!("automation {}", a.id))
            .collect();
        for status in [
            DelayedTriggerStatus::Pending,
            DelayedTriggerStatus::Dispatching,
        ] {
            scheduled.extend(
                automations
                    .list_triggers(Some(status), None)
                    .map_err(|e| ApiError::internal(format!("Failed to list triggers: {e}")))?
                    .into_iter()
                    .filter(|t| t.destination_thread_id.as_deref() == Some(id.as_str()))
                    .map(|t| format!("trigger {}", t.trigger_id)),
            );
        }
        if !scheduled.is_empty() {
            return Err(ApiError::conflict(format!(
                "thread {id} still receives scheduled messages ({}); cancel or pause them first",
                scheduled.join(", ")
            )));
        }
    }
    let thread = state
        .runtime_threads
        .delete_thread(&id)
        .await
        .map_err(map_thread_err)?;
    tracing::info!(thread_id = %id, "runtime thread deleted");
    let Some(session_id) = thread.session_id.clone() else {
        return Ok(Json(json!({ "deleted": id, "session_deleted": false })));
    };
    let shared = state
        .runtime_threads
        .list_threads(ThreadListFilter::IncludeArchived, None)
        .await
        .map_err(map_thread_err)?
        .iter()
        .any(|other| other.session_id.as_deref() == Some(session_id.as_str()));
    if shared {
        return Ok(Json(json!({
            "deleted": id,
            "session_id": session_id,
            "session_deleted": false,
            "session_kept": "another thread is bound to this saved session",
        })));
    }
    let sessions_dir = state.sessions_dir.clone();
    let target = session_id.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        let manager = SessionManager::new(sessions_dir)
            .map_err(|e| ApiError::internal(format!("Failed to open sessions dir: {e}")))?;
        manager
            .delete_session(&target)
            .map_err(|e| map_session_err(&target, e, "delete"))
    })
    .await
    .map_err(|_| ApiError::internal("session delete failed"))?;
    Ok(Json(match outcome {
        Ok(()) => json!({ "deleted": id, "session_id": session_id, "session_deleted": true }),
        Err(error) => json!({
            "deleted": id,
            "session_id": session_id,
            "session_deleted": false,
            "session_error": error.message,
        }),
    }))
}

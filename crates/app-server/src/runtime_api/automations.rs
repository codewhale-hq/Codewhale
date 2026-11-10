use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde::{Deserialize, Serialize};

use codewhale_engine::automation_manager::{
    AutomationMissedPolicy, AutomationMissedStatus, MissedRunDecision, resolve_missed_run_shared,
};

use super::{ApiError, RuntimeApiState, automation_mutation, map_automation_err};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct MissedPolicyRequest {
    #[serde(default)]
    policy: Option<AutomationMissedPolicy>,
}

pub(super) async fn list_missed(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Vec<AutomationMissedStatus>>, ApiError> {
    let manager = state.automations.lock().await;
    let missed = manager
        .list_missed()
        .map_err(|e| ApiError::internal(format!("Failed to list missed automations: {e}")))?;
    Ok(Json(missed))
}

pub(super) async fn get_missed(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<AutomationMissedStatus>, ApiError> {
    let manager = state.automations.lock().await;
    let status = manager.missed_status(&id).map_err(map_automation_err)?;
    Ok(Json(status))
}

pub(super) async fn set_missed_policy(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<MissedPolicyRequest>,
) -> Result<Response, ApiError> {
    automation_mutation(
        &state,
        &headers,
        &format!("PUT /v1/automations/{id}/missed/policy"),
        &req,
        StatusCode::OK,
        map_automation_err,
        async {
            state
                .automations
                .lock()
                .await
                .set_missed_policy(&id, req.policy)
        },
    )
    .await
}

pub(super) async fn run_missed(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    answer(state, id, headers, MissedRunDecision::RunNow, "run").await
}

pub(super) async fn skip_missed(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    answer(state, id, headers, MissedRunDecision::Skip, "skip").await
}

async fn answer(
    state: RuntimeApiState,
    id: String,
    headers: HeaderMap,
    decision: MissedRunDecision,
    action: &str,
) -> Result<Response, ApiError> {
    automation_mutation(
        &state,
        &headers,
        &format!("POST /v1/automations/{id}/missed/{action}"),
        &decision,
        StatusCode::OK,
        map_automation_err,
        resolve_missed_run_shared(&state.automations, &id, decision, &state.task_manager),
    )
    .await
}

use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, RuntimeApiState, map_thread_err};

const MEMBER_START_WAIT: Duration = Duration::from_secs(20);
const MAX_MEMBER_PROMPT_CHARS: usize = 256_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StartMemberRequest {
    operation_key: String,
    member_id: String,
    display_name: String,
    prompt: String,
    fleet: crate::fleet::store::FleetFile,
}

fn opaque(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

pub(super) async fn start_thread_member(
    State(state): State<RuntimeApiState>,
    Path(thread_id): Path<String>,
    Json(request): Json<StartMemberRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let display_name = request.display_name.trim();
    if !opaque(&request.operation_key) || !opaque(&request.member_id) {
        return Err(ApiError::bad_request(
            "operation_key and member_id must be 1-128 URL-safe characters",
        ));
    }
    if display_name.is_empty()
        || display_name.chars().count() > 80
        || display_name.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(
            "display_name must be one printable line of at most 80 characters",
        ));
    }
    let prompt = request.prompt.trim();
    if prompt.is_empty() || prompt.chars().count() > MAX_MEMBER_PROMPT_CHARS {
        return Err(ApiError::bad_request(
            "prompt must contain 1-256000 characters",
        ));
    }
    let engine = state
        .runtime_threads
        .get_engine(&thread_id)
        .await
        .map_err(map_thread_err)?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    engine
        .send(crate::core::ops::Op::StartFleetMember {
            start: Box::new(crate::tools::subagent::FleetMemberStart {
                operation_key: request.operation_key.clone(),
                fleet: request.fleet,
                member_id: request.member_id.clone(),
                display_name: display_name.to_string(),
                prompt: prompt.to_string(),
            }),
            tx: std::sync::Arc::new(std::sync::Mutex::new(Some(tx))),
        })
        .await
        .map_err(|err| ApiError::internal(format!("Failed to reach the thread's engine: {err}")))?;
    match tokio::time::timeout(MEMBER_START_WAIT, rx).await {
        Ok(Ok(Ok(agent_id))) => Ok((
            StatusCode::OK,
            Json(json!({
                "thread_id": thread_id,
                "member_id": request.member_id,
                "agent_id": agent_id,
                "state": "started",
            })),
        )),
        Ok(Ok(Err(message))) => Err(ApiError::conflict(message)),
        Ok(Err(_)) => Err(ApiError::internal(
            "The engine closed before starting the member",
        )),
        Err(_) => Ok((
            StatusCode::ACCEPTED,
            Json(json!({
                "thread_id": thread_id,
                "member_id": request.member_id,
                "operation_key": request.operation_key,
                "state": "queued",
            })),
        )),
    }
}

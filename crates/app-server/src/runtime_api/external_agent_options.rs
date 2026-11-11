use std::path::PathBuf;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;

use codewhale_engine::fleet::acp::AgentOptions;
use codewhale_engine::fleet::agents::ExecutorUnavailable;

use super::{ApiError, RuntimeApiState, external_agent_refusal};

const MAX_AGENT_ID_BYTES: usize = 64;
const MAX_CWD_BYTES: usize = 4096;

#[derive(Debug, Deserialize)]
pub(super) struct OptionsQuery {
    cwd: Option<PathBuf>,
}

fn valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_AGENT_ID_BYTES
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

pub(super) async fn get_external_agent_options(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(query): Query<OptionsQuery>,
) -> Result<Json<AgentOptions>, ApiError> {
    if !valid_agent_id(&id) {
        return Err(ApiError::bad_request(
            "the agent id must name an agent from GET /v1/agents/external",
        ));
    }
    let cwd = query
        .cwd
        .ok_or_else(|| ApiError::bad_request("cwd is required"))?;
    let text = cwd.to_string_lossy();
    if text.len() > MAX_CWD_BYTES || text.chars().any(char::is_control) || !cwd.is_absolute() {
        return Err(ApiError::bad_request(
            "cwd must be an absolute directory path",
        ));
    }
    let probe = cwd.clone();
    let is_dir = tokio::task::spawn_blocking(move || probe.is_dir())
        .await
        .map_err(|_| ApiError::internal("cwd check failed"))?;
    if !is_dir {
        return Err(ApiError::not_found(
            "cwd is not a directory on this computer",
        ));
    }
    match state
        .runtime_threads
        .external_agent_options(&id, &cwd)
        .await
    {
        Ok(options) => Ok(Json(options)),
        Err(error) => {
            let mut refusal = external_agent_refusal(&error);
            if let Some(refusal) = refusal.as_mut()
                && refusal.code == Some(ExecutorUnavailable::UNKNOWN_AGENT)
            {
                refusal.status = axum::http::StatusCode::NOT_FOUND;
            }
            Err(refusal.unwrap_or_else(|| ApiError::internal(error.to_string())))
        }
    }
}

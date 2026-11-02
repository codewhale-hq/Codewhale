//! Reviewed external MCP imports. Existing runtime bearer auth and body limits
//! apply; preview/apply never spawn a process, connect, or echo credential values.
use super::{ApiError, RuntimeApiState, mcp_expected_revision};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use codewhale_engine::mcp::external_import::{
    ImportContext, ImportDecision, ImportPreview, ImportReceipt, apply_reviewed_import,
    preview_imports,
};
use serde::Deserialize;

fn import_error(error: anyhow::Error) -> ApiError {
    ApiError {
        status: if error.is::<codewhale_engine::mcp::McpRevisionConflict>() {
            StatusCode::PRECONDITION_FAILED
        } else {
            StatusCode::CONFLICT
        },
        message: error.to_string(),
        code: None,
    }
}

pub(super) async fn preview(
    State(state): State<RuntimeApiState>,
) -> Result<Json<ImportPreview>, ApiError> {
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let path = state.config.read().mcp_config_path();
        let plugins = state
            .plugin_discovery
            .registry_for_workspace(&state.workspace);
        let context =
            ImportContext::new(&state.workspace, &path, plugins.as_ref()).map_err(import_error)?;
        preview_imports(&context).map(Json).map_err(import_error)
    })
    .await
    .map_err(|_| ApiError::internal("MCP import preview failed"))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ApplyRequest {
    id: String,
    content_hash: String,
    decision: ImportDecision,
}

impl ApplyRequest {
    fn validate(&self) -> Result<(), ApiError> {
        if ![&self.id, &self.content_hash]
            .iter()
            .all(|value| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
            || self.decision == ImportDecision::Skip
        {
            return Err(ApiError::bad_request(
                "Choose a reviewed candidate and approve or decline",
            ));
        }
        Ok(())
    }
}

pub(super) async fn apply(
    State(state): State<RuntimeApiState>,
    headers: HeaderMap,
    Json(request): Json<ApplyRequest>,
) -> Result<Json<ImportReceipt>, ApiError> {
    let expected = mcp_expected_revision(&headers)?;
    request.validate()?;
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let path = state.config.read().mcp_config_path();
        let plugins = state
            .plugin_discovery
            .registry_for_workspace(&state.workspace);
        let context =
            ImportContext::new(&state.workspace, &path, plugins.as_ref()).map_err(import_error)?;
        apply_reviewed_import(
            &context,
            &request.id,
            &request.content_hash,
            &expected,
            request.decision,
        )
        .map(Json)
        .map_err(import_error)
    })
    .await
    .map_err(|_| ApiError::internal("MCP import failed"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewed_import_request_requires_closed_shape_exact_identity_and_decision() {
        let request = serde_json::json!({"id": "a".repeat(64), "content_hash": "b".repeat(64), "decision": "approve"});
        assert!(
            serde_json::from_value::<ApplyRequest>(request.clone())
                .unwrap()
                .validate()
                .is_ok()
        );
        for (key, value) in [("id", "x"), ("content_hash", "z"), ("decision", "skip")] {
            let mut invalid = request.clone();
            invalid[key] = value.into();
            assert_eq!(
                serde_json::from_value::<ApplyRequest>(invalid)
                    .unwrap()
                    .validate()
                    .unwrap_err()
                    .status,
                StatusCode::BAD_REQUEST
            );
        }
        let mut unknown = request;
        unknown["path"] = "/arbitrary/source".into();
        assert!(serde_json::from_value::<ApplyRequest>(unknown).is_err());
    }

    #[test]
    fn reviewed_import_requires_concrete_configuration_revision() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            mcp_expected_revision(&headers).unwrap_err().status,
            StatusCode::PRECONDITION_REQUIRED
        );
        for invalid in ["*", "W/\"mcp-v1-absent\"", "mcp-v1-invalid"] {
            headers.insert(axum::http::header::IF_MATCH, invalid.parse().unwrap());
            assert_eq!(
                mcp_expected_revision(&headers).unwrap_err().status,
                StatusCode::BAD_REQUEST
            );
        }
        headers.insert(
            axum::http::header::IF_MATCH,
            "\"mcp-v1-absent\"".parse().unwrap(),
        );
        assert_eq!(mcp_expected_revision(&headers).unwrap(), "mcp-v1-absent");
    }
}

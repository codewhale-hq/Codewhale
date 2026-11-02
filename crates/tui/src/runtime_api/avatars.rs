//! Authenticated projection of the existing Native avatar registrations.
use super::RuntimeApiState;
use axum::{Json, extract::State};

pub(super) async fn list_avatars(
    State(state): State<RuntimeApiState>,
) -> Json<Vec<codewhale_protocol::avatar::RegisteredPack>> {
    Json(codewhale_engine::extension_host::avatars::catalog(state.workspace.clone()).await)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AtlasQuery {
    content_hash: String,
}

pub(super) async fn get_atlas(
    State(state): State<RuntimeApiState>,
    axum::extract::Path((handle, page)): axum::extract::Path<(u64, usize)>,
    axum::extract::Query(query): axum::extract::Query<AtlasQuery>,
) -> Result<Json<String>, axum::http::StatusCode> {
    codewhale_engine::extension_host::avatars::atlas(
        state.workspace.clone(),
        handle,
        page,
        &query.content_hash,
    )
    .await
    .map(Json)
    .ok_or(axum::http::StatusCode::NOT_FOUND)
}

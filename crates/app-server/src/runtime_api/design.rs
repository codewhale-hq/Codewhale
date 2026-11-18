use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};

use codewhale_engine::design::{DesignError, DesignStore};

use super::workspace::scoped_workspace;
use super::{ApiError, RuntimeApiState};

const RENDER_SCHEME_SOURCE: &str = "artboard:";

#[derive(Debug, Default, Deserialize)]
pub(super) struct DesignQuery {
    thread_id: Option<String>,
    base: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MoveRequest {
    x: i64,
    y: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CommentRequest {
    artboard_id: String,
    x: f64,
    y: f64,
    body: String,
    author: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ResolveRequest {
    resolved: bool,
}

fn map_err(err: DesignError) -> ApiError {
    match err {
        DesignError::Invalid(msg) => ApiError::bad_request(msg),
        DesignError::NotFound(msg) => ApiError::not_found(msg),
        DesignError::Io(msg) => ApiError::internal(msg),
    }
}

async fn with_store<T: Send + 'static>(
    state: &RuntimeApiState,
    thread_id: Option<&str>,
    f: impl FnOnce(DesignStore) -> Result<T, DesignError> + Send + 'static,
) -> Result<T, ApiError> {
    let workspace = scoped_workspace(state, thread_id).await?;
    tokio::task::spawn_blocking(move || f(DesignStore::for_workspace(&workspace)))
        .await
        .map_err(|_| ApiError::internal("design store task failed"))?
        .map_err(map_err)
}

fn raw(bytes: Vec<u8>, content_type: &'static str) -> Response {
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

fn asset_type(path: &str) -> &'static str {
    match path.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        _ => "application/octet-stream",
    }
}

pub(super) async fn list_designs(
    State(state): State<RuntimeApiState>,
    Query(query): Query<DesignQuery>,
) -> Result<Json<Value>, ApiError> {
    let designs = with_store(&state, query.thread_id.as_deref(), |store| store.list()).await?;
    Ok(Json(json!({ "designs": designs })))
}

pub(super) async fn get_design(
    State(state): State<RuntimeApiState>,
    Path(doc): Path<String>,
    Query(query): Query<DesignQuery>,
) -> Result<Json<Value>, ApiError> {
    let design = with_store(&state, query.thread_id.as_deref(), move |store| store.load(&doc)).await?;
    Ok(Json(json!({ "design": design })))
}

pub(super) async fn render_artboard(
    State(state): State<RuntimeApiState>,
    Path((doc, artboard)): Path<(String, String)>,
    Query(query): Query<DesignQuery>,
) -> Result<Response, ApiError> {
    let base = query.base.unwrap_or_else(|| format!("artboard://{doc}/"));
    if !base.starts_with("artboard://") || base.contains(['"', '<', '>', ' ']) {
        return Err(ApiError::bad_request("base must be an artboard:// URL"));
    }
    let html = with_store(&state, query.thread_id.as_deref(), move |store| {
        store.compose_document(&doc, &artboard, &base, RENDER_SCHEME_SOURCE)
    })
    .await?;
    let mut response = raw(html.into_bytes(), "text/html; charset=utf-8");
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "sandbox; default-src 'none'; style-src 'unsafe-inline' artboard:; img-src artboard: data:; font-src artboard: data:; script-src 'none'; frame-src 'none'; form-action 'none'",
        ),
    );
    Ok(response)
}

pub(super) async fn design_asset(
    State(state): State<RuntimeApiState>,
    Path((doc, path)): Path<(String, String)>,
    Query(query): Query<DesignQuery>,
) -> Result<Response, ApiError> {
    let content_type = asset_type(&path);
    let bytes = with_store(&state, query.thread_id.as_deref(), move |store| {
        let file = store.asset_path(&doc, &path)?;
        let meta = std::fs::symlink_metadata(&file)
            .map_err(|_| DesignError::NotFound("asset not found".to_string()))?;
        if !meta.is_file() {
            return Err(DesignError::NotFound("asset not found".to_string()));
        }
        Ok(std::fs::read(&file)?)
    })
    .await?;
    Ok(raw(bytes, content_type))
}

pub(super) async fn move_artboard(
    State(state): State<RuntimeApiState>,
    Path((doc, artboard)): Path<(String, String)>,
    Query(query): Query<DesignQuery>,
    Json(request): Json<MoveRequest>,
) -> Result<Json<Value>, ApiError> {
    let board = with_store(&state, query.thread_id.as_deref(), move |store| {
        store.move_artboard(&doc, &artboard, request.x, request.y)
    })
    .await?;
    Ok(Json(json!({ "artboard": board })))
}

pub(super) async fn add_comment(
    State(state): State<RuntimeApiState>,
    Path(doc): Path<String>,
    Query(query): Query<DesignQuery>,
    Json(request): Json<CommentRequest>,
) -> Result<Json<Value>, ApiError> {
    let comment = with_store(&state, query.thread_id.as_deref(), move |store| {
        store.add_comment(
            &doc,
            &request.artboard_id,
            request.x,
            request.y,
            request.author.as_deref().unwrap_or("You"),
            &request.body,
        )
    })
    .await?;
    Ok(Json(json!({ "comment": comment })))
}

pub(super) async fn resolve_comment(
    State(state): State<RuntimeApiState>,
    Path((doc, comment)): Path<(String, String)>,
    Query(query): Query<DesignQuery>,
    Json(request): Json<ResolveRequest>,
) -> Result<Json<Value>, ApiError> {
    let comment = with_store(&state, query.thread_id.as_deref(), move |store| {
        store.set_comment_resolved(&doc, &comment, request.resolved)
    })
    .await?;
    Ok(Json(json!({ "comment": comment })))
}

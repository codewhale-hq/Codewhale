use crate::config_rpc::{CONFIG_LOAD_ERROR, CONFIG_SAVE_ERROR, process_app_request};
use crate::turns::{handle_thread_request, run_http_thread_message, run_prompt_turn};
use crate::{
    AppServerOptions, AppState, AppTransport, DEFAULT_CORS_ORIGINS, JsonRpcError,
    MAX_HTTP_BODY_BYTES, RUNTIME_UNAVAILABLE_CODE, THREAD_NOT_FOUND_CODE, chat_completions,
    legacy_deepseek_compat,
};
use anyhow::{Result, bail};
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use codewhale_protocol::runtime::MAX_RUNTIME_IMAGE_BODY_BYTES;
use codewhale_protocol::{AppRequest, AppResponse, PromptRequest, ThreadRequest};
use serde_json::{Value, json};
use std::path::PathBuf;
use tower_http::cors::CorsLayer;
use uuid::Uuid;

/// Protected routes the `Capabilities` response advertises. A test sends a
/// request to each one through [`app_router`], so an entry here without a
/// handler fails the tests instead of shipping a dead route.
///
/// There is no `/tool`: a direct tool call outside a turn would need its own
/// tool catalog and approval decision, and the Engine behind the runtime
/// bridge is the only tool and approval authority. Tools run inside turns
/// (`/prompt`, `/thread` messages). Compatibility turns require an explicit
/// empty `allowed_tools` list; interactive execution is refused before turn
/// admission. Tool-enabled work belongs on `app-server --http` and `/v1/*`.
pub(crate) const ADVERTISED_ROUTES: &[&str] = &["/thread", "/app", "/prompt"];

/// Existing compatibility routes adopted by the canonical host listener.
/// Both callers share the exact existing dispatcher/auth/body-limit routes.
pub fn runtime_compatibility_router(
    mut state: AppState,
    cors_origins: &[String],
    auth_token: Option<String>,
    workspace: Option<PathBuf>,
) -> Router {
    // Only the listener's gate changes. The captured bridge keeps the original
    // owner's in-memory credential and all shared dispatcher state.
    state.auth_token = auth_token;
    state.frontend_workspace = workspace;
    app_router(state, cors_origins)
}

pub(crate) fn app_router(state: AppState, cors_origins: &[String]) -> Router {
    let protected_routes = Router::new()
        .route(
            "/thread",
            post(thread_handler).layer(axum::extract::DefaultBodyLimit::max(
                MAX_RUNTIME_IMAGE_BODY_BYTES,
            )),
        )
        .route("/app", post(app_handler))
        .route(
            "/prompt",
            post(prompt_handler).layer(axum::extract::DefaultBodyLimit::max(
                MAX_RUNTIME_IMAGE_BODY_BYTES,
            )),
        )
        .route(
            "/v1/chat/completions",
            post(chat_completions::chat_completions_handler),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_app_server_token,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(protected_routes)
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(cors_layer(cors_origins))
        .with_state(state)
}

pub(crate) async fn healthz() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "protocol": "v2",
        "service": legacy_deepseek_compat::SERVICE_NAME
    }))
}

/// Render a routing failure as a typed HTTP error body.
///
/// Deliberately *not* a success-shaped payload with the error stuffed into a
/// content field: a client must be able to tell "the model said this" from
/// "nothing ran".
pub(crate) fn http_error_from_jsonrpc(err: JsonRpcError) -> (StatusCode, Json<Value>) {
    let (status, code) = match err.code {
        -32600 | -32602 => (StatusCode::BAD_REQUEST, "invalid_request"),
        THREAD_NOT_FOUND_CODE => (StatusCode::NOT_FOUND, "thread_not_found"),
        RUNTIME_UNAVAILABLE_CODE => (StatusCode::SERVICE_UNAVAILABLE, "runtime_unavailable"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    };
    (
        status,
        Json(json!({
            "error": {
                "code": code,
                "jsonrpc_code": err.code,
                "message": err.message,
                "data": err.data,
            }
        })),
    )
}

pub(crate) async fn thread_handler(
    State(state): State<AppState>,
    Json(req): Json<ThreadRequest>,
) -> Response {
    // A message is a turn, and turns belong to the runtime — not to the
    // bookkeeping `Runtime` behind the other thread operations. This mirrors
    // the interception stdio `thread/message` has always done.
    if let ThreadRequest::Message {
        thread_id,
        input,
        allowed_tools,
        images,
        max_output_tokens,
    } = req
    {
        return match run_http_thread_message(
            &state,
            thread_id,
            input,
            allowed_tools,
            images,
            max_output_tokens,
        )
        .await
        {
            Ok(res) => (StatusCode::OK, Json(res)).into_response(),
            Err(err) => http_error_from_jsonrpc(err).into_response(),
        };
    }
    match handle_thread_request(&state, req).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(err) => http_error_from_jsonrpc(err).into_response(),
    }
}

/// `POST /prompt` — runs a genuine model turn through the runtime bridge.
///
/// Note what this handler does *not* do: it never takes the `Runtime` write
/// lock. The old implementation held it across the whole request while doing
/// no model work at all.
pub(crate) async fn prompt_handler(
    State(state): State<AppState>,
    Json(req): Json<PromptRequest>,
) -> Response {
    let mut sink = tokio::io::sink();
    match run_prompt_turn(&state, &mut sink, req).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(err) => http_error_from_jsonrpc(err).into_response(),
    }
}

pub(crate) async fn app_handler(
    State(state): State<AppState>,
    Json(req): Json<AppRequest>,
) -> (StatusCode, Json<AppResponse>) {
    let response = process_app_request(&state, req, AppTransport::Http).await;
    (app_response_status(&response), Json(response))
}

pub(crate) fn app_response_status(response: &AppResponse) -> StatusCode {
    if response.ok {
        return StatusCode::OK;
    }
    if response.data.get("request_id").is_some() {
        StatusCode::CONFLICT
    } else if response
        .data
        .get("error")
        .and_then(Value::as_str)
        .is_some_and(|err| err.starts_with(CONFIG_LOAD_ERROR) || err.starts_with(CONFIG_SAVE_ERROR))
    {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::BAD_REQUEST
    }
}

pub(crate) fn resolve_auth_token(options: &AppServerOptions) -> Result<Option<String>> {
    let configured = options.auth_token.as_ref().map(|token| token.trim());
    if let Some(token) = configured
        && token.is_empty()
    {
        bail!("app-server auth token cannot be empty");
    }
    let has_explicit_token = configured.is_some();

    if options.insecure_no_auth {
        if !options.listen.ip().is_loopback() {
            bail!("refusing unauthenticated app-server bind on non-loopback address");
        }
        eprintln!("warning: app-server HTTP auth disabled by --insecure-no-auth");
        return Ok(None);
    }

    if !has_explicit_token && !options.listen.ip().is_loopback() {
        bail!(
            "refusing non-loopback app-server bind without explicit auth token; pass --auth-token or set CODEWHALE_APP_SERVER_TOKEN"
        );
    }

    let token = configured
        .map(str::to_string)
        .unwrap_or_else(|| format!("cwapp_{}", Uuid::new_v4().simple()));
    for line in app_server_auth_status_lines(has_explicit_token) {
        eprintln!("{line}");
    }
    Ok(Some(token))
}

pub(crate) fn app_server_auth_status_lines(has_explicit_token: bool) -> Vec<&'static str> {
    if has_explicit_token {
        return vec!["app-server auth: bearer token required for HTTP routes."];
    }
    vec![
        "app-server auth: generated bearer token for this process (not printed).",
        "  Pass --auth-token or set CODEWHALE_APP_SERVER_TOKEN when another client needs to connect.",
    ]
}

pub(crate) fn cors_layer(extra_origins: &[String]) -> CorsLayer {
    let mut origins: Vec<HeaderValue> = DEFAULT_CORS_ORIGINS
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();
    for raw in extra_origins {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match HeaderValue::from_str(trimmed) {
            Ok(value) if !origins.contains(&value) => origins.push(value),
            Ok(_) => {}
            Err(err) => {
                eprintln!("warning: ignoring invalid app-server CORS origin `{trimmed}`: {err}")
            }
        }
    }

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

pub(crate) async fn require_app_server_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.auth_token.as_deref() else {
        return next.run(req).await;
    };
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .is_some_and(|token| {
            codewhale_core::secret_eq::constant_time_eq(token.as_bytes(), expected.as_bytes())
        });

    if authorized {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "app-server bearer token required",
                    "status": StatusCode::UNAUTHORIZED.as_u16(),
                }
            })),
        )
            .into_response()
    }
}

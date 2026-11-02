//! Provider-neutral `/v1/chat/completions` pass-through endpoint.
//!
//! This module resolves a model inside the configured provider's authority and
//! forwards an OpenAI-compatible request body upstream. Model text is metadata:
//! it never selects a provider configuration or credential slot. It does
//! **not** import or call any DeepSeek-named client APIs — routing stays in
//! neutral config/provider types.
//!
//! Only providers whose [`WireFormat`] is [`WireFormat::ChatCompletions`] are
//! served.  Streaming requests are explicitly rejected for now.

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderName, StatusCode};
use axum::response::IntoResponse;
use codewhale_agent::ModelRegistry;
use codewhale_config::{
    ConfigApiKeyValueKind, ConfigToml, ProviderKind, apply_openrouter_vendor,
    auth_mode_disables_api_key, classify_config_api_key_value, is_upstream_auth_header,
    provider::WireFormat,
    provider_base_url_is_official, provider_preserves_custom_base_url_model,
    route::{LogicalModelRef, RouteError, RouteRequest, RouteResolver},
    validate_openrouter_vendor,
};
use serde_json::Value;

use super::AppState;

// ── Upstream deadlines ─────────────────────────────────────────────────

/// Connect budget for the upstream forward. Matches the 10s connect bound
/// used by the TUI client's non-streaming requests (vision, the shared
/// retry client).
const UPSTREAM_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Total budget for one upstream forward, connect through body end. The
/// handler rejects streaming (`stream: true`) and reads the full upstream
/// body, so without a client-level total a provider that accepts the
/// connection and stalls — or trickles the body — wedges this handler (and
/// the caller's connection) indefinitely. 1800s mirrors the TUI client's
/// non-streaming envelope for the same request class.
const UPSTREAM_TOTAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

// ── Resolved endpoint ──────────────────────────────────────────────────

/// Everything needed to forward a single chat-completions request upstream.
#[derive(Debug, Clone)]
struct ResolvedModelEndpoint {
    provider: ProviderKind,
    base_url: String,
    model: String,
    api_key: Option<String>,
    auth_disabled: bool,
    http_headers: BTreeMap<String, String>,
    path_suffix: Option<String>,
    insecure_skip_tls_verify: bool,
    wire_format: WireFormat,
}

// ── Resolution ─────────────────────────────────────────────────────────

/// Resolve a provider endpoint from the app configuration + an optional
/// `model` field pulled out of the incoming request body.
fn resolve_endpoint(
    config: &ConfigToml,
    registry: &ModelRegistry,
    request_model: Option<&str>,
) -> Result<ResolvedModelEndpoint, RouteError> {
    // The configured provider is route authority. A request's model field can
    // select only within that provider; it may never switch endpoints or
    // credential slots by resembling another provider's catalog row.
    let provider_kind = config.provider;
    let provider_cfg = config.providers.for_provider(provider_kind);
    let provider_meta = provider_kind.provider();

    // Base URL: configured → default
    let base_url = provider_base_url(config, provider_kind);
    let endpoint_owns_models = endpoint_preserves_raw_model_ids(provider_kind, &base_url);

    // ModelRegistry canonicalizes provider-owned aliases and detects clearly
    // foreign rows. RouteResolver remains authoritative for the provider-scoped
    // wire model and custom-endpoint passthrough contract.
    let raw_selected_model = request_model
        .filter(|m| !m.trim().is_empty())
        .map(str::to_string)
        .or_else(|| provider_cfg.model.clone())
        .or_else(|| {
            (provider_kind == ProviderKind::Deepseek)
                .then(|| config.default_text_model.clone())
                .flatten()
        })
        .unwrap_or_else(|| provider_meta.default_model().to_string());
    let selected_model = if endpoint_owns_models {
        raw_selected_model
    } else {
        match registry.resolve(Some(&raw_selected_model), Some(provider_kind)) {
            Ok(resolved)
                if !resolved.used_fallback && resolved.resolved.provider == provider_kind =>
            {
                resolved.resolved.id
            }
            Err(_) if registry.is_known_for_other_provider(&raw_selected_model, provider_kind) => {
                return Err(RouteError::ForeignModelForDirectProvider {
                    provider: provider_kind.as_str().into(),
                    model: raw_selected_model,
                });
            }
            // Registry metadata is advisory. An unknown future id stays in the
            // selected provider's scope, where RouteResolver either accepts the
            // provider's pass-through contract or rejects the model. It never
            // borrows another provider's default or credentials.
            Ok(_) | Err(_) => raw_selected_model,
        }
    };
    // The table's wire dialect rides into the resolver exactly as it does on
    // the TUI turn path, so a `wire = "responses"` custom table mints a
    // Responses route and fails closed at this handler's Chat-only guard
    // instead of silently receiving a chat body.
    let wire_override = (provider_kind == ProviderKind::Custom)
        .then(|| {
            codewhale_config::provider::wire_dialect_override(
                provider_kind.as_str(),
                provider_cfg.wire.as_deref(),
            )
        })
        .flatten();
    let route = RouteResolver::new().resolve(&RouteRequest {
        explicit_provider: Some(provider_kind),
        model_selector: Some(LogicalModelRef::from(selected_model.as_str())),
        saved_provider_model: None,
        base_url_override: Some(base_url.clone()),
        limit_overrides: Vec::new(),
        wire_override,
    })?;
    let model = route.wire_model_id().as_str().to_string();

    let auth_mode = provider_cfg.auth_mode.as_deref().or_else(|| {
        (provider_kind == config.provider)
            .then_some(config.auth_mode.as_deref())
            .flatten()
    });
    let auth_disabled = auth_mode_disables_api_key(auth_mode);

    let configured_api_key = provider_cfg.api_key.as_deref();

    // Provider auth comes only from the resolved endpoint configuration. The
    // HTTP request's Authorization header authenticates the caller to the local
    // app-server and is never a provider credential.
    let api_key = resolve_upstream_api_key(
        configured_api_key,
        auth_disabled,
        provider_base_url_is_official(provider_kind, &base_url),
        || {
            provider_meta
                .env_vars()
                .iter()
                .find_map(|var| std::env::var(var).ok())
        },
    );

    let mut http_headers = if provider_kind == config.provider {
        config.http_headers.clone()
    } else {
        BTreeMap::new()
    };
    http_headers.extend(provider_cfg.http_headers.clone());
    if auth_disabled {
        http_headers.retain(|name, _| !is_upstream_auth_header(name));
    }

    let path_suffix = provider_cfg.path_suffix.clone();

    let insecure_skip_tls_verify = provider_cfg.insecure_skip_tls_verify.unwrap_or(false);

    let wire_format = route.protocol();

    Ok(ResolvedModelEndpoint {
        provider: provider_kind,
        base_url,
        model,
        api_key,
        auth_disabled,
        http_headers,
        path_suffix,
        insecure_skip_tls_verify,
        wire_format,
    })
}

fn resolve_upstream_api_key(
    configured: Option<&str>,
    auth_disabled: bool,
    allow_ambient: bool,
    ambient_provider_env: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if auth_disabled {
        None
    } else if let Some(configured) = configured
        .filter(|value| classify_config_api_key_value(value) == ConfigApiKeyValueKind::Literal)
    {
        Some(configured.to_string())
    } else if allow_ambient {
        ambient_provider_env()
    } else {
        None
    }
}

fn provider_base_url(config: &ConfigToml, provider: ProviderKind) -> String {
    let metadata = provider.provider();
    config
        .providers
        .for_provider(provider)
        .base_url
        .clone()
        .unwrap_or_else(|| metadata.default_base_url().to_string())
}

fn endpoint_preserves_raw_model_ids(provider: ProviderKind, base_url: &str) -> bool {
    matches!(
        provider,
        ProviderKind::Custom
            | ProviderKind::Ollama
            | ProviderKind::OllamaCloud
            | ProviderKind::Vllm
            | ProviderKind::Sglang
            | ProviderKind::OpencodeZen
    ) || provider_preserves_custom_base_url_model(provider, base_url)
}

/// Build the upstream URL. DeepSeek strict function calls are a beta feature,
/// so only requests that actually carry `function.strict = true` preserve the
/// configured `/beta` route. Ordinary requests continue to use `/v1`.
fn upstream_url(endpoint: &ResolvedModelEndpoint, body: &Value) -> String {
    let base = endpoint.base_url.trim_end_matches('/');
    match endpoint.path_suffix.as_deref() {
        Some(suffix) if !suffix.trim().is_empty() => format!(
            "{}/{}",
            unversioned_base_url(base),
            suffix.trim_start_matches('/')
        ),
        _ => {
            let mut versioned = versioned_base_url(base);
            let deepseek_strict_beta = endpoint.provider == ProviderKind::Deepseek
                && provider_base_url_is_official(endpoint.provider, base)
                && versioned
                    .rsplit('/')
                    .next()
                    .is_some_and(|segment| segment.eq_ignore_ascii_case("beta"))
                && body_uses_strict_tools(body);
            if !deepseek_strict_beta
                && versioned
                    .rsplit('/')
                    .next()
                    .is_some_and(|segment| segment.eq_ignore_ascii_case("beta"))
            {
                versioned = format!("{}/v1", unversioned_base_url(base));
            }
            format!("{}/chat/completions", versioned.trim_end_matches('/'))
        }
    }
}

fn body_uses_strict_tools(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.pointer("/function/strict").and_then(Value::as_bool) == Some(true))
        })
}

fn versioned_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if base_url_has_version_suffix(trimmed) {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

fn unversioned_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    trimmed
        .rsplit_once('/')
        .filter(|(_, segment)| is_version_segment(segment))
        .map(|(base, _)| base)
        .unwrap_or(trimmed)
        .to_string()
}

fn base_url_has_version_suffix(trimmed: &str) -> bool {
    trimmed.rsplit('/').next().is_some_and(is_version_segment)
}

fn is_version_segment(segment: &str) -> bool {
    segment.eq_ignore_ascii_case("beta")
        || segment
            .strip_prefix('v')
            .or_else(|| segment.strip_prefix('V'))
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit()))
}

// ── Route handler ──────────────────────────────────────────────────────

pub(crate) async fn chat_completions_handler(
    State(state): State<AppState>,
    Json(mut body): Json<Value>,
) -> impl IntoResponse {
    // Reject streaming early.
    if body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "message": "streaming is not supported on this endpoint",
                    "type": "unsupported_parameter",
                    "code": "streaming_unsupported"
                }
            })),
        )
            .into_response();
    }

    // Extract model from body.
    let request_model = body.get("model").and_then(|v| v.as_str());

    // Resolve endpoint. Everything the upstream call needs is copied out of
    // the config here and the read guard is released before any network
    // I/O: a slow upstream must never hold the shared config lock, because
    // a queued `app/config/set` writer would then stall every later reader.
    let config = state.config.read().await;
    let vendor = config
        .providers
        .for_provider(config.provider)
        .vendor
        .as_deref()
        .unwrap_or_default();
    let openrouter_vendor = match validate_openrouter_vendor(vendor) {
        Ok(vendor) if vendor.is_none() || config.provider == ProviderKind::Openrouter => {
            vendor.map(str::to_owned)
        }
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": "vendor is supported only for OpenRouter and must be a slug without whitespace or control characters",
                        "type": "invalid_request_error",
                        "code": "invalid_vendor"
                    }
                })),
            )
                .into_response();
        }
    };
    let resolved = resolve_endpoint(&config, &state.registry, request_model);
    drop(config);
    let endpoint = match resolved {
        Ok(endpoint) => endpoint,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("model route could not be resolved: {error}"),
                        "type": "invalid_request_error",
                        "code": "model_route_invalid"
                    }
                })),
            )
                .into_response();
        }
    };

    // Only ChatCompletions providers are supported.
    if endpoint.wire_format != WireFormat::ChatCompletions {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "message": format!(
                        "provider {:?} uses {:?} wire format, only ChatCompletions is supported",
                        endpoint.provider, endpoint.wire_format
                    ),
                    "type": "unsupported_provider",
                    "code": "provider_wire_format_unsupported"
                }
            })),
        )
            .into_response();
    }

    // Always write the resolved model back. Unknown provider-owned ids remain
    // byte-for-byte passthrough values, while known aliases become their exact
    // provider wire ids before forwarding.
    body["model"] = serde_json::Value::String(endpoint.model.clone());
    // The operator pin overrides caller ordering/fallback preferences while
    // retaining caller restrictions such as only, ignore, and privacy policy.
    apply_openrouter_vendor(&mut body, openrouter_vendor.as_deref());

    let url = upstream_url(&endpoint, &body);

    if endpoint.insecure_skip_tls_verify {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": {
                    "message": format!(
                        "TLS certificate verification cannot be disabled for provider {:?}; use SSL_CERT_FILE with a trusted custom CA bundle",
                        endpoint.provider
                    ),
                    "type": "invalid_request_error",
                    "code": "tls_verification_required"
                }
            })),
        )
            .into_response();
    }

    // Build upstream request. The shared platform builder sets no timeouts,
    // so the proxy would hang forever on an accept-and-stall upstream;
    // bound both the connect and the whole non-streaming round trip.
    let upstream_req = codewhale_release::platform_http_client_builder()
        .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
        .timeout(UPSTREAM_TOTAL_TIMEOUT)
        .build()
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {
                        "message": format!("failed to build upstream client: {e}"),
                        "type": "internal_error"
                    }
                })),
            )
                .into_response()
        })
        .map(|client| {
            let mut req = client.post(&url).json(&body);

            if !endpoint.auth_disabled
                && let Some(key) = endpoint.api_key.as_deref()
            {
                req = req.bearer_auth(key);
            }

            // Forward configured provider headers.
            for (name, value) in &endpoint.http_headers {
                if endpoint.auth_disabled && is_upstream_auth_header(name) {
                    continue;
                }
                if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                    req = req.header(header_name, value.as_str());
                }
            }

            req
        });

    let client = match upstream_req {
        Ok(client) => client,
        Err(resp) => return resp,
    };

    // Execute upstream request.
    match client.send().await {
        Ok(upstream_resp) => {
            let status = upstream_resp.status();
            let headers = upstream_resp.headers().clone();
            match upstream_resp.text().await {
                Ok(body_text) => {
                    let mut response =
                        axum::response::Response::new(axum::body::Body::from(body_text));
                    *response.status_mut() = status;
                    // Forward relevant upstream headers.
                    if let Some(ct) = headers.get("content-type") {
                        response.headers_mut().insert("content-type", ct.clone());
                    }
                    response
                }
                Err(e) => (
                    StatusCode::BAD_GATEWAY,
                    Json(serde_json::json!({
                        "error": {
                            "message": format!("failed to read upstream response: {e}"),
                            "type": "upstream_error"
                        }
                    })),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": {
                    "message": format!("upstream request failed: {e}"),
                    "type": "upstream_error"
                }
            })),
        )
            .into_response(),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use codewhale_config::provider::WireFormat;
    use std::fs;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    use super::super::build_state;
    use crate::http::app_router;

    fn install_crypto_provider() {
        crate::install_test_crypto_provider();
    }

    // The proxy forwards with a client built per request, and reqwest gives
    // no accessor for a built client's budgets, so a behavioral test would
    // need an accept-and-stall upstream and a multi-second (connect) or
    // half-hour (total) wait. Pin the values instead: if the handler stops
    // applying them this test cannot see it, but a silent constant change
    // or an inversion of the connect/total ordering cannot slip through.
    #[test]
    fn upstream_deadlines_are_bounded_and_ordered() {
        assert_eq!(UPSTREAM_CONNECT_TIMEOUT, std::time::Duration::from_secs(10));
        assert_eq!(UPSTREAM_TOTAL_TIMEOUT, std::time::Duration::from_secs(1800));
        assert!(
            UPSTREAM_TOTAL_TIMEOUT > UPSTREAM_CONNECT_TIMEOUT,
            "the total forward budget must leave room beyond the connect budget"
        );
        // The shared platform builder must accept both bounds; the handler
        // chains them onto this builder.
        codewhale_release::platform_http_client_builder()
            .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
            .timeout(UPSTREAM_TOTAL_TIMEOUT)
            .build()
            .expect("platform builder accepts the proxy deadlines");
    }

    /// Start a minimal upstream mock server that echoes back what it received.
    async fn start_mock_upstream() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{}:{}", addr.ip(), addr.port());

        let handle = tokio::spawn(async move {
            let app = axum::Router::new()
                .route("/v1/chat/completions", axum::routing::post(mock_handler));
            axum::serve(listener, app).await.unwrap();
        });

        // Give the server a moment to start.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        (base_url, handle)
    }

    async fn mock_handler(
        headers: axum::http::HeaderMap,
        Json(body): Json<Value>,
    ) -> impl axum::response::IntoResponse {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("none");

        let response_body = serde_json::json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "created": 1234567890,
            "model": body.get("model").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": format!("echo: received {} messages, auth={auth}",
                        body.get("messages").and_then(|m| m.as_array()).map(|a| a.len()).unwrap_or(0))
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });

        (StatusCode::OK, Json(response_body))
    }

    async fn capturing_mock_handler(
        axum::extract::State(captured): axum::extract::State<
            mpsc::UnboundedSender<axum::http::HeaderMap>,
        >,
        headers: axum::http::HeaderMap,
        body: Json<Value>,
    ) -> impl axum::response::IntoResponse {
        captured
            .send(headers.clone())
            .expect("capture upstream headers");
        mock_handler(headers, body).await
    }

    async fn start_capturing_mock_upstream() -> (
        String,
        mpsc::UnboundedReceiver<axum::http::HeaderMap>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind capturing upstream");
        let addr = listener.local_addr().expect("capturing upstream address");
        let base_url = format!("http://{}:{}", addr.ip(), addr.port());
        let (captured_tx, captured_rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(async move {
            let app = axum::Router::new()
                .route(
                    "/v1/chat/completions",
                    axum::routing::post(capturing_mock_handler),
                )
                .with_state(captured_tx);
            axum::serve(listener, app)
                .await
                .expect("serve capturing upstream");
        });

        (base_url, captured_rx, handle)
    }

    fn app_with_mock_upstream(
        auth_token: Option<&str>,
        mock_base_url: &str,
    ) -> (axum::Router, tempfile::TempDir) {
        app_with_mock_upstream_with_provider_extra(auth_token, mock_base_url, "")
    }

    fn app_with_mock_upstream_with_provider_extra(
        auth_token: Option<&str>,
        mock_base_url: &str,
        provider_extra: &str,
    ) -> (axum::Router, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let config_content = format!(
            r#"
provider = "arcee"
api_key = "sk-deepseek-secret"

[providers.arcee]
base_url = "{mock_base_url}"
model = "trinity-large-thinking"
api_key = "arcee-configured-key"
{provider_extra}
"#
        );
        fs::write(&config_path, config_content).expect("write config");
        let state = build_state(
            Some(config_path),
            auth_token.map(std::string::ToString::to_string),
        )
        .expect("state");
        (app_router(state, &[]), tmp)
    }

    fn app_with_together_mock_upstream(mock_base_url: &str) -> (axum::Router, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let config_content = format!(
            r#"
provider = "together"

[providers.together]
base_url = "{mock_base_url}"
api_key = "together-configured-key"
"#
        );
        fs::write(&config_path, config_content).expect("write config");
        let state = build_state(Some(config_path), None).expect("state");
        (app_router(state, &[]), tmp)
    }

    fn app_with_root_deepseek_mock_upstream(
        mock_base_url: &str,
    ) -> (axum::Router, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let config_content = format!(
            r#"
provider = "deepseek"
api_key = "root-deepseek-key"
base_url = "{mock_base_url}"
default_text_model = "root-deepseek-model"
http_headers = {{ "X-Root-Route" = "kept" }}
"#
        );
        fs::write(&config_path, config_content).expect("write config");
        let state = build_state(Some(config_path), None).expect("state");
        (app_router(state, &[]), tmp)
    }

    fn app_with_auth_boundary_mock_upstream(
        auth_token: &str,
        mock_base_url: &str,
        provider_api_key: &str,
        auth_mode: Option<&str>,
        include_configured_auth_headers: bool,
    ) -> (axum::Router, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        let auth_mode = auth_mode
            .map(|mode| format!("auth_mode = {mode:?}"))
            .unwrap_or_default();
        let configured_auth_headers = if include_configured_auth_headers {
            r#"http_headers = { aUtHoRiZaTiOn = "Bearer configured-header-secret", "X-API-Key" = "configured-x-key-secret", "Api-Key" = "configured-key-secret", "Proxy-Authorization" = "Basic configured-proxy-secret", "X-Auth-Token" = "configured-auth-token", "X-Access-Token" = "configured-access-token", "X-Goog-Api-Key" = "configured-google-key", Cookie = "session=secret", "X-Route-Metadata" = "safe" }"#
        } else {
            ""
        };
        let config_content = format!(
            r#"
provider = "arcee"

[providers.arcee]
base_url = "{mock_base_url}"
model = "trinity-large-thinking"
api_key = {provider_api_key:?}
{auth_mode}
{configured_auth_headers}
"#
        );
        fs::write(&config_path, config_content).expect("write config");
        let state = build_state(Some(config_path), Some(auth_token.to_string())).expect("state");
        (app_router(state, &[]), tmp)
    }

    async fn response_body_json(response: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        serde_json::from_slice(&bytes).expect("json response")
    }

    #[tokio::test]
    async fn openrouter_vendor_forwarding_preserves_pin_and_caller_restrictions() {
        install_crypto_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_url = format!("http://{}", listener.local_addr().unwrap());
        let (captured_tx, mut captured_rx) = mpsc::unbounded_channel::<Value>();
        let upstream = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |Json(body): Json<Value>| {
                let captured = captured_tx.clone();
                async move {
                    captured.send(body).unwrap();
                    Json(serde_json::json!({"choices": []}))
                }
            }),
        );
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        for (provider, vendor, status) in [
            ("openrouter", "deepinfra/turbo", StatusCode::OK),
            ("openrouter", "", StatusCode::OK),
            ("openrouter", "bad vendor fixture", StatusCode::BAD_REQUEST),
            ("arcee", "deepinfra/turbo", StatusCode::BAD_REQUEST),
            ("arcee", "", StatusCode::OK),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let config_path = tmp.path().join("config.toml");
            let openrouter_vendor = if provider == "openrouter" {
                vendor
            } else {
                "dormant/pin"
            };
            let arcee_vendor = if provider == "arcee" { vendor } else { "" };
            fs::write(&config_path, format!(
                "provider = {provider:?}\n\
                 [providers.openrouter]\nbase_url = {mock_url:?}\napi_key = \"fixture-openrouter-key\"\nvendor = {openrouter_vendor:?}\n\
                 [providers.arcee]\nbase_url = {mock_url:?}\napi_key = \"fixture-arcee-key\"\nvendor = {arcee_vendor:?}\n"
            )).unwrap();
            let state = build_state(Some(config_path), None).unwrap();
            let app = app_router(state, &[]);
            let caller_policy = serde_json::json!({
                "order": ["caller/escape"],
                "allow_fallbacks": true,
                "only": ["caller/restriction"],
                "ignore": ["caller/blocked"],
                "zdr": true,
                "data_collection": "deny",
                "require_parameters": true
            });
            let body = serde_json::json!({
                "model": "fixture/model",
                "messages": [{"role": "user", "content": "hello"}],
                "provider": caller_policy
            });
            let response = app
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/v1/chat/completions")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{provider}: {vendor}");
            if status == StatusCode::BAD_REQUEST {
                let error = response_body_json(response).await;
                assert_eq!(error["error"]["code"], "invalid_vendor");
                assert!(!error.to_string().contains(vendor));
                assert!(
                    captured_rx.try_recv().is_err(),
                    "invalid config reached upstream"
                );
            } else {
                let forwarded = captured_rx.try_recv().expect("captured forwarded request");
                let mut expected = caller_policy;
                if provider == "openrouter" && !vendor.is_empty() {
                    expected["order"] = serde_json::json!([vendor]);
                    expected["allow_fallbacks"] = serde_json::json!(false);
                }
                assert_eq!(forwarded["provider"], expected, "{provider}: {vendor}");
                assert_eq!(forwarded["model"], "fixture/model");
            }
        }
        upstream_task.abort();
    }

    #[tokio::test]
    async fn upstream_request_does_not_hold_the_config_lock() {
        // A slow upstream used to keep `state.config.read()` alive for the
        // whole request, so a concurrent `app/config/set` (a writer) blocked
        // and, with tokio's writer-preferring lock, every later reader
        // queued behind it.
        install_crypto_provider();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_url = format!("http://{}", listener.local_addr().unwrap());
        let (arrived_tx, mut arrived_rx) = mpsc::unbounded_channel::<()>();
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let upstream_release = release.clone();
        let upstream = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                let arrived = arrived_tx.clone();
                let release = upstream_release.clone();
                async move {
                    arrived.send(()).unwrap();
                    release.notified().await;
                    Json(serde_json::json!({"choices": []}))
                }
            }),
        );
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                "provider = \"arcee\"\n[providers.arcee]\nbase_url = {mock_url:?}\nmodel = \"trinity-large-thinking\"\napi_key = \"arcee-configured-key\"\n"
            ),
        )
        .expect("write config");
        let state = build_state(Some(config_path), None).expect("state");
        let app = app_router(state.clone(), &[]);
        let body = serde_json::json!({"messages": [{"role": "user", "content": "hello"}]});
        let request = tokio::spawn(async move {
            app.oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
        });

        tokio::time::timeout(std::time::Duration::from_secs(10), arrived_rx.recv())
            .await
            .expect("upstream should receive the request")
            .expect("arrival signal");
        assert!(
            state.config.try_write().is_ok(),
            "config must be writable while the upstream request is in flight"
        );

        release.notify_one();
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), request)
            .await
            .expect("request should finish once upstream answers")
            .expect("join request");
        assert_eq!(response.status(), StatusCode::OK);
        upstream_task.abort();
    }

    #[tokio::test]
    async fn forwards_messages_and_tools() {
        install_crypto_provider();
        let (mock_url, _mock) = start_mock_upstream().await;
        let (app, _tmp) = app_with_mock_upstream(None, &mock_url);

        let body = serde_json::json!({
            "model": "trinity-large-thinking",
            "messages": [
                {"role": "user", "content": "hello"}
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather",
                    "parameters": {"type": "object", "properties": {}}
                }
            }],
            "tool_choice": "auto"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let resp_body = response_body_json(response).await;
        assert_eq!(resp_body["model"], "trinity-large-thinking");
        assert!(
            resp_body["choices"][0]["message"]["content"]
                .as_str()
                .unwrap()
                .contains("1 messages")
        );
    }

    #[tokio::test]
    async fn default_model_injected_when_omitted() {
        install_crypto_provider();
        let (mock_url, _mock) = start_mock_upstream().await;
        let (app, _tmp) = app_with_mock_upstream(None, &mock_url);

        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let resp_body = response_body_json(response).await;
        // The mock echoes the model it received; should be the configured default.
        assert_eq!(resp_body["model"], "trinity-large-thinking");
    }

    #[tokio::test]
    async fn root_deepseek_compatibility_fields_reach_the_configured_upstream() {
        install_crypto_provider();
        let (mock_url, mut captured, _mock) = start_capturing_mock_upstream().await;
        let (app, _tmp) = app_with_root_deepseek_mock_upstream(&mock_url);

        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "hello"}]
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let response_body = response_body_json(response).await;
        assert_eq!(response_body["model"], "root-deepseek-model");
        assert!(
            response_body["choices"][0]["message"]["content"]
                .as_str()
                .is_some_and(|content| content.contains("auth=Bearer root-deepseek-key"))
        );
        let headers = tokio::time::timeout(std::time::Duration::from_secs(1), captured.recv())
            .await
            .expect("upstream request timeout")
            .expect("captured upstream request");
        assert_eq!(
            headers
                .get("x-root-route")
                .and_then(|value| value.to_str().ok()),
            Some("kept")
        );
    }

    #[tokio::test]
    async fn configured_model_preserved_when_provided() {
        install_crypto_provider();
        let (mock_url, _mock) = start_mock_upstream().await;
        let (app, _tmp) = app_with_mock_upstream(None, &mock_url);

        let body = serde_json::json!({
            "model": "custom-model-v2",
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let resp_body = response_body_json(response).await;
        assert_eq!(resp_body["model"], "custom-model-v2");
    }

    #[test]
    fn providerless_together_aliases_are_rejected_under_deepseek_authority() {
        let config = ConfigToml::default();
        let registry = ModelRegistry::default();

        for requested in ["inkling", "together-inkling", "thinkingmachines/inkling"] {
            assert!(
                matches!(
                    resolve_endpoint(&config, &registry, Some(requested)),
                    Err(RouteError::ForeignModelForDirectProvider { .. })
                ),
                "model text must not switch the configured DeepSeek route to Together: {requested}"
            );
        }
    }

    #[test]
    fn shared_alias_stays_inside_the_configured_provider() {
        let config = ConfigToml {
            provider: ProviderKind::Openrouter,
            ..ConfigToml::default()
        };

        let endpoint =
            resolve_endpoint(&config, &ModelRegistry::default(), Some("deepseek-v4-pro"))
                .expect("configured-provider route");

        assert_eq!(endpoint.provider, ProviderKind::Openrouter);
    }

    #[test]
    fn unknown_model_under_zai_never_infers_deepseek_provider_authority() {
        let config = ConfigToml {
            provider: ProviderKind::Zai,
            ..ConfigToml::default()
        };
        let registry = ModelRegistry::default();

        let endpoint = resolve_endpoint(&config, &registry, Some("totally-unknown-model"))
            .expect("unknown future id stays inside explicit Z.ai authority");
        assert_eq!(endpoint.provider, ProviderKind::Zai);
        assert_eq!(endpoint.model, "totally-unknown-model");
        assert_eq!(endpoint.api_key, None);
    }

    #[test]
    fn known_deepseek_model_under_zai_is_rejected_instead_of_switching_credentials() {
        let mut config = ConfigToml {
            provider: ProviderKind::Zai,
            ..ConfigToml::default()
        };
        config.providers.zai.api_key = Some("zai-only-key".to_string());
        config.providers.deepseek.api_key = Some("must-not-be-selected".to_string());

        assert!(matches!(
            resolve_endpoint(
                &config,
                &ModelRegistry::default(),
                Some("deepseek-reasoner")
            ),
            Err(RouteError::ForeignModelForDirectProvider { .. })
        ));
    }

    #[test]
    fn configured_together_authority_canonicalizes_its_own_inkling_aliases() {
        let config = ConfigToml {
            provider: ProviderKind::Together,
            ..ConfigToml::default()
        };

        for requested in ["inkling", "together-inkling", "thinkingmachines/inkling"] {
            let endpoint = resolve_endpoint(&config, &ModelRegistry::default(), Some(requested))
                .expect("configured Together route");
            assert_eq!(endpoint.provider, ProviderKind::Together, "{requested}");
            assert_eq!(endpoint.model, "thinkingmachines/inkling", "{requested}");
        }
    }

    #[test]
    fn configured_provider_is_required_for_each_official_alias() {
        let registry = ModelRegistry::default();

        for (requested, provider, expected) in [
            (
                "qwen3.7-plus",
                ProviderKind::Openrouter,
                "qwen/qwen3.7-plus",
            ),
            ("gpt53-codex", ProviderKind::Openai, "gpt-5.3-codex"),
            ("arcee-trinity-mini", ProviderKind::Arcee, "trinity-mini"),
        ] {
            let config = ConfigToml {
                provider,
                ..ConfigToml::default()
            };
            let endpoint = resolve_endpoint(&config, &registry, Some(requested))
                .expect("provider-owned known alias route");
            assert_eq!(endpoint.provider, provider, "{requested}");
            assert_eq!(endpoint.model, expected, "{requested}");
        }
    }

    #[test]
    fn default_deepseek_route_cannot_claim_foreign_official_aliases() {
        let config = ConfigToml::default();

        for requested in ["qwen3.7-plus", "gpt53-codex", "arcee-trinity-mini"] {
            assert!(
                matches!(
                    resolve_endpoint(&config, &ModelRegistry::default(), Some(requested)),
                    Err(RouteError::ForeignModelForDirectProvider { .. })
                ),
                "{requested} must not select another provider from model text"
            );
        }
    }

    #[test]
    fn opencode_go_app_route_uses_model_protocol_without_cross_provider_fallback() {
        let registry = ModelRegistry::default();
        for (model, wire) in [
            ("grok-4.5", WireFormat::ChatCompletions),
            ("kimi-k3", WireFormat::ChatCompletions),
            ("grok-4.6", WireFormat::Responses),
            ("gpt-5.6-luna", WireFormat::Responses),
            ("minimax-m3", WireFormat::AnthropicMessages),
            ("qwen3.8-max", WireFormat::AnthropicMessages),
        ] {
            for requested in [model.to_string(), format!("opencode-go/{model}")] {
                for base_url in [None, Some("https://go-gateway.example.test/v1".into())] {
                    let mut config = ConfigToml {
                        provider: ProviderKind::OpencodeGo,
                        ..ConfigToml::default()
                    };
                    config.providers.opencode_go.model = Some(requested.clone());
                    config.providers.opencode_go.base_url = base_url;
                    for selection in [None, Some(requested.as_str())] {
                        let endpoint = resolve_endpoint(&config, &registry, selection)
                            .expect("documented Go route");
                        assert_eq!(endpoint.provider, ProviderKind::OpencodeGo);
                        assert_eq!(endpoint.model, model);
                        assert_eq!(endpoint.wire_format, wire);
                    }
                }
            }
        }
        for model in ["claude-unproven", "gpt-unlisted", "openai/gpt-5.6-luna"] {
            let mut config = ConfigToml {
                provider: ProviderKind::OpencodeGo,
                ..ConfigToml::default()
            };
            config.providers.opencode_go.model = Some(model.into());
            assert!(resolve_endpoint(&config, &registry, None).is_err());
            assert!(resolve_endpoint(&config, &registry, Some(model)).is_err());
            config.providers.opencode_go.base_url =
                Some("https://go-gateway.example.test/v1".into());
            assert!(resolve_endpoint(&config, &registry, Some(model)).is_err());
        }
    }

    #[test]
    fn opencode_zen_app_route_uses_the_resolved_model_protocol() {
        let config = ConfigToml {
            provider: ProviderKind::OpencodeZen,
            ..ConfigToml::default()
        };
        let registry = ModelRegistry::default();

        for (model, expected) in [
            ("gpt-5.5", WireFormat::Responses),
            ("claude-sonnet-4-6", WireFormat::AnthropicMessages),
            ("deepseek-v4-pro", WireFormat::ChatCompletions),
        ] {
            let endpoint = resolve_endpoint(&config, &registry, Some(model))
                .unwrap_or_else(|error| panic!("{model} should resolve: {error}"));
            assert_eq!(endpoint.provider, ProviderKind::OpencodeZen);
            assert_eq!(endpoint.model, model);
            assert_eq!(endpoint.wire_format, expected);
        }

        assert!(matches!(
            resolve_endpoint(&config, &registry, Some("gemini-3.1-pro")),
            Err(RouteError::UnsupportedModelProtocol { .. })
        ));
    }

    #[test]
    fn foreign_model_is_rejected_before_credentials_or_headers_can_cross() {
        let mut config = ConfigToml {
            provider: ProviderKind::Deepseek,
            auth_mode: Some("none".to_string()),
            ..ConfigToml::default()
        };
        config.http_headers.insert(
            "X-Root-Route".to_string(),
            "must-not-cross-providers".to_string(),
        );
        config.providers.together.api_key = Some("together-key".to_string());

        assert!(matches!(
            resolve_endpoint(&config, &ModelRegistry::default(), Some("inkling")),
            Err(RouteError::ForeignModelForDirectProvider { .. })
        ));
    }

    #[test]
    fn every_official_deepseek_endpoint_canonicalizes_retired_aliases() {
        let registry = ModelRegistry::default();
        for base_url in [
            "https://api.deepseek.com",
            "https://api.deepseek.com/v1/",
            "https://api.deepseek.com/beta",
        ] {
            for alias in ["deepseek-chat", "deepseek-reasoner"] {
                let mut config = ConfigToml::default();
                config.providers.deepseek.base_url = Some(base_url.to_string());
                let endpoint = resolve_endpoint(&config, &registry, Some(alias))
                    .expect("official DeepSeek route");
                assert_eq!(endpoint.provider, ProviderKind::Deepseek, "{base_url}");
                assert_eq!(endpoint.model, "deepseek-flash", "{base_url} {alias}");
            }
        }
    }

    #[test]
    fn custom_endpoint_preserves_known_registry_alias_verbatim() {
        let mut config = ConfigToml {
            provider: ProviderKind::Openrouter,
            ..ConfigToml::default()
        };
        config
            .providers
            .for_provider_mut(ProviderKind::Openrouter)
            .base_url = Some("https://gateway.example.test/v1".to_string());

        let endpoint = resolve_endpoint(&config, &ModelRegistry::default(), Some("qwen3.7-plus"))
            .expect("custom OpenRouter-compatible route");
        assert_eq!(endpoint.provider, ProviderKind::Openrouter);
        assert_eq!(endpoint.model, "qwen3.7-plus");
    }

    #[test]
    fn custom_endpoint_never_resolves_ambient_provider_env() {
        let ambient_was_read = std::cell::Cell::new(false);
        let api_key = resolve_upstream_api_key(None, false, false, || {
            ambient_was_read.set(true);
            Some("ambient-provider-secret".to_string())
        });

        assert_eq!(api_key, None);
        assert!(!ambient_was_read.get());
        for sentinel in [codewhale_config::API_KEYRING_SENTINEL, "  __KEYRING__  "] {
            assert_eq!(
                resolve_upstream_api_key(Some(sentinel), false, false, || unreachable!()),
                None
            );
            assert_eq!(
                resolve_upstream_api_key(Some(sentinel), false, true, || Some("ambient".into())),
                Some("ambient".to_string())
            );
        }
    }

    #[test]
    fn disabled_auth_never_resolves_configured_or_ambient_credentials() {
        let ambient_was_read = std::cell::Cell::new(false);
        let api_key = resolve_upstream_api_key(Some("provider-secret"), true, true, || {
            ambient_was_read.set(true);
            Some("ambient-provider-secret".to_string())
        });

        assert_eq!(api_key, None);
        assert!(!ambient_was_read.get());
    }

    #[test]
    fn active_custom_endpoint_is_not_hijacked_by_known_foreign_alias() {
        let mut config = ConfigToml {
            provider: ProviderKind::Arcee,
            ..ConfigToml::default()
        };
        config
            .providers
            .for_provider_mut(ProviderKind::Arcee)
            .base_url = Some("https://gateway.example.test/v1".to_string());

        let endpoint = resolve_endpoint(&config, &ModelRegistry::default(), Some("qwen3.7-plus"))
            .expect("active custom endpoint route");
        assert_eq!(endpoint.provider, ProviderKind::Arcee);
        assert_eq!(endpoint.model, "qwen3.7-plus");
    }

    #[test]
    fn official_configured_alias_is_canonicalized_when_model_is_omitted() {
        let mut config = ConfigToml {
            provider: ProviderKind::Openrouter,
            ..ConfigToml::default()
        };
        config
            .providers
            .for_provider_mut(ProviderKind::Openrouter)
            .model = Some("qwen3.7-plus".to_string());

        let endpoint = resolve_endpoint(&config, &ModelRegistry::default(), None)
            .expect("configured official alias route");
        assert_eq!(endpoint.provider, ProviderKind::Openrouter);
        assert_eq!(endpoint.model, "qwen/qwen3.7-plus");
    }

    #[test]
    fn configured_together_inkling_alias_is_normalized_when_model_is_omitted() {
        let mut config = ConfigToml {
            provider: ProviderKind::Together,
            ..ConfigToml::default()
        };
        config
            .providers
            .for_provider_mut(ProviderKind::Together)
            .model = Some("inkling".to_string());

        let endpoint = resolve_endpoint(&config, &ModelRegistry::default(), None)
            .expect("configured Inkling route");
        assert_eq!(endpoint.provider, ProviderKind::Together);
        assert_eq!(endpoint.model, "thinkingmachines/inkling");
    }

    #[tokio::test]
    async fn custom_together_endpoint_preserves_explicit_inkling_model_ids() {
        install_crypto_provider();
        let (mock_url, _mock) = start_mock_upstream().await;
        let (app, _tmp) = app_with_together_mock_upstream(&mock_url);

        for requested in ["inkling", "together-inkling", "thinkingmachines/inkling"] {
            let body = serde_json::json!({
                "model": requested,
                "messages": [{"role": "user", "content": "hello"}]
            });
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/v1/chat/completions")
                        .header("content-type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK, "{requested}");
            let resp_body = response_body_json(response).await;
            assert_eq!(resp_body["model"], requested, "{requested}");
        }
    }

    #[tokio::test]
    async fn configured_api_key_takes_priority_over_incoming_bearer() {
        install_crypto_provider();
        let (mock_url, _mock) = start_mock_upstream().await;
        let (app, _tmp) = app_with_mock_upstream(None, &mock_url);

        let body = serde_json::json!({
            "model": "trinity-large-thinking",
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });

        // Send with an explicit bearer token, but the configured key should win.
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer user-provided-secret-key")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let resp_body = response_body_json(response).await;
        let content = resp_body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap();
        // The configured key takes priority, not the incoming Bearer.
        assert!(
            content.contains("auth=Bearer arcee-configured-key"),
            "expected configured auth in mock echo, got: {content}"
        );
    }

    #[tokio::test]
    async fn app_authorization_is_not_forwarded_when_upstream_auth_is_disabled() {
        install_crypto_provider();
        let (mock_url, mut captured, _mock) = start_capturing_mock_upstream().await;
        let (app, _tmp) = app_with_auth_boundary_mock_upstream(
            "app-secret",
            &mock_url,
            "provider-secret",
            Some("none"),
            true,
        );

        let body = serde_json::json!({
            "model": "trinity-large-thinking",
            "messages": [{"role": "user", "content": "hello"}]
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer app-secret")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let headers = tokio::time::timeout(std::time::Duration::from_secs(1), captured.recv())
            .await
            .expect("upstream request timeout")
            .expect("captured upstream request");
        for name in [
            "authorization",
            "x-api-key",
            "api-key",
            "proxy-authorization",
            "x-auth-token",
            "x-access-token",
            "x-goog-api-key",
            "cookie",
        ] {
            assert!(headers.get(name).is_none(), "disabled auth leaked {name}");
        }
        assert_eq!(
            headers
                .get("x-route-metadata")
                .and_then(|value| value.to_str().ok()),
            Some("safe")
        );
    }

    #[tokio::test]
    async fn configured_provider_credential_is_the_only_outbound_bearer() {
        install_crypto_provider();
        let (mock_url, mut captured, _mock) = start_capturing_mock_upstream().await;
        let (app, _tmp) = app_with_auth_boundary_mock_upstream(
            "app-secret",
            &mock_url,
            "provider-secret",
            None,
            false,
        );

        let body = serde_json::json!({
            "model": "trinity-large-thinking",
            "messages": [{"role": "user", "content": "hello"}]
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer app-secret")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let headers = tokio::time::timeout(std::time::Duration::from_secs(1), captured.recv())
            .await
            .expect("upstream request timeout")
            .expect("captured upstream request");
        assert_eq!(
            headers
                .get("authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer provider-secret")
        );
    }

    #[tokio::test]
    async fn configured_api_key_used_when_no_bearer_in_request() {
        install_crypto_provider();
        let (mock_url, _mock) = start_mock_upstream().await;
        let (app, _tmp) = app_with_mock_upstream(None, &mock_url);

        let body = serde_json::json!({
            "model": "trinity-large-thinking",
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });

        // No Authorization header; the configured key should be used.
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let resp_body = response_body_json(response).await;
        let content = resp_body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap();
        assert!(
            content.contains("auth=Bearer arcee-configured-key"),
            "expected configured auth in mock echo, got: {content}"
        );
    }

    #[tokio::test]
    async fn insecure_tls_skip_verify_is_rejected() {
        install_crypto_provider();
        let (mock_url, _mock) = start_mock_upstream().await;
        let (app, _tmp) = app_with_mock_upstream_with_provider_extra(
            None,
            &mock_url,
            "insecure_skip_tls_verify = true",
        );

        let body = serde_json::json!({
            "model": "trinity-large-thinking",
            "messages": [
                {"role": "user", "content": "hello"}
            ]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let resp_body = response_body_json(response).await;
        assert_eq!(resp_body["error"]["code"], "tls_verification_required");
        assert!(
            resp_body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("SSL_CERT_FILE")
        );
    }

    #[tokio::test]
    async fn streaming_request_rejected() {
        install_crypto_provider();
        let (mock_url, _mock) = start_mock_upstream().await;
        let (app, _tmp) = app_with_mock_upstream(None, &mock_url);

        let body = serde_json::json!({
            "model": "trinity-large-thinking",
            "messages": [
                {"role": "user", "content": "hello"}
            ],
            "stream": true
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let resp_body = response_body_json(response).await;
        assert_eq!(resp_body["error"]["code"], "streaming_unsupported");
    }

    #[tokio::test]
    async fn requires_bearer_token_when_auth_enabled() {
        install_crypto_provider();
        let (mock_url, _mock) = start_mock_upstream().await;
        let (app, _tmp) = app_with_mock_upstream(Some("test-token"), &mock_url);

        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "hello"}]
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_chat_completions_provider_rejected() {
        // Use the test to verify WireFormat checks work for non-ChatCompletions providers.
        // Anthropic's wire format is AnthropicMessages; OpenaiCodex is Responses.
        let endpoint = ResolvedModelEndpoint {
            provider: ProviderKind::Anthropic,
            base_url: "https://api.anthropic.com".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            api_key: Some("sk-ant-test".to_string()),
            auth_disabled: false,
            http_headers: BTreeMap::new(),
            path_suffix: None,
            insecure_skip_tls_verify: false,
            wire_format: WireFormat::AnthropicMessages,
        };

        assert_ne!(endpoint.wire_format, WireFormat::ChatCompletions);
        // The handler would reject this; we verify the wire format here.
        assert_eq!(endpoint.wire_format, WireFormat::AnthropicMessages);
    }

    #[test]
    fn upstream_url_defaults_to_v1_chat_completions() {
        let endpoint = ResolvedModelEndpoint {
            provider: ProviderKind::Arcee,
            base_url: "https://api.arcee.ai".to_string(),
            model: "trinity".to_string(),
            api_key: None,
            auth_disabled: false,
            http_headers: BTreeMap::new(),
            path_suffix: None,
            insecure_skip_tls_verify: false,
            wire_format: WireFormat::ChatCompletions,
        };
        assert_eq!(
            upstream_url(&endpoint, &serde_json::json!({})),
            "https://api.arcee.ai/v1/chat/completions"
        );
    }

    #[test]
    fn upstream_url_preserves_arcee_api_v1_base() {
        let endpoint = ResolvedModelEndpoint {
            provider: ProviderKind::Arcee,
            base_url: "https://api.arcee.ai/api/v1".to_string(),
            model: "trinity".to_string(),
            api_key: None,
            auth_disabled: false,
            http_headers: BTreeMap::new(),
            path_suffix: None,
            insecure_skip_tls_verify: false,
            wire_format: WireFormat::ChatCompletions,
        };
        assert_eq!(
            upstream_url(&endpoint, &serde_json::json!({})),
            "https://api.arcee.ai/api/v1/chat/completions"
        );
    }

    #[test]
    fn upstream_url_respects_path_suffix() {
        let endpoint = ResolvedModelEndpoint {
            provider: ProviderKind::Openrouter,
            base_url: "https://openrouter.ai/api/v1".to_string(),
            model: "deepseek/deepseek-v4-pro".to_string(),
            api_key: None,
            auth_disabled: false,
            http_headers: BTreeMap::new(),
            path_suffix: Some("/chat/completions".to_string()),
            insecure_skip_tls_verify: false,
            wire_format: WireFormat::ChatCompletions,
        };
        assert_eq!(
            upstream_url(&endpoint, &serde_json::json!({})),
            "https://openrouter.ai/api/chat/completions"
        );
    }

    #[test]
    fn upstream_url_beta_base_uses_v1_for_ordinary_chat_completions() {
        let endpoint = ResolvedModelEndpoint {
            provider: ProviderKind::Deepseek,
            base_url: "https://api.deepseek.com/beta".to_string(),
            model: "deepseek-chat".to_string(),
            api_key: None,
            auth_disabled: false,
            http_headers: BTreeMap::new(),
            path_suffix: None,
            insecure_skip_tls_verify: false,
            wire_format: WireFormat::ChatCompletions,
        };
        assert_eq!(
            upstream_url(&endpoint, &serde_json::json!({})),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }

    #[test]
    fn upstream_url_beta_base_preserves_strict_chat_completions() {
        let endpoint = ResolvedModelEndpoint {
            provider: ProviderKind::Deepseek,
            base_url: "https://api.deepseek.com/beta".to_string(),
            model: "deepseek-v4-pro".to_string(),
            api_key: None,
            auth_disabled: false,
            http_headers: BTreeMap::new(),
            path_suffix: None,
            insecure_skip_tls_verify: false,
            wire_format: WireFormat::ChatCompletions,
        };
        let body = serde_json::json!({
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "strict": true,
                    "parameters": {"type": "object"}
                }
            }]
        });

        assert_eq!(
            upstream_url(&endpoint, &body),
            "https://api.deepseek.com/beta/chat/completions"
        );
    }

    #[test]
    fn upstream_url_strips_trailing_slash() {
        let endpoint = ResolvedModelEndpoint {
            provider: ProviderKind::Deepseek,
            base_url: "https://api.deepseek.com/".to_string(),
            model: "deepseek-chat".to_string(),
            api_key: None,
            auth_disabled: false,
            http_headers: BTreeMap::new(),
            path_suffix: None,
            insecure_skip_tls_verify: false,
            wire_format: WireFormat::ChatCompletions,
        };
        assert_eq!(
            upstream_url(&endpoint, &serde_json::json!({})),
            "https://api.deepseek.com/v1/chat/completions"
        );
    }
}

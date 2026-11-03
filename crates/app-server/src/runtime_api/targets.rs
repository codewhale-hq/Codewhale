//! Remote / cloud attach surface for native clients (APPS-50).
//!
//! A "target" is a Codewhale runtime a client talks to. This server is always
//! the local target; remote targets are other `codewhale serve --http`
//! endpoints, and the *client* owns that registry — a runtime never persists
//! peer endpoints, relays sessions, or mints a second process owner. SSH
//! workspaces and hosted cloud computers are control-plane features, so those
//! routes answer honestly (`supported: false`) and refuse writes with 501
//! instead of 404 — a client can render real disabled states with reasons.
//!
//! Routes:
//!   GET  /v1/targets          — this runtime's target record
//!   POST /v1/targets          — 501: target registry is client-owned
//!   POST /v1/targets/switch   — 501: client-owned; never mid-turn server-side
//!   GET  /v1/remote           — this runtime's reachability posture
//!   POST /v1/remote/connect   — probe a candidate remote runtime endpoint
//!   GET  /v1/ssh              — SSH workspaces: control-plane owned
//!   POST /v1/ssh/connect      — 501
//!   GET  /v1/cloud            — hosted computers: control-plane owned
//!   POST /v1/cloud/attach     — 501

use std::net::IpAddr;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ApiError, RuntimeApiState};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_MAX_BYTES: usize = 64 * 1024;
const CONTROL_PLANE_OWNER: &str = "codewhale-control-plane";

fn self_target(state: &RuntimeApiState) -> Value {
    json!({
        "id": "local",
        "kind": "local",
        "current": true,
        "endpoint": format!("http://{}:{}", display_host(&state.bind_host), state.bind_port),
        "auth_required": state.auth_required,
        "service": "codewhale-runtime-api",
        "codewhale_version": env!("CARGO_PKG_VERSION"),
        "state": "ready",
    })
}

fn display_host(host: &str) -> String {
    if host.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn unsupported_surface(feature: &str) -> Value {
    json!({
        "supported": false,
        "owner": CONTROL_PLANE_OWNER,
        "reason": format!(
            "{feature} is owned by the Codewhale control plane (managed apps); this runtime does not broker it"
        ),
    })
}

pub(super) async fn list_targets(State(state): State<RuntimeApiState>) -> Json<Value> {
    Json(json!({
        "targets": [self_target(&state)],
        // Remote attach means the client points at another runtime's /v1 API;
        // nothing server-side needs to (or may) switch.
        "remote": {
            "supported": true,
            "attach": "client",
            "probe": "POST /v1/remote/connect",
        },
        "ssh": unsupported_surface("SSH remote workspaces"),
        "cloud": unsupported_surface("Hosted cloud computers"),
        "tailnet": {
            "supported": true,
            "inventory": "/v1/tailnet",
            "enrollment": "external",
            "execution": "explicit_fleet_workers",
        },
        "placement": { "version": 1, "preview": "POST /v1/fleet/placement", "dispatch": "operator_fleet_cli" },
    }))
}

pub(super) async fn create_target(State(_state): State<RuntimeApiState>) -> ApiError {
    ApiError::not_implemented(
        "target registry is client-owned; attach by pointing the client at a runtime endpoint",
    )
}

pub(super) async fn switch_target(State(_state): State<RuntimeApiState>) -> ApiError {
    ApiError::not_implemented(
        "target switching is client-owned; a switch must never move a running task or replay input",
    )
}

pub(super) async fn remote_status(State(state): State<RuntimeApiState>) -> Json<Value> {
    let loopback_only = super::is_loopback_bind_host(&state.bind_host);
    Json(json!({
        "bind_host": state.bind_host,
        "port": state.bind_port,
        "loopback_only": loopback_only,
        "reachable_from_lan": !loopback_only,
        "auth_required": state.auth_required,
        "mobile": state.mobile_enabled,
        // The runtime API has no TLS terminator; non-loopback reachability
        // assumes a verified overlay (VPN/mesh), never plain LAN trust.
        "tls": false,
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoteConnectRequest {
    /// Base URL of the candidate runtime, e.g. `http://192.168.1.5:7878`.
    /// Credentials in the URL are refused — runtime tokens never travel
    /// through this server.
    endpoint: String,
}

/// Probe a candidate remote endpoint's `/v1/runtime/info`. The probe is
/// unauthenticated against the remote on purpose: this route must never be a
/// bearer-token forwarding primitive, and `runtime/info` answers signed-out
/// identity + `auth_required` without one. A negative reachability verdict is
/// data (`ok: false`), not a server error — the probe itself succeeded.
pub(super) async fn remote_connect(
    State(_state): State<RuntimeApiState>,
    Json(req): Json<RemoteConnectRequest>,
) -> Result<Json<Value>, ApiError> {
    let url = reqwest::Url::parse(req.endpoint.trim())
        .map_err(|_| ApiError::bad_request("endpoint must be an http(s) URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ApiError::bad_request(
            "endpoint scheme must be http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ApiError::bad_request(
            "credentials in endpoints are refused; send the remote's runtime token from the client",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| ApiError::bad_request("endpoint must name a host"))?;
    // Origin only — the runtime API always serves at /v1 regardless of the
    // path a user pasted.
    let origin = match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    };
    let probe_url = format!("{origin}/v1/runtime/info");

    let client = codewhale_release::tls::reqwest_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|error| ApiError::internal(format!("probe client unavailable: {error}")))?;
    let mut response = match client.get(&probe_url).send().await {
        Ok(response) => response,
        Err(error) => {
            return Ok(Json(json!({
                "ok": false,
                "endpoint": origin,
                "reason": "unreachable",
                "detail": error.to_string(),
            })));
        }
    };
    let status = response.status();
    let mut bytes = Vec::new();
    let mut too_large = false;
    while let Ok(Some(chunk)) = response.chunk().await {
        if bytes.len() + chunk.len() > PROBE_MAX_BYTES {
            too_large = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    if too_large {
        return Ok(Json(json!({
            "ok": false,
            "endpoint": origin,
            "reason": "response too large to be a runtime identity",
        })));
    }
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(body) => body,
        Err(_) => {
            return Ok(Json(json!({
                "ok": false,
                "endpoint": origin,
                "status": status.as_u16(),
                "reason": "not a Codewhale runtime",
            })));
        }
    };
    if body.get("service").and_then(Value::as_str) != Some("codewhale-runtime-api") {
        return Ok(Json(json!({
            "ok": false,
            "endpoint": origin,
            "status": status.as_u16(),
            "reason": "not a Codewhale runtime",
        })));
    }
    Ok(Json(json!({
        "ok": true,
        "endpoint": origin,
        "remote": {
            "kind": "remote",
            "endpoint": origin,
            "service": "codewhale-runtime-api",
            "runtime_api_version": body.get("runtime_api_version").cloned().unwrap_or(Value::Null),
            "codewhale_version": body.get("codewhale_version").cloned().unwrap_or(Value::Null),
            "auth_required": body.get("auth_required").cloned().unwrap_or(Value::Null),
            "bind_host": body.get("bind_host").cloned().unwrap_or(Value::Null),
            "port": body.get("port").cloned().unwrap_or(Value::Null),
        },
        // The verdict is all this server knows: attaching means the client
        // re-targets its own transport at `endpoint` with that runtime's token.
        "attach": "client",
    })))
}

pub(super) async fn ssh_status(State(_state): State<RuntimeApiState>) -> Json<Value> {
    Json(unsupported_surface("SSH remote workspaces"))
}

pub(super) async fn ssh_connect(State(_state): State<RuntimeApiState>) -> ApiError {
    ApiError::not_implemented(
        "SSH remote workspaces are owned by the Codewhale control plane; this runtime does not open SSH transports",
    )
}

pub(super) async fn cloud_status(State(_state): State<RuntimeApiState>) -> Json<Value> {
    Json(unsupported_surface("Hosted cloud computers"))
}

pub(super) async fn cloud_attach(State(_state): State<RuntimeApiState>) -> ApiError {
    ApiError::not_implemented(
        "hosted cloud computers are owned by the Codewhale control plane; this runtime does not provision them",
    )
}

pub(super) async fn tailnet_inventory(
    State(_state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    let result = tokio::task::spawn_blocking(codewhale_engine::fleet::tailnet::inventory)
        .await
        .map_err(|error| ApiError::internal(format!("Tailnet inventory task failed: {error}")))?;
    Ok(Json(match result {
        Ok(nodes) => json!({ "available": true, "nodes": nodes, "grants_execution": false }),
        Err(error) => {
            json!({ "available": false, "nodes": [], "code": "tailnet_unavailable", "reason": error.to_string(), "grants_execution": false })
        }
    }))
}

pub(super) async fn placement_preview(
    State(_state): State<RuntimeApiState>,
    Json(mut document): Json<codewhale_engine::fleet::task_spec::FleetTaskSpecDocument>,
) -> Result<Json<Value>, ApiError> {
    if document.tasks.len() > 64 {
        return Err(ApiError::bad_request(
            "Placement previews support up to 64 tasks",
        ));
    }
    codewhale_engine::fleet::task_spec::validate_task_spec_document(&document)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    if document.workers.is_empty() {
        document.workers.push(serde_json::from_value(json!({
            "id": "local", "name": "This computer", "host": { "kind": "local" }, "capabilities": ["local"],
        })).map_err(|error| ApiError::internal(error.to_string()))?);
    }
    let unavailable = std::collections::BTreeSet::new();
    let decisions: Vec<_> = document
        .tasks
        .iter()
        .map(|task| {
            codewhale_engine::fleet::placement::decide(task, &document.workers, &unavailable)
        })
        .collect();
    Ok(Json(
        json!({ "execution": "preview", "evidence": "operator_declared", "grants_execution": false, "decisions": decisions }),
    ))
}

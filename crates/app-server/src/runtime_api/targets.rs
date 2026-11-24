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

use std::net::{IpAddr, SocketAddr};
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

fn is_blocked_remote_probe_ip(ip: &IpAddr) -> bool {
    if codewhale_net::guard::is_cloud_metadata_ip(ip) {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.octets()[0] == 0
                || v4.is_multicast()
                || v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            if codewhale_net::guard::embedded_ipv4_addrs(*v6)
                .iter()
                .any(|v4| is_blocked_remote_probe_ip(&IpAddr::V4(*v4)))
            {
                return true;
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || matches!(v6.segments(), [0xfe80..=0xfebf, ..])
        }
    }
}

fn is_loopback_hostname(host: &str) -> bool {
    let bare = host.trim().trim_end_matches('.').to_ascii_lowercase();
    bare == "localhost" || bare == "localhost.localdomain" || bare.ends_with(".localhost")
}

fn parse_literal_ip(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
        .parse()
        .ok()
}

enum ProbeTargetResolution {
    Pinned(Option<(String, IpAddr)>),
    Unreachable(String),
}

async fn validate_and_resolve_probe_target(
    host: &str,
    port: u16,
) -> Result<ProbeTargetResolution, ApiError> {
    const BLOCKED_MSG: &str =
        "remote probe refuses loopback, link-local, metadata, unspecified, and multicast targets";

    if is_loopback_hostname(host) {
        return Err(ApiError::bad_request(BLOCKED_MSG));
    }
    if let Some(ip) = parse_literal_ip(host) {
        if is_blocked_remote_probe_ip(&ip) {
            return Err(ApiError::bad_request(BLOCKED_MSG));
        }
        return Ok(ProbeTargetResolution::Pinned(None));
    }
    let resolved =
        match tokio::time::timeout(PROBE_TIMEOUT, tokio::net::lookup_host((host, port))).await {
            Ok(Ok(addrs)) => addrs.collect::<Vec<SocketAddr>>(),
            Ok(Err(error)) => return Ok(ProbeTargetResolution::Unreachable(error.to_string())),
            Err(_) => {
                return Ok(ProbeTargetResolution::Unreachable(
                    "timed out resolving endpoint host".to_string(),
                ));
            }
        };
    if resolved.is_empty() {
        return Ok(ProbeTargetResolution::Unreachable(
            "endpoint host resolved to no addresses".to_string(),
        ));
    }
    for addr in &resolved {
        if is_blocked_remote_probe_ip(&addr.ip()) {
            return Err(ApiError::bad_request(BLOCKED_MSG));
        }
    }
    Ok(ProbeTargetResolution::Pinned(Some((
        host.to_string(),
        resolved[0].ip(),
    ))))
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
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ApiError::bad_request("endpoint must specify a valid port"))?;
    // Origin only — the runtime API always serves at /v1 regardless of the
    // path a user pasted.
    let origin = match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    };
    let dns_pin = match validate_and_resolve_probe_target(host, port).await? {
        ProbeTargetResolution::Pinned(pin) => pin,
        ProbeTargetResolution::Unreachable(detail) => {
            return Ok(Json(json!({
                "ok": false,
                "endpoint": origin,
                "reason": "unreachable",
                "detail": detail,
            })));
        }
    };
    let probe_url = format!("{origin}/v1/runtime/info");

    let mut builder = codewhale_net::guard::guarded_reqwest_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(PROBE_TIMEOUT);
    if let Some((pin_host, pin_ip)) = dns_pin {
        builder = builder.resolve(&pin_host, SocketAddr::new(pin_ip, port));
    }
    let client = builder
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_probe_blocks_loopback_metadata_unspecified_and_multicast_ips() {
        for blocked in [
            "127.0.0.1",
            "127.255.255.254",
            "169.254.169.254",
            "169.254.0.1",
            "100.100.100.200",
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "::",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:0.0.0.0",
            "::ffff:0:127.0.0.1",
            "64:ff9b::127.0.0.1",
            "64:ff9b::0.0.0.0",
            "64:ff9b:1::127.0.0.1",
            "2002:7f00:1::",
            "2001:0:7f00:1::",
        ] {
            let ip: IpAddr = blocked.parse().unwrap();
            assert!(
                is_blocked_remote_probe_ip(&ip),
                "{blocked} should be blocked"
            );
        }
        for allowed in [
            "192.168.1.5",
            "10.0.0.12",
            "100.64.0.1",
            "93.184.216.34",
            "fd7a:115c:a1e0::1",
            "2606:2800:220:1:248:1893:25c8:1946",
        ] {
            let ip: IpAddr = allowed.parse().unwrap();
            assert!(
                !is_blocked_remote_probe_ip(&ip),
                "{allowed} should be allowed for remote runtime probes"
            );
        }
    }

    #[tokio::test]
    async fn validate_and_resolve_probe_target_blocks_localhost_and_metadata() {
        for host in [
            "localhost",
            "sub.localhost",
            "localhost.localdomain",
            "127.0.0.1",
            "[::1]",
            "169.254.169.254",
            "[fe80::1]",
            "0.0.0.0",
            "[::]",
            "224.0.0.1",
        ] {
            assert!(
                validate_and_resolve_probe_target(host, 7878).await.is_err(),
                "{host} should be rejected"
            );
        }
    }
}

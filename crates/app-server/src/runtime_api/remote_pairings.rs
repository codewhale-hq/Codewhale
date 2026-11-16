use std::net::SocketAddr;
use std::time::Instant;

use axum::Json;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use codewhale_engine::extension_host::{RemoteDenial, RemoteSecret, RemoteState, manager};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::mobile::RemoteGrantBinding;
use super::{ApiError, RuntimeApiState, auth};

#[derive(Clone)]
pub(super) struct RemoteSessionScope {
    pub(super) grant: String,
    pub(super) session_id: String,
    pub(super) token: RemoteSecret,
    pub(super) cancel: CancellationToken,
    pub(super) expires_at: Instant,
}

impl std::fmt::Debug for RemoteSessionScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSessionScope")
            .field("grant", &self.grant)
            .field("session_id", &self.session_id)
            .field("token", &"[redacted]")
            .field("cancelled", &self.cancel.is_cancelled())
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MobileRoute<'a> {
    StreamTicketRefresh,
    ThreadsList,
    ThreadsSummary,
    ThreadGet(&'a str),
    ThreadEvents(&'a str),
    TurnCreate(&'a str),
    TurnSteer(&'a str),
    TurnInterrupt(&'a str),
    Disallowed,
}

impl<'a> MobileRoute<'a> {
    pub(super) fn thread_id(self) -> Option<&'a str> {
        match self {
            Self::ThreadGet(id)
            | Self::ThreadEvents(id)
            | Self::TurnCreate(id)
            | Self::TurnSteer(id)
            | Self::TurnInterrupt(id) => Some(id),
            Self::StreamTicketRefresh
            | Self::ThreadsList
            | Self::ThreadsSummary
            | Self::Disallowed => None,
        }
    }

    pub(super) fn is_allowed(self) -> bool {
        !matches!(self, Self::Disallowed)
    }
}

fn valid_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains('\\')
        && !lower.contains("%2f")
        && !lower.contains("%5c")
}

pub(super) fn classify_mobile_route<'a>(method: &Method, path: &'a str) -> MobileRoute<'a> {
    let segments: Vec<&str> = path.split('/').collect();
    match (method, segments.as_slice()) {
        (&Method::POST, ["", "__codewhale", "mobile", "stream-ticket"]) => {
            MobileRoute::StreamTicketRefresh
        }
        (&Method::GET, ["", "v1", "threads"]) => MobileRoute::ThreadsList,
        (&Method::GET, ["", "v1", "threads", "summary"]) => MobileRoute::ThreadsSummary,
        (&Method::GET, ["", "v1", "threads", id]) if valid_segment(id) => {
            MobileRoute::ThreadGet(id)
        }
        (&Method::GET, ["", "v1", "threads", id, "events"]) if valid_segment(id) => {
            MobileRoute::ThreadEvents(id)
        }
        (&Method::POST, ["", "v1", "threads", id, "turns"]) if valid_segment(id) => {
            MobileRoute::TurnCreate(id)
        }
        (&Method::POST, ["", "v1", "threads", id, "turns", turn_id, "steer"])
            if valid_segment(id) && valid_segment(turn_id) =>
        {
            MobileRoute::TurnSteer(id)
        }
        (&Method::POST, ["", "v1", "threads", id, "turns", turn_id, "interrupt"])
            if valid_segment(id) && valid_segment(turn_id) =>
        {
            MobileRoute::TurnInterrupt(id)
        }
        _ => MobileRoute::Disallowed,
    }
}

pub(super) enum GrantAuthOutcome {
    Allow(RemoteSessionScope),
    Forbidden,
    DropAndUnauthorized,
}

pub(super) fn authorize_grant_binding(
    binding: &RemoteGrantBinding,
    route: MobileRoute<'_>,
) -> GrantAuthOutcome {
    if binding.cancel.is_cancelled() || Instant::now() >= binding.expires_at {
        return GrantAuthOutcome::DropAndUnauthorized;
    }
    let target_session = route.thread_id().unwrap_or(&binding.session_id);
    match manager().authorize_remote_client(binding.token.expose(), target_session) {
        Ok(_grant) => {
            if route.is_allowed() {
                GrantAuthOutcome::Allow(RemoteSessionScope {
                    grant: binding.grant.clone(),
                    session_id: binding.session_id.clone(),
                    token: binding.token.clone(),
                    cancel: binding.cancel.clone(),
                    expires_at: binding.expires_at,
                })
            } else {
                GrantAuthOutcome::Forbidden
            }
        }
        Err(RemoteDenial::ForeignSession) => GrantAuthOutcome::Forbidden,
        Err(RemoteDenial::Unknown | RemoteDenial::NonLoopback) => {
            GrantAuthOutcome::DropAndUnauthorized
        }
    }
}

pub(super) fn scope_is_live_sync(scope: &RemoteSessionScope) -> bool {
    !scope.cancel.is_cancelled()
        && Instant::now() < scope.expires_at
        && manager()
            .authorize_remote_client(scope.token.expose(), &scope.session_id)
            .is_ok()
}

pub(super) async fn scope_is_live(state: &RuntimeApiState, scope: &RemoteSessionScope) -> bool {
    scope_is_live_sync(scope)
        && state
            .runtime_threads
            .get_thread(&scope.session_id)
            .await
            .is_ok_and(|thread| !thread.archived)
}

pub(super) async fn scope_is_live_for_frame(
    state: &RuntimeApiState,
    scope: &RemoteSessionScope,
    frame: &crate::RuntimeOwnerEventFrame,
) -> bool {
    if !scope_is_live_sync(scope) {
        return false;
    }
    if matches!(
        frame.event.as_str(),
        "approval.grant_revoked" | "thread.updated" | "thread.deleted"
    ) {
        return state
            .runtime_threads
            .get_thread(&scope.session_id)
            .await
            .is_ok_and(|thread| !thread.archived);
    }
    true
}

pub(super) fn evict_scope(state: &RuntimeApiState, scope: &RemoteSessionScope) {
    scope.cancel.cancel();
    if let Some(mobile) = state.mobile.as_ref() {
        mobile.revoke_grant(&scope.grant);
    }
}

#[derive(Serialize)]
pub(super) struct RemotePairingItem {
    grant: String,
    plugin_id: String,
    plugin_name: String,
    session_id: String,
    state: &'static str,
    plugin_label: String,
    client_label: Option<String>,
    expires_in_seconds: u64,
}

fn pairing_state_name(state: RemoteState) -> &'static str {
    match state {
        RemoteState::AwaitingHost => "awaiting_host",
        RemoteState::Paired => "paired",
    }
}

fn unknown_grant_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": {
                "message": "not found",
                "status": StatusCode::NOT_FOUND.as_u16(),
            }
        })),
    )
        .into_response()
}

pub(super) async fn list_pairings() -> Response {
    let items: Vec<RemotePairingItem> = manager()
        .remote_pairings()
        .into_iter()
        .map(|row| RemotePairingItem {
            grant: row.grant,
            plugin_id: row.plugin_id,
            plugin_name: row.plugin_name,
            session_id: row.session_id,
            state: pairing_state_name(row.state),
            plugin_label: row.plugin_label,
            client_label: row.client_label,
            expires_in_seconds: row.expires_in_seconds,
        })
        .collect();
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(items),
    )
        .into_response()
}

pub(super) async fn present_pairing(
    State(state): State<RuntimeApiState>,
    Path(grant): Path<String>,
) -> Response {
    if state.mobile.is_none() {
        return ApiError::conflict("mobile control is disabled").into_response();
    }
    let Some(row) = manager()
        .remote_pairings()
        .into_iter()
        .find(|item| item.grant == grant)
    else {
        return unknown_grant_response();
    };
    if row.state != RemoteState::AwaitingHost {
        return unknown_grant_response();
    }
    match state.runtime_threads.get_thread(&row.session_id).await {
        Ok(thread) if !thread.archived => {}
        Ok(_) | Err(_) => {
            return ApiError::conflict("pairing conversation is not available over Runtime HTTP")
                .into_response();
        }
    }
    let Ok(secret) = manager().present_remote_pairing(&grant) else {
        return unknown_grant_response();
    };
    let origin = auth::runtime_http_origin(&state);
    let url = format!("{origin}/__codewhale/mobile/pair/{}", secret.expose());
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
        Json(json!({
            "grant": grant,
            "url": url,
            "expires_in_seconds": row.expires_in_seconds,
        })),
    )
        .into_response()
}

pub(super) async fn withdraw_pairing(
    State(state): State<RuntimeApiState>,
    Path(grant): Path<String>,
) -> Response {
    let _ = manager().withdraw_remote_grant(&grant);
    if let Some(mobile) = state.mobile.as_ref() {
        mobile.revoke_grant(&grant);
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(super) struct RedeemPairingQuery {
    label: Option<String>,
}

pub(super) async fn redeem_pairing(
    State(state): State<RuntimeApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(secret): Path<String>,
    headers: HeaderMap,
    Query(query): Query<RedeemPairingQuery>,
) -> Response {
    let Some(mobile_state) = state.mobile.as_ref() else {
        return super::mobile_not_found();
    };
    let header_label = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let client_label = query
        .label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or(header_label)
        .unwrap_or("Mobile browser");

    let client = match manager().redeem_remote_pairing(&secret, client_label, peer.ip()) {
        Ok(client) => client,
        Err(RemoteDenial::NonLoopback) => {
            return super::secured_mobile_text(StatusCode::FORBIDDEN, "bootstrap unavailable");
        }
        Err(RemoteDenial::Unknown | RemoteDenial::ForeignSession) => {
            return super::secured_mobile_text(StatusCode::UNAUTHORIZED, "bootstrap unavailable");
        }
    };

    let session = mobile_state.issue_grant_session(
        client.grant,
        client.session_id,
        client.token,
        client.expires_in_seconds,
    );
    super::mobile_redirect_response(session)
}

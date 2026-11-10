//! Embedded, loopback-only browser client for the Runtime API.

use codewhale_core::secret_eq::constant_time_eq;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use super::RuntimeApiState;

const WEB_HTML: &str = include_str!("../runtime_web/index.html");
const WEB_CSS: &str = include_str!("../runtime_web/styles.css");
const WEB_JS: &str = include_str!("../runtime_web/app.mjs");
const WEB_ICON: &[u8] = include_bytes!("../runtime_web/codewhale-192.png");
// The nonce remains single-use and loopback-only, but it is handed to a
// person through the browser launcher or terminal. Two minutes proved too
// short when the launcher was delayed or did not open a tab.
pub(super) const BOOTSTRAP_TTL: Duration = Duration::from_secs(10 * 60);
const WEB_SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const BOOTSTRAP_PREFIX: &str = "cwwb_";
const WEB_SESSION_PREFIX: &str = "cwws_";
pub(super) const WEB_REQUEST_HEADER: &str = "x-codewhale-web-request";
pub(super) const WEB_STREAM_TICKET_QUERY: &str = "web_stream_ticket";
const WEB_SESSION_COOKIE_NAME: &str = "codewhale_web_session";
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'";

#[derive(Clone)]
pub(super) struct RuntimeWebState {
    bootstrap: Arc<Mutex<Option<BootstrapCapability>>>,
    session_token: Arc<str>,
    request_proof: Arc<Mutex<Option<String>>>,
    stream_tickets: Arc<Mutex<Vec<BootstrapCapability>>>,
    session_expires_at: Instant,
}

struct BootstrapCapability {
    nonce: String,
    expires_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BootstrapError {
    Invalid,
    Expired,
    NonLoopback,
}

impl RuntimeWebState {
    pub(super) fn new() -> (Self, String) {
        Self::new_with_ttls(BOOTSTRAP_TTL, WEB_SESSION_TTL)
    }

    fn new_with_ttls(bootstrap_ttl: Duration, session_ttl: Duration) -> (Self, String) {
        let nonce = format!("{BOOTSTRAP_PREFIX}{}", Uuid::new_v4().simple());
        let session_token = format!(
            "{WEB_SESSION_PREFIX}{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let state = Self {
            bootstrap: Arc::new(Mutex::new(Some(BootstrapCapability {
                nonce: nonce.clone(),
                expires_at: Instant::now() + bootstrap_ttl,
            }))),
            session_token: session_token.into(),
            request_proof: Arc::new(Mutex::new(None)),
            stream_tickets: Arc::new(Mutex::new(Vec::new())),
            session_expires_at: Instant::now() + session_ttl,
        };
        (state, nonce)
    }

    pub(super) fn consume(
        &self,
        nonce: &str,
        peer_ip: IpAddr,
    ) -> Result<(String, String), BootstrapError> {
        if !peer_ip.is_loopback() {
            return Err(BootstrapError::NonLoopback);
        }
        if !valid_bootstrap_nonce(nonce) {
            return Err(BootstrapError::Invalid);
        }

        let mut slot = self
            .bootstrap
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(capability) = slot.as_ref() else {
            return Err(BootstrapError::Invalid);
        };
        if Instant::now() >= capability.expires_at {
            *slot = None;
            return Err(BootstrapError::Expired);
        }
        if !constant_time_eq(nonce.as_bytes(), capability.nonce.as_bytes()) {
            return Err(BootstrapError::Invalid);
        }

        let _capability = slot.take().expect("bootstrap capability checked above");
        let proof = format!(
            "cwwr_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        *self.request_proof.lock().unwrap_or_else(|p| p.into_inner()) = Some(proof.clone());
        Ok((self.session_token.to_string(), proof))
    }

    pub(super) fn matches_request(&self, cookie_header: Option<&str>, proof: Option<&str>) -> bool {
        self.matches_session_cookie(cookie_header)
            && proof.is_some_and(|proof| {
                self.request_proof
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref()
                    .is_some_and(|expected| constant_time_eq(proof.as_bytes(), expected.as_bytes()))
            })
    }

    // Reconnects consume short-lived, single-use tickets. Retain up to 32
    // pending tickets so independent tabs do not replace each other's ticket;
    // excess requests evict the oldest pending ticket.
    pub(super) fn refresh_stream_ticket(
        &self,
        cookie_header: Option<&str>,
        proof: Option<&str>,
    ) -> Option<String> {
        if !self.matches_request(cookie_header, proof) {
            return None;
        }
        let ticket = format!(
            "cwwt_{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let mut tickets = self
            .stream_tickets
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        tickets.retain(|issued| now < issued.expires_at);
        if tickets.len() == 32 {
            tickets.remove(0);
        }
        tickets.push(BootstrapCapability {
            nonce: ticket.clone(),
            expires_at: now + super::mobile::STREAM_TICKET_TTL,
        });
        Some(ticket)
    }

    pub(super) fn consume_stream_ticket(
        &self,
        cookie_header: Option<&str>,
        ticket: Option<&str>,
    ) -> bool {
        if !self.matches_session_cookie(cookie_header) {
            return false;
        }
        let mut tickets = self
            .stream_tickets
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        tickets.retain(|issued| now < issued.expires_at);
        let Some(index) = tickets.iter().position(|issued| {
            ticket
                .is_some_and(|ticket| constant_time_eq(ticket.as_bytes(), issued.nonce.as_bytes()))
        }) else {
            return false;
        };
        tickets.remove(index);
        true
    }

    pub(super) fn matches_session_cookie(&self, cookie_header: Option<&str>) -> bool {
        let presented = cookie_value(cookie_header, WEB_SESSION_COOKIE_NAME).unwrap_or_default();
        let token_matches = constant_time_eq(presented.as_bytes(), self.session_token.as_bytes());
        token_matches & (Instant::now() < self.session_expires_at)
    }
}

pub(super) fn bootstrap_url(addr: SocketAddr, nonce: &str) -> String {
    format!("http://{addr}/__codewhale/bootstrap/{nonce}")
}

pub(super) async fn exchange_bootstrap(
    State(state): State<RuntimeApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(nonce): Path<String>,
) -> Response {
    let Some(web) = state.web.as_ref() else {
        return not_found();
    };
    let (session_token, request_proof) = match web.consume(&nonce, peer.ip()) {
        Ok(token) => token,
        Err(BootstrapError::NonLoopback) => {
            return secured_text(StatusCode::FORBIDDEN, "bootstrap unavailable");
        }
        Err(BootstrapError::Invalid | BootstrapError::Expired) => {
            return secured_text(StatusCode::UNAUTHORIZED, "bootstrap unavailable");
        }
    };

    let cookie = web_session_cookie(&session_token);
    let mut response = (StatusCode::SEE_OTHER, "").into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/#p={request_proof}")).expect("hex request proof"),
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("percent-encoded Runtime cookie is a valid header"),
    );
    secure_headers(&mut response, "text/plain; charset=utf-8");
    response
}

pub(super) async fn refresh_stream_ticket(
    State(state): State<RuntimeApiState>,
    req: Request,
) -> Response {
    let Some(web) = state.web.as_ref() else {
        return not_found();
    };
    if !super::auth::web_session_request_is_authorized(&req, &state, web) {
        return super::auth::runtime_token_required_response();
    }
    let ticket = web.refresh_stream_ticket(
        req.headers()
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok()),
        req.headers()
            .get(WEB_REQUEST_HEADER)
            .and_then(|v| v.to_str().ok()),
    );
    let Some(ticket) = ticket else {
        return super::auth::runtime_token_required_response();
    };
    let mut response = Json(serde_json::json!({"stream_ticket": ticket})).into_response();
    secure_headers(&mut response, "application/json");
    response
}

pub(super) async fn web_page(State(state): State<RuntimeApiState>) -> Response {
    if state.web.is_none() {
        return not_found();
    }
    web_page_response()
}

fn web_page_response() -> Response {
    secured_asset("text/html; charset=utf-8", WEB_HTML.to_owned())
}

pub(super) async fn web_styles(State(state): State<RuntimeApiState>) -> Response {
    if state.web.is_none() {
        return not_found();
    }
    secured_asset("text/css; charset=utf-8", WEB_CSS)
}

pub(super) async fn web_script(State(state): State<RuntimeApiState>) -> Response {
    if state.web.is_none() {
        return not_found();
    }
    secured_asset("text/javascript; charset=utf-8", WEB_JS)
}

pub(super) async fn web_icon(State(state): State<RuntimeApiState>) -> Response {
    if state.web.is_none() {
        return not_found();
    }
    let mut response = Response::new(Body::from(WEB_ICON));
    secure_headers(&mut response, "image/png");
    response
}

fn web_session_cookie(session_token: &str) -> String {
    format!("{WEB_SESSION_COOKIE_NAME}={session_token}; HttpOnly; SameSite=Strict; Path=/")
}

fn cookie_value<'a>(cookie_header: Option<&'a str>, name: &str) -> Option<&'a str> {
    cookie_header.and_then(|cookie| {
        cookie.split(';').find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name).then_some(value.trim())
        })
    })
}

fn valid_bootstrap_nonce(value: &str) -> bool {
    value.strip_prefix(BOOTSTRAP_PREFIX).is_some_and(|random| {
        random.len() == 32 && random.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn secured_asset(content_type: &'static str, body: impl IntoResponse) -> Response {
    let mut response = body.into_response();
    secure_headers(&mut response, content_type);
    response
}

fn secured_text(status: StatusCode, body: &'static str) -> Response {
    let mut response = (status, body).into_response();
    secure_headers(&mut response, "text/plain; charset=utf-8");
    response
}

fn not_found() -> Response {
    secured_text(StatusCode::NOT_FOUND, "not found")
}

fn secure_headers(response: &mut Response, content_type: &'static str) {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_surface_hardening_web_proof_and_ticket_expiry() {
        let (web, nonce) = RuntimeWebState::new();
        let (token, proof) = web.consume(&nonce, "127.0.0.1".parse().unwrap()).unwrap();
        let cookie = web_session_cookie(&token);
        assert!(!cookie.contains(&proof));
        assert!(!web.matches_request(None, Some(&proof)));
        assert!(!web.matches_request(Some(&cookie), Some(&token)));
        assert!(web.matches_request(Some(&cookie), Some(&proof)));
        let ticket = web
            .refresh_stream_ticket(Some(&cookie), Some(&proof))
            .unwrap();
        assert!(!web.consume_stream_ticket(None, Some(&ticket)));
        assert!(!web.consume_stream_ticket(Some(&cookie), Some("wrong-ticket")));
        web.stream_tickets.lock().unwrap()[0].expires_at = Instant::now();
        assert!(!web.consume_stream_ticket(Some(&cookie), Some(&ticket)));
        let ticket = web
            .refresh_stream_ticket(Some(&cookie), Some(&proof))
            .unwrap();
        let mut expired = web.clone();
        expired.session_expires_at = Instant::now();
        assert!(!expired.matches_request(Some(&cookie), Some(&proof)));
        assert!(!expired.consume_stream_ticket(Some(&cookie), Some(&ticket)));
    }

    #[tokio::test]
    async fn runtime_surface_review_web_page_never_carries_the_request_proof() {
        let (web, nonce) = RuntimeWebState::new();
        let (token, proof) = web.consume(&nonce, "127.0.0.1".parse().unwrap()).unwrap();
        let response = web_page_response();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            response.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let html = std::str::from_utf8(&body).unwrap();
        assert!(!html.contains(&proof));
        assert!(!html.contains(&token));
        assert!(!html.contains("codewhale-web-request"));
        assert_eq!(
            web.consume(&nonce, "127.0.0.1".parse().unwrap()),
            Err(BootstrapError::Invalid)
        );
    }

    #[test]
    fn runtime_surface_review_web_tabs_keep_independent_bounded_tickets() {
        let (web, nonce) = RuntimeWebState::new();
        let (token, proof) = web.consume(&nonce, "127.0.0.1".parse().unwrap()).unwrap();
        let cookie = web_session_cookie(&token);
        let first = web
            .refresh_stream_ticket(Some(&cookie), Some(&proof))
            .unwrap();
        let second = web
            .refresh_stream_ticket(Some(&cookie), Some(&proof))
            .unwrap();
        assert!(web.consume_stream_ticket(Some(&cookie), Some(&first)));
        assert!(web.consume_stream_ticket(Some(&cookie), Some(&second)));
        assert!(!web.consume_stream_ticket(Some(&cookie), Some(&first)));
        assert!(!web.consume_stream_ticket(Some(&cookie), Some(&second)));
        let oldest = web
            .refresh_stream_ticket(Some(&cookie), Some(&proof))
            .unwrap();
        for _ in 0..32 {
            web.refresh_stream_ticket(Some(&cookie), Some(&proof))
                .unwrap();
        }
        assert_eq!(web.stream_tickets.lock().unwrap().len(), 32);
        assert!(!web.consume_stream_ticket(Some(&cookie), Some(&oldest)));
    }

    #[test]
    fn bootstrap_is_loopback_only_one_time_and_expires() {
        let (state, nonce) =
            RuntimeWebState::new_with_ttls(Duration::from_secs(60), Duration::from_secs(60));
        assert_eq!(
            state.consume(&nonce, "192.0.2.4".parse().unwrap()),
            Err(BootstrapError::NonLoopback)
        );
        let (session_token, _) = state
            .consume(&nonce, "127.0.0.1".parse().unwrap())
            .expect("valid loopback bootstrap");
        assert!(session_token.starts_with(WEB_SESSION_PREFIX));
        assert!(state.matches_session_cookie(Some(&format!(
            "theme=dark; {WEB_SESSION_COOKIE_NAME}={session_token}"
        ))));
        assert_eq!(
            state.consume(&nonce, "127.0.0.1".parse().unwrap()),
            Err(BootstrapError::Invalid)
        );

        let (expired, expired_nonce) =
            RuntimeWebState::new_with_ttls(Duration::ZERO, Duration::from_secs(60));
        assert_eq!(
            expired.consume(&expired_nonce, "::1".parse().unwrap()),
            Err(BootstrapError::Expired)
        );
    }

    #[test]
    fn web_session_survives_reload_then_expires_and_rejects_wrong_tokens() {
        let (state, nonce) =
            RuntimeWebState::new_with_ttls(Duration::from_secs(60), Duration::from_secs(60));
        let (session_token, _) = state
            .consume(&nonce, "127.0.0.1".parse().unwrap())
            .expect("valid loopback bootstrap");
        let cookie = format!("{WEB_SESSION_COOKIE_NAME}={session_token}");
        assert!(state.matches_session_cookie(Some(&cookie)));
        assert!(
            state.matches_session_cookie(Some(&cookie)),
            "the same process-local session remains valid across a page reload"
        );
        assert!(!state.matches_session_cookie(Some(
            "codewhale_web_session=cwws_0000000000000000000000000000000000000000000000000000000000000000"
        )));

        let (expired, _nonce) =
            RuntimeWebState::new_with_ttls(Duration::from_secs(60), Duration::ZERO);
        let expired_cookie = format!(
            "{WEB_SESSION_COOKIE_NAME}={}",
            expired.session_token.as_ref()
        );
        assert!(!expired.matches_session_cookie(Some(&expired_cookie)));
    }

    #[test]
    fn bootstrap_rejects_malformed_or_wrong_capabilities_without_consuming() {
        let (state, nonce) = RuntimeWebState::new();
        for invalid in ["", "cwwb_short", "cwwb_gggggggggggggggggggggggggggggggg"] {
            assert_eq!(
                state.consume(invalid, "127.0.0.1".parse().unwrap()),
                Err(BootstrapError::Invalid)
            );
        }
        let mut wrong = nonce.clone();
        wrong.replace_range(wrong.len() - 1.., "0");
        if wrong == nonce {
            wrong.replace_range(wrong.len() - 1.., "1");
        }
        assert_eq!(
            state.consume(&wrong, "127.0.0.1".parse().unwrap()),
            Err(BootstrapError::Invalid)
        );
        assert!(
            state
                .consume(&nonce, "127.0.0.1".parse().unwrap())
                .expect("valid bootstrap remains available")
                .0
                .starts_with(WEB_SESSION_PREFIX)
        );
    }

    #[test]
    fn cookie_has_exact_security_attributes_without_the_runtime_bearer() {
        let session_token = format!("{WEB_SESSION_PREFIX}{}", "01".repeat(16));
        let runtime_bearer = "cwrt_runtime_secret_never_in_browser_storage";
        let cookie = web_session_cookie(&session_token);
        assert_eq!(
            cookie,
            format!("codewhale_web_session={session_token}; HttpOnly; SameSite=Strict; Path=/")
        );
        assert!(!cookie.contains(runtime_bearer));
        assert!(!cookie.contains("Domain="));
    }

    #[test]
    fn launcher_url_contains_only_the_one_time_capability() {
        let token = "cwrt_runtime_secret_never_in_browser_arguments";
        let nonce = format!("{BOOTSTRAP_PREFIX}{}", "01".repeat(16));
        let url = bootstrap_url("127.0.0.1:7878".parse().unwrap(), &nonce);
        assert!(url.ends_with(&nonce));
        assert!(!url.contains(token));
        assert!(!url.contains('?'));
        assert!(!url.contains('#'));
    }

    #[test]
    fn embedded_client_keeps_runtime_bearer_private_and_has_no_unsafe_html_sink() {
        for asset in [WEB_HTML, WEB_JS] {
            assert!(!asset.contains("localStorage"));
            assert!(!asset.contains("codewhale_runtime_token"));
            assert!(!asset.contains("innerHTML"));
            assert!(!asset.contains("http://"));
            assert!(!asset.contains("https://"));
        }
    }
}

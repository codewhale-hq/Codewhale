//! Embedded, loopback-only browser client for the Runtime API.

use codewhale_core::secret_eq::constant_time_eq;
use std::fs;
use std::io::Read as _;
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use super::RuntimeApiState;

const WEB_HTML: &str = include_str!("../runtime_web/index.html");
const WEB_CSS: &str = include_str!("../runtime_web/styles.css");
const WEB_JS: &str = include_str!("../runtime_web/app.mjs");
const WEB_ICON: &[u8] = include_bytes!("../runtime_web/codewhale-192.png");
pub(super) const WEB_DIST_ENV: &str = "CODEWHALE_WEB_DIST";
const MAX_WEB_DIST_ASSET_BYTES: usize = 16 * 1024 * 1024;
// The nonce remains single-use and loopback-only, but it is handed to a
// person through the browser launcher or terminal. Two minutes proved too
// short when the launcher was delayed or did not open a tab.
pub(super) const BOOTSTRAP_TTL: Duration = Duration::from_secs(10 * 60);
const WEB_SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const BOOTSTRAP_PREFIX: &str = "cwwb_";
const WEB_SESSION_PREFIX: &str = "cwws_";
pub(super) const WEB_REQUEST_HEADER: &str = "x-codewhale-web-request";
pub(super) const WEB_STREAM_TICKET_QUERY: &str = "web_stream_ticket";
pub(super) const WEB_SESSION_COOKIE_NAME: &str = "codewhale_web_session";
const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'";
const APP_WEB_CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; worker-src 'self'; font-src 'self'; img-src 'self' data: blob:; connect-src 'self'; media-src blob:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'self'; frame-ancestors 'none'";
const PLUGIN_HOST_CONTENT_SECURITY_POLICY: &str = "default-src 'none'; script-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-src 'none'; frame-ancestors 'self'";

#[derive(Clone)]
pub(super) struct RuntimeWebState {
    bootstrap: Arc<Mutex<Option<BootstrapCapability>>>,
    session_token: Arc<str>,
    request_proof: Arc<Mutex<Option<String>>>,
    stream_tickets: Arc<Mutex<Vec<BootstrapCapability>>>,
    session_expires_at: Instant,
    web_dist_dir: Option<Arc<PathBuf>>,
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
        Self::new_with_dist(resolve_web_dist_from_env())
    }

    pub(super) fn new_with_dist(web_dist_dir: Option<PathBuf>) -> (Self, String) {
        Self::new_with_ttls_and_dist(BOOTSTRAP_TTL, WEB_SESSION_TTL, web_dist_dir)
    }

    fn new_with_ttls(bootstrap_ttl: Duration, session_ttl: Duration) -> (Self, String) {
        Self::new_with_ttls_and_dist(bootstrap_ttl, session_ttl, None)
    }

    fn new_with_ttls_and_dist(
        bootstrap_ttl: Duration,
        session_ttl: Duration,
        web_dist_dir: Option<PathBuf>,
    ) -> (Self, String) {
        let nonce = format!("{BOOTSTRAP_PREFIX}{}", Uuid::new_v4().simple());
        let session_token = format!(
            "{WEB_SESSION_PREFIX}{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let validated_dist = web_dist_dir
            .as_deref()
            .and_then(validate_web_dist_dir)
            .map(Arc::new);
        let state = Self {
            bootstrap: Arc::new(Mutex::new(Some(BootstrapCapability {
                nonce: nonce.clone(),
                expires_at: Instant::now() + bootstrap_ttl,
            }))),
            session_token: session_token.into(),
            request_proof: Arc::new(Mutex::new(None)),
            stream_tickets: Arc::new(Mutex::new(Vec::new())),
            session_expires_at: Instant::now() + session_ttl,
            web_dist_dir: validated_dist,
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
    let Some(web) = state.web.as_ref() else {
        return not_found();
    };
    if let Some(dist_dir) = web.web_dist_dir.as_deref() {
        return serve_dist_html(dist_dir, "index.html", APP_WEB_CONTENT_SECURITY_POLICY);
    }
    web_page_response()
}

pub(super) async fn web_fallback(State(state): State<RuntimeApiState>, req: Request) -> Response {
    let Some(dist_dir) = state
        .web
        .as_ref()
        .and_then(|web| web.web_dist_dir.as_deref())
    else {
        return not_found();
    };
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return not_found();
    }
    let raw_path = req.uri().path();
    if is_reserved_runtime_path(raw_path) {
        return not_found();
    }
    let rel = raw_path.trim_start_matches('/');
    if rel.is_empty() {
        return serve_dist_html(dist_dir, "index.html", APP_WEB_CONTENT_SECURITY_POLICY);
    }
    if rel == "plugin-host.html" {
        return serve_dist_html(
            dist_dir,
            "plugin-host.html",
            PLUGIN_HOST_CONTENT_SECURITY_POLICY,
        );
    }
    let Some(clean_rel) = sanitize_dist_relative_path(rel) else {
        return not_found();
    };
    if let Some(bytes) = read_confined_dist_file(dist_dir, &clean_rel) {
        let Some(content_type) = content_type_for_path(&clean_rel) else {
            return not_found();
        };
        let is_immutable = rel.starts_with("static/");
        let csp = if content_type.starts_with("text/html") {
            APP_WEB_CONTENT_SECURITY_POLICY
        } else {
            CONTENT_SECURITY_POLICY
        };
        let mut response = Response::new(Body::from(bytes));
        secure_headers_with_policy(
            &mut response,
            content_type,
            csp,
            if is_immutable {
                "public, max-age=31536000, immutable"
            } else {
                "no-store"
            },
        );
        return response;
    }
    // Bounded SPA fallback: serve index.html only for extensionless app routes.
    if clean_rel.extension().is_none() && !rel.starts_with("static/") && !rel.starts_with("assets/")
    {
        return serve_dist_html(dist_dir, "index.html", APP_WEB_CONTENT_SECURITY_POLICY);
    }
    not_found()
}

fn resolve_web_dist_from_env() -> Option<PathBuf> {
    let raw = std::env::var_os(WEB_DIST_ENV)?;
    if raw.is_empty() {
        return None;
    }
    Some(PathBuf::from(raw))
}

fn validate_web_dist_dir(dir: &FsPath) -> Option<PathBuf> {
    let meta = fs::symlink_metadata(dir).ok()?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return None;
    }
    let canonical = fs::canonicalize(dir).ok()?;
    let index = canonical.join("index.html");
    let index_meta = fs::symlink_metadata(&index).ok()?;
    if index_meta.file_type().is_symlink() || !index_meta.is_file() {
        return None;
    }
    Some(canonical)
}

fn is_reserved_runtime_path(path: &str) -> bool {
    path == "/v1"
        || path.starts_with("/v1/")
        || path == "/__codewhale"
        || path.starts_with("/__codewhale/")
        || path == "/health"
        || path == "/mobile"
        || path.starts_with("/mobile/")
}

fn sanitize_dist_relative_path(rel: &str) -> Option<PathBuf> {
    if rel.is_empty()
        || rel.len() > 512
        || rel.contains('\\')
        || rel.bytes().any(|b| b < 0x20 || b == 0x7f)
    {
        return None;
    }
    let mut out = PathBuf::new();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.starts_with('.') {
            return None;
        }
        let mut comps = FsPath::new(part).components();
        match (comps.next(), comps.next()) {
            (Some(Component::Normal(seg)), None) => out.push(seg),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn read_confined_dist_file(root: &FsPath, rel: &FsPath) -> Option<Vec<u8>> {
    let mut current = root.to_path_buf();
    for component in rel.components() {
        let Component::Normal(seg) = component else {
            return None;
        };
        current.push(seg);
        let meta = fs::symlink_metadata(&current).ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
    }
    let meta = fs::symlink_metadata(&current).ok()?;
    if !meta.is_file() || meta.len() > MAX_WEB_DIST_ASSET_BYTES as u64 {
        return None;
    }
    let canonical = fs::canonicalize(&current).ok()?;
    if !canonical.starts_with(root) {
        return None;
    }
    let file = fs::File::open(&canonical).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_WEB_DIST_ASSET_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() > MAX_WEB_DIST_ASSET_BYTES {
        return None;
    }
    Some(buf)
}

fn serve_dist_html(root: &FsPath, file_name: &str, csp: &'static str) -> Response {
    let Some(bytes) = read_confined_dist_file(root, FsPath::new(file_name)) else {
        return not_found();
    };
    let mut response = Response::new(Body::from(bytes));
    secure_headers_with_policy(&mut response, "text/html; charset=utf-8", csp, "no-store");
    response
}

fn content_type_for_path(path: &FsPath) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "html" => Some("text/html; charset=utf-8"),
        "js" | "mjs" => Some("text/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "ico" => Some("image/x-icon"),
        "json" => Some("application/json; charset=utf-8"),
        "webmanifest" => Some("application/manifest+json; charset=utf-8"),
        "woff2" => Some("font/woff2"),
        "woff" => Some("font/woff"),
        "ttf" => Some("font/ttf"),
        "wasm" => Some("application/wasm"),
        "txt" => Some("text/plain; charset=utf-8"),
        _ => None,
    }
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
    secure_headers_with_policy(response, content_type, CONTENT_SECURITY_POLICY, "no-store");
}

fn secure_headers_with_policy(
    response: &mut Response,
    content_type: &'static str,
    csp: &'static str,
    cache_control: &'static str,
) {
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(csp),
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

    #[test]
    fn web_dist_path_confinement_and_mime_types() {
        let dir = tempfile::tempdir().unwrap();
        let dist = dir.path().join("dist");
        fs::create_dir_all(dist.join("static")).unwrap();
        fs::write(
            dist.join("index.html"),
            "<!doctype html><div id=root></div>",
        )
        .unwrap();
        fs::write(
            dist.join("plugin-host.html"),
            "<!doctype html><div id=plugin-root></div>",
        )
        .unwrap();
        fs::write(dist.join("static/app-123.js"), "console.log('ok');").unwrap();
        fs::write(dist.join("static/app-123.css"), "body { color: #000; }").unwrap();
        fs::write(dist.join(".env"), "SECRET=1").unwrap();
        let outside = dir.path().join("secret.txt");
        fs::write(&outside, "top-secret").unwrap();

        let root = validate_web_dist_dir(&dist).expect("valid dist dir");
        assert!(read_confined_dist_file(&root, FsPath::new("index.html")).is_some());
        assert!(read_confined_dist_file(&root, FsPath::new("plugin-host.html")).is_some());
        assert!(read_confined_dist_file(&root, FsPath::new("static/app-123.js")).is_some());

        assert_eq!(
            content_type_for_path(FsPath::new("static/app-123.js")),
            Some("text/javascript; charset=utf-8")
        );
        assert_eq!(
            content_type_for_path(FsPath::new("static/app-123.css")),
            Some("text/css; charset=utf-8")
        );
        assert_eq!(content_type_for_path(FsPath::new("secret.pem")), None);

        for bad in [
            "../secret.txt",
            "static/../../secret.txt",
            ".env",
            "a\\b.js",
            "",
        ] {
            assert!(
                sanitize_dist_relative_path(bad).is_none(),
                "expected refusal for {bad:?}"
            );
        }

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, dist.join("static/escape.js")).unwrap();
            assert!(read_confined_dist_file(&root, FsPath::new("static/escape.js")).is_none());
        }

        let missing = dir.path().join("no-index");
        fs::create_dir_all(&missing).unwrap();
        assert!(validate_web_dist_dir(&missing).is_none());
    }

    #[tokio::test]
    async fn web_dist_serves_index_plugin_host_assets_spa_fallback_and_preserves_bootstrap_cookie()
    {
        let dir = tempfile::tempdir().unwrap();
        let dist = dir.path().join("dist");
        fs::create_dir_all(dist.join("static")).unwrap();
        fs::write(
            dist.join("index.html"),
            "<!doctype html><div id=\"root\">app</div>",
        )
        .unwrap();
        fs::write(
            dist.join("plugin-host.html"),
            "<!doctype html><div id=\"plugin-root\">host</div>",
        )
        .unwrap();
        fs::write(dist.join("static/app-abc.js"), "export const ok = true;").unwrap();
        fs::write(dist.join("static/app-abc.css"), "body{margin:0}").unwrap();

        let (web_state, nonce) = RuntimeWebState::new_with_dist(Some(dist.clone()));
        let (session_token, proof) = web_state
            .consume(&nonce, "127.0.0.1".parse().unwrap())
            .expect("bootstrap succeeds with dist configured");
        let cookie = web_session_cookie(&session_token);
        assert!(web_state.matches_request(Some(&cookie), Some(&proof)));

        let root = web_state
            .web_dist_dir
            .as_deref()
            .expect("dist dir attached");
        let index_resp = serve_dist_html(root, "index.html", APP_WEB_CONTENT_SECURITY_POLICY);
        assert_eq!(index_resp.status(), StatusCode::OK);
        assert_eq!(
            index_resp.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert_eq!(index_resp.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            index_resp.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("frame-src 'self'")
        );

        let host_resp = serve_dist_html(
            root,
            "plugin-host.html",
            PLUGIN_HOST_CONTENT_SECURITY_POLICY,
        );
        assert_eq!(host_resp.status(), StatusCode::OK);
        assert_eq!(host_resp.headers()[header::CACHE_CONTROL], "no-store");
        let host_csp = host_resp.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap();
        assert!(host_csp.contains("connect-src 'none'"));
        assert!(host_csp.contains("frame-ancestors 'self'"));

        assert!(is_reserved_runtime_path("/v1/sessions"));
        assert!(is_reserved_runtime_path("/__codewhale/bootstrap/abc"));
        assert!(is_reserved_runtime_path("/health"));
        assert!(!is_reserved_runtime_path("/settings/plugins"));
        assert!(!is_reserved_runtime_path("/plugin-host.html"));
    }
}

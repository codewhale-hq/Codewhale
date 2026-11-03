//! Origin-bound browser session support for the loopback mobile control page.
//!
//! The Runtime bearer is deliberately never represented by these values. A
//! one-time terminal bootstrap (or an explicit bearer header at the session
//! endpoint) creates an opaque, process-local HttpOnly cookie plus browser
//! proofs held in origin-scoped session storage. The cookie is host scoped by
//! HTTP semantics and therefore can reach sibling ports; the proofs cannot.

use codewhale_core::secret_eq::constant_time_eq;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

pub(super) const MOBILE_SESSION_COOKIE_NAME: &str = "codewhale_mobile_session";
pub(super) const MOBILE_REQUEST_HEADER: &str = "x-codewhale-mobile-request";
pub(super) const MOBILE_STREAM_TICKET_QUERY: &str = "mobile_stream_ticket";
pub(super) const BOOTSTRAP_TTL: Duration = Duration::from_secs(10 * 60);
pub(super) const SESSION_TTL: Duration = Duration::from_secs(30 * 60);
pub(super) const STREAM_TICKET_TTL: Duration = Duration::from_secs(5 * 60);

const BOOTSTRAP_PREFIX: &str = "cwmb_";
const SESSION_PREFIX: &str = "cwms_";
const REQUEST_PREFIX: &str = "cwmr_";
const STREAM_PREFIX: &str = "cwmt_";

#[derive(Clone)]
pub(super) struct RuntimeMobileState {
    bootstrap: Arc<Mutex<Option<BootstrapCapability>>>,
    sessions: Arc<Mutex<HashMap<String, MobileSession>>>,
    session_ttl: Duration,
    stream_ticket_ttl: Duration,
}

struct BootstrapCapability {
    nonce: String,
    expires_at: Instant,
}

struct MobileSession {
    request_proof: String,
    stream_ticket: Option<String>,
    expires_at: Instant,
    stream_ticket_expires_at: Instant,
}

#[derive(Debug, Clone)]
pub(super) struct MobileSessionBootstrap {
    pub(super) session_cookie: String,
    pub(super) request_proof: String,
    pub(super) stream_ticket: String,
    pub(super) session_ttl_seconds: u64,
    pub(super) stream_ticket_ttl_seconds: u64,
}

#[derive(Debug, Clone)]
pub(super) struct MobileStreamTicket {
    pub(super) ticket: String,
    pub(super) expires_in_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BootstrapError {
    Invalid,
    Expired,
    NonLoopback,
}

impl RuntimeMobileState {
    pub(super) fn new() -> (Self, String) {
        Self::new_with_ttls(BOOTSTRAP_TTL, SESSION_TTL, STREAM_TICKET_TTL)
    }

    fn new_with_ttls(
        bootstrap_ttl: Duration,
        session_ttl: Duration,
        stream_ticket_ttl: Duration,
    ) -> (Self, String) {
        let nonce = random_capability(BOOTSTRAP_PREFIX);
        let state = Self {
            bootstrap: Arc::new(Mutex::new(Some(BootstrapCapability {
                nonce: nonce.clone(),
                expires_at: Instant::now() + bootstrap_ttl,
            }))),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            session_ttl,
            stream_ticket_ttl,
        };
        (state, nonce)
    }

    /// Consume the terminal bootstrap once, only from a loopback peer.
    pub(super) fn consume_bootstrap(
        &self,
        nonce: &str,
        peer_ip: IpAddr,
    ) -> Result<MobileSessionBootstrap, BootstrapError> {
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
        drop(slot);

        Ok(self.issue_session())
    }

    /// Create an opaque mobile browser session after explicit bearer proof.
    pub(super) fn issue_session(&self) -> MobileSessionBootstrap {
        let session_cookie = random_capability(SESSION_PREFIX);
        let request_proof = random_capability(REQUEST_PREFIX);
        let stream_ticket = random_capability(STREAM_PREFIX);
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| session.expires_at > now);
        sessions.insert(
            session_cookie.clone(),
            MobileSession {
                request_proof: request_proof.clone(),
                stream_ticket: Some(stream_ticket.clone()),
                expires_at: now + self.session_ttl,
                stream_ticket_expires_at: now + self.stream_ticket_ttl,
            },
        );
        MobileSessionBootstrap {
            session_cookie,
            request_proof,
            stream_ticket,
            session_ttl_seconds: self.session_ttl.as_secs(),
            stream_ticket_ttl_seconds: self.stream_ticket_ttl.as_secs(),
        }
    }

    /// Validate a cookie plus the origin-scoped proof used by fetch requests.
    pub(super) fn matches_request(
        &self,
        cookie_header: Option<&str>,
        request_proof: Option<&str>,
    ) -> bool {
        let Some(session_cookie) = cookie_value(cookie_header, MOBILE_SESSION_COOKIE_NAME) else {
            return false;
        };
        let Some(request_proof) = request_proof else {
            return false;
        };
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| session.expires_at > now);
        let Some(session) = sessions.get(session_cookie) else {
            return false;
        };
        constant_time_eq(request_proof.as_bytes(), session.request_proof.as_bytes())
    }

    /// Consume a short-lived stream ticket. A reconnect must mint a fresh one
    /// through the cookie-plus-request-proof endpoint.
    pub(super) fn consume_stream_ticket(
        &self,
        cookie_header: Option<&str>,
        stream_ticket: Option<&str>,
    ) -> bool {
        let Some(session_cookie) = cookie_value(cookie_header, MOBILE_SESSION_COOKIE_NAME) else {
            return false;
        };
        let Some(stream_ticket) = stream_ticket else {
            return false;
        };
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| session.expires_at > now);
        let Some(session) = sessions.get_mut(session_cookie) else {
            return false;
        };
        if now >= session.stream_ticket_expires_at {
            session.stream_ticket = None;
            return false;
        }
        let matches = session
            .stream_ticket
            .as_ref()
            .is_some_and(|issued| constant_time_eq(stream_ticket.as_bytes(), issued.as_bytes()));
        if matches {
            session.stream_ticket = None;
        }
        matches
    }

    /// Mint a new single-use stream ticket after a normal origin-bound request.
    pub(super) fn refresh_stream_ticket(
        &self,
        cookie_header: Option<&str>,
        request_proof: Option<&str>,
    ) -> Option<MobileStreamTicket> {
        let session_cookie = cookie_value(cookie_header, MOBILE_SESSION_COOKIE_NAME)?;
        let request_proof = request_proof?;
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sessions.retain(|_, session| session.expires_at > now);
        let session = sessions.get_mut(session_cookie)?;
        if !constant_time_eq(request_proof.as_bytes(), session.request_proof.as_bytes()) {
            return None;
        }
        let ticket = random_capability(STREAM_PREFIX);
        session.stream_ticket = Some(ticket.clone());
        session.stream_ticket_expires_at = now + self.stream_ticket_ttl;
        Some(MobileStreamTicket {
            ticket,
            expires_in_seconds: self.stream_ticket_ttl.as_secs(),
        })
    }
}

pub(super) fn bootstrap_url(addr: SocketAddr, nonce: &str) -> String {
    format!("http://{addr}/__codewhale/mobile/bootstrap/{nonce}")
}

pub(super) fn mobile_session_cookie(session_cookie: &str) -> String {
    format!(
        "{MOBILE_SESSION_COOKIE_NAME}={session_cookie}; Max-Age={}; HttpOnly; SameSite=Strict; Path=/",
        SESSION_TTL.as_secs()
    )
}

fn random_capability(prefix: &str) -> String {
    format!(
        "{prefix}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
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
        random.len() == 64 && random.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_is_loopback_only_one_time_and_yields_no_runtime_bearer() {
        let (state, nonce) = RuntimeMobileState::new_with_ttls(
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        assert!(matches!(
            state.consume_bootstrap(&nonce, "192.0.2.4".parse().unwrap()),
            Err(BootstrapError::NonLoopback)
        ));
        let session = state
            .consume_bootstrap(&nonce, "127.0.0.1".parse().unwrap())
            .expect("valid loopback bootstrap");
        assert!(session.session_cookie.starts_with(SESSION_PREFIX));
        assert!(session.request_proof.starts_with(REQUEST_PREFIX));
        assert!(session.stream_ticket.starts_with(STREAM_PREFIX));
        assert_ne!(session.session_cookie, session.request_proof);
        assert!(matches!(
            state.consume_bootstrap(&nonce, "127.0.0.1".parse().unwrap()),
            Err(BootstrapError::Invalid)
        ));
    }

    #[test]
    fn session_requires_origin_scoped_proof_and_stream_tickets_are_single_use() {
        let (state, _nonce) = RuntimeMobileState::new_with_ttls(
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let session = state.issue_session();
        let cookie = format!("{MOBILE_SESSION_COOKIE_NAME}={}", session.session_cookie);
        assert!(!state.matches_request(Some(&cookie), None));
        assert!(!state.matches_request(Some(&cookie), Some("wrong-proof")));
        assert!(state.matches_request(Some(&cookie), Some(&session.request_proof)));
        assert!(state.consume_stream_ticket(Some(&cookie), Some(&session.stream_ticket)));
        assert!(
            !state.consume_stream_ticket(Some(&cookie), Some(&session.stream_ticket)),
            "a captured EventSource URL cannot be replayed"
        );

        let replacement = state
            .refresh_stream_ticket(Some(&cookie), Some(&session.request_proof))
            .expect("valid browser request can mint one replacement");
        assert!(state.consume_stream_ticket(Some(&cookie), Some(&replacement.ticket)));
    }

    #[test]
    fn cookie_has_exact_security_attributes_and_no_domain() {
        let session_cookie = format!("{SESSION_PREFIX}{}", "01".repeat(32));
        let cookie = mobile_session_cookie(&session_cookie);
        assert_eq!(
            cookie,
            format!(
                "{MOBILE_SESSION_COOKIE_NAME}={session_cookie}; Max-Age=1800; HttpOnly; SameSite=Strict; Path=/"
            )
        );
        assert!(!cookie.contains("Domain="));
        assert!(!cookie.contains("cwrt_"));
    }

    #[test]
    fn launcher_url_contains_only_the_one_time_capability() {
        let nonce = format!("{BOOTSTRAP_PREFIX}{}", "01".repeat(32));
        let url = bootstrap_url("127.0.0.1:7878".parse().unwrap(), &nonce);
        assert!(url.ends_with(&nonce));
        assert!(!url.contains('?'));
        assert!(!url.contains('#'));
        assert!(!url.contains("cwrt_"));
    }
}

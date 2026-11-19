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

use tokio_util::sync::CancellationToken;
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
const STREAM_TICKET_PENDING_LIMIT: usize = 32;

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

#[derive(Clone)]
pub(super) struct RemoteGrantBinding {
    pub(super) grant: String,
    pub(super) session_id: String,
    pub(super) token: codewhale_engine::extension_host::RemoteSecret,
    pub(super) cancel: CancellationToken,
    pub(super) expires_at: Instant,
}

impl std::fmt::Debug for RemoteGrantBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteGrantBinding")
            .field("grant", &self.grant)
            .field("session_id", &self.session_id)
            .field("token", &"[redacted]")
            .field("cancelled", &self.cancel.is_cancelled())
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

struct MobileSession {
    request_proof: String,
    stream_tickets: Vec<BootstrapCapability>,
    expires_at: Instant,
    cancel: CancellationToken,
    remote_grant: Option<RemoteGrantBinding>,
}

impl std::fmt::Debug for MobileSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileSession")
            .field("request_proof", &"[redacted]")
            .field("stream_tickets", &self.stream_tickets.len())
            .field("expires_at", &self.expires_at)
            .field("cancelled", &self.cancel.is_cancelled())
            .field("remote_grant", &self.remote_grant)
            .finish()
    }
}

#[derive(Clone)]
pub(super) struct MobileSessionBootstrap {
    pub(super) session_cookie: String,
    pub(super) request_proof: String,
    pub(super) stream_ticket: String,
    pub(super) session_ttl_seconds: u64,
    pub(super) stream_ticket_ttl_seconds: u64,
}

impl std::fmt::Debug for MobileSessionBootstrap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileSessionBootstrap")
            .field("session_cookie", &"[redacted]")
            .field("request_proof", &"[redacted]")
            .field("stream_ticket", &"[redacted]")
            .field("session_ttl_seconds", &self.session_ttl_seconds)
            .field("stream_ticket_ttl_seconds", &self.stream_ticket_ttl_seconds)
            .finish()
    }
}

#[derive(Clone)]
pub(super) struct MobileStreamTicket {
    pub(super) ticket: String,
    pub(super) expires_in_seconds: u64,
}

impl std::fmt::Debug for MobileStreamTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileStreamTicket")
            .field("ticket", &"[redacted]")
            .field("expires_in_seconds", &self.expires_in_seconds)
            .finish()
    }
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

    pub(super) fn new_with_ttls(
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
        self.issue_session_with(None, None)
    }

    pub(super) fn issue_grant_session(
        &self,
        grant: String,
        session_id: String,
        token: codewhale_engine::extension_host::RemoteSecret,
        client_expires_in_seconds: u64,
    ) -> MobileSessionBootstrap {
        self.issue_session_with(
            Some((grant, session_id, token)),
            Some(Duration::from_secs(client_expires_in_seconds)),
        )
    }

    fn issue_session_with(
        &self,
        remote_grant: Option<(
            String,
            String,
            codewhale_engine::extension_host::RemoteSecret,
        )>,
        max_ttl: Option<Duration>,
    ) -> MobileSessionBootstrap {
        let session_cookie = random_capability(SESSION_PREFIX);
        let request_proof = random_capability(REQUEST_PREFIX);
        let stream_ticket = random_capability(STREAM_PREFIX);
        let effective_session_ttl =
            max_ttl.map_or(self.session_ttl, |limit| self.session_ttl.min(limit));
        let effective_ticket_ttl = self.stream_ticket_ttl.min(effective_session_ttl);
        let now = Instant::now();
        let expires_at = now + effective_session_ttl;
        let cancel = CancellationToken::new();
        let remote_grant = remote_grant.map(|(grant, session_id, token)| RemoteGrantBinding {
            grant,
            session_id,
            token,
            cancel: cancel.clone(),
            expires_at,
        });
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        retain_live_sessions(&mut sessions, now);
        if effective_session_ttl > Duration::ZERO {
            sessions.insert(
                session_cookie.clone(),
                MobileSession {
                    request_proof: request_proof.clone(),
                    stream_tickets: vec![BootstrapCapability {
                        nonce: stream_ticket.clone(),
                        expires_at: now + effective_ticket_ttl,
                    }],
                    expires_at,
                    cancel,
                    remote_grant,
                },
            );
        }
        MobileSessionBootstrap {
            session_cookie,
            request_proof,
            stream_ticket,
            session_ttl_seconds: effective_session_ttl.as_secs(),
            stream_ticket_ttl_seconds: effective_ticket_ttl.as_secs(),
        }
    }

    pub(super) fn authorize_proof(
        &self,
        cookie_header: Option<&str>,
        request_proof: Option<&str>,
    ) -> Option<Option<RemoteGrantBinding>> {
        let session_cookie = cookie_value(cookie_header, MOBILE_SESSION_COOKIE_NAME)?;
        let request_proof = request_proof?;
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        retain_live_sessions(&mut sessions, now);
        let session = sessions.get(session_cookie)?;
        constant_time_eq(request_proof.as_bytes(), session.request_proof.as_bytes())
            .then(|| session.remote_grant.clone())
    }

    #[cfg(test)]
    pub(super) fn matches_request(
        &self,
        cookie_header: Option<&str>,
        request_proof: Option<&str>,
    ) -> bool {
        self.authorize_proof(cookie_header, request_proof).is_some()
    }

    pub(super) fn peek_stream_ticket(
        &self,
        cookie_header: Option<&str>,
        stream_ticket: Option<&str>,
    ) -> Option<Option<RemoteGrantBinding>> {
        let session_cookie = cookie_value(cookie_header, MOBILE_SESSION_COOKIE_NAME)?;
        let stream_ticket = stream_ticket?;
        let now = Instant::now();
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        retain_live_sessions(&mut sessions, now);
        let session = sessions.get_mut(session_cookie)?;
        session
            .stream_tickets
            .retain(|issued| now < issued.expires_at);
        let matches = session
            .stream_tickets
            .iter()
            .any(|issued| constant_time_eq(stream_ticket.as_bytes(), issued.nonce.as_bytes()));
        matches.then(|| session.remote_grant.clone())
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
        retain_live_sessions(&mut sessions, now);
        let Some(session) = sessions.get_mut(session_cookie) else {
            return false;
        };
        session
            .stream_tickets
            .retain(|issued| now < issued.expires_at);
        let Some(index) = session.stream_tickets.iter().position(|issued| {
            constant_time_eq(stream_ticket.as_bytes(), issued.nonce.as_bytes())
        }) else {
            return false;
        };
        session.stream_tickets.remove(index);
        true
    }

    pub(super) fn remove_session_by_cookie(&self, cookie_header: Option<&str>) -> bool {
        let Some(session_cookie) = cookie_value(cookie_header, MOBILE_SESSION_COOKIE_NAME) else {
            return false;
        };
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(removed) = sessions.remove(session_cookie) else {
            return false;
        };
        removed.cancel.cancel();
        true
    }

    fn revoke_grant_sessions(&self, predicate: impl Fn(&RemoteGrantBinding) -> bool) -> usize {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut revoked = 0;
        sessions.retain(|_, session| {
            if session.remote_grant.as_ref().is_some_and(&predicate) {
                session.cancel.cancel();
                revoked += 1;
                false
            } else {
                true
            }
        });
        revoked
    }

    pub(super) fn revoke_grant(&self, grant: &str) -> usize {
        self.revoke_grant_sessions(|binding| binding.grant == grant)
    }

    pub(super) fn revoke_remote_session(&self, session_id: &str) -> usize {
        self.revoke_grant_sessions(|binding| binding.session_id == session_id)
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
        retain_live_sessions(&mut sessions, now);
        let session = sessions.get_mut(session_cookie)?;
        if !constant_time_eq(request_proof.as_bytes(), session.request_proof.as_bytes()) {
            return None;
        }
        let ticket = random_capability(STREAM_PREFIX);
        let ticket_ttl = self
            .stream_ticket_ttl
            .min(session.expires_at.saturating_duration_since(now));
        session
            .stream_tickets
            .retain(|issued| now < issued.expires_at);
        if session.stream_tickets.len() == STREAM_TICKET_PENDING_LIMIT {
            session.stream_tickets.remove(0);
        }
        session.stream_tickets.push(BootstrapCapability {
            nonce: ticket.clone(),
            expires_at: now + ticket_ttl,
        });
        Some(MobileStreamTicket {
            ticket,
            expires_in_seconds: ticket_ttl.as_secs(),
        })
    }
}

pub(super) fn bootstrap_url(addr: SocketAddr, nonce: &str) -> String {
    format!("http://{addr}/__codewhale/mobile/bootstrap/{nonce}")
}

pub(super) fn mobile_session_cookie_with_max_age(
    session_cookie: &str,
    max_age_seconds: u64,
) -> String {
    format!(
        "{MOBILE_SESSION_COOKIE_NAME}={session_cookie}; Max-Age={max_age_seconds}; HttpOnly; SameSite=Strict; Path=/"
    )
}

#[cfg(test)]
pub(super) fn mobile_session_cookie(session_cookie: &str) -> String {
    mobile_session_cookie_with_max_age(session_cookie, SESSION_TTL.as_secs())
}

fn retain_live_sessions(sessions: &mut HashMap<String, MobileSession>, now: Instant) {
    sessions.retain(|_, session| {
        if session.expires_at > now {
            true
        } else {
            session.cancel.cancel();
            false
        }
    });
}

fn random_capability(prefix: &str) -> String {
    format!(
        "{prefix}{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

pub(super) fn cookie_value<'a>(cookie_header: Option<&'a str>, name: &str) -> Option<&'a str> {
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
    fn refreshed_stream_tickets_coexist_within_one_session() {
        let (state, _nonce) = RuntimeMobileState::new_with_ttls(
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(60),
        );
        let session = state.issue_session();
        let cookie = format!("{MOBILE_SESSION_COOKIE_NAME}={}", session.session_cookie);
        let first = state
            .refresh_stream_ticket(Some(&cookie), Some(&session.request_proof))
            .expect("first refresh");
        let second = state
            .refresh_stream_ticket(Some(&cookie), Some(&session.request_proof))
            .expect("second refresh");
        assert_ne!(first.ticket, second.ticket);
        assert!(
            state.consume_stream_ticket(Some(&cookie), Some(&first.ticket)),
            "a second refresh must not evict the ticket a reconnect already holds"
        );
        assert!(state.consume_stream_ticket(Some(&cookie), Some(&second.ticket)));
        assert!(
            !state.consume_stream_ticket(Some(&cookie), Some(&second.ticket)),
            "each ticket stays single-use"
        );
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

    #[test]
    fn debug_formatting_redacts_session_secrets() {
        let (state, _nonce) = RuntimeMobileState::new_with_ttls(
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(30),
        );
        let bootstrap = state.issue_session();
        let cookie = format!("{MOBILE_SESSION_COOKIE_NAME}={}", bootstrap.session_cookie);
        let bootstrap_debug = format!("{bootstrap:?}");
        assert!(!bootstrap_debug.contains(&bootstrap.session_cookie));
        assert!(!bootstrap_debug.contains(&bootstrap.request_proof));
        assert!(!bootstrap_debug.contains(&bootstrap.stream_ticket));

        let ticket = state
            .refresh_stream_ticket(Some(&cookie), Some(&bootstrap.request_proof))
            .expect("refresh ticket");
        let ticket_debug = format!("{ticket:?}");
        assert!(!ticket_debug.contains(&ticket.ticket));

        assert!(state.remove_session_by_cookie(Some(&cookie)));
        assert!(!state.matches_request(Some(&cookie), Some(&bootstrap.request_proof)));
    }
}

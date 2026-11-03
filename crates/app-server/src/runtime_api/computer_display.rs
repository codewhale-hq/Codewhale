//! `/v1/computer/*` — the Engine's view of the computer it runs on.
//!
//! ARCHITECTURE §3.3 (research/computer): TigerVNC `Xvnc` serves RFB on a
//! Unix socket only (`/run/cw/vnc.sock`, group `cw-display`). This module is
//! the single external path to it:
//!
//! - `GET /v1/computer/display` upgrades to a WebSocket that carries raw RFB
//!   3.8 bytes as binary frames. The Engine completes the upstream handshake
//!   itself (security None on the socket) and offers only None downstream,
//!   because the WebSocket is already authenticated.
//! - Server-to-client bytes pass through verbatim.
//! - Client-to-server bytes go through [`ClientParser`], a length-tracked,
//!   fail-closed parser that runs in its **own task**, so a panic in it ends
//!   one display connection and never a turn. Only message types 0, 2, 3, 4,
//!   5, 6, 150 and 251 are allowed; any other type closes the stream.
//!   Input (4 key, 5 pointer, 6 clipboard, 251 resize) is dropped unless the
//!   connection's principal holds the control lease.
//! - The lease is human-only and lives here, in the Engine. Agents read it
//!   (`GET /v1/computer`) to refuse input tools while a human drives.
//!
//! Auth: every route here authenticates itself (it is merged outside the
//! `/v1` route layer) because the display WebSocket also accepts a
//! single-use `?ticket=` for browser clients that cannot set headers. Tickets
//! are redacted by [`redact_query_secrets`] wherever a URI is logged. The
//! Engine never trusts the peer address (S0 Q3: `/proxy` peers arrive as
//! `10.0.0.2`, not loopback) — only a token.
//!
//! Human keystrokes are never logged or put in events: events carry time
//! spans and counts only. Frames are never events.

use codewhale_core::secret_eq::constant_time_eq;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::extract::ws::{
    CloseFrame, Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const DISPLAY_SOCKET_ENV: &str = "CODEWHALE_COMPUTER_DISPLAY_SOCKET";
const DISPLAY_IDLE_ENV: &str = "CODEWHALE_COMPUTER_DISPLAY_IDLE_SECS";
const DEFAULT_DISPLAY_SOCKET: &str = "/run/cw/vnc.sock";
/// §5: the Engine closes idle displays after 10 minutes.
const DEFAULT_IDLE: Duration = Duration::from_secs(600);
/// A lease with no human input for this long expires.
const LEASE_IDLE_TTL: Duration = Duration::from_secs(300);
/// §2.3: client tokens last at most one hour.
const CLIENT_TOKEN_MAX_TTL_SECS: u64 = 3600;
const CLIENT_TOKEN_MIN_TTL_SECS: u64 = 60;
const CLIENT_TOKEN_MAX_ACTIVE: usize = 64;
const DISPLAY_TICKET_TTL: Duration = Duration::from_secs(30);
const DISPLAY_TICKET_MAX_ACTIVE: usize = 64;
const EVENT_LOG_CAP: usize = 512;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const SUPERVISOR_TICK: Duration = Duration::from_secs(5);
const RFB_VERSION_38: &[u8; 12] = b"RFB 003.008\n";
const DEVICE_ID_MAX_BYTES: usize = 128;

/// Query keys whose values are secrets and must never reach a log line.
const SECRET_QUERY_KEYS: &[&str] = &[
    "ticket",
    super::mobile::MOBILE_STREAM_TICKET_QUERY,
    "token",
    "access_token",
];

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Immutable authority minted into a device token; a label is never a grant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ClientTokenIntent {
    #[default]
    Watch,
    Drive,
}

/// `Owner` is the master Runtime token; a device token retains its mint intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Principal {
    Owner,
    Client {
        token_id: String,
        device_id: String,
        intent: ClientTokenIntent,
    },
}

impl Principal {
    pub(super) fn can_drive(&self) -> bool {
        matches!(
            self,
            Principal::Owner
                | Principal::Client {
                    intent: ClientTokenIntent::Drive,
                    ..
                }
        )
    }

    fn holder(&self) -> String {
        match self {
            Principal::Owner => "owner".to_string(),
            Principal::Client { device_id, .. } => format!("device:{device_id}"),
        }
    }

    fn device_id(&self) -> Option<&str> {
        match self {
            Principal::Owner => None,
            Principal::Client { device_id, .. } => Some(device_id),
        }
    }
}

struct ClientToken {
    id: String,
    device_id: String,
    intent: ClientTokenIntent,
    label: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

struct DisplayTicket {
    principal: Principal,
    expires: Instant,
}

struct Lease {
    principal: Principal,
    acquired_at: DateTime<Utc>,
    acquired_instant: Instant,
    last_activity: Instant,
    input_events: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ComputerEvent {
    pub seq: u64,
    #[serde(rename = "type")]
    pub kind: String,
    pub at: DateTime<Utc>,
    pub data: Value,
}

struct Inner {
    // Only the Unix display socket is dialed; other platforms report it absent.
    #[cfg_attr(not(unix), allow(dead_code))]
    socket_path: PathBuf,
    idle_close: Duration,
    lease_ttl: Duration,
    lease: parking_lot::Mutex<Option<Lease>>,
    client_tokens: parking_lot::Mutex<HashMap<[u8; 32], ClientToken>>,
    tickets: parking_lot::Mutex<HashMap<[u8; 32], DisplayTicket>>,
    events: parking_lot::Mutex<VecDeque<ComputerEvent>>,
    next_seq: AtomicU64,
    next_connection: AtomicU64,
    attached: AtomicU64,
}

/// Engine-side computer state: display socket, control lease, client tokens,
/// display tickets and the `computer.*` event log.
#[derive(Clone)]
pub(crate) struct ComputerState {
    inner: Arc<Inner>,
}

/// Accept a display socket path only if it is absolute and made of plain
/// components: no `.`/`..`, no NUL. The configured value cannot walk the
/// Engine out of the directory it names, and the socket-type check at use
/// (`display_socket_present`) refuses anything that is not a Unix socket.
fn validated_socket_path(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    // This is a Unix socket setting even on hosts without Unix transport.
    // Host-native Path parsing would reject /run/... on Windows, or normalize
    // away the dot/repeated-separator components this contract must refuse.
    let relative = raw.strip_prefix('/')?;
    if raw.contains(['\0', '\\'])
        || relative
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return None;
    }
    Some(PathBuf::from(raw))
}

impl ComputerState {
    pub(crate) fn new(socket_path: PathBuf, idle_close: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                socket_path,
                idle_close,
                lease_ttl: LEASE_IDLE_TTL,
                lease: parking_lot::Mutex::new(None),
                client_tokens: parking_lot::Mutex::new(HashMap::new()),
                tickets: parking_lot::Mutex::new(HashMap::new()),
                events: parking_lot::Mutex::new(VecDeque::new()),
                next_seq: AtomicU64::new(1),
                next_connection: AtomicU64::new(1),
                attached: AtomicU64::new(0),
            }),
        }
    }

    pub(crate) fn from_env() -> Self {
        let socket = std::env::var(DISPLAY_SOCKET_ENV)
            .ok()
            .and_then(|raw| {
                let checked = validated_socket_path(&raw);
                if checked.is_none() && !raw.trim().is_empty() {
                    tracing::warn!(
                        target: "codewhale::computer",
                        "{DISPLAY_SOCKET_ENV} must be an absolute path with no `.`/`..` \
                         components; using {DEFAULT_DISPLAY_SOCKET}"
                    );
                }
                checked
            })
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DISPLAY_SOCKET));
        let idle = std::env::var(DISPLAY_IDLE_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|secs| *secs > 0)
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_IDLE);
        Self::new(socket, idle)
    }

    fn emit(&self, kind: &str, data: Value) {
        let mut events = self.inner.events.lock();
        let seq = self.inner.next_seq.fetch_add(1, Ordering::Relaxed);
        let event = ComputerEvent {
            seq,
            kind: kind.to_string(),
            at: Utc::now(),
            data,
        };
        tracing::info!(target: "codewhale::computer", seq, kind, "computer event");
        if events.len() >= EVENT_LOG_CAP {
            events.pop_front();
        }
        events.push_back(event);
    }

    pub(super) fn events_since(&self, since: u64) -> (Vec<ComputerEvent>, u64) {
        let events = self.inner.events.lock();
        let list: Vec<_> = events.iter().filter(|e| e.seq > since).cloned().collect();
        let next = self
            .inner
            .next_seq
            .load(Ordering::Relaxed)
            .saturating_sub(1);
        (list, next)
    }

    // -- tokens --------------------------------------------------------------

    /// Whether `bearer` is a live (unexpired, unrevoked) client token.
    pub(super) fn client_principal(&self, bearer: &str) -> Option<Principal> {
        let key = hash(bearer);
        let now = Utc::now();
        let tokens = self.inner.client_tokens.lock();
        let token = tokens.get(&key)?;
        (token.expires_at > now).then(|| Principal::Client {
            token_id: token.id.clone(),
            device_id: token.device_id.clone(),
            intent: token.intent,
        })
    }

    fn principal_is_live(&self, principal: &Principal) -> bool {
        match principal {
            Principal::Owner => true,
            Principal::Client { token_id, .. } => {
                let now = Utc::now();
                self.inner
                    .client_tokens
                    .lock()
                    .values()
                    .any(|t| &t.id == token_id && t.expires_at > now)
            }
        }
    }

    fn mint_client_token(
        &self,
        device_id: String,
        ttl_secs: u64,
        label: Option<String>,
        intent: ClientTokenIntent,
    ) -> Result<(String, ClientTokenView), ApiErr> {
        let now = Utc::now();
        let mut tokens = self.inner.client_tokens.lock();
        tokens.retain(|_, t| t.expires_at > now);
        if tokens.len() >= CLIENT_TOKEN_MAX_ACTIVE {
            return Err(ApiErr::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many active client tokens; revoke one first",
            ));
        }
        let secret = format!(
            "cwct_{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let id = format!("ct_{}", uuid::Uuid::new_v4().simple());
        let token = ClientToken {
            id,
            device_id,
            intent,
            label,
            created_at: now,
            expires_at: now + chrono::Duration::seconds(ttl_secs as i64),
        };
        let view = ClientTokenView::from(&token);
        tokens.insert(hash(&secret), token);
        Ok((secret, view))
    }

    fn revoke_client_token(&self, id: &str) -> bool {
        let mut tokens = self.inner.client_tokens.lock();
        let before = tokens.len();
        tokens.retain(|_, t| t.id != id);
        before != tokens.len()
    }

    fn list_client_tokens(&self) -> Vec<ClientTokenView> {
        let now = Utc::now();
        let mut tokens = self.inner.client_tokens.lock();
        tokens.retain(|_, t| t.expires_at > now);
        let mut list: Vec<_> = tokens.values().map(ClientTokenView::from).collect();
        list.sort_by_key(|a| a.created_at);
        list
    }

    // -- display tickets -----------------------------------------------------

    fn mint_ticket(&self, principal: Principal) -> Result<String, ApiErr> {
        let now = Instant::now();
        let mut tickets = self.inner.tickets.lock();
        tickets.retain(|_, t| t.expires > now);
        if tickets.len() >= DISPLAY_TICKET_MAX_ACTIVE {
            return Err(ApiErr::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many outstanding display tickets",
            ));
        }
        let secret = format!("cwdt_{}", uuid::Uuid::new_v4().simple());
        tickets.insert(
            hash(&secret),
            DisplayTicket {
                principal,
                expires: now + DISPLAY_TICKET_TTL,
            },
        );
        Ok(secret)
    }

    /// Single use: a ticket is removed on the first redemption attempt,
    /// whether or not it had expired.
    fn redeem_ticket(&self, ticket: &str) -> Option<Principal> {
        let entry = self.inner.tickets.lock().remove(&hash(ticket))?;
        (entry.expires > Instant::now() && self.principal_is_live(&entry.principal))
            .then_some(entry.principal)
    }

    // -- lease ---------------------------------------------------------------

    /// Expire a stale lease (emitting `computer.control.expired`) and return
    /// a snapshot of whatever lease remains.
    fn sweep_lease(&self) -> Option<LeaseView> {
        let mut guard = self.inner.lease.lock();
        let expired = guard.as_ref().is_some_and(|lease| {
            lease.last_activity.elapsed() >= self.inner.lease_ttl
                || !self.principal_is_live(&lease.principal)
        });
        if expired {
            let lease = guard.take().expect("checked above");
            drop(guard);
            self.emit(
                "computer.control.expired",
                lease_span_data(&lease, "expired"),
            );
            return None;
        }
        guard.as_ref().map(|lease| self.lease_view(lease))
    }

    fn lease_view(&self, lease: &Lease) -> LeaseView {
        let remaining = self
            .inner
            .lease_ttl
            .saturating_sub(lease.last_activity.elapsed());
        LeaseView {
            holder: lease.principal.holder(),
            device_id: lease.principal.device_id().map(str::to_string),
            acquired_at: lease.acquired_at,
            expires_at: Utc::now() + chrono::Duration::from_std(remaining).unwrap_or_default(),
            input_events: lease.input_events,
        }
    }

    fn acquire(&self, principal: &Principal, force: bool) -> Result<LeaseView, LeaseView> {
        self.sweep_lease();
        let mut guard = self.inner.lease.lock();
        if let Some(current) = guard.as_mut() {
            if current.principal == *principal {
                current.last_activity = Instant::now();
                return Ok(self.lease_view(current));
            }
            if !force {
                return Err(self.lease_view(current));
            }
            let previous = guard.take().expect("checked above");
            self.emit(
                "computer.control.released",
                lease_span_data(&previous, "taken_over"),
            );
        }
        let now = Instant::now();
        let lease = Lease {
            principal: principal.clone(),
            acquired_at: Utc::now(),
            acquired_instant: now,
            last_activity: now,
            input_events: 0,
        };
        let view = self.lease_view(&lease);
        *guard = Some(lease);
        drop(guard);
        self.emit(
            "computer.control.acquired",
            json!({ "holder": view.holder, "device_id": view.device_id }),
        );
        Ok(view)
    }

    fn release(&self, principal: &Principal) -> bool {
        let mut guard = self.inner.lease.lock();
        if guard
            .as_ref()
            .is_some_and(|lease| lease.principal == *principal)
        {
            let lease = guard.take().expect("checked above");
            drop(guard);
            self.emit(
                "computer.control.released",
                lease_span_data(&lease, "hand_back"),
            );
            true
        } else {
            false
        }
    }

    fn holds_lease(&self, principal: &Principal) -> bool {
        principal.can_drive()
            && self.principal_is_live(principal)
            && self.inner.lease.lock().as_ref().is_some_and(|lease| {
                lease.principal == *principal
                    && lease.last_activity.elapsed() < self.inner.lease_ttl
            })
    }

    fn note_input(&self, principal: &Principal, count: u64) {
        if let Some(lease) = self.inner.lease.lock().as_mut()
            && lease.principal == *principal
        {
            lease.last_activity = Instant::now();
            lease.input_events += count;
        }
    }
}

fn lease_span_data(lease: &Lease, reason: &str) -> Value {
    json!({
        "holder": lease.principal.holder(),
        "device_id": lease.principal.device_id(),
        "reason": reason,
        "held_ms": lease.acquired_instant.elapsed().as_millis() as u64,
        "input_events": lease.input_events,
    })
}

fn hash(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

#[derive(Debug, Clone, Serialize)]
struct LeaseView {
    holder: String,
    device_id: Option<String>,
    acquired_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    input_events: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ClientTokenView {
    id: String,
    device_id: String,
    intent: ClientTokenIntent,
    label: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl From<&ClientToken> for ClientTokenView {
    fn from(t: &ClientToken) -> Self {
        Self {
            id: t.id.clone(),
            device_id: t.device_id.clone(),
            intent: t.intent,
            label: t.label.clone(),
            created_at: t.created_at,
            expires_at: t.expires_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Redaction
// ---------------------------------------------------------------------------

/// Replace the value of every secret-bearing query parameter
/// (`ticket`, `mobile_stream_ticket`, `token`, `access_token`) with
/// `redacted`. Use on any URI before it reaches a log line or a proxy.
pub(crate) fn redact_query_secrets(uri: &str) -> String {
    let Some((path, query)) = uri.split_once('?') else {
        return uri.to_string();
    };
    let (query, fragment) = match query.split_once('#') {
        Some((q, f)) => (q, Some(f)),
        None => (query, None),
    };
    let redacted: Vec<String> = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _))
                if SECRET_QUERY_KEYS
                    .iter()
                    .any(|k| k.eq_ignore_ascii_case(key)) =>
            {
                format!("{key}=redacted")
            }
            _ => pair.to_string(),
        })
        .collect();
    let mut out = format!("{path}?{}", redacted.join("&"));
    if fragment.is_some() {
        // Fragments never reach a server, but a logged client URL could carry
        // one (the mobile bootstrap redirect does); drop it wholesale.
        out.push_str("#redacted");
    }
    out
}

// ---------------------------------------------------------------------------
// Client-to-server RFB parser
// ---------------------------------------------------------------------------

const MAX_ENCODINGS: usize = 64;
const MAX_CUT_TEXT: usize = 256 * 1024;
const MAX_SCREENS: usize = 16;

/// Encodings a client may ask Xvnc for. Anything else is stripped from
/// `SetEncodings` so the server never starts a sub-protocol (Fence, xvp,
/// QEMU keys, extended clipboard) whose client replies this parser would
/// refuse.
fn encoding_allowed(encoding: i32) -> bool {
    matches!(
        encoding,
        0 | 1 | 2 | 5 | 7 | 16 // Raw, CopyRect, RRE, Hextile, Tight, ZRLE
            | -223 // DesktopSize
            | -224 // LastRect
            | -239 // Cursor
            | -307 // DesktopName
            | -308 // ExtendedDesktopSize
            | -313 // ContinuousUpdates
            | -32..=-23 // JPEG quality
            | -256..=-247 // compression level
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParseError {
    UnknownType(u8),
    TooLarge { message_type: u8, len: usize },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::UnknownType(t) => write!(f, "unknown client message type {t}"),
            ParseError::TooLarge { message_type, len } => {
                write!(f, "client message type {message_type} too large ({len})")
            }
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeedStats {
    pub input_forwarded: u64,
    pub input_dropped: u64,
}

/// Length-tracked parser for RFB client messages after `ClientInit`.
/// Holds partial messages across WebSocket frames.
#[derive(Default)]
pub(crate) struct ClientParser {
    buf: Vec<u8>,
}

impl ClientParser {
    /// Feed client bytes. Complete, allowed messages are appended to `out`;
    /// input messages are appended only when `input_allowed`. Returns an
    /// error (and the stream must close) on any unknown or oversized message.
    pub(crate) fn feed(
        &mut self,
        data: &[u8],
        input_allowed: bool,
        out: &mut Vec<u8>,
    ) -> Result<FeedStats, ParseError> {
        self.buf.extend_from_slice(data);
        let mut stats = FeedStats::default();
        let mut offset = 0;
        loop {
            let rest = &self.buf[offset..];
            let Some(&message_type) = rest.first() else {
                break;
            };
            let need = match message_type {
                0 => Some(20),
                2 => (rest.len() >= 4)
                    .then(|| {
                        let n = u16::from_be_bytes([rest[2], rest[3]]) as usize;
                        (n, 4 + 4 * n)
                    })
                    .map(|(n, len)| if n > MAX_ENCODINGS { usize::MAX } else { len }),
                3 => Some(10),
                4 => Some(8),
                5 => Some(6),
                6 => (rest.len() >= 8).then(|| {
                    let n = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
                    if n > MAX_CUT_TEXT { usize::MAX } else { 8 + n }
                }),
                150 => Some(10),
                251 => (rest.len() >= 8).then(|| {
                    let n = rest[6] as usize;
                    if n > MAX_SCREENS {
                        usize::MAX
                    } else {
                        8 + 16 * n
                    }
                }),
                other => return Err(ParseError::UnknownType(other)),
            };
            let Some(need) = need else { break };
            if need == usize::MAX {
                return Err(ParseError::TooLarge {
                    message_type,
                    len: rest.len(),
                });
            }
            if rest.len() < need {
                break;
            }
            let message = &rest[..need];
            match message_type {
                2 => {
                    let kept: Vec<[u8; 4]> = message[4..]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .copied()
                        .filter(|c| encoding_allowed(i32::from_be_bytes(*c)))
                        .collect();
                    out.extend_from_slice(&[2, 0]);
                    out.extend_from_slice(&(kept.len() as u16).to_be_bytes());
                    for c in &kept {
                        out.extend_from_slice(c);
                    }
                }
                4 | 5 | 6 | 251 => {
                    if input_allowed {
                        out.extend_from_slice(message);
                        stats.input_forwarded += 1;
                    } else {
                        stats.input_dropped += 1;
                    }
                }
                _ => out.extend_from_slice(message),
            }
            offset += need;
        }
        self.buf.drain(..offset);
        Ok(stats)
    }
}

// ---------------------------------------------------------------------------
// Handshakes
// ---------------------------------------------------------------------------

#[cfg(unix)]
async fn read_reason<S: AsyncRead + Unpin>(s: &mut S) -> String {
    let Ok(len) = s.read_u32().await else {
        return String::new();
    };
    let mut reason = vec![0u8; (len as usize).min(1024)];
    let _ = s.read_exact(&mut reason).await;
    String::from_utf8_lossy(&reason).into_owned()
}

/// Complete the RFB 3.8 handshake with Xvnc as a client (security None) and
/// send a shared `ClientInit`, so each viewer gets its own connection without
/// disconnecting the others. After this returns, the next upstream bytes are
/// `ServerInit`.
#[cfg(unix)]
pub(crate) async fn upstream_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
) -> Result<(), String> {
    let mut version = [0u8; 12];
    s.read_exact(&mut version)
        .await
        .map_err(|e| format!("read server version: {e}"))?;
    if !version.starts_with(b"RFB 003.") {
        return Err("upstream is not an RFB server".to_string());
    }
    s.write_all(RFB_VERSION_38)
        .await
        .map_err(|e| format!("write version: {e}"))?;
    let count = s
        .read_u8()
        .await
        .map_err(|e| format!("read security: {e}"))?;
    if count == 0 {
        return Err(format!("upstream refused: {}", read_reason(s).await));
    }
    let mut types = vec![0u8; count as usize];
    s.read_exact(&mut types)
        .await
        .map_err(|e| format!("read security types: {e}"))?;
    if !types.contains(&1) {
        return Err("upstream does not offer security type None".to_string());
    }
    s.write_all(&[1])
        .await
        .map_err(|e| format!("write security: {e}"))?;
    let result = s
        .read_u32()
        .await
        .map_err(|e| format!("read security result: {e}"))?;
    if result != 0 {
        return Err(format!(
            "upstream security failed: {}",
            read_reason(s).await
        ));
    }
    s.write_all(&[1])
        .await
        .map_err(|e| format!("write ClientInit: {e}"))?;
    Ok(())
}

/// Buffered reader over the client half of the WebSocket.
struct WsIn<R> {
    stream: R,
    pending: Vec<u8>,
}

impl<R> WsIn<R>
where
    R: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
{
    /// Next chunk of client bytes. `Ok(None)` is a clean close; text frames
    /// are a protocol violation (RFB is binary).
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        if !self.pending.is_empty() {
            return Ok(Some(std::mem::take(&mut self.pending)));
        }
        loop {
            match self.stream.next().await {
                None | Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(Message::Binary(bytes))) => return Ok(Some(bytes.to_vec())),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                Some(Ok(Message::Text(_))) => return Err("text frame on RFB stream".to_string()),
                Some(Err(err)) => return Err(format!("websocket: {err}")),
            }
        }
    }

    async fn read_exact(&mut self, n: usize) -> Result<Vec<u8>, String> {
        let mut acc = Vec::with_capacity(n);
        while acc.len() < n {
            let Some(chunk) = self.next_chunk().await? else {
                return Err("client closed during handshake".to_string());
            };
            acc.extend_from_slice(&chunk);
        }
        self.pending = acc.split_off(n);
        Ok(acc)
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExitReason {
    ClientClosed,
    UpstreamClosed,
    ProtocolViolation(String),
    ParserPanic,
    IdleClosed,
    Revoked,
    Error(String),
}

impl ExitReason {
    fn label(&self) -> String {
        match self {
            ExitReason::ClientClosed => "client_closed".into(),
            ExitReason::UpstreamClosed => "upstream_closed".into(),
            ExitReason::ProtocolViolation(detail) => format!("protocol_violation: {detail}"),
            ExitReason::ParserPanic => "parser_panic".into(),
            ExitReason::IdleClosed => "idle_closed".into(),
            ExitReason::Revoked => "revoked".into(),
            ExitReason::Error(detail) => format!("error: {detail}"),
        }
    }

    fn close_code(&self) -> u16 {
        match self {
            ExitReason::ClientClosed | ExitReason::UpstreamClosed | ExitReason::IdleClosed => 1000,
            ExitReason::ProtocolViolation(_) => 1008,
            ExitReason::Revoked => 4401,
            ExitReason::ParserPanic | ExitReason::Error(_) => 1011,
        }
    }
}

struct SessionCounters {
    last_human_input: parking_lot::Mutex<Instant>,
    last_screen_bytes: parking_lot::Mutex<Instant>,
    input_forwarded: AtomicU64,
    input_dropped: AtomicU64,
}

/// Parser task body: client bytes → [`ClientParser`] → upstream writer.
async fn parser_loop<R, W>(
    mut ws_in: WsIn<R>,
    mut upstream: W,
    computer: ComputerState,
    principal: Principal,
    counters: Arc<SessionCounters>,
) -> ExitReason
where
    R: futures_util::Stream<Item = Result<Message, axum::Error>> + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut parser = ClientParser::default();
    let mut out = Vec::with_capacity(4096);
    loop {
        let chunk = match ws_in.next_chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => return ExitReason::ClientClosed,
            Err(detail) => return ExitReason::ProtocolViolation(detail),
        };
        out.clear();
        let allowed = computer.holds_lease(&principal);
        let stats = match parser.feed(&chunk, allowed, &mut out) {
            Ok(stats) => stats,
            Err(err) => return ExitReason::ProtocolViolation(err.to_string()),
        };
        if stats.input_forwarded > 0 {
            computer.note_input(&principal, stats.input_forwarded);
            *counters.last_human_input.lock() = Instant::now();
            counters
                .input_forwarded
                .fetch_add(stats.input_forwarded, Ordering::Relaxed);
        }
        if stats.input_dropped > 0 {
            counters
                .input_dropped
                .fetch_add(stats.input_dropped, Ordering::Relaxed);
        }
        if !out.is_empty() && upstream.write_all(&out).await.is_err() {
            return ExitReason::UpstreamClosed;
        }
    }
}

/// Map a parser task's join result to an exit reason. A panic inside the
/// parser is contained here: it ends this display connection only.
fn parser_exit(result: Result<ExitReason, tokio::task::JoinError>) -> ExitReason {
    match result {
        Ok(reason) => reason,
        Err(err) if err.is_panic() => ExitReason::ParserPanic,
        Err(err) => ExitReason::Error(err.to_string()),
    }
}

async fn run_session<U>(
    socket: WebSocket,
    upstream: U,
    computer: ComputerState,
    principal: Principal,
) where
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let connection_id = computer
        .inner
        .next_connection
        .fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let (mut ws_tx, ws_rx) = socket.split();
    let mut ws_in = WsIn {
        stream: ws_rx,
        pending: Vec::new(),
    };

    // Downstream handshake: offer RFB 3.8 with security None only.
    let handshake = async {
        ws_tx
            .send(Message::Binary(RFB_VERSION_38.to_vec().into()))
            .await
            .map_err(|e| e.to_string())?;
        let version = ws_in.read_exact(12).await?;
        if version != RFB_VERSION_38 {
            return Err("client must speak RFB 003.008".to_string());
        }
        ws_tx
            .send(Message::Binary(vec![1u8, 1].into()))
            .await
            .map_err(|e| e.to_string())?;
        let choice = ws_in.read_exact(1).await?;
        if choice != [1] {
            return Err("client chose an unsupported security type".to_string());
        }
        ws_tx
            .send(Message::Binary(vec![0u8, 0, 0, 0].into()))
            .await
            .map_err(|e| e.to_string())?;
        // ClientInit: its shared flag is ignored; upstream is always shared.
        ws_in.read_exact(1).await?;
        Ok::<(), String>(())
    };
    if let Err(detail) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .unwrap_or_else(|_| Err("handshake timed out".to_string()))
    {
        let _ = ws_tx
            .send(Message::Close(Some(CloseFrame {
                code: 1008,
                reason: "rfb handshake failed".into(),
            })))
            .await;
        tracing::info!(target: "codewhale::computer", %detail, "display handshake failed");
        return;
    }

    computer.inner.attached.fetch_add(1, Ordering::Relaxed);
    computer.emit(
        "computer.display.attached",
        json!({
            "connection_id": connection_id,
            "holder": principal.holder(),
            "device_id": principal.device_id(),
        }),
    );

    let counters = Arc::new(SessionCounters {
        last_human_input: parking_lot::Mutex::new(Instant::now()),
        last_screen_bytes: parking_lot::Mutex::new(Instant::now()),
        input_forwarded: AtomicU64::new(0),
        input_dropped: AtomicU64::new(0),
    });
    let (mut up_r, up_w) = tokio::io::split(upstream);

    // Parser in its own task (§3.3): a panic here must not reach a turn.
    let mut parser = tokio::spawn(parser_loop(
        ws_in,
        up_w,
        computer.clone(),
        principal.clone(),
        counters.clone(),
    ));

    let mut tick = tokio::time::interval(SUPERVISOR_TICK);
    tick.tick().await;
    let mut buf = vec![0u8; 64 * 1024];
    let reason = loop {
        tokio::select! {
            joined = &mut parser => break parser_exit(joined),
            read = up_r.read(&mut buf) => match read {
                Ok(0) | Err(_) => break ExitReason::UpstreamClosed,
                Ok(n) => {
                    *counters.last_screen_bytes.lock() = Instant::now();
                    if ws_tx.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                        break ExitReason::ClientClosed;
                    }
                }
            },
            _ = tick.tick() => {
                computer.sweep_lease();
                if !computer.principal_is_live(&principal) {
                    break ExitReason::Revoked;
                }
                let idle = computer.inner.idle_close;
                let human_idle = counters.last_human_input.lock().elapsed() >= idle;
                let screen_idle = counters.last_screen_bytes.lock().elapsed() >= idle;
                if human_idle && screen_idle {
                    break ExitReason::IdleClosed;
                }
            }
        }
    };
    parser.abort();

    let _ = ws_tx
        .send(Message::Close(Some(CloseFrame {
            code: reason.close_code(),
            reason: reason.label().into(),
        })))
        .await;
    computer.inner.attached.fetch_sub(1, Ordering::Relaxed);
    let span = json!({
        "connection_id": connection_id,
        "holder": principal.holder(),
        "reason": reason.label(),
        "attached_ms": started.elapsed().as_millis() as u64,
        "input_forwarded": counters.input_forwarded.load(Ordering::Relaxed),
        "input_dropped": counters.input_dropped.load(Ordering::Relaxed),
    });
    if reason == ExitReason::IdleClosed {
        computer.emit("computer.display.idle_closed", span.clone());
    }
    computer.emit("computer.display.detached", span);
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct RouteState {
    computer: ComputerState,
    runtime_token: Option<String>,
}

struct ApiErr {
    status: StatusCode,
    message: String,
    extra: Option<Value>,
}

impl ApiErr {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            extra: None,
        }
    }
    fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "runtime API bearer token required",
        )
    }
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        let mut body = json!({
            "error": { "message": self.message, "status": self.status.as_u16() }
        });
        if let Some(extra) = self.extra {
            body["error"]["detail"] = extra;
        }
        (self.status, Json(body)).into_response()
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .or_else(|| {
            headers
                .get("x-codewhale-runtime-token")
                .and_then(|v| v.to_str().ok())
        })
}

fn principal_from_headers(state: &RouteState, headers: &HeaderMap) -> Option<Principal> {
    let Some(expected) = state.runtime_token.as_deref() else {
        return Some(Principal::Owner);
    };
    let presented = bearer(headers)?;
    if constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
        return Some(Principal::Owner);
    }
    state.computer.client_principal(presented)
}

fn require_principal(state: &RouteState, headers: &HeaderMap) -> Result<Principal, ApiErr> {
    principal_from_headers(state, headers).ok_or_else(ApiErr::unauthorized)
}

fn require_driver(state: &RouteState, headers: &HeaderMap) -> Result<Principal, ApiErr> {
    let principal = require_principal(state, headers)?;
    if !principal.can_drive() {
        return Err(ApiErr::new(
            StatusCode::FORBIDDEN,
            "watch client tokens cannot control this Computer",
        ));
    }
    Ok(principal)
}

/// Routes for the computer surface, merged into the Runtime API router
/// outside the `/v1` auth layer (each handler authenticates itself).
pub(super) fn router<S>(computer: ComputerState, runtime_token: Option<String>) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/v1/computer", get(computer_status))
        .route("/v1/computer/events", get(computer_events))
        .route("/v1/computer/control/acquire", post(control_acquire))
        .route("/v1/computer/control/release", post(control_release))
        .route("/v1/computer/display/tickets", post(display_ticket))
        .route("/v1/computer/display", get(display_ws))
        .route(
            "/v1/auth/client-tokens",
            get(list_client_tokens).post(create_client_token),
        )
        .route("/v1/auth/client-tokens/{id}", delete(revoke_client_token))
        .with_state(RouteState {
            computer,
            runtime_token,
        })
}

async fn computer_status(State(state): State<RouteState>, headers: HeaderMap) -> Response {
    let principal = match require_principal(&state, &headers) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let lease = state.computer.sweep_lease();
    let you_hold = state.computer.holds_lease(&principal);
    let (_, seq) = state.computer.events_since(u64::MAX);
    Json(json!({
        "display": {
            "available": display_socket_present(&state.computer).await,
            "attached": state.computer.inner.attached.load(Ordering::Relaxed),
            "idle_close_seconds": state.computer.inner.idle_close.as_secs(),
        },
        "control": {
            "lease": lease,
            "you_hold_lease": you_hold,
            "human_driving": lease.is_some(),
            "lease_idle_ttl_seconds": state.computer.inner.lease_ttl.as_secs(),
        },
        "events_seq": seq,
    }))
    .into_response()
}

async fn display_socket_present(computer: &ComputerState) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        tokio::fs::metadata(&computer.inner.socket_path)
            .await
            .map(|m| m.file_type().is_socket())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = computer;
        false
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    since: Option<u64>,
}

async fn computer_events(
    State(state): State<RouteState>,
    headers: HeaderMap,
    Query(query): Query<EventsQuery>,
) -> Response {
    if let Err(e) = require_principal(&state, &headers) {
        return e.into_response();
    }
    state.computer.sweep_lease();
    let (events, next) = state.computer.events_since(query.since.unwrap_or(0));
    Json(json!({ "events": events, "next_since": next })).into_response()
}

#[derive(Deserialize, Default)]
struct AcquireBody {
    #[serde(default)]
    force: bool,
}

async fn control_acquire(
    State(state): State<RouteState>,
    headers: HeaderMap,
    body: Option<Json<AcquireBody>>,
) -> Response {
    let principal = match require_driver(&state, &headers) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let force = body.map(|Json(b)| b.force).unwrap_or(false);
    match state.computer.acquire(&principal, force) {
        Ok(lease) => Json(json!({ "lease": lease })).into_response(),
        Err(current) => ApiErr {
            status: StatusCode::CONFLICT,
            message: "another client holds the control lease".to_string(),
            extra: Some(json!({ "lease": current })),
        }
        .into_response(),
    }
}

async fn control_release(State(state): State<RouteState>, headers: HeaderMap) -> Response {
    let principal = match require_driver(&state, &headers) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    if state.computer.release(&principal) {
        Json(json!({ "released": true })).into_response()
    } else {
        ApiErr::new(
            StatusCode::CONFLICT,
            "this client does not hold the control lease",
        )
        .into_response()
    }
}

async fn display_ticket(State(state): State<RouteState>, headers: HeaderMap) -> Response {
    let principal = match require_principal(&state, &headers) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    match state.computer.mint_ticket(principal) {
        Ok(ticket) => (
            StatusCode::CREATED,
            Json(json!({
                "ticket": ticket,
                "expires_in_seconds": DISPLAY_TICKET_TTL.as_secs(),
            })),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct DisplayQuery {
    ticket: Option<String>,
}

async fn display_ws(
    State(state): State<RouteState>,
    headers: HeaderMap,
    uri: Uri,
    Query(query): Query<DisplayQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let principal = match principal_from_headers(&state, &headers) {
        Some(p) => p,
        None => match query
            .ticket
            .as_deref()
            .and_then(|ticket| state.computer.redeem_ticket(ticket))
        {
            Some(p) => p,
            None => return ApiErr::unauthorized().into_response(),
        },
    };
    tracing::info!(
        target: "codewhale::computer",
        uri = %redact_query_secrets(&uri.to_string()),
        holder = %principal.holder(),
        "computer display attach"
    );
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    let upstream = match connect_upstream(&state.computer).await {
        Ok(upstream) => upstream,
        Err(detail) => {
            tracing::warn!(target: "codewhale::computer", %detail, "computer display unavailable");
            return ApiErr::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "computer display unavailable",
            )
            .into_response();
        }
    };
    let computer = state.computer.clone();
    ws.on_upgrade(move |socket| run_session(socket, upstream, computer, principal))
}

#[cfg(unix)]
async fn connect_upstream(computer: &ComputerState) -> Result<tokio::net::UnixStream, String> {
    let connect = async {
        let mut stream = tokio::net::UnixStream::connect(&computer.inner.socket_path)
            .await
            .map_err(|e| format!("connect display socket: {e}"))?;
        upstream_handshake(&mut stream).await?;
        Ok::<_, String>(stream)
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, connect)
        .await
        .unwrap_or_else(|_| Err("display handshake timed out".to_string()))
}

#[cfg(not(unix))]
async fn connect_upstream(_computer: &ComputerState) -> Result<tokio::io::DuplexStream, String> {
    Err("the computer display is Unix-only".to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateClientTokenBody {
    device_id: String,
    ttl_seconds: Option<u64>,
    label: Option<String>,
    #[serde(default)]
    intent: ClientTokenIntent,
}

fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= DEVICE_ID_MAX_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// Minting is master-token only: a client token can never mint another.
fn require_owner(state: &RouteState, headers: &HeaderMap) -> Result<(), ApiErr> {
    let Some(expected) = state.runtime_token.as_deref() else {
        return Err(ApiErr::new(
            StatusCode::CONFLICT,
            "client tokens need Runtime API auth; this Engine runs without a token",
        ));
    };
    match bearer(headers) {
        Some(presented) if constant_time_eq(presented.as_bytes(), expected.as_bytes()) => Ok(()),
        Some(presented) if state.computer.client_principal(presented).is_some() => {
            Err(ApiErr::new(
                StatusCode::FORBIDDEN,
                "client tokens cannot manage client tokens",
            ))
        }
        _ => Err(ApiErr::unauthorized()),
    }
}

async fn create_client_token(
    State(state): State<RouteState>,
    headers: HeaderMap,
    Json(body): Json<CreateClientTokenBody>,
) -> Response {
    if let Err(e) = require_owner(&state, &headers) {
        return e.into_response();
    }
    let device_id = body.device_id.trim().to_string();
    if !valid_device_id(&device_id) {
        return ApiErr::new(
            StatusCode::BAD_REQUEST,
            "device_id must be 1-128 of [A-Za-z0-9._:-]",
        )
        .into_response();
    }
    let ttl = body
        .ttl_seconds
        .unwrap_or(CLIENT_TOKEN_MAX_TTL_SECS)
        .clamp(CLIENT_TOKEN_MIN_TTL_SECS, CLIENT_TOKEN_MAX_TTL_SECS);
    let label = body
        .label
        .map(|l| l.chars().take(128).collect::<String>())
        .filter(|l| !l.trim().is_empty());
    match state
        .computer
        .mint_client_token(device_id, ttl, label, body.intent)
    {
        Ok((token, view)) => (
            StatusCode::CREATED,
            Json(json!({
                "token": token,
                "id": view.id,
                "device_id": view.device_id,
                "intent": view.intent,
                "label": view.label,
                "created_at": view.created_at,
                "expires_at": view.expires_at,
            })),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

async fn list_client_tokens(State(state): State<RouteState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_owner(&state, &headers) {
        return e.into_response();
    }
    Json(json!({ "tokens": state.computer.list_client_tokens() })).into_response()
}

async fn revoke_client_token(
    State(state): State<RouteState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(e) = require_owner(&state, &headers) {
        return e.into_response();
    }
    if state.computer.revoke_client_token(&id) {
        state.computer.sweep_lease();
        StatusCode::NO_CONTENT.into_response()
    } else {
        ApiErr::new(StatusCode::NOT_FOUND, "no such client token").into_response()
    }
}

#[cfg(test)]
#[path = "computer_display_tests.rs"]
mod tests;

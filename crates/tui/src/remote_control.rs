//! Account-owned remote control for the active TUI session.
//!
//! This is deliberately a typed relay, not a remote shell. The control plane
//! may send prompts, approval decisions, and run-control requests for the exact
//! enrolled target. Provider credentials, paths, environment variables, and
//! arbitrary command strings never cross this boundary.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use reqwest::Url;
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use codewhale_models::{ContentBlock, Message};

use crate::{
    core::events::{Event as EngineEvent, TurnOutcomeStatus},
    git_status::normalize_observed_git_repo,
    runtime_chat_relay::{
        RuntimeChatControlScope, RuntimeChatProjection, RuntimeChatPrompt, RuntimeChatRelayHost,
    },
};

const PRODUCTION_CONTROL_PLANE: &str = "https://api.codewhale.net/";
const ENROLLMENT_SECRET_SLOT: &str = "cwc-remote-control-enrollment-v1";
/// Machine-stable device identity. It outlives individual enrollments so the
/// control plane can fold every folder enrolled from this terminal into one
/// computer instead of one row per `/rc`.
const DEVICE_IDENTITY_SECRET_SLOT: &str = "cwc-remote-control-device-v1";
/// The only web origin whose session links the terminal will surface or open.
const APP_ORIGIN_HOST: &str = "app.codewhale.net";
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(25);
const SYNC_INTERVAL: Duration = Duration::from_millis(1_200);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_RUNS: usize = 64;
const MAX_COMMANDS: usize = 128;
const JS_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const MAX_RUNTIME_ENVELOPE_BYTES: usize = 128 * 1024;
const SNAPSHOT_ENVELOPE_BYTE_BUDGET: usize = 120 * 1024;
const MAX_SNAPSHOT_MESSAGES: usize = 64;
const MAX_SNAPSHOT_MESSAGE_CHARS: usize = 128 * 1024;
const MIN_TRUNCATED_MESSAGE_CHARS: usize = 32;
const MAX_REMOTE_ERROR_MESSAGE_BYTES: usize = 4 * 1024;
const RUNTIME_UPLOAD_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const RUNTIME_UPLOAD_MAX_BACKOFF: Duration = Duration::from_secs(5);
const RUNTIME_CHAT_RELAY_PROTOCOL: &str = "codewhale.runtime-chat-relay.v1";
const RUNTIME_CHAT_CATALOG_TIMESTAMP: &str = "1970-01-01T00:00:00Z";
const CAPABILITIES: &[&str] = &["evidence-ledger", "fim", "git", "shell"];
/// How long an aborted or failed relay keeps local input locked. Matches the
/// server-side runner lease expiry with margin; local input never returns
/// while the server could still consider a remote owner live.
const OWNERSHIP_LOCK_AFTER_FAILURE: Duration = Duration::from_secs(95);
/// Ceiling for draining unacknowledged runtime events during `/rc stop`.
/// Deliberately below `OWNERSHIP_LOCK_AFTER_FAILURE` so a failed drain still
/// resolves into the ownership-locked path before the lease question is moot.
const STOP_DRAIN_DEADLINE: Duration = Duration::from_secs(45);
const JOURNAL_SCHEMA_VERSION: u64 = 2;
const CLASSIC_SESSION_STATE_SCHEMA_VERSION: u64 = 3;
/// Hard bounds for the crash-recoverable unacknowledged-envelope journal.
const MAX_JOURNAL_EVENTS: usize = 256;
const MAX_JOURNAL_ENCODED_BYTES: usize = 4 * 1024 * 1024;
/// Capacity held back exclusively for integrity-critical envelopes (terminal
/// turn state, approvals, failures, resynchronization snapshots). Ordinary
/// deltas may never consume this headroom.
const JOURNAL_RESERVED_INTEGRITY_EVENTS: usize = 64;
const JOURNAL_RESERVED_INTEGRITY_BYTES: usize = 1024 * 1024;
/// A deferred (not yet handed to transport) delta envelope may grow to this
/// encoded size through coalescing before it is forced onto the wire.
const DELTA_COALESCE_BYTE_CAP: usize = 32 * 1024;
const JOURNAL_SETUP_ERROR: &str = "Remote control could not prepare its private delivery journal.";
const JOURNAL_UNTRUSTED_ERROR: &str = "The saved remote-control delivery journal could not be trusted; it was set aside. The account run may show an incomplete turn.";
const JOURNAL_LEGACY_SCOPE_ERROR: &str = "A saved legacy remote-control delivery journal is not bound to this workspace and cannot be replayed safely; it was preserved for explicit recovery.";
const CLASSIC_LEASE_SCOPE_ERROR: &str = "This saved session still owns an unfinished account turn in its original workspace; reconnect that workspace or create a new local session.";

#[cfg(test)]
static TEST_JOURNAL_PERSIST_FAILURES: std::sync::Mutex<Vec<(PathBuf, usize)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn inject_journal_persist_failures(path: &Path, count: usize) {
    assert!(count > 0);
    TEST_JOURNAL_PERSIST_FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((path.to_path_buf(), count));
}

#[cfg(test)]
fn take_journal_persist_failure(path: &Path) -> bool {
    let mut failures = TEST_JOURNAL_PERSIST_FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(index) = failures.iter().position(|(target, _)| target == path) else {
        return false;
    };
    if failures[index].1 > 1 {
        failures[index].1 -= 1;
    } else {
        failures.remove(index);
    }
    true
}

/// Envelopes whose loss would strand account-side truth: terminal turn state,
/// approval requests, failure records, and resynchronization snapshots. They
/// draw on reserved journal capacity, are never dropped silently, and gate
/// `/rc stop` until the server cursor covers them.
fn integrity_critical_event(event: &str) -> bool {
    matches!(
        event,
        "turn.completed"
            | "approval.required"
            | "approval.resolved"
            | "item.failed"
            | "session.snapshot"
            | "runtime.catalog"
    )
}

fn runtime_envelope_event(envelope: &Value) -> Option<&str> {
    envelope.get("event").and_then(Value::as_str)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteControlAction {
    Start,
    Stop,
}

#[derive(Clone)]
pub struct RemoteStart {
    pub workspace_label: String,
    pub target_ref: String,
    pub session_id: String,
    pub runtime_version: String,
    pub runtime_commit: String,
    /// Directory that holds the crash-recoverable delivery journal. `None`
    /// runs memory-only and is reserved for tests; production callers must
    /// always provide a private directory under the Codewhale home.
    pub journal_dir: Option<PathBuf>,
    /// Observed `owner/name` from `git remote get-url origin`, when the folder
    /// is a Git checkout. This is a display receipt, never a path or GitHub App
    /// grant.
    pub git_remote: Option<String>,
}

#[derive(Debug, Clone)]
pub enum RemoteEvent {
    Notice(String),
    Connected {
        account_ref: String,
        runner_id: String,
        target_ref: String,
        attachment: RemoteAttachment,
        links: RemoteLinks,
    },
    Attachment {
        account_ref: String,
        target_ref: String,
        attachment: RemoteAttachment,
        links: RemoteLinks,
    },
    RuntimeCursor {
        run_id: String,
        cursor: u64,
    },
    Command {
        run_id: String,
        seq: u64,
        command: RemoteCommand,
    },
    RuntimeChatProjection(RuntimeChatProjection),
    /// Internal handoff receipt: the connected worker has dropped its clone
    /// of the old immutable Runtime Chat host, so the controller can safely
    /// reopen the same durable scope with a freshly supplied provider config.
    RuntimeChatHostReleased,
    Failed(String),
    /// The relay died before any server-confirmed lease existed — during
    /// enrollment, device authorization, or the first connect. No lease can
    /// still be live, so nothing is locked and `/rc` can retry immediately.
    FailedPreLease(String),
    Stopped,
    OwnershipRestored {
        approvals: Vec<PendingRemoteApproval>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteAttachment {
    pub run_id: String,
    pub workspace_id: String,
    pub runtime_cursor: u64,
    pub snapshot_present: bool,
    pub runtime_chat_relay_protocol: String,
    pub runtime_chat_relay_challenge: String,
}

/// Web links the control plane advertises for the attached session. Both are
/// optional: an older control plane omits them and the terminal then simply
/// shows no link. Links are validated against the Codewhale app origin before
/// they are ever displayed or opened; the terminal never invents one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemoteLinks {
    /// `https://app.codewhale.net/session?run=<runId>` for the live run.
    pub run_url: Option<String>,
    /// `https://app.codewhale.net/settings?section=workspaces` for this computer.
    pub computer_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunnerConnection {
    runner_id: String,
    attachment: RemoteAttachment,
    links: RemoteLinks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteCommand {
    Prompt {
        turn_id: String,
        prompt: String,
    },
    RuntimeChatPrompt(Box<RuntimeChatPrompt>),
    Approval {
        gate: String,
        approved: bool,
    },
    Control {
        action: RemoteControlRequest,
        turn_id: Option<String>,
        runtime_chat: Option<RuntimeChatControlScope>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteControlRequest {
    Interrupt,
    Cancel,
}

enum RelayPhase {
    /// Everything before the first server-confirmed lease: control-plane base
    /// resolution, enrollment, device authorization, and the first connect.
    Enrolling,
    /// The server confirmed a lease (Connected was emitted). Any failure now
    /// is a lost-after-lease disconnect and stays fail-closed.
    Leased,
}

impl RelayPhase {
    fn lease_confirmed(&self) -> bool {
        matches!(self, Self::Leased)
    }
}

#[derive(Clone)]
enum WorkerCommand {
    Upload {
        run_id: String,
        acknowledgements: Vec<CommandAcknowledgement>,
        envelopes: Vec<Value>,
    },
    ReleaseRuntimeChatHost,
    InstallRuntimeChatHost(RuntimeChatRelayHost),
    Stop,
}

struct PendingRuntimeChatConfiguration {
    config: crate::config::Config,
    plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
    private_root: PathBuf,
    target_ref: String,
    session_id: String,
}

#[derive(Debug, Default)]
struct RuntimeTransportOutbox {
    events: BTreeMap<(String, u64), Value>,
}

/// One unacknowledged runtime envelope owned by the controller.
///
/// `handed_off` records whether the envelope may already have reached the
/// server through the transport worker. Once true the envelope is immutable:
/// ambiguous retries must resend byte-identical JSON.
#[derive(Debug, Clone, PartialEq)]
struct PendingRuntimeEnvelope {
    envelope: Value,
    encoded_len: usize,
    integrity: bool,
    handed_off: bool,
}

/// Crash-recoverable journal of unacknowledged runtime envelopes.
///
/// The file name is hash-derived so nothing about the workspace or session
/// leaks through the path; the directory is private and the file owner-only.
/// Neither the path nor the contents are ever reported to the control plane
/// or written to logs. Acknowledged prefixes are compacted on every persist,
/// and a journal that cannot be verified fails closed at load time.
struct RuntimeEventJournal {
    path: PathBuf,
    scope_tag: String,
    legacy_path: PathBuf,
    legacy_unscoped_path: PathBuf,
    active_index_path: PathBuf,
    classic_lease: parking_lot::Mutex<Option<ClassicRunLease>>,
    _classic_session_lock: ClassicSessionOwnerLock,
}

#[derive(Debug)]
struct ClassicSessionOwnerLock {
    _file: File,
}

impl ClassicSessionOwnerLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(path)
            .map_err(|_| JOURNAL_SETUP_ERROR.to_string())?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            // SAFETY: `file` is open and live.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(CLASSIC_LEASE_SCOPE_ERROR.to_string());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::Storage::FileSystem::LockFile;
            // SAFETY: `file` is open and live.
            if unsafe { LockFile(file.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX) } == 0 {
                return Err(CLASSIC_LEASE_SCOPE_ERROR.to_string());
            }
        }
        Ok(Self { _file: file })
    }
}

impl Drop for ClassicSessionOwnerLock {
    fn drop(&mut self) {
        // close() alone does not release the lock while a descriptor
        // duplicated into a child spawned between fork and exec still shares
        // this open file description; unlocking first lets a same-process
        // reopen proceed (#5735, #6698), as the relay and runtime store locks do.
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            // SAFETY: Drop runs only while `_file` still owns this descriptor.
            unsafe {
                libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::Storage::FileSystem::UnlockFile;
            // SAFETY: Drop runs only while `_file` still owns this handle.
            unsafe {
                UnlockFile(self._file.as_raw_handle() as _, 0, 0, u32::MAX, u32::MAX);
            }
        }
    }
}

fn runtime_journal_scope_tag(target_ref: &str, session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"cwc-remote-control-journal.v2\0");
    hasher.update(target_ref.as_bytes());
    hasher.update(b"\0");
    hasher.update(session_id.as_bytes());
    bytes_to_hex(&hasher.finalize())[..32].to_string()
}

fn runtime_journal_session_tag(session_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"cwc-remote-control-journal\0");
    hasher.update(session_id.as_bytes());
    bytes_to_hex(&hasher.finalize())[..32].to_string()
}

fn classic_recovery_turn_id(run_id: &str, lease_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale.classic-recovery-turn.v1\0");
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(lease_id.as_bytes());
    format!("turn_recovered_{}", &bytes_to_hex(&hasher.finalize())[..24])
}

impl RuntimeEventJournal {
    fn open(dir: &Path, target_ref: &str, session_id: &str) -> Result<Self, String> {
        let scope_tag = runtime_journal_scope_tag(target_ref, session_id);
        let legacy_session_tag = runtime_journal_session_tag(session_id);
        std::fs::create_dir_all(dir).map_err(|_| JOURNAL_SETUP_ERROR.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| JOURNAL_SETUP_ERROR.to_string())?;
        }
        let legacy_path = dir.join(format!("journal_{legacy_session_tag}.json"));
        let active_index_path = dir.join(format!("active_classic_{legacy_session_tag}.json"));
        let classic_session_lock =
            ClassicSessionOwnerLock::acquire(&active_index_path.with_extension("lock"))?;
        if !active_index_path.exists()
            && (legacy_path.exists() || legacy_path.with_extension("unscoped").exists())
        {
            return Err(JOURNAL_LEGACY_SCOPE_ERROR.to_string());
        }
        let journal = Self {
            path: dir.join(format!("journal_{scope_tag}.json")),
            scope_tag,
            legacy_unscoped_path: legacy_path.with_extension("unscoped"),
            legacy_path,
            active_index_path,
            classic_lease: parking_lot::Mutex::new(None),
            _classic_session_lock: classic_session_lock,
        };
        if journal.active_index_path.exists() {
            let lease = journal.read_active_index()?;
            *journal.classic_lease.lock() = lease;
        } else {
            journal.write_active_index(None)?;
        }
        Ok(journal)
    }

    /// Loads every journaled envelope, or fails closed when the journal
    /// cannot be trusted (corrupt, oversized, or written for another
    /// session). A missing file is an ordinary empty journal.
    fn load(&self) -> Result<HashMap<String, BTreeMap<u64, Value>>, String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.legacy_path.exists() || self.legacy_unscoped_path.exists() {
                    // Schema 1 authenticated only a session id. The same
                    // saved session can be opened from another target, so its
                    // envelopes cannot be relabelled into this v2
                    // target+session scope without server-side ownership
                    // proof. Preserve it and fail closed.
                    return Err(JOURNAL_LEGACY_SCOPE_ERROR.to_string());
                }
                return Ok(HashMap::new());
            }
            Err(_) => return Err(JOURNAL_UNTRUSTED_ERROR.to_string()),
        };
        if bytes.len() > MAX_JOURNAL_ENCODED_BYTES.saturating_mul(2) {
            return Err(JOURNAL_UNTRUSTED_ERROR.to_string());
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| JOURNAL_UNTRUSTED_ERROR.to_string())?;
        let identity_valid = value.get("schemaVersion").and_then(Value::as_u64)
            == Some(JOURNAL_SCHEMA_VERSION)
            && value.get("scope").and_then(Value::as_str) == Some(self.scope_tag.as_str());
        if !identity_valid {
            return Err(JOURNAL_UNTRUSTED_ERROR.to_string());
        }
        let runs = value
            .get("runs")
            .and_then(Value::as_object)
            .ok_or_else(|| JOURNAL_UNTRUSTED_ERROR.to_string())?;
        let mut restored: HashMap<String, BTreeMap<u64, Value>> = HashMap::new();
        let mut total_events = 0usize;
        let mut total_bytes = 0usize;
        for (run_id, envelopes) in runs {
            if !valid_opaque_ref(run_id) {
                return Err(JOURNAL_UNTRUSTED_ERROR.to_string());
            }
            let envelopes = envelopes
                .as_array()
                .ok_or_else(|| JOURNAL_UNTRUSTED_ERROR.to_string())?;
            let mut events = BTreeMap::new();
            for envelope in envelopes {
                let seq = runtime_envelope_seq(envelope)
                    .ok_or_else(|| JOURNAL_UNTRUSTED_ERROR.to_string())?;
                let encoded_len = serde_json::to_vec(envelope)
                    .map(|body| body.len())
                    .unwrap_or(usize::MAX);
                if encoded_len > MAX_RUNTIME_ENVELOPE_BYTES {
                    return Err(JOURNAL_UNTRUSTED_ERROR.to_string());
                }
                total_events += 1;
                total_bytes = total_bytes.saturating_add(encoded_len);
                if total_events > MAX_JOURNAL_EVENTS || total_bytes > MAX_JOURNAL_ENCODED_BYTES {
                    return Err(JOURNAL_UNTRUSTED_ERROR.to_string());
                }
                if events.insert(seq, envelope.clone()).is_some() {
                    return Err(JOURNAL_UNTRUSTED_ERROR.to_string());
                }
            }
            if !events.is_empty() {
                restored.insert(run_id.clone(), events);
            }
        }
        Ok(restored)
    }

    /// Atomically replaces the journal with the current unacknowledged set.
    /// An empty set removes the file entirely (prompt compaction).
    fn persist(
        &self,
        pending: &HashMap<String, BTreeMap<u64, PendingRuntimeEnvelope>>,
    ) -> Result<(), String> {
        #[cfg(test)]
        if take_journal_persist_failure(&self.path) {
            return Err(JOURNAL_SETUP_ERROR.to_string());
        }
        let classic_lease = self.classic_lease.lock().clone();
        if pending.values().all(BTreeMap::is_empty) && classic_lease.is_none() {
            self.remove();
            return Ok(());
        }
        let mut runs = serde_json::Map::new();
        for (run_id, events) in pending {
            if events.is_empty() {
                continue;
            }
            runs.insert(
                run_id.clone(),
                Value::Array(
                    events
                        .values()
                        .map(|entry| entry.envelope.clone())
                        .collect(),
                ),
            );
        }
        let body = serde_json::to_vec(&json!({
            "schemaVersion": JOURNAL_SCHEMA_VERSION,
            "scope": self.scope_tag,
            "runs": runs,
        }))
        .map_err(|_| JOURNAL_SETUP_ERROR.to_string())?;
        crate::utils::write_atomic(&self.path, &body).map_err(|_| JOURNAL_SETUP_ERROR.to_string())
    }

    fn remove(&self) {
        if self.classic_lease.lock().is_some() {
            return;
        }
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("tmp"));
    }

    /// Moves an untrusted journal aside so the failure is explicit and a
    /// deliberate later `/rc` start can proceed from a clean slate.
    fn quarantine_current(&self) {
        let _ = std::fs::rename(&self.path, self.path.with_extension("corrupt"));
    }

    fn quarantine_legacy(&self) {
        if self.legacy_path.exists() {
            let _ = std::fs::rename(&self.legacy_path, &self.legacy_unscoped_path);
        }
    }

    fn classic_lease(&self) -> Option<ClassicRunLease> {
        self.classic_lease.lock().clone()
    }

    fn read_active_index(&self) -> Result<Option<ClassicRunLease>, String> {
        let bytes = std::fs::read(&self.active_index_path)
            .map_err(|_| JOURNAL_UNTRUSTED_ERROR.to_string())?;
        if bytes.len() > 2048 {
            return Err(JOURNAL_UNTRUSTED_ERROR.to_string());
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| JOURNAL_UNTRUSTED_ERROR.to_string())?;
        if value.get("schemaVersion").and_then(Value::as_u64)
            != Some(CLASSIC_SESSION_STATE_SCHEMA_VERSION)
            || value.get("scope").and_then(Value::as_str) != Some(self.scope_tag.as_str())
        {
            return Err(CLASSIC_LEASE_SCOPE_ERROR.to_string());
        }
        match value.get("lease") {
            None | Some(Value::Null) => Ok(None),
            Some(value) => {
                let lease: ClassicRunLease = serde_json::from_value(value.clone())
                    .map_err(|_| JOURNAL_UNTRUSTED_ERROR.to_string())?;
                if !lease.valid() {
                    return Err(JOURNAL_UNTRUSTED_ERROR.to_string());
                }
                Ok(Some(lease))
            }
        }
    }

    fn write_active_index(&self, lease: Option<&ClassicRunLease>) -> Result<(), String> {
        #[cfg(test)]
        if take_journal_persist_failure(&self.active_index_path) {
            return Err(JOURNAL_SETUP_ERROR.to_string());
        }
        let index = serde_json::to_vec(&json!({
            "schemaVersion": CLASSIC_SESSION_STATE_SCHEMA_VERSION,
            "scope": self.scope_tag,
            "lease": lease,
        }))
        .map_err(|_| JOURNAL_SETUP_ERROR.to_string())?;
        crate::utils::write_atomic(&self.active_index_path, &index)
            .map_err(|_| JOURNAL_SETUP_ERROR.to_string())
    }

    fn set_classic_lease(
        &self,
        lease: Option<ClassicRunLease>,
        pending: &HashMap<String, BTreeMap<u64, PendingRuntimeEnvelope>>,
    ) -> Result<(), String> {
        let previous = self.classic_lease.lock().clone();
        let indexed = self.read_active_index()?;
        if indexed != previous {
            return Err(CLASSIC_LEASE_SCOPE_ERROR.to_string());
        }
        if let Some(lease) = &lease {
            self.write_active_index(Some(lease))?;
        }
        *self.classic_lease.lock() = lease.clone();
        if let Err(error) = self.persist(pending) {
            *self.classic_lease.lock() = previous.clone();
            let _ = self.write_active_index(previous.as_ref());
            return Err(error);
        }
        if lease.is_none()
            && let Err(error) = self.write_active_index(None)
        {
            *self.classic_lease.lock() = previous.clone();
            let _ = self.write_active_index(previous.as_ref());
            return Err(error);
        }
        Ok(())
    }

    /// Advances the canonical sequence floor for the active classic Work run.
    ///
    /// The target journal can compact an acknowledged prefix, so the
    /// session-wide lease must retain the highest server/local sequence while
    /// a provider turn is still active. Recovery uses this floor before it
    /// synthesizes a terminal event and can therefore never reuse a sequence
    /// the server already acknowledged.
    fn advance_classic_seq_floor(&self, run_id: &str, seq: u64) -> Result<(), String> {
        let Some(mut lease) = self.classic_lease.lock().clone() else {
            return Ok(());
        };
        if lease.run_id != run_id || seq <= lease.seq_floor {
            return Ok(());
        }
        let indexed = self.read_active_index()?;
        if indexed.as_ref() != self.classic_lease.lock().as_ref() {
            return Err(CLASSIC_LEASE_SCOPE_ERROR.to_string());
        }
        lease.seq_floor = seq;
        self.write_active_index(Some(&lease))?;
        *self.classic_lease.lock() = Some(lease);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimePostOutcome {
    Accepted(u64),
    Retryable,
    AccessTokenExpired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeFlushOutcome {
    Idle,
    Accepted { run_id: String, cursor: u64 },
    Retryable,
    AccessTokenExpired,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandAcknowledgement {
    command_seq: u64,
    command_type: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedEnrollment {
    schema_version: u64,
    control_plane_base: String,
    runner_enrollment_id: String,
    account_ref: String,
    device_id: String,
    target_ref: String,
    target_grant_ref: String,
    runtime_version: String,
    runtime_commit: String,
    bootstrap_secret: String,
}

/// The machine-stable device identity persisted independently of any
/// enrollment. Enrollments are deleted and re-created when the target or
/// runtime changes; this record is only ever created once per keychain.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedDeviceIdentity {
    schema_version: u64,
    device_id: String,
}

impl PersistedDeviceIdentity {
    fn valid(&self) -> bool {
        self.schema_version == 1 && valid_opaque_ref(&self.device_id)
    }
}

/// Pick the device id this terminal presents to the control plane. A valid
/// saved identity always wins; otherwise the device id of an existing
/// enrollment is adopted (so upgrading terminals keep their computer row);
/// otherwise a fresh id is minted. Returns the id and whether it must be
/// saved.
fn resolve_device_identity(
    saved: Option<PersistedDeviceIdentity>,
    enrollment_device_id: Option<&str>,
) -> (String, bool) {
    if let Some(saved) = saved.filter(PersistedDeviceIdentity::valid) {
        return (saved.device_id, false);
    }
    if let Some(existing) = enrollment_device_id.filter(|value| valid_opaque_ref(value)) {
        return (existing.to_string(), true);
    }
    (format!("device_{}", uuid::Uuid::new_v4().simple()), true)
}

#[derive(Clone)]
struct LiveEnrollment {
    persisted: PersistedEnrollment,
    access_token: String,
}

#[derive(Debug, Clone)]
struct ActiveRelayRun {
    run_id: String,
    turn_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClassicRunLease {
    run_id: String,
    turn_id: Option<String>,
    lease_id: String,
    seq_floor: u64,
}

impl ClassicRunLease {
    fn valid(&self) -> bool {
        valid_opaque_ref(&self.run_id)
            && self.turn_id.as_deref().is_none_or(valid_opaque_ref)
            && valid_opaque_ref(&self.lease_id)
            && self.seq_floor <= JS_MAX_SAFE_INTEGER
    }
}

#[derive(Debug, Clone)]
pub struct PendingRemoteApproval {
    pub tool_id: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    #[default]
    Off,
    Connecting,
    Connected,
    Stopping,
    Failed,
}

/// UI-thread owner for remote-control state and typed transport channels.
pub struct RemoteControlController {
    status: Status,
    status_detail: String,
    account_ref: Option<String>,
    target_ref: Option<String>,
    links: RemoteLinks,
    /// Latest server-confirmed run attachment. Kept separately from
    /// `active_run`: an attachment can exist while the session is idle, and a
    /// mid-turn `/rc` can bind the already-running local turn to it without
    /// inventing a second prompt.
    attached_run_id: Option<String>,
    attached_workspace_id: Option<String>,
    active_run: Option<ActiveRelayRun>,
    /// A local dispatch that was already in flight when the web attachment
    /// became ready, but whose typed `TurnStarted` event has not landed yet.
    /// The first such event promotes this exact run into `active_run`.
    pending_local_turn_run: Option<String>,
    event_seq: HashMap<String, u64>,
    uploaded_snapshots: HashSet<String>,
    pending_runtime_events: HashMap<String, BTreeMap<u64, PendingRuntimeEnvelope>>,
    pending_approvals: HashMap<String, PendingRemoteApproval>,
    command_fingerprints: HashMap<(String, u64), String>,
    worker_tx: Option<mpsc::UnboundedSender<WorkerCommand>>,
    event_rx: Option<mpsc::UnboundedReceiver<RemoteEvent>>,
    worker: Option<tokio::task::JoinHandle<()>>,
    applying_remote_command: bool,
    ownership_blocked_until: Option<Instant>,
    journal: Option<RuntimeEventJournal>,
    /// At most one deferred (unsent, still coalescible) delta seq per run.
    deferred_delta: HashMap<String, u64>,
    /// Runs whose deltas were shed under pressure; truth is restored with a
    /// bounded snapshot at the next terminal boundary.
    resync_required: HashSet<String>,
    /// Runs that crossed their terminal boundary with `resync_required` set;
    /// the UI drains these via `take_pending_resync`.
    resync_ready: Vec<String>,
    pending_event_count: usize,
    pending_encoded_bytes: usize,
    /// Separate native Runtime host for account Chat. It is configured by the
    /// TUI composition root and never points at the current interactive turn.
    runtime_chat: Option<RuntimeChatRelayHost>,
    /// A same-scope reconnect may need the old immutable host long enough to
    /// drain terminal/catalog rows behind the server cursor. New prompts stay
    /// fail-closed while this fresh configuration waits for an acknowledged
    /// boundary and a worker/controller host handoff.
    pending_runtime_chat_configuration: Option<PendingRuntimeChatConfiguration>,
    runtime_chat_refresh_release_requested: bool,
    runtime_chat_attachment: Option<RemoteAttachment>,
    uploaded_runtime_catalog_receipt: HashMap<String, String>,
}

impl Default for RemoteControlController {
    fn default() -> Self {
        Self {
            status: Status::Off,
            status_detail: "off".to_string(),
            account_ref: None,
            target_ref: None,
            links: RemoteLinks::default(),
            attached_run_id: None,
            attached_workspace_id: None,
            active_run: None,
            pending_local_turn_run: None,
            event_seq: HashMap::new(),
            uploaded_snapshots: HashSet::new(),
            pending_runtime_events: HashMap::new(),
            pending_approvals: HashMap::new(),
            command_fingerprints: HashMap::new(),
            worker_tx: None,
            event_rx: None,
            worker: None,
            applying_remote_command: false,
            ownership_blocked_until: None,
            journal: None,
            deferred_delta: HashMap::new(),
            resync_required: HashSet::new(),
            resync_ready: Vec::new(),
            pending_event_count: 0,
            pending_encoded_bytes: 0,
            runtime_chat: None,
            pending_runtime_chat_configuration: None,
            runtime_chat_refresh_release_requested: false,
            runtime_chat_attachment: None,
            uploaded_runtime_catalog_receipt: HashMap::new(),
        }
    }
}

impl RemoteControlController {
    /// Acquires and validates the saved-session authority before any Runtime
    /// Chat host/provider configuration is opened.
    ///
    /// The session-wide lifetime lock is intentionally acquired first. This
    /// fixes the lock order across processes: a competing workspace cannot
    /// make us release an existing Runtime store owner and only later discover
    /// that the saved session is permanently bound elsewhere.
    pub(crate) fn prepare_remote_control_session_journal(
        &mut self,
        dir: &Path,
        target_ref: &str,
        session_id: &str,
    ) -> Result<(), String> {
        let requested_scope = runtime_journal_scope_tag(target_ref, session_id);
        let requested_session_path = dir.join(format!(
            "active_classic_{}.json",
            runtime_journal_session_tag(session_id)
        ));
        if let Some(current) = self.journal.as_ref() {
            if current.active_index_path == requested_session_path {
                if current.scope_tag != requested_scope {
                    return Err(CLASSIC_LEASE_SCOPE_ERROR.to_string());
                }
                return Ok(());
            }
            if self.has_active_run() || self.has_unacknowledged_integrity_events() {
                return Err(
                    "The previous saved session still owns account delivery; reconnect it before opening another local session."
                        .to_string(),
                );
            }
        }
        drop(self.journal.take());
        let journal = RuntimeEventJournal::open(dir, target_ref, session_id)?;
        match journal.load() {
            Ok(restored) => {
                self.reset_pending_from(restored);
                self.journal = Some(journal);
                Ok(())
            }
            Err(error) => {
                if error == JOURNAL_LEGACY_SCOPE_ERROR {
                    journal.quarantine_legacy();
                } else {
                    journal.quarantine_current();
                }
                Err(error)
            }
        }
    }

    pub(crate) fn configure_runtime_chat(
        &mut self,
        config: crate::config::Config,
        plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
        private_root: PathBuf,
        target_ref: String,
        session_id: String,
    ) -> Result<(), String> {
        if matches!(
            self.status,
            Status::Connecting | Status::Connected | Status::Stopping
        ) {
            return Err("Remote control is already active.".to_string());
        }
        let requested = PendingRuntimeChatConfiguration {
            config,
            plugin_registry,
            private_root,
            target_ref,
            session_id,
        };
        if let Some(host) = self.runtime_chat.as_ref() {
            if host.scope_matches(&requested.target_ref, &requested.session_id)
                && (host.has_any_unsettled_turns() || self.has_unacknowledged_integrity_events())
            {
                // Reuse the existing lifetime owner and durable manager only
                // while recovery still needs it. The fresh config is retained
                // separately and every new prompt is rejected until the
                // terminal/catalog backlog is server-acked and the connected
                // worker has explicitly dropped its clone.
                self.pending_runtime_chat_configuration = Some(requested);
                self.runtime_chat_refresh_release_requested = false;
                return Ok(());
            }
            if host.has_any_unsettled_turns() {
                return Err(
                    "An isolated Runtime Chat turn is still active; reconnect its original remote-control session before opening another."
                        .to_string(),
                );
            }
        }
        if self.has_unacknowledged_integrity_events() {
            return Err(
                "The current remote-control session still has unacknowledged terminal events; reconnect it before opening another session."
                    .to_string(),
            );
        }
        // A completed `/rc` session deliberately keeps its durable host until
        // the next start. Release that lifetime owner lock before reopening the
        // same account/session namespace with a fresh provider configuration.
        self.install_runtime_chat_configuration(requested)
    }

    fn install_runtime_chat_configuration(
        &mut self,
        requested: PendingRuntimeChatConfiguration,
    ) -> Result<(), String> {
        drop(self.runtime_chat.take());
        let host = RuntimeChatRelayHost::open(
            requested.config,
            requested.plugin_registry,
            requested.private_root,
            requested.target_ref,
            requested.session_id,
        )?;
        self.runtime_chat = Some(host);
        self.pending_runtime_chat_configuration = None;
        self.runtime_chat_refresh_release_requested = false;
        self.uploaded_runtime_catalog_receipt.clear();
        Ok(())
    }

    fn schedule_runtime_chat_refresh_if_ready(&mut self) -> Result<(), String> {
        if self.pending_runtime_chat_configuration.is_none()
            || self.runtime_chat_refresh_release_requested
            || self.has_unacknowledged_integrity_events()
            || self
                .runtime_chat
                .as_ref()
                .is_some_and(RuntimeChatRelayHost::has_any_unsettled_turns)
        {
            return Ok(());
        }
        if self.status != Status::Connected {
            return Ok(());
        }
        let tx = self.worker_tx.as_ref().ok_or_else(|| {
            "Runtime Chat could not refresh its provider configuration safely.".to_string()
        })?;
        tx.send(WorkerCommand::ReleaseRuntimeChatHost)
            .map_err(|_| {
                "Runtime Chat could not refresh its provider configuration safely.".to_string()
            })?;
        self.runtime_chat_refresh_release_requested = true;
        self.status_detail = "refreshing the local Runtime model configuration".to_string();
        Ok(())
    }

    fn complete_runtime_chat_refresh(&mut self) -> Result<(), String> {
        if !self.runtime_chat_refresh_release_requested
            || self.has_unacknowledged_integrity_events()
            || self
                .runtime_chat
                .as_ref()
                .is_some_and(RuntimeChatRelayHost::has_any_unsettled_turns)
        {
            return Err("Runtime Chat received an invalid provider-refresh handoff.".to_string());
        }
        let requested = self
            .pending_runtime_chat_configuration
            .take()
            .ok_or_else(|| {
                "Runtime Chat received an invalid provider-refresh handoff.".to_string()
            })?;
        // The worker has already removed its clone. Dropping the controller's
        // final old host now releases both the relay scope and native store
        // owner locks before the fresh immutable configuration is opened.
        drop(self.runtime_chat.take());
        let host = RuntimeChatRelayHost::open(
            requested.config,
            requested.plugin_registry,
            requested.private_root,
            requested.target_ref,
            requested.session_id,
        )?;
        if let (Some(account_ref), Some(target_ref)) =
            (self.account_ref.as_deref(), self.target_ref.as_deref())
        {
            host.bind_account(account_ref, target_ref)?;
        }
        if let Some(run_id) = self.attached_run_id.as_deref() {
            host.authorize_run(run_id)?;
        }
        self.runtime_chat = Some(host.clone());
        self.runtime_chat_refresh_release_requested = false;
        self.uploaded_runtime_catalog_receipt.clear();
        self.worker_tx
            .as_ref()
            .ok_or_else(|| {
                "Runtime Chat could not install its refreshed provider configuration.".to_string()
            })?
            .send(WorkerCommand::InstallRuntimeChatHost(host))
            .map_err(|_| {
                "Runtime Chat could not install its refreshed provider configuration.".to_string()
            })?;
        if let Some(attachment) = self.runtime_chat_attachment.clone() {
            self.upload_runtime_chat_catalog(&attachment)?;
        }
        self.status_detail = "web mirror connected".to_string();
        Ok(())
    }

    pub fn start(&mut self, start: RemoteStart) -> Result<(), String> {
        if matches!(
            self.status,
            Status::Connecting | Status::Connected | Status::Stopping
        ) {
            return Err("Remote control is already active.".to_string());
        }
        if self.status == Status::Failed
            && self
                .ownership_blocked_until
                .is_some_and(|deadline| Instant::now() < deadline)
        {
            return Err(
                "The previous remote lease may still be active; wait for ownership to return before reconnecting."
                    .to_string(),
            );
        }
        if self.active_run.is_some() || self.pending_local_turn_run.is_some() {
            return Err(
                "The local Work turn is still running; wait for its terminal receipt before reconnecting remote control."
                    .to_string(),
            );
        }
        if self.has_unacknowledged_integrity_events()
            && self
                .runtime_chat
                .as_ref()
                .is_none_or(|host| !host.scope_matches(&start.target_ref, &start.session_id))
        {
            return Err(
                "The previous session still has unacknowledged terminal events; reconnect that exact session first."
                    .to_string(),
            );
        }
        if !valid_runtime_version(&start.runtime_version)
            || !valid_runtime_commit(&start.runtime_commit)
            || !valid_opaque_ref(&start.target_ref)
            || !valid_session_ref(&start.session_id)
        {
            return Err("This build or session does not have an enrollable identity.".to_string());
        }
        if !self.pending_runtime_events.is_empty()
            && let Some(journal) = &self.journal
        {
            // A prior send-first fault is impossible now, but a disk failure
            // can leave a classic Work terminal retained only in live memory.
            // Repair that exact pending set before loading/replacing the same
            // session journal; never let reconnect clear the sole copy.
            journal.persist(&self.pending_runtime_events)?;
        }
        match &start.journal_dir {
            Some(dir) => self.prepare_remote_control_session_journal(
                dir,
                &start.target_ref,
                &start.session_id,
            )?,
            None => self.journal = None,
        }
        self.recover_classic_lease_before_worker()?;
        self.recover_runtime_chat_ownership_before_worker()?;
        let (worker_tx, worker_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        self.stop_worker();
        self.status = Status::Connecting;
        self.status_detail = "waiting for account authorization".to_string();
        self.target_ref = Some(start.target_ref.clone());
        self.attached_run_id = None;
        self.attached_workspace_id = None;
        self.runtime_chat_attachment = None;
        self.uploaded_runtime_catalog_receipt.clear();
        self.active_run = None;
        self.pending_local_turn_run = None;
        self.worker_tx = Some(worker_tx);
        self.event_rx = Some(event_rx);
        let runtime_chat = self.runtime_chat.clone();
        self.worker = Some(tokio::spawn(async move {
            let mut phase = RelayPhase::Enrolling;
            if let Err(error) =
                relay_worker(start, runtime_chat, worker_rx, event_tx.clone(), &mut phase).await
            {
                let _ = if phase.lease_confirmed() {
                    event_tx.send(RemoteEvent::Failed(error))
                } else {
                    event_tx.send(RemoteEvent::FailedPreLease(error))
                };
            }
        }));
        Ok(())
    }

    fn recover_runtime_chat_ownership_before_worker(&self) -> Result<(), String> {
        let Some(host) = &self.runtime_chat else {
            return Ok(());
        };
        if host.has_any_unsettled_turns() || self.has_unacknowledged_integrity_events() {
            // A prior process may have stopped either while native inference
            // was still running or after terminal projection was durable but
            // before CWC acknowledged its account-delivery envelope. Reclaim
            // the exclusive attached-run writer synchronously before worker
            // enrollment (and therefore before any autonomous provider work)
            // can begin.
            host.recover_inference_ownership_for_pending_delivery()?;
        }
        Ok(())
    }

    fn recover_classic_lease_before_worker(&mut self) -> Result<(), String> {
        let Some(lease) = self
            .journal
            .as_ref()
            .and_then(RuntimeEventJournal::classic_lease)
        else {
            return Ok(());
        };
        self.event_seq
            .entry(lease.run_id.clone())
            .and_modify(|known| *known = (*known).max(lease.seq_floor))
            .or_insert(lease.seq_floor);
        let recovery_turn_id = lease
            .turn_id
            .clone()
            .unwrap_or_else(|| classic_recovery_turn_id(&lease.run_id, &lease.lease_id));
        let already_terminal =
            self.pending_runtime_events
                .get(&lease.run_id)
                .is_some_and(|events| {
                    events.iter().any(|(seq, entry)| {
                        *seq > lease.seq_floor
                            && runtime_envelope_event(&entry.envelope) == Some("turn.completed")
                            && entry.envelope.get("turn_id").and_then(Value::as_str)
                                == Some(recovery_turn_id.as_str())
                    })
                });
        if !already_terminal {
            let seq = self.next_runtime_seq(&lease.run_id);
            let envelope = runtime_envelope(
                seq,
                "turn.completed",
                Some(&recovery_turn_id),
                chrono::Utc::now().to_rfc3339(),
                json!({
                    "turn": {
                        "status": "failed",
                        "usage": {},
                        "error": "The local Runtime restarted before terminal delivery."
                    }
                }),
            );
            if !self.queue_runtime_envelope(&lease.run_id, envelope) {
                return Err(
                    "Remote control could not recover its interrupted account turn.".to_string(),
                );
            }
        }
        self.journal
            .as_ref()
            .ok_or_else(|| "Remote control lost its delivery journal during recovery.".to_string())?
            .set_classic_lease(None, &self.pending_runtime_events)?;
        Ok(())
    }

    /// Why `/rc stop` must currently be refused, if any reason exists.
    ///
    /// Stopping is only safe once no remote turn is active and every
    /// integrity-critical envelope (terminal turn state, approvals, failures,
    /// resynchronization snapshots) is behind the server-confirmed cursor.
    pub fn stop_refusal(&self) -> Option<String> {
        if self.has_active_run() {
            return Some(
                "Finish or interrupt the active remote turn before stopping remote control."
                    .to_string(),
            );
        }
        if self
            .runtime_chat
            .as_ref()
            .is_some_and(RuntimeChatRelayHost::has_any_unsettled_turns)
        {
            return Some(
                "Finish or interrupt the active Runtime Chat turn before stopping remote control."
                    .to_string(),
            );
        }
        if self.has_unacknowledged_integrity_events() {
            return Some(
                "The server has not yet acknowledged this session's terminal or approval events; try /rc stop again in a moment."
                    .to_string(),
            );
        }
        None
    }

    fn has_unacknowledged_integrity_events(&self) -> bool {
        self.pending_runtime_events
            .values()
            .flat_map(BTreeMap::values)
            .any(|entry| entry.integrity)
    }

    pub(crate) fn runtime_chat_blocks_local_dispatch(&self) -> bool {
        self.has_durable_classic_lease()
            || self.pending_runtime_chat_configuration.is_some()
            || self.runtime_chat.as_ref().is_some_and(|host| {
                host.has_any_unsettled_turns() || self.has_unacknowledged_integrity_events()
            })
    }

    #[cfg(test)]
    pub(crate) fn block_runtime_chat_dispatch_for_tests(&mut self) {
        self.pending_runtime_chat_configuration = Some(PendingRuntimeChatConfiguration {
            config: crate::config::Config::default(),
            plugin_registry: std::sync::Arc::new(crate::plugins::PluginRegistry::empty(Path::new(
                ".",
            ))),
            private_root: PathBuf::from("runtime-chat-test-block"),
            target_ref: "target_test_block".to_string(),
            session_id: "session_test_block".to_string(),
        });
    }

    fn attachment_switch_refusal(&self, next_run_id: &str) -> Option<String> {
        let current_run_id = self.attached_run_id.as_deref()?;
        if current_run_id == next_run_id {
            return None;
        }
        if self.has_active_run() {
            return Some(
                "Finish the active Work turn before attaching another account run.".to_string(),
            );
        }
        let has_unacknowledged_terminal = self
            .pending_runtime_events
            .get(current_run_id)
            .is_some_and(|events| events.values().any(|entry| entry.integrity));
        has_unacknowledged_terminal.then(|| {
            "The current Runtime Chat turn must be acknowledged before attaching another run."
                .to_string()
        })
    }

    pub fn stop(&mut self) {
        if let Some(refusal) = self.stop_refusal() {
            self.status_detail = refusal;
            return;
        }
        if self.status == Status::Connecting {
            // The worker may have completed its server-side connect just before
            // the UI consumed RemoteEvent::Connected. Aborting it cannot prove
            // that no lease exists, so retain the ownership lock through the
            // server expiry instead of returning local input immediately.
            self.stop_worker();
            self.status = Status::Failed;
            self.status_detail =
                "authorization cancelled; waiting for any server lease to expire safely"
                    .to_string();
            self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
        } else if self.status == Status::Connected {
            // Hand every deferred delta to the transport first so the worker's
            // pre-heartbeat drain covers the complete unacknowledged set.
            self.hand_off_all_deferred();
            let queued = self
                .worker_tx
                .as_ref()
                .is_some_and(|tx| tx.send(WorkerCommand::Stop).is_ok());
            self.worker_tx = None;
            if queued {
                self.status = Status::Stopping;
                self.status_detail = "confirming the runner is offline".to_string();
            } else {
                self.status = Status::Failed;
                self.status_detail =
                    "waiting for the last server lease to expire safely".to_string();
                self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
            }
        }
        if self.status == Status::Failed && self.ownership_blocked_until.is_none() {
            // A pre-lease failure holds no lease; stopping is an ordinary
            // reset, not a drain confirmation.
            self.status = Status::Off;
        }
        if self.status == Status::Off {
            self.account_ref = None;
            self.links = RemoteLinks::default();
            self.attached_run_id = None;
            self.attached_workspace_id = None;
            self.runtime_chat_attachment = None;
            self.active_run = None;
            self.pending_local_turn_run = None;
            self.pending_approvals.clear();
            self.command_fingerprints.clear();
            self.ownership_blocked_until = None;
        }
    }

    fn stop_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
        if let Some(host) = &self.runtime_chat {
            // Projection claims are only an in-process handoff lease between
            // the worker and this controller. Aborting the worker or dropping
            // its receiver must make every unjournaled native event eligible
            // for replay on the same durable host.
            host.release_all_projection_claims();
        }
        self.worker_tx = None;
        self.event_rx = None;
        self.runtime_chat_refresh_release_requested = false;
    }

    fn quarantine_rejected_authority(&mut self, error: String) -> RemoteEvent {
        // Once an attachment fails its account/run/catalog authority checks,
        // no later event from that worker can be trusted. In particular, the
        // classic Prompt and Approval command paths predate Runtime Chat and
        // must not keep consuming commands from a rejected re-enrollment.
        self.stop_worker();
        self.status = Status::Failed;
        self.status_detail = error.clone();
        self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
        RemoteEvent::Failed(error)
    }

    pub fn try_next_event(&mut self) -> Option<RemoteEvent> {
        // Coalescing ends at the next UI poll: hand any deferred delta to the
        // transport so live viewers never wait more than one tick.
        self.hand_off_all_deferred();
        if self.status == Status::Failed
            && self
                .ownership_blocked_until
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            if self.has_active_run()
                || self.has_unacknowledged_integrity_events()
                || self
                    .runtime_chat
                    .as_ref()
                    .is_some_and(RuntimeChatRelayHost::has_any_unsettled_turns)
            {
                // The server lease may have expired, but provider-backed work
                // still owns this private Runtime store. Keep the host/scope
                // lock and allow only a same-session reconnect to resume
                // projection/drain; never transition Off and reopen a second
                // manager over the active store.
                self.stop_worker();
                self.status_detail =
                    "this session still has unsettled or unacknowledged Runtime Chat work; reconnect it to drain"
                        .to_string();
                self.ownership_blocked_until = None;
                return None;
            }
            let approvals = self
                .pending_approvals
                .drain()
                .map(|(_, value)| value)
                .collect();
            self.stop_worker();
            self.status = Status::Off;
            self.status_detail = "off".to_string();
            self.ownership_blocked_until = None;
            return Some(RemoteEvent::OwnershipRestored { approvals });
        }
        let event = self.event_rx.as_mut()?.try_recv().ok()?;
        match &event {
            RemoteEvent::Connected {
                account_ref,
                target_ref,
                attachment,
                links,
                ..
            } => {
                if let Some(error) = self.attachment_switch_refusal(&attachment.run_id) {
                    return Some(self.quarantine_rejected_authority(error));
                }
                if let Some(host) = &self.runtime_chat
                    && let Err(error) = host
                        .bind_account(account_ref, target_ref)
                        .and_then(|()| host.authorize_run(&attachment.run_id))
                {
                    return Some(self.quarantine_rejected_authority(error));
                }
                self.apply_attachment(attachment);
                if self.pending_runtime_chat_configuration.is_none()
                    && self.runtime_chat.is_some()
                    && let Err(error) = self.upload_runtime_chat_catalog(attachment)
                {
                    return Some(self.quarantine_rejected_authority(error));
                }
                self.links = links.clone();
                // Journal recovery may hold unacknowledged envelopes for runs
                // beyond this attachment; resend every pending run now.
                self.flush_all_pending();
                self.status = Status::Connected;
                self.status_detail = "web mirror connected".to_string();
                self.ownership_blocked_until = None;
                self.account_ref = Some(account_ref.clone());
                self.target_ref = Some(target_ref.clone());
                if let Err(error) = self.schedule_runtime_chat_refresh_if_ready() {
                    return Some(self.quarantine_rejected_authority(error));
                }
            }
            RemoteEvent::Attachment {
                account_ref,
                target_ref,
                attachment,
                links,
            } => {
                if let Some(error) = self.attachment_switch_refusal(&attachment.run_id) {
                    return Some(self.quarantine_rejected_authority(error));
                }
                if let Some(host) = &self.runtime_chat
                    && let Err(error) = host
                        .bind_account(account_ref, target_ref)
                        .and_then(|()| host.authorize_run(&attachment.run_id))
                {
                    return Some(self.quarantine_rejected_authority(error));
                }
                self.apply_attachment(attachment);
                if self.pending_runtime_chat_configuration.is_none()
                    && self.runtime_chat.is_some()
                    && let Err(error) = self.upload_runtime_chat_catalog(attachment)
                {
                    return Some(self.quarantine_rejected_authority(error));
                }
                self.links = links.clone();
                self.account_ref = Some(account_ref.clone());
                self.target_ref = Some(target_ref.clone());
                if let Err(error) = self.schedule_runtime_chat_refresh_if_ready() {
                    return Some(self.quarantine_rejected_authority(error));
                }
            }
            RemoteEvent::RuntimeCursor { run_id, cursor } => {
                self.reconcile_runtime_cursor(run_id, *cursor);
                if let Err(error) = self.schedule_runtime_chat_refresh_if_ready() {
                    return Some(self.quarantine_rejected_authority(error));
                }
            }
            RemoteEvent::RuntimeChatHostReleased => {
                if let Err(error) = self.complete_runtime_chat_refresh() {
                    return Some(self.quarantine_rejected_authority(error));
                }
            }
            RemoteEvent::RuntimeChatProjection(projection) => {
                if let Err(error) = self.upload_runtime_chat_projection(projection) {
                    if let Some(host) = &self.runtime_chat {
                        host.release_projection(
                            &projection.native_thread_id,
                            projection.native_seq,
                        );
                    }
                    self.stop_worker();
                    self.status = Status::Failed;
                    self.status_detail = error.clone();
                    self.ownership_blocked_until =
                        Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
                    return Some(RemoteEvent::Failed(error));
                }
            }
            RemoteEvent::FailedPreLease(reason) => {
                // No server-confirmed lease ever existed, so nothing is
                // locked: no reconnect blackout, no approval handoff to
                // undo, and `/rc` can retry immediately.
                self.status = Status::Failed;
                self.status_detail = reason.clone();
                self.ownership_blocked_until = None;
                if !self.has_durable_classic_lease() {
                    self.active_run = None;
                    self.pending_local_turn_run = None;
                }
            }
            RemoteEvent::Failed(reason) => {
                self.status = Status::Failed;
                self.status_detail =
                    format!("{reason}; waiting for the last server lease to expire safely");
                self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
                // Exact unacknowledged runtime envelopes remain owned by this
                // controller (and its journal). Keep any classic provider-turn
                // association live as well: the engine can still deliver its
                // exact terminal status/usage after the transport fails.
            }
            RemoteEvent::Stopped => {
                self.status = Status::Off;
                self.status_detail = "off".to_string();
                self.active_run = None;
                self.pending_local_turn_run = None;
                self.attached_run_id = None;
                self.attached_workspace_id = None;
                self.runtime_chat_attachment = None;
                self.links = RemoteLinks::default();
                self.ownership_blocked_until = None;
                // The worker only reports Stopped after draining through the
                // server-confirmed cursor and posting the offline heartbeat,
                // so an empty pending set means the journal is spent.
                if self.pending_runtime_events.values().all(BTreeMap::is_empty)
                    && let Some(journal) = &self.journal
                {
                    journal.remove();
                }
                if !self.pending_approvals.is_empty() {
                    let approvals = self
                        .pending_approvals
                        .drain()
                        .map(|(_, value)| value)
                        .collect();
                    return Some(RemoteEvent::OwnershipRestored { approvals });
                }
            }
            RemoteEvent::Notice(_)
            | RemoteEvent::Command { .. }
            | RemoteEvent::OwnershipRestored { .. } => {}
        }
        Some(event)
    }

    /// The validated web link for the live session, once the control plane
    /// has advertised one.
    pub fn run_url(&self) -> Option<&str> {
        if matches!(self.status, Status::Connected | Status::Stopping) {
            self.links.run_url.as_deref()
        } else {
            None
        }
    }

    /// The validated web link for this computer's settings row, if advertised.
    pub fn computer_url(&self) -> Option<&str> {
        if matches!(self.status, Status::Connected | Status::Stopping) {
            self.links.computer_url.as_deref()
        } else {
            None
        }
    }

    /// One word for the posture bar's right slot, `None` while remote
    /// control is off so the bar paints nothing rather than "off".
    #[must_use]
    pub fn status_word(&self) -> Option<&'static str> {
        match self.status {
            Status::Off => None,
            Status::Connecting => Some("connecting"),
            Status::Connected => Some("connected"),
            Status::Stopping => Some("stopping"),
            Status::Failed => Some("failed"),
        }
    }

    #[must_use]
    pub(crate) fn status(&self) -> Status {
        self.status
    }

    /// Test-only: advertise a validated live session link without a control
    /// plane. Mirrors the shape the runner-lease parser installs (`Connected`
    /// plus validated links) so `/rc link`/`/rc open` routing can be exercised
    /// deterministically offline.
    #[cfg(test)]
    pub(crate) fn install_live_link_for_test(&mut self, run_url: &str, computer_url: Option<&str>) {
        self.status = Status::Connected;
        self.status_detail = "test link".to_string();
        self.links = RemoteLinks {
            run_url: Some(run_url.to_string()),
            computer_url: computer_url.map(str::to_string),
        };
    }

    pub fn status_line(&self) -> String {
        match self.status {
            Status::Off => "Remote control: off".to_string(),
            Status::Connecting => format!("Remote control: connecting · {}", self.status_detail),
            Status::Connected => match self.links.run_url.as_deref() {
                Some(url) => format!(
                    "Remote control: connected · account {} · {} · open {url}",
                    self.account_ref.as_deref().unwrap_or("account"),
                    self.status_detail
                ),
                None => format!(
                    "Remote control: connected · account {} · {}",
                    self.account_ref.as_deref().unwrap_or("account"),
                    self.status_detail
                ),
            },
            Status::Stopping => {
                "Remote control: stopping · confirming the runner is offline".to_string()
            }
            Status::Failed => {
                if self
                    .ownership_blocked_until
                    .is_some_and(|deadline| Instant::now() < deadline)
                {
                    format!(
                        "Remote control: lost after connecting · {} · reconnect waits for the server lease to drain",
                        self.status_detail
                    )
                } else {
                    format!(
                        "Remote control: failed before connecting · {} · /rc to retry",
                        self.status_detail
                    )
                }
            }
        }
    }

    /// Whether the web mirror can carry an approval decision right now.
    ///
    /// `Connecting` deliberately does not qualify: there is not yet an
    /// attachment/run cursor able to carry a typed approval, so the local
    /// card stays the only actionable surface until `Connected`. A
    /// transport failure also disqualifies — a dead relay cannot deliver a
    /// decision, and the local card remains the source of truth either way.
    ///
    /// Mirror semantics: this never gates *local* input. It only decides
    /// whether the approval card is *also* shared with the web.
    pub fn can_share_approval_with_web(&self) -> bool {
        let attached = match self.status {
            // A connection alone is not enough. Until a concrete typed turn
            // id is bound, `record_remote_approval` has nowhere safe to send
            // a decision, so the card stays local-only.
            Status::Connected | Status::Stopping => self.active_run.is_some(),
            Status::Off | Status::Connecting | Status::Failed => false,
        };
        attached && !self.applying_remote_command
    }

    /// Record that the *local* surface answered an approval, so a late web
    /// decision for the same tool is acknowledged as "no longer pending"
    /// instead of double-answering the engine. First decision wins; the
    /// other surface is told.
    pub fn resolve_pending_approval(&mut self, tool_id: &str, approved: bool) -> bool {
        self.settle_pending_approval(
            tool_id,
            if approved { "approved" } else { "denied" },
            "terminal",
        )
    }

    /// The request's wait ended without a decision from any surface — its
    /// agent's work ended, or it was answered through another path. Retire
    /// the web's copy as `withdrawn`, never as a person's denial.
    pub fn withdraw_pending_approval(&mut self, tool_id: &str) -> bool {
        self.settle_pending_approval(tool_id, "withdrawn", "agent")
    }

    fn settle_pending_approval(&mut self, tool_id: &str, decision: &str, decided_by: &str) -> bool {
        let gate = projected_approval_id(tool_id);
        if self.pending_approvals.remove(&gate).is_none() {
            return false;
        }
        if let Some(active) = self.active_run.clone() {
            self.upload_envelope(
                &active.run_id,
                "approval.resolved",
                Some(&active.turn_id),
                json!({
                    "id": gate,
                    "approval_id": gate,
                    "decision": decision,
                    "decided_by": decided_by,
                }),
            );
        }
        true
    }

    pub fn set_applying_remote_command(&mut self, value: bool) {
        self.applying_remote_command = value;
    }

    /// Test-only: put the controller into the exact state a live connected
    /// mirror with an attached run would be in, without a relay worker.
    #[cfg(test)]
    pub(crate) fn force_mirror_connected_for_tests(&mut self, run_id: &str, turn_id: &str) {
        self.status = Status::Connected;
        self.status_detail = "web mirror connected".to_string();
        self.attached_run_id = Some(run_id.to_string());
        self.active_run = Some(ActiveRelayRun {
            run_id: run_id.to_string(),
            turn_id: turn_id.to_string(),
        });
    }

    #[cfg(test)]
    pub(crate) fn queue_remote_event_for_tests(&mut self, event: RemoteEvent) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        event_tx.send(event).expect("test event receiver");
        self.event_rx = Some(event_rx);
        if self.worker_tx.is_none() {
            let (worker_tx, _worker_rx) = mpsc::unbounded_channel();
            self.worker_tx = Some(worker_tx);
        }
    }

    pub fn claim_command(
        &mut self,
        run_id: &str,
        seq: u64,
        command: &RemoteCommand,
    ) -> Result<bool, String> {
        if self.status != Status::Connected || self.attached_run_id.as_deref() != Some(run_id) {
            return Err(
                "The remote command does not belong to the current authorized attachment."
                    .to_string(),
            );
        }
        let fingerprint = command_fingerprint(command);
        let key = (run_id.to_string(), seq);
        if let Some(existing) = self.command_fingerprints.get(&key) {
            if existing == &fingerprint {
                // Runtime Chat's native operation key is idempotent. The UI
                // bridge deliberately re-enters that path for an exact replay
                // so a failed acknowledgement POST is issued again; admission
                // gates below describe *new* work and must not false-fail it.
                return Ok(false);
            }
            return Err(
                "The control plane reused a command sequence with different content.".to_string(),
            );
        }
        if let RemoteCommand::RuntimeChatPrompt(prompt) = command
            && let Some(host) = &self.runtime_chat
            && host.is_exact_prompt_replay(prompt)?
        {
            // The controller's command-sequence cache is intentionally
            // process-local, while the native operation/turn binding is
            // durable. After a crash or deferred provider-config refresh,
            // recognize that durable exact replay before new-work gates and
            // reissue its acknowledgement through the idempotent host path.
            self.command_fingerprints.insert(key, fingerprint);
            return Ok(false);
        }
        match command {
            RemoteCommand::Prompt { .. } | RemoteCommand::RuntimeChatPrompt(_)
                if self.pending_runtime_chat_configuration.is_some() =>
            {
                return Err(
                    "Wait for Runtime Chat to finish refreshing its local model configuration."
                        .to_string(),
                );
            }
            RemoteCommand::RuntimeChatPrompt(_) if self.has_active_run() => {
                return Err("Finish the active Work turn before starting Runtime Chat.".to_string());
            }
            RemoteCommand::RuntimeChatPrompt(_) if self.has_unacknowledged_integrity_events() => {
                return Err(
                    "Wait for the current Runtime Chat catalog or terminal event to be acknowledged before starting another turn."
                        .to_string(),
                );
            }
            RemoteCommand::Prompt { .. }
                if self
                    .runtime_chat
                    .as_ref()
                    .is_some_and(RuntimeChatRelayHost::has_any_unsettled_turns)
                    || self
                        .pending_runtime_events
                        .get(run_id)
                        .is_some_and(|events| events.values().any(|entry| entry.integrity)) =>
            {
                return Err(
                    "Finish and acknowledge the active Runtime Chat turn before starting Work."
                        .to_string(),
                );
            }
            _ => {}
        }
        self.command_fingerprints.insert(key, fingerprint);
        Ok(true)
    }

    pub fn activate_prompt(&mut self, run_id: &str, turn_id: &str) -> Result<(), String> {
        self.activate_prompt_with_lease_id(run_id, turn_id, String::new())
    }

    fn activate_prompt_with_lease_id(
        &mut self,
        run_id: &str,
        turn_id: &str,
        lease_id: String,
    ) -> Result<(), String> {
        let active = ActiveRelayRun {
            run_id: run_id.to_string(),
            turn_id: turn_id.to_string(),
        };
        self.begin_classic_lease(ClassicRunLease {
            run_id: active.run_id.clone(),
            turn_id: Some(active.turn_id.clone()),
            lease_id,
            seq_floor: self.runtime_seq_floor(&active.run_id),
        })?;
        self.active_run = Some(active);
        Ok(())
    }

    fn promote_pending_local_turn(&mut self, run_id: &str, turn_id: &str) -> Result<(), String> {
        let lease_id = self
            .journal
            .as_ref()
            .and_then(RuntimeEventJournal::classic_lease)
            .filter(|lease| lease.run_id == run_id && lease.turn_id.is_none())
            .map(|lease| lease.lease_id)
            .ok_or_else(|| {
                "Remote control lost its durable pre-dispatch lease generation.".to_string()
            })?;
        self.activate_prompt_with_lease_id(run_id, turn_id, lease_id)
    }

    #[cfg(test)]
    pub fn active_run_matches(&self, run_id: &str) -> bool {
        self.active_run
            .as_ref()
            .is_some_and(|active| active.run_id == run_id)
            || self.pending_local_turn_run.as_deref() == Some(run_id)
    }

    /// Legacy Work cancellation is exact-turn scoped. A delayed command for
    /// an older turn in the same run must never cancel the current provider
    /// lifecycle.
    pub fn active_turn_matches(&self, run_id: &str, turn_id: &str) -> bool {
        self.active_run
            .as_ref()
            .is_some_and(|active| active.run_id == run_id && active.turn_id == turn_id)
            || self
                .journal
                .as_ref()
                .and_then(RuntimeEventJournal::classic_lease)
                .is_some_and(|lease| {
                    lease.run_id == run_id && lease.turn_id.as_deref() == Some(turn_id)
                })
    }

    /// A remotely-owned turn must reach a terminal engine event before the
    /// user can release the relay lease. Dropping the worker earlier would
    /// discard the run binding and strand the control-plane ledger while the
    /// local engine continued producing results.
    pub fn has_active_run(&self) -> bool {
        self.active_run.is_some()
            || self.pending_local_turn_run.is_some()
            || self.has_durable_classic_lease()
    }

    fn has_durable_classic_lease(&self) -> bool {
        self.journal
            .as_ref()
            .and_then(RuntimeEventJournal::classic_lease)
            .is_some()
    }

    /// Bind the server-confirmed attachment to the local turn that was
    /// already running when `/rc` was invoked.
    ///
    /// With a typed runtime turn id the binding is immediate. During the
    /// narrow dispatch-before-`TurnStarted` window, the run is parked and the
    /// first typed start event completes the binding. Replays are idempotent:
    /// an existing binding is never replaced and no runtime envelope is
    /// emitted by this method itself.
    pub fn attach_current_local_turn(&mut self, turn_id: Option<&str>) -> bool {
        if self.status != Status::Connected || self.has_active_run() {
            return false;
        }
        let Some(run_id) = self.attached_run_id.clone() else {
            return false;
        };
        match turn_id.map(str::trim).filter(|turn_id| !turn_id.is_empty()) {
            Some(turn_id) => {
                if self.activate_prompt(&run_id, turn_id).is_err() {
                    return false;
                }
            }
            None => {
                if self
                    .begin_classic_lease(ClassicRunLease {
                        run_id: run_id.clone(),
                        turn_id: None,
                        lease_id: String::new(),
                        seq_floor: self.runtime_seq_floor(&run_id),
                    })
                    .is_err()
                {
                    return false;
                }
                self.pending_local_turn_run = Some(run_id);
            }
        }
        true
    }

    /// Release a dispatch-window binding when the local dispatcher becomes
    /// idle without ever producing a typed `TurnStarted`. The account-owned
    /// attachment remains connected and can accept a later web prompt; only
    /// the nonexistent turn lease is removed.
    pub fn release_unstarted_local_turn(&mut self) -> bool {
        if self.pending_local_turn_run.is_none() {
            return false;
        }
        if self.finish_classic_lease().is_err() {
            return false;
        }
        self.pending_local_turn_run = None;
        true
    }

    fn begin_classic_lease(&mut self, lease: ClassicRunLease) -> Result<(), String> {
        let mut lease = lease;
        lease.seq_floor = lease.seq_floor.max(self.runtime_seq_floor(&lease.run_id));
        if let Some(previous) = self
            .journal
            .as_ref()
            .and_then(RuntimeEventJournal::classic_lease)
            .filter(|previous| previous.run_id == lease.run_id)
        {
            lease.seq_floor = lease.seq_floor.max(previous.seq_floor);
        }
        // A blank id always starts a fresh generation. The dispatch-window
        // promotion above is the only valid reuse and supplies its durable id
        // explicitly so a stale pre-dispatch lease cannot be inherited here.
        if lease.lease_id.is_empty() {
            lease.lease_id = format!("classic_lease_{}", uuid::Uuid::new_v4().simple());
        }
        if !lease.valid() {
            return Err("The remote Work turn identity is invalid.".to_string());
        }
        let Some(journal) = &self.journal else {
            #[cfg(test)]
            return Ok(());
            #[cfg(not(test))]
            {
                self.status = Status::Failed;
                self.status_detail =
                    "Remote control requires a durable account-delivery journal.".to_string();
                return Err(self.status_detail.clone());
            }
        };
        if let Err(error) = journal.set_classic_lease(Some(lease), &self.pending_runtime_events) {
            self.status = Status::Failed;
            self.status_detail = error.clone();
            self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
            return Err(error);
        }
        Ok(())
    }

    fn finish_classic_lease(&mut self) -> Result<(), String> {
        let Some(journal) = &self.journal else {
            #[cfg(test)]
            return Ok(());
            #[cfg(not(test))]
            return Err("Remote control requires a durable account-delivery journal.".to_string());
        };
        if let Err(error) = journal.set_classic_lease(None, &self.pending_runtime_events) {
            self.status = Status::Failed;
            self.status_detail = error.clone();
            self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
            return Err(error);
        }
        Ok(())
    }

    /// A remote prompt can fail during local route preparation before the
    /// engine owns a turn and therefore before it can emit `EngineEvent::Error`.
    /// That failure is still terminal for the account-owned run.
    pub fn fail_active_dispatch(&mut self, error: &str) {
        self.fail_active_run("dispatch_failed", error);
    }

    fn apply_attachment(&mut self, attachment: &RemoteAttachment) {
        self.attached_run_id = Some(attachment.run_id.clone());
        self.attached_workspace_id = Some(attachment.workspace_id.clone());
        self.runtime_chat_attachment = Some(attachment.clone());
        self.reconcile_runtime_cursor(&attachment.run_id, attachment.runtime_cursor);
        let local_cursor = self
            .pending_runtime_events
            .get(&attachment.run_id)
            .and_then(|events| events.last_key_value().map(|(seq, _)| *seq))
            .unwrap_or(0);
        let cursor = self.event_seq.entry(attachment.run_id.clone()).or_insert(0);
        *cursor = (*cursor).max(attachment.runtime_cursor).max(local_cursor);
        self.flush_pending_runtime_events(&attachment.run_id);
        // `snapshot_present` is server history, not proof that this freshly
        // loaded TUI process has uploaded its current saved history. The local
        // marker below prevents ordinary same-process reconnect duplication.
    }

    fn upload_runtime_chat_catalog(&mut self, attachment: &RemoteAttachment) -> Result<(), String> {
        if self.pending_runtime_chat_configuration.is_some() {
            return Err(
                "Runtime Chat must refresh its local model configuration before advertising a catalog."
                    .to_string(),
            );
        }
        let host = self
            .runtime_chat
            .clone()
            .ok_or_else(|| "This Runtime cannot advertise an isolated Chat relay.".to_string())?;
        let payload = host.catalog(&attachment.runtime_chat_relay_challenge)?;
        let payload_fingerprint = RuntimeChatRelayHost::catalog_payload_fingerprint(&payload)?;
        let receipt_key = format!(
            "{}:{}",
            attachment.runtime_chat_relay_challenge, payload_fingerprint
        );
        if self
            .uploaded_runtime_catalog_receipt
            .get(&attachment.run_id)
            .is_some_and(|receipt| receipt == &receipt_key)
        {
            return Ok(());
        }
        let source_event_id =
            runtime_chat_catalog_source_event_id(&attachment.run_id, &payload_fingerprint);
        if self.pending_runtime_chat_catalog_matches(&attachment.run_id, &source_event_id) {
            self.uploaded_runtime_catalog_receipt
                .insert(attachment.run_id.clone(), receipt_key);
            return Ok(());
        }
        // A local receipt proves only that the catalog was journaled, not that
        // the server cursor acknowledged it. After restart, resend the stable
        // semantic source id whenever no matching pending entry remains. CWC
        // deduplicates an exact replay and rejects conflicting reuse, while a
        // changed safe catalog gets a different payload fingerprint/source id.
        let seq = self.next_runtime_seq(&attachment.run_id);
        let envelope = runtime_chat_envelope(
            seq,
            "runtime.catalog",
            None,
            None,
            Some(&source_event_id),
            RUNTIME_CHAT_CATALOG_TIMESTAMP.to_string(),
            payload,
        );
        if !self.queue_runtime_chat_envelope(&attachment.run_id, envelope) {
            return Err("Runtime Chat could not journal its challenge-bound catalog.".to_string());
        }
        self.uploaded_runtime_catalog_receipt
            .insert(attachment.run_id.clone(), receipt_key);
        Ok(())
    }

    fn pending_runtime_chat_catalog_matches(&self, run_id: &str, source_event_id: &str) -> bool {
        self.pending_runtime_events
            .get(run_id)
            .is_some_and(|events| {
                events.values().any(|event| {
                    event.envelope.get("event").and_then(Value::as_str) == Some("runtime.catalog")
                        && event
                            .envelope
                            .get("source_event_id")
                            .and_then(Value::as_str)
                            == Some(source_event_id)
                })
            })
    }

    fn upload_runtime_chat_projection(
        &mut self,
        projection: &RuntimeChatProjection,
    ) -> Result<(), String> {
        if self.attached_run_id.as_deref() != Some(projection.run_id.as_str()) {
            // A reconnect can change the single attached run after the worker
            // claimed an event from the previous run. Leave that durable
            // backlog with its rightful run instead of delivering it across
            // authority or poisoning the new attachment.
            self.runtime_chat
                .as_ref()
                .ok_or_else(|| "This Runtime Chat relay is not configured.".to_string())?
                .release_projection(&projection.native_thread_id, projection.native_seq);
            return Ok(());
        }
        let seq = self.next_runtime_seq(&projection.run_id);
        let envelope = runtime_chat_envelope(
            seq,
            projection.event,
            Some(&projection.virtual_thread_id),
            Some(&projection.virtual_turn_id),
            Some(&projection.source_event_id),
            projection.timestamp.clone(),
            projection.payload.clone(),
        );
        if !self.queue_runtime_chat_envelope(&projection.run_id, envelope) {
            return Err("Runtime Chat could not journal its native turn event.".to_string());
        }
        self.runtime_chat
            .as_ref()
            .ok_or_else(|| "This Runtime Chat relay is not configured.".to_string())?
            .mark_projected(
                &projection.native_thread_id,
                projection.native_seq,
                &projection.virtual_turn_id,
                projection.event,
            )
    }

    pub fn upload_snapshot(&mut self, run_id: &str, messages: &[Message]) {
        if self.uploaded_snapshots.contains(run_id) {
            return;
        }
        let seq = self.next_runtime_seq(run_id);
        let envelope = bounded_session_snapshot_envelope(seq, messages);
        if self.queue_runtime_envelope(run_id, envelope) {
            self.uploaded_snapshots.insert(run_id.to_string());
        }
    }

    pub fn acknowledge(
        &self,
        run_id: &str,
        seq: u64,
        command: &RemoteCommand,
        status: &str,
        error: Option<String>,
    ) {
        let Some(tx) = &self.worker_tx else {
            return;
        };
        let _ = tx.send(WorkerCommand::Upload {
            run_id: run_id.to_string(),
            acknowledgements: vec![CommandAcknowledgement {
                command_seq: seq,
                command_type: command.kind().to_string(),
                status: status.to_string(),
                turn_id: command.turn_id().map(ToString::to_string),
                error: error.map(|value| value.chars().take(800).collect()),
            }],
            envelopes: Vec::new(),
        });
    }

    pub(crate) async fn apply_runtime_chat_prompt(
        &self,
        command: &RuntimeChatPrompt,
    ) -> Result<(), String> {
        if self.status != Status::Connected
            || self.attached_run_id.as_deref() != Some(command.run_id.as_str())
            || self.attached_workspace_id.as_deref() != Some(command.workspace.id.as_str())
            || self.target_ref.as_deref() != Some(command.workspace.target_ref.as_str())
        {
            return Err("The Runtime Chat command does not match the attached run.".to_string());
        }
        let host = self
            .runtime_chat
            .clone()
            .ok_or_else(|| "This Runtime Chat relay is not configured.".to_string())?;
        if self.pending_runtime_chat_configuration.is_some()
            && !host.is_exact_prompt_replay(command)?
        {
            return Err("Runtime Chat is refreshing its local model configuration.".to_string());
        }
        host.apply_prompt(command).await
    }

    pub(crate) async fn interrupt_runtime_chat(
        &self,
        run_id: &str,
        scope: &RuntimeChatControlScope,
        virtual_turn_id: &str,
    ) -> Result<(), String> {
        if self.status != Status::Connected || self.attached_run_id.as_deref() != Some(run_id) {
            return Err("The Runtime Chat interrupt does not match the attached run.".to_string());
        }
        let host = self
            .runtime_chat
            .clone()
            .ok_or_else(|| "This Runtime Chat relay is not configured.".to_string())?;
        host.interrupt(run_id, scope, virtual_turn_id).await
    }

    pub fn record_remote_approval(
        &mut self,
        tool_id: &str,
        tool_name: &str,
        description: &str,
        _input: &Value,
        _approval_key: &str,
        _intent_summary: Option<&str>,
    ) -> String {
        let gate = projected_approval_id(tool_id);
        self.pending_approvals.insert(
            gate.clone(),
            PendingRemoteApproval {
                tool_id: tool_id.to_string(),
            },
        );
        if let Some(active) = self.active_run.clone() {
            self.upload_envelope(
                &active.run_id,
                "approval.required",
                Some(&active.turn_id),
                json!({
                    "id": gate,
                    "approval_id": gate,
                    "tool_name": tool_name,
                    "description": description,
                }),
            );
        }
        gate
    }

    pub fn pending_approval_tool_id(&self, gate: &str) -> Option<String> {
        self.pending_approvals
            .get(gate)
            .map(|approval| approval.tool_id.clone())
    }

    pub fn take_pending_approval(&mut self, gate: &str) -> Option<String> {
        self.pending_approvals
            .remove(gate)
            .map(|approval| approval.tool_id)
    }

    pub fn observe_engine_event(&mut self, event: &EngineEvent) {
        // `/rc` can attach while the host is still preparing a turn. The
        // server run is known first; `TurnStarted` supplies the authoritative
        // runtime turn id later. Promote exactly once, before the ordinary
        // projection below observes the event.
        if let EngineEvent::TurnStarted { turn_id, .. } = event
            && self.active_run.is_none()
            && let Some(run_id) = self.pending_local_turn_run.take()
            && self.promote_pending_local_turn(&run_id, turn_id).is_err()
        {
            self.pending_local_turn_run = Some(run_id);
            return;
        }
        let Some(active) = self.active_run.clone() else {
            return;
        };
        match event {
            EngineEvent::MessageDelta { content, .. } => {
                self.upload_delta(&active.run_id, &active.turn_id, content);
            }
            EngineEvent::ToolCallStarted { id, name, .. } => {
                self.upload_envelope(
                    &active.run_id,
                    "item.started",
                    Some(&active.turn_id),
                    json!({ "tool": { "id": id, "name": name, "input": {} } }),
                );
            }
            EngineEvent::ToolCallComplete { id, result, .. } => {
                let (event_name, status) = if result.is_ok() {
                    ("item.completed", "completed")
                } else {
                    ("item.failed", "failed")
                };
                self.upload_envelope(
                    &active.run_id,
                    event_name,
                    Some(&active.turn_id),
                    json!({
                        "item": {
                            "id": id,
                            "kind": "tool_call",
                            "status": status,
                            "summary": "",
                            "detail": "",
                        }
                    }),
                );
            }
            EngineEvent::TurnStarted { turn_id, route, .. } => {
                self.active_run = Some(ActiveRelayRun {
                    run_id: active.run_id.clone(),
                    turn_id: turn_id.clone(),
                });
                self.upload_envelope(
                    &active.run_id,
                    "turn.started",
                    Some(turn_id),
                    json!({
                        "turn": {
                            "model": route.as_ref().map(|value| value.model.as_str()).unwrap_or(""),
                            "mode": "",
                        }
                    }),
                );
            }
            EngineEvent::TurnComplete { usage, status, .. } => {
                let status = match status {
                    TurnOutcomeStatus::Completed => "completed",
                    TurnOutcomeStatus::Interrupted => "interrupted",
                    TurnOutcomeStatus::Failed => "failed",
                };
                let terminal_durable = self.upload_envelope(
                    &active.run_id,
                    "turn.completed",
                    Some(&active.turn_id),
                    json!({ "turn": { "status": status, "usage": usage } }),
                );
                if terminal_durable && self.finish_classic_lease().is_ok() {
                    if self.resync_required.remove(&active.run_id) {
                        // Deltas were shed under pressure during this turn; the UI
                        // must now upload a bounded current snapshot so account
                        // truth is restored at the terminal boundary.
                        self.resync_ready.push(active.run_id.clone());
                    }
                    self.active_run = None;
                    self.pending_local_turn_run = None;
                }
            }
            EngineEvent::Error {
                envelope,
                recoverable,
            } if !recoverable => {
                self.fail_active_run(&envelope.code, &envelope.message);
            }
            _ => {}
        }
    }

    fn fail_active_run(&mut self, code: &str, error: &str) {
        let Some(active) = self.active_run.clone() else {
            return;
        };
        let message = bounded_remote_error_message(error);
        let item_id = projected_error_item_id(&active.run_id, &active.turn_id, code);
        let item_durable = self.upload_envelope(
            &active.run_id,
            "item.failed",
            Some(&active.turn_id),
            json!({
                "item": {
                    "id": item_id,
                    "kind": "error",
                    "status": "failed",
                    "summary": message,
                    "detail": message,
                }
            }),
        );
        let terminal_durable = self.upload_envelope(
            &active.run_id,
            "turn.completed",
            Some(&active.turn_id),
            json!({ "turn": { "status": "failed", "usage": {} } }),
        );
        if item_durable && terminal_durable && self.finish_classic_lease().is_ok() {
            self.active_run = None;
            self.pending_local_turn_run = None;
        }
    }

    fn upload_envelope(
        &mut self,
        run_id: &str,
        event: &str,
        turn_id: Option<&str>,
        payload: Value,
    ) -> bool {
        let prior_status = self.status;
        let prior_status_detail = self.status_detail.clone();
        let prior_ownership_blocked_until = self.ownership_blocked_until;
        let seq = self.next_runtime_seq(run_id);
        let envelope = runtime_envelope(
            seq,
            event,
            turn_id,
            chrono::Utc::now().to_rfc3339(),
            payload,
        );
        if self.queue_runtime_envelope(run_id, envelope) {
            return true;
        }
        if integrity_critical_event(event)
            && self
                .journal
                .as_ref()
                .is_some_and(|journal| journal.persist(&self.pending_runtime_events).is_ok())
        {
            // Atomic replacement can fail transiently (sharing/AV/power-loss
            // boundary). The exact envelope is still retained in memory;
            // make one immediate durable retry before returning to UI code
            // that may clear an approval or terminal lease. Only after that
            // retry succeeds may transport handoff occur.
            if self.status == Status::Failed
                && self.status_detail
                    == "Remote control could not durably journal its account event."
            {
                self.status = prior_status;
                self.status_detail = prior_status_detail;
                self.ownership_blocked_until = prior_ownership_blocked_until;
            }
            self.flush_pending_runtime_events(run_id);
            return true;
        }
        false
    }

    fn next_runtime_seq(&self, run_id: &str) -> u64 {
        let acknowledged = self.event_seq.get(run_id).copied().unwrap_or(0);
        let pending = self
            .pending_runtime_events
            .get(run_id)
            .and_then(|events| events.last_key_value().map(|(seq, _)| *seq))
            .unwrap_or(0);
        acknowledged.max(pending).saturating_add(1)
    }

    fn runtime_seq_floor(&self, run_id: &str) -> u64 {
        self.next_runtime_seq(run_id).saturating_sub(1)
    }

    fn queue_runtime_envelope(&mut self, run_id: &str, envelope: Value) -> bool {
        // Per-run sequence order must reach the transport in order, so any
        // deferred delta is handed off before a later envelope is queued.
        // Persist the new sequence before transport handoff. This gives the
        // ordinary mirrored Work path the same crash boundary as Runtime Chat:
        // a send may be replayed after an ambiguous crash, but an accepted
        // terminal/approval/failure row is never send-first and then lost.
        self.hand_off_deferred(run_id);
        self.queue_runtime_envelope_inner(run_id, envelope, false, true, false)
    }

    fn queue_runtime_chat_envelope(&mut self, run_id: &str, envelope: Value) -> bool {
        // Native Chat advances its own durable event cursor only after this
        // account-delivery journal is durable. Persist before handing the
        // envelope to transport; a resend after an ambiguous send is safe via
        // the stable source_event_id contract.
        self.hand_off_deferred(run_id);
        self.queue_runtime_envelope_inner(run_id, envelope, false, true, true)
    }

    fn queue_runtime_envelope_inner(
        &mut self,
        run_id: &str,
        envelope: Value,
        defer: bool,
        require_durable_journal: bool,
        rollback_on_journal_failure: bool,
    ) -> bool {
        if require_durable_journal && self.journal.is_none() {
            self.status_detail =
                "Remote control requires a durable account-delivery journal.".to_string();
            self.status = Status::Failed;
            self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
            return false;
        }
        let Some(seq) = runtime_envelope_seq(&envelope) else {
            self.status_detail = "a local runtime event had no valid sequence".to_string();
            return false;
        };
        let encoded_len = serde_json::to_vec(&envelope)
            .map(|body| body.len())
            .unwrap_or(usize::MAX);
        if encoded_len > MAX_RUNTIME_ENVELOPE_BYTES {
            self.status_detail = "a local runtime event exceeded the safe relay limit".to_string();
            return false;
        }
        let integrity = runtime_envelope_event(&envelope).is_some_and(integrity_critical_event);
        let already_pending = self
            .pending_runtime_events
            .get(run_id)
            .and_then(|events| events.get(&seq))
            .is_some();
        let previous_event_seq = self.event_seq.get(run_id).copied();
        if already_pending {
            let entry = self
                .pending_runtime_events
                .get(run_id)
                .and_then(|events| events.get(&seq))
                .expect("checked above");
            if entry.envelope != envelope {
                self.status_detail =
                    "a local runtime sequence changed before acknowledgement".to_string();
                return false;
            }
        } else {
            if !self.reserve_capacity(run_id, encoded_len, integrity) {
                return false;
            }
            self.pending_runtime_events
                .entry(run_id.to_string())
                .or_default()
                .insert(
                    seq,
                    PendingRuntimeEnvelope {
                        envelope: envelope.clone(),
                        encoded_len,
                        integrity,
                        handed_off: false,
                    },
                );
            self.pending_event_count += 1;
            self.pending_encoded_bytes = self.pending_encoded_bytes.saturating_add(encoded_len);
        }
        self.event_seq
            .entry(run_id.to_string())
            .and_modify(|cursor| *cursor = (*cursor).max(seq))
            .or_insert(seq);
        if require_durable_journal
            && self
                .journal
                .as_ref()
                .expect("checked above")
                .persist(&self.pending_runtime_events)
                .is_err()
        {
            if rollback_on_journal_failure && !already_pending {
                let remove_run = if let Some(events) = self.pending_runtime_events.get_mut(run_id) {
                    events.remove(&seq);
                    events.is_empty()
                } else {
                    false
                };
                if remove_run {
                    self.pending_runtime_events.remove(run_id);
                }
                self.pending_event_count = self.pending_event_count.saturating_sub(1);
                self.pending_encoded_bytes = self.pending_encoded_bytes.saturating_sub(encoded_len);
            }
            if rollback_on_journal_failure {
                match previous_event_seq {
                    Some(cursor) => {
                        self.event_seq.insert(run_id.to_string(), cursor);
                    }
                    None => {
                        self.event_seq.remove(run_id);
                    }
                }
            }
            self.status_detail =
                "Remote control could not durably journal its account event.".to_string();
            self.status = Status::Failed;
            self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
            return false;
        }
        if defer {
            self.deferred_delta.insert(run_id.to_string(), seq);
        } else {
            if let Some(entry) = self
                .pending_runtime_events
                .get_mut(run_id)
                .and_then(|events| events.get_mut(&seq))
            {
                entry.handed_off = true;
            }
            self.send_runtime_envelope(run_id, envelope);
            if !require_durable_journal {
                self.persist_journal();
            }
        }
        true
    }

    /// Bounded-journal admission control.
    ///
    /// Integrity-critical envelopes may use the full budget, ordinary deltas
    /// only the unreserved share. A shed delta marks the run for terminal-
    /// boundary resynchronization; a shed integrity envelope can never happen
    /// silently — the relay fails closed and local input stays locked through
    /// the server lease expiry.
    fn reserve_capacity(&mut self, run_id: &str, encoded_len: usize, integrity: bool) -> bool {
        let (event_budget, byte_budget) = if integrity {
            (MAX_JOURNAL_EVENTS, MAX_JOURNAL_ENCODED_BYTES)
        } else {
            (
                MAX_JOURNAL_EVENTS - JOURNAL_RESERVED_INTEGRITY_EVENTS,
                MAX_JOURNAL_ENCODED_BYTES - JOURNAL_RESERVED_INTEGRITY_BYTES,
            )
        };
        if self.pending_event_count < event_budget
            && self.pending_encoded_bytes.saturating_add(encoded_len) <= byte_budget
        {
            return true;
        }
        if integrity {
            self.status = Status::Failed;
            self.status_detail =
                "the runtime delivery buffer overflowed; waiting for the last server lease to expire safely"
                    .to_string();
            self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
        } else {
            self.resync_required.insert(run_id.to_string());
        }
        false
    }

    /// Streams a message delta, coalescing into the run's deferred envelope
    /// while that envelope has provably never been handed to the transport.
    fn upload_delta(&mut self, run_id: &str, turn_id: &str, content: &str) {
        if let Some(seq) = self.deferred_delta.get(run_id).copied() {
            if self.try_coalesce_delta(run_id, seq, turn_id, content) {
                return;
            }
            self.hand_off_deferred(run_id);
        }
        let seq = self.next_runtime_seq(run_id);
        let envelope = runtime_envelope(
            seq,
            "item.delta",
            Some(turn_id),
            chrono::Utc::now().to_rfc3339(),
            json!({ "kind": "agent_message", "delta": content }),
        );
        self.queue_runtime_envelope_inner(run_id, envelope, true, false, false);
    }

    fn try_coalesce_delta(&mut self, run_id: &str, seq: u64, turn_id: &str, content: &str) -> bool {
        let Some(entry) = self
            .pending_runtime_events
            .get_mut(run_id)
            .and_then(|events| events.get_mut(&seq))
        else {
            return false;
        };
        if entry.handed_off
            || entry.envelope.get("turn_id").and_then(Value::as_str) != Some(turn_id)
        {
            return false;
        }
        let Some(existing) = entry
            .envelope
            .pointer("/payload/delta")
            .and_then(Value::as_str)
        else {
            return false;
        };
        let merged = format!("{existing}{content}");
        let mut candidate = entry.envelope.clone();
        candidate["payload"]["delta"] = Value::String(merged);
        let encoded_len = serde_json::to_vec(&candidate)
            .map(|body| body.len())
            .unwrap_or(usize::MAX);
        if encoded_len > DELTA_COALESCE_BYTE_CAP {
            return false;
        }
        let old_len = entry.encoded_len;
        entry.envelope = candidate;
        entry.encoded_len = encoded_len;
        self.pending_encoded_bytes = self
            .pending_encoded_bytes
            .saturating_sub(old_len)
            .saturating_add(encoded_len);
        true
    }

    /// Hands the run's deferred delta to the transport. From this point the
    /// envelope may have reached the server and becomes immutable.
    fn hand_off_deferred(&mut self, run_id: &str) {
        let Some(seq) = self.deferred_delta.get(run_id).copied() else {
            return;
        };
        let Some(journal) = &self.journal else {
            self.status = Status::Failed;
            self.status_detail =
                "Remote control requires a durable account-delivery journal.".to_string();
            self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
            return;
        };
        if journal.persist(&self.pending_runtime_events).is_err() {
            self.status = Status::Failed;
            self.status_detail =
                "Remote control could not durably journal its account event.".to_string();
            self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
            return;
        }
        self.deferred_delta.remove(run_id);
        let Some(envelope) = self
            .pending_runtime_events
            .get_mut(run_id)
            .and_then(|events| events.get_mut(&seq))
            .map(|entry| {
                entry.handed_off = true;
                entry.envelope.clone()
            })
        else {
            return;
        };
        self.send_runtime_envelope(run_id, envelope);
    }

    fn hand_off_all_deferred(&mut self) {
        let runs: Vec<String> = self.deferred_delta.keys().cloned().collect();
        for run_id in runs {
            self.hand_off_deferred(&run_id);
        }
    }

    /// The UI drains this after each engine event batch and answers with
    /// `upload_resync_snapshot` for the returned run.
    pub fn take_pending_resync(&mut self) -> Option<String> {
        self.resync_ready.pop()
    }

    /// Uploads a bounded current-history snapshot to repair account truth
    /// after deltas were shed under pressure.
    pub fn upload_resync_snapshot(&mut self, run_id: &str, messages: &[Message]) {
        let seq = self.next_runtime_seq(run_id);
        let envelope = bounded_session_snapshot_envelope(seq, messages);
        self.queue_runtime_envelope(run_id, envelope);
    }

    fn persist_journal(&mut self) {
        let Some(journal) = &self.journal else {
            return;
        };
        if journal.persist(&self.pending_runtime_events).is_err() {
            // Crash durability is degraded, but nothing is lost silently: the
            // live relay keeps every envelope in memory and `/rc stop` still
            // requires the server-confirmed drain.
            self.status_detail =
                "the delivery journal could not be written; stop waits for server confirmation"
                    .to_string();
        }
    }

    /// Replaces the in-memory pending set from a verified journal load.
    fn reset_pending_from(&mut self, restored: HashMap<String, BTreeMap<u64, Value>>) {
        self.pending_runtime_events.clear();
        self.deferred_delta.clear();
        self.pending_event_count = 0;
        self.pending_encoded_bytes = 0;
        for (run_id, events) in restored {
            let mut pending = BTreeMap::new();
            for (seq, envelope) in events {
                let encoded_len = serde_json::to_vec(&envelope)
                    .map(|body| body.len())
                    .unwrap_or(usize::MAX);
                let integrity =
                    runtime_envelope_event(&envelope).is_some_and(integrity_critical_event);
                self.pending_event_count += 1;
                self.pending_encoded_bytes = self.pending_encoded_bytes.saturating_add(encoded_len);
                pending.insert(
                    seq,
                    PendingRuntimeEnvelope {
                        envelope,
                        encoded_len,
                        integrity,
                        handed_off: false,
                    },
                );
            }
            if let Some((top, _)) = pending.last_key_value() {
                let top = *top;
                self.event_seq
                    .entry(run_id.clone())
                    .and_modify(|cursor| *cursor = (*cursor).max(top))
                    .or_insert(top);
            }
            if !pending.is_empty() {
                self.pending_runtime_events.insert(run_id, pending);
            }
        }
    }

    fn send_runtime_envelope(&self, run_id: &str, envelope: Value) {
        let Some(tx) = &self.worker_tx else {
            return;
        };
        let _ = tx.send(WorkerCommand::Upload {
            run_id: run_id.to_string(),
            acknowledgements: Vec::new(),
            envelopes: vec![envelope],
        });
    }

    fn flush_pending_runtime_events(&mut self, run_id: &str) {
        // A reconnect resend covers everything, deferred deltas included;
        // after this every envelope may have reached the server.
        self.deferred_delta.remove(run_id);
        let mut to_send = Vec::new();
        if let Some(events) = self.pending_runtime_events.get_mut(run_id) {
            for entry in events.values_mut() {
                entry.handed_off = true;
                to_send.push(entry.envelope.clone());
            }
        }
        if to_send.is_empty() {
            return;
        }
        for envelope in to_send {
            self.send_runtime_envelope(run_id, envelope);
        }
        self.persist_journal();
    }

    fn flush_all_pending(&mut self) {
        let runs: Vec<String> = self.pending_runtime_events.keys().cloned().collect();
        for run_id in runs {
            self.flush_pending_runtime_events(&run_id);
        }
    }

    fn reconcile_runtime_cursor(&mut self, run_id: &str, cursor: u64) {
        if cursor > JS_MAX_SAFE_INTEGER {
            self.status_detail = "the server returned an unsafe runtime cursor".to_string();
            return;
        }
        let covered_classic_terminal = self
            .journal
            .as_ref()
            .and_then(RuntimeEventJournal::classic_lease)
            .filter(|lease| lease.run_id == run_id)
            .is_some_and(|lease| {
                let turn_id = lease
                    .turn_id
                    .clone()
                    .unwrap_or_else(|| classic_recovery_turn_id(&lease.run_id, &lease.lease_id));
                self.pending_runtime_events
                    .get(run_id)
                    .is_some_and(|events| {
                        events.range(..=cursor).any(|(seq, entry)| {
                            *seq > lease.seq_floor
                                && runtime_envelope_event(&entry.envelope) == Some("turn.completed")
                                && entry.envelope.get("turn_id").and_then(Value::as_str)
                                    == Some(turn_id.as_str())
                        })
                    })
            });
        let classic_state_result = if covered_classic_terminal {
            self.journal
                .as_ref()
                .expect("lease came from this journal")
                .set_classic_lease(None, &self.pending_runtime_events)
        } else if let Some(journal) = &self.journal {
            journal.advance_classic_seq_floor(run_id, cursor)
        } else {
            Ok(())
        };
        if let Err(error) = classic_state_result {
            // Never compact the only durable evidence of the run's sequence
            // history unless the canonical active lease first records the
            // acknowledged floor. A reconnect can retry this exact cursor.
            self.status = Status::Failed;
            self.status_detail = error;
            self.ownership_blocked_until = Some(Instant::now() + OWNERSHIP_LOCK_AFTER_FAILURE);
            return;
        }
        if covered_classic_terminal {
            self.active_run = None;
            self.pending_local_turn_run = None;
        }
        let mut empty = false;
        let mut retired_any = false;
        if let Some(events) = self.pending_runtime_events.get_mut(run_id) {
            let retired: Vec<u64> = events.range(..=cursor).map(|(seq, _)| *seq).collect();
            for seq in retired {
                if let Some(entry) = events.remove(&seq) {
                    retired_any = true;
                    self.pending_event_count = self.pending_event_count.saturating_sub(1);
                    self.pending_encoded_bytes =
                        self.pending_encoded_bytes.saturating_sub(entry.encoded_len);
                }
            }
            empty = events.is_empty();
        }
        if empty {
            self.pending_runtime_events.remove(run_id);
        }
        if self
            .deferred_delta
            .get(run_id)
            .is_some_and(|seq| *seq <= cursor)
        {
            self.deferred_delta.remove(run_id);
        }
        self.event_seq
            .entry(run_id.to_string())
            .and_modify(|known| *known = (*known).max(cursor))
            .or_insert(cursor);
        if retired_any {
            // Compact the acknowledged prefix out of the journal promptly.
            self.persist_journal();
        }
        // Runtime Chat retains the exclusive provider-request lease through
        // local terminal projection *and* this server-confirmed cursor. Only
        // then may interactive/autonomous provider work resume for the
        // attached CWC run. Catalog/approval integrity rows conservatively
        // keep the same gate closed until their own acknowledgement as well.
        if !self.has_unacknowledged_integrity_events()
            && let Some(host) = &self.runtime_chat
        {
            host.release_inference_ownership_if_settled();
        }
    }
}

impl Drop for RemoteControlController {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

impl RemoteCommand {
    fn kind(&self) -> &'static str {
        match self {
            Self::Prompt { .. } | Self::RuntimeChatPrompt(_) => "prompt.request",
            Self::Approval { .. } => "approval.decision",
            Self::Control { .. } => "run.control",
        }
    }

    fn turn_id(&self) -> Option<&str> {
        match self {
            Self::Prompt { turn_id, .. } => Some(turn_id),
            Self::RuntimeChatPrompt(prompt) => Some(&prompt.turn_id),
            Self::Control { turn_id, .. } => turn_id.as_deref(),
            Self::Approval { .. } => None,
        }
    }

    fn is_runtime_chat(&self) -> bool {
        matches!(
            self,
            Self::RuntimeChatPrompt(_)
                | Self::Control {
                    runtime_chat: Some(_),
                    ..
                }
        )
    }
}

/// Opaque identity for the enrolled folder. It is a hash of the workspace
/// path only: every session opened in the same folder shares one target, so
/// the control plane sees one grant per folder rather than one per session
/// (the session itself travels separately as `sessionRef`).
pub fn target_ref(workspace: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale-remote-target:v2\0");
    hasher.update(workspace.to_string_lossy().as_bytes());
    format!("target_{}", &bytes_to_hex(&hasher.finalize())[..32])
}

/// Status-bar banner shown while the web owns this session. When the control
/// plane advertised a session link the banner leads with it; otherwise it
/// falls back to the opaque account and runner receipts.
pub fn remote_control_banner(account_ref: &str, runner_id: &str, run_url: Option<&str>) -> String {
    match run_url {
        Some(url) => format!("WEB MIRROR · {url} · /rc stop"),
        None => format!("WEB MIRROR · account {account_ref} · runner {runner_id} · /rc stop"),
    }
}

/// Transcript note announcing where the live session can be followed.
pub fn remote_control_link_notice(run_url: &str) -> String {
    format!(
        "Remote control is live at {run_url} — run /rc open to open it in your browser, or /rc link to print it. Both surfaces stay usable; one turn runs at a time."
    )
}

fn runtime_envelope(
    seq: u64,
    event: &str,
    turn_id: Option<&str>,
    timestamp: String,
    payload: Value,
) -> Value {
    json!({
        "schema_version": 1,
        "seq": seq,
        "event": event,
        "kind": event,
        "turn_id": turn_id,
        "timestamp": timestamp,
        "payload": payload,
    })
}

fn runtime_chat_envelope(
    seq: u64,
    event: &str,
    thread_id: Option<&str>,
    turn_id: Option<&str>,
    source_event_id: Option<&str>,
    timestamp: String,
    payload: Value,
) -> Value {
    let mut envelope = json!({
        "schema_version": 2,
        "seq": seq,
        "event": event,
        "kind": event,
        "timestamp": timestamp,
        "payload": payload,
    });
    if let Some(thread_id) = thread_id {
        envelope["thread_id"] = json!(thread_id);
    }
    if let Some(turn_id) = turn_id {
        envelope["turn_id"] = json!(turn_id);
    }
    if let Some(source_event_id) = source_event_id {
        envelope["source_event_id"] = json!(source_event_id);
    }
    envelope
}

fn runtime_chat_catalog_source_event_id(run_id: &str, payload_fingerprint: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale.runtime-chat-catalog.v1\0");
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(payload_fingerprint.as_bytes());
    format!("catalog_{}", bytes_to_hex(&hasher.finalize()))
}

fn runtime_envelope_seq(envelope: &Value) -> Option<u64> {
    envelope
        .get("seq")
        .and_then(Value::as_u64)
        .filter(|seq| (1..=JS_MAX_SAFE_INTEGER).contains(seq))
}

fn bounded_session_snapshot_envelope(seq: u64, messages: &[Message]) -> Value {
    let timestamp = chrono::Utc::now().to_rfc3339();
    let candidates = messages
        .iter()
        .rev()
        .filter_map(project_session_message)
        .take(MAX_SNAPSHOT_MESSAGES)
        .collect::<Vec<_>>();
    let mut kept = Vec::<Value>::new();
    for (role, text) in candidates {
        let full = json!({ "role": role, "text": text });
        kept.insert(0, full);
        if snapshot_envelope_len(seq, &timestamp, &kept) <= SNAPSHOT_ENVELOPE_BYTE_BUDGET {
            continue;
        }
        kept.remove(0);
        let max_chars = text.chars().count();
        let mut low = 0usize;
        let mut high = max_chars;
        while low < high {
            let mid = low + (high - low).div_ceil(2);
            let prefix = unicode_prefix(&text, mid);
            kept.insert(0, json!({ "role": role, "text": prefix }));
            let fits =
                snapshot_envelope_len(seq, &timestamp, &kept) <= SNAPSHOT_ENVELOPE_BYTE_BUDGET;
            kept.remove(0);
            if fits {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        if low >= MIN_TRUNCATED_MESSAGE_CHARS || (kept.is_empty() && low > 0) {
            kept.insert(
                0,
                json!({ "role": role, "text": unicode_prefix(&text, low) }),
            );
        }
        break;
    }
    let envelope = runtime_envelope(
        seq,
        "session.snapshot",
        None,
        timestamp,
        json!({ "messages": kept }),
    );
    debug_assert!(
        serde_json::to_vec(&envelope).is_ok_and(|body| body.len() <= SNAPSHOT_ENVELOPE_BYTE_BUDGET)
    );
    envelope
}

fn project_session_message(message: &Message) -> Option<(String, String)> {
    let role = match message.role.as_str() {
        "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return None;
    }
    Some((
        role.to_string(),
        text.chars()
            .take(MAX_SNAPSHOT_MESSAGE_CHARS)
            .collect::<String>(),
    ))
}

fn snapshot_envelope_len(seq: u64, timestamp: &str, messages: &[Value]) -> usize {
    serde_json::to_vec(&runtime_envelope(
        seq,
        "session.snapshot",
        None,
        timestamp.to_string(),
        json!({ "messages": messages }),
    ))
    .map(|body| body.len())
    .unwrap_or(usize::MAX)
}

fn unicode_prefix(value: &str, chars: usize) -> String {
    value.chars().take(chars).collect()
}

fn projected_approval_id(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"local-runtime:approval\0");
    hasher.update(raw.as_bytes());
    format!("local_approval_{}", &bytes_to_hex(&hasher.finalize())[..24])
}

/// Whether this view is the approval card for exactly `gate` (the projected
/// approval id). Used by the web mirror to dismiss the matching card — never
/// an unrelated approval that happens to be on top.
pub(crate) fn view_is_approval_for_gate(
    view: &dyn crate::tui::views::ModalView,
    gate: &str,
) -> bool {
    view.kind() == crate::tui::views::ModalKind::Approval
        && view
            .approval_request_id()
            .is_some_and(|tool_id| projected_approval_id(tool_id) == gate)
}

fn projected_error_item_id(run_id: &str, turn_id: &str, code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"local-runtime:error\0");
    hasher.update(run_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(turn_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(code.as_bytes());
    format!("local_item_{}", &bytes_to_hex(&hasher.finalize())[..24])
}

fn bounded_remote_error_message(error: &str) -> String {
    let without_nul = error.replace('\0', " ");
    let redacted = codewhale_config::persistence::redact_secrets(&without_nul);
    let message = redacted.trim();
    if message.is_empty() {
        return "The local model turn failed.".to_string();
    }
    crate::utils::truncate_with_ellipsis(message, MAX_REMOTE_ERROR_MESSAGE_BYTES, "…")
}

fn command_fingerprint(command: &RemoteCommand) -> String {
    let canonical = format!("{command:?}");
    bytes_to_hex(&Sha256::digest(canonical.as_bytes()))
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

async fn relay_worker(
    start: RemoteStart,
    mut runtime_chat: Option<RuntimeChatRelayHost>,
    mut worker_rx: mpsc::UnboundedReceiver<WorkerCommand>,
    event_tx: mpsc::UnboundedSender<RemoteEvent>,
    phase: &mut RelayPhase,
) -> Result<(), String> {
    let base = runner_control_plane_base()?;
    let client = crate::tls::reqwest_client_builder()
        .https_only(!cfg!(debug_assertions))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| "Remote control could not initialize secure networking.".to_string())?;

    let saved_enrollment = load_persisted_enrollment()?;
    // The device id is stable across enrollments: it is what lets the control
    // plane fold every folder enrolled from this terminal into one computer.
    let device_id = stable_device_id(
        saved_enrollment
            .as_ref()
            .map(|saved| saved.device_id.as_str()),
    )?;
    let mut enrollment = match saved_enrollment {
        Some(saved) if saved.matches(&start, &base) => {
            match refresh_enrollment(&client, saved).await {
                Ok(enrollment) => enrollment,
                Err(error) if error == "runner_enrollment_revoked" => {
                    delete_persisted_enrollment();
                    enroll_device(&client, &base, &start, &device_id, &event_tx).await?
                }
                Err(error) => return Err(error),
            }
        }
        Some(_) => {
            delete_persisted_enrollment();
            enroll_device(&client, &base, &start, &device_id, &event_tx).await?
        }
        None => enroll_device(&client, &base, &start, &device_id, &event_tx).await?,
    };

    let connection = connect_runner(&client, &enrollment, &start).await?;
    // connect_runner answered with a server-confirmed attachment: a lease
    // exists from here on, and every later failure is a lost-after-lease
    // disconnect that must stay fail-closed.
    *phase = RelayPhase::Leased;
    let mut runner_id = connection.runner_id.clone();
    event_tx
        .send(RemoteEvent::Connected {
            account_ref: enrollment.persisted.account_ref.clone(),
            runner_id: runner_id.clone(),
            target_ref: start.target_ref.clone(),
            attachment: connection.attachment,
            links: connection.links,
        })
        .map_err(|_| "The terminal remote-control owner stopped.".to_string())?;
    let mut last_heartbeat = Instant::now() - HEARTBEAT_INTERVAL;
    let mut command_cursor: HashMap<String, u64> = HashMap::new();
    let mut delivered: HashMap<(String, u64), String> = HashMap::new();
    let mut runtime_outbox = RuntimeTransportOutbox::default();
    let mut runtime_upload_tick = tokio::time::interval(RUNTIME_UPLOAD_RETRY_INTERVAL);
    runtime_upload_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut runtime_chat_tick = tokio::time::interval(RUNTIME_UPLOAD_RETRY_INTERVAL);
    runtime_chat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // One deadline across loop iterations. Reconstructing `sleep(SYNC_INTERVAL)`
    // lets the 250ms Runtime Chat tick reset poll/heartbeat/token refresh.
    let mut sync_tick =
        tokio::time::interval_at(tokio::time::Instant::now() + SYNC_INTERVAL, SYNC_INTERVAL);
    sync_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut runtime_retry_delay = RUNTIME_UPLOAD_RETRY_INTERVAL;
    let mut runtime_retry_not_before = Instant::now();
    // One credential refresh per rejection streak (see
    // `RUNTIME_REFRESHED_CREDENTIAL_REFUSED`).
    let mut runtime_refreshed_since_accept = false;

    loop {
        tokio::select! {
            command = worker_rx.recv() => {
                match command {
                    Some(WorkerCommand::Upload { run_id, acknowledgements, envelopes }) => {
                        if !envelopes.is_empty() {
                            if !acknowledgements.is_empty() || envelopes.len() != 1 {
                                return Err("The local runtime queued an invalid event batch.".to_string());
                            }
                            runtime_outbox.enqueue(&run_id, envelopes[0].clone())?;
                            continue;
                        }
                        let body = Some(json!({ "acknowledgements": acknowledgements, "envelopes": envelopes }));
                        let result = runner_request(
                            &client,
                            &enrollment,
                            Method::POST,
                            &["api", "local-runners", &runner_id, "runs", &run_id, "events"],
                            &[],
                            body.clone(),
                        )
                        .await;
                        if let Err(err) = result {
                            if err == "runner_access_token_expired" {
                                refresh_enrollment_and_reconnect(
                                    &client,
                                    &mut enrollment,
                                    &mut runner_id,
                                    &start,
                                    &event_tx,
                                )
                                .await?;
                                runner_request(
                                    &client,
                                    &enrollment,
                                    Method::POST,
                                    &["api", "local-runners", &runner_id, "runs", &run_id, "events"],
                                    &[],
                                    body,
                                )
                                .await?;
                            } else {
                                return Err(err);
                            }
                        }
                    }
                    Some(WorkerCommand::ReleaseRuntimeChatHost) => {
                        runtime_chat.take();
                        event_tx
                            .send(RemoteEvent::RuntimeChatHostReleased)
                            .map_err(|_| "The terminal remote-control owner stopped.".to_string())?;
                    }
                    Some(WorkerCommand::InstallRuntimeChatHost(host)) => {
                        if runtime_chat.is_some() {
                            return Err(
                                "Runtime Chat received an invalid provider-refresh install."
                                    .to_string(),
                            );
                        }
                        runtime_chat = Some(host);
                    }
                    Some(WorkerCommand::Stop) | None => {
                        // Do not return local input until the control plane has
                        // durably released this lease. Every queued runtime
                        // envelope must first drain behind the server-confirmed
                        // cursor; only then may the offline heartbeat be
                        // posted. If either cannot be confirmed, this worker
                        // errors out and the UI keeps ownership locked through
                        // the server-side lease expiry instead.
                        drain_runtime_outbox_for_stop(
                            &client,
                            &mut enrollment,
                            &mut runner_id,
                            &start,
                            &event_tx,
                            &mut runtime_outbox,
                            Instant::now() + STOP_DRAIN_DEADLINE,
                        )
                        .await?;
                        let hb = post_heartbeat(&client, &enrollment, &runner_id, &start, "offline").await;
                        if let Err(err) = hb {
                            if err == "runner_access_token_expired" {
                                refresh_enrollment_and_reconnect(
                                    &client,
                                    &mut enrollment,
                                    &mut runner_id,
                                    &start,
                                    &event_tx,
                                )
                                .await?;
                                post_heartbeat(&client, &enrollment, &runner_id, &start, "offline").await?;
                            } else {
                                return Err(err);
                            }
                        }
                        let _ = event_tx.send(RemoteEvent::Stopped);
                        return Ok(());
                    }
                }
            }
            _ = runtime_upload_tick.tick(), if !runtime_outbox.events.is_empty() => {
                if Instant::now() < runtime_retry_not_before {
                    continue;
                }
                match runtime_outbox
                    .try_flush_one(&client, &enrollment, &runner_id)
                    .await?
                {
                    RuntimeFlushOutcome::Idle => {
                        runtime_retry_delay = RUNTIME_UPLOAD_RETRY_INTERVAL;
                        runtime_retry_not_before = Instant::now();
                    }
                    RuntimeFlushOutcome::Retryable => {
                        runtime_retry_not_before = Instant::now() + runtime_retry_delay;
                        runtime_retry_delay = runtime_retry_delay
                            .saturating_mul(2)
                            .min(RUNTIME_UPLOAD_MAX_BACKOFF);
                    }
                    RuntimeFlushOutcome::Accepted { run_id, cursor } => {
                        runtime_retry_delay = RUNTIME_UPLOAD_RETRY_INTERVAL;
                        runtime_retry_not_before = Instant::now();
                        runtime_refreshed_since_accept = false;
                        event_tx
                            .send(RemoteEvent::RuntimeCursor { run_id, cursor })
                            .map_err(|_| "The terminal remote-control owner stopped.".to_string())?;
                    }
                    RuntimeFlushOutcome::AccessTokenExpired => {
                        if std::mem::replace(&mut runtime_refreshed_since_accept, true) {
                            return Err(RUNTIME_REFRESHED_CREDENTIAL_REFUSED.to_string());
                        }
                        refresh_enrollment_and_reconnect(
                            &client,
                            &mut enrollment,
                            &mut runner_id,
                            &start,
                            &event_tx,
                        )
                        .await?;
                        runtime_retry_delay = RUNTIME_UPLOAD_RETRY_INTERVAL;
                        runtime_retry_not_before = Instant::now();
                    }
                }
            }
            _ = runtime_chat_tick.tick(), if runtime_chat.is_some() => {
                let host = runtime_chat.as_ref().expect("select guard requires Runtime Chat host");
                for projection in host.pending_projections().await? {
                    event_tx
                        .send(RemoteEvent::RuntimeChatProjection(projection))
                        .map_err(|_| "The terminal remote-control owner stopped.".to_string())?;
                }
            }
            _ = sync_tick.tick() => {
                if enrollment_needs_refresh(&enrollment) {
                    // Proactive refresh before expiry; reconnect to keep runner lease valid.
                    match refresh_enrollment(&client, enrollment.persisted.clone()).await {
                        Ok(new_enrollment) => {
                            install_reconnected_enrollment(
                                &mut enrollment,
                                new_enrollment,
                                &start,
                            )?;
                            reconnect_runner(
                                &client,
                                &enrollment,
                                &mut runner_id,
                                &start,
                                &event_tx,
                            )
                            .await?;
                        }
                        Err(err) if err == "runner_enrollment_revoked" => {
                            delete_persisted_enrollment();
                            let device_id = enrollment.persisted.device_id.clone();
                            let candidate =
                                enroll_device(&client, &base, &start, &device_id, &event_tx).await?;
                            if let Err(error) = install_reconnected_enrollment(
                                &mut enrollment,
                                candidate,
                                &start,
                            ) {
                                // enroll_device persists its candidate. Never retain a
                                // credential for authority the live worker rejected.
                                delete_persisted_enrollment();
                                return Err(error);
                            }
                            reconnect_runner(
                                &client,
                                &enrollment,
                                &mut runner_id,
                                &start,
                                &event_tx,
                            )
                            .await?;
                        }
                        Err(err) => return Err(err),
                    }
                }
                if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                    let hb = post_heartbeat(&client, &enrollment, &runner_id, &start, "active").await;
                    if let Err(err) = hb {
                        if err == "runner_access_token_expired" {
                            refresh_enrollment_and_reconnect(
                                &client,
                                &mut enrollment,
                                &mut runner_id,
                                &start,
                                &event_tx,
                            )
                            .await?;
                            post_heartbeat(&client, &enrollment, &runner_id, &start, "active").await?;
                        } else {
                            return Err(err);
                        }
                    }
                    last_heartbeat = Instant::now();
                }
                let runs = match list_runs(&client, &enrollment, &runner_id).await {
                    Ok(v) => v,
                    Err(err) if err == "runner_access_token_expired" => {
                        refresh_enrollment_and_reconnect(
                            &client,
                            &mut enrollment,
                            &mut runner_id,
                            &start,
                            &event_tx,
                        )
                        .await?;
                        list_runs(&client, &enrollment, &runner_id).await?
                    }
                    Err(err) => return Err(err),
                };
                for run_id in runs {
                    let since = command_cursor.get(&run_id).copied().unwrap_or(0);
                    let listed_commands = match list_commands(
                        &client,
                        &enrollment,
                        &runner_id,
                        &run_id,
                        since,
                    )
                    .await
                    {
                        Ok(v) => v,
                        Err(err) if err == "runner_access_token_expired" => {
                            refresh_enrollment_and_reconnect(
                                &client,
                                &mut enrollment,
                                &mut runner_id,
                                &start,
                                &event_tx,
                            )
                            .await?;
                            list_commands(&client, &enrollment, &runner_id, &run_id, since).await?
                        }
                        Err(err) => return Err(err),
                    };
                    for listed in listed_commands {
                        let seq = listed.seq;
                        if !listed.ack_status.is_empty() {
                            if listed.ack_status == "accepted" {
                                let rr = recover_run(
                                    &client,
                                    &enrollment,
                                    &runner_id,
                                    &run_id,
                                    "accepted command has no terminal acknowledgement after runner restart",
                                )
                                .await;
                                if let Err(err) = rr {
                                    if err == "runner_access_token_expired" {
                                        refresh_enrollment_and_reconnect(
                                            &client,
                                            &mut enrollment,
                                            &mut runner_id,
                                            &start,
                                            &event_tx,
                                        )
                                        .await?;
                                        recover_run(
                                            &client,
                                            &enrollment,
                                            &runner_id,
                                            &run_id,
                                            "accepted command has no terminal acknowledgement after runner restart",
                                        )
                                        .await?;
                                    } else {
                                        return Err(err);
                                    }
                                }
                            }
                            command_cursor.insert(run_id.clone(), seq);
                            continue;
                        }
                        let command = parse_remote_command(&listed.command, &run_id)?;
                        let fingerprint = command_fingerprint(&command);
                        let key = (run_id.clone(), seq);
                        if let Some(existing) = delivered.get(&key) {
                            if existing != &fingerprint {
                                return Err("The control plane replayed a changed command sequence.".to_string());
                            }
                        } else {
                            delivered.insert(key, fingerprint);
                            // Runtime Chat reports `applied` only after the
                            // durable native thread manager has accepted the
                            // exact operation key. Leaving the command
                            // unacknowledged until then makes crash replay
                            // naturally re-enter the idempotent native path.
                            if !command.is_runtime_chat() {
                                let up = upload_command_accepted(
                                    &client,
                                    &enrollment,
                                    &runner_id,
                                    &run_id,
                                    seq,
                                    &command,
                                )
                                .await;
                                if let Err(err) = up {
                                    if err == "runner_access_token_expired" {
                                        refresh_enrollment_and_reconnect(
                                            &client,
                                            &mut enrollment,
                                            &mut runner_id,
                                            &start,
                                            &event_tx,
                                        )
                                        .await?;
                                        upload_command_accepted(
                                            &client,
                                            &enrollment,
                                            &runner_id,
                                            &run_id,
                                            seq,
                                            &command,
                                        )
                                        .await?;
                                    } else {
                                        return Err(err);
                                    }
                                }
                            }
                            event_tx.send(RemoteEvent::Command {
                                run_id: run_id.clone(),
                                seq,
                                command,
                            }).map_err(|_| "The terminal remote-control owner stopped.".to_string())?;
                        }
                        command_cursor.insert(run_id.clone(), seq.max(since));
                    }
                }
            }
        }
    }
}

impl RuntimeTransportOutbox {
    fn enqueue(&mut self, run_id: &str, envelope: Value) -> Result<(), String> {
        if !valid_opaque_ref(run_id) {
            return Err("The local runtime queued an invalid run id.".to_string());
        }
        let seq = runtime_envelope_seq(&envelope)
            .ok_or_else(|| "The local runtime queued an invalid event sequence.".to_string())?;
        let encoded = serde_json::to_vec(&envelope)
            .map_err(|_| "The local runtime could not encode an event.".to_string())?;
        if encoded.len() > MAX_RUNTIME_ENVELOPE_BYTES {
            return Err("The local runtime queued an oversized event.".to_string());
        }
        let key = (run_id.to_string(), seq);
        if let Some(existing) = self.events.get(&key) {
            if existing != &envelope {
                return Err(
                    "The local runtime changed an unacknowledged event sequence.".to_string(),
                );
            }
            return Ok(());
        }
        self.events.insert(key, envelope);
        Ok(())
    }

    async fn try_flush_one(
        &mut self,
        client: &Client,
        enrollment: &LiveEnrollment,
        runner_id: &str,
    ) -> Result<RuntimeFlushOutcome, String> {
        let Some(((run_id, seq), envelope)) = self
            .events
            .first_key_value()
            .map(|(key, value)| (key.clone(), value.clone()))
        else {
            return Ok(RuntimeFlushOutcome::Idle);
        };
        match post_runtime_event(client, enrollment, runner_id, &run_id, seq, &envelope).await? {
            RuntimePostOutcome::Retryable => Ok(RuntimeFlushOutcome::Retryable),
            RuntimePostOutcome::AccessTokenExpired => Ok(RuntimeFlushOutcome::AccessTokenExpired),
            RuntimePostOutcome::Accepted(cursor) => {
                self.events.retain(|(pending_run, pending_seq), _| {
                    pending_run != &run_id || *pending_seq > cursor
                });
                Ok(RuntimeFlushOutcome::Accepted { run_id, cursor })
            }
        }
    }
}

/// A runtime event refused (401/403) again right after a successful
/// credential refresh is a terminal rejection, not an expired token: a
/// revoked runner or a run this runner may not write. Refreshing again would
/// loop at the upload retry interval forever.
const RUNTIME_REFRESHED_CREDENTIAL_REFUSED: &str = "The remote-control server refused runtime events again with a freshly refreshed credential; stopping instead of retrying.";

/// Flushes every queued runtime envelope through the server-confirmed cursor
/// before a stop may be acknowledged. Emits `RuntimeCursor` events so the
/// controller compacts its journal as acknowledgements land. Failing to drain
/// by `deadline` is a hard error: the stop is *not* confirmed and the caller
/// must leave ownership locked.
#[allow(clippy::too_many_arguments)]
async fn drain_runtime_outbox_for_stop(
    client: &Client,
    enrollment: &mut LiveEnrollment,
    runner_id: &mut String,
    start: &RemoteStart,
    event_tx: &mpsc::UnboundedSender<RemoteEvent>,
    outbox: &mut RuntimeTransportOutbox,
    deadline: Instant,
) -> Result<(), String> {
    let mut delay = RUNTIME_UPLOAD_RETRY_INTERVAL;
    let mut refreshed_since_accept = false;
    while !outbox.events.is_empty() {
        if Instant::now() >= deadline {
            return Err(
                "queued runtime events were not server-acknowledged in time; the stop was not confirmed"
                    .to_string(),
            );
        }
        match outbox.try_flush_one(client, enrollment, runner_id).await? {
            RuntimeFlushOutcome::Idle => break,
            RuntimeFlushOutcome::Accepted { run_id, cursor } => {
                delay = RUNTIME_UPLOAD_RETRY_INTERVAL;
                refreshed_since_accept = false;
                let _ = event_tx.send(RemoteEvent::RuntimeCursor { run_id, cursor });
            }
            RuntimeFlushOutcome::Retryable => {
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2).min(RUNTIME_UPLOAD_MAX_BACKOFF);
            }
            RuntimeFlushOutcome::AccessTokenExpired => {
                if std::mem::replace(&mut refreshed_since_accept, true) {
                    return Err(RUNTIME_REFRESHED_CREDENTIAL_REFUSED.to_string());
                }
                refresh_enrollment_and_reconnect(client, enrollment, runner_id, start, event_tx)
                    .await?;
            }
        }
    }
    Ok(())
}

async fn post_runtime_event(
    client: &Client,
    enrollment: &LiveEnrollment,
    runner_id: &str,
    run_id: &str,
    seq: u64,
    envelope: &Value,
) -> Result<RuntimePostOutcome, String> {
    let url = control_plane_url(
        &enrollment.persisted.control_plane_base,
        &["api", "local-runners", runner_id, "runs", run_id, "events"],
        &[],
    )?;
    let response = match client
        .post(url)
        .bearer_auth(&enrollment.access_token)
        .json(&json!({
            "acknowledgements": [],
            "envelopes": [envelope],
        }))
        .send()
        .await
    {
        Ok(response) => response,
        Err(_) => return Ok(RuntimePostOutcome::Retryable),
    };
    let status = response.status();
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Ok(RuntimePostOutcome::AccessTokenExpired);
    }
    if matches!(
        status,
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
    {
        return Ok(RuntimePostOutcome::Retryable);
    }
    if !status.is_success() {
        return Err(format!(
            "The remote-control server rejected runtime event {seq} ({status})."
        ));
    }
    let value = match read_bounded_json(response).await {
        Ok(value) => value,
        Err(_) => return Ok(RuntimePostOutcome::Retryable),
    };
    let Some(cursor) = value
        .get("cursor")
        .and_then(Value::as_u64)
        .filter(|cursor| *cursor >= seq && *cursor <= JS_MAX_SAFE_INTEGER)
    else {
        // A success without a durable cursor is indistinguishable from a lost
        // response. Retain and retry the exact same event body.
        return Ok(RuntimePostOutcome::Retryable);
    };
    Ok(RuntimePostOutcome::Accepted(cursor))
}

impl PersistedEnrollment {
    fn matches(&self, start: &RemoteStart, base: &str) -> bool {
        self.schema_version == 1
            && self.control_plane_base == base
            && self.target_ref == start.target_ref
            && self.runtime_version == start.runtime_version
            && self.runtime_commit == start.runtime_commit
            && valid_opaque_ref(&self.runner_enrollment_id)
            && valid_opaque_ref(&self.account_ref)
            && valid_opaque_ref(&self.device_id)
            && valid_opaque_ref(&self.target_grant_ref)
            && valid_secret(&self.bootstrap_secret)
    }
}

async fn enroll_device(
    client: &Client,
    base: &str,
    start: &RemoteStart,
    device_id: &str,
    event_tx: &mpsc::UnboundedSender<RemoteEvent>,
) -> Result<LiveEnrollment, String> {
    let value = public_request(
        client,
        Method::POST,
        control_plane_url(base, &["api", "runner", "device", "start"], &[])?,
        json!({
            "deviceId": device_id,
            "deviceLabel": "Codewhale terminal",
            "targetRef": start.target_ref,
            "targetLabel": start.workspace_label,
            "runtimeVersion": start.runtime_version,
            "runtimeCommit": start.runtime_commit,
            "capabilities": CAPABILITIES,
        }),
    )
    .await?;
    let device_code = secret_field(&value, "deviceCode")?;
    let user_code = string_field(&value, "userCode")?;
    let verification_uri = string_field(&value, "verificationUriComplete")?;
    let interval = value
        .get("interval")
        .and_then(Value::as_u64)
        .filter(|value| (1..=30).contains(value))
        .ok_or_else(|| {
            "Codewhale returned an invalid device authorization interval.".to_string()
        })?;
    let expires_in = value
        .get("expiresIn")
        .and_then(Value::as_u64)
        .filter(|value| (60..=1800).contains(value))
        .ok_or_else(|| "Codewhale returned an invalid device authorization expiry.".to_string())?;
    validate_authorization_url(&verification_uri, &user_code)?;
    let _ = event_tx.send(RemoteEvent::Notice(format!(
        "Authorize this terminal at {verification_uri} (code {user_code})."
    )));
    let _ = webbrowser::open(&verification_uri);
    let deadline = Instant::now() + Duration::from_secs(expires_in);
    loop {
        if Instant::now() >= deadline {
            return Err("Remote-control authorization expired; run /rc again.".to_string());
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let response = client
            .post(control_plane_url(
                base,
                &["api", "runner", "device", "token"],
                &[],
            )?)
            .json(&json!({ "deviceCode": device_code }))
            .send()
            .await
            .map_err(|_| "Remote-control authorization could not reach Codewhale.".to_string())?;
        if response.status() == StatusCode::ACCEPTED {
            continue;
        }
        if !response.status().is_success() {
            return Err("Remote-control authorization was rejected.".to_string());
        }
        let exchange = read_bounded_json(response).await?;
        let enrollment = enrollment_from_exchange(exchange, base, device_id, start)?;
        save_persisted_enrollment(&enrollment.persisted)?;
        return Ok(enrollment);
    }
}

fn enrollment_from_exchange(
    value: Value,
    base: &str,
    device_id: &str,
    start: &RemoteStart,
) -> Result<LiveEnrollment, String> {
    if value.get("status").and_then(Value::as_str) != Some("approved") {
        return Err("Codewhale returned an invalid runner credential.".to_string());
    }
    let record = value
        .get("enrollment")
        .filter(|value| value.is_object())
        .ok_or_else(|| "Codewhale returned an invalid runner credential.".to_string())?;
    let enrollment_id = opaque_field(record, "id")?;
    let account_ref = opaque_field(record, "userId")?;
    let returned_device = opaque_field(record, "deviceId")?;
    if returned_device != device_id
        || record.get("runtimeVersion").and_then(Value::as_str)
            != Some(start.runtime_version.as_str())
        || record.get("runtimeCommit").and_then(Value::as_str)
            != Some(start.runtime_commit.as_str())
        || !exact_capabilities(record.get("capabilities"))
    {
        return Err("The runner credential does not match this terminal.".to_string());
    }
    let target_grant_ref = record
        .get("targetGrants")
        .and_then(Value::as_array)
        .and_then(|grants| {
            grants.iter().find(|grant| {
                grant.get("targetRef").and_then(Value::as_str) == Some(start.target_ref.as_str())
                    && grant
                        .get("revokedAt")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .is_empty()
            })
        })
        .and_then(|grant| grant.get("grantId"))
        .and_then(Value::as_str)
        .filter(|value| valid_opaque_ref(value))
        .ok_or_else(|| "Codewhale returned no grant for this session.".to_string())?
        .to_string();
    Ok(LiveEnrollment {
        persisted: PersistedEnrollment {
            schema_version: 1,
            control_plane_base: base.to_string(),
            runner_enrollment_id: enrollment_id,
            account_ref,
            device_id: returned_device,
            target_ref: start.target_ref.clone(),
            target_grant_ref,
            runtime_version: start.runtime_version.clone(),
            runtime_commit: start.runtime_commit.clone(),
            bootstrap_secret: secret_field(&value, "bootstrapSecret")?,
        },
        access_token: access_token(&value)?,
    })
}

async fn refresh_enrollment(
    client: &Client,
    persisted: PersistedEnrollment,
) -> Result<LiveEnrollment, String> {
    let url = control_plane_url(
        &persisted.control_plane_base,
        &["api", "runner", "enrollments", "token"],
        &[],
    )?;
    let response = client
        .post(url)
        .json(&json!({
            "enrollmentId": persisted.runner_enrollment_id,
            "bootstrapSecret": persisted.bootstrap_secret,
        }))
        .send()
        .await
        .map_err(|_| "Remote-control credential refresh could not reach Codewhale.".to_string())?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return Err("runner_enrollment_revoked".to_string());
    }
    if !response.status().is_success() {
        return Err("Remote-control credential refresh was rejected.".to_string());
    }
    let value = read_bounded_json(response).await?;
    let record = value
        .get("enrollment")
        .filter(|value| value.is_object())
        .ok_or_else(|| "Codewhale returned an invalid refreshed credential.".to_string())?;
    if record.get("id").and_then(Value::as_str) != Some(persisted.runner_enrollment_id.as_str())
        || record.get("userId").and_then(Value::as_str) != Some(persisted.account_ref.as_str())
        || record.get("deviceId").and_then(Value::as_str) != Some(persisted.device_id.as_str())
        || record.get("runtimeVersion").and_then(Value::as_str)
            != Some(persisted.runtime_version.as_str())
        || record.get("runtimeCommit").and_then(Value::as_str)
            != Some(persisted.runtime_commit.as_str())
        || !exact_capabilities(record.get("capabilities"))
    {
        return Err("Codewhale returned a mismatched refreshed credential.".to_string());
    }
    Ok(LiveEnrollment {
        persisted,
        access_token: access_token(&value)?,
    })
}

async fn connect_runner(
    client: &Client,
    enrollment: &LiveEnrollment,
    start: &RemoteStart,
) -> Result<RunnerConnection, String> {
    let value = runner_request(
        client,
        enrollment,
        Method::POST,
        &["api", "local-runners", "connect"],
        &[],
        Some(connect_runner_body(enrollment, start)),
    )
    .await?;
    parse_runner_connection(&value, enrollment, start)
}

fn connect_runner_body(enrollment: &LiveEnrollment, start: &RemoteStart) -> Value {
    let mut body = json!({
        "deviceId": enrollment.persisted.device_id,
        "targetRef": start.target_ref,
        "displayLabel": start.workspace_label,
        "runtimeVersion": start.runtime_version,
        "runtimeCommit": start.runtime_commit,
        "capabilities": CAPABILITIES,
        "status": "active",
        // This is the only session attachment input. It is an opaque runtime
        // id, never a workspace path, prompt, environment, or credential.
        "sessionRef": start.session_id,
        "runtimeChatRelayProtocol": RUNTIME_CHAT_RELAY_PROTOCOL,
    });
    if let Some(repo) = start
        .git_remote
        .as_deref()
        .and_then(normalize_observed_git_repo)
    {
        body["gitRemote"] = json!(repo);
    }
    body
}

fn parse_runner_connection(
    value: &Value,
    enrollment: &LiveEnrollment,
    start: &RemoteStart,
) -> Result<RunnerConnection, String> {
    let response = value
        .as_object()
        .filter(|record| {
            record.len() == 2 && record.contains_key("runner") && record.contains_key("attachment")
        })
        .ok_or_else(|| "Codewhale returned an invalid runner attachment response.".to_string())?;
    let runner = response
        .get("runner")
        .and_then(Value::as_object)
        .ok_or_else(|| "Codewhale returned an invalid runner lease.".to_string())?;
    let runner_id = runner
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| valid_opaque_ref(value))
        .map(ToString::to_string)
        .ok_or_else(|| "Codewhale returned an invalid runner lease.".to_string())?;
    let runner_binding_matches = runner.get("userId").and_then(Value::as_str)
        == Some(enrollment.persisted.account_ref.as_str())
        && runner.get("deviceId").and_then(Value::as_str)
            == Some(enrollment.persisted.device_id.as_str())
        && runner.get("targetRef").and_then(Value::as_str) == Some(start.target_ref.as_str())
        && runner.get("runtimeVersion").and_then(Value::as_str)
            == Some(start.runtime_version.as_str())
        && runner.get("runtimeCommit").and_then(Value::as_str)
            == Some(start.runtime_commit.as_str())
        && runner.get("controlPath").and_then(Value::as_str) == Some("outbound_relay")
        && runner.get("status").and_then(Value::as_str) == Some("active")
        && runner.get("active").and_then(Value::as_bool) == Some(true)
        && exact_capabilities(runner.get("capabilities"));
    if !runner_binding_matches {
        return Err("Codewhale returned a runner lease for a different session.".to_string());
    }

    let attachment = response
        .get("attachment")
        .and_then(Value::as_object)
        .filter(|record| {
            record.len() == 6
                && record.contains_key("runId")
                && record.contains_key("workspaceId")
                && record.contains_key("runtimeCursor")
                && record.contains_key("snapshotPresent")
                && record.contains_key("runtimeChatRelayProtocol")
                && record.contains_key("runtimeChatRelayChallenge")
        })
        .ok_or_else(|| "Codewhale returned an invalid session attachment.".to_string())?;
    let run_id = attachment
        .get("runId")
        .and_then(Value::as_str)
        .filter(|value| valid_opaque_ref(value))
        .map(ToString::to_string)
        .ok_or_else(|| "Codewhale returned an invalid attached run.".to_string())?;
    let workspace_id = attachment
        .get("workspaceId")
        .and_then(Value::as_str)
        .filter(|value| valid_opaque_ref(value))
        .map(ToString::to_string)
        .ok_or_else(|| "Codewhale returned an invalid attached workspace.".to_string())?;
    let runtime_cursor = attachment
        .get("runtimeCursor")
        .and_then(Value::as_u64)
        .filter(|value| *value <= JS_MAX_SAFE_INTEGER)
        .ok_or_else(|| "Codewhale returned an invalid runtime event cursor.".to_string())?;
    let snapshot_present = attachment
        .get("snapshotPresent")
        .and_then(Value::as_bool)
        .ok_or_else(|| "Codewhale returned an invalid snapshot receipt.".to_string())?;
    let runtime_chat_relay_protocol = attachment
        .get("runtimeChatRelayProtocol")
        .and_then(Value::as_str)
        .filter(|value| *value == RUNTIME_CHAT_RELAY_PROTOCOL)
        .map(ToString::to_string)
        .ok_or_else(|| {
            "Codewhale returned an unsupported Runtime Chat relay protocol.".to_string()
        })?;
    let runtime_chat_relay_challenge = attachment
        .get("runtimeChatRelayChallenge")
        .and_then(Value::as_str)
        .filter(|value| {
            (32..=128).contains(&value.len())
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
        .map(ToString::to_string)
        .ok_or_else(|| "Codewhale returned an invalid Runtime Chat relay challenge.".to_string())?;

    let links = parse_remote_links(runner, &run_id);

    Ok(RunnerConnection {
        runner_id,
        attachment: RemoteAttachment {
            run_id,
            workspace_id,
            runtime_cursor,
            snapshot_present,
            runtime_chat_relay_protocol,
            runtime_chat_relay_challenge,
        },
        links,
    })
}

/// Read the optional `runUrl` / `computerUrl` advertised on the runner lease.
/// Absent fields yield `None`; present-but-invalid values are dropped (never
/// displayed, never opened) rather than failing the attachment, because a
/// link is a convenience receipt and not part of the ownership contract.
fn parse_remote_links(runner: &serde_json::Map<String, Value>, run_id: &str) -> RemoteLinks {
    let run_url = runner
        .get("runUrl")
        .and_then(Value::as_str)
        .and_then(|value| validate_run_url(value, run_id));
    let computer_url = runner
        .get("computerUrl")
        .and_then(Value::as_str)
        .and_then(validate_computer_url);
    RemoteLinks {
        run_url,
        computer_url,
    }
}

/// Parse a web link and accept it only on the Codewhale app origin (or, in
/// debug builds only, a loopback development origin) with no credentials,
/// port, or fragment — the same shape `validate_authorization_url` enforces.
fn app_link_url(value: &str) -> Option<Url> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 2048 {
        return None;
    }
    let url = Url::parse(trimmed).ok()?;
    let production_origin =
        url.scheme() == "https" && url.host_str() == Some(APP_ORIGIN_HOST) && url.port().is_none();
    let debug_loopback = cfg!(debug_assertions)
        && url.scheme() == "http"
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost"));
    if !(production_origin || debug_loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url)
}

/// `/session?run=<runId>` for exactly the attached run; anything else is
/// dropped so the terminal can never send the user to a different session.
fn validate_run_url(value: &str, run_id: &str) -> Option<String> {
    let url = app_link_url(value)?;
    let pairs = url.query_pairs().collect::<Vec<_>>();
    if url.path() != "/session" || pairs.len() != 1 || pairs[0].0 != "run" || pairs[0].1 != run_id {
        return None;
    }
    Some(url.to_string())
}

/// `/settings` (optionally `?section=…`) on the app origin.
fn validate_computer_url(value: &str) -> Option<String> {
    let url = app_link_url(value)?;
    let pairs = url.query_pairs().collect::<Vec<_>>();
    if url.path() != "/settings"
        || pairs.len() > 1
        || pairs.iter().any(|(key, _)| *key != "section")
    {
        return None;
    }
    Some(url.to_string())
}

async fn post_heartbeat(
    client: &Client,
    enrollment: &LiveEnrollment,
    runner_id: &str,
    start: &RemoteStart,
    status: &str,
) -> Result<(), String> {
    runner_request(
        client,
        enrollment,
        Method::POST,
        &["api", "local-runners", runner_id, "heartbeat"],
        &[],
        Some(json!({
            "runtimeVersion": start.runtime_version,
            "runtimeCommit": start.runtime_commit,
            "capabilities": CAPABILITIES,
            "status": status,
        })),
    )
    .await
    .map(|_| ())
}

async fn list_runs(
    client: &Client,
    enrollment: &LiveEnrollment,
    runner_id: &str,
) -> Result<Vec<String>, String> {
    let value = runner_request(
        client,
        enrollment,
        Method::GET,
        &["api", "local-runners", runner_id, "runs"],
        &[],
        None,
    )
    .await?;
    let runs = value
        .get("runs")
        .and_then(Value::as_array)
        .filter(|runs| runs.len() <= MAX_RUNS)
        .ok_or_else(|| "Codewhale returned an invalid runner run list.".to_string())?;
    runs.iter()
        .map(|run| {
            run.get("id")
                .and_then(Value::as_str)
                .filter(|value| valid_opaque_ref(value))
                .map(ToString::to_string)
                .ok_or_else(|| "Codewhale returned an invalid runner run.".to_string())
        })
        .collect()
}

async fn list_commands(
    client: &Client,
    enrollment: &LiveEnrollment,
    runner_id: &str,
    run_id: &str,
    since: u64,
) -> Result<Vec<ListedCommand>, String> {
    let value = runner_request(
        client,
        enrollment,
        Method::GET,
        &[
            "api",
            "local-runners",
            runner_id,
            "runs",
            run_id,
            "commands",
        ],
        &[
            ("since_seq", since.to_string()),
            ("include_accepted", "1".to_string()),
        ],
        None,
    )
    .await?;
    let commands = value
        .get("commands")
        .and_then(Value::as_array)
        .filter(|commands| commands.len() <= MAX_COMMANDS)
        .ok_or_else(|| "Codewhale returned an invalid command list.".to_string())?;
    commands
        .iter()
        .map(|item| {
            let seq = item
                .get("seq")
                .and_then(Value::as_u64)
                .filter(|value| *value > since)
                .ok_or_else(|| "Codewhale returned an invalid command sequence.".to_string())?;
            let command = item
                .get("command")
                .filter(|value| value.is_object())
                .cloned()
                .ok_or_else(|| "Codewhale returned an invalid typed command.".to_string())?;
            Ok(ListedCommand {
                seq,
                command,
                ack_status: item
                    .get("ackStatus")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            })
        })
        .collect()
}

struct ListedCommand {
    seq: u64,
    command: Value,
    ack_status: String,
}

async fn upload_command_accepted(
    client: &Client,
    enrollment: &LiveEnrollment,
    runner_id: &str,
    run_id: &str,
    seq: u64,
    command: &RemoteCommand,
) -> Result<(), String> {
    runner_request(
        client,
        enrollment,
        Method::POST,
        &["api", "local-runners", runner_id, "runs", run_id, "events"],
        &[],
        Some(json!({
            "acknowledgements": [{
                "commandSeq": seq,
                "commandType": command.kind(),
                "status": "accepted",
                "turnId": command.turn_id(),
            }],
            "envelopes": [],
        })),
    )
    .await
    .map(|_| ())
}

async fn recover_run(
    client: &Client,
    enrollment: &LiveEnrollment,
    runner_id: &str,
    run_id: &str,
    reason: &str,
) -> Result<(), String> {
    runner_request(
        client,
        enrollment,
        Method::POST,
        &[
            "api",
            "local-runners",
            runner_id,
            "runs",
            run_id,
            "recovery",
        ],
        &[],
        Some(json!({ "reason": reason })),
    )
    .await
    .map(|_| ())
}

fn parse_remote_command(value: &Value, expected_run_id: &str) -> Result<RemoteCommand, String> {
    if value.get("runId").and_then(Value::as_str) != Some(expected_run_id) {
        return Err("A remote command targeted a different run.".to_string());
    }
    match value.get("type").and_then(Value::as_str) {
        Some("prompt.request") => {
            if value.get("images").is_some() && value.get("runtimeBindingId").is_none() {
                return Err("Image input is unavailable for legacy remote Work; use native Runtime or Runtime Chat.".to_string());
            }
            let exact_legacy_prompt = value.as_object().is_some_and(|record| {
                record.len() == 4
                    && ["type", "runId", "turnId", "prompt"]
                        .iter()
                        .all(|key| record.contains_key(*key))
            });
            if !exact_legacy_prompt {
                // A managed Chat prompt has a rich deny-unknown schema. Any
                // field beyond the exact legacy Work shape must satisfy that
                // schema in full; missing binding/version/route/tool fields can
                // never downgrade into the permissive Work parser.
                let prompt: RuntimeChatPrompt =
                    serde_json::from_value(value.clone()).map_err(|_| {
                        "Codewhale sent an invalid Runtime Chat prompt contract.".to_string()
                    })?;
                prompt.validate_shape()?;
                if prompt.run_id != expected_run_id {
                    return Err("A Runtime Chat prompt targeted a different run.".to_string());
                }
                return Ok(RemoteCommand::RuntimeChatPrompt(Box::new(prompt)));
            }
            let turn_id = value
                .get("turnId")
                .and_then(Value::as_str)
                .filter(|value| valid_opaque_ref(value))
                .ok_or_else(|| "A remote prompt had no valid turn id.".to_string())?;
            let prompt = value
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty() && value.len() <= 128 * 1024)
                .ok_or_else(|| "A remote prompt was empty or oversized.".to_string())?;
            Ok(RemoteCommand::Prompt {
                turn_id: turn_id.to_string(),
                prompt: prompt.to_string(),
            })
        }
        Some("approval.decision") => {
            let exact_approval = value.as_object().is_some_and(|record| {
                record.len() == 4
                    && ["type", "runId", "gate", "decision"]
                        .iter()
                        .all(|key| record.contains_key(*key))
            });
            if !exact_approval {
                return Err("Codewhale sent an invalid approval contract.".to_string());
            }
            let gate = value
                .get("gate")
                .and_then(Value::as_str)
                .filter(|value| valid_opaque_ref(value))
                .ok_or_else(|| "A remote approval had no valid gate id.".to_string())?;
            let approved = match value.get("decision").and_then(Value::as_str) {
                Some("approved") => true,
                Some("denied") => false,
                _ => return Err("A remote approval had an invalid decision.".to_string()),
            };
            Ok(RemoteCommand::Approval {
                gate: gate.to_string(),
                approved,
            })
        }
        Some("run.control") => {
            let rich_runtime_control =
                value.get("runtimeBindingId").is_some() || value.get("runtimeThreadId").is_some();
            if rich_runtime_control {
                let record = value
                    .as_object()
                    .filter(|record| {
                        record.len() == 7
                            && record.contains_key("type")
                            && record.contains_key("runId")
                            && record.contains_key("action")
                            && record.contains_key("reason")
                            && record.contains_key("turnId")
                            && record.contains_key("runtimeBindingId")
                            && record.contains_key("runtimeThreadId")
                    })
                    .ok_or_else(|| {
                        "Codewhale sent an invalid Runtime Chat interrupt contract.".to_string()
                    })?;
                if record.get("action").and_then(Value::as_str) != Some("interrupt") {
                    return Err("Runtime Chat supports only an exact turn interrupt.".to_string());
                }
                let reason = record
                    .get("reason")
                    .and_then(Value::as_str)
                    .filter(|reason| {
                        !reason.trim().is_empty() && reason.len() <= 800 && !reason.contains('\0')
                    })
                    .ok_or_else(|| "A Runtime Chat interrupt reason is invalid.".to_string())?;
                let _ = reason;
                let turn_id = record
                    .get("turnId")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "A Runtime Chat interrupt turn is invalid.".to_string())?
                    .to_string();
                let scope = RuntimeChatControlScope {
                    runtime_binding_id: record
                        .get("runtimeBindingId")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "A Runtime Chat interrupt binding is invalid.".to_string())?
                        .to_string(),
                    runtime_thread_id: record
                        .get("runtimeThreadId")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "A Runtime Chat interrupt thread is invalid.".to_string())?
                        .to_string(),
                };
                scope.validate_for_turn(&turn_id)?;
                return Ok(RemoteCommand::Control {
                    action: RemoteControlRequest::Interrupt,
                    turn_id: Some(turn_id),
                    runtime_chat: Some(scope),
                });
            }
            let exact_legacy_control = value.as_object().is_some_and(|record| {
                record.len() == 4
                    && ["type", "runId", "action", "turnId"]
                        .iter()
                        .all(|key| record.contains_key(*key))
            });
            if !exact_legacy_control {
                return Err("Codewhale sent an invalid run-control contract.".to_string());
            }
            let action = match value.get("action").and_then(Value::as_str) {
                Some("interrupt") => RemoteControlRequest::Interrupt,
                Some("cancel") => RemoteControlRequest::Cancel,
                _ => return Err("A remote run-control command had an invalid action.".to_string()),
            };
            let turn_id = value
                .get("turnId")
                .and_then(Value::as_str)
                .filter(|turn_id| valid_opaque_ref(turn_id))
                .map(ToString::to_string)
                .ok_or_else(|| "A remote run-control command had an invalid turn.".to_string())?;
            Ok(RemoteCommand::Control {
                action,
                turn_id: Some(turn_id),
                runtime_chat: None,
            })
        }
        _ => Err("Codewhale sent an unsupported remote command.".to_string()),
    }
}

async fn runner_request(
    client: &Client,
    enrollment: &LiveEnrollment,
    method: Method,
    segments: &[&str],
    query: &[(&str, String)],
    body: Option<Value>,
) -> Result<Value, String> {
    let url = control_plane_url(&enrollment.persisted.control_plane_base, segments, query)?;
    let mut request = client
        .request(method, url)
        .bearer_auth(&enrollment.access_token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Remote control lost its secure connection.".to_string())?;
    if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        return Err("runner_access_token_expired".to_string());
    }
    if !response.status().is_success() {
        let status = response.status();
        let excerpt = rejection_excerpt(response).await;
        return Err(match excerpt {
            Some(reason) => {
                format!("The remote-control server rejected a request ({status}): {reason}.")
            }
            None => format!("The remote-control server rejected a request ({status})."),
        });
    }
    let limit = if segments.last() == Some(&"commands") {
        codewhale_protocol::runtime::MAX_RUNTIME_IMAGE_BODY_BYTES
    } else {
        MAX_RESPONSE_BYTES
    };
    read_bounded_json_with_limit(response, limit).await
}

async fn public_request(
    client: &Client,
    method: Method,
    url: Url,
    body: Value,
) -> Result<Value, String> {
    let response = client
        .request(method, url)
        .json(&body)
        .send()
        .await
        .map_err(|_| "Remote control could not reach Codewhale.".to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let excerpt = rejection_excerpt(response).await;
        return Err(match excerpt {
            Some(reason) => {
                format!("Codewhale rejected remote-control enrollment ({status}): {reason}.")
            }
            None => format!("Codewhale rejected remote-control enrollment ({status})."),
        });
    }
    read_bounded_json(response).await
}

/// Bounded error-body read: at most MAX_RESPONSE_BYTES, so a misbehaving
/// control plane cannot force an unbounded in-memory read through the
/// rejection-excerpt path.
async fn rejection_excerpt(response: reqwest::Response) -> Option<String> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.ok()? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_RESPONSE_BYTES {
            return None;
        }
    }
    sanitized_rejection_excerpt(&body)
}

/// Sanitized, bounded reason excerpt from a rejection body. Only the
/// conventional error fields are read, control characters are stripped, and
/// the excerpt is capped — raw server bytes are never echoed further.
fn sanitized_rejection_excerpt(body: &[u8]) -> Option<String> {
    let parsed: Value = serde_json::from_slice(body).ok()?;
    for field in ["error", "message", "title"] {
        if let Some(text) = parsed.get(field).and_then(Value::as_str) {
            let cleaned: String = text
                .chars()
                .filter(|character| !character.is_control())
                .take(140)
                .collect();
            let trimmed = cleaned.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

async fn read_bounded_json(response: reqwest::Response) -> Result<Value, String> {
    read_bounded_json_with_limit(response, MAX_RESPONSE_BYTES).await
}

async fn read_bounded_json_with_limit(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Value, String> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("Codewhale returned an oversized remote-control response.".to_string());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Codewhale returned an unreadable response.".to_string())?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err("Codewhale returned an oversized remote-control response.".to_string());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| "Codewhale returned an invalid remote-control response.".to_string())
}

fn runner_control_plane_base() -> Result<String, String> {
    if cfg!(debug_assertions)
        && let Ok(value) = std::env::var("CWC_RUNNER_CONTROL_PLANE_BASE")
    {
        let parsed =
            Url::parse(&value).map_err(|_| "The runner control plane is invalid.".to_string())?;
        let loopback = parsed.scheme() == "http"
            && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost"))
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none();
        if loopback {
            return Ok(parsed.to_string());
        }
        return Err(
            "Debug remote control only accepts an explicit loopback control plane.".to_string(),
        );
    }
    Ok(PRODUCTION_CONTROL_PLANE.to_string())
}

fn control_plane_url(
    base: &str,
    segments: &[&str],
    query: &[(&str, String)],
) -> Result<Url, String> {
    let mut url =
        Url::parse(base).map_err(|_| "The runner control plane is invalid.".to_string())?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| "The runner control plane is invalid.".to_string())?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    Ok(url)
}

fn load_persisted_enrollment() -> Result<Option<PersistedEnrollment>, String> {
    let Some(raw) = remote_control_secrets()
        .get(ENROLLMENT_SECRET_SLOT)
        .map_err(|error| format!("Could not read the saved remote-control enrollment: {error}"))?
    else {
        return Ok(None);
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|_| "The saved remote-control enrollment is invalid.".to_string())
}

fn save_persisted_enrollment(enrollment: &PersistedEnrollment) -> Result<(), String> {
    let raw = serde_json::to_string(enrollment)
        .map_err(|_| "Could not encode the remote-control enrollment.".to_string())?;
    remote_control_secrets()
        .set(ENROLLMENT_SECRET_SLOT, &raw)
        .map_err(|error| format!("Could not securely save the remote-control enrollment: {error}"))
}

fn load_persisted_device_identity() -> Result<Option<PersistedDeviceIdentity>, String> {
    let Some(raw) = remote_control_secrets()
        .get(DEVICE_IDENTITY_SECRET_SLOT)
        .map_err(|error| format!("Could not read the saved remote-control device id: {error}"))?
    else {
        return Ok(None);
    };
    // An unreadable identity is replaced rather than fatal: the worst case is
    // one extra computer row, never a lost session.
    Ok(serde_json::from_str(&raw).ok())
}

fn save_persisted_device_identity(identity: &PersistedDeviceIdentity) -> Result<(), String> {
    let raw = serde_json::to_string(identity)
        .map_err(|_| "Could not encode the remote-control device id.".to_string())?;
    remote_control_secrets()
        .set(DEVICE_IDENTITY_SECRET_SLOT, &raw)
        .map_err(|error| format!("Could not securely save the remote-control device id: {error}"))
}

/// Load (or mint and persist) the machine-stable device id.
fn stable_device_id(enrollment_device_id: Option<&str>) -> Result<String, String> {
    let saved = load_persisted_device_identity()?;
    let (device_id, needs_save) = resolve_device_identity(saved, enrollment_device_id);
    if needs_save {
        save_persisted_device_identity(&PersistedDeviceIdentity {
            schema_version: 1,
            device_id: device_id.clone(),
        })?;
    }
    Ok(device_id)
}

fn delete_persisted_enrollment() {
    if let Err(error) = remote_control_secrets().delete(ENROLLMENT_SECRET_SLOT) {
        tracing::warn!("could not delete revoked remote-control enrollment: {error}");
    }
}

/// Remote-control enrollment/device receipts deliberately use Codewhale's
/// permission-bounded file store even when the operator chose the OS keyring
/// for model-provider credentials. This keeps `/rc` and desktop dogfood from
/// triggering a Keychain access dialog while preserving provider credential
/// custody and its explicit backend selection unchanged.
fn remote_control_secrets() -> codewhale_secrets::Secrets {
    codewhale_secrets::Secrets::file_backed()
}

fn install_reconnected_enrollment(
    current: &mut LiveEnrollment,
    candidate: LiveEnrollment,
    start: &RemoteStart,
) -> Result<(), String> {
    if candidate.persisted.account_ref != current.persisted.account_ref
        || candidate.persisted.target_ref != current.persisted.target_ref
        || candidate.persisted.target_ref != start.target_ref
        || candidate.persisted.device_id != current.persisted.device_id
    {
        return Err(
            "Remote control stopped because re-authorization selected a different account or folder. Start a new local session to switch authority."
                .to_string(),
        );
    }
    *current = candidate;
    Ok(())
}

async fn refresh_enrollment_and_reconnect(
    client: &Client,
    enrollment: &mut LiveEnrollment,
    runner_id: &mut String,
    start: &RemoteStart,
    event_tx: &mpsc::UnboundedSender<RemoteEvent>,
) -> Result<(), String> {
    let base = enrollment.persisted.control_plane_base.clone();
    match refresh_enrollment(client, enrollment.persisted.clone()).await {
        Ok(new_enrollment) => {
            install_reconnected_enrollment(enrollment, new_enrollment, start)?;
            reconnect_runner(client, enrollment, runner_id, start, event_tx).await
        }
        Err(err) if err == "runner_enrollment_revoked" => {
            delete_persisted_enrollment();
            let device_id = enrollment.persisted.device_id.clone();
            let candidate = enroll_device(client, &base, start, &device_id, event_tx).await?;
            if let Err(error) = install_reconnected_enrollment(enrollment, candidate, start) {
                delete_persisted_enrollment();
                return Err(error);
            }
            reconnect_runner(client, enrollment, runner_id, start, event_tx).await
        }
        Err(err) => Err(err),
    }
}

async fn reconnect_runner(
    client: &Client,
    enrollment: &LiveEnrollment,
    runner_id: &mut String,
    start: &RemoteStart,
    event_tx: &mpsc::UnboundedSender<RemoteEvent>,
) -> Result<(), String> {
    let connection = connect_runner(client, enrollment, start).await?;
    *runner_id = connection.runner_id;
    event_tx
        .send(RemoteEvent::Attachment {
            account_ref: enrollment.persisted.account_ref.clone(),
            target_ref: enrollment.persisted.target_ref.clone(),
            attachment: connection.attachment,
            links: connection.links,
        })
        .map_err(|_| "The terminal remote-control owner stopped.".to_string())
}

fn enrollment_needs_refresh(enrollment: &LiveEnrollment) -> bool {
    jwt_expiry(&enrollment.access_token)
        .is_none_or(|expiry| expiry <= epoch_seconds().saturating_add(60))
}

fn jwt_expiry(token: &str) -> Option<u64> {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let payload = URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?;
    serde_json::from_slice::<Value>(&payload)
        .ok()?
        .get("exp")?
        .as_u64()
}

fn access_token(value: &Value) -> Result<String, String> {
    let token = value
        .get("credential")
        .and_then(|value| value.get("accessToken"))
        .and_then(Value::as_str)
        .filter(|value| {
            (64..=8192).contains(&value.len()) && !value.chars().any(char::is_whitespace)
        })
        .ok_or_else(|| "Codewhale returned an invalid runner access token.".to_string())?
        .to_string();
    if jwt_expiry(&token).is_none_or(|expiry| expiry <= epoch_seconds()) {
        return Err("Codewhale returned an expired runner access token.".to_string());
    }
    Ok(token)
}

fn exact_capabilities(value: Option<&Value>) -> bool {
    let Some(items) = value.and_then(Value::as_array) else {
        return false;
    };
    let mut actual = items.iter().filter_map(Value::as_str).collect::<Vec<_>>();
    actual.sort_unstable();
    actual == CAPABILITIES
}

fn validate_authorization_url(value: &str, user_code: &str) -> Result<(), String> {
    let url = Url::parse(value)
        .map_err(|_| "Codewhale returned an invalid authorization URL.".to_string())?;
    let pairs = url.query_pairs().collect::<Vec<_>>();
    if url.scheme() != "https"
        || url.host_str() != Some("app.codewhale.net")
        || url.path() != "/runner/authorize"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || pairs.len() != 1
        || pairs[0].0 != "user_code"
        || pairs[0].1 != user_code
    {
        return Err("Codewhale returned an invalid authorization URL.".to_string());
    }
    Ok(())
}

fn string_field(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 2048)
        .map(ToString::to_string)
        .ok_or_else(|| format!("Codewhale returned an invalid {field}."))
}

fn secret_field(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| valid_secret(value))
        .map(ToString::to_string)
        .ok_or_else(|| format!("Codewhale returned an invalid {field}."))
}

fn opaque_field(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| valid_opaque_ref(value))
        .map(ToString::to_string)
        .ok_or_else(|| format!("Codewhale returned an invalid {field}."))
}

fn valid_opaque_ref(value: &str) -> bool {
    (3..=160).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn valid_session_ref(value: &str) -> bool {
    (1..=160).contains(&value.len())
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'@')
        })
        && !value.contains("..")
}

fn valid_secret(value: &str) -> bool {
    (32..=8192).contains(&value.len()) && !value.chars().any(char::is_whitespace)
}

fn valid_runtime_version(value: &str) -> bool {
    semver::Version::parse(value).is_ok() && value.len() <= 64
}

fn valid_runtime_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::Role;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use wiremock::{
        Mock, MockServer, Request, Respond, ResponseTemplate,
        matchers::{body_json, method, path, query_param},
    };

    #[derive(Clone, Default)]
    struct AmbiguousRuntimeResponder {
        bodies: Arc<Mutex<Vec<Value>>>,
    }

    #[derive(Clone, Default)]
    struct ControlPollCounter {
        hits: Arc<AtomicUsize>,
    }

    impl Respond for ControlPollCounter {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            self.hits.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({ "runs": [] }))
        }
    }

    impl Respond for AmbiguousRuntimeResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value = serde_json::from_slice(&request.body).expect("runtime request JSON");
            let mut bodies = self.bodies.lock().expect("runtime request bodies");
            bodies.push(body);
            if bodies.len() == 1 {
                // Model a committed request whose response was truncated in
                // transit. The client must keep the exact body and retry.
                ResponseTemplate::new(200).set_body_raw("{", "application/json")
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "accepted": [],
                    "count": 0,
                    "cursor": 1
                }))
            }
        }
    }

    fn text_message(role: &str, text: impl Into<String>) -> Message {
        Message {
            role: Role::from(role),
            content: vec![ContentBlock::Text {
                text: text.into(),
                cache_control: None,
            }],
        }
    }

    fn fixture_start() -> RemoteStart {
        RemoteStart {
            workspace_label: "private-project".to_string(),
            target_ref: "target_fixture".to_string(),
            session_id: "session:fixture@01".to_string(),
            runtime_version: "0.9.6".to_string(),
            runtime_commit: "a".repeat(40),
            journal_dir: None,
            git_remote: None,
        }
    }

    fn fixture_enrollment(base: &str) -> LiveEnrollment {
        LiveEnrollment {
            persisted: PersistedEnrollment {
                schema_version: 1,
                control_plane_base: base.to_string(),
                runner_enrollment_id: "enrollment_fixture".to_string(),
                account_ref: "account_fixture".to_string(),
                device_id: "device_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                target_grant_ref: "grant_fixture".to_string(),
                runtime_version: "0.9.6".to_string(),
                runtime_commit: "a".repeat(40),
                bootstrap_secret: "b".repeat(43),
            },
            access_token: "fixture-runner-access-token".to_string(),
        }
    }

    fn fixture_connection_response() -> Value {
        json!({
            "runner": {
                "id": "runner_fixture",
                "userId": "account_fixture",
                "deviceId": "device_fixture",
                "targetRef": "target_fixture",
                "displayLabel": "private-project",
                "runtimeVersion": "0.9.6",
                "runtimeCommit": "a".repeat(40),
                "capabilities": CAPABILITIES,
                "controlPath": "outbound_relay",
                "status": "active",
                "active": true,
                "capacity": 1,
                "lastHeartbeatAt": "2026-08-08T12:00:00.000Z",
                "expiresAt": "2026-08-08T12:01:30.000Z",
                "revokedAt": "",
                "createdAt": "2026-08-08T12:00:00.000Z",
                "updatedAt": "2026-08-08T12:00:00.000Z"
            },
            "attachment": {
                "runId": "run_fixture",
                "workspaceId": "workspace_fixture",
                "runtimeCursor": 41,
                "snapshotPresent": false,
                "runtimeChatRelayProtocol": RUNTIME_CHAT_RELAY_PROTOCOL,
                "runtimeChatRelayChallenge": "a".repeat(32)
            }
        })
    }

    #[test]
    fn observed_git_repo_is_owner_name_not_a_path() {
        assert_eq!(
            normalize_observed_git_repo("git@github.com:codewhale-hq/CodeWhale.git").as_deref(),
            Some("codewhale-hq/CodeWhale")
        );
        assert_eq!(
            normalize_observed_git_repo("https://github.com/Hmbown/cwc.git").as_deref(),
            Some("Hmbown/cwc")
        );
        assert_eq!(
            normalize_observed_git_repo("/Volumes/VIXinSSD/CW/codewhale"),
            None
        );
        for private_or_untrusted in [
            "file:///Users/alice/PrivateProject.git",
            "https://internal.example/acme/secret-repo.git",
            "https://alice:password@github.com/acme/repo.git",
            "https://github.com/acme/repo.git?token=secret",
            "ssh://git@github.com/acme/repo.git",
            "acme/secret-repo",
        ] {
            assert_eq!(
                normalize_observed_git_repo(private_or_untrusted),
                None,
                "untrusted git identity must not cross: {private_or_untrusted}"
            );
        }
    }

    #[test]
    fn connect_body_can_carry_an_observed_repo_without_a_path() {
        let enrollment = fixture_enrollment("https://api.codewhale.net/");
        let mut start = fixture_start();
        start.git_remote = Some("git@github.com:codewhale-hq/CodeWhale.git".to_string());
        let body = connect_runner_body(&enrollment, &start);
        assert_eq!(body["gitRemote"], "codewhale-hq/CodeWhale");
        assert!(body.get("workspacePath").is_none());
        assert!(body.get("path").is_none());
    }

    #[test]
    fn live_worker_rejects_reenrollment_authority_switch_before_assignment() {
        let start = fixture_start();
        let mut current = fixture_enrollment("https://api.codewhale.net/");
        let original_access_token = current.access_token.clone();
        let mut other_account = current.clone();
        other_account.persisted.account_ref = "account_other".to_string();
        other_account.access_token = "other-account-access-token".to_string();
        let error = install_reconnected_enrollment(&mut current, other_account, &start)
            .expect_err("a live worker must never change tenant authority");
        assert!(error.contains("different account or folder"));
        assert_eq!(current.persisted.account_ref, "account_fixture");
        assert_eq!(current.persisted.target_ref, "target_fixture");
        assert_eq!(current.access_token, original_access_token);

        let mut other_target = current.clone();
        other_target.persisted.target_ref = "target_other".to_string();
        assert!(install_reconnected_enrollment(&mut current, other_target, &start).is_err());
        assert_eq!(current.persisted.target_ref, "target_fixture");

        let mut refreshed = current.clone();
        refreshed.access_token = "same-authority-refreshed-access-token".to_string();
        install_reconnected_enrollment(&mut current, refreshed, &start).unwrap();
        assert_eq!(
            current.access_token,
            "same-authority-refreshed-access-token"
        );
    }

    #[test]
    fn target_identity_is_stable_without_exposing_the_path() {
        let target = target_ref(Path::new("/Users/alice/private/project"));
        assert!(target.starts_with("target_"));
        assert_eq!(target.len(), 39);
        assert!(!target.contains("alice"));
        // Every session opened in the same folder shares one target, so the
        // control plane keeps one grant per folder rather than one per `/rc`.
        assert_eq!(
            target,
            target_ref(Path::new("/Users/alice/private/project"))
        );
        assert_ne!(target, target_ref(Path::new("/Users/alice/private/other")));
    }

    #[test]
    fn runtime_chat_same_session_reconfigure_releases_then_reacquires_owner_lock() {
        let root = tempfile::tempdir().unwrap();
        let registry = Arc::new(crate::plugins::PluginRegistry::empty(root.path()));
        let mut controller = RemoteControlController::default();
        let old_config = crate::config::Config {
            provider: Some("ollama".to_string()),
            default_text_model: Some("old-safe-model".to_string()),
            ..crate::config::Config::default()
        };
        controller
            .configure_runtime_chat(
                old_config,
                Arc::clone(&registry),
                root.path().join("runtime-chat"),
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        assert_eq!(
            controller
                .runtime_chat
                .as_ref()
                .unwrap()
                .configured_default_model_for_tests(),
            "old-safe-model"
        );

        let mut competing = RemoteControlController::default();
        assert!(
            competing
                .configure_runtime_chat(
                    crate::config::Config::default(),
                    Arc::clone(&registry),
                    root.path().join("runtime-chat"),
                    "target_fixture".to_string(),
                    "session_fixture".to_string(),
                )
                .is_err()
        );

        let new_config = crate::config::Config {
            provider: Some("ollama".to_string()),
            default_text_model: Some("new-safe-model".to_string()),
            ..crate::config::Config::default()
        };
        controller
            .configure_runtime_chat(
                new_config,
                Arc::clone(&registry),
                root.path().join("runtime-chat"),
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        assert_eq!(
            controller
                .runtime_chat
                .as_ref()
                .unwrap()
                .configured_default_model_for_tests(),
            "new-safe-model",
            "a settled same-scope restart must refresh immutable provider configuration"
        );
        drop(controller);
        competing
            .configure_runtime_chat(
                crate::config::Config::default(),
                registry,
                root.path().join("runtime-chat"),
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
    }

    #[test]
    fn runtime_chat_backlog_refreshes_immutable_config_before_new_catalog_or_prompt() {
        let root = tempfile::tempdir().unwrap();
        let registry = Arc::new(crate::plugins::PluginRegistry::empty(root.path()));
        let mut controller = RemoteControlController::default();
        let old_config = crate::config::Config {
            provider: Some("ollama".to_string()),
            default_text_model: Some("old-safe-model".to_string()),
            ..crate::config::Config::default()
        };
        controller
            .configure_runtime_chat(
                old_config,
                Arc::clone(&registry),
                root.path().join("runtime-chat"),
                "target-1".to_string(),
                "session-1".to_string(),
            )
            .unwrap();
        controller
            .runtime_chat
            .as_ref()
            .unwrap()
            .bind_account("account-1", "target-1")
            .unwrap();
        controller.journal = Some(
            RuntimeEventJournal::open(root.path(), "target-1", "session-1")
                .expect("runtime chat delivery journal"),
        );

        let pending_seq = controller.next_runtime_seq("run-1");
        assert!(controller.queue_runtime_chat_envelope(
            "run-1",
            runtime_chat_envelope(
                pending_seq,
                "runtime.catalog",
                None,
                None,
                Some("catalog_old_fixture"),
                RUNTIME_CHAT_CATALOG_TIMESTAMP.to_string(),
                json!({ "old": true }),
            ),
        ));
        let new_config = crate::config::Config {
            provider: Some("ollama".to_string()),
            default_text_model: Some("new-safe-model".to_string()),
            ..crate::config::Config::default()
        };
        controller
            .configure_runtime_chat(
                new_config,
                Arc::clone(&registry),
                root.path().join("runtime-chat"),
                "target-1".to_string(),
                "session-1".to_string(),
            )
            .unwrap();
        assert!(controller.pending_runtime_chat_configuration.is_some());
        assert_eq!(
            controller
                .runtime_chat
                .as_ref()
                .unwrap()
                .configured_default_model_for_tests(),
            "old-safe-model",
            "the old host remains only as the durable backlog owner"
        );

        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        controller.worker_tx = Some(worker_tx);
        controller.event_rx = Some(event_rx);
        controller.status = Status::Connected;
        controller.account_ref = Some("account-1".to_string());
        controller.target_ref = Some("target-1".to_string());
        controller.attached_run_id = Some("run-1".to_string());
        controller.attached_workspace_id = Some("workspace-1".to_string());
        controller.runtime_chat_attachment = Some(RemoteAttachment {
            run_id: "run-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            runtime_cursor: pending_seq,
            snapshot_present: false,
            runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
            runtime_chat_relay_challenge: "c".repeat(32),
        });
        let command = parse_remote_command(&runtime_chat_prompt_fixture(), "run-1").unwrap();
        assert!(
            controller.claim_command("run-1", 1, &command).is_err(),
            "no newly configured prompt may reach the stale host while backlog drains"
        );

        event_tx
            .send(RemoteEvent::RuntimeCursor {
                run_id: "run-1".to_string(),
                cursor: pending_seq,
            })
            .unwrap();
        assert!(matches!(
            controller.try_next_event(),
            Some(RemoteEvent::RuntimeCursor { .. })
        ));
        assert!(matches!(
            worker_rx.try_recv().unwrap(),
            WorkerCommand::ReleaseRuntimeChatHost
        ));

        event_tx.send(RemoteEvent::RuntimeChatHostReleased).unwrap();
        assert!(matches!(
            controller.try_next_event(),
            Some(RemoteEvent::RuntimeChatHostReleased)
        ));
        assert_eq!(
            controller
                .runtime_chat
                .as_ref()
                .unwrap()
                .configured_default_model_for_tests(),
            "new-safe-model"
        );
        assert!(matches!(
            worker_rx.try_recv().unwrap(),
            WorkerCommand::InstallRuntimeChatHost(_)
        ));
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("the refreshed host must upload a new challenge-bound catalog");
        };
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0]["event"], "runtime.catalog");
        let serialized = envelopes[0].to_string();
        assert!(serialized.contains("new-safe-model"));
        assert!(!serialized.contains("old-safe-model"));
        assert!(controller.pending_runtime_chat_configuration.is_none());
    }

    #[tokio::test]
    async fn runtime_chat_provider_ownership_releases_only_after_terminal_server_cursor() {
        let root = tempfile::tempdir().unwrap();
        let mut controller = RemoteControlController::default();
        controller
            .configure_runtime_chat(
                crate::config::Config::default(),
                Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                root.path().join("runtime-chat"),
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        let host = controller.runtime_chat.as_ref().unwrap().clone();
        controller.journal = Some(
            RuntimeEventJournal::open(root.path(), "target_fixture", "session_fixture")
                .expect("runtime chat delivery journal"),
        );
        host.bind_account("account_fixture", "target_fixture")
            .unwrap();
        let native_thread_id = "thr_terminal_gate_fixture";
        let virtual_thread_id = format!("local_thread_{}", "a".repeat(24));
        let virtual_turn_id = format!("local_turn_{}", "b".repeat(24));
        host.install_unsettled_turn_for_tests(
            "run_fixture",
            native_thread_id,
            &virtual_thread_id,
            &virtual_turn_id,
        )
        .unwrap();
        host.acquire_inference_ownership_for_tests().await;
        assert!(host.inference_ownership_is_held_for_tests());

        let seq = controller.next_runtime_seq("run_fixture");
        assert!(controller.queue_runtime_chat_envelope(
            "run_fixture",
            runtime_chat_envelope(
                seq,
                "turn.completed",
                Some(&virtual_thread_id),
                Some(&virtual_turn_id),
                Some("native_event_terminal_gate_fixture"),
                "2026-08-23T00:00:00Z".to_string(),
                json!({ "status": "completed" }),
            ),
        ));
        host.mark_projected(native_thread_id, 1, &virtual_turn_id, "turn.completed")
            .unwrap();
        assert!(!host.has_any_unsettled_turns());
        controller.reconcile_runtime_cursor("run_fixture", seq.saturating_sub(1));
        assert!(
            host.inference_ownership_is_held_for_tests(),
            "local terminal projection is not a server acknowledgement"
        );
        controller.reconcile_runtime_cursor("run_fixture", seq);
        assert!(
            !host.inference_ownership_is_held_for_tests(),
            "the accepted server cursor releases attached-run provider ownership"
        );
    }

    #[tokio::test]
    async fn restart_rehydrates_runtime_chat_ownership_until_terminal_cursor() {
        let root = tempfile::tempdir().unwrap();
        let runtime_root = root.path().join("runtime-chat");
        let journal_root = root.path().join("journal");
        std::fs::create_dir_all(&journal_root).unwrap();
        let terminal_seq;
        {
            let mut first = RemoteControlController::default();
            first
                .configure_runtime_chat(
                    crate::config::Config::default(),
                    Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                    runtime_root.clone(),
                    "target_fixture".to_string(),
                    "session_fixture".to_string(),
                )
                .unwrap();
            first.journal = Some(
                RuntimeEventJournal::open(&journal_root, "target_fixture", "session_fixture")
                    .unwrap(),
            );
            let host = first.runtime_chat.as_ref().unwrap().clone();
            host.bind_account("account_fixture", "target_fixture")
                .unwrap();
            let native_thread_id = "thr_restart_terminal_gate";
            let virtual_thread_id = format!("local_thread_{}", "c".repeat(24));
            let virtual_turn_id = format!("local_turn_{}", "d".repeat(24));
            host.install_unsettled_turn_for_tests(
                "run_fixture",
                native_thread_id,
                &virtual_thread_id,
                &virtual_turn_id,
            )
            .unwrap();
            host.acquire_inference_ownership_for_tests().await;
            terminal_seq = first.next_runtime_seq("run_fixture");
            assert!(first.queue_runtime_chat_envelope(
                "run_fixture",
                runtime_chat_envelope(
                    terminal_seq,
                    "turn.completed",
                    Some(&virtual_thread_id),
                    Some(&virtual_turn_id),
                    Some("native_event_restart_terminal_gate"),
                    "2026-08-23T00:00:00Z".to_string(),
                    json!({ "turn": { "status": "completed" } }),
                ),
            ));
            host.mark_projected(native_thread_id, 1, &virtual_turn_id, "turn.completed")
                .unwrap();
            assert!(!host.has_any_unsettled_turns());
        }

        let mut reopened = RemoteControlController::default();
        reopened
            .configure_runtime_chat(
                crate::config::Config::default(),
                Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                runtime_root,
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        let journal =
            RuntimeEventJournal::open(&journal_root, "target_fixture", "session_fixture").unwrap();
        reopened.reset_pending_from(journal.load().unwrap());
        reopened.journal = Some(journal);
        reopened
            .recover_runtime_chat_ownership_before_worker()
            .unwrap();

        let mut participant = tokio::spawn(async {
            crate::client::acquire_remote_control_inference_participant().await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(40), &mut participant)
                .await
                .is_err(),
            "restart recovery must retain the provider writer before CWC cursor ack"
        );
        reopened.reconcile_runtime_cursor("run_fixture", terminal_seq);
        let permit = tokio::time::timeout(Duration::from_secs(1), participant)
            .await
            .expect("participant resumes after cursor")
            .expect("participant task");
        drop(permit);
    }

    #[tokio::test]
    async fn restart_reclaims_unsettled_runtime_chat_before_worker_or_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let runtime_root = root.path().join("runtime-chat");
        let native_thread_id = "thr_restart_unsettled_gate";
        let virtual_thread_id = format!("local_thread_{}", "e".repeat(24));
        let virtual_turn_id = format!("local_turn_{}", "f".repeat(24));
        {
            let mut first = RemoteControlController::default();
            first
                .configure_runtime_chat(
                    crate::config::Config::default(),
                    Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                    runtime_root.clone(),
                    "target_fixture".to_string(),
                    "session_fixture".to_string(),
                )
                .unwrap();
            let host = first.runtime_chat.as_ref().unwrap();
            host.bind_account("account_fixture", "target_fixture")
                .unwrap();
            host.install_unsettled_turn_for_tests(
                "run_fixture",
                native_thread_id,
                &virtual_thread_id,
                &virtual_turn_id,
            )
            .unwrap();
        }

        let mut reopened = RemoteControlController::default();
        reopened
            .configure_runtime_chat(
                crate::config::Config::default(),
                Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                runtime_root,
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        let host = reopened.runtime_chat.as_ref().unwrap().clone();
        assert!(host.has_any_unsettled_turns());

        let participant = crate::client::acquire_remote_control_inference_participant().await;
        let error = reopened
            .recover_runtime_chat_ownership_before_worker()
            .unwrap_err();
        assert!(error.contains("active local turn"), "{error}");
        assert!(!host.inference_ownership_is_held_for_tests());
        drop(participant);

        reopened
            .recover_runtime_chat_ownership_before_worker()
            .unwrap();
        let mut blocked = tokio::spawn(async {
            crate::client::acquire_remote_control_inference_participant().await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(40), &mut blocked)
                .await
                .is_err(),
            "an unsettled recovered turn must own the gate before Connected"
        );
        host.mark_projected(native_thread_id, 1, &virtual_turn_id, "turn.completed")
            .unwrap();
        host.release_inference_ownership_if_settled();
        let permit = tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .expect("participant resumes after durable terminal settlement")
            .expect("participant task");
        drop(permit);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn actual_start_reclaims_runtime_chat_writer_before_worker_spawn() {
        let root = tempfile::tempdir().unwrap();
        let runtime_root = root.path().join("runtime-chat");
        let journal_root = root.path().join("journal");
        let native_thread_id = "thr_actual_start_unsettled";
        let virtual_thread_id = format!("local_thread_{}", "c".repeat(24));
        let virtual_turn_id = format!("local_turn_{}", "d".repeat(24));
        {
            let mut first = RemoteControlController::default();
            first
                .prepare_remote_control_session_journal(
                    &journal_root,
                    "target_fixture",
                    "session_fixture",
                )
                .unwrap();
            first
                .configure_runtime_chat(
                    crate::config::Config::default(),
                    Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                    runtime_root.clone(),
                    "target_fixture".to_string(),
                    "session_fixture".to_string(),
                )
                .unwrap();
            first
                .runtime_chat
                .as_ref()
                .unwrap()
                .bind_account("account_fixture", "target_fixture")
                .unwrap();
            first
                .runtime_chat
                .as_ref()
                .unwrap()
                .install_unsettled_turn_for_tests(
                    "run_fixture",
                    native_thread_id,
                    &virtual_thread_id,
                    &virtual_turn_id,
                )
                .unwrap();
        }

        let mut reopened = RemoteControlController::default();
        reopened
            .prepare_remote_control_session_journal(
                &journal_root,
                "target_fixture",
                "session_fixture",
            )
            .unwrap();
        reopened
            .configure_runtime_chat(
                crate::config::Config::default(),
                Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                runtime_root,
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        let host = reopened.runtime_chat.as_ref().unwrap().clone();
        let start = RemoteStart {
            session_id: "session_fixture".to_string(),
            journal_dir: Some(journal_root),
            ..fixture_start()
        };

        let participant = crate::client::acquire_remote_control_inference_participant().await;
        let error = reopened.start(start.clone()).unwrap_err();
        assert!(error.contains("active local turn"), "{error}");
        assert!(reopened.worker.is_none());
        assert_eq!(reopened.status, Status::Off);
        drop(participant);

        reopened.start(start).unwrap();
        assert!(host.inference_ownership_is_held_for_tests());
        assert_eq!(reopened.status, Status::Connecting);
        assert!(reopened.worker.is_some());
        // This current-thread test has not yielded since spawning the worker,
        // so abort it before enrollment can perform any network I/O.
        reopened.stop_worker();
        host.mark_projected(native_thread_id, 1, &virtual_turn_id, "turn.completed")
            .unwrap();
        host.release_inference_ownership_if_settled();
        assert!(!host.inference_ownership_is_held_for_tests());
    }

    #[test]
    fn aborting_a_relay_worker_releases_unjournaled_projection_claims() {
        let root = tempfile::tempdir().unwrap();
        let mut controller = RemoteControlController::default();
        controller
            .configure_runtime_chat(
                crate::config::Config::default(),
                Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                root.path().join("runtime-chat"),
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        let host = controller.runtime_chat.as_ref().unwrap().clone();
        host.install_projection_claim_for_tests("thr_claimed", 7);
        assert!(host.projection_is_claimed_for_tests("thr_claimed", 7));

        controller.stop_worker();

        assert!(!host.projection_is_claimed_for_tests("thr_claimed", 7));
    }

    #[tokio::test]
    async fn idle_runtime_chat_ticks_cannot_starve_the_control_poll() {
        if !cfg!(debug_assertions) {
            return;
        }
        let _env = crate::test_support::lock_test_env();
        let secrets_root = tempfile::tempdir().expect("isolated remote-control secrets");
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", secrets_root.path());
        let server = MockServer::start().await;
        let plane = format!("{}/", server.uri().trim_end_matches('/'));
        let _control_plane =
            crate::test_support::EnvVarGuard::set("CWC_RUNNER_CONTROL_PLANE_BASE", &plane);
        let base = runner_control_plane_base().expect("loopback control plane");
        save_persisted_enrollment(&fixture_enrollment(&base).persisted)
            .expect("persist matching enrollment");

        let access_token = crate::test_support::future_test_jwt(&"a".repeat(40));
        Mock::given(method("POST"))
            .and(path("/api/runner/enrollments/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "enrollment": {
                    "id": "enrollment_fixture",
                    "userId": "account_fixture",
                    "deviceId": "device_fixture",
                    "runtimeVersion": "0.9.6",
                    "runtimeCommit": "a".repeat(40),
                    "capabilities": CAPABILITIES,
                },
                "credential": { "accessToken": access_token },
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/local-runners/connect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_connection_response()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/local-runners/runner_fixture/heartbeat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .mount(&server)
            .await;
        let polls = ControlPollCounter::default();
        Mock::given(method("GET"))
            .and(path("/api/local-runners/runner_fixture/runs"))
            .respond_with(polls.clone())
            .mount(&server)
            .await;

        let runtime_root = tempfile::tempdir().expect("idle Runtime Chat host");
        let host = RuntimeChatRelayHost::open(
            crate::config::Config::default(),
            Arc::new(crate::plugins::PluginRegistry::empty(runtime_root.path())),
            runtime_root.path().to_path_buf(),
            "target_fixture".to_string(),
            "session_fixture".to_string(),
        )
        .expect("open idle Runtime Chat host");
        let start = fixture_start();
        let (_worker_tx, worker_rx) = mpsc::unbounded_channel();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(async move {
            let mut phase = RelayPhase::Enrolling;
            relay_worker(start, Some(host), worker_rx, event_tx, &mut phase).await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match event_rx.recv().await {
                    Some(RemoteEvent::Connected { .. }) => break,
                    Some(RemoteEvent::Notice(_)) => {}
                    Some(RemoteEvent::Failed(error)) => {
                        panic!("relay worker failed before attach: {error}");
                    }
                    Some(RemoteEvent::FailedPreLease(error)) => {
                        panic!("relay worker failed before attach: {error}");
                    }
                    Some(_) => panic!("relay worker emitted an unexpected event before attach"),
                    None => panic!("relay worker stopped before attach"),
                }
            }
        })
        .await
        .expect("relay worker should attach");

        tokio::time::sleep(RUNTIME_UPLOAD_RETRY_INTERVAL.saturating_mul(2)).await;
        assert_eq!(
            polls.hits.load(Ordering::SeqCst),
            0,
            "the first control poll must still wait SYNC_INTERVAL while idle chat ticks fire"
        );
        tokio::time::sleep(SYNC_INTERVAL).await;
        assert!(
            polls.hits.load(Ordering::SeqCst) >= 1,
            "idle Runtime Chat ticks must not reset the control poll"
        );
        worker.abort();
        let _ = worker.await;
    }

    #[test]
    fn preattachment_failure_cannot_release_a_recovered_unsettled_turn() {
        let root = tempfile::tempdir().unwrap();
        let registry = Arc::new(crate::plugins::PluginRegistry::empty(root.path()));
        let mut controller = RemoteControlController::default();
        controller
            .configure_runtime_chat(
                crate::config::Config::default(),
                Arc::clone(&registry),
                root.path().join("runtime-chat"),
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        let host = controller.runtime_chat.as_ref().unwrap();
        host.bind_account("account_fixture", "target_fixture")
            .unwrap();
        host.install_unsettled_turn_for_tests(
            "run_old",
            "thr_recovered",
            &format!("local_thread_{}", "a".repeat(24)),
            &format!("local_turn_{}", "b".repeat(24)),
        )
        .unwrap();
        let retained_model = host.configured_default_model_for_tests();
        controller
            .configure_runtime_chat(
                crate::config::Config {
                    provider: Some("ollama".to_string()),
                    default_text_model: Some("must-not-replace-live-host".to_string()),
                    ..crate::config::Config::default()
                },
                Arc::clone(&registry),
                root.path().join("runtime-chat"),
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        assert_eq!(
            controller
                .runtime_chat
                .as_ref()
                .unwrap()
                .configured_default_model_for_tests(),
            retained_model,
            "an unsettled same-scope turn must retain its lifetime manager"
        );
        controller.status = Status::Failed;
        controller.ownership_blocked_until = Some(Instant::now() - Duration::from_millis(1));

        assert!(controller.try_next_event().is_none());
        assert_eq!(controller.status, Status::Failed);
        assert!(controller.runtime_chat.is_some());
        assert!(
            controller
                .configure_runtime_chat(
                    crate::config::Config::default(),
                    registry,
                    root.path().join("runtime-chat"),
                    "target_other".to_string(),
                    "session_other".to_string(),
                )
                .is_err()
        );
    }

    #[test]
    fn runtime_chat_reenrollment_rejects_attachment_from_a_different_account() {
        let root = tempfile::tempdir().unwrap();
        let registry = Arc::new(crate::plugins::PluginRegistry::empty(root.path()));
        let (mut controller, _worker_rx, event_tx, _journal_root) = wired_controller();
        controller
            .configure_runtime_chat(
                crate::config::Config::default(),
                registry,
                root.path().join("runtime-chat"),
                "target_fixture".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        controller
            .runtime_chat
            .as_ref()
            .unwrap()
            .bind_account("account_a", "target_fixture")
            .unwrap();
        controller.account_ref = Some("account_a".to_string());
        controller.target_ref = Some("target_fixture".to_string());
        controller.attached_run_id = Some("run_account_a".to_string());

        event_tx
            .send(RemoteEvent::Attachment {
                account_ref: "account_b".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_account_b".to_string(),
                    workspace_id: "workspace_account_b".to_string(),
                    runtime_cursor: 0,
                    snapshot_present: false,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "b".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();

        let event = controller.try_next_event().unwrap();
        let RemoteEvent::Failed(error) = event else {
            panic!("an account-switch attachment must fail closed");
        };
        assert!(error.contains("belongs to another account"), "{error}");
        assert_eq!(controller.account_ref.as_deref(), Some("account_a"));
        assert_eq!(controller.attached_run_id.as_deref(), Some("run_account_a"));
        assert_eq!(controller.status, Status::Failed);
        assert!(controller.worker_tx.is_none());
        assert!(controller.event_rx.is_none());
        for command in [
            RemoteCommand::Prompt {
                turn_id: "turn_rejected".to_string(),
                prompt: "must not execute".to_string(),
            },
            RemoteCommand::Approval {
                gate: "local_approval_rejected".to_string(),
                approved: true,
            },
        ] {
            assert!(
                controller
                    .claim_command("run_account_b", 1, &command)
                    .is_err(),
                "rejected account command must not be claimable: {command:?}"
            );
        }
    }

    #[tokio::test]
    async fn connect_request_opts_into_runtime_chat_without_exposing_local_state() {
        let server = MockServer::start().await;
        let enrollment = fixture_enrollment(&format!("{}/", server.uri()));
        let start = fixture_start();
        let expected = json!({
            "deviceId": "device_fixture",
            "targetRef": "target_fixture",
            "displayLabel": "private-project",
            "runtimeVersion": "0.9.6",
            "runtimeCommit": "a".repeat(40),
            "capabilities": CAPABILITIES,
            "status": "active",
            "sessionRef": "session:fixture@01",
            "runtimeChatRelayProtocol": RUNTIME_CHAT_RELAY_PROTOCOL
        });
        assert_eq!(connect_runner_body(&enrollment, &start), expected);
        for forbidden in [
            "sessionId",
            "workspacePath",
            "path",
            "prompt",
            "environment",
            "env",
            "token",
            "credential",
        ] {
            assert!(expected.get(forbidden).is_none(), "leaked {forbidden}");
        }
        Mock::given(method("POST"))
            .and(path("/api/local-runners/connect"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_connection_response()))
            .expect(1)
            .mount(&server)
            .await;
        let client = crate::tls::reqwest_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("fixture client");

        let connection = connect_runner(&client, &enrollment, &start)
            .await
            .expect("strict runner attachment");

        assert_eq!(connection.runner_id, "runner_fixture");
        assert_eq!(connection.attachment.run_id, "run_fixture");
        assert_eq!(connection.attachment.runtime_cursor, 41);
        assert!(!connection.attachment.snapshot_present);
        assert_eq!(
            connection.attachment.runtime_chat_relay_protocol,
            RUNTIME_CHAT_RELAY_PROTOCOL
        );
        assert_eq!(
            connection.attachment.runtime_chat_relay_challenge,
            "a".repeat(32)
        );
    }

    #[test]
    fn connection_without_links_yields_no_urls() {
        let enrollment = fixture_enrollment("https://api.codewhale.net/");
        let start = fixture_start();
        let connection =
            parse_runner_connection(&fixture_connection_response(), &enrollment, &start)
                .expect("legacy lease without links still attaches");
        assert_eq!(connection.links, RemoteLinks::default());
        assert!(connection.links.run_url.is_none());
        assert!(connection.links.computer_url.is_none());
    }

    #[test]
    fn connection_links_are_parsed_from_the_runner_lease() {
        let enrollment = fixture_enrollment("https://api.codewhale.net/");
        let start = fixture_start();
        let mut value = fixture_connection_response();
        value["runner"]["runUrl"] = json!("https://app.codewhale.net/session?run=run_fixture");
        value["runner"]["computerUrl"] =
            json!("https://app.codewhale.net/settings?section=workspaces");
        let connection = parse_runner_connection(&value, &enrollment, &start)
            .expect("lease with links attaches");
        assert_eq!(
            connection.links.run_url.as_deref(),
            Some("https://app.codewhale.net/session?run=run_fixture")
        );
        assert_eq!(
            connection.links.computer_url.as_deref(),
            Some("https://app.codewhale.net/settings?section=workspaces")
        );
    }

    #[test]
    fn connection_links_off_origin_or_for_another_run_are_dropped_not_fatal() {
        let enrollment = fixture_enrollment("https://api.codewhale.net/");
        let start = fixture_start();
        for spoofed in [
            "http://app.codewhale.net/session?run=run_fixture",
            "https://app.codewhale.net.evil.example/session?run=run_fixture",
            "https://evil.example/session?run=run_fixture",
            "https://user:pw@app.codewhale.net/session?run=run_fixture",
            "https://app.codewhale.net:8443/session?run=run_fixture",
            "https://app.codewhale.net/session?run=run_fixture#token=abc",
            "https://app.codewhale.net/session?run=run_other",
            "https://app.codewhale.net/session?run=run_fixture&next=https://evil.example",
            "https://app.codewhale.net/logout?run=run_fixture",
            "",
            "not a url",
        ] {
            let mut value = fixture_connection_response();
            value["runner"]["runUrl"] = json!(spoofed);
            value["runner"]["computerUrl"] = json!(spoofed);
            let connection = parse_runner_connection(&value, &enrollment, &start)
                .expect("a bad link must not break the attachment");
            assert!(
                connection.links.run_url.is_none(),
                "run link accepted: {spoofed}"
            );
            assert!(
                connection.links.computer_url.is_none(),
                "computer link accepted: {spoofed}"
            );
        }
        // A non-string value is treated as absent.
        let mut value = fixture_connection_response();
        value["runner"]["runUrl"] = json!(42);
        let connection = parse_runner_connection(&value, &enrollment, &start).unwrap();
        assert!(connection.links.run_url.is_none());
    }

    #[test]
    fn banner_leads_with_the_session_link_when_present() {
        let with_link = remote_control_banner(
            "account_fixture",
            "runner_fixture",
            Some("https://app.codewhale.net/session?run=run_fixture"),
        );
        assert_eq!(
            with_link,
            "WEB MIRROR · https://app.codewhale.net/session?run=run_fixture · /rc stop"
        );
        let without_link = remote_control_banner("account_fixture", "runner_fixture", None);
        assert_eq!(
            without_link,
            "WEB MIRROR · account account_fixture · runner runner_fixture · /rc stop"
        );
        let notice =
            remote_control_link_notice("https://app.codewhale.net/session?run=run_fixture");
        assert!(notice.starts_with(
            "Remote control is live at https://app.codewhale.net/session?run=run_fixture"
        ));
        assert!(notice.contains("/rc open"));
        assert!(notice.contains("/rc link"));
    }

    #[test]
    fn controller_exposes_links_only_while_connected() {
        let mut controller = RemoteControlController::default();
        assert!(controller.run_url().is_none());
        let (worker_tx, _worker_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        controller.worker_tx = Some(worker_tx);
        controller.event_rx = Some(event_rx);
        event_tx
            .send(RemoteEvent::Connected {
                account_ref: "account_fixture".to_string(),
                runner_id: "runner_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_fixture".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 0,
                    snapshot_present: false,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks {
                    run_url: Some("https://app.codewhale.net/session?run=run_fixture".to_string()),
                    computer_url: Some(
                        "https://app.codewhale.net/settings?section=workspaces".to_string(),
                    ),
                },
            })
            .unwrap();
        controller.try_next_event().unwrap();
        assert_eq!(
            controller.run_url(),
            Some("https://app.codewhale.net/session?run=run_fixture")
        );
        assert_eq!(
            controller.computer_url(),
            Some("https://app.codewhale.net/settings?section=workspaces")
        );
        assert!(
            controller
                .status_line()
                .contains("open https://app.codewhale.net/session?run=run_fixture")
        );

        event_tx.send(RemoteEvent::Stopped).unwrap();
        controller.try_next_event().unwrap();
        assert!(controller.run_url().is_none());
        assert!(controller.computer_url().is_none());
    }

    #[test]
    fn connecting_never_blocks_local_prompts_and_shares_approvals_only_once_attached() {
        let mut controller = RemoteControlController::default();
        controller.status = Status::Connecting;
        controller.status_detail = "waiting for account authorization".to_string();
        // Mirror semantics: there is no local-input gate at all. The only
        // shared-decision surface is the approval card, and only once a
        // typed turn is bound.
        assert!(
            !controller.can_share_approval_with_web(),
            "without a confirmed run cursor the web cannot receive an approval"
        );

        controller.status = Status::Connected;
        controller.attached_run_id = Some("run_fixture".to_string());
        assert!(
            !controller.can_share_approval_with_web(),
            "a connected idle session has no typed turn to receive approvals"
        );
        assert!(controller.attach_current_local_turn(Some("turn_fixture")));
        assert!(controller.can_share_approval_with_web());
    }

    #[test]
    fn persisted_device_identity_survives_a_reload_and_outlives_enrollments() {
        let (minted, needs_save) = resolve_device_identity(None, None);
        assert!(needs_save);
        assert!(minted.starts_with("device_"));
        assert!(valid_opaque_ref(&minted));

        let identity = PersistedDeviceIdentity {
            schema_version: 1,
            device_id: minted.clone(),
        };
        let raw = serde_json::to_string(&identity).expect("encode device identity");
        let reloaded: PersistedDeviceIdentity =
            serde_json::from_str(&raw).expect("decode device identity");
        assert_eq!(reloaded, identity);

        // A saved identity wins over any enrollment's id and needs no re-save.
        let (resolved, needs_save) =
            resolve_device_identity(Some(reloaded.clone()), Some("device_enrolled"));
        assert_eq!(resolved, minted);
        assert!(!needs_save);

        // Without a saved identity, an existing enrollment's device id is
        // adopted (upgrading terminals keep their computer row) and persisted.
        let (adopted, needs_save) = resolve_device_identity(None, Some("device_enrolled"));
        assert_eq!(adopted, "device_enrolled");
        assert!(needs_save);

        // An unreadable identity is replaced, never fatal.
        let broken = PersistedDeviceIdentity {
            schema_version: 2,
            device_id: "x".to_string(),
        };
        let (replaced, needs_save) = resolve_device_identity(Some(broken), None);
        assert_ne!(replaced, "x");
        assert!(needs_save);
        assert!(serde_json::from_str::<PersistedDeviceIdentity>("{\"deviceId\":\"a\"}").is_err());
    }

    #[test]
    fn remote_control_receipts_never_select_the_system_keychain() {
        assert!(
            remote_control_secrets()
                .backend_name()
                .starts_with("file-based"),
            "/rc enrollment must not trigger an OS keychain prompt"
        );
    }

    #[test]
    fn attachment_response_validation_fails_closed() {
        let enrollment = fixture_enrollment("https://api.codewhale.net/");
        let start = fixture_start();
        let valid = fixture_connection_response();
        assert!(parse_runner_connection(&valid, &enrollment, &start).is_ok());

        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove("attachment");
        assert!(parse_runner_connection(&missing, &enrollment, &start).is_err());

        let mut oversized_cursor = valid.clone();
        oversized_cursor["attachment"]["runtimeCursor"] = json!(JS_MAX_SAFE_INTEGER + 1);
        assert!(parse_runner_connection(&oversized_cursor, &enrollment, &start).is_err());

        let mut false_receipt = valid.clone();
        false_receipt["attachment"]["snapshotPresent"] = json!("false");
        assert!(parse_runner_connection(&false_receipt, &enrollment, &start).is_err());

        let mut extra_authority = valid.clone();
        extra_authority["attachment"]["workspacePath"] = json!("/private/project");
        assert!(parse_runner_connection(&extra_authority, &enrollment, &start).is_err());

        let mut wrong_protocol = valid.clone();
        wrong_protocol["attachment"]["runtimeChatRelayProtocol"] = json!("legacy");
        assert!(parse_runner_connection(&wrong_protocol, &enrollment, &start).is_err());

        let mut invalid_challenge = valid.clone();
        invalid_challenge["attachment"]["runtimeChatRelayChallenge"] = json!("too-short");
        assert!(parse_runner_connection(&invalid_challenge, &enrollment, &start).is_err());

        let mut wrong_control_path = valid;
        wrong_control_path["runner"]["controlPath"] = json!("direct_native");
        assert!(parse_runner_connection(&wrong_control_path, &enrollment, &start).is_err());
    }

    #[test]
    fn attachment_cursor_seeds_the_first_runtime_event_sequence() {
        let (mut controller, mut worker_rx, event_tx, _journal_root) = wired_controller();
        event_tx
            .send(RemoteEvent::Connected {
                account_ref: "account_fixture".to_string(),
                runner_id: "runner_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_fixture".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 41,
                    snapshot_present: false,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();

        assert!(matches!(
            controller.try_next_event(),
            Some(RemoteEvent::Connected { .. })
        ));
        controller.upload_snapshot("run_fixture", &[]);

        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("expected snapshot upload");
        };
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0]["event"], "session.snapshot");
        assert_eq!(envelopes[0]["seq"], 42);
    }

    #[test]
    fn connected_attachment_adopts_the_existing_turn_and_streams_typed_state_once() {
        let (mut controller, mut worker_rx, event_tx, _journal_root) = wired_controller();
        event_tx
            .send(RemoteEvent::Connected {
                account_ref: "account_fixture".to_string(),
                runner_id: "runner_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_fixture".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 11,
                    snapshot_present: false,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();
        assert!(matches!(
            controller.try_next_event(),
            Some(RemoteEvent::Connected { .. })
        ));
        assert!(
            !controller.can_share_approval_with_web(),
            "a connected attachment without a bound typed turn keeps approvals local"
        );
        assert!(controller.attach_current_local_turn(Some("turn_existing")));
        assert!(
            controller.can_share_approval_with_web(),
            "the bound active turn can carry typed approvals to the web"
        );
        assert!(controller.has_active_run());
        assert!(controller.active_run_matches("run_fixture"));
        assert!(
            !controller.attach_current_local_turn(Some("turn_duplicate")),
            "replaying the attachment must not replace or duplicate the turn"
        );

        controller.observe_engine_event(&EngineEvent::MessageDelta {
            index: 0,
            content: "existing turn output".to_string(),
        });
        controller.observe_engine_event(&EngineEvent::ToolCallStarted {
            model_call: None,
            id: "tool_existing".to_string(),
            name: "shell".to_string(),
            input: json!({ "never": "relayed" }),
        });
        let gate = controller.record_remote_approval(
            "tool_existing",
            "shell",
            "approve bounded fixture",
            &json!({ "credential": "must-not-cross" }),
            "approval-key",
            None,
        );

        let mut envelopes = Vec::new();
        for _ in 0..3 {
            let WorkerCommand::Upload {
                envelopes: batch, ..
            } = worker_rx.try_recv().expect("typed active-turn upload")
            else {
                panic!("active-turn state must use the runtime envelope channel");
            };
            envelopes.extend(batch);
        }
        assert_eq!(
            envelopes
                .iter()
                .map(|event| event["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["item.delta", "item.started", "approval.required"]
        );
        assert!(
            envelopes
                .iter()
                .all(|event| event["turn_id"] == "turn_existing")
        );
        assert_eq!(
            envelopes[2]["payload"]["approval_id"].as_str(),
            Some(gate.as_str())
        );
        let projected = serde_json::to_string(&envelopes).unwrap();
        assert!(!projected.contains("must-not-cross"));
        assert!(!projected.contains("approval-key"));
        assert!(worker_rx.try_recv().is_err());
    }

    #[test]
    fn dispatch_window_attachment_promotes_on_typed_start_and_reconnect_is_idempotent() {
        let (mut controller, mut worker_rx, event_tx, _journal_root) = wired_controller();
        let attachment = RemoteAttachment {
            run_id: "run_fixture".to_string(),
            workspace_id: "workspace_fixture".to_string(),
            runtime_cursor: 3,
            snapshot_present: false,
            runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
            runtime_chat_relay_challenge: "a".repeat(32),
        };
        event_tx
            .send(RemoteEvent::Connected {
                account_ref: "account_fixture".to_string(),
                runner_id: "runner_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: attachment.clone(),
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();

        assert!(controller.attach_current_local_turn(None));
        let pending_lease = controller
            .journal
            .as_ref()
            .and_then(RuntimeEventJournal::classic_lease)
            .expect("dispatch-window lease is durable");
        assert!(pending_lease.turn_id.is_none());
        assert!(
            controller.has_active_run(),
            "the dispatch window gates stop"
        );
        assert!(controller.active_run_matches("run_fixture"));
        assert!(
            !controller.can_share_approval_with_web(),
            "a pending dispatch has no typed turn id, so approvals stay local"
        );
        assert!(worker_rx.try_recv().is_err());

        controller.observe_engine_event(&EngineEvent::TurnStarted {
            turn_id: "turn_started_later".to_string(),
            created_at: chrono::Utc::now(),
            route: None,
            submission_id: None,
        });
        let WorkerCommand::Upload { envelopes, .. } =
            worker_rx.try_recv().expect("one typed turn start")
        else {
            panic!("turn start must use the runtime envelope channel");
        };
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0]["seq"], 4);
        assert_eq!(envelopes[0]["event"], "turn.started");
        assert_eq!(envelopes[0]["turn_id"], "turn_started_later");
        let promoted_lease = controller
            .journal
            .as_ref()
            .and_then(RuntimeEventJournal::classic_lease)
            .expect("typed start keeps the durable lease");
        assert_eq!(
            promoted_lease.turn_id.as_deref(),
            Some("turn_started_later")
        );
        assert_eq!(
            promoted_lease.lease_id, pending_lease.lease_id,
            "typed start promotes the same dispatch generation"
        );
        let started_envelope = envelopes[0].clone();
        assert!(
            controller.can_share_approval_with_web(),
            "typed TurnStarted promotes the dispatch into a web-owned active turn"
        );

        event_tx
            .send(RemoteEvent::Attachment {
                account_ref: "account_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment,
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();
        assert!(
            !controller.attach_current_local_turn(Some("turn_replayed")),
            "a reconnect must not rebind the live turn"
        );
        let WorkerCommand::Upload { envelopes, .. } = worker_rx
            .try_recv()
            .expect("the unacknowledged start is replayed unchanged")
        else {
            panic!("runtime replay must use the envelope channel");
        };
        assert_eq!(
            envelopes,
            vec![started_envelope],
            "retry safety preserves the original sequence and payload"
        );
        assert!(worker_rx.try_recv().is_err());

        controller.observe_engine_event(&turn_complete_event());
        let WorkerCommand::Upload { envelopes, .. } =
            worker_rx.try_recv().expect("one terminal receipt")
        else {
            panic!("turn completion must use the runtime envelope channel");
        };
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0]["seq"], 5);
        assert_eq!(envelopes[0]["event"], "turn.completed");
        assert_eq!(envelopes[0]["turn_id"], "turn_started_later");
        assert!(!controller.has_active_run());
        assert!(worker_rx.try_recv().is_err());
    }

    #[test]
    fn dispatch_window_that_ends_before_typed_start_returns_to_idle_attachment() {
        let (mut controller, mut worker_rx, event_tx, _journal_root) = wired_controller();
        event_tx
            .send(RemoteEvent::Connected {
                account_ref: "account_fixture".to_string(),
                runner_id: "runner_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_fixture".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 8,
                    snapshot_present: false,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();

        assert!(controller.attach_current_local_turn(None));
        assert!(controller.has_active_run());
        assert!(controller.release_unstarted_local_turn());
        assert!(!controller.has_active_run());
        assert!(!controller.can_share_approval_with_web());
        assert!(
            !controller.release_unstarted_local_turn(),
            "reconciling the same idle boundary is idempotent"
        );
        assert!(worker_rx.try_recv().is_err());
    }

    #[test]
    fn fresh_controller_refreshes_old_server_snapshot_then_deduplicates_reconnects() {
        let (mut controller, mut worker_rx, event_tx, _journal_root) = wired_controller();
        event_tx
            .send(RemoteEvent::Connected {
                account_ref: "account_fixture".to_string(),
                runner_id: "runner_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_fixture".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 7,
                    snapshot_present: true,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();

        controller.upload_snapshot("run_fixture", &[]);
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("fresh controller must refresh saved history");
        };
        assert_eq!(envelopes[0]["seq"], 8);

        event_tx
            .send(RemoteEvent::Attachment {
                account_ref: "account_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_fixture".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 7,
                    snapshot_present: false,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();
        controller.upload_snapshot("run_fixture", &[]);
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("unacknowledged snapshot must be retried");
        };
        assert_eq!(envelopes[0]["seq"], 8);
        controller.upload_snapshot("run_fixture", &[]);
        assert!(worker_rx.try_recv().is_err());

        event_tx
            .send(RemoteEvent::Attachment {
                account_ref: "account_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_fixture".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 8,
                    snapshot_present: true,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();
        controller.upload_snapshot("run_fixture", &[]);
        assert!(worker_rx.try_recv().is_err());
    }

    #[test]
    fn reconnect_cursor_retires_only_the_acknowledged_prefix() {
        let (mut controller, mut worker_rx, event_tx, _journal_root) = wired_controller();
        controller.event_seq.insert("run_fixture".to_string(), 6);
        controller.upload_envelope(
            "run_fixture",
            "item.delta",
            None,
            json!({ "delta": "seven" }),
        );
        controller.upload_envelope(
            "run_fixture",
            "item.delta",
            None,
            json!({ "delta": "eight" }),
        );
        worker_rx.try_recv().unwrap();
        worker_rx.try_recv().unwrap();

        event_tx
            .send(RemoteEvent::Attachment {
                account_ref: "account_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_fixture".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 7,
                    snapshot_present: false,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();

        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("seq 8 must remain pending");
        };
        assert_eq!(envelopes.len(), 1);
        assert_eq!(envelopes[0]["seq"], 8);
        assert_eq!(
            controller
                .pending_runtime_events
                .get("run_fixture")
                .unwrap()
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![8]
        );

        event_tx
            .send(RemoteEvent::Attachment {
                account_ref: "account_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_fixture".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 6,
                    snapshot_present: false,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("older cursor cannot discard seq 8");
        };
        assert_eq!(envelopes[0]["seq"], 8);

        event_tx
            .send(RemoteEvent::RuntimeCursor {
                run_id: "run_fixture".to_string(),
                cursor: 8,
            })
            .unwrap();
        controller.try_next_event().unwrap();
        assert!(
            !controller
                .pending_runtime_events
                .contains_key("run_fixture")
        );
    }

    #[tokio::test]
    async fn ambiguous_success_retries_the_identical_runtime_event_until_cursor_acceptance() {
        let server = MockServer::start().await;
        let responder = AmbiguousRuntimeResponder::default();
        Mock::given(method("POST"))
            .and(path(
                "/api/local-runners/runner_fixture/runs/run_fixture/events",
            ))
            .respond_with(responder.clone())
            .expect(2)
            .mount(&server)
            .await;
        let enrollment = fixture_enrollment(&format!("{}/", server.uri()));
        let client = crate::tls::reqwest_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("fixture client");
        let envelope = runtime_envelope(
            1,
            "item.delta",
            None,
            "2026-08-08T12:00:00Z".to_string(),
            json!({ "delta": "exact body" }),
        );
        let mut outbox = RuntimeTransportOutbox::default();
        outbox
            .enqueue("run_fixture", envelope)
            .expect("queue runtime event");

        assert_eq!(
            outbox
                .try_flush_one(&client, &enrollment, "runner_fixture")
                .await
                .unwrap(),
            RuntimeFlushOutcome::Retryable
        );
        assert_eq!(outbox.events.len(), 1);
        assert_eq!(
            outbox
                .try_flush_one(&client, &enrollment, "runner_fixture")
                .await
                .unwrap(),
            RuntimeFlushOutcome::Accepted {
                run_id: "run_fixture".to_string(),
                cursor: 1,
            }
        );
        assert!(outbox.events.is_empty());
        let bodies = responder.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0], bodies[1]);
    }

    #[test]
    fn snapshot_envelope_is_unicode_safe_and_keeps_newest_history() {
        let messages = (0..80)
            .map(|index| {
                let marker = format!("message-{index:02}-");
                text_message(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    marker + &"🫧\"\\\n".repeat(1_500),
                )
            })
            .collect::<Vec<_>>();

        let envelope = bounded_session_snapshot_envelope(1, &messages);
        let encoded = serde_json::to_vec(&envelope).unwrap();
        let retained = envelope["payload"]["messages"].as_array().unwrap();

        assert!(encoded.len() <= SNAPSHOT_ENVELOPE_BYTE_BUDGET);
        assert!(encoded.len() < MAX_RUNTIME_ENVELOPE_BYTES);
        assert!(!retained.is_empty());
        assert!(retained.len() <= MAX_SNAPSHOT_MESSAGES);
        assert!(
            retained.last().unwrap()["text"]
                .as_str()
                .unwrap()
                .starts_with("message-79-")
        );
        for message in retained {
            let text = message["text"].as_str().unwrap();
            assert!(!text.contains('\u{FFFD}'));
            assert!(text.is_char_boundary(text.len()));
        }
    }

    #[test]
    fn snapshot_truncation_pins_the_exact_encoded_byte_boundary() {
        let source = "🫧\"\\\n".repeat(40_000);
        let message = text_message("assistant", source);
        let envelope = bounded_session_snapshot_envelope(9, std::slice::from_ref(&message));
        let encoded = serde_json::to_vec(&envelope).unwrap();
        assert!(encoded.len() <= SNAPSHOT_ENVELOPE_BYTE_BUDGET);

        let retained = envelope["payload"]["messages"][0]["text"].as_str().unwrap();
        let projected = project_session_message(&message).unwrap().1;
        let retained_chars = retained.chars().count();
        let next = projected.chars().nth(retained_chars).unwrap();
        let mut one_more = retained.to_string();
        one_more.push(next);
        let timestamp = envelope["timestamp"].as_str().unwrap();
        let expanded = vec![json!({ "role": "assistant", "text": one_more })];
        assert!(
            snapshot_envelope_len(9, timestamp, &expanded) > SNAPSHOT_ENVELOPE_BYTE_BUDGET,
            "one more Unicode scalar must cross the chosen encoded boundary"
        );
    }

    #[test]
    fn fatal_engine_error_projects_failure_and_releases_the_remote_run() {
        let (mut controller, mut worker_rx, _event_tx, _journal_root) = wired_controller();
        controller
            .activate_prompt("run_fixture", "turn_fixture")
            .unwrap();
        let secret = "sk-runtime-secret-that-must-not-cross-the-relay";
        let message = format!(
            "DeepSeek API key: {secret}\n{}",
            "🫧".repeat(MAX_REMOTE_ERROR_MESSAGE_BYTES)
        );

        controller.observe_engine_event(&EngineEvent::Error {
            envelope: crate::error_taxonomy::ErrorEnvelope::new(
                crate::error_taxonomy::ErrorCategory::Authentication,
                crate::error_taxonomy::ErrorSeverity::Critical,
                false,
                "llm_auth_error",
                message,
            ),
            recoverable: false,
        });

        assert!(!controller.has_active_run());
        let WorkerCommand::Upload {
            envelopes: failed, ..
        } = worker_rx.try_recv().expect("fatal item upload")
        else {
            panic!("fatal error must upload an item.failed envelope");
        };
        let WorkerCommand::Upload {
            envelopes: completed,
            ..
        } = worker_rx.try_recv().expect("fatal turn upload")
        else {
            panic!("fatal error must upload a terminal turn envelope");
        };
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0]["seq"], 1);
        assert_eq!(failed[0]["event"], "item.failed");
        assert_eq!(failed[0]["turn_id"], "turn_fixture");
        assert_eq!(failed[0]["payload"]["item"]["kind"], "error");
        assert_eq!(failed[0]["payload"]["item"]["status"], "failed");
        let projected = failed[0]["payload"]["item"]["detail"]
            .as_str()
            .expect("bounded error detail");
        assert!(projected.len() <= MAX_REMOTE_ERROR_MESSAGE_BYTES);
        assert!(projected.is_char_boundary(projected.len()));
        assert!(!projected.contains(secret));
        assert!(projected.contains("[redacted]"));

        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0]["seq"], 2);
        assert_eq!(completed[0]["event"], "turn.completed");
        assert_eq!(completed[0]["turn_id"], "turn_fixture");
        assert_eq!(completed[0]["payload"]["turn"]["status"], "failed");
        assert_eq!(
            controller.pending_runtime_events["run_fixture"]
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(worker_rx.try_recv().is_err());
    }

    #[test]
    fn recoverable_engine_error_stays_nonterminal_for_provider_fallback() {
        let mut controller = RemoteControlController::default();
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        controller.worker_tx = Some(worker_tx);
        controller
            .activate_prompt("run_fixture", "turn_fixture")
            .unwrap();

        controller.observe_engine_event(&EngineEvent::Error {
            envelope: crate::error_taxonomy::ErrorEnvelope::network(
                "temporary provider connection failure",
            ),
            recoverable: true,
        });

        assert!(controller.active_run_matches("run_fixture"));
        assert!(controller.pending_runtime_events.is_empty());
        assert!(worker_rx.try_recv().is_err());
    }

    #[test]
    fn terminal_pre_dispatch_error_uses_the_same_failure_projection() {
        let (mut controller, mut worker_rx, _event_tx, _journal_root) = wired_controller();
        controller
            .activate_prompt("run_fixture", "turn_fixture")
            .unwrap();

        controller.fail_active_dispatch(
            "DeepSeek API key: sk-preflight-secret-that-must-not-cross-the-relay",
        );

        assert!(!controller.has_active_run());
        let WorkerCommand::Upload { envelopes, .. } =
            worker_rx.try_recv().expect("pre-dispatch item upload")
        else {
            panic!("pre-dispatch error must upload item.failed");
        };
        assert_eq!(envelopes[0]["event"], "item.failed");
        assert!(!envelopes[0].to_string().contains("sk-preflight-secret"));
        let WorkerCommand::Upload { envelopes, .. } =
            worker_rx.try_recv().expect("pre-dispatch turn upload")
        else {
            panic!("pre-dispatch error must upload turn.completed");
        };
        assert_eq!(envelopes[0]["event"], "turn.completed");
        assert_eq!(envelopes[0]["payload"]["turn"]["status"], "failed");
        assert!(worker_rx.try_recv().is_err());
    }

    #[test]
    fn typed_command_parser_rejects_shell_and_cross_run_content() {
        let prompt = parse_remote_command(
            &json!({
                "type": "prompt.request",
                "runId": "run-1",
                "turnId": "turn-1",
                "prompt": "Continue",
            }),
            "run-1",
        )
        .unwrap();
        assert_eq!(
            prompt,
            RemoteCommand::Prompt {
                turn_id: "turn-1".to_string(),
                prompt: "Continue".to_string(),
            }
        );
        assert!(
            parse_remote_command(
                &json!({
                    "type": "shell",
                    "runId": "run-1",
                    "command": "rm -rf /",
                }),
                "run-1"
            )
            .is_err()
        );
        assert!(
            parse_remote_command(
                &json!({
                    "type": "prompt.request",
                    "runId": "run-other",
                    "turnId": "turn-1",
                    "prompt": "Continue",
                }),
                "run-1"
            )
            .is_err()
        );
    }

    fn runtime_chat_prompt_fixture() -> Value {
        json!({
            "type": "prompt.request",
            "runId": "run-1",
            "turnId": format!("local_turn_{}", "b".repeat(24)),
            "operationKey": "operation-1",
            "runtimeBindingId": "binding-1",
            "runtimeThreadId": format!("local_thread_{}", "a".repeat(24)),
            "prompt": "Hello from account Chat",
            "systemPrompt": "Answer directly.",
            "model": "model-1",
            "modelProvider": "ollama",
            "modelProviderId": "ollama",
            "reasoningEffort": "high",
            "allowedTools": [],
            "mode": "chat",
            "requestedMode": "chat",
            "workspace": {
                "id": "workspace-1",
                "targetRef": "target-1"
            }
        })
    }

    #[test]
    fn runtime_chat_parser_requires_exact_route_tools_mode_and_interrupt_scope() {
        let command = parse_remote_command(&runtime_chat_prompt_fixture(), "run-1").unwrap();
        assert!(matches!(command, RemoteCommand::RuntimeChatPrompt(_)));

        for mutation in [
            ("allowedTools", json!(["bash"])),
            ("mode", json!("work")),
            ("requestedMode", json!("operate")),
            ("modelProvider", json!("https://provider.invalid")),
        ] {
            let mut value = runtime_chat_prompt_fixture();
            value[mutation.0] = mutation.1;
            assert!(parse_remote_command(&value, "run-1").is_err());
        }
        let mut unknown = runtime_chat_prompt_fixture();
        unknown["workspacePath"] = json!("/private/workspace");
        assert!(parse_remote_command(&unknown, "run-1").is_err());
        let mut missing_markers = runtime_chat_prompt_fixture();
        let record = missing_markers.as_object_mut().unwrap();
        record.remove("operationKey");
        record.remove("runtimeBindingId");
        record.remove("runtimeThreadId");
        assert!(
            parse_remote_command(&missing_markers, "run-1").is_err(),
            "a rich Chat-shaped payload cannot downgrade into a legacy Work prompt"
        );

        let interrupt = json!({
            "type": "run.control",
            "runId": "run-1",
            "action": "interrupt",
            "reason": "Stop this turn",
            "turnId": format!("local_turn_{}", "b".repeat(24)),
            "runtimeBindingId": "binding-1",
            "runtimeThreadId": format!("local_thread_{}", "a".repeat(24))
        });
        assert!(matches!(
            parse_remote_command(&interrupt, "run-1").unwrap(),
            RemoteCommand::Control {
                runtime_chat: Some(_),
                ..
            }
        ));
        let mut drifted = interrupt;
        drifted["runtimeThreadId"] = json!(format!("local_thread_{}", "F".repeat(24)));
        assert!(parse_remote_command(&drifted, "run-1").is_err());
    }

    #[test]
    fn legacy_control_requires_and_matches_the_exact_active_turn() {
        assert!(
            parse_remote_command(
                &json!({
                    "type": "run.control",
                    "runId": "run-1",
                    "action": "interrupt"
                }),
                "run-1"
            )
            .is_err(),
            "run-only cancellation can target a newer turn in the same run"
        );
        assert!(
            parse_remote_command(
                &json!({
                    "type": "run.control",
                    "runId": "run-1",
                    "action": "interrupt",
                    "turnId": "../../stale"
                }),
                "run-1"
            )
            .is_err()
        );
        let parsed = parse_remote_command(
            &json!({
                "type": "run.control",
                "runId": "run-1",
                "action": "interrupt",
                "turnId": "turn-current"
            }),
            "run-1",
        )
        .unwrap();
        assert!(matches!(
            parsed,
            RemoteCommand::Control {
                turn_id: Some(ref turn_id),
                runtime_chat: None,
                ..
            } if turn_id == "turn-current"
        ));

        let mut controller = RemoteControlController::default();
        controller.activate_prompt("run-1", "turn-current").unwrap();
        assert!(controller.active_turn_matches("run-1", "turn-current"));
        assert!(!controller.active_turn_matches("run-1", "turn-stale"));
        assert!(!controller.active_turn_matches("run-other", "turn-current"));
    }

    #[test]
    fn runtime_chat_semantic_envelope_carries_stable_native_source_id() {
        let source_event_id = format!("native_event_{}", "a".repeat(64));
        let semantic = runtime_chat_envelope(
            9,
            "item.delta",
            Some("local_thread_fixture"),
            Some("local_turn_fixture"),
            Some(&source_event_id),
            "2026-08-23T00:00:00Z".to_string(),
            json!({ "delta": "hello" }),
        );
        assert_eq!(semantic["schema_version"], 2);
        assert_eq!(semantic["source_event_id"], source_event_id);

        let catalog_payload = json!({
            "schemaVersion": 2,
            "challenge": "a".repeat(32),
            "providers": [{ "id": "safe", "models": [{ "id": "model-1" }] }]
        });
        let fingerprint =
            RuntimeChatRelayHost::catalog_payload_fingerprint(&catalog_payload).unwrap();
        let catalog_source_id = runtime_chat_catalog_source_event_id("run_fixture", &fingerprint);
        let catalog = runtime_chat_envelope(
            1,
            "runtime.catalog",
            None,
            None,
            Some(&catalog_source_id),
            RUNTIME_CHAT_CATALOG_TIMESTAMP.to_string(),
            catalog_payload.clone(),
        );
        let replay = runtime_chat_envelope(
            99,
            "runtime.catalog",
            None,
            None,
            Some(&catalog_source_id),
            RUNTIME_CHAT_CATALOG_TIMESTAMP.to_string(),
            catalog_payload,
        );
        assert_eq!(catalog["source_event_id"], catalog_source_id);
        let mut catalog_semantic = catalog;
        let mut replay_semantic = replay;
        catalog_semantic.as_object_mut().unwrap().remove("seq");
        replay_semantic.as_object_mut().unwrap().remove("seq");
        assert_eq!(catalog_semantic, replay_semantic);

        let changed_fingerprint = RuntimeChatRelayHost::catalog_payload_fingerprint(
            &json!({ "schemaVersion": 2, "challenge": "a".repeat(32), "providers": [] }),
        )
        .unwrap();
        assert_ne!(
            runtime_chat_catalog_source_event_id("run_fixture", &changed_fingerprint),
            catalog_source_id
        );
    }

    #[test]
    fn approval_projection_matches_control_plane_namespace() {
        assert_eq!(projected_approval_id("tool-call-1").len(), 39);
        assert!(projected_approval_id("tool-call-1").starts_with("local_approval_"));
        assert_ne!(
            projected_approval_id("tool-call-1"),
            projected_approval_id("tool-call-2")
        );
    }

    #[test]
    fn authorization_url_is_exact_and_cannot_redirect_or_add_parameters() {
        assert!(
            validate_authorization_url(
                "https://app.codewhale.net/runner/authorize?user_code=ABCD-EFGH-JKLM",
                "ABCD-EFGH-JKLM",
            )
            .is_ok()
        );
        for spoofed in [
            "http://app.codewhale.net/runner/authorize?user_code=ABCD-EFGH-JKLM",
            "https://app.codewhale.net.evil.example/runner/authorize?user_code=ABCD-EFGH-JKLM",
            "https://app.codewhale.net/runner/authorize?user_code=ABCD-EFGH-JKLM&next=https://evil.example",
            "https://app.codewhale.net/runner/authorize?user_code=WRONG-CODE",
        ] {
            assert!(validate_authorization_url(spoofed, "ABCD-EFGH-JKLM").is_err());
        }
    }

    #[test]
    fn command_sequences_are_content_bound_and_replay_safe() {
        let mut controller = RemoteControlController::default();
        controller.status = Status::Connected;
        controller.attached_run_id = Some("run-1".to_string());
        let prompt = RemoteCommand::Prompt {
            turn_id: "turn-1".to_string(),
            prompt: "Continue".to_string(),
        };
        assert_eq!(controller.claim_command("run-1", 1, &prompt), Ok(true));
        assert_eq!(controller.claim_command("run-1", 1, &prompt), Ok(false));
        assert!(
            controller
                .claim_command(
                    "run-1",
                    1,
                    &RemoteCommand::Prompt {
                        turn_id: "turn-1".to_string(),
                        prompt: "Changed".to_string(),
                    },
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn durable_runtime_chat_replay_bypasses_only_new_work_recovery_gates() {
        let root = tempfile::tempdir().unwrap();
        let runtime_root = root.path().join("runtime-chat");
        let command = parse_remote_command(&runtime_chat_prompt_fixture(), "run-1").unwrap();
        let RemoteCommand::RuntimeChatPrompt(prompt) = &command else {
            panic!("fixture must parse as Runtime Chat");
        };
        {
            let mut first = RemoteControlController::default();
            first
                .configure_runtime_chat(
                    crate::config::Config::default(),
                    Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                    runtime_root.clone(),
                    "target-1".to_string(),
                    "session_fixture".to_string(),
                )
                .unwrap();
            let host = first.runtime_chat.as_ref().unwrap();
            host.bind_account("account_fixture", "target-1").unwrap();
            host.install_prompt_replay_for_tests(prompt, true)
                .await
                .unwrap();
        }

        let mut controller = RemoteControlController::default();
        controller
            .configure_runtime_chat(
                crate::config::Config::default(),
                Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                runtime_root.clone(),
                "target-1".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        controller
            .runtime_chat
            .as_ref()
            .unwrap()
            .bind_account("account_fixture", "target-1")
            .unwrap();
        controller
            .runtime_chat
            .as_ref()
            .unwrap()
            .authorize_run("run-1")
            .unwrap();
        let envelope = runtime_chat_envelope(
            9,
            "turn.completed",
            Some(&prompt.runtime_thread_id),
            Some(&prompt.turn_id),
            Some("native_event_exact_replay_gate"),
            "2026-08-23T00:00:00Z".to_string(),
            json!({ "turn": { "status": "completed" } }),
        );
        controller.pending_runtime_events.insert(
            "run-1".to_string(),
            BTreeMap::from([(
                9,
                PendingRuntimeEnvelope {
                    encoded_len: serde_json::to_vec(&envelope).unwrap().len(),
                    envelope,
                    integrity: true,
                    handed_off: true,
                },
            )]),
        );
        let changed_config = crate::config::Config {
            default_text_model: Some("changed-after-restart".to_string()),
            ..crate::config::Config::default()
        };
        controller
            .configure_runtime_chat(
                changed_config,
                Arc::new(crate::plugins::PluginRegistry::empty(root.path())),
                runtime_root,
                "target-1".to_string(),
                "session_fixture".to_string(),
            )
            .unwrap();
        assert!(controller.pending_runtime_chat_configuration.is_some());
        controller.status = Status::Connected;
        controller.attached_run_id = Some("run-1".to_string());
        controller.attached_workspace_id = Some(prompt.workspace.id.clone());
        controller.target_ref = Some(prompt.workspace.target_ref.clone());

        assert_eq!(controller.claim_command("run-1", 17, &command), Ok(false));
        controller.apply_runtime_chat_prompt(prompt).await.unwrap();
        let mut changed = runtime_chat_prompt_fixture();
        changed["prompt"] = json!("changed replay body");
        let changed = parse_remote_command(&changed, "run-1").unwrap();
        assert!(controller.claim_command("run-1", 17, &changed).is_err());
    }

    #[tokio::test]
    async fn pre_lease_failure_never_locks_and_allows_immediate_retry() {
        let (mut controller, _worker_rx, event_tx, _journal_root) = wired_controller();
        controller.status = Status::Connecting;
        event_tx
            .send(RemoteEvent::FailedPreLease(
                "Codewhale rejected remote-control enrollment (403): client version not accepted."
                    .to_string(),
            ))
            .unwrap();
        assert!(matches!(
            controller.try_next_event(),
            Some(RemoteEvent::FailedPreLease(_))
        ));
        assert_eq!(controller.status, Status::Failed);
        assert!(
            controller.ownership_blocked_until.is_none(),
            "a rejection before any lease must never start the reconnect blackout"
        );
        let line = controller.status_line();
        assert!(line.contains("failed before connecting"), "{line}");
        assert!(line.contains("/rc to retry"), "{line}");
        assert!(
            line.contains("403"),
            "the sanitized HTTP status must surface: {line}"
        );
        assert!(line.contains("client version not accepted"), "{line}");

        // Stopping after a pre-lease failure is an ordinary reset (no lease
        // to drain, no blackout to honor).
        controller.stop();
        assert_eq!(controller.status, Status::Off);

        // Immediate retry is allowed — no lease drain wait.
        let result = controller.start(RemoteStart {
            workspace_label: "fixture".to_string(),
            target_ref: "target_fixture".to_string(),
            session_id: "session_fixture".to_string(),
            runtime_version: "0.9.1".to_string(),
            runtime_commit: "a".repeat(40),
            journal_dir: None,
            git_remote: None,
        });
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn sanitized_rejection_excerpt_reads_only_bounded_error_fields() {
        assert_eq!(
            sanitized_rejection_excerpt(
                br#"{"error":"client version not accepted","details":"noise"}"#
            )
            .as_deref(),
            Some("client version not accepted")
        );
        assert_eq!(
            sanitized_rejection_excerpt(br#"{"message":"enrollment closed"}"#).as_deref(),
            Some("enrollment closed")
        );
        // Control characters are stripped, never echoed. A JSON-escaped NUL
        // parses into the value; the strip must remove it. A raw NUL byte
        // makes serde_json reject the body outright, which is also safe
        // (no excerpt) — assert both directions.
        let with_control = "{\"error\":\"bad\\u0000opaque\"}".to_string();
        assert_eq!(
            sanitized_rejection_excerpt(with_control.as_bytes()).as_deref(),
            Some("badopaque")
        );
        let raw_nul = "{\"error\":\"bad\u{0}opaque\"}".to_string();
        assert_eq!(sanitized_rejection_excerpt(raw_nul.as_bytes()), None);
        // Non-JSON bodies yield no excerpt.
        assert_eq!(sanitized_rejection_excerpt(b"<html>403</html>"), None);
        // Overlong reasons are capped.
        let long = format!("{{\"error\":\"{}\"}}", "x".repeat(400));
        let excerpt = sanitized_rejection_excerpt(long.as_bytes()).expect("capped excerpt");
        assert!(excerpt.chars().count() <= 140);
    }

    #[test]
    fn view_gate_matching_never_confuses_two_approval_cards() {
        use crate::tui::approval::ApprovalRequest;
        use crate::tui::approval::ApprovalView;
        use crate::tui::views::ViewStack;

        let request = ApprovalRequest::new(
            "tool_A",
            "edit",
            "Edit A",
            &serde_json::json!({ "file": "a" }),
            "approval_key_A",
        );
        let card = ApprovalView::new(request);
        let gate_a = projected_approval_id("tool_A");
        let gate_b = projected_approval_id("tool_B");

        let mut stack = ViewStack::new();
        stack.push(card);
        assert!(
            !stack.remove_approval_for_gate(&gate_b),
            "a different gate must NEVER match this card — the whole point of identity-aware dismissal"
        );
        assert!(!stack.remove_approval_for_gate("local_approval_missing"));
        assert!(!stack.is_empty());
        assert!(
            stack.remove_approval_for_gate(&gate_a),
            "the matching gate must match"
        );
        assert!(stack.is_empty());
    }

    #[tokio::test]
    async fn enrollment_rejection_carries_a_sanitized_actionable_reason() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/device"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "error": "client version not accepted",
                "documentation": "https://example.test/docs"
            })))
            .mount(&server)
            .await;
        let client = crate::tls::reqwest_client_builder()
            .https_only(false)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .expect("test client");
        let error = public_request(
            &client,
            Method::POST,
            Url::parse(&format!(
                "{}/oauth/device",
                server.uri().trim_end_matches('/')
            ))
            .expect("server url"),
            json!({ "audience": "codewhale-runner" }),
        )
        .await
        .expect_err("403 must fail");
        assert!(error.contains("403"), "{error}");
        assert!(error.contains("client version not accepted"), "{error}");
        assert!(
            !error.contains("documentation"),
            "only the conventional error fields may surface: {error}"
        );
    }

    #[tokio::test]
    async fn failed_relay_keeps_reconnect_blocked_until_lease_expiry() {
        let mut controller = RemoteControlController::default();
        controller.status = Status::Failed;
        controller.ownership_blocked_until = Some(Instant::now() + Duration::from_secs(90));
        // Mirror semantics: local input is never locked, but reconnecting
        // while the server lease may still be live is refused — the web must
        // not see two runners for the same session.
        let start = RemoteStart {
            workspace_label: "fixture".to_string(),
            target_ref: "target_fixture".to_string(),
            session_id: "session_fixture".to_string(),
            runtime_version: "0.9.1".to_string(),
            runtime_commit: "a".repeat(40),
            journal_dir: None,
            git_remote: None,
        };
        assert!(controller.start(start.clone()).is_err());
        // The web cannot answer shared approvals while the relay is failed.
        assert!(!controller.can_share_approval_with_web());
        controller.ownership_blocked_until = Some(Instant::now() - Duration::from_secs(1));
        assert!(controller.start(start).is_ok());
    }

    #[test]
    fn stop_after_lease_expiry_preserves_pending_approvals_for_restoration() {
        let mut controller = RemoteControlController::default();
        controller.status = Status::Failed;
        controller.ownership_blocked_until = Some(Instant::now() - Duration::from_secs(1));
        controller.pending_approvals.insert(
            "approval_fixture".to_string(),
            PendingRemoteApproval {
                tool_id: "tool_fixture".to_string(),
            },
        );

        controller.stop();
        assert_eq!(controller.status, Status::Failed);
        assert_eq!(controller.pending_approvals.len(), 1);

        let event = controller.try_next_event();
        assert!(matches!(
            event,
            Some(RemoteEvent::OwnershipRestored { approvals })
                if approvals.len() == 1 && approvals[0].tool_id == "tool_fixture"
        ));
        assert_eq!(controller.status, Status::Off);
        assert!(controller.pending_approvals.is_empty());
    }

    #[test]
    fn cancelling_a_connect_keeps_reconnect_blocked_until_lease_drain() {
        let mut controller = RemoteControlController::default();
        controller.status = Status::Connecting;
        controller.stop();

        assert_eq!(controller.status, Status::Failed);
        // Mirror semantics: nothing about a cancelled connect locks local
        // input; reconnect stays blocked until the possible lease drains.
        let result = controller.start(RemoteStart {
            workspace_label: "fixture".to_string(),
            target_ref: "target_fixture".to_string(),
            session_id: "session_fixture".to_string(),
            runtime_version: "0.9.1".to_string(),
            runtime_commit: "a".repeat(40),
            journal_dir: None,
            git_remote: None,
        });
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("previous remote lease"));
    }

    #[test]
    fn failed_worker_retains_snapshot_marker_and_exact_unacked_event() {
        let (mut controller, mut worker_rx, event_tx, _journal_root) = wired_controller();
        controller.status = Status::Connected;
        controller.upload_snapshot("run-1", &[]);
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("snapshot queued");
        };
        let exact = envelopes[0].clone();
        event_tx
            .send(RemoteEvent::Failed("fixture disconnect".to_string()))
            .unwrap();

        assert!(matches!(
            controller.try_next_event(),
            Some(RemoteEvent::Failed(_))
        ));
        assert!(controller.uploaded_snapshots.contains("run-1"));
        assert_eq!(
            controller.pending_runtime_events["run-1"]
                .values()
                .next()
                .map(|entry| &entry.envelope),
            Some(&exact)
        );
        // Fail-closed: the web can no longer answer shared approvals, and
        // reconnecting waits out the possible server lease.
        assert!(!controller.can_share_approval_with_web());
        assert!(controller.ownership_blocked_until.is_some());
    }

    #[tokio::test]
    async fn cwc_runner_wire_contract_preserves_pending_and_recovery_commands() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/local-runners/runner-1/runs/run-1/commands"))
            .and(query_param("since_seq", "0"))
            .and(query_param("include_accepted", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "commands": [{
                    "seq": 1,
                    "deliveryStatus": "pending",
                    "ackStatus": "",
                    "command": {
                        "type": "prompt.request",
                        "runId": "run-1",
                        "turnId": "turn-1",
                        "prompt": "Continue from the web."
                    }
                }, {
                    "seq": 2,
                    "deliveryStatus": "acknowledged",
                    "ackStatus": "accepted",
                    "command": {
                        "type": "run.control",
                        "runId": "run-1",
                        "action": "interrupt"
                    }
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/local-runners/runner-1/runs/run-1/events"))
            .and(body_json(json!({
                "acknowledgements": [{
                    "commandSeq": 1,
                    "commandType": "prompt.request",
                    "status": "accepted",
                    "turnId": "turn-1"
                }],
                "envelopes": []
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "accepted": [],
                "count": 1,
                "cursor": 0
            })))
            .expect(1)
            .mount(&server)
            .await;

        let enrollment = LiveEnrollment {
            persisted: PersistedEnrollment {
                schema_version: 1,
                control_plane_base: format!("{}/", server.uri()),
                runner_enrollment_id: "enrollment-1".to_string(),
                account_ref: "account-1".to_string(),
                device_id: "device-1".to_string(),
                target_ref: "target-1".to_string(),
                target_grant_ref: "grant-1".to_string(),
                runtime_version: "0.9.1".to_string(),
                runtime_commit: "a".repeat(40),
                bootstrap_secret: "b".repeat(43),
            },
            access_token: "fixture-runner-access-token".to_string(),
        };
        let client = crate::tls::reqwest_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("fixture client");

        let listed = list_commands(&client, &enrollment, "runner-1", "run-1", 0)
            .await
            .expect("CWC command list");
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].ack_status, "");
        assert_eq!(listed[1].ack_status, "accepted");
        let prompt =
            parse_remote_command(&listed[0].command, "run-1").expect("typed prompt command");
        upload_command_accepted(
            &client,
            &enrollment,
            "runner-1",
            "run-1",
            listed[0].seq,
            &prompt,
        )
        .await
        .expect("durable accepted acknowledgement");
    }

    fn wired_controller() -> (
        RemoteControlController,
        mpsc::UnboundedReceiver<WorkerCommand>,
        mpsc::UnboundedSender<RemoteEvent>,
        tempfile::TempDir,
    ) {
        let mut controller = RemoteControlController::default();
        let (worker_tx, worker_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let journal_root = tempfile::tempdir().expect("wired controller journal root");
        controller.journal = Some(
            RuntimeEventJournal::open(
                journal_root.path(),
                "target_wired_fixture",
                "session_wired_fixture",
            )
            .expect("wired controller journal"),
        );
        controller.worker_tx = Some(worker_tx);
        controller.event_rx = Some(event_rx);
        (controller, worker_rx, event_tx, journal_root)
    }

    fn turn_complete_event() -> EngineEvent {
        EngineEvent::TurnComplete {
            usage: codewhale_models::Usage::default(),
            parent_route_usage: codewhale_models::Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        }
    }

    #[test]
    fn stop_refusal_holds_until_terminal_event_is_acknowledged() {
        let (mut controller, mut worker_rx, event_tx, _journal_root) = wired_controller();
        controller
            .activate_prompt("run_fixture", "turn_fixture")
            .unwrap();
        let refusal = controller.stop_refusal().expect("active turn blocks stop");
        assert!(refusal.contains("active remote turn"), "{refusal}");

        controller.observe_engine_event(&turn_complete_event());
        assert!(
            !controller.has_active_run(),
            "the terminal event releases the run binding"
        );
        let refusal = controller
            .stop_refusal()
            .expect("a queued but unacknowledged terminal event must still block stop");
        assert!(refusal.contains("acknowledged"), "{refusal}");
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("the terminal envelope must be handed to the transport");
        };
        assert_eq!(envelopes[0]["event"], "turn.completed");
        let seq = envelopes[0]["seq"].as_u64().expect("terminal seq");

        event_tx
            .send(RemoteEvent::RuntimeCursor {
                run_id: "run_fixture".to_string(),
                cursor: seq,
            })
            .unwrap();
        controller.try_next_event().unwrap();
        assert_eq!(
            controller.stop_refusal(),
            None,
            "a server-acknowledged terminal event unblocks stop"
        );
    }

    #[test]
    fn different_run_attachment_is_quarantined_during_active_classic_work() {
        let (mut controller, _worker_rx, event_tx, _journal_root) = wired_controller();
        let attachment = |run_id: &str| RemoteAttachment {
            run_id: run_id.to_string(),
            workspace_id: "workspace_fixture".to_string(),
            runtime_cursor: 0,
            snapshot_present: false,
            runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
            runtime_chat_relay_challenge: "a".repeat(32),
        };
        event_tx
            .send(RemoteEvent::Connected {
                account_ref: "account_fixture".to_string(),
                runner_id: "runner_fixture".to_string(),
                target_ref: "target_wired_fixture".to_string(),
                attachment: attachment("run_a"),
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();
        controller.activate_prompt("run_a", "turn_a").unwrap();

        event_tx
            .send(RemoteEvent::Attachment {
                account_ref: "account_fixture".to_string(),
                target_ref: "target_wired_fixture".to_string(),
                attachment: attachment("run_b"),
                links: RemoteLinks::default(),
            })
            .unwrap();
        let rejected = controller
            .try_next_event()
            .expect("rejected attachment event");
        assert!(matches!(rejected, RemoteEvent::Failed(_)));
        assert_eq!(controller.attached_run_id.as_deref(), Some("run_a"));
        assert!(controller.active_turn_matches("run_a", "turn_a"));
        assert_eq!(controller.status, Status::Failed);
        assert!(
            controller.worker_tx.is_none(),
            "rejected authority is quarantined"
        );
        assert!(
            controller
                .claim_command(
                    "run_b",
                    1,
                    &RemoteCommand::Prompt {
                        turn_id: "turn_b".to_string(),
                        prompt: "must not run".to_string(),
                    },
                )
                .is_err(),
            "no command from the rejected run may execute"
        );
    }

    #[test]
    fn failed_stop_stays_fail_closed_with_no_dual_ownership() {
        let (mut controller, _worker_rx, event_tx, _journal_root) = wired_controller();
        controller.status = Status::Connected;
        controller.stop();
        assert_eq!(controller.status, Status::Stopping);
        assert!(
            !controller.can_share_approval_with_web(),
            "stopping must close the shared-decision channel before confirmation"
        );

        // The worker could not confirm the drain or the offline heartbeat.
        event_tx
            .send(RemoteEvent::Failed(
                "the offline heartbeat could not be delivered".to_string(),
            ))
            .unwrap();
        let event = controller.try_next_event().unwrap();
        assert!(matches!(event, RemoteEvent::Failed(_)));
        assert!(
            !controller.can_share_approval_with_web(),
            "an unconfirmed stop must stay fail-closed through the lease expiry"
        );
        assert!(controller.ownership_blocked_until.is_some());
        assert!(
            controller.status_line().contains("lost after connecting"),
            "{}",
            controller.status_line()
        );
        assert!(
            controller.try_next_event().is_none(),
            "ownership must not be restored while the lease could still be live"
        );
    }

    #[tokio::test]
    async fn stop_drain_flushes_runtime_outbox_with_byte_identical_retries() {
        let server = MockServer::start().await;
        let responder = AmbiguousRuntimeResponder::default();
        Mock::given(method("POST"))
            .and(path(
                "/api/local-runners/runner_fixture/runs/run_fixture/events",
            ))
            .respond_with(responder.clone())
            .expect(2)
            .mount(&server)
            .await;
        let mut enrollment = fixture_enrollment(&format!("{}/", server.uri()));
        let mut runner_id = "runner_fixture".to_string();
        let client = crate::tls::reqwest_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("fixture client");
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut outbox = RuntimeTransportOutbox::default();
        outbox
            .enqueue(
                "run_fixture",
                runtime_envelope(
                    1,
                    "turn.completed",
                    Some("turn_fixture"),
                    "2026-08-08T12:00:00Z".to_string(),
                    json!({ "turn": { "status": "completed", "usage": {} } }),
                ),
            )
            .expect("queue terminal envelope");

        drain_runtime_outbox_for_stop(
            &client,
            &mut enrollment,
            &mut runner_id,
            &fixture_start(),
            &event_tx,
            &mut outbox,
            Instant::now() + Duration::from_secs(10),
        )
        .await
        .expect("the drain must complete before stop is confirmed");

        assert!(outbox.events.is_empty(), "the outbox must drain fully");
        let RemoteEvent::RuntimeCursor { run_id, cursor } = event_rx
            .try_recv()
            .expect("cursor event for journal compaction")
        else {
            panic!("drain must surface the server cursor");
        };
        assert_eq!(run_id, "run_fixture");
        assert_eq!(cursor, 1);
        let bodies = responder.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2, "ambiguous response must be retried");
        assert_eq!(bodies[0], bodies[1], "retries must be byte-identical");
    }

    #[tokio::test]
    async fn stop_drain_stops_when_a_fresh_credential_is_refused_again() {
        let _env = crate::test_support::lock_test_env();
        let secrets_root = tempfile::tempdir().expect("isolated remote-control secrets");
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", secrets_root.path());
        let server = MockServer::start().await;
        // A terminal refusal: the runner may not write this run, and a new
        // access token does not change that.
        Mock::given(method("POST"))
            .and(path(
                "/api/local-runners/runner_fixture/runs/run_fixture/events",
            ))
            .respond_with(ResponseTemplate::new(403))
            .expect(2)
            .mount(&server)
            .await;
        let access_token = crate::test_support::future_test_jwt(&"a".repeat(40));
        Mock::given(method("POST"))
            .and(path("/api/runner/enrollments/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "enrollment": {
                    "id": "enrollment_fixture",
                    "userId": "account_fixture",
                    "deviceId": "device_fixture",
                    "runtimeVersion": "0.9.6",
                    "runtimeCommit": "a".repeat(40),
                    "capabilities": CAPABILITIES,
                },
                "credential": { "accessToken": access_token },
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/local-runners/connect"))
            .respond_with(ResponseTemplate::new(200).set_body_json(fixture_connection_response()))
            .mount(&server)
            .await;
        let mut enrollment = fixture_enrollment(&format!("{}/", server.uri()));
        let mut runner_id = "runner_fixture".to_string();
        let client = crate::tls::reqwest_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("fixture client");
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut outbox = RuntimeTransportOutbox::default();
        outbox
            .enqueue(
                "run_fixture",
                runtime_envelope(
                    1,
                    "turn.completed",
                    Some("turn_fixture"),
                    "2026-08-08T12:00:00Z".to_string(),
                    json!({ "turn": { "status": "completed", "usage": {} } }),
                ),
            )
            .expect("queue terminal envelope");

        let error = drain_runtime_outbox_for_stop(
            &client,
            &mut enrollment,
            &mut runner_id,
            &fixture_start(),
            &event_tx,
            &mut outbox,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .expect_err("a refusal that survives a refresh must end the drain");
        assert_eq!(error, RUNTIME_REFRESHED_CREDENTIAL_REFUSED);
        assert!(
            !outbox.events.is_empty(),
            "the refused envelope stays queued for a later, authorized resend"
        );
    }

    #[tokio::test]
    async fn stop_drain_deadline_failure_refuses_to_confirm_stop() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/api/local-runners/runner_fixture/runs/run_fixture/events",
            ))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let mut enrollment = fixture_enrollment(&format!("{}/", server.uri()));
        let mut runner_id = "runner_fixture".to_string();
        let client = crate::tls::reqwest_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("fixture client");
        let (event_tx, _event_rx) = mpsc::unbounded_channel();
        let mut outbox = RuntimeTransportOutbox::default();
        outbox
            .enqueue(
                "run_fixture",
                runtime_envelope(
                    1,
                    "turn.completed",
                    Some("turn_fixture"),
                    "2026-08-08T12:00:00Z".to_string(),
                    json!({ "turn": { "status": "completed", "usage": {} } }),
                ),
            )
            .expect("queue terminal envelope");

        let error = drain_runtime_outbox_for_stop(
            &client,
            &mut enrollment,
            &mut runner_id,
            &fixture_start(),
            &event_tx,
            &mut outbox,
            Instant::now() + Duration::from_millis(700),
        )
        .await
        .expect_err("an undrained outbox must fail the stop");
        assert!(error.contains("not confirmed"), "{error}");
        assert!(
            !outbox.events.is_empty(),
            "the exact unacknowledged envelope must be retained for the reconnect resend"
        );
    }

    #[test]
    fn journal_roundtrip_restores_unacknowledged_envelopes_byte_identically() {
        let dir = tempfile::tempdir().expect("journal tempdir");
        let journal =
            RuntimeEventJournal::open(dir.path(), "target:fixture@01", "session:fixture@01")
                .expect("journal setup");
        assert!(journal.load().expect("missing file is empty").is_empty());

        let delta = runtime_envelope(
            1,
            "item.delta",
            Some("turn_fixture"),
            "2026-08-08T12:00:00Z".to_string(),
            json!({ "kind": "agent_message", "delta": "exact 🫧 body" }),
        );
        let terminal = runtime_envelope(
            2,
            "turn.completed",
            Some("turn_fixture"),
            "2026-08-08T12:00:01Z".to_string(),
            json!({ "turn": { "status": "completed", "usage": {} } }),
        );
        let mut pending: HashMap<String, BTreeMap<u64, PendingRuntimeEnvelope>> = HashMap::new();
        let mut events = BTreeMap::new();
        for envelope in [delta.clone(), terminal.clone()] {
            let seq = runtime_envelope_seq(&envelope).unwrap();
            let encoded_len = serde_json::to_vec(&envelope).unwrap().len();
            let integrity = runtime_envelope_event(&envelope).is_some_and(integrity_critical_event);
            events.insert(
                seq,
                PendingRuntimeEnvelope {
                    envelope,
                    encoded_len,
                    integrity,
                    handed_off: true,
                },
            );
        }
        pending.insert("run_fixture".to_string(), events);
        journal.persist(&pending).expect("atomic persist");
        drop(journal);

        let reopened =
            RuntimeEventJournal::open(dir.path(), "target:fixture@01", "session:fixture@01")
                .expect("journal reopen");
        let restored = reopened.load().expect("verified load");
        let events = restored.get("run_fixture").expect("restored run");
        assert_eq!(events.len(), 2);
        assert_eq!(
            serde_json::to_vec(&events[&1]).unwrap(),
            serde_json::to_vec(&delta).unwrap(),
            "a restored envelope must re-serialize byte-identically for ambiguous retries"
        );
        assert_eq!(
            serde_json::to_vec(&events[&2]).unwrap(),
            serde_json::to_vec(&terminal).unwrap()
        );

        // Compaction: an empty pending set removes the file entirely.
        pending.get_mut("run_fixture").unwrap().clear();
        reopened.persist(&pending).expect("compacting persist");
        assert!(!reopened.path.exists(), "acknowledged journals are deleted");
    }

    #[test]
    fn legacy_session_only_journal_is_preserved_and_repeated_starts_fail_closed() {
        let dir = tempfile::tempdir().expect("journal tempdir");
        let session_id = "session_legacy_shared";
        let journal_a = RuntimeEventJournal::open(dir.path(), "target_a", session_id).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"cwc-remote-control-journal\0");
        hasher.update(session_id.as_bytes());
        let session_tag = bytes_to_hex(&hasher.finalize())[..32].to_string();
        assert_eq!(
            journal_a.legacy_path,
            dir.path().join(format!("journal_{session_tag}.json"))
        );
        let envelope = runtime_envelope(
            1,
            "turn.completed",
            Some("turn_legacy"),
            "2026-08-08T12:00:00Z".to_string(),
            json!({ "turn": { "status": "completed" } }),
        );
        crate::utils::write_atomic(
            &journal_a.legacy_path,
            &serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "session": session_tag,
                "runs": { "run_legacy": [envelope] },
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(journal_a.load().unwrap_err(), JOURNAL_LEGACY_SCOPE_ERROR);
        journal_a.quarantine_legacy();
        let unscoped_path = journal_a.legacy_unscoped_path.clone();
        assert!(unscoped_path.exists());
        assert!(!journal_a.path.exists());
        drop(journal_a);

        let retry_a = RuntimeEventJournal::open(dir.path(), "target_a", session_id).unwrap();
        assert_eq!(retry_a.load().unwrap_err(), JOURNAL_LEGACY_SCOPE_ERROR);
        drop(retry_a);
        assert_eq!(
            RuntimeEventJournal::open(dir.path(), "target_b", session_id)
                .err()
                .expect("the legacy session remains permanently bound to target A"),
            CLASSIC_LEASE_SCOPE_ERROR
        );
        assert!(unscoped_path.exists());
    }

    #[test]
    fn corrupt_v2_quarantine_never_moves_an_unrelated_legacy_journal() {
        let dir = tempfile::tempdir().expect("journal tempdir");
        let journal = RuntimeEventJournal::open(dir.path(), "target_b", "session_shared").unwrap();
        crate::utils::write_atomic(&journal.legacy_path, b"legacy target A recovery material")
            .unwrap();
        crate::utils::write_atomic(&journal.path, b"{not-json").unwrap();
        assert_eq!(journal.load().unwrap_err(), JOURNAL_UNTRUSTED_ERROR);
        journal.quarantine_current();
        assert!(journal.legacy_path.exists());
        assert!(!journal.legacy_unscoped_path.exists());
        assert!(journal.path.with_extension("corrupt").exists());
    }

    #[test]
    fn runtime_chat_projection_is_not_handed_off_before_durable_journal_write() {
        let dir = tempfile::tempdir().expect("journal tempdir");
        let journal =
            RuntimeEventJournal::open(dir.path(), "target:fixture@01", "session:fixture@01")
                .expect("journal setup");
        let journal_path = journal.path.clone();
        let mut controller = RemoteControlController::default();
        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        controller.worker_tx = Some(worker_tx);
        controller.journal = Some(journal);
        let envelope = runtime_chat_envelope(
            1,
            "turn.completed",
            Some("local_thread_aaaaaaaaaaaaaaaaaaaaaaaa"),
            Some("local_turn_bbbbbbbbbbbbbbbbbbbbbbbb"),
            Some("native_event_fixture"),
            RUNTIME_CHAT_CATALOG_TIMESTAMP.to_string(),
            json!({ "turn": { "status": "completed" } }),
        );

        inject_journal_persist_failures(&journal_path, 1);
        assert!(
            !controller.queue_runtime_chat_envelope("run_fixture", envelope.clone()),
            "a failed durable append must reject native cursor advancement"
        );
        assert!(controller.pending_runtime_events.is_empty());
        assert!(!controller.event_seq.contains_key("run_fixture"));
        assert!(worker_rx.try_recv().is_err());

        assert!(controller.queue_runtime_chat_envelope("run_fixture", envelope));
        assert!(journal_path.exists());
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("durable retry must hand off the exact projection");
        };
        assert_eq!(envelopes[0]["seq"], 1);
    }

    #[test]
    fn classic_terminal_journal_failure_stays_retained_and_fail_closed_until_cursor() {
        let (mut controller, mut worker_rx, _event_tx, _journal_root) = wired_controller();
        controller.status = Status::Connected;
        controller
            .activate_prompt("run_fixture", "turn_fixture")
            .unwrap();
        let journal_path = controller.journal.as_ref().unwrap().path.clone();
        inject_journal_persist_failures(&journal_path, 2);

        controller.observe_engine_event(&turn_complete_event());
        assert_eq!(controller.status, Status::Failed);
        assert!(worker_rx.try_recv().is_err(), "failed append must not send");
        assert!(
            controller.has_unacknowledged_integrity_events(),
            "classic terminal must remain retryable in memory"
        );
        assert!(controller.stop_refusal().is_some());
        controller.ownership_blocked_until = Some(Instant::now() - Duration::from_millis(1));
        assert!(controller.try_next_event().is_none());
        assert_eq!(
            controller.status,
            Status::Failed,
            "lease expiry cannot discard an unacknowledged classic terminal"
        );

        controller
            .journal
            .as_ref()
            .unwrap()
            .persist(&controller.pending_runtime_events)
            .expect("repair durable journal");
        let (worker_tx, mut repaired_worker_rx) = mpsc::unbounded_channel();
        controller.worker_tx = Some(worker_tx);
        controller.flush_pending_runtime_events("run_fixture");
        let WorkerCommand::Upload { envelopes, .. } = repaired_worker_rx.try_recv().unwrap() else {
            panic!("repaired terminal must be handed off");
        };
        let seq = runtime_envelope_seq(&envelopes[0]).unwrap();
        controller.reconcile_runtime_cursor("run_fixture", seq);
        assert!(!controller.has_unacknowledged_integrity_events());
    }

    #[test]
    fn classic_worker_failure_retains_live_turn_until_exact_terminal_is_durable() {
        let (mut controller, _worker_rx, event_tx, _journal_root) = wired_controller();
        controller.status = Status::Connected;
        controller
            .activate_prompt("run_fixture", "turn_fixture")
            .unwrap();
        event_tx
            .send(RemoteEvent::Failed("fixture transport loss".to_string()))
            .unwrap();
        assert!(matches!(
            controller.try_next_event(),
            Some(RemoteEvent::Failed(_))
        ));
        assert!(controller.has_active_run());
        assert!(controller.has_durable_classic_lease());

        controller.ownership_blocked_until = Some(Instant::now() - Duration::from_millis(1));
        assert!(controller.try_next_event().is_none());
        assert_eq!(controller.status, Status::Failed);
        assert!(controller.has_active_run());
        assert!(controller.start(fixture_start()).is_err());

        let usage = codewhale_models::Usage {
            input_tokens: 17,
            output_tokens: 5,
            ..codewhale_models::Usage::default()
        };
        controller.observe_engine_event(&EngineEvent::TurnComplete {
            usage: usage.clone(),
            parent_route_usage: usage.clone(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        });
        assert!(!controller.has_active_run());
        assert!(!controller.has_durable_classic_lease());
        let terminals: Vec<&PendingRuntimeEnvelope> =
            controller.pending_runtime_events["run_fixture"]
                .values()
                .filter(|entry| runtime_envelope_event(&entry.envelope) == Some("turn.completed"))
                .collect();
        assert_eq!(terminals.len(), 1);
        assert_eq!(
            terminals[0].envelope["payload"]["turn"]["usage"],
            serde_json::to_value(usage).unwrap()
        );
    }

    #[test]
    fn classic_recovery_uses_persisted_seq_floor_and_ignores_older_terminal() {
        let root = tempfile::tempdir().unwrap();
        let old_terminal = runtime_envelope(
            40,
            "turn.completed",
            Some("turn_floor_fixture"),
            "2026-08-08T12:00:00Z".to_string(),
            json!({ "turn": { "status": "completed", "usage": {} } }),
        );
        {
            let mut first = RemoteControlController::default();
            first.journal = Some(
                RuntimeEventJournal::open(root.path(), "target_floor", "session_floor").unwrap(),
            );
            first.reset_pending_from(HashMap::from([(
                "run_floor".to_string(),
                BTreeMap::from([(40, old_terminal.clone())]),
            )]));
            first.event_seq.insert("run_floor".to_string(), 41);
            first
                .activate_prompt("run_floor", "turn_floor_fixture")
                .unwrap();
            assert_eq!(
                first
                    .journal
                    .as_ref()
                    .unwrap()
                    .classic_lease()
                    .unwrap()
                    .seq_floor,
                41
            );
        }

        let mut reopened = RemoteControlController::default();
        let journal =
            RuntimeEventJournal::open(root.path(), "target_floor", "session_floor").unwrap();
        reopened.reset_pending_from(journal.load().unwrap());
        reopened.journal = Some(journal);
        reopened.recover_classic_lease_before_worker().unwrap();

        let events = &reopened.pending_runtime_events["run_floor"];
        assert!(events.contains_key(&40));
        let recovered = events.get(&42).expect("recovery follows the durable floor");
        assert_eq!(
            runtime_envelope_event(&recovered.envelope),
            Some("turn.completed")
        );
        assert_eq!(recovered.envelope["turn_id"], "turn_floor_fixture");
        assert_eq!(recovered.envelope["payload"]["turn"]["status"], "failed");
        assert!(!reopened.has_durable_classic_lease());
    }

    #[test]
    fn blank_predispatch_lease_id_always_starts_a_fresh_generation() {
        let root = tempfile::tempdir().unwrap();
        let mut controller = RemoteControlController::default();
        controller.journal = Some(
            RuntimeEventJournal::open(root.path(), "target_generation", "session_generation")
                .unwrap(),
        );
        controller
            .begin_classic_lease(ClassicRunLease {
                run_id: "run_generation".to_string(),
                turn_id: None,
                lease_id: String::new(),
                seq_floor: 0,
            })
            .unwrap();
        let stale_lease = controller
            .journal
            .as_ref()
            .and_then(RuntimeEventJournal::classic_lease)
            .unwrap();

        controller
            .begin_classic_lease(ClassicRunLease {
                run_id: "run_generation".to_string(),
                turn_id: None,
                lease_id: String::new(),
                seq_floor: 0,
            })
            .unwrap();
        let fresh_lease = controller
            .journal
            .as_ref()
            .and_then(RuntimeEventJournal::classic_lease)
            .unwrap();

        assert_ne!(
            fresh_lease.lease_id, stale_lease.lease_id,
            "a stale empty-turn lease cannot define the next recovery identity"
        );
    }

    #[test]
    fn separate_predispatch_crashes_on_one_run_get_distinct_recovery_turn_ids() {
        let root = tempfile::tempdir().unwrap();
        let first_recovery_turn;
        {
            let mut first = RemoteControlController::default();
            first.journal = Some(
                RuntimeEventJournal::open(root.path(), "target_generation", "session_generation")
                    .unwrap(),
            );
            first
                .begin_classic_lease(ClassicRunLease {
                    run_id: "run_generation".to_string(),
                    turn_id: None,
                    lease_id: String::new(),
                    seq_floor: 0,
                })
                .unwrap();
        }
        {
            let mut recovered = RemoteControlController::default();
            let journal =
                RuntimeEventJournal::open(root.path(), "target_generation", "session_generation")
                    .unwrap();
            recovered.reset_pending_from(journal.load().unwrap());
            recovered.journal = Some(journal);
            recovered.recover_classic_lease_before_worker().unwrap();
            first_recovery_turn =
                recovered.pending_runtime_events["run_generation"][&1].envelope["turn_id"]
                    .as_str()
                    .unwrap()
                    .to_string();
            recovered.reconcile_runtime_cursor("run_generation", 1);
            recovered
                .begin_classic_lease(ClassicRunLease {
                    run_id: "run_generation".to_string(),
                    turn_id: None,
                    lease_id: String::new(),
                    seq_floor: recovered.runtime_seq_floor("run_generation"),
                })
                .unwrap();
        }
        let mut recovered_again = RemoteControlController::default();
        let journal =
            RuntimeEventJournal::open(root.path(), "target_generation", "session_generation")
                .unwrap();
        recovered_again.reset_pending_from(journal.load().unwrap());
        recovered_again.journal = Some(journal);
        recovered_again
            .recover_classic_lease_before_worker()
            .unwrap();
        let second_recovery_turn = recovered_again.pending_runtime_events["run_generation"][&2]
            .envelope["turn_id"]
            .as_str()
            .unwrap();
        assert_ne!(first_recovery_turn, second_recovery_turn);
    }

    /// #6698: a process spawned between fork and exec while a journal is open
    /// holds a duplicate of the lock descriptor. Dropping the journal must
    /// still release the session lock, or the next same-process reopen fails
    /// with the unfinished-account-turn error.
    #[cfg(unix)]
    #[test]
    fn classic_session_lock_releases_while_a_duplicated_descriptor_survives() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("active_classic_fixture.lock");
        let lock = ClassicSessionOwnerLock::acquire(&path).unwrap();
        let inherited = lock._file.try_clone().unwrap();
        drop(lock);
        let reopened = ClassicSessionOwnerLock::acquire(&path)
            .expect("dropping the owner releases the lock despite a surviving duplicate");
        drop(reopened);
        drop(inherited);
    }

    #[test]
    fn classic_terminal_cursor_repairs_failed_canonical_clear_before_compaction() {
        let (mut controller, _worker_rx, _event_tx, journal_root) = wired_controller();
        controller.status = Status::Connected;
        controller
            .activate_prompt("run_fixture", "turn_fixture")
            .unwrap();
        let state_path = controller
            .journal
            .as_ref()
            .unwrap()
            .active_index_path
            .clone();
        inject_journal_persist_failures(&state_path, 1);

        controller.observe_engine_event(&turn_complete_event());
        assert!(controller.has_durable_classic_lease());
        assert!(controller.has_active_run());
        let terminal_seq = *controller.pending_runtime_events["run_fixture"]
            .last_key_value()
            .unwrap()
            .0;
        controller.reconcile_runtime_cursor("run_fixture", terminal_seq);
        assert!(!controller.has_durable_classic_lease());
        assert!(!controller.has_active_run());
        assert!(controller.pending_runtime_events.is_empty());
        drop(controller);

        let journal = RuntimeEventJournal::open(
            journal_root.path(),
            "target_wired_fixture",
            "session_wired_fixture",
        )
        .unwrap();
        assert!(journal.classic_lease().is_none());
        assert!(journal.load().unwrap().is_empty());
    }

    #[test]
    fn saved_session_is_permanently_bound_to_its_first_remote_target() {
        let dir = tempfile::tempdir().expect("journal tempdir");
        let first = RuntimeEventJournal::open(dir.path(), "target_a", "session_shared")
            .expect("first target journal");
        assert_eq!(
            RuntimeEventJournal::open(dir.path(), "target_b", "session_shared")
                .err()
                .expect("a second target must fail closed"),
            CLASSIC_LEASE_SCOPE_ERROR
        );

        let pending_for = |run_id: &str, marker: &str| {
            let envelope = runtime_envelope(
                1,
                "turn.completed",
                None,
                "2026-08-08T12:00:00Z".to_string(),
                json!({ "turn": { "status": "completed" }, "marker": marker }),
            );
            let encoded_len = serde_json::to_vec(&envelope).unwrap().len();
            HashMap::from([(
                run_id.to_string(),
                BTreeMap::from([(
                    1,
                    PendingRuntimeEnvelope {
                        envelope,
                        encoded_len,
                        integrity: true,
                        handed_off: true,
                    },
                )]),
            )])
        };
        first.persist(&pending_for("run_a", "a")).unwrap();
        assert_eq!(
            first.load().unwrap().keys().collect::<Vec<_>>(),
            vec!["run_a"]
        );
        drop(first);
        assert_eq!(
            RuntimeEventJournal::open(dir.path(), "target_b", "session_shared")
                .err()
                .expect("the permanent target binding must fail closed"),
            CLASSIC_LEASE_SCOPE_ERROR,
            "the target binding survives terminal settlement and process restart"
        );
        RuntimeEventJournal::open(dir.path(), "target_a", "session_shared")
            .expect("the original target can reopen its saved session");
    }

    #[cfg(unix)]
    #[test]
    fn journal_directory_and_file_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().expect("journal tempdir");
        let dir = base.path().join("journal");
        let journal = RuntimeEventJournal::open(&dir, "target:fixture@01", "session:fixture@01")
            .expect("journal setup");
        let mut pending: HashMap<String, BTreeMap<u64, PendingRuntimeEnvelope>> = HashMap::new();
        let envelope = runtime_envelope(
            1,
            "turn.completed",
            None,
            "2026-08-08T12:00:00Z".to_string(),
            json!({ "turn": { "status": "completed", "usage": {} } }),
        );
        let encoded_len = serde_json::to_vec(&envelope).unwrap().len();
        pending.insert(
            "run_fixture".to_string(),
            BTreeMap::from([(
                1,
                PendingRuntimeEnvelope {
                    envelope,
                    encoded_len,
                    integrity: true,
                    handed_off: true,
                },
            )]),
        );
        journal.persist(&pending).expect("atomic persist");
        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "journal directory must be private");
        let file_mode = std::fs::metadata(&journal.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600, "journal file must be owner-only");
    }

    #[test]
    fn corrupt_journal_fails_closed_and_start_quarantines_it() {
        let dir = tempfile::tempdir().expect("journal tempdir");
        let probe = RuntimeEventJournal::open(dir.path(), "target_fixture", "session:fixture@01")
            .expect("journal setup");
        std::fs::write(&probe.path, b"{ not json").expect("plant corrupt journal");
        let corrupt_path = probe.path.clone();
        drop(probe);

        let mut controller = RemoteControlController::default();
        let error = controller
            .start(RemoteStart {
                journal_dir: Some(dir.path().to_path_buf()),
                ..fixture_start()
            })
            .expect_err("a corrupt journal must fail closed");
        assert_eq!(error, JOURNAL_UNTRUSTED_ERROR);
        assert_eq!(controller.status, Status::Off, "no relay may start");
        assert!(
            !corrupt_path.exists(),
            "the untrusted journal must not stay in place"
        );
        assert!(
            corrupt_path.with_extension("corrupt").exists(),
            "the untrusted journal is quarantined, not silently discarded"
        );

        // A mismatched session tag is equally untrusted.
        let other =
            RuntimeEventJournal::open(dir.path(), "target:fixture@01", "session:fixture@02")
                .expect("journal setup");
        std::fs::write(
            &other.path,
            serde_json::to_vec(&json!({
                "schemaVersion": JOURNAL_SCHEMA_VERSION,
                "scope": "00000000000000000000000000000000",
                "runs": {},
            }))
            .unwrap(),
        )
        .expect("plant mismatched journal");
        assert_eq!(other.load().unwrap_err(), JOURNAL_UNTRUSTED_ERROR);
    }

    #[test]
    fn start_recovers_journaled_envelopes_and_resends_on_connect() {
        let dir = tempfile::tempdir().expect("journal tempdir");
        {
            let journal =
                RuntimeEventJournal::open(dir.path(), "target:fixture@01", "session:fixture@01")
                    .expect("journal setup");
            let envelope = runtime_envelope(
                3,
                "turn.completed",
                Some("turn_fixture"),
                "2026-08-08T12:00:00Z".to_string(),
                json!({ "turn": { "status": "completed", "usage": {} } }),
            );
            let encoded_len = serde_json::to_vec(&envelope).unwrap().len();
            let pending = HashMap::from([(
                "run_fixture".to_string(),
                BTreeMap::from([(
                    3,
                    PendingRuntimeEnvelope {
                        envelope,
                        encoded_len,
                        integrity: true,
                        handed_off: true,
                    },
                )]),
            )]);
            journal
                .persist(&pending)
                .expect("previous process persisted");
        }

        let mut controller = RemoteControlController::default();
        let journal =
            RuntimeEventJournal::open(dir.path(), "target:fixture@01", "session:fixture@01")
                .expect("journal setup");
        controller.reset_pending_from(journal.load().expect("clean recovery"));
        controller.journal = Some(journal);
        assert!(
            controller.has_unacknowledged_integrity_events(),
            "recovered terminal state must gate /rc stop until acknowledged"
        );
        assert!(controller.stop_refusal().is_some());

        let (worker_tx, mut worker_rx) = mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        controller.worker_tx = Some(worker_tx);
        controller.event_rx = Some(event_rx);
        event_tx
            .send(RemoteEvent::Connected {
                account_ref: "account_fixture".to_string(),
                runner_id: "runner_fixture".to_string(),
                target_ref: "target_fixture".to_string(),
                attachment: RemoteAttachment {
                    run_id: "run_other".to_string(),
                    workspace_id: "workspace_fixture".to_string(),
                    runtime_cursor: 0,
                    snapshot_present: false,
                    runtime_chat_relay_protocol: RUNTIME_CHAT_RELAY_PROTOCOL.to_string(),
                    runtime_chat_relay_challenge: "a".repeat(32),
                },
                links: RemoteLinks::default(),
            })
            .unwrap();
        controller.try_next_event().unwrap();
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("recovered envelopes must resend on connect");
        };
        assert_eq!(envelopes[0]["seq"], 3);
        assert_eq!(envelopes[0]["event"], "turn.completed");
    }

    #[test]
    fn delta_pressure_sheds_to_resync_and_preserves_integrity_capacity() {
        let (mut controller, _worker_rx, _event_tx, _journal_root) = wired_controller();
        let delta_budget = MAX_JOURNAL_EVENTS - JOURNAL_RESERVED_INTEGRITY_EVENTS;
        for index in 0..delta_budget {
            assert!(
                controller.queue_runtime_envelope(
                    "run_fixture",
                    runtime_envelope(
                        (index + 1) as u64,
                        "item.delta",
                        Some("turn_fixture"),
                        "2026-08-08T12:00:00Z".to_string(),
                        json!({ "kind": "agent_message", "delta": index.to_string() }),
                    ),
                ),
                "delta {index} fits the unreserved budget"
            );
        }
        assert_eq!(controller.pending_event_count, delta_budget);

        let shed_seq = (delta_budget + 1) as u64;
        assert!(
            !controller.queue_runtime_envelope(
                "run_fixture",
                runtime_envelope(
                    shed_seq,
                    "item.delta",
                    Some("turn_fixture"),
                    "2026-08-08T12:00:00Z".to_string(),
                    json!({ "kind": "agent_message", "delta": "over budget" }),
                ),
            ),
            "a delta beyond the unreserved budget is shed"
        );
        assert_eq!(controller.pending_event_count, delta_budget);
        assert!(controller.resync_required.contains("run_fixture"));
        assert_ne!(
            controller.status,
            Status::Failed,
            "delta pressure is ordinary and must not fail the relay"
        );

        // Reserved capacity keeps the terminal boundary deliverable, and the
        // terminal boundary schedules the resynchronization snapshot.
        controller
            .activate_prompt("run_fixture", "turn_fixture")
            .unwrap();
        controller.observe_engine_event(&turn_complete_event());
        assert!(
            controller.has_unacknowledged_integrity_events(),
            "the terminal envelope must use the reserved capacity"
        );
        assert_eq!(
            controller.take_pending_resync().as_deref(),
            Some("run_fixture"),
            "the shed run resynchronizes at its terminal boundary"
        );
        controller.upload_resync_snapshot("run_fixture", &[]);
        assert!(
            controller
                .pending_runtime_events
                .get("run_fixture")
                .is_some_and(|events| events.values().any(|entry| runtime_envelope_event(
                    &entry.envelope
                ) == Some("session.snapshot"))),
            "the bounded snapshot restores account truth"
        );
    }

    #[test]
    fn integrity_overflow_fails_closed_without_restoring_input() {
        let (mut controller, _worker_rx, _event_tx, _journal_root) = wired_controller();
        controller.status = Status::Connected;
        for index in 0..MAX_JOURNAL_EVENTS {
            assert!(controller.queue_runtime_envelope(
                "run_fixture",
                runtime_envelope(
                    (index + 1) as u64,
                    "item.failed",
                    Some("turn_fixture"),
                    "2026-08-08T12:00:00Z".to_string(),
                    json!({ "item": { "id": index.to_string(), "kind": "error" } }),
                ),
            ));
        }

        assert!(!controller.queue_runtime_envelope(
            "run_fixture",
            runtime_envelope(
                (MAX_JOURNAL_EVENTS + 1) as u64,
                "turn.completed",
                Some("turn_fixture"),
                "2026-08-08T12:00:00Z".to_string(),
                json!({ "turn": { "status": "failed", "usage": {} } }),
            ),
        ));
        assert_eq!(
            controller.status,
            Status::Failed,
            "losing integrity state can never be silent"
        );
        assert!(
            !controller.can_share_approval_with_web(),
            "a failed-closed relay keeps the shared-decision channel closed"
        );
    }

    #[test]
    fn message_deltas_coalesce_until_a_handoff_boundary() {
        let (mut controller, mut worker_rx, _event_tx, _journal_root) = wired_controller();
        controller
            .activate_prompt("run_fixture", "turn_fixture")
            .unwrap();
        controller.observe_engine_event(&EngineEvent::MessageDelta {
            index: 0,
            content: "Hello ".to_string(),
        });
        controller.observe_engine_event(&EngineEvent::MessageDelta {
            index: 0,
            content: "world".to_string(),
        });
        assert!(
            worker_rx.try_recv().is_err(),
            "deferred deltas coalesce before any transport handoff"
        );
        let events = controller
            .pending_runtime_events
            .get("run_fixture")
            .unwrap();
        assert_eq!(events.len(), 1, "both deltas share one envelope");
        assert_eq!(
            events.values().next().unwrap().envelope["payload"]["delta"],
            "Hello world"
        );

        controller.observe_engine_event(&EngineEvent::ToolCallStarted {
            model_call: None,
            id: "tool_fixture".to_string(),
            name: "shell".to_string(),
            input: json!({}),
        });
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("the coalesced delta must hand off before a later event");
        };
        assert_eq!(envelopes[0]["event"], "item.delta");
        assert_eq!(envelopes[0]["payload"]["delta"], "Hello world");
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("the tool event follows the coalesced delta");
        };
        assert_eq!(envelopes[0]["event"], "item.started");

        // Once handed off an envelope is immutable; new deltas open a fresh
        // envelope that flushes on the next UI poll.
        controller.observe_engine_event(&EngineEvent::MessageDelta {
            index: 0,
            content: "again".to_string(),
        });
        assert!(worker_rx.try_recv().is_err());
        assert!(controller.try_next_event().is_none());
        let WorkerCommand::Upload { envelopes, .. } = worker_rx.try_recv().unwrap() else {
            panic!("the UI poll hands the deferred delta to the transport");
        };
        assert_eq!(envelopes[0]["payload"]["delta"], "again");
    }
    #[test]
    fn runtime_image_legacy_work_never_downgrades_to_text() {
        let image = crate::image_attach::tests::runtime_image_fixture(1);
        let command = json!({"type":"prompt.request","runId":"run_fixture","turnId":"turn_fixture","prompt":"look","images":[image]});
        assert!(
            parse_remote_command(&command, "run_fixture")
                .unwrap_err()
                .contains("legacy remote Work")
        );
        let mut text = command;
        text.as_object_mut().unwrap().remove("images");
        assert!(matches!(
            parse_remote_command(&text, "run_fixture").unwrap(),
            RemoteCommand::Prompt { .. }
        ));
    }

    #[tokio::test]
    async fn runtime_image_command_response_has_bounded_larger_budget() {
        use axum::{Router, routing::get};
        let payload = serde_json::to_string(
            &json!({"commands":[],"fixture": "x".repeat(MAX_RESPONSE_BYTES + 1)}),
        )
        .unwrap();
        let app = Router::new().route(
            "/",
            get(move || {
                let payload = payload.clone();
                async move { payload }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = crate::tls::reqwest_client();
        let url = format!("http://{addr}/");
        assert!(
            read_bounded_json(client.get(&url).send().await.unwrap())
                .await
                .is_err()
        );
        let parsed = read_bounded_json_with_limit(
            client.get(&url).send().await.unwrap(),
            codewhale_protocol::runtime::MAX_RUNTIME_IMAGE_BODY_BYTES,
        )
        .await
        .unwrap();
        assert_eq!(
            parsed["fixture"].as_str().unwrap().len(),
            MAX_RESPONSE_BYTES + 1
        );
        server.abort();
    }
}

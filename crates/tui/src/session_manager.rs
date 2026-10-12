//! Session management for resuming conversations.
//!
//! This module provides functionality for:
//! - Saving sessions to disk
//! - Listing previous sessions
//! - Resuming sessions by ID
//! - Managing session lifecycle

use crate::approval_log::{ApprovalReceipt, ApprovalReceiptStore, ApprovalReplay};
use crate::artifacts::ArtifactRecord;
use crate::config::ProviderKind;
use crate::model_routing::AutoRouteReceipt;
use crate::project_context::find_git_root;
use crate::session_tree::{SessionEntry, SessionImportContainer, SessionJournal};
use crate::tools::goal::{GoalPauseReason, GoalSnapshot};
use crate::tools::plan::PlanSnapshot;
use crate::tools::todo::TodoListSnapshot;
use crate::utils::write_atomic;
use crate::work_graph::ReasoningEffortTier;
use chrono::{DateTime, Utc};
use codewhale_core::ContextReference;
#[cfg(test)]
use codewhale_core::{ContextReferenceKind, ContextReferenceSource};
use codewhale_models::{ContentBlock, Message, SystemPrompt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;
use uuid::Uuid;

/// Maximum number of active (non-archived) transcripts to retain.
///
/// A transcript that falls out of this window is archived, never unlinked
/// (#6136); archived records sit outside the cap until the user prunes them,
/// and empty auto-created stubs are capped separately (#6137).
const MAX_SESSIONS: usize = 50;
/// Maximum empty auto-created stubs ("New Session", zero messages) to keep.
///
/// The product writes one per boot; they are junk that must never occupy a
/// transcript's slot in the cap (#6137).
const MAX_EMPTY_SESSION_STUBS: usize = 10;
/// Maximum session title length, in `char`s. Matches the bound the session
/// picker's rename prompt has always enforced.
pub const MAX_SESSION_TITLE_CHARS: usize = 100;
pub(crate) const WORK_GRAPH_IMPORT_ARCHIVE_DIR: &str = ".work-graph-import-archive";
/// Per-session JSONL sidecar holding journal entries a bounded save moved out
/// of the session document (#6842): `<sessions>/.journal-archive/<id>.jsonl`.
pub(crate) const JOURNAL_ARCHIVE_DIR: &str = ".journal-archive";
/// Off-branch entries a session document may carry before an autosave
/// archives the excess. Together with [`JOURNAL_RETAINED_DEAD_ENTRIES`] this
/// keeps a compacted journal far below `MAX_CANONICAL_HISTORY_ENTRIES`.
const JOURNAL_PRUNE_DEAD_THRESHOLD: usize = 4_096;
/// Recent off-branch entries kept in the document after archiving, so `/tree`
/// and `/branch` stay cheap for recent history.
const JOURNAL_RETAINED_DEAD_ENTRIES: usize = 1_024;
const SESSION_GOALS_DIR: &str = ".goals";
const CURRENT_SESSION_GOAL_SCHEMA_VERSION: u32 = 2;
const MAX_SESSION_GOAL_OBJECTIVE_CHARS: usize = 8_192;
const MAX_SESSION_GOAL_FILE_BYTES: u64 = 64 * 1_024;
pub(crate) const CURRENT_SESSION_SCHEMA_VERSION: u32 = 1;
const CURRENT_QUEUE_SCHEMA_VERSION: u32 = 1;
const LATE_USAGE_DIR: &str = ".late-usage";
const CURRENT_LATE_USAGE_SCHEMA_VERSION: u32 = 1;
const MAX_LATE_USAGE_UNRESOLVED_RECORDS_PER_SESSION: usize = 64;
const MAX_LATE_USAGE_LEDGER_BYTES: u64 = 1024 * 1024;
const LATE_USAGE_DELETED: &[u8] = b"codewhale-session-deleted-v1\n";
fn is_false(value: &bool) -> bool {
    !*value
}

const LATE_USAGE_UNAVAILABLE_REASON: &str = "late_usage_ledger_unavailable";

#[derive(Clone, Copy)]
enum SessionRemoval {
    Explicit,
    Retention,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LateUsageRecord {
    source_fingerprint: String,
    turn_fingerprint: String,
    route: crate::cost_status::EffectiveRouteEnvelope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    usage: Option<codewhale_models::Usage>,
    #[serde(
        default,
        skip_serializing_if = "crate::cost_status::RuntimeUsageMissingReason::is_success"
    )]
    reason: crate::cost_status::RuntimeUsageMissingReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    decision: Option<crate::cost_status::RuntimeDecisionReceipt>,
}

impl LateUsageRecord {
    fn captured(
        turn_id: &str,
        source_id: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
        usage: Option<&codewhale_models::Usage>,
        decision: Option<&crate::cost_status::RuntimeDecisionReceipt>,
        reason: crate::cost_status::RuntimeUsageMissingReason,
    ) -> Self {
        Self {
            source_fingerprint: crate::cost_status::usage_source_fingerprint(source_id),
            turn_fingerprint: crate::cost_status::usage_source_fingerprint(turn_id),
            route: route.sanitized_for_persistence(),
            usage: usage
                .filter(|usage| **usage != codewhale_models::Usage::default())
                .cloned(),
            reason,
            decision: decision.map(crate::cost_status::RuntimeDecisionReceipt::sanitized),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LateUsageLedger {
    schema_version: u32,
    #[serde(default)]
    records: Vec<LateUsageRecord>,
    #[serde(default)]
    overflowed: bool,
}

impl Default for LateUsageLedger {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_LATE_USAGE_SCHEMA_VERSION,
            records: Vec::new(),
            overflowed: false,
        }
    }
}

fn is_sha256_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

const fn default_session_schema_version() -> u32 {
    CURRENT_SESSION_SCHEMA_VERSION
}

const fn default_queue_schema_version() -> u32 {
    CURRENT_QUEUE_SCHEMA_VERSION
}

fn normalize_managed_dir(path: PathBuf) -> std::io::Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "managed directory path cannot be empty",
        ));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        )
    }) && path.is_relative()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "managed directory path cannot contain traversal components",
        ));
    }
    if path.is_absolute() {
        return Ok(path);
    }
    std::env::current_dir().map(|cwd| cwd.join(path))
}

/// Open (creating if needed) an advisory-lock sidecar that is owner-only
/// (0600), one regular link, and never followed through a symlink.
pub(crate) fn open_private_lock_file(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = options.open(path)?;
        validate_private_regular_file(&file, path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let file = options.open(path)?;
        validate_private_regular_file(&file, path)?;
        Ok(file)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let file = options.open(path)?;
        validate_private_regular_file(&file, path)?;
        Ok(file)
    }
}

fn open_private_read_file(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    validate_private_regular_file(&file, path)?;
    Ok(file)
}

#[cfg(unix)]
fn validate_private_regular_file(file: &fs::File, path: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "private sidecar file {} must be one regular filesystem link",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn validate_private_regular_file(file: &fs::File, path: &Path) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
    };

    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "private sidecar file {} must be a non-reparse regular file",
                path.display()
            ),
        ));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` keeps the handle valid and `info` is writable for the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if info.nNumberOfLinks != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "private sidecar file {} must have exactly one filesystem link",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
fn validate_private_regular_file(file: &fs::File, path: &Path) -> io::Result<()> {
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("private sidecar file {} must be regular", path.display()),
        ));
    }
    Ok(())
}

/// Persisted queued message for offline/degraded mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedSessionMessage {
    pub display: String,
    #[serde(default)]
    pub skill_instruction: Option<String>,
    #[serde(default)]
    pub skill_provenance: Option<crate::skills::SkillProvenance>,
}

/// Persisted queue state for recovery after restart/crash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineQueueState {
    #[serde(default = "default_queue_schema_version")]
    pub schema_version: u32,
    /// Session ID this queue belongs to. Redundant with the per-session file
    /// name it is stored under; the UI's restore path still compares it
    /// against the live session before adopting the messages.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub messages: Vec<QueuedSessionMessage>,
    #[serde(default)]
    pub draft: Option<QueuedSessionMessage>,
}

/// Result of explicitly repairing a persisted session for process resume.
///
/// Normal snapshot reads must not infer that an unmatched tool call crashed:
/// an embedding host can persist and inspect a session while that tool is
/// still running. Hosts should use [`SessionManager::load_session_snapshot`]
/// during normal operation and reserve this recovery path for a known process
/// or engine restart.
#[derive(Debug, Clone)]
pub struct SessionRecovery {
    pub session: SavedSession,
    pub changed: bool,
    #[cfg_attr(not(test), expect(dead_code))]
    pub repaired_call_count: usize,
    #[cfg_attr(not(test), expect(dead_code))]
    pub duplicate_result_count: usize,
    #[cfg_attr(not(test), expect(dead_code))]
    pub orphan_result_count: usize,
}

impl Default for OfflineQueueState {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_QUEUE_SCHEMA_VERSION,
            session_id: None,
            messages: Vec::new(),
            draft: None,
        }
    }
}

/// Durable context-reference metadata attached to a user message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionContextReference {
    pub message_index: usize,
    pub reference: ContextReference,
}

/// Session metadata stored with each saved session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    /// Unique session identifier
    pub id: String,
    /// Actual host Runtime authority; independent of the conversation id.
    /// Legacy/imported conversations have no binding until saved by a host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_store: Option<crate::runtime_threads::RuntimeStoreBinding>,
    /// Human-readable title (derived from first message)
    pub title: String,
    /// When the session was created
    pub created_at: DateTime<Utc>,
    /// When the session was last updated
    pub updated_at: DateTime<Utc>,
    /// Number of messages in the session
    pub message_count: usize,
    /// Total tokens used
    pub total_tokens: u64,
    /// Model used for the session
    pub model: String,
    /// Provider used for the session model. Defaults for legacy saved sessions.
    #[serde(default = "default_model_provider")]
    pub model_provider: String,
    /// Exact configured provider key. This is separate from `model_provider`
    /// so old consumers can keep treating that field as the built-in provider
    /// kind (`custom` for every named custom route).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    /// Workspace directory
    pub workspace: PathBuf,
    /// Optional mode label (agent/plan/etc.)
    #[serde(default)]
    pub mode: Option<String>,
    /// Accumulated cost data for persisted billing and high-water mark.
    #[serde(default)]
    pub cost: SessionCostSnapshot,
    /// Source session id when this session was created with `deepseek fork`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// Source message count at fork time. This is intentionally coarse:
    /// current saved sessions are linear JSON files, not per-entry trees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from_message_count: Option<usize>,
    /// Cumulative turn duration in seconds (sum of completed turn elapsed
    /// times). Persisted so the footer "worked" chip survives restarts
    /// (#2038).
    #[serde(default)]
    pub cumulative_turn_secs: u64,
    /// Durable archive flag (#2934 / #4397). Archived sessions stay on disk
    /// and stay loadable; they are hidden from the default browse surfaces
    /// and are never chosen by auto-resume.
    ///
    /// This mirrors `ThreadRecord::archived` in [`crate::runtime_threads`] so
    /// the TUI session surfaces and the Runtime API/web dashboard project the
    /// same lifecycle field instead of two divergent notions of "put away".
    /// Additive and `skip_serializing_if`-guarded: sessions written before
    /// v0.9.2 load as `archived = false` and round-trip byte-identically
    /// until the flag is actually set.
    #[serde(default, skip_serializing_if = "is_not_archived")]
    pub archived: bool,
    #[serde(default)]
    pub spawn_depth: u32,
}

fn is_not_archived(archived: &bool) -> bool {
    !*archived
}

/// Sessions currently owned by an in-process interactive surface (the TUI).
///
/// A saved session is a file, and a running TUI holds the authoritative copy
/// in memory: it autosaves the whole document from `App` state. That makes an
/// out-of-band write to the *same* session unsafe — the next autosave would
/// silently revert it. Rather than let that happen quietly, the owner claims
/// the id here and any external writer is refused.
///
/// A static registry rather than a field on `RuntimeApiState` because the
/// embedded Runtime API runs inside the TUI process. A standalone
/// `codewhale web` has an empty registry, so external writers consult
/// [`SessionManager::is_session_live_anywhere`], which also sees the
/// cross-process lease a TUI in another process holds.
static LIVE_SESSIONS: std::sync::OnceLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn live_sessions() -> &'static std::sync::RwLock<std::collections::HashSet<String>> {
    LIVE_SESSIONS.get_or_init(Default::default)
}

/// Who is asking to mutate a saved session.
///
/// This is an authority distinction, not a convenience one: the owner may
/// write because it will update its in-memory copy in the same step; anyone
/// else may not, because it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionMutator {
    /// The in-process surface that currently owns the session (the TUI). It
    /// is responsible for updating its cached metadata atomically with the
    /// write — see `App::apply_session_mutation`.
    Owner,
    /// Any other writer: the Runtime API, the web dashboard, a second
    /// process. Refused while the session is claimed.
    External,
}

/// Set the claimed session to exactly `session_id` (or nothing).
///
/// The TUI owns at most one session at a time, so switching sessions must
/// release the previous claim in the same step — otherwise a `/new` would
/// leave the old id permanently locked against the dashboard.
pub fn set_live_session(session_id: Option<&str>) {
    let session_id = session_id.map(str::trim).filter(|id| !id.is_empty());
    if let Ok(mut live) = live_sessions().write() {
        live.clear();
        if let Some(id) = session_id {
            live.insert(id.to_string());
        }
    }
    // A lease for any other session is released with the claim it backed.
    if let Ok(mut lease) = LIVE_SESSION_LEASE.lock()
        && lease.as_ref().map(|(id, _)| id.as_str()) != session_id
    {
        *lease = None;
    }
}

/// The cross-process half of the live claim (#6144): an exclusive lock on
/// `.late-usage/<id>.live`, held for as long as this process owns the
/// session. The registry above only protects against writers in this
/// process; a standalone `codewhale serve` or a second TUI could still
/// rewrite or delete the document an interactive session is about to
/// autosave over. This lock is separate from the per-write `<id>.lock`, so the
/// owner's own saves never contend with it. It is a liveness signal only —
/// the file carries no data. `None` for the file records a lease that could
/// not be taken (another process holds it), so it is not retried per save.
static LIVE_SESSION_LEASE: std::sync::Mutex<Option<(String, Option<fs::File>)>> =
    std::sync::Mutex::new(None);

/// Lock attempts [`SessionManager::reserve_session_for_attach`] makes
/// before it treats contention as a running owner.
const LIVE_LEASE_ATTACH_ATTEMPTS: u32 = 3;

/// The refusal for attaching to a session another process has open.
pub(crate) fn session_open_elsewhere(session_id: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::ResourceBusy,
        format!(
            "session {session_id} is open in another Codewhale window. \
             Continue it there, or run `codewhale fork {session_id}` to work on a copy"
        ),
    )
}

/// A reservation of a session's cross-process live lease, taken by
/// [`SessionManager::reserve_session_for_attach`] before a surface attaches.
/// Dropping it releases the reservation; [`Self::commit`] makes the session
/// this process's live session and releases the previous one's lease.
#[must_use = "an uncommitted reservation is released when dropped"]
#[derive(Debug)]
pub struct SessionLease {
    id: String,
    /// `None` when this process already holds the session's lease.
    file: Option<fs::File>,
}

impl SessionLease {
    /// Make the reserved session this process's live session. Called after
    /// the attach has been validated and applied, so a failed attach never
    /// costs the session this process already owns its lease.
    pub fn commit(self) {
        set_live_session(Some(&self.id));
        if let Some(file) = self.file
            && let Ok(mut lease) = LIVE_SESSION_LEASE.lock()
        {
            *lease = Some((self.id, Some(file)));
        }
    }
}

/// Is this session currently owned by **this process's** interactive surface?
///
/// The registry is process-local. Reclamation must not treat a missing entry
/// here as proof that no other Codewhale process still owns the directory.
#[must_use]
pub fn is_live_session(session_id: &str) -> bool {
    live_sessions()
        .read()
        .is_ok_and(|live| live.contains(session_id))
}

/// The error an external writer gets when the session is live.
///
/// `ResourceBusy` so callers can map it to a typed conflict rather than
/// pattern-matching on a message.
pub(crate) fn live_session_conflict(session_id: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::ResourceBusy,
        format!(
            "session '{session_id}' is open in an interactive Codewhale session; \
             change it there instead — an external write would be reverted by its next autosave"
        ),
    )
}

/// File-name stem of the sidecar mapping session ids to the session
/// instance (process boot) that created their persisted record. Lives in
/// the sessions directory next to the `<id>.json` records it describes.
const SESSION_BOOT_OWNERS_STEM: &str = "session_boot_owners";

static SESSION_BOOT_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Identity of this running session instance (one per process boot).
///
/// Mirrors the `SubAgentManager` boot id from #405: persisted records are
/// stamped with the instance that created them, so a later Codewhale
/// instance in the same workspace can tell restored rows from its own live
/// work (#4416).
#[must_use]
pub fn current_session_boot_id() -> &'static str {
    SESSION_BOOT_ID.get_or_init(|| format!("boot_{}", &Uuid::new_v4().to_string()[..12]))
}

/// Which archive states a session listing includes.
///
/// Deliberately the same three-way shape as
/// [`crate::runtime_threads::ThreadListFilter`] so `/v1/sessions` and
/// `/v1/threads` answer the same `include_archived` / `archived_only` query
/// pair with the same semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionListFilter {
    /// Only `archived = false` sessions. The browse default.
    #[default]
    ActiveOnly,
    /// Active and archived sessions, newest first.
    IncludeArchived,
    /// Only `archived = true` sessions.
    ArchivedOnly,
}

impl SessionListFilter {
    /// Resolve the `include_archived` / `archived_only` query pair the same
    /// way the threads routes do.
    #[must_use]
    pub fn from_query(include_archived: Option<bool>, archived_only: Option<bool>) -> Self {
        if archived_only.unwrap_or(false) {
            Self::ArchivedOnly
        } else if include_archived.unwrap_or(false) {
            Self::IncludeArchived
        } else {
            Self::ActiveOnly
        }
    }

    #[must_use]
    pub fn admits(self, archived: bool) -> bool {
        match self {
            Self::ActiveOnly => !archived,
            Self::IncludeArchived => true,
            Self::ArchivedOnly => archived,
        }
    }
}

fn default_model_provider() -> String {
    "deepseek".to_string()
}

impl SessionMetadata {
    pub(crate) fn set_model_provider_route(&mut self, kind: &str, identity: Option<&str>) {
        self.model_provider = kind.to_string();
        self.model_provider_id = identity.map(str::to_string);
    }
}

/// Cost and high-water-mark fields persisted with each session.
///
/// The coverage fields below are persisted **alongside** the money so a restored
/// session can still say what its total covers. Without them a reload produced a
/// dollar figure with no completeness information, which then rendered as "0 of 0
/// turns priced" — a fabricated claim of a complete total. Sessions written
/// before these fields existed deserialize them from `Default`, which is
/// indistinguishable from that same false reading, so the load path detects the
/// legacy shape explicitly (see [`Self::coverage_is_legacy_unknown`]) rather than
/// trusting the defaults (#4318).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionCostSnapshot {
    /// Accumulated parent-turn session cost in USD.
    #[serde(default)]
    pub session_cost_usd: f64,
    /// Accumulated parent-turn session cost in CNY.
    #[serde(default)]
    pub session_cost_cny: f64,
    /// Accumulated sub-agent/background LLM cost in USD.
    #[serde(default)]
    pub subagent_cost_usd: f64,
    /// Accumulated sub-agent/background LLM cost in CNY.
    #[serde(default)]
    pub subagent_cost_cny: f64,
    /// Max-ever displayed session+subagent cost in USD (preserves #244
    /// monotonic guarantee across session restarts).
    #[serde(default)]
    pub displayed_cost_high_water_usd: f64,
    /// Max-ever displayed session+subagent cost in CNY.
    #[serde(default)]
    pub displayed_cost_high_water_cny: f64,
    /// Turns whose route was money-metered and produced an authoritative price.
    /// These are exactly the turns the persisted totals contain.
    #[serde(default)]
    pub priced_turns: u32,
    /// Money-metered (or unknown-basis) turns that produced no authoritative
    /// price, so their spend is missing from the persisted totals.
    #[serde(default)]
    pub unpriced_turns: u32,
    /// CNY-specific coverage. USD-only routes are unpriced in CNY rather than
    /// silently contributing a fabricated zero.
    #[serde(default)]
    pub cny_priced_turns: u32,
    #[serde(default)]
    pub cny_unpriced_turns: u32,
    /// Stable reason labels for the unpriced turns.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub unpriced_reasons: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub cny_unpriced_reasons: BTreeSet<String>,
    /// Token classes used on some route that carry no published price.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub unpriced_classes: BTreeSet<String>,
    /// Provenance labels of the pricing rows the totals were built from.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub pricing_provenances: BTreeSet<String>,
    /// Live-pricing downgrade receipts recorded while building the totals.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub live_pricing_defects: BTreeSet<String>,
    /// Live rows that failed validation and had no usable bundled fallback.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub live_pricing_unusable_defects: BTreeSet<String>,
    /// Redacted per-route receipts: provider, configured identity, wire model,
    /// billing surface, endpoint fingerprint, billing mode, currency. Never a URL, a
    /// credential, or a filesystem path.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub route_receipts: BTreeSet<String>,
    /// Redacted provider-response identities already included in the live and
    /// durable sub-agent totals. Worker records persist the same fingerprints.
    ///
    /// Known limit (#6842): unbounded, ~70 bytes per background call. It is
    /// the replay-idempotency set for money (late-usage overlay and restored
    /// dedupe), so it is never capped; `load_session_metadata` grows its read
    /// to fit a large block instead of falling back to a whole-file read.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub usage_source_fingerprints: BTreeSet<String>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "crate::cost_status::deserialize_missing_usage_sources"
    )]
    pub missing_usage_sources: BTreeMap<String, crate::cost_status::MissingUsageCoverage>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub missing_usage_overflowed: bool,
    /// Written by builds that track coverage, so a reader can tell "this session
    /// genuinely had zero money-metered turns" apart from "this session predates
    /// coverage tracking". Absent on legacy rows.
    #[serde(default)]
    pub coverage_recorded: bool,
}

impl SessionCostSnapshot {
    fn absorb_late_background_cost(&mut self, pool: &crate::cost_status::PendingBackgroundCost) {
        let pool = crate::cost_status::project_missing_usage_ledger(
            &mut self.missing_usage_sources,
            &mut self.missing_usage_overflowed,
            &mut self.unpriced_turns,
            &mut self.cny_unpriced_turns,
            pool,
        );
        let estimate = crate::pricing::CostEstimate {
            usd: self.subagent_cost_usd,
            cny: self.subagent_cost_cny,
        }
        .saturating_add(pool.estimate);
        self.subagent_cost_usd = estimate.usd;
        self.subagent_cost_cny = estimate.cny;
        self.priced_turns = self.priced_turns.saturating_add(pool.priced_turns);
        self.unpriced_turns = self.unpriced_turns.saturating_add(pool.unpriced_turns);
        self.cny_priced_turns = self.cny_priced_turns.saturating_add(pool.cny_priced_turns);
        self.cny_unpriced_turns = self
            .cny_unpriced_turns
            .saturating_add(pool.cny_unpriced_turns);
        self.unpriced_reasons
            .extend(pool.unpriced_reasons.iter().map(ToString::to_string));
        self.cny_unpriced_reasons
            .extend(pool.cny_unpriced_reasons.iter().map(ToString::to_string));
        self.unpriced_classes
            .extend(pool.unpriced_classes.iter().map(ToString::to_string));
        self.pricing_provenances
            .extend(pool.pricing_provenances.iter().map(ToString::to_string));
        self.live_pricing_defects
            .extend(pool.live_pricing_defects.iter().map(ToString::to_string));
        self.live_pricing_unusable_defects.extend(
            pool.live_pricing_unusable_defects
                .iter()
                .map(ToString::to_string),
        );
        self.route_receipts
            .extend(pool.route_receipts.iter().cloned());
        self.usage_source_fingerprints
            .extend(pool.usage_source_fingerprints.iter().cloned());
        self.coverage_recorded = true;
        let total = self.total_estimate();
        self.displayed_cost_high_water_usd = self.displayed_cost_high_water_usd.max(total.usd);
        self.displayed_cost_high_water_cny = self.displayed_cost_high_water_cny.max(total.cny);
    }

    /// Session + subagent spend as **one** dual-currency accumulator.
    ///
    /// The persisted USD and CNY columns are projections of per-turn
    /// [`crate::pricing::CostEstimate`]s that were accumulated jointly; every
    /// display total is derived from this single fold so the two currencies
    /// cannot be re-summed by separate code paths that then drift (#4939).
    /// CNY is *not* an FX multiple of USD: a turn carries CNY only when its
    /// route published an authoritative CNY row (provider-published
    /// dual-currency pricing, e.g. DeepSeek's CNY table), and a USD-only turn
    /// contributes exactly zero CNY while `cny_unpriced_turns` records the gap.
    #[must_use]
    pub fn total_estimate(&self) -> crate::pricing::CostEstimate {
        crate::pricing::CostEstimate {
            usd: self.session_cost_usd,
            cny: self.session_cost_cny,
        }
        .saturating_add(crate::pricing::CostEstimate {
            usd: self.subagent_cost_usd,
            cny: self.subagent_cost_cny,
        })
    }

    /// Session + subagent cost in USD.
    pub fn total_usd(&self) -> f64 {
        self.total_estimate()
            .amount(crate::pricing::CostCurrency::Usd)
    }

    /// Session + subagent cost in CNY.
    pub fn total_cny(&self) -> f64 {
        self.total_estimate()
            .amount(crate::pricing::CostCurrency::Cny)
    }

    /// Whether this snapshot's coverage state must be shown as unknown.
    ///
    /// True when the snapshot has no coverage evidence — the signature of a
    /// session written before coverage was persisted. Reporting any such
    /// session as "0 of 0 priced" would claim completeness without evidence,
    /// including when the saved amount is zero.
    #[must_use]
    pub fn coverage_is_legacy_unknown(&self) -> bool {
        !self.coverage_recorded
    }
}

impl SessionMetadata {
    /// Copy cost fields from another metadata (used when forking a session).
    pub fn copy_cost_from(&mut self, other: &SessionMetadata) {
        self.cost = other.cost.clone();
    }

    /// Record additive lineage metadata for a forked saved session.
    pub fn mark_forked_from(&mut self, parent: &SessionMetadata) {
        self.parent_session_id = Some(parent.id.clone());
        self.forked_from_message_count = Some(parent.message_count);
    }
}

/// Durable Work-panel state. Optional on [`SavedSession`] so every session
/// written before v0.8.68 remains loadable without migration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SessionWorkState {
    /// Authoritative Work Graph. Optional so pre-Work-Graph sessions and old
    /// binaries continue to exchange fully populated Plan/To-do views.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<crate::work_graph::WorkGraphSnapshot>,
    #[serde(default, skip_serializing_if = "TodoListSnapshot::is_empty")]
    pub todos: TodoListSnapshot,
    #[serde(default, skip_serializing_if = "PlanSnapshot::is_empty")]
    pub plan: PlanSnapshot,
}

/// Bounded goal projection persisted beside the owning saved session.
///
/// This intentionally excludes completion prose, verifier output, transcripts,
/// and filesystem evidence. The saved session already owns conversation
/// history; restart only needs the typed control state that makes the next turn
/// continue the same objective without trusting text reconstructed from it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionGoalState {
    #[serde(default = "current_session_goal_schema_version")]
    pub schema_version: u32,
    pub objective: String,
    pub status: SessionGoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u32>,
    #[serde(default)]
    pub tokens_used: u64,
    #[serde(default)]
    pub time_used_seconds: u64,
    #[serde(default)]
    pub continuation_count: u32,
    #[serde(default)]
    pub elapsed_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<GoalPauseReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_gap_fingerprint: Option<String>,
    #[serde(default)]
    pub repeated_gap_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_gap_pass: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionGoalStatus {
    Active,
    Paused,
    Complete,
    Blocked,
}

const fn current_session_goal_schema_version() -> u32 {
    CURRENT_SESSION_GOAL_SCHEMA_VERSION
}

impl SessionGoalState {
    /// Convert a runtime update into the durable, bounded session contract.
    /// The canonical empty runtime snapshot removes the sidecar.
    pub fn from_runtime(snapshot: &GoalSnapshot) -> io::Result<Option<Self>> {
        if snapshot.objective.is_none() && snapshot.status.trim() == "none" {
            return Ok(None);
        }
        let objective = snapshot
            .objective
            .as_deref()
            .map(str::trim)
            .filter(|objective| !objective.is_empty())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "goal snapshot has no objective")
            })?;
        let status = match snapshot.status.trim() {
            "active" => SessionGoalStatus::Active,
            "paused" => SessionGoalStatus::Paused,
            "complete" => SessionGoalStatus::Complete,
            "blocked" => SessionGoalStatus::Blocked,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("goal snapshot has unsupported status '{other}'"),
                ));
            }
        };
        let state = Self {
            schema_version: CURRENT_SESSION_GOAL_SCHEMA_VERSION,
            objective: objective.to_string(),
            status,
            token_budget: snapshot.token_budget,
            tokens_used: snapshot.tokens_used,
            time_used_seconds: snapshot.time_used_seconds,
            continuation_count: snapshot.continuation_count,
            elapsed_seconds: snapshot.elapsed_seconds.unwrap_or_default(),
            pause_reason: snapshot.pause_reason,
            goal_id: snapshot.goal_id.clone(),
            last_gap_fingerprint: snapshot.last_gap_fingerprint.clone(),
            repeated_gap_count: snapshot.repeated_gap_count,
            last_gap_pass: snapshot.last_gap_pass,
        };
        state.validate()?;
        Ok(Some(state))
    }

    pub fn validate(&self) -> io::Result<()> {
        if self.schema_version > CURRENT_SESSION_GOAL_SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Session goal schema v{} is newer than supported v{}",
                    self.schema_version, CURRENT_SESSION_GOAL_SCHEMA_VERSION
                ),
            ));
        }
        let objective = self.objective.trim();
        if objective.is_empty() || objective.chars().count() > MAX_SESSION_GOAL_OBJECTIVE_CHARS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Session goal objective must contain 1..={MAX_SESSION_GOAL_OBJECTIVE_CHARS} characters"
                ),
            ));
        }
        if self
            .goal_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 128)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid session goal revision",
            ));
        }
        codewhale_protocol::validate_goal_stall_state(
            self.last_gap_fingerprint.as_deref(),
            self.repeated_gap_count,
            self.last_gap_pass,
            self.continuation_count,
        )
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    #[must_use]
    pub fn to_runtime_snapshot(&self) -> GoalSnapshot {
        GoalSnapshot {
            goal_id: self.goal_id.clone(),
            objective: Some(self.objective.clone()),
            status: match self.status {
                SessionGoalStatus::Active => "active",
                SessionGoalStatus::Paused => "paused",
                SessionGoalStatus::Complete => "complete",
                SessionGoalStatus::Blocked => "blocked",
            }
            .to_string(),
            token_budget: self.token_budget,
            tokens_used: self.tokens_used,
            time_used_seconds: self.time_used_seconds,
            continuation_count: self.continuation_count,
            elapsed_seconds: Some(self.elapsed_seconds),
            evidence: None,
            blocker: None,
            pause_reason: self.pause_reason,
            completion_verification: None,
            advisories: Vec::new(),
            last_gap_fingerprint: self.last_gap_fingerprint.clone(),
            repeated_gap_count: self.repeated_gap_count,
            last_gap_pass: self.last_gap_pass,
            progress: None,
        }
    }
}

impl SessionWorkState {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.graph
            .as_ref()
            .is_none_or(crate::work_graph::WorkGraphSnapshot::is_empty)
            && self.todos.is_empty()
            && self.plan.is_empty()
    }
}

/// Latest concrete Auto route and the decision receipt that produced it.
///
/// This is additive, optional session metadata: sessions written before
/// v0.9.1 deserialize with no receipt and keep their legacy restore behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedAutoRouteReceipt {
    pub(crate) provider: ProviderKind,
    pub(crate) provider_identity: String,
    pub(crate) model: String,
    pub(crate) receipt: AutoRouteReceipt,
    /// Canonical effective reasoning receipt for the selected route, including
    /// routes where a concrete tier cannot be proven. Optional so older
    /// sessions remain loadable.
    pub(crate) effective_reasoning_effort: Option<ReasoningEffortTier>,
}

#[derive(Serialize, Deserialize)]
struct SavedAutoRouteReceiptWire {
    provider: String,
    provider_identity: String,
    model: String,
    receipt: AutoRouteReceipt,
    /// Canonical effective reasoning receipt for the selected route, including
    /// routes where a concrete tier cannot be proven. Optional so older
    /// sessions remain loadable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effective_reasoning_effort: Option<ReasoningEffortTier>,
}

impl Serialize for SavedAutoRouteReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let provider = codewhale_config::descriptors::tui_wire_tag_for_route(
            self.provider,
            &self.provider_identity,
        )
        .ok_or_else(|| serde::ser::Error::custom("contradictory saved provider identity"))?;
        SavedAutoRouteReceiptWire {
            provider: provider.into(),
            provider_identity: self.provider_identity.clone(),
            model: self.model.clone(),
            receipt: self.receipt.clone(),
            effective_reasoning_effort: self.effective_reasoning_effort,
        }
        .serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for SavedAutoRouteReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SavedAutoRouteReceiptWire::deserialize(deserializer)?;
        let provider = codewhale_config::descriptors::kind_from_tui_wire_tag(
            &wire.provider,
            &wire.provider_identity,
        )
        .ok_or_else(|| serde::de::Error::custom("contradictory saved provider identity"))?;
        Ok(Self {
            provider,
            provider_identity: wire.provider_identity,
            model: wire.model,
            receipt: wire.receipt,
            effective_reasoning_effort: wire.effective_reasoning_effort,
        })
    }
}

/// Most turn outcomes one session record keeps; the oldest drop first.
pub(crate) const MAX_SAVED_TURN_OUTCOMES: usize = 64;
/// Longest error text one saved outcome keeps, in `char`s.
const MAX_SAVED_TURN_OUTCOME_ERROR_CHARS: usize = 4_000;

/// A turn that ended `Failed`, as the person saw it end.
///
/// The transcript only holds messages, so before this record a failed turn
/// left nothing but the user's prompt behind: once the TUI closed, resume,
/// export, and the app had no way to say why the turn stopped. The error is
/// the text the live transcript showed, passed through the shared secret
/// redactor before it is stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedTurnOutcome {
    pub status: crate::core::events::TurnOutcomeStatus,
    /// User-facing error text, secrets redacted.
    pub error: String,
    pub ended_at: DateTime<Utc>,
    /// Transcript messages that existed when the turn ended. Resume places
    /// the notice after that many messages (clamped to the transcript).
    pub after_message_count: usize,
}

impl SavedTurnOutcome {
    /// Build the persisted record for a failed turn. Redacts before bounding
    /// so a cut can never leave half a secret behind.
    pub(crate) fn failed(error: &str, after_message_count: usize) -> Self {
        let redacted = codewhale_secrets::redact::redact_secrets(error.trim());
        let error = if redacted.chars().count() > MAX_SAVED_TURN_OUTCOME_ERROR_CHARS {
            let mut cut: String = redacted
                .chars()
                .take(MAX_SAVED_TURN_OUTCOME_ERROR_CHARS)
                .collect();
            cut.push('…');
            cut
        } else {
            redacted
        };
        Self {
            status: crate::core::events::TurnOutcomeStatus::Failed,
            error,
            ended_at: Utc::now(),
            after_message_count,
        }
    }
}

/// Append `outcome`, keeping only the newest [`MAX_SAVED_TURN_OUTCOMES`].
pub(crate) fn push_turn_outcome(outcomes: &mut Vec<SavedTurnOutcome>, outcome: SavedTurnOutcome) {
    outcomes.push(outcome);
    if outcomes.len() > MAX_SAVED_TURN_OUTCOMES {
        let excess = outcomes.len() - MAX_SAVED_TURN_OUTCOMES;
        outcomes.drain(..excess);
    }
}

/// A saved session containing full conversation history
/// Starting with v0.9.5 (#5262) the canonical history is the append-only entry journal (`journal` / `leaf_id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    /// Schema version for migration compatibility
    #[serde(default = "default_session_schema_version")]
    pub schema_version: u32,
    /// Session metadata
    pub metadata: SessionMetadata,
    /// Conversation messages — derived from the journal's active branch (kept for compat).
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<SessionJournal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<String>,
    /// System prompt if any
    pub system_prompt: Option<String>,
    /// Compact linked context references for user-visible `@path` and
    /// `/attach` mentions. Optional for backward-compatible session loads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_references: Vec<SessionContextReference>,
    /// Metadata registry of large outputs produced during this session.
    /// Artifact contents are stored in the session-owned artifact directory.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRecord>,
    /// Session-owned approval evidence. The append-only sidecar is canonical
    /// during a live turn; this projection makes saved snapshots self-
    /// describing without putting receipts in the model transcript.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) approval_receipts: Vec<ApprovalReceipt>,
    /// To-do and plan state shown in the Work sidebar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_state: Option<SessionWorkState>,
    /// User-configured tab/window title for this session (`/title`), shown as
    /// `[title] …` in front of the terminal window title. Optional for
    /// backward-compatible session loads; absent sessions use the `title`
    /// config default instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    /// Most recent accepted/completed Auto decision, when the saved model mode
    /// is `auto`. Optional for backward-compatible session loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_auto_route: Option<SavedAutoRouteReceipt>,
    /// Turns that ended `Failed`, oldest first, bounded to
    /// [`MAX_SAVED_TURN_OUTCOMES`]. Not model context: the terminal-outcome
    /// record resume, export, and the Runtime API read back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) turn_outcomes: Vec<SavedTurnOutcome>,
}
impl SavedSession {
    /// Drop the journal-derived compatibility projection before an async
    /// persistence request takes ownership. Disk serialization restores it.
    pub(crate) fn compact_for_persistence_queue(&mut self) {
        if self.journal.is_some() {
            self.messages = Vec::new();
        }
    }

    /// Bring `messages` and the journal into the shape the on-disk schema
    /// expects, in place.
    ///
    /// This used to be `storage_compatible_copy`, which cloned the whole
    /// session to do it. On the debounced persistence path the caller already
    /// owns the value and `compact_for_persistence_queue` has already emptied
    /// `messages`, so the clone was pure waste — two full deep copies of the
    /// history per write (#6214 T3).
    ///
    /// The no-op cases are load-bearing and must stay no-ops: with no journal,
    /// or with `messages` already equal to the journal's active branch, the
    /// session serializes exactly as it arrived — including a
    /// `metadata.message_count` that disagrees with `messages.len()`. Rewriting
    /// that count here would silently edit live data on every save.
    pub(crate) fn make_storage_compatible(&mut self) {
        let Some(journal) = self.journal.as_ref() else {
            return;
        };
        if self.messages.is_empty() {
            self.messages = journal.to_messages();
        } else {
            if self.messages == journal.to_messages() {
                return;
            }
            // Split the `journal` / `messages` borrows; the take is returned
            // before this function ends, so the session is never left short.
            let messages = std::mem::take(&mut self.messages);
            if let Some(journal) = self.journal.as_mut() {
                journal.rebranch_active_messages(&messages);
                self.leaf_id = journal.leaf_id.clone();
            }
            self.messages = messages;
        }
        self.metadata.message_count = self.messages.len();
    }

    pub fn ensure_journal(&mut self) {
        if self.journal.is_some() {
            if self.leaf_id.is_none() {
                self.leaf_id = self.journal.as_ref().and_then(|j| j.leaf_id.clone());
            }
            let active = self
                .journal
                .as_ref()
                .map(|j| j.to_messages())
                .unwrap_or_default();
            if !active.is_empty() {
                self.messages = active;
                self.metadata.message_count = self.messages.len();
            }
            return;
        }
        let journal =
            SessionJournal::from_messages(self.messages.clone(), self.metadata.spawn_depth);
        self.leaf_id = journal.leaf_id.clone();
        self.journal = Some(journal);
    }
    #[expect(dead_code)]
    pub fn journal_append_message(&mut self, message: Message) -> String {
        self.ensure_journal();
        let journal = self.journal.as_mut().expect("journal ensured");
        let id = journal.append_message(message.clone());
        self.leaf_id = journal.leaf_id.clone();
        self.messages = journal.to_messages();
        self.metadata.message_count = self.messages.len();
        self.metadata.updated_at = Utc::now();
        id
    }
    pub fn journal_branch_to(&mut self, entry_id: &str) -> Result<(), String> {
        self.ensure_journal();
        let journal = self.journal.as_mut().expect("journal ensured");
        journal.branch_to(entry_id)?;
        self.leaf_id = journal.leaf_id.clone();
        self.messages = journal.to_messages();
        self.metadata.message_count = self.messages.len();
        self.metadata.updated_at = Utc::now();
        Ok(())
    }
    #[expect(dead_code)]
    pub fn active_entries(&self) -> Vec<SessionEntry> {
        self.journal
            .as_ref()
            .map(|j| j.root_to_leaf().into_iter().cloned().collect())
            .unwrap_or_default()
    }
    /// `created_at` of the active branch's message entries, in order — the
    /// stamps a resumed session hands back to the live message log so the
    /// next save preserves append times instead of rewriting them to resume
    /// time.
    pub fn journal_message_stamps(&self) -> Vec<DateTime<Utc>> {
        self.journal
            .as_ref()
            .map(|journal| {
                journal
                    .root_to_leaf()
                    .iter()
                    .filter(|entry| {
                        matches!(
                            entry.kind,
                            crate::session_tree::SessionEntryKind::Message { .. }
                        )
                    })
                    .map(|entry| entry.created_at)
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn export_container(&self, source: &str) -> SessionImportContainer {
        let journal = self.journal.clone().unwrap_or_else(|| {
            SessionJournal::from_messages(self.messages.clone(), self.metadata.spawn_depth)
        });
        SessionImportContainer::new(
            source.to_string(),
            &journal,
            serde_json::to_value(&self.metadata).ok(),
        )
    }
    pub fn import_foreign(
        container: SessionImportContainer,
        workspace: PathBuf,
        model: String,
    ) -> Result<Self, String> {
        let journal = container.into_journal()?;
        validate_saved_journal(&journal).map_err(|error| error.to_string())?;
        let leaf_id = journal.leaf_id.clone();
        let messages = journal.to_messages();
        let now = Utc::now();
        let spawn_depth = journal.spawn_depth.saturating_add(1);
        // Reuse the conversation-derived title so an imported session that
        // opens with runtime-owned control traffic (Operate contract, restore
        // checkpoint) is named after the real prompt, not the envelope.
        let title = conversation_derived_title(&messages)
            .unwrap_or_else(|| crate::session_manager::DEFAULT_SESSION_TITLE.to_string());
        let metadata = SessionMetadata {
            id: Uuid::new_v4().to_string(),
            title,
            created_at: now,
            updated_at: now,
            message_count: messages.len(),
            total_tokens: 0,
            model,
            model_provider: default_model_provider(),
            model_provider_id: None,
            workspace,
            mode: None,
            cost: SessionCostSnapshot::default(),
            parent_session_id: None,
            forked_from_message_count: None,
            runtime_store: None,
            cumulative_turn_secs: 0,
            archived: false,
            spawn_depth,
        };
        let mut journal = journal;
        journal.spawn_depth = spawn_depth;
        Ok(Self {
            schema_version: CURRENT_SESSION_SCHEMA_VERSION,
            metadata,
            messages,
            journal: Some(journal),
            leaf_id,
            system_prompt: None,
            context_references: Vec::new(),
            artifacts: Vec::new(),
            approval_receipts: Vec::new(),
            work_state: None,
            window_title: None,
            last_auto_route: None,
            turn_outcomes: Vec::new(),
        })
    }
}

/// Validate every stored branch before deriving a projection. The journal's
/// traversal safely stops on malformed edges, but persistence must refuse such
/// a document instead of treating a truncated path as the complete conversation.
fn validate_saved_journal(journal: &SessionJournal) -> io::Result<()> {
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    if journal.schema_version > crate::session_tree::CURRENT_JOURNAL_SCHEMA_VERSION {
        return Err(invalid("saved journal schema is newer than supported"));
    }
    journal
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut parents = std::collections::HashMap::with_capacity(journal.entries.len());
    for entry in &journal.entries {
        if entry.id.is_empty()
            || parents
                .insert(entry.id.as_str(), entry.parent_id.as_deref())
                .is_some()
        {
            return Err(invalid(
                "saved journal contains an empty or duplicate entry id",
            ));
        }
    }
    let mut colors = std::collections::HashMap::with_capacity(parents.len());
    for id in parents.keys().copied() {
        let mut cursor = Some(id);
        let mut path = Vec::new();
        while let Some(id) = cursor {
            match colors.get(id) {
                Some(1) => return Err(invalid("saved journal contains a cycle")),
                Some(2) => break,
                _ => {
                    colors.insert(id, 1);
                    path.push(id);
                    cursor = parents[id];
                }
            }
        }
        for id in path {
            colors.insert(id, 2);
        }
    }
    Ok(())
}

/// Archived ids per session that the live app has not yet dropped from its
/// in-memory journal. A bounded save records them only after its document
/// write lands; the UI takes them on its next snapshot (no I/O) so RAM
/// matches disk, and a racing save skips re-appending ids still listed here.
fn archived_journal_ids()
-> &'static std::sync::Mutex<std::collections::HashMap<String, std::collections::HashSet<String>>> {
    static IDS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::collections::HashSet<String>>>,
    > = std::sync::OnceLock::new();
    IDS.get_or_init(Default::default)
}

/// Take the ids a bounded save archived for `session_id`, for the live
/// journal to drop. Never touches the disk.
pub(crate) fn take_archived_journal_ids(session_id: &str) -> std::collections::HashSet<String> {
    archived_journal_ids()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_id)
        .unwrap_or_default()
}

fn journal_archive_path(sessions_dir: &Path, session_id: &str) -> io::Result<PathBuf> {
    let id = session_id.trim();
    let mut components = Path::new(id).components();
    if id.is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid session id for journal archive",
        ));
    }
    Ok(sessions_dir
        .join(JOURNAL_ARCHIVE_DIR)
        .join(format!("{id}.jsonl")))
}

/// Every entry archived for `session_id`, oldest first, deduplicated by id
/// (a save that crashed before its document landed re-archives the same
/// entries). A torn final line — a crash mid-append, whose entries are still
/// in the document — is skipped; damage anywhere else is an error.
pub(crate) fn load_journal_archive(
    sessions_dir: &Path,
    session_id: &str,
) -> io::Result<Vec<SessionEntry>> {
    use std::io::Read as _;
    let path = journal_archive_path(sessions_dir, session_id)?;
    let mut file = match open_private_read_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut raw = String::new();
    file.read_to_string(&mut raw)?;
    let lines: Vec<&str> = raw.lines().filter(|line| !line.trim().is_empty()).collect();
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::with_capacity(lines.len());
    for (index, line) in lines.iter().enumerate() {
        match serde_json::from_str::<SessionEntry>(line) {
            Ok(entry) => {
                if seen.insert(entry.id.clone()) {
                    entries.push(entry);
                }
            }
            Err(error) if index + 1 == lines.len() && !raw.ends_with('\n') => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping torn final journal-archive line"
                );
            }
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "journal archive {} line {}: {error}",
                        path.display(),
                        index + 1
                    ),
                ));
            }
        }
    }
    Ok(entries)
}

fn serialize_saved_session(mut session: SavedSession) -> io::Result<String> {
    if let Some(journal) = session.journal.as_ref() {
        validate_saved_journal(journal)?;
    }
    session.make_storage_compatible();
    serde_json::to_string_pretty(&session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Repair dangling tool-call/result pairs in an already-loaded session and
/// rebranch its journal to the repaired messages. Returns the repair receipt;
/// callers decide whether the result gets persisted (`SessionManager::resume_*`
/// does, foreign `/load` files do not).
pub(crate) fn repair_recovered_session(
    session: &mut SavedSession,
) -> crate::tool_history_repair::ToolRepairReceipt {
    let repair = crate::tool_history_repair::repair_tool_call_pairs(&mut session.messages);
    if !repair.is_empty() {
        if let Some(journal) = session.journal.as_mut() {
            journal.rebranch_active_messages(&session.messages);
            session.leaf_id = journal.leaf_id.clone();
        }
        session.metadata.message_count = session.messages.len();
        tracing::warn!(
            session_id = %session.metadata.id,
            repaired_call_ids = ?repair.repaired_call_ids,
            duplicate_result_ids = ?repair.duplicate_result_ids,
            orphan_result_ids = ?repair.orphan_result_ids,
            "repaired persisted tool call/result history"
        );
    }
    repair
}

/// Manager for session persistence operations
#[derive(Debug)]
pub struct SessionManager {
    /// Directory where sessions are stored
    sessions_dir: PathBuf,
    /// Re-entrancy guard: archiving a record saves it, and every save runs
    /// retention. Without this, a backlog past the cap would nest one
    /// cleanup per archived transcript instead of draining in one pass.
    retention_in_progress: AtomicBool,
}

/// One interactive editor owns a session's unsent text until its last queued
/// write finishes. The stable lock file is never unlinked: replacing it would
/// let two processes lock different files for the same session.
#[derive(Debug)]
pub struct OfflineQueueLease {
    session_id: String,
    _file: fs::File,
}

impl OfflineQueueLease {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl Drop for OfflineQueueLease {
    fn drop(&mut self) {
        // A forked child can briefly retain the same open-file description.
        // Release the editor's lock now, rather than waiting for every inherited
        // descriptor to close, as RuntimeProcessOwnerLock does on shutdown.
        #[cfg(all(unix, not(target_os = "solaris")))]
        {
            use std::os::fd::AsRawFd as _;
            // SAFETY: the lease still owns this descriptor throughout Drop.
            unsafe {
                libc::flock(self._file.as_raw_fd(), libc::LOCK_UN);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle as _;
            use windows_sys::Win32::Storage::FileSystem::UnlockFile;
            // SAFETY: the lease owns the handle; fd-lock locks byte 0 only.
            unsafe {
                UnlockFile(self._file.as_raw_handle() as _, 0, 0, 1, 0);
            }
        }
        // fd-lock uses process-associated fcntl locks on Solaris. They are not
        // inherited by fork and closing this descriptor releases the lock.
    }
}

/// Origin of a crash-recovery checkpoint file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointSource {
    /// Per-session checkpoint file `checkpoints/<session_id>.json`.
    Session(String),
    /// Legacy single-slot checkpoint file `checkpoints/latest.json`.
    Legacy,
}

/// A crash-recovery checkpoint file discovered on disk (metadata only —
/// callers load the session content separately).
#[derive(Debug, Clone)]
pub struct CheckpointRef {
    pub source: CheckpointSource,
    #[cfg_attr(not(test), expect(dead_code))]
    pub path: PathBuf,
    pub modified: std::time::SystemTime,
}

/// File names in `checkpoints/` that are never per-session checkpoints.
const LEGACY_CHECKPOINT_FILE: &str = "latest.json";
/// Pre-per-session global offline queue, still read once for migration.
const OFFLINE_QUEUE_FILE: &str = "offline_queue.json";
/// Per-session offline queue file: `checkpoints/<session_id>.offline_queue.json`.
const OFFLINE_QUEUE_SUFFIX: &str = ".offline_queue.json";

pub(crate) fn is_offline_queue_file(name: &str) -> bool {
    name == OFFLINE_QUEUE_FILE || name.ends_with(OFFLINE_QUEUE_SUFFIX)
}

impl SessionManager {
    fn approval_receipt_store(&self) -> ApprovalReceiptStore {
        ApprovalReceiptStore::new(self.sessions_dir.clone())
    }

    fn hydrate_approval_receipts(&self, session: &mut SavedSession) -> io::Result<()> {
        if let Some(durable) = self
            .approval_receipt_store()
            .load_if_present(&session.metadata.id)?
        {
            // Only a missing log permits legacy embedded evidence to stand.
            // An empty or torn-first log must not resurrect an old approval.
            session.approval_receipts = durable;
        }
        ApprovalReplay::from_receipts(&session.approval_receipts)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        Ok(())
    }

    /// Reconstruct completed approvals and interrupted unmatched asks for one
    /// session without consulting the model transcript.
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn replay_approvals(&self, session_id: &str) -> io::Result<ApprovalReplay> {
        self.approval_receipt_store().replay(session_id)
    }

    fn validated_session_id<'a>(&self, id: &'a str) -> std::io::Result<&'a str> {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Session id cannot be empty",
            ));
        }
        if !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Invalid session id '{id}'"),
            ));
        }
        if trimmed == SESSION_BOOT_OWNERS_STEM {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Session id '{trimmed}' collides with a reserved sessions file"),
            ));
        }
        Ok(trimmed)
    }

    /// Metadata of saved session `id`, read without loading its transcript.
    pub fn load_session_metadata_by_id(&self, id: &str) -> std::io::Result<SessionMetadata> {
        Self::load_session_metadata(&self.validated_session_path(id)?)
    }

    fn validated_session_path(&self, id: &str) -> std::io::Result<PathBuf> {
        let trimmed = self.validated_session_id(id)?;
        Ok(self.sessions_dir.join(format!("{trimmed}.json")))
    }

    fn checkpoints_dir(&self) -> PathBuf {
        self.sessions_dir.join("checkpoints")
    }

    fn session_goals_dir(&self) -> PathBuf {
        self.sessions_dir.join(SESSION_GOALS_DIR)
    }

    fn checked_existing_session_goals_dir(&self) -> std::io::Result<Option<PathBuf>> {
        let dir = self.session_goals_dir();
        let metadata = match fs::symlink_metadata(&dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Session goal store {} must be a real directory",
                    dir.display()
                ),
            ));
        }
        Ok(Some(dir))
    }

    fn ensure_session_goals_dir(&self) -> std::io::Result<PathBuf> {
        if let Some(dir) = self.checked_existing_session_goals_dir()? {
            return Ok(dir);
        }
        let dir = self.session_goals_dir();
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        self.checked_existing_session_goals_dir()?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("Session goal store {} was not created", dir.display()),
            )
        })
    }

    fn validated_session_goal_path(&self, session_id: &str) -> std::io::Result<PathBuf> {
        let id = self.validated_session_id(session_id)?;
        Ok(self.session_goals_dir().join(format!("{id}.json")))
    }

    fn checked_existing_session_goal_file(path: &Path) -> std::io::Result<bool> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Session goal {} must be a regular file", path.display()),
            ));
        }
        Ok(true)
    }

    fn validated_checkpoint_path(&self, session_id: &str) -> std::io::Result<PathBuf> {
        let trimmed = self.validated_session_id(session_id)?;
        // Reserved file names inside `checkpoints/` must never collide with a
        // per-session checkpoint file.
        if format!("{trimmed}.json") == LEGACY_CHECKPOINT_FILE
            || format!("{trimmed}.json") == OFFLINE_QUEUE_FILE
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Session id '{trimmed}' collides with a reserved checkpoint file"),
            ));
        }
        Ok(self.checkpoints_dir().join(format!("{trimmed}.json")))
    }

    /// Create a new `SessionManager` with the specified sessions directory
    pub fn new(sessions_dir: PathBuf) -> std::io::Result<Self> {
        let sessions_dir = normalize_managed_dir(sessions_dir)?;
        // Ensure the sessions directory exists
        fs::create_dir_all(&sessions_dir)?;
        Ok(Self {
            sessions_dir,
            retention_in_progress: AtomicBool::new(false),
        })
    }

    /// Create a `SessionManager` using the default location.
    pub fn default_location() -> std::io::Result<Self> {
        Self::new(default_sessions_dir()?)
    }

    /// Return the resolved sessions directory path.
    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// The live-lease file for `session_id`; see [`set_live_session`].
    fn live_lease_path(&self, session_id: &str, create_dir: bool) -> io::Result<PathBuf> {
        let (late_path, _) = if create_dir {
            self.ensure_late_usage_paths(session_id)?
        } else {
            self.late_usage_paths(session_id)?
        };
        Ok(late_path.with_extension("live"))
    }

    /// Claim `session_id` for this process's interactive surface: the
    /// in-process registry ([`set_live_session`]) plus the cross-process
    /// lease in this store, so writers in other processes see it too.
    ///
    /// Known limitation: this claim is best-effort. Attach paths reserve the
    /// lease up front ([`Self::reserve_session_for_attach`]), but a session
    /// that starts fresh is claimed only at its first snapshot, and a claim
    /// that loses the lock to another process's probe or external write, or
    /// cannot open the lease file, runs unleased until a later snapshot
    /// retries it (an open failure is logged only at debug). In that window
    /// another process's external writer can take the lease and write, and
    /// this session's next autosave reverts that write.
    pub fn claim_live_session(&self, session_id: &str) {
        set_live_session(Some(session_id));
        let id = session_id.trim();
        let Ok(mut lease) = LIVE_SESSION_LEASE.lock() else {
            return;
        };
        // A lease already held is kept. One that could not be taken is tried
        // again: another process's liveness probe briefly holds the same lock,
        // and a claim that lost that race once must not run unleased for the
        // rest of the session.
        let retry = match lease.as_ref() {
            Some((held, Some(_))) if held == id => return,
            Some((held, None)) => held == id,
            _ => false,
        };
        let file = self.live_lease_path(id, true).and_then(|path| {
            let file = open_private_lock_file(&path)?;
            Ok(crate::runtime_threads::try_lock_file_exclusive(&file)?.then_some(file))
        });
        match &file {
            Ok(Some(_)) => {}
            Ok(None) if !retry => tracing::warn!(
                session_id = id,
                "another Codewhale process already holds this session open"
            ),
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(session_id = id, %error, "session live lease unavailable");
            }
        }
        *lease = Some((id.to_string(), file.ok().flatten()));
    }

    /// Reserve `session_id` before attaching to it (`resume`, `--continue`,
    /// `exec --resume`, the session picker, `/load`). Unlike
    /// [`Self::claim_live_session`], which records whatever it gets, this
    /// refuses with `ResourceBusy` when another process holds the session
    /// open: attaching anyway gives the document two autosaving writers, and
    /// the last save silently drops the other's turns. Taking the lock is the
    /// check, so nothing can claim the session between a check and the attach.
    ///
    /// The reservation does not touch the session this process owns now: that
    /// claim and its lease stay until [`SessionLease::commit`], which the
    /// caller runs only once the new session is loaded and applied. Dropping
    /// the reservation (a load or apply that failed) releases it and leaves
    /// the current session leased.
    ///
    /// It fails closed: a lease that cannot be opened or locked is an error,
    /// not an unguarded attach.
    pub fn reserve_session_for_attach(&self, session_id: &str) -> io::Result<SessionLease> {
        let id = self.validated_session_id(session_id)?;
        if LIVE_SESSION_LEASE
            .lock()
            .is_ok_and(|lease| matches!(lease.as_ref(), Some((held, Some(_))) if held == id))
        {
            return Ok(SessionLease {
                id: id.to_string(),
                file: None,
            });
        }
        let lease_unavailable = |error: io::Error| {
            io::Error::new(
                error.kind(),
                format!("could not take session {id}'s live lease: {error}"),
            )
        };
        let file = self
            .live_lease_path(id, true)
            .and_then(|path| open_private_lock_file(&path))
            .map_err(lease_unavailable)?;
        // A liveness probe from another process holds this lock for a moment;
        // a few short retries tell that apart from a running owner.
        for attempt in 0..LIVE_LEASE_ATTACH_ATTEMPTS {
            if crate::runtime_threads::try_lock_file_exclusive(&file).map_err(lease_unavailable)? {
                return Ok(SessionLease {
                    id: id.to_string(),
                    file: Some(file),
                });
            }
            if attempt + 1 < LIVE_LEASE_ATTACH_ATTEMPTS {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        Err(session_open_elsewhere(id))
    }

    /// Hold the existing live lease throughout an external mutation. A
    /// liveness probe releases its lock before returning and cannot protect
    /// the subsequent read/write from another surface attaching meanwhile.
    ///
    /// Refuses with `ResourceBusy` when an interactive session in this or
    /// another process holds the session, and with `InvalidInput` for a
    /// malformed id. It may sleep briefly between lock attempts (see
    /// [`Self::reserve_session_for_attach`]), so async callers run it under
    /// `spawn_blocking`. Keep the returned lease alive until the write lands.
    pub(crate) fn reserve_session_for_external_write(&self, id: &str) -> io::Result<SessionLease> {
        let live = live_sessions()
            .read()
            .map_err(|_| io::Error::other("session ownership registry unavailable"))?;
        if live.contains(id.trim()) {
            return Err(live_session_conflict(id));
        }
        drop(live);
        let lease = self.reserve_session_for_attach(id)?;
        // A same-process attach may have committed after the registry read.
        // Its already-held lease is borrowed, never ours to mutate through.
        if lease.file.is_none() {
            return Err(live_session_conflict(id));
        }
        Ok(lease)
    }

    /// Is `session_id` open in an interactive session in this process *or any
    /// other*? A read-only hint for listings and recovery candidates (the
    /// session picker, interrupted-work discovery, retention's candidate
    /// scan). It never authorizes a write: the probe releases its lock before
    /// returning, so every mutation — rename, archive, delete, the Runtime
    /// API's export and save, `scrub-secrets` — holds
    /// [`Self::reserve_session_for_external_write`] across its load and save
    /// instead (#6144).
    ///
    /// Fails closed: a malformed id, or a lease that cannot be opened, reads
    /// as live, so a caller skips it rather than acting on it. Callers that
    /// must tell a malformed id apart (a 400, not a 409) validate first or
    /// reserve the lease, which reports `InvalidInput`.
    ///
    /// Known limitation: startup's stale-checkpoint pruning
    /// (`load_recent_checkpoints` in `lib.rs`) still clears a checkpoint
    /// older than a day after this released probe. A session that attached
    /// in that gap and is mid-turn refreshes its checkpoint, so only a
    /// day-old checkpoint of a session attached in the same instant is
    /// exposed.
    #[must_use]
    pub fn is_session_live_anywhere(&self, session_id: &str) -> bool {
        if is_live_session(session_id) {
            return true;
        }
        let Ok(path) = self.live_lease_path(session_id, false) else {
            return true;
        };
        let file = match open_private_read_file(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
            Err(_) => return true,
        };
        // Contention means a live holder; acquiring proves none, and the
        // probe's lock is released when `file` drops here.
        !matches!(
            crate::runtime_threads::try_lock_file_exclusive(&file),
            Ok(true)
        )
    }

    /// Hold `session_id`'s live lease the way another running Codewhale
    /// process does: an OS lock on its own open file description.
    #[cfg(test)]
    pub(crate) fn hold_live_lease_elsewhere(&self, session_id: &str) -> fs::File {
        let path = self.live_lease_path(session_id, true).expect("lease path");
        let lease = open_private_lock_file(&path).expect("open lease");
        assert!(crate::runtime_threads::try_lock_file_exclusive(&lease).expect("lock lease"));
        lease
    }

    /// Whether a saved document exists for `session_id`.
    #[must_use]
    pub fn session_document_exists(&self, session_id: &str) -> bool {
        self.validated_session_path(session_id)
            .is_ok_and(|path| path.is_file())
    }

    fn late_usage_paths(&self, session_id: &str) -> io::Result<(PathBuf, PathBuf)> {
        let session_id = self.validated_session_id(session_id)?;
        let dir = self.sessions_dir.join(LATE_USAGE_DIR);
        match fs::symlink_metadata(&dir) {
            Ok(metadata) => {
                #[cfg(windows)]
                let linked = {
                    use std::os::windows::fs::MetadataExt as _;
                    metadata.file_attributes()
                        & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                        != 0
                };
                #[cfg(not(windows))]
                let linked = metadata.file_type().is_symlink();
                if linked || !metadata.is_dir() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "late usage store must be a real directory",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok((
            dir.join(format!("{session_id}.json")),
            dir.join(format!("{session_id}.lock")),
        ))
    }

    /// Only mutations create accounting storage. Snapshot/list reads must work
    /// for a healthy transcript even when no sidecar has ever been written.
    fn ensure_late_usage_paths(&self, session_id: &str) -> io::Result<(PathBuf, PathBuf)> {
        self.late_usage_paths(session_id)?;
        let dir = self.sessions_dir.join(LATE_USAGE_DIR);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let paths = self.late_usage_paths(session_id)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&dir)?
                .set_permissions(fs::Permissions::from_mode(0o700))?;
        }
        Ok(paths)
    }

    /// A deletion marker and its stable lock survive deletion, without any
    /// route or usage data. A captured callback must never recreate the ledger.
    fn late_usage_is_deleted(path: &Path) -> io::Result<bool> {
        use std::io::Read as _;
        let tombstone = match open_private_read_file(&path.with_extension("deleted")) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut marker = Vec::with_capacity(LATE_USAGE_DELETED.len());
        tombstone
            .take(u64::try_from(LATE_USAGE_DELETED.len()).unwrap_or(u64::MAX) + 1)
            .read_to_end(&mut marker)?;
        if marker != LATE_USAGE_DELETED {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid late usage deletion marker",
            ));
        }
        Ok(true)
    }

    fn write_late_usage_ledger(path: &Path, ledger: &LateUsageLedger) -> io::Result<()> {
        let bytes = serde_json::to_vec(ledger)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_LATE_USAGE_LEDGER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "late usage ledger exceeds its size bound",
            ));
        }
        write_atomic(path, &bytes)
    }

    fn load_late_usage_unlocked(path: &Path) -> io::Result<LateUsageLedger> {
        let file = match open_private_read_file(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(LateUsageLedger::default());
            }
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        if metadata.len() > MAX_LATE_USAGE_LEDGER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "late usage ledger {} exceeds its size bound",
                    path.display()
                ),
            ));
        }
        use std::io::Read as _;
        let mut raw = Vec::with_capacity(
            usize::try_from(metadata.len().min(MAX_LATE_USAGE_LEDGER_BYTES)).unwrap_or(0),
        );
        file.take(MAX_LATE_USAGE_LEDGER_BYTES.saturating_add(1))
            .read_to_end(&mut raw)?;
        if u64::try_from(raw.len()).unwrap_or(u64::MAX) > MAX_LATE_USAGE_LEDGER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "late usage ledger {} exceeds its size bound",
                    path.display()
                ),
            ));
        }
        let ledger: LateUsageLedger = serde_json::from_slice(&raw)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if ledger.schema_version != CURRENT_LATE_USAGE_SCHEMA_VERSION
            || ledger
                .records
                .iter()
                .filter(|record| {
                    record
                        .usage
                        .as_ref()
                        .is_none_or(|usage| *usage == codewhale_models::Usage::default())
                })
                .count()
                > MAX_LATE_USAGE_UNRESOLVED_RECORDS_PER_SESSION
            || ledger.records.iter().any(|record| {
                !is_sha256_fingerprint(&record.source_fingerprint)
                    || !is_sha256_fingerprint(&record.turn_fingerprint)
                    || record.decision.as_ref().is_some_and(|r| !r.is_bounded())
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "late usage ledger has an unsupported or unbounded shape",
            ));
        }
        Ok(ledger)
    }

    fn with_session_write_admission<T>(
        &self,
        session_id: &str,
        write: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        let (path, lock_path) = self.ensure_late_usage_paths(session_id)?;
        let lock_file = open_private_lock_file(&lock_path)?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock.write()?;
        if Self::late_usage_is_deleted(&path)? {
            return Ok(None);
        }
        write().map(Some)
    }

    /// Run an out-of-band rewrite of a session's files (`scrub-secrets`)
    /// under the same per-session lock every save takes, so it cannot
    /// interleave with a save, and while holding the session's live lease, so
    /// no interactive session holds the conversation in memory to put the old
    /// content back on its next autosave. `None` when the session was
    /// deleted; an invalid id is an `InvalidInput` error, and a session open
    /// in an interactive surface is `ResourceBusy`, left untouched.
    ///
    /// The lease is taken inside the save lock with non-blocking attempts, so
    /// the reverse order elsewhere (lease, then a save) cannot deadlock: one
    /// side reports busy instead.
    pub(crate) fn with_session_file_lock<T>(
        &self,
        session_id: &str,
        rewrite: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<Option<T>> {
        let session_id = self.validated_session_id(session_id)?;
        self.with_session_write_admission(session_id, || {
            let _lease = self.reserve_session_for_external_write(session_id)?;
            rewrite()
        })
    }

    /// Serialize active accounting admission with deletion of its origin.
    /// A retired origin is handled without running the callback. Callers must
    /// release this boundary before attempting a late-usage append, which
    /// independently checks retirement under the same stable lock.
    pub(crate) fn with_live_session_origin(
        &self,
        session_id: &str,
        accept: impl FnOnce() -> bool,
    ) -> io::Result<Option<bool>> {
        self.with_session_write_admission(session_id, || Ok(accept()))
    }

    fn retired_session_write_error() -> io::Error {
        io::Error::new(io::ErrorKind::NotFound, "session was deleted")
    }

    fn persist_late_usage_record(
        &self,
        session_id: &str,
        incoming: LateUsageRecord,
    ) -> io::Result<bool> {
        let (path, lock_path) = self.ensure_late_usage_paths(session_id)?;
        let lock_file = open_private_lock_file(&lock_path)?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock.write()?;
        if Self::late_usage_is_deleted(&path)? {
            // Handled, rather than a failed sink that should queue a retry.
            return Ok(true);
        }
        let mut ledger = Self::load_late_usage_unlocked(&path)?;
        let original = ledger.clone();
        let source_fingerprint = incoming.source_fingerprint.clone();
        if let Some(record) = ledger
            .records
            .iter_mut()
            .find(|record| record.source_fingerprint == source_fingerprint)
        {
            if record.turn_fingerprint != incoming.turn_fingerprint
                || record.route != incoming.route
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "late usage source changed its captured origin or route",
                ));
            }
            let mut changed = false;
            if record
                .usage
                .as_ref()
                .is_none_or(|usage| *usage == codewhale_models::Usage::default())
                && let Some(usage) = incoming.usage.as_ref()
            {
                record.usage = Some(usage.clone());
                changed = true;
            }
            if let Some(decision) = incoming.decision.as_ref() {
                if !decision.is_bounded() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "decision receipt exceeds its bound",
                    ));
                }
                record.decision = Some(decision.sanitized());
                changed = true;
            }
            if changed {
                Self::write_late_usage_candidate(&path, &original, &ledger)?;
            }
            return Ok(true);
        }
        let known_usage = incoming.usage.as_ref();
        if known_usage.is_none()
            && ledger
                .records
                .iter()
                .filter(|record| {
                    record
                        .usage
                        .as_ref()
                        .is_none_or(|usage| *usage == codewhale_models::Usage::default())
                })
                .count()
                == MAX_LATE_USAGE_UNRESOLVED_RECORDS_PER_SESSION
        {
            if !ledger.overflowed {
                ledger.overflowed = true;
                Self::write_late_usage_ledger(&path, &ledger)?;
            }
            return Ok(true);
        }
        ledger.records.push(incoming);
        Self::write_late_usage_candidate(&path, &original, &ledger)?;
        Ok(true)
    }

    /// Preserve the last complete ledger on a failed known-receipt append.
    /// Its existing overflow flag records settlement incompleteness; the error
    /// is returned, never treated as handled or a reason to repeat a provider.
    fn write_late_usage_candidate(
        path: &Path,
        original: &LateUsageLedger,
        candidate: &LateUsageLedger,
    ) -> io::Result<()> {
        match Self::write_late_usage_ledger(path, candidate) {
            Ok(()) => Ok(()),
            Err(error) => {
                if !original.overflowed {
                    let mut retained = original.clone();
                    retained.overflowed = true;
                    if let Err(gap_error) = Self::write_late_usage_ledger(path, &retained) {
                        tracing::warn!(%gap_error, "late usage settlement gap could not be persisted");
                    }
                }
                Err(error)
            }
        }
    }

    pub(crate) fn persist_late_decision_receipt(
        &self,
        session_id: &str,
        turn_id: &str,
        receipt: &crate::cost_status::RuntimeDecisionReceipt,
    ) -> io::Result<bool> {
        if !receipt.is_bounded() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "decision receipt exceeds its bound",
            ));
        }
        self.persist_late_usage_record(
            session_id,
            LateUsageRecord::captured(
                turn_id,
                &receipt.source_id,
                &receipt.route,
                receipt.usage.as_ref().filter(|_| receipt.usage_complete),
                Some(receipt),
                crate::cost_status::RuntimeUsageMissingReason::SuccessWithoutUsage,
            ),
        )
    }

    /// Read the bounded provider decision evidence from the existing origin ledger.
    #[cfg(test)]
    pub(crate) fn decision_receipts_for_session(
        &self,
        session_id: &str,
    ) -> io::Result<Vec<crate::cost_status::RuntimeDecisionReceipt>> {
        Ok(self
            .load_late_usage(session_id)?
            .records
            .into_iter()
            .filter_map(|record| record.decision)
            .collect())
    }

    pub(crate) fn persist_late_runtime_usage(
        &self,
        session_id: &str,
        turn_id: &str,
        record: &crate::cost_status::RuntimeUsageRecord,
    ) -> io::Result<bool> {
        self.persist_late_usage_record(
            session_id,
            LateUsageRecord::captured(
                turn_id,
                &record.source_id,
                &record.usage.route,
                Some(&record.usage.usage),
                None,
                crate::cost_status::RuntimeUsageMissingReason::SuccessWithoutUsage,
            ),
        )
    }

    pub(crate) fn persist_late_runtime_drop(
        &self,
        session_id: &str,
        turn_id: &str,
        record: &crate::cost_status::RuntimeUsageDropRecord,
    ) -> io::Result<bool> {
        self.persist_late_usage_record(
            session_id,
            LateUsageRecord::captured(
                turn_id,
                &record.source_id,
                &record.route,
                None,
                None,
                record.reason,
            ),
        )
    }

    fn with_session_read_lock<T>(
        &self,
        session_id: &str,
        read: impl FnOnce(&Path) -> io::Result<T>,
    ) -> io::Result<T> {
        let (path, lock_path) = self.late_usage_paths(session_id)?;
        let lock_file = match open_private_read_file(&lock_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Atomic replacement makes a copied ledger readable without
                // creating a lock. No writer can have published a tombstone
                // without first creating the stable lock.
                return read(&path);
            }
            Err(error) => return Err(error),
        };
        let lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock.read()?;
        read(&path)
    }

    fn load_late_usage(&self, session_id: &str) -> io::Result<LateUsageLedger> {
        self.with_session_read_lock(session_id, |path| {
            if Self::late_usage_is_deleted(path)? {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "session accounting was deleted",
                ));
            }
            Self::load_late_usage_unlocked(path)
        })
    }

    fn apply_late_usage_to_metadata(&self, metadata: &mut SessionMetadata) {
        let ledger = match self.load_late_usage(&metadata.id) {
            Ok(ledger) => ledger,
            Err(_) => {
                // The transcript is independent of optional accounting data.
                // Keep a stable gap receipt even when this projection is later
                // saved; loading it again must not invent another missing call.
                let fingerprint = crate::cost_status::usage_source_fingerprint(&format!(
                    "late-usage-unavailable:{}",
                    crate::cost_status::usage_source_fingerprint(&metadata.id)
                ));
                if metadata.cost.usage_source_fingerprints.insert(fingerprint) {
                    metadata.cost.unpriced_turns = metadata.cost.unpriced_turns.saturating_add(1);
                    metadata.cost.cny_unpriced_turns =
                        metadata.cost.cny_unpriced_turns.saturating_add(1);
                }
                metadata
                    .cost
                    .unpriced_reasons
                    .insert(LATE_USAGE_UNAVAILABLE_REASON.to_string());
                metadata
                    .cost
                    .cny_unpriced_reasons
                    .insert(LATE_USAGE_UNAVAILABLE_REASON.to_string());
                metadata.cost.coverage_recorded = true;
                return;
            }
        };
        for mut record in ledger.records {
            if record.usage.as_ref() == Some(&codewhale_models::Usage::default()) {
                record.usage = None;
            }
            if let Some(receipt) = &record.decision
                && receipt.is_bounded()
            {
                metadata
                    .cost
                    .route_receipts
                    .insert(receipt.diagnostic_receipt());
            }
            let coverage =
                crate::cost_status::MissingUsageCoverage::for_route(&record.route, record.reason);
            let source_fingerprint = record.source_fingerprint.clone();
            let source_id = format!("late:{}", record.source_fingerprint);
            let mut pending = if let Some(usage) = record.usage.as_ref() {
                crate::cost_status::background_cost_for_runtime_usage(
                    &crate::cost_status::RuntimeUsageRecord {
                        source_id,
                        usage: crate::cost_status::EffectiveRouteUsage {
                            route: record.route,
                            usage: usage.clone(),
                        },
                    },
                )
            } else {
                crate::cost_status::background_cost_for_runtime_drop(
                    &crate::cost_status::RuntimeUsageDropRecord {
                        reason: record.reason,
                        source_id,
                        route: record.route,
                    },
                )
            };
            // The sidecar already stores the canonical SHA-256 identity. Do
            // not hash it again while projecting the receipt into the saved
            // session, or a concurrent main-snapshot writer that already
            // contains the response would not dedupe against this overlay.
            pending.usage_source_fingerprints.clear();
            pending
                .usage_source_fingerprints
                .insert(source_fingerprint.clone());
            let was_missing = metadata
                .cost
                .missing_usage_sources
                .contains_key(&source_fingerprint);
            if metadata
                .cost
                .usage_source_fingerprints
                .contains(&source_fingerprint)
                && (!was_missing || record.usage.is_none())
            {
                continue;
            }
            pending.missing_usage_sources.clear();
            pending.resolved_missing_usage_sources.clear();
            if record.usage.is_some() {
                pending
                    .resolved_missing_usage_sources
                    .insert(source_fingerprint.clone());
            } else {
                pending
                    .missing_usage_sources
                    .insert(source_fingerprint.clone(), coverage);
            }
            if let Some(usage) = record.usage {
                metadata.total_tokens = metadata
                    .total_tokens
                    .saturating_add(u64::from(usage.input_tokens))
                    .saturating_add(u64::from(usage.output_tokens));
            }
            metadata.cost.absorb_late_background_cost(&pending);
        }
        if ledger.overflowed {
            let fingerprint = crate::cost_status::usage_source_fingerprint(&format!(
                "late-usage-overflow:{}",
                crate::cost_status::usage_source_fingerprint(&metadata.id)
            ));
            if metadata.cost.usage_source_fingerprints.insert(fingerprint) {
                metadata.cost.unpriced_turns = metadata.cost.unpriced_turns.saturating_add(1);
                metadata.cost.cny_unpriced_turns =
                    metadata.cost.cny_unpriced_turns.saturating_add(1);
                metadata
                    .cost
                    .unpriced_reasons
                    .insert("late_usage_ledger_overflow".to_string());
                metadata
                    .cost
                    .cny_unpriced_reasons
                    .insert("late_usage_ledger_overflow".to_string());
                metadata.cost.coverage_recorded = true;
            }
        }
    }

    /// Persist the bounded goal control state for one saved session.
    /// `None` is the canonical clear operation and is idempotent.
    pub fn save_session_goal(
        &self,
        session_id: &str,
        goal: Option<&SessionGoalState>,
    ) -> std::io::Result<()> {
        let path = self.validated_session_goal_path(session_id)?;
        let Some(goal) = goal else {
            if self.checked_existing_session_goals_dir()?.is_some() && path.exists() {
                fs::remove_file(path)?;
            }
            return Ok(());
        };
        goal.validate()?;
        self.ensure_session_goals_dir()?;
        Self::checked_existing_session_goal_file(&path)?;
        let content = serde_json::to_string_pretty(goal)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        write_atomic(&path, content.as_bytes())
    }

    /// Load a saved session's durable goal, rejecting malformed or future
    /// records instead of silently starting a different objective.
    pub fn load_session_goal(&self, session_id: &str) -> std::io::Result<Option<SessionGoalState>> {
        let path = self.validated_session_goal_path(session_id)?;
        if self.checked_existing_session_goals_dir()?.is_none()
            || !Self::checked_existing_session_goal_file(&path)?
        {
            return Ok(None);
        }
        let file_len = fs::metadata(&path)?.len();
        if file_len > MAX_SESSION_GOAL_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Session goal {} is {file_len} bytes; maximum is {MAX_SESSION_GOAL_FILE_BYTES}",
                    path.display()
                ),
            ));
        }
        let raw = fs::read_to_string(path)?;
        let goal: SessionGoalState = serde_json::from_str(&raw)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        goal.validate()?;
        Ok(Some(goal))
    }

    fn hydrate_recovered_runtime_binding(&self, session: &mut SavedSession) -> std::io::Result<()> {
        // Compare under the session write lock. A stale process may neither
        // resurrect a missing binding nor replace a different recovered owner.
        // An adoptable empty store (#6207) counts as abandonable on either
        // side, exactly like a missing one: there is no durable work to lose
        // in either direction.
        if let Some(incoming) = session.metadata.runtime_store.as_ref()
            && let Ok(persisted) =
                Self::load_session_metadata(&self.validated_session_path(&session.metadata.id)?)
            && let Some(binding) = persisted.runtime_store
            && incoming != &binding
        {
            let incoming_abandonable = incoming.is_missing_session_store().unwrap_or(false)
                || incoming.is_adoptable_empty_store().unwrap_or(false);
            if incoming_abandonable && binding.validate_existing_store().is_ok() {
                session.metadata.runtime_store = Some(binding);
            } else {
                let persisted_abandonable = binding.is_missing_session_store().unwrap_or(false)
                    || binding.is_adoptable_empty_store().unwrap_or(false);
                if !persisted_abandonable {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Session Runtime ownership changed; reopen the session before saving",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Save a session to disk using atomic write (temp file + fsync + rename).
    ///
    /// Borrowing form: clones once so the ~150 existing `&session` call sites
    /// keep working. The debounced persistence path already owns its value and
    /// calls [`Self::save_session_owned`] instead (#6214 T3).
    pub fn save_session(&self, session: &SavedSession) -> std::io::Result<PathBuf> {
        self.save_session_owned(session.clone())
    }

    /// Save a session to disk, consuming it.
    pub(crate) fn save_session_owned(&self, session: SavedSession) -> std::io::Result<PathBuf> {
        self.save_session_inner(session, None)
    }

    /// The autosave path (#6842): like [`Self::save_session_owned`], but when
    /// the journal carries more than `JOURNAL_PRUNE_DEAD_THRESHOLD`
    /// off-branch entries, first append the excess to the session's journal
    /// archive (fsynced), then write the document without them. If the
    /// archive cannot be written nothing is pruned. Runs on the persistence
    /// actor (`tui/persistence_actor.rs` `flush_inner`), never the UI loop.
    pub(crate) fn save_session_bounded(&self, session: SavedSession) -> std::io::Result<PathBuf> {
        self.save_session_inner(
            session,
            Some((JOURNAL_PRUNE_DEAD_THRESHOLD, JOURNAL_RETAINED_DEAD_ENTRIES)),
        )
    }

    /// Bring a saved document that exceeds the full-history transport bound
    /// back under it by archiving every off-branch journal entry (kept in the
    /// journal archive sidecar, the same durable step the autosave prune
    /// uses). The active branch is never touched, so the conversation is
    /// unchanged. Known limit: a document whose active branch alone exceeds
    /// the bound stays refused.
    fn compact_oversized_session(&self, id: &str) -> io::Result<()> {
        let session = self.load_session_snapshot_with_limit(id, None)?;
        self.save_session_inner(session, Some((0, 0))).map(|_| ())
    }

    fn save_session_inner(
        &self,
        session: SavedSession,
        prune: Option<(usize, usize)>,
    ) -> std::io::Result<PathBuf> {
        let session_id = session.metadata.id.clone();
        let path = self.validated_session_path(&session_id)?;
        // Not a `move` closure: `session` is consumed inside, so inference
        // captures it by value while `path` and `session_id` stay borrowed for
        // the caller to use after the write.
        self.with_session_write_admission(&session_id, || {
            let already_persisted = path.exists()
                || self
                    .validated_checkpoint_path(&session_id)
                    .is_ok_and(|checkpoint| checkpoint.exists());

            // Still the pre-hydration value, and still before write_atomic.
            self.archive_before_first_graph_write(&session, &path)?;

            let mut durable_session = session;
            let archived = match prune {
                Some((threshold, budget)) => {
                    self.archive_dead_journal_entries(&mut durable_session, threshold, budget)
                }
                None => Vec::new(),
            };
            self.hydrate_recovered_runtime_binding(&mut durable_session)?;
            self.hydrate_approval_receipts(&mut durable_session)?;
            let content = serialize_saved_session(durable_session)?;

            // Atomic write via write_atomic (NamedTempFile + fsync + persist)
            write_atomic(&path, content.as_bytes())?;
            self.stamp_session_boot_owner_for_new_record(&session_id, already_persisted);
            if !archived.is_empty() {
                archived_journal_ids()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entry(session_id.clone())
                    .or_default()
                    .extend(archived);
            }
            Ok(())
        })?
        .ok_or_else(Self::retired_session_write_error)?;

        // Cleanup may delete sessions, so release this session's lifecycle
        // lock first instead of recursively acquiring it during cleanup.
        self.cleanup_old_sessions()?;

        Ok(path)
    }

    /// Save a crash-recovery checkpoint for in-flight turns.
    ///
    /// Checkpoints are keyed per session (`checkpoints/<session_id>.json`) so
    /// concurrent sessions never overwrite each other's crash-recovery state.
    pub fn save_checkpoint(&self, session: &SavedSession) -> std::io::Result<PathBuf> {
        self.save_checkpoint_owned(session.clone())
    }

    /// Save a crash-recovery checkpoint, consuming the session.
    pub(crate) fn save_checkpoint_owned(&self, session: SavedSession) -> std::io::Result<PathBuf> {
        let session_id = session.metadata.id.clone();
        let path = self.validated_checkpoint_path(&session_id)?;
        self.with_session_write_admission(&session_id, || {
            let session_path = self.validated_session_path(&session_id)?;
            self.archive_before_first_graph_write(&session, &session_path)?;
            fs::create_dir_all(self.checkpoints_dir())?;
            let already_persisted = path.exists() || session_path.exists();
            let mut durable_session = session;
            self.hydrate_recovered_runtime_binding(&mut durable_session)?;
            self.hydrate_approval_receipts(&mut durable_session)?;
            let content = serialize_saved_session(durable_session)?;
            write_atomic(&path, content.as_bytes())?;
            self.stamp_session_boot_owner_for_new_record(&session_id, already_persisted);
            Ok(())
        })?
        .ok_or_else(Self::retired_session_write_error)?;
        Ok(path)
    }

    fn session_boot_owners_path(&self) -> PathBuf {
        self.sessions_dir
            .join(format!("{SESSION_BOOT_OWNERS_STEM}.json"))
    }

    fn load_session_boot_owners(&self) -> BTreeMap<String, String> {
        fs::read_to_string(self.session_boot_owners_path())
            .ok()
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    /// Does any durable record (session file or crash checkpoint) exist for
    /// this session id?
    fn session_record_exists(&self, session_id: &str) -> bool {
        self.validated_session_path(session_id)
            .is_ok_and(|path| path.exists())
            || self
                .validated_checkpoint_path(session_id)
                .is_ok_and(|path| path.exists())
    }

    /// Record which session instance owns `session_id`'s persisted record.
    ///
    /// Entries whose durable record no longer exists are pruned on the same
    /// write, so the sidecar cannot grow without bound.
    pub(crate) fn record_session_boot_owner(
        &self,
        session_id: &str,
        boot_id: &str,
    ) -> std::io::Result<()> {
        let id = self.validated_session_id(session_id)?.to_string();
        self.with_boot_owners_lock(|| {
            let mut owners = self.load_session_boot_owners();
            owners.retain(|owned, _| owned == &id || self.session_record_exists(owned));
            owners.insert(id, boot_id.to_string());
            let content = serde_json::to_string_pretty(&owners)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            write_atomic(&self.session_boot_owners_path(), content.as_bytes())
        })
    }

    /// Serialize the sidecar's read-modify-write across processes. Two
    /// processes stamping at once each read the old map and the second rename
    /// dropped the first one's entry (#6144 P8).
    fn with_boot_owners_lock<T>(&self, update: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let lock_file = open_private_lock_file(
            &self
                .sessions_dir
                .join(format!("{SESSION_BOOT_OWNERS_STEM}.lock")),
        )?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock.write()?;
        update()
    }

    /// The session-instance boot id stamped on this session's persisted
    /// record, when one was recorded.
    #[must_use]
    pub fn session_boot_owner(&self, session_id: &str) -> Option<String> {
        let id = self.validated_session_id(session_id).ok()?;
        self.load_session_boot_owners().get(id).cloned()
    }

    /// Was this session's persisted record created by a different session
    /// instance (an earlier or sibling Codewhale process)?
    ///
    /// Mirrors `SubAgentManager::is_from_prior_session` (#405): a durable
    /// record with no stamped owner predates the marker and is classified as
    /// prior-instance work, while an id with no durable record at all is
    /// this instance's own not-yet-persisted session.
    #[must_use]
    pub fn session_from_prior_instance(&self, session_id: &str) -> bool {
        match self.session_boot_owner(session_id) {
            Some(owner) => owner != current_session_boot_id(),
            None => self.session_record_exists(session_id),
        }
    }

    /// Stamp this instance as creator when a save writes the first durable
    /// record for `session_id`. A record that already existed keeps its
    /// original owner: re-serializing another instance's work (crash
    /// recovery, external mutation) must not re-badge it as ours.
    fn stamp_session_boot_owner_for_new_record(&self, session_id: &str, already_persisted: bool) {
        if already_persisted || self.session_boot_owner(session_id).is_some() {
            return;
        }
        if let Err(error) = self.record_session_boot_owner(session_id, current_session_boot_id()) {
            tracing::warn!(session_id, %error, "could not stamp session boot owner");
        }
    }

    fn clear_session_boot_owner(&self, session_id: &str) {
        let Ok(id) = self.validated_session_id(session_id) else {
            return;
        };
        let _ = self.with_boot_owners_lock(|| {
            let mut owners = self.load_session_boot_owners();
            if owners.remove(id).is_none() {
                return Ok(());
            }
            if let Ok(content) = serde_json::to_string_pretty(&owners) {
                write_atomic(&self.session_boot_owners_path(), content.as_bytes())?;
            }
            Ok(())
        });
    }

    /// Move the journal's excess off-branch entries into the archive sidecar.
    /// Order is the data-loss guard: plan on the unchanged journal, append and
    /// fsync the planned entries, and only then remove them from the document
    /// about to be written. Any failure leaves the journal whole. Returns the
    /// removed ids.
    fn archive_dead_journal_entries(
        &self,
        session: &mut SavedSession,
        dead_threshold: usize,
        dead_budget: usize,
    ) -> Vec<String> {
        let session_id = session.metadata.id.clone();
        let Some(journal) = session.journal.as_mut() else {
            return Vec::new();
        };
        let plan = journal.prune_plan(dead_threshold, dead_budget);
        if plan.is_empty() {
            return Vec::new();
        }
        let ids: std::collections::HashSet<String> = plan.into_iter().collect();
        // Ids a previous save archived but the live journal still carries
        // are already durable; append only the rest.
        let already = archived_journal_ids()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .cloned()
            .unwrap_or_default();
        let fresh: Vec<&SessionEntry> = journal
            .entries
            .iter()
            .filter(|entry| ids.contains(&entry.id) && !already.contains(&entry.id))
            .collect();
        if let Err(error) = self.append_journal_archive(&session_id, &fresh) {
            tracing::warn!(
                %session_id,
                %error,
                "journal archive write failed; keeping the full journal"
            );
            return Vec::new();
        }
        match journal.remove_entries(&ids) {
            Ok(_) => {
                session.leaf_id = journal.leaf_id.clone();
                ids.into_iter().collect()
            }
            Err(error) => {
                tracing::warn!(%session_id, %error, "journal prune refused; keeping the full journal");
                Vec::new()
            }
        }
    }

    /// Append `entries` to the session's journal archive, one JSON object per
    /// line, and fsync before returning. The sidecar is owner-only and never
    /// followed through a link.
    fn append_journal_archive(
        &self,
        session_id: &str,
        entries: &[&SessionEntry],
    ) -> io::Result<()> {
        use std::io::{Seek as _, SeekFrom, Write as _};
        if entries.is_empty() {
            return Ok(());
        }
        let path = journal_archive_path(&self.sessions_dir, session_id)?;
        let dir = self.sessions_dir.join(JOURNAL_ARCHIVE_DIR);
        if let Ok(metadata) = fs::symlink_metadata(&dir)
            && crate::plugins::metadata_is_link_or_reparse(&metadata)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "journal archive directory is a link",
            ));
        }
        let created_dir = !dir.exists();
        fs::create_dir_all(&dir)?;
        let mut buf = Vec::new();
        for entry in entries {
            serde_json::to_writer(&mut buf, entry)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            buf.push(b'\n');
        }
        let created_file = !path.exists();
        let mut file = open_private_lock_file(&path)?;
        file.seek(SeekFrom::End(0))?;
        file.write_all(&buf)?;
        file.sync_all()?;
        #[cfg(unix)]
        if created_file || created_dir {
            // Make the new directory entries durable, not only the bytes.
            fs::File::open(&dir)?.sync_all()?;
            if created_dir {
                fs::File::open(&self.sessions_dir)?.sync_all()?;
            }
        }
        #[cfg(not(unix))]
        let _ = (created_file, created_dir);
        Ok(())
    }

    /// Entries archived out of `session_id`'s journal by bounded saves.
    pub(crate) fn load_journal_archive(&self, session_id: &str) -> io::Result<Vec<SessionEntry>> {
        load_journal_archive(&self.sessions_dir, self.validated_session_id(session_id)?)
    }

    /// Bring an archived entry, and every ancestor the journal no longer
    /// holds, back into `session`'s journal so `/branch` can select it.
    /// `Ok(false)` when `entry_id` is in neither the journal nor the archive.
    pub(crate) fn restore_archived_journal_chain(
        &self,
        session: &mut SavedSession,
        entry_id: &str,
    ) -> io::Result<bool> {
        session.ensure_journal();
        let session_id = session.metadata.id.clone();
        let journal = session.journal.as_mut().expect("journal ensured");
        if journal.contains(entry_id) {
            return Ok(true);
        }
        let archive = self.load_journal_archive(&session_id)?;
        let by_id: std::collections::HashMap<&str, &SessionEntry> = archive
            .iter()
            .map(|entry| (entry.id.as_str(), entry))
            .collect();
        let mut chain = Vec::new();
        let mut cursor = Some(entry_id);
        while let Some(id) = cursor {
            if journal.contains(id) {
                break;
            }
            let Some(entry) = by_id.get(id) else {
                if chain.is_empty() {
                    return Ok(false);
                }
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("archived entry {entry_id} is missing ancestor {id}"),
                ));
            };
            if chain.len() > by_id.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "journal archive contains a cycle",
                ));
            }
            chain.push((*entry).clone());
            cursor = entry.parent_id.as_deref();
        }
        let restored: Vec<String> = chain.iter().map(|entry| entry.id.clone()).collect();
        journal.entries.extend(chain.into_iter().rev());
        validate_saved_journal(journal)?;
        // The live app must not later drop what was just brought back.
        if let Some(pending) = archived_journal_ids()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&session_id)
        {
            for id in &restored {
                pending.remove(id);
            }
        }
        Ok(true)
    }

    /// Remove the session's journal archive with the session itself. A
    /// linked archive directory is not followed.
    fn remove_journal_archive(&self, id: &str) -> io::Result<()> {
        let dir = self.sessions_dir.join(JOURNAL_ARCHIVE_DIR);
        match fs::symlink_metadata(&dir) {
            Ok(metadata) if crate::plugins::metadata_is_link_or_reparse(&metadata) => {
                tracing::warn!(
                    path = %dir.display(),
                    "journal archive directory is a link; its copies were not removed"
                );
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        match fs::remove_file(journal_archive_path(&self.sessions_dir, id)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Preserve the exact pre-import session once, before the first graph-
    /// bearing session or checkpoint write can replace it.
    fn archive_before_first_graph_write(
        &self,
        session: &SavedSession,
        source: &Path,
    ) -> std::io::Result<()> {
        let writes_graph = session
            .work_state
            .as_ref()
            .and_then(|state| state.graph.as_ref())
            .is_some_and(|graph| !graph.is_empty());
        if !writes_graph || !source.exists() {
            return Ok(());
        }
        let bytes = fs::read(source)?;
        let already_graph_backed = serde_json::from_slice::<SavedSession>(&bytes)
            .ok()
            .and_then(|saved| saved.work_state)
            .and_then(|state| state.graph)
            .is_some_and(|graph| !graph.is_empty());
        if already_graph_backed {
            return Ok(());
        }
        let archive_dir = self.sessions_dir.join(WORK_GRAPH_IMPORT_ARCHIVE_DIR);
        fs::create_dir_all(&archive_dir)?;
        let archive =
            archive_dir.join(source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid session path")
            })?);
        if !archive.exists() {
            write_atomic(&archive, &bytes)?;
        }
        Ok(())
    }

    fn read_checkpoint_file(&self, path: &Path) -> std::io::Result<Option<SavedSession>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        let mut session: SavedSession = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if session.schema_version > CURRENT_SESSION_SCHEMA_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Checkpoint schema v{} is newer than supported v{}",
                    session.schema_version, CURRENT_SESSION_SCHEMA_VERSION
                ),
            ));
        }
        // A crash after retirement but before checkpoint removal must not
        // offer the deleted origin for recovery. Optional accounting damage
        // still permits recovery and is projected as incomplete below.
        if self
            .with_session_read_lock(&session.metadata.id, Self::late_usage_is_deleted)
            .unwrap_or(false)
        {
            return Ok(None);
        }
        session.system_prompt = strip_legacy_truncation_note(session.system_prompt);
        self.hydrate_approval_receipts(&mut session)?;
        self.apply_late_usage_to_metadata(&mut session.metadata);
        Ok(Some(session))
    }

    /// Load a specific session's crash-recovery checkpoint if present.
    pub fn load_session_checkpoint(
        &self,
        session_id: &str,
    ) -> std::io::Result<Option<SavedSession>> {
        let path = self.validated_checkpoint_path(session_id)?;
        self.read_checkpoint_file(&path)
    }

    /// Load the legacy single-slot checkpoint (`checkpoints/latest.json`) if
    /// present. Compatibility read only — this release no longer writes it.
    pub fn load_legacy_checkpoint(&self) -> std::io::Result<Option<SavedSession>> {
        let path = self.checkpoints_dir().join(LEGACY_CHECKPOINT_FILE);
        self.read_checkpoint_file(&path)
    }

    pub(crate) fn legacy_checkpoint_origin(&self) -> io::Result<Option<String>> {
        use std::io::Read as _;

        let path = self.checkpoints_dir().join(LEGACY_CHECKPOINT_FILE);
        let file = match open_private_read_file(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        // Lifecycle cleanup only needs the leading metadata. Never follow
        // links or read an unbounded legacy transcript to identify its owner.
        let mut prefix = Vec::new();
        file.take(1024 * 1024).read_to_end(&mut prefix)?;
        extract_top_level_metadata(&prefix)
            .map(|metadata| Some(metadata.id))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unknown legacy checkpoint origin",
                )
            })
    }

    /// Clear one session's crash-recovery checkpoint. Scoped: this can never
    /// remove another session's checkpoint file or the legacy slot.
    pub fn clear_session_checkpoint(&self, session_id: &str) -> std::io::Result<()> {
        let path = self.validated_checkpoint_path(session_id)?;
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Remove the legacy single-slot checkpoint file.
    pub fn clear_legacy_checkpoint(&self) -> std::io::Result<()> {
        let path = self.checkpoints_dir().join(LEGACY_CHECKPOINT_FILE);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }

    /// Enumerate all crash-recovery checkpoint files (per-session files plus
    /// the legacy single slot), sorted most recently modified first. Only
    /// file metadata is read here; callers load content per candidate.
    pub fn list_checkpoints(&self) -> std::io::Result<Vec<CheckpointRef>> {
        let dir = self.checkpoints_dir();
        let mut refs = Vec::new();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(refs),
            Err(err) => return Err(err),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let source = if name == LEGACY_CHECKPOINT_FILE {
                CheckpointSource::Legacy
            } else if is_offline_queue_file(name) {
                // Parked offline queues live in this directory but are not
                // crash-recovery checkpoints.
                continue;
            } else {
                let session_id = name.trim_end_matches(".json").to_string();
                if self.validated_checkpoint_path(&session_id).is_err() {
                    continue;
                }
                CheckpointSource::Session(session_id)
            };
            let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
                continue;
            };
            refs.push(CheckpointRef {
                source,
                path,
                modified,
            });
        }
        refs.sort_by_key(|r| std::cmp::Reverse(r.modified));
        Ok(refs)
    }

    /// Does `session_id` still hold a crash-recovery checkpoint — the
    /// durable sign the session ended mid-turn (#5715)?
    #[must_use]
    pub fn session_has_checkpoint(&self, session_id: &str) -> bool {
        self.validated_checkpoint_path(session_id)
            .is_ok_and(|path| path.exists())
    }

    /// The most recent workspace-scoped session that still holds a
    /// crash-recovery checkpoint — durable evidence a prior session in this
    /// workspace ended mid-turn (#5715). Metadata only; the transcript is
    /// never read. `exclude` is the live session's own id: its in-flight
    /// checkpoint is current work, not prior work, and engine respawns
    /// inside one session must not report the session's own checkpoint.
    /// Sessions this process instance created are likewise excluded.
    pub fn interrupted_workspace_session(
        &self,
        workspace: &Path,
        exclude: Option<&str>,
    ) -> Option<SessionMetadata> {
        // Newest-first already; a checkpoint file only survives a session
        // that never reached a settled save.
        for checkpoint in self.list_checkpoints().ok()? {
            let CheckpointSource::Session(id) = checkpoint.source else {
                continue;
            };
            // A checkpoint another running session is still refreshing is
            // that session's in-flight work, not a prior crash.
            if Some(id.as_str()) == exclude
                || !self.session_from_prior_instance(&id)
                || self.is_session_live_anywhere(&id)
            {
                continue;
            }
            // One malformed id or unreadable record must not hide a later
            // valid checkpoint — skip and keep scanning.
            let Ok(path) = self.validated_session_path(&id) else {
                continue;
            };
            if let Ok(meta) = Self::load_session_metadata(&path)
                && workspace_scope_matches(&meta.workspace, workspace)
            {
                return Some(meta);
            }
        }
        None
    }

    /// Migrate a session recovered from the legacy single-slot checkpoint to
    /// a per-session checkpoint file. Never overwrites an existing
    /// per-session file and leaves the legacy file in place (older binaries
    /// still read it; the legacy writer is already gone). Returns whether a
    /// file was written.
    pub fn write_session_checkpoint_if_absent(
        &self,
        session: &SavedSession,
    ) -> std::io::Result<bool> {
        let path = self.validated_checkpoint_path(&session.metadata.id)?;
        if path.exists() {
            return Ok(false);
        }
        self.save_checkpoint(session)?;
        Ok(true)
    }

    /// Acquire before loading or editing a queue, including on in-process
    /// resume. A per-write lock is insufficient: the second editor's stale
    /// snapshot would overwrite the first as soon as its write completed.
    pub fn acquire_offline_queue_lease(
        &self,
        session_id: &str,
    ) -> io::Result<std::sync::Arc<OfflineQueueLease>> {
        let session_id = self.validated_session_id(session_id)?.to_string();
        let directory = self.checkpoints_dir();
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{session_id}.offline_queue.lock"));
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let mut lock = fd_lock::RwLock::new(file);
        let guard = lock.try_write().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("Cannot open session {session_id}: its queued input is already open in another window, or its previous writes are still finishing ({error})"),
            )
        })?;
        // fd-lock's guard borrows its owner. Retain the underlying descriptor
        // instead so this lease can travel with asynchronous writes. Forgetting
        // this non-owning guard keeps the OS lock held; the final Arc explicitly
        // unlocks in Drop. The OS also releases it when the process crashes.
        std::mem::forget(guard);
        Ok(std::sync::Arc::new(OfflineQueueLease {
            session_id,
            _file: lock.into_inner(),
        }))
    }

    /// Park this session's offline queue (queued + draft messages).
    ///
    /// Queues are keyed per session (`checkpoints/<session_id>.offline_queue.json`)
    /// for exactly the reason checkpoints are: concurrent Codewhale instances
    /// must never overwrite — or delete — each other's unsent user text.
    ///
    /// A queue with no session id has no owner to restore it to, so parking is
    /// refused rather than written to a shared file where the next boot would
    /// destroy it.
    pub fn save_offline_queue_state(
        &self,
        state: &OfflineQueueState,
        session_id: Option<&str>,
    ) -> std::io::Result<PathBuf> {
        let session_id = session_id.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Offline queue cannot be parked without a session id",
            )
        })?;
        let path = self.validated_offline_queue_path(session_id)?;
        fs::create_dir_all(self.checkpoints_dir())?;
        let mut owned = state.clone();
        // The stamp is redundant with the file name; it stays because the UI's
        // restore path still compares it against the live session id.
        owned.session_id = Some(self.validated_session_id(session_id)?.to_string());
        let content = serde_json::to_string_pretty(&owned)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        write_atomic(&path, content.as_bytes())?;
        Ok(path)
    }

    /// Load one session's parked offline queue if present.
    pub fn load_offline_queue_state(
        &self,
        session_id: &str,
    ) -> std::io::Result<Option<OfflineQueueState>> {
        let path = self.validated_offline_queue_path(session_id)?;
        Ok(match Self::read_offline_queue_file(&path)? {
            Some(state) => Some(state),
            None => self.adopt_legacy_offline_queue(session_id, &path)?,
        })
    }

    /// Remove one named session's parked offline queue.
    pub fn clear_offline_queue_state_for(&self, session_id: &str) -> std::io::Result<()> {
        let path = self.validated_offline_queue_path(session_id)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn validated_offline_queue_path(&self, session_id: &str) -> std::io::Result<PathBuf> {
        let trimmed = self.validated_session_id(session_id)?;
        Ok(self
            .checkpoints_dir()
            .join(format!("{trimmed}{OFFLINE_QUEUE_SUFFIX}")))
    }

    fn read_offline_queue_file(path: &Path) -> std::io::Result<Option<OfflineQueueState>> {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let state: OfflineQueueState = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if state.schema_version > CURRENT_QUEUE_SCHEMA_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Offline queue schema v{} is newer than supported v{}",
                    state.schema_version, CURRENT_QUEUE_SCHEMA_VERSION
                ),
            ));
        }
        Ok(Some(state))
    }

    /// Migrate the pre-per-session global queue (`checkpoints/offline_queue.json`).
    ///
    /// It holds user-authored text, so it is adopted only by the session it was
    /// stamped for, and it is removed only once this session's copy is durably
    /// written. A queue stamped for someone else — or for nobody — is left
    /// exactly where it is, still readable, for its owner to claim.
    fn adopt_legacy_offline_queue(
        &self,
        session_id: &str,
        path: &Path,
    ) -> std::io::Result<Option<OfflineQueueState>> {
        let legacy = self.checkpoints_dir().join(OFFLINE_QUEUE_FILE);
        // A corrupt or future-schema legacy file must not fail this session's
        // boot: leave it on disk untouched and start with an empty queue.
        let Ok(Some(state)) = Self::read_offline_queue_file(&legacy) else {
            return Ok(None);
        };
        if state.session_id.as_deref() != Some(self.validated_session_id(session_id)?) {
            return Ok(None);
        }
        let content = serde_json::to_string_pretty(&state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        fs::create_dir_all(self.checkpoints_dir())?;
        write_atomic(path, content.as_bytes())?;
        match fs::remove_file(&legacy) {
            Ok(()) => {}
            // A second instance of the same session can win the adoption
            // race: both read the legacy file, both write this session's
            // per-session copy, and the twin's remove already retired the
            // legacy one. The queue is durably adopted either way, so a
            // vanished legacy file is success here, not a boot error.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(Some(state))
    }

    /// Read a session snapshot without repairing tool call/result pairs.
    ///
    /// This is the correct API for embedding hosts that inspect or update a
    /// durable session while an engine may still be executing a tool call.
    /// A dangling `tool_use` is not proof of a crashed process in that state.
    pub fn load_session_snapshot(&self, id: &str) -> std::io::Result<SavedSession> {
        self.load_session_snapshot_with_limit(id, None)
    }

    /// The same protected session parser with an explicit transport byte ceiling.
    /// The file read itself is capped before allocating or parsing its document.
    pub(crate) fn load_session_snapshot_bounded(
        &self,
        id: &str,
        limit: usize,
    ) -> io::Result<SavedSession> {
        self.load_session_snapshot_with_limit(id, Some(limit))
    }

    fn load_session_snapshot_with_limit(
        &self,
        id: &str,
        limit: Option<usize>,
    ) -> io::Result<SavedSession> {
        let path = self.validated_session_path(id)?;
        let content = match limit {
            None => fs::read_to_string(&path)?,
            Some(limit) => {
                use std::io::Read;
                let mut file = fs::File::open(&path)?;
                if file.metadata()?.len() > limit as u64 {
                    let before = file.metadata()?.len();
                    let compacted = self.compact_oversized_session(id).is_ok();
                    file = fs::File::open(&path)?;
                    let after = file.metadata()?.len();
                    if !compacted || after > limit as u64 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "session is {:.1} MiB and the full-history transport limit is {:.1} MiB; archiving its off-branch journal entries left it at {:.1} MiB, so its active conversation alone is over the limit; source retained",
                                before as f64 / 1_048_576.0,
                                limit as f64 / 1_048_576.0,
                                after as f64 / 1_048_576.0,
                            ),
                        ));
                    }
                }
                let mut bytes = Vec::new();
                file.take((limit as u64).saturating_add(1))
                    .read_to_end(&mut bytes)?;
                if bytes.len() > limit {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "session grew beyond full-history transport bound; source retained",
                    ));
                }
                String::from_utf8(bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            }
        };
        let mut session: SavedSession = serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if session.schema_version > CURRENT_SESSION_SCHEMA_VERSION {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Session schema v{} is newer than supported v{}",
                    session.schema_version, CURRENT_SESSION_SCHEMA_VERSION
                ),
            ));
        }

        // The file name is the identity callers asked for. A document that
        // names another session would attach that session's receipts, lease
        // and later saves to the wrong record.
        if session.metadata.id != id.trim() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Session file {} records a different session id",
                    path.display()
                ),
            ));
        }

        session.system_prompt = strip_legacy_truncation_note(session.system_prompt);
        if let Some(journal) = session.journal.as_ref() {
            validate_saved_journal(journal)?;
        }
        session.ensure_journal();
        self.hydrate_approval_receipts(&mut session)?;
        self.apply_late_usage_to_metadata(&mut session.metadata);

        Ok(session)
    }

    /// Load and repair a session after a known process or engine restart.
    ///
    /// The returned repair remains in memory until the caller persists
    /// `recovery.session`. Keeping persistence explicit lets embedding hosts
    /// serialize recovery with their own transcript mutation lock.
    pub fn recover_session_for_resume(&self, id: &str) -> std::io::Result<SessionRecovery> {
        let mut session = self.load_session_snapshot(id)?;
        let repair = repair_recovered_session(&mut session);

        Ok(SessionRecovery {
            session,
            changed: !repair.is_empty(),
            repaired_call_count: repair.repaired_call_ids.len(),
            duplicate_result_count: repair.duplicate_result_ids.len(),
            orphan_result_count: repair.orphan_result_ids.len(),
        })
    }

    /// Load, repair, and durably persist a session being resumed.
    ///
    /// Resume is where a crash-repaired history becomes durable: the repaired
    /// record replaces the interrupted one so the same repair does not re-run
    /// on every later load. A persist failure is logged and the repaired
    /// in-memory session is still returned — a failed write-back must not
    /// strand the resume.
    pub fn resume_session(&self, id: &str) -> std::io::Result<SessionRecovery> {
        let recovery = self.recover_session_for_resume(id)?;
        if recovery.changed
            && let Err(error) = self.save_session(&recovery.session)
        {
            tracing::warn!(
                session_id = %recovery.session.metadata.id,
                %error,
                "repaired session history could not be persisted; the repair will re-run on the next load"
            );
        }
        Ok(recovery)
    }

    /// [`Self::resume_session`] for a surface that will own and autosave the
    /// session: it first reserves the session's live lease, and refuses with
    /// `ResourceBusy` when another process has it open
    /// ([`Self::reserve_session_for_attach`]). The caller commits the
    /// returned lease once the session is applied.
    ///
    /// A session that was interrupted mid-turn is attached in its interrupted
    /// state: its crash checkpoint, when newer than the saved document, is
    /// promoted first. Attaching the older document instead dropped the
    /// in-flight turn, and the next autosave then cleared the only record of
    /// it while the turn's file edits stayed on disk.
    pub fn attach_session(&self, id: &str) -> std::io::Result<(SessionRecovery, SessionLease)> {
        let lease = self.reserve_session_for_attach(id)?;
        self.promote_interrupted_checkpoint(id);
        let recovery = self.resume_session(id)?;
        Ok((recovery, lease))
    }

    /// Persist `id`'s crash checkpoint as its saved document when the
    /// checkpoint is the newer of the two (or there is no document yet), then
    /// consume it. Callers hold the session's attach lease. A stale checkpoint
    /// never replaces a newer document; an unreadable document and a failed
    /// save leave both files as they were.
    fn promote_interrupted_checkpoint(&self, id: &str) {
        let Ok(Some(checkpoint)) = self.load_session_checkpoint(id) else {
            return;
        };
        match self.load_session(id) {
            Ok(saved) if saved.metadata.updated_at >= checkpoint.metadata.updated_at => {}
            Ok(_) => {
                if self.save_session(&checkpoint).is_err() {
                    return;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.save_session(&checkpoint).is_err() {
                    return;
                }
            }
            // A document that exists but cannot be read is not ours to
            // replace here; the attach reports it and the checkpoint stays.
            Err(_) => return,
        }
        let _ = self.clear_session_checkpoint(id);
    }

    /// [`Self::attach_session`] with a partial-ID prefix.
    pub fn attach_session_by_prefix(
        &self,
        prefix: &str,
    ) -> std::io::Result<(SessionRecovery, SessionLease)> {
        self.attach_session(&self.resolve_session_id_prefix(prefix)?)
    }

    /// [`Self::attach_session`] for a session file `/load` has already read.
    /// Either way the surface becomes the writer of the id the file names, so
    /// a managed record and a foreign file take the same contract: the id's
    /// live lease is reserved first and refused while another window has it
    /// open. A managed record then resumes through the manager so its repair
    /// is persisted in place; a foreign file is not ours to rewrite, so its
    /// journal projection and repair stay in memory.
    pub fn attach_session_file(
        &self,
        parsed: SavedSession,
        path: &Path,
    ) -> std::io::Result<(SavedSession, SessionLease)> {
        if self.owns_session_path(&parsed.metadata.id, path) {
            return self
                .attach_session(&parsed.metadata.id)
                .map(|(recovery, lease)| (recovery.session, lease));
        }
        let lease = self.reserve_session_for_attach(&parsed.metadata.id)?;
        let mut session = parsed;
        if let Some(journal) = session.journal.as_ref() {
            validate_saved_journal(journal)?;
        }
        session.ensure_journal();
        repair_recovered_session(&mut session);
        Ok((session, lease))
    }

    /// [`Self::resume_session`] with a partial-ID prefix.
    pub fn resume_session_by_prefix(&self, prefix: &str) -> std::io::Result<SessionRecovery> {
        self.resume_session(&self.resolve_session_id_prefix(prefix)?)
    }

    /// True when `path` is this store's durable record for `id`. File-based
    /// session loads use it to decide whether a repair may be written back in
    /// place or must stay in memory (a foreign file is not ours to rewrite).
    pub(crate) fn owns_session_path(&self, id: &str, path: &Path) -> bool {
        let Ok(managed) = self.validated_session_path(id) else {
            return false;
        };
        managed == path
            || managed
                .canonicalize()
                .is_ok_and(|managed| path.canonicalize().is_ok_and(|path| managed == path))
    }

    /// Load a session by ID for the standalone CodeWhale resume flow.
    ///
    /// This preserves the historical recovery behavior for existing callers.
    /// Embedding hosts performing ordinary runtime reads should use
    /// [`Self::load_session_snapshot`] instead.
    pub fn load_session(&self, id: &str) -> std::io::Result<SavedSession> {
        self.recover_session_for_resume(id)
            .map(|recovery| recovery.session)
    }

    /// Load a session by partial ID prefix
    pub fn load_session_by_prefix(&self, prefix: &str) -> std::io::Result<SavedSession> {
        self.load_session(&self.resolve_session_id_prefix(prefix)?)
    }

    /// Resolve a unique ID without applying resume-time repair to its record.
    pub(crate) fn resolve_session_id_prefix(&self, prefix: &str) -> std::io::Result<String> {
        let sessions = self.list_sessions()?;

        // One session listed more than once (a stray copy of its file under
        // another name) is still one session, not an ambiguous prefix.
        let mut matches: Vec<_> = sessions
            .into_iter()
            .filter(|s| s.id.starts_with(prefix))
            .collect();
        matches.sort_by(|a, b| a.id.cmp(&b.id));
        matches.dedup_by(|a, b| a.id == b.id);

        match matches.len() {
            0 => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("No session found with prefix: {prefix}"),
            )),
            1 => Ok(matches[0].id.clone()),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Ambiguous prefix '{}' matches {} sessions",
                    prefix,
                    matches.len()
                ),
            )),
        }
    }

    /// List all saved sessions, sorted by most recently updated
    pub fn list_sessions(&self) -> std::io::Result<Vec<SessionMetadata>> {
        Ok(self
            .list_session_records()?
            .into_iter()
            .map(|(_, mut metadata)| {
                self.apply_late_usage_to_metadata(&mut metadata);
                metadata
            })
            .collect())
    }

    /// [`Self::list_sessions`] without the late-usage cost overlay, keeping
    /// the file each record was read from.
    /// A record's file is not always `<id>.json`: early builds wrote
    /// `session_<timestamp>.json`, and retention must address those by path.
    fn list_session_records(&self) -> std::io::Result<Vec<(PathBuf, SessionMetadata)>> {
        let mut sessions = Vec::new();

        for entry in fs::read_dir(&self.sessions_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "json")
                && let Ok(session) = Self::load_session_metadata(&path)
            {
                sessions.push((path, session));
            }
        }

        // Sort by updated_at descending (most recent first)
        sessions.sort_by_key(|(_, s)| std::cmp::Reverse(s.updated_at));

        Ok(sessions)
    }

    /// Set the durable archive flag on a saved session and return the
    /// resulting metadata.
    ///
    /// This is the single writer for the flag: the picker, the `/sessions`
    /// command, and `PATCH /v1/sessions/{id}` all route through it so the TUI
    /// and the web dashboard cannot drift into two archive notions. A no-op
    /// call (already in the requested state) still returns the metadata and
    /// does not rewrite the file.
    pub fn set_session_archived(
        &self,
        id: &str,
        archived: bool,
        mutator: SessionMutator,
    ) -> std::io::Result<SessionMetadata> {
        let _lease = (mutator == SessionMutator::External)
            .then(|| self.reserve_session_for_external_write(id))
            .transpose()?;
        let mut session = self.load_session(id)?;
        if session.metadata.archived == archived {
            return Ok(session.metadata);
        }
        session.metadata.archived = archived;
        self.save_session(&session)?;
        Ok(session.metadata)
    }

    /// Re-read the durable lifecycle fields for `metadata` from disk.
    ///
    /// This is the autosave-survival guard. A TUI autosave rebuilds the whole
    /// session document from in-memory `App` state; any lifecycle field it
    /// carries from a stale cache would silently revert a rename or archive
    /// that landed in between — including one applied by the picker earlier in
    /// the same event loop, or by `/rename` while a snapshot was already
    /// queued.
    ///
    /// So rather than trusting any cache, the writer re-reads the persisted
    /// values immediately before writing. `title`, `archived`, `created_at`,
    /// and fork lineage are *lifecycle* state owned by the file, not
    /// conversation state owned by the running turn. Reading them back costs
    /// one bounded metadata-prefix read.
    ///
    /// Returns `true` when an existing record was found and merged. A missing
    /// record is not an error: the first save of a new session has nothing to
    /// merge from.
    pub fn merge_persisted_lifecycle(&self, metadata: &mut SessionMetadata) -> bool {
        let Ok(path) = self.validated_session_path(&metadata.id) else {
            return false;
        };
        let Ok(persisted) = Self::load_session_metadata(&path) else {
            return false;
        };
        metadata.title = persisted.title;
        metadata.archived = persisted.archived;
        metadata.created_at = persisted.created_at;
        metadata.parent_session_id = persisted.parent_session_id;
        metadata.forked_from_message_count = persisted.forked_from_message_count;
        metadata.runtime_store = persisted.runtime_store;
        true
    }

    /// Rename a saved session and return the resulting metadata.
    ///
    /// Titles are trimmed and bounded to [`MAX_SESSION_TITLE_CHARS`]
    /// characters (counted in `char`s, not bytes, so a CJK or emoji title is
    /// not truncated mid-scalar). Created-at and fork lineage are untouched.
    pub fn rename_session(
        &self,
        id: &str,
        title: &str,
        mutator: SessionMutator,
    ) -> std::io::Result<SessionMetadata> {
        let title = normalize_session_title(title)?;
        let _lease = (mutator == SessionMutator::External)
            .then(|| self.reserve_session_for_external_write(id))
            .transpose()?;
        let mut session = self.load_session(id)?;
        if session.metadata.title == title {
            return Ok(session.metadata);
        }
        session.metadata.title = title;
        self.save_session(&session)?;
        Ok(session.metadata)
    }

    /// Load only the metadata from a session file.
    ///
    /// Optimization for #337: previously this called
    /// `serde_json::from_reader` which forces serde to scan every token in
    /// the file just to validate JSON structure — including the
    /// (potentially many MB of) `messages` and `tool_log` arrays we're
    /// going to discard. For a user with hundreds of long sessions, a
    /// single `list_sessions()` call could chew through tens of MB of
    /// JSON per startup.
    ///
    /// We now read at most 64 KB up front and string-extract the
    /// top-level `metadata` object, which is invariably tiny (~500 B)
    /// and appears before any large `messages`/`tool_log` payload. We
    /// fall back to a full-file read only if the prefix doesn't yield a
    /// parseable metadata block (e.g. an oddly-formatted legacy file).
    pub(crate) fn load_session_metadata(path: &Path) -> std::io::Result<SessionMetadata> {
        use std::io::Read;

        const PREFIX_BYTES: usize = 64 * 1024;
        let mut file = fs::File::open(path)?;
        let mut buf = Vec::with_capacity(PREFIX_BYTES);
        file.by_ref()
            .take(PREFIX_BYTES as u64)
            .read_to_end(&mut buf)?;

        if let Some(mut metadata) = extract_top_level_metadata(&buf) {
            apply_legacy_title_recovery(&mut metadata, &buf);
            return Ok(metadata);
        }

        // Metadata wasn't extractable from the prefix: usually a `metadata`
        // block longer than 64 KB (a long-lived session's usage fingerprints,
        // #6842), rarely unusual key ordering. Grow the read geometrically
        // until the block closes, so the cost tracks the metadata's size
        // rather than the transcript's; only past `GROWN_PREFIX_LIMIT` (or
        // for a document that really lacks the block) read everything.
        const GROWN_PREFIX_LIMIT: usize = 16 * 1024 * 1024;
        let mut limit = PREFIX_BYTES;
        while buf.len() == limit && limit < GROWN_PREFIX_LIMIT {
            let next = (limit * 2).min(GROWN_PREFIX_LIMIT);
            file.by_ref()
                .take((next - limit) as u64)
                .read_to_end(&mut buf)?;
            limit = next;
            if let Some(mut metadata) = extract_top_level_metadata(&buf) {
                apply_legacy_title_recovery(&mut metadata, &buf);
                return Ok(metadata);
            }
        }
        let mut rest = Vec::new();
        file.read_to_end(&mut rest)?;
        buf.extend_from_slice(&rest);
        let mut metadata = extract_top_level_metadata(&buf).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "session file missing parseable `metadata` block",
            )
        })?;
        apply_legacy_title_recovery(&mut metadata, &buf);
        Ok(metadata)
    }

    /// Whether `remove_session` has anything of `id`'s to act on: its
    /// document, a recovery checkpoint (its own, or the legacy slot it
    /// originated), or a deletion marker whose cleanup a retry finishes. The
    /// same test `remove_session` repeats under the session's lock, made
    /// before any lease or lock file is created for the id.
    fn session_may_have_records(&self, id: &str, path: &Path) -> io::Result<bool> {
        match fs::symlink_metadata(path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Ok(checkpoint) = self.validated_checkpoint_path(id)
            && checkpoint.try_exists()?
        {
            return Ok(true);
        }
        if matches!(self.legacy_checkpoint_origin(), Ok(Some(origin)) if origin == id.trim()) {
            return Ok(true);
        }
        let (late_path, _) = self.late_usage_paths(id)?;
        Self::late_usage_is_deleted(&late_path)
    }

    /// Delete a session and its recovery checkpoints, retiring its origin.
    pub fn delete_session(&self, id: &str) -> std::io::Result<()> {
        self.remove_session(id, SessionRemoval::Explicit)
    }

    /// Remove the pre-import copy of `session_path` that the first
    /// graph-bearing write kept (see `archive_before_first_graph_write`): it is
    /// the same transcript, so a deleted session must not survive in it.
    ///
    /// A linked archive directory is not followed, as the late-usage store
    /// refuses one: deletion must not reach a same-named file elsewhere.
    fn remove_import_archive_copy(&self, session_path: &Path) -> io::Result<()> {
        let Some(name) = session_path.file_name() else {
            return Ok(());
        };
        let dir = self.sessions_dir.join(WORK_GRAPH_IMPORT_ARCHIVE_DIR);
        match fs::symlink_metadata(&dir) {
            Ok(metadata) if crate::plugins::metadata_is_link_or_reparse(&metadata) => {
                tracing::warn!(
                    path = %dir.display(),
                    "work-graph import archive is a link; its copies were not removed"
                );
                return Ok(());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        match fs::remove_file(dir.join(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn remove_session(&self, id: &str, removal: SessionRemoval) -> std::io::Result<()> {
        let path = self.validated_session_path(id)?;
        // Reserving the lease creates `.late-usage/<id>.live`; an id with
        // nothing to remove must not leave one behind. The authoritative
        // check below repeats this under the session's lock.
        if !self.session_may_have_records(id, &path)? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Session '{}' not found", id.trim()),
            ));
        }
        let _lease = self.reserve_session_for_external_write(id)?;
        // Older ordinary snapshots may use a name reserved by the checkpoint
        // directory. Such a name must never address its shared legacy files.
        let checkpoint = self.validated_checkpoint_path(id).ok();
        let legacy_checkpoint = self.checkpoints_dir().join(LEGACY_CHECKPOINT_FILE);
        let (late_path, lock_path) = self.ensure_late_usage_paths(id)?;
        let lock_file = open_private_lock_file(&lock_path)?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock.write()?;
        let already_deleted = Self::late_usage_is_deleted(&late_path)?;
        let legacy_origin = self.legacy_checkpoint_origin();
        let owns_legacy_checkpoint =
            matches!(&legacy_origin, Ok(Some(origin)) if origin == id.trim());
        let has_recovery = match checkpoint.as_ref() {
            Some(path) => path.try_exists()?,
            None => false,
        } || owns_legacy_checkpoint;
        if !already_deleted {
            // An unknown id must not acquire a deletion marker. A prior
            // tombstone, however, lets a retry finish interrupted cleanup.
            match fs::symlink_metadata(&path) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound && has_recovery => {}
                Err(error) => return Err(error),
            }
        }
        if matches!(removal, SessionRemoval::Retention) && !already_deleted {
            // Retention never acts on an uncertain origin. An unreadable
            // legacy checkpoint may be this id's crash recovery, and a
            // damaged recovery leaves the ordinary snapshot as the only
            // readable copy of the conversation. Fail closed: keep every byte
            // and let the caller skip this record.
            if let Err(error) = &legacy_origin {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "retention left session '{}' untouched: the legacy checkpoint's origin is unreadable ({error})",
                        id.trim()
                    ),
                ));
            }
        }
        if matches!(removal, SessionRemoval::Retention) && has_recovery && !already_deleted {
            // Retention owns the ordinary snapshot, not crash recovery. Keep
            // the origin and its accounting/evidence writable for resume.
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            // The pre-import copy is an older ordinary snapshot, not recovery.
            return self.remove_import_archive_copy(&path);
        }
        // The store this document is bound to is named by its binding, not by
        // its id: a host store usually sits under another conversation's
        // directory. Read it before the document is gone (#6144 P2).
        let bound_store = Self::load_session_metadata(&path)
            .ok()
            .and_then(|metadata| metadata.runtime_store);
        self.save_session_goal(id, None)?;
        // Publish the tombstone before removing data. A crash or a delayed
        // callback can no longer re-create this session's accounting. The
        // stable lock inode must never be removed or atomically replaced.
        if !already_deleted {
            write_atomic(&late_path.with_extension("deleted"), LATE_USAGE_DELETED)?;
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match fs::remove_file(&late_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Some(checkpoint) = checkpoint {
            match fs::remove_file(checkpoint) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        if owns_legacy_checkpoint {
            match fs::remove_file(&legacy_checkpoint) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.remove_import_archive_copy(&path)?;
        self.remove_journal_archive(id)?;
        self.clear_session_boot_owner(id);
        let session_dir = self.sessions_dir.join(id.trim());
        if session_dir.exists() {
            if crate::plugins::metadata_is_link_or_reparse(&fs::symlink_metadata(&session_dir)?) {
                // Preserve remove_dir_all's existing no-follow behavior.
                fs::remove_dir_all(session_dir)?;
                return Ok(());
            }
            // Other conversations and automations can share this host's Runtime
            // authority. Deleting a transcript must never delete that store —
            // including a `runtime-recovered-*` sibling, which another
            // document may be bound to.
            for entry in fs::read_dir(&session_dir)? {
                let entry = entry?;
                if is_runtime_store_dir_name(&entry.file_name()) {
                    continue;
                }
                if entry.file_type()?.is_dir() {
                    fs::remove_dir_all(entry.path())?;
                } else {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        self.retire_released_stores(id, bound_store);
        if session_dir.is_dir() && fs::read_dir(&session_dir)?.next().is_none() {
            fs::remove_dir(session_dir)?;
        }
        Ok(())
    }

    /// Set aside the stores a deleted document released — the one its
    /// binding names, and any left under its own directory — when nothing
    /// else binds them and they hold no work. Never unlinks; a store in use,
    /// holding work, or bound elsewhere stays exactly where it is (#6144 P2).
    fn retire_released_stores(
        &self,
        id: &str,
        bound_store: Option<crate::runtime_threads::RuntimeStoreBinding>,
    ) {
        let mut candidates: Vec<PathBuf> = bound_store
            .map(|binding| binding.data_dir)
            .into_iter()
            .collect();
        if let Ok(entries) = fs::read_dir(self.sessions_dir.join(id.trim())) {
            candidates.extend(
                entries
                    .flatten()
                    .filter(|entry| is_runtime_store_dir_name(&entry.file_name()))
                    .map(|entry| entry.path()),
            );
        }
        for store in candidates {
            crate::session_reconcile::retire_unbound_store(self, &store, "session deleted");
        }
    }

    /// Clean up old sessions to stay within the active cap.
    pub fn cleanup_old_sessions(&self) -> std::io::Result<()> {
        self.cleanup_old_sessions_keeping(None)
    }

    /// As [`Self::cleanup_old_sessions`], but never touches `keep` — the
    /// session being resumed at boot. Without this, a background cleanup that
    /// races session restore can retire the just-resumed session when 50+
    /// newer records exist (its `updated_at` is not bumped until first save).
    ///
    /// The cap counts *active* transcripts: archived records sit outside it
    /// until the user prunes them, and empty auto-created stubs get their own
    /// small cap so they can never push a real transcript out (#6136, #6137).
    /// A transcript past the cap is archived, never unlinked; the store's
    /// destructive paths stay the explicit user actions.
    pub fn cleanup_old_sessions_keeping(&self, keep: Option<&str>) -> std::io::Result<()> {
        // Archiving saves the record, and every save runs retention again.
        // Drain a backlog in one pass here instead of nesting one cleanup per
        // archived transcript.
        if self
            .retention_in_progress
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        let result = self.cleanup_old_sessions_inner(keep);
        self.retention_in_progress
            .store(false, std::sync::atomic::Ordering::SeqCst);
        result
    }

    fn cleanup_old_sessions_inner(&self, keep: Option<&str>) -> std::io::Result<()> {
        let records = self.list_session_records()?;

        // What retention owes each class (#6136/#6137): archived records are
        // already outside the cap; empty auto-created stubs are junk the
        // product writes on every boot and are capped apart so they can never
        // occupy a transcript's slot; everything else carries the
        // MAX_SESSIONS window.
        let mut active: Vec<&SessionMetadata> = Vec::new();
        let mut stubs: Vec<(&Path, &SessionMetadata)> = Vec::new();
        for (path, session) in &records {
            if session.archived {
                continue;
            }
            if is_empty_auto_created_session(session) {
                stubs.push((path, session));
            } else {
                active.push(session);
            }
        }

        for session in active.iter().skip(MAX_SESSIONS) {
            if keep.is_some_and(|id| id == session.id) {
                continue;
            }
            // `External` keeps the live-session guard honest: a record
            // another process is driving is not ours to retire.
            if let Err(err) = self.set_session_archived(&session.id, true, SessionMutator::External)
            {
                tracing::warn!(
                    target: "session",
                    session = session.id,
                    ?err,
                    "retention could not archive a transcript past the cap; it stays active"
                );
            }
        }

        for (listed_path, session) in stubs.iter().skip(MAX_EMPTY_SESSION_STUBS) {
            if keep.is_some_and(|id| id == session.id) {
                continue;
            }
            if let Err(err) = self.retire_empty_stub(listed_path, &session.id) {
                tracing::warn!(
                    target: "session",
                    session = session.id,
                    ?err,
                    "retention could not remove an empty session stub"
                );
            }
        }

        // A directory without a top-level session snapshot is not proof of an
        // orphan: runtime threads and automations own independent durable stores,
        // including in other processes and before their first snapshot. Retention
        // only retires records it listed above; never infer authority to delete
        // other directories from an absent transcript or process-local claim.

        Ok(())
    }

    /// Retire one empty auto-created stub that retention listed at
    /// `listed_path`.
    ///
    /// Early builds wrote records as `session_<timestamp>.json`, so the file
    /// retention listed is not always `<id>.json`. Removing such a stub by id
    /// could never find it: it was listed again, failed with `NotFound`, and
    /// warned on every launch forever. That record has no id-addressed
    /// accounting, checkpoint, or directory (nothing could reach it by id), so
    /// retiring it is removing exactly the file that was listed. A stub that
    /// is already gone when removal runs (another process's retention got
    /// there first) is already removed, not an error.
    fn retire_empty_stub(&self, listed_path: &Path, id: &str) -> std::io::Result<()> {
        let canonical = self.validated_session_path(id)?;
        let result = if listed_path == canonical {
            self.remove_session(id, SessionRemoval::Retention)
        } else {
            let _lease = self.reserve_session_for_external_write(id)?;
            fs::remove_file(listed_path)
        };
        match result {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// Remove session files whose `updated_at` is older than `max_age`
    /// from the persisted-sessions directory. Returns the number of
    /// records pruned. Building block for #406's phase-2 auto-archive
    /// on boot; today the user-facing entry point is the
    /// `/sessions prune <days>` slash command.
    ///
    /// Crash-recovery safety: skips the per-session checkpoint files
    /// (`checkpoints/<session_id>.json`), the legacy single-slot
    /// checkpoint (`checkpoints/latest.json`), and any file under `checkpoints/`
    /// — those are owned by the checkpoint subsystem and live with
    /// stricter durability rules. Only top-level `<session_id>.json`
    /// files are candidates.
    ///
    /// `max_age` is checked against the metadata's `updated_at`
    /// timestamp embedded in the JSON, not the filesystem mtime — the
    /// user may have rsynced their `~/.deepseek` between machines and
    /// fs mtimes can lie.
    #[cfg_attr(not(test), expect(dead_code))]
    pub fn prune_sessions_older_than(
        &self,
        max_age: std::time::Duration,
    ) -> std::io::Result<usize> {
        self.prune_sessions_older_than_keeping(max_age, None)
    }

    /// As [`Self::prune_sessions_older_than`], but never deletes `keep` — the
    /// active session. A just-resumed session's `updated_at` is stale until
    /// its first post-resume save, so an age prune could otherwise delete the
    /// live session out from under the TUI.
    pub fn prune_sessions_older_than_keeping(
        &self,
        max_age: std::time::Duration,
        keep: Option<&str>,
    ) -> std::io::Result<usize> {
        // A max_age too large to represent (or to subtract from now) means
        // nothing can be old enough to prune — keep everything instead of
        // panicking on DateTime underflow or inventing a fallback horizon.
        let Some(cutoff) = chrono::Duration::from_std(max_age)
            .ok()
            .and_then(|age| Utc::now().checked_sub_signed(age))
        else {
            return Ok(0);
        };
        let sessions = self.list_sessions()?;
        let mut pruned = 0usize;
        for session in sessions {
            if keep.is_some_and(|id| id == session.id) {
                continue;
            }
            if session.updated_at < cutoff {
                if let Err(err) = self.remove_session(&session.id, SessionRemoval::Retention) {
                    tracing::warn!(
                        target: "session",
                        session = session.id,
                        ?err,
                        "session prune skipped a record",
                    );
                    continue;
                }
                pruned += 1;
            }
        }
        Ok(pruned)
    }

    /// Get the most recent session scoped to the current workspace.
    ///
    /// Archived sessions are skipped: archiving is the user saying "not this
    /// one", and `--continue` / auto-resume must honour that rather than
    /// dragging a put-away session back.
    pub fn get_latest_session_for_workspace(
        &self,
        workspace: &Path,
    ) -> std::io::Result<Option<SessionMetadata>> {
        let sessions = self.list_sessions()?;
        Ok(sessions.into_iter().find(|session| {
            !session.archived
                && workspace_scope_matches(&session.workspace, workspace)
                && !is_empty_auto_created_session(session)
        }))
    }

    /// Search sessions by title
    pub fn search_sessions(&self, query: &str) -> std::io::Result<Vec<SessionMetadata>> {
        let query_lower = query.to_lowercase();
        let sessions = self.list_sessions()?;

        Ok(sessions
            .into_iter()
            .filter(|s| s.title.to_lowercase().contains(&query_lower))
            .collect())
    }
}

/// A Runtime store directory inside a session directory: `runtime`, or a
/// `runtime-recovered-*` sibling opened for a conversation whose store was
/// missing.
pub(crate) fn is_runtime_store_dir_name(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| name == "runtime" || name.starts_with("runtime-recovered-"))
}

/// Unicode format characters that never belong in a session title: bidi
/// embeddings/overrides/isolates and marks, zero-width joiners/spaces, the
/// soft hyphen, BOM, and line/paragraph separators. Together with
/// `char::is_control` (C0, DEL, C1 — so ESC, BEL, ST, and OSC introducers)
/// this is the one character policy for the persisted title, the terminal
/// tab title, and every plain-text listing that echoes a title.
pub(crate) fn is_title_format_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

/// One-line notice that a prior session in `workspace` ended mid-turn
/// (#5715), for the session-pinned prompt prefix. `current_session_id` is
/// excluded: an in-flight checkpoint of the live session is current work,
/// not prior work. Returns `None` when no interrupted session exists.
pub(crate) fn session_recovery_hint(
    workspace: &Path,
    current_session_id: Option<&str>,
) -> Option<String> {
    let manager = SessionManager::default_location().ok()?;
    let meta = manager.interrupted_workspace_session(workspace, current_session_id)?;
    Some(format!(
        "A previous Codewhale session in this workspace (\"{}\", id {}, last active {}) has a recovery checkpoint — it likely ended mid-task. Use session_search/session_get to inspect it and offer to summarize or continue the work; resuming is the user's decision (e.g. /resume).",
        meta.title,
        truncate_id(&meta.id),
        meta.updated_at.format("%Y-%m-%d %H:%M UTC"),
    ))
}

/// Drop control and bidi/zero-width format characters from a title.
///
/// A session title is user- or content-derived text that later reaches an
/// OSC 0 terminal title, `codewhale sessions` stdout, and the picker, so the
/// persisted value must not be able to carry a raw escape sequence. Ordinary
/// text, punctuation, CJK, and emoji pass through untouched.
pub fn sanitize_session_title(raw: &str) -> String {
    raw.chars()
        .filter(|ch| !ch.is_control() && !is_title_format_char(*ch))
        .collect()
}

/// Sanitize, trim, and bound a user-supplied session title.
///
/// Returns `InvalidInput` for an empty title or one longer than
/// [`MAX_SESSION_TITLE_CHARS`] so every rename surface (picker, `/rename`,
/// `PATCH /v1/sessions/{id}`) rejects the same inputs with the same reason.
pub fn normalize_session_title(title: &str) -> std::io::Result<String> {
    let sanitized = sanitize_session_title(title);
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Session title cannot be empty",
        ));
    }
    if trimmed.chars().count() > MAX_SESSION_TITLE_CHARS {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Session title cannot exceed {MAX_SESSION_TITLE_CHARS} characters"),
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn workspace_scope_matches(saved_workspace: &Path, current_workspace: &Path) -> bool {
    if paths_equivalent(saved_workspace, current_workspace) {
        return true;
    }

    // Repository identity comes from the containing checkout itself (Git
    // dir/worktree traversal shared with project-context scope resolution),
    // never from branch names or paths mentioned in conversation.
    let canonical = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    match (
        find_git_root(&canonical(saved_workspace)),
        find_git_root(&canonical(current_workspace)),
    ) {
        (Some(saved_root), Some(current_root)) => paths_equivalent(&saved_root, &current_root),
        _ => false,
    }
}

pub(crate) fn is_empty_auto_created_session(session: &SessionMetadata) -> bool {
    session.message_count == 0
        && session
            .title
            .trim()
            .eq_ignore_ascii_case(DEFAULT_SESSION_TITLE)
}

pub(crate) fn paths_equivalent(lhs: &Path, rhs: &Path) -> bool {
    let lhs_canonical = fs::canonicalize(lhs).ok();
    let rhs_canonical = fs::canonicalize(rhs).ok();
    match (lhs_canonical, rhs_canonical) {
        (Some(lhs), Some(rhs)) => lhs == rhs,
        _ => lhs == rhs,
    }
}

/// Resolve the default session directory path.
///
/// v0.8.44: prefers `~/.codewhale/sessions`, falls back to
/// `~/.deepseek/sessions` for existing installs. Uses the write-path resolver
/// so the first access relocates any legacy `~/.deepseek/sessions` into
/// `~/.codewhale/sessions` when the primary directory is missing (#3240).
/// If an older build already created an empty primary sessions directory, copy
/// missing legacy entries into it without overwriting newer CodeWhale data.
pub fn default_sessions_dir() -> std::io::Result<PathBuf> {
    if let Some(dir) = unsealed_sessions_dir() {
        return Ok(dir);
    }
    let dir = codewhale_config::ensure_state_dir("sessions")
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::NotFound, e.to_string()))?;
    match merge_missing_legacy_session_entries(&dir) {
        Ok(0) => {}
        Ok(count) => {
            tracing::info!(
                target: "session::migration",
                "Copied {count} missing legacy session entries into {}",
                dir.display()
            );
        }
        Err(err) => {
            tracing::warn!(
                target: "session::migration",
                "Could not copy legacy sessions into {}: {err}",
                dir.display()
            );
        }
    }
    Ok(dir)
}

/// `App::new` lists recent sessions, so merely building a fixture reaches the
/// sessions directory — including the legacy relocation `ensure_state_dir`
/// performs, which would move a developer's real `~/.deepseek/sessions`.
/// An unsealed test gets a private directory (and no migration); a sealed one
/// follows its own environment.
#[cfg(test)]
fn unsealed_sessions_dir() -> Option<PathBuf> {
    crate::test_support::unsealed_state_dir("sessions")
}

#[cfg(not(test))]
fn unsealed_sessions_dir() -> Option<PathBuf> {
    None
}

fn merge_missing_legacy_session_entries(primary: &Path) -> io::Result<usize> {
    if codewhale_paths::codewhale_home_is_explicit() {
        return Ok(0);
    }

    let legacy = codewhale_config::legacy_deepseek_home()
        .map_err(|e| io::Error::new(io::ErrorKind::NotFound, e.to_string()))?
        .join("sessions");
    if !legacy.is_dir() || paths_equivalent(primary, &legacy) {
        return Ok(0);
    }

    copy_missing_dir_entries(&legacy, primary)
}

fn copy_missing_dir_entries(src: &Path, dst: &Path) -> io::Result<usize> {
    fs::create_dir_all(dst)?;
    let mut copied = 0;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let source = entry.path();
        let target = dst.join(entry.file_name());

        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if entry.file_name() == std::ffi::OsStr::new("checkpoints") || target.exists() {
                continue;
            }
            copied += copy_missing_dir_entries(&source, &target)?;
        } else if file_type.is_file() {
            copied += usize::from(copy_file_create_new(&source, &target)?);
        }
    }
    Ok(copied)
}

fn copy_file_create_new(src: &Path, dst: &Path) -> io::Result<bool> {
    let mut source = fs::File::open(src)?;
    let mut target = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
    {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => return Err(err),
    };
    if let Err(err) = io::copy(&mut source, &mut target) {
        let _ = fs::remove_file(dst);
        return Err(err);
    }
    Ok(true)
}

/// Prune snapshots older than `max_age` for `workspace`.
///
/// Always non-fatal. Returns silently — callers don't need the count
/// (the underlying repo logs at WARN if anything blew up).
pub fn prune_workspace_snapshots(workspace: &Path, max_age: std::time::Duration) {
    match crate::snapshot::prune_older_than(workspace, max_age) {
        Ok(0) => {}
        Ok(n) => {
            tracing::debug!(target: "snapshot", "boot prune removed {n} snapshot(s)");
        }
        Err(e) => {
            tracing::warn!(target: "snapshot", "boot prune failed: {e}");
        }
    }
}

/// Create a new `SavedSession` from conversation state
#[cfg(test)]
pub fn create_saved_session(
    messages: &[Message],
    model: &str,
    workspace: &Path,
    total_tokens: u64,
    system_prompt: Option<&SystemPrompt>,
) -> SavedSession {
    create_saved_session_with_mode(
        messages,
        model,
        workspace,
        total_tokens,
        system_prompt,
        None,
    )
}

/// Placeholder title used for a session that has no first user message yet.
/// `build_session_snapshot` (tui/ui/frame.rs) treats a title equal to this
/// constant as an auto-generated placeholder and lets the conversation-derived
/// title win once a user message exists. Keep this string stable on purpose.
pub(crate) const DEFAULT_SESSION_TITLE: &str = "New Session";

/// Create a new `SavedSession` from conversation state with optional mode label
pub fn create_saved_session_with_mode(
    messages: &[Message],
    model: &str,
    workspace: &Path,
    total_tokens: u64,
    system_prompt: Option<&SystemPrompt>,
    mode: Option<&str>,
) -> SavedSession {
    create_saved_session_with_id_and_mode(
        Uuid::new_v4().to_string(),
        messages,
        model,
        workspace,
        total_tokens,
        system_prompt,
        mode,
    )
}

/// Create a new `SavedSession` using a caller-owned session id.
pub fn create_saved_session_with_id_and_mode(
    id: String,
    messages: &[Message],
    model: &str,
    workspace: &Path,
    total_tokens: u64,
    system_prompt: Option<&SystemPrompt>,
    mode: Option<&str>,
) -> SavedSession {
    create_saved_session_with_id_mode_and_stamps(
        id,
        messages,
        &[],
        model,
        workspace,
        total_tokens,
        system_prompt,
        mode,
    )
}

/// Create a new `SavedSession` whose journal entries keep the time each
/// message actually landed. `message_stamps[i]` is the append time of
/// `messages[i]`; a missing stamp falls back to now. Callers without a
/// live stamp log pass `&[]` and get the historical save-time behavior.
pub fn create_saved_session_with_id_mode_and_stamps(
    id: String,
    messages: &[Message],
    message_stamps: &[DateTime<Utc>],
    model: &str,
    workspace: &Path,
    total_tokens: u64,
    system_prompt: Option<&SystemPrompt>,
    mode: Option<&str>,
) -> SavedSession {
    create_saved_session_inner(
        id,
        messages,
        SessionJournal::from_messages_stamped(messages.to_vec(), message_stamps, 0),
        model,
        workspace,
        total_tokens,
        system_prompt,
        mode,
        true,
    )
}

/// Create a snapshot whose `messages` projection is left empty (#6214 T3).
///
/// The journal still carries every message, and every serialization path
/// rehydrates the projection (`serialize_saved_session` runs
/// `make_storage_compatible`), so the on-disk bytes are identical to the
/// filled form. The persistence queue drops the projection anyway
/// (`compact_for_persistence_queue`), so building it first is one full
/// history copy per debounced flush for nothing. Callers that serialize a
/// journal-only snapshot directly must run `make_storage_compatible` first.
pub fn create_saved_session_journal_only(
    id: String,
    messages: &[Message],
    journal: SessionJournal,
    model: &str,
    workspace: &Path,
    total_tokens: u64,
    system_prompt: Option<&SystemPrompt>,
    mode: Option<&str>,
) -> SavedSession {
    create_saved_session_inner(
        id,
        messages,
        journal,
        model,
        workspace,
        total_tokens,
        system_prompt,
        mode,
        false,
    )
}

fn create_saved_session_inner(
    id: String,
    messages: &[Message],
    journal: SessionJournal,
    model: &str,
    workspace: &Path,
    total_tokens: u64,
    system_prompt: Option<&SystemPrompt>,
    mode: Option<&str>,
    fill_messages: bool,
) -> SavedSession {
    let now = Utc::now();

    // Generate title from the first real user message (runtime-owned control
    // traffic is skipped by `conversation_derived_title`). Fall back to the
    // placeholder when no user-authored prompt exists yet.
    let title =
        conversation_derived_title(messages).unwrap_or_else(|| DEFAULT_SESSION_TITLE.to_string());

    let leaf_id = journal.leaf_id.clone();
    SavedSession {
        schema_version: CURRENT_SESSION_SCHEMA_VERSION,
        metadata: SessionMetadata {
            id,
            title,
            created_at: now,
            updated_at: now,
            message_count: messages.len(),
            total_tokens,
            model: model.to_string(),
            model_provider: default_model_provider(),
            model_provider_id: None,
            workspace: workspace.to_path_buf(),
            mode: mode.map(str::to_string),
            cost: SessionCostSnapshot::default(),
            parent_session_id: None,
            forked_from_message_count: None,
            runtime_store: None,
            cumulative_turn_secs: 0,
            archived: false,
            spawn_depth: journal.spawn_depth,
        },
        messages: if fill_messages {
            messages.to_vec()
        } else {
            Vec::new()
        },
        journal: Some(journal),
        leaf_id,
        system_prompt: system_prompt_to_string(system_prompt),
        context_references: Vec::new(),
        artifacts: Vec::new(),
        approval_receipts: Vec::new(),
        work_state: None,
        window_title: None,
        last_auto_route: None,
        turn_outcomes: Vec::new(),
    }
}

/// Update an existing session with new messages
pub fn update_session(
    mut session: SavedSession,
    messages: &[Message],
    total_tokens: u64,
    system_prompt: Option<&SystemPrompt>,
) -> SavedSession {
    session.schema_version = CURRENT_SESSION_SCHEMA_VERSION;
    session.ensure_journal();
    let old_len = session.messages.len();
    let new_len = messages.len();
    if new_len >= old_len && messages[..old_len] == session.messages[..] {
        if let Some(journal) = session.journal.as_mut() {
            for msg in &messages[old_len..] {
                journal.append_message(msg.clone());
            }
            session.leaf_id = journal.leaf_id.clone();
        }
    } else if (new_len != old_len || messages != session.messages.as_slice())
        && let Some(journal) = session.journal.as_mut()
    {
        let common = messages
            .iter()
            .zip(session.messages.iter())
            .take_while(|(a, b)| a == b)
            .count();
        if common > 0 && common <= journal.entries.len() {
            let target_id = journal
                .root_to_leaf()
                .get(common - 1)
                .map(|entry| entry.id.clone());
            if let Some(target_id) = target_id {
                let _ = journal.branch_to(&target_id);
            } else {
                journal.leaf_id = None;
            }
        } else if common == 0 {
            journal.leaf_id = journal.entries.first().and_then(|e| e.parent_id.clone());
            if journal.leaf_id.is_none() && !journal.entries.is_empty() {
                journal.leaf_id = None;
            }
        }
        for msg in messages.iter().skip(common) {
            journal.append_message(msg.clone());
        }
        session.leaf_id = journal.leaf_id.clone();
    }
    session.messages.clear();
    session.messages.extend_from_slice(messages);
    session.metadata.updated_at = Utc::now();
    session.metadata.message_count = messages.len();
    session.metadata.total_tokens = total_tokens;
    session.system_prompt = system_prompt_to_string(system_prompt);
    session
}

/// Strip a stale `[Session note]` block that was written by the old
/// 500-message cap. Only removes notes that contain the specific
/// "older messages were dropped" phrase — ordinary user-added
/// `[Session note]` prompts are left untouched.
fn strip_legacy_truncation_note(system_prompt: Option<String>) -> Option<String> {
    let sp = system_prompt?;
    let Some(trimmed) = sp.strip_prefix("[Session note]\n") else {
        return Some(sp);
    };
    // Only strip if this is the known cap_messages note.
    if !trimmed.contains("older messages were dropped") {
        return Some(sp);
    }
    // The note block ends with "\n\n---\n\n" (7 chars) followed by the real prompt.
    trimmed
        .find("\n\n---\n\n")
        .map(|pos| trimmed[pos + 7..].to_string())
}

/// Byte offset of `key` (a quoted JSON key such as `"metadata"`) outside any
/// string literal. Brace/string-aware so a key name quoted inside an earlier
/// message body is never matched.
fn find_json_key(bytes: &[u8], key: &[u8]) -> Option<usize> {
    let mut idx = 0usize;
    let mut in_string = false;
    let mut escape = false;
    while idx < bytes.len() {
        let c = bytes[idx];
        if escape {
            escape = false;
        } else if c == b'\\' {
            escape = true;
        } else if c == b'"' {
            if !in_string && bytes[idx..].starts_with(key) {
                return Some(idx);
            }
            in_string = !in_string;
        }
        idx += 1;
    }
    None
}

/// Offset of the value opening with `open` that follows the key at
/// `key_offset`.
fn json_value_start(bytes: &[u8], key_offset: usize, key_len: usize, open: u8) -> Option<usize> {
    let mut idx = key_offset + key_len;
    while idx < bytes.len() && (bytes[idx] as char).is_whitespace() {
        idx += 1;
    }
    if idx >= bytes.len() || bytes[idx] != b':' {
        return None;
    }
    idx += 1;
    while idx < bytes.len() && (bytes[idx] as char).is_whitespace() {
        idx += 1;
    }
    (idx < bytes.len() && bytes[idx] == open).then_some(idx)
}

/// Exclusive end of the balanced `{...}` starting at `start`, or `None` when
/// the buffer is truncated before it closes.
fn json_object_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (offset, &c) in bytes[start..].iter().enumerate() {
        if escape {
            escape = false;
            continue;
        }
        match c {
            b'\\' => escape = true,
            b'"' => in_string = !in_string,
            b'{' if !in_string => depth += 1,
            b'}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The longest valid UTF-8 prefix of `buf`. A fixed-size read prefix can end
/// inside a multi-byte character (routine for CJK or emoji transcripts, which
/// serde writes unescaped); rejecting the whole buffer then forced a full-file
/// read of every such session on each listing.
///
/// Only a character cut at the very end is trimmed. An invalid byte anywhere
/// else means a damaged file, and an empty prefix sends it down the full read,
/// which reports it.
fn utf8_prefix(buf: &[u8]) -> &str {
    match std::str::from_utf8(buf) {
        Ok(s) => s,
        // `valid_up_to` marks a char boundary, so this slice is valid.
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&buf[..error.valid_up_to()]).unwrap_or_default()
        }
        Err(_) => "",
    }
}

/// String-scan a JSON byte buffer for the top-level `"metadata":{...}`
/// block and return it parsed. Returns `None` if no balanced metadata
/// object is present in the buffer.
///
/// Supports the optimisation in `SessionManager::load_session_metadata`
/// (#337). The scanner is brace-balanced and string-aware so a `{` or
/// `}` appearing inside a string literal doesn't perturb the depth
/// count.
fn extract_top_level_metadata(buf: &[u8]) -> Option<SessionMetadata> {
    let s = utf8_prefix(buf);
    let bytes = s.as_bytes();
    const KEY: &[u8] = b"\"metadata\"";
    let start = json_value_start(bytes, find_json_key(bytes, KEY)?, KEY.len(), b'{')?;
    let end = json_object_end(bytes, start)?;
    serde_json::from_str::<SessionMetadata>(&s[start..end]).ok()
}

/// Complete message objects from the front of the `messages` array, plus
/// whether the array was seen to end. A message the prefix cut in half is
/// simply absent; nothing is reconstructed.
fn extract_leading_messages(buf: &[u8], max: usize) -> (Vec<Message>, bool) {
    let s = utf8_prefix(buf);
    let bytes = s.as_bytes();
    const KEY: &[u8] = b"\"messages\"";
    let Some(key_offset) = find_json_key(bytes, KEY) else {
        return (Vec::new(), false);
    };
    let Some(array_start) = json_value_start(bytes, key_offset, KEY.len(), b'[') else {
        return (Vec::new(), false);
    };
    let mut cursor = array_start + 1;
    let mut out = Vec::new();
    loop {
        while cursor < bytes.len() && matches!(bytes[cursor], b' ' | b'\t' | b'\r' | b'\n' | b',') {
            cursor += 1;
        }
        if cursor < bytes.len() && bytes[cursor] == b']' {
            return (out, true);
        }
        if out.len() >= max || cursor >= bytes.len() || bytes[cursor] != b'{' {
            return (out, false);
        }
        let Some(end) = json_object_end(bytes, cursor) else {
            return (out, false);
        };
        let Ok(message) = serde_json::from_str::<Message>(&s[cursor..end]) else {
            return (out, false);
        };
        out.push(message);
        cursor = end;
    }
}

/// How many leading messages the legacy-title recovery will parse. The
/// enclosing read is already bounded to a 64 KB prefix (#337); this bounds
/// the parse inside it.
const LEGACY_TITLE_SCAN_MESSAGES: usize = 24;

/// Recover a title that a superseded derivation took from runtime control
/// traffic, using the session's own first real user prompt.
///
/// Provenance is proven, never guessed: the stored title has to be exactly
/// what the old rule produced — [`truncate_title`] of a message the current
/// classifier rejects as not a user turn. A renamed session, and a person who
/// literally typed an envelope as their first message, never match, so their
/// text is kept. Returns `None` when there is nothing proven to recover.
fn recovered_legacy_title(
    stored: &str,
    messages: &[Message],
    array_complete: bool,
) -> Option<String> {
    let stale = messages.iter().any(|message| {
        crate::runtime_handoff::classify_user_turn_prompt(message)
            == crate::runtime_handoff::UserTurnPromptKind::NotPrompt
            && message.content.iter().any(|block| match block {
                ContentBlock::Text { text, .. } => {
                    truncate_title(text, 50) == stored
                        || truncate_title(extract_user_prompt(text), 50) == stored
                }
                _ => false,
            })
    });
    if !stale {
        return None;
    }
    match conversation_derived_title(messages) {
        Some(title) => Some(title),
        // No user turn in what we read. Only claim the conversation has none
        // when the array actually ended inside the prefix; a truncated read
        // keeps the stored title rather than inventing a neutral one.
        None if array_complete => Some(DEFAULT_SESSION_TITLE.to_string()),
        None => None,
    }
}

/// Apply [`recovered_legacy_title`] to freshly loaded metadata. In memory
/// only — the session file is never rewritten, so the stored title (and any
/// rename) survives on disk.
fn apply_legacy_title_recovery(metadata: &mut SessionMetadata, buf: &[u8]) {
    // Cost gate, never the rename decision: an envelope title always opens
    // with `<`, so this keeps #337's bounded-parse win for ordinary titles.
    // Whether to rewrite is `recovered_legacy_title`'s proven provenance.
    if !metadata.title.starts_with('<') {
        return;
    }
    let (messages, complete) = extract_leading_messages(buf, LEGACY_TITLE_SCAN_MESSAGES);
    if messages.is_empty() {
        return;
    }
    if let Some(title) = recovered_legacy_title(&metadata.title, &messages, complete) {
        metadata.title = title;
    }
}

fn system_prompt_to_string(system_prompt: Option<&SystemPrompt>) -> Option<String> {
    match system_prompt {
        Some(SystemPrompt::Text(text)) => Some(text.clone()),
        Some(SystemPrompt::Blocks(blocks)) => Some(
            blocks
                .iter()
                .map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n\n---\n\n"),
        ),
        None => None,
    }
}

/// Truncate a session ID to 8 characters for compact display.
/// Returns a `&str` borrowing from the input — no allocation.
pub fn truncate_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Strip a leading `<turn_meta>...</turn_meta>` block from saved user text.
///
/// Older sessions can have turn metadata prefixed to the first user message.
/// The session picker and generated session titles should show the user's
/// prompt, not the cache/debug envelope.
pub(crate) fn extract_user_prompt(raw: &str) -> &str {
    let trimmed = raw.trim_start();
    let Some(after_open) = trimmed.strip_prefix("<turn_meta>") else {
        return trimmed;
    };
    if let Some(close_pos) = after_open.find("</turn_meta>") {
        return after_open[close_pos + "</turn_meta>".len()..].trim_start();
    }
    after_open.trim_start()
}

/// Clean a stored title for display, falling back to a neutral label.
pub(crate) fn extract_title(raw: &str) -> &str {
    let title = extract_user_prompt(raw);
    if title.is_empty() { "Session" } else { title }
}

/// Strip common inline thinking/reasoning XML sections from saved assistant
/// text before it is shown in session previews.
pub(crate) fn strip_thinking_tags(text: &str) -> String {
    if !text.contains("<think") && !text.contains("<thinking") && !text.contains("<reasoning") {
        return text.to_string();
    }

    let tags = ["think", "thinking", "reasoning"];
    let mut result = text.to_string();
    for tag in tags {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        while let Some(start) = result.find(&open) {
            let Some(end) = result[start..].find(&close) else {
                break;
            };
            let end_abs = start + end + close.len();
            result.replace_range(start..end_abs, "");
        }
    }
    result
}

/// Truncate a string to create a title (character-safe for UTF-8)
fn truncate_title(s: &str, max_len: usize) -> String {
    let s = s.trim();
    // Older sessions may carry a title saved before sanitization existed;
    // never echo raw controls into stdout or the picker. Take the first
    // line before sanitizing so a legacy multi-line title still shows only
    // its first line.
    let first_line = sanitize_session_title(s.lines().next().unwrap_or(s));
    let first_line = first_line.trim();

    let char_count = first_line.chars().count();
    if char_count <= max_len {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(max_len - 3).collect();
        format!("{truncated}...")
    }
}

/// Derive the auto-title from the first real user message of a conversation.
///
/// Returns `None` when no user-authored message exists to name the session
/// after (an empty transcript, or one holding only runtime-owned control
/// traffic); callers fall back to [`DEFAULT_SESSION_TITLE`].
///
/// Chat-template compatibility forces runtime-owned control traffic
/// (sub-agent handoffs, the Operate contract, restore checkpoints) through
/// `role = "user"`, but an internal envelope is not what the person typed.
/// Prompt eligibility comes from the existing user-turn classifier; the live
/// title fallback shares the same selection through `conversation_title_prompt`.
fn conversation_derived_title(messages: &[Message]) -> Option<String> {
    conversation_title_prompt(messages).map(|prompt| truncate_title(prompt, 50))
}

/// Select the first real user turn's text for persisted and live titles.
/// Keep an image-only turn as the first user boundary, and strip historical
/// leading turn metadata without introducing another provenance classifier.
pub(crate) fn conversation_title_prompt(messages: &[Message]) -> Option<&str> {
    messages
        .iter()
        .find(|message| {
            crate::runtime_handoff::classify_user_turn_prompt(message)
                != crate::runtime_handoff::UserTurnPromptKind::NotPrompt
        })
        .and_then(|m| {
            m.content.iter().find_map(|block| match block {
                ContentBlock::Text { text, .. } => {
                    let prompt = extract_user_prompt(text);
                    if prompt.is_empty() {
                        None
                    } else {
                        Some(prompt)
                    }
                }
                _ => None,
            })
        })
}

/// Format a session for display in a picker
pub fn format_session_line(meta: &SessionMetadata) -> String {
    let age = format_age(&meta.updated_at);
    let updated = format_session_updated_at(&meta.updated_at, &age);
    let truncated_title = truncate_title(extract_title(&meta.title), 40);
    let fork_label = if meta.parent_session_id.is_some() {
        " | fork"
    } else {
        ""
    };

    format!(
        "{} | {} | {} msgs{} | {}",
        truncate_id(&meta.id),
        truncated_title,
        meta.message_count,
        fork_label,
        updated
    )
}

pub(crate) fn format_session_updated_at(dt: &DateTime<Utc>, age: &str) -> String {
    format!("{} ({age})", dt.format("%Y-%m-%d %H:%M UTC"))
}

/// Format a datetime as relative age
fn format_age(dt: &DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(*dt);

    if duration.num_minutes() < 1 {
        "just now".to_string()
    } else if duration.num_hours() < 1 {
        format!("{}m ago", duration.num_minutes())
    } else if duration.num_days() < 1 {
        format!("{}h ago", duration.num_hours())
    } else if duration.num_weeks() < 1 {
        format!("{}d ago", duration.num_days())
    } else {
        format!("{}w ago", duration.num_weeks())
    }
}

// === Unit Tests ===

#[cfg(test)]
mod tests {
    include!("session_manager/test_cases_01.rs");

    include!("session_manager/test_cases_02.rs");

    include!("session_manager/test_cases_03.rs");

    include!("session_manager/test_cases_04.rs");
}

#[cfg(test)]
mod storage_compatible_tests {
    use super::*;

    fn user(text: &str) -> Message {
        Message {
            role: codewhale_models::Role::from("user"),
            content: vec![codewhale_models::ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    /// Journal-only snapshots (#6214 T3) skip building the `messages`
    /// projection, but a save still lands full history: serialization
    /// rehydrates the projection from the journal, so a reload is whole.
    #[test]
    fn journal_only_snapshot_saves_and_reloads_full_history() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let messages = vec![user("first"), user("answer")];
        let sparse = create_saved_session_journal_only(
            "roundtrip".to_string(),
            &messages,
            SessionJournal::from_messages(messages.clone(), 0),
            "test-model",
            tmp.path(),
            7,
            None,
            None,
        );
        assert!(
            sparse.messages.is_empty(),
            "journal-only snapshots carry no messages projection"
        );
        assert_eq!(
            sparse.journal.as_ref().expect("journal").to_messages(),
            messages,
            "the journal still carries every message"
        );
        let manager = SessionManager::new(tmp.path().join("sessions")).expect("manager");
        manager.save_session_owned(sparse).expect("save sparse");
        let reloaded = manager.load_session("roundtrip").expect("reload");
        assert_eq!(reloaded.messages, messages);
    }

    /// The no-op cases must stay no-ops, byte for byte.
    ///
    /// `make_storage_compatible` replaced a clone-and-return-`Option` helper
    /// (#6214 T3). Two of that helper's paths returned `None`, and the caller
    /// then serialized the *original* — so a `metadata.message_count` that
    /// disagrees with `messages.len()` survived untouched. Rewriting it in
    /// place would silently edit live data on every save, and nothing else in
    /// the suite catches that.
    #[test]
    fn make_storage_compatible_leaves_the_no_op_cases_byte_identical() {
        let workspace = std::env::temp_dir();
        let messages = vec![user("one"), user("two")];

        // 1. No journal at all (legacy, pre-journal files): untouched.
        let mut legacy = create_saved_session(&messages, "test-model", &workspace, 0, None);
        legacy.journal = None;
        legacy.metadata.message_count = 99; // deliberately disagrees
        let before = serde_json::to_string_pretty(&legacy).expect("legacy json");
        let mut after_session = legacy.clone();
        after_session.make_storage_compatible();
        assert_eq!(
            serde_json::to_string_pretty(&after_session).expect("legacy json"),
            before,
            "a session with no journal must serialize exactly as it arrived"
        );
        assert_eq!(after_session.metadata.message_count, 99);

        // 2. Journal present and `messages` already equals its active branch:
        //    still untouched, including the disagreeing count.
        let mut settled = create_saved_session(&messages, "test-model", &workspace, 0, None);
        assert!(settled.journal.is_some(), "fixture must carry a journal");
        settled.messages = settled
            .journal
            .as_ref()
            .expect("journal present")
            .to_messages();
        settled.metadata.message_count = 99;
        let before = serde_json::to_string_pretty(&settled).expect("settled json");
        let mut after_session = settled.clone();
        after_session.make_storage_compatible();
        assert_eq!(
            serde_json::to_string_pretty(&after_session).expect("settled json"),
            before,
            "an already-consistent session must not be rewritten"
        );
        assert_eq!(
            after_session.metadata.message_count, 99,
            "the early return must happen before message_count is recomputed"
        );
    }

    /// The queued (journal-only) path is what the debounced flush actually
    /// writes: `compact_for_persistence_queue` empties `messages` first.
    #[test]
    fn make_storage_compatible_rehydrates_a_compacted_queue_snapshot() {
        let workspace = std::env::temp_dir();
        let messages = vec![user("one"), user("two"), user("three")];
        let mut session = create_saved_session(&messages, "test-model", &workspace, 0, None);
        let expected = session
            .journal
            .as_ref()
            .expect("journal present")
            .to_messages();

        session.compact_for_persistence_queue();
        assert!(
            session.messages.is_empty(),
            "the queued snapshot is journal-only"
        );

        session.make_storage_compatible();
        assert_eq!(
            session.messages, expected,
            "the compat projection is rebuilt from the journal"
        );
        assert_eq!(session.metadata.message_count, expected.len());
    }
}

#[cfg(test)]
mod canonical_journal_admission_tests {
    use super::*;
    #[test]
    fn corrupted_saved_branch_refuses_read_and_write_without_truncating_source() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path().join("sessions")).unwrap();
        let message = Message {
            role: codewhale_models::Role::User,
            content: vec![ContentBlock::Text {
                text: "retained".into(),
                cache_control: None,
            }],
        };
        let mut session = create_saved_session_with_id_and_mode(
            "corrupt-graph".into(),
            &[message],
            "test-model",
            tmp.path(),
            0,
            None,
            None,
        );
        let mut empty_root = session.clone();
        empty_root.metadata.id = "empty-root-graph".into();
        let mut alternative = session.messages.clone();
        alternative[0].role = codewhale_models::Role::Assistant;
        let graph = empty_root.journal.as_mut().unwrap();
        graph.rebranch_active_messages(&alternative);
        let retained_entries = graph.entries.clone();
        assert_eq!(retained_entries.len(), 2);
        graph.rebranch_active_messages(&[]);
        empty_root.leaf_id = None;
        empty_root.messages.clear();
        empty_root.metadata.message_count = 0;
        manager.save_session(&empty_root).unwrap();
        let observed = manager
            .load_session_snapshot_bounded(
                "empty-root-graph",
                codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES,
            )
            .unwrap();
        assert!(observed.messages.is_empty());
        assert_eq!(observed.leaf_id, None);
        assert_eq!(observed.journal.as_ref().unwrap().leaf_id, None);
        assert_eq!(observed.journal.as_ref().unwrap().entries, retained_entries);
        let graph = session.journal.as_mut().unwrap();
        graph.entries[0].parent_id = Some(graph.entries[0].id.clone());
        let bytes = serde_json::to_vec(&session).unwrap();
        let path = manager.validated_session_path("corrupt-graph").unwrap();
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            manager
                .load_session_snapshot("corrupt-graph")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(manager.save_session(&session).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(
            SavedSession::import_foreign(
                session.export_container("fixture"),
                tmp.path().into(),
                "test-model".into()
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod journal_bound_tests {
    //! #6842: bounded autosaves archive superseded journal entries before
    //! dropping them from the session document.
    use super::*;
    use std::collections::HashSet;

    fn text(text: &str) -> Message {
        Message {
            role: codewhale_models::Role::User,
            content: vec![ContentBlock::Text {
                text: text.into(),
                cache_control: None,
            }],
        }
    }

    /// A session whose journal holds `rounds` superseded copies of a
    /// 10-message tail, as repeated compaction leaves it.
    fn compacted_session(id: &str, workspace: &Path, rounds: usize) -> SavedSession {
        let mut session = create_saved_session_with_id_and_mode(
            id.into(),
            &[text("start")],
            "test-model",
            workspace,
            0,
            None,
            None,
        );
        let journal = session.journal.as_mut().unwrap();
        for round in 0..rounds {
            let tail: Vec<Message> = (0..10)
                .map(|i| text(&format!("round {round} message {i}")))
                .collect();
            journal.rebranch_active_messages(&tail);
        }
        session.leaf_id = journal.leaf_id.clone();
        session.messages = journal.to_messages();
        session.metadata.message_count = session.messages.len();
        session
    }

    fn all_ids(entries: &[SessionEntry]) -> HashSet<String> {
        entries.iter().map(|e| e.id.clone()).collect()
    }

    #[test]
    fn bounded_save_archives_then_prunes_and_live_journal_follows() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path().join("sessions")).unwrap();
        let session = compacted_session("bounded-a", tmp.path(), 460);
        let original = session.journal.clone().unwrap();
        let active = original.to_messages();

        manager.save_session_bounded(session).unwrap();

        let saved = manager.load_session("bounded-a").unwrap();
        let journal = saved.journal.as_ref().unwrap();
        assert!(
            journal.len() <= 10 + JOURNAL_RETAINED_DEAD_ENTRIES,
            "{}",
            journal.len()
        );
        assert_eq!(journal.to_messages(), active);
        assert_eq!(saved.leaf_id, original.leaf_id);
        let archive = manager.load_journal_archive("bounded-a").unwrap();
        // Nothing lost: document + archive is exactly the original journal.
        let mut union = all_ids(&journal.entries);
        union.extend(all_ids(&archive));
        assert_eq!(union, all_ids(&original.entries));
        assert_eq!(archive.len() + journal.len(), original.len());

        // The live journal drops exactly what the save archived.
        let mut live = original.clone();
        let archived = take_archived_journal_ids("bounded-a");
        assert_eq!(archived, all_ids(&archive));
        live.remove_entries(&archived).unwrap();
        assert_eq!(all_ids(&live.entries), all_ids(&journal.entries));
        assert!(take_archived_journal_ids("bounded-a").is_empty());

        // A plain save never prunes.
        let small = compacted_session("bounded-plain", tmp.path(), 460);
        let len = small.journal.as_ref().unwrap().len();
        manager.save_session(&small).unwrap();
        let reloaded = manager.load_session("bounded-plain").unwrap();
        assert_eq!(reloaded.journal.unwrap().len(), len);
    }

    #[test]
    fn archive_failure_prunes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions.clone()).unwrap();
        // A regular file where the archive directory belongs.
        fs::write(sessions.join(JOURNAL_ARCHIVE_DIR), b"not a dir").unwrap();
        let session = compacted_session("bounded-b", tmp.path(), 460);
        let len = session.journal.as_ref().unwrap().len();
        manager.save_session_bounded(session).unwrap();
        let saved = manager.load_session("bounded-b").unwrap();
        assert_eq!(saved.journal.unwrap().len(), len);
        assert!(take_archived_journal_ids("bounded-b").is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn archive_written_then_document_write_fails_loses_nothing() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions.clone()).unwrap();
        let session = compacted_session("bounded-c", tmp.path(), 460);
        let original = session.journal.clone().unwrap();
        manager.save_session(&session).unwrap();
        fs::create_dir_all(sessions.join(JOURNAL_ARCHIVE_DIR)).unwrap();
        // The archive can be appended, but the document's temp file cannot
        // be created: the crash window between the two writes.
        fs::set_permissions(&sessions, fs::Permissions::from_mode(0o500)).unwrap();
        let failed = manager.save_session_bounded(session.clone());
        fs::set_permissions(&sessions, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(failed.is_err(), "document write must fail in this setup");
        let on_disk = manager.load_session("bounded-c").unwrap();
        assert_eq!(on_disk.journal.as_ref().unwrap(), &original);
        assert!(
            !manager
                .load_journal_archive("bounded-c")
                .unwrap()
                .is_empty()
        );
        assert!(take_archived_journal_ids("bounded-c").is_empty());

        // Retrying archives the same entries again; the reader dedupes and
        // the union is still the whole journal.
        manager.save_session_bounded(session).unwrap();
        let saved = manager.load_session("bounded-c").unwrap();
        let archive = manager.load_journal_archive("bounded-c").unwrap();
        let mut union = all_ids(&saved.journal.unwrap().entries);
        union.extend(all_ids(&archive));
        assert_eq!(union, all_ids(&original.entries));
        let _ = take_archived_journal_ids("bounded-c");
    }

    #[test]
    fn branch_to_an_archived_entry_restores_its_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path().join("sessions")).unwrap();
        let session = compacted_session("bounded-d", tmp.path(), 460);
        let original = session.journal.clone().unwrap();
        manager.save_session_bounded(session).unwrap();
        let archived = take_archived_journal_ids("bounded-d");
        // The deepest archived entry of the oldest round.
        let target = original
            .entries
            .iter()
            .filter(|e| archived.contains(&e.id))
            .find(|e| {
                e.kind
                    .as_message()
                    .is_some_and(|m| m.content == text("round 0 message 9").content)
            })
            .unwrap()
            .id
            .clone();

        let mut session = manager.load_session("bounded-d").unwrap();
        assert!(session.journal_branch_to(&target).is_err());
        assert!(
            manager
                .restore_archived_journal_chain(&mut session, &target)
                .unwrap()
        );
        session.journal_branch_to(&target).unwrap();
        manager.save_session(&session).unwrap();
        let reloaded = manager.load_session("bounded-d").unwrap();
        assert_eq!(reloaded.leaf_id.as_deref(), Some(target.as_str()));
        // Each round replaced the whole tail, so round 0 is its own root chain.
        let expected: Vec<Message> = (0..10)
            .map(|i| text(&format!("round 0 message {i}")))
            .collect();
        assert_eq!(reloaded.messages, expected);

        let mut missing = manager.load_session("bounded-d").unwrap();
        assert!(
            !manager
                .restore_archived_journal_chain(&mut missing, "no-such-entry")
                .unwrap()
        );
    }

    #[test]
    fn deleting_a_session_removes_its_journal_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let manager = SessionManager::new(sessions.clone()).unwrap();
        manager
            .save_session_bounded(compacted_session("bounded-e", tmp.path(), 460))
            .unwrap();
        let _ = take_archived_journal_ids("bounded-e");
        let path = journal_archive_path(&sessions, "bounded-e").unwrap();
        assert!(path.exists());
        manager.delete_session("bounded-e").unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn metadata_larger_than_the_prefix_is_read_without_the_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path().join("sessions")).unwrap();
        let mut session = compacted_session("bounded-f", tmp.path(), 10);
        for i in 0..9_000 {
            session.metadata.cost.usage_source_fingerprints.insert(
                crate::cost_status::usage_source_fingerprint(&format!("call-{i}")),
            );
        }
        let path = manager.save_session(&session).unwrap();
        // Replace the transcript with 1 MB of non-JSON: listing must still
        // find the >64 KB metadata block (the grown read stops once it
        // closes; this checks correctness, not how many bytes were read).
        let bytes = fs::read(&path).unwrap();
        let start = find_json_key(&bytes, b"\"metadata\"").unwrap();
        let open = json_value_start(&bytes, start, 10, b'{').unwrap();
        let end = json_object_end(&bytes, open).unwrap();
        assert!(
            end > 64 * 1024,
            "metadata must exceed the old prefix: {end}"
        );
        let mut damaged = bytes[..end].to_vec();
        damaged.extend(std::iter::repeat_n(b'x', 1 << 20));
        fs::write(&path, &damaged).unwrap();
        let metadata = SessionManager::load_session_metadata(&path).unwrap();
        assert_eq!(metadata.id, "bounded-f");
        assert_eq!(metadata.cost.usage_source_fingerprints.len(), 9_000);
    }
}

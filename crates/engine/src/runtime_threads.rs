//! Durable thread/turn/item runtime for the HTTP API and background tasks.
//!
//! Execution follows the configured provider route while exposing Codex-like
//! lifecycle semantics (threads, turns, items, interrupt/steer, and replayable
//! events).

// Background-task runtime — runs alongside the TUI. Raw stdio prints
// here would still land in the alt-screen on whichever terminal the
// foreground TUI happens to own. Route everything through `tracing::*`
// instead — see `runtime_log` for the rationale.
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, Notify, RwLock as AsyncRwLock, broadcast, mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::compaction::CompactionConfig;
use crate::core::engine::handle::SteerOutcome;
use crate::core::engine::{
    EngineConfig, EngineHandle, spawn_engine_with_authoritative_route_config,
};
use crate::core::events::{Event as EngineEvent, TurnOutcomeStatus};
use crate::core::ops::{Op, TurnSpec};
use crate::cost_status::{
    EffectiveRouteEnvelope, EffectiveRouteUsage, RouteBillingMode, RuntimeUsageDropRecord,
    RuntimeUsageRecord,
};
use crate::provider_catalog_live::ProviderLivePricingQuote;
use crate::route_budget::{
    auto_compact_default_for_route, known_route_limits, route_context_window_tokens,
};
use crate::route_runtime::{ResolvedRuntimeRoute, resolve_runtime_route_for_identity};
use crate::tools::plan::new_shared_plan_state;
use crate::tools::shell::{SharedShellManager, new_shared_shell_manager};
use crate::tools::subagent::{
    SharedSubAgentManager, SubAgentStatus, new_shared_runtime_subagent_manager,
};
use crate::tools::todo::new_shared_todo_list;
#[cfg(test)]
use codewhale_config::AppMode;
use codewhale_config::ProviderKind;
#[cfg(test)]
use codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL;
use codewhale_execpolicy::ApprovalMode;
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message, SystemPrompt, ToolCallKey, ToolCaller, Usage};
use codewhale_protocol::agent_mail::{
    AGENT_MAIL_EVENT_CANCELED, AGENT_MAIL_EVENT_DELIVERED, AGENT_MAIL_EVENT_DELIVERING,
    AGENT_MAIL_EVENT_DELIVERY_FAILED, AGENT_MAIL_EVENT_QUEUED, AGENT_MAIL_EVENT_READ,
    AGENT_MAIL_SCHEMA_VERSION, AgentMailAddress, AgentMailDeliveryMode, AgentMailEnvelope,
    AgentMailEventPayload, AgentMailFailureCode, AgentMailFailureReceipt, AgentMailMessageId,
    AgentMailSendRequest, AgentMailSendResponse, AgentMailStatus, MAX_AGENT_MAIL_DELIVERY_ATTEMPTS,
    MAX_AGENT_MAIL_SUMMARY_BYTES,
};
use codewhale_protocol::runtime::{
    DynamicToolCallContent, DynamicToolCallParams, DynamicToolCallResult, DynamicToolSpec,
    TurnEnvironmentParams,
};
use codewhale_runtime::config::{Config, MAX_SUBAGENTS, ProviderIdentity};
use codewhale_runtime::runtime_policy::RuntimePolicyProjection;
pub const RUNTIME_EVENT_REPLAY_BATCH_SIZE: usize = 256;
pub const MAX_RUNTIME_EVENT_REPLAY_TAIL: usize = 4096;
pub const MAX_RUNTIME_TURN_OPERATION_KEY_BYTES: usize = 128;
const EVENT_TRANSACTION_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_TRANSACTION_LOCK_POLL: Duration = Duration::from_millis(5);
const EVENT_TRANSACTION_LOCK_FILE: &str = "events.lock";
const RUNTIME_PROCESS_OWNER_LOCK_FILE: &str = "runtime-process.owner.lock";
const RUNTIME_PROCESS_OWNER_LOCK_HELD: &str = "This runtime is already active in another process. Close the other Codewhale session and try again, or set CODEWHALE_RUNTIME_DIR to a different directory.";
const AGENT_MAIL_OWNER_FILE: &str = "owner.json";
/// Every directory `RuntimeThreadStore::open` creates to hold work. Emptiness
/// across all of them is what lets a switch adopt an existing store (#6207);
/// `adoptable_empty_store_reports_nothing_to_abandon` pins this list against
/// `open` by asserting each directory is load-bearing on its own.
const RUNTIME_STORE_WORK_DIRS: [&str; 7] = [
    "threads",
    "turns",
    "items",
    "events",
    "goals",
    "agent-mail",
    "turn-operations",
];
const TURN_OPERATION_BINDING_SCHEMA_VERSION: u32 = 1;
pub const MAX_ROUTED_USAGE_RECORDS_PER_TURN: usize = 64;

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EventAppendTestFault {
    AfterFlush,
    AfterSync,
}

#[cfg(any(test, feature = "test-support"))]
static TEST_EVENT_APPEND_FAULTS: std::sync::Mutex<Vec<(String, EventAppendTestFault, usize)>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-support"))]
pub type EventAppendTestFaultRestore = (String, Option<(EventAppendTestFault, usize)>);

#[cfg(any(test, feature = "test-support"))]
pub fn set_test_event_append_fault(
    thread_id: &str,
    fault: EventAppendTestFault,
    remaining: usize,
) -> EventAppendTestFaultRestore {
    assert!(remaining > 0, "event append fault count must be positive");
    let mut pending = TEST_EVENT_APPEND_FAULTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = pending
        .iter()
        .position(|(target, _, _)| target == thread_id)
        .map(|index| {
            let (_, previous_fault, previous_remaining) = pending.remove(index);
            (previous_fault, previous_remaining)
        });
    pending.push((thread_id.to_string(), fault, remaining));
    (thread_id.to_string(), previous)
}

#[cfg(any(test, feature = "test-support"))]
pub fn restore_test_event_append_fault(restore: EventAppendTestFaultRestore) {
    let (thread_id, previous) = restore;
    let mut pending = TEST_EVENT_APPEND_FAULTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(index) = pending
        .iter()
        .position(|(target, _, _)| target == &thread_id)
    {
        pending.remove(index);
    }
    if let Some((fault, remaining)) = previous {
        pending.push((thread_id, fault, remaining));
    }
}

#[cfg(any(test, feature = "test-support"))]
fn take_test_event_append_fault(thread_id: &str, expected: EventAppendTestFault) -> bool {
    let mut pending = TEST_EVENT_APPEND_FAULTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(index) = pending
        .iter()
        .position(|(target, fault, _)| target == thread_id && *fault == expected)
    else {
        return false;
    };
    if pending[index].2 > 1 {
        pending[index].2 -= 1;
    } else {
        pending.remove(index);
    }
    true
}

fn validated_record_id<'a>(id: &'a str, label: &str) -> Result<&'a str> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        bail!("{label} cannot be empty");
    }
    if trimmed != id {
        bail!("{label} cannot contain leading or trailing whitespace");
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(trimmed)
}

fn sort_turn_items_by_start(items: &mut [TurnItemRecord]) {
    let fallback = Utc::now();
    items.sort_by(|a, b| {
        let left = a.started_at.unwrap_or(fallback);
        let right = b.started_at.unwrap_or(fallback);
        left.cmp(&right)
    });
}

/// Bumped to 2 for v0.6.6 after live engine semantics changed. The persisted
/// thread/turn/item records did not change shape, but a v1 reader on a v2
/// session should still fail closed rather than silently mis-replay.
// Text-only writes retain v2. Image-bearing records require v3 so an older
// binary refuses recovery instead of silently dropping accepted attachments.
const CURRENT_RUNTIME_SCHEMA_VERSION: u32 = 2;
const IMAGE_RUNTIME_SCHEMA_VERSION: u32 = 3;
// Explicit allowances need a newer reader so old binaries cannot retry uncapped.
const OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION: u32 = 4;
const PREFIX_CHECKPOINT_RUNTIME_SCHEMA_VERSION: u32 = 5;
const MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION: u32 = OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION;

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn serialize_route_label_option<S>(
    value: &Option<String>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    value
        .as_deref()
        .map(codewhale_runtime::safe_label::sanitize_persisted_route_label)
        .serialize(serializer)
}

fn serialize_endpoint_fingerprint_option<S>(
    value: &Option<String>,
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    value
        .as_deref()
        .filter(|fingerprint| {
            fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .map(str::to_ascii_lowercase)
        .serialize(serializer)
}

fn serialize_routed_usage_source_ids<S>(
    values: &[String],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    values
        .iter()
        .map(|value| routed_usage_source_fingerprint(value))
        .collect::<Vec<_>>()
        .serialize(serializer)
}

fn routed_usage_source_fingerprint(source_id: &str) -> String {
    let source_id = source_id.trim();
    if source_id.len() == 64 && source_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        source_id.to_ascii_lowercase()
    } else {
        crate::cost_status::usage_source_fingerprint(source_id)
    }
}

fn serialize_routed_usage_drop_records<S>(
    values: &[RuntimeUsageDropRecord],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    values
        .iter()
        .map(|record| RuntimeUsageDropRecord {
            reason: record.reason,
            source_id: routed_usage_source_fingerprint(&record.source_id),
            route: record.route.sanitized_for_persistence(),
        })
        .collect::<Vec<_>>()
        .serialize(serializer)
}
fn serialize_decision_receipts<S>(
    values: &[crate::cost_status::RuntimeDecisionReceipt],
    serializer: S,
) -> std::result::Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    values
        .iter()
        .map(crate::cost_status::RuntimeDecisionReceipt::sanitized)
        .collect::<Vec<_>>()
        .serialize(serializer)
}

const fn default_runtime_schema_version() -> u32 {
    CURRENT_RUNTIME_SCHEMA_VERSION
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTurnStatus {
    Queued,
    InProgress,
    Completed,
    Failed,
    Interrupted,
    Canceled,
}

impl RuntimeTurnStatus {
    /// Queued or in-progress — the statuses `GET /v1/threads/running` counts.
    pub const fn is_active_work(self) -> bool {
        matches!(self, Self::Queued | Self::InProgress)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnItemKind {
    UserMessage,
    AgentMessage,
    AgentReasoning,
    ToolCall,
    FileChange,
    CommandExecution,
    ContextCompaction,
    Status,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnItemLifecycleStatus {
    Queued,
    InProgress,
    Completed,
    Failed,
    Interrupted,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadRecord {
    #[serde(default = "default_runtime_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub model: String,
    /// Generic provider kind for this thread's model route. Named custom
    /// routes remain `custom` for compatibility with enum-only consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    /// Exact non-secret configured provider key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider_id: Option<String>,
    /// Optional thread-level reasoning preference. A turn may override this;
    /// when absent, the Runtime falls back to the configured preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Optional thread-level model-visible tool allowlist. `None` keeps the
    /// normal configured tool catalog; `Some([])` deliberately exposes none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<Vec<String>>,
    pub workspace: PathBuf,
    pub mode: String,
    /// Named default permission posture for new turns. Absent on legacy
    /// records, whose effective posture is derived from the old fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_posture: Option<String>,
    pub allow_shell: bool,
    pub trust_mode: bool,
    pub auto_approve: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_response_bookmark: Option<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    /// User-set title for the thread. When `None`, consumers fall back to a
    /// derived title (typically the latest turn's input summary). Added in
    /// v0.8.10 (#562); old runtime records simply have no `title` and behave
    /// as before. Schema version is not bumped because this field is purely
    /// additive metadata — older readers ignore it without misinterpretation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Full-fidelity saved history prefix. Engine recovery verifies its checkpoint
    /// and appends later durable Runtime turns without duplicating saved messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Exact saved transcript and the last Runtime turn it already contains.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_session_checkpoint: Option<SavedSessionCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read: Option<ThreadReadMarker>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadReadMarker {
    pub item_id: String,
    pub last_in_turn: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark_item_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedSessionCheckpoint {
    pub covered_turn_id: Option<String>,
    /// Fingerprint of the document's first `messages_len` messages.
    pub messages_sha256: String,
    /// How many leading document messages the fingerprint covers (#6144).
    ///
    /// The document is the conversation's own record, and other writers
    /// append to it legitimately — a TUI autosave of the same id, a later
    /// `PUT /v1/sessions`. A fingerprint of the *whole* document turned every
    /// such append into "changed after this thread's checkpoint" and stranded
    /// the thread. Fingerprinting a prefix keeps the thread's history exact
    /// (document prefix + its own later turns) while the document grows.
    /// `None` is a checkpoint written before this field; it is migrated on
    /// read by finding the prefix its whole-document fingerprint names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages_len: Option<usize>,
    /// A backtracked fork may retain only a prefix of the verified snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_messages: Option<usize>,
}

fn session_messages_sha256(messages: &[Message]) -> Result<String> {
    Ok(hex_digest(Sha256::digest(serde_json::to_vec(messages)?)))
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// How many leading `messages` the checkpoint's fingerprint covers, or `None`
/// when no prefix of this document is the one the thread was bound to.
///
/// A checkpoint with `messages_len` is checked directly. A legacy one
/// fingerprinted the whole document at bind time, and the document may have
/// grown since, so every prefix is a candidate. `serde_json` writes a slice as
/// `[` + elements joined by `,` + `]`, so one pass that serializes each message
/// once can fingerprint every prefix — no quadratic re-serialization.
pub fn checkpoint_prefix_len(
    checkpoint: &SavedSessionCheckpoint,
    messages: &[Message],
) -> Result<Option<usize>> {
    if let Some(len) = checkpoint.messages_len {
        if len > messages.len() {
            return Ok(None);
        }
        return Ok(
            (session_messages_sha256(&messages[..len])? == checkpoint.messages_sha256)
                .then_some(len),
        );
    }
    let mut hasher = Sha256::new();
    hasher.update(b"[");
    let close = |hasher: &Sha256| hex_digest(hasher.clone().chain_update(b"]").finalize());
    if close(&hasher) == checkpoint.messages_sha256 {
        return Ok(Some(0));
    }
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            hasher.update(b",");
        }
        hasher.update(serde_json::to_vec(message)?);
        if close(&hasher) == checkpoint.messages_sha256 {
            return Ok(Some(index + 1));
        }
    }
    Ok(None)
}

pub fn checkpoint_covers_every_turn(
    checkpoint: &SavedSessionCheckpoint,
    turns: &[TurnRecord],
) -> bool {
    match checkpoint.covered_turn_id.as_deref() {
        Some(id) => turns.last().is_some_and(|turn| turn.id == id),
        None => turns.is_empty(),
    }
}

/// A thread's saved-session binding no longer describes any readable
/// document: the document is gone, or its prefix is not the one the thread
/// was bound to. The thread still owns its turns, so the binding is dropped
/// (with a receipt) and the thread hydrates from them instead of failing.
#[derive(Debug)]
pub struct StaleSessionBinding {
    pub reason: String,
}

impl std::fmt::Display for StaleSessionBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for StaleSessionBinding {}

/// The session document a Runtime thread writes under when nothing bound it
/// to one yet (#6144).
///
/// A Runtime thread's engine used to mint a random conversation id every time
/// it was loaded, so everything it kept under its session directory —
/// compaction transfers, background-shell evidence, truncation spills — went
/// to a directory no document would ever name, and even the thread lost it at
/// the next load. Exporting the thread (`POST /v1/sessions`) minted yet
/// another id. Deriving the id from the thread makes all three agree, and
/// makes the export idempotent: a retry after a crash between "save the
/// document" and "bind the checkpoint" finds the document it already wrote.
pub fn thread_session_id(thread_id: &str) -> String {
    // The thread's own id: the same id its engine runs under (see
    // `ensure_engine_loaded`, #6621), so the export, the engine's session
    // directory and the thread all name one conversation, and the export
    // stays idempotent because the id is fixed by the thread.
    thread_id.to_string()
}

/// Stored item correlation is a projection of the existing history identity.
/// Explicit executions never fall back to legacy provider ids. New non-model
/// events carry an execution id but no provider id and do not enter history.
#[derive(Clone, Copy)]
struct RuntimeToolIdentity<'a> {
    execution_id: Option<&'a str>,
    provider_id: &'a str,
}

impl<'a> RuntimeToolIdentity<'a> {
    fn read(metadata: Option<&'a Value>, side: &str) -> Result<Option<Self>> {
        let Some(meta) = metadata else {
            return Ok(None);
        };
        let Some(local) = meta.get(side) else {
            return Ok(None);
        };
        let local = local.as_str().context("Invalid stored tool correlation")?;
        let Some(execution) = meta.get("execution_id") else {
            return Ok((!local.is_empty()).then_some(Self {
                execution_id: None,
                provider_id: local,
            }));
        };
        let execution = execution
            .as_str()
            .filter(|id| !id.is_empty())
            .context("Invalid stored tool execution identity")?;
        anyhow::ensure!(
            local == execution,
            "Stored tool execution identity mismatch"
        );
        let Some(provider) = meta.get("provider_tool_use_id") else {
            return Ok(None);
        };
        let provider = provider
            .as_str()
            .filter(|id| !id.is_empty())
            .context("Invalid stored provider tool identity")?;
        Ok(Some(Self {
            execution_id: Some(execution),
            provider_id: provider,
        }))
    }

    fn key(self) -> (ToolCallKey<'a>, &'a str) {
        (
            self.execution_id.map_or(
                ToolCallKey::LegacyProvider(self.provider_id),
                ToolCallKey::Execution,
            ),
            self.provider_id,
        )
    }
}

/// A persisted execution cannot acquire a different provider pairing on a
/// later result, turn, or import block. This is a transient validation index,
/// never a grant or another history store; legacy wire-only keys stay unchanged.
fn check_execution_provider(
    bindings: &mut HashMap<String, String>,
    execution: Option<&str>,
    provider: &str,
) -> Result<()> {
    if let Some(execution) = execution {
        anyhow::ensure!(
            !execution.is_empty() && !provider.is_empty(),
            "Invalid stored tool execution identity"
        );
        if let Some(previous) = bindings.get(execution) {
            anyhow::ensure!(
                previous == provider,
                "Stored tool execution has inconsistent provider identity"
            );
        } else {
            bindings.insert(execution.to_string(), provider.to_string());
        }
    }
    Ok(())
}

/// Legacy item reconstruction covered generic tool rows only. New model
/// correlation survives the UI's shell/file classification as well; a local
/// non-model record is still excluded by RuntimeToolIdentity::read.
fn projects_tool_history(item: &TurnItemRecord) -> bool {
    item.kind == TurnItemKind::ToolCall
        || (matches!(
            item.kind,
            TurnItemKind::FileChange | TurnItemKind::CommandExecution
        ) && item
            .metadata
            .as_ref()
            .is_some_and(|meta| meta.get("execution_id").is_some()))
}

/// The result a rebuilt history must give a tool call whose outcome the turn
/// store never recorded, or `None` when nothing is missing.
///
/// A call that was interrupted or canceled can be persisted as a single
/// `tool_call` item that carries the call alone, and so can a call that failed
/// under a runtime older than #6803, which kept the failure text on the item's
/// own `detail` without marking it `tool_result_for`. (A failure recorded now
/// carries that marker, so the record answers it like any completed call and
/// nothing is missing here.) Rebuilding
/// only the call leaves it unanswered, which a provider rejects outright —
/// `No tool output found for tool call …`. The missing result is also what
/// stopped the first of two responses from flushing, so the rebuild glued them
/// into one assistant message (`thinking`, `call`, `thinking`, `call`) and the
/// provider read *that* as the first call never being answered, even after the
/// request-time repair had answered it.
///
/// Both are cured by answering the call where its own outcome is known: the
/// item's failure text for a failure, and the repository's interrupted-call
/// notice for a call the process never finished.
///
/// An answer is only ever paired with a call the rebuild actually emits, so the
/// identity required here is the one the caller needs before it emits the call
/// at all (#5823): a snapshot with no tool name is skipped rather than replayed
/// as an empty shell, and answering it would leave the result on its own.
///
/// A call still running is deliberately left alone. A rebuild of a live turn
/// must not invent an outcome the engine is still waiting for — the result is
/// coming — so this answers only the terminal failures.
fn unanswered_call_result(
    item: &TurnItemRecord,
    identity: RuntimeToolIdentity<'_>,
    call_name: &str,
    recorded_results: &HashSet<(ToolCallKey<'_>, &str)>,
) -> Option<String> {
    if identity.provider_id.is_empty()
        || call_name.is_empty()
        || recorded_results.contains(&identity.key())
    {
        return None;
    }
    match item.status {
        TurnItemLifecycleStatus::Failed => {
            Some(item.detail.clone().unwrap_or_else(|| item.summary.clone()))
        }
        TurnItemLifecycleStatus::Interrupted | TurnItemLifecycleStatus::Canceled => {
            Some(codewhale_runtime::tool_history_repair::CRASH_REPAIR_CONTENT.to_string())
        }
        TurnItemLifecycleStatus::Queued
        | TurnItemLifecycleStatus::InProgress
        | TurnItemLifecycleStatus::Completed => None,
    }
}

/// Bounded per-turn request facts copied only from the engine's terminal
/// request snapshot. These are distinct from provider-reported `TurnUsage`:
/// a model-client request and its HTTP retries may have no usage receipt.
/// These counts are observations, never provider invoices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTurnRequestDiagnostics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<RuntimeTurnStopReason>,
    /// Parent streaming model-client calls, including stream retries.
    pub model_requests_started: u32,
    /// HTTP retries entered inside the captured streaming request, not invoices.
    #[serde(default)]
    pub transport_retries: u32,
    /// Retries after a stream ended before content was observed.
    pub transparent_stream_retries: u32,
    /// Retry/resume attempts after an interrupted stream.
    pub stream_resumes: u32,
    /// Bounded re-requests after a reasoning-only provider response.
    #[serde(default)]
    pub reasoning_only_reprompts: u32,
    /// Bounded re-requests after a clean empty provider stop.
    #[serde(default)]
    pub empty_stop_retries: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTurnStopReason {
    ProviderNoToolCall,
    ProviderToolCallMissing,
    StepBudgetExhausted,
    NoProgress,
    Interrupted,
    Failed,
}

impl From<&crate::tool_inspection::TurnStopDiagnostics> for RuntimeTurnRequestDiagnostics {
    fn from(diagnostics: &crate::tool_inspection::TurnStopDiagnostics) -> Self {
        Self {
            stop_reason: diagnostics.reason.map(|reason| match reason {
                crate::tool_inspection::TurnStopReason::ProviderNoToolCall => {
                    RuntimeTurnStopReason::ProviderNoToolCall
                }
                crate::tool_inspection::TurnStopReason::ProviderToolCallMissing => {
                    RuntimeTurnStopReason::ProviderToolCallMissing
                }
                crate::tool_inspection::TurnStopReason::StepBudgetExhausted => {
                    RuntimeTurnStopReason::StepBudgetExhausted
                }
                crate::tool_inspection::TurnStopReason::NoProgress => {
                    RuntimeTurnStopReason::NoProgress
                }
                crate::tool_inspection::TurnStopReason::Interrupted => {
                    RuntimeTurnStopReason::Interrupted
                }
                crate::tool_inspection::TurnStopReason::Failed => RuntimeTurnStopReason::Failed,
            }),
            model_requests_started: diagnostics.model_requests_started,
            transport_retries: diagnostics.transport_retries,
            transparent_stream_retries: diagnostics.transparent_stream_retries,
            stream_resumes: diagnostics.stream_resumes,
            reasoning_only_reprompts: diagnostics.reasoning_only_reprompts,
            empty_stop_retries: diagnostics.empty_stop_retries,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeOperationSpan {
    pub span_id: String,
    pub activity_kind: codewhale_protocol::engine_owner::OwnerActivityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeOperationCompletion {
    pub span_id: String,
    pub activity_kind: codewhale_protocol::engine_owner::OwnerActivityKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    pub completed_at: DateTime<Utc>,
    pub outcome: codewhale_protocol::engine_owner::OwnerOperationOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeOperationActivity {
    pub observed_at: DateTime<Utc>,
    pub active: Vec<RuntimeOperationSpan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_completed: Option<RuntimeOperationCompletion>,
    #[serde(default)]
    pub overflowed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimePrefixCheckpoint {
    checkpoint_id: String,
    session_id: String,
    agent_id: Option<String>,
    runtime_turn_id: String,
    engine_turn_id: String,
    call_id: String,
    owner_id: String,
    source_sha256: String,
    persisted_sha256: String,
    messages_len: usize,
    projection: String,
    item_ids: Vec<String>,
    covered_items: HashMap<String, String>,
    pending_items: Vec<String>,
    tool_declarations: HashMap<String, String>,
}

fn checkpoint_item_digest(item: &TurnItemRecord) -> Result<String> {
    Ok(hex_digest(Sha256::digest(serde_json::to_vec(&json!({
        "kind": item.kind,
        "summary": item.summary,
        "detail": item.detail,
        "metadata": item.metadata,
    }))?)))
}

fn ordered_turn_items(
    turn: &TurnRecord,
    items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
) -> Result<Vec<TurnItemRecord>> {
    let stored = items_by_turn.get(&turn.id).cloned().unwrap_or_default();
    let mut by_id = HashMap::new();
    for item in &stored {
        anyhow::ensure!(
            item.turn_id == turn.id,
            "Stored item belongs to another turn"
        );
        anyhow::ensure!(
            by_id.insert(item.id.clone(), item.clone()).is_none(),
            "Duplicate stored item"
        );
    }
    let mut ordered = Vec::with_capacity(stored.len());
    for id in &turn.item_ids {
        if let Some(item) = by_id.remove(id) {
            ordered.push(item);
        }
    }
    for item in stored {
        if by_id.remove(&item.id).is_some() {
            ordered.push(item);
        }
    }
    Ok(ordered)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    #[serde(default, skip_serializing)]
    pub prefix_checkpoint: Option<RuntimePrefixCheckpoint>,
    /// Admitted per-request allowance. Older turns have no explicit allowance.
    #[serde(
        default,
        rename = "maxOutputTokens",
        alias = "max_output_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_output_tokens: Option<std::num::NonZeroU32>,
    #[serde(default = "default_runtime_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub thread_id: String,
    pub status: RuntimeTurnStatus,
    pub input_summary: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Accounting-only receipt for auxiliary work completed before parent
    /// admission. It stays visible in history and cost totals, but never
    /// advances the thread's latest accepted turn, including after restart.
    #[serde(default)]
    pub routing_settlement: bool,
    /// Portion of `usage` served by this turn's persisted effective route.
    /// New records use this for parent cost/token accumulation, then add each
    /// routed child independently. Legacy absence falls back to `usage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_route_usage: Option<Usage>,
    /// Canonical posture that governed this turn. New records always carry
    /// this receipt; old records deserialize with no fabricated value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_posture: Option<String>,
    /// Canonical mode this turn ran in (`agent` / `plan` / `operate`), resolved
    /// from the same policy projection as `permission_posture`. It is the
    /// per-turn record of the mode, which the thread cannot answer: `mode` may
    /// have been switched since, so a client reading the thread at completion
    /// learns how the thread is set up *now*, not how the run it is looking at
    /// ran. New records always carry it; records this runtime did not run (an
    /// imported conversation, a failed settlement for an unaccepted turn) and
    /// pre-existing records have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Concrete generic provider kind selected for this turn.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_route_label_option"
    )]
    pub effective_provider: Option<String>,
    /// Exact non-secret configured provider key selected for this turn.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_route_label_option"
    )]
    pub effective_provider_id: Option<String>,
    /// Requested OpenRouter upstream frozen at dispatch, when pinned.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_route_label_option"
    )]
    pub effective_openrouter_vendor: Option<String>,
    /// Non-secret discriminator for routes whose provider/model pair spans
    /// different billing systems (for example StepFun PAYG vs Step Plan).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_route_label_option"
    )]
    pub effective_billing_surface: Option<String>,
    /// SHA-256 fingerprint of the concrete dispatch endpoint. Raw URLs are
    /// intentionally never persisted.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_endpoint_fingerprint_option"
    )]
    pub effective_endpoint_fingerprint: Option<String>,
    /// Immutable provider-live rate receipt frozen at CodeWhale's pre-permit
    /// application-dispatch boundary. Missing on legacy records; Baseten then
    /// stays unpriced while OpenRouter may use only its immutable bundled row.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::provider_catalog_live::deserialize_optional_provider_live_pricing"
    )]
    pub effective_provider_live_pricing: Option<ProviderLivePricingQuote>,
    /// Immutable billing classification captured before dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_billing_mode: Option<RouteBillingMode>,
    /// Dispatch timestamp used for historical/live pricing lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_dispatched_at: Option<DateTime<Utc>>,
    /// Concrete wire model selected for this turn (especially important when
    /// the thread is configured as `auto`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_route_label_option"
    )]
    pub effective_model: Option<String>,
    /// Model calls made beneath this parent turn, each paired with its own
    /// immutable route. These are exclusive of `effective_route_usage`;
    /// `usage` remains the authoritative all-token turn total and may include
    /// inline RLM/guardian calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routed_usage: Vec<EffectiveRouteUsage>,
    /// Decision evidence shares this durable origin-turn ledger, including
    /// responses arriving after the parent terminal event.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "serialize_decision_receipts"
    )]
    pub decision_receipts: Vec<crate::cost_status::RuntimeDecisionReceipt>,
    /// Exact missing-usage provider responses paired with their frozen route.
    /// Sources are persisted only as fingerprints, routes are sanitized, and
    /// the shared source ledger bounds usage plus missing-usage records.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "serialize_routed_usage_drop_records"
    )]
    pub routed_usage_drop_records: Vec<RuntimeUsageDropRecord>,
    /// Fingerprints of provider-call identities already appended to this turn.
    /// This durable ledger makes mailbox delivery, direct sinks, fallback
    /// recovery, and process restart idempotent without persisting raw ids.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "serialize_routed_usage_source_ids"
    )]
    pub routed_usage_source_ids: Vec<String>,
    /// Background provider calls discarded from the bounded fallback journal.
    /// Non-zero means token/cost aggregation is necessarily incomplete.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub routed_usage_dropped_records: u64,
    /// Terminal facts about parent streaming model-client calls. This is
    /// absent until the engine emits its terminal request snapshot. It counts
    /// model-client calls, not HTTP retries within the client or invoices.
    #[serde(
        default,
        rename = "modelRequestDiagnostics",
        alias = "model_request_diagnostics",
        skip_serializing_if = "Option::is_none"
    )]
    pub model_request_diagnostics: Option<RuntimeTurnRequestDiagnostics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_activity: Option<RuntimeOperationActivity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub item_ids: Vec<String>,
    #[serde(default)]
    pub steer_count: usize,
    /// Stable Agent Mail id that caused this turn. This is the durable
    /// idempotency bridge between a claimed mail envelope and the existing
    /// turn queue; ordinary external-user turns leave it unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_mail_message_id: Option<String>,
    /// What this turn produced, merged from its items' refs and, once
    /// settled, the workspace snapshot delta. Empty until the turn ends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<TurnArtifactRef>,
    /// Where the workspace-level accounting stands. `None` while the turn
    /// runs (and on records written before this field existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<TurnWorkspaceArtifacts>,
    /// Workspace snapshots the engine took while this turn ran, in the order
    /// the engine reported them (one FIFO event channel, one consumer per
    /// turn): the turn's `pre_turn` restore point, one `tool` snapshot before
    /// each file-modifying tool call, and its `post_turn` state. A thread owns
    /// exactly the restore points recorded on its own turns — a fork owns the
    /// ones its cloned turns carry — and turn-scoped undo and file revert
    /// resolve only these. Records written before this field existed, turns
    /// imported from a saved session, and turns that ran with snapshots off
    /// carry none, and so have no restore point.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_snapshots: Vec<codewhale_snapshot::WorkspaceSnapshotRef>,
}

impl TurnRecord {
    fn validate_output_token_limit(&self) -> Result<()> {
        if let Some(activity) = &self.operation_activity {
            let safe_id =
                |id: &str| !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control);
            anyhow::ensure!(
                activity.active.len() <= 256
                    && activity.active.iter().all(|span| {
                        safe_id(&span.span_id)
                            && span.action_id.as_deref().is_none_or(safe_id)
                            && span.started_at <= activity.observed_at
                    })
                    && activity.last_completed.as_ref().is_none_or(|span| {
                        safe_id(&span.span_id)
                            && span.action_id.as_deref().is_none_or(safe_id)
                            && span.completed_at <= activity.observed_at
                    }),
                "Turn operation activity exceeds its bound"
            );
        }
        if self.decision_receipts.len() > MAX_ROUTED_USAGE_RECORDS_PER_TURN
            || self.decision_receipts.iter().any(|r| !r.is_bounded())
        {
            bail!("Turn decision evidence exceeds its bound");
        }
        anyhow::ensure!(
            self.prefix_checkpoint.is_none()
                || self.schema_version >= PREFIX_CHECKPOINT_RUNTIME_SCHEMA_VERSION,
            "Runtime prefix checkpoint requires its recovery schema"
        );
        if self.schema_version > PREFIX_CHECKPOINT_RUNTIME_SCHEMA_VERSION {
            bail!(
                "Turn schema v{} is newer than supported v{}",
                self.schema_version,
                PREFIX_CHECKPOINT_RUNTIME_SCHEMA_VERSION
            );
        }
        if (self.max_output_tokens.is_some()
            && self.schema_version < OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION)
            || (self.max_output_tokens.is_none()
                && self.schema_version == OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION)
        {
            bail!("Turn output allowance does not match its schema");
        }
        Ok(())
    }

    pub fn effective_provider_label(&self) -> Option<&str> {
        self.effective_provider_id
            .as_deref()
            .filter(|identity| !identity.trim().is_empty())
            .or_else(|| {
                self.effective_provider
                    .as_deref()
                    .filter(|provider| !provider.trim().is_empty())
            })
    }

    fn persist_effective_route(&mut self, route: &EffectiveRouteEnvelope) {
        let route = route.sanitized_for_persistence();
        self.effective_provider = Some(route.provider.as_str().to_string());
        self.effective_provider_id = Some(route.provider_identity);
        self.effective_openrouter_vendor = route.openrouter_vendor;
        self.effective_billing_surface = route.billing_surface;
        self.effective_endpoint_fingerprint = route.endpoint_fingerprint;
        self.effective_provider_live_pricing = route.provider_live_pricing;
        self.effective_billing_mode = Some(route.billing_mode);
        self.effective_dispatched_at = Some(route.dispatched_at);
        self.effective_model = Some(route.model);
    }

    /// Rehydrate only a complete persisted dispatch record. Legacy rows must
    /// not borrow a provider identity or timestamp from the current thread.
    fn effective_route_envelope(&self) -> Option<EffectiveRouteEnvelope> {
        let provider = self
            .effective_provider
            .as_deref()
            .and_then(ProviderKind::parse)?;
        let provider_identity = self
            .effective_provider_id
            .as_deref()
            .filter(|identity| !identity.trim().is_empty())?
            .to_string();
        let model = self
            .effective_model
            .as_deref()
            .filter(|model| !model.trim().is_empty())?
            .to_string();
        let dispatched_at = self.effective_dispatched_at?;
        Some(
            EffectiveRouteEnvelope {
                provider,
                provider_identity,
                model,
                openrouter_vendor: self.effective_openrouter_vendor.clone(),
                billing_surface: self.effective_billing_surface.clone(),
                endpoint_fingerprint: self.effective_endpoint_fingerprint.clone(),
                provider_live_pricing: self.effective_provider_live_pricing.clone(),
                billing_mode: self
                    .effective_billing_mode
                    .unwrap_or(RouteBillingMode::Unknown),
                dispatched_at,
            }
            .sanitized_for_persistence(),
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnItemRecord {
    #[serde(default = "default_runtime_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub turn_id: String,
    pub kind: TurnItemKind,
    pub status: TurnItemLifecycleStatus,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// Legacy projection of `artifacts`: workspace-relative paths of the
    /// files this item created, changed or renamed (never deleted files,
    /// spills or media). Derived only by `legacy_artifact_refs`.
    #[serde(default)]
    pub artifact_refs: Vec<PathBuf>,
    /// What this item produced, as typed references. The authority for a
    /// single tool call's artifacts; the turn aggregate is merged from these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<TurnArtifactRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
}

impl TurnItemRecord {
    fn set_image_content(&mut self, content: Vec<ContentBlock>) {
        self.schema_version = IMAGE_RUNTIME_SCHEMA_VERSION;
        let metadata = self.metadata.get_or_insert_with(|| json!({}));
        metadata["runtime_image_content"] = json!(content);
    }

    fn user_content(&self) -> Result<Vec<ContentBlock>> {
        if (self.schema_version == IMAGE_RUNTIME_SCHEMA_VERSION
            || (self.schema_version == OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION
                && self
                    .metadata
                    .as_ref()
                    .is_some_and(|meta| meta.get("runtime_image_content").is_some())))
            && self.kind == TurnItemKind::UserMessage
        {
            let content = self
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("runtime_image_content"))
                .context("persisted image input is missing its content")?;
            let blocks: Vec<ContentBlock> =
                serde_json::from_value(content.clone()).context("invalid persisted image input")?;
            crate::image_attach::validate_stored_image_content(&blocks)?;
            if self.detail.as_deref().is_some_and(|text| !text.trim().is_empty())
                && !blocks.iter().any(|block| matches!(block, ContentBlock::Text { text, .. } if !text.trim().is_empty()))
            {
                bail!("persisted image input is missing its prompt");
            }
            return Ok(blocks);
        }
        let text = self.detail.as_ref().unwrap_or(&self.summary);
        Ok(if text.trim().is_empty() {
            Vec::new()
        } else {
            vec![ContentBlock::Text {
                text: text.clone(),
                cache_control: None,
            }]
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEventRecord {
    #[serde(default = "default_runtime_schema_version")]
    pub schema_version: u32,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    pub event: String,
    pub payload: Value,
}

/// Event name for a runtime store record the runtime could not read, parse,
/// or write. Its payload is a [`RuntimeStoreFailureNotice`] (#5931).
pub const RUNTIME_STORE_FAILURE_EVENT: &str = "runtime.store_failure";

/// Which runtime store file family failed. The nouns are the store's own
/// directories (`threads/`, `turns/`, `items/`), so a notice can point at one
/// file and mean it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStoreRecordKind {
    Thread,
    Turn,
    Item,
}

impl std::fmt::Display for RuntimeStoreRecordKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Thread => "thread",
            Self::Turn => "turn",
            Self::Item => "item",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStoreOperation {
    Read,
    Parse,
    Write,
}

impl std::fmt::Display for RuntimeStoreOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Parse => "parse",
            Self::Write => "write",
        })
    }
}

/// A runtime store record the operator's own disk could not read, parse, or
/// write. The store's load/save paths attach it as typed `anyhow` context, so
/// a monitor recognizes a store fault by type instead of by message text and
/// can name the file and the next action in a visible notice (#5931).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStoreRecordFailure {
    pub operation: RuntimeStoreOperation,
    pub record_kind: RuntimeStoreRecordKind,
    pub record_id: String,
    pub path: PathBuf,
}

impl RuntimeStoreRecordFailure {
    fn new(
        operation: RuntimeStoreOperation,
        record_kind: RuntimeStoreRecordKind,
        record_id: &str,
        path: &Path,
    ) -> Self {
        Self {
            operation,
            record_kind,
            record_id: record_id.to_string(),
            path: path.to_path_buf(),
        }
    }

    /// The typed store fault behind an error, if any layer of it is one.
    #[must_use]
    pub fn from_error(error: &anyhow::Error) -> Option<&Self> {
        error.downcast_ref::<Self>()
    }

    /// What the operator can do about it: which file to move aside, or
    /// where to check space and permissions. Nothing here is guessed.
    #[must_use]
    pub fn next_action(&self) -> String {
        match self.operation {
            RuntimeStoreOperation::Read | RuntimeStoreOperation::Parse => format!(
                "Move {} aside (or delete it) and retry; the thread's other records stay in place.",
                self.path.display()
            ),
            RuntimeStoreOperation::Write => format!(
                "Check free space and permissions for {}, then retry; nothing was overwritten.",
                self.path.display()
            ),
        }
    }

    /// Build the event payload for this fault. `terminal` says the runtime
    /// already knows the turn can never reach `turn.completed`.
    #[must_use]
    pub fn notice(&self, error: &anyhow::Error, terminal: bool) -> RuntimeStoreFailureNotice {
        let reason = error
            .chain()
            .last()
            .map(ToString::to_string)
            .unwrap_or_default();
        let next_action = self.next_action();
        let message = format!(
            "Session runtime store: {} {} at {} could not be {}: {reason}. {next_action}",
            self.record_kind,
            self.record_id,
            self.path.display(),
            match self.operation {
                RuntimeStoreOperation::Read => "read",
                RuntimeStoreOperation::Parse => "parsed",
                RuntimeStoreOperation::Write => "written",
            },
        );
        RuntimeStoreFailureNotice {
            failure: self.clone(),
            error: format!("{error:#}"),
            reason,
            next_action,
            message,
            terminal,
        }
    }
}

impl std::fmt::Display for RuntimeStoreRecordFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Failed to {} {} {}",
            self.operation,
            self.record_kind,
            self.path.display()
        )
    }
}

/// Payload of a `runtime.store_failure` event: the operator's own on-disk
/// state failed, and every consumer (task timeline, SSE client, TUI toast)
/// gets the file, the reason, and the next action rather than a log line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStoreFailureNotice {
    #[serde(flatten)]
    pub failure: RuntimeStoreRecordFailure,
    /// Full error chain, outermost first.
    pub error: String,
    /// Root cause alone (an OS or parser message), for compact surfaces.
    pub reason: String,
    /// What the operator can do about it.
    pub next_action: String,
    /// One-line operator text: record, path, reason, and next action.
    pub message: String,
    /// True when this turn will never reach `turn.completed`: its own record
    /// is unreadable or unwritable, so nothing can be terminalized. Drivers
    /// waiting on the turn should stop waiting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}

pub struct RuntimeEventReplay {
    /// Cursor immediately before the first replayed event. For a tail-limited
    /// replay this advances past omitted history so continuity remains exact.
    pub base_seq: u64,
    /// Filesystem parsing happens on the blocking pool and publishes bounded
    /// chunks through this small channel, applying backpressure instead of
    /// allocating an unbounded backlog on a Tokio worker. A closed channel
    /// means history is complete; every failure, a worker panic included,
    /// arrives as an `Err` batch first.
    pub batches: mpsc::Receiver<std::result::Result<Vec<RuntimeEventRecord>, String>>,
}

type RuntimeEventReader = BufReader<std::io::Take<File>>;

enum RuntimeEventMatch {
    ThreadStarted,
    TurnCompleted {
        turn_id: String,
    },
    DynamicTerminal {
        turn_id: String,
        call_id: String,
    },
    AgentMail {
        event_name: String,
        message_id: String,
        attempt_count: u8,
    },
    WorkerLifecycle {
        turn_id: String,
        agent_id: String,
        event_name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeStoreState {
    #[serde(default = "default_runtime_schema_version")]
    schema_version: u32,
    next_seq: u64,
}

impl Default for RuntimeStoreState {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
            next_seq: 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventAppendFailureDisposition {
    RolledBack,
    Indeterminate,
}

#[derive(Debug)]
struct RuntimeEventAppendError {
    disposition: EventAppendFailureDisposition,
    append_error: String,
    rollback_error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("Runtime event lock timed out after {0:?}")]
struct RuntimeEventLockTimeout(Duration);

impl RuntimeEventAppendError {
    const fn retry_safe(&self) -> bool {
        matches!(self.disposition, EventAppendFailureDisposition::RolledBack)
    }
}

impl std::fmt::Display for RuntimeEventAppendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rollback_error {
            Some(rollback_error) => write!(
                formatter,
                "Runtime event append is indeterminate after append error ({}) and rollback error ({})",
                self.append_error, rollback_error
            ),
            None => write!(
                formatter,
                "Runtime event append failed and was rolled back: {}",
                self.append_error
            ),
        }
    }
}

impl std::error::Error for RuntimeEventAppendError {}

#[derive(Debug, Clone)]
pub struct RuntimeThreadStore {
    threads_dir: PathBuf,
    turns_dir: PathBuf,
    items_dir: PathBuf,
    events_dir: PathBuf,
    goals_dir: PathBuf,
    /// Serializes goal controls and revision-fenced progress transactions.
    /// Acquired after `active` at admission; never held across an await.
    goal_mutation: Arc<parking_lot::Mutex<()>>,
    mail_dir: PathBuf,
    turn_operations_dir: PathBuf,
    owner_id: String,
    state_path: PathBuf,
    event_lock_path: PathBuf,
    /// Serializes load-modify-save operations on thread records. The guard is
    /// synchronous and must never cross an `.await`; JSON records are small,
    /// and one global guard avoids per-thread lock lifecycle races.
    thread_mutation: Arc<parking_lot::Mutex<()>>,
    /// Serializes load-modify-save operations on turn records. Like the
    /// thread guard, it is synchronous and never crosses an `.await`.
    /// Reentrant so [`Self::save_turn`] can take the guard while an outer
    /// RMW transaction already holds it; every turn write goes through that
    /// path so a store-side settle cannot be stomped by a concurrent monitor
    /// save that loaded a stale in-flight copy.
    turn_mutation: Arc<parking_lot::ReentrantMutex<()>>,
    /// Serializes envelope claim/state transitions. The durable envelope is
    /// the queue; this guard prevents concurrent replay/wake requests from
    /// starting more than one turn for the same message.
    mail_mutation: Arc<parking_lot::Mutex<()>>,
    /// Turn id -> item ids, filled by one items-directory read and kept
    /// current by the item writers; see [`Self::item_ids_for_turns`].
    item_index: Arc<parking_lot::RwLock<ItemIndex>>,
    /// Serializes the one items-directory read that fills `item_index`. Held
    /// across the whole read, so a caller that arrives while it runs waits for
    /// its result instead of repeating it — and so an item writer can tell
    /// whether a read is running by probing it. A writer never holds it, so a
    /// running turn's write is never behind a store-wide read.
    item_index_seed: Arc<parking_lot::Mutex<()>>,
    /// Files read by whole-directory turn scans (`list_all_turns`). Shared
    /// across store clones so a `spawn_blocking` snapshot still counts against
    /// the manager the test holds. Per-store so parallel tests do not collide.
    #[cfg(any(test, feature = "test-support"))]
    turn_dir_files_read: Arc<std::sync::atomic::AtomicU64>,
    /// Files read by whole-directory item scans (`list_items_for_turn` and
    /// `list_items_for_turns_map`).
    #[cfg(any(test, feature = "test-support"))]
    item_dir_files_read: Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeStoreOwner {
    owner_id: String,
}

/// Every item id the store holds, grouped by turn.
///
/// An item's filename carries the item id and nothing else, so a reader that
/// wants one thread's items has no way to ask the filesystem for them: the only
/// answer is to read every item record. This makes that whole-directory read a
/// one-time cost per store instead of a per-request one.
#[derive(Debug, Default)]
struct ItemIndex {
    /// `None` until the items directory has been read once.
    by_turn: Option<HashMap<String, Vec<String>>>,
    /// Item writes that landed while the directory read was running and may
    /// therefore have been missed by it. Drained into `by_turn` when it is
    /// published. Only writes that race a read are recorded; see
    /// [`RuntimeThreadStore::note_item_in_index`].
    pending: Vec<(String, String)>,
}

impl RuntimeThreadStore {
    pub fn open(root: PathBuf) -> Result<Self> {
        let root = checked_runtime_store_root(root)?;
        ensure_runtime_store_dir(&root)?;
        let threads_dir = root.join("threads");
        let turns_dir = root.join("turns");
        let items_dir = root.join("items");
        let events_dir = root.join("events");
        let goals_dir = root.join("goals");
        let mail_dir = root.join("agent-mail");
        let turn_operations_dir = root.join("turn-operations");
        ensure_runtime_store_dir(&threads_dir)?;
        ensure_runtime_store_dir(&turns_dir)?;
        ensure_runtime_store_dir(&items_dir)?;
        ensure_runtime_store_dir(&events_dir)?;
        ensure_runtime_store_dir(&goals_dir)?;
        ensure_runtime_store_dir(&mail_dir)?;
        ensure_runtime_store_dir(&turn_operations_dir)?;
        let state_path = root.join("state.json");
        let owner_path = root.join(AGENT_MAIL_OWNER_FILE);
        let event_lock_path = root.join(EVENT_TRANSACTION_LOCK_FILE);
        // The owner namespaces operation-key fingerprints. Creating it outside
        // a cross-process transaction lets two first-start processes mint
        // different owners, and therefore different operation locks, for the
        // same store. Reuse the root event lock before any owner-derived path
        // is computed so all processes load exactly one durable owner.
        let owner_id = load_or_create_runtime_store_owner(&owner_path, &event_lock_path)?;
        let store = Self {
            threads_dir,
            turns_dir,
            items_dir,
            events_dir,
            goals_dir,
            mail_dir,
            turn_operations_dir,
            owner_id,
            state_path,
            event_lock_path,
            thread_mutation: Arc::new(parking_lot::Mutex::new(())),
            turn_mutation: Arc::new(parking_lot::ReentrantMutex::new(())),
            goal_mutation: Arc::new(parking_lot::Mutex::new(())),
            mail_mutation: Arc::new(parking_lot::Mutex::new(())),
            item_index: Arc::new(parking_lot::RwLock::new(ItemIndex::default())),
            item_index_seed: Arc::new(parking_lot::Mutex::new(())),
            #[cfg(any(test, feature = "test-support"))]
            turn_dir_files_read: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(any(test, feature = "test-support"))]
            item_dir_files_read: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        store.with_event_transaction(EVENT_TRANSACTION_LOCK_TIMEOUT, || {
            repair_torn_event_log_tails(&store.events_dir)?;
            if store.state_path.exists() {
                load_runtime_store_state(&store.state_path)?;
            } else {
                write_json_atomic(&store.state_path, &RuntimeStoreState::default())?;
            }
            Ok(())
        })?;
        store.recover_incomplete_turn_operations()?;
        store.recover_claimed_agent_mail()?;
        Ok(store)
    }

    /// Open an existing store only to read it: no directories, owner file,
    /// torn-tail repair, or recovery. `None` when `root` holds no store.
    /// Offline readers (`codewhale receipts`) use this so reading a thread
    /// never mutates a store a live `codewhale serve` may own. Event reads
    /// still take the shared event lock, so they see only committed records.
    ///
    /// The item index this store builds (see [`Self::ensure_item_index`]) is
    /// a snapshot of one open: a store another process owns keeps writing
    /// items, and those are invisible here until the next open reads the
    /// directory again. That is the contract every caller already lives
    /// under — `codewhale receipts` opens the store once per invocation and
    /// exits — so holding an instance across foreign writes serves no
    /// caller today. If one ever does, invalidate or reopen rather than
    /// serving a stale map silently.
    pub fn open_read_only(root: PathBuf) -> Result<Option<Self>> {
        let root = checked_runtime_store_root(root)?;
        let threads_dir = root.join("threads");
        if !threads_dir.is_dir() {
            return Ok(None);
        }
        Ok(Some(Self {
            threads_dir,
            turns_dir: root.join("turns"),
            items_dir: root.join("items"),
            events_dir: root.join("events"),
            goals_dir: root.join("goals"),
            mail_dir: root.join("agent-mail"),
            turn_operations_dir: root.join("turn-operations"),
            owner_id: String::new(),
            state_path: root.join("state.json"),
            event_lock_path: root.join(EVENT_TRANSACTION_LOCK_FILE),
            thread_mutation: Arc::new(parking_lot::Mutex::new(())),
            turn_mutation: Arc::new(parking_lot::ReentrantMutex::new(())),
            goal_mutation: Arc::new(parking_lot::Mutex::new(())),
            mail_mutation: Arc::new(parking_lot::Mutex::new(())),
            item_index: Arc::new(parking_lot::RwLock::new(ItemIndex::default())),
            item_index_seed: Arc::new(parking_lot::Mutex::new(())),
            #[cfg(any(test, feature = "test-support"))]
            turn_dir_files_read: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(any(test, feature = "test-support"))]
            item_dir_files_read: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }))
    }

    fn open_event_lock(&self) -> Result<File> {
        let file =
            open_runtime_store_file(&self.event_lock_path, "Runtime event lock", |options| {
                options.create(true).truncate(false).read(true).write(true);
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .context("Failed to secure Runtime event lock")?;
        }
        Ok(file)
    }

    fn with_event_transaction<T>(
        &self,
        timeout: Duration,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let mut lock = fd_lock::RwLock::new(self.open_event_lock()?);
        let started = Instant::now();
        let mut operation = Some(operation);
        loop {
            match lock
                .try_write()
                .map(|_guard| operation.take().expect("event transaction runs once")())
            {
                Ok(result) => return result,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    wait_for_event_lock(started, timeout)?;
                }
                Err(error) => return Err(error).context("Failed to lock Runtime events"),
            }
        }
    }

    fn record_path(base: &Path, id: &str, extension: &str, label: &str) -> Result<PathBuf> {
        let id = validated_record_id(id, label)?;
        Ok(base.join(format!("{id}.{extension}")))
    }

    fn thread_path(&self, thread_id: &str) -> Result<PathBuf> {
        Self::record_path(&self.threads_dir, thread_id, "json", "thread id")
    }

    fn turn_path(&self, turn_id: &str) -> Result<PathBuf> {
        Self::record_path(&self.turns_dir, turn_id, "json", "turn id")
    }

    fn item_path(&self, item_id: &str) -> Result<PathBuf> {
        Self::record_path(&self.items_dir, item_id, "json", "item id")
    }

    fn events_path(&self, thread_id: &str) -> Result<PathBuf> {
        Self::record_path(&self.events_dir, thread_id, "jsonl", "thread id")
    }

    fn goal_path(&self, thread_id: &str) -> Result<PathBuf> {
        Self::record_path(&self.goals_dir, thread_id, "json", "thread id")
    }

    fn mail_path(&self, message_id: &AgentMailMessageId) -> Result<PathBuf> {
        Self::record_path(
            &self.mail_dir,
            message_id.as_str(),
            "json",
            "Agent Mail message id",
        )
    }

    fn turn_operation_path(&self, operation_key_fingerprint: &str) -> Result<PathBuf> {
        validate_sha256_fingerprint(operation_key_fingerprint, "operation key fingerprint")?;
        Self::record_path(
            &self.turn_operations_dir,
            &format!("op_{operation_key_fingerprint}"),
            "json",
            "turn operation binding id",
        )
    }

    fn history_operation_path(&self, fingerprint: &str) -> Result<PathBuf> {
        validate_sha256_fingerprint(fingerprint, "history operation fingerprint")?;
        Self::record_path(
            &self.turn_operations_dir,
            &format!("history_{fingerprint}"),
            "json",
            "history operation",
        )
    }

    fn thread_create_operation_path(&self, fingerprint: &str) -> Result<PathBuf> {
        validate_sha256_fingerprint(fingerprint, "thread creation operation fingerprint")?;
        Self::record_path(
            &self.turn_operations_dir,
            &format!("thread_create_{fingerprint}"),
            "json",
            "thread creation operation",
        )
    }

    fn load_history_operation(&self, fingerprint: &str) -> Result<Option<RuntimeHistoryOperation>> {
        let path = self.history_operation_path(fingerprint)?;
        let raw = match read_store_file(&path) {
            Ok(raw) => raw,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let operation: RuntimeHistoryOperation = serde_json::from_str(&raw)?;
        operation.validate()?;
        anyhow::ensure!(
            operation.key_fingerprint == fingerprint,
            "history operation filename does not match its binding"
        );
        Ok(Some(operation))
    }

    fn save_history_operation(&self, operation: &RuntimeHistoryOperation) -> Result<()> {
        operation.validate()?;
        write_json_atomic(
            &self.history_operation_path(&operation.key_fingerprint)?,
            operation,
        )
    }

    fn turn_operation_lock_path(&self, operation_key_fingerprint: &str) -> Result<PathBuf> {
        validate_sha256_fingerprint(operation_key_fingerprint, "operation key fingerprint")?;
        Self::record_path(
            &self.turn_operations_dir,
            &format!("op_{operation_key_fingerprint}"),
            "lock",
            "turn operation claim lock id",
        )
    }

    fn open_turn_operation_claim_lock(&self, operation_key_fingerprint: &str) -> Result<File> {
        let path = self.turn_operation_lock_path(operation_key_fingerprint)?;
        open_runtime_store_file(&path, "Runtime turn operation claim lock", |options| {
            options.create(true).truncate(false).read(true).write(true);
        })
    }

    fn with_turn_operation_claim<T>(
        &self,
        operation_key_fingerprint: Option<&str>,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let Some(operation_key_fingerprint) = operation_key_fingerprint else {
            return operation();
        };
        let mut claim =
            fd_lock::RwLock::new(self.open_turn_operation_claim_lock(operation_key_fingerprint)?);
        let _guard = self.acquire_turn_operation_claim(&mut claim)?;
        operation()
    }

    fn acquire_turn_operation_claim<'a>(
        &self,
        claim: &'a mut fd_lock::RwLock<File>,
    ) -> Result<fd_lock::RwLockWriteGuard<'a, File>> {
        match claim.try_write() {
            Ok(guard) => Ok(guard),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                bail!("Runtime turn operation is already being claimed; retry")
            }
            Err(error) => Err(error).context("Failed to claim Runtime turn operation"),
        }
    }

    /// Remove a binding left before its turn record by a process crash.
    ///
    /// Bindings are committed before turns, while engine submission happens
    /// only after both are durable. A binding with no turn therefore never
    /// reached the engine and is safe to discard during startup recovery.
    fn recover_incomplete_turn_operations(&self) -> Result<()> {
        let operations_dir = checked_existing_runtime_store_dir(&self.turn_operations_dir)?;
        for entry in fs::read_dir(&operations_dir)
            .with_context(|| format!("Failed to read {}", operations_dir.display()))?
        {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let raw = read_store_file(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            if path
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("thread_create_"))
            {
                let binding: RuntimeThreadCreateBinding = serde_json::from_str(&raw)?;
                binding.validate()?;
                anyhow::ensure!(
                    path == self
                        .thread_create_operation_path(&binding.operation_key_fingerprint)?,
                    "thread creation operation path mismatch"
                );
                continue;
            }
            if path
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("history_"))
            {
                let history: RuntimeHistoryOperation = serde_json::from_str(&raw)?;
                history.validate()?;
                anyhow::ensure!(
                    path == self.history_operation_path(&history.key_fingerprint)?,
                    "history operation path mismatch"
                );
                // Keep both reserved and committed work: a retry observes this
                // exact identity instead of replaying an uncertain creation.
                continue;
            }
            let observed: RuntimeTurnOperationBinding = serde_json::from_str(&raw)
                .with_context(|| format!("Failed to parse {}", path.display()))?;
            observed.validate()?;
            self.with_turn_operation_claim(Some(&observed.operation_key_fingerprint), || {
                // A live writer may have replaced the file between the
                // directory scan and this claim. Re-read under the same
                // cross-process lock used by `start_turn` before deciding
                // that the binding is torn.
                let Some(binding) =
                    self.load_turn_operation_binding(&observed.operation_key_fingerprint)?
                else {
                    return Ok(());
                };
                if !self.turn_path(&binding.turn_id)?.exists() {
                    // Persistence is binding -> item -> turn. A process can
                    // stop after the item write but before the turn commit;
                    // that item was never submitted to an engine and has no
                    // authoritative parent. Remove it under the same operation
                    // claim before making the key retryable.
                    for item in self.list_items_for_turn(&binding.turn_id)? {
                        self.remove_item(&item.id)?;
                    }
                    remove_file_if_exists(&path)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }

    fn save_turn_operation_binding(&self, binding: &RuntimeTurnOperationBinding) -> Result<()> {
        binding.validate()?;
        write_json_atomic(
            &self.turn_operation_path(&binding.operation_key_fingerprint)?,
            binding,
        )
    }

    fn load_turn_operation_binding(
        &self,
        operation_key_fingerprint: &str,
    ) -> Result<Option<RuntimeTurnOperationBinding>> {
        let path = self.turn_operation_path(operation_key_fingerprint)?;
        let raw = match read_store_file(&path) {
            Ok(raw) => raw,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Failed to read Runtime turn operation {}", path.display())
                });
            }
        };
        let binding: RuntimeTurnOperationBinding =
            serde_json::from_str(&raw).with_context(|| {
                format!("Failed to parse Runtime turn operation {}", path.display())
            })?;
        binding.validate()?;
        Ok(Some(binding))
    }

    fn remove_turn_operation_binding(&self, operation_key_fingerprint: &str) -> Result<()> {
        remove_file_if_exists(&self.turn_operation_path(operation_key_fingerprint)?)
    }

    fn recover_claimed_agent_mail(&self) -> Result<()> {
        let _mail_mutation = self.mail_mutation.lock();
        for mut mail in self.list_agent_mail()? {
            if mail.status != AgentMailStatus::Delivering {
                continue;
            }
            mail.status = AgentMailStatus::Failed;
            mail.failure = Some(AgentMailFailureReceipt {
                code: AgentMailFailureCode::DeliveryRejected,
                message: "Delivery claim recovered after runtime restart".to_string(),
                retryable: true,
                failed_at: Utc::now(),
            });
            self.save_agent_mail(&mail)?;
        }
        Ok(())
    }

    fn save_agent_mail(&self, mail: &AgentMailEnvelope) -> Result<()> {
        mail.validate().map_err(|error| anyhow!(error))?;
        write_json_atomic(&self.mail_path(&mail.message_id)?, mail)
    }

    fn load_agent_mail(&self, message_id: &AgentMailMessageId) -> Result<AgentMailEnvelope> {
        let path = self.mail_path(message_id)?;
        let raw = read_store_file(&path)
            .with_context(|| format!("Failed to read Agent Mail envelope {}", path.display()))?;
        let mail: AgentMailEnvelope = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse Agent Mail envelope {}", path.display()))?;
        mail.validate().map_err(|error| anyhow!(error))?;
        Ok(mail)
    }

    fn list_agent_mail(&self) -> Result<Vec<AgentMailEnvelope>> {
        let mut out = Vec::new();
        let mail_dir = checked_existing_runtime_store_dir(&self.mail_dir)?;
        for entry in fs::read_dir(&mail_dir)
            .with_context(|| format!("Failed to read {}", mail_dir.display()))?
        {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let raw = read_store_file(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            let mail: AgentMailEnvelope = serde_json::from_str(&raw)
                .with_context(|| format!("Failed to parse {}", path.display()))?;
            mail.validate().map_err(|error| anyhow!(error))?;
            out.push(mail);
        }
        out.sort_by_key(|mail| mail.created_at);
        Ok(out)
    }

    /// Persist a goal record for a thread. The goal is stored as a JSON file
    /// in the `goals/` subdirectory; it is independent of the TUI state store
    /// and requires only that the runtime thread exists.
    #[cfg(any(test, feature = "test-support"))]
    pub fn save_goal(&self, goal: &codewhale_protocol::ThreadGoal) -> Result<()> {
        let _guard = self.goal_mutation.lock();
        self.save_goal_under_lock(goal)
    }

    fn save_goal_under_lock(&self, goal: &codewhale_protocol::ThreadGoal) -> Result<()> {
        goal.validate_stall_state().map_err(anyhow::Error::msg)?;
        write_json_atomic(&self.goal_path(&goal.thread_id)?, goal)
    }

    /// Load the goal for a thread, returning `None` if no goal has been set.
    /// A corrupt record that is still Active with an exhausted stall window
    /// is restored paused: the engine pauses NoProgress in the same locked
    /// mutation that fills the window, so Active-at-ceiling can only come
    /// from an interrupted write.
    pub fn load_goal(&self, thread_id: &str) -> Result<Option<codewhale_protocol::ThreadGoal>> {
        let path = self.goal_path(thread_id)?;
        if !path.exists() {
            return Ok(None);
        }
        let raw = read_store_file(&path)
            .with_context(|| format!("Failed to read goal {}", path.display()))?;
        let mut goal: codewhale_protocol::ThreadGoal = serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse goal {}", path.display()))?;
        goal.validate_stall_state().map_err(anyhow::Error::msg)?;
        goal.normalize_restored_stall_state();
        Ok(Some(goal))
    }

    pub fn thread_exists(&self, thread_id: &str) -> bool {
        self.thread_path(thread_id)
            .map(|path| path.is_file())
            .unwrap_or(false)
    }

    pub fn list_goal_thread_ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.goals_dir)
            .with_context(|| format!("Failed to read {}", self.goals_dir.display()))?
        {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                ids.push(stem.to_string());
            }
        }
        Ok(ids)
    }

    /// A late turn can only update the revision admitted with that turn.
    /// Load/compare/write share the same guard as explicit save/delete.
    /// Mutate the goal only while it is still the revision the caller read:
    /// the same `goal_id` and, when `expected_status` is given, the same
    /// status. Returns `Ok(None)` without writing when either changed, so a
    /// transition decided against a stale read (for example Blocked computed
    /// while a concurrent Complete landed) can never overwrite the newer state.
    fn update_goal_if_revision(
        &self,
        thread_id: &str,
        goal_id: &str,
        expected_status: Option<codewhale_protocol::ThreadGoalStatus>,
        update: impl FnOnce(&mut codewhale_protocol::ThreadGoal),
    ) -> Result<Option<codewhale_protocol::ThreadGoal>> {
        let _guard = self.goal_mutation.lock();
        self.update_goal_if_revision_under_lock(thread_id, goal_id, expected_status, update)
    }

    fn update_goal_if_revision_under_lock(
        &self,
        thread_id: &str,
        goal_id: &str,
        expected_status: Option<codewhale_protocol::ThreadGoalStatus>,
        update: impl FnOnce(&mut codewhale_protocol::ThreadGoal),
    ) -> Result<Option<codewhale_protocol::ThreadGoal>> {
        let Some(mut goal) = self.load_goal(thread_id)? else {
            return Ok(None);
        };
        if goal.goal_id != goal_id
            || expected_status.is_some_and(|expected| goal.status != expected)
        {
            return Ok(None);
        }
        update(&mut goal);
        goal.validate_stall_state().map_err(anyhow::Error::msg)?;
        write_json_atomic(&self.goal_path(thread_id)?, &goal)?;
        Ok(Some(goal))
    }

    /// Persist a goal the model created mid-turn through `create_goal`, but
    /// only when the thread still has no durable goal: a concurrent explicit
    /// PUT/DELETE is the newer revision and always wins. Returns the adopted
    /// goal_id when this call created the record, `None` otherwise.
    fn create_goal_from_snapshot_if_absent(
        &self,
        thread_id: &str,
        snapshot: &crate::tools::goal::GoalSnapshot,
    ) -> Result<Option<String>> {
        let _guard = self.goal_mutation.lock();
        if !snapshot.is_active() || self.load_goal(thread_id)?.is_some() {
            return Ok(None);
        }
        let Some(objective) = snapshot
            .objective
            .as_deref()
            .map(str::trim)
            .filter(|objective| !objective.is_empty())
        else {
            return Ok(None);
        };
        let now = chrono::Utc::now().timestamp();
        let goal_id = snapshot
            .goal_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let goal = codewhale_protocol::ThreadGoal {
            thread_id: thread_id.to_string(),
            goal_id: goal_id.clone(),
            objective: objective.to_string(),
            status: codewhale_protocol::ThreadGoalStatus::Active,
            token_budget: snapshot.token_budget.map(i64::from),
            tokens_used: 0,
            time_used_seconds: 0,
            continuation_count: 0,
            created_at: now,
            updated_at: now,
            last_gap_fingerprint: None,
            repeated_gap_count: 0,
            last_gap_pass: None,
            pause_reason: None,
        };
        goal.validate_stall_state().map_err(anyhow::Error::msg)?;
        write_json_atomic(&self.goal_path(thread_id)?, &goal)?;
        Ok(Some(goal_id))
    }

    /// Remove the goal for a thread, returning `true` if one existed.
    fn delete_goal_under_lock(&self, thread_id: &str) -> Result<bool> {
        let path = self.goal_path(thread_id)?;
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_file(&path)
            .with_context(|| format!("Failed to delete goal {}", path.display()))?;
        Ok(true)
    }

    pub fn save_thread(&self, thread: &ThreadRecord) -> Result<()> {
        let path = self.thread_path(&thread.id)?;
        write_json_atomic(&path, thread).with_context(|| {
            RuntimeStoreRecordFailure::new(
                RuntimeStoreOperation::Write,
                RuntimeStoreRecordKind::Thread,
                &thread.id,
                &path,
            )
        })
    }

    pub fn save_turn(&self, turn: &TurnRecord) -> Result<()> {
        // Hold the turn mutation guard for every write — including callers that
        // already hold it (reentrant) — so unlocked test/API settles cannot be
        // overwritten by a monitor RMW that loaded an older in-flight snapshot.
        let _turn_mutation = self.turn_mutation.lock();
        turn.validate_output_token_limit()?;
        validated_record_id(&turn.thread_id, "thread id")?;
        let path = self.turn_path(&turn.id)?;
        let mut stored = serde_json::to_value(turn)?;
        if let Some(checkpoint) = &turn.prefix_checkpoint {
            stored["prefix_checkpoint"] = serde_json::to_value(checkpoint)?;
        }
        write_json_atomic(&path, &stored).with_context(|| {
            RuntimeStoreRecordFailure::new(
                RuntimeStoreOperation::Write,
                RuntimeStoreRecordKind::Turn,
                &turn.id,
                &path,
            )
        })
    }

    pub fn save_item(&self, item: &TurnItemRecord) -> Result<()> {
        validated_record_id(&item.turn_id, "turn id")?;
        let path = self.item_path(&item.id)?;
        write_json_atomic(&path, item).with_context(|| {
            RuntimeStoreRecordFailure::new(
                RuntimeStoreOperation::Write,
                RuntimeStoreRecordKind::Item,
                &item.id,
                &path,
            )
        })?;
        self.note_item_in_index(&item.turn_id, &item.id);
        Ok(())
    }

    /// Publish many items at once, paying the items directory's costs once.
    ///
    /// Each `save_item` fsyncs the items directory — right for one record,
    /// ruinous for the hundreds a fork clones. The stale-temp sweep is
    /// throttled per directory inside `write_atomic`; unthrottled it cost 22 ms
    /// of directory scan per item, 34 s for one fork's 771. The per-record
    /// checks and the atomic replace are unchanged; see
    /// [`codewhale_util::utils::write_atomic_batch`].
    ///
    /// Ordering stays the caller's: items, then their turns, then the thread
    /// record that makes them reachable.
    pub fn save_items_batch(&self, items: &[&TurnItemRecord]) -> Result<()> {
        let mut files = Vec::with_capacity(items.len());
        for item in items {
            validated_record_id(&item.turn_id, "turn id")?;
            let path = self.item_path(&item.id)?;
            reject_symlinked_store_file(&path)?;
            let payload = serde_json::to_string_pretty(item)?;
            files.push((path, payload.into_bytes()));
        }
        codewhale_util::utils::write_atomic_batch(&files)
            .with_context(|| format!("Failed to write {} store items", files.len()))?;
        for item in items {
            self.note_item_in_index(&item.turn_id, &item.id);
        }
        Ok(())
    }

    /// The engine history a compacting turn settled on, stored once under
    /// its digest beside the items (listings read only `.json`). Forks share
    /// it by reference, so it is never removed.
    fn settled_history_path(&self, digest: &str) -> Result<PathBuf> {
        Self::record_path(&self.items_dir, digest, "history", "settled history digest")
    }

    pub fn save_settled_history(&self, messages: &[Message]) -> Result<Value> {
        let bytes = serde_json::to_vec(messages)?;
        let digest = hex_digest(Sha256::digest(&bytes));
        let path = self.settled_history_path(&digest)?;
        reject_symlinked_store_file(&path)?;
        codewhale_util::utils::write_atomic(&path, &bytes)
            .with_context(|| format!("Failed to write settled history {}", path.display()))?;
        Ok(json!({ "sha256": digest, "messages": messages.len() }))
    }

    /// The newest settled history among `turns`, and how many of them it
    /// covers. A reference whose file is missing, altered or unreadable is an
    /// error: replaying the raw turns instead would silently undo compaction.
    fn settled_history(
        &self,
        turns: &[TurnRecord],
        items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
    ) -> Result<Option<(usize, Vec<Message>)>> {
        for (index, turn) in turns.iter().enumerate().rev() {
            let Some((item, reference)) = items_by_turn
                .get(&turn.id)
                .into_iter()
                .flatten()
                .rev()
                .filter(|item| item.kind == TurnItemKind::ContextCompaction)
                .find_map(|item| {
                    let reference = item.metadata.as_ref()?.get("settled_history")?;
                    Some((item, reference))
                })
            else {
                continue;
            };
            let digest = reference
                .get("sha256")
                .and_then(Value::as_str)
                .filter(|digest| {
                    digest.len() == 64
                        && digest
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .with_context(|| {
                    format!(
                        "Compaction item {} on turn {} names an invalid settled history",
                        item.id, turn.id
                    )
                })?;
            let expected = reference
                .get("messages")
                .and_then(Value::as_u64)
                .and_then(|count| usize::try_from(count).ok())
                .with_context(|| {
                    format!(
                        "Compaction item {} on turn {} names no settled history length",
                        item.id, turn.id
                    )
                })?;
            let path = self.settled_history_path(digest)?;
            reject_symlinked_store_file(&path)?;
            let bytes = fs::read(&path).with_context(|| {
                format!(
                    "Settled history for turn {} is unreadable: {}",
                    turn.id,
                    path.display()
                )
            })?;
            anyhow::ensure!(
                hex_digest(Sha256::digest(&bytes)) == digest,
                "Settled history {} for turn {} does not match its digest",
                path.display(),
                turn.id
            );
            let messages: Vec<Message> = serde_json::from_slice(&bytes)
                .with_context(|| format!("Failed to parse settled history {}", path.display()))?;
            anyhow::ensure!(
                messages.len() == expected,
                "Settled history {} for turn {} holds {} messages, not {expected}",
                path.display(),
                turn.id,
                messages.len()
            );
            return Ok(Some((index + 1, messages)));
        }
        Ok(None)
    }

    fn load_prefix_history(&self, checkpoint: &RuntimePrefixCheckpoint) -> Result<Vec<Message>> {
        anyhow::ensure!(
            checkpoint.persisted_sha256.len() == 64
                && checkpoint
                    .persisted_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                && checkpoint.projection == "runtime_receipt_redacted",
            "Invalid Runtime prefix checkpoint reference"
        );
        let path = self.settled_history_path(&checkpoint.persisted_sha256)?;
        reject_symlinked_store_file(&path)?;
        let bytes = fs::read(&path).context("Runtime prefix checkpoint is unreadable")?;
        anyhow::ensure!(
            hex_digest(Sha256::digest(&bytes)) == checkpoint.persisted_sha256,
            "Runtime prefix checkpoint digest mismatch"
        );
        let messages: Vec<Message> = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            messages.len() == checkpoint.messages_len,
            "Runtime prefix checkpoint length mismatch"
        );
        Ok(messages)
    }

    fn checkpoint_tail(
        &self,
        turn: &TurnRecord,
        items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
        messages: &[Message],
    ) -> Result<HashMap<String, Vec<TurnItemRecord>>> {
        let checkpoint = turn
            .prefix_checkpoint
            .as_ref()
            .context("Missing Runtime prefix checkpoint")?;
        anyhow::ensure!(
            checkpoint.session_id == turn.thread_id
                && checkpoint.runtime_turn_id == turn.id
                && checkpoint.agent_id.is_none()
                && checkpoint.owner_id == self.owner_id
                && !checkpoint.engine_turn_id.is_empty()
                && !checkpoint.call_id.is_empty()
                && !checkpoint.checkpoint_id.is_empty()
                && checkpoint.source_sha256.len() == 64
                && checkpoint
                    .source_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "Runtime prefix checkpoint owner mismatch"
        );
        anyhow::ensure!(
            turn.item_ids.starts_with(&checkpoint.item_ids)
                && checkpoint.item_ids.iter().collect::<HashSet<_>>().len()
                    == checkpoint.item_ids.len()
                && checkpoint
                    .covered_items
                    .keys()
                    .all(|id| checkpoint.item_ids.contains(id))
                && checkpoint
                    .pending_items
                    .iter()
                    .all(|id| checkpoint.item_ids.contains(id)
                        && !checkpoint.covered_items.contains_key(id)),
            "Runtime prefix checkpoint item cursor changed"
        );
        let mut calls = HashMap::<(Option<String>, String), ContentBlock>::new();
        let mut results = HashMap::<(Option<String>, String), ContentBlock>::new();
        let mut call_order = Vec::new();
        let mut execution_providers = HashMap::new();
        for block in messages.iter().flat_map(|message| &message.content) {
            match block {
                ContentBlock::ToolUse {
                    execution_id, id, ..
                } => {
                    check_execution_provider(
                        &mut execution_providers,
                        execution_id.as_deref(),
                        id,
                    )?;
                    let key = (execution_id.clone(), id.clone());
                    anyhow::ensure!(
                        !calls.contains_key(&key) || results.contains_key(&key),
                        "Checkpoint contains duplicate open tool identity"
                    );
                    results.remove(&key);
                    call_order.retain(|previous| previous != &key);
                    call_order.push(key.clone());
                    calls.insert(key, block.clone());
                }
                ContentBlock::ToolResult {
                    execution_id,
                    tool_use_id,
                    ..
                } => {
                    check_execution_provider(
                        &mut execution_providers,
                        execution_id.as_deref(),
                        tool_use_id,
                    )?;
                    let key = (execution_id.clone(), tool_use_id.clone());
                    anyhow::ensure!(
                        calls.contains_key(&key) && results.insert(key, block.clone()).is_none(),
                        "Checkpoint contains an unpaired tool result"
                    );
                }
                _ => {}
            }
        }
        anyhow::ensure!(
            calls.len() == checkpoint.tool_declarations.len()
                && calls
                    .keys()
                    .all(
                        |key| serde_json::to_string(key).ok().is_some_and(|key| checkpoint
                            .tool_declarations
                            .get(&key)
                            .is_some_and(|digest| digest.len() == 64
                                && digest.bytes().all(|byte| byte.is_ascii_hexdigit())))
                    ),
            "Runtime prefix checkpoint tool declarations are incomplete"
        );
        let items = ordered_turn_items(turn, items_by_turn)?;
        let present: HashSet<_> = items.iter().map(|item| item.id.as_str()).collect();
        anyhow::ensure!(
            checkpoint
                .item_ids
                .iter()
                .all(|id| present.contains(id.as_str())),
            "Runtime prefix checkpoint item is missing"
        );
        let mut retained = Vec::new();
        let mut answered = HashSet::new();
        for mut item in items {
            if let Some(expected) = checkpoint.covered_items.get(&item.id) {
                anyhow::ensure!(
                    checkpoint_item_digest(&item)? == *expected
                        && (projects_tool_history(&item)
                            || !matches!(
                                item.status,
                                TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::Canceled
                            )),
                    "Runtime prefix checkpoint covered item changed"
                );
                if !projects_tool_history(&item) {
                    continue;
                }
            }
            anyhow::ensure!(
                !checkpoint.item_ids.contains(&item.id)
                    || checkpoint.pending_items.contains(&item.id)
                    || !matches!(
                        item.kind,
                        TurnItemKind::UserMessage
                            | TurnItemKind::AgentMessage
                            | TurnItemKind::AgentReasoning
                    )
                    || matches!(
                        item.status,
                        TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::Canceled
                    ),
                "Runtime prefix checkpoint covered item reference is missing"
            );
            if projects_tool_history(&item) {
                let identity =
                    RuntimeToolIdentity::read(item.metadata.as_ref(), "tool_use_id")?.or(
                        RuntimeToolIdentity::read(item.metadata.as_ref(), "tool_result_for")?,
                    );
                if let Some(identity) = identity {
                    check_execution_provider(
                        &mut execution_providers,
                        identity.execution_id,
                        identity.provider_id,
                    )?;
                    let key = (
                        identity.execution_id.map(str::to_owned),
                        identity.provider_id.to_owned(),
                    );
                    if calls.contains_key(&key) {
                        let declaration = checkpoint
                            .tool_declarations
                            .get(&serde_json::to_string(&key)?)
                            .context("Runtime prefix tool declaration reference is missing")?;
                        let mut isolated = turn.clone();
                        isolated.item_ids = vec![item.id.clone()];
                        let projected = reconstruct_messages_from_turns_with(
                            &[isolated],
                            &HashMap::from([(turn.id.clone(), vec![item.clone()])]),
                        )?;
                        let mut outcome = None;
                        for block in projected.into_iter().flat_map(|message| message.content) {
                            match &block {
                                ContentBlock::ToolUse { .. } => anyhow::ensure!(
                                    hex_digest(Sha256::digest(serde_json::to_vec(&block)?))
                                        == *declaration,
                                    "Runtime prefix tool declaration changed"
                                ),
                                ContentBlock::ToolResult { .. } => outcome = Some(block),
                                _ => {}
                            }
                        }
                        if results.contains_key(&key) {
                            anyhow::ensure!(
                                checkpoint.covered_items.contains_key(&item.id),
                                "Runtime prefix completed tool receipt is not bound to its cursor"
                            );
                            continue;
                        }
                        if let Some(outcome) = outcome {
                            anyhow::ensure!(
                                answered.insert(key.clone()),
                                "Duplicate Runtime prefix tool result"
                            );
                            let ContentBlock::ToolResult {
                                content,
                                is_error,
                                content_blocks,
                                ..
                            } = outcome
                            else {
                                unreachable!()
                            };
                            let metadata = item
                                .metadata
                                .as_mut()
                                .and_then(Value::as_object_mut)
                                .context("Invalid Runtime prefix tool metadata")?;
                            metadata.remove("tool_use_id");
                            metadata.insert(
                                "tool_result_for".into(),
                                json!(key.0.as_deref().unwrap_or(&key.1)),
                            );
                            metadata.insert("is_error".into(), json!(is_error.unwrap_or(false)));
                            if let Some(content_blocks) = content_blocks {
                                metadata.insert("content_blocks".into(), json!(content_blocks));
                            }
                            item.detail = Some(content);
                            retained.push(item);
                        }
                        continue;
                    }
                    anyhow::ensure!(
                        !checkpoint.item_ids.contains(&item.id),
                        "Runtime prefix does not contain a covered tool declaration"
                    );
                }
            }
            retained.push(item);
        }
        if !turn.status.is_active_work() {
            let mut missing = Vec::new();
            for key in call_order
                .iter()
                .filter(|key| !results.contains_key(*key) && !answered.contains(*key))
            {
                let id = format!("checkpoint_result_{}", missing.len());
                missing.push(TurnItemRecord {
                    schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                    id,
                    turn_id: turn.id.clone(),
                    kind: TurnItemKind::ToolCall,
                    status: TurnItemLifecycleStatus::Interrupted,
                    summary: String::new(),
                    detail: Some(
                        codewhale_runtime::tool_history_repair::CRASH_REPAIR_CONTENT.to_string(),
                    ),
                    metadata: Some(match &key.0 {
                        Some(execution) => {
                            json!({"tool_result_for": execution, "execution_id": execution,
                            "provider_tool_use_id": key.1, "is_error": true})
                        }
                        None => json!({"tool_result_for": key.1, "is_error": true}),
                    }),
                    artifact_refs: Vec::new(),
                    artifacts: Vec::new(),
                    started_at: None,
                    ended_at: None,
                });
            }
            let boundary = retained
                .iter()
                .position(|item| {
                    matches!(
                        item.kind,
                        TurnItemKind::UserMessage
                            | TurnItemKind::AgentMessage
                            | TurnItemKind::AgentReasoning
                    )
                })
                .unwrap_or(retained.len());
            retained.splice(boundary..boundary, missing);
        }
        let mut tail = items_by_turn.clone();
        tail.insert(turn.id.clone(), retained);
        Ok(tail)
    }

    fn history_with_prefix(
        &self,
        turns: &[TurnRecord],
        items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
        mut messages: Vec<Message>,
        mut covered: usize,
    ) -> Result<Vec<Message>> {
        if let Some((settled, history)) = self.settled_history(&turns[covered..], items_by_turn)? {
            covered += settled;
            messages = history;
        }
        if let Some((index, turn)) = turns
            .iter()
            .enumerate()
            .skip(covered)
            .rev()
            .find(|(_, turn)| turn.prefix_checkpoint.is_some())
        {
            messages = self.load_prefix_history(turn.prefix_checkpoint.as_ref().unwrap())?;
            let tail = self.checkpoint_tail(turn, items_by_turn, &messages)?;
            let mut remaining = turns[index..].to_vec();
            remaining[0].item_ids = tail
                .get(&turn.id)
                .into_iter()
                .flatten()
                .map(|item| item.id.clone())
                .collect();
            messages.extend(reconstruct_messages_from_turns_with(&remaining, &tail)?);
        } else {
            messages.extend(reconstruct_messages_from_turns_with(
                &turns[covered..],
                items_by_turn,
            )?);
        }
        Ok(messages)
    }

    /// The model-visible history the records hold: the newest canonical
    /// checkpoint, then later item outcomes and turns.
    pub fn history_from_records(
        &self,
        turns: &[TurnRecord],
        items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
    ) -> Result<Vec<Message>> {
        self.history_with_prefix(turns, items_by_turn, Vec::new(), 0)
    }

    /// Journal for one history seed into an existing thread (#6555), kept
    /// beside the thread record as `<thread>.seed` (listings read only
    /// `.json`). It names every record the seed will write before the first
    /// one lands; the thread record — the seed's commit record — is written
    /// after them, and the journal is removed last.
    fn seed_journal_path(&self, thread_id: &str) -> Result<PathBuf> {
        Self::record_path(&self.threads_dir, thread_id, "seed", "thread id")
    }

    fn save_seed_journal(&self, journal: &SeedJournal) -> Result<()> {
        let path = self.seed_journal_path(&journal.thread_id)?;
        reject_symlinked_store_file(&path)?;
        write_json_atomic(&path, journal)
            .with_context(|| format!("Failed to write seed journal {}", path.display()))
    }

    fn remove_seed_journal(&self, thread_id: &str) -> Result<()> {
        remove_file_if_exists(&self.seed_journal_path(thread_id)?)
    }

    /// Remove every record an uncommitted seed named, then its journal. Ids in
    /// a journal were minted fresh for that seed, so nothing else is touched.
    fn discard_seed(&self, journal: &SeedJournal) -> Result<()> {
        // Validate the whole deletion set first. A damaged journal must not
        // retire records from another thread, even when some earlier records
        // in its list were legitimate members of this seed.
        for turn_id in &journal.turn_ids {
            match self.load_turn(turn_id) {
                Ok(turn) if turn.id == *turn_id && turn.thread_id == journal.thread_id => {}
                Ok(_) => anyhow::bail!("seed journal names a turn outside its thread"),
                Err(error) if store_record_missing(&error) => {}
                Err(error) => {
                    return Err(error).context("cannot validate seed turn before cleanup");
                }
            }
        }
        for item_id in &journal.item_ids {
            match self.load_item(item_id) {
                Ok(item) if item.id == *item_id && journal.turn_ids.contains(&item.turn_id) => {}
                Ok(_) => anyhow::bail!("seed journal names an item outside its turns"),
                Err(error) if store_record_missing(&error) => {}
                Err(error) => {
                    return Err(error).context("cannot validate seed item before cleanup");
                }
            }
        }
        for turn_id in &journal.turn_ids {
            self.remove_turn(turn_id)?;
        }
        for item_id in &journal.item_ids {
            self.remove_item(item_id)?;
        }
        self.remove_seed_journal(&journal.thread_id)
    }

    /// Startup half of the seed transaction: a journal whose thread pointer
    /// still matches the one it recorded is an unpublished partial seed and
    /// is discarded, so recovery can never restore it as history. A journal
    /// whose seed provably committed is simply removed.
    ///
    /// Uncertainty is per thread, like [`Self::list_threads_lenient`]
    /// (#6144 P7): a journal whose intent or ownership cannot be proven
    /// quarantines only its thread. Nothing of that thread is deleted or
    /// rewritten, the journal stays for repair, and the returned ids are held
    /// out of recovery so their pointers are not recomputed over a seed that
    /// may be partial. Every other thread in the store recovers normally.
    ///
    /// Known limitation: a quarantined thread stays exactly as recorded until
    /// its journal is repaired or removed by hand — including a turn that was
    /// in flight when the process stopped, which is not terminalized.
    /// Listings and thread detail still read its records as stored.
    fn settle_seed_journals(&self) -> Result<HashSet<String>> {
        let mut quarantined = HashSet::new();
        for path in self.seed_journal_paths()? {
            match self.judge_seed_journal(&path) {
                SeedJournalVerdict::Committed { thread_id } => {
                    if let Err(error) = self.remove_seed_journal(&thread_id) {
                        // History is committed either way; the next start
                        // judges the journal again.
                        tracing::warn!(
                            target: "runtime",
                            thread_id = %thread_id,
                            "committed seed journal not removed: {error:#}"
                        );
                    }
                }
                SeedJournalVerdict::Uncommitted(journal) => {
                    if let Err(error) = self.discard_seed(&journal) {
                        tracing::error!(
                            target: "runtime",
                            thread_id = %journal.thread_id,
                            journal = %path.display(),
                            "partial seed not discarded; thread held out of recovery and its \
                             journal kept for repair: {error:#}"
                        );
                        quarantined.insert(journal.thread_id);
                    }
                }
                SeedJournalVerdict::Quarantined { thread_ids, reason } => {
                    tracing::error!(
                        target: "runtime",
                        thread_ids = ?thread_ids,
                        journal = %path.display(),
                        "seed journal cannot be settled ({reason}); thread held out of recovery \
                         and every record preserved for repair"
                    );
                    quarantined.extend(thread_ids);
                }
            }
        }
        Ok(quarantined)
    }

    /// Every `<thread>.seed` journal in the store.
    fn seed_journal_paths(&self) -> Result<Vec<PathBuf>> {
        let threads_dir = checked_existing_runtime_store_dir(&self.threads_dir)?;
        let mut paths = Vec::new();
        for entry in fs::read_dir(&threads_dir)
            .with_context(|| format!("Failed to read {}", threads_dir.display()))?
        {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "seed") {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    /// What one seed journal proves, without changing anything. Both the
    /// startup settle and the read-only reconcile reader
    /// ([`HeldRuntimeStore::recoverable_threads`]) judge journals here, so
    /// they agree on what counts as history.
    fn judge_seed_journal(&self, path: &Path) -> SeedJournalVerdict {
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let journal = match read_store_file(path).and_then(|raw| {
            serde_json::from_str::<SeedJournal>(&raw)
                .with_context(|| format!("Failed to parse {}", path.display()))
        }) {
            Ok(journal) => journal,
            Err(error) => {
                // Without readable intent an unpublished seed cannot be told
                // from accepted history.
                return SeedJournalVerdict::Quarantined {
                    thread_ids: vec![stem],
                    reason: format!("journal unreadable: {error:#}"),
                };
            }
        };
        let owned = self
            .seed_journal_path(&journal.thread_id)
            .is_ok_and(|expected| expected.file_name() == path.file_name());
        if !owned {
            return SeedJournalVerdict::Quarantined {
                thread_ids: vec![stem, journal.thread_id],
                reason: "journal names another thread".to_string(),
            };
        }
        // A seed that names no records has nothing to publish or withhold.
        if journal.turn_ids.is_empty() && journal.item_ids.is_empty() {
            return SeedJournalVerdict::Uncommitted(journal);
        }
        let thread = match self.load_thread(&journal.thread_id) {
            Ok(thread) => thread,
            // The thread is gone; nothing can publish its seed records.
            Err(error) if store_record_missing(&error) => {
                return SeedJournalVerdict::Uncommitted(journal);
            }
            Err(error) => {
                return SeedJournalVerdict::Quarantined {
                    thread_ids: vec![journal.thread_id],
                    reason: format!("thread record unreadable: {error:#}"),
                };
            }
        };
        if thread.latest_turn_id == journal.previous_latest_turn_id {
            return SeedJournalVerdict::Uncommitted(journal);
        }
        if journal.turn_ids.last() == thread.latest_turn_id.as_ref() {
            return SeedJournalVerdict::Committed {
                thread_id: journal.thread_id,
            };
        }
        match self.seed_complete_beneath_later_turn(&journal, &thread) {
            Ok(true) => SeedJournalVerdict::Committed {
                thread_id: journal.thread_id,
            },
            Ok(false) => SeedJournalVerdict::Quarantined {
                thread_ids: vec![journal.thread_id],
                reason: "thread pointer names neither the pre-seed turn nor the seed, and the \
                         complete seed is not provably beneath it"
                    .to_string(),
            },
            Err(error) => SeedJournalVerdict::Quarantined {
                thread_ids: vec![journal.thread_id],
                reason: format!("seed records unreadable: {error:#}"),
            },
        }
    }

    /// True when a journal was left behind by a seed that committed and the
    /// thread then moved on: the journal removal after the commit failed and
    /// later turns advanced the pointer. `thread_mutation` serializes seeding
    /// against every other pointer move, so a pointer that names a turn of
    /// this thread created after the seed's last turn, while every record the
    /// seed journaled is still present and still its own, sits above the
    /// complete seed. A seed with a missing or foreign record is never
    /// proven here — that thread is quarantined instead.
    fn seed_complete_beneath_later_turn(
        &self,
        journal: &SeedJournal,
        thread: &ThreadRecord,
    ) -> Result<bool> {
        let Some(pointer) = thread.latest_turn_id.as_deref() else {
            return Ok(false);
        };
        if journal.turn_ids.iter().any(|turn_id| turn_id == pointer) {
            return Ok(false);
        }
        let later = match self.load_turn(pointer) {
            Ok(turn) => turn,
            Err(error) if store_record_missing(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        if later.id != pointer || later.thread_id != journal.thread_id {
            return Ok(false);
        }
        let mut last_seeded_at = None;
        for turn_id in &journal.turn_ids {
            let turn = match self.load_turn(turn_id) {
                Ok(turn) => turn,
                Err(error) if store_record_missing(&error) => return Ok(false),
                Err(error) => return Err(error),
            };
            if turn.id != *turn_id || turn.thread_id != journal.thread_id {
                return Ok(false);
            }
            last_seeded_at = Some(turn.created_at);
        }
        for item_id in &journal.item_ids {
            let item = match self.load_item(item_id) {
                Ok(item) => item,
                Err(error) if store_record_missing(&error) => return Ok(false),
                Err(error) => return Err(error),
            };
            if item.id != *item_id || !journal.turn_ids.contains(&item.turn_id) {
                return Ok(false);
            }
        }
        Ok(last_seeded_at.is_some_and(|seeded_at| later.created_at > seeded_at))
    }

    fn remove_turn(&self, turn_id: &str) -> Result<()> {
        remove_file_if_exists(&self.turn_path(turn_id)?)
    }

    fn remove_thread(&self, thread_id: &str) -> Result<()> {
        remove_file_if_exists(&self.thread_path(thread_id)?)
    }

    fn remove_item(&self, item_id: &str) -> Result<()> {
        remove_file_if_exists(&self.item_path(item_id)?)
    }

    pub fn load_thread(&self, thread_id: &str) -> Result<ThreadRecord> {
        let path = self.thread_path(thread_id)?;
        let raw = read_store_file(&path).with_context(|| {
            RuntimeStoreRecordFailure::new(
                RuntimeStoreOperation::Read,
                RuntimeStoreRecordKind::Thread,
                thread_id,
                &path,
            )
        })?;
        let record: ThreadRecord = serde_json::from_str(&raw).with_context(|| {
            RuntimeStoreRecordFailure::new(
                RuntimeStoreOperation::Parse,
                RuntimeStoreRecordKind::Thread,
                thread_id,
                &path,
            )
        })?;
        if record.schema_version > MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION {
            bail!(
                "Thread schema v{} is newer than supported v{}",
                record.schema_version,
                MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION
            );
        }
        Ok(record)
    }

    pub fn load_turn(&self, turn_id: &str) -> Result<TurnRecord> {
        let path = self.turn_path(turn_id)?;
        let raw = read_store_file(&path).with_context(|| {
            RuntimeStoreRecordFailure::new(
                RuntimeStoreOperation::Read,
                RuntimeStoreRecordKind::Turn,
                turn_id,
                &path,
            )
        })?;
        let record: TurnRecord = serde_json::from_str(&raw).with_context(|| {
            RuntimeStoreRecordFailure::new(
                RuntimeStoreOperation::Parse,
                RuntimeStoreRecordKind::Turn,
                turn_id,
                &path,
            )
        })?;
        record.validate_output_token_limit()?;
        Ok(record)
    }

    pub fn load_item(&self, item_id: &str) -> Result<TurnItemRecord> {
        let path = self.item_path(item_id)?;
        let raw = read_store_file(&path).with_context(|| {
            RuntimeStoreRecordFailure::new(
                RuntimeStoreOperation::Read,
                RuntimeStoreRecordKind::Item,
                item_id,
                &path,
            )
        })?;
        let record: TurnItemRecord = serde_json::from_str(&raw).with_context(|| {
            RuntimeStoreRecordFailure::new(
                RuntimeStoreOperation::Parse,
                RuntimeStoreRecordKind::Item,
                item_id,
                &path,
            )
        })?;
        if record.schema_version > MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION {
            bail!(
                "Item schema v{} is newer than supported v{}",
                record.schema_version,
                MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION
            );
        }
        if matches!(
            record.schema_version,
            IMAGE_RUNTIME_SCHEMA_VERSION | OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION
        ) && record.kind == TurnItemKind::UserMessage
        {
            record.user_content()?;
        }
        Ok(record)
    }

    pub fn list_threads(&self) -> Result<Vec<ThreadRecord>> {
        let (threads, skipped) = self.list_threads_lenient()?;
        for skipped in skipped {
            tracing::warn!(target: "runtime", "skipped an unreadable thread record: {skipped}");
        }
        Ok(threads)
    }

    /// Every readable thread, plus a description of each record that could
    /// not be read. One corrupt record used to fail the whole thread rail
    /// (#6144 P7); it is now skipped and reported instead. A record from a
    /// newer schema is still refused outright — skipping it would hide work
    /// a newer build owns.
    pub fn list_threads_lenient(&self) -> Result<(Vec<ThreadRecord>, Vec<String>)> {
        let mut out = Vec::new();
        let mut skipped = Vec::new();
        let threads_dir = checked_existing_runtime_store_dir(&self.threads_dir)?;
        for entry in fs::read_dir(&threads_dir)
            .with_context(|| format!("Failed to read {}", threads_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let thread: ThreadRecord = match read_store_file(&path)
                .with_context(|| format!("Failed to read {}", path.display()))
                .and_then(|raw| {
                    serde_json::from_str(&raw)
                        .with_context(|| format!("Failed to parse {}", path.display()))
                }) {
                Ok(thread) => thread,
                Err(error) => {
                    skipped.push(format!("{error:#}"));
                    continue;
                }
            };
            if thread.schema_version > MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION {
                bail!(
                    "Thread schema v{} is newer than supported v{}",
                    thread.schema_version,
                    MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION
                );
            }
            out.push(thread);
        }
        out.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
        Ok((out, skipped))
    }

    pub fn list_turns_for_thread(&self, thread_id: &str) -> Result<Vec<TurnRecord>> {
        validated_record_id(thread_id, "thread id")?;
        let mut out = self.list_all_turns()?;
        out.retain(|turn| turn.thread_id == thread_id);
        Ok(out)
    }

    /// Every turn in the store, sorted by creation time. One directory scan;
    /// callers that need multiple threads' turns (boot recovery) use this
    /// instead of paying a full scan per thread (#3757).
    pub fn list_all_turns(&self) -> Result<Vec<TurnRecord>> {
        let mut out = Vec::new();
        let turns_dir = checked_existing_runtime_store_dir(&self.turns_dir)?;
        for entry in fs::read_dir(&turns_dir)
            .with_context(|| format!("Failed to read {}", turns_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let raw = read_store_file(&path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            #[cfg(any(test, feature = "test-support"))]
            self.turn_dir_files_read
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let turn: TurnRecord = serde_json::from_str(&raw)
                .with_context(|| format!("Failed to parse {}", path.display()))?;
            turn.validate_output_token_limit()?;
            out.push(turn);
        }
        out.sort_by_key(|a| a.created_at);
        Ok(out)
    }

    pub fn list_items_for_turn(&self, turn_id: &str) -> Result<Vec<TurnItemRecord>> {
        validated_record_id(turn_id, "turn id")?;
        let mut out = Vec::new();
        let items_dir = checked_existing_runtime_store_dir(&self.items_dir)?;
        for entry in fs::read_dir(&items_dir)
            .with_context(|| format!("Failed to read {}", items_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let item_id = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let raw = read_store_file(&path).with_context(|| {
                RuntimeStoreRecordFailure::new(
                    RuntimeStoreOperation::Read,
                    RuntimeStoreRecordKind::Item,
                    &item_id,
                    &path,
                )
            })?;
            #[cfg(any(test, feature = "test-support"))]
            self.item_dir_files_read
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let item: TurnItemRecord = serde_json::from_str(&raw).with_context(|| {
                RuntimeStoreRecordFailure::new(
                    RuntimeStoreOperation::Parse,
                    RuntimeStoreRecordKind::Item,
                    &item_id,
                    &path,
                )
            })?;
            if item.schema_version > MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION {
                bail!(
                    "Item schema v{} is newer than supported v{}",
                    item.schema_version,
                    MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION
                );
            }
            if matches!(
                item.schema_version,
                IMAGE_RUNTIME_SCHEMA_VERSION | OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION
            ) && item.kind == TurnItemKind::UserMessage
            {
                item.user_content()?;
            }
            if item.turn_id == turn_id {
                out.push(item);
            }
        }
        sort_turn_items_by_start(&mut out);
        Ok(out)
    }

    fn thread_item(
        &self,
        thread_id: &str,
        item_id: &str,
    ) -> Result<Option<(TurnItemRecord, TurnRecord)>> {
        let item = match self.load_item(item_id) {
            Ok(item) => item,
            Err(error) if store_record_missing(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        match self.load_turn(&item.turn_id) {
            Ok(turn) if turn.thread_id == thread_id => Ok(Some((item, turn))),
            Ok(_) => Ok(None),
            Err(error) if store_record_missing(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn list_items_for_turns_map(
        &self,
        turn_ids: &[String],
    ) -> Result<HashMap<String, Vec<TurnItemRecord>>> {
        if turn_ids.is_empty() {
            return Ok(HashMap::new());
        }

        for turn_id in turn_ids {
            validated_record_id(turn_id, "turn id")?;
        }

        let wanted: HashSet<&str> = turn_ids.iter().map(String::as_str).collect();
        let mut out: HashMap<String, Vec<TurnItemRecord>> = HashMap::new();
        for (turn_id, item_ids) in self.item_ids_for_turns(&wanted)? {
            for item_id in item_ids {
                // An index entry whose file is gone contributes nothing, which
                // is what the directory walk this replaced reported too.
                if !self.item_path(&item_id)?.exists() {
                    continue;
                }
                let item = self.load_item(&item_id)?;
                out.entry(turn_id.clone()).or_default().push(item);
            }
        }

        for items in out.values_mut() {
            sort_turn_items_by_start(items);
        }
        Ok(out)
    }

    /// The item ids this store holds for each of `turn_ids`, from the one
    /// items-directory read [`Self::ensure_item_index`] performs.
    fn item_ids_for_turns(&self, turn_ids: &HashSet<&str>) -> Result<HashMap<String, Vec<String>>> {
        self.ensure_item_index()?;
        let index = self.item_index.read();
        let by_turn = index
            .by_turn
            .as_ref()
            .context("item index must be published before it is read")?;
        Ok(turn_ids
            .iter()
            .filter_map(|turn_id| {
                by_turn
                    .get(*turn_id)
                    .map(|item_ids| ((*turn_id).to_string(), item_ids.clone()))
            })
            .collect())
    }

    /// Read the items directory once, and never again for this store.
    ///
    /// An item's filename carries the item id and nothing else, so the only way
    /// to learn which items a turn has is to read every item record. Both
    /// callers used to do exactly that per request: [`Self::get_thread_detail`]
    /// (`GET /v1/threads/{id}`, one walk per thread opened) and the fork
    /// preparation whose items the transcript rebuild reads. On the store this
    /// was measured against — 61,441 items, 294MB, 140 threads — one walk costs
    /// ~1.4s warm and 6.7s cold, and it was the whole of a thread's open time; a
    /// median thread holds 272 items, so reading the ones it needs is ~7ms.
    ///
    /// The walk is not parallelizable either, which is why this reads once
    /// instead of reading harder: sixteen concurrent `cat` streams of the same
    /// 61k files finished in 3.6s against 2.1s for one, and four in 1.5s. The
    /// cost is the per-file syscall, not the bytes.
    ///
    /// The read runs under the seed guard and outside the index guard, so a
    /// writer never waits for it. A write that lands while the read runs may or
    /// may not be in the directory snapshot it takes, so the writer records that
    /// write and the published map drains the records. A write with no read
    /// running records nothing: its file is already on disk, so the next read
    /// finds it in the directory itself.
    fn ensure_item_index(&self) -> Result<()> {
        if self.item_index.read().by_turn.is_some() {
            return Ok(());
        }

        // One read at a time. A caller that finds the read already running
        // waits here and then takes the map it produced, rather than reading
        // the same directory a second time.
        let _seed = self.item_index_seed.lock();
        if self.item_index.read().by_turn.is_some() {
            return Ok(());
        }

        let mut by_turn: HashMap<String, Vec<String>> = HashMap::new();
        let items_dir = checked_existing_runtime_store_dir(&self.items_dir)?;
        for entry in fs::read_dir(&items_dir)
            .with_context(|| format!("Failed to read {}", items_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let item_id = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            #[cfg(any(test, feature = "test-support"))]
            self.item_dir_files_read
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let item = self.load_item(&item_id)?;
            by_turn.entry(item.turn_id).or_default().push(item.id);
        }

        let mut index = self.item_index.write();
        for (turn_id, item_id) in std::mem::take(&mut index.pending) {
            let item_ids = by_turn.entry(turn_id).or_default();
            if !item_ids.contains(&item_id) {
                item_ids.push(item_id);
            }
        }
        index.by_turn = Some(by_turn);
        Ok(())
    }

    /// Keep the item index current across an item write.
    ///
    /// The two writers ([`Self::save_item`] and [`Self::save_items_batch`]) are
    /// the only paths that can put an item in this store, so this is what makes
    /// the index exact rather than a snapshot: a reader sees every item the
    /// directory would have shown it, including the late ones
    /// [`Self::attach_item_to_turn`] deliberately leaves out of a settled turn's
    /// `item_ids`.
    fn note_item_in_index(&self, turn_id: &str, item_id: &str) {
        // Probing the seed guard says whether a directory read is running, and
        // the guard is dropped immediately: this call must never wait behind
        // one. The item this follows is already on disk, so a read that starts
        // later finds it in the directory; only a read that is *already*
        // running may have passed the file, and only that case is recorded —
        // which is what keeps this queue finite in a process that writes items
        // without ever reading them.
        let read_in_flight = self.item_index_seed.try_lock().is_none();
        let mut index = self.item_index.write();
        if index.by_turn.is_none() {
            if read_in_flight {
                index
                    .pending
                    .push((turn_id.to_string(), item_id.to_string()));
            }
            return;
        }
        // The runtime saves one item id many times (in progress, then
        // completed or failed), and a write that raced the directory read may
        // already be in the map from that read. An id is listed once per turn.
        let item_ids = index
            .by_turn
            .as_mut()
            .expect("checked above")
            .entry(turn_id.to_string())
            .or_default();
        if !item_ids.iter().any(|known| known == item_id) {
            item_ids.push(item_id.to_string());
        }
    }

    /// The newest message text for each row of the thread list, read from the
    /// thread's own newest turn.
    ///
    /// A row shows the newest message of the newest turn that has one. That
    /// used to be answered by [`Self::newest_message_text_by_turn`] over every
    /// turn of every listed thread, and that call walks the whole items
    /// directory — an item filename carries only the item id, never its turn.
    /// A page of 100 threads out of a 140-thread store therefore read all
    /// 58,048 item records to fill 100 previews: ~1.4s per summary, on every
    /// summary, measured 2026-09-26 against the 658MB store, which is the
    /// wait the rail's spinner was covering.
    ///
    /// A turn record carries its own `item_ids`, so one turn's newest message
    /// can be read directly, and the walk below stops at the first thread turn
    /// that yields one. The page measured above resolves all 100 previews from
    /// 165 item files — it reads the tail of each row's newest turn and
    /// nothing else.
    ///
    /// The choice of turn is the one the batch read made: newest first, an
    /// older turn only when the newer ones hold no message text at all.
    fn newest_message_text_by_thread(
        &self,
        turns_by_thread: &HashMap<String, Vec<TurnRecord>>,
    ) -> Result<HashMap<String, String>> {
        let mut previews: HashMap<String, String> = HashMap::new();
        // A turn written before `item_ids` existed cannot be read by id: only a
        // scan of the items directory can find it. Those turns are collected
        // so the scan runs once for the page rather than once per turn, and the
        // thread's walk resumes from the turn it stopped on.
        let mut legacy_turn_ids: Vec<String> = Vec::new();
        let mut legacy_stops: Vec<(&str, usize)> = Vec::new();

        for (thread_id, turns) in turns_by_thread {
            let mut walked = turns.len();
            while walked > 0 {
                walked -= 1;
                let turn = &turns[walked];
                if turn.item_ids.is_empty() {
                    // This turn and every turn older than it are legacy, and
                    // both need the same directory scan: hand them all over at
                    // once instead of scanning per turn.
                    for turn in &turns[..=walked] {
                        legacy_turn_ids.push(turn.id.clone());
                    }
                    legacy_stops.push((thread_id.as_str(), walked));
                    break;
                }
                if let Some(text) = self.newest_message_text_in_turn(turn)? {
                    previews.insert(thread_id.clone(), text);
                    break;
                }
            }
        }

        if !legacy_turn_ids.is_empty() {
            let legacy = self.newest_message_text_by_turn(&legacy_turn_ids)?;
            for (thread_id, walked) in legacy_stops {
                let turns = &turns_by_thread[thread_id];
                if let Some(text) = turns[..=walked]
                    .iter()
                    .rev()
                    .find_map(|turn| legacy.get(&turn.id).cloned())
                {
                    previews.insert(thread_id.to_string(), text);
                }
            }
        }

        Ok(previews)
    }

    /// The newest message text `turn` appended, read from the turn's own item
    /// list instead of from a scan of the items directory.
    ///
    /// A turn appends its items in order, so the last message item it wrote is
    /// the newest message it has: what follows that message is a status,
    /// reasoning, file-change or tool record, never another message. Walking
    /// the ids from the tail therefore ends within the handful of files between
    /// the end of the turn and its last message, where reading the turn whole
    /// would read every item it ever wrote.
    ///
    /// Selection agrees with [`Self::newest_message_text_by_turn`] on every
    /// turn of the 658MB / 58k-item store this was measured against (538 turns,
    /// 3,537 message items, 2026-09-26): non-message items never win and an
    /// empty message is skipped rather than reported. The comparison that call
    /// makes is on `started_at`, and append order is timestamp order because
    /// the runtime writes each item as its turn produces it — for every source
    /// of `item_ids`, not only the live one: a fork rebuilds the cloned turn's
    /// list from items already ordered by [`sort_turn_items_by_start`], and an
    /// imported conversation pushes its seed items in the order the importer
    /// stamped them. Where the two orders could disagree is a hand-edited or
    /// externally written store; there this walk agrees with what
    /// `RuntimeThreadManager::reconstruct_messages_from_turns_with` would
    /// replay for the model — `item_ids` order — rather than with `started_at`,
    /// which is the agreement that matters for a preview.
    ///
    /// Reading only `turn.item_ids` is complete for messages, not just fast:
    /// every writer of an `AgentMessage`/`UserMessage` item registers the id on
    /// its turn at the same time. Streaming turns attach as they go (the turn
    /// is active, so [`Self::attach_item_to_turn`] appends); `start_turn` and
    /// `steer_turn` push the user item onto a turn they have just loaded or
    /// verified in progress; an imported conversation writes the whole list
    /// before its turn exists. [`Self::attach_item_to_turn`] does leave a
    /// settled turn's `item_ids` untouched, but the only items that reach a
    /// settled turn that way are status, tool and error records — kinds this
    /// walk skips — never messages, so a settled turn cannot hold a late
    /// message its preview missed.
    fn newest_message_text_in_turn(&self, turn: &TurnRecord) -> Result<Option<String>> {
        for item_id in turn.item_ids.iter().rev() {
            // A turn can name an item whose file was since removed; the
            // directory walk this replaced never saw such an id, so it must
            // not fail the whole summary page either.
            if !self.item_path(item_id)?.exists() {
                continue;
            }
            let item = self.load_item(item_id)?;
            if !matches!(
                item.kind,
                TurnItemKind::AgentMessage | TurnItemKind::UserMessage
            ) {
                continue;
            }
            let text = item.detail.unwrap_or(item.summary);
            if text.trim().is_empty() {
                continue;
            }
            return Ok(Some(text));
        }
        Ok(None)
    }

    /// The newest agent/user message text in each requested turn, in one pass.
    ///
    /// This is the directory-scan fallback: the thread summary reads previews
    /// through [`Self::newest_message_text_by_thread`], and reaches this call
    /// only for turns whose record predates `item_ids`.
    ///
    /// [`Self::list_items_for_turns_map`] materializes every item of every
    /// requested turn. The thread summary needs only the last user/agent
    /// message per turn, and one page can span most of the store, so retaining
    /// that whole projection for a page held every item it covered in memory
    /// at once — a page's peak that grew with the page instead of with one
    /// thread. This holds at most two message texts per requested turn — the
    /// newest dated one and the last undated one read — and nothing else:
    /// tool calls, results and artifacts are read and dropped, and an older
    /// dated message is dropped the moment a newer one is read.
    ///
    /// The winner is the message [`sort_turn_items_by_start`] would place
    /// last: that comparator reads only `started_at`, substitutes one "now"
    /// for a missing timestamp once everything has been read, and is stable,
    /// so the later-read message wins a tie. The choice between the dated
    /// and the undated candidate is therefore made after the scan, against a
    /// "now" taken then: a message persisted mid-scan with a timestamp later
    /// than a "now" taken up front would otherwise outrank an undated one
    /// the sort would have placed last.
    ///
    /// Known limitation: peak memory is still up to two texts per turn of
    /// the page, untruncated, because the caller decides which turn's text
    /// becomes the row's preview and how much of it to show.
    pub fn newest_message_text_by_turn(
        &self,
        turn_ids: &[String],
    ) -> Result<HashMap<String, String>> {
        if turn_ids.is_empty() {
            return Ok(HashMap::new());
        }

        for turn_id in turn_ids {
            validated_record_id(turn_id, "turn id")?;
        }

        let wanted: HashSet<&str> = turn_ids.iter().map(String::as_str).collect();
        #[derive(Default)]
        struct Candidates {
            /// Newest `started_at` read so far; a later read wins a tie.
            dated: Option<(DateTime<Utc>, String)>,
            /// Last message read without a `started_at`.
            undated: Option<String>,
        }
        let mut per_turn: HashMap<String, Candidates> = HashMap::new();
        let items_dir = checked_existing_runtime_store_dir(&self.items_dir)?;
        for entry in fs::read_dir(&items_dir)
            .with_context(|| format!("Failed to read {}", items_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let item_id = path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default();
            let raw = read_store_file(&path).with_context(|| {
                RuntimeStoreRecordFailure::new(
                    RuntimeStoreOperation::Read,
                    RuntimeStoreRecordKind::Item,
                    &item_id,
                    &path,
                )
            })?;
            #[cfg(any(test, feature = "test-support"))]
            self.item_dir_files_read
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let item: TurnItemRecord = serde_json::from_str(&raw).with_context(|| {
                RuntimeStoreRecordFailure::new(
                    RuntimeStoreOperation::Parse,
                    RuntimeStoreRecordKind::Item,
                    &item_id,
                    &path,
                )
            })?;
            if item.schema_version > MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION {
                bail!(
                    "Item schema v{} is newer than supported v{}",
                    item.schema_version,
                    MAX_SUPPORTED_RUNTIME_SCHEMA_VERSION
                );
            }
            if !wanted.contains(item.turn_id.as_str()) {
                continue;
            }
            if !matches!(
                item.kind,
                TurnItemKind::AgentMessage | TurnItemKind::UserMessage
            ) {
                continue;
            }
            if matches!(
                item.schema_version,
                IMAGE_RUNTIME_SCHEMA_VERSION | OUTPUT_LIMIT_RUNTIME_SCHEMA_VERSION
            ) && item.kind == TurnItemKind::UserMessage
            {
                item.user_content()?;
            }
            let text = item.detail.unwrap_or(item.summary);
            if text.trim().is_empty() {
                continue;
            }
            let slot = per_turn.entry(item.turn_id).or_default();
            match item.started_at {
                Some(started_at) => {
                    if slot
                        .dated
                        .as_ref()
                        .is_none_or(|(held, _)| *held <= started_at)
                    {
                        slot.dated = Some((started_at, text));
                    }
                }
                None => slot.undated = Some(text),
            }
        }

        // The sort's stand-in for a missing timestamp, taken once the scan
        // is over exactly as `sort_turn_items_by_start` takes it after
        // collection: an undated message reads as newest unless a dated one
        // is timestamped later than this instant.
        let fallback = Utc::now();
        Ok(per_turn
            .into_iter()
            .filter_map(|(turn_id, slot)| {
                let text = match (slot.dated, slot.undated) {
                    (Some((started_at, dated)), Some(undated)) => {
                        if started_at > fallback {
                            dated
                        } else {
                            undated
                        }
                    }
                    (Some((_, dated)), None) => dated,
                    (None, Some(undated)) => undated,
                    (None, None) => return None,
                };
                Some((turn_id, text))
            })
            .collect())
    }

    pub async fn append_event(
        &self,
        thread_id: &str,
        turn_id: Option<&str>,
        item_id: Option<&str>,
        event: impl Into<String>,
        payload: Value,
    ) -> Result<RuntimeEventRecord> {
        validated_record_id(thread_id, "thread id")?;
        if let Some(turn_id) = turn_id {
            validated_record_id(turn_id, "turn id")?;
        }
        if let Some(item_id) = item_id {
            validated_record_id(item_id, "item id")?;
        }
        let store = self.clone();
        let thread_id = thread_id.to_string();
        let turn_id = turn_id.map(ToString::to_string);
        let item_id = item_id.map(ToString::to_string);
        let event = event.into();
        tokio::task::spawn_blocking(move || {
            store.append_event_transaction(
                thread_id,
                turn_id,
                item_id,
                event,
                payload,
                EVENT_TRANSACTION_LOCK_TIMEOUT,
            )
        })
        .await
        .context("Runtime event transaction worker failed")?
    }

    fn append_event_transaction(
        &self,
        thread_id: String,
        turn_id: Option<String>,
        item_id: Option<String>,
        event: String,
        payload: Value,
        lock_timeout: Duration,
    ) -> Result<RuntimeEventRecord> {
        let path = self.events_path(&thread_id)?;
        self.with_event_transaction(lock_timeout, || {
            reject_symlinked_store_dir(&self.events_dir)?;
            repair_torn_event_log_tail(&path)?;
            let mut state = load_runtime_store_state(&self.state_path)?;
            let seq = state.next_seq;
            state.next_seq = seq
                .checked_add(1)
                .context("Runtime event sequence exhausted")?;
            write_json_atomic(&self.state_path, &state)?;

            let record = RuntimeEventRecord {
                schema_version: CURRENT_RUNTIME_SCHEMA_VERSION,
                seq,
                timestamp: Utc::now(),
                thread_id,
                turn_id,
                item_id,
                event,
                payload,
            };

            let mut file = open_runtime_store_file(&path, "event append", |options| {
                options.create(true).append(true);
            })?;
            let rollback_file =
                open_runtime_store_file(&path, "Runtime event rollback", |options| {
                    options.write(true);
                })?;
            validate_same_runtime_store_file_handles(&file, &rollback_file, &path)?;
            let original_len = file
                .metadata()
                .with_context(|| format!("Failed to inspect {}", path.display()))?
                .len();
            let mut line = serde_json::to_vec(&record)?;
            // A trailing newline is the commit marker. Startup removes a
            // parseable but unterminated tail without reusing its sequence.
            line.push(b'\n');
            let append_result = (|| -> std::io::Result<()> {
                file.write_all(&line)?;
                file.flush()?;
                #[cfg(any(test, feature = "test-support"))]
                if take_test_event_append_fault(&record.thread_id, EventAppendTestFault::AfterFlush)
                {
                    return Err(std::io::Error::other(
                        "injected Runtime event failure after flush",
                    ));
                }
                file.sync_all()?;
                #[cfg(any(test, feature = "test-support"))]
                if take_test_event_append_fault(&record.thread_id, EventAppendTestFault::AfterSync)
                {
                    return Err(std::io::Error::other(
                        "injected Runtime event failure after fsync",
                    ));
                }
                Ok(())
            })();
            if let Err(append_error) = append_result {
                // A failed flush/fsync can still leave the complete JSONL record
                // visible (or even durable). Roll back to the exact pre-append
                // offset and fsync that truncation before reporting a retryable
                // error. If rollback itself fails, classify the write as
                // indeterminate so callers never restore/retry and duplicate a
                // possibly committed terminal receipt.
                // The pre-opened rollback handle was identity-checked before
                // any bytes were written and stays live across this transaction.
                drop(file);
                let rollback_result =
                    rollback_failed_event_append_handle(&rollback_file, original_len);
                let error = match rollback_result {
                    Ok(()) => RuntimeEventAppendError {
                        disposition: EventAppendFailureDisposition::RolledBack,
                        append_error: append_error.to_string(),
                        rollback_error: None,
                    },
                    Err(rollback_error) => RuntimeEventAppendError {
                        disposition: EventAppendFailureDisposition::Indeterminate,
                        append_error: append_error.to_string(),
                        rollback_error: Some(rollback_error.to_string()),
                    },
                };
                return Err(anyhow!(error));
            }
            Ok(record)
        })
    }

    fn extension_command_events(
        &self,
        thread_id: &str,
        fingerprint: &str,
    ) -> Result<(Option<Value>, Option<Value>, bool)> {
        let mut started = None;
        let mut finished = None;
        let mut cancelled = false;
        self.for_each_event(thread_id, |event| {
            if event
                .payload
                .get("operation_fingerprint")
                .and_then(Value::as_str)
                == Some(fingerprint)
            {
                match event.event.as_str() {
                    "extension_command_started" => started = Some(event.payload),
                    "extension_command_completed" => finished = Some(event.payload),
                    "extension_command_cancelled" => cancelled = true,
                    _ => {}
                }
            }
            Ok(())
        })?;
        Ok((started, finished, cancelled))
    }

    pub fn events_since(
        &self,
        thread_id: &str,
        since_seq: Option<u64>,
    ) -> Result<Vec<RuntimeEventRecord>> {
        let mut out = Vec::new();
        self.for_each_event(thread_id, |event| {
            if since_seq.is_none_or(|since| event.seq > since) {
                out.push(event);
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// Visit every complete event of a thread in order without collecting
    /// the log, for callers that only need a summary of a long history.
    pub fn for_each_event(
        &self,
        thread_id: &str,
        mut visit: impl FnMut(RuntimeEventRecord) -> Result<()>,
    ) -> Result<()> {
        let path = self.events_path(thread_id)?;
        let Some(mut reader) = self.open_event_reader(thread_id)? else {
            return Ok(());
        };
        while let Some(event) = read_complete_event(&mut reader, &path)? {
            visit(event)?;
        }
        Ok(())
    }

    /// Incremental JSONL replay from a byte cursor. The returned cursor only
    /// advances past complete newline-terminated records so a live tail can
    /// be retried without rereading earlier history.
    pub fn events_from_offset(
        &self,
        thread_id: &str,
        offset: u64,
        limit: Option<usize>,
    ) -> Result<(Vec<RuntimeEventRecord>, u64)> {
        let path = self.events_path(thread_id)?;
        self.with_event_transaction(EVENT_TRANSACTION_LOCK_TIMEOUT, || {
            reject_symlinked_store_dir(&self.events_dir)?;
            if !path.exists() {
                return Ok((Vec::new(), offset));
            }
            let mut file =
                open_runtime_store_file(&path, "Runtime event cursor replay", |options| {
                    options.read(true);
                })?;
            let committed_len = file
                .metadata()
                .with_context(|| format!("Failed to inspect {}", path.display()))?
                .len();
            let start = offset.min(committed_len);
            file.seek(SeekFrom::Start(start))?;
            let mut reader = BufReader::new(file.take(committed_len.saturating_sub(start)));
            let mut out = Vec::new();
            let mut cursor = start;
            while let Some((event, consumed)) = read_complete_event_bytes(&mut reader, &path)? {
                cursor += consumed;
                out.push(event);
                if limit.is_some_and(|limit| out.len() >= limit) {
                    break;
                }
            }
            Ok((out, cursor))
        })
    }

    fn publish_event_replay(
        &self,
        thread_id: &str,
        since_seq: Option<u64>,
        tail_limit: Option<usize>,
        base_tx: oneshot::Sender<std::result::Result<u64, String>>,
        batch_tx: mpsc::Sender<std::result::Result<Vec<RuntimeEventRecord>, String>>,
    ) {
        let mut base_tx = Some(base_tx);
        // A panic here must not look like the end of history. Unwound, it
        // would drop `batch_tx` with no `Err`, the stream would read the
        // closed channel as "history complete", go live, and silently skip
        // the rest — with no `previous_seq` gap any client could detect.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match tail_limit {
            Some(limit) => {
                self.publish_tail_event_replay(thread_id, since_seq, limit, &mut base_tx, &batch_tx)
            }
            None => self.publish_full_event_replay(thread_id, since_seq, &mut base_tx, &batch_tx),
        }));
        Self::route_replay_outcome(outcome, &mut base_tx, &batch_tx);
    }

    /// Deliver a replay worker's failure, including a panic, to whoever is
    /// waiting: the request before the base cursor was sent (HTTP 500), the
    /// open stream after it (`stream.end`). Success needs nothing: dropping
    /// `batch_tx` is how the stream learns history is complete.
    fn route_replay_outcome(
        outcome: std::thread::Result<Result<()>>,
        base_tx: &mut Option<oneshot::Sender<std::result::Result<u64, String>>>,
        batch_tx: &mpsc::Sender<std::result::Result<Vec<RuntimeEventRecord>, String>>,
    ) {
        let message = match outcome {
            Ok(Ok(())) => return,
            Ok(Err(error)) => format!("{error:#}"),
            Err(panic) => {
                let detail = panic
                    .downcast_ref::<&str>()
                    .map(|detail| (*detail).to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic payload".to_string());
                format!("Runtime event replay worker panicked: {detail}")
            }
        };
        if let Some(base_tx) = base_tx.take() {
            let _ = base_tx.send(Err(message));
        } else {
            let _ = batch_tx.blocking_send(Err(message));
        }
    }

    fn open_event_reader(&self, thread_id: &str) -> Result<Option<RuntimeEventReader>> {
        let path = self.events_path(thread_id)?;
        self.with_event_transaction(EVENT_TRANSACTION_LOCK_TIMEOUT, || {
            reject_symlinked_store_dir(&self.events_dir)?;
            if !path.exists() {
                return Ok(None);
            }
            let file = open_runtime_store_file(&path, "Runtime event replay", |options| {
                options.read(true);
            })?;
            let committed_len = file
                .metadata()
                .with_context(|| format!("Failed to inspect {}", path.display()))?
                .len();
            Ok(Some(BufReader::new(file.take(committed_len))))
        })
    }

    fn contains_event(&self, thread_id: &str, expected: &RuntimeEventMatch) -> Result<bool> {
        let Some(mut reader) = self.open_event_reader(thread_id)? else {
            return Ok(false);
        };
        let path = self.events_path(thread_id)?;
        while let Some(event) = read_complete_event(&mut reader, &path)? {
            let matches = match expected {
                RuntimeEventMatch::ThreadStarted => {
                    event.event == "thread.started"
                        && event
                            .payload
                            .get("thread")
                            .and_then(|thread| thread.get("id"))
                            .and_then(Value::as_str)
                            == Some(thread_id)
                }
                RuntimeEventMatch::TurnCompleted { turn_id } => {
                    event.event == "turn.completed"
                        && event.turn_id.as_deref() == Some(turn_id.as_str())
                }
                RuntimeEventMatch::WorkerLifecycle {
                    turn_id,
                    agent_id,
                    event_name,
                } => {
                    event.event == *event_name
                        && event.turn_id.as_deref() == Some(turn_id.as_str())
                        && event.payload.get("agent_id").and_then(Value::as_str)
                            == Some(agent_id.as_str())
                }
                RuntimeEventMatch::DynamicTerminal { turn_id, call_id } => {
                    matches!(
                        event.event.as_str(),
                        "tool_call.resolved" | "tool_call.canceled" | "tool_call.timeout"
                    ) && event.turn_id.as_deref() == Some(turn_id.as_str())
                        && event.payload.get("call_id").and_then(Value::as_str)
                            == Some(call_id.as_str())
                }
                RuntimeEventMatch::AgentMail {
                    event_name,
                    message_id,
                    attempt_count,
                } => {
                    event.event == *event_name
                        && event
                            .payload
                            .get("mail")
                            .and_then(|mail| mail.get("message_id"))
                            .and_then(Value::as_str)
                            == Some(message_id.as_str())
                        && event
                            .payload
                            .get("mail")
                            .and_then(|mail| mail.get("attempt_count"))
                            .and_then(Value::as_u64)
                            == Some(*attempt_count as u64)
                }
            };
            if matches {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn publish_full_event_replay(
        &self,
        thread_id: &str,
        since_seq: Option<u64>,
        base_tx: &mut Option<oneshot::Sender<std::result::Result<u64, String>>>,
        batch_tx: &mpsc::Sender<std::result::Result<Vec<RuntimeEventRecord>, String>>,
    ) -> Result<()> {
        let Some(mut reader) = self.open_event_reader(thread_id)? else {
            if let Some(base_tx) = base_tx.take() {
                let _ = base_tx.send(Ok(since_seq.unwrap_or(0)));
            }
            return Ok(());
        };
        if base_tx
            .take()
            .is_some_and(|base_tx| base_tx.send(Ok(since_seq.unwrap_or(0))).is_err())
        {
            return Ok(());
        }

        let path = self.events_path(thread_id)?;
        let mut batch = Vec::with_capacity(RUNTIME_EVENT_REPLAY_BATCH_SIZE);
        while let Some(event) = read_complete_event(&mut reader, &path)? {
            if since_seq.is_some_and(|since| event.seq <= since) {
                continue;
            }
            batch.push(event);
            if batch.len() == RUNTIME_EVENT_REPLAY_BATCH_SIZE {
                if batch_tx.blocking_send(Ok(batch)).is_err() {
                    return Ok(());
                }
                batch = Vec::with_capacity(RUNTIME_EVENT_REPLAY_BATCH_SIZE);
            }
        }
        if !batch.is_empty() {
            let _ = batch_tx.blocking_send(Ok(batch));
        }
        Ok(())
    }

    fn publish_tail_event_replay(
        &self,
        thread_id: &str,
        since_seq: Option<u64>,
        tail_limit: usize,
        base_tx: &mut Option<oneshot::Sender<std::result::Result<u64, String>>>,
        batch_tx: &mpsc::Sender<std::result::Result<Vec<RuntimeEventRecord>, String>>,
    ) -> Result<()> {
        let Some(mut reader) = self.open_event_reader(thread_id)? else {
            if let Some(base_tx) = base_tx.take() {
                let _ = base_tx.send(Ok(since_seq.unwrap_or(0)));
            }
            return Ok(());
        };
        let path = self.events_path(thread_id)?;
        let mut base_seq = since_seq.unwrap_or(0);
        let mut tail = VecDeque::with_capacity(tail_limit.min(RUNTIME_EVENT_REPLAY_BATCH_SIZE));
        while let Some(event) = read_complete_event(&mut reader, &path)? {
            if since_seq.is_some_and(|since| event.seq <= since) {
                continue;
            }
            if tail_limit == 0 {
                base_seq = event.seq;
                continue;
            }
            tail.push_back(event);
            if tail.len() > tail_limit
                && let Some(omitted) = tail.pop_front()
            {
                base_seq = omitted.seq;
            }
        }
        if base_tx
            .take()
            .is_some_and(|base_tx| base_tx.send(Ok(base_seq)).is_err())
        {
            return Ok(());
        }
        while !tail.is_empty() {
            let take = tail.len().min(RUNTIME_EVENT_REPLAY_BATCH_SIZE);
            let batch = tail.drain(..take).collect::<Vec<_>>();
            if batch_tx.blocking_send(Ok(batch)).is_err() {
                return Ok(());
            }
        }
        Ok(())
    }

    pub async fn current_seq(&self) -> Result<u64> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || {
            store.with_event_transaction(EVENT_TRANSACTION_LOCK_TIMEOUT, || {
                Ok(load_runtime_store_state(&store.state_path)?
                    .next_seq
                    .saturating_sub(1))
            })
        })
        .await
        .context("Runtime event cursor worker failed")?
    }
}

/// Why a session switch refused to adopt an existing Runtime store.
///
/// Returned by [`RuntimeStoreBinding::adoption_refusal`]; the first guard
/// that did not provably hold wins. Known limitation: it names one reason,
/// not every one — a store both held and non-empty reports only the hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreAdoptionRefusal {
    /// The store is not at `<state>/sessions/<id>/runtime` (or a
    /// `runtime-recovered-*` sibling), or a symlink sits on the way down.
    Unconfined,
    /// The confined store path is not an existing directory.
    NotADirectory,
    /// Another live process holds the store's process-owner lock.
    HeldByLiveProcess,
    /// The named store directory holds work a switch would abandon.
    HasDurableWork { dir: &'static str },
    /// An automation is pinned to this store's execution scope.
    ScopePinnedAutomation,
}

impl std::fmt::Display for StoreAdoptionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconfined => f.write_str("the saved store is outside the session directory"),
            Self::NotADirectory => f.write_str("the saved store path is not an existing directory"),
            Self::HeldByLiveProcess => {
                f.write_str("another running Codewhale process holds the saved store")
            }
            Self::HasDurableWork { dir } => {
                write!(f, "the saved store still holds work in `{dir}`")
            }
            Self::ScopePinnedAutomation => {
                f.write_str("an automation is pinned to the saved store")
            }
        }
    }
}

/// Canonical spellings of configured sessions roots, keyed by the lexical
/// root `resolve_state_dir("sessions")` returns (tests move the state dir per
/// case, so one slot is not enough). The confinement predicate compares
/// against this instead of resolving a path itself, so a `/resume`, `/load`
/// or launch resume on the UI runtime never waits on filesystem resolution:
/// those entry points warm the cache on a blocking thread first
/// ([`prepare_canonical_sessions_root`]) (#6522).
static CANONICAL_SESSIONS_ROOTS: std::sync::Mutex<Vec<(PathBuf, PathBuf)>> =
    std::sync::Mutex::new(Vec::new());

fn cached_canonical_sessions_root(sessions: &Path) -> Option<PathBuf> {
    CANONICAL_SESSIONS_ROOTS
        .lock()
        .ok()?
        .iter()
        .find(|(lexical, _)| lexical == sessions)
        .map(|(_, canonical)| canonical.clone())
}

/// Resolve and remember the canonical form of `sessions`. Blocking: reach it
/// through [`prepare_canonical_sessions_root`] from async code. A root that
/// does not exist yet is not remembered, so a later call can still resolve it.
fn resolve_canonical_sessions_root(sessions: &Path) -> Option<PathBuf> {
    let canonical = sessions.canonicalize().ok()?;
    if let Ok(mut cache) = CANONICAL_SESSIONS_ROOTS.lock()
        && !cache.iter().any(|(lexical, _)| lexical == sessions)
    {
        cache.push((sessions.to_path_buf(), canonical.clone()));
    }
    Some(canonical)
}

/// Resolve the configured sessions root's canonical spelling on a blocking
/// thread so the store-confinement checks that follow on the UI runtime are
/// pure comparisons.
pub async fn prepare_canonical_sessions_root() {
    let Ok(sessions) = codewhale_config::resolve_state_dir("sessions") else {
        return;
    };
    if cached_canonical_sessions_root(&sessions).is_some() {
        return;
    }
    let _ = tokio::task::spawn_blocking(move || resolve_canonical_sessions_root(&sessions)).await;
}

/// Durable host authority shared by conversations created in that host.
/// A conversation id can change at launch; the locked Runtime store cannot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeStoreBinding {
    pub data_dir: PathBuf,
    pub execution_scope: String,
}

impl RuntimeStoreBinding {
    /// The confinement shared by every store-recovery predicate: the bound
    /// store must sit at `<state>/sessions/<session-id>/runtime` (or a
    /// `runtime-recovered-*` sibling) with no symlink on the way down. Wrong
    /// owner, symlinks and external paths fail closed for all callers.
    fn is_confined_session_store(&self) -> Result<bool> {
        let sessions = codewhale_config::resolve_state_dir("sessions")?;
        let Some(session_dir) = self.data_dir.parent() else {
            return Ok(false);
        };
        let Some(store_name) = self.data_dir.file_name().and_then(|name| name.to_str()) else {
            return Ok(false);
        };
        // A host records its binding from the store's canonical root
        // (`checked_runtime_store_root`), while the configured sessions root
        // is lexical. The two differ whenever an ancestor is spelled another
        // way — Windows `\\?\C:\` verbatim prefixes and 8.3 short names, or
        // a symlinked home/TMPDIR on Unix — and every real binding then read
        // as unconfined, so no switch could ever adopt it (#6418). Accept the
        // configured spelling or its canonical form only; nothing in the
        // binding's own path is resolved, so the symlink checks below still
        // fail closed.
        //
        // The canonical root comes from the cache the async entry points
        // (`TaskManager::start`, `/resume`, `/load`) warm off the UI runtime
        // via `prepare_canonical_sessions_root`; only a caller that never
        // warmed it (synchronous tests and tools) resolves it here.
        let parent = session_dir.parent();
        let under_sessions = parent == Some(sessions.as_path())
            || cached_canonical_sessions_root(&sessions)
                .or_else(|| resolve_canonical_sessions_root(&sessions))
                .is_some_and(|canonical| parent == Some(canonical.as_path()));
        if !under_sessions
            || !(store_name == "runtime" || store_name.starts_with("runtime-recovered-"))
            || !session_dir
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(codewhale_runtime::artifacts::is_valid_session_id)
        {
            return Ok(false);
        }
        for path in [&sessions, session_dir, &self.data_dir] {
            reject_symlinked_store_dir(path)?;
        }
        Ok(true)
    }

    /// Only a missing, confined session store can recover from its transcript.
    /// Existing stores with a wrong owner, symlinks and external paths fail closed.
    pub fn is_missing_session_store(&self) -> Result<bool> {
        if !self.is_confined_session_store()? {
            return Ok(false);
        }
        match fs::symlink_metadata(&self.data_dir) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(err) => Err(err.into()),
            Ok(_) => Ok(false),
        }
    }

    /// The first store directory holding durable work, or `None` when the
    /// store holds nothing a session switch could abandon.
    ///
    /// The switch path can rebind a conversation but cannot carry a store's
    /// durable work across — queued tasks, pending approvals, agent mail —
    /// which is why only a *missing* store was ever allowed to recover. A
    /// store that exists and is empty is the case that policy never covered:
    /// there is nothing to abandon, so refusing protects nothing, and a
    /// force-quit leaves exactly this shape (#6207).
    ///
    /// Callers establish confinement and that `data_dir` is a directory
    /// first; this only reads. Scope-pinned automations live outside the
    /// store directories and are covered by
    /// [`Self::has_scope_pinned_automation`], not here.
    fn first_durable_work_dir(&self) -> Result<Option<&'static str>> {
        for name in RUNTIME_STORE_WORK_DIRS {
            match fs::read_dir(self.data_dir.join(name)) {
                Ok(mut entries) => {
                    if entries.next().is_some() {
                        return Ok(Some(name));
                    }
                }
                // A store opened by an older build may predate a directory;
                // absent is empty.
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err.into()),
            }
        }
        // A sequence past its initial value means events were appended, even
        // if those files have since been pruned — reported as `events`.
        match fs::read_to_string(self.data_dir.join("state.json")) {
            Ok(raw) => {
                let state: RuntimeStoreState = serde_json::from_str(&raw)?;
                Ok((state.next_seq > 1).then_some("events"))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// True when another live process holds this store's process-owner lock.
    ///
    /// `open_inner` acquires the lock before the store is opened and holds it
    /// for the manager's lifetime, so a live holder's disk state is moving
    /// under us and an emptiness read against it is meaningless — that was
    /// the race that reverted the first #6207 fix. A missing lock file means
    /// no manager ever opened the store. The probe lock is released on drop;
    /// nothing is created or retained.
    ///
    /// Fails closed: an unconfined path reads as held, and an unexpected IO
    /// error refuses the adopt.
    pub fn has_live_holder(&self) -> Result<bool> {
        if !self.is_confined_session_store()? {
            return Ok(true);
        }
        let path = self.data_dir.join(RUNTIME_PROCESS_OWNER_LOCK_FILE);
        let file = match fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(err) => return Err(err.into()),
        };
        match codewhale_util::lock::try_lock_exclusive(&file) {
            Ok(()) => Ok(false),
            Err(error) if codewhale_util::lock::is_contention(&error) => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    /// True when an automation's execution scope matches this binding.
    ///
    /// Scope-pinned automations are recorded outside the store directories, so
    /// [`Self::first_durable_work_dir`] cannot see them — adopting their store
    /// would orphan their scheduled work. A missing automations directory
    /// means no definitions exist. Read-only: the manager is only opened when
    /// the directory exists, and listing takes no locks.
    ///
    /// Fails closed: an unconfined path reads as pinned, and an unreadable
    /// automations directory refuses the adopt.
    pub fn has_scope_pinned_automation(&self) -> Result<bool> {
        if !self.is_confined_session_store()? {
            return Ok(true);
        }
        let root = crate::automation_manager::default_automations_dir();
        if !root.join("automations").is_dir() {
            return Ok(false);
        }
        let manager = crate::automation_manager::AutomationManager::open(root)?;
        Ok(manager.list_automations()?.iter().any(|automation| {
            automation.execution_scope.as_deref() == Some(self.execution_scope.as_str())
        }))
    }

    /// Why a session switch may not adopt the bound store, or `None` when it
    /// may: confined, a directory, unheld, empty, with no scope-pinned
    /// automation. Liveness is checked before emptiness — a live holder's
    /// disk state moves under the read — and the automation check runs last
    /// because it parses every definition.
    ///
    /// Fails closed: every refusal is the first condition that did not
    /// provably hold, and an unexpected IO or parse error is an `Err`, which
    /// callers treat as a refusal. The reason exists so a user can be told
    /// *which* guard refused (#6418); it never widens what is adoptable.
    pub fn adoption_refusal(&self) -> Result<Option<StoreAdoptionRefusal>> {
        if !self.is_confined_session_store()? {
            return Ok(Some(StoreAdoptionRefusal::Unconfined));
        }
        if !self.data_dir.is_dir() {
            return Ok(Some(StoreAdoptionRefusal::NotADirectory));
        }
        if self.has_live_holder()? {
            return Ok(Some(StoreAdoptionRefusal::HeldByLiveProcess));
        }
        if let Some(dir) = self.first_durable_work_dir()? {
            return Ok(Some(StoreAdoptionRefusal::HasDurableWork { dir }));
        }
        if self.has_scope_pinned_automation()? {
            return Ok(Some(StoreAdoptionRefusal::ScopePinnedAutomation));
        }
        Ok(None)
    }

    /// True when the bound store exists and a switch may adopt it; see
    /// [`Self::adoption_refusal`] for the reason when it may not.
    pub fn is_adoptable_empty_store(&self) -> Result<bool> {
        Ok(self.adoption_refusal()?.is_none())
    }

    pub fn validate_existing_store(&self) -> Result<()> {
        anyhow::ensure!(
            self.data_dir.is_absolute(),
            "Saved Runtime store path must be absolute"
        );
        let root = checked_existing_runtime_store_dir(&self.data_dir)?;
        let owner: RuntimeStoreOwner =
            serde_json::from_str(&read_store_file(&root.join(AGENT_MAIL_OWNER_FILE))?)?;
        validated_record_id(&owner.owner_id, "Runtime owner id")?;
        anyhow::ensure!(
            runtime_execution_scope(&owner.owner_id, &root.join(EVENT_TRANSACTION_LOCK_FILE))
                == self.execution_scope,
            "Saved session Runtime store ownership does not match; refusing to recover another scope"
        );
        Ok(())
    }
}

fn runtime_execution_scope(owner_id: &str, event_lock_path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(owner_id.as_bytes());
    digest.update([0]);
    digest.update(event_lock_path.as_os_str().as_encoded_bytes());
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn runtime_dir_override() -> Option<PathBuf> {
    std::env::var("CODEWHALE_RUNTIME_DIR")
        .or_else(|_| std::env::var("DEEPSEEK_RUNTIME_DIR"))
        .ok()
        .filter(|override_dir| !override_dir.trim().is_empty())
        .map(PathBuf::from)
}

/// The canonicalized runtime-dir override, when one is set.
///
/// The control plane derives its endpoint from the same selection the store
/// uses (`daemon_socket::store_selected_socket_path`), so a non-default store
/// never shares the default owner's socket. Canonicalized through
/// [`checked_runtime_store_root`] so an aliased, relative or `..`-bearing
/// override cannot fork a second owner silently, and the same path the store
/// is compared against later. An unresolvable override is an error, not a
/// silent `None`: falling back would hand the caller the default endpoint for
/// a store that is not the default one.
#[cfg(unix)]
pub fn canonical_runtime_dir_override() -> Result<Option<PathBuf>> {
    runtime_dir_override()
        .map(checked_runtime_store_root)
        .transpose()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RuntimeTurnOperationBinding {
    schema_version: u32,
    thread_id: String,
    turn_id: String,
    operation_key_fingerprint: String,
    request_fingerprint: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeThreadCreateBinding {
    schema_version: u32,
    operation_key_fingerprint: String,
    request_fingerprint: String,
    thread_id: String,
    created_at: DateTime<Utc>,
    committed: bool,
}

impl RuntimeThreadCreateBinding {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.schema_version == 1,
            "unsupported thread creation operation schema"
        );
        validated_record_id(&self.thread_id, "thread creation operation id")?;
        validate_sha256_fingerprint(
            &self.operation_key_fingerprint,
            "thread creation operation fingerprint",
        )?;
        validate_sha256_fingerprint(
            &self.request_fingerprint,
            "thread creation request fingerprint",
        )
    }
}

impl RuntimeTurnOperationBinding {
    fn validate(&self) -> Result<()> {
        if self.schema_version > TURN_OPERATION_BINDING_SCHEMA_VERSION {
            bail!(
                "Runtime turn operation binding schema v{} is newer than supported v{}",
                self.schema_version,
                TURN_OPERATION_BINDING_SCHEMA_VERSION
            );
        }
        validated_record_id(&self.thread_id, "operation thread id")?;
        validated_record_id(&self.turn_id, "operation turn id")?;
        validate_sha256_fingerprint(&self.operation_key_fingerprint, "operation key fingerprint")?;
        validate_sha256_fingerprint(&self.request_fingerprint, "operation request fingerprint")?;
        Ok(())
    }
}

fn validate_runtime_turn_operation_key(value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("operation_key cannot be empty");
    }
    if value.len() > MAX_RUNTIME_TURN_OPERATION_KEY_BYTES {
        bail!("operation_key cannot exceed {MAX_RUNTIME_TURN_OPERATION_KEY_BYTES} UTF-8 bytes");
    }
    if value.trim() != value {
        bail!("operation_key cannot contain leading or trailing whitespace");
    }
    if value.chars().any(char::is_control) {
        bail!("operation_key cannot contain control characters");
    }
    Ok(())
}

fn validate_sha256_fingerprint(value: &str, label: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} must be a SHA-256 hex digest");
    }
    Ok(())
}

impl RuntimeStoreBinding {
    /// The binding a host that opened `data_dir` would have recorded: its
    /// canonical root and the scope derived from the store's owner. A store
    /// whose owner was never written has no scope an automation could pin.
    pub fn for_store_dir(data_dir: &Path) -> Result<Self> {
        let root = checked_runtime_store_root(data_dir.to_path_buf())?;
        let execution_scope = match read_store_file(&root.join(AGENT_MAIL_OWNER_FILE)) {
            Ok(raw) => {
                let owner: RuntimeStoreOwner = serde_json::from_str(&raw)?;
                runtime_execution_scope(&owner.owner_id, &root.join(EVENT_TRANSACTION_LOCK_FILE))
            }
            Err(_) if !root.join(AGENT_MAIL_OWNER_FILE).exists() => String::new(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            data_dir: root,
            execution_scope,
        })
    }

    /// Take this store's process-owner lock for maintenance, or `None` when
    /// a live process holds it (#6144).
    ///
    /// Holding the same lock a host takes in `open_inner` is what makes the
    /// maintenance exact: while it is held no process can open the store, so
    /// an emptiness read cannot race new work, and a move cannot pull the
    /// store out from under an opener — the opener fails its lock instead.
    /// Unconfined paths and non-directories are refused.
    pub fn try_hold(&self) -> Result<Option<HeldRuntimeStore>> {
        anyhow::ensure!(
            self.is_confined_session_store()?,
            "Runtime store {} is outside the sessions directory",
            self.data_dir.display()
        );
        anyhow::ensure!(
            self.data_dir.is_dir(),
            "Runtime store {} is not a directory",
            self.data_dir.display()
        );
        let lock = RuntimeProcessOwnerLock::try_acquire_file(
            &self.data_dir.join(RUNTIME_PROCESS_OWNER_LOCK_FILE),
            true,
        )?;
        Ok(lock.map(|lock| HeldRuntimeStore {
            binding: self.clone(),
            _lock: lock,
        }))
    }
}

/// A Runtime store held under its process-owner lock by maintenance —
/// session reconcile, or the switch/delete that just stopped binding it.
pub struct HeldRuntimeStore {
    binding: RuntimeStoreBinding,
    _lock: RuntimeProcessOwnerLock,
}

/// One thread recovered from a store no document references.
pub struct RecoverableThread {
    pub thread: ThreadRecord,
    pub messages: Vec<Message>,
}

impl HeldRuntimeStore {
    pub fn binding(&self) -> &RuntimeStoreBinding {
        &self.binding
    }

    /// Why this store must be kept, or `None` when it holds nothing: the same
    /// "nothing to lose" test a session switch applies before adopting it,
    /// read while no process can add work.
    pub fn keep_reason(&self) -> Result<Option<String>> {
        if let Some(dir) = self.binding.first_durable_work_dir()? {
            return Ok(Some(format!("holds work in `{dir}`")));
        }
        if self.binding.has_scope_pinned_automation()? {
            return Ok(Some("an automation is pinned to it".to_string()));
        }
        Ok(None)
    }

    /// Move the store to `destination`, holding its lock for the whole move,
    /// so no opener can race it. Nothing is unlinked.
    ///
    /// The store directory itself is not renamed: it contains the open lock
    /// file, and Windows refuses to rename a directory while a handle inside
    /// it is open. Each entry is renamed into `destination` instead, the lock
    /// file last. Renaming the open, locked lock file is allowed on every
    /// platform: unix renames the inode the lock is on, and on Windows std
    /// opens files with `FILE_SHARE_DELETE`, which permits a rename while the
    /// handle is open (a `LockFile` byte-range lock does not block it). The
    /// lock is dropped only after the emptied source directory is removed.
    ///
    /// Unlinking the lock file after dropping it is what this must never do
    /// (#6144): on unix `remove_file` succeeds while another process holds a
    /// lock on the file, so an opener that locked in that gap would hold a
    /// lock on an unlinked inode, and the next opener would create and lock a
    /// fresh file: two owners of one store. An opener that opened the old
    /// path before the move and locks after it sees its file is no longer at
    /// the path and reopens (see [`RuntimeProcessOwnerLock`]).
    pub fn move_to(self, destination: &Path) -> Result<()> {
        anyhow::ensure!(
            !destination.exists(),
            "set-aside destination {} already exists",
            destination.display()
        );
        let source = self.binding.data_dir.clone();
        fs::create_dir_all(destination)
            .with_context(|| format!("Failed to create {}", destination.display()))?;
        let mut names: Vec<std::ffi::OsString> = Vec::new();
        let listed = fs::read_dir(&source)
            .and_then(|entries| {
                for entry in entries {
                    let name = entry?.file_name();
                    if name != RUNTIME_PROCESS_OWNER_LOCK_FILE {
                        names.push(name);
                    }
                }
                Ok(())
            })
            .with_context(|| format!("Failed to read Runtime store {}", source.display()));
        if let Err(error) = listed {
            let _ = fs::remove_dir(destination);
            return Err(error);
        }
        names.push(RUNTIME_PROCESS_OWNER_LOCK_FILE.into());
        let mut moved: Vec<&std::ffi::OsString> = Vec::new();
        for name in &names {
            if let Err(error) = fs::rename(source.join(name), destination.join(name)) {
                // Put back what already moved so the store is never left
                // split across two directories; the lock is still held.
                for name in moved.into_iter().rev() {
                    let _ = fs::rename(destination.join(name), source.join(name));
                }
                let _ = fs::remove_dir(destination);
                return Err(anyhow::Error::from(error).context(format!(
                    "Failed to move Runtime store {} to {}",
                    source.display(),
                    destination.display()
                )));
            }
            moved.push(name);
        }
        #[cfg(any(test, feature = "test-support"))]
        run_owner_lock_test_hook(OwnerLockTestPoint::LockFileMoved);
        // Still holding the lock. A directory that is no longer empty means
        // an opener created a fresh lock file in it after the move; that
        // store is its own, so it stays.
        let _ = fs::remove_dir(&source);
        drop(self);
        Ok(())
    }

    fn open_store(&self) -> Result<RuntimeThreadStore> {
        RuntimeThreadStore::open(self.binding.data_dir.clone())
    }

    /// Threads in this store that name no session document, with their
    /// history rebuilt from their own turns. Threads with no turns carry no
    /// conversation to recover and are left out.
    ///
    /// Seed journals (#6555) are judged exactly as Runtime startup judges
    /// them, but read-only (this also serves dry runs): an uncommitted seed's
    /// turns are not history, and a thread whose journal cannot be settled is
    /// left out entirely.
    pub fn recoverable_threads(&self) -> Result<Vec<RecoverableThread>> {
        let store = self.open_store()?;
        let mut quarantined = HashSet::new();
        let mut unpublished_turns = HashSet::new();
        for path in store.seed_journal_paths()? {
            match store.judge_seed_journal(&path) {
                SeedJournalVerdict::Committed { .. } => {}
                SeedJournalVerdict::Uncommitted(journal) => {
                    unpublished_turns.extend(journal.turn_ids);
                }
                SeedJournalVerdict::Quarantined { thread_ids, reason } => {
                    tracing::warn!(
                        target: "runtime",
                        thread_ids = ?thread_ids,
                        journal = %path.display(),
                        "thread not recovered: seed journal cannot be settled ({reason})"
                    );
                    quarantined.extend(thread_ids);
                }
            }
        }
        let (threads, _) = store.list_threads_lenient()?;
        let mut out = Vec::new();
        for thread in threads {
            if thread.session_id.is_some() || quarantined.contains(&thread.id) {
                continue;
            }
            let mut turns = store.list_turns_for_thread(&thread.id)?;
            turns.retain(|turn| !unpublished_turns.contains(&turn.id));
            if turns.is_empty() {
                continue;
            }
            let turn_ids: Vec<String> = turns.iter().map(|turn| turn.id.clone()).collect();
            let items = store.list_items_for_turns_map(&turn_ids)?;
            let messages = store.history_from_records(&turns, &items)?;
            if messages.is_empty() {
                continue;
            }
            out.push(RecoverableThread { thread, messages });
        }
        Ok(out)
    }

    /// Bind `thread_id` to the document just written for it, covering every
    /// turn the document was rebuilt from.
    pub fn bind_recovered_thread(
        &self,
        thread_id: &str,
        session: &crate::session_manager::SavedSession,
    ) -> Result<()> {
        let store = self.open_store()?;
        let _thread_mutation = store.thread_mutation.lock();
        let mut thread = store.load_thread(thread_id)?;
        thread.session_id = Some(session.metadata.id.clone());
        thread.saved_session_checkpoint = Some(SavedSessionCheckpoint {
            covered_turn_id: thread.latest_turn_id.clone(),
            messages_sha256: session_messages_sha256(&session.messages)?,
            messages_len: Some(session.messages.len()),
            retained_messages: None,
        });
        thread.updated_at = Utc::now();
        store.save_thread(&thread)
    }

    /// Unbind threads whose session document no longer exists (#6144 R4).
    /// `exists` answers for a session id; a thread whose document is present
    /// is untouched — its checkpoint is verified (and migrated) when it loads.
    pub fn unbind_threads_without_documents(&self, exists: impl Fn(&str) -> bool) -> Result<usize> {
        if !self.binding.data_dir.join("threads").is_dir() {
            return Ok(0);
        }
        let store = self.open_store()?;
        let (threads, _) = store.list_threads_lenient()?;
        let mut unbound = 0;
        for thread in threads {
            let Some(session_id) = thread.session_id.as_deref() else {
                continue;
            };
            if exists(session_id) {
                continue;
            }
            unbound += unbind_session_threads_in_store(
                &store,
                &self.binding.data_dir,
                session_id,
                "its session document no longer exists",
            )?;
        }
        Ok(unbound)
    }
}

/// Clear `session_id` (and its checkpoint) on every thread in `store` that
/// names it, recording each old binding in the reconcile receipts.
fn unbind_session_threads_in_store(
    store: &RuntimeThreadStore,
    store_dir: &Path,
    session_id: &str,
    reason: &str,
) -> Result<usize> {
    let _thread_mutation = store.thread_mutation.lock();
    let (threads, _) = store.list_threads_lenient()?;
    let mut unbound = 0;
    for mut thread in threads {
        if thread.session_id.as_deref() != Some(session_id) {
            continue;
        }
        crate::session_reconcile::record_thread_unbound(store_dir, &thread, reason);
        thread.session_id = None;
        thread.saved_session_checkpoint = None;
        thread.updated_at = Utc::now();
        store.save_thread(&thread)?;
        unbound += 1;
    }
    Ok(unbound)
}

/// How many times an owner-lock acquire reopens the path after finding the
/// file it locked was moved out of the store. Each retry needs another
/// maintenance move to land in the same window, so a few is plenty.
const OWNER_LOCK_MOVED_RETRIES: usize = 4;

/// True when `file` is still the file at `path`. [`HeldRuntimeStore::move_to`]
/// renames a held lock file out of its store, so a process that opened the
/// path before that move and locked after it holds a lock on the moved store,
/// not on the one at `path` (#6144). A held file that is no longer one
/// regular file (unlinked, or linked twice) is not at the path either.
fn owner_lock_is_at_path(file: &File, path: &Path) -> Result<bool> {
    let Ok(held) = runtime_store_file_identity(file) else {
        return Ok(false);
    };
    let current = match open_runtime_store_file(path, "Runtime process owner lock", |options| {
        options.read(true);
    }) {
        Ok(current) => current,
        Err(error)
            if error
                .root_cause()
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    };
    Ok(runtime_store_file_identity(&current)? == held)
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OwnerLockTestPoint {
    /// In `RuntimeProcessOwnerLock::acquire`, after the lock file is opened
    /// and before it is locked.
    LockFileOpened,
    /// In `HeldRuntimeStore::move_to`, after the held lock file is renamed
    /// into the destination and before the lock is dropped.
    LockFileMoved,
}

#[cfg(any(test, feature = "test-support"))]
type OwnerLockTestHooks = Vec<(OwnerLockTestPoint, Box<dyn FnOnce()>)>;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static OWNER_LOCK_TEST_HOOKS: std::cell::RefCell<OwnerLockTestHooks> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Run `hook` once, on this thread, the next time `point` is reached.
#[cfg(any(test, feature = "test-support"))]
pub fn set_owner_lock_test_hook(point: OwnerLockTestPoint, hook: impl FnOnce() + 'static) {
    OWNER_LOCK_TEST_HOOKS.with(|hooks| hooks.borrow_mut().push((point, Box::new(hook))));
}

#[cfg(any(test, feature = "test-support"))]
fn run_owner_lock_test_hook(point: OwnerLockTestPoint) {
    let hook = OWNER_LOCK_TEST_HOOKS.with(|hooks| {
        let mut hooks = hooks.borrow_mut();
        let index = hooks.iter().position(|(at, _)| *at == point)?;
        Some(hooks.remove(index).1)
    });
    if let Some(hook) = hook {
        hook();
    }
}

#[derive(Debug)]
pub struct RuntimeProcessOwnerLock {
    _file: File,
    #[cfg(any(unix, windows))]
    generation: String,
    /// Set once [`Self::record_holder`] wrote a holder record, so `Drop` clears
    /// it before the lock is released instead of leaving a stale pid behind.
    holder_recorded: bool,
}

impl RuntimeProcessOwnerLock {
    /// Reuse the Runtime's protected OS lease for task-store ownership. A
    /// missing existing lease is uncertainty, never proof that an owner died.
    pub fn try_acquire_file(path: &Path, create: bool) -> Result<Option<Self>> {
        let root = checked_runtime_store_root(
            path.parent()
                .context("Owner lease has no parent")?
                .to_path_buf(),
        )?;
        for _ in 0..OWNER_LOCK_MOVED_RETRIES {
            ensure_runtime_store_dir(&root)?;
            let file = open_runtime_store_file(path, "Execution owner lease", |options| {
                options
                    .create(create)
                    .truncate(false)
                    .read(true)
                    .write(true);
            })?;
            match codewhale_util::lock::try_lock_exclusive(&file) {
                Ok(()) => {}
                Err(error) if codewhale_util::lock::is_contention(&error) => return Ok(None),
                Err(error) => {
                    return Err(error).context("Failed to acquire execution owner lease");
                }
            }
            if owner_lock_is_at_path(&file, path)? {
                return Ok(Some(Self {
                    _file: file,
                    #[cfg(any(unix, windows))]
                    generation: uuid::Uuid::new_v4().to_string(),
                    holder_recorded: false,
                }));
            }
            // Moved out of this store between open and lock: reopen.
        }
        Ok(None)
    }

    /// Host admission reuses this exact store lease, never a PID-file lock.
    #[cfg(any(unix, windows))]
    pub fn try_acquire_for_host(root: &Path, create: bool) -> Result<Option<Self>> {
        if !create {
            checked_existing_runtime_store_dir(root)?;
        }
        Self::try_acquire_file(&root.join(RUNTIME_PROCESS_OWNER_LOCK_FILE), create)
    }

    pub fn validate_at_root(&self, root: &Path) -> Result<()> {
        let root = checked_runtime_store_root(root.to_path_buf())?;
        anyhow::ensure!(
            owner_lock_is_at_path(&self._file, &root.join(RUNTIME_PROCESS_OWNER_LOCK_FILE))?,
            "held Runtime owner lease changed before host recovery"
        );
        Ok(())
    }

    pub fn acquire(root: &Path) -> Result<Self> {
        let root = checked_runtime_store_root(root.to_path_buf())?;
        let path = root.join(RUNTIME_PROCESS_OWNER_LOCK_FILE);
        // Same-process drop-then-reopen can observe WouldBlock for a brief
        // window while the previous fd is still closing — the same close-
        // release race #5735 hit on the Runtime Chat scope lock. Retry only
        // that contention; a lock that stays held still belongs to its owner.
        let deadline = Instant::now() + Duration::from_millis(25);
        for _ in 0..OWNER_LOCK_MOVED_RETRIES {
            ensure_runtime_store_dir(&root)?;
            let file = open_runtime_store_file(&path, "Runtime process owner lock", |options| {
                options.create(true).truncate(false).read(true).write(true);
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .context("Failed to protect Runtime process owner lock")?;
            }
            #[cfg(any(test, feature = "test-support"))]
            run_owner_lock_test_hook(OwnerLockTestPoint::LockFileOpened);
            loop {
                match codewhale_util::lock::try_lock_exclusive(&file) {
                    Ok(()) => break,
                    Err(error) if codewhale_util::lock::is_contention(&error) => {
                        if Instant::now() >= deadline {
                            return Err(Self::held_error());
                        }
                        std::thread::yield_now();
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => {
                        return Err(error).context("Failed to acquire Runtime process owner lock");
                    }
                }
            }
            if owner_lock_is_at_path(&file, &path)? {
                return Ok(Self {
                    _file: file,
                    #[cfg(any(unix, windows))]
                    generation: uuid::Uuid::new_v4().to_string(),
                    holder_recorded: false,
                });
            }
            // A maintenance move renamed the file out of this store between
            // our open and our lock; the lock we hold is on the moved store.
        }
        Err(Self::held_error())
    }

    /// Write this process's pid and the acquisition time into the lock file so
    /// a contender that finds the lock busy can say who holds it (#6573).
    /// Best effort: diagnostics must never fail the lock holder.
    pub fn record_holder(&mut self) {
        let since_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        let record = format!("pid={} since_ms={since_ms}\n", std::process::id());
        let mut file = &self._file;
        let written = file
            .set_len(0)
            .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
            .and_then(|()| file.write_all(record.as_bytes()));
        match written {
            Ok(()) => self.holder_recorded = true,
            Err(error) => tracing::debug!(%error, "Could not record lock holder"),
        }
    }

    /// Read the holder recorded by [`Self::record_holder`]: its pid and how
    /// long it has held the lock. Never blocks (the lock does not cover the
    /// record: `flock` is advisory, and the Windows range starts after it) and
    /// never fails: anything unreadable or malformed is `None`.
    pub fn read_holder(path: &Path) -> Option<(u32, Duration)> {
        let file = open_runtime_store_file(path, "Owner lock holder record", |options| {
            options.read(true);
        })
        .ok()?;
        let mut text = String::new();
        file.take(codewhale_util::lock::OWNER_LOCK_HOLDER_MAX_BYTES)
            .read_to_string(&mut text)
            .ok()?;
        let mut pid = None;
        let mut since_ms = None;
        for field in text.split_whitespace() {
            match field.split_once('=') {
                Some(("pid", value)) => pid = value.parse::<u32>().ok(),
                Some(("since_ms", value)) => since_ms = value.parse::<u64>().ok(),
                _ => {}
            }
        }
        let since = std::time::UNIX_EPOCH + Duration::from_millis(since_ms?);
        let held_for = std::time::SystemTime::now()
            .duration_since(since)
            .unwrap_or_default();
        Some((pid?, held_for))
    }

    /// A held lock is typed `WouldBlock` so callers (the credential scrub,
    /// #6601) can tell a busy owner from a real lock failure.
    fn held_error() -> anyhow::Error {
        std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            RUNTIME_PROCESS_OWNER_LOCK_HELD,
        )
        .into()
    }
}

impl Drop for RuntimeProcessOwnerLock {
    fn drop(&mut self) {
        if self.holder_recorded {
            // Best effort: a stale pid must not outlive the lock it described.
            let _ = self._file.set_len(0);
        }
        // close() also releases, but unlocking first lets a same-process
        // reopen proceed without racing the previous fd's teardown (#5735).
        codewhale_util::lock::unlock(&self._file);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub struct SnapshotTestPoint {
    pub thread_id: String,
    pub latest_seq: u64,
    pub resume: oneshot::Sender<()>,
}

#[cfg(any(test, feature = "test-support"))]
pub struct ReplayTestPoint {
    pub thread_id: String,
    pub resume: oneshot::Sender<()>,
}

/// Canonical history intent lives in the existing Runtime operation directory.
/// A reserved identity remains recoverable after an interrupted request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHistoryOperation {
    version: u32,
    key_fingerprint: String,
    pub committed: bool,
    pub created_at: chrono::DateTime<Utc>,
    pub receipt: codewhale_protocol::CanonicalThreadReceipt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_witness: Option<RuntimeHistoryWitness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub association: Option<codewhale_protocol::CanonicalThreadOperationAssociation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_document_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    goal_source_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    goal_target_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_goal_source_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_goal_target_digest: Option<String>,
}

/// Only reconstruction facts accompany the existing full-journal digest. The
/// full graph remains in the protected SavedSession, not another history store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHistoryWitness {
    pub entries_len: usize,
    pub leaf_id: Option<String>,
    pub schema_version: u32,
    pub spawn_depth: u32,
    pub seed_from_message_index: usize,
    pub workspace: PathBuf,
}
impl RuntimeHistoryOperation {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.version == 1 && self.receipt.version == 1,
            "unsupported history operation schema"
        );
        validate_sha256_fingerprint(&self.key_fingerprint, "history operation fingerprint")?;
        validate_sha256_fingerprint(&self.receipt.request_digest, "history request digest")?;
        validate_sha256_fingerprint(&self.receipt.history_digest, "history digest")?;
        validated_record_id(&self.receipt.runtime_thread_id, "history thread id")?;
        validated_record_id(&self.receipt.session_id, "history session id")?;
        validate_runtime_turn_operation_key(&self.receipt.operation_key)?;
        anyhow::ensure!(
            self.key_fingerprint
                == codewhale_runtime::hashing::sha256_hex(
                    format!(
                        "codewhale:history-operation:v1\0{}",
                        self.receipt.operation_key
                    )
                    .as_bytes()
                ),
            "history operation key does not match its binding"
        );
        anyhow::ensure!(
            self.receipt.data_dir.is_absolute()
                && !self.receipt.execution_scope.is_empty()
                && self.receipt.execution_scope.len() <= 128,
            "invalid history operation store binding"
        );
        if let Some(witness) = self.journal_witness.as_ref() {
            anyhow::ensure!(
                witness.entries_len <= codewhale_protocol::MAX_CANONICAL_HISTORY_ENTRIES
                    && witness.seed_from_message_index <= witness.entries_len
                    && witness.workspace.is_absolute()
                    && witness.schema_version == 1
                    && witness
                        .leaf_id
                        .as_ref()
                        .is_none_or(|id| id.len() <= 256 && !id.is_empty()),
                "invalid canonical operation journal witness"
            );
        }
        for digest in [
            self.session_goal_source_digest.as_ref(),
            self.session_goal_target_digest.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_sha256_fingerprint(digest, "history session goal digest")?;
        }
        anyhow::ensure!(
            self.session_goal_source_digest.is_some() == self.session_goal_target_digest.is_some(),
            "history session goal witness is incomplete"
        );
        if let Some(digest) = self.target_document_digest.as_ref() {
            validate_sha256_fingerprint(digest, "history target document digest")?;
        }
        anyhow::ensure!(
            self.goal_source_digest.is_some() == self.goal_target_digest.is_some(),
            "incomplete imported goal witness"
        );
        for digest in [
            self.goal_source_digest.as_ref(),
            self.goal_target_digest.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            validate_sha256_fingerprint(digest, "imported goal witness")?;
        }
        if let Some(association) = self.association.as_ref() {
            use codewhale_protocol::CanonicalThreadOperationKind;
            anyhow::ensure!(
                match association.kind {
                    CanonicalThreadOperationKind::Create =>
                        association.source_runtime_thread_id.is_none()
                            && association.source_session_id.is_none(),
                    CanonicalThreadOperationKind::Resume | CanonicalThreadOperationKind::Fork =>
                        association.source_session_id.is_some(),
                },
                "invalid canonical operation source association"
            );
            if let Some(id) = association.source_runtime_thread_id.as_ref() {
                validated_record_id(id, "history source thread id")?;
            }
            if let Some(id) = association.source_session_id.as_ref() {
                validated_record_id(id, "history source session id")?;
            }
            anyhow::ensure!(
                self.journal_witness.is_some(),
                "operation association has no captured scope witness"
            );
        }
        Ok(())
    }
}

/// Durable intent of one `seed_thread_from_messages` transaction (#6555).
/// The seed committed when the thread's `latest_turn_id` moved from
/// `previous_latest_turn_id` to the final journaled turn; see
/// [`RuntimeThreadStore::settle_seed_journals`].
#[derive(Debug, Serialize, Deserialize)]
struct SeedJournal {
    thread_id: String,
    previous_latest_turn_id: Option<String>,
    turn_ids: Vec<String>,
    item_ids: Vec<String>,
}

/// What a seed journal proves; see
/// [`RuntimeThreadStore::judge_seed_journal`].
enum SeedJournalVerdict {
    /// The seed committed; its records are history and the journal is stale.
    Committed { thread_id: String },
    /// The seed never published; its records are withheld and discarded.
    Uncommitted(SeedJournal),
    /// Intent or ownership is uncertain. Nothing is deleted; these threads
    /// are held out of recovery until the journal is repaired.
    Quarantined {
        thread_ids: Vec<String>,
        reason: String,
    },
}

/// True when a store read failed only because the record file is absent.
fn store_record_missing(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
}

fn duration_ms(start: DateTime<Utc>, end: DateTime<Utc>) -> u64 {
    let millis = (end - start).num_milliseconds();
    if millis.is_negative() {
        0
    } else {
        u64::try_from(millis).unwrap_or(u64::MAX)
    }
}

pub fn checked_runtime_store_root(root: PathBuf) -> Result<PathBuf> {
    if root.as_os_str().is_empty() {
        bail!("Runtime store root cannot be empty");
    }
    if root
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("Runtime store root cannot contain '..' components");
    }
    let absolute = if root.is_absolute() {
        root
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for runtime store")?
            .join(root)
    };
    match absolute.canonicalize() {
        Ok(path) => Ok(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(normalize_path_components(&absolute))
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "Failed to resolve runtime store root {}",
                absolute.display()
            )
        }),
    }
}

fn checked_existing_runtime_store_dir(path: &Path) -> Result<PathBuf> {
    reject_symlinked_store_dir(path)?;
    path.canonicalize()
        .with_context(|| format!("Failed to resolve {}", path.display()))
}

fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

fn reject_symlinked_store_file(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "Runtime store file must not be a symlink: {}",
            path.display()
        );
    }
    Ok(())
}

fn open_runtime_store_file(
    path: &Path,
    purpose: &str,
    configure: impl FnOnce(&mut OpenOptions),
) -> Result<File> {
    reject_symlinked_store_file(path)?;
    let mut options = OpenOptions::new();
    configure(&mut options);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .with_context(|| format!("Failed to open {purpose} {}", path.display()))?;
    runtime_store_file_identity(&file)
        .with_context(|| format!("Invalid {purpose} {}", path.display()))?;
    Ok(file)
}

fn load_or_create_runtime_store_owner(owner_path: &Path, event_lock_path: &Path) -> Result<String> {
    let lock_file =
        open_runtime_store_file(event_lock_path, "Runtime store ownership lock", |options| {
            options.create(true).truncate(false).read(true).write(true);
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        lock_file
            .set_permissions(fs::Permissions::from_mode(0o600))
            .context("Failed to secure Runtime store ownership lock")?;
    }
    let mut lock = fd_lock::RwLock::new(lock_file);
    let started = Instant::now();
    loop {
        match lock.try_write() {
            Ok(_guard) => {
                if owner_path.exists() {
                    let raw = read_store_file(owner_path)
                        .with_context(|| format!("Failed to read {}", owner_path.display()))?;
                    let owner: RuntimeStoreOwner = serde_json::from_str(&raw)
                        .with_context(|| format!("Failed to parse {}", owner_path.display()))?;
                    validated_record_id(&owner.owner_id, "Runtime owner id")?;
                    return Ok(owner.owner_id);
                }
                let owner_id = format!("owner_{}", Uuid::new_v4().simple());
                write_json_atomic(
                    owner_path,
                    &RuntimeStoreOwner {
                        owner_id: owner_id.clone(),
                    },
                )?;
                return Ok(owner_id);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                wait_for_event_lock(started, EVENT_TRANSACTION_LOCK_TIMEOUT)?;
            }
            Err(error) => {
                return Err(error).context("Failed to lock Runtime store ownership");
            }
        }
    }
}

#[cfg(unix)]
fn runtime_store_file_identity(file: &File) -> Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.nlink() == 1,
        "not one regular file"
    );
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn runtime_store_file_identity(file: &File) -> Result<(u64, u64)> {
    use std::os::windows::fs::MetadataExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
    };

    let metadata = file.metadata()?;
    let safe = metadata.is_file() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0;
    anyhow::ensure!(safe, "not a regular non-reparse file");
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle and writable output remain valid for the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error()).context("Inspect Runtime store file identity");
    }
    anyhow::ensure!(info.nNumberOfLinks == 1, "has multiple filesystem links");
    Ok((
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}

#[cfg(all(not(unix), not(windows)))]
fn runtime_store_file_identity(file: &File) -> Result<(u64, u64)> {
    anyhow::ensure!(file.metadata()?.is_file(), "must be a regular file");
    Ok((0, 0))
}

fn validate_same_runtime_store_file_handles(
    first: &File,
    second: &File,
    path: &Path,
) -> Result<()> {
    let first = runtime_store_file_identity(first)?;
    let second = runtime_store_file_identity(second)?;
    anyhow::ensure!(
        first == second,
        "Runtime event file changed: {}",
        path.display()
    );
    Ok(())
}

fn wait_for_event_lock(started: Instant, timeout: Duration) -> Result<()> {
    let elapsed = started.elapsed();
    if elapsed >= timeout {
        return Err(anyhow!(RuntimeEventLockTimeout(timeout)));
    }
    std::thread::sleep(EVENT_TRANSACTION_LOCK_POLL.min(timeout - elapsed));
    Ok(())
}

fn rollback_failed_event_append_handle(rollback_file: &File, original_len: u64) -> Result<()> {
    rollback_file
        .set_len(original_len)
        .context("Failed to roll back Runtime event")?;
    rollback_file
        .sync_all()
        .context("Failed to sync Runtime event rollback")
}

fn reject_symlinked_store_dir(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "Runtime store directory must not be a symlink: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!("Runtime store path must be a directory: {}", path.display());
    }
    Ok(())
}

fn ensure_runtime_store_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("Failed to create {}", path.display()))?;
    reject_symlinked_store_dir(path)
}

fn read_complete_event(
    reader: &mut impl BufRead,
    path: &Path,
) -> Result<Option<RuntimeEventRecord>> {
    Ok(read_complete_event_bytes(reader, path)?.map(|(event, _)| event))
}

fn read_complete_event_bytes(
    reader: &mut impl BufRead,
    path: &Path,
) -> Result<Option<(RuntimeEventRecord, u64)>> {
    let mut skipped = 0u64;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        // A concurrent append can be visible before write_all finishes. The
        // subscribed broadcast path will deliver that event after its durable
        // append completes, so stop at an unterminated live tail instead of
        // misclassifying it as durable corruption. Store startup separately
        // truncates an unterminated tail left by a dead process.
        if !line.ends_with('\n') {
            return Ok(None);
        }
        skipped += u64::try_from(line.len()).unwrap_or(u64::MAX);
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str(&line)
            .with_context(|| format!("Failed to parse event line in {}", path.display()))?;
        return Ok(Some((event, skipped)));
    }
}

/// Remove only an unterminated final JSONL fragment left by a process or
/// machine stopping before the append's newline commit marker. This includes
/// an otherwise valid JSON object whose delimiter never reached disk: without
/// the newline, the append did not commit. A newline-terminated bad record is
/// not crash debris we can identify safely, so normal replay keeps rejecting
/// it instead of silently discarding durable data.
fn repair_torn_event_log_tails(events_dir: &Path) -> Result<()> {
    let events_dir = checked_existing_runtime_store_dir(events_dir)?;
    for entry in fs::read_dir(&events_dir)
        .with_context(|| format!("Failed to read {}", events_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        if !entry
            .file_type()
            .with_context(|| format!("Failed to inspect {}", path.display()))?
            .is_file()
        {
            continue;
        }
        repair_torn_event_log_tail(&path)?;
    }
    Ok(())
}

fn repair_torn_event_log_tail(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut file = open_runtime_store_file(path, "Runtime event tail recovery", |options| {
        options.read(true).write(true);
    })?;
    let len = file
        .metadata()
        .with_context(|| format!("Failed to inspect {}", path.display()))?
        .len();
    if len == 0 {
        return Ok(());
    }

    file.seek(SeekFrom::End(-1))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }

    let mut search_end = len;
    let mut truncate_at = 0_u64;
    let mut buffer = [0_u8; 8 * 1024];
    let buffer_len = u64::try_from(buffer.len()).expect("event recovery buffer fits u64");
    while search_end > 0 {
        let chunk_len = usize::try_from(search_end.min(buffer_len))
            .expect("event recovery chunk length fits usize");
        let chunk_len_u64 = u64::try_from(chunk_len).expect("event recovery chunk length fits u64");
        let chunk_start = search_end - chunk_len_u64;
        file.seek(SeekFrom::Start(chunk_start))?;
        file.read_exact(&mut buffer[..chunk_len])?;
        if let Some(index) = buffer[..chunk_len].iter().rposition(|byte| *byte == b'\n') {
            truncate_at = chunk_start
                + u64::try_from(index).expect("event recovery newline index fits u64")
                + 1;
            break;
        }
        search_end = chunk_start;
    }

    file.set_len(truncate_at)
        .with_context(|| format!("Failed to truncate torn tail in {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("Failed to sync repaired {}", path.display()))?;
    tracing::warn!(
        path = %path.display(),
        removed_bytes = len.saturating_sub(truncate_at),
        "Recovered an unterminated Runtime event-log tail"
    );
    Ok(())
}

fn read_store_file(path: &Path) -> Result<String> {
    reject_symlinked_store_file(path)?;
    fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
}

fn load_runtime_store_state(path: &Path) -> Result<RuntimeStoreState> {
    let file = open_runtime_store_file(path, "Runtime store state", |options| {
        options.read(true);
    })?;
    serde_json::from_reader(file).with_context(|| format!("Failed to parse {}", path.display()))
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory {}", parent.display()))?;
    }
    reject_symlinked_store_file(path)?;
    let payload = serde_json::to_string_pretty(value)?;
    codewhale_util::utils::write_atomic(path, payload.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    reject_symlinked_store_file(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("Failed to remove {}", path.display())),
    }
}

pub mod manager;
mod turn_artifacts;
pub use turn_artifacts::{
    FileChangeKind, TurnArtifactKind, TurnArtifactRef, TurnWorkspaceArtifacts, TurnWorkspaceReason,
    TurnWorkspaceState,
};

#[cfg(test)]
mod tests;

pub fn reconstruct_messages_from_turns_with(
    turns: &[TurnRecord],
    items_by_turn: &HashMap<String, Vec<TurnItemRecord>>,
) -> Result<Vec<Message>> {
    let mut messages = Vec::new();
    let mut execution_providers = HashMap::new();
    for turn in turns {
        let items = ordered_turn_items(turn, items_by_turn)?;

        for item in items.iter().filter(|item| projects_tool_history(item)) {
            for side in ["tool_use_id", "tool_result_for"] {
                if let Some(identity) = RuntimeToolIdentity::read(item.metadata.as_ref(), side)? {
                    check_execution_provider(
                        &mut execution_providers,
                        identity.execution_id,
                        identity.provider_id,
                    )?;
                }
            }
        }

        // Only results this same projection will emit can settle a call.
        // New execution and legacy provider key domains remain disjoint;
        // provider identity must also agree with an explicit execution.
        let recorded_results: HashSet<_> = items
            .iter()
            .filter(|item| projects_tool_history(item))
            .map(|item| RuntimeToolIdentity::read(item.metadata.as_ref(), "tool_result_for"))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .map(RuntimeToolIdentity::key)
            .collect();

        let mut assistant_blocks: Vec<ContentBlock> = Vec::new();
        let mut user_blocks: Vec<ContentBlock> = Vec::new();
        let flush_assistant = |blocks: &mut Vec<ContentBlock>, msgs: &mut Vec<Message>| {
            if !blocks.is_empty() {
                msgs.push(Message {
                    role: Role::Assistant,
                    content: std::mem::take(blocks),
                });
            }
        };
        let flush_user = |blocks: &mut Vec<ContentBlock>, msgs: &mut Vec<Message>| {
            if !blocks.is_empty() {
                msgs.push(Message {
                    role: Role::User,
                    content: std::mem::take(blocks),
                });
            }
        };
        for item in &items {
            match item.kind {
                TurnItemKind::UserMessage => {
                    // A steer the engine never committed is recorded
                    // `queued` or `canceled` precisely because the model
                    // never saw it. Replaying it here would put words into
                    // the context that the record says were not delivered
                    // — the record is right, so it stays out (#6276).
                    if matches!(
                        item.status,
                        TurnItemLifecycleStatus::Queued | TurnItemLifecycleStatus::Canceled
                    ) {
                        continue;
                    }
                    flush_assistant(&mut assistant_blocks, &mut messages);
                    user_blocks.extend(item.user_content()?);
                }
                TurnItemKind::AgentMessage => {
                    flush_user(&mut user_blocks, &mut messages);
                    let text = item.detail.clone().unwrap_or_else(|| item.summary.clone());
                    if !text.trim().is_empty() {
                        assistant_blocks.push(ContentBlock::Text {
                            text,
                            cache_control: None,
                        });
                    }
                }
                TurnItemKind::AgentReasoning => {
                    flush_user(&mut user_blocks, &mut messages);
                    let thinking = item.detail.clone().unwrap_or_else(|| item.summary.clone());
                    if !thinking.trim().is_empty() {
                        assistant_blocks.push(ContentBlock::Thinking {
                            thinking,
                            signature: None,
                            state: None,
                            redacted_data: None,
                        });
                    }
                }
                _ if projects_tool_history(item) => {
                    let meta = item.metadata.as_ref();
                    let meta_str = |key: &str| {
                        meta.and_then(|m| m.get(key))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    };
                    let call = RuntimeToolIdentity::read(meta, "tool_use_id")?;
                    let result = RuntimeToolIdentity::read(meta, "tool_result_for")?;
                    let tool_name = meta_str("tool_name");
                    // A call whose own outcome the store never recorded is
                    // answered with the outcome it does hold, decided while
                    // the id is still borrowed — see
                    // `unanswered_call_result`.
                    let unanswered = call.and_then(|identity| {
                        unanswered_call_result(item, identity, &tool_name, &recorded_results)
                            .map(|content| (identity, content))
                    });
                    // Completed live turns persist the call and its result
                    // on one item; seeded history persists them as two.
                    // Both shapes must rebuild the paired tool_call /
                    // tool_result. Snapshots persisted before tool identity
                    // was durable carry neither side: skip them rather than
                    // replay an empty tool_call shell that strict
                    // OpenAI-compatible endpoints reject (#5823).
                    if let Some(identity) = call.filter(|_| !tool_name.is_empty()) {
                        flush_user(&mut user_blocks, &mut messages);
                        let input_str = meta
                            .and_then(|m| m.get("tool_input"))
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| item.detail.clone())
                            .unwrap_or_default();
                        let input: serde_json::Value =
                            serde_json::from_str(&input_str).unwrap_or(serde_json::Value::Null);
                        assistant_blocks.push(ContentBlock::ToolUse {
                            execution_id: identity.execution_id.map(str::to_string),
                            id: identity.provider_id.to_string(),
                            name: tool_name,
                            input,
                            caller: meta
                                .and_then(|m| m.get("tool_caller"))
                                .map(|value| serde_json::from_value(value.clone()))
                                .transpose()
                                .context("Invalid stored tool caller")?,
                            thought_signature: meta
                                .and_then(|m| m.get("tool_thought_signature"))
                                .map(|value| {
                                    value
                                        .as_str()
                                        .map(str::to_string)
                                        .context("Invalid stored tool thought signature")
                                })
                                .transpose()?,
                        });
                    }
                    if let Some(identity) = result {
                        flush_assistant(&mut assistant_blocks, &mut messages);
                        let content = item.detail.clone().unwrap_or_default();
                        let is_error = meta
                            .and_then(|m| m.get("is_error"))
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        let content_blocks = meta
                            .and_then(|m| m.get("content_blocks"))
                            .and_then(Value::as_array)
                            .cloned();
                        user_blocks.push(ContentBlock::ToolResult {
                            execution_id: identity.execution_id.map(str::to_string),
                            tool_use_id: identity.provider_id.to_string(),
                            content,
                            is_error: if is_error { Some(true) } else { None },
                            content_blocks,
                        });
                    } else if let Some((identity, content)) = unanswered {
                        // The call above had no recorded outcome: pair it
                        // here, in the position the live transcript held
                        // its result, so the rebuilt turn is complete.
                        flush_assistant(&mut assistant_blocks, &mut messages);
                        user_blocks.push(ContentBlock::ToolResult {
                            execution_id: identity.execution_id.map(str::to_string),
                            tool_use_id: identity.provider_id.to_string(),
                            content,
                            is_error: Some(true),
                            content_blocks: None,
                        });
                    }
                }
                _ => {}
            }
        }
        flush_assistant(&mut assistant_blocks, &mut messages);
        flush_user(&mut user_blocks, &mut messages);
    }
    Ok(messages)
}

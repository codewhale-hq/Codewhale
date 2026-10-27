//! `EventMsg`-out API in `crates/protocol` (issue #5261, Phase A1 of the
//! core/protocol extraction spec).
//!
//! Mirrors `crates/tui/src/core/events::Event` variant-by-variant as a
//! serializable protocol. The TUI's `rx_event` / `Event` channel, the
//! app-server's SSE stream, and the CLI's `stream-json` output all speak this
//! one type so headless and TUI observe byte-identical event shapes for the
//! same `Op`.
//!
//! Parity is compile-enforced from the engine side:
//! `crates/tui/src/core/protocol_parity.rs` matches every engine `Event`
//! variant exhaustively into an `EventMsg` (`protocol_covers_engine_events`).
//! Adding an engine variant without a twin here fails to compile.
//!
//! Payload fidelity rules for this phase:
//!
//! - Scalars, ids, lifecycle enums, route/billing receipts, tool outcomes,
//!   MCP snapshots, approvals, and gate decisions are typed here.
//! - Deep domain payloads whose canonical serde type still lives above this
//!   crate (goal snapshots, sub-agent results, coordination projections,
//!   mailbox messages, transcript messages, tool catalogs, tool inspection
//!   snapshots, workflow UI events) cross as `serde_json::Value` produced by
//!   that type's own `Serialize`. They are typed in later phases; the variant
//!   and its field names are already stable.
//! - Engine-only handles (`oneshot`/`Notify`, `Arc<HookExecutor>`) never
//!   cross. A variant that carried one is projected without it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ResponseChannel;
use crate::UserInputQuestionEvent;
use crate::ids::{SessionId, ThreadId};

/// Final status for a turn (`TurnComplete.status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcomeStatus {
    Completed,
    Interrupted,
    Failed,
}

impl TurnOutcomeStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }
}

/// Token usage reported by a provider for one model call or one whole turn.
/// Field-for-field twin of the engine's `Usage`; `None` means the provider
/// did not report the fact, never zero.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_hit_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_miss_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_write_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_replay_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_execution_requests: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_search_requests: Option<u32>,
}

/// Secret-free proof of the base route a turn's client was installed on.
/// The credential generation digest is redacted by design on the engine side
/// and never crosses; only its presence does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRouteReceipt {
    pub provider: String,
    pub provider_identity: String,
    pub wire_model: String,
    pub endpoint_identity: String,
    pub credential_generation_present: bool,
}

/// Credential/pay-mode product truth captured at the client-freeze boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteProduct {
    /// No product fact was captured. Not a licence to guess.
    Unproven,
    /// Subscription-backed with this user-facing quota label.
    Subscription { label: String },
    /// Bills per token.
    Metered,
}

/// Billing evidence captured at application admission before the provider permit.
/// This does not attest network delivery. Absent before admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteBillingEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrouter_vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_surface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_fingerprint: Option<String>,
    /// Validated frozen provider-live quote serialized by the runtime owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_live_pricing: Option<Value>,
    /// `RouteBillingMode` in snake_case.
    pub billing_mode: String,
    pub dispatched_at: DateTime<Utc>,
}

/// Provider/model route resolved for a model-backed turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRoute {
    /// `ProviderKind` key (`deepseek`, `openai`, `custom`, ...).
    pub provider: String,
    /// Exact non-secret configured route key.
    pub provider_identity: String,
    pub model: String,
    pub auto_model: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<TurnRouteReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing: Option<RouteBillingEnvelope>,
    /// Endpoint the client was frozen against, verbatim. Empty when unknown.
    pub base_url: String,
    pub billing_product: RouteProduct,
}

/// Structured error surfaced by a tool execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolCallError {
    InvalidInput { message: String },
    MissingField { field: String },
    PathEscape { path: PathBuf },
    ExecutionFailed { message: String },
    Timeout { seconds: u64 },
    Cancelled { message: String },
    NotAvailable { message: String },
    PermissionDenied { message: String },
}

/// Outcome of a tool call: the engine's `Result<ToolResult, ToolError>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ToolCallOutcome {
    Ok {
        content: String,
        success: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    Err {
        error: ToolCallError,
    },
}

/// Lifecycle metadata paired with a human-readable `AgentProgress` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProgressActivity {
    /// `AgentWorkerStatus` in snake_case.
    pub worker_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

/// Receipt for an operator follow-up to a child agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SubAgentFollowUpOutcome {
    Ok {
        agent_id: String,
        target_agent_id: String,
        delivered: bool,
        resumed: bool,
        note: String,
    },
    Err {
        reason: String,
    },
}

/// One row of the receipts-only agent roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRosterRow {
    pub worker_id: String,
    pub display_name: String,
    pub model: String,
    /// Coarse rail state:
    /// `running | waiting | parked | done | failed | cancelled`.
    pub state: String,
    /// `AgentWorkerStatus` in snake_case.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub millis: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_microusd: Option<u64>,
    pub steps_taken: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    pub run_id: String,
}

/// One discovered MCP tool / resource / prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpDiscoveredItem {
    pub name: String,
    pub model_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One configured MCP server as seen by the engine-owned pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerSnapshot {
    pub name: String,
    pub enabled: bool,
    pub required: bool,
    pub transport: String,
    pub command_or_url: String,
    pub connect_timeout: u64,
    pub execute_timeout: u64,
    pub read_timeout: u64,
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `advertised | legacy_fallback | not_observed`.
    pub capability_metadata: String,
    pub tools: Vec<McpDiscoveredItem>,
    pub resources: Vec<McpDiscoveredItem>,
    pub prompts: Vec<McpDiscoveredItem>,
}

/// Engine-owned MCP pool snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpManagerSnapshot {
    pub config_path: PathBuf,
    pub config_exists: bool,
    pub reload_required: bool,
    pub servers: Vec<McpServerSnapshot>,
}

/// Structured clarification request (`request_user_input`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInputRequest {
    #[serde(default)]
    pub require_typed_answers: bool,
    pub questions: Vec<UserInputQuestionEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserInputClosedOutcome {
    Answered,
    Cancelled,
    TimedOut,
    Unavailable,
}

/// Which permission gate produced a `ToolGateDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolGate {
    AutoReviewDeterministic,
    AutoReviewGuardian,
}

/// What a permission gate decided for one proposed tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolGateVerdict {
    Allowed,
    Denied,
    Unavailable,
}

/// One event emitted by the core engine to every consumer (TUI, CLI,
/// app-server, tests). This is the `EventMsg`-out half of the `Op`-in /
/// `EventMsg`-out contract: a projection of every internal engine `Event`
/// variant plus the thread/session ids that route it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum EventMsg {
    /// A route compatibility check omitted tools from the provider request.
    ToolProjectionWarning {
        thread_id: ThreadId,
        session_id: SessionId,
        provider: String,
        omitted_tool_names: Vec<String>,
        omitted_tool_count: u64,
    },

    RequestImageWindow {
        thread_id: ThreadId,
        session_id: SessionId,
        provider: String,
        model: String,
        total: u64,
        elided: u64,
        cap: u64,
        cap_source: String,
        message: String,
    },

    /// Workspace snapshots (undo) are off for this workspace. `reason` is one
    /// rendered, localized line: the consequence, the gate that refused, and
    /// the recovery that actually lifts *that* gate (the size cap's config key
    /// appears only for the size gate).
    SnapshotsDisabled {
        thread_id: ThreadId,
        session_id: SessionId,
        workspace: String,
        reason: String,
    },

    // === Streaming ===
    MessageStarted {
        thread_id: ThreadId,
        session_id: SessionId,
        index: u64,
    },
    /// Incremental content delta on the `text` (message) or `reasoning`
    /// (thinking) channel.
    ResponseDelta {
        thread_id: ThreadId,
        session_id: SessionId,
        index: u64,
        delta: String,
        #[serde(default, skip_serializing_if = "ResponseChannel::is_text")]
        channel: ResponseChannel,
    },
    MessageComplete {
        thread_id: ThreadId,
        session_id: SessionId,
        index: u64,
    },
    ThinkingStarted {
        thread_id: ThreadId,
        session_id: SessionId,
        index: u64,
    },
    ThinkingComplete {
        thread_id: ThreadId,
        session_id: SessionId,
        index: u64,
    },

    // === Tools ===
    ToolCallStarted {
        thread_id: ThreadId,
        session_id: SessionId,
        tool_call_id: String,
        tool_name: String,
        input: Value,
    },
    /// Liveness pulse while a tool future remains pending. Carries no output.
    ToolCallHeartbeat {
        thread_id: ThreadId,
        session_id: SessionId,
    },
    ToolExecutionStarted {
        thread_id: ThreadId,
        session_id: SessionId,
        tool_call_id: String,
    },
    ToolResultContent {
        thread_id: ThreadId,
        session_id: SessionId,
        tool_call_id: String,
        blocks: Value,
    },
    ToolCallComplete {
        thread_id: ThreadId,
        session_id: SessionId,
        tool_call_id: String,
        tool_name: String,
        result: ToolCallOutcome,
    },
    /// Trusted Engine-owned activity for an operation that passed dispatch
    /// and authority checks. No arguments, command, or result is
    /// included in the pet-facing activity contract.
    OperationActivityStarted {
        thread_id: ThreadId,
        session_id: SessionId,
        span_id: String,
        activity_kind: crate::engine_owner::OwnerActivityKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_id: Option<String>,
    },
    OperationActivityCompleted {
        thread_id: ThreadId,
        session_id: SessionId,
        span_id: String,
        activity_kind: crate::engine_owner::OwnerActivityKind,
        outcome: crate::engine_owner::OwnerOperationOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action_id: Option<String>,
    },

    // === Turn lifecycle ===
    TurnStarted {
        thread_id: ThreadId,
        session_id: SessionId,
        turn_id: String,
        created_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route: Option<TurnRoute>,
        /// Host submission correlation echo: the token an in-process host
        /// stamped on its `SendMessage`/`EditLastTurn` op, echoed verbatim by
        /// that turn's start; `None` for every engine self-started turn.
        /// Additive and default-absent on the wire. Only in-process engine
        /// handles can stamp a token — the wire `Op` carries no correlation
        /// field — so wire submitters only ever observe `None` here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        submission_id: Option<String>,
    },
    /// Bounded tool-field projection from a prepared model-client request
    /// (`ToolInspectionSnapshot` serialized).
    ToolRequestSnapshot {
        thread_id: ThreadId,
        session_id: SessionId,
        snapshot: Value,
    },
    /// A workspace snapshot the engine took for the running turn
    /// (`WorkspaceSnapshotRef` serialized: `kind`, `snapshot_id`, `tree_id`,
    /// `session_id`, optional `tool_call_id`, `write_paths` and
    /// `changed_paths`).
    WorkspaceSnapshotTaken {
        thread_id: ThreadId,
        session_id: SessionId,
        snapshot: Value,
    },
    /// Immutable billing route captured at application admission.
    RouteDispatched {
        thread_id: ThreadId,
        session_id: SessionId,
        turn_id: String,
        route: TurnRoute,
    },
    TurnComplete {
        thread_id: ThreadId,
        session_id: SessionId,
        /// Actual started turn; absent when a submitted operation fails
        /// before starting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        /// Host submission correlation, including preflight rejection. Runtime
        /// self-starts do not carry a host token. Older producers default to
        /// absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        submission_id: Option<String>,
        /// Manual compaction operation identity, never the surrounding host
        /// submission.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        compaction_id: Option<String>,
        status: TurnOutcomeStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        usage: TokenUsage,
        /// Parent-route subset; absent in legacy events, whose split is unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_route_usage: Option<TokenUsage>,
        #[serde(default)]
        routed_usage_dropped_records: u64,
        /// Tool catalog sent with this turn's model request (`Tool` serialized).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_catalog: Option<Vec<Value>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        base_url: Option<String>,
    },
    ProviderRequestReceipt {
        thread_id: ThreadId,
        session_id: SessionId,
        receipt: Value,
    },

    /// Usage for one model call within the turn.
    TurnUsage {
        #[serde(
            default,
            rename = "maxOutputTokens",
            skip_serializing_if = "Option::is_none"
        )]
        max_output_tokens: Option<u32>,
        thread_id: ThreadId,
        session_id: SessionId,
        usage: TokenUsage,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_token_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_ms: Option<u64>,
    },

    /// Child-call telemetry; cost belongs to its own routed receipt.
    RoutedTurnUsage {
        thread_id: ThreadId,
        session_id: SessionId,
        usage: TokenUsage,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        first_token_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_ms: Option<u64>,
    },

    // === Goals ===
    /// Runtime goal state changed (`GoalSnapshot` serialized).
    GoalUpdated {
        thread_id: ThreadId,
        session_id: SessionId,
        snapshot: Value,
    },
    GoalContinuationWaiting {
        thread_id: ThreadId,
        session_id: SessionId,
        delay_seconds: u64,
    },
    GoalContinuationWaitEnded {
        thread_id: ThreadId,
        session_id: SessionId,
        interrupted: bool,
    },

    // === Compaction / purge ===
    CompactionStarted {
        thread_id: ThreadId,
        session_id: SessionId,
        id: String,
        auto: bool,
        message: String,
    },
    CompactionCompleted {
        thread_id: ThreadId,
        session_id: SessionId,
        id: String,
        auto: bool,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        messages_before: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        messages_after: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary_prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        post_input_tokens: Option<u64>,
    },
    CompactionCancelled {
        thread_id: ThreadId,
        session_id: SessionId,
        id: String,
        auto: bool,
        message: String,
    },
    CompactionFailed {
        thread_id: ThreadId,
        session_id: SessionId,
        id: String,
        auto: bool,
        message: String,
    },
    PurgeStarted {
        thread_id: ThreadId,
        session_id: SessionId,
        message: String,
    },
    PurgeCompleted {
        thread_id: ThreadId,
        session_id: SessionId,
        messages_before: u64,
        messages_after: u64,
        removed_count: u64,
        replaced_count: u64,
        message: String,
    },
    PurgeFailed {
        thread_id: ThreadId,
        session_id: SessionId,
        message: String,
    },

    // === Sub-agents ===
    AgentSpawned {
        thread_id: ThreadId,
        session_id: SessionId,
        owner_session_id: String,
        id: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker_status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_run_id: Option<String>,
        spawn_depth: u32,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route_source: Option<String>,
        /// The name the agent goes by (workflow task label, dispatch name, or
        /// role). Absent from older producers; never the raw id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
    },
    AgentProgress {
        thread_id: ThreadId,
        session_id: SessionId,
        owner_session_id: String,
        id: String,
        status: String,
        activity: AgentProgressActivity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_run_id: Option<String>,
        spawn_depth: u32,
    },
    AgentComplete {
        thread_id: ThreadId,
        session_id: SessionId,
        owner_session_id: String,
        id: String,
        result: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worker_status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_run_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spawn_depth: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuable: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_name: Option<String>,
    },
    SubAgentFollowUp {
        thread_id: ThreadId,
        session_id: SessionId,
        owner_session_id: String,
        agent_id: String,
        outcome: SubAgentFollowUpOutcome,
    },
    /// Sub-agent listing. `agents` are `SubAgentResult`s and `coordination`
    /// is the `CoordinationDetailProjection`, both serialized.
    AgentList {
        thread_id: ThreadId,
        session_id: SessionId,
        owner_session_id: String,
        agents: Vec<Value>,
        coordination: Value,
        /// `agent_id` -> queued follow-up count; only non-zero entries.
        #[serde(default)]
        queued_follow_ups: BTreeMap<String, u64>,
        roster: Vec<AgentRosterRow>,
    },
    /// Structured sub-agent mailbox envelope (`MailboxMessage` serialized).
    /// Deduplicate on `(turn_id, seq)`, never `seq` alone.
    SubAgentMailbox {
        thread_id: ThreadId,
        session_id: SessionId,
        owner_session_id: String,
        turn_id: String,
        seq: u64,
        message: Value,
    },
    /// Live workflow UI event. `ui_event` is the flattened
    /// `{"type": ..., "at_ms": ..., ...}` object (named `event` on the
    /// engine side; renamed here because `event` is the wire tag).
    WorkflowUi {
        thread_id: ThreadId,
        session_id: SessionId,
        owner_session_id: String,
        run_id: String,
        ui_event: Value,
    },

    // === System ===
    Error {
        thread_id: ThreadId,
        session_id: SessionId,
        /// `ErrorCategory` in snake_case.
        category: String,
        /// `ErrorSeverity` in snake_case.
        severity: String,
        recoverable: bool,
        code: String,
        message: String,
    },
    Status {
        thread_id: ThreadId,
        session_id: SessionId,
        message: String,
    },
    McpSessionBoot {
        thread_id: ThreadId,
        session_id: SessionId,
        generation: u64,
        snapshot: McpManagerSnapshot,
        connecting: Vec<String>,
        finished: bool,
    },
    /// Rendered `/preview-request` manifest.
    RequestManifestReady {
        thread_id: ThreadId,
        session_id: SessionId,
        rendered: String,
    },
    /// Pause terminal input events for an interactive subprocess. The engine's
    /// in-process acknowledgement handle does not cross the wire.
    PauseEvents {
        thread_id: ThreadId,
        session_id: SessionId,
    },
    ResumeEvents {
        thread_id: ThreadId,
        session_id: SessionId,
    },
    ApprovalRequired {
        thread_id: ThreadId,
        session_id: SessionId,
        id: String,
        tool_name: String,
        description: String,
        input: Value,
        approval_key: String,
        approval_grouping_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent_summary: Option<String>,
        approval_force_prompt: bool,
    },
    ApprovalWithdrawn {
        thread_id: ThreadId,
        session_id: SessionId,
        id: String,
    },
    UserInputRequired {
        thread_id: ThreadId,
        session_id: SessionId,
        id: String,
        request: UserInputRequest,
    },
    UserInputClosed {
        thread_id: ThreadId,
        session_id: SessionId,
        id: String,
        outcome: UserInputClosedOutcome,
    },
    /// Authoritative API conversation state (`Message`s / `SystemPrompt`
    /// serialized).
    SessionUpdated {
        thread_id: ThreadId,
        session_id: SessionId,
        engine_session_id: String,
        messages: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instruction_assembly: Option<Value>,
        model: String,
        workspace: PathBuf,
    },
    ElevationRequired {
        thread_id: ThreadId,
        session_id: SessionId,
        tool_id: String,
        tool_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command: Option<String>,
        denial_reason: String,
        blocked_network: bool,
        blocked_write: bool,
    },
    LspRepairUpdate {
        thread_id: ThreadId,
        session_id: SessionId,
        diagnostics_found: u64,
        files: u64,
        injected: bool,
    },
    ToolGateDecision {
        thread_id: ThreadId,
        session_id: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<String>,
        tool_id: String,
        tool_name: String,
        gate: ToolGate,
        decision: ToolGateVerdict,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        risk: Option<String>,
        reason: String,
    },
    AdvisoryNote {
        thread_id: ThreadId,
        session_id: SessionId,
        turn_id: String,
        note: String,
        tool_call_count: u32,
    },

    // === Prefix cache ===
    PrefixCacheChange {
        thread_id: ThreadId,
        session_id: SessionId,
        description: String,
        system_prompt_changed: bool,
        tools_changed: bool,
        stability_pct: u32,
        changed: bool,
        pinned_combined_hash: String,
        pin_reason: String,
        last_miss_reason: String,
        context_updates: u64,
    },
}

/// Envelope that carries an `EventMsg` over the wire / channel with a
/// monotonic seq so consumers can detect drops. Mirrors the existing
/// `RuntimeEventEnvelope` but typed to `EventMsg`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: u64,
    pub thread_id: ThreadId,
    pub session_id: SessionId,
    pub turn_id: Option<String>,
    pub event: EventMsg,
}

/// Every wire tag `EventMsg` can carry, in declaration order. Kept next to
/// the enum so a new variant is added here in the same edit; the test below
/// proves the list and `kind_str` agree.
pub const EVENT_KINDS: &[&str] = &[
    "tool_projection_warning",
    "request_image_window",
    "message_started",
    "response_delta",
    "message_complete",
    "thinking_started",
    "thinking_complete",
    "tool_call_started",
    "tool_call_heartbeat",
    "tool_call_complete",
    "operation_activity_started",
    "operation_activity_completed",
    "turn_started",
    "tool_request_snapshot",
    "workspace_snapshot_taken",
    "route_dispatched",
    "turn_complete",
    "turn_usage",
    "routed_turn_usage",
    "goal_updated",
    "goal_continuation_waiting",
    "goal_continuation_wait_ended",
    "compaction_started",
    "compaction_completed",
    "compaction_cancelled",
    "compaction_failed",
    "purge_started",
    "purge_completed",
    "purge_failed",
    "agent_spawned",
    "agent_progress",
    "agent_complete",
    "sub_agent_follow_up",
    "agent_list",
    "sub_agent_mailbox",
    "workflow_ui",
    "error",
    "status",
    "mcp_session_boot",
    "request_manifest_ready",
    "pause_events",
    "resume_events",
    "approval_required",
    "approval_withdrawn",
    "user_input_required",
    "user_input_closed",
    "session_updated",
    "elevation_required",
    "lsp_repair_update",
    "tool_gate_decision",
    "advisory_note",
    "prefix_cache_change",
];

impl EventMsg {
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::ToolProjectionWarning { .. } => "tool_projection_warning",
            Self::RequestImageWindow { .. } => "request_image_window",
            Self::SnapshotsDisabled { .. } => "snapshots_disabled",
            Self::MessageStarted { .. } => "message_started",
            Self::ResponseDelta { .. } => "response_delta",
            Self::MessageComplete { .. } => "message_complete",
            Self::ThinkingStarted { .. } => "thinking_started",
            Self::ThinkingComplete { .. } => "thinking_complete",
            Self::ToolCallStarted { .. } => "tool_call_started",
            Self::ToolCallHeartbeat { .. } => "tool_call_heartbeat",
            Self::ToolExecutionStarted { .. } => "tool_execution_started",
            Self::ToolResultContent { .. } => "tool_result_content",
            Self::ToolCallComplete { .. } => "tool_call_complete",
            Self::OperationActivityStarted { .. } => "operation_activity_started",
            Self::OperationActivityCompleted { .. } => "operation_activity_completed",
            Self::TurnStarted { .. } => "turn_started",
            Self::ToolRequestSnapshot { .. } => "tool_request_snapshot",
            Self::WorkspaceSnapshotTaken { .. } => "workspace_snapshot_taken",
            Self::RouteDispatched { .. } => "route_dispatched",
            Self::TurnComplete { .. } => "turn_complete",
            Self::ProviderRequestReceipt { .. } => "provider_request_receipt",
            Self::TurnUsage { .. } => "turn_usage",
            Self::RoutedTurnUsage { .. } => "routed_turn_usage",
            Self::GoalUpdated { .. } => "goal_updated",
            Self::GoalContinuationWaiting { .. } => "goal_continuation_waiting",
            Self::GoalContinuationWaitEnded { .. } => "goal_continuation_wait_ended",
            Self::CompactionStarted { .. } => "compaction_started",
            Self::CompactionCompleted { .. } => "compaction_completed",
            Self::CompactionCancelled { .. } => "compaction_cancelled",
            Self::CompactionFailed { .. } => "compaction_failed",
            Self::PurgeStarted { .. } => "purge_started",
            Self::PurgeCompleted { .. } => "purge_completed",
            Self::PurgeFailed { .. } => "purge_failed",
            Self::AgentSpawned { .. } => "agent_spawned",
            Self::AgentProgress { .. } => "agent_progress",
            Self::AgentComplete { .. } => "agent_complete",
            Self::SubAgentFollowUp { .. } => "sub_agent_follow_up",
            Self::AgentList { .. } => "agent_list",
            Self::SubAgentMailbox { .. } => "sub_agent_mailbox",
            Self::WorkflowUi { .. } => "workflow_ui",
            Self::Error { .. } => "error",
            Self::Status { .. } => "status",
            Self::McpSessionBoot { .. } => "mcp_session_boot",
            Self::RequestManifestReady { .. } => "request_manifest_ready",
            Self::PauseEvents { .. } => "pause_events",
            Self::ResumeEvents { .. } => "resume_events",
            Self::ApprovalRequired { .. } => "approval_required",
            Self::ApprovalWithdrawn { .. } => "approval_withdrawn",
            Self::UserInputRequired { .. } => "user_input_required",
            Self::UserInputClosed { .. } => "user_input_closed",
            Self::SessionUpdated { .. } => "session_updated",
            Self::ElevationRequired { .. } => "elevation_required",
            Self::LspRepairUpdate { .. } => "lsp_repair_update",
            Self::ToolGateDecision { .. } => "tool_gate_decision",
            Self::AdvisoryNote { .. } => "advisory_note",
            Self::PrefixCacheChange { .. } => "prefix_cache_change",
        }
    }

    #[must_use]
    pub fn thread_id(&self) -> &ThreadId {
        match self {
            Self::ToolProjectionWarning { thread_id, .. }
            | Self::RequestImageWindow { thread_id, .. }
            | Self::SnapshotsDisabled { thread_id, .. }
            | Self::MessageStarted { thread_id, .. }
            | Self::ResponseDelta { thread_id, .. }
            | Self::MessageComplete { thread_id, .. }
            | Self::ThinkingStarted { thread_id, .. }
            | Self::ThinkingComplete { thread_id, .. }
            | Self::ToolCallStarted { thread_id, .. }
            | Self::ToolCallHeartbeat { thread_id, .. }
            | Self::ToolExecutionStarted { thread_id, .. }
            | Self::ToolResultContent { thread_id, .. }
            | Self::ToolCallComplete { thread_id, .. }
            | Self::OperationActivityStarted { thread_id, .. }
            | Self::OperationActivityCompleted { thread_id, .. }
            | Self::TurnStarted { thread_id, .. }
            | Self::ToolRequestSnapshot { thread_id, .. }
            | Self::WorkspaceSnapshotTaken { thread_id, .. }
            | Self::RouteDispatched { thread_id, .. }
            | Self::TurnComplete { thread_id, .. }
            | Self::ProviderRequestReceipt { thread_id, .. }
            | Self::TurnUsage { thread_id, .. }
            | Self::RoutedTurnUsage { thread_id, .. }
            | Self::GoalUpdated { thread_id, .. }
            | Self::GoalContinuationWaiting { thread_id, .. }
            | Self::GoalContinuationWaitEnded { thread_id, .. }
            | Self::CompactionStarted { thread_id, .. }
            | Self::CompactionCompleted { thread_id, .. }
            | Self::CompactionCancelled { thread_id, .. }
            | Self::CompactionFailed { thread_id, .. }
            | Self::PurgeStarted { thread_id, .. }
            | Self::PurgeCompleted { thread_id, .. }
            | Self::PurgeFailed { thread_id, .. }
            | Self::AgentSpawned { thread_id, .. }
            | Self::AgentProgress { thread_id, .. }
            | Self::AgentComplete { thread_id, .. }
            | Self::SubAgentFollowUp { thread_id, .. }
            | Self::AgentList { thread_id, .. }
            | Self::SubAgentMailbox { thread_id, .. }
            | Self::WorkflowUi { thread_id, .. }
            | Self::Error { thread_id, .. }
            | Self::Status { thread_id, .. }
            | Self::McpSessionBoot { thread_id, .. }
            | Self::RequestManifestReady { thread_id, .. }
            | Self::PauseEvents { thread_id, .. }
            | Self::ResumeEvents { thread_id, .. }
            | Self::ApprovalRequired { thread_id, .. }
            | Self::ApprovalWithdrawn { thread_id, .. }
            | Self::UserInputRequired { thread_id, .. }
            | Self::UserInputClosed { thread_id, .. }
            | Self::SessionUpdated { thread_id, .. }
            | Self::ElevationRequired { thread_id, .. }
            | Self::LspRepairUpdate { thread_id, .. }
            | Self::ToolGateDecision { thread_id, .. }
            | Self::AdvisoryNote { thread_id, .. }
            | Self::PrefixCacheChange { thread_id, .. } => thread_id,
        }
    }

    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        match self {
            Self::ToolProjectionWarning { session_id, .. }
            | Self::RequestImageWindow { session_id, .. }
            | Self::SnapshotsDisabled { session_id, .. }
            | Self::MessageStarted { session_id, .. }
            | Self::ResponseDelta { session_id, .. }
            | Self::MessageComplete { session_id, .. }
            | Self::ThinkingStarted { session_id, .. }
            | Self::ThinkingComplete { session_id, .. }
            | Self::ToolCallStarted { session_id, .. }
            | Self::ToolCallHeartbeat { session_id, .. }
            | Self::ToolExecutionStarted { session_id, .. }
            | Self::ToolResultContent { session_id, .. }
            | Self::ToolCallComplete { session_id, .. }
            | Self::OperationActivityStarted { session_id, .. }
            | Self::OperationActivityCompleted { session_id, .. }
            | Self::TurnStarted { session_id, .. }
            | Self::ToolRequestSnapshot { session_id, .. }
            | Self::WorkspaceSnapshotTaken { session_id, .. }
            | Self::RouteDispatched { session_id, .. }
            | Self::TurnComplete { session_id, .. }
            | Self::ProviderRequestReceipt { session_id, .. }
            | Self::TurnUsage { session_id, .. }
            | Self::RoutedTurnUsage { session_id, .. }
            | Self::GoalUpdated { session_id, .. }
            | Self::GoalContinuationWaiting { session_id, .. }
            | Self::GoalContinuationWaitEnded { session_id, .. }
            | Self::CompactionStarted { session_id, .. }
            | Self::CompactionCompleted { session_id, .. }
            | Self::CompactionCancelled { session_id, .. }
            | Self::CompactionFailed { session_id, .. }
            | Self::PurgeStarted { session_id, .. }
            | Self::PurgeCompleted { session_id, .. }
            | Self::PurgeFailed { session_id, .. }
            | Self::AgentSpawned { session_id, .. }
            | Self::AgentProgress { session_id, .. }
            | Self::AgentComplete { session_id, .. }
            | Self::SubAgentFollowUp { session_id, .. }
            | Self::AgentList { session_id, .. }
            | Self::SubAgentMailbox { session_id, .. }
            | Self::WorkflowUi { session_id, .. }
            | Self::Error { session_id, .. }
            | Self::Status { session_id, .. }
            | Self::McpSessionBoot { session_id, .. }
            | Self::RequestManifestReady { session_id, .. }
            | Self::PauseEvents { session_id, .. }
            | Self::ResumeEvents { session_id, .. }
            | Self::ApprovalRequired { session_id, .. }
            | Self::ApprovalWithdrawn { session_id, .. }
            | Self::UserInputRequired { session_id, .. }
            | Self::UserInputClosed { session_id, .. }
            | Self::SessionUpdated { session_id, .. }
            | Self::ElevationRequired { session_id, .. }
            | Self::LspRepairUpdate { session_id, .. }
            | Self::ToolGateDecision { session_id, .. }
            | Self::AdvisoryNote { session_id, .. }
            | Self::PrefixCacheChange { session_id, .. } => session_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids() -> (ThreadId, SessionId) {
        (ThreadId::new(), SessionId::new())
    }

    /// One instance of every variant, in declaration order. A new variant
    /// must be added here too, or `every_variant_is_listed_once` fails.
    fn every_variant() -> Vec<EventMsg> {
        let (t, s) = ids();
        let usage = TokenUsage {
            input_tokens: 1,
            output_tokens: 2,
            ..TokenUsage::default()
        };
        let route = TurnRoute {
            provider: "deepseek".into(),
            provider_identity: "deepseek".into(),
            model: "deepseek-chat".into(),
            auto_model: false,
            receipt: Some(TurnRouteReceipt {
                provider: "deepseek".into(),
                provider_identity: "deepseek".into(),
                wire_model: "deepseek-chat".into(),
                endpoint_identity: "api.deepseek.com".into(),
                credential_generation_present: true,
            }),
            billing: Some(RouteBillingEnvelope {
                openrouter_vendor: None,
                billing_surface: None,
                endpoint_fingerprint: Some("fp".into()),
                provider_live_pricing: None,
                billing_mode: "metered".into(),
                dispatched_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
            }),
            base_url: "https://api.deepseek.com".into(),
            billing_product: RouteProduct::Subscription {
                label: "pro".into(),
            },
        };
        vec![
            EventMsg::ToolProjectionWarning {
                thread_id: t.clone(),
                session_id: s.clone(),
                provider: "openai".into(),
                omitted_tool_names: vec!["a".into()],
                omitted_tool_count: 3,
            },
            EventMsg::RequestImageWindow {
                thread_id: t.clone(),
                session_id: s.clone(),
                provider: "vllm".into(),
                model: "m".into(),
                total: 6,
                elided: 3,
                cap: 3,
                cap_source: "provider_rejection".into(),
                message: "m".into(),
            },
            EventMsg::MessageStarted {
                thread_id: t.clone(),
                session_id: s.clone(),
                index: 0,
            },
            EventMsg::ResponseDelta {
                thread_id: t.clone(),
                session_id: s.clone(),
                index: 0,
                delta: "hi".into(),
                channel: ResponseChannel::Reasoning,
            },
            EventMsg::MessageComplete {
                thread_id: t.clone(),
                session_id: s.clone(),
                index: 0,
            },
            EventMsg::ThinkingStarted {
                thread_id: t.clone(),
                session_id: s.clone(),
                index: 1,
            },
            EventMsg::ThinkingComplete {
                thread_id: t.clone(),
                session_id: s.clone(),
                index: 1,
            },
            EventMsg::ToolCallStarted {
                thread_id: t.clone(),
                session_id: s.clone(),
                tool_call_id: "c1".into(),
                tool_name: "read_file".into(),
                input: json!({"path": "x"}),
            },
            EventMsg::ToolCallHeartbeat {
                thread_id: t.clone(),
                session_id: s.clone(),
            },
            EventMsg::ToolCallComplete {
                thread_id: t.clone(),
                session_id: s.clone(),
                tool_call_id: "c1".into(),
                tool_name: "read_file".into(),
                result: ToolCallOutcome::Err {
                    error: ToolCallError::Timeout { seconds: 3 },
                },
            },
            EventMsg::OperationActivityStarted {
                thread_id: t.clone(),
                session_id: s.clone(),
                span_id: "span-1".into(),
                activity_kind: crate::engine_owner::OwnerActivityKind::Reading,
                action_id: Some("read_file".into()),
            },
            EventMsg::OperationActivityCompleted {
                thread_id: t.clone(),
                session_id: s.clone(),
                span_id: "span-1".into(),
                activity_kind: crate::engine_owner::OwnerActivityKind::Reading,
                action_id: Some("read_file".into()),
                outcome: crate::engine_owner::OwnerOperationOutcome::Succeeded,
            },
            EventMsg::TurnStarted {
                thread_id: t.clone(),
                session_id: s.clone(),
                turn_id: "turn-1".into(),
                created_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
                route: Some(route.clone()),
                submission_id: None,
            },
            EventMsg::ToolRequestSnapshot {
                thread_id: t.clone(),
                session_id: s.clone(),
                snapshot: json!({"tool_count": 2}),
            },
            EventMsg::WorkspaceSnapshotTaken {
                thread_id: t.clone(),
                session_id: s.clone(),
                snapshot: json!({"kind": "pre_turn", "tree_id": "t"}),
            },
            EventMsg::RouteDispatched {
                thread_id: t.clone(),
                session_id: s.clone(),
                turn_id: "turn-1".into(),
                route,
            },
            EventMsg::TurnComplete {
                submission_id: None,
                compaction_id: None,
                thread_id: t.clone(),
                session_id: s.clone(),
                turn_id: None,
                status: TurnOutcomeStatus::Failed,
                error: Some("boom".into()),
                usage: usage.clone(),
                parent_route_usage: Some(usage.clone()),
                routed_usage_dropped_records: 0,
                tool_catalog: Some(vec![json!({"name": "read_file"})]),
                base_url: None,
            },
            EventMsg::TurnUsage {
                max_output_tokens: None,
                thread_id: t.clone(),
                session_id: s.clone(),
                usage: usage.clone(),
                duration_ms: 10,
                first_token_ms: Some(2),
                request_ms: None,
            },
            EventMsg::RoutedTurnUsage {
                thread_id: t.clone(),
                session_id: s.clone(),
                usage,
                duration_ms: 10,
                first_token_ms: Some(2),
                request_ms: None,
            },
            EventMsg::GoalUpdated {
                thread_id: t.clone(),
                session_id: s.clone(),
                snapshot: json!({"status": "active"}),
            },
            EventMsg::GoalContinuationWaiting {
                thread_id: t.clone(),
                session_id: s.clone(),
                delay_seconds: 5,
            },
            EventMsg::GoalContinuationWaitEnded {
                thread_id: t.clone(),
                session_id: s.clone(),
                interrupted: true,
            },
            EventMsg::CompactionStarted {
                thread_id: t.clone(),
                session_id: s.clone(),
                id: "cmp-1".into(),
                auto: true,
                message: "m".into(),
            },
            EventMsg::CompactionCompleted {
                thread_id: t.clone(),
                session_id: s.clone(),
                id: "cmp-1".into(),
                auto: true,
                message: "m".into(),
                messages_before: Some(10),
                messages_after: Some(2),
                summary_prompt: None,
                post_input_tokens: Some(100),
            },
            EventMsg::CompactionCancelled {
                thread_id: t.clone(),
                session_id: s.clone(),
                id: "cmp-1".into(),
                auto: false,
                message: "m".into(),
            },
            EventMsg::CompactionFailed {
                thread_id: t.clone(),
                session_id: s.clone(),
                id: "cmp-1".into(),
                auto: false,
                message: "m".into(),
            },
            EventMsg::PurgeStarted {
                thread_id: t.clone(),
                session_id: s.clone(),
                message: "m".into(),
            },
            EventMsg::PurgeCompleted {
                thread_id: t.clone(),
                session_id: s.clone(),
                messages_before: 4,
                messages_after: 2,
                removed_count: 2,
                replaced_count: 0,
                message: "m".into(),
            },
            EventMsg::PurgeFailed {
                thread_id: t.clone(),
                session_id: s.clone(),
                message: "m".into(),
            },
            EventMsg::AgentSpawned {
                thread_id: t.clone(),
                session_id: s.clone(),
                owner_session_id: "owner".into(),
                id: "a1".into(),
                prompt: "p".into(),
                worker_status: Some("starting".into()),
                parent_run_id: None,
                spawn_depth: 1,
                model: "m".into(),
                route_source: Some("task.model".into()),
                display_name: Some("audit docs".into()),
            },
            EventMsg::AgentProgress {
                thread_id: t.clone(),
                session_id: s.clone(),
                owner_session_id: "owner".into(),
                id: "a1".into(),
                status: "running".into(),
                activity: AgentProgressActivity {
                    worker_status: "running_tool".into(),
                    step: Some(2),
                    tool_name: Some("bash".into()),
                },
                parent_run_id: None,
                spawn_depth: 1,
            },
            EventMsg::AgentComplete {
                thread_id: t.clone(),
                session_id: s.clone(),
                owner_session_id: "owner".into(),
                id: "a1".into(),
                result: "done".into(),
                worker_status: Some("completed".into()),
                parent_run_id: None,
                spawn_depth: Some(1),
                continuable: Some(false),
                display_name: Some("audit docs".into()),
            },
            EventMsg::SubAgentFollowUp {
                thread_id: t.clone(),
                session_id: s.clone(),
                owner_session_id: "owner".into(),
                agent_id: "a1".into(),
                outcome: SubAgentFollowUpOutcome::Ok {
                    agent_id: "a1".into(),
                    target_agent_id: "a2".into(),
                    delivered: false,
                    resumed: true,
                    note: "resumed".into(),
                },
            },
            EventMsg::AgentList {
                thread_id: t.clone(),
                session_id: s.clone(),
                owner_session_id: "owner".into(),
                agents: vec![json!({"id": "a1"})],
                coordination: json!({}),
                queued_follow_ups: BTreeMap::from([("a1".to_string(), 1)]),
                roster: vec![AgentRosterRow {
                    worker_id: "w1".into(),
                    display_name: "scout".into(),
                    model: "m".into(),
                    state: "running".into(),
                    status: "running".into(),
                    activity: None,
                    millis: Some(5),
                    input_tokens: None,
                    output_tokens: None,
                    cost_microusd: None,
                    steps_taken: 1,
                    parent_run_id: None,
                    run_id: "r1".into(),
                }],
            },
            EventMsg::SubAgentMailbox {
                thread_id: t.clone(),
                session_id: s.clone(),
                owner_session_id: "owner".into(),
                turn_id: "turn-1".into(),
                seq: 7,
                message: json!({"kind": "spawned"}),
            },
            EventMsg::WorkflowUi {
                thread_id: t.clone(),
                session_id: s.clone(),
                owner_session_id: "owner".into(),
                run_id: "run-1".into(),
                ui_event: json!({"type": "task_started"}),
            },
            EventMsg::Error {
                thread_id: t.clone(),
                session_id: s.clone(),
                category: "network".into(),
                severity: "error".into(),
                recoverable: true,
                code: "E1".into(),
                message: "m".into(),
            },
            EventMsg::Status {
                thread_id: t.clone(),
                session_id: s.clone(),
                message: "m".into(),
            },
            EventMsg::McpSessionBoot {
                thread_id: t.clone(),
                session_id: s.clone(),
                generation: 1,
                snapshot: McpManagerSnapshot {
                    config_path: PathBuf::from("/tmp/mcp.json"),
                    config_exists: true,
                    reload_required: false,
                    servers: vec![McpServerSnapshot {
                        name: "fs".into(),
                        enabled: true,
                        required: false,
                        transport: "stdio".into(),
                        command_or_url: "npx".into(),
                        connect_timeout: 1,
                        execute_timeout: 2,
                        read_timeout: 3,
                        connected: true,
                        error: None,
                        capability_metadata: "advertised".into(),
                        tools: vec![McpDiscoveredItem {
                            name: "read".into(),
                            model_name: "fs__read".into(),
                            description: None,
                        }],
                        resources: vec![],
                        prompts: vec![],
                    }],
                },
                connecting: vec!["slow".into()],
                finished: false,
            },
            EventMsg::RequestManifestReady {
                thread_id: t.clone(),
                session_id: s.clone(),
                rendered: "manifest".into(),
            },
            EventMsg::PauseEvents {
                thread_id: t.clone(),
                session_id: s.clone(),
            },
            EventMsg::ResumeEvents {
                thread_id: t.clone(),
                session_id: s.clone(),
            },
            EventMsg::ApprovalRequired {
                thread_id: t.clone(),
                session_id: s.clone(),
                id: "c1".into(),
                tool_name: "bash".into(),
                description: "rm".into(),
                input: json!({"command": "rm"}),
                approval_key: "k".into(),
                approval_grouping_key: "g".into(),
                intent_summary: None,
                approval_force_prompt: true,
            },
            EventMsg::ApprovalWithdrawn {
                thread_id: t.clone(),
                session_id: s.clone(),
                id: "c1".into(),
            },
            EventMsg::UserInputRequired {
                thread_id: t.clone(),
                session_id: s.clone(),
                id: "c2".into(),
                request: UserInputRequest {
                    require_typed_answers: true,
                    questions: vec![UserInputQuestionEvent {
                        header: "h".into(),
                        id: "q1".into(),
                        question: "?".into(),
                        detail: Some("Exact detail".into()),
                        intent: Some(crate::UserInputIntentEvent::PlanReview {
                            approve: "Proceed".into(),
                            call_id: Some("plan-1".into()),
                        }),
                        options: vec![],
                        allow_free_text: true,
                        multi_select: false,
                    }],
                },
            },
            EventMsg::UserInputClosed {
                thread_id: t.clone(),
                session_id: s.clone(),
                id: "c2".into(),
                outcome: UserInputClosedOutcome::TimedOut,
            },
            EventMsg::SessionUpdated {
                thread_id: t.clone(),
                session_id: s.clone(),
                engine_session_id: "sess".into(),
                messages: vec![json!({"role": "user", "content": []})],
                system_prompt: Some(json!("sys")),
                instruction_assembly: None,
                model: "m".into(),
                workspace: PathBuf::from("/ws"),
            },
            EventMsg::ElevationRequired {
                thread_id: t.clone(),
                session_id: s.clone(),
                tool_id: "c3".into(),
                tool_name: "bash".into(),
                command: Some("curl".into()),
                denial_reason: "net".into(),
                blocked_network: true,
                blocked_write: false,
            },
            EventMsg::LspRepairUpdate {
                thread_id: t.clone(),
                session_id: s.clone(),
                diagnostics_found: 1,
                files: 1,
                injected: true,
            },
            EventMsg::ToolGateDecision {
                thread_id: t.clone(),
                session_id: s.clone(),
                agent_id: None,
                tool_id: "c4".into(),
                tool_name: "bash".into(),
                gate: ToolGate::AutoReviewGuardian,
                decision: ToolGateVerdict::Denied,
                risk: Some("high".into()),
                reason: "no".into(),
            },
            EventMsg::AdvisoryNote {
                thread_id: t.clone(),
                session_id: s.clone(),
                turn_id: "turn-1".into(),
                note: "n".into(),
                tool_call_count: 2,
            },
            EventMsg::PrefixCacheChange {
                thread_id: t,
                session_id: s,
                description: "d".into(),
                system_prompt_changed: false,
                tools_changed: true,
                stability_pct: 90,
                changed: true,
                pinned_combined_hash: "h".into(),
                pin_reason: "initial".into(),
                last_miss_reason: String::new(),
                context_updates: 0,
            },
        ]
    }

    #[test]
    fn every_variant_is_listed_once() {
        let kinds: Vec<&str> = every_variant().iter().map(EventMsg::kind_str).collect();
        assert_eq!(
            kinds, EVENT_KINDS,
            "EVENT_KINDS must list every variant in order"
        );
    }

    #[test]
    fn every_variant_round_trips_and_tags_by_event() {
        for msg in every_variant() {
            let value = serde_json::to_value(&msg).unwrap();
            assert_eq!(value["event"], msg.kind_str(), "{msg:?}");
            assert_eq!(value["thread_id"], msg.thread_id().to_string(), "{msg:?}");
            assert_eq!(value["session_id"], msg.session_id().to_string(), "{msg:?}");
            let back: EventMsg = serde_json::from_value(value).unwrap();
            assert_eq!(back, msg);
        }
    }

    #[test]
    fn agent_events_from_producers_without_a_display_name_still_load() {
        // #6565 added `display_name`; payloads written before it omit it.
        let mut every = every_variant();
        every.retain(|msg| {
            matches!(
                msg,
                EventMsg::AgentSpawned { .. } | EventMsg::AgentComplete { .. }
            )
        });
        assert_eq!(every.len(), 2);
        for msg in every {
            let mut value = serde_json::to_value(&msg).unwrap();
            assert_eq!(value["display_name"], "audit docs");
            value.as_object_mut().unwrap().remove("display_name");
            let back: EventMsg = serde_json::from_value(value.clone()).unwrap();
            match &back {
                EventMsg::AgentSpawned { display_name, .. }
                | EventMsg::AgentComplete { display_name, .. } => assert_eq!(*display_name, None),
                other => panic!("unexpected {other:?}"),
            }
            assert!(
                serde_json::to_value(&back)
                    .unwrap()
                    .get("display_name")
                    .is_none()
            );
        }
    }

    #[test]
    fn event_msg_roundtrip() {
        let msg = EventMsg::TurnComplete {
            submission_id: None,
            compaction_id: None,
            thread_id: ThreadId::new(),
            session_id: SessionId::new(),
            turn_id: Some("turn-1".into()),
            status: TurnOutcomeStatus::Completed,
            error: None,
            usage: TokenUsage::default(),
            parent_route_usage: None,
            routed_usage_dropped_records: 0,
            tool_catalog: None,
            base_url: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: EventMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind_str(), "turn_complete");
        assert!(json.contains(r#""status":"completed""#));
    }

    #[test]
    fn turn_complete_ownership_is_additive_and_round_trips() {
        for (turn_id, submission_id, compaction_id) in [
            (Some("turn-1"), Some("sub-host-1"), None),
            (None, Some("sub-rejected"), None),
            (Some("autonomous-turn"), None, None),
            (None, None, Some("compact-1")),
            (None, None, None),
        ] {
            let msg = EventMsg::TurnComplete {
                thread_id: ThreadId::new(),
                session_id: SessionId::new(),
                turn_id: turn_id.map(str::to_string),
                submission_id: submission_id.map(str::to_string),
                compaction_id: compaction_id.map(str::to_string),
                status: TurnOutcomeStatus::Failed,
                error: Some("fixture rejection".into()),
                usage: TokenUsage::default(),
                parent_route_usage: None,
                routed_usage_dropped_records: 0,
                tool_catalog: None,
                base_url: None,
            };
            let value = serde_json::to_value(&msg).unwrap();
            for (key, expected) in [
                ("turn_id", turn_id),
                ("submission_id", submission_id),
                ("compaction_id", compaction_id),
            ] {
                if let Some(expected) = expected {
                    assert_eq!(value[key], expected);
                } else {
                    assert!(value.get(key).is_none(), "{key} must stay absent");
                }
            }
            let back: EventMsg = serde_json::from_value(value).unwrap();
            assert_eq!(
                back, msg,
                "legacy absent fields and owned receipts round-trip"
            );
        }
    }

    #[test]
    fn text_channel_is_elided_on_the_wire() {
        let msg = EventMsg::ResponseDelta {
            thread_id: ThreadId::new(),
            session_id: SessionId::new(),
            index: 0,
            delta: "x".into(),
            channel: ResponseChannel::Text,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("channel"), "{json}");
        let back: EventMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    /// A host-stamped `submission_id` crosses the wire verbatim, a `None`
    /// token stays absent, and a payload from a producer that predates the
    /// field still deserializes (`serde(default)`).
    #[test]
    fn turn_started_submission_id_is_additive_and_default_absent() {
        let msg = EventMsg::TurnStarted {
            thread_id: ThreadId::new(),
            session_id: SessionId::new(),
            turn_id: "turn-1".into(),
            created_at: DateTime::<Utc>::from_timestamp(1, 0).unwrap(),
            route: None,
            submission_id: Some("sub-host-1".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.contains(r#""submission_id":"sub-host-1""#),
            "a host-stamped token must be serialized: {json}"
        );
        let back: EventMsg = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);

        let none = EventMsg::TurnStarted {
            thread_id: ThreadId::new(),
            session_id: SessionId::new(),
            turn_id: "turn-2".into(),
            created_at: DateTime::<Utc>::from_timestamp(2, 0).unwrap(),
            route: None,
            submission_id: None,
        };
        let value = serde_json::to_value(&none).unwrap();
        assert!(
            value.get("submission_id").is_none(),
            "a self-started turn's None token must stay absent on the wire: {value}"
        );
        // An older producer that predates the field: the absent key defaults.
        let back: EventMsg = serde_json::from_value(value).unwrap();
        assert_eq!(back, none);
    }
}

//! Operations submitted by the UI to the core engine.
//!
//! These operations flow from the TUI to the engine via a channel,
//! allowing the UI to remain responsive while the engine processes requests.

use crate::compaction::CompactionConfig;
use crate::route_runtime::ResolvedRuntimeRoute;
use crate::tools::goal::GoalStatus;
use codewhale_config::AppMode;
use codewhale_config::ProviderKind;
use codewhale_execpolicy::ApprovalMode;
use codewhale_models::{Message, SystemPrompt};
use codewhale_protocol::runtime::DynamicToolSpec;
use std::path::PathBuf;

/// Prefix used for tool-call ids created by local composer shell shortcuts.
pub const USER_SHELL_TOOL_ID_PREFIX: &str = "user_shell_";

pub type FleetMemberStarted = Result<String, String>;

/// Snapshot of session state for saving to disk.
/// Returned by `Op::GetSessionSnapshot` via a oneshot channel.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SessionSnapshot {
    /// The live conversation id this engine session is running under.
    ///
    /// Every workspace snapshot the conversation takes is tagged with it. A
    /// Runtime thread's engine runs under the thread's own id; a save that
    /// names no document persists under this id, so one conversation keeps
    /// one document. Runtime snapshot ownership is the receipts recorded on
    /// the thread's turns, not this binding (see `patch_undo_workspace_files`).
    pub session_id: String,
    pub messages: Vec<Message>,
    pub total_tokens: u64,
    pub model: String,
    /// Generic provider kind retained for serialized compatibility.
    pub model_provider: String,
    /// Exact non-secret configured provider key.
    pub model_provider_id: Option<String>,
    pub workspace: PathBuf,
    pub system_prompt: Option<SystemPrompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruction_assembly: Option<serde_json::Value>,
    pub mode: String,
}

/// Live context-window posture for one thread, computed where the session
/// state actually lives. Returned by `Op::GetContextBudget` via a oneshot
/// channel so HTTP clients (GPUI usage panel) never re-derive the engine's
/// token math or route limits at the API boundary.
#[derive(Debug, Clone)]
pub struct SessionContextBudget {
    /// Total context window for the active route (input + output), in tokens.
    pub window_tokens: u64,
    /// Estimated input tokens on the same basis the visible context meter
    /// and the auto-compaction gate use (`estimate_input_tokens_for_pressure`,
    /// without the overflow guard's 1.5x inflation). This is the number a
    /// "context filling up" indicator shows.
    pub input_tokens: u64,
    /// Provider-billed prompt tokens from the most recent parent-route
    /// request that still describes the live message list. `None` when no
    /// provider count exists yet (fresh session) — never a fabricated zero.
    pub billed_input_tokens: Option<u64>,
    /// Output tokens reserved for the turn after route clamps.
    pub output_cap_tokens: u64,
    /// Spendable input ceiling (`window - output_cap - headroom`, intersected
    /// with any provider-published hard input limit).
    pub input_budget_ceiling: u64,
    /// Input tokens still available before the reserved boundary.
    pub available_input_tokens: u64,
    /// Input level at which compaction is suggested.
    pub compaction_trigger_tokens: u64,
    /// `input_tokens / window_tokens` as a percentage (0..=100).
    pub usage_percent: f64,
    /// Coarse pressure label (`low`/`moderate`/`high`/`critical`).
    pub pressure: &'static str,
    /// Route identity the budget was computed for.
    pub model: String,
    pub provider: String,
    pub model_provider_id: Option<String>,
}

/// Provider request runtime state surfaced by `/provider`.
/// Returned by `Op::GetProviderRuntimeStatus` via a oneshot channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRuntimeStatus {
    pub provider: ProviderKind,
    pub request_concurrency_limit: Option<usize>,
    pub active_provider_requests: usize,
}

/// Idle Engine snapshot used by a one-shot host before shutting down.
/// The completion inbox belongs to the Engine; a terminal worker alone does
/// not prove that its parent has consumed the handback.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubAgentSettlement {
    pub running_children: usize,
    /// A Workflow can still be coordinating between child phases.
    pub running_workflows: usize,
    pub pending_completions: usize,
}

impl SubAgentSettlement {
    #[must_use]
    pub fn is_settled(self) -> bool {
        self.running_children == 0 && self.running_workflows == 0 && self.pending_completions == 0
    }
}

/// Engine-owned MCP snapshot plus the exact event generation it supersedes.
/// The TUI uses the receipt to reject already-queued boot events even when it
/// had not rendered that generation before the direct `/mcp` action.
#[derive(Debug, Clone)]
pub struct McpManagerUpdate {
    pub snapshot: crate::mcp::McpManagerSnapshot,
    pub generation: u64,
}

/// Result of rebuilding the engine-owned MCP pool in process.
pub type McpReloadResult = Result<McpManagerUpdate, String>;

/// Result of the one-shot boot connection pass for the engine-owned MCP pool.
///
/// This shares the reload result shape while remaining a separate operation:
/// boot may fill an empty live pool, but it must not force a config reload or
/// invalidate already-ready connections.
pub type McpBootstrapResult = Result<McpManagerUpdate, String>;

/// Origin of text being introduced as a user-role turn.
///
/// Chat providers force several runtime/control-plane signals through
/// `role = "user"` for compatibility, so role alone is not authority.
#[allow(dead_code)] // Some origins are reserved for ingestion sites landing after the first gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserInputProvenance {
    /// Text typed or submitted through the active UI/API input boundary.
    ExternalUser,
    /// Runtime-generated continuation, diagnostic, or tool feedback.
    Runtime,
    /// Completion/event text from a child worker or sub-agent handoff.
    SubAgentHandoff,
    /// A bounded, typed Agent Mail envelope delivered by the durable runtime.
    /// Provider protocols still receive a user-role projection, but this
    /// provenance can never inherit external-user authority.
    AgentMail,
    /// Text restored from a saved/imported transcript.
    ImportedTranscript,
    /// Text recalled from memory or another persisted source.
    MemoryRecall,
    /// Assistant-authored text that is shaped like a user response.
    AssistantGenerated,
}

impl UserInputProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalUser => "external_user",
            Self::Runtime => "runtime",
            Self::SubAgentHandoff => "subagent_handoff",
            Self::AgentMail => "agent_mail",
            Self::ImportedTranscript => "imported_transcript",
            Self::MemoryRecall => "memory_recall",
            Self::AssistantGenerated => "assistant_generated",
        }
    }

    pub fn can_authorize_work(self) -> bool {
        matches!(self, Self::ExternalUser)
    }
}

/// Per-turn authority payload carried by [`Op::SendMessage`]. Extracted from
/// the enum arm so new per-turn fields accrete here instead of widening the
/// variant; the serializable twin is `codewhale_protocol::op::TurnSpec`.
#[derive(Debug)]
pub struct TurnSpec {
    pub profile_constitution:
        Option<codewhale_config::user_constitution::ProfileConstitutionSnapshot>,
    /// Admitted allowance for this turn only; never changes session settings.
    pub max_output_tokens: Option<std::num::NonZeroU32>,
    pub content: String,
    /// Inline image bytes validated by Runtime admission; no file references.
    pub images: Vec<codewhale_protocol::runtime::RuntimeImageInput>,
    pub mode: AppMode,
    /// Exact, structurally resolved route authority for this turn. The
    /// engine activates its client before mutating turn state; injected
    /// engines may use their already-supplied client with the same receipt.
    pub route: Box<ResolvedRuntimeRoute>,
    /// Compaction policy derived from the same provider route. Carrying it
    /// atomically avoids a model/limit mismatch before `SendMessage`.
    pub compaction: Box<CompactionConfig>,
    /// Auxiliary provider calls completed while planning this exact turn
    /// (currently Auto's classifier), bounded and paired with their own
    /// immutable routes. The engine folds their tokens into total usage
    /// only; they never enter the parent route's billing aggregate.
    pub initial_routed_usage: Box<crate::cost_status::RuntimeUsageBatch>,
    pub goal_objective: Option<String>,
    pub goal_token_budget: Option<u32>,
    pub goal_status: GoalStatus,
    /// Reasoning-effort tier: `"off" | "low" | "medium" | "high" | "max"`.
    /// `None` lets the provider apply its default.
    pub reasoning_effort: Option<String>,
    /// True when the user selected auto thinking, even though the UI sends
    /// a concrete per-turn value to the model API.
    pub reasoning_effort_auto: bool,
    /// True when the user selected auto model routing.
    pub auto_model: bool,
    pub allow_shell: bool,
    pub trust_mode: bool,
    pub auto_approve: bool,
    pub approval_mode: ApprovalMode,
    pub translation_enabled: bool,
    /// Tool restriction from custom slash command frontmatter.
    /// `None` means the current turn may use the normal tool set.
    pub allowed_tools: Option<Vec<String>>,
    /// Runtime-supplied tools available only for this turn.
    pub dynamic_tools: Vec<DynamicToolSpec>,
    /// Hook executor for control-plane hooks.
    /// `ToolCallBefore` hooks may deny a tool call with exit code 2.
    pub hook_executor: Option<std::sync::Arc<crate::hooks::HookExecutor>>,
    pub verbosity: Option<String>,
    /// Structural input origin. This gates whether the turn may inherit
    /// YOLO/auto-approval authority; user-shaped text is not enough.
    pub provenance: UserInputProvenance,
    /// Host-supplied correlation token for this submission, echoed verbatim on
    /// the turn's `Event::TurnStarted`. Hosts that arm submit→`TurnStarted`
    /// window actions (e.g. a deferred stop replay) use the echo to bind those
    /// actions to the turn that actually started: every engine self-started
    /// turn (idle sub-agent completion, background shell wake, goal
    /// continuation) and the composer shell command turn carry no token, so
    /// their start cannot be mistaken for a pending submission even when it
    /// overtakes it in the event stream. `None` for callers that do not
    /// correlate. The token only survives the in-process engine path: the
    /// wire op projection and the durable-runtime submission path carry none,
    /// so hosts submitting over those channels cannot correlate.
    pub submission_id: Option<String>,
}

/// Operations that can be submitted to the engine.
#[derive(Debug)]
pub enum Op {
    Review {
        turn: TurnSpec,
        request: codewhale_protocol::op::ReviewOperation,
    },
    /// Send a message to the AI
    SendMessage(TurnSpec),

    /// Re-check and dispatch an interactive goal continuation when this
    /// operation reaches the front of the engine queue. Keeping this distinct
    /// from `SendMessage` prevents a queued `/goal pause` or `/goal clear`
    /// from being overwritten by a stale synthetic Active snapshot.
    ContinueGoal {
        /// Runtime-supplied tools remain available across the synthetic turn
        /// that continues the same logical goal run.
        dynamic_tools: Vec<DynamicToolSpec>,
        /// Opaque identity for an engine-owned synthetic continuation. Direct
        /// callers use `None`; the engine uses `Some` to coalesce one token
        /// across capacity-waiting, enqueued, and running-adjacent states.
        engine_schedule_id: Option<u64>,
    },

    /// Execute a user-submitted composer shell command (`! <command>`) without
    /// sending a model turn. This still routes through `exec_shell`, approval,
    /// sandbox, and command-safety handling.
    RunShellCommand {
        command: String,
        mode: AppMode,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
    },

    /// Set the runtime goal status without dispatching a model turn. Used by
    /// `/goal pause`, `/goal resume`, `/goal clear`, etc. so the engine's
    /// `SharedGoalState` learns the new status immediately and a queued
    /// continuation doesn't overwrite it back to Active.
    SetGoalStatus {
        status: GoalStatus,
        /// When `true`, clear the objective entirely (`/goal clear`).
        clear: bool,
        /// Accepted control revision; None lets direct callers mint it.
        goal_id: Option<String>,
    },

    /// Set (or replace) the active goal objective and immediately start goal
    /// work through the runtime's continuation steering. `/goal <objective>`
    /// is the caller; the objective is never echoed as a raw user message.
    SetGoalObjective {
        objective: String,
        token_budget: Option<u32>,
        /// Accepted control revision; None lets direct callers mint it.
        goal_id: Option<String>,
    },

    /// Describe the exact request the next turn would send, without
    /// sending it (`/preview-request`, #1004).
    ///
    /// Handled by the engine because only the engine can rebuild the current
    /// tool catalog, MCP state, mode, gates, permission posture, and resolved
    /// route. Pure inspection: it adds no message, no turn, and no tool call.
    PreviewOutboundRequest {
        inputs: Box<crate::core::engine::preview::PreviewRequestInputs>,
        /// Render the manifest as JSON instead of the human-readable table.
        json: bool,
        /// Explicit disclosure of the base prompt only; effective system text
        /// remains protected behind hashes.
        base_prompt_only: bool,
    },

    /// List current sub-agents and their status
    ListSubAgents,

    /// Inspect child settlement at the Engine's idle operation boundary.
    /// This does not drain the completion inbox or dispatch a second loop.
    GetSubAgentSettlement {
        tx: std::sync::Arc<
            std::sync::Mutex<Option<tokio::sync::oneshot::Sender<SubAgentSettlement>>>,
        >,
    },

    /// Cancel a running sub-agent by id or session name.
    CancelSubAgent { agent_id: String },

    /// Deliver an operator follow-up to one child on its own fork: live
    /// delivery to a running child, or a checkpoint continuation (new agent
    /// id) for an interrupted or completed child. Terminal failed/cancelled
    /// children answer with a receipt explaining why they cannot continue.
    FollowUpSubAgent { agent_id: String, text: String },

    StartFleetMember {
        start: Box<crate::tools::subagent::FleetMemberStart>,
        tx: std::sync::Arc<
            std::sync::Mutex<Option<tokio::sync::oneshot::Sender<FleetMemberStarted>>>,
        >,
    },

    /// Change the operating mode
    ChangeMode {
        mode: AppMode,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
        configured_sandbox_mode: Option<String>,
    },

    /// Update the model being used and refresh stable prompt context.
    SetModel {
        model: String,
        mode: AppMode,
        route_limits: Option<codewhale_config::route::RouteLimits>,
    },

    /// Update auto-compaction settings
    SetCompaction { config: CompactionConfig },

    /// Update the SSE idle timeout used for subsequent streamed turns.
    SetStreamChunkTimeout { timeout_secs: u64 },

    /// Update sub-agent runtime controls for subsequent turns.
    SetSubagentRuntimeConfig {
        enabled: bool,
        max_subagents: usize,
        launch_concurrency: usize,
        max_spawn_depth: u32,
        api_timeout_secs: u64,
        heartbeat_timeout_secs: u64,
    },

    /// Update the web-search backend for subsequent tool calls.
    SetSearchProvider {
        provider: codewhale_runtime::config::SearchProvider,
    },

    /// Replace the engine's merged Fleet roster after the setup wizard saves a
    /// project or personal profile. Subsequent turns can use the new role
    /// immediately instead of requiring an application restart.
    SetFleetRoster {
        roster: std::sync::Arc<crate::fleet::roster::FleetRoster>,
    },

    /// Sync engine session state (used for resume/load)
    SyncSession {
        session_id: Option<String>,
        messages: Vec<Message>,
        system_prompt: Option<SystemPrompt>,
        system_prompt_override: bool,
        model: String,
        workspace: PathBuf,
        mode: AppMode,
    },

    /// Rewind only the exact conversation observed by the caller. The Engine
    /// compares the full expected state before changing history or caches.
    /// A rejected rewind returns None and performs no mutation or inference.
    RewindConversation {
        expected: Box<SessionSnapshot>,
        messages: Vec<Message>,
        tx: tokio::sync::oneshot::Sender<Option<SessionSnapshot>>,
    },

    /// Run context compaction on one exact, structurally resolved provider
    /// route with policy derived from that same descriptor.
    CompactContext {
        /// Stable request identity allocated before the operation enters the
        /// bounded mailbox. Cancellation uses this id even when the provider
        /// future has not started yet.
        id: String,
        route: Box<ResolvedRuntimeRoute>,
        compaction: Box<CompactionConfig>,
    },

    /// Cancel one exact queued or running context-compaction request.
    CancelCompaction { id: String },

    /// Get a snapshot of the current session state (messages, tokens, etc.)
    /// for saving to disk. Returns the result via the oneshot sender so
    /// the caller doesn't have to compete with the SSE event stream.
    GetSessionSnapshot {
        tx: std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<SessionSnapshot>>>>,
    },

    /// Get the live context-window budget for this session's route. Computed
    /// on the engine so `active_route_limits`, the memoized token estimate,
    /// and the last billed prompt size all come from one authority.
    GetContextBudget {
        tx: std::sync::Arc<
            std::sync::Mutex<Option<tokio::sync::oneshot::Sender<Option<SessionContextBudget>>>>,
        >,
    },

    /// Get active provider request concurrency state for readiness surfaces.
    GetProviderRuntimeStatus {
        tx: std::sync::Arc<
            std::sync::Mutex<Option<tokio::sync::oneshot::Sender<ProviderRuntimeStatus>>>,
        >,
    },

    /// Populate the engine-owned MCP pool once at UI boot and return a
    /// snapshot from that exact pool. This is not a config reload and never
    /// constructs a UI-owned discovery pool. Optional servers never block
    /// the first model turn: that turn snapshots currently-ready tools.
    BootstrapMcp {
        tx: std::sync::Arc<
            std::sync::Mutex<Option<tokio::sync::oneshot::Sender<McpBootstrapResult>>>,
        >,
    },

    /// Retry one failed MCP server on the existing engine pool and return a
    /// full snapshot. Ready siblings are never invalidated or reconnected.
    RetryMcpServer {
        name: String,
        tx: std::sync::Arc<
            std::sync::Mutex<Option<tokio::sync::oneshot::Sender<McpBootstrapResult>>>,
        >,
    },

    /// Force the engine-owned MCP config/catalog to reload and reconnect.
    /// The returned snapshot is taken from that same live pool.
    ReloadMcp {
        config_path: PathBuf,
        tx: std::sync::Arc<std::sync::Mutex<Option<tokio::sync::oneshot::Sender<McpReloadResult>>>>,
    },

    /// Run agent-driven context purging.
    PurgeContext,

    /// Edit the last user message: remove the last user+assistant exchange
    /// from the session, then re-send with the new content.
    #[cfg_attr(not(test), expect(dead_code))]
    EditLastTurn {
        new_message: String,
        /// Host-supplied correlation token, echoed on the replayed turn's
        /// `Event::TurnStarted` (see `TurnSpec::submission_id`).
        submission_id: Option<String>,
    },

    /// Enable or disable the background advisor watcher for this session.
    /// When enabled, a fire-and-forget background task runs after each turn
    /// that contained tool calls and emits an `Event::AdvisoryNote` with
    /// concise observations. (#3982)
    SetAdvisorEnabled { enabled: bool },

    /// Shutdown the engine
    Shutdown,
}

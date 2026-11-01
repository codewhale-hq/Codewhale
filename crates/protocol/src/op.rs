//! `Op`-in API in `crates/protocol` (issue #5261, Phase A1 of the
//! core/protocol extraction spec).
//!
//! The TUI engine already had an internal channel (`Op` in
//! `crates/engine/src/core/ops.rs` with `tx_op` / `rx_op` and `tx_steer`).
//! This protocol file formalizes that channel so TUI, CLI, app-server, and
//! tests share one serializable API. The wire is `OpEnvelope` + `Op`;
//! transports that already speak JSON (app-server, tests) can send the
//! envelope directly, while in-process callers continue to use the typed
//! enum.
//!
//! Parity is compile-enforced from the engine side:
//! `crates/engine/src/core/protocol_parity.rs` matches every engine `Op`
//! variant exhaustively into a protocol `Op` (`protocol_covers_engine_ops`).
//!
//! What is deliberately stripped at this boundary:
//!
//! - `mpsc` / `oneshot` reply channels (`GetSubAgentSettlement`, `GetSessionSnapshot`,
//!   `GetContextBudget`, `GetProviderRuntimeStatus`, `BootstrapMcp`, `RetryMcpServer`,
//!   `ReloadMcp`). Over the wire the reply is an `EventMsg` or a response
//!   frame, not a channel.
//! - `Arc<HookExecutor>` on `SendMessage`: hooks are host configuration, not
//!   turn input.
//! - Resolved provider routes and clients. Only the non-secret receipt
//!   (`model`, `model_provider`) crosses; the engine re-resolves.
//! - `Ruleset`, transcript `Message`s, and `SystemPrompt` cross as
//!   `serde_json::Value` from their own `Serialize` until typed in a later
//!   phase.
//!
//! `Steer` and `Cancel` have no engine `Op` twin on purpose: the engine
//! carries them on `tx_steer` and the cancellation token. They are protocol
//! operations regardless, because every out-of-process client needs them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Accept `engine_schedule_id` on the wire for compatibility, then discard it.
///
/// Deserialization is the out-of-process boundary: the engine's own
/// `Op::ContinueGoal` travels an in-process channel and never reaches here, so
/// anything this sees was supplied by a caller who must not be able to set it.
/// Returning `None` sends such a request down the ordinary host-injected path
/// instead of letting it consume a pending engine schedule.
fn drop_engine_owned_schedule_id<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<u64>::deserialize(deserializer)?;
    Ok(None)
}

use crate::ids::{SessionId, ThreadId};
use crate::runtime::DynamicToolSpec;

/// Every `Op` is paired with the ids that route it. This is the
/// `Op`-in half of the `Op`-in / `EventMsg`-out contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpEnvelope {
    /// Monotonic `op:<n>` for dedup / tracing within a session.
    pub op_id: String,
    pub thread_id: ThreadId,
    pub session_id: SessionId,
    pub op: Op,
}

/// Token-limit facts for a route. `None` is unknown, never zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

/// Compaction policy derived from a provider route. Twin of the engine's
/// `CompactionConfig`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionPolicy {
    pub enabled: bool,
    pub token_threshold: u64,
    pub model: String,
    /// `supported | unsupported | unknown`.
    #[serde(default = "default_capability_state")]
    pub image_input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_context_window: Option<u32>,
    #[serde(default)]
    pub cache_summary: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_cost_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
}

fn default_capability_state() -> String {
    "unknown".to_string()
}

/// Per-turn authority payload carried by [`Op::SendMessage`]. Serializable
/// twin of `crates/engine/src/core/ops::TurnSpec`; extracted from the enum arm so
/// new per-turn fields accrete here instead of widening the variant. The
/// `SendMessage(TurnSpec)` newtype keeps the internally-tagged wire shape
/// byte-identical: `{"kind":"send_message", ...fields}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_constitution: Option<serde_json::Value>,
    #[serde(
        default,
        rename = "maxOutputTokens",
        alias = "max_output_tokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_output_tokens: Option<std::num::NonZeroU32>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::runtime::RuntimeImageInput>,
    /// Effective mode for this turn (`"plan" | "agent" | "operate"` etc).
    #[serde(default = "default_mode")]
    pub mode: String,
    /// Optional explicit route/model the caller resolved already (mirrors
    /// `ResolvedRuntimeRoute` in `crates_tui::route_runtime`). `None` means
    /// "use the thread's current route".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    /// Tool restriction from slash-command frontmatter.
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Runtime-supplied dynamic tools for this turn only.
    #[serde(default)]
    pub dynamic_tools: Vec<DynamicToolSpec>,
    /// Structural input provenance — only `external_user` may inherit
    /// YOLO/auto-approval authority (mirrors `UserInputProvenance`).
    #[serde(default = "default_provenance")]
    pub provenance: String,
    /// Compaction policy carried atomically with the route receipt.
    /// Boxed only to keep the enum small; the wire shape is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Box<CompactionPolicy>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_objective: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_token_budget: Option<u32>,
    /// `active | paused | complete | blocked`.
    #[serde(default = "default_goal_status")]
    pub goal_status: String,
    /// `"off" | "low" | "medium" | "high" | "max"`; `None` = provider default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub reasoning_effort_auto: bool,
    #[serde(default)]
    pub auto_model: bool,
    #[serde(default)]
    pub allow_shell: bool,
    #[serde(default)]
    pub trust_mode: bool,
    #[serde(default)]
    pub auto_approve: bool,
    /// `auto | bypass | suggest | never`.
    #[serde(default = "default_approval_mode")]
    pub approval_mode: String,
    #[serde(default)]
    pub translation_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOperationIntent {
    pub title: String,
    pub description: String,
    pub commits: String,
    pub linked_issue: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSourceFile {
    pub path: String,
    pub revision: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewPriorFinding {
    pub key: String,
    pub path: String,
    pub line: u32,
    pub summary: String,
    pub discussion: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOperationContext {
    pub files: Vec<ReviewSourceFile>,
    pub omitted: Vec<String>,
    pub previous_findings: Vec<ReviewPriorFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewOperation {
    pub version: u32,
    pub repository: String,
    pub pr_number: u64,
    pub head_sha: String,
    pub base_sha: String,
    pub diff: String,
    pub intent: ReviewOperationIntent,
    pub guidance: String,
    pub max_claims: usize,
    pub max_nits: usize,
    pub max_chars: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ReviewOperationContext>,
}

/// Operations that can be submitted to the core engine. This is the
/// protocol view of `crates/engine/src/core/ops::Op` — same lifecycle,
/// same provenance gate — but serializable and free of `mpsc` / `oneshot`
/// fields. In-process callers convert at the boundary; out-of-process
/// callers send the JSON directly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Op {
    Review {
        turn: Box<TurnSpec>,
        request: ReviewOperation,
    },
    /// Drive one model turn: `role=user` content plus the resolved route
    /// receipt the engine will freeze at the client-freeze boundary. Headless
    /// and TUI must produce byte-identical `MessageRequest`s for identical
    /// `Op::SendMessage` payloads.
    SendMessage(TurnSpec),

    /// Steer an in-flight turn with additional user content (drains into
    /// the turn loop's `rx_steer` channel).
    Steer {
        content: String,
    },

    /// Re-check and dispatch a goal continuation (synthetic turn that
    /// continues the same logical goal run).
    ContinueGoal {
        #[serde(default)]
        dynamic_tools: Vec<DynamicToolSpec>,
        /// Engine-owned coalescing token; direct callers send `None`.
        ///
        /// Enforced, not merely documented: the engine mints these on an
        /// in-process channel (`tx_op.try_send`) and they never cross serde,
        /// so any value arriving through deserialization came from outside.
        /// The IDs are predictable per-session counters, and a matching guess
        /// is treated as an already-delayed internal token -- it would consume
        /// the pending schedule and skip the host-injected quiet period. Wire
        /// input is therefore always dropped to `None`.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "drop_engine_owned_schedule_id"
        )]
        engine_schedule_id: Option<u64>,
    },

    /// Execute a local composer shell command without a model turn.
    RunShellCommand {
        command: String,
        #[serde(default = "default_mode")]
        mode: String,
        #[serde(default)]
        allow_shell: bool,
        #[serde(default)]
        trust_mode: bool,
        #[serde(default)]
        auto_approve: bool,
        #[serde(default = "default_approval_mode")]
        approval_mode: String,
    },

    /// Set goal status without dispatching a model turn.
    SetGoalStatus {
        status: String,
        #[serde(default)]
        clear: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal_id: Option<String>,
    },

    /// Set (or replace) the active goal objective and start goal work.
    SetGoalObjective {
        objective: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_budget: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal_id: Option<String>,
    },

    Cancel,
    Shutdown,

    /// Describe the exact request the next turn would send without sending it
    /// (`/dryrun` / `/preview-request`, #1004). Headless and TUI must render
    /// identical manifests for identical inputs.
    PreviewOutboundRequest {
        #[serde(default)]
        json: bool,
        #[serde(default)]
        base_prompt_only: bool,
        #[serde(default = "default_mode")]
        mode: String,
        #[serde(default)]
        allow_shell: bool,
        #[serde(default)]
        trust_mode: bool,
        #[serde(default)]
        auto_approve: bool,
        #[serde(default = "default_approval_mode")]
        approval_mode: String,
        #[serde(default)]
        allowed_tools: Option<Vec<String>>,
        #[serde(default)]
        dynamic_tools: Vec<DynamicToolSpec>,
        #[serde(default = "default_provenance")]
        provenance: String,
        /// The model selector the user chose (`auto` when auto routing).
        #[serde(default)]
        requested_model: String,
        #[serde(default)]
        requested_reasoning: String,
        #[serde(default)]
        auto_model: bool,
        #[serde(default)]
        hypothetical_prompt_supplied: bool,
        /// Model-facing text of the hypothetical next message, when the host
        /// planner resolved one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hypothetical_prompt: Option<String>,
        /// Why no exact next turn exists, when it does not.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unresolved: Option<String>,
    },

    ListSubAgents,
    /// Inspect live children and pending handbacks at the Engine's idle
    /// boundary. The host owns the response channel; it is not wire input.
    GetSubAgentSettlement,
    CancelSubAgent {
        agent_id: String,
    },
    FollowUpSubAgent {
        agent_id: String,
        text: String,
    },
    StartFleetMember {
        member_id: String,
    },

    ChangeMode {
        #[serde(default = "default_mode")]
        mode: String,
        #[serde(default)]
        allow_shell: bool,
        #[serde(default)]
        trust_mode: bool,
        #[serde(default)]
        auto_approve: bool,
        #[serde(default = "default_approval_mode")]
        approval_mode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        configured_sandbox_mode: Option<String>,
    },

    SetModel {
        model: String,
        #[serde(default = "default_mode")]
        mode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        route_limits: Option<RouteLimits>,
    },

    SetCompaction {
        config: CompactionPolicy,
    },

    /// Legacy serialized permission-update operation, retained for wire
    /// compatibility. The in-process runtime now publishes directly through
    /// its shared policy store instead of queuing a second replacement.
    SetPermissionRuleset {
        ruleset: Value,
    },

    SetStreamChunkTimeout {
        timeout_secs: u64,
    },

    SetSubagentRuntimeConfig {
        enabled: bool,
        max_subagents: u64,
        launch_concurrency: u64,
        max_spawn_depth: u32,
        api_timeout_secs: u64,
        heartbeat_timeout_secs: u64,
    },

    /// `SearchProvider` in snake_case.
    SetSearchProvider {
        provider: String,
    },

    /// Replace the engine's merged Fleet roster. Only the roster's identity
    /// crosses: member ids in precedence order plus the load state.
    SetFleetRoster {
        member_ids: Vec<String>,
        #[serde(default)]
        exact_selection: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        load_error: Option<String>,
    },

    /// Sync engine session state (resume/load). `messages` are transcript
    /// `Message`s and `system_prompt` a `SystemPrompt`, both serialized.
    SyncSession {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine_session_id: Option<String>,
        messages: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        system_prompt: Option<Value>,
        #[serde(default)]
        system_prompt_override: bool,
        model: String,
        workspace: PathBuf,
        #[serde(default = "default_mode")]
        mode: String,
    },

    /// Rewind an unchanged conversation. `expected` is the complete observed
    /// session snapshot; a receiver must compare it before changing history.
    /// The success/refusal receipt travels out-of-band.
    RewindConversation {
        expected: Value,
        messages: Vec<Value>,
    },

    /// Run context compaction on one exact provider route.
    CompactContext {
        id: String,
        model: String,
        model_provider: String,
        compaction: CompactionPolicy,
    },

    CancelCompaction {
        id: String,
    },

    /// Request a session snapshot; the reply travels out-of-band.
    GetSessionSnapshot,
    /// Request the live context-window budget for the session's route; the
    /// reply travels out-of-band.
    GetContextBudget,
    /// Request provider concurrency state; the reply travels out-of-band.
    GetProviderRuntimeStatus,
    /// Populate the engine-owned MCP pool once at boot; reply out-of-band.
    BootstrapMcp,
    RetryMcpServer {
        name: String,
    },
    ReloadMcp {
        config_path: PathBuf,
    },

    PurgeContext,
    EditLastTurn {
        new_message: String,
    },
    SetAdvisorEnabled {
        enabled: bool,
    },
}

fn default_mode() -> String {
    "agent".to_string()
}

fn default_provenance() -> String {
    "external_user".to_string()
}

fn default_goal_status() -> String {
    "active".to_string()
}

fn default_approval_mode() -> String {
    "suggest".to_string()
}

/// Every wire tag `Op` can carry, in declaration order.
pub const OP_KINDS: &[&str] = &[
    "review",
    "send_message",
    "steer",
    "continue_goal",
    "run_shell_command",
    "set_goal_status",
    "set_goal_objective",
    "cancel",
    "shutdown",
    "preview_outbound_request",
    "list_sub_agents",
    "get_sub_agent_settlement",
    "cancel_sub_agent",
    "follow_up_sub_agent",
    "start_fleet_member",
    "change_mode",
    "set_model",
    "set_compaction",
    "set_permission_ruleset",
    "set_stream_chunk_timeout",
    "set_subagent_runtime_config",
    "set_search_provider",
    "set_fleet_roster",
    "sync_session",
    "rewind_conversation",
    "compact_context",
    "cancel_compaction",
    "get_session_snapshot",
    "get_context_budget",
    "get_provider_runtime_status",
    "bootstrap_mcp",
    "retry_mcp_server",
    "reload_mcp",
    "purge_context",
    "edit_last_turn",
    "set_advisor_enabled",
];

impl Op {
    #[must_use]
    pub fn is_send_message(&self) -> bool {
        matches!(self, Self::SendMessage(_))
    }

    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Review { .. } => "review",
            Self::SendMessage(_) => "send_message",
            Self::Steer { .. } => "steer",
            Self::ContinueGoal { .. } => "continue_goal",
            Self::RunShellCommand { .. } => "run_shell_command",
            Self::SetGoalStatus { .. } => "set_goal_status",
            Self::SetGoalObjective { .. } => "set_goal_objective",
            Self::Cancel => "cancel",
            Self::Shutdown => "shutdown",
            Self::PreviewOutboundRequest { .. } => "preview_outbound_request",
            Self::ListSubAgents => "list_sub_agents",
            Self::GetSubAgentSettlement => "get_sub_agent_settlement",
            Self::CancelSubAgent { .. } => "cancel_sub_agent",
            Self::FollowUpSubAgent { .. } => "follow_up_sub_agent",
            Self::StartFleetMember { .. } => "start_fleet_member",
            Self::ChangeMode { .. } => "change_mode",
            Self::SetModel { .. } => "set_model",
            Self::SetCompaction { .. } => "set_compaction",
            Self::SetPermissionRuleset { .. } => "set_permission_ruleset",
            Self::SetStreamChunkTimeout { .. } => "set_stream_chunk_timeout",
            Self::SetSubagentRuntimeConfig { .. } => "set_subagent_runtime_config",
            Self::SetSearchProvider { .. } => "set_search_provider",
            Self::SetFleetRoster { .. } => "set_fleet_roster",
            Self::SyncSession { .. } => "sync_session",
            Self::RewindConversation { .. } => "rewind_conversation",
            Self::CompactContext { .. } => "compact_context",
            Self::CancelCompaction { .. } => "cancel_compaction",
            Self::GetSessionSnapshot => "get_session_snapshot",
            Self::GetContextBudget => "get_context_budget",
            Self::GetProviderRuntimeStatus => "get_provider_runtime_status",
            Self::BootstrapMcp => "bootstrap_mcp",
            Self::RetryMcpServer { .. } => "retry_mcp_server",
            Self::ReloadMcp { .. } => "reload_mcp",
            Self::PurgeContext => "purge_context",
            Self::EditLastTurn { .. } => "edit_last_turn",
            Self::SetAdvisorEnabled { .. } => "set_advisor_enabled",
        }
    }
}

/// Build a headless `SendMessage` envelope with fresh ids. This is the
/// one-line helper every headless caller (CLI `exec`, app-server, tests)
/// uses so TUI and headless start a session identically.
#[must_use]
pub fn headless_send_message_op(thread_id: ThreadId, content: impl Into<String>) -> OpEnvelope {
    OpEnvelope {
        op_id: format!("op-{}", uuid::Uuid::new_v4()),
        thread_id: thread_id.clone(),
        session_id: SessionId::new(),
        op: Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: content.into(),
            images: Vec::new(),
            mode: default_mode(),
            model: None,
            model_provider: None,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            provenance: default_provenance(),
            compaction: None,
            goal_objective: None,
            goal_token_budget: None,
            goal_status: default_goal_status(),
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            approval_mode: default_approval_mode(),
            translation_enabled: false,
            verbosity: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> CompactionPolicy {
        CompactionPolicy {
            enabled: true,
            token_threshold: 100_000,
            model: "deepseek-chat".into(),
            image_input: "unknown".into(),
            effective_context_window: Some(128_000),
            cache_summary: true,
            focus: None,
            runtime_cost_owner: None,
            workspace: None,
        }
    }

    /// One instance of every variant, in declaration order.
    fn every_variant() -> Vec<Op> {
        let Op::SendMessage(turn) = headless_send_message_op(ThreadId::new(), "review").op else {
            unreachable!()
        };
        vec![
            Op::Review {
                turn: Box::new(turn),
                request: ReviewOperation {
                    version: 1,
                    repository: "owner/repo".into(),
                    pr_number: 1,
                    head_sha: "a".repeat(40),
                    base_sha: "b".repeat(40),
                    diff: "diff".into(),
                    intent: ReviewOperationIntent {
                        title: String::new(),
                        description: String::new(),
                        commits: String::new(),
                        linked_issue: String::new(),
                    },
                    guidance: String::new(),
                    max_claims: 40,
                    max_nits: 3,
                    max_chars: 60_000,
                    context: None,
                },
            },
            headless_send_message_op(ThreadId::new(), "hello").op,
            Op::Steer {
                content: "more".into(),
            },
            Op::ContinueGoal {
                dynamic_tools: vec![],
                // Engine-owned: deliberately not round-trippable. See
                // `wire_supplied_engine_schedule_id_is_dropped`.
                engine_schedule_id: None,
            },
            Op::RunShellCommand {
                command: "ls".into(),
                mode: "agent".into(),
                allow_shell: true,
                trust_mode: false,
                auto_approve: false,
                approval_mode: "suggest".into(),
            },
            Op::SetGoalStatus {
                goal_id: None,
                status: "paused".into(),
                clear: false,
            },
            Op::SetGoalObjective {
                goal_id: None,
                objective: "ship".into(),
                token_budget: Some(10),
            },
            Op::Cancel,
            Op::Shutdown,
            Op::PreviewOutboundRequest {
                json: true,
                base_prompt_only: false,
                mode: "plan".into(),
                allow_shell: false,
                trust_mode: false,
                auto_approve: false,
                approval_mode: "auto".into(),
                allowed_tools: Some(vec!["read_file".into()]),
                dynamic_tools: vec![],
                provenance: "external_user".into(),
                requested_model: "auto".into(),
                requested_reasoning: "high".into(),
                auto_model: true,
                hypothetical_prompt_supplied: true,
                hypothetical_prompt: Some("hi".into()),
                unresolved: None,
            },
            Op::ListSubAgents,
            Op::GetSubAgentSettlement,
            Op::CancelSubAgent {
                agent_id: "a1".into(),
            },
            Op::FollowUpSubAgent {
                agent_id: "a1".into(),
                text: "go".into(),
            },
            Op::StartFleetMember {
                member_id: "m1".into(),
            },
            Op::ChangeMode {
                mode: "operate".into(),
                allow_shell: true,
                trust_mode: true,
                auto_approve: false,
                approval_mode: "bypass".into(),
                configured_sandbox_mode: Some("workspace-write".into()),
            },
            Op::SetModel {
                model: "m".into(),
                mode: "agent".into(),
                route_limits: Some(RouteLimits {
                    context_tokens: Some(1),
                    input_tokens: None,
                    output_tokens: None,
                }),
            },
            Op::SetCompaction { config: policy() },
            Op::SetPermissionRuleset {
                ruleset: json!({"rules": []}),
            },
            Op::SetStreamChunkTimeout { timeout_secs: 30 },
            Op::SetSubagentRuntimeConfig {
                enabled: true,
                max_subagents: 4,
                launch_concurrency: 2,
                max_spawn_depth: 1,
                api_timeout_secs: 60,
                heartbeat_timeout_secs: 10,
            },
            Op::SetSearchProvider {
                provider: "brave".into(),
            },
            Op::SetFleetRoster {
                member_ids: vec!["scout".into()],
                exact_selection: false,
                load_error: None,
            },
            Op::SyncSession {
                engine_session_id: Some("s".into()),
                messages: vec![json!({"role": "user", "content": []})],
                system_prompt: None,
                system_prompt_override: false,
                model: "m".into(),
                workspace: PathBuf::from("/ws"),
                mode: "agent".into(),
            },
            Op::RewindConversation {
                expected: json!({"session_id": "s", "messages": []}),
                messages: vec![],
            },
            Op::CompactContext {
                id: "cmp-1".into(),
                model: "m".into(),
                model_provider: "deepseek".into(),
                compaction: policy(),
            },
            Op::CancelCompaction { id: "cmp-1".into() },
            Op::GetSessionSnapshot,
            Op::GetContextBudget,
            Op::GetProviderRuntimeStatus,
            Op::BootstrapMcp,
            Op::RetryMcpServer { name: "fs".into() },
            Op::ReloadMcp {
                config_path: PathBuf::from("/tmp/mcp.json"),
            },
            Op::PurgeContext,
            Op::EditLastTurn {
                new_message: "again".into(),
            },
            Op::SetAdvisorEnabled { enabled: true },
        ]
    }

    #[test]
    fn every_variant_is_listed_once() {
        let kinds: Vec<&str> = every_variant().iter().map(Op::kind_str).collect();
        assert_eq!(kinds, OP_KINDS, "OP_KINDS must list every variant in order");
    }

    #[test]
    fn wire_supplied_engine_schedule_id_is_dropped() {
        // The engine mints these on an in-process channel, so a value arriving
        // through serde came from an out-of-process caller. Honouring it would
        // let a guessed counter consume the pending schedule and skip the
        // host-injected quiet period.
        let op: Op = serde_json::from_value(json!({
            "kind": "continue_goal",
            "dynamic_tools": [],
            "engine_schedule_id": 3,
        }))
        .expect("continue_goal with a wire-supplied schedule id still parses");
        match op {
            Op::ContinueGoal {
                engine_schedule_id, ..
            } => assert_eq!(
                engine_schedule_id, None,
                "engine_schedule_id must never be settable from the wire"
            ),
            other => panic!("expected ContinueGoal, got {other:?}"),
        }
    }

    #[test]
    fn every_variant_round_trips_and_tags_by_kind() {
        for op in every_variant() {
            let value = serde_json::to_value(&op).unwrap();
            assert_eq!(value["kind"], op.kind_str(), "{op:?}");
            let back: Op = serde_json::from_value(value).unwrap();
            assert_eq!(back, op);
        }
    }

    #[test]
    fn op_envelope_roundtrip() {
        let env = headless_send_message_op(ThreadId::new(), "hello");
        let json = serde_json::to_string(&env).unwrap();
        let back: OpEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back.thread_id, env.thread_id);
        assert!(back.op.is_send_message());
    }

    #[test]
    fn minimal_send_message_json_still_parses_with_defaults() {
        // The pre-A1 wire shape: only `content` plus the tag.
        let op: Op = serde_json::from_value(json!({
            "kind": "send_message",
            "content": "hello"
        }))
        .unwrap();
        let Op::SendMessage(TurnSpec {
            mode,
            provenance,
            goal_status,
            approval_mode,
            ..
        }) = op
        else {
            panic!("expected send_message");
        };
        assert_eq!(mode, "agent");
        assert_eq!(provenance, "external_user");
        assert_eq!(goal_status, "active");
        assert_eq!(approval_mode, "suggest");
    }

    #[test]
    fn steer_roundtrip() {
        let op = Op::Steer {
            content: "more".into(),
        };
        let json = serde_json::to_string(&op).unwrap();
        let back: Op = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind_str(), "steer");
    }
}

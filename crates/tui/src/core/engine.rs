//! Core engine for `DeepSeek` CLI.
//!
//! The engine handles all AI interactions in a background task,
//! communicating with the UI via channels. This enables:
//! - Non-blocking UI during API calls
//! - Real-time streaming updates
//! - Proper cancellation support
//! - Tool execution orchestration

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use codewhale_config::route::CapabilityState;
use codewhale_execpolicy::{AskForApproval, ExecPolicyContext};
use codewhale_protocol::runtime::DynamicToolSpec;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, RwLock, mpsc};
use tokio_util::sync::CancellationToken;

use crate::approval_log::ApprovalReceiptStore;
use crate::client::CodewhaleClient;
use crate::compaction::{CompactionConfig, PreparedCompactionEnvelope, compact_messages_safe};
use crate::config::{Config, DEFAULT_MAX_SUBAGENTS, DEFAULT_TEXT_MODEL, ProviderKind};
use crate::core::model_client::SharedModelClient;
use crate::error_taxonomy::{ErrorCategory, ErrorEnvelope, ErrorSeverity, StreamError};
use crate::features::{Feature, Features};
use crate::mcp::{McpConfig, McpPool, McpSupervisorUpdate};
use crate::prompts;
use crate::purge::{emit_purge_completed, emit_purge_failed, emit_purge_started, run_purge};
#[cfg(test)]
use crate::route_runtime::resolve_runtime_route;
use crate::route_runtime::{
    ResolvedRuntimeRoute, ValidatedRuntimeRoute, resolve_runtime_route_for_identity,
};
use crate::snapshot::{WorkspaceSnapshotKind, WorkspaceSnapshotRef};
use crate::tools::goal::{
    GoalPauseReason, GoalSnapshot, GoalStatus, SharedGoalState, new_shared_goal_state,
};
use crate::tools::plan::{SharedPlanState, new_shared_plan_state};
use crate::tools::shell::{SharedShellManager, new_shared_shell_manager};
use crate::tools::spec::{
    ApprovalRequirement, ResourceClaim, RichToolResult, ToolError, ToolExecutionOutcome, ToolResult,
};
use crate::tools::spec::{
    RuntimeToolServices, SharedFileReadTracker, new_shared_file_read_tracker,
};
use crate::tools::subagent::{
    FleetRole, ForegroundChildRegistry, Mailbox, MailboxMessage, SharedSubAgentManager,
    SubAgentCompletion, SubAgentExecutionLimits, SubAgentForkContext, SubAgentManager,
    SubAgentResult, SubAgentRuntime, SubAgentStatus, TerminalEventDelivery,
    agent_worker_owner_snapshot, new_shared_subagent_manager_with_state_root_and_timeout,
};
use crate::tools::todo::{SharedTodoList, new_shared_todo_list};
use crate::tools::user_input::{UserInputRequest, UserInputResponse};
use crate::tools::{ToolContext, ToolRegistryBuilder};
use crate::utils::spawn_supervised;
use crate::worker_profile::WorkerRuntimeProfile;
use crate::working_set::WorkingSet;
use codewhale_config::AppMode;
use codewhale_execpolicy::ApprovalMode;
#[cfg(test)]
use codewhale_models::ToolCaller;
use codewhale_models::{
    ContentBlock, ContentBlockStart, Delta, Message, OUTPUT_CEILING_STOP_REASON, StreamEvent,
    SystemPrompt, Tool, Usage, is_incomplete_stop_reason, is_output_limit_stop_reason,
    stop_contradicts_output_ceiling, stop_reason_detail,
};

#[cfg(test)]
use super::authority::agent_approval_mode_for_turn;
use super::authority::{
    PolicyNarrowingEvent, RunOrigin, TurnAuthority, effective_input_policy, shell_policy_for_mode,
};
use super::events::{Event, TurnOutcomeStatus, TurnRoute};
use super::ops::{
    McpManagerUpdate, Op, ProviderRuntimeStatus, SessionContextBudget, SessionSnapshot, TurnSpec,
    USER_SHELL_TOOL_ID_PREFIX, UserInputProvenance,
};
use super::session::Session;
use super::tool_parser;
use super::turn::{TurnContext, format_snapshot_label, post_turn_snapshot};
use codewhale_models::Role;

const ENGINE_OP_CHANNEL_CAPACITY: usize = 32;
/// Bound on the sub-agent completion inbox (#6147). One completion per
/// terminal child plus small re-queue batches; a full inbox drops the wake,
/// and the terminal-results synthesis path still delivers the content it
/// names at the next explicit turn — the same deferral a host-managed
/// engine already has.
const SUBAGENT_COMPLETION_CHANNEL_CAPACITY: usize = 256;
/// Bound on the MCP session-boot progress channel (#6147). One progress
/// update per pending server plus the terminal update; progress drops are
/// tolerated by design (`let _ =`), and the terminal `Finished` waits for a
/// slot instead of being dropped.
const MCP_BOOT_CHANNEL_CAPACITY: usize = 64;
const GOAL_CONTINUATION_FAILURE_DETAIL_MAX_BYTES: usize = 512;
const PLAN_SHELL_NETWORK_DENIED_HINT: &str = "Shell command blocked: in Plan mode shell commands run in a read-only sandbox with no writes and no network access. The user can change modes with /mode.";

fn context_pressure_message(usage_percent: f64) -> Option<&'static str> {
    if usage_percent >= codewhale_runtime::context_budget::CONTEXT_CRITICAL_THRESHOLD_PERCENT {
        Some(
            "Context pressure: critical — CRITICAL: stop expanding scope; run /compact immediately or finish the current task",
        )
    } else if usage_percent >= codewhale_runtime::context_budget::CONTEXT_WARNING_THRESHOLD_PERCENT
    {
        Some(
            "Context pressure: warning — ESCALATED: prefer /compact, narrow scope, or finish the current task",
        )
    } else {
        None
    }
}

fn agent_list_event(manager: &SubAgentManager, active_session_id: &str) -> Event {
    // One clock read shared by every row, so elapsed values in a single
    // listing are consistent with each other (#5479).
    let now_ms = crate::tools::subagent::epoch_millis_now();
    Event::AgentList {
        owner_session_id: active_session_id.to_string(),
        agents: manager.list_for_session(active_session_id),
        coordination: manager.coordination_detail_projection_for_session(
            active_session_id,
            None,
            24,
        ),
        queued_follow_ups: manager.queued_follow_up_counts_for_session(active_session_id),
        roster: crate::agent_roster::build_agent_roster(
            &manager.list_worker_records_for_session(active_session_id),
            now_ms,
        ),
    }
}

/// The `<turn_meta>` line naming the permission posture a turn ran under
/// (`permission_chip_label`). Receipts read it back from saved transcripts,
/// so the writer and the reader share this prefix.
pub(crate) const PERMISSION_POSTURE_LINE: &str = "Current permission posture: ";
const MCP_REGISTRY_FIRST_INSTRUCTION_SOURCE: &str = "runtime:mcp-registry-first";
const MCP_REGISTRY_FIRST_INSTRUCTION: &str = "## MCP Registry\n\nThe Registry installs and connects a local MCP server when this session lacks a capability. It is a fallback for a capability you do not have, not a step before ordinary work.\n\nPrefer what is already available, in order: tools already in this catalog, the project's own scripts, tests, and dev tooling, and platform capabilities. Creating a file, reading a fixture, running a repo command, and checking your own output are ordinary work — do them directly.\n\nReach for the Registry once you have identified a specific capability that no available tool covers and that you would otherwise install or reimplement, such as a document or media converter, access to an external database or service, or a protocol client. Then call `registry_sync` with a `query` naming that capability; it scores the local Registry snapshot host-side and returns at most eight matches, so the full index never enters the conversation. When a returned server plausibly covers that capability, call `start_registry_mcp_server` with its exact name rather than installing or running its package command through the shell. If nothing matches, refine the query once, then continue with local tools.\n\nBoth Registry tools are deferred: load one with `tool_search` before its first call, and use the returned schema. If a call instead reports that it only loaded the schema, retry once with that schema. Do not go searching for them for work you can already do.";
/// The one system prompt for an isolated Runtime Chat session. The engine owns
/// it; the Runtime Chat relay sends the same text (plus any account chat
/// instructions) so the two never drift apart (#6517).
pub(crate) const ISOLATED_CHAT_SYSTEM_PROMPT: &str = "You are Codewhale Chat. Answer the user's request directly and conversationally. This is an isolated chat-only session: no local project, workspace, memory, skill, account, credential, path, or runtime context is available or implied. Do not claim to inspect or change local files, run tools, or perform work execution.";

pub(crate) fn sanitize_isolated_chat_attachments(mut text: String) -> String {
    let references = codewhale_core::media_attachment_references(&text);
    for reference in references.into_iter().rev() {
        let replacement = if text[reference.start_byte..reference.end_byte].ends_with('\n') {
            "[Attachment omitted: Runtime Chat cannot read local file references.]\n"
        } else {
            "[Attachment omitted: Runtime Chat cannot read local file references.]"
        };
        text.replace_range(reference.start_byte..reference.end_byte, replacement);
    }
    text
}

/// Snapshot of parent state that can be passed to forked sub-agents without
/// rewriting the parent transcript.
///
/// Deliberately **Work-free**: this is captured once at turn start, and Work
/// state changes during the turn. The To-do section of the fork-state block is
/// resolved at the actual fork seam instead (see
/// `SubAgentForkContext::with_resolved_state_block`), so a `work_update`
/// followed by an `agent` spawn in the same turn hands the child the current
/// list rather than the one that existed before the turn's first tool call.
#[derive(Debug, Clone, Default)]
struct StructuredState {
    mode_label: String,
    workspace: PathBuf,
    cwd: Option<PathBuf>,
    working_set_summary: Option<String>,
    subagent_snapshots: Vec<SubAgentResult>,
}

impl StructuredState {
    async fn capture(
        mode_label: impl Into<String>,
        workspace: PathBuf,
        cwd: Option<PathBuf>,
        working_set: &WorkingSet,
        subagents: Option<&SharedSubAgentManager>,
        active_session_id: &str,
    ) -> Self {
        let working_set_summary = working_set.summary_block(&workspace);

        let subagent_snapshots = if let Some(handle) = subagents {
            let mut guard = handle.write().await;
            guard.cleanup_for_session(active_session_id, Duration::from_secs(60 * 60));
            guard
                .list_for_session(active_session_id)
                .into_iter()
                .filter(|s| matches!(s.status, SubAgentStatus::Running))
                .collect()
        } else {
            Vec::new()
        };

        Self {
            mode_label: mode_label.into(),
            workspace,
            cwd,
            working_set_summary,
            subagent_snapshots,
        }
    }

    #[must_use]
    fn to_system_block(&self) -> Option<String> {
        let mut out = String::new();
        out.push_str("## Fork State\n\n");
        out.push_str(&format!("- Mode: `{}`\n", self.mode_label));
        out.push_str(&format!("- Workspace: `{}`\n", self.workspace.display()));
        if let Some(cwd) = self.cwd.as_ref() {
            out.push_str(&format!("- Cwd: `{}`\n", cwd.display()));
        }

        // No Work section here on purpose: it is appended at the fork seam from
        // the authoritative projection (#3983), because this block is captured
        // at turn start and Work moves during the turn.
        if !self.subagent_snapshots.is_empty() {
            out.push_str("\n### Open Sub-Agents\n");
            for s in &self.subagent_snapshots {
                let role = s.assignment.role.as_deref().unwrap_or("-");
                let goal = if s.assignment.objective.is_empty() {
                    "(no objective set)"
                } else {
                    s.assignment.objective.as_str()
                };
                out.push_str(&format!("- `{}` (role: {}) - {}\n", s.agent_id, role, goal));
            }
        }

        if let Some(working_set) = self.working_set_summary.as_deref() {
            out.push('\n');
            out.push_str(working_set);
            out.push('\n');
        }

        Some(out)
    }
}

fn user_shell_turn_outcome(
    result: &Result<ToolResult, ToolError>,
    cancel_requested: bool,
) -> TurnOutcomeStatus {
    let tool_reported_cancel = result.as_ref().is_ok_and(|tool_result| {
        tool_result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("canceled"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });

    if cancel_requested || tool_reported_cancel {
        TurnOutcomeStatus::Interrupted
    } else if result.as_ref().is_ok_and(|tool_result| tool_result.success) {
        TurnOutcomeStatus::Completed
    } else {
        TurnOutcomeStatus::Failed
    }
}

// === Types ===

/// Configuration for the engine
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Model identifier to use for responses.
    pub model: String,
    /// Route/offering limits for the active provider+model, when the runtime
    /// route resolver had concrete catalog facts.
    pub active_route_limits: Option<codewhale_config::route::RouteLimits>,
    /// Workspace root for tool execution and file operations.
    pub workspace: PathBuf,
    /// Host-owned conversation id the engine adopts at construction.
    ///
    /// Interactive hosts claim a session id before the engine exists: the
    /// per-session Runtime store lock and the first crash checkpoint are both
    /// keyed by it. The engine must run the conversation the host persists,
    /// so it adopts this id instead of minting a second one that the host
    /// only learns about from the first `SessionUpdated` event (which left
    /// the turn-start checkpoint orphaned under the host id). `None`
    /// (headless/embed callers) keeps the generated id.
    pub session_id: Option<String>,
    /// Optional host-owned root for delegated-agent runtime state.
    ///
    /// When unset, the worker ledger, complete transcript artifacts and
    /// coordination lock retain their historical location under
    /// `workspace/.codewhale/state`. Embedders may set a session-scoped root
    /// to separate that control-plane state from the execution workspace.
    /// Child cwd and file authority still derive from `workspace`; hosts using
    /// distinct state roots for the same workspace must coordinate conflicting
    /// writes themselves or isolate writers with worktrees.
    pub subagent_state_root: Option<PathBuf>,
    /// Allow shell tool execution when true.
    pub allow_shell: bool,
    /// Enable trust mode (skip approvals) when true.
    pub trust_mode: bool,
    /// Path to the notes file used by the notes tool.
    pub notes_path: PathBuf,
    /// Path to the MCP configuration file.
    pub mcp_config_path: PathBuf,
    /// OAuth callback overrides (`mcp_oauth_callback_port` / `_url`) so the
    /// self-serve MCP login tool honors a pre-registered redirect URI the
    /// same way `/mcp login` does.
    pub mcp_oauth_callback_port: Option<u16>,
    pub mcp_oauth_callback_url: Option<String>,
    /// Directory containing discoverable skills.
    pub skills_dir: PathBuf,
    /// Restrict skill discovery to CodeWhale-owned roots plus explicit
    /// `skills_dir` configuration.
    pub skills_discovery_mode: crate::skills::SkillDiscoveryMode,
    /// Immutable plugin authority snapshot scoped to `workspace`. Normal App
    /// hosts provide this explicitly; headless/embed callers that leave it
    /// unset receive a fresh workspace-specific snapshot in [`Engine::new`].
    pub plugin_registry: Option<Arc<crate::plugins::PluginRegistry>>,
    /// Sources injected as `<instructions source="…">` blocks in the system
    /// prompt (#454). Each entry is either a disk path (read at render time)
    /// or an inline string. Loaded in declared order from the user's
    /// `instructions = [...]` config or constructed by embedders.
    ///
    /// Generalized from `Vec<PathBuf>` so embedders can inject inline content
    /// without staging a disk file. `From<PathBuf>` impl keeps existing callers
    /// working with `.into()` at the call site.
    pub instructions: Vec<crate::prompts::InstructionSource>,
    pub project_context_pack_enabled: bool,
    /// When true, the model is instructed to respond in the current locale
    /// and a post-hoc translation layer replaces remaining English output.
    pub translation_enabled: bool,
    pub verbosity: Option<String>,
    /// Maximum number of assistant steps before stopping. Ordinary interactive
    /// hosts use [`DEFAULT_MODEL_STEPS`]; explicit test/embed callers may
    /// still install a finite boundary.
    pub max_steps: u32,
    /// Maximum number of concurrently active subagents.
    pub max_subagents: usize,
    /// Maximum queued + running sub-agents admitted for this engine session.
    pub max_admitted_subagents: usize,
    /// Number of direct (depth-1) sub-agents that may execute concurrently
    /// before further launches queue for a launch slot (#3095).
    /// Resolved from `[subagents] launch_concurrency`.
    pub launch_concurrency: usize,
    /// Whether the model-facing `agent` tool is available after applying
    /// feature flags and `[subagents]` opt-out controls.
    pub subagents_enabled: bool,
    /// Feature flags controlling tool availability.
    pub features: Features,
    /// Deterministic auto-review policy for tool calls.
    pub auto_review_policy: crate::core::authority::auto_review::AutoReviewPolicy,
    /// Auto-compaction settings for long conversations.
    pub compaction: CompactionConfig,
    /// Shared Todo list state.
    pub todos: SharedTodoList,
    /// Shared Plan state.
    pub plan_state: SharedPlanState,
    /// Shared runtime goal state for model-visible goal tools.
    pub goal_state: SharedGoalState,
    /// Maximum sub-agent recursion depth (default 3). See
    /// `SubAgentRuntime::max_spawn_depth`. Override via
    /// `[subagents] max_depth = N` in `~/.codewhale/config.toml`.
    pub max_spawn_depth: u32,
    /// Per-domain network policy decider (#135). Shared across the session so
    /// session-scoped approvals (`/network allow <host>`) persist for the
    /// remainder of the run.
    pub network_policy: Option<crate::network_policy::NetworkPolicyDecider>,
    /// Whether to take side-git workspace snapshots before/after each turn.
    pub snapshots_enabled: bool,
    /// Maximum workspace size (in bytes) before snapshots self-disable on
    /// first init. `0` disables the cap. Resolved from
    /// `[snapshots] max_workspace_gb` × 1 GB at engine construction.
    pub snapshots_max_workspace_bytes: u64,
    /// The host records every `Event::WorkspaceSnapshotTaken` receipt on the
    /// running turn and resolves turn-scoped undo from them (the Runtime
    /// API). The engine then:
    ///
    /// - takes the post-turn snapshot *before* `TurnComplete`, so its receipt
    ///   belongs to the turn it closes — one arriving after `TurnComplete`
    ///   would land after the turn settled, on the next turn, or nowhere once
    ///   the engine is evicted;
    /// - bounds every tool call that may write (any call not read-only) with
    ///   a `tool` snapshot before it and a `post_tool` snapshot after it, so
    ///   the spans in which the turn's own tools ran are known and a change
    ///   made outside them (another thread, an editor) is never taken for the
    ///   turn's;
    /// - reports on each receipt the paths changed since the turn's previous
    ///   one.
    ///
    /// Interactive hosts keep `false`: the TUI does not record receipts,
    /// keeps the post-turn snapshot off its input path (#234), and snapshots
    /// only before file-writing tools.
    pub record_restore_points: bool,
    /// Post-edit LSP diagnostics injection (#136). When `None`, the engine
    /// constructs a disabled manager so the field is always present.
    pub lsp_config: Option<crate::lsp::LspConfig>,
    /// Durable runtime services exposed to model-visible tools.
    pub runtime_services: RuntimeToolServices,
    /// Per-role/type sub-agent model overrides already resolved from config.
    pub subagent_model_overrides: HashMap<String, crate::config::SubagentModelOverride>,
    /// Merged fleet roster (built-ins + config + personal/workspace agent
    /// files) shared by model-spawned sub-agents and fleet dispatch
    /// (#fleet-roster cutover (v0.8.67)). Defaults to built-ins only; the
    /// engine-config construction sites load it at session start and the setup
    /// wizard refreshes it after each successful profile save.
    pub fleet_roster: std::sync::Arc<crate::fleet::roster::FleetRoster>,
    /// Whether the user-memory feature is enabled (#489). When `true` the
    /// engine reads `memory_path` on each prompt assembly and prepends a
    /// `<user_memory>` block to the system prompt.
    pub memory_enabled: bool,
    /// Path to the user memory file (#489). Always populated; only
    /// consulted when `memory_enabled` is `true`.
    pub memory_path: PathBuf,
    /// Default directory for Xiaomi MiMo speech/TTS tool outputs.
    pub speech_output_dir: Option<PathBuf>,
    pub vision_config: Option<crate::config::VisionModelConfig>,
    pub goal_objective: Option<String>,
    pub goal_token_budget: Option<u32>,
    pub goal_status: GoalStatus,
    /// Safety backstop on automatic goal continuation passes (#5052).
    /// Resolved from `[goal] max_continuations` in config.toml; `0` disables
    /// the backstop so only completion, blocked state, or the continuation
    /// limit stops an operate-mode goal run.
    pub goal_max_continuations: u32,
    /// Delay between successful interactive goal turns. `0` continues
    /// immediately; positive values opt coordinator goals into a cancellable
    /// quiet period (#5508).
    pub goal_continuation_delay_seconds: u64,
    /// Whether a goal's `token_budget` is a hard stop (`BudgetLimit`) instead
    /// of advisory telemetry. Resolved from `[goal] enforce_token_budget`;
    /// default `false` (#6013).
    pub goal_enforce_token_budget: bool,
    /// Maximum number of automatic re-requests when the model returns only
    /// reasoning without any answer or tool call. Defaults to 2.
    /// Resolved from `[reasoning_only] max_reprompts` in config.toml.
    pub reasoning_only_max_reprompts: u32,
    /// Nudge carried by a reasoning-only re-request once a bare retry has
    /// already come back answerless. `None` uses the built-in text; an empty
    /// string disables the nudge and keeps every retry a bare one.
    ///
    /// The nudge is attached to one outbound request and never added to the
    /// session — see the reasoning-only branch in `turn_loop`.
    /// Resolved from `[reasoning_only] reprompt_message` in config.toml.
    pub reasoning_only_reprompt_message: Option<String>,
    /// Tool restriction from custom slash command frontmatter.
    /// `None` means the current turn may use the normal tool set.
    pub allowed_tools: Option<Vec<String>>,
    /// Tool deny-list.  Deny always wins over allow (#3027).
    /// `None` means no tools are explicitly denied.
    pub disallowed_tools: Option<Vec<String>>,
    /// Hard per-turn cap on admitted tool calls (#4415). `None` (the default)
    /// means unlimited and leaves the turn admission gate inert. Task hosts
    /// set this from the task's structured `max_tool_calls` constraint; the
    /// per-turn counter itself lives in the turn loop, not here.
    pub max_tool_calls: Option<u32>,
    /// Hook executor for control-plane hooks.
    /// `ToolCallBefore` hooks may deny a tool call with exit code 2.
    pub hook_executor: Option<std::sync::Arc<crate::hooks::HookExecutor>>,
    /// Resolved BCP-47 locale tag (e.g. `"en"`, `"zh-Hans"`, `"ja"`)
    /// for the `## Environment` block in the system prompt. The
    /// caller resolves this from `Settings` once at engine
    /// construction; the engine never touches disk for it.
    pub locale_tag: String,
    /// When true, force `tool_choice: "required"` and opt compatible function
    /// schemas into DeepSeek beta strict mode.
    pub strict_tool_mode: bool,
    /// Workshop / large-tool-output routing (#548). `None` disables routing.
    pub workshop: Option<crate::tools::large_output_router::WorkshopConfig>,
    /// Which search backend `web_search` should use. Default: Firecrawl.
    pub search_provider: crate::config::SearchProvider,
    /// Optional Firecrawl key, or required key for other API search providers.
    /// Metaso also falls back to the `METASO_API_KEY` env var.
    /// Baidu also falls back to `BAIDU_SEARCH_API_KEY`.
    pub search_api_key: Option<String>,
    /// Optional DuckDuckGo-compatible HTML endpoint override.
    pub search_base_url: Option<String>,
    /// `Config::search_native`: `None` lets provider-native search lead on
    /// routes that offer it; `Some(false)` keeps the chosen provider first.
    pub search_native: Option<bool>,
    /// Per-step DeepSeek API timeout for sub-agent `create_message` requests.
    /// Resolved from `[subagents] api_timeout_secs` (clamped to 1..=3600)
    /// once at engine construction, then threaded onto every
    /// `SubAgentRuntime` the engine builds (#1806, #1808).
    pub subagent_api_timeout: Duration,
    /// Per-SSE-chunk idle timeout for streamed model responses.
    /// Resolved from `[tui].stream_chunk_timeout_secs` (or the legacy
    /// `DEEPSEEK_STREAM_IDLE_TIMEOUT_SECS`) and updated live by `/config`.
    pub stream_chunk_timeout: Duration,
    /// Cumulative wall-clock budget for one turn (R1). Counted across every
    /// model step of the turn, excluding time blocked on a human approval
    /// decision. Resolved from `[tui].turn_wall_clock_secs`; `Duration::MAX`
    /// (no limit) by default — see [`turn_budget::resolve_turn_wall_clock`].
    pub turn_wall_clock: Duration,
    /// Per-step cap on accumulated streamed content, in bytes (R1). Resolved
    /// from `[tui].stream_max_content_mb`. Pre-R1 this was the hard-coded
    /// `STREAM_MAX_CONTENT_BYTES`; it is still finite by default and now
    /// overridable.
    pub stream_max_content_bytes: usize,
    /// Per-step cap on a single stream's wall-clock duration (R1). Resolved
    /// from `[tui].stream_max_duration_secs`. Pre-R1 this was the hard-coded
    /// `STREAM_MAX_DURATION_SECS`.
    pub stream_max_duration: Duration,
    /// Stream-level retry budgets (#6700): whole-request resumes (also spent
    /// by stream-open failures, #6699), in-stream transparent retries, and
    /// the per-stream error streak. Resolved from `[tui].stream_max_resumes`,
    /// `[tui].stream_max_transparent_retries` and `[tui].stream_max_errors`;
    /// the defaults are the historical compiled-in values.
    pub stream_retry_limits: turn_budget::StreamRetryLimits,
    /// Bounded wait for SSE response headers (#6700). Resolved from
    /// `[tui].stream_open_timeout_secs`, then
    /// `CODEWHALE_STREAM_OPEN_TIMEOUT_SECS`; only the awaiting-model
    /// heartbeat bound reads it here — the client owns the real timeout.
    pub stream_open_timeout: Duration,
    /// No-progress heartbeat timeout for live sub-agents. Used by the manager
    /// and parent wait loop to auto-cancel stuck children before they exhaust
    /// the sub-agent slot pool indefinitely (#2614).
    pub subagent_heartbeat_timeout: Duration,
    /// Native tools that should stay in the model-visible catalog even when
    /// they are outside the small default core surface (#2076).
    pub tools_always_load: HashSet<String>,
    /// Effective `request_user_input` payload ceilings resolved from `[tools]`
    /// (#5949). One authority for the validator, the tool schema, and the
    /// tool description.
    pub user_input_limits: crate::tools::user_input::UserInputLimits,
    /// Wait for a user-input answer before cancelling it (#6003). `None`
    /// or `Some(Duration::ZERO)` waits until the person answers or cancels.
    /// A positive duration is one absolute deadline for that wait.
    pub user_input_timeout: Option<Duration>,
    /// Per-turn step allowance while a goal is active (#5994). Hosts opt in
    /// with their resolved `[goal] max_steps`; `None` keeps the ordinary
    /// `max_steps` ceiling for goal turns too — which is what exec/worker
    /// paths with explicit per-invocation ceilings must see.
    pub goal_max_steps: Option<u32>,
    /// When true and `/usr/bin/bwrap` is executable on Linux, route exec_shell
    /// through bubblewrap (#2184).
    pub prefer_bwrap: bool,
    /// User-configured bwrap mount extensions (#5410): extra read-only roots
    /// and writable device nodes such as `/dev/null`.
    pub bwrap_extensions: crate::sandbox::BwrapMountExtensions,
    /// Sandbox read deny-list (S1). One source of truth for two enforcement
    /// points: the OS wrappers get its subtree paths, and the in-process
    /// file-reading tools consult it directly (they run inside the harness
    /// process and are never wrapped by `sandbox-exec` or `bwrap`).
    /// Defense-in-depth, not a security boundary — see
    /// `crate::sandbox::read_guard` for what it does and does not stop.
    pub read_denylist: crate::sandbox::read_guard::ReadDenylist,
    /// Tool override and plugin configuration (`[tools]` table in config.toml).
    /// Applied to the per-turn tool registry after built-in tools are registered.
    /// When `None`, no overrides or plugin loading occurs.
    pub tools: Option<crate::config::ToolsConfig>,
    /// Whether tools should follow symbolic links. When `true`, symlinked
    /// directories are traversed by walk-based tools and symlinked paths
    /// that resolve outside the workspace are still allowed (the symlink
    /// itself must be inside the workspace). Mirrors the
    /// `workspace_follow_symlinks` setting.
    pub workspace_follow_symlinks: bool,
    /// Ask-only permission rules loaded from sibling `permissions.toml`.
    pub exec_policy_engine: codewhale_execpolicy::ExecPolicyEngine,
    /// Whether turn startup may write terminal title/taskbar OSC sequences.
    /// Interactive TUI sessions enable this; headless and machine-readable
    /// hosts disable it so stdout remains protocol-clean.
    pub terminal_chrome_enabled: bool,
    /// Resolved advisor watcher configuration (#3982). Off by default.
    /// Updated live by `Op::SetAdvisorEnabled`.
    pub advisor_config: crate::tools::subagent::AdvisorConfig,
}

/// Uncapped model steps for hosts without an explicit configured ceiling.
/// Wall-clock and stream budgets are independent. See
/// [`turn_budget::resolve_max_model_steps`].
pub(crate) const DEFAULT_MODEL_STEPS: u32 = turn_budget::DEFAULT_MAX_MODEL_STEPS;

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            model: DEFAULT_TEXT_MODEL.to_string(),
            active_route_limits: None,
            workspace: PathBuf::from("."),
            session_id: None,
            subagent_state_root: None,
            allow_shell: true,
            trust_mode: false,
            notes_path: PathBuf::from("notes.txt"),
            mcp_config_path: PathBuf::from("mcp.json"),
            mcp_oauth_callback_port: None,
            mcp_oauth_callback_url: None,
            skills_dir: crate::skills::default_skills_dir(),
            skills_discovery_mode: crate::skills::SkillDiscoveryMode::Compatible,
            plugin_registry: None,
            instructions: Vec::new(),
            project_context_pack_enabled: false,
            translation_enabled: false,
            // Callers opt into a finite model-step boundary explicitly.
            max_steps: DEFAULT_MODEL_STEPS,
            max_subagents: DEFAULT_MAX_SUBAGENTS,
            max_admitted_subagents: DEFAULT_MAX_SUBAGENTS,
            launch_concurrency: DEFAULT_MAX_SUBAGENTS,
            subagents_enabled: true,
            features: Features::with_defaults(),
            auto_review_policy: crate::core::authority::auto_review::AutoReviewPolicy::default(),
            compaction: CompactionConfig::default(),
            todos: new_shared_todo_list(),
            plan_state: new_shared_plan_state(),
            goal_state: new_shared_goal_state(),
            max_spawn_depth: crate::tools::subagent::DEFAULT_MAX_SPAWN_DEPTH,
            network_policy: None,
            snapshots_enabled: true,
            snapshots_max_workspace_bytes:
                crate::snapshot::DEFAULT_MAX_WORKSPACE_BYTES_FOR_SNAPSHOT,
            record_restore_points: false,
            lsp_config: None,
            runtime_services: RuntimeToolServices::default(),
            subagent_model_overrides: HashMap::new(),
            fleet_roster: std::sync::Arc::new(crate::fleet::roster::FleetRoster::built_ins_only()),
            memory_enabled: false,
            memory_path: PathBuf::from("./memory.md"),
            speech_output_dir: None,
            vision_config: None,
            strict_tool_mode: false,
            goal_objective: None,
            goal_token_budget: None,
            goal_status: GoalStatus::Active,
            goal_max_continuations: crate::goal_loop::DEFAULT_MAX_GOAL_CONTINUATIONS,
            goal_continuation_delay_seconds: 0,
            goal_enforce_token_budget: false,
            reasoning_only_max_reprompts: crate::config::DEFAULT_REASONING_ONLY_REPROMPTS,
            // `None` means "use the built-in nudge". Storing the default text
            // here instead would make an operator's explicit empty string
            // indistinguishable from having set nothing.
            reasoning_only_reprompt_message: None,
            allowed_tools: None,
            disallowed_tools: None,
            max_tool_calls: None,
            hook_executor: None,
            locale_tag: "en".to_string(),
            workshop: None,
            search_provider: crate::config::SearchProvider::default(),
            search_api_key: None,
            search_base_url: None,
            search_native: None,
            subagent_api_timeout: Duration::from_secs(
                crate::config::DEFAULT_SUBAGENT_API_TIMEOUT_SECS,
            ),
            stream_chunk_timeout: Duration::from_secs(
                crate::config::DEFAULT_STREAM_CHUNK_TIMEOUT_SECS,
            ),
            turn_wall_clock: turn_budget::resolve_turn_wall_clock(None),
            stream_max_content_bytes: turn_budget::DEFAULT_STREAM_MAX_CONTENT_BYTES,
            stream_max_duration: Duration::from_secs(turn_budget::DEFAULT_STREAM_MAX_DURATION_SECS),
            stream_retry_limits: turn_budget::StreamRetryLimits::default(),
            stream_open_timeout: crate::client::resolve_stream_open_timeout(None),
            subagent_heartbeat_timeout: Duration::from_secs(
                crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS,
            ),
            tools_always_load: HashSet::new(),
            user_input_limits: crate::tools::user_input::UserInputLimits::default(),
            user_input_timeout: None,
            goal_max_steps: None,
            // Mirrors `Config::prefers_bwrap`: on unless explicitly opted out.
            prefer_bwrap: true,
            bwrap_extensions: crate::sandbox::BwrapMountExtensions::default(),
            // Fail-closed (F7): `Engine::new` unconditionally installs this
            // list process-wide via `read_guard::set_active`, so a default
            // here must be the built-in credential-store defaults — an empty
            // list would override the safe fallback and fail open. Mirrors
            // `Config::default`, where `sandbox_read_denylist_defaults`
            // defaults to true.
            read_denylist: crate::sandbox::read_guard::ReadDenylist::build(true, &[], &[]),
            verbosity: None,
            tools: None,
            workspace_follow_symlinks: false,
            exec_policy_engine: codewhale_execpolicy::ExecPolicyEngine::new(Vec::new(), Vec::new()),
            terminal_chrome_enabled: true,
            advisor_config: crate::tools::subagent::AdvisorConfig::disabled(),
        }
    }
}

/// Reason the active turn was cancelled. The token from `tokio_util`
/// does not carry a cause, so the engine keeps a sibling latch for
/// approval and user-input waits that need to explain cancellation.
///
/// `External`, `Preempted`, and `Internal` are reserved for the
/// remaining direct cancellation paths tracked in #1541.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// User-initiated cancel (Esc, `/cancel`, click cancel on modal).
    User,
    /// External / runtime-API cancel (HTTP `DELETE /v1/threads/...`,
    /// task manager stop, parent agent cancel).
    External,
    /// Cancel triggered when a new turn starts before the previous one
    /// finished — e.g. plain Enter while busy after the queueing path
    /// pre-empts the running turn.
    #[expect(dead_code)]
    Preempted,
    /// Engine internals tore down the turn (drop, channel close,
    /// shutdown). Rare — surfaced as an internal error.
    Internal,
    /// The UI watchdog ended a turn that stopped making progress.
    Stalled,
}

impl CancelReason {
    fn describe(self) -> &'static str {
        match self {
            Self::User => "user cancelled the request",
            Self::External => "request cancelled by external caller",
            Self::Preempted => "request was preempted by a new turn",
            Self::Internal => "engine torn down before approval resolved",
            Self::Stalled => "the turn stalled and was ended by the watchdog",
        }
    }
}

/// Handle to communicate with the engine
#[derive(Clone)]
pub struct EngineHandle {
    goal_state: SharedGoalState,
    /// Send operations to the engine
    pub tx_op: mpsc::Sender<Op>,
    /// Receive events from the engine
    pub rx_event: Arc<RwLock<mpsc::Receiver<Event>>>,
    /// Shared pointer to the cancellation token for the current request.
    cancel_token: Arc<StdMutex<CancellationToken>>,
    /// Latched reason for the most recent cancellation. Read by the
    /// approval / user-input handlers to enrich their error strings.
    /// Cleared by the engine when a fresh turn starts.
    cancel_reason: Arc<StdMutex<Option<CancelReason>>>,
    /// Send approval decisions to the engine
    tx_approval: mpsc::Sender<ApprovalDecision>,
    /// Send user input responses to the engine
    tx_user_input: mpsc::Sender<UserInputDecision>,
    /// Send steer input for an in-flight turn.
    tx_steer: mpsc::Sender<handle::SteerInput>,
    turn_controls: Arc<StdMutex<handle::TurnControls>>,
    /// Shared pause flag set by the TUI and read by the turn loop.
    shared_paused: Arc<StdMutex<bool>>,
    /// Whether the host must construct the route's concrete provider client
    /// before it mutates turn state. Real engines own concrete provider I/O;
    /// explicit injected/mock engines own that seam themselves.
    client_preflight_required: bool,
    /// Typed live permission authority shared with the running turn. A mode
    /// change publishes here before its mailbox op is queued, so gates never
    /// consult a stale per-turn copy.
    live_runtime_authority: Arc<StdMutex<LiveRuntimeAuthorityState>>,
    /// Out-of-band authority for one exact compaction request. The engine can
    /// be awaiting a provider while its bounded op mailbox is unable to drain,
    /// so cancellation cannot depend on processing a later mailbox entry.
    compaction_cancellation: Arc<StdMutex<CompactionCancellationState>>,
    /// Read-only view of the engine's turn-phase heartbeat (#6184).
    turn_heartbeat: Arc<turn_heartbeat::TurnHeartbeat>,
    /// Where an agent's pending approval waits. An answer to one is handed
    /// straight to it: the engine may be streaming or running tools and not
    /// reading approvals for a long time.
    subagent_manager: SharedSubAgentManager,
    terminal_worker_events: Option<Arc<TerminalEventDelivery>>,
}

const MAX_PENDING_COMPACTION_CANCELLATIONS: usize = 64;

#[derive(Debug, Default)]
struct CompactionCancellationState {
    active: Option<(String, CancellationToken)>,
    pending: VecDeque<String>,
}

impl CompactionCancellationState {
    fn request(&mut self, id: &str) {
        if let Some((active_id, token)) = self.active.as_ref()
            && active_id == id
        {
            token.cancel();
            return;
        }
        if self.pending.iter().any(|pending| pending == id) {
            return;
        }
        if self.pending.len() >= MAX_PENDING_COMPACTION_CANCELLATIONS {
            self.pending.pop_front();
        }
        self.pending.push_back(id.to_string());
    }

    fn claim(&mut self, id: &str) -> Option<CancellationToken> {
        if let Some(index) = self.pending.iter().position(|pending| pending == id) {
            self.pending.remove(index);
            return None;
        }
        let token = CancellationToken::new();
        self.active = Some((id.to_string(), token.clone()));
        Some(token)
    }

    fn finish(&mut self, id: &str) {
        if self
            .active
            .as_ref()
            .is_some_and(|(active_id, _)| active_id == id)
        {
            self.active = None;
        }
        if let Some(index) = self.pending.iter().position(|pending| pending == id) {
            self.pending.remove(index);
        }
    }
}

impl EngineHandle {
    #[must_use]
    pub(crate) fn subagent_manager(&self) -> SharedSubAgentManager {
        self.subagent_manager.clone()
    }

    pub(crate) fn mark_worker_event_delivery_failed(&self) {
        if let Some(events) = self.terminal_worker_events.as_ref() {
            events.mark_failed();
        }
    }

    pub(crate) async fn wait_terminal_worker_events(&self) -> Result<()> {
        if let Some(events) = self.terminal_worker_events.as_ref() {
            tokio::time::timeout(FOREGROUND_CHILD_SETTLE_GRACE, events.wait())
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "terminal worker event delivery timed out during Runtime shutdown"
                    )
                })??;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn set_subagent_manager(&mut self, manager: SharedSubAgentManager) {
        self.subagent_manager = manager;
    }

    /// Publish typed compaction cancellation immediately, then enqueue the
    /// matching operation when capacity permits. The shared authority is what
    /// stops a running provider future; the operation keeps the mailbox
    /// protocol explicit and clears a late, already-settled request safely.
    pub fn cancel_compaction(&self, id: impl Into<String>) -> Result<()> {
        let id = id.into();
        self.compaction_cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .request(&id);
        match self.tx_op.try_send(Op::CancelCompaction { id }) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(anyhow::anyhow!("engine operation channel closed"))
            }
        }
    }
}

// `impl EngineHandle { ... }` moved to `engine/handle.rs` so the
// mailbox API can be reviewed independently of the engine internals.

// === Engine ===

/// Background MCP boot progress from the spawn-time connect task.
enum McpBootUpdate {
    Progress {
        generation: u64,
        authority_errors: Arc<HashMap<String, String>>,
        connection_errors: HashMap<String, String>,
        connecting: Vec<String>,
    },
    Finished {
        generation: u64,
        authority_errors: Arc<HashMap<String, String>>,
        connection_errors: HashMap<String, String>,
    },
}

type ExplicitConnectJoin =
    Result<(String, Result<crate::mcp::McpConnection, anyhow::Error>), tokio::task::JoinError>;

/// In-flight connects a turn started because its explicit tool selection
/// named them (#6033). `names` tracks the still-unresolved servers so an
/// abort can clear their `connecting` marks in the pool; the catalog
/// generation pins the authority the batch was spawned under.
struct ExplicitMcpConnects {
    connects: tokio::task::JoinSet<(String, Result<crate::mcp::McpConnection, anyhow::Error>)>,
    names: HashSet<String>,
    catalog_generation: u64,
}

mod child_host;
#[cfg(test)]
pub(crate) use child_host::{ChildProbeCall, ChildSurfaceProbe};
mod host_profile;
pub(crate) mod rlm_host;
pub(crate) use host_profile::{EngineHostProfile, TurnNarrowing};

/// The core engine that processes operations and emits events
enum EngineHostSetup {
    Child(child_host::ChildHostSetup),
    Rlm(rlm_host::RlmHostSetup),
}

pub(crate) fn compaction_checkpoint_for_restore(
    messages: &[codewhale_models::Message],
    system_prompt: Option<codewhale_models::SystemPrompt>,
) -> Option<codewhale_models::SystemPrompt> {
    extract_compaction_summary_prompt(system_prompt).or_else(|| {
        // Current Engine projections carry the checkpoint in history,
        // with a stripped system prompt. Only the existing structural
        // validator can authorize a history block as that checkpoint.
        messages.iter().rev().find_map(|message| {
            if !crate::compaction::is_wire_compaction_checkpoint_message(message) {
                return None;
            }
            match &message.content[0] {
                codewhale_models::ContentBlock::Text { text, .. } => {
                    Some(codewhale_models::SystemPrompt::Text(text.clone()))
                }
                _ => None,
            }
        })
    })
}

/// The history a restore installs: sub-agent handoffs become restored
/// checkpoints and the compaction checkpoint replaces its summary in place.
/// Resume acknowledgement compares against this same projection.
pub(crate) fn project_history_for_restore(
    messages: Vec<codewhale_models::Message>,
    compaction_checkpoint: Option<&codewhale_models::SystemPrompt>,
) -> Vec<codewhale_models::Message> {
    crate::compaction::restore_compaction_checkpoint(
        crate::runtime_handoff::project_owned_messages_for_restore(messages),
        compaction_checkpoint,
    )
}

pub struct Engine {
    rlm_host: Option<rlm_host::RlmHostState>,
    host_profile: EngineHostProfile,
    child_host: Option<child_host::ChildHostState>,
    turn_narrowing: TurnNarrowing,
    turn_acp_shell_ceiling: Option<bool>,
    config: EngineConfig,
    api_config: Config,
    /// Runtime-host authority consulted only when constructing a later turn
    /// descriptor (goal continuation, idle child completion, `/edit`). Active
    /// turns keep their already-installed immutable descriptor.
    authoritative_route_config: Option<Arc<parking_lot::RwLock<Config>>>,
    codewhale_client: Option<CodewhaleClient>,
    /// Provider-neutral client used by the canonical main turn loop. Concrete
    /// clients remain temporarily available to provider-specific helper tools
    /// while those boundaries migrate independently.
    model_client: Option<SharedModelClient>,
    /// Test/embedding seam: an explicitly injected provider-neutral client
    /// remains the I/O authority while typed routes still validate receipts,
    /// endpoint metadata, and budgets.
    model_client_injected: bool,
    codewhale_client_error: Option<String>,
    api_key_env_only_recovery: Option<String>,
    session: Session,
    /// One lazy, session-scoped working kernel for inline `repl` blocks.
    /// Its context is refreshed before each run, while user-created Python
    /// state stays alive across model turns.
    repl_kernel: Option<crate::repl::PythonRuntime>,
    subagent_manager: SharedSubAgentManager,
    subagent_execution_limits: Option<SubAgentExecutionLimits>,
    /// The deterministic Auto-Review policy shared with every child runtime
    /// so children are gated by the same rules as the parent turn.
    shared_auto_review_policy: Arc<crate::core::authority::auto_review::AutoReviewPolicy>,
    shell_manager: SharedShellManager,
    /// Read-before-edit snapshots live for the session, not for one turn's
    /// transient `ToolContext` (#4475).
    file_read_tracker: SharedFileReadTracker,
    mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
    /// The tool-surface budget the current turn's catalog was shaped with,
    /// so a mid-turn MCP refresh reshapes its slice the same way (#5939).
    turn_tool_surface_budget: Option<crate::model_profile::ToolSurfaceBudget>,
    /// Last connection diagnosis for each configured MCP server.
    ///
    /// Failed transports are intentionally absent from `McpPool::connections`,
    /// so a later one-server retry cannot reconstruct sibling failures from
    /// the pool alone. Keeping the diagnoses beside the engine-owned pool
    /// lets every full manager snapshot remain truthful without reconnecting
    /// unrelated servers.
    mcp_connection_errors: HashMap<String, String>,
    /// True while the spawn-time concurrent connect pass is still running.
    /// `mcp_tools` snapshots ready servers instead of waiting on optionals.
    mcp_boot_in_flight: bool,
    mcp_boot_rx: Option<mpsc::Receiver<McpBootUpdate>>,
    /// Supervisor sweep updates. `Some` while the supervisor task is armed;
    /// the channel closing (task exited with the pool) disarms it and the
    /// next pool ensure respawns.
    mcp_supervisor_rx: Option<mpsc::Receiver<McpSupervisorUpdate>>,
    /// Abort the supervisor's reconnect handshakes when its pool is retired.
    mcp_supervisor_task: Option<tokio::task::AbortHandle>,
    mcp_boot_done: Option<tokio::sync::watch::Receiver<bool>>,
    /// Generation owned by the currently installed boot receiver. Terminal
    /// cleanup is conditional on this exact value so an older pass can never
    /// clear a newer receiver.
    mcp_boot_generation: Option<u64>,
    /// Abort handle for the boot pass that owns `mcp_boot_generation`. A
    /// session or workspace boundary drops the pool that pass is dialing
    /// into, so it aborts the pass instead of letting it run on (C02-08).
    mcp_boot_task: Option<tokio::task::AbortHandle>,
    /// Monotonic generation for engine-authored MCP session snapshots. Boot
    /// task updates retain their spawn generation so later passes can reject
    /// only genuinely stale work.
    mcp_event_generation: u64,
    /// Workspace-scoped immutable plugin catalogue and authority receipts.
    plugin_registry: Arc<crate::plugins::PluginRegistry>,
    /// This engine's hold on the process-wide extension host (`[features]
    /// extension_host`), carrying `plugin_registry`. `None` with the flag off
    /// and for engines without a plugin snapshot of their own (isolated
    /// chats), which must never revoke another engine's plugins.
    extension_host: Option<crate::extension_host::HostAttachment>,
    /// Immutable contribution snapshot for the running turn. Re-delivery after
    /// compaction uses these same bytes, never a mid-turn host re-sampling.
    extension_prompt_block: Option<String>,
    constitution_block: Option<String>,
    /// The local-fallback notice for an unavailable account profile is shown
    /// once per engine; later turns only log it.
    profile_constitution_fallback_noticed: bool,
    /// Model requests in a row that failed with the same upstream HTTP
    /// status on the same model (#6889). Cleared when a request opens its
    /// stream or fails some other way.
    repeated_upstream_failure: Option<streaming::RepeatedUpstreamFailure>,
    live_header: crate::prompt_zones::SharedLiveHeader<crate::route_receipt::TurnRouteReceipt>,
    api_provider: ProviderKind,
    /// One captured admitted route. Presentation snapshots derive strings from
    /// it; a changed table cannot be blessed by reinterpreting those strings.
    api_provider_identity: Option<crate::config::ProviderIdentity>,
    active_route_limits: Option<codewhale_config::route::RouteLimits>,
    active_route_capabilities: codewhale_config::route::RouteCapabilities,
    /// The endpoint the current client was built from: base URL, endpoint
    /// key, and wire protocol. Part of the route identity the input-bill
    /// carry-over is keyed on; `None` until a route is installed.
    active_route_endpoint: Option<codewhale_config::route::ResolvedEndpoint>,
    rx_op: mpsc::Receiver<Op>,
    live_runtime_authority: Arc<StdMutex<LiveRuntimeAuthorityState>>,
    compaction_cancellation: Arc<StdMutex<CompactionCancellationState>>,
    /// Clone of the op-channel sender, so the engine can self-dispatch ops
    /// (e.g. a goal-continuation `SendMessage` after a turn completes).
    tx_op: mpsc::Sender<Op>,
    /// At most one engine-owned continuation across capacity-waiting and
    /// enqueued states. The authoritative dynamic-tool set stays here so a
    /// later successful turn can refresh it without adding a second token.
    scheduled_goal_continuation: Option<ScheduledGoalContinuation>,
    goal_continuation_schedule_seq: u64,
    rx_approval: mpsc::Receiver<ApprovalDecision>,
    /// Canonical per-session approval evidence. A missing/unwritable store is
    /// retained as an error so construction can stay infallible while every
    /// approval gate still fails closed.
    approval_receipt_store: Result<ApprovalReceiptStore, String>,
    rx_user_input: mpsc::Receiver<UserInputDecision>,
    rx_steer: mpsc::Receiver<handle::SteerInput>,
    queued_steers: std::collections::VecDeque<handle::SteerInput>,
    turn_controls: Arc<StdMutex<handle::TurnControls>>,
    admitted_turn_control: Option<handle::TurnControl>,
    tx_event: mpsc::Sender<Event>,
    /// Wakeup channel for the parent turn loop when a direct child sub-agent
    /// terminates (issue #756). Cloned into `SubAgentRuntime` so the runtime
    /// can fan completion events back into the engine.
    tx_subagent_completion: mpsc::Sender<SubAgentCompletion>,
    /// Receiver paired with `tx_subagent_completion`. Drained at the
    /// turn-loop's empty-tool_uses branch to surface `<codewhale:subagent.done>`
    /// sentinels into the parent's transcript before deciding to end the turn.
    pub(super) rx_subagent_completion: mpsc::Receiver<SubAgentCompletion>,
    /// Sub-agent completions already injected into the parent transcript.
    /// Channel delivery and watchdog reconciliation both mark this set so a
    /// dropped event can be synthesized once without duplicating a later
    /// delivery.
    delivered_subagent_completion_ids: HashSet<String>,
    cancel_token: CancellationToken,
    shared_cancel_token: Arc<StdMutex<CancellationToken>>,
    /// Latched reason for the current cancellation, mirrored to
    /// `EngineHandle::cancel_reason`. Read by `approval.rs` when
    /// surfacing the "Request cancelled while awaiting …" error so the
    /// user-facing message names a cause.
    pub(super) cancel_reason: Arc<StdMutex<Option<CancelReason>>>,
    tool_exec_lock: Arc<RwLock<()>>,
    turn_counter: u64,
    /// Tree of the running turn's latest restore-point snapshot, the base the
    /// next receipt's `changed_paths` is computed against. `None` before a
    /// turn's first snapshot and after one failed, so a span that cannot be
    /// accounted for is reported as unknown rather than folded into the next.
    restore_point_since: Option<crate::snapshot::SnapshotId>,
    /// Post-edit LSP diagnostics injection (#136). Populated unconditionally
    /// — when LSP is disabled in config, this is an inert manager that
    /// always returns `None` from `diagnostics_for`.
    lsp_manager: Arc<crate::lsp::LspManager>,
    /// External sandbox backend (#516). When `Some`, exec_shell routes commands
    /// through this instead of spawning a local process.
    sandbox_backend: Option<std::sync::Arc<dyn crate::sandbox::backend::SandboxBackend>>,
    /// Session-pinned execution boundary used by model-visible sandbox labels.
    /// This must not be re-probed per turn or metadata bytes can drift.
    sandbox_enforcement: crate::sandbox::policy::SandboxEnforcement,
    /// Live no-new-privileges flag, read once at construction beside
    /// `sandbox_enforcement`: both are fixed for the process, so the per-turn
    /// posture line stays byte-stable for the session.
    no_new_privs_active: Option<bool>,
    /// Diagnostics collected during the current step's tool calls. Drained
    /// and forwarded as a synthetic user message before the next API call.
    pending_lsp_blocks: Vec<crate::lsp::DiagnosticBlock>,
    /// Current operating mode. Updated on `ChangeMode` and `SendMessage`.
    current_mode: AppMode,
    /// R1: cumulative wall-clock budget for the turn currently running.
    /// Restarted at the top of every `run_turn`, checked at the
    /// provider-request boundary, and paused while the turn is blocked on a
    /// human approval decision. It lives on the engine rather than in
    /// `TurnContext` so `request_tool_approval` — which never sees the turn
    /// context — can pause it.
    turn_wall_clock: turn_budget::TurnWallClock,
    /// The most recent authority narrowing, if any (#3947). Kept on the engine
    /// so doctor and debug surfaces can answer "why is this tool unavailable"
    /// with the same record the user and the model already saw.
    last_policy_narrowing: Option<PolicyNarrowingEvent>,
    /// The git snapshot line last emitted in a `<turn_meta>` block this
    /// session (#5187, k3-gap F3). The snapshot re-collects branch/dirty
    /// state every turn, so without change-detection the block's bytes drift
    /// after every edit the model itself makes, defeating cross-turn prefix
    /// stability. `None` until the first block is built; the line is then
    /// emitted only when the snapshot actually changed.
    last_turn_meta_git_snapshot: StdMutex<Option<String>>,
    /// Process-local cache for `estimated_input_tokens`. Memoizes the most
    /// recent token estimate keyed on `(session.messages_revision,
    /// system_prompt_fingerprint)`. Five call sites per turn consult this
    /// (engine capacity checkpoints, seam manager, trim budget, etc.) plus
    /// four TUI / command consumers; the cache turns N×O(messages) walks
    /// into a single recompute on a content change.
    token_estimate_cache: TokenEstimateCache,
    /// Shared pause flag set by the TUI and read before tool execution.
    shared_paused: Arc<StdMutex<bool>>,
    /// Rate-limit + dedup guard for the background advisor watcher (#3982).
    /// `None` until the first turn completes with the advisor enabled, then
    /// held for the session lifetime so state persists across turns.
    advisor_emission_guard: Option<Arc<tokio::sync::Mutex<crate::tools::subagent::EmissionGuard>>>,
    /// Turn-phase heartbeat (#6184): where the active turn is and when it
    /// last made progress. Shared with `EngineHandle` and supervised by the
    /// stall watchdog spawned in `run`.
    pub(crate) turn_heartbeat: Arc<turn_heartbeat::TurnHeartbeat>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveRuntimeAuthority {
    mode: AppMode,
    allow_shell: bool,
    trust_mode: bool,
    auto_approve: bool,
    approval_mode: ApprovalMode,
    configured_sandbox_mode: Option<String>,
}

impl LiveRuntimeAuthority {
    fn from_fields(
        mode: AppMode,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
        configured_sandbox_mode: Option<String>,
    ) -> Self {
        let authority = TurnAuthority::from_effective_fields(
            mode,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
        );
        Self::from_turn_authority(&authority, configured_sandbox_mode)
    }

    fn from_turn_authority(
        authority: &TurnAuthority,
        configured_sandbox_mode: Option<String>,
    ) -> Self {
        let approval_mode = authority.approval_mode_for_session();
        Self {
            mode: authority.mode,
            allow_shell: authority.allow_shell,
            trust_mode: authority.trust_mode,
            auto_approve: authority.auto_approve || approval_mode == ApprovalMode::Bypass,
            approval_mode,
            configured_sandbox_mode,
        }
    }

    /// Whether `self` grants less than `prior` along any axis: a stricter
    /// approval posture, a lost shell/trust/auto-approve bit, a stricter
    /// configured sandbox, or a mode switch that is not a step out of Plan.
    ///
    /// A call the user approved under `prior` stays approved under a posture
    /// that is equal or broader; only a narrowing sends it back for a retry.
    pub(crate) fn narrows(&self, prior: &Self) -> bool {
        fn posture_rank(mode: ApprovalMode) -> u8 {
            match mode {
                ApprovalMode::Never => 0,
                ApprovalMode::Suggest => 1,
                ApprovalMode::Auto => 2,
                ApprovalMode::Bypass => 3,
            }
        }
        fn sandbox_rank(mode: Option<&str>) -> Option<u8> {
            match mode {
                Some("read-only") => Some(0),
                Some("workspace-write") => Some(1),
                Some("external-sandbox") => Some(2),
                None => Some(3),
                // An unknown value cannot be ordered; treat any move to or
                // from it as a narrowing.
                Some(_) => None,
            }
        }
        let mode_narrowed = self.mode != prior.mode && prior.mode != AppMode::Plan;
        let sandbox_narrowed = self.configured_sandbox_mode != prior.configured_sandbox_mode
            && match (
                sandbox_rank(self.configured_sandbox_mode.as_deref()),
                sandbox_rank(prior.configured_sandbox_mode.as_deref()),
            ) {
                (Some(now), Some(before)) => now < before,
                _ => true,
            };
        mode_narrowed
            || sandbox_narrowed
            || posture_rank(self.approval_mode) < posture_rank(prior.approval_mode)
            || (prior.allow_shell && !self.allow_shell)
            || (prior.trust_mode && !self.trust_mode)
            || (prior.auto_approve && !self.auto_approve)
    }

    fn permission_snapshot(&self) -> RuntimePermissionAuthority {
        RuntimePermissionAuthority {
            auto_approve: self.auto_approve,
            trust_mode: self.trust_mode,
            approval_mode: self.approval_mode,
        }
    }
}

#[derive(Debug)]
struct LiveRuntimeAuthorityState {
    revision: u64,
    applied_revision: u64,
    authority: LiveRuntimeAuthority,
}

impl LiveRuntimeAuthorityState {
    fn new(authority: LiveRuntimeAuthority) -> Self {
        Self {
            revision: 0,
            applied_revision: 0,
            authority,
        }
    }
}

/// The session's live permission posture, as every agent it spawned reads it.
///
/// Agents used to copy the posture when they were spawned, so a session the
/// person switched to Full Access kept gating its running agents with the old
/// posture: an agent spawned under Auto-Review went on asking the guardian,
/// and being denied, after the person had granted Full Access. Each agent call
/// now re-reads the posture the person last chose — the same cell the TUI
/// decides that agent's approval prompts against — through the projection
/// the parent turn uses (trust, approval, shell policy, sandbox).
#[derive(Clone)]
pub(crate) struct LivePosture {
    state: Arc<StdMutex<LiveRuntimeAuthorityState>>,
    workspace: PathBuf,
    network_access: crate::core::authority::SandboxNetworkAccess,
}

impl LivePosture {
    /// The posture the person last chose.
    pub(crate) fn read(&self) -> LiveRuntimeAuthority {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .authority
            .clone()
    }

    /// Project the live posture onto one agent call — the same projection the
    /// parent turn applies to its own calls — and return what was read.
    pub(crate) fn apply(&self, context: &mut ToolContext) -> LiveRuntimeAuthority {
        let live = self.read();
        project_turn_authority(
            context,
            &TurnAuthority::from_effective_fields(
                live.mode,
                live.allow_shell,
                live.trust_mode,
                live.auto_approve,
                live.approval_mode,
            ),
            &self.workspace,
            live.configured_sandbox_mode.as_deref(),
            self.network_access,
        );
        live
    }
}

/// Project one permission authority onto a tool context: trust, approval,
/// shell policy, and the sandbox that posture implies. The parent turn and
/// every agent call use this one projection.
fn project_turn_authority(
    context: &mut ToolContext,
    authority: &TurnAuthority,
    workspace: &Path,
    configured_sandbox_mode: Option<&str>,
    network_access: crate::core::authority::SandboxNetworkAccess,
) {
    context.trust_mode = authority.trust_mode;
    context.auto_approve = authority.auto_approve;
    context.approval_mode = authority.approval_mode;
    context.set_shell_policy(authority.shell_policy());
    context.elevated_sandbox_policy =
        Some(authority.sandbox_policy(workspace, configured_sandbox_mode, network_access));
    context.shell_network_denied_hint =
        matches!(authority.mode, AppMode::Plan).then(|| PLAN_SHELL_NETWORK_DENIED_HINT.to_string());
}

#[cfg(test)]
impl LivePosture {
    /// A posture cell a test switches the way the TUI does.
    pub(crate) fn for_tests(workspace: &Path, approval_mode: ApprovalMode) -> Self {
        let posture = Self {
            state: Arc::new(StdMutex::new(LiveRuntimeAuthorityState::new(
                LiveRuntimeAuthority::from_fields(
                    AppMode::Agent,
                    true,
                    false,
                    false,
                    ApprovalMode::Suggest,
                    None,
                ),
            ))),
            workspace: workspace.to_path_buf(),
            network_access: crate::core::authority::SandboxNetworkAccess::Restricted,
        };
        posture.switch_for_tests(approval_mode);
        posture
    }

    pub(crate) fn switch_for_tests(&self, approval_mode: ApprovalMode) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .authority = LiveRuntimeAuthority::from_fields(
            AppMode::Agent,
            true,
            false,
            false,
            approval_mode,
            None,
        );
    }
}

/// Runtime-facing view of the engine's exact live permission authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimePermissionAuthority {
    pub(crate) auto_approve: bool,
    pub(crate) trust_mode: bool,
    pub(crate) approval_mode: ApprovalMode,
}

fn claim_subagent_completion(
    delivered_ids: &mut HashSet<String>,
    completion: SubAgentCompletion,
) -> Option<SubAgentCompletion> {
    delivered_ids
        .insert(completion.agent_id.clone())
        .then_some(completion)
}

fn claim_subagent_completion_for_session(
    delivered_ids: &mut HashSet<String>,
    active_session_id: &str,
    completion: SubAgentCompletion,
) -> Option<SubAgentCompletion> {
    if completion.owner_session_id != active_session_id {
        tracing::warn!(
            target: "subagent",
            agent_id = %completion.agent_id,
            owner_session_id = %completion.owner_session_id,
            active_session_id,
            "discarding sub-agent completion for an inactive session"
        );
        return None;
    }
    claim_subagent_completion(delivered_ids, completion)
}

#[derive(Debug)]
enum GoalContinuationAction {
    Inactive,
    Dispatch {
        content: String,
        snapshot: Box<GoalSnapshot>,
    },
    Stopped {
        message: String,
        reason: GoalPauseReason,
    },
}

struct ScheduledGoalContinuation {
    id: u64,
    dynamic_tools: Vec<DynamicToolSpec>,
    enqueued: bool,
    /// `Some` only while the configured between-turn quiet period is active.
    /// Once it expires the same schedule record becomes the existing queued
    /// `ContinueGoal` token; there is no second scheduler.
    ready_at: Option<Instant>,
    /// Retained after expiry so a cancellation racing the timer can still
    /// publish an interrupted wait receipt before provider dispatch.
    was_delayed: bool,
}

enum SendMessageOutcome {
    NotStarted {
        error: Option<String>,
    },
    Finished {
        status: TurnOutcomeStatus,
        error: Option<String>,
    },
}

/// Idle-poll cadence for unclaimed background shell completion while a
/// goal is active. Coarse on purpose: this is a liveness backstop, not an
/// animation loop.
const SHELL_WAKE_POLL_MS: u64 = 750;

enum EngineRunInput {
    Operation(Box<Op>),
    SubAgentCompletion(SubAgentCompletion),
    /// A background shell job finished while the engine sat idle with an
    /// active goal. Shell completion is pull-only (no channel), so without
    /// this wake an active goal waiting on background work stayed inert until
    /// the user typed something (morning-report continuation gap).
    ShellCompletionWake,
    /// One MCP boot progress/settled update from the spawn-time connect task.
    McpBootUpdate(McpBootUpdate),
    /// One connection-supervisor sweep: deaths, recoveries, failed attempts.
    McpSupervisorUpdate(McpSupervisorUpdate),
}

impl SendMessageOutcome {
    fn started(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }
}

// === Internal tool helpers ===

fn subagent_mailbox_message_is_best_effort(message: &MailboxMessage) -> bool {
    matches!(
        message,
        MailboxMessage::Progress { .. }
            | MailboxMessage::ToolCallStarted { .. }
            | MailboxMessage::ToolCallCompleted { .. }
    )
}

const SUBAGENT_MAILBOX_BEST_EFFORT_MIN_INTERVAL: Duration = Duration::from_millis(100);

fn subagent_mailbox_best_effort_send_permitted(
    last_sent_at: &mut HashMap<String, Instant>,
    message: &MailboxMessage,
    now: Instant,
) -> bool {
    if !subagent_mailbox_message_is_best_effort(message) {
        return true;
    }

    let agent_id = message.agent_id().to_string();
    if last_sent_at
        .get(&agent_id)
        .is_some_and(|last| now.duration_since(*last) < SUBAGENT_MAILBOX_BEST_EFFORT_MIN_INTERVAL)
    {
        return false;
    }

    last_sent_at.insert(agent_id, now);
    true
}

/// Forward one turn-scoped mailbox envelope. Returns `false` when the engine
/// event channel is closed and the drainer should stop.
async fn forward_subagent_mailbox_message(
    tx: &mpsc::Sender<Event>,
    owner_session_id: &str,
    turn_id: &str,
    seq: u64,
    message: MailboxMessage,
    best_effort_sent_at: &mut HashMap<String, Instant>,
) -> bool {
    let event = Event::SubAgentMailbox {
        owner_session_id: owner_session_id.to_string(),
        turn_id: turn_id.to_string(),
        seq,
        message,
    };
    if let Event::SubAgentMailbox { message, .. } = &event
        && subagent_mailbox_message_is_best_effort(message)
    {
        if !subagent_mailbox_best_effort_send_permitted(
            best_effort_sent_at,
            message,
            Instant::now(),
        ) {
            return true;
        }
        return match tx.try_send(event) {
            Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => true,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
        };
    }
    tx.send(event).await.is_ok()
}

/// Which config-source refresh precedes a connect pass.
enum McpConnectRefresh {
    /// Session boot: re-read only when the sources moved (mtime/content).
    IfChanged,
    /// Explicit reload: force a re-read and drop every live connection
    /// first, so even a byte-identical config re-dials under the current
    /// credentials. A malformed source fails the pass before anything is
    /// dropped.
    Force,
}

impl Engine {
    /// Surface the snapshots-disabled notice a blocking snapshot task parked
    /// (#5930). Called at turn boundaries; each session gets its own notice.
    pub(super) async fn emit_pending_snapshot_notices(&self) {
        use crate::core::turn::SnapshotsDisabledNoticeUi as _;
        for notice in crate::core::turn::take_snapshots_disabled_notices(
            &self.session.workspace,
            Some(&self.session.id),
        ) {
            // One rendered line, localized once here: the TUI toasts it as-is
            // and `/status` re-renders it from the retained observation.
            let reason = notice.localize(codewhale_localization::resolve_locale(
                &self.config.locale_tag,
            ));
            let _ = self
                .send_event(Event::SnapshotsDisabled {
                    workspace: notice.workspace,
                    reason,
                })
                .await;
        }
    }

    /// Replay the execution-boundary facts a conformance golden was recorded
    /// under. They reach the model only through `<turn_meta>`'s sandbox
    /// posture line; production engines always probe this host at
    /// construction, and never call this. Its only caller, the scripted
    /// conformance families, is Unix-only.
    #[cfg(all(test, unix))]
    pub(crate) fn pin_recorded_platform_posture(
        &mut self,
        enforcement: crate::sandbox::policy::SandboxEnforcement,
        no_new_privs_active: Option<bool>,
    ) {
        self.sandbox_enforcement = enforcement;
        self.no_new_privs_active = no_new_privs_active;
    }

    fn begin_turn_control(&mut self) -> handle::TurnControlGuard {
        self.begin_turn_control_for_provenance(UserInputProvenance::ExternalUser)
    }

    fn begin_turn_control_for_provenance(
        &mut self,
        provenance: UserInputProvenance,
    ) -> handle::TurnControlGuard {
        let mut controls = self
            .turn_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let control = self.admitted_turn_control.take().unwrap_or_else(|| {
            let mut control = controls.fresh();
            if !provenance.can_authorize_work() {
                // Idle handoffs are continuations of the existing user
                // request. Retain cancellation while holding the same
                // activation lock used by cancel_with_reason, so a cancel
                // during an earlier status send cannot be reset here.
                // Reuse the scope itself so cancelling the handoff also
                // stops siblings launched before the ordinary parent reply.
                control.cancel = self.cancel_token.clone();
                control.reason = Arc::clone(&self.cancel_reason);
            }
            control
        });
        self.cancel_token = control.cancel.clone();
        self.cancel_reason = Arc::clone(&control.reason);
        *self
            .shared_cancel_token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = control.cancel.clone();
        *self
            .shared_paused
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
        controls.active = Some(control.clone());
        handle::TurnControlGuard {
            controls: Arc::clone(&self.turn_controls),
            id: control.id,
            narrowing: control.narrowing,
        }
    }

    /// Take the next steer belonging to the active turn.
    ///
    /// Steers addressed to a turn that has already moved on are discarded
    /// here; dropping their [`handle::SteerInput`] reports
    /// [`handle::SteerOutcome::Dropped`] to the sender, so a discard is never
    /// silent (#6276). The returned [`handle::PendingSteer`] is unsettled:
    /// the caller must `commit()` it once the text is in the turn's record,
    /// and dropping it otherwise reports `Dropped` too.
    fn next_turn_steer(&mut self) -> Option<handle::PendingSteer> {
        let active_id = self
            .turn_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .as_ref()
            .map(|control| control.id);
        // Future-control inputs must retain their exact target, and remain
        // bounded even while another turn drains this channel repeatedly.
        // At most one channel-capacity of local lookahead is retained.
        while self.queued_steers.len() < self.rx_steer.max_capacity() {
            let Ok(steer) = self.rx_steer.try_recv() else {
                break;
            };
            if steer.replace_pending {
                // Drop only not-yet-committed inputs addressed to this exact
                // control. Their original senders receive Dropped via RAII.
                self.queued_steers
                    .retain(|older| older.turn_id != steer.turn_id);
            }
            self.queued_steers.push_back(steer);
        }
        self.queued_steers
            .retain(|steer| match (steer.turn_id, active_id) {
                (Some(target), Some(active)) => target >= active,
                (None, Some(_)) => false,
                _ => true,
            });
        self.queued_steers
            .iter()
            .position(|steer| steer.turn_id == active_id)
            .and_then(|index| self.queued_steers.remove(index))
            .map(handle::SteerInput::into_pending)
    }

    fn env_only_api_key_recovery_hint(api_config: &Config) -> Option<String> {
        if !crate::config::active_provider_uses_env_only_api_key(api_config) {
            return None;
        }

        let identity = api_config.active_provider_identity().ok()?;
        let provider = identity.provider;
        let env_var = provider.provider().env_vars().join(" / ");

        Some(format!(
            "The rejected key came from {env_var}; no saved config key is present.\n\
             Run `codewhale auth status` to inspect credential sources, then \
             `codewhale auth set --provider {provider}` to save a valid key in ~/.codewhale/config.toml, \
             or remove the stale export and open a fresh shell.",
            provider = identity.key
        ))
    }

    /// Where the user message just added sits in the session, for
    /// [`Self::retract_unanswered_user_message`].
    pub(super) fn mark_unanswered_user_message(&self) -> crate::core::turn::UnansweredUserMessage {
        crate::core::turn::UnansweredUserMessage {
            len: self.session.messages.len(),
            revision: self.session.messages_revision,
        }
    }

    /// Remove this turn's user message when nothing followed it — the request
    /// was refused before any model output (#6566). The mark is the session
    /// length and messages revision right after the message was added: an
    /// append since (an answer, a tool call, a runtime note) changes the
    /// length, and a rewrite (compaction, context recovery) changes the
    /// revision even when the length happens to match, so either leaves the
    /// session as it is rather than delete some other message.
    pub(super) fn retract_unanswered_user_message(
        &mut self,
        mark: crate::core::turn::UnansweredUserMessage,
    ) -> bool {
        if !self.can_retract_unanswered_user_message(mark) {
            return false;
        }
        self.session.messages.truncate_to(mark.len - 1);
        self.session.bump_messages_revision();
        true
    }

    pub(super) fn can_retract_unanswered_user_message(
        &self,
        mark: crate::core::turn::UnansweredUserMessage,
    ) -> bool {
        mark.len > 0
            && self.session.messages.len() == mark.len
            && self.session.messages_revision == mark.revision
            && self
                .session
                .messages
                .last()
                .is_some_and(|message| message.role == Role::User)
    }

    pub(super) fn decorate_auth_error_message(&self, message: String) -> String {
        let Some(hint) = self.api_key_env_only_recovery.as_ref() else {
            return message;
        };
        if crate::error_taxonomy::classify_error_message(&message) != ErrorCategory::Authentication
            || message.contains("no saved config key is present")
        {
            return message;
        }
        format!("{message}\n\n{hint}")
    }

    /// A provider's input bill describes one route's tokenization of one
    /// prompt. The auto-compaction gate and the preflight guard lift the
    /// honest estimate to the last bill, so a bill carried across a route
    /// switch would measure the next request with the previous route's
    /// tokenizer and prefix — a 256k route's 150k bill would send a 128k
    /// route straight into emergency compaction before anything was sent.
    /// Drop the bill when the route identity, endpoint, model, or limits
    /// change and let the first request on the new route re-bill. A
    /// re-install of the same route (every turn installs its host-resolved
    /// route) keeps the carry-over the compaction gate relies on (#5577).
    /// The endpoint — base URL, endpoint key, and wire protocol — is part of
    /// the identity: a named custom provider keeps its name, model string,
    /// and (usually absent) limits across a config reload that points it at
    /// a different server, and a catalog refresh can keep even the URL while
    /// moving a model from Chat Completions to Responses. A different server
    /// is a different tokenizer, and a different wire format serializes the
    /// same prompt differently.
    ///
    /// Known limitation: a same-route change of the system prefix is not a
    /// route change here; its size shows up as growth once the next request
    /// bills.
    /// The facts that make an endpoint the same endpoint for the bill
    /// carry-over: where the request goes and how it is serialized.
    fn endpoint_identity(
        endpoint: &codewhale_config::route::ResolvedEndpoint,
    ) -> (&str, &str, codewhale_config::route::RequestProtocol) {
        (
            endpoint.base_url.as_str(),
            endpoint.endpoint_key.as_str(),
            endpoint.protocol,
        )
    }

    fn forget_input_bill_if_route_changes(
        &mut self,
        identity: &str,
        provider_id: Option<&str>,
        endpoint: Option<&codewhale_config::route::ResolvedEndpoint>,
        model: &str,
        limits: Option<codewhale_config::route::RouteLimits>,
    ) {
        let same_route = self.api_provider_identity.as_ref().is_some_and(|current| {
            current.key.as_str() == identity && current.persisted_id() == provider_id
        }) && self
            .active_route_endpoint
            .as_ref()
            .map(Self::endpoint_identity)
            == endpoint.map(Self::endpoint_identity)
            && self.session.model == model
            && self.active_route_limits == limits;
        if !same_route {
            self.session.latest_parent_input_tokens = None;
        }
    }

    /// Install a route that the host already resolved and client-preflighted.
    /// No identity guessing or config re-resolution is allowed at this
    /// boundary: the descriptor is the single authority for the turn.
    fn install_validated_runtime_route(&mut self, route: ValidatedRuntimeRoute) {
        let provider = route.identity.provider;
        let identity = route.identity;
        let model = route.model;
        let limits = crate::route_budget::known_route_limits(route.candidate.limits());
        let capabilities = route.candidate.capabilities();
        let api_config = *route.config;
        let client = route.client;

        let endpoint = route.candidate.endpoint().clone();
        self.forget_input_bill_if_route_changes(
            identity.key.as_str(),
            identity.persisted_id(),
            Some(&endpoint),
            &model,
            limits,
        );
        self.active_route_endpoint = Some(endpoint);
        self.api_provider = provider;
        self.api_provider_identity = Some(identity);
        self.api_config = api_config;
        self.active_route_limits = limits;
        self.active_route_capabilities = capabilities;
        self.api_key_env_only_recovery = Self::env_only_api_key_recovery_hint(&self.api_config);
        self.codewhale_client = Some(client.clone());
        if !self.model_client_injected {
            self.model_client = Some(Arc::new(client.clone()));
        }
        self.codewhale_client_error = None;
        self.session.model = model;
        self.config.model.clone_from(&self.session.model);
    }

    /// Activate a structurally resolved route at the engine boundary. Normal
    /// engines construct the concrete client before any turn state changes.
    /// Embedders/tests that explicitly injected a provider-neutral client keep
    /// that client as the I/O authority while still installing the exact route
    /// identity, model, config, and budget receipt.
    fn install_resolved_runtime_route(
        &mut self,
        mut route: ResolvedRuntimeRoute,
    ) -> Result<(), String> {
        if !self.model_client_injected {
            self.install_validated_runtime_route(route.validate()?);
            return Ok(());
        }

        let preflighted_client = route.take_preflighted_client();
        let provider = route.identity.provider;
        let identity = route.identity;
        let model = route.model;
        let limits = crate::route_budget::known_route_limits(route.candidate.limits());
        let capabilities = route.candidate.capabilities();
        let api_config = *route.config;
        let concrete_client = preflighted_client
            .map(Ok)
            .unwrap_or_else(|| CodewhaleClient::from_candidate(&api_config, &route.candidate));

        let endpoint = route.candidate.endpoint().clone();
        self.forget_input_bill_if_route_changes(
            identity.key.as_str(),
            identity.persisted_id(),
            Some(&endpoint),
            &model,
            limits,
        );
        self.active_route_endpoint = Some(endpoint);
        self.api_provider = provider;
        self.api_provider_identity = Some(identity);
        self.api_config = api_config;
        self.active_route_limits = limits;
        self.active_route_capabilities = capabilities;
        self.api_key_env_only_recovery = Self::env_only_api_key_recovery_hint(&self.api_config);
        match concrete_client {
            Ok(client) => {
                self.codewhale_client = Some(client.clone());
                self.codewhale_client_error = None;
            }
            Err(err) => {
                self.codewhale_client = None;
                self.codewhale_client_error = Some(err.to_string());
            }
        }
        self.session.model = model;
        self.config.model.clone_from(&self.session.model);
        Ok(())
    }

    fn current_runtime_route(&self) -> Result<ResolvedRuntimeRoute, String> {
        let config = self
            .authoritative_route_config
            .as_ref()
            .map(|config| config.read().clone())
            .unwrap_or_else(|| self.api_config.clone());
        let identity = self
            .api_provider_identity
            .as_ref()
            .ok_or_else(|| "current route has no admitted provider identity".to_string())?;
        config.verify_provider_identity(identity)?;
        resolve_runtime_route_for_identity(&config, identity, Some(&self.session.model))
    }

    /// Create a new engine with the given configuration
    pub fn new(config: EngineConfig, api_config: &Config) -> (Self, EngineHandle) {
        Self::new_admitted(config, api_config, None)
    }

    fn new_admitted(
        config: EngineConfig,
        api_config: &Config,
        host: Option<EngineHostSetup>,
    ) -> (Self, EngineHandle) {
        Self::new_admitted_with_subagent_manager(config, api_config, host, None)
    }

    fn new_admitted_with_subagent_manager(
        mut config: EngineConfig,
        api_config: &Config,
        mut host: Option<EngineHostSetup>,
        supplied_subagent_manager: Option<SharedSubAgentManager>,
    ) -> (Self, EngineHandle) {
        crate::tls::ensure_rustls_crypto_provider();

        // Compaction re-states the user's `/anchor` file after its summary;
        // hand it the workspace root once so every prepared pass can read it.
        if config.compaction.workspace.is_none() && !api_config.runtime_chat_isolated {
            config.compaction.workspace = Some(config.workspace.clone());
        }

        // Unlike a Skill body, this instruction is visible on the first model
        // request. Registry discovery is a fallback for missing capabilities;
        // result matching stays with the model and the index stays host-side.
        //
        // It describes when discovery is worth a turn; it is not a gate ahead
        // of ordinary work. The earlier "must call `registry_sync` before a
        // manual implementation" phrasing named two deferred tools as
        // mandatory, so a plain "write an HTML page and read a fixture" turn
        // spent its steps on `tool_search` for `registry_sync` and on starting
        // a browser server instead of writing the file.
        if config.features.enabled(Feature::Mcp) && !api_config.runtime_chat_isolated {
            config
                .instructions
                .push(crate::prompts::InstructionSource::Inline {
                    name: MCP_REGISTRY_FIRST_INSTRUCTION_SOURCE.to_string(),
                    content: MCP_REGISTRY_FIRST_INSTRUCTION.to_string(),
                });
        }

        if let Some(objective) = normalized_goal_objective(config.goal_objective.as_deref()) {
            sync_goal_state_from_host(
                &config.goal_state,
                Some(&objective),
                config.goal_token_budget,
                config.goal_status,
            );
        }

        let (tx_op, rx_op) = mpsc::channel(ENGINE_OP_CHANNEL_CAPACITY);
        let (tx_event, rx_event) = mpsc::channel(4 * streaming::MAX_TOOL_CALLS_PER_RESPONSE);
        let (tx_approval, rx_approval) = mpsc::channel(64);
        let (tx_user_input, rx_user_input) = mpsc::channel(32);
        let (tx_steer, rx_steer) = mpsc::channel(64);
        let turn_controls = Arc::new(StdMutex::new(handle::TurnControls::default()));
        let (tx_subagent_completion, rx_subagent_completion) =
            mpsc::channel(SUBAGENT_COMPLETION_CHANNEL_CAPACITY);
        let cancel_token = host.as_ref().map_or_else(CancellationToken::new, |host| {
            host.cancel_token().child_token()
        });
        let shared_cancel_token = Arc::new(StdMutex::new(cancel_token.clone()));
        let cancel_reason: Arc<StdMutex<Option<CancelReason>>> = Arc::new(StdMutex::new(None));
        let shared_paused = Arc::new(StdMutex::new(false));
        let initial_authority = host.as_ref().map_or_else(
            || {
                LiveRuntimeAuthority::from_fields(
                    AppMode::Agent,
                    config.allow_shell,
                    config.trust_mode,
                    false,
                    ApprovalMode::Suggest,
                    api_config.sandbox_mode.clone(),
                )
            },
            |host| host.initial_authority(api_config),
        );
        let live_runtime_authority = Arc::new(StdMutex::new(LiveRuntimeAuthorityState::new(
            initial_authority,
        )));
        let compaction_cancellation =
            Arc::new(StdMutex::new(CompactionCancellationState::default()));
        let tool_exec_lock = Arc::new(RwLock::new(()));
        let own_plugin_registry = config
            .plugin_registry
            .as_ref()
            .filter(|registry| registry.workspace() == config.workspace)
            .cloned();
        // Experimental extension host: start in the background, never on the
        // first-prompt path. Its tools join at the next turn's rebuild. Only
        // an engine with its own plugin snapshot attaches; the empty fallback
        // below would desire nothing and must not affect other engines.
        let extension_host = match host.as_mut() {
            Some(EngineHostSetup::Child(child)) => child.attachment.take(),
            Some(EngineHostSetup::Rlm(_)) => None,
            None => own_plugin_registry
                .as_ref()
                .filter(|_| config.features.enabled(Feature::ExtensionHost))
                .map(|registry| {
                    let manager = crate::extension_host::manager();
                    let attachment = manager.attach(Arc::clone(registry));
                    attachment.sync_in_background();
                    attachment
                }),
        };
        let plugin_registry = extension_host
            .as_ref()
            .map(|attachment| attachment.plugin_view())
            .or(own_plugin_registry)
            .unwrap_or_else(|| Arc::new(crate::plugins::PluginRegistry::empty(&config.workspace)));

        // Create clients for both providers
        let (codewhale_client, codewhale_client_error) = match host
            .as_ref()
            .map(|host| Ok(host.client().clone()))
            .unwrap_or_else(|| CodewhaleClient::new(api_config))
        {
            Ok(client) => (Some(client), None),
            Err(err) => (None, Some(err.to_string())),
        };
        let model_client = codewhale_client
            .as_ref()
            .map(|client| Arc::new(client.clone()) as SharedModelClient);
        let api_provider_identity = api_config.active_provider_identity().ok();
        let api_provider = api_provider_identity
            .as_ref()
            .map_or(ProviderKind::Custom, |identity| identity.provider);
        let api_key_env_only_recovery = Self::env_only_api_key_recovery_hint(api_config);

        let mut session = Session::new(
            config.model.clone(),
            config.workspace.clone(),
            config.allow_shell,
            config.trust_mode,
            config.notes_path.clone(),
            config.mcp_config_path.clone(),
        );
        if let Some(session_id) = config
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            session.id = session_id.to_string();
        }
        if let Some(attachment) = extension_host.as_ref() {
            attachment.set_identity(
                Some(session.id.clone()),
                host.as_ref().and_then(EngineHostSetup::owner_agent_id),
            );
        }
        config.hook_executor = config.hook_executor.as_ref().map(|hooks| {
            Arc::new(hooks.bind_caller(crate::hooks::HookCaller {
                workspace: config.workspace.clone(),
                plugins: Some(Arc::clone(&plugin_registry)),
                session_id: Some(session.id.clone()),
                agent_id: host.as_ref().and_then(EngineHostSetup::owner_agent_id),
                origin_turn_id: None,
                origin_call_id: None,
            }))
        });
        // Set up stable system prompt with project context (default to agent mode).
        // Per-turn working-set metadata is injected into the latest user
        // message at request time so file churn does not rewrite this prefix.
        // Session start boundary: reconcile this session's interrupted memory
        // contexts (prepared but never dispatch-acknowledged — e.g. the process
        // died mid-turn), then prepare this session's prompt packet through the
        // durable receipt path so the Context Lens can show what was assembled
        // for it. Both are inert when memory is disabled — no store I/O.
        if host.is_none()
            && config.memory_enabled
            && let Some(store) =
                crate::native_memory::NativeMemoryStore::from_global_path(&config.memory_path)
        {
            match store.session_start(&config.workspace, &session.id) {
                Ok(0) => {}
                Ok(interrupted) => tracing::info!(
                    interrupted,
                    "memory contexts from this session never completed dispatch"
                ),
                Err(error) => {
                    tracing::warn!(%error, "memory session-start reconcile failed")
                }
            }
        }
        let user_memory_block = crate::native_memory::native_prompt_block_traced(
            host.is_none() && config.memory_enabled,
            &config.memory_path,
            &config.workspace,
            &session.id,
        );
        let prompt_goal_objective =
            goal_objective_for_prompt(config.goal_objective.as_deref(), &config.goal_state);
        // #5715: name a prior workspace session that ended mid-turn so the
        // model can offer recovery without being asked. Frozen-prefix
        // contributor — computed once here, identical for every turn.
        let recovery_hint = host
            .is_none()
            .then(|| {
                crate::session_manager::session_recovery_hint(
                    &config.workspace,
                    Some(session.id.as_str()),
                )
            })
            .flatten();
        let prompt_host = if config.terminal_chrome_enabled {
            prompts::PromptHost::Interactive
        } else {
            prompts::PromptHost::Headless
        };
        let system_prompt = if let Some(host) = host.as_ref() {
            host.system_prompt().clone()
        } else if api_config.runtime_chat_isolated {
            SystemPrompt::Text(ISOLATED_CHAT_SYSTEM_PROMPT.to_string())
        } else {
            prompts::system_prompt_for_mode_with_context_skills_session_and_approval_for_host(
                &config.workspace,
                None,
                Some(&config.skills_dir),
                Some(&config.instructions),
                prompts::PromptSessionContext {
                    user_memory_block: user_memory_block.as_deref(),
                    goal_objective: prompt_goal_objective.as_deref(),
                    project_context_pack_enabled: config.project_context_pack_enabled,
                    locale_tag: &config.locale_tag,
                    translation_enabled: config.translation_enabled,
                    model_id: &config.model,
                    context_window_override: Some(
                        crate::route_budget::route_context_window_tokens(
                            api_provider,
                            &config.model,
                            config.active_route_limits,
                        ),
                    ),
                    verbosity: config.verbosity.as_deref(),
                    recovery_hint: recovery_hint.as_deref(),
                    skills_discovery_mode: config.skills_discovery_mode,
                    plugin_registry: Some(plugin_registry.as_ref()),
                    // Matches `current_mode`'s initial value below; a later
                    // `/mode` switch re-runs `refresh_system_prompt`.
                    mode: AppMode::Agent,
                },
                prompt_host,
            )
        };
        let stable_prompt = Some(system_prompt);
        session.last_system_prompt_hash = Some(system_prompt_hash(stable_prompt.as_ref()));
        session.system_prompt = stable_prompt;

        // Initialize prefix-cache stability monitor (lazy-pin).
        // The system prompt is available now but the tool catalog isn't
        // fully built until the first turn, so we start unpinned. The
        // first `check_and_update` call in the turn loop will pin the
        // fingerprint automatically.
        let _ = session.prefix_stability.get_or_insert_with(|| {
            // Use the tool registry's spec names for fingerprinting.
            // At this point tool spec builders may not be registered yet,
            // so we start with None — fingerprint will pin on first request.
            codewhale_core::prefix_cache::PrefixStabilityManager::new_unpinned()
        });

        let subagent_execution_limits = supplied_subagent_manager
            .as_ref()
            .map(|_| {
                SubAgentExecutionLimits::new(
                    config.max_subagents,
                    config.max_admitted_subagents,
                    config.subagent_heartbeat_timeout,
                    config.launch_concurrency,
                    api_config.subagent_default_max_steps(),
                    api_config
                        .subagent_default_wall_time_secs()
                        .map(Duration::from_secs),
                )
            })
            .or_else(|| match host.as_ref() {
                Some(EngineHostSetup::Child(child)) => {
                    child.authority.runtime.execution_limits.clone()
                }
                _ => None,
            });
        let subagent_manager = supplied_subagent_manager.unwrap_or_else(|| {
            host.as_ref().map_or_else(
                || {
                    let subagent_state_root = config
                        .subagent_state_root
                        .clone()
                        .unwrap_or_else(|| config.workspace.clone());
                    new_shared_subagent_manager_with_state_root_and_timeout(
                        config.workspace.clone(),
                        subagent_state_root,
                        config.max_subagents,
                        config.max_admitted_subagents,
                        config.subagent_heartbeat_timeout,
                        config.launch_concurrency,
                        // #5324: defaults are captured operator config, not call fields.
                        api_config.subagent_default_max_steps(),
                        api_config
                            .subagent_default_wall_time_secs()
                            .map(std::time::Duration::from_secs),
                    )
                },
                |host| Arc::clone(host.subagent_manager()),
            )
        });
        // The OS wrappers below only cover child processes. Codewhale's own
        // `read_file`/`read`/`read_media` tools read in-process, so the same
        // deny-list is installed process-wide for them to consult (S1).
        if host.is_none() {
            crate::sandbox::read_guard::set_active(config.read_denylist.clone());
        }
        let shell_manager = host
            .as_ref()
            .map(|host| host.context().shell_manager.clone())
            .or_else(|| config.runtime_services.shell_manager.clone())
            .unwrap_or_else(|| new_shared_shell_manager(config.workspace.clone()));
        if host.is_none() {
            match shell_manager.lock() {
                Ok(mut manager) => {
                    manager.set_prefer_bwrap(config.prefer_bwrap);
                    manager.set_bwrap_extensions(config.bwrap_extensions.clone());
                    manager.set_denied_read_subpaths(config.read_denylist.subtree_paths());
                }
                Err(poisoned) => {
                    let mut manager = poisoned.into_inner();
                    manager.set_prefer_bwrap(config.prefer_bwrap);
                    manager.set_bwrap_extensions(config.bwrap_extensions.clone());
                    manager.set_denied_read_subpaths(config.read_denylist.subtree_paths());
                }
            }
        }
        let file_read_tracker = host
            .as_ref()
            .map_or_else(new_shared_file_read_tracker, |host| {
                host.context().file_read_tracker.clone()
            });
        let lsp_manager = Arc::new(match config.lsp_config.clone() {
            Some(cfg) => crate::lsp::LspManager::new(cfg, config.workspace.clone()),
            None => crate::lsp::LspManager::disabled(),
        });

        // External sandbox backend (#516). Logged but non-fatal: if the
        // backend fails to construct, the engine continues with local
        // execution as the fallback.
        let sandbox_backend = host
            .as_ref()
            .map(|host| host.context().sandbox_backend.clone())
            .unwrap_or_else(|| {
                crate::sandbox::backend::create_backend(api_config)
                    .unwrap_or_else(|e| {
                        tracing::warn!("Failed to create sandbox backend: {e}");
                        None
                    })
                    .map(std::sync::Arc::from)
            });
        let sandbox_enforcement = if sandbox_backend.is_some() {
            crate::sandbox::policy::SandboxEnforcement::ExternalBackend
        } else if crate::sandbox::get_platform_sandbox_with_bwrap_preference(config.prefer_bwrap)
            .is_some()
        {
            crate::sandbox::policy::SandboxEnforcement::LocalOs
        } else {
            crate::sandbox::policy::SandboxEnforcement::Unavailable
        };

        let active_route_limits = config.active_route_limits;
        let shared_auto_review_policy = host.as_ref().map_or_else(
            || Arc::new(config.auto_review_policy.clone()),
            EngineHostSetup::review_policy,
        );
        let approval_receipt_store = host.as_ref().map_or_else(
            || {
                #[cfg(not(test))]
                {
                    ApprovalReceiptStore::default_location().map_err(|err| err.to_string())
                }
                #[cfg(test)]
                {
                    Ok(ApprovalReceiptStore::new(std::env::temp_dir().join(
                        format!("codewhale-approval-tests-{}", uuid::Uuid::new_v4()),
                    )))
                }
            },
            EngineHostSetup::approval_store,
        );
        // R1: seed the wall clock from the config the engine is built with.
        // `run_turn` restarts it per turn; this initial value only matters
        // for hosts that inspect the engine before the first turn.
        let turn_wall_clock_budget = config.turn_wall_clock;
        let host_profile = match host.as_ref() {
            Some(EngineHostSetup::Child(_)) => EngineHostProfile::Child,
            Some(EngineHostSetup::Rlm(_)) => EngineHostProfile::Rlm,
            None => EngineHostProfile::Normal,
        };
        let mcp_pool = host.as_ref().and_then(|host| match host {
            EngineHostSetup::Child(child) => child.authority.runtime.mcp_pool.clone(),
            EngineHostSetup::Rlm(_) => None,
        });
        let (child_host, rlm_host) = match host {
            Some(EngineHostSetup::Child(child)) => {
                (Some(child_host::ChildHostState::from(child)), None)
            }
            Some(EngineHostSetup::Rlm(rlm)) => (None, Some(rlm_host::RlmHostState::from(rlm))),
            None => (None, None),
        };
        let engine = Engine {
            rlm_host,
            config,
            api_config: api_config.clone(),
            authoritative_route_config: None,
            host_profile,
            turn_narrowing: TurnNarrowing::Inherit,
            turn_acp_shell_ceiling: None,
            codewhale_client,
            model_client,
            model_client_injected: false,
            codewhale_client_error,
            api_key_env_only_recovery,
            session,
            repl_kernel: None,
            subagent_manager,
            subagent_execution_limits,
            shared_auto_review_policy,
            shell_manager,
            file_read_tracker,
            mcp_pool,
            turn_tool_surface_budget: None,
            mcp_connection_errors: HashMap::new(),
            mcp_boot_in_flight: false,
            mcp_boot_rx: None,
            mcp_supervisor_rx: None,
            mcp_supervisor_task: None,
            mcp_boot_done: None,
            mcp_boot_generation: None,
            mcp_boot_task: None,
            mcp_event_generation: 0,
            plugin_registry,
            extension_host,
            extension_prompt_block: None,
            constitution_block: None,
            profile_constitution_fallback_noticed: false,
            repeated_upstream_failure: None,
            live_header: crate::prompt_zones::new_live_header_cell(),
            api_provider,
            api_provider_identity,
            active_route_limits,
            active_route_capabilities: codewhale_config::route::RouteCapabilities::default(),
            active_route_endpoint: None,
            rx_op,
            live_runtime_authority: Arc::clone(&live_runtime_authority),
            compaction_cancellation: Arc::clone(&compaction_cancellation),
            tx_op: tx_op.clone(),
            scheduled_goal_continuation: None,
            goal_continuation_schedule_seq: 0,
            rx_approval,
            approval_receipt_store,
            rx_user_input,
            rx_steer,
            queued_steers: std::collections::VecDeque::new(),
            turn_controls: Arc::clone(&turn_controls),
            admitted_turn_control: None,
            tx_event,
            tx_subagent_completion,
            rx_subagent_completion,
            delivered_subagent_completion_ids: HashSet::new(),
            cancel_token: cancel_token.clone(),
            shared_cancel_token: shared_cancel_token.clone(),
            cancel_reason: cancel_reason.clone(),
            tool_exec_lock,
            turn_counter: 0,
            restore_point_since: None,
            lsp_manager,
            pending_lsp_blocks: Vec::new(),
            sandbox_backend,
            sandbox_enforcement,
            no_new_privs_active: crate::sandbox::process_hardening::no_new_privs_active(),
            current_mode: AppMode::Agent,
            turn_wall_clock: turn_budget::TurnWallClock::start(turn_wall_clock_budget),
            last_policy_narrowing: None,
            last_turn_meta_git_snapshot: StdMutex::new(None),
            token_estimate_cache: TokenEstimateCache::new(),
            shared_paused: shared_paused.clone(),
            advisor_emission_guard: None,
            turn_heartbeat: turn_heartbeat::TurnHeartbeat::new(),
            child_host,
        };
        let handle = EngineHandle {
            goal_state: engine.config.goal_state.clone(),
            tx_op,
            rx_event: Arc::new(RwLock::new(rx_event)),
            cancel_token: shared_cancel_token,
            cancel_reason,
            tx_approval,
            tx_user_input,
            tx_steer,
            turn_controls,
            shared_paused,
            client_preflight_required: true,
            live_runtime_authority,
            compaction_cancellation,
            turn_heartbeat: Arc::clone(&engine.turn_heartbeat),
            subagent_manager: Arc::clone(&engine.subagent_manager),
            terminal_worker_events: engine
                .subagent_execution_limits
                .as_ref()
                .map(|limits| limits.terminal_events.clone()),
        };

        (engine, handle)
    }

    /// Construct the real Engine with an injected provider-neutral model
    /// client. The event loop, prompt assembly, tool registry/execution,
    /// cancellation, and session projection are unchanged; only the model I/O
    /// boundary is replaced.
    #[cfg(test)]
    pub fn new_with_model_client(
        config: EngineConfig,
        api_config: &Config,
        client: SharedModelClient,
    ) -> (Self, EngineHandle) {
        let (mut engine, mut handle) = Self::new(config, api_config);
        engine.model_client = Some(client);
        engine.model_client_injected = true;
        engine.codewhale_client_error = None;
        handle.client_preflight_required = false;
        (engine, handle)
    }

    async fn handle_run_shell_command(
        &mut self,
        command: String,
        mode: AppMode,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
    ) {
        let turn_control = self.begin_turn_control();
        let Ok(terminal_permit) = streaming::reserve_event_capacity(
            &self.tx_event,
            Some(&self.cancel_token),
            streaming::EventReservationPolicy::Strict,
        )
        .await
        else {
            return;
        };
        let Ok(start_permit) = streaming::reserve_event_capacity(
            &self.tx_event,
            Some(&self.cancel_token),
            streaming::EventReservationPolicy::Strict,
        )
        .await
        else {
            return;
        };
        self.turn_counter = self.turn_counter.saturating_add(1);

        let turn_id = format!(
            "{}{seq}",
            USER_SHELL_TOOL_ID_PREFIX,
            seq = self.turn_counter
        );
        let tool_id = turn_id.clone();
        let tool_name = "Bash".to_string();
        let tool_input = json!({ "action": "run", "command": command, "source": "user" });
        let snapshot_prompt = tool_input["command"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        let authority = TurnAuthority::from_effective_fields(
            mode,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
        );
        let prior = self.applied_runtime_authority();
        self.apply_runtime_mode_policy(&authority);
        self.discard_kernels_if_narrowed(&prior).await;

        start_permit.send(Event::TurnStarted {
            turn_id: turn_id.clone(),
            created_at: chrono::Utc::now(),
            route: None,
            // Composer shell commands have no host submission envelope.
            submission_id: None,
        });

        // The command runs from this snapshot to the post-turn one, so the
        // receipt names it as the call that span belongs to.
        self.take_restore_point(
            WorkspaceSnapshotKind::PreTurn,
            format_snapshot_label("pre-turn", self.turn_counter, Some(&snapshot_prompt)),
            Some(tool_id.as_str()),
            None,
        )
        .await;

        self.emit_pending_snapshot_notices().await;

        let _ = self
            .send_event(Event::ToolCallStarted {
                model_call: None,
                id: tool_id.clone(),
                name: tool_name.clone(),
                input: tool_input.clone(),
            })
            .await;

        let tool_context = self.build_tool_context(mode, auto_approve);
        let registry = ToolRegistryBuilder::new()
            .with_shell_tools()
            .build(tool_context);

        let result = if mode == AppMode::Plan {
            Err(ToolError::permission_denied(
                "Tool 'bash' is unavailable in Plan mode".to_string(),
            ))
        } else if !self.config.features.enabled(Feature::ShellTool) {
            Err(ToolError::not_available(
                "Tool 'bash' is disabled by feature flag".to_string(),
            ))
        } else if let Some(spec) = registry.get(&tool_name) {
            // #5191: the human typed this command — typing it IS the approval.
            // The tool-approval modal gates model-provenance calls; applying it
            // to a user-typed `!` command asks the user to re-approve what they
            // just typed. Typed exec ask-rules still apply as hard Block
            // denies, and the sandbox/execpolicy layer stays the real safety
            // boundary. Model-issued shell calls keep the standard approval
            // path; this branch is strictly composer provenance.
            let ask_rule_decision = exec_shell_ask_rule_decision(
                &self.config,
                &tool_name,
                &tool_input,
                &self.session.workspace,
                self.session.approval_mode,
            );
            if let Some(ToolAskRuleDecision::Block(reason)) = ask_rule_decision {
                Err(ToolError::permission_denied(reason))
            } else {
                emit_tool_audit(json!({
                    "event": "tool.user_provenance_preapproved",
                    "tool_id": tool_id.clone(),
                    "tool_name": tool_name.clone(),
                    "source": "composer_bang",
                }));
                Self::execute_tool_with_lock(
                    self.tool_exec_lock.clone(),
                    spec.supports_parallel(),
                    false,
                    self.tx_event.clone(),
                    Some(self.cancel_token.clone()),
                    tool_name.clone(),
                    Some(tool_id.clone()),
                    tool_input.clone(),
                    self.session.workspace.clone(),
                    Some(&registry),
                    None,
                    None,
                )
                .await
                .map(RichToolResult::into_result)
            }
        } else {
            Err(ToolError::not_available(
                "tool 'Bash' is not registered".to_string(),
            ))
        };

        let mut result = result;
        if let Ok(tool_result) = result.as_mut()
            && let Some(path) = crate::tools::truncate::apply_spillover_with_artifact(
                tool_result,
                &tool_id,
                &tool_name,
                &self.session.id,
            )
        {
            emit_tool_audit(json!({
                "event": "tool.spillover",
                "tool_id": tool_id.clone(),
                "tool_name": tool_name.clone(),
                "path": path.display().to_string(),
                "source": "composer_bang",
            }));
        }

        let status = user_shell_turn_outcome(&result, self.cancel_token.is_cancelled());
        let error = result.as_ref().err().map(ToString::to_string);

        let _ = self
            .send_event(Event::ToolCallComplete {
                model_call: None,
                id: tool_id,
                name: tool_name,
                result,
            })
            .await;

        if status == TurnOutcomeStatus::Interrupted {
            self.emit_interrupted_survivor_status().await;
        }
        self.post_turn_snapshot_before_complete(&snapshot_prompt)
            .await;
        let pending_post_turn = self.reserve_post_turn_snapshot();
        let status = terminal_turn_status_at_settlement(status, self.cancel_token.is_cancelled());
        terminal_permit.send(Event::TurnComplete {
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status,
            error,
            tool_catalog: None,
            base_url: None,
        });

        self.post_turn_snapshot_after_complete(
            "post-shell-turn-snapshot",
            snapshot_prompt,
            pending_post_turn,
        );
        drop(turn_control);
    }

    /// Take one workspace snapshot for the running turn and report it as an
    /// `Event::WorkspaceSnapshotTaken` receipt. Returns whether it was taken.
    ///
    /// A snapshot that fails (or is gated off) reports nothing: the host then
    /// has no such restore point for the turn and must say so rather than
    /// guess one. With [`EngineConfig::record_restore_points`] the receipt
    /// also carries the paths changed since the turn's previous snapshot,
    /// and a failure leaves the next span unknown instead of merging it into
    /// the one before.
    pub(crate) async fn take_restore_point(
        &mut self,
        kind: WorkspaceSnapshotKind,
        label: String,
        tool_call_id: Option<&str>,
        write_paths: Option<Vec<String>>,
    ) -> bool {
        if !self.config.snapshots_enabled {
            return false;
        }
        let record = self.config.record_restore_points;
        let since = if record && kind != WorkspaceSnapshotKind::PreTurn {
            self.restore_point_since.clone()
        } else {
            None
        };
        let workspace = self.session.workspace.clone();
        let cap = self.config.snapshots_max_workspace_bytes;
        let sid = self.session.id.clone();
        #[cfg(test)]
        let env_ticket = crate::test_support::env_scope_ticket();
        let taken = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let _membership = crate::test_support::join_env_scope(env_ticket);
            super::turn::restore_point_snapshot(&workspace, &label, cap, Some(&sid), since.as_ref())
        })
        .await
        .ok()
        .flatten();
        let Some((taken, changed)) = taken else {
            self.restore_point_since = None;
            return false;
        };
        if record {
            self.restore_point_since = Some(taken.tree.clone());
        }
        let mut snapshot = WorkspaceSnapshotRef::new(kind, &taken, &self.session.id, tool_call_id);
        snapshot.write_paths = write_paths;
        snapshot.changed_paths = changed.map(|paths| {
            paths
                .into_iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect()
        });
        let _ = self
            .send_event(Event::WorkspaceSnapshotTaken { snapshot })
            .await;
        true
    }

    /// With [`EngineConfig::record_restore_points`], take the post-turn
    /// snapshot now — before `TurnComplete` — and report it.
    async fn post_turn_snapshot_before_complete(&mut self, prompt: &str) {
        if !self.config.record_restore_points {
            return;
        }
        let label = format_snapshot_label("post-turn", self.turn_counter, Some(prompt));
        self.take_restore_point(WorkspaceSnapshotKind::PostTurn, label, None, None)
            .await;
    }

    /// Without [`EngineConfig::record_restore_points`], reserve the post-turn
    /// snapshot [`Self::post_turn_snapshot_after_complete`] takes. Called
    /// before `TurnComplete`, so a `/undo` the user types as soon as the
    /// turn ends waits for that snapshot instead of racing it (#6644).
    fn reserve_post_turn_snapshot(&self) -> Option<crate::snapshot::PendingPostTurnSnapshot> {
        (self.config.snapshots_enabled && !self.config.record_restore_points)
            .then(crate::snapshot::PendingPostTurnSnapshot::reserve)
    }

    /// Take the post-turn snapshot reserved by
    /// [`Self::reserve_post_turn_snapshot`] fire-and-forget: `TurnComplete`
    /// is already emitted, so the UI is unblocked and the user can type /
    /// select / paste immediately (#234). The git work proceeds on the
    /// blocking pool, and the reservation is released once it is done.
    fn post_turn_snapshot_after_complete(
        &self,
        task: &'static str,
        prompt: String,
        pending: Option<crate::snapshot::PendingPostTurnSnapshot>,
    ) {
        let Some(pending) = pending else {
            return;
        };
        let post_workspace = self.session.workspace.clone();
        let post_seq = self.turn_counter;
        let post_cap = self.config.snapshots_max_workspace_bytes;
        let post_sid = self.session.id.clone();
        crate::utils::spawn_blocking_supervised(task, move || {
            post_turn_snapshot(
                &post_workspace,
                post_seq,
                post_cap,
                Some(&prompt),
                Some(&post_sid),
            );
            drop(pending);
        });
    }

    /// Apply a user/host mode-or-posture change to the live session.
    ///
    /// Single authority source for mode/permission state: both the run loop
    /// and the active turn's typed live-authority drain land here.
    async fn apply_change_mode(
        &mut self,
        mode: AppMode,
        allow_shell: bool,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
        configured_sandbox_mode: Option<String>,
    ) -> bool {
        let authority = TurnAuthority::from_effective_fields(
            mode,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
        );
        let effective_approval = authority.approval_mode_for_session();
        let changed = self.current_mode != authority.mode
            || self.session.allow_shell != authority.allow_shell
            || self.session.trust_mode != authority.trust_mode
            || self.session.auto_approve
                != (authority.auto_approve || effective_approval == ApprovalMode::Bypass)
            || self.session.approval_mode != effective_approval
            || self.api_config.sandbox_mode != configured_sandbox_mode;
        let prior = self.applied_runtime_authority();
        self.api_config.sandbox_mode = configured_sandbox_mode;
        self.apply_runtime_mode_policy(&authority);
        self.discard_kernels_if_narrowed(&prior).await;
        if !changed {
            return false;
        }
        self.emit_session_updated().await;
        let _ = self
            .send_event(Event::status(format!(
                // Payload first, and short enough for the posture bar's right
                // slot. "Runtime policy changed to: X / Y" sheds at the colon —
                // the bar's notice shedder cuts at clause joints and keeps the
                // head — so the user read "Runtime policy changed to" with the
                // policy itself gone, which is the one word the notice exists
                // to carry. Product words only (§19): Permissions, then
                // Plan / Work / Operate — not "Policy" or the ACT tag.
                "Permissions: {} · {}",
                effective_approval.permission_chip_label(),
                match mode {
                    AppMode::Plan => "Plan",
                    AppMode::Agent => "Work",
                    AppMode::Operate => "Operate",
                },
            )))
            .await;
        true
    }

    fn take_pending_runtime_authority(&self) -> Option<LiveRuntimeAuthority> {
        if let Some(parent) = self
            .child_host
            .as_ref()
            .and_then(|child| child.authority.runtime.context.live_posture.as_ref())
        {
            let current = parent.read();
            return (current != self.applied_runtime_authority()).then_some(current);
        }
        let mut state = self
            .live_runtime_authority
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.applied_revision == state.revision {
            return None;
        }
        state.applied_revision = state.revision;
        Some(state.authority.clone())
    }

    fn runtime_authority_snapshot(&self) -> LiveRuntimeAuthority {
        if let Some(parent) = self
            .child_host
            .as_ref()
            .and_then(|child| child.authority.runtime.context.live_posture.as_ref())
        {
            return parent.read();
        }
        self.live_runtime_authority
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .authority
            .clone()
    }

    async fn apply_runtime_authority(&mut self, authority: LiveRuntimeAuthority) -> bool {
        self.apply_change_mode(
            authority.mode,
            authority.allow_shell,
            authority.trust_mode,
            authority.auto_approve,
            authority.approval_mode,
            authority.configured_sandbox_mode,
        )
        .await
    }

    /// Apply the newest published authority, if any. Returns whether the
    /// live posture actually changed: a republished identical posture (a
    /// PATCH that only renamed the thread, a repeated mode pick) is not a
    /// change and must not invalidate planned or approved calls.
    async fn apply_pending_runtime_authority(&mut self) -> bool {
        let Some(authority) = self.take_pending_runtime_authority() else {
            return false;
        };
        self.apply_runtime_authority(authority).await
    }

    /// The posture this engine is enforcing right now, read from the live
    /// session rather than the shared (possibly newer, unapplied) snapshot.
    fn applied_runtime_authority(&self) -> LiveRuntimeAuthority {
        LiveRuntimeAuthority {
            mode: self.current_mode,
            allow_shell: self.session.allow_shell,
            trust_mode: self.session.trust_mode,
            auto_approve: self.session.auto_approve,
            approval_mode: self.session.approval_mode,
            configured_sandbox_mode: self.api_config.sandbox_mode.clone(),
        }
    }

    fn record_applied_runtime_authority(&self, authority: &TurnAuthority) {
        let applied = LiveRuntimeAuthority::from_turn_authority(
            authority,
            self.api_config.sandbox_mode.clone(),
        );
        let mut state = self
            .live_runtime_authority
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Never overwrite a newer, not-yet-applied user change with the turn
        // posture that preceded it.
        if state.revision == state.applied_revision || state.authority == applied {
            state.authority = applied;
            state.applied_revision = state.revision;
        }
    }

    /// A Python kernel keeps whatever it imported, opened or started under the
    /// posture it ran in. Once the posture grants less than `prior`, drop the
    /// session kernel and every persistent RLM kernel so later code starts in
    /// a fresh interpreter. Nothing takes a kernel out and puts it back, so a
    /// round already in flight cannot restore one.
    ///
    /// Known limitation: these interpreters are approval-gated local
    /// subprocesses, not OS-sandboxed; discarding them bounds reuse, it does
    /// not undo effects of code that already ran.
    async fn discard_kernels_if_narrowed(&mut self, prior: &LiveRuntimeAuthority) {
        if !self.applied_runtime_authority().narrows(prior) {
            return;
        }
        self.repl_kernel = None;
        self.config
            .runtime_services
            .rlm_sessions
            .lock()
            .await
            .clear();
    }

    fn apply_runtime_mode_policy(&mut self, authority: &TurnAuthority) {
        // Prompt composition is mode-agnostic. Keep the hash-guarded refresh
        // because embedders may still derive custom prompt bytes from session
        // context; bundled prompts remain byte-identical across modes.
        let mode_changed = self.current_mode != authority.mode;
        self.current_mode = authority.mode;
        if mode_changed {
            self.refresh_system_prompt_with_reason("mode");
        }
        self.session.allow_shell = authority.allow_shell;
        self.config.allow_shell = authority.allow_shell;
        self.session.trust_mode = authority.trust_mode;
        self.config.trust_mode = authority.trust_mode;
        self.session.approval_mode = authority.approval_mode_for_session();
        self.session.auto_approve =
            authority.auto_approve || self.session.approval_mode == ApprovalMode::Bypass;
        self.record_applied_runtime_authority(authority);
    }

    async fn schedule_goal_continuation(&mut self, dynamic_tools: Vec<DynamicToolSpec>) {
        let delay_seconds = self.config.goal_continuation_delay_seconds;
        let ready_at =
            (delay_seconds > 0).then(|| Instant::now() + Duration::from_secs(delay_seconds));
        if self.scheduled_goal_continuation.is_some() {
            let should_announce = {
                let scheduled = self
                    .scheduled_goal_continuation
                    .as_mut()
                    .expect("scheduled continuation checked above");
                // A normal user turn or idle child handoff can finish while
                // the prior synthetic token is already queued. Refresh that
                // one token instead of multiplying autonomous turns and spend.
                scheduled.dynamic_tools = dynamic_tools;
                if !scheduled.enqueued {
                    scheduled.ready_at = ready_at;
                }
                delay_seconds > 0 && !scheduled.enqueued
            };
            self.try_flush_pending_goal_continuation();
            if should_announce {
                let _ = self
                    .send_event(Event::GoalContinuationWaiting { delay_seconds })
                    .await;
            }
            return;
        }

        self.goal_continuation_schedule_seq =
            self.goal_continuation_schedule_seq.wrapping_add(1).max(1);
        self.scheduled_goal_continuation = Some(ScheduledGoalContinuation {
            id: self.goal_continuation_schedule_seq,
            dynamic_tools,
            enqueued: false,
            ready_at,
            was_delayed: delay_seconds > 0,
        });
        self.try_flush_pending_goal_continuation();
        if delay_seconds > 0 {
            let _ = self
                .send_event(Event::GoalContinuationWaiting { delay_seconds })
                .await;
        }
    }

    async fn cancel_scheduled_goal_continuation(&mut self, interrupted: bool) {
        if let Some(scheduled) = self.scheduled_goal_continuation.take() {
            tracing::debug!(
                "cancelled an outstanding goal continuation after a non-completed turn"
            );
            if scheduled.was_delayed {
                let _ = self
                    .send_event(Event::GoalContinuationWaitEnded { interrupted })
                    .await;
            }
        }
    }

    fn take_scheduled_goal_continuation(
        &mut self,
        engine_schedule_id: Option<u64>,
        direct_dynamic_tools: Vec<DynamicToolSpec>,
    ) -> Option<Vec<DynamicToolSpec>> {
        let Some(schedule_id) = engine_schedule_id else {
            return Some(direct_dynamic_tools);
        };
        let Some(scheduled) = self.scheduled_goal_continuation.take() else {
            tracing::warn!(
                schedule_id,
                "discarding stale engine-owned goal continuation token"
            );
            return None;
        };
        if scheduled.id != schedule_id {
            tracing::warn!(
                schedule_id,
                current_schedule_id = scheduled.id,
                "discarding superseded engine-owned goal continuation token"
            );
            self.scheduled_goal_continuation = Some(scheduled);
            return None;
        }

        // Clear before executing the synthetic turn. A successful execution
        // may now schedule exactly one replacement; inactive/failed turns do
        // not leave a phantom outstanding marker behind.
        Some(scheduled.dynamic_tools)
    }

    fn has_scheduled_goal_continuation(&self) -> bool {
        self.scheduled_goal_continuation.is_some()
    }

    /// Install the conversation identity portion of `SyncSession` and clear
    /// process-local capabilities that must never cross that boundary. The
    /// returned id is the conversation being closed; callers use it to scope
    /// asynchronous fleet finalization before loading the new history.
    /// Conversation id this engine persists and reports in `SessionUpdated`.
    /// Test-only observation point for the host/engine id contract.
    #[cfg(test)]
    pub(crate) fn session_id(&self) -> &str {
        &self.session.id
    }

    /// Install one restored history and invalidate every dependent prompt/token
    /// cache. Resume and conditional undo share this exact implementation.
    fn restore_session_history(
        &mut self,
        messages: Vec<codewhale_models::Message>,
        system_prompt: Option<codewhale_models::SystemPrompt>,
        system_prompt_override: bool,
    ) {
        self.session.tool_activation_cache.clear();
        let compaction_checkpoint =
            compaction_checkpoint_for_restore(&messages, system_prompt.clone());
        let restored_messages =
            project_history_for_restore(messages, compaction_checkpoint.as_ref());
        self.session.messages = restored_messages.into();
        // Direct field assignment bypasses `add_message` /
        // `replace_messages`, which own the messages-revision
        // bump the token-estimate cache keys on (#perf-r5).
        // Without this bump the first estimate after a
        // session restore is computed against whatever
        // history revision was current before the sync — a
        // stale number can flow into capacity checkpoints.
        self.session.bump_messages_revision();
        self.session.latest_parent_input_tokens = None;
        self.session.compaction_summary_prompt = compaction_checkpoint;
        self.session.system_prompt =
            crate::compaction::strip_compaction_summaries(system_prompt.as_ref());
        self.session.last_system_prompt_hash =
            Some(system_prompt_hash(self.session.system_prompt.as_ref()));
        // Prompt pins and drift baselines describe the
        // conversation that was active before this sync. The
        // next submitted turn must establish the installed
        // conversation's own full prefix instead of comparing
        // it with that stale baseline and emitting a
        // `<context_update>` from an empty/restored prompt.
        // Host-owned overrides remain byte-stable because the
        // refresh path exits early while the override is set.
        self.session.pinned_prompt_context = None;
        self.session.context_update_baseline = None;
        // A session sync installs a new (or restored) prefix.
        // Declare it so the next request re-pins the KV-cache
        // prefix under a logged `resume` reason instead of
        // reporting undeclared drift.
        self.session.pending_prefix_change_reason = Some("resume".to_string());
        // Host-supplied prompts are persisted prefixes. Keep them
        // byte-stable; mode/runtime state is projected per request.
        self.session.system_prompt_override =
            system_prompt_override && self.session.system_prompt.is_some();
    }

    fn session_snapshot(&self) -> SessionSnapshot {
        let total_tokens =
            self.session.total_usage.input_tokens + self.session.total_usage.output_tokens;
        SessionSnapshot {
            session_id: self.session.id.clone(),
            messages: self.session.messages.to_vec(),
            total_tokens,
            model: self.session.model.clone(),
            model_provider: self.api_provider_identity.as_ref().map_or_else(
                || "unavailable".to_string(),
                |identity| identity.persisted_kind().to_string(),
            ),
            model_provider_id: self
                .api_provider_identity
                .as_ref()
                .and_then(|identity| identity.persisted_id().map(str::to_string)),
            workspace: self.session.workspace.clone(),
            system_prompt: self.session.system_prompt.clone(),
            mode: self.current_mode.as_setting().to_string(),
        }
    }

    fn install_synced_session_id(&mut self, next_session_id: String) -> Option<String> {
        let previous_session_id = self.session.id.clone();
        if next_session_id == previous_session_id {
            return None;
        }
        // A synthetic token may already be queued in `rx_op`; dropping the
        // authoritative schedule makes that token fail closed when drained.
        self.scheduled_goal_continuation = None;
        // Runtime-added MCP servers are conversation capabilities even when
        // both conversations use the same workspace. The sync handler restarts
        // the session boot once the new session is installed.
        self.drop_mcp_pool();
        // C02-17: cumulative usage belongs to the conversation that spent
        // it. The sync carries no prior total for the installed session, so
        // it starts from zero instead of inheriting the previous session's
        // tokens into its snapshot and saved `total_tokens`.
        self.session.total_usage = Default::default();
        self.session.id = next_session_id;
        if let Some(attachment) = self.extension_host.as_ref() {
            attachment.set_identity(Some(self.session.id.clone()), None);
        }
        Some(previous_session_id)
    }

    fn bounded_redacted_goal_failure_detail(&self, detail: &str) -> Option<String> {
        let detail = detail.trim();
        if detail.is_empty() {
            return None;
        }
        // This message becomes durable goal state. Reuse the model boundary's
        // exact configured-secret redactor when available; that helper also
        // applies the config persistence redactor as a universal backstop.
        let detail = self.codewhale_client.as_ref().map_or_else(
            || codewhale_config::persistence::redact_secrets(detail),
            |client| client.redact_model_bound_text(detail),
        );
        Some(crate::utils::truncate_with_ellipsis(
            &detail,
            GOAL_CONTINUATION_FAILURE_DETAIL_MAX_BYTES,
            "…",
        ))
    }

    fn goal_continuation_failure_message(&self, error: Option<&str>) -> String {
        self.bounded_redacted_goal_failure_detail(error.unwrap_or_default()).map_or_else(
            || {
                "Goal continuation blocked because the model turn failed without a provider reason. Fix the provider route or credentials, then resume the goal."
                    .to_string()
            },
            |detail| {
                format!(
                    "Goal continuation blocked because the model turn failed: {detail}. Fix the failure, then resume the goal."
                )
            },
        )
    }

    fn goal_turn_not_started_message(&self, error: Option<&str>) -> String {
        self.bounded_redacted_goal_failure_detail(error.unwrap_or_default()).map_or_else(
            || {
                "Goal continuation blocked because the next model turn could not be started. Fix the provider route or credentials, then resume the goal."
                    .to_string()
            },
            |detail| {
                format!(
                    "Goal continuation blocked because the next model turn could not be started: {detail}. Fix the provider route or credentials, then resume the goal."
                )
            },
        )
    }

    fn try_flush_pending_goal_continuation(&mut self) {
        let Some(scheduled) = self.scheduled_goal_continuation.as_ref() else {
            return;
        };
        if scheduled.enqueued {
            return;
        }
        if scheduled.ready_at.is_some() {
            return;
        }
        let schedule_id = scheduled.id;

        match self.tx_op.try_send(Op::ContinueGoal {
            // The authoritative set stays in `scheduled_goal_continuation` so
            // later completed turns can refresh it without moving this token.
            dynamic_tools: Vec::new(),
            engine_schedule_id: Some(schedule_id),
        }) {
            Ok(()) => {
                if let Some(scheduled) = self.scheduled_goal_continuation.as_mut()
                    && scheduled.id == schedule_id
                {
                    scheduled.enqueued = true;
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!("goal continuation dropped because the engine mailbox is closed");
                if self
                    .scheduled_goal_continuation
                    .as_ref()
                    .is_some_and(|scheduled| scheduled.id == schedule_id)
                {
                    self.scheduled_goal_continuation = None;
                }
            }
            Err(mpsc::error::TrySendError::Full(_)) => {}
        }
    }

    async fn next_run_input(&mut self, host_managed_turns: bool) -> Option<EngineRunInput> {
        loop {
            // A full mailbox means queued controls must run first. Retrying at
            // the top of each receive appends the continuation behind the
            // remaining controls as soon as one slot becomes available.
            self.try_flush_pending_goal_continuation();
            if self.has_scheduled_goal_continuation() {
                let (enqueued, ready_at) = self
                    .scheduled_goal_continuation
                    .as_ref()
                    .map(|scheduled| (scheduled.enqueued, scheduled.ready_at))
                    .expect("scheduled continuation checked above");
                if enqueued {
                    // The synthetic token sits behind every operation that was
                    // already queued when it was scheduled. Drain FIFO through
                    // that token before accepting an idle child completion.
                    return self
                        .rx_op
                        .recv()
                        .await
                        .map(|op| EngineRunInput::Operation(Box::new(op)));
                }

                if let Some(ready_at) = ready_at {
                    let cancel = self.cancel_token.clone();
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => {
                            self.cancel_scheduled_goal_continuation(true).await;
                            continue;
                        }
                        // Goal status controls and ordinary user messages stay
                        // responsive throughout the wait. A pause/clear action
                        // cancels this exact record in its normal handler.
                        op = self.rx_op.recv() => {
                            return op.map(|op| EngineRunInput::Operation(Box::new(op)));
                        }
                        () = tokio::time::sleep(ready_at.saturating_duration_since(Instant::now())) => {
                            if let Some(scheduled) = self.scheduled_goal_continuation.as_mut()
                                && scheduled.ready_at == Some(ready_at)
                            {
                                scheduled.ready_at = None;
                                let _ = self.send_event(Event::GoalContinuationWaitEnded {
                                    interrupted: false,
                                }).await;
                            }
                            continue;
                        }
                    }
                }

                // A record that is ready but could not enter the full mailbox
                // waits for one queued control. The next loop pass retries the
                // same coalesced token, so there is no spin or duplicate turn.
                return self
                    .rx_op
                    .recv()
                    .await
                    .map(|op| EngineRunInput::Operation(Box::new(op)));
            } else {
                let subagent_wake_armed = !host_managed_turns && !self.cancel_token.is_cancelled();
                let shell_wake_armed = !host_managed_turns && self.idle_shell_wake_armed();
                let mcp_boot_armed = self.mcp_boot_rx.is_some();
                let mcp_supervisor_armed = self.mcp_supervisor_rx.is_some();
                tokio::select! {
                    op = self.rx_op.recv() => {
                        return op.map(|op| EngineRunInput::Operation(Box::new(op)));
                    }
                    completion = self.rx_subagent_completion.recv(), if subagent_wake_armed => {
                        return completion.map(EngineRunInput::SubAgentCompletion);
                    }
                    // No call of this engine awaits approval while idle, and
                    // the handle hands agent answers to the agent directly:
                    // a stale decision has no waiter and is dropped.
                    _ = self.rx_approval.recv() => {}
                    update = async {
                        match self.mcp_boot_rx.as_mut() {
                            Some(rx) => rx.recv().await,
                            None => None,
                        }
                    }, if mcp_boot_armed => {
                        match update {
                            Some(update) => return Some(EngineRunInput::McpBootUpdate(update)),
                            None => self.mcp_boot_rx = None,
                        }
                    }
                    update = async {
                        match self.mcp_supervisor_rx.as_mut() {
                            Some(rx) => rx.recv().await,
                            None => None,
                        }
                    }, if mcp_supervisor_armed => {
                        match update {
                            Some(update) => {
                                return Some(EngineRunInput::McpSupervisorUpdate(update))
                            }
                            // Task exited with the pool; the next pool
                            // ensure respawns against the new one.
                            None => self.mcp_supervisor_rx = None,
                        }
                    }
                    // Background shells have no completion channel, so an
                    // idle engine polls while background work is outstanding,
                    // unless the person interrupted the owning turn.
                    () = tokio::time::sleep(Duration::from_millis(SHELL_WAKE_POLL_MS)), if shell_wake_armed => {
                        if self.finished_background_shell_pending() {
                            return Some(EngineRunInput::ShellCompletionWake);
                        }
                    }
                }
            }
        }
    }

    /// Whether the idle loop should poll for background shell completion: a
    /// background job is running or has finished without being claimed yet.
    /// Plain interactive sessions arm exactly like goal sessions — a finished
    /// background task must reach the model without waiting for the user to
    /// type, the same wake an idle sub-agent completion already gets.
    fn idle_shell_wake_armed(&self) -> bool {
        if self.cancel_token.is_cancelled() {
            return false;
        }
        self.shell_manager
            .lock()
            .map(|manager| manager.may_have_undelivered_completion_for_session(&self.session.id))
            .unwrap_or(false)
    }

    /// Whether a finished background job is waiting to be claimed.
    fn finished_background_shell_pending(&self) -> bool {
        self.shell_manager
            .lock()
            .map(|mut manager| manager.has_finished_unreported_jobs_for_session(&self.session.id))
            .unwrap_or(false)
    }

    /// An idle-engine wake for finished background shell work. With an active
    /// goal this queues a goal continuation; without one it starts an ordinary
    /// runtime turn so the completion reaches the model immediately instead of
    /// sitting unclaimed until the user types. Either way the evidence itself
    /// is claimed by the boundary drain in `handle_send_message`, so the
    /// follow-up turn reads the completion payload the same way a
    /// user-initiated turn would.
    async fn handle_idle_shell_completion_wake(&mut self) {
        // Cancellation can arrive after the idle poll selected this wake.
        // Keep the evidence unclaimed for the next requested turn or /jobs;
        // a surviving shell must not silently restart an interrupted model.
        if self.cancel_token.is_cancelled() {
            return;
        }
        let goal_active = self
            .config
            .goal_state
            .lock()
            .map(|state| state.snapshot().is_active())
            .unwrap_or(false);
        if goal_active {
            let _ = self
                .send_event(Event::status(
                    "Background shell work finished; continuing the active goal".to_string(),
                ))
                .await;
            self.schedule_goal_continuation(Vec::new()).await;
            return;
        }
        let route = match self.current_runtime_route() {
            Ok(route) => route,
            Err(err) => {
                // No route, no turn. Claim the once-only completion now so a
                // dead route cannot re-arm the wake into the same error every
                // poll tick; the user sees what finished and where the output
                // lives, and the next healthy turn proceeds normally.
                let finished = self
                    .shell_manager
                    .lock()
                    .map(|mut manager| {
                        manager
                            .drain_finished_jobs_with_evidence_for_session(&self.session.id)
                            .len()
                    })
                    .unwrap_or(0);
                let _ = self.send_event(Event::error(ErrorEnvelope::fatal_auth(format!(
                        "{finished} background shell task(s) finished, but the turn cannot resume because the provider route is no longer valid: {err}. Their output stays available via /jobs."
                    ))))
                    .await;
                return;
            }
        };
        let _ = self
            .send_event(Event::status(
                "Background shell work finished; resuming the turn".to_string(),
            ))
            .await;
        let _ = self
            .handle_send_message(TurnSpec {
                profile_constitution: None,
                content:
                    "[runtime] A background shell task finished; its completion evidence follows."
                        .to_string(),
                mode: self.current_mode,
                route: Box::new(route),
                compaction: Box::new(self.config.compaction.clone()),
                initial_routed_usage: Box::new(crate::cost_status::RuntimeUsageBatch::default()),
                goal_objective: self.config.goal_objective.clone(),
                goal_token_budget: self.config.goal_token_budget,
                goal_status: self.config.goal_status,
                reasoning_effort: self.session.reasoning_effort.clone(),
                reasoning_effort_auto: self.session.reasoning_effort_auto,
                auto_model: self.session.auto_model,
                allow_shell: self.session.allow_shell,
                trust_mode: self.session.trust_mode,
                auto_approve: self.session.auto_approve,
                approval_mode: self.session.approval_mode,
                translation_enabled: self.config.translation_enabled,
                allowed_tools: self.config.allowed_tools.clone(),
                dynamic_tools: Vec::new(),
                hook_executor: self.config.hook_executor.clone(),
                verbosity: self.config.verbosity.clone(),
                provenance: UserInputProvenance::Runtime,
                images: Vec::new(),
                max_output_tokens: None,
                // Background shell completion wake: no host submission to
                // correlate with.
                submission_id: None,
            })
            .await;
    }

    /// Run the engine event loop
    pub async fn run(self) {
        let _ = self.run_owned().await;
    }

    #[allow(clippy::too_many_lines)]
    async fn run_owned(mut self) -> Option<Result<crate::tools::subagent::SubAgentResult>> {
        let mut child_result = None;
        // RuntimeThreadManager owns durable turn claims and installs a thread
        // id in runtime services. Only the interactive TUI may autonomously
        // create a new turn while the engine is otherwise idle; a hosted
        // engine must wait for its host to claim and explicitly dispatch the
        // next turn so events cannot be attached to the wrong durable record.
        let host_managed_turns = self.host_managed_turns();
        // #6184: supervise the turn heartbeat from outside the turn future,
        // so a wedged await still produces a log line, a stall record and a
        // status event. The watchdog exits once the event channel closes.
        let stall_watchdog = turn_heartbeat::spawn_turn_stall_watchdog(
            Arc::clone(&self.turn_heartbeat),
            self.tx_event.clone(),
        );
        let _stall_watchdog_guard = turn_heartbeat::AbortOnDrop(stall_watchdog);
        if let Err(error) = self
            .start_mcp_session_boot(McpConnectRefresh::IfChanged)
            .await
        {
            tracing::debug!(
                "MCP session boot failed: {}",
                crate::mcp::format_mcp_error_for_display(&error)
            );
        }

        loop {
            let Some(input) = self.next_run_input(host_managed_turns).await else {
                break;
            };

            // Runtime posture updates publish through shared typed state
            // before attempting their best-effort wake-up. If the mailbox was
            // already full, its next queued operation is the wake-up: apply
            // the latest authority before doing any work under an obsolete
            // policy.
            if matches!(&input, EngineRunInput::Operation(_)) {
                self.apply_pending_runtime_authority().await;
            }

            match input {
                EngineRunInput::SubAgentCompletion(completion) => {
                    self.handle_idle_subagent_completion(completion).await;
                }
                EngineRunInput::McpBootUpdate(update) => {
                    self.apply_mcp_boot_update(update).await;
                }
                EngineRunInput::McpSupervisorUpdate(update) => {
                    self.apply_mcp_supervisor_update(update).await;
                }
                EngineRunInput::ShellCompletionWake => {
                    self.handle_idle_shell_completion_wake().await;
                }
                EngineRunInput::Operation(op) => match *op {
                    Op::SendMessage(spec) => {
                        self.admitted_turn_control = {
                            let mut controls = self
                                .turn_controls
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let control = controls.pending.pop_front();
                            controls.active = control.clone();
                            control
                        };
                        // Keep the send-message state machine out of this
                        // event-loop future's stack frame.
                        if self.child_host.is_some() {
                            match Box::pin(self.handle_child_send_message(spec)).await {
                                Ok(None) => {}
                                Ok(Some(result)) => {
                                    child_result = Some(Ok(result));
                                    break;
                                }
                                Err(error) => {
                                    child_result = Some(Err(error));
                                    break;
                                }
                            }
                        } else {
                            Box::pin(self.handle_send_message(spec)).await;
                        }
                    }
                    Op::ContinueGoal {
                        dynamic_tools,
                        engine_schedule_id,
                    } => {
                        // Cancellation can race the delay expiry after the
                        // coalesced token entered the mailbox. Re-check the
                        // same turn token before consuming the schedule so an
                        // interrupt at the boundary never starts a provider
                        // request and is not erased by the next turn's reset.
                        if engine_schedule_id.is_some() && self.cancel_token.is_cancelled() {
                            self.cancel_scheduled_goal_continuation(true).await;
                            continue;
                        }
                        let Some(dynamic_tools) = self
                            .take_scheduled_goal_continuation(engine_schedule_id, dynamic_tools)
                        else {
                            continue;
                        };
                        // Host-injected tokens carry their own quiet period:
                        // host-managed sessions never run the engine-owned
                        // scheduler, so this arm is their only continuation
                        // dispatch site and must honor
                        // [goal] continuation_delay_seconds itself before
                        // dispatching. The wait is biased-cancellable (Esc,
                        // steer, or host cancel always wins over a racing
                        // expiry), and the live goal is re-read below only
                        // after it, so a pause/clear/complete/blocked landing
                        // during the quiet period cancels the pass and
                        // failures never continue. Engine-owned tokens (Some)
                        // already waited out ready_at in the scheduler and
                        // keep those semantics untouched.
                        if engine_schedule_id.is_none()
                            && crate::goal_loop::await_continuation_wait(
                                crate::goal_loop::continuation_wait(
                                    self.config.goal_continuation_delay_seconds,
                                ),
                                &self.cancel_token,
                            )
                            .await
                                == crate::goal_loop::ContinuationWaitOutcome::Cancelled
                        {
                            continue;
                        }
                        // Status controls queued while the previous turn was
                        // running are processed before this operation, and a
                        // host-injected quiet period has now elapsed. Re-read
                        // the live goal so pause/clear/complete/blocked can
                        // cancel a stale continuation without starting a turn.
                        let (content, goal_snapshot) = match self.goal_continuation_if_active() {
                            GoalContinuationAction::Inactive => continue,
                            GoalContinuationAction::Dispatch { content, snapshot } => {
                                (content, *snapshot)
                            }
                            GoalContinuationAction::Stopped { message, reason } => {
                                self.pause_goal_continuation(reason, message).await;
                                continue;
                            }
                        };
                        // Budget and inactive-state decisions are route
                        // independent. Resolve the live route only for a real
                        // dispatch so an exhausted goal still reaches its
                        // truthful terminal state when provider config drifted.
                        let route = match self.current_runtime_route() {
                            Ok(route) => route,
                            Err(err) => {
                                let message = format!(
                                    "Goal continuation blocked because its provider route is no longer valid: {err}. Fix the route, then resume the goal."
                                );
                                let _ = self.send_event(Event::error(ErrorEnvelope::fatal_auth(format!(
                                        "Goal continuation stopped because its provider route is no longer valid: {err}"
                                    ))))
                                    .await;
                                self.block_goal_continuation(message).await;
                                continue;
                            }
                        };

                        let _ = self
                            .handle_send_message(TurnSpec {
                                profile_constitution: None,
                                content,
                                mode: self.current_mode,
                                route: Box::new(route),
                                compaction: Box::new(self.config.compaction.clone()),
                                initial_routed_usage: Box::new(
                                    crate::cost_status::RuntimeUsageBatch::default(),
                                ),
                                goal_objective: goal_snapshot.objective,
                                goal_token_budget: goal_snapshot.token_budget,
                                goal_status: GoalStatus::Active,
                                reasoning_effort: self.session.reasoning_effort.clone(),
                                reasoning_effort_auto: self.session.reasoning_effort_auto,
                                auto_model: self.session.auto_model,
                                allow_shell: self.session.allow_shell,
                                trust_mode: self.session.trust_mode,
                                auto_approve: self.session.auto_approve,
                                approval_mode: self.session.approval_mode,
                                translation_enabled: self.config.translation_enabled,
                                allowed_tools: self.config.allowed_tools.clone(),
                                dynamic_tools,
                                hook_executor: self.config.hook_executor.clone(),
                                verbosity: self.config.verbosity.clone(),
                                provenance: UserInputProvenance::Runtime,
                                images: Vec::new(),
                                max_output_tokens: None,
                                // Engine-scheduled goal continuation: no host
                                // submission to correlate with.
                                submission_id: None,
                            })
                            .await;
                    }
                    Op::RunShellCommand {
                        command,
                        mode,
                        allow_shell,
                        trust_mode,
                        auto_approve,
                        approval_mode,
                    } => {
                        self.handle_run_shell_command(
                            command,
                            mode,
                            allow_shell,
                            trust_mode,
                            auto_approve,
                            approval_mode,
                        )
                        .await;
                    }
                    Op::SetGoalStatus {
                        status,
                        clear,
                        goal_id,
                    } => {
                        self.handle_set_goal_status(status, clear, goal_id).await;
                    }
                    Op::SetGoalObjective {
                        objective,
                        token_budget,
                        goal_id,
                    } => {
                        self.handle_set_goal_objective(objective, token_budget, goal_id)
                            .await;
                    }
                    Op::PreviewOutboundRequest {
                        inputs,
                        json,
                        base_prompt_only,
                    } => {
                        // Pure inspection: no turn is started, no message is
                        // added, no engine state is written, and no provider
                        // request is sent. Facts that are not exactly knowable
                        // come back as typed unavailable sections rather than
                        // as an error or a guess.
                        let rendered = if base_prompt_only {
                            crate::request_manifest::exact_base_prompt_only()
                        } else {
                            let manifest = self.build_request_manifest(*inputs).await;
                            if json {
                                manifest.to_json()
                            } else {
                                manifest.render()
                            }
                        };
                        let _ = self
                            .send_event(Event::RequestManifestReady { rendered })
                            .await;
                    }
                    Op::ListSubAgents => {
                        // #3803: the sidebar refresh is a read-only snapshot.
                        // Render from a read lock; only take the write lock to
                        // run cleanup on a bounded cadence, so a UI refresh storm
                        // during a sub-agent fanout no longer contends for the
                        // write lock (against completions/persistence) on every
                        // request. Cleanup still auto-cancels stale agents.
                        let active_session_id = self.session.id.clone();
                        self.touch_workers_with_running_shells().await;
                        let due = {
                            let manager = self.subagent_manager.read().await;
                            manager.cleanup_due(
                                crate::tools::subagent::SUBAGENT_LIST_CLEANUP_MIN_INTERVAL,
                            )
                        };
                        let event = if due {
                            let mut manager = self.subagent_manager.write().await;
                            manager.cleanup_for_session(
                                &active_session_id,
                                Duration::from_secs(60 * 60),
                            );
                            agent_list_event(&manager, &active_session_id)
                        } else {
                            let manager = self.subagent_manager.read().await;
                            agent_list_event(&manager, &active_session_id)
                        };
                        // #3802: use non-blocking send — this is a refresh event
                        // that can safely be dropped when the channel is full.
                        // The next drain cycle will re-request the list.
                        if let Err(_e) = self.tx_event.try_send(event) {
                            tracing::debug!(
                                "Event channel full; dropping ListSubAgents refresh (will retry next drain)"
                            );
                        }
                    }
                    Op::GetSubAgentSettlement { tx } => {
                        let snapshot = self.subagent_settlement_snapshot().await;
                        if let Some(tx) = tx
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        {
                            let _ = tx.send(snapshot);
                        }
                    }
                    Op::CancelSubAgent { agent_id } => {
                        let active_session_id = self.session.id.clone();
                        let cancelled = self
                            .subagent_manager
                            .write()
                            .await
                            .cancel_agent_for_session(&active_session_id, &agent_id);
                        let result = match cancelled {
                            Ok(snapshot) => {
                                let snapshot = crate::tools::subagent::settle_requested_child(
                                    &self.subagent_manager,
                                    snapshot,
                                )
                                .await;
                                // F4: cancelling keeps the work — inventory and
                                // checkpoint what the child left, off the lock.
                                crate::tools::subagent::preserve_cancelled_work(
                                    &self.subagent_manager,
                                    snapshot,
                                )
                                .await;
                                let manager = self.subagent_manager.read().await;
                                Ok(agent_list_event(&manager, &active_session_id))
                            }
                            Err(err) => Err(err),
                        };
                        match result {
                            Ok(event) => {
                                if let Err(_e) = self.tx_event.try_send(event) {
                                    tracing::debug!(
                                        "Event channel full; dropping CancelSubAgent refresh"
                                    );
                                }
                            }
                            Err(err) => {
                                let _ =
                                    self.tx_event
                                        .try_send(Event::error(ErrorEnvelope::transient(format!(
                                            "Failed to cancel sub-agent {agent_id}: {err}"
                                        ))));
                            }
                        }
                    }
                    Op::FollowUpSubAgent { agent_id, text } => {
                        let active_session_id = self.session.id.clone();
                        let runtime = self.off_turn_subagent_runtime();
                        let manager_handle = Arc::clone(&self.subagent_manager);
                        let (outcome, refresh) = {
                            let mut manager = self.subagent_manager.write().await;
                            let outcome = manager
                                .continue_child_from_user_for_session(
                                    &active_session_id,
                                    manager_handle,
                                    runtime,
                                    &agent_id,
                                    &text,
                                )
                                .map_err(|err| err.to_string());
                            (outcome, agent_list_event(&manager, &active_session_id))
                        };
                        let _ = self
                            .send_event(Event::SubAgentFollowUp {
                                owner_session_id: active_session_id,
                                agent_id,
                                outcome,
                            })
                            .await;
                        if let Err(_e) = self.tx_event.try_send(refresh) {
                            tracing::debug!(
                                "Event channel full; dropping FollowUpSubAgent refresh"
                            );
                        }
                    }
                    Op::ChangeMode { .. } => {
                        // The mailbox payload may predate a newer posture that
                        // was published while the channel was full. Apply the
                        // single live snapshot so a stale queued ChangeMode
                        // can never roll authority backward.
                        let authority = self.runtime_authority_snapshot();
                        self.apply_runtime_authority(authority).await;
                    }
                    Op::SetModel {
                        model,
                        mode: _,
                        route_limits,
                    } => {
                        let identity = self.api_provider_identity.clone();
                        // SetModel carries no route: the endpoint stays the
                        // one the current client is built on.
                        let endpoint = self.active_route_endpoint.clone();
                        self.forget_input_bill_if_route_changes(
                            identity
                                .as_ref()
                                .map_or("unavailable", |identity| identity.key.as_str()),
                            identity
                                .as_ref()
                                .and_then(|identity| identity.persisted_id()),
                            endpoint.as_ref(),
                            &model,
                            route_limits,
                        );
                        self.session.auto_model = model.trim().eq_ignore_ascii_case("auto");
                        self.session.model = model;
                        self.config.model.clone_from(&self.session.model);
                        self.active_route_limits = route_limits;
                        // This lightweight operation carries no executable
                        // route candidate, so old provider/model capability
                        // facts must not bleed into the new model.
                        self.active_route_capabilities =
                            codewhale_config::route::RouteCapabilities::default();
                        self.refresh_system_prompt_with_reason("model");
                        self.emit_session_updated().await;
                        let _ = self
                            .send_event(Event::status(format!(
                                "Model set to: {}",
                                self.session.model
                            )))
                            .await;
                    }
                    Op::SetCompaction { config } => {
                        // Hosts resend the compaction config on every route,
                        // model or session sync, and those syncs move the
                        // model, window and thresholds without the user
                        // touching the switch. Only the switch is news: an
                        // acknowledgement for anything else used to overwrite
                        // a real error in the footer (U1) and the "Resumed:"
                        // receipt a session restore had just shown.
                        let enabled = config.enabled;
                        let switched = self.config.compaction.enabled != enabled;
                        self.config.compaction = config;
                        if switched {
                            let _ = self
                                .send_event(Event::status(format!(
                                    "Make room automatically: {}",
                                    if enabled { "on" } else { "off" }
                                )))
                                .await;
                        }
                    }
                    Op::SetStreamChunkTimeout { timeout_secs } => {
                        self.config.stream_chunk_timeout = Duration::from_secs(timeout_secs);
                        let _ = self
                            .send_event(Event::status(format!(
                                "Stream chunk timeout set to {timeout_secs}s"
                            )))
                            .await;
                    }
                    Op::SetSubagentRuntimeConfig {
                        enabled,
                        max_subagents,
                        launch_concurrency,
                        max_spawn_depth,
                        api_timeout_secs,
                        heartbeat_timeout_secs,
                    } => {
                        self.config.subagents_enabled = enabled;
                        self.config.max_subagents =
                            max_subagents.clamp(1, crate::config::MAX_SUBAGENTS);
                        self.config.launch_concurrency =
                            launch_concurrency.clamp(1, self.config.max_subagents);
                        self.config.max_spawn_depth =
                            max_spawn_depth.min(codewhale_config::MAX_SPAWN_DEPTH_CEILING);
                        self.config.subagent_api_timeout = Duration::from_secs(api_timeout_secs);
                        self.config.subagent_heartbeat_timeout =
                            Duration::from_secs(heartbeat_timeout_secs);
                        let launch_gate_applied =
                            if let Some(limits) = self.subagent_execution_limits.as_mut() {
                                limits.update(
                                    self.config.max_subagents,
                                    self.config.max_admitted_subagents,
                                    self.config.subagent_heartbeat_timeout,
                                    self.config.launch_concurrency,
                                );
                                true
                            } else {
                                let mut manager = self.subagent_manager.write().await;
                                manager.update_runtime_limits(
                                    self.config.max_subagents,
                                    self.config.max_admitted_subagents,
                                    self.config.subagent_heartbeat_timeout,
                                    self.config.launch_concurrency,
                                )
                            };
                        let launch_note = if launch_gate_applied {
                            ""
                        } else {
                            "; launch_concurrency takes full effect after active sub-agents finish or the session restarts"
                        };
                        let _ = self.send_event(Event::status(format!(
                                "Sub-agent runtime updated: enabled={enabled}, max_subagents={}, launch_concurrency={}, max_depth={}{}",
                                self.config.max_subagents,
                                self.config.launch_concurrency,
                                self.config.max_spawn_depth,
                                launch_note
                            )))
                            .await;
                    }
                    Op::SetFleetRoster { roster } => {
                        self.config.fleet_roster = roster;
                        let _ = self
                            .send_event(Event::status(
                                "Fleet roster refreshed for subsequent turns".to_string(),
                            ))
                            .await;
                    }
                    Op::SyncSession {
                        session_id,
                        messages,
                        system_prompt,
                        system_prompt_override,
                        model,
                        workspace,
                        mode,
                    } => {
                        let plugin_workspace_changed =
                            self.plugin_registry.workspace() != workspace.as_path();
                        let previous_session_id = self.session.id.clone();
                        let next_session_id = if let Some(session_id) = session_id {
                            session_id
                        } else if messages.is_empty() && system_prompt.is_none() {
                            uuid::Uuid::new_v4().to_string()
                        } else {
                            previous_session_id.clone()
                        };
                        let closed_session_id = self.install_synced_session_id(next_session_id);
                        // SyncSession installs a conversation's identity; an id
                        // change IS a conversation boundary in this runtime —
                        // callers must pass their own conversation id for
                        // same-conversation re-syncs. A boundary in the same
                        // process does not rebuild the sub-agent manager, so the
                        // previous conversation's live children and write claims
                        // must be finalized here or they keep gating writers in
                        // the new conversation (#5372). Same-session reloads
                        // keep their id and are deliberately left untouched.
                        if let Some(closed_session_id) = closed_session_id {
                            let finalized = self
                                .subagent_manager
                                .write()
                                .await
                                .finalize_session_close_for_session(&closed_session_id);
                            crate::tools::subagent::settle_requested_children_for_session(
                                &self.subagent_manager,
                                &closed_session_id,
                                None,
                            )
                            .await;
                            if finalized > 0 {
                                tracing::info!(
                                    target: "subagent",
                                    finalized,
                                    "finalized sub-agent fleet for closed session"
                                );
                            }
                        }
                        self.restore_session_history(
                            messages,
                            system_prompt,
                            system_prompt_override,
                        );
                        self.session.auto_model = model.trim().eq_ignore_ascii_case("auto");
                        self.session.model = model;
                        self.session.workspace = workspace.clone();
                        self.current_mode = mode;
                        self.config.model.clone_from(&self.session.model);
                        self.config.workspace = workspace.clone();
                        if plugin_workspace_changed {
                            self.plugin_registry =
                                self.plugin_registry.rediscover_for_workspace(&workspace);
                            self.config.plugin_registry = Some(Arc::clone(&self.plugin_registry));
                            // A pool may contain plugin servers and authority
                            // receipts from the previous workspace snapshot.
                            self.drop_mcp_pool();
                            if let Some(attachment) = &self.extension_host {
                                attachment.set_plugins(Arc::clone(&self.plugin_registry));
                                self.plugin_registry = attachment.plugin_view();
                                attachment.sync_in_background();
                            }
                        }
                        let ctx =
                            crate::project_context::load_project_context_with_parents(&workspace);
                        self.session.project_context = if ctx.has_instructions() {
                            Some(ctx)
                        } else {
                            None
                        };
                        self.session.rebuild_working_set();
                        self.reconcile_restored_work_bindings().await;
                        if self.mcp_pool.is_none()
                            && let Err(error) = self
                                .start_mcp_session_boot(McpConnectRefresh::IfChanged)
                                .await
                        {
                            tracing::debug!(
                                "MCP session boot failed: {}",
                                crate::mcp::format_mcp_error_for_display(&error)
                            );
                        }
                        // SessionUpdated acknowledges the sync. A generic status
                        // would immediately cover the host's confirmed resume receipt.
                        self.emit_session_updated().await;
                    }
                    Op::RewindConversation {
                        expected,
                        messages,
                        tx,
                    } => {
                        // Compare at the Engine mailbox boundary, not only in
                        // the UI: queued work may have changed the conversation
                        // since preflight. Refuse before touching any state.
                        if self.session_snapshot() != *expected
                            || messages.len() >= expected.messages.len()
                            || !expected.messages.starts_with(&messages)
                        {
                            let _ = tx.send(None);
                            continue;
                        }
                        self.restore_session_history(
                            messages,
                            self.session.system_prompt.clone(),
                            self.session.system_prompt_override,
                        );
                        self.session.rebuild_working_set();
                        self.reconcile_restored_work_bindings().await;
                        let _ = tx.send(Some(self.session_snapshot()));
                        self.emit_session_updated().await;
                    }
                    Op::CompactContext {
                        id,
                        route,
                        compaction,
                    } => {
                        self.handle_manual_compaction_op(id, *route, *compaction)
                            .await;
                    }
                    Op::CancelCompaction { id } => {
                        // Cancellation is published out-of-band by the handle
                        // so a provider await cannot block it. Draining the
                        // typed op only clears a late, already-settled marker.
                        self.finish_compaction(&id);
                    }
                    Op::GetSessionSnapshot { tx } => {
                        let snapshot = self.session_snapshot();
                        if let Some(tx) = tx.lock().ok().and_then(|mut g| g.take()) {
                            let _ = tx.send(snapshot);
                        }
                    }
                    Op::GetContextBudget { tx } => {
                        let input_tokens = self.estimated_input_tokens() as u64;
                        let budget = route_context_budget_for_route(
                            self.api_provider,
                            &self.session.model,
                            self.active_route_limits,
                            usize::try_from(input_tokens).unwrap_or(usize::MAX),
                        );
                        let snapshot = budget.map(|budget| SessionContextBudget {
                            window_tokens: budget.window_tokens,
                            input_tokens,
                            billed_input_tokens: self
                                .session
                                .latest_parent_input_tokens
                                .map(u64::from),
                            output_cap_tokens: budget.output_cap_tokens,
                            input_budget_ceiling: budget.input_budget_ceiling,
                            available_input_tokens: budget.available_input_tokens,
                            compaction_trigger_tokens: budget.compaction_trigger_tokens,
                            usage_percent: budget.usage_percent(),
                            pressure: budget.pressure.label(),
                            model: self.session.model.clone(),
                            provider: self.api_provider_identity.as_ref().map_or_else(
                                || "unavailable".to_string(),
                                |identity| identity.persisted_kind().to_string(),
                            ),
                            model_provider_id: self
                                .api_provider_identity
                                .as_ref()
                                .and_then(|identity| identity.persisted_id().map(str::to_string)),
                        });
                        if let Some(tx) = tx.lock().ok().and_then(|mut g| g.take()) {
                            let _ = tx.send(snapshot);
                        }
                    }
                    Op::GetProviderRuntimeStatus { tx } => {
                        let status = if let Some(client) = self.codewhale_client.as_ref() {
                            ProviderRuntimeStatus {
                                provider: client.api_provider(),
                                request_concurrency_limit: client
                                    .provider_request_concurrency_limit(),
                                active_provider_requests: client.active_provider_requests(),
                            }
                        } else {
                            let provider = self.api_provider;
                            ProviderRuntimeStatus {
                                provider,
                                request_concurrency_limit: self
                                    .api_provider_identity
                                    .as_ref()
                                    .and_then(|identity| {
                                        self.api_config.provider_max_concurrency(identity)
                                    }),
                                active_provider_requests: 0,
                            }
                        };
                        if let Some(tx) = tx.lock().ok().and_then(|mut g| g.take()) {
                            let _ = tx.send(status);
                        }
                    }
                    Op::BootstrapMcp { tx } => {
                        let result = self.bootstrap_mcp_pool().await.map_err(|error| {
                            codewhale_config::persistence::redact_secrets(&format!("{error:#}"))
                        });
                        if let Some(tx) = tx.lock().ok().and_then(|mut guard| guard.take()) {
                            let _ = tx.send(result);
                        }
                    }
                    Op::RetryMcpServer { name, tx } => {
                        let result = self.retry_mcp_server(&name).await.map_err(|error| {
                            codewhale_config::persistence::redact_secrets(&format!("{error:#}"))
                        });
                        if let Some(tx) = tx.lock().ok().and_then(|mut guard| guard.take()) {
                            let _ = tx.send(result);
                        }
                    }
                    Op::ReloadMcp { config_path, tx } => {
                        let result = self.reload_mcp_pool(config_path).await.map_err(|error| {
                            codewhale_config::persistence::redact_secrets(&format!("{error:#}"))
                        });
                        if let Some(tx) = tx.lock().ok().and_then(|mut guard| guard.take()) {
                            let _ = tx.send(result);
                        }
                    }
                    Op::PurgeContext => {
                        if let Some(pm) = self.session.prefix_stability.as_mut() {
                            pm.note_history_reset("clear");
                        }
                        self.handle_purge().await;
                    }
                    Op::EditLastTurn {
                        new_message,
                        submission_id,
                    } => {
                        let route = match self.current_runtime_route() {
                            Ok(route) => route,
                            Err(err) => {
                                self.reject_edit_last_turn(ErrorEnvelope::new(
                                    ErrorCategory::Authentication,
                                    ErrorSeverity::Critical,
                                    false,
                                    "edit_last_turn_invalid_route",
                                    format!(
                                        "Cannot edit the last turn because its provider route is no longer valid: {err}"
                                    ),
                                ))
                                .await;
                                continue;
                            }
                        };
                        // #383: /edit — remove the last user+assistant exchange
                        // from the session, then re-send with the new content.
                        // Tool results and runtime-owned internal envelopes are
                        // also persisted with role "user", so locate the cut
                        // point by genuine user prompt — a bare role scan would
                        // land mid-turn on a tool_result and keep the old
                        // prompt plus its tool round-trips in history.
                        let idx = match crate::runtime_handoff::edit_last_turn_target(
                            &self.session.messages,
                        ) {
                            crate::runtime_handoff::EditLastTurnTarget::Editable(idx) => idx,
                            crate::runtime_handoff::EditLastTurnTarget::Unsupported => {
                                self.reject_edit_last_turn(ErrorEnvelope::new(
                                    ErrorCategory::InvalidInput,
                                    ErrorSeverity::Error,
                                    false,
                                    "edit_last_turn_unsupported_user_content",
                                    "Cannot edit the last turn because the latest user message has no editable text content.",
                                ))
                                .await;
                                continue;
                            }
                            crate::runtime_handoff::EditLastTurnTarget::Missing => {
                                self.reject_edit_last_turn(ErrorEnvelope::new(
                                    ErrorCategory::State,
                                    ErrorSeverity::Error,
                                    false,
                                    "edit_last_turn_no_user_prompt",
                                    "Cannot edit the last turn because the session history has no user message to replace.",
                                ))
                                .await;
                                continue;
                            }
                        };
                        // C02-02: stage the cut. The removed exchange is kept
                        // until the replacement turn actually starts; a send
                        // that never starts puts it back (below).
                        let removed_exchange = self
                            .session
                            .messages
                            .get(idx..)
                            .map(<[_]>::to_vec)
                            .unwrap_or_default();
                        self.session.messages.truncate_to(idx);
                        self.session.bump_messages_revision();
                        // Now dispatch the new message as a normal send,
                        // reusing the engine's stored mode/model config.
                        let mode = self.current_mode;
                        let outcome = self
                            .handle_send_message(TurnSpec {
                                profile_constitution: None,
                                content: new_message.clone(),
                                mode,
                                route: Box::new(route),
                                compaction: Box::new(self.config.compaction.clone()),
                                initial_routed_usage: Box::new(
                                    crate::cost_status::RuntimeUsageBatch::default(),
                                ),
                                goal_objective: self.config.goal_objective.clone(),
                                goal_token_budget: self.config.goal_token_budget,
                                goal_status: self.config.goal_status,
                                reasoning_effort: self.session.reasoning_effort.clone(),
                                reasoning_effort_auto: self.session.reasoning_effort_auto,
                                auto_model: self.session.auto_model,
                                allow_shell: self.session.allow_shell,
                                trust_mode: self.session.trust_mode,
                                auto_approve: self.session.auto_approve,
                                approval_mode: self.session.approval_mode,
                                translation_enabled: self.config.translation_enabled,
                                allowed_tools: self.config.allowed_tools.clone(),
                                dynamic_tools: Vec::new(),
                                hook_executor: self.config.hook_executor.clone(),
                                verbosity: self.config.verbosity.clone(),
                                provenance: UserInputProvenance::ExternalUser,
                                images: Vec::new(),
                                max_output_tokens: None,
                                submission_id,
                            })
                            .await;
                        if matches!(outcome, SendMessageOutcome::NotStarted { .. }) {
                            // Anything the failed send appended after the cut
                            // (a drained shell-completion notice) stays, after
                            // the restored exchange.
                            let appended = self
                                .session
                                .messages
                                .get(idx..)
                                .map(<[_]>::to_vec)
                                .unwrap_or_default();
                            self.session.messages.truncate_to(idx);
                            self.session.messages.push_batch(removed_exchange);
                            self.session.messages.push_batch(appended);
                            self.session.bump_messages_revision();
                            self.emit_session_updated().await;
                        }
                    }
                    Op::SetAdvisorEnabled { enabled } => {
                        self.config.advisor_config.enabled = enabled;
                        let state = if enabled { "enabled" } else { "disabled" };
                        let _ = self.send_event(Event::status(format!(
                                "Advisor watcher {state}. Notes will appear after turns with tool calls."
                            )))
                            .await;
                        tracing::info!(target: "advisor", "advisor watcher {state}");
                    }
                    Op::SetSearchProvider { provider } => {
                        self.config.search_provider = provider;
                        // A provider picked in-session is a pin; only an
                        // explicit `[search] native = true` still leads.
                        self.config.search_native.get_or_insert(false);
                    }
                    Op::Shutdown => {
                        break;
                    }
                },
            }
        }

        // #freeze: flush any sub-agent checkpoint that the hot-path debounce
        // coalesced away, so a graceful shutdown keeps the latest progress.
        {
            let mut manager = self.subagent_manager.write().await;
            let children = manager.list_for_session(&self.session.id);
            for child in children {
                let owned = self.child_host.as_ref().is_none_or(|owner| {
                    child.parent_run_id.as_deref() == Some(owner.authority.owner_agent_id.as_str())
                });
                if owned && child.status == SubAgentStatus::Running {
                    let _ = manager.cancel_agent_for_session(&self.session.id, &child.agent_id);
                }
            }
            manager.flush_pending_persist();
        }
        crate::tools::subagent::settle_requested_children_for_session(
            &self.subagent_manager,
            &self.session.id,
            self.child_host
                .as_ref()
                .map(|child| child.authority.owner_agent_id.as_str()),
        )
        .await;
        self.subagent_manager.write().await.flush_pending_persist();

        // #420: graceful MCP shutdown — send SIGTERM and give stdio servers
        // a brief window to exit before drop fires SIGKILL via kill_on_drop.
        // Best-effort: pool may not exist (no MCP configured) and the lock
        // can fail under contention; either way the kill_on_drop fallback
        // still reaps the children.
        if self.child_host.is_none()
            && let Some(pool) = self.mcp_pool.as_ref()
        {
            let mut guard = pool.lock().await;
            guard.shutdown_all().await;
        }
        child_result
    }

    fn host_managed_turns(&self) -> bool {
        self.child_host.is_some() || self.config.runtime_services.active_thread_id.is_some()
    }

    async fn subagent_settlement_snapshot(&self) -> crate::core::ops::SubAgentSettlement {
        // Terminal delivery enqueues the completion while holding this write
        // lock, before changing Running to terminal. Keep the read guard until
        // both observations are captured so no completion can fall in the gap.
        let manager = self.subagent_manager.read().await;
        crate::core::ops::SubAgentSettlement {
            running_children: self.child_host.as_ref().map_or_else(
                || manager.live_count_for_session(&self.session.id),
                |child| {
                    manager.live_count_for_parent(&self.session.id, &child.authority.owner_agent_id)
                },
            ),
            // Workflow terminal delivery queues its receipt before removing
            // the controller. Observe controllers before the inbox so a gap
            // between phases cannot look like a settled parent.
            running_workflows: crate::tools::workflow::live_workflow_count(
                &self.session.workspace,
                &self.session.id,
            ),
            pending_completions: self.rx_subagent_completion.len(),
        }
    }

    async fn emit_session_updated(&self) {
        if let Some(job) = self.child_job()
            && let Err(error) = job
                .project(
                    &self.session.messages,
                    job.steps(),
                    self.session.system_prompt.as_ref(),
                )
                .await
        {
            self.cancel_token.cancel();
            tracing::error!(%error, "child Session projection failed; turn stopped");
        }
        let _ = self
            .send_event(Event::SessionUpdated {
                session_id: self.session.id.clone(),
                messages: self.session.messages.snapshot(),
                system_prompt: self.session.system_prompt.clone(),
                model: self.session.model.clone(),
                workspace: self.session.workspace.clone(),
            })
            .await;
    }

    fn goal_snapshot_for_event(&self) -> Option<GoalSnapshot> {
        match self.config.goal_state.lock() {
            Ok(state) => {
                let snapshot = state.snapshot();
                snapshot.objective.is_some().then_some(snapshot)
            }
            Err(err) => {
                tracing::warn!("goal state lock poisoned while emitting goal update: {err}");
                None
            }
        }
    }

    async fn emit_goal_updated(&self) {
        if let Some(snapshot) = self.goal_snapshot_for_event() {
            let _ = self.send_event(Event::GoalUpdated { snapshot }).await;
        }
    }

    fn record_goal_usage_for_turn(&self, usage: &Usage, started: Instant) {
        let token_delta =
            u64::from(usage.input_tokens).saturating_add(u64::from(usage.output_tokens));
        let time_delta_seconds = started.elapsed().as_secs();
        if token_delta == 0 && time_delta_seconds == 0 {
            return;
        }
        match self.config.goal_state.lock() {
            Ok(mut state) => state.record_usage(token_delta, time_delta_seconds, started),
            Err(err) => tracing::warn!("goal state lock poisoned while recording usage: {err}"),
        }
    }

    fn active_input_tokens_with_current_text(
        &self,
        current_text: &str,
        system_prompt: Option<&SystemPrompt>,
    ) -> usize {
        // Estimate the installed history IN PLACE — no full-transcript clone
        // per `<turn_meta>` build (#perf-r5). `&AppendLog` deref-coerces to
        // `&[Message]` exactly like the cache call site.
        let base = estimate_input_tokens_conservative(&self.session.messages, system_prompt);
        if current_text.trim().is_empty() {
            return base;
        }
        // Arithmetic equivalent of pushing one more user message: `own`
        // un-inflated tokens (Text block rule, `len()/4` — same as the
        // estimator's per-message byte sum S) plus one framing increment.
        // The estimator inflates S by ceil(3/2) as a WHOLE, so
        // ceil((S+own)*3/2) − ceil(S*3/2) = floor(own*3/2) + 1 exactly when
        // S is even and own is odd; pinned exhaustively (80k pairs) and per
        // case by `context_pressure_delta_matches_clone_and_push_reference`.
        let sum: usize = self
            .session
            .messages
            .iter()
            .map(|m| {
                crate::compaction::estimate_tokens_for_message(
                    m,
                    crate::compaction::message_has_tool_use(m),
                )
            })
            .sum();
        let own = current_text.len() / 4;
        let mut inflated_delta = own * 3 / 2;
        if sum.is_multiple_of(2) && own % 2 == 1 {
            inflated_delta += 1;
        }
        base.saturating_add(inflated_delta).saturating_add(12)
    }

    fn append_resource_metadata_lines(
        &self,
        lines: &mut Vec<String>,
        current_text: &str,
        prompt_context: &NextTurnPromptContext,
        system_prompt: Option<&SystemPrompt>,
    ) {
        if let Some(line) = self.context_pressure_line(current_text, prompt_context, system_prompt)
        {
            lines.push(line);
        }
        if let Some(line) = self.active_goal_token_budget_line(prompt_context) {
            lines.push(line);
        }
    }

    /// Goal pacing for the model: the budget figure only, and only while a
    /// goal is actually active. Usage/time deltas, rates, and continuation
    /// counts are UI telemetry — they changed every turn and invalidated the
    /// prefix cache without adding model-steering signal.
    fn active_goal_token_budget_line(
        &self,
        prompt_context: &NextTurnPromptContext,
    ) -> Option<String> {
        let objective = prompt_context.goal_objective.as_deref()?;
        let snapshot = self.config.goal_state.lock().ok()?.snapshot();
        let same_goal =
            normalized_goal_objective(snapshot.objective.as_deref()).as_deref() == Some(objective);
        let token_budget = if same_goal {
            snapshot.token_budget
        } else {
            prompt_context.goal_token_budget
        }?;
        Some(format!("Active goal token budget: {token_budget}"))
    }

    async fn add_session_message(&mut self, mut message: Message) {
        self.redact_tool_results_for_transcript(&mut message);
        self.session.add_message(message);
        self.emit_session_updated().await;
    }

    /// Scrub credentials from tool output once, as it enters the transcript
    /// (B1). The transcript is what session JSON, the journal and every
    /// later request are built from, so a token a tool printed is never
    /// written to disk live. Honors the confirmed `[redaction] model_bound`
    /// opt-out the same way the request boundary does.
    fn redact_tool_results_for_transcript(&self, message: &mut Message) {
        for block in &mut message.content {
            let ContentBlock::ToolResult {
                content,
                content_blocks,
                ..
            } = block
            else {
                continue;
            };
            *content = self.redact_tool_output_for_transcript(content);
            for value in content_blocks.iter_mut().flatten() {
                if value.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    && let Some(serde_json::Value::String(text)) = value.get_mut("text")
                {
                    *text = self.redact_tool_output_for_transcript(text);
                }
            }
        }
    }

    fn redact_tool_output_for_transcript(&self, text: &str) -> String {
        match self.codewhale_client.as_ref() {
            Some(client) => client.redact_tool_output_for_transcript(text),
            None => codewhale_config::persistence::redact_model_bound_secrets(text),
        }
    }

    async fn add_interrupted_assistant_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let message = Message {
            role: Role::InterruptedAssistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        };
        let already_committed = self.session.messages.last().is_some_and(|last| {
            matches!(
                last.role.as_str(),
                "assistant" | codewhale_models::INTERRUPTED_ASSISTANT_ROLE
            ) && last.content == message.content
        });
        if already_committed {
            return;
        }
        self.add_session_message(message).await;
    }

    #[allow(clippy::too_many_arguments)]
    fn turn_metadata_block(
        &self,
        routed_model: &str,
        auto_model: bool,
        reasoning_effort: Option<&str>,
        reasoning_effort_auto: bool,
        provenance: UserInputProvenance,
        current_text: &str,
        policy_narrowing: Option<&PolicyNarrowingEvent>,
    ) -> ContentBlock {
        let prompt_context = self.installed_next_turn_prompt_context();
        self.turn_metadata_block_from_snapshot(
            routed_model,
            auto_model,
            reasoning_effort,
            reasoning_effort_auto,
            provenance,
            current_text,
            TurnMetadataSnapshot {
                prompt_context: &prompt_context,
                system_prompt: self.session.system_prompt.as_ref(),
                approval_mode: self.session.approval_mode,
                working_set: &self.session.working_set,
                policy_narrowing,
            },
        )
    }

    /// Build `<turn_meta>` from an explicit snapshot of the session state a
    /// turn installs *before* it writes the block.
    ///
    /// Production installs approval posture, policy narrowing, and the
    /// observed working set on `self`, then reads them back here.
    /// `/preview-request` cannot install any of that — it describes a turn
    /// that has not started — so it passes the values it would have installed,
    /// including a *clone* of the working set with the hypothetical message
    /// already observed. That is what makes the previewed block byte-identical
    /// to the real one without a single write.
    #[allow(clippy::too_many_arguments)]
    fn turn_metadata_block_from_snapshot(
        &self,
        _routed_model: &str,
        _auto_model: bool,
        _reasoning_effort: Option<&str>,
        _reasoning_effort_auto: bool,
        provenance: UserInputProvenance,
        current_text: &str,
        snapshot: TurnMetadataSnapshot<'_>,
    ) -> ContentBlock {
        let TurnMetadataSnapshot {
            prompt_context,
            system_prompt,
            approval_mode,
            working_set,
            policy_narrowing,
        } = snapshot;
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let working_set_summary = working_set
            .summary_block(&self.config.workspace)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Facts only (#4780 + turn-meta diet). Mode behavior lives in runtime
        // policy and the tool catalog, not prose. Preserve the compact
        // permission label so the model can distinguish Ask, Auto-Review, Full
        // Access, and Never without repeating question-discipline prose.
        // Route/effort/model lines are telemetry the model cannot act on.
        // DGF-02 (dogfood 2026-08-02): the model was never told its own
        // sandbox posture, so an approved-then-sandbox-blocked write read as
        // a mystery failure it burned turns "debugging". Derive the posture
        // from the same resolver tool execution uses. The execution boundary
        // is snapshotted at engine construction: local OS wrapper, configured
        // external backend, or unavailable. External raw-command backends do
        // not inherit local workspace/network enforcement claims. Stable per
        // session, so ordinary turns stay byte-identical.
        let sandbox_posture = crate::core::authority::sandbox_policy_for_turn(
            prompt_context.mode,
            approval_mode,
            self.api_config.sandbox_mode.as_deref(),
            &self.config.workspace,
            crate::core::authority::SandboxNetworkAccess::from_config(
                self.api_config.sandbox_network_access,
            ),
        );
        let mut lines = vec![
            format!("Current local date: {today}"),
            // Workspace path moved here from the static `## Environment` block so
            // the static system prefix stays byte-stable across sessions (see
            // `render_environment_block` for the prefix-cache rationale).
            format!("Current workspace: {}", self.config.workspace.display()),
            format!(
                "{PERMISSION_POSTURE_LINE}{}",
                approval_mode.permission_chip_label()
            ),
            format!(
                "Current sandbox posture: {}",
                sandbox_posture.posture_label_with_enforcement_and_no_new_privs(
                    self.sandbox_enforcement,
                    self.no_new_privs_active,
                )
            ),
        ];
        if approval_mode == ApprovalMode::Never {
            lines.push(
                "Approval prompts are disabled; do not request escalation for this turn."
                    .to_string(),
            );
        }
        // On ordinary external turns the user's own message is authoritative by
        // construction, so provenance is redundant. On non-external turns
        // (sub-agent handoff, runtime events) the *reduced* authority is the
        // sole signal, so surface it as one condensed line.
        if !provenance.can_authorize_work() {
            lines.push(format!(
                "Input provenance: {} (non-authoritative)",
                provenance.as_str()
            ));
        }
        // #3947: when runtime policy narrowed this turn's authority, the model
        // learns that it happened, why, and the exact sentence the user saw.
        // Emitted only on a narrowed turn, so the ordinary turn's metadata
        // stays byte-stable.
        if let Some(event) = policy_narrowing {
            lines.push(format!("Authority narrowing: {}", event.reason().as_str()));
            lines.push(format!("Authority transition: {}", event.transition()));
            lines.push(format!("Authority narrowing status: {}", event.message()));
        }
        self.append_resource_metadata_lines(
            &mut lines,
            current_text,
            prompt_context,
            system_prompt,
        );
        if let Some(working_set_summary) = working_set_summary {
            lines.push(working_set_summary);
        }
        // #5187 (k3-gap F3): the git snapshot re-collects branch/dirty state
        // every turn, so the line's bytes changed after every edit the model
        // itself made — churning the block and priming caution each turn.
        // Emit it only when the snapshot actually changed since the last
        // emitted block; the model can always run `git status` for a fresh
        // read.
        if let Some(git_snapshot) = crate::git_status::collect(&self.config.workspace) {
            let mut last = self
                .last_turn_meta_git_snapshot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if last.as_deref() != Some(git_snapshot.as_str()) {
                *last = Some(git_snapshot.clone());
                lines.push(format!("Git workspace: {git_snapshot}"));
            }
        }
        let summary = lines.join("\n");

        ContentBlock::Text {
            text: format!("<turn_meta>\n{summary}\n</turn_meta>"),
            cache_control: None,
        }
    }

    /// Assemble the content blocks of a user turn.
    ///
    /// The text comes first and the turn metadata last — both positions are
    /// load-bearing for prompt caching (see
    /// [`Self::turn_metadata_block`]), so resolved images are inserted between
    /// them rather than at either end.
    ///
    /// The composer stores an attachment as a `[Attached image: …]` text line
    /// and the bytes are read here, once, as the message is built. That keeps
    /// multi-megabyte payloads out of the composer and undo history, and it
    /// means deleting the line deletes the attachment for free. Anything that
    /// cannot be attached becomes a visible notice instead of vanishing.
    ///
    /// Whether the model can *see* the result is decided per request, not
    /// here — see `image_attach::strip_images_when_unsupported`.
    fn user_content_blocks(&self, text: String) -> Vec<ContentBlock> {
        // Managed Chat accepts validated inline bytes, never host paths. Treat
        // attachment-marker syntax as an omitted attachment so an account prompt can never make
        // this host read a local path or echo that host path to a provider.
        if self.api_config.runtime_chat_isolated {
            return vec![ContentBlock::Text {
                text: sanitize_isolated_chat_attachments(text),
                cache_control: None,
            }];
        }
        let expanded = crate::image_attach::expand_attachment_blocks(&text);
        let mut content = Vec::with_capacity(2 + expanded.blocks.len());
        content.push(ContentBlock::Text {
            text,
            cache_control: None,
        });
        content.extend(expanded.blocks);
        if let Some(notice) = crate::image_attach::notice_block(&expanded.notices) {
            content.push(notice);
        }
        content
    }

    /// The user message a turn would build, from an explicit state snapshot.
    ///
    /// Same block order and same constructor as
    /// [`Self::user_text_message_with_turn_metadata_for_route_and_provenance`];
    /// only the source of the turn-metadata inputs differs. See
    /// [`Self::turn_metadata_block_from_snapshot`].
    #[allow(clippy::too_many_arguments)]
    pub(super) fn user_text_message_from_snapshot(
        &self,
        text: String,
        routed_model: &str,
        auto_model: bool,
        reasoning_effort: Option<&str>,
        reasoning_effort_auto: bool,
        provenance: UserInputProvenance,
        snapshot: TurnMetadataSnapshot<'_>,
    ) -> Message {
        let turn_metadata = (!self.api_config.runtime_chat_isolated).then(|| {
            self.turn_metadata_block_from_snapshot(
                routed_model,
                auto_model,
                reasoning_effort,
                reasoning_effort_auto,
                provenance,
                &text,
                snapshot,
            )
        });
        let mut content = self.user_content_blocks(text);
        if let Some(turn_metadata) = turn_metadata {
            content.push(turn_metadata);
        }
        Message {
            role: Role::User,
            content,
        }
    }

    fn user_text_message_with_turn_metadata(&self, text: String) -> Message {
        self.user_text_message_with_turn_metadata_for_route(
            text,
            &self.session.model,
            self.session.auto_model,
            self.session.reasoning_effort.as_deref(),
            self.session.reasoning_effort_auto,
        )
    }

    fn user_text_message_with_turn_metadata_for_route(
        &self,
        text: String,
        routed_model: &str,
        auto_model: bool,
        reasoning_effort: Option<&str>,
        reasoning_effort_auto: bool,
    ) -> Message {
        self.user_text_message_with_turn_metadata_for_route_and_provenance(
            text,
            routed_model,
            auto_model,
            reasoning_effort,
            reasoning_effort_auto,
            UserInputProvenance::ExternalUser,
        )
    }

    fn runtime_text_message_with_turn_metadata(
        &self,
        text: String,
        provenance: UserInputProvenance,
    ) -> Message {
        self.user_text_message_with_turn_metadata_for_route_and_provenance(
            text,
            &self.session.model,
            self.session.auto_model,
            self.session.reasoning_effort.as_deref(),
            self.session.reasoning_effort_auto,
            provenance,
        )
    }

    fn user_text_message_with_turn_metadata_for_route_and_provenance(
        &self,
        text: String,
        routed_model: &str,
        auto_model: bool,
        reasoning_effort: Option<&str>,
        reasoning_effort_auto: bool,
        provenance: UserInputProvenance,
    ) -> Message {
        // Place the user text first and turn_meta last so that the leading
        // bytes of each user message stay stable across date / model-route /
        // working-set changes. DeepSeek's KV prefix cache matches byte
        // sequences from the start of each message; when turn_meta (which
        // contains the current date) sits at position 0 the entire user
        // message prefix is invalidated at every date boundary. Moving it
        // to the tail preserves the user-input prefix and limits cache
        // invalidation to the trailing metadata block.
        let turn_metadata = (!self.api_config.runtime_chat_isolated).then(|| {
            self.turn_metadata_block(
                routed_model,
                auto_model,
                reasoning_effort,
                reasoning_effort_auto,
                provenance,
                &text,
                self.last_policy_narrowing.as_ref(),
            )
        });
        let mut content = self.user_content_blocks(text);
        if let Some(turn_metadata) = turn_metadata {
            content.push(turn_metadata);
        }
        Message {
            role: Role::User,
            content,
        }
    }

    async fn handle_idle_subagent_completion(&mut self, first: SubAgentCompletion) {
        // Cancellation can race the idle receive, just as it can race a
        // background-shell wake. Keep the receipt queued for the next explicit
        // turn; canceled workers must not restart their interrupted parent.
        if self.cancel_token.is_cancelled() {
            let _ = self.tx_subagent_completion.try_send(first);
            return;
        }
        let mut completions = Vec::new();
        if let Some(completion) = claim_subagent_completion_for_session(
            &mut self.delivered_subagent_completion_ids,
            &self.session.id,
            first,
        ) {
            completions.push(completion);
        }
        while let Ok(completion) = self.rx_subagent_completion.try_recv() {
            if let Some(completion) = claim_subagent_completion_for_session(
                &mut self.delivered_subagent_completion_ids,
                &self.session.id,
                completion,
            ) {
                completions.push(completion);
            }
        }

        if completions.is_empty() {
            return;
        }

        let claimed_ids = completions
            .iter()
            .map(|completion| completion.agent_id.clone())
            .collect::<Vec<_>>();
        let route = match self.current_runtime_route() {
            Ok(route) => route,
            Err(err) => {
                for agent_id in claimed_ids {
                    self.delivered_subagent_completion_ids.remove(&agent_id);
                }
                let _ = self.send_event(Event::error(ErrorEnvelope::fatal_auth(format!(
                        "Cannot resume the turn because its provider route is no longer valid: {err}"
                    ))))
                    .await;
                let outcome = SendMessageOutcome::NotStarted {
                    error: Some(format!("provider route is no longer valid: {err}")),
                };
                self.reconcile_non_completed_goal_turn(&outcome).await;
                return;
            }
        };

        let count = completions.len();
        let content = completions
            .iter()
            .map(|completion| {
                if completion.is_high_priority_failure() {
                    crate::runtime_handoff::subagent_failure_runtime_text(&completion.payload)
                } else {
                    crate::runtime_handoff::subagent_completion_runtime_text(&completion.payload)
                }
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let failed = completions
            .iter()
            .filter(|completion| completion.is_high_priority_failure())
            .count();
        let failure_suffix = if failed == 0 {
            String::new()
        } else {
            format!(" ({failed} failed)")
        };

        let _ = self
            .send_event(Event::status(format!(
                "Resuming turn with {count} idle sub-agent completion(s){failure_suffix}"
            )))
            .await;

        let outcome = self
            .handle_send_message(TurnSpec {
                profile_constitution: None,
                content,
                mode: self.current_mode,
                route: Box::new(route),
                compaction: Box::new(self.config.compaction.clone()),
                initial_routed_usage: Box::new(crate::cost_status::RuntimeUsageBatch::default()),
                goal_objective: self.config.goal_objective.clone(),
                goal_token_budget: self.config.goal_token_budget,
                goal_status: self.config.goal_status,
                reasoning_effort: self.session.reasoning_effort.clone(),
                reasoning_effort_auto: self.session.reasoning_effort_auto,
                auto_model: self.session.auto_model,
                allow_shell: self.session.allow_shell,
                trust_mode: self.session.trust_mode,
                auto_approve: self.session.auto_approve,
                approval_mode: self.session.approval_mode,
                translation_enabled: self.config.translation_enabled,
                allowed_tools: self.config.allowed_tools.clone(),
                dynamic_tools: Vec::new(),
                hook_executor: self.config.hook_executor.clone(),
                verbosity: self.config.verbosity.clone(),
                provenance: UserInputProvenance::SubAgentHandoff,
                images: Vec::new(),
                max_output_tokens: None,
                // Idle sub-agent completion resume: no host submission to
                // correlate with.
                submission_id: None,
            })
            .await;
        if !outcome.started() {
            for agent_id in claimed_ids {
                self.delivered_subagent_completion_ids.remove(&agent_id);
            }
            if self.cancel_token.is_cancelled() {
                // Admission lost to cancellation before the transcript took
                // ownership. Leave these receipts for the next explicit turn.
                for completion in completions {
                    let _ = self.tx_subagent_completion.try_send(completion);
                }
            }
        }
    }

    /// Handle a send message operation
    #[allow(clippy::too_many_arguments)]
    /// After a turn completes, decide whether an active goal should keep going.
    /// Returns a continuation to dispatch, an explicit terminal backstop stop,
    /// or Inactive when no follow-up turn belongs in the queue.
    ///
    /// A goal runs until the model self-reports done/blocked or the user pauses
    /// or clears. Token/time accounting remains telemetry. The loop is "until
    /// done," not "until N turns" (#5052); a configurable safety
    /// backstop (`[goal] max_continuations`, `0` = unlimited) still halts a
    /// pathological loop that never emits a terminal signal.
    fn goal_continuation_if_active(&self) -> GoalContinuationAction {
        // ACP admits one finite prompt turn; it has no autonomous-goal lifecycle.
        // Leave the existing goal record intact for its owning frontend.
        if self.is_acp_turn() || self.child_host.is_some() || self.rlm_host.is_some() {
            return GoalContinuationAction::Inactive;
        }
        let mut state = match self.config.goal_state.lock() {
            Ok(state) => state,
            Err(err) => {
                tracing::warn!("goal state lock poisoned during continuation check: {err}");
                return GoalContinuationAction::Inactive;
            }
        };
        let snapshot = state.snapshot();
        if !snapshot.is_active() {
            return GoalContinuationAction::Inactive;
        }

        // The snapshot status is a string ("active", "paused", "complete",
        // "blocked"). Map it to the goal-loop decision core's status enum.
        let status = match snapshot.status.as_str() {
            "active" => crate::goal_loop::GoalRunStatus::Active,
            "complete" => crate::goal_loop::GoalRunStatus::Completed,
            // Paused / Blocked / unknown → no continuation.
            _ => return GoalContinuationAction::Inactive,
        };

        let decision = crate::goal_loop::decide_continuation(
            status,
            crate::goal_loop::GoalProgress {
                tokens_used: snapshot.tokens_used,
                time_used_seconds: snapshot.time_used_seconds,
                continuations: snapshot.continuation_count,
            },
            // Unbounded like grokbuild (agent-call cap) and kimicode swarm
            // (turnBudget per-task, resumable): token/time are telemetry only
            // unless `[goal] enforce_token_budget` opts a set budget into a
            // hard stop (#6013); otherwise only Completed/Blocked/
            // ContinuationLimit pause the loop. The goal's own budget must be
            // carried here: `unbounded()` has none, which left the enforced
            // stop unreachable on this cross-turn gate (T08-03).
            crate::goal_loop::GoalBudget {
                token_budget: snapshot.token_budget.map(u64::from),
                time_budget_seconds: None,
                enforce_token_budget: self.config.goal_enforce_token_budget,
                max_continuations: self.config.goal_max_continuations,
            },
        );

        match decision {
            crate::goal_loop::ContinuationDecision::Continue => {
                // A cross-turn dispatch is a real continuation pass just like
                // the bounded intra-turn retry in `turn_loop`. Record it before
                // rendering and carrying the snapshot so the durable prompt,
                // telemetry, and next host sync all agree on the pass number.
                state.record_continuation();
                let snapshot = state.snapshot();
                GoalContinuationAction::Dispatch {
                    content: crate::tools::goal::render_continuation_prompt(
                        &snapshot,
                        snapshot.continuation_count,
                    ),
                    snapshot: Box::new(snapshot),
                }
            }
            crate::goal_loop::ContinuationDecision::Stop(reason) => {
                tracing::info!(?reason, "goal continuation stopped");
                let (message, pause_reason) = match reason {
                    crate::goal_loop::StopReason::ContinuationLimit => (
                        format!(
                            "Goal paused after {} automatic continuations without a terminal result (safety backstop; raise or disable via [goal] max_continuations); inspect progress, then resume if useful.",
                            self.config.goal_max_continuations,
                        ),
                        GoalPauseReason::Backoff,
                    ),
                    crate::goal_loop::StopReason::BudgetLimit => (
                        "Goal paused: the goal's token budget was reached and \
                         [goal] enforce_token_budget makes that a hard stop; \
                         raise the budget or resume to continue."
                            .to_string(),
                        GoalPauseReason::BudgetLimit,
                    ),
                    crate::goal_loop::StopReason::Completed
                    | crate::goal_loop::StopReason::Blocked => {
                        return GoalContinuationAction::Inactive;
                    }
                };
                GoalContinuationAction::Stopped {
                    message,
                    reason: pause_reason,
                }
            }
        }
    }

    /// Reject an edit operation before model dispatch while still completing
    /// the submitted host lifecycle. `Event::Error` is advisory to embedded
    /// hosts; `TurnComplete(Failed)` is the authoritative terminal signal that
    /// releases their busy state and closes the admitted operation.
    async fn reject_edit_last_turn(&mut self, envelope: ErrorEnvelope) {
        let message = envelope.message.clone();
        let _ = self.send_event(Event::error(envelope)).await;
        let _ = self
            .send_event(Event::TurnComplete {
                usage: Usage::default(),
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: TurnOutcomeStatus::Failed,
                error: Some(message.clone()),
                tool_catalog: None,
                base_url: None,
            })
            .await;
        let outcome = SendMessageOutcome::NotStarted {
            error: Some(message),
        };
        self.reconcile_non_completed_goal_turn(&outcome).await;
    }

    /// Reconcile a turn that did not complete with the autonomous goal loop.
    /// Hosted engines leave lifecycle decisions to their durable host. The
    /// interactive engine must cancel any older queued synthetic token first,
    /// then project an active goal into a truthful non-running state.
    async fn reconcile_non_completed_goal_turn(&mut self, outcome: &SendMessageOutcome) {
        if self.host_managed_turns() {
            return;
        }

        self.cancel_scheduled_goal_continuation(false).await;
        match outcome {
            SendMessageOutcome::NotStarted { error } => {
                let message = self.goal_turn_not_started_message(error.as_deref());
                self.block_goal_continuation(message).await;
            }
            SendMessageOutcome::Finished {
                status: TurnOutcomeStatus::Failed,
                error,
            } => {
                let message = self.goal_continuation_failure_message(error.as_deref());
                self.block_goal_continuation(message).await;
            }
            SendMessageOutcome::Finished {
                status: TurnOutcomeStatus::Interrupted,
                ..
            } => {
                // Goals are durable session objectives. An interrupted model
                // turn (Esc, steer, compaction, cancel) must cancel only the
                // auto-continuation timer — already done above — and leave the
                // goal Active. pause_reason=User is reserved for explicit
                // `/goal pause`. Requiring `/goal resume` after every interrupt
                // was a dogfood lie (2026-07-24).
                let message = if self
                    .goal_snapshot_for_event()
                    .is_some_and(|goal| goal.is_active())
                {
                    "Turn interrupted; session goal stays active."
                } else {
                    "Turn interrupted."
                };
                let _ = self.send_event(Event::status(message.to_string())).await;
            }
            SendMessageOutcome::Finished {
                status: TurnOutcomeStatus::Completed,
                ..
            } => {}
        }
    }

    /// A route/client rejection can happen before normal turn setup copies the
    /// host's just-declared goal into SharedGoalState. Seed only that goal
    /// descriptor so the rejection can publish a truthful Blocked snapshot;
    /// no user message or provider turn state is mutated here.
    fn sync_unstarted_goal_for_terminal_projection(
        &mut self,
        objective: Option<&str>,
        token_budget: Option<u32>,
        status: GoalStatus,
    ) {
        let objective = normalized_goal_objective(objective);
        if objective.is_none() || status != GoalStatus::Active {
            return;
        }
        sync_goal_state_from_host(
            &self.config.goal_state,
            objective.as_deref(),
            token_budget,
            status,
        );
        self.config.goal_objective = objective;
        self.config.goal_token_budget = token_budget;
        self.config.goal_status = status;
    }

    /// Transition a still-active interactive goal to Blocked and publish every
    /// host projection in one ordered path. Continuation failures happen
    /// outside a model tool call, so without this bridge the loop can stop while
    /// the prompt and sidebar continue to claim the goal is actively running.
    async fn block_goal_continuation(&mut self, message: String) {
        let snapshot = match self.config.goal_state.lock() {
            Ok(mut state) => {
                if state.is_active()
                    && let Err(err) = state.mark_runtime_blocked(message.clone())
                {
                    tracing::warn!("failed to mark goal continuation blocked: {err}");
                    return;
                }
                let snapshot = state.snapshot();
                if snapshot.status != GoalStatus::Blocked.as_str() {
                    // Not an ordering bug: only an Active goal is moved to
                    // Blocked above, so reaching here means there was no
                    // active goal to block — most often no goal at all
                    // (`status=none`) on an ordinary turn that failed, or one
                    // the user paused or completed during the turn. The
                    // turn's own failure already reached the host through
                    // `TurnComplete`; there is nothing goal-side to publish.
                    tracing::debug!(
                        status = %snapshot.status,
                        "no active goal to block after a non-completed turn"
                    );
                    return;
                }
                snapshot
            }
            Err(err) => {
                tracing::warn!("goal state lock poisoned while blocking continuation: {err}");
                return;
            }
        };

        self.config.goal_objective.clone_from(&snapshot.objective);
        self.config.goal_token_budget = snapshot.token_budget;
        self.config.goal_status = GoalStatus::Blocked;
        self.refresh_system_prompt_with_reason("goal");
        self.emit_session_updated().await;
        let _ = self.send_event(Event::GoalUpdated { snapshot }).await;
        let _ = self.send_event(Event::status(message)).await;
    }

    /// Resume the shared goal when its only blocker was a runtime stop and it
    /// is the objective this turn names; publish the change like any other
    /// goal transition.
    async fn resume_runtime_blocked_goal(&mut self, objective: Option<&str>) -> bool {
        let snapshot = match self.config.goal_state.lock() {
            Ok(mut state) => {
                if normalized_goal_objective(state.objective())
                    != normalized_goal_objective(objective)
                    || !state.resume_after_runtime_block()
                {
                    return false;
                }
                state.snapshot()
            }
            Err(err) => {
                tracing::warn!("goal state lock poisoned while resuming a goal: {err}");
                return false;
            }
        };
        self.config.goal_status = GoalStatus::Active;
        self.emit_session_updated().await;
        let _ = self.send_event(Event::GoalUpdated { snapshot }).await;
        let _ = self
            .send_event(Event::status(
                "Goal resumed: your message continues the work the earlier turn stopped",
            ))
            .await;
        true
    }

    /// Resume a goal the model handed back at a milestone, when it is the
    /// objective this turn names; publish the change like any other goal
    /// transition. A yield is a hand-back rather than a judgement about the
    /// work, so answering it continues the work.
    async fn resume_yielded_goal(&mut self, objective: Option<&str>) -> bool {
        let snapshot = match self.config.goal_state.lock() {
            Ok(mut state) => {
                if normalized_goal_objective(state.objective())
                    != normalized_goal_objective(objective)
                    || !state.resume_after_yield()
                {
                    return false;
                }
                state.snapshot()
            }
            Err(err) => {
                tracing::warn!("goal state lock poisoned while resuming a yielded goal: {err}");
                return false;
            }
        };
        self.config.goal_status = GoalStatus::Active;
        self.emit_session_updated().await;
        let _ = self.send_event(Event::GoalUpdated { snapshot }).await;
        let _ = self
            .send_event(Event::status(
                "Goal resumed: your message continues the work the earlier turn handed back",
            ))
            .await;
        true
    }

    /// Pause a still-active goal with an inspectable reason and publish every
    /// host projection in one ordered path.
    async fn pause_goal_continuation(&mut self, reason: GoalPauseReason, message: String) {
        let snapshot = match self.config.goal_state.lock() {
            Ok(mut state) => {
                if !state.is_active() {
                    return;
                }
                if let Err(err) = state.mark_paused(reason) {
                    tracing::warn!("failed to pause goal continuation: {err}");
                    return;
                }
                state.snapshot()
            }
            Err(err) => {
                tracing::warn!("goal state lock poisoned while pausing interruption: {err}");
                return;
            }
        };

        self.config.goal_objective.clone_from(&snapshot.objective);
        self.config.goal_token_budget = snapshot.token_budget;
        self.config.goal_status = GoalStatus::Paused;
        self.refresh_system_prompt_with_reason("goal");
        self.emit_session_updated().await;
        let _ = self.send_event(Event::GoalUpdated { snapshot }).await;
        let _ = self.send_event(Event::status(message)).await;
    }

    /// Handle `/goal pause|resume|clear|complete|blocked` by writing the new
    /// status to `SharedGoalState` so the cross-turn continuation loop respects
    /// it. This does NOT dispatch a model turn — it's a control-plane update.
    async fn handle_set_goal_status(
        &mut self,
        status: GoalStatus,
        clear: bool,
        goal_id: Option<String>,
    ) {
        if clear || status != GoalStatus::Active {
            self.cancel_scheduled_goal_continuation(true).await;
        }
        // A continuation is scheduled only on a real transition INTO Active
        // from a non-active state (paused/blocked resume). Re-asserting
        // Active on an already-active goal must not stack a second
        // autonomous turn on top of the loop that is already running.
        let was_active = self
            .config
            .goal_state
            .lock()
            .map(|state| state.is_active())
            .unwrap_or(false);
        let snapshot = match self.config.goal_state.lock() {
            Ok(mut state) => {
                if clear {
                    // `/goal clear` — wipe the objective entirely.
                    state.sync_from_host_status(None, None, GoalStatus::Active);
                } else {
                    // Update only the status; keep the objective and budget.
                    // `sync_from_host_status` resets usage when the objective
                    // changes, but here we pass the existing objective so usage
                    // is preserved (pause/resume shouldn't reset the counter).
                    let objective = state.objective().map(str::to_string);
                    let budget = state.token_budget();
                    if status == GoalStatus::Active {
                        state.resume(goal_id);
                    } else {
                        state.sync_from_host_status(objective.as_deref(), budget, status);
                    }
                }
                state.snapshot()
            }
            Err(err) => {
                tracing::warn!("goal state lock poisoned during SetGoalStatus: {err}");
                return;
            }
        };

        // Keep every host-side projection aligned with the authoritative
        // SharedGoalState. In particular, a cleared state must also clear the
        // configured fallback used by `goal_objective_for_prompt`; otherwise a
        // prompt refresh would silently restore the old <session_goal> block.
        self.config.goal_objective.clone_from(&snapshot.objective);
        self.config.goal_token_budget = snapshot.token_budget;
        self.config.goal_status = if snapshot.objective.is_some() {
            status
        } else {
            GoalStatus::Active
        };
        self.refresh_system_prompt_with_reason("goal");
        self.emit_session_updated().await;
        // Unlike routine end-of-turn updates, an explicit clear must publish
        // the canonical empty snapshot. Keeping this scoped to the control op
        // avoids an unrelated no-goal turn racing with a newly declared goal in
        // the UI while still letting the clear win over a preceding active
        // TurnComplete snapshot.
        let snapshot_has_objective = snapshot.objective.is_some();
        let _ = self.send_event(Event::GoalUpdated { snapshot }).await;

        let label = if clear {
            "cleared"
        } else {
            match status {
                GoalStatus::Active => "resumed",
                GoalStatus::Paused => "paused",
                GoalStatus::Complete => "complete",
                GoalStatus::Blocked => "blocked",
            }
        };
        let _ = self
            .send_event(Event::status(format!("Goal {label}.")))
            .await;

        // Resuming an objective-bearing goal restarts the runtime's own
        // steering loop — the kickoff is a continuation turn, never a raw
        // user message echoing the objective (codex `/goal resume` parity).
        let resumed_into_active = !clear && status == GoalStatus::Active && !was_active;
        if resumed_into_active && snapshot_has_objective {
            self.schedule_goal_continuation(Vec::new()).await;
        }
    }

    /// `/goal <objective>` — control-plane goal set (codex `/goal` parity).
    /// The engine is authoritative: the objective lands in
    /// `SharedGoalState`, every host projection is refreshed, `GoalUpdated`
    /// publishes the new snapshot, and the first goal turn is dispatched as
    /// runtime steering (the continuation prompt built from the goal
    /// snapshot). The objective is never echoed as a raw user message.
    async fn handle_set_goal_objective(
        &mut self,
        objective: String,
        token_budget: Option<u32>,
        goal_id: Option<String>,
    ) {
        let Some(objective) = normalized_goal_objective(Some(&objective)) else {
            let _ = self
                .send_event(Event::status(
                    "Goal not set: the objective is empty after trimming.".to_string(),
                ))
                .await;
            return;
        };
        match self.config.goal_state.lock() {
            Ok(mut state) => state.replace(&objective, token_budget, goal_id),
            Err(error) => {
                tracing::warn!("goal state lock poisoned during replacement: {error}");
                return;
            }
        }
        self.config.goal_objective = Some(objective);
        self.config.goal_token_budget = token_budget;
        self.config.goal_status = GoalStatus::Active;
        self.refresh_system_prompt_with_reason("goal");
        self.emit_session_updated().await;
        let snapshot = match self.config.goal_state.lock() {
            Ok(state) => state.snapshot(),
            Err(err) => {
                tracing::warn!("goal state lock poisoned during SetGoalObjective: {err}");
                return;
            }
        };
        let _ = self.send_event(Event::GoalUpdated { snapshot }).await;
        let _ = self
            .send_event(Event::status("Goal set; starting goal work.".to_string()))
            .await;
        self.schedule_goal_continuation(Vec::new()).await;
    }

    /// Build the turn's tool registry and the model-facing tool catalog.
    ///
    /// This is the single authority for "what tools would the next request
    /// carry". `handle_send_message` calls it with [`SubAgentWiring::Live`]
    /// and [`McpAccess::Connect`]; `/preview-request` calls it with
    /// [`SubAgentWiring::Inert`] and [`McpAccess::PassiveSnapshot`], which
    /// together remove every side effect of the build — no fork snapshot, no
    /// spawned mailbox drainer, no pool creation, no `connect_all`, no status
    /// events — while producing a byte-identical catalog for the state that
    /// is already live.
    ///
    /// The session's `last_tool_catalog` is never an acceptable substitute:
    /// it is one turn stale and stores the pre-activation catalog rather than
    /// the active subset the provider would actually receive.
    ///
    /// `allowed_tools` is the command-scoped allow-list gate the catalog is
    /// filtered under. It is an explicit **parameter**, not a read of
    /// `self.config.allowed_tools`, because the preview's gate belongs to a
    /// turn that has not been installed: writing it onto the engine and
    /// restoring it afterwards would leave the wrong gate installed across
    /// every `.await` in this function, and would leave it installed
    /// permanently if the task were cancelled or panicked between the two
    /// writes.
    #[allow(clippy::too_many_arguments)]
    async fn build_turn_tool_registry_and_catalog(
        &mut self,
        input_policy: &TurnAuthority,
        dynamic_tools: &[DynamicToolSpec],
        allowed_tools: Option<Vec<String>>,
        wiring: SubAgentWiring,
        mcp_access: McpAccess,
        route: TurnRouteContext,
        turn_id: &str,
    ) -> TurnToolBuild {
        // Account-owned Chat is a text-only inference boundary. Do not build
        // native/plugin/dynamic registries, connect or snapshot MCP, capture a
        // sub-agent fork context, or create a sub-agent mailbox before an
        // empty allow-list later filters the wire catalog.
        if self.api_config.runtime_chat_isolated {
            let registry = ToolRegistryBuilder::new().build(ToolContext::for_empty_registry());
            return TurnToolBuild {
                surface: ToolSurfacePolicy::new(
                    registry,
                    Some(Vec::new()),
                    input_policy.mode,
                    &HashSet::new(),
                    &[],
                    false,
                    Some(Vec::new()),
                    None,
                    Some(0),
                    tool_catalog::ToolMode::Direct,
                ),
                mcp_tool_names: Vec::new(),
                mcp: McpToolState::Disabled,
                subagent_runtime_model: None,
                mailbox: None,
                plugin_tool_names: HashSet::new(),
            };
        }
        if self.is_acp_turn() {
            return self.acp_tool_build(input_policy, &route, allowed_tools);
        }
        if self.rlm_host.is_some() {
            return self.rlm_tool_build(input_policy, &route);
        }
        // Build tool registry and tool list for the current mode
        let todo_list = self.config.todos.clone();
        let plan_state = self.config.plan_state.clone();

        let mut tool_context = self.build_tool_context_for_turn(input_policy, &route);
        if wiring.is_live() {
            tool_context.rlm_caller = rlm_host::CapturedRlmCaller::capture(
                self,
                input_policy,
                &route,
                &tool_context,
                turn_id,
            )
            .ok()
            .map(Arc::new);
        }
        // Ensure MCP pool is initialized before building the tool registry,
        // so start_mcp_server can be registered when Feature::Mcp is enabled.
        // A passive snapshot must not create the pool: allocating it is engine
        // state a preview has no business writing.
        if self.config.features.enabled(Feature::Mcp) && mcp_access.may_connect() {
            let _ = self.ensure_mcp_pool().await;
            self.wait_for_explicit_mcp_boot(allowed_tools.as_deref())
                .await;
        }
        let builder = if let Some(child) = self.child_host.as_ref() {
            let mut runtime = child.authority.nested_runtime(
                tool_context.clone(),
                self.tx_subagent_completion.clone(),
                SubAgentForkContext {
                    live_header: crate::prompt_zones::new_live_header_cell(),
                    messages: self.messages_with_turn_metadata(),
                    structured_state_block: None,
                    work_source: Some(self.todo_source()),
                },
            );
            if let Some(client) = route.client.as_ref() {
                runtime.client = client.clone();
            }
            runtime.model = route.model.clone();
            runtime.api_config = Some(Arc::new(*route.api_config.clone()));
            runtime.reasoning_effort = route.reasoning_effort.clone();
            runtime.reasoning_effort_auto = route.reasoning_effort_auto;
            child
                .authority
                .tool_registry_builder(
                    runtime,
                    self.config.todos.clone(),
                    self.config.plan_state.clone(),
                )
                .with_dynamic_tools(dynamic_tools)
        } else {
            self.build_turn_tool_registry_builder_for_route(
                input_policy.mode,
                input_policy.allow_shell,
                route.client.clone(),
                &route.model,
                todo_list,
                plan_state,
            )
            .with_dynamic_tools(dynamic_tools)
        };

        let subagents_available =
            self.config.subagents_enabled && self.config.features.enabled(Feature::Subagents);

        let fork_context_for_runtime = if subagents_available && wiring.is_live() {
            let state = StructuredState::capture(
                input_policy.mode.label(),
                self.config.workspace.clone(),
                std::env::current_dir().ok(),
                &self.session.working_set,
                Some(&self.subagent_manager),
                &self.session.id,
            )
            .await;
            Some(SubAgentForkContext {
                live_header: Arc::clone(&self.live_header),
                messages: self.messages_with_turn_metadata(),
                structured_state_block: state.to_system_block(),
                // Resolve at spawn time so a todo_write earlier in this turn
                // reaches the child rather than freezing turn-start state.
                work_source: Some(self.todo_source()),
            })
        } else {
            None
        };

        // Mailbox for structured sub-agent envelopes (#128/#130). One per
        // turn: the receiver is drained by a short-lived task that converts
        // envelopes into `Event::SubAgentMailbox` so the UI can route them
        // to the matching in-transcript card. The drainer exits naturally
        // when every cloned sender is dropped at turn-end.
        let mailbox_for_runtime =
            if subagents_available && wiring.is_live() && self.child_host.is_none() {
                let cancel_token = self.cancel_token.child_token();
                let foreground_children = Arc::new(ForegroundChildRegistry::new());
                let (mailbox, mut receiver) = Mailbox::new(cancel_token.clone());
                let tx_event_clone = self.tx_event.clone();
                let mailbox_owner_session_id = self.session.id.clone();
                let mailbox_turn_id = turn_id.to_string();
                let (flush_tx, mut flush_rx) = tokio::sync::oneshot::channel();
                let drain_handle = spawn_supervised(
                    "subagent-mailbox-drainer",
                    std::panic::Location::caller(),
                    async move {
                        let mut best_effort_sent_at: HashMap<String, Instant> = HashMap::new();
                        'drain: loop {
                            tokio::select! {
                                biased;
                                _ = &mut flush_rx => {
                                    for envelope in receiver.drain_available() {
                                        if !forward_subagent_mailbox_message(
                                            &tx_event_clone,
                                            &mailbox_owner_session_id,
                                            &mailbox_turn_id,
                                            envelope.seq,
                                            envelope.message,
                                            &mut best_effort_sent_at,
                                        ).await {
                                            break 'drain;
                                        }
                                    }
                                    break;
                                }
                                envelope = receiver.recv() => {
                                    let Some(envelope) = envelope else { break };
                                    if !forward_subagent_mailbox_message(
                                        &tx_event_clone,
                                        &mailbox_owner_session_id,
                                        &mailbox_turn_id,
                                        envelope.seq,
                                        envelope.message,
                                        &mut best_effort_sent_at,
                                    ).await {
                                        break;
                                    }
                                }
                            }
                        }
                    },
                );
                Some(TurnMailboxBarrier {
                    mailbox,
                    cancel_token,
                    foreground_children,
                    flush_tx,
                    drain_handle,
                    settle_grace: FOREGROUND_CHILD_SETTLE_GRACE,
                })
            } else {
                None
            };

        let mcp_pool = if self.config.features.enabled(Feature::Mcp) {
            if mcp_access.may_connect() {
                self.ensure_mcp_pool().await.ok()
            } else {
                self.mcp_pool.clone()
            }
        } else {
            None
        };

        let mut subagent_runtime_model = None;
        let mut tool_registry = if self.child_host.is_some() {
            builder.build(tool_context)
        } else if subagents_available {
            let runtime = if let Some(client) = route.client.clone() {
                let runtime_allow_shell =
                    input_policy.allow_shell && !matches!(input_policy.mode, AppMode::Plan);
                let runtime_shell_policy =
                    shell_policy_for_mode(input_policy.mode, runtime_allow_shell);
                subagent_runtime_model = Some(route.model.clone());
                let mut rt = SubAgentRuntime::new(
                    client,
                    route.model.clone(),
                    tool_context.clone(),
                    runtime_allow_shell,
                    Some(self.tx_event.clone()),
                    Arc::clone(&self.subagent_manager),
                )
                .with_locale_tag(route.locale_tag.clone())
                .with_role_models(route.role_models.clone())
                .with_api_config((*route.api_config).clone())
                .with_execution_limits(self.subagent_execution_limits.clone())
                .with_auto_model(route.auto_model)
                .with_reasoning_effort(route.reasoning_effort.clone(), route.reasoning_effort_auto)
                .with_agent_tool_surface_options(
                    self.agent_tool_surface_options(runtime_shell_policy),
                )
                .with_max_spawn_depth(self.config.max_spawn_depth)
                .with_step_api_timeout(self.config.subagent_api_timeout)
                .with_speech_output_dir(self.config.speech_output_dir.clone())
                .with_mcp_pool(mcp_pool.clone())
                .with_todos(self.config.todos.clone())
                .with_parent_completion_tx(self.tx_subagent_completion.clone())
                .with_parent_compaction(&self.config.compaction)
                .with_parent_mode(input_policy.mode)
                .with_approval_receipt_store(self.approval_receipt_store.clone())
                .with_permission_posture(
                    Arc::clone(&self.shared_auto_review_policy),
                    self.config.terminal_chrome_enabled,
                );
                if matches!(input_policy.mode, AppMode::Plan) {
                    rt.worker_profile = WorkerRuntimeProfile::for_role(FleetRole::Planner);
                }
                // #4042: stamp the session's --disallowed-tools onto the parent
                // runtime so every model-spawned sub-agent inherits the deny-list
                // (plan-mode role override above is intentionally before this).
                rt.worker_profile.denied_tools =
                    self.config.disallowed_tools.clone().unwrap_or_default();
                if let Some(context) = fork_context_for_runtime.clone() {
                    rt = rt.with_fork_context(context);
                }
                if let Some(barrier) = mailbox_for_runtime.as_ref() {
                    rt = rt
                        .with_mailbox(barrier.mailbox.clone())
                        .with_cancel_token(barrier.cancel_token.clone())
                        .with_foreground_children(Arc::clone(&barrier.foreground_children));
                }
                Some(rt)
            } else {
                None
            };
            if let Some(subagent_runtime) = runtime {
                builder
                    .with_subagent_tools(self.subagent_manager.clone(), subagent_runtime)
                    .build(tool_context)
            } else {
                tracing::warn!(
                    "Sub-agents enabled but no API client available, falling back to basic tool set"
                );
                builder.build(tool_context)
            }
        } else {
            builder.build(tool_context)
        };

        // Load plugin tools from the user's tools directory and apply any
        // config.toml overrides. Explicit overrides win over auto-discovered
        // scripts with the same tool name; neither may replace a built-in.
        let extension_host = self
            .extension_host
            .as_ref()
            .filter(|_| self.config.features.enabled(Feature::ExtensionHost));
        if let Some(attachment) = extension_host {
            // Natives only: scripts are added next and must not count as built-ins.
            attachment
                .manager()
                .note_native_names(tool_registry.names());
            attachment.sync_in_background();
        }
        let (mut plugin_tool_names, refused_overrides) =
            configure_plugin_tools(&mut tool_registry, self.config.tools.as_ref());
        // The registry is rebuilt every turn: name each refused override to
        // the user once per process, not once per turn.
        for name in refused_overrides {
            if first_notice(&name) {
                let _ = self
                    .send_event(Event::status(
                        crate::tools::registry::override_refusal_notice(&name),
                    ))
                    .await;
            }
        }
        // Extension tools go in last and never replace a name already present
        // (`ToolRegistry::register` would overwrite it silently). Only this
        // engine's own plugins' tools are installed.
        if let Some(attachment) = extension_host {
            plugin_tool_names.extend(attachment.install_tools(&mut tool_registry));
        }

        let mcp_state = if self.config.features.enabled(Feature::Mcp) {
            if mcp_access.may_connect() {
                let tools = self.mcp_tools().await;
                let server_count = match self.mcp_pool.as_ref() {
                    Some(pool) => pool.lock().await.connected_servers().len(),
                    None => 0,
                };
                McpToolState::Live {
                    tools,
                    server_count,
                }
            } else {
                self.passive_mcp_snapshot().await
            }
        } else {
            McpToolState::Disabled
        };
        // Captured before the catalog closure consumes the tool list, so a
        // caller can attribute MCP contributions without a second connect.
        let mcp_tools = mcp_state.tools().to_vec();
        let mcp_tool_names: Vec<String> = mcp_tools.iter().map(|tool| tool.name.clone()).collect();
        if self.child_host.is_none() && matches!(mcp_state, McpToolState::Live { .. }) {
            let native_names = tool_registry.names();
            for entry in &self.config.tools_always_load {
                let selects = |name: &str| match entry.strip_suffix('*') {
                    Some(prefix) => name.starts_with(prefix),
                    None => name == entry,
                };
                if !native_names.iter().any(|name| selects(name))
                    && !mcp_tool_names.iter().any(|name| selects(name))
                    && first_notice(&format!("always_load:{entry}"))
                {
                    let _ = self
                        .send_event(Event::status(format!(
                            "always_load entry `{entry}` matches no tool; MCP tool names carry a plugin-N prefix that renumbers when plugins change."
                        )))
                        .await;
                }
            }
        }
        // The surface budget belongs to the route the request would go to,
        // which is not necessarily the installed one under auto routing.
        let capability = route.capability_profile();
        let always_load = self.config.tools_always_load.clone();
        self.turn_tool_surface_budget = Some(capability.tool_surface_budget);
        if self.child_host.is_some() {
            tool_registry.remove_tool("create_goal");
            tool_registry.remove_tool("update_goal");
            let machine_tools: Vec<_> = tool_registry
                .names()
                .into_iter()
                .filter(|name| crate::tools::subagent::is_machine_control_tool(name))
                .map(str::to_owned)
                .collect();
            for name in machine_tools {
                tool_registry.remove_tool(&name);
            }
        }
        let base_catalog = self.child_host.as_ref().map_or_else(
            || tool_registry.to_api_tools_with_cache(true),
            |child| {
                child
                    .authority
                    .tools_for_model(&tool_registry, &child.authority.agent_type)
            },
        );
        let catalog = build_model_tool_catalog_with_surface(
            base_catalog,
            if self.child_host.is_some() {
                Vec::new()
            } else {
                mcp_tools
            },
            input_policy.mode,
            &always_load,
            capability.tool_surface_budget,
        );
        let mut surface = ToolSurfacePolicy::new(
            tool_registry,
            Some(catalog),
            input_policy.mode,
            &always_load,
            &input_policy.dynamic_active_tools,
            self.config.strict_tool_mode,
            allowed_tools,
            self.config.disallowed_tools.clone(),
            self.config.max_tool_calls,
            // Model metadata wins once wired; today the hint is always None
            // and the [features] flags decide (model_registry follow-up).
            tool_catalog::requested_tool_mode(None, &self.config.features),
        );
        if let Some(child) = self.child_host.as_ref() {
            Self::narrow_child_surface(&child.authority, &mut surface);
            let prefix = &child.fork_cache_prefix;
            surface.catalog.sort_by_key(|tool| {
                prefix
                    .iter()
                    .position(|name| name == &tool.name)
                    .unwrap_or(prefix.len())
            });
            surface.active = tool_catalog::active_tools_for_request(
                &surface.catalog,
                &surface.active_names,
                surface.strict_tool_mode,
            );
        }
        TurnToolBuild {
            surface,
            mcp_tool_names,
            mcp: mcp_state,
            subagent_runtime_model,
            mailbox: mailbox_for_runtime,
            plugin_tool_names,
        }
    }

    /// Read-only MCP snapshot for `/preview-request` (#1004).
    ///
    /// Never creates the pool, never calls `connect_all`, never reloads a
    /// config source, never starts a server, and never emits a status event.
    /// It answers exactly one question: *is the tool set the next turn would
    /// send already known?* It is known only when the pool exists, every
    /// enabled server is connected, and no config source has changed since
    /// the pool last read them. Otherwise the honest answer is "unavailable",
    /// because a real turn would connect and discover more tools.
    async fn passive_mcp_snapshot(&self) -> McpToolState {
        let Some(pool) = self.mcp_pool.as_ref() else {
            return McpToolState::Unavailable {
                reason: McpUnavailable::PoolNotStarted,
            };
        };
        let pool = pool.lock().await;
        if !pool.config_sources_unchanged() {
            return McpToolState::Unavailable {
                reason: McpUnavailable::ConfigChangedSinceConnect,
            };
        }
        let connected: Vec<&str> = pool.connected_servers();
        let pending = pool
            .enabled_server_names()
            .into_iter()
            .filter(|name| !connected.iter().any(|connected| *connected == name))
            .count();
        if pending > 0 {
            return McpToolState::Unavailable {
                reason: McpUnavailable::ServersNotConnected { pending },
            };
        }
        McpToolState::Live {
            tools: pool.to_api_tools(),
            server_count: connected.len(),
        }
    }

    async fn handle_send_message(&mut self, spec: TurnSpec) -> SendMessageOutcome {
        let autonomous =
            self.admitted_turn_control.is_none() && !spec.provenance.can_authorize_work();
        // Claim first. Pending controls never affect the active Engine policy.
        // The wrapper restores on every ordinary/early return of the body;
        // dropping or panicking the entire Engine future cannot run a successor.
        let control = self.begin_turn_control_for_provenance(spec.provenance);
        let previous = std::mem::replace(&mut self.turn_narrowing, control.narrowing);
        let shell_ceiling = self.is_acp_turn().then_some(spec.allow_shell);
        let previous_shell = std::mem::replace(&mut self.turn_acp_shell_ceiling, shell_ceiling);
        let outcome = self.handle_admitted_message(spec, autonomous).await;
        self.turn_acp_shell_ceiling = previous_shell;
        self.turn_narrowing = previous;
        drop(control);
        outcome
    }

    async fn handle_admitted_message(
        &mut self,
        spec: TurnSpec,
        autonomous: bool,
    ) -> SendMessageOutcome {
        let TurnSpec {
            profile_constitution,
            max_output_tokens,
            content,
            images,
            mode,
            route,
            compaction,
            initial_routed_usage,
            goal_objective,
            goal_token_budget,
            goal_status,
            reasoning_effort,
            reasoning_effort_auto,
            auto_model,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
            translation_enabled,
            allowed_tools,
            dynamic_tools,
            hook_executor,
            verbosity,
            provenance,
            submission_id,
        } = spec;
        let route = *route;
        let compaction = *compaction;
        let initial_routed_usage = *initial_routed_usage;
        let initial_usage_owner = compaction.runtime_cost_owner.clone();
        if autonomous && self.cancel_token.is_cancelled() {
            crate::cost_status::report_runtime_usage_batch(
                crate::cost_status::scope_token(),
                initial_usage_owner.as_deref(),
                &initial_routed_usage,
            );
            return SendMessageOutcome::NotStarted { error: None };
        }
        // All surfaces reuse the same bounded validator. Runtime already checks
        // before admission; this also protects direct in-process operations.
        let images = match crate::image_attach::prepare_stored_images(&images) {
            Ok(images) => images,
            Err(error) => {
                let message = error.to_string();
                crate::cost_status::report_runtime_usage_batch(
                    crate::cost_status::scope_token(),
                    initial_usage_owner.as_deref(),
                    &initial_routed_usage,
                );
                let _ = self
                    .send_event(Event::error(ErrorEnvelope::new(
                        crate::error_taxonomy::ErrorCategory::InvalidInput,
                        crate::error_taxonomy::ErrorSeverity::Error,
                        true,
                        "image_input_invalid",
                        message.clone(),
                    )))
                    .await;
                return SendMessageOutcome::NotStarted {
                    error: Some(message),
                };
            }
        };
        // Reserve both lifecycle observations before mutating the session.
        // Otherwise cancellation during a blocked TurnStarted send could
        // create a completion with no start. The production queue has 256
        // slots; these local permits do not create a second event authority.
        //
        // A durable host (Runtime threads) recorded this turn before the
        // engine saw it and settles it only on its terminal event. A queued
        // cancellation there must still yield the ordered TurnStarted and
        // Interrupted TurnComplete (the cancelled token stops the turn before
        // any provider dispatch), so its admission waits for capacity instead
        // of refusing on cancellation. An interactive queued cancellation
        // keeps no lifecycle.
        // Internal follow-ups belong to the already-admitted work. They must
        // not replace its account preferences with the host operator's profile.
        let constitution_block = if self.rlm_host.is_some()
            || (!provenance.can_authorize_work() && profile_constitution.is_none())
        {
            self.constitution_block.clone()
        } else {
            let host_supplied = profile_constitution.is_some();
            match crate::profile_constitution::capture(
                self.api_config.account_profile.as_deref(),
                profile_constitution,
            )
            .await
            {
                Ok(block) => block,
                // Local use never depends on the Codewhale account: an expired
                // sign-in or unreachable account service falls back to the
                // signed-out local constitution, never another account's.
                Err(error) if !host_supplied => {
                    tracing::warn!(%error, "account profile constitution unavailable; using local constitution");
                    if !self.profile_constitution_fallback_noticed {
                        self.profile_constitution_fallback_noticed = true;
                        let _ = self
                            .send_event(Event::status(
                                codewhale_localization::tr(
                                    codewhale_localization::resolve_locale(&self.config.locale_tag),
                                    codewhale_localization::MessageId::ProfileConstitutionUnavailableLocal,
                                )
                                .to_string(),
                            ))
                            .await;
                    }
                    crate::prompts::load_user_constitution_block()
                }
                Err(error) => {
                    crate::cost_status::report_runtime_usage_batch(
                        crate::cost_status::scope_token(),
                        initial_usage_owner.as_deref(),
                        &initial_routed_usage,
                    );
                    let _ = self
                        .send_event(Event::error(ErrorEnvelope::new(
                            ErrorCategory::InvalidInput,
                            ErrorSeverity::Error,
                            true,
                            "profile_constitution_unavailable",
                            error.to_string(),
                        )))
                        .await;
                    return SendMessageOutcome::NotStarted {
                        error: Some(error.to_string()),
                    };
                }
            }
        };
        let admission_cancel = (!self.host_managed_turns()).then_some(&self.cancel_token);
        let admission = async {
            let terminal = streaming::reserve_event_capacity(
                &self.tx_event,
                admission_cancel,
                streaming::EventReservationPolicy::Strict,
            )
            .await?;
            let started = streaming::reserve_event_capacity(
                &self.tx_event,
                admission_cancel,
                streaming::EventReservationPolicy::Strict,
            )
            .await?;
            Ok::<_, streaming::EventSendError>((terminal, started))
        };
        let (terminal_permit, start_permit) = match admission.await {
            Ok(permits) => permits,
            Err(reason) => {
                // A queued operation is not an admitted turn. No TurnStarted,
                // session mutation or provider dispatch happened, but an Auto
                // classifier may already have produced billed routed usage.
                crate::cost_status::report_runtime_usage_batch(
                    crate::cost_status::scope_token(),
                    initial_usage_owner.as_deref(),
                    &initial_routed_usage,
                );
                return SendMessageOutcome::NotStarted {
                    error: (reason == streaming::EventSendError::Closed).then(|| {
                        "Cannot start the turn because its event consumer is closed".to_string()
                    }),
                };
            }
        };

        // Goals are created by the model (`create_goal`) or by the leading
        // `/goal <objective>` command; the host never infers one from
        // wording (docs/design/TUI_DECONSTRUCTION.md — founder clarification
        // 2026-09-09: the model decides when a goal is useful). The
        // natural-language `/goal` prose parser that used to recognize
        // "make it your /goal to ..." is gone with the #6290 rework — a
        // prose ask reaches the model, which calls `create_goal` when a goal
        // is actually useful.
        //
        // KV-cache effect: none. Goal state still flows through the existing
        // volatile <session_goal> contributor; nothing here touches the
        // stable prefix.

        let effective_provider = route.identity.provider;
        let provider_identity = route.identity.key.to_string();
        let model = route.model.clone();
        let route_limits = crate::route_budget::known_route_limits(route.candidate.limits());
        let route_capabilities = route.candidate.capabilities();
        let route_api_config = route.config.clone();
        let dispatched_identity = route.identity.clone();
        // Freeze the billing receipt here, while `route` is still the single
        // authority for this turn: `route.config` is the identity-scoped
        // Config the client is being built from, and `route.candidate` names
        // the endpoint it will call. After `install_resolved_runtime_route`
        // consumes `route`, the only sound source for these facts is this
        // receipt — an ambient `Config` read at TurnStarted or TurnComplete
        // would follow a later provider switch, auto-router hop, or custom
        // table change onto the wrong vendor.
        let dispatched_base_url = route.candidate.endpoint().base_url.clone();
        let dispatched_product =
            crate::route_billing::capture_product(&route.config, &route.identity);
        if let Err(err) = self.install_resolved_runtime_route(route) {
            let cost_scope = crate::cost_status::scope_token();
            crate::cost_status::report_runtime_usage_batch(
                cost_scope,
                initial_usage_owner.as_deref(),
                &initial_routed_usage,
            );
            let _ = self
                .send_event(Event::error(ErrorEnvelope::fatal_auth(format!(
                    "Cannot start the turn because its provider route is not ready: {err}"
                ))))
                .await;
            self.sync_unstarted_goal_for_terminal_projection(
                goal_objective.as_deref(),
                goal_token_budget,
                goal_status,
            );
            let outcome = SendMessageOutcome::NotStarted { error: Some(err) };
            self.reconcile_non_completed_goal_turn(&outcome).await;
            return outcome;
        }

        // Deliver completions that arrived after the previous turn before the
        // next user request is sent. This keeps background shell work
        // model-visible without requiring an explicit wait/poll tool call.
        let shell_completions = if self.is_acp_turn() {
            Vec::new()
        } else {
            self.drain_shell_completion_events()
        };
        if !shell_completions.is_empty() {
            self.add_session_message(crate::runtime_handoff::shell_completion_runtime_message(
                &shell_completions,
            ))
            .await;
            if let Some(status) =
                crate::core::engine::turn_loop::shell_completion_status_text(&shell_completions, "")
            {
                let _ = self.send_event(Event::status(status)).await;
            }
        }

        // A person writing to a goal that only the runtime stopped (a failed
        // or timed-out continuation) is continuing the work: resume it as a
        // new revision instead of running a goalless turn against a stale
        // blocker. A goal the model handed back at a milestone is the same
        // shape — the model stopped for an answer, not because anything is
        // wrong — so answering it continues the work too. Blockers the model
        // reported and pauses the user asked for stay until an explicit
        // resume, and automated inputs never resume anything.
        let resumed_goal = if !self.is_acp_turn() && provenance == UserInputProvenance::ExternalUser
        {
            match goal_status {
                GoalStatus::Blocked => {
                    self.resume_runtime_blocked_goal(goal_objective.as_deref())
                        .await
                }
                GoalStatus::Paused => self.resume_yielded_goal(goal_objective.as_deref()).await,
                _ => false,
            }
        } else {
            false
        };
        let goal_status = if resumed_goal {
            GoalStatus::Active
        } else {
            goal_status
        };
        let input_policy = effective_input_policy(
            provenance,
            mode,
            &content,
            allow_shell,
            trust_mode,
            auto_approve,
            approval_mode,
        );
        let prompt_context = NextTurnPromptContext::for_planned_turn(
            effective_provider,
            model.clone(),
            route_limits,
            input_policy.mode,
            goal_objective.clone(),
            goal_status,
            goal_token_budget,
            translation_enabled,
            verbosity.clone(),
        );
        // #3947: an effective-mode change is never silent. The structured
        // event is recorded first (so doctor and this turn's metadata can read
        // it), then rendered to the UI from that same value.
        self.last_policy_narrowing = input_policy.narrowing.clone();
        if let Some(status) = input_policy.status() {
            let _ = self.send_event(Event::status(status)).await;
        }

        // Track the complete effective mode policy so mid-turn metadata, `/edit`,
        // idle worker resumptions, and approval gates cannot read a stale policy
        // after the UI changed modes (#3568).
        let prior = self.applied_runtime_authority();
        self.apply_runtime_mode_policy(&input_policy);
        self.discard_kernels_if_narrowed(&prior).await;

        // Create turn context first so start event includes a stable turn id.
        // An active goal gets the host's goal allowance (#5994); turns with
        // an explicit per-invocation ceiling (exec --max-turns, child
        // workers) never see it because those hosts leave `goal_max_steps`
        // unset.
        let goal_turn = goal_objective.is_some() && goal_status == GoalStatus::Active;
        let mut turn = if self.is_acp_turn() {
            TurnContext::new(self.config.max_steps.clamp(1, 50))
        } else if goal_turn && let Some(goal_max_steps) = self.config.goal_max_steps {
            TurnContext::with_budget_source(
                goal_max_steps,
                crate::core::turn::StepBudgetSource::Goal,
            )
        } else {
            TurnContext::new(self.config.max_steps)
        };
        turn.max_output_tokens = max_output_tokens;
        self.turn_counter = self.turn_counter.saturating_add(1);
        let turn_started_at = chrono::Utc::now();
        // Mint the route receipt from the client that `install_resolved_runtime_route`
        // actually installed above — the same client `Event::TurnComplete`
        // reports `base_url` from. Hosts must not re-derive this from config
        // when they process `TurnStarted`: by then config may already describe
        // a different endpoint or credential.
        let route_receipt = if self.model_client_injected {
            // Provider-neutral injected clients are the I/O authority, while
            // `codewhale_client` is only an auxiliary route-shaping client.
            // It cannot truthfully receipt a transport it did not perform.
            None
        } else {
            self.codewhale_client
                .as_ref()
                .map(|client| client.turn_route_receipt())
        };
        let route_base_url = self
            .codewhale_client
            .as_ref()
            .map(|client| client.base_url());
        let turn_route = TurnRoute {
            provider: effective_provider,
            provider_identity,
            model: model.clone(),
            auto_model,
            receipt: route_receipt,
            // A start is not an application dispatch. The billing envelope is
            // attached below, then stamped at the pre-permit admission boundary.
            billing: None,
            // The classification receipt, by contrast, is frozen here at the
            // client-freeze boundary and is readable from `TurnStarted` on.
            base_url: dispatched_base_url,
            billing_product: dispatched_product,
        };
        // Billing provenance follows the *route* that was installed for this
        // turn, which is authoritative even when a test or embedder injected the
        // transport: `codewhale_client`'s base URL is the resolved route's
        // endpoint either way. This is a weaker claim than `receipt`, which
        // digests the credential an injected client did not use and is therefore
        // withheld above.
        let dispatch_billing = crate::core::events::RouteBillingEnvelope {
            openrouter_vendor: self
                .codewhale_client
                .as_ref()
                .and_then(|client| client.openrouter_vendor().map(str::to_string)),
            billing_surface: crate::route_billing::billing_surface_for_dispatch(
                Some(&route_api_config),
                &dispatched_identity,
                route_base_url,
            )
            .map(str::to_string),
            endpoint_fingerprint: route_base_url.and_then(crate::cost_status::endpoint_fingerprint),
            // A live rate is not evidence at turn creation. `turn_loop`
            // freezes it from the exact fresh cache scope at CodeWhale's
            // pre-permit application-dispatch boundary.
            provider_live_pricing: None,
            // Classified from this turn's own frozen receipt, not from a
            // second ambient `for_route` read. Both halves of the route then
            // answer from the same captured endpoint + credential product, so
            // the application-dispatch envelope and the receipt carried on
            // `TurnRoute` cannot disagree about how this turn bills.
            billing_mode: crate::route_billing::for_dispatched_receipt(
                crate::route_billing::DispatchedReceipt {
                    provider: effective_provider,
                    identity: Some(turn_route.provider_identity.as_str()),
                    base_url: turn_route.base_url.as_str(),
                    product: turn_route.billing_product,
                },
            )
            .into(),
            // Provisional. Replaced with the pre-permit application-dispatch
            // instant when `run_turn` emits `Event::RouteDispatched`.
            dispatched_at: turn_started_at,
        };
        turn.pending_route = Some(TurnRoute {
            billing: Some(dispatch_billing),
            ..turn_route.clone()
        });

        // Emit turn started event IMMEDIATELY so the UI knows the turn is
        // active. The snapshot below can take 30+ seconds on slow filesystems
        // (e.g. WSL2 /mnt/c) and must not delay the TurnStarted event.
        start_permit.send(Event::TurnStarted {
            turn_id: turn.id.clone(),
            created_at: turn_started_at,
            route: Some(turn_route),
            // Bind submit-window actions to the turn admitted by this permit.
            submission_id,
        });

        // Auto's classifier completed before this parent turn was admitted.
        // Bind its exact routed records to the now-accepted turn: total tokens
        // and model-call telemetry include the work, while parent_route_usage
        // remains untouched so the parent quote can never price it.
        turn.add_routed_usages(
            initial_routed_usage
                .records
                .iter()
                .map(|record| &record.usage.usage),
        );
        // Exact missing-usage records are persisted route-aware by the runtime
        // sink. TurnComplete carries only any count whose exact route record
        // was truncated, otherwise the terminal merge would count the same
        // provider response twice and misclassify subscription/local calls.
        let residual_dropped_records = initial_routed_usage.dropped_records.saturating_sub(
            u64::try_from(initial_routed_usage.drop_records.len()).unwrap_or(u64::MAX),
        );
        turn.add_routed_usage_dropped_records(residual_dropped_records);
        let initial_cost_scope = crate::cost_status::scope_token();
        crate::cost_status::report_runtime_usage_batch(
            initial_cost_scope,
            initial_usage_owner.as_deref(),
            &crate::cost_status::RuntimeUsageBatch {
                decisions: initial_routed_usage.decisions.clone(),
                ..Default::default()
            },
        );
        for record in &initial_routed_usage.records {
            crate::cost_status::report_effective_route_for_runtime(
                initial_cost_scope,
                initial_usage_owner.as_deref(),
                &record.source_id,
                &record.usage.route,
                &record.usage.usage,
            );
            let _ = self
                .send_event(Event::RoutedTurnUsage {
                    usage: record.usage.usage.clone(),
                    duration_ms: 0,
                    first_token_ms: None,
                    request_ms: None,
                })
                .await;
        }
        for record in &initial_routed_usage.drop_records {
            crate::cost_status::report_unreceipted_provider_success(
                initial_cost_scope,
                initial_usage_owner.as_deref(),
                &record.source_id,
                &record.route,
            );
        }

        // Apply the host-resolved route budget before building the request.
        // The model, limits, and compaction policy arrive in one operation so
        // no provider request can observe a partially updated route.
        self.active_route_limits = route_limits;
        self.config.compaction = compaction;
        // Snapshot the workspace BEFORE we touch a single tool. Run the git
        // work on the blocking pool so the async runtime stays responsive;
        // failure is non-fatal (the helper logs at WARN).
        // The label carries a truncated first line of the prompt so
        // `/restore` listings are human-readable.
        self.take_restore_point(
            WorkspaceSnapshotKind::PreTurn,
            format_snapshot_label("pre-turn", self.turn_counter, Some(&content)),
            None,
            None,
        )
        .await;

        self.emit_pending_snapshot_notices().await;

        // A new turn means any leftover retry banner (success cleared
        // it, failure pinned it) is no longer relevant — reset to idle
        // so the footer doesn't display a stale failure row across
        // turns (#499).
        crate::retry_status::clear();

        // Clone user prompt for post-turn snapshot label before `content`
        // is moved into `user_text_message_with_turn_metadata_for_route` below.
        let snapshot_prompt_post = content.clone();

        if self.model_client.is_none() {
            let message = self
                .codewhale_client_error
                .as_deref()
                .map(|err| format!("Failed to send message: {err}"))
                .unwrap_or_else(|| "Failed to send message: API client not configured".to_string());
            let _ = self
                .send_event(Event::error(ErrorEnvelope::fatal_auth(message.clone())))
                .await;
            let status = terminal_turn_status_at_settlement(
                TurnOutcomeStatus::Failed,
                self.cancel_token.is_cancelled(),
            );
            let error = (status == TurnOutcomeStatus::Failed).then_some(message.clone());
            terminal_permit.send(Event::TurnComplete {
                usage: turn.usage.clone(),
                parent_route_usage: turn.parent_route_usage.clone(),
                routed_usage_dropped_records: turn.routed_usage_dropped_records,
                status,
                error: error.clone(),
                tool_catalog: None,
                base_url: None,
            });
            self.sync_unstarted_goal_for_terminal_projection(
                goal_objective.as_deref(),
                goal_token_budget,
                goal_status,
            );
            // The reserved lifecycle is settled above, but no model client
            // ever received this turn: it did not start. `/edit` restores the
            // exchange it cut only for NotStarted (C02-02), and goal
            // reconciliation names the same fact.
            let outcome = SendMessageOutcome::NotStarted {
                error: Some(message),
            };
            self.reconcile_non_completed_goal_turn(&outcome).await;
            return outcome;
        }

        // Headless/runtime hosts supply their durable turn owner. Interactive
        // turns historically supplied none, leaving a detached child with
        // only the soon-to-be-sealed mailbox. Install this turn-local sink
        // only after every pre-dispatch failure return, and retire/clear it at
        // settlement so the next turn always receives a fresh owner.
        let interactive_runtime_cost_owner = if self.config.terminal_chrome_enabled
            && self.config.compaction.runtime_cost_owner.is_none()
        {
            let owner = format!("interactive:{}:{}", self.session.id, turn.id);
            crate::cost_status::register_persistent_interactive_runtime_usage_sink(
                &owner,
                crate::cost_status::scope_token(),
                &self.session.id,
                &turn.id,
            );
            self.config.compaction.runtime_cost_owner = Some(owner.clone());
            Some(owner)
        } else {
            None
        };

        let previous_goal_objective = self.config.goal_objective.clone();
        let previous_goal_token_budget = self.config.goal_token_budget;
        let previous_goal_status = self.config.goal_status;

        self.session.model = model.clone();
        self.config.model.clone_from(&self.session.model);
        self.config.goal_objective = goal_objective.clone();
        self.config.goal_token_budget = goal_token_budget;
        self.config.goal_status = goal_status;
        if normalized_goal_objective(previous_goal_objective.as_deref())
            != normalized_goal_objective(goal_objective.as_deref())
            || previous_goal_token_budget != goal_token_budget
            || previous_goal_status != goal_status
        {
            sync_goal_state_from_host(
                &self.config.goal_state,
                normalized_goal_objective(goal_objective.as_deref()).as_deref(),
                goal_token_budget,
                goal_status,
            );
        }
        self.config.allowed_tools = allowed_tools;
        self.config.hook_executor = hook_executor;
        self.session.reasoning_effort = reasoning_effort;
        self.session.reasoning_effort_auto = reasoning_effort_auto;
        self.session.auto_model = auto_model;
        self.config.translation_enabled = translation_enabled;
        self.config.verbosity = verbosity;

        // Capture only this engine's reviewed, live plugin contributions. A
        // failed/withdrawn capture records retirement. Full bounded snapshots
        // use ordinary session history, not the smaller workspace line delta.
        if let Some(attachment) = self.extension_host.as_ref() {
            self.plugin_registry = attachment.plugin_view();
        }
        self.config.hook_executor = self.config.hook_executor.as_ref().map(|hooks| {
            Arc::new(
                hooks.bind_caller(crate::hooks::HookCaller {
                    workspace: self.session.workspace.clone(),
                    plugins: Some(Arc::clone(&self.plugin_registry)),
                    session_id: Some(self.session.id.clone()),
                    agent_id: self
                        .child_host
                        .as_ref()
                        .map(|child| child.authority.owner_agent_id.clone()),
                    origin_turn_id: Some(turn.id.clone()),
                    origin_call_id: None,
                }),
            )
        });
        self.extension_prompt_block = if self.rlm_host.is_some() {
            self.extension_prompt_block.clone()
        } else if self.config.features.enabled(Feature::ExtensionHost) {
            if let Some(attachment) = &self.extension_host {
                match attachment.prompt_sections().await.and_then(|sections| {
                    crate::extension_host::prompt::render_prompt_sections_for_turn(
                        &sections,
                        &prompt_context.model,
                        &self.session.workspace,
                    )
                }) {
                    Ok(block) => block,
                    Err(reason) => {
                        tracing::warn!(%reason, "extension prompt contributions unavailable");
                        let _ = self
                            .send_event(Event::status(
                                codewhale_localization::tr(
                                    codewhale_localization::resolve_locale(&self.config.locale_tag),
                                    codewhale_localization::MessageId::ExtensionPromptUnavailable,
                                )
                                .to_string(),
                            ))
                            .await;
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
        self.constitution_block = constitution_block;
        self.record_current_constitution().await;
        self.record_current_extension_prompt_contributions().await;

        // Compose from the immutable values accepted for this turn. Preview
        // receives the same context before anything is installed, so prompt
        // bytes cannot depend on stale session state or mutation order. The
        // pinned header only moves on an explicit-input change; workspace
        // drift arrives as a `<context_update>` user message appended below.
        let context_update = self.refresh_pinned_header_for_turn(&prompt_context);
        if let Some(update) = context_update {
            self.session.add_message(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: update,
                    cache_control: None,
                }],
            });
        }

        self.record_mode_notice(mode);

        // The Operate contract (docs/MODES.md) precedes the first Operate
        // prompt. KV-cache effect: append-only history, one user-role runtime
        // message; it is derived from the session log rather than a flag so a
        // cleared, restored, or compacted session gets it again exactly once
        // and every later Operate turn does not repeat it.
        if mode == AppMode::Operate
            && !self
                .session
                .messages
                .iter()
                .any(crate::runtime_handoff::is_current_operate_contract_message)
        {
            self.session
                .add_message(crate::runtime_handoff::operate_contract_runtime_message());
        }

        self.session
            .working_set
            .observe_user_message(&content, &self.session.workspace);

        // Add the user message through the same explicit snapshot constructor
        // preview uses. Route limits and mode in resource metadata therefore
        // belong to this turn even when the previous route was different.
        let mut user_msg = self.user_text_message_from_snapshot(
            content,
            &model,
            auto_model,
            self.session.reasoning_effort.as_deref(),
            self.session.reasoning_effort_auto,
            provenance,
            TurnMetadataSnapshot {
                prompt_context: &prompt_context,
                system_prompt: self.session.system_prompt.as_ref(),
                approval_mode: self.session.approval_mode,
                working_set: &self.session.working_set,
                policy_narrowing: self.last_policy_narrowing.as_ref(),
            },
        );
        let image_index = if self.api_config.runtime_chat_isolated {
            user_msg.content.len()
        } else {
            user_msg.content.len().saturating_sub(1)
        };
        user_msg.content.splice(image_index..image_index, images);
        self.session.add_message(user_msg);
        turn.unanswered_user_message = Some(self.mark_unanswered_user_message());

        self.emit_session_updated().await;

        // Build tool registry and tool list for the current mode
        let turn_id_for_mailbox = turn.id.clone();
        let TurnToolBuild {
            surface,
            mailbox: mut mailbox_for_runtime,
            plugin_tool_names,
            ..
        } = self
            .build_turn_tool_registry_and_catalog(
                &input_policy,
                &dynamic_tools,
                self.config.allowed_tools.clone(),
                SubAgentWiring::Live,
                McpAccess::Connect,
                TurnRouteContext {
                    provider: self.api_provider,
                    model: self.config.model.clone(),
                    capabilities: route_capabilities,
                    limits: self.active_route_limits,
                    client: self.codewhale_client.clone(),
                    api_config: route_api_config,
                    locale_tag: self.config.locale_tag.clone(),
                    role_models: self.subagent_role_models(),
                    auto_model,
                    reasoning_effort: self.session.reasoning_effort.clone(),
                    reasoning_effort_auto: self.session.reasoning_effort_auto,
                },
                &turn_id_for_mailbox,
            )
            .await;
        let tool_catalog_for_event = Some(surface.catalog.clone());

        // Resolve, once per turn, the out-of-request facts the read-only
        // request projection is allowed to report: flattened registry facts,
        // the MCP pool's own server attribution, and the engine-injected
        // catalog names. This is where `plugin_tool_names` and the pool lock
        // live; the snapshot itself is built later, at the request seam, from
        // the tools actually prepared for that step.
        let mut tool_surface = crate::tool_inspection::ToolSurfaceContext {
            registry: surface.registry.registry_facts(&plugin_tool_names),
            mcp_servers: match self.mcp_pool.as_ref() {
                Some(pool) => pool.lock().await.resolved_tool_servers(),
                None => std::collections::BTreeMap::new(),
            },
            synthetic_names: default_synthetic_catalog_tool_names(),
            provider: crate::tool_inspection::ProviderAvailability::Unknown,
        };
        tool_surface.provider = self.tool_surface_provider_receipt();

        let base_url_for_event = if self.model_client_injected {
            None
        } else {
            self.codewhale_client
                .as_ref()
                .map(|client| client.base_url().to_string())
        };

        // Main turn loop. Catch panics here so an internal error surfaces as a
        // failed TurnComplete instead of unwinding through `engine.run()` and
        // killing the whole engine-event-loop task — which left the UI stuck
        // on "working" forever with the engine silently dead (#2583, #1269).
        use futures_util::FutureExt as _;
        let foreground_children_for_turn = mailbox_for_runtime
            .as_ref()
            .map(|barrier| Arc::clone(&barrier.foreground_children));
        let turn_result = std::panic::AssertUnwindSafe(async {
            // Keep the turn state machine out of the enclosing event-loop
            // futures. Their nested poll frames must fit ordinary thread
            // stacks while cloning route config or executing tools.
            Box::pin(self.run_turn(
                &mut turn,
                surface,
                foreground_children_for_turn,
                Some(tool_surface),
            ))
            .await
        })
        .catch_unwind()
        .await;
        // Every return path (including a caught panic) leaves the phase idle,
        // so the stall watchdog never reports a turn that already ended.
        self.turn_heartbeat.idle();
        let (mut status, error) = match turn_result {
            Ok(outcome) => outcome,
            Err(panic) => {
                let detail = crate::utils::panic_message(&*panic);
                crate::utils::record_caught_panic("engine-event-loop", &detail);
                (
                    TurnOutcomeStatus::Failed,
                    Some(format!(
                        "The engine hit an internal error and stopped this turn: {detail}. \
                         Your session is intact — send your message again to retry. \
                         A crash report was saved to ~/.codewhale/crashes/."
                    )),
                )
            }
        };

        // Update session usage
        self.session.total_usage.add(&turn.usage);
        if !self.is_acp_turn() {
            self.record_goal_usage_for_turn(&turn.usage, turn.started_at);
        }

        // Cancellation wins until the terminal settlement decision. `run_turn`
        // performs its own final check, but an Esc/interrupt can arrive while
        // its clean-exit receipts are being appended. Recheck at this seam so
        // that pre-settlement cancellation remains terminal Cancelled child
        // work rather than continuing after a normal answer.
        let status_at_settlement =
            terminal_turn_status_at_settlement(status, self.cancel_token.is_cancelled());
        if status_at_settlement != status {
            status = status_at_settlement;
            let _ = self
                .send_event(Event::status(
                    "Request cancelled while settling turn-owned sub-agents",
                ))
                .await;
        }

        // Seal the mailbox before the terminal event and flush under its
        // existing grace. A stopped consumer can force that drainer to be
        // aborted; the warning names the lost observation boundary. Child
        // cost owners/leases remain independent of UI delivery, and no late
        // envelope is attached to the following turn.
        if let Some(barrier) = mailbox_for_runtime.take() {
            if status == TurnOutcomeStatus::Completed && !turn.budget_exhausted_final_report {
                barrier.continue_and_flush().await;
            } else {
                // The join is deadline-bounded: a child that never observes
                // its cancel token is named and left shutting down rather
                // than withholding `TurnComplete` forever (#6184).
                let unsettled = barrier.cancel_and_flush().await;
                if !unsettled.is_empty() {
                    let _ = self.send_event(Event::status(format!(
                            "Turn ended while sub-agent(s) were still shutting down: {}. Their late receipts were dropped.",
                            unsettled.join(", ")
                        )))
                        .await;
                }
            }
        }
        // The advisor is dispatched after TurnComplete, but its usage still
        // belongs to this originating turn. Acquire the owner lease before an
        // interactive owner is marked terminal so a late provider response
        // retains its exact sink instead of falling into a later session.
        let advisor_usage_context = (!self.is_acp_turn()
            && self.config.advisor_config.enabled
            && status == TurnOutcomeStatus::Completed
            && self.codewhale_client.is_some())
        .then(|| {
            crate::tools::subagent::advisor::AdvisorUsageContext::capture(
                self.config.compaction.runtime_cost_owner.as_deref(),
            )
        });
        if let Some(owner) = interactive_runtime_cost_owner.as_deref() {
            crate::cost_status::finish_runtime_usage_owner(owner);
            // This owner is turn-local. Leaving it in the reusable engine
            // config makes the next interactive turn skip registration and
            // route background usage into a retired sink/journal. Host-owned
            // runtime turn ids never enter this branch and remain untouched.
            self.config.compaction.runtime_cost_owner = None;
        }

        // Emit turn complete event — after all post-turn bookkeeping so
        // the terminal is immediately responsive when the UI receives it.
        self.emit_goal_updated().await;
        if status == TurnOutcomeStatus::Interrupted {
            self.emit_interrupted_survivor_status().await;
        }
        if let Some(snapshot) = turn.terminal_request_snapshot(status) {
            let _ = self
                .send_event(Event::ToolRequestSnapshot { snapshot })
                .await;
        }
        self.post_turn_snapshot_before_complete(&snapshot_prompt_post)
            .await;
        let pending_post_turn = self.reserve_post_turn_snapshot();
        // Cancellation still owns the decision while mailbox/snapshot
        // bookkeeping runs. The reserved completion itself never waits.
        status = terminal_turn_status_at_settlement(status, self.cancel_token.is_cancelled());
        // `event_sent` means the TurnComplete event reached the UI channel —
        // never that the user saw model output. (#6184: the old `delivered`
        // name was read as user-visible delivery on Interrupted turns that
        // rendered nothing.)
        let completion_sender = terminal_permit.send(Event::TurnComplete {
            usage: turn.usage,
            parent_route_usage: turn.parent_route_usage,
            routed_usage_dropped_records: turn.routed_usage_dropped_records,
            status,
            error: error.clone(),
            tool_catalog: tool_catalog_for_event,
            base_url: base_url_for_event,
        });
        let turn_complete_event_sent = !completion_sender.is_closed();
        tracing::info!(
            target: "engine.turn",
            status = ?status,
            event_sent = turn_complete_event_sent,
            "engine turn completion settled"
        );

        // Post-turn snapshot, unless it was already taken before
        // TurnComplete (see `EngineConfig::record_restore_points`).
        self.post_turn_snapshot_after_complete(
            "post-turn-snapshot",
            snapshot_prompt_post,
            pending_post_turn,
        );

        // ── Background advisor watcher (#3982) ────────────────────────────
        // Fire-and-forget: TurnComplete is already emitted. The advisor
        // reads a bounded snapshot of session messages (immutable clone),
        // makes a short LLM advisory call, and emits `Event::AdvisoryNote`.
        // Any failure is logged and swallowed — it must never affect the
        // parent turn's outcome.
        if !self.is_acp_turn()
            && self.config.advisor_config.enabled
            && matches!(status, TurnOutcomeStatus::Completed)
            && let Some(client) = self.codewhale_client.clone()
            && let Some(usage_context) = advisor_usage_context
        {
            // Lazily create the shared emission guard on first use.
            let guard = self
                .advisor_emission_guard
                .get_or_insert_with(|| {
                    Arc::new(tokio::sync::Mutex::new(
                        crate::tools::subagent::EmissionGuard::new(),
                    ))
                })
                .clone();

            let advisor_messages: Vec<codewhale_models::Message> = self.session.messages.to_vec();
            let advisor_config = self.config.advisor_config.clone();
            // This clone is frozen before the detached task starts and keeps
            // every configured provider route available for an explicit
            // cross-provider advisor model without consulting later UI state.
            let advisor_route_config = self.api_config.clone();
            let advisor_model = self.session.model.clone();
            let advisor_tx = self.tx_event.clone();
            let advisor_turn_id = turn.id.clone();

            crate::utils::spawn_supervised(
                "advisor-watcher",
                std::panic::Location::caller(),
                async move {
                    crate::tools::subagent::run_advisor_for_turn(
                        advisor_turn_id,
                        advisor_messages,
                        advisor_config,
                        client,
                        advisor_route_config,
                        advisor_model,
                        usage_context,
                        guard,
                        advisor_tx,
                    )
                    .await;
                },
            );
        }

        // ── Cross-turn goal continuation ───────────────────────────────────
        // When the interactive engine owns turn lifecycle, a successful turn
        // with an active goal re-dispatches a synthetic continuation through
        // its own op channel. RuntimeThreadManager engines instead yield here:
        // their host must create the next durable claim before dispatching any
        // further turn. A Failed or Interrupted turn never continues.
        //
        // #5994: a turn that exhausted the goal step budget got its bounded
        // final report already. An unfinished goal pauses with BudgetLimit
        // instead of silently re-arming another full goal turn; a verified
        // completion reported in that final turn still wins.
        let goal_budget_exhausted = turn.budget_source == crate::core::turn::StepBudgetSource::Goal
            && turn.budget_exhausted_final_report;
        if !self.is_acp_turn() && goal_budget_exhausted {
            let goal_still_active = self
                .config
                .goal_state
                .lock()
                .map(|state| state.is_active())
                .unwrap_or(false);
            if goal_still_active {
                self.pause_goal_continuation(
                    GoalPauseReason::BudgetLimit,
                    format!(
                        "Goal paused: the [goal] max_steps budget ({}) was exhausted. Review the final report, then resume the goal explicitly to continue.",
                        turn.max_steps
                    ),
                )
                .await;
            }
        }
        let outcome = SendMessageOutcome::Finished { status, error };
        if !goal_budget_exhausted
            && !self.host_managed_turns()
            && matches!(
                &outcome,
                SendMessageOutcome::Finished {
                    status: TurnOutcomeStatus::Completed,
                    ..
                }
            )
        {
            // Queue a typed continuation instead of freezing an Active goal
            // snapshot into a generic message. The operation re-reads the live
            // state when consumed, after any already-queued goal controls.
            self.schedule_goal_continuation(dynamic_tools).await;
        } else {
            self.reconcile_non_completed_goal_turn(&outcome).await;
        }
        outcome
    }

    async fn handle_purge(&mut self) {
        let zero_usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            ..Usage::default()
        };
        let Some(client) = self.codewhale_client.clone() else {
            let message = "Purge unavailable: API client not configured".to_string();
            emit_purge_failed(&self.tx_event, message.clone()).await;
            let _ = self
                .send_event(Event::error(ErrorEnvelope::fatal_auth(message.clone())))
                .await;
            let _ = self
                .send_event(Event::TurnComplete {
                    usage: zero_usage,
                    parent_route_usage: Usage::default(),
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Failed,
                    error: Some(message),
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            return;
        };

        emit_purge_started(
            &self.tx_event,
            "Agent context purge in progress\u{2026}".to_string(),
        )
        .await;
        let messages_before = self.session.messages.len();

        let (status, error) = match run_purge(
            &client,
            self.api_provider,
            &self.session.id,
            &self.session.messages,
            &self.session.model,
            self.session.reasoning_effort.clone(),
            client.effective_max_output_tokens(&self.session.model),
        )
        .await
        {
            Ok(result) => {
                let messages_after = result.messages.len();
                self.session.replace_messages(result.messages);
                self.emit_session_updated().await;

                let summary = format!(
                    "Purge complete: {messages_before} → {messages_after} messages \
                         ({} removed, {} condensed, {} offloaded)",
                    result.removed_count, result.replaced_count, result.offloaded_count,
                );
                emit_purge_completed(
                    &self.tx_event,
                    messages_before,
                    messages_after,
                    result.removed_count,
                    result.replaced_count,
                    summary,
                )
                .await;
                (TurnOutcomeStatus::Completed, None)
            }
            Err(e) => {
                emit_purge_failed(&self.tx_event, e.clone()).await;
                (TurnOutcomeStatus::Failed, Some(e))
            }
        };

        let _ = self
            .send_event(Event::TurnComplete {
                usage: zero_usage,
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status,
                error,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    }

    /// Turn-visible background shell jobs still running right now, formatted
    /// for the interrupt-honesty status line (DGF-03, dogfood 2026-08-02):
    /// Esc stops the model turn, not detached shell work. Without this,
    /// files landing on disk after "Turn interrupted" read as a lie.
    fn running_background_shell_survivors(&self) -> Vec<String> {
        let Ok(mut manager) = self.shell_manager.lock() else {
            return Vec::new();
        };
        manager
            .list_jobs_for_session(&self.session.id)
            .into_iter()
            .filter(|job| matches!(job.status, crate::tools::shell::ShellStatus::Running))
            .map(|job| {
                const MAX_COMMAND_CHARS: usize = 48;
                let mut command: String = job.command.chars().take(MAX_COMMAND_CHARS).collect();
                if job.command.chars().count() > MAX_COMMAND_CHARS {
                    command.push('…');
                }
                format!("{} `{command}`", job.id)
            })
            .collect()
    }

    /// Emit the interrupt-honesty status naming still-running background
    /// shell jobs. Called on the paths that can classify a turn as
    /// Interrupted, immediately before their `TurnComplete` event.
    async fn emit_interrupted_survivor_status(&self) {
        let survivors = self.running_background_shell_survivors();
        if survivors.is_empty() {
            return;
        }
        let _ = self.send_event(Event::status(format!(
                "Turn interrupted, but {} background shell job(s) continue and may still write files: {}. Use /jobs to inspect or kill.",
                survivors.len(),
                survivors.join(", ")
            )))
            .await;
    }

    /// The pressure estimate (`estimate_input_tokens_for_pressure`) over the
    /// installed history: the number compaction receipts, the refusal trace
    /// and the context-budget snapshot report, equal to what the gate and the
    /// meter read. Not the 1.5x overflow guard.
    fn estimated_input_tokens(&mut self) -> usize {
        // Memoized on (session.messages_revision, system-prompt fingerprint).
        // The cache invalidates as soon as either input changes; until then
        // repeated calls (capacity checkpoints, /status, context inspector,
        // TUI footer) all hit the cached value.
        self.token_estimate_cache.lookup_or_compute(
            self.session.messages_revision,
            self.session.system_prompt.as_ref(),
            &self.session.messages,
        )
    }

    /// Role/type model map for sub-agent runtimes: roster member pins first,
    /// then explicit `[subagents]` overrides on top so explicit config wins
    /// (#fleet-roster cutover (v0.8.67)).
    fn subagent_role_models(&self) -> HashMap<String, crate::config::SubagentModelOverride> {
        let mut models = self.config.fleet_roster.model_overrides();
        models.extend(
            self.config
                .subagent_model_overrides
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
        models
    }

    fn build_tool_context(&self, mode: AppMode, auto_approve: bool) -> ToolContext {
        let authority = TurnAuthority::from_effective_fields(
            mode,
            self.session.allow_shell,
            self.session.trust_mode,
            auto_approve,
            self.session.approval_mode,
        );
        let route = TurnRouteContext {
            provider: self.api_provider,
            model: self.session.model.clone(),
            capabilities: self.active_route_capabilities,
            limits: self.active_route_limits,
            client: self.codewhale_client.clone(),
            api_config: Box::new(self.api_config.clone()),
            locale_tag: self.config.locale_tag.clone(),
            role_models: self.subagent_role_models(),
            auto_model: self.session.auto_model,
            reasoning_effort: self.session.reasoning_effort.clone(),
            reasoning_effort_auto: self.session.reasoning_effort_auto,
        };
        self.build_tool_context_for_turn(&authority, &route)
    }

    /// Build a child runtime from the installed session route, outside any
    /// turn, for operator follow-ups that continue a child from its checkpoint
    /// (`Op::FollowUpSubAgent`). Mirrors the per-turn runtime the `agent` tool
    /// receives, minus the turn-scoped fork context and mailbox barrier: a
    /// continued fork is a background child of the session, not of a turn.
    fn off_turn_subagent_runtime(&self) -> Option<SubAgentRuntime> {
        let client = self.codewhale_client.clone()?;
        let mode = self.current_mode;
        let allow_shell = self.session.allow_shell && !matches!(mode, AppMode::Plan);
        let shell_policy = shell_policy_for_mode(mode, allow_shell);
        let tool_context = self.build_tool_context(mode, self.session.auto_approve);
        let mut rt = SubAgentRuntime::new(
            client,
            self.session.model.clone(),
            tool_context,
            allow_shell,
            Some(self.tx_event.clone()),
            Arc::clone(&self.subagent_manager),
        )
        .with_locale_tag(self.config.locale_tag.clone())
        .with_role_models(self.subagent_role_models())
        .with_api_config(self.api_config.clone())
        .with_execution_limits(self.subagent_execution_limits.clone())
        .with_auto_model(self.session.auto_model)
        .with_reasoning_effort(
            self.session.reasoning_effort.clone(),
            self.session.reasoning_effort_auto,
        )
        .with_agent_tool_surface_options(self.agent_tool_surface_options(shell_policy))
        .with_max_spawn_depth(self.config.max_spawn_depth)
        .with_step_api_timeout(self.config.subagent_api_timeout)
        .with_speech_output_dir(self.config.speech_output_dir.clone())
        .with_mcp_pool(self.mcp_pool.clone())
        .with_todos(self.config.todos.clone())
        .with_parent_completion_tx(self.tx_subagent_completion.clone())
        .with_parent_compaction(&self.config.compaction)
        .with_parent_mode(mode)
        .with_approval_receipt_store(self.approval_receipt_store.clone())
        .with_permission_posture(
            Arc::clone(&self.shared_auto_review_policy),
            self.config.terminal_chrome_enabled,
        );
        if matches!(mode, AppMode::Plan) {
            rt.worker_profile = WorkerRuntimeProfile::for_role(FleetRole::Planner);
        }
        rt.worker_profile.denied_tools = self.config.disallowed_tools.clone().unwrap_or_default();
        Some(rt)
    }

    /// Project the current engine authority onto an already-built registry.
    /// Registries own long-lived services and tool definitions; permission,
    /// shell, and sandbox policy are live turn state and must not be read from
    /// the registry's start-of-turn snapshot after a Runtime posture switch.
    fn live_tool_context(
        &self,
        registry: Option<&crate::tools::ToolRegistry>,
    ) -> Option<ToolContext> {
        let mut context = registry?.context().clone();
        let authority = TurnAuthority::from_effective_fields(
            self.current_mode,
            self.session.allow_shell,
            self.session.trust_mode,
            self.session.auto_approve,
            self.session.approval_mode,
        );
        project_turn_authority(
            &mut context,
            &authority,
            &self.session.workspace,
            self.api_config.sandbox_mode.as_deref(),
            crate::core::authority::SandboxNetworkAccess::from_config(
                self.api_config.sandbox_network_access,
            ),
        );
        context.turn_deadline = self.nested_work_deadline();
        Some(context)
    }

    /// One absolute bound for admitted nested work, resolved after any human
    /// wait so approval time remains excluded by the Engine's clock.
    fn nested_work_deadline(&self) -> Option<tokio::time::Instant> {
        tokio::time::Instant::now().checked_add(
            self.turn_wall_clock
                .budget()
                .saturating_sub(self.turn_wall_clock.spent()),
        )
    }

    /// The network decider this turn runs under: the live policy, re-read from
    /// the document the session was launched with.
    ///
    /// `self.config.network_policy` is the snapshot `Engine::new` took when the
    /// session was spawned. `/network allow <host>` edits `config.toml` and
    /// promises "Retry the command now.", but it cannot reach that snapshot —
    /// so without this re-read the host stayed refused until the next engine
    /// spawn. Same shape as the workspace trust list loaded below, which also
    /// re-reads per tool-context build so `/trust add` lands mid-session; a
    /// hand edit of `[network]` lands through it too.
    ///
    /// Both directions have to land. `/network deny <host>` writes the same
    /// `[network]` table `/network allow` does, and a session that started
    /// without one adopts it the moment it appears — otherwise tightening a
    /// policy mid-session would need the restart this exists to remove.
    ///
    /// A session that started gated keeps the policy it holds when the document
    /// stops carrying a `[network]` table, so removing the table cannot leave a
    /// session ungated by accident.
    ///
    /// The session cache rides along on a refresh, so a host approved through
    /// the approval prompt survives it. An adopted table has no cache to
    /// inherit: the session had no decider, so nothing was ever approved under
    /// one.
    fn current_network_decider(&self) -> Option<crate::network_policy::NetworkPolicyDecider> {
        let Some(path) = self.api_config.loaded_config_path.as_deref() else {
            return self.config.network_policy.clone();
        };
        let Some(document) = crate::config::network_policy_from_document(path) else {
            // The document carries no `[network]` table. Keep what the session
            // resolved: removing a table must not ungate a gated run.
            return self.config.network_policy.clone();
        };
        match self.config.network_policy.as_ref() {
            // A managed overlay or a Fleet denial produced this policy, so the
            // document is a lower layer: it may add hosts, but it may not widen
            // the fallback or lift a denial.
            Some(decider)
                if decider.is_authoritative() || self.api_config.network_layer_is_managed =>
            {
                Some(
                    decider
                        .with_policy_refreshed(decider.policy().folded_with_lower_layer(document)),
                )
            }
            Some(decider) => Some(decider.with_policy_refreshed(document)),
            // Nothing resolved a policy for this session, so there is no higher
            // layer to protect. Adopt the table the moment it exists — that is
            // how `/network deny <host>` lands where there was no policy yet.
            None => Some(crate::network_policy::NetworkPolicyDecider::with_default_audit(document)),
        }
    }

    /// Build one tool context from the already-resolved turn authority and
    /// route. A preview owns values that are deliberately not installed on the
    /// session; rebuilding either from `self.session` would give it the prior
    /// turn's shell posture, context window, model, route capabilities, and
    /// provider-native search client.
    fn build_tool_context_for_turn(
        &self,
        authority: &TurnAuthority,
        route: &TurnRouteContext,
    ) -> ToolContext {
        // Load the per-workspace trusted-paths list (#29) on every tool-context
        // build. Cheap (a small JSON file) and always reflects the latest
        // `/trust add` / `/trust remove` mutations without an explicit cache
        // refresh hook.
        if let Some(child) = self.child_host.as_ref() {
            let mut context = child
                .authority
                .context()
                .with_route_context_window(crate::route_budget::route_context_window_tokens(
                    route.provider,
                    &route.model,
                    route.limits,
                ))
                .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
                    self.session.id.clone(),
                    route.model.clone(),
                    self.session.workspace.clone(),
                    self.session.system_prompt.clone(),
                    self.session.messages.snapshot(),
                ))
                .with_cancel_token(self.cancel_token.clone())
                .with_origin_turn_id(self.turn_counter.to_string());
            context.route_capabilities = route.capabilities;
            context.provider_native_search =
                if route.capabilities.server_side_web_search.is_supported()
                    && self.config.search_native != Some(false)
                {
                    route
                        .client
                        .clone()
                        .and_then(crate::client::ProviderNativeSearchClient::new)
                } else {
                    None
                };
            context.acp_host = self.is_acp_turn().then_some(authority.mode);
            context.refresh_live_posture();
            return context;
        }
        let trusted = crate::workspace_trust::WorkspaceTrust::load_for(&self.session.workspace);
        let mut trusted_external_paths = trusted.paths().to_vec();
        let clipboard_images_dir = crate::config::clipboard_images_dir(&self.session.workspace);
        if !trusted_external_paths
            .iter()
            .any(|path| path == &clipboard_images_dir)
        {
            trusted_external_paths.push(clipboard_images_dir);
        }
        let mut ctx = ToolContext::with_auto_approve(
            self.session.workspace.clone(),
            authority.trust_mode,
            self.session.notes_path.clone(),
            self.session.mcp_config_path.clone(),
            authority.auto_approve,
        )
        .with_state_namespace(self.session.id.clone())
        .with_route_context_window(crate::route_budget::route_context_window_tokens(
            route.provider,
            &route.model,
            route.limits,
        ))
        .with_features(self.config.features.clone())
        .with_shell_manager(self.shell_manager.clone())
        .with_file_read_tracker(self.file_read_tracker.clone())
        .with_runtime_services(self.config.runtime_services.clone())
        .with_skills_config(
            self.config.skills_dir.clone(),
            self.config.skills_discovery_mode,
        )
        .with_plugin_registry(Arc::clone(&self.plugin_registry))
        .with_session_objects(crate::rlm::session::SessionObjectSnapshot::new(
            self.session.id.clone(),
            route.model.clone(),
            self.session.workspace.clone(),
            self.session.system_prompt.clone(),
            self.session.messages.snapshot(),
        ))
        .with_cancel_token(self.cancel_token.clone())
        .with_shell_policy(authority.shell_policy())
        .with_trusted_external_paths(trusted_external_paths)
        .with_follow_symlinks(self.config.workspace_follow_symlinks);
        ctx.acp_host = self.is_acp_turn().then_some(authority.mode);
        ctx.disallowed_tools = self.config.disallowed_tools.clone().unwrap_or_default();
        ctx.persist_services_enabled = self.config.runtime_services.persist_services_enabled;
        // A tool that starts work of its own (a durable task) pins the posture
        // this turn was authorized under, so the work cannot silently run wider
        // or narrower than the session that asked for it.
        ctx.approval_mode = authority.approval_mode;

        // Hand the user-memory path to tools so the model-callable
        // `remember` tool can append entries (#489). `None` when the
        // feature is disabled — tools short-circuit on that.
        if self.config.memory_enabled {
            ctx.memory_path = Some(self.config.memory_path.clone());
        }

        if let Some(decider) = self.current_network_decider() {
            ctx = ctx.with_network_policy(decider);
        }

        // Adaptive evidence routing is engine-native and opt-in
        // (`CODEWHALE_ADAPTIVE_OUTPUT_ROUTING`); `[workshop]` only customizes
        // thresholds. The router stays attached so an enabled process stamps
        // routing metadata without rebuilding the context.
        let router = crate::tools::large_output_router::LargeOutputRouter::new(
            self.config.workshop.clone().unwrap_or_default(),
        );
        ctx = ctx.with_large_output_router(router);

        // Wire the external sandbox backend (#516). exec_shell checks this
        // field and routes commands through the backend instead of spawning
        // a local process when it's set.
        if let Some(backend) = self.sandbox_backend.as_ref() {
            ctx = ctx.with_sandbox_backend(std::sync::Arc::clone(backend));
        }

        // Wire search provider config.
        ctx.search_provider = self.config.search_provider;
        ctx.search_api_key = self.config.search_api_key.clone();
        ctx.search_base_url = self.config.search_base_url.clone();
        ctx.route_capabilities = route.capabilities;
        if route.capabilities.server_side_web_search.is_supported()
            && self.config.search_native != Some(false)
        {
            ctx.provider_native_search = route
                .client
                .as_ref()
                .cloned()
                .and_then(crate::client::ProviderNativeSearchClient::new);
        }

        let network_access = crate::core::authority::SandboxNetworkAccess::from_config(
            self.api_config.sandbox_network_access,
        );
        let policy = authority.sandbox_policy(
            &self.session.workspace,
            self.api_config.sandbox_mode.as_deref(),
            network_access,
        );
        let mut ctx = ctx.with_elevated_sandbox_policy(policy);
        ctx.live_posture = self
            .child_host
            .as_ref()
            .and_then(|child| child.authority.runtime.context.live_posture.clone())
            .or_else(|| {
                Some(LivePosture {
                    state: Arc::clone(&self.live_runtime_authority),
                    workspace: self.session.workspace.clone(),
                    network_access,
                })
            });
        if matches!(authority.mode, AppMode::Plan) {
            ctx = ctx.with_shell_network_denied_hint(PLAN_SHELL_NETWORK_DENIED_HINT);
        }
        ctx
    }

    /// Revalidate durable owners after a saved session is installed. Owner
    /// stores apply restart recovery first; the graph consumes only their
    /// monotonic snapshots and never infers liveness from prior UI state.
    async fn reconcile_restored_work_bindings(&self) {
        let Some(work) = self.config.runtime_services.work.as_ref() else {
            return;
        };
        let session_id = self.session.id.as_str();
        let candidates = work
            .reconcilable_durable_bindings(Some(session_id))
            .into_iter()
            .collect::<HashSet<_>>();
        let checked_at = chrono::Utc::now().timestamp_millis();

        let mut seen_tasks = HashSet::new();
        let mut task_inventory_available = false;
        if let Some(task_manager) = self.config.runtime_services.task_manager.as_ref() {
            match task_manager
                .list_tasks_for_owner(None, None, session_id)
                .await
            {
                Ok(tasks) => {
                    task_inventory_available = true;
                    for task in tasks {
                        let external = format!("task:{}", task.id);
                        if !candidates.contains(&external) {
                            continue;
                        }
                        seen_tasks.insert(external.clone());
                        if !task.execution_binding_known {
                            continue;
                        }
                        if let Err(err) = work.reconcile_operation(
                            session_id,
                            crate::work_graph::task_owner_snapshot(
                                &task.id,
                                task.status,
                                task.lifecycle_seq,
                                task.created_at,
                                task.started_at,
                                task.ended_at,
                            ),
                        ) {
                            tracing::warn!(task_id = %task.id, error = %err, "failed to reconcile restored task owner");
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "Task owner inventory unavailable; retaining Work bindings")
                }
            }
        }
        for external in candidates
            .iter()
            .filter(|_| task_inventory_available)
            .filter(|external| external.starts_with("task:"))
            .filter(|external| !seen_tasks.contains(*external))
        {
            if let Err(err) = work.reconcile_observation(
                session_id,
                external,
                crate::work_graph::OperationObservation::OwnerMissing { checked_at },
            ) {
                tracing::warn!(%external, error = %err, "failed to mark missing task owner");
            }
        }

        let worker_records = self.subagent_manager.read().await.list_worker_records();
        let mut seen_workers = HashSet::new();
        for record in worker_records {
            let Some(snapshot) = agent_worker_owner_snapshot(&record) else {
                continue;
            };
            if !candidates.contains(&snapshot.external) {
                continue;
            }
            seen_workers.insert(snapshot.external.clone());
            if let Err(err) = work.reconcile_operation(session_id, snapshot) {
                tracing::warn!(worker_id = %record.spec.worker_id, error = %err, "failed to reconcile restored worker owner");
            }
        }
        for external in candidates
            .iter()
            .filter(|external| external.starts_with("worker:"))
            .filter(|external| !seen_workers.contains(*external))
        {
            if let Err(err) = work.reconcile_observation(
                session_id,
                external,
                crate::work_graph::OperationObservation::OwnerMissing { checked_at },
            ) {
                tracing::warn!(%external, error = %err, "failed to mark missing worker owner");
            }
        }

        if let Err(err) = crate::tools::workflow::reconcile_persisted_workflow_bindings(
            work,
            session_id,
            &self.session.workspace,
        ) {
            tracing::warn!(error = %err, "failed to reconcile restored workflow owners");
        }
    }

    async fn ensure_mcp_pool(&mut self) -> Result<Arc<AsyncMutex<McpPool>>, ToolError> {
        if let Some(pool) = self.mcp_pool.clone() {
            pool.lock()
                .await
                .bind_caller_plugins(Arc::clone(&self.plugin_registry))
                .map_err(|error| ToolError::not_available(error.to_string()))?;
            self.ensure_mcp_supervisor();
            return Ok(pool);
        }
        // C02-18: misconfiguration fails loud. A missing file is an ordinary
        // empty config (`load_config` returns the default); an unreadable or
        // malformed one still yields an empty, source-aware pool so a fixed
        // file and `/mcp reload` recover in-process — but the person is told
        // that the configured servers are gone, not left to find no tools.
        let mut load_failure = None;
        let mut pool = McpPool::from_config_path_with_workspace_and_plugins(
            &self.session.mcp_config_path,
            &self.session.workspace,
            Arc::clone(&self.plugin_registry),
        )
        .unwrap_or_else(|e| {
            let reason = crate::mcp::format_mcp_error_for_display(&e);
            tracing::warn!("MCP config unavailable: {reason}");
            load_failure = Some(reason);
            McpPool::empty_with_workspace_config_sources(
                &self.session.mcp_config_path,
                &self.session.workspace,
                Arc::clone(&self.plugin_registry),
            )
            .unwrap_or_else(|fallback_error| {
                tracing::warn!(
                    "MCP reload source setup failed: {}",
                    crate::mcp::format_mcp_error_for_display(&fallback_error)
                );
                McpPool::new(McpConfig::default())
            })
        });
        if let Some(reason) = load_failure {
            let _ = self.send_event(Event::status(format!(
                    "MCP config could not be loaded, so none of its servers are available: {reason}. Fix it, then run /mcp reload."
                )))
                .await;
        }
        pool = pool.with_backend(crate::mcp::McpBackend::from_config(&self.api_config));
        pool = pool.with_disallowed_tools(self.config.disallowed_tools.clone().unwrap_or_default());
        if let Some(decider) = self.current_network_decider() {
            pool = pool.with_network_policy(decider);
        }
        // The self-serve login tool honors the same pre-registered redirect
        // overrides `/mcp login` uses, or providers with pinned callback
        // URIs reject its ephemeral loopback.
        pool = pool.with_oauth_callback(
            self.config.mcp_oauth_callback_port,
            self.config.mcp_oauth_callback_url.clone(),
        );
        let pool = Arc::new(AsyncMutex::new(pool));
        self.mcp_pool = Some(Arc::clone(&pool));
        self.ensure_mcp_supervisor();
        Ok(pool)
    }

    /// Start the connection supervisor once per pool. The task holds only a
    /// Weak between sweeps, but an in-flight handshake holds the pool. Keep
    /// its abort handle so a boundary stops that work and queued diagnoses.
    fn ensure_mcp_supervisor(&mut self) {
        if self.child_host.is_some() || self.mcp_supervisor_rx.is_some() {
            return;
        }
        let Some(pool) = self.mcp_pool.as_ref() else {
            return;
        };
        let (tx, rx) = mpsc::channel(16);
        self.mcp_supervisor_rx = Some(rx);
        let weak = Arc::downgrade(pool);
        let task = spawn_supervised(
            "mcp-supervisor",
            std::panic::Location::caller(),
            McpPool::supervise_pool(weak, tx),
        );
        self.mcp_supervisor_task = Some(task.abort_handle());
    }

    /// Apply one supervisor sweep: deaths and failures refresh the engine's
    /// error map, recoveries clear it, parking writes the suspended notice.
    /// Emits a finished boot update exactly when something changed, so the
    /// Extensions rows flip with liveness instead of parking on stale-ready.
    async fn apply_mcp_supervisor_update(&mut self, update: McpSupervisorUpdate) {
        if update.is_empty() {
            return;
        }
        for (name, error) in update.died.into_iter().chain(update.failed) {
            self.mcp_connection_errors.insert(name, error);
        }
        for name in &update.recovered {
            self.mcp_connection_errors.remove(name);
        }
        for name in update.parked {
            self.mcp_connection_errors.insert(
                name.clone(),
                format!(
                    "Auto-reconnect suspended after repeated failures; `/mcp retry {name}` to try again."
                ),
            );
        }
        let generation = self.next_mcp_event_generation();
        self.emit_mcp_session_boot(generation, true).await;
    }

    /// Force the engine-owned pool to re-read its config sources and start
    /// the reconnect pass, returning the interim snapshot immediately.
    ///
    /// This is the explicit `/mcp reload` path. It deliberately does **not**
    /// wait for the connect batch: a config with many servers can take
    /// minutes to settle, and a caller that waited from the TUI starved
    /// input and redraw for the whole batch. The batch is the same
    /// supervised pass session boot already uses, so progress and the
    /// finished receipt arrive as `Event::McpSessionBoot` updates under the
    /// returned generation. A malformed source returns Err **before** any
    /// live connection is dropped.
    async fn reload_mcp_pool(&mut self, config_path: PathBuf) -> anyhow::Result<McpManagerUpdate> {
        if self.mcp_boot_in_flight {
            self.wait_for_mcp_boot().await;
        }
        if config_path != self.session.mcp_config_path {
            // Transactional swap without handshakes under the lock; the
            // forced re-read below then re-dials the freshly installed
            // sources.
            let pool = self
                .ensure_mcp_pool()
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            {
                let mut pool = pool.lock().await;
                pool.switch_workspace_config_source(
                    &config_path,
                    &self.session.workspace,
                    Arc::clone(&self.plugin_registry),
                )?;
            }
            self.session.mcp_config_path = config_path;
        }
        let generation = self
            .start_mcp_session_boot(McpConnectRefresh::Force)
            .await?;
        let snapshot = self.mcp_session_snapshot().await?;
        Ok(McpManagerUpdate {
            snapshot,
            generation,
        })
    }

    async fn mcp_session_snapshot(&self) -> anyhow::Result<crate::mcp::McpManagerSnapshot> {
        let pool = self
            .mcp_pool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MCP pool is not started"))?;
        let pool = pool.lock().await;
        Ok(pool.manager_snapshot(
            &self.session.mcp_config_path,
            false,
            &self.mcp_connection_errors,
        ))
    }

    fn mcp_connecting_names(pool: &McpPool, errors: &HashMap<String, String>) -> Vec<String> {
        // The pool tracks spawned-but-unresolved connects (#6033). Inferring
        // "connecting" from enabled-minus-connected mislabels every on-demand
        // server — `lazy = true` or plugin-contributed, never started — as
        // mid-handshake.
        pool.connecting_servers()
            .into_iter()
            .filter(|name| !errors.contains_key(name))
            .collect()
    }

    fn next_mcp_event_generation(&mut self) -> u64 {
        self.mcp_event_generation = self.mcp_event_generation.saturating_add(1);
        self.mcp_event_generation
    }

    fn replace_mcp_boot_errors(
        &mut self,
        authority_errors: &HashMap<String, String>,
        mut connection_errors: HashMap<String, String>,
    ) {
        // Each update owns the ordinary connection diagnoses for this pass,
        // so replacing the map drops stale transport errors. Reviewed-plugin
        // authority failures are a separate, non-pending set and must remain
        // visible throughout the pass; they win if a name ever overlaps.
        connection_errors.extend(authority_errors.clone());
        self.mcp_connection_errors = connection_errors;
    }

    fn finish_mcp_boot_generation(&mut self, generation: u64) -> bool {
        if self.mcp_boot_generation != Some(generation) {
            return false;
        }
        self.mcp_boot_generation = None;
        self.mcp_boot_task = None;
        self.mcp_boot_in_flight = false;
        self.mcp_boot_rx = None;
        self.mcp_boot_done = None;
        true
    }

    /// Drop the engine-owned MCP pool at a session or workspace boundary.
    /// The in-flight boot pass dials into the dropped pool: abort it and
    /// clear its generation, receiver and diagnoses, so the next pool starts
    /// clean instead of waiting on — or adopting progress and connection
    /// errors from — the previous conversation's pass (C02-08).
    fn drop_mcp_pool(&mut self) {
        self.mcp_pool = None;
        if let Some(task) = self.mcp_boot_task.take() {
            task.abort();
        }
        if let Some(task) = self.mcp_supervisor_task.take() {
            task.abort();
        }
        self.mcp_supervisor_rx = None;
        self.mcp_boot_generation = None;
        self.mcp_boot_in_flight = false;
        self.mcp_boot_rx = None;
        self.mcp_boot_done = None;
        self.mcp_connection_errors.clear();
    }

    async fn emit_mcp_session_boot(&self, generation: u64, finished: bool) {
        let Ok(snapshot) = self.mcp_session_snapshot().await else {
            return;
        };
        let connecting = if finished {
            Vec::new()
        } else if let Some(pool) = self.mcp_pool.as_ref() {
            let pool = pool.lock().await;
            Self::mcp_connecting_names(&pool, &self.mcp_connection_errors)
        } else {
            Vec::new()
        };
        // Zero servers and nothing connecting is not a session-boot surface.
        if snapshot.servers.is_empty() && connecting.is_empty() {
            return;
        }
        let _ = self.tx_event.try_send(Event::McpSessionBoot {
            generation,
            snapshot,
            connecting,
            finished,
        });
    }

    async fn apply_mcp_boot_update(&mut self, update: McpBootUpdate) {
        match update {
            McpBootUpdate::Progress {
                generation,
                authority_errors,
                connection_errors,
                connecting,
            } => {
                if self.mcp_boot_generation != Some(generation) {
                    return;
                }
                if generation < self.mcp_event_generation {
                    return;
                }
                self.mcp_event_generation = generation;
                self.replace_mcp_boot_errors(&authority_errors, connection_errors);
                self.session.pending_prefix_change_reason = Some("mcp-session-boot".to_string());
                if let Ok(snapshot) = self.mcp_session_snapshot().await {
                    let _ = self.tx_event.try_send(Event::McpSessionBoot {
                        generation,
                        snapshot,
                        connecting,
                        finished: false,
                    });
                }
            }
            McpBootUpdate::Finished {
                generation,
                authority_errors,
                connection_errors,
            } => {
                if self.mcp_boot_generation != Some(generation) {
                    return;
                }
                if generation < self.mcp_event_generation {
                    self.finish_mcp_boot_generation(generation);
                    return;
                }
                self.mcp_event_generation = generation;
                self.replace_mcp_boot_errors(&authority_errors, connection_errors);
                self.finish_mcp_boot_generation(generation);
                self.session.pending_prefix_change_reason = Some("mcp-session-boot".to_string());
                self.emit_mcp_session_boot(generation, true).await;
            }
        }
    }

    async fn drain_mcp_boot_updates(&mut self) {
        let receiver_generation = self.mcp_boot_generation;
        let Some(mut rx) = self.mcp_boot_rx.take() else {
            return;
        };
        while let Ok(update) = rx.try_recv() {
            // Apply without emitting until the last queued update so the UI
            // sees one settled receipt rather than a burst.
            match update {
                McpBootUpdate::Progress {
                    generation,
                    authority_errors,
                    connection_errors,
                    connecting: _,
                } => {
                    if self.mcp_boot_generation != Some(generation) {
                        continue;
                    }
                    if generation < self.mcp_event_generation {
                        continue;
                    }
                    self.mcp_event_generation = generation;
                    self.replace_mcp_boot_errors(&authority_errors, connection_errors);
                    self.session.pending_prefix_change_reason =
                        Some("mcp-session-boot".to_string());
                }
                McpBootUpdate::Finished {
                    generation,
                    authority_errors,
                    connection_errors,
                } => {
                    if self.mcp_boot_generation != Some(generation) {
                        continue;
                    }
                    if generation < self.mcp_event_generation {
                        self.finish_mcp_boot_generation(generation);
                        break;
                    }
                    self.mcp_event_generation = generation;
                    self.replace_mcp_boot_errors(&authority_errors, connection_errors);
                    self.finish_mcp_boot_generation(generation);
                    self.session.pending_prefix_change_reason =
                        Some("mcp-session-boot".to_string());
                    break;
                }
            }
        }
        if self.mcp_boot_in_flight && self.mcp_boot_generation == receiver_generation {
            self.mcp_boot_rx = Some(rx);
        }
    }

    /// How long a caller will block on the session boot before proceeding with
    /// whatever has connected so far.
    ///
    /// This bounds the *wait*, never the boot: the supervised boot task keeps
    /// running, so a slow server still lands through the normal progress
    /// updates and appears once it is ready. The per-server connect timeout
    /// does not cover everything that can stall a stdio server — `npx -y` and
    /// `uvx` download their package on first run — so without an outer bound a
    /// single cold fetch left `/mcp` waiting on `mcp_boot_done` forever, which
    /// reads to the user as a frozen application.
    const MCP_BOOT_UI_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

    async fn wait_for_mcp_boot(&mut self) {
        if let Some(rx) = self.mcp_boot_done.as_mut() {
            let settled = tokio::time::timeout(Self::MCP_BOOT_UI_WAIT, async {
                while !*rx.borrow() {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
            if settled.is_err() {
                // Not an error: the boot continues in the background and its
                // progress updates still arrive. Say so rather than silently
                // returning a short server list as if it were complete.
                tracing::info!(
                    wait_secs = Self::MCP_BOOT_UI_WAIT.as_secs(),
                    "MCP session boot still connecting; continuing with the servers ready so far"
                );
            }
        }
        self.drain_mcp_boot_updates().await;
    }

    /// `tools_always_load` plus a turn's `allowed_tools`, normalized to the
    /// lowercase `mcp_*` names the selection grammar uses.
    fn explicit_mcp_tool_names(&self, allowed_tools: Option<&[String]>) -> Vec<String> {
        self.config
            .tools_always_load
            .iter()
            .chain(allowed_tools.into_iter().flatten())
            .map(|name| name.trim().to_ascii_lowercase())
            .filter(|name| name.starts_with("mcp_"))
            .collect()
    }

    /// Explicit MCP tool selections need their schemas on the first request.
    /// Under lazy boot a selected server may never have been started, so this
    /// begins those connects itself — off the mailbox — and then waits on the
    /// boot pass and the explicit connects together under the one deadline.
    async fn wait_for_explicit_mcp_boot(&mut self, allowed_tools: Option<&[String]>) {
        let requested = self.explicit_mcp_tool_names(allowed_tools);
        let Some(pool) = self.mcp_pool.as_ref() else {
            return;
        };
        let names = pool
            .lock()
            .await
            .explicitly_selected_server_names(&requested);
        self.wait_for_named_mcp_boot(&names, Self::MCP_BOOT_UI_WAIT, None)
            .await;
    }

    /// One bounded connection wait for explicit selection and actual discovery.
    /// The names are existing pool identities, never guessed tool suffixes.
    async fn wait_for_named_mcp_boot(
        &mut self,
        names: &[String],
        wait: Duration,
        withdraw: Option<&CancellationToken>,
    ) {
        if names.is_empty() {
            return;
        }
        // A turn must start even when a selected server never answers. An
        // unreachable or un-authenticated MCP server is an ordinary state, not
        // an exceptional one, so waiting without a deadline here turned one bad
        // row in the config into an unresponsive session. Past the deadline the
        // turn proceeds with the tools that are ready; the connects keep
        // running, and the missing server's tools become available on a later
        // turn.
        let deadline = tokio::time::Instant::now() + wait;
        let mut explicit = self.start_named_mcp_connects(names).await;
        let started_explicit = !explicit.names.is_empty();
        if started_explicit {
            // The connects are in flight now — surfaces should show the
            // selected servers as connecting, not configured.
            let generation = self.next_mcp_event_generation();
            self.emit_mcp_session_boot(generation, false).await;
        }
        while self.mcp_boot_in_flight || !explicit.connects.is_empty() {
            if tokio::time::Instant::now() >= deadline {
                tracing::info!(
                    waited_secs = wait.as_secs(),
                    "starting the turn before every selected MCP server is ready"
                );
                break;
            }
            self.drain_mcp_boot_updates().await;
            let Some(pool) = self.mcp_pool.as_ref() else {
                break;
            };
            let connecting =
                Self::mcp_connecting_names(&*pool.lock().await, &self.mcp_connection_errors);
            let needs_schema = connecting.iter().any(|server| names.contains(server));
            if !needs_schema {
                break;
            }
            // The deadline has to cover this await too: a server that accepts
            // the connection and then goes quiet sends no progress update at
            // all, so checking only at the top of the loop would still park the
            // turn here indefinitely.
            enum WaitOutcome {
                Cancel,
                Deadline,
                Boot(Option<McpBootUpdate>),
                Connect(Option<Box<ExplicitConnectJoin>>),
            }
            let outcome = tokio::select! {
                _ = self.cancel_token.cancelled() => WaitOutcome::Cancel,
                () = async {
                    match withdraw {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending().await,
                    }
                } => WaitOutcome::Cancel,
                () = tokio::time::sleep_until(deadline) => WaitOutcome::Deadline,
                update = async {
                    match self.mcp_boot_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => WaitOutcome::Boot(update),
                joined = explicit.connects.join_next(), if !explicit.connects.is_empty() => {
                    WaitOutcome::Connect(joined.map(Box::new))
                }
            };
            match outcome {
                WaitOutcome::Cancel | WaitOutcome::Deadline => break,
                WaitOutcome::Boot(Some(update)) => self.apply_mcp_boot_update(update).await,
                // The boot channel closing means the pass ended without a
                // Finished update; explicit connects may still be running.
                WaitOutcome::Boot(None) => {
                    if let Some(generation) = self.mcp_boot_generation {
                        self.finish_mcp_boot_generation(generation);
                    }
                }
                WaitOutcome::Connect(Some(joined)) => {
                    self.store_explicit_connect_result(&mut explicit, *joined)
                        .await;
                }
                WaitOutcome::Connect(None) => {}
            }
        }
        if !explicit.connects.is_empty() {
            explicit.connects.abort_all();
            if let Some(pool) = self.mcp_pool.as_ref() {
                pool.lock().await.cancel_connecting(&explicit.names);
            }
        }
        if started_explicit {
            // Close out the in-flight marks: a deadline-aborted server must
            // stop reading "connecting" on the next paint.
            let generation = self.next_mcp_event_generation();
            self.emit_mcp_session_boot(generation, !self.mcp_boot_in_flight)
                .await;
        }
    }

    /// Start connects for servers an explicit tool selection covers but the
    /// boot pass left lazy (#6033). Selection is intent: cooldowns do not
    /// apply, but enabled/allowed/plugin authority checks do.
    async fn start_named_mcp_connects(&mut self, names: &[String]) -> ExplicitMcpConnects {
        let mut state = ExplicitMcpConnects {
            connects: tokio::task::JoinSet::new(),
            names: HashSet::new(),
            catalog_generation: 0,
        };
        let Some(pool) = self.mcp_pool.as_ref() else {
            return state;
        };
        let (pending, errors, timeouts, network_policy, generation, backend) = {
            let mut pool = pool.lock().await;
            let (pending, errors) = pool.take_pending_connects_for(names);
            (
                pending,
                errors,
                pool.connect_timeouts(),
                pool.cloned_network_policy(),
                pool.current_catalog_generation(),
                pool.backend(),
            )
        };
        for (name, error) in errors {
            self.mcp_connection_errors
                .insert(name, crate::mcp::format_mcp_error_for_display(&error));
        }
        state.catalog_generation = generation;
        state.names = pending.iter().map(|(name, _)| name.clone()).collect();
        state.connects =
            McpPool::spawn_pending_connects(pending, timeouts, network_policy, generation, backend);
        state
    }

    /// Store one resolved explicit connect under the same authority
    /// discipline as the boot pass: a config reload mid-handshake invalidates
    /// the rest of the batch instead of letting old-authority results land.
    async fn store_explicit_connect_result(
        &mut self,
        explicit: &mut ExplicitMcpConnects,
        joined: ExplicitConnectJoin,
    ) {
        let (name, result) =
            joined.unwrap_or_else(|error| ("connection task".to_string(), Err(error.into())));
        explicit.names.remove(&name);
        let Some(pool) = self.mcp_pool.as_ref() else {
            return;
        };
        {
            let mut pool = pool.lock().await;
            let reload = pool.reload_if_config_changed().await;
            if reload.is_err() || pool.current_catalog_generation() != explicit.catalog_generation {
                explicit.connects.abort_all();
                // `name` already left `names` above — its mark dies with the
                // aborted batch too.
                explicit.names.insert(name);
                pool.cancel_connecting(&explicit.names);
                explicit.names.clear();
                if let Err(error) = reload {
                    self.mcp_connection_errors.insert(
                        "configuration".to_string(),
                        crate::mcp::format_mcp_error_for_display(&error),
                    );
                }
                return;
            }
            let result =
                result.and_then(|connection| pool.store_ready_connection(name.clone(), connection));
            match result {
                Ok(()) => {
                    self.mcp_connection_errors.remove(&name);
                }
                Err(error) => {
                    pool.note_connect_failure(&name, &error);
                    self.mcp_connection_errors
                        .insert(name, crate::mcp::format_mcp_error_for_display(&error));
                }
            }
        }
        self.session.pending_prefix_change_reason = Some("mcp-session-boot".to_string());
        // A resolved selection connect refreshes the server surfaces the
        // same way a `/mcp` retry does, without waiting for the boot pass.
        let generation = self.next_mcp_event_generation();
        self.emit_mcp_session_boot(
            generation,
            !self.mcp_boot_in_flight && explicit.connects.is_empty(),
        )
        .await;
    }

    /// Start the concurrent connect pass without occupying the engine mailbox.
    /// Optional servers stay in the background unless the task explicitly
    /// selects their tools; `mcp_tools` snapshots whatever is already ready.
    ///
    /// Returns the event generation the pass owns, or `Ok(0)` when nothing
    /// was started (the feature gate, or a child engine sharing its parent's
    /// pool, skipped a session boot). Progress and the finished receipt flow
    /// as `Event::McpSessionBoot` updates under that generation.
    async fn start_mcp_session_boot(&mut self, refresh: McpConnectRefresh) -> anyhow::Result<u64> {
        if matches!(refresh, McpConnectRefresh::IfChanged)
            && (self.child_host.is_some() || !self.config.features.enabled(Feature::Mcp))
        {
            // Nothing to start. The only caller that reads the generation is
            // the explicit reload, which never takes this branch.
            return Ok(0);
        }
        let pool = match self.ensure_mcp_pool().await {
            Ok(pool) => pool,
            Err(error) => {
                if matches!(refresh, McpConnectRefresh::Force) {
                    return Err(anyhow::anyhow!(error.to_string()));
                }
                tracing::debug!("MCP session boot skipped: {error}");
                return Ok(0);
            }
        };

        // Boot connects every configured server in the background except
        // those marked `lazy = true`, which connect on demand — a selected
        // turn, a `/mcp` connect, or a lazy tool-name resolution.
        let requested = self.explicit_mcp_tool_names(self.config.allowed_tools.as_deref());
        let (pending, auth_errors, timeouts, network_policy, catalog_generation, backend) = {
            let mut pool = pool.lock().await;
            match refresh {
                McpConnectRefresh::IfChanged => {
                    if let Err(error) = pool.reload_if_config_changed().await {
                        tracing::debug!(
                            "MCP session boot config reload failed: {}",
                            crate::mcp::format_mcp_error_for_display(&error)
                        );
                    }
                }
                // A malformed source returns Err before anything is dropped,
                // so a failed explicit reload leaves the live pool intact.
                McpConnectRefresh::Force => pool.force_reload_config_sources()?,
            }
            let eager = pool.eager_boot_server_names(&requested);
            let (pending, auth_errors) = pool.collect_pending_connects(Some(&eager));
            (
                pending,
                auth_errors,
                pool.connect_timeouts(),
                pool.cloned_network_policy(),
                pool.current_catalog_generation(),
                pool.backend(),
            )
        };

        let authority_errors = auth_errors
            .into_iter()
            .map(|(name, error)| (name, crate::mcp::format_mcp_error_for_display(&error)))
            .collect::<HashMap<_, _>>();
        self.mcp_connection_errors = authority_errors.clone();
        let authority_errors = Arc::new(authority_errors);
        let generation = self.next_mcp_event_generation();

        if pending.is_empty() {
            self.mcp_boot_in_flight = false;
            self.mcp_boot_generation = None;
            self.emit_mcp_session_boot(generation, true).await;
            return Ok(generation);
        }

        self.mcp_boot_in_flight = true;
        self.mcp_boot_generation = Some(generation);
        let (progress_tx, progress_rx) = mpsc::channel(MCP_BOOT_CHANNEL_CAPACITY);
        let (done_tx, done_rx) = tokio::sync::watch::channel(false);
        self.mcp_boot_rx = Some(progress_rx);
        self.mcp_boot_done = Some(done_rx);

        self.emit_mcp_session_boot(generation, false).await;

        let pool_for_task = Arc::clone(&pool);
        let boot_task = spawn_supervised(
            "mcp-session-boot",
            std::panic::Location::caller(),
            async move {
                let mut remaining: HashSet<String> =
                    pending.iter().map(|(name, _)| name.clone()).collect();
                let mut connects = McpPool::spawn_pending_connects(
                    pending,
                    timeouts,
                    network_policy,
                    catalog_generation,
                    backend,
                );
                let mut connection_errors = HashMap::new();
                while let Some(joined) = connects.join_next().await {
                    let (name, result) = joined
                        .unwrap_or_else(|error| ("connection task".to_string(), Err(error.into())));
                    remaining.remove(&name);
                    let connecting = {
                        let mut pool = pool_for_task.lock().await;
                        // A turn may have reloaded the pool while these handshakes
                        // were in flight. Never let their old authority or failures
                        // overwrite the newly installed configuration.
                        let reload = pool.reload_if_config_changed().await;
                        if reload.is_err()
                            || pool.current_catalog_generation() != catalog_generation
                        {
                            connects.abort_all();
                            // `name` already left `remaining` above — its
                            // mark dies with the aborted pass too.
                            remaining.insert(name.clone());
                            pool.cancel_connecting(&remaining);
                            connection_errors.clear();
                            if let Err(error) = reload {
                                connection_errors.insert(
                                    "configuration".to_string(),
                                    crate::mcp::format_mcp_error_for_display(&error),
                                );
                            }
                            break;
                        }
                        let result = result.and_then(|connection| {
                            pool.store_ready_connection(name.clone(), connection)
                        });
                        if let Err(error) = result {
                            pool.note_connect_failure(&name, &error);
                            connection_errors
                                .insert(name, crate::mcp::format_mcp_error_for_display(&error));
                        }
                        // The pool's in-flight set also names connects a turn
                        // started on an explicit selection while this pass was
                        // running — report what is actually connecting.
                        pool.connecting_servers()
                    };
                    let _ = progress_tx.try_send(McpBootUpdate::Progress {
                        generation,
                        authority_errors: Arc::clone(&authority_errors),
                        connection_errors: connection_errors.clone(),
                        connecting,
                    });
                }
                {
                    let pool = pool_for_task.lock().await;
                    let mut required = Vec::new();
                    pool.push_required_server_errors(&mut required);
                    for (name, error) in required {
                        connection_errors
                            .entry(name)
                            .or_insert_with(|| crate::mcp::format_mcp_error_for_display(&error));
                    }
                }
                // The terminal update carries the boot's settlement signal;
                // wait for a slot instead of dropping it (#6147).
                let _ = progress_tx
                    .send(McpBootUpdate::Finished {
                        generation,
                        authority_errors,
                        connection_errors,
                    })
                    .await;
                let _ = done_tx.send(true);
            },
        );
        self.mcp_boot_task = Some(boot_task.abort_handle());

        Ok(generation)
    }

    /// Connect the configured servers through the one engine-owned pool and
    /// snapshot that exact pool for the boot UI. `connect_all` is bounded and
    /// concurrent; already-ready connections are preserved, and unlike the
    /// explicit reload path no config source is force-reloaded.
    async fn bootstrap_mcp_pool(&mut self) -> anyhow::Result<McpManagerUpdate> {
        if self.mcp_pool.is_none() {
            let _ = self.ensure_mcp_pool().await;
        }
        // Wait for the boot, but never without a deadline. Servers that fail
        // fast — a missing binary, a refused connection — are diagnosed in
        // milliseconds, and that diagnosis is the whole value of `/mcp`, so
        // returning before it lands would report an empty picture. Servers that
        // *stall* are the problem: `npx -y` and `uvx` fetch their package on
        // first run, and one cold fetch used to hold the view open forever.
        // Past the deadline the view renders what is known and the background
        // boot keeps running, so slower servers land on a later snapshot.
        if self.mcp_boot_in_flight {
            self.wait_for_mcp_boot().await;
        }
        self.drain_mcp_boot_updates().await;
        let snapshot = self.mcp_session_snapshot().await?;
        let generation = self.next_mcp_event_generation();
        Ok(McpManagerUpdate {
            snapshot,
            generation,
        })
    }

    async fn retry_mcp_server(&mut self, name: &str) -> anyhow::Result<McpManagerUpdate> {
        if self.mcp_boot_in_flight {
            self.wait_for_mcp_boot().await;
        }
        let pool = self
            .ensure_mcp_pool()
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let prepared = {
            let mut pool = pool.lock().await;
            pool.take_retry_connect(name).map(|pending| {
                (
                    pending,
                    pool.connect_timeouts(),
                    pool.cloned_network_policy(),
                    pool.current_catalog_generation(),
                    pool.backend(),
                )
            })
        };
        let attempt = match prepared {
            Ok((pending, timeouts, network_policy, generation, backend)) => {
                let mut connects = McpPool::spawn_pending_connects(
                    vec![pending],
                    timeouts,
                    network_policy,
                    generation,
                    backend,
                );
                let connection = match connects.join_next().await {
                    Some(Ok((_, connection))) => connection,
                    Some(Err(error)) => Err(error.into()),
                    None => Err(anyhow::anyhow!(
                        "MCP connection task did not start for {name}"
                    )),
                };
                Ok((generation, connection))
            }
            Err(error) => Err(error),
        };
        let mut pool = pool.lock().await;
        let result = match attempt {
            Ok((generation, connection)) => {
                let stale = pool.current_catalog_generation() != generation;
                let stored = connection.and_then(|connection| {
                    let tools = connection.tools().len();
                    pool.store_ready_connection(name.to_string(), connection)
                        .map(|()| tools)
                });
                if let Err(error) = &stored {
                    if stale {
                        pool.cancel_connecting(&HashSet::from([name.to_string()]));
                    } else {
                        pool.note_connect_failure(name, error);
                    }
                }
                stored
            }
            Err(error) => Err(error),
        };
        // The outcome is logged here, at the one manager every surface drives,
        // so a panel retry, `/mcp retry`, and the runtime API all leave the
        // same receipt in the session log.
        match result {
            Ok(tools) => {
                tracing::info!(
                    target: "mcp",
                    server = %name,
                    tools,
                    "MCP server connected on retry"
                );
                self.mcp_connection_errors.remove(name);
            }
            Err(error) => {
                let reason = crate::mcp::format_mcp_error_for_display(&error);
                tracing::warn!(
                    target: "mcp",
                    server = %name,
                    error = %reason,
                    "MCP server retry failed"
                );
                self.mcp_connection_errors.insert(name.to_string(), reason);
            }
        }
        let snapshot = pool.manager_snapshot(
            &self.session.mcp_config_path,
            false,
            &self.mcp_connection_errors,
        );
        self.mcp_connection_errors.retain(|server, _| {
            snapshot
                .servers
                .iter()
                .any(|configured| configured.name == *server)
        });
        drop(pool);
        let generation = self.next_mcp_event_generation();
        let _ = self.tx_event.try_send(Event::McpSessionBoot {
            generation,
            snapshot: snapshot.clone(),
            connecting: Vec::new(),
            finished: true,
        });
        Ok(McpManagerUpdate {
            snapshot,
            generation,
        })
    }

    async fn mcp_tools(&mut self) -> Vec<Tool> {
        let pool = match self.ensure_mcp_pool().await {
            Ok(pool) => pool,
            Err(err) => {
                tracing::debug!("MCP tools unavailable: {err}");
                return Vec::new();
            }
        };

        if self.mcp_boot_in_flight {
            // Optional servers are still connecting in the background. Snapshot
            // currently-ready tools so the first LLM call is not serialized
            // behind the slowest handshake. Declare the refresh here as well
            // as on Progress: a ready connection can precede its mailbox update.
            self.session.pending_prefix_change_reason = Some("mcp-session-boot".to_string());
            return pool.lock().await.to_api_tools();
        }

        // Boot is lazy (#6033): unselected servers stay unconnected on
        // purpose, so there is no per-turn sweep here. A `required` server
        // that never got an attempt still owes the session an honest error
        // row — `push_required_server_errors` only fills gaps a real
        // diagnosis did not already cover.
        let mut gaps = Vec::new();
        {
            let pool = pool.lock().await;
            pool.push_required_server_errors(&mut gaps);
        }
        let mut inserted = false;
        for (name, error) in gaps {
            self.mcp_connection_errors.entry(name).or_insert_with(|| {
                inserted = true;
                crate::mcp::format_mcp_error_for_display(&error)
            });
        }
        if inserted {
            // Failures stay on the session-boot snapshot, not as Status toasts.
            let generation = self.next_mcp_event_generation();
            self.emit_mcp_session_boot(generation, true).await;
        }
        pool.lock().await.to_api_tools()
    }

    /// Handle a turn using the DeepSeek API.
    #[allow(clippy::too_many_lines)]
    /// Refresh the stable system prompt based on current non-mode context.
    #[cfg_attr(not(test), expect(dead_code))]
    fn refresh_system_prompt(&mut self) {
        self.refresh_system_prompt_with_reason("system");
    }

    fn refresh_system_prompt_with_reason(&mut self, reason: &str) {
        let context = self.installed_next_turn_prompt_context();
        self.refresh_system_prompt_from_context_with_reason(&context, reason);
    }

    // KV-cache effect: append-only user history. SessionUpdated persists this
    // warning even when an explicit prompt rebuild replaces the system prefix.
    fn record_project_trust_warning(&mut self) {
        let warning = crate::skills::untrusted_project_skills_warning(
            &self.session.workspace,
            Some(&self.config.skills_dir),
            self.config.skills_discovery_mode,
        );
        let previous = self
            .session
            .messages
            .iter()
            .rev()
            .find(|message| crate::runtime_handoff::is_workspace_trust_message(message));
        if warning.is_none() && previous.is_none() {
            return;
        }
        let message = crate::runtime_handoff::workspace_trust_runtime_message(warning.as_deref());
        if previous != Some(&message) {
            self.session.add_message(message);
        }
    }

    /// Record the current mode and its purpose as a runtime notice (KV-cache
    /// effect: append-only user history; the system prompt is byte-identical
    /// across modes).
    ///
    /// Derived from the session log, like the workspace-trust note: a session
    /// with no notice gets one only in Plan, and once a notice exists a new
    /// one is appended whenever the mode differs from the latest, so leaving
    /// Plan is announced too and history never ends on a stale mode. A
    /// compaction that dropped the notice re-records it on the next Plan turn.
    /// Child and RLM hosts get none: their mode is fixed by their parent.
    fn record_mode_notice(&mut self, mode: AppMode) {
        if self.child_host.is_some() || self.rlm_host.is_some() {
            return;
        }
        let previous = self
            .session
            .messages
            .iter()
            .rev()
            .find(|message| crate::runtime_handoff::mode_notice_display(message).is_some());
        let message = crate::runtime_handoff::mode_runtime_message(mode);
        let record = match previous {
            None => mode == AppMode::Plan,
            Some(previous) => previous != &message,
        };
        if record {
            self.session.add_message(message);
        }
    }

    /// Record connected MCP servers' `initialize` guidance in session history
    /// before a model request (KV-cache effect: append-only user history).
    ///
    /// Only servers owning at least one tool in `catalog` — the turn's final
    /// model-facing catalog, already narrowed by the allow/deny posture — are
    /// included. A new event is appended only when the rendered guidance
    /// differs from the latest one in history, so an unchanged server set
    /// never grows the transcript or moves the cached prefix, and guidance a
    /// compaction dropped is re-recorded. The event is persisted with the
    /// session and rendered in the transcript, so what the model saw stays
    /// auditable.
    pub(super) async fn record_mcp_server_instructions(&mut self, catalog: &[Tool]) {
        let visible: std::collections::HashSet<&str> = catalog
            .iter()
            .filter(|tool| crate::mcp::McpPool::is_mcp_tool(&tool.name))
            .map(|tool| tool.name.as_str())
            .collect();
        let servers = match self.mcp_pool.as_ref() {
            Some(pool) if !visible.is_empty() => {
                // Never stall a model request behind a pool busy with a
                // handshake; the next request boundary records it instead.
                let Ok(pool) = pool.try_lock() else {
                    return;
                };
                pool.model_server_instructions(|name| visible.contains(name))
            }
            _ => Vec::new(),
        };
        let previous =
            self.session.messages.iter().rev().find(|message| {
                crate::runtime_handoff::is_mcp_server_instructions_message(message)
            });
        if servers.is_empty() && previous.is_none() {
            return;
        }
        let message = crate::runtime_handoff::mcp_server_instructions_runtime_message(&servers);
        if previous == Some(&message) {
            return;
        }
        self.add_session_message(message).await;
        let status = if servers.is_empty() {
            "MCP server guidance withdrawn from context".to_string()
        } else {
            let names: Vec<&str> = servers.iter().map(|(name, _)| name.as_str()).collect();
            format!(
                "MCP server guidance added to context (shown in transcript): {}",
                names.join(", ")
            )
        };
        let _ = self.send_event(Event::status(status)).await;
    }

    /// Name the configured MCP servers this turn may load, in session history
    /// before a model request (KV-cache effect: append-only user history).
    ///
    /// Session boot is lazy and MCP tools are deferred, so a server the user
    /// added is otherwise invisible to the model until the user names it. The
    /// list is exactly what an MCP-focused `tool_search` may connect under
    /// this turn's ceiling ([`Self::discover_mcp_for_tool_search`]): enabled,
    /// allowed servers, and none when `tool_search` is not in `catalog`.
    /// Names only — nothing is connected and no schema is loaded. Derived
    /// from the session log like the guidance above, so an unchanged list is
    /// recorded once, a changed one supersedes it, and one a compaction
    /// dropped is recorded again. Child and RLM hosts get none: their tool
    /// surface is fixed by their parent.
    async fn record_mcp_configured_servers(
        &mut self,
        policy: &ToolSurfacePolicy,
        catalog: &[Tool],
    ) {
        if self.child_host.is_some()
            || self.rlm_host.is_some()
            || self.is_acp_turn()
            || self.api_config.runtime_chat_isolated
            || !self.config.features.enabled(Feature::Mcp)
        {
            return;
        }
        let servers = match self.mcp_pool.as_ref() {
            Some(pool) if catalog.iter().any(|tool| is_tool_search_tool(&tool.name)) => {
                // Never stall a model request behind a pool busy with a
                // handshake; the next request boundary records it instead.
                let Ok(pool) = pool.try_lock() else {
                    return;
                };
                pool.discoverable_configured_servers(|server| policy.permits_mcp_discovery(server))
            }
            _ => Vec::new(),
        };
        let previous = self.session.messages.iter().rev().find(|message| {
            crate::runtime_handoff::mcp_configured_servers_display(message).is_some()
        });
        if servers.is_empty() && previous.is_none() {
            return;
        }
        let message = crate::runtime_handoff::mcp_configured_servers_runtime_message(&servers);
        if previous == Some(&message) {
            return;
        }
        self.add_session_message(message).await;
    }

    /// Record the entire bounded mod snapshot in the existing session history.
    /// The latest snapshot supersedes earlier ones; an empty capture withdraws
    /// them. Comparing the log also re-delivers instructions after compaction
    /// or resume without maintaining another prompt store or changing the prefix.
    async fn record_current_extension_prompt_contributions(&mut self) {
        let block = self.extension_prompt_block.clone();
        self.record_extension_prompt_contributions(block.as_deref())
            .await;
    }

    async fn record_current_constitution(&mut self) {
        let previous = self
            .session
            .messages
            .iter()
            .rev()
            .find(|message| crate::runtime_handoff::constitution_display(message).is_some());
        if self.constitution_block.is_none() && previous.is_none() {
            return;
        }
        let message = crate::runtime_handoff::constitution_runtime_message(
            self.constitution_block.as_deref(),
        );
        if previous == Some(&message) {
            return;
        }
        self.add_session_message(message).await;
        let receipt = match self.constitution_block.as_deref() {
            Some(block) if block.starts_with("Account profile constitution,") => block
                .lines()
                .next()
                .unwrap_or("Profile constitution applied."),
            Some(_) => "Local constitution applied to this turn.",
            None => "Personal constitution withdrawn for this turn.",
        };
        let _ = self.send_event(Event::status(receipt.to_owned())).await;
    }

    async fn record_extension_prompt_contributions(&mut self, block: Option<&str>) {
        let previous = self.session.messages.iter().rev().find(|message| {
            crate::runtime_handoff::extension_prompt_contributions_display(message).is_some()
        });
        if block.is_none() && previous.is_none() {
            return;
        }
        let message = crate::runtime_handoff::extension_prompt_contributions_runtime_message(block);
        if previous == Some(&message) {
            return;
        }
        self.add_session_message(message).await;
    }

    /// Recompose the stable system prompt from current context. When the bytes
    /// actually change (hash differs), record `reason` as the declared cause
    /// so the turn loop's prefix check re-pins the KV-cache prefix under a
    /// logged reason instead of reporting undeclared drift. This is only ever
    /// called from explicit header-change edges (session construction, submit
    /// turn boundary, `/model`, mode change, goal edits) — never mid-tool-loop,
    /// so an agent writing a file cannot silently move the pinned prefix.
    fn refresh_system_prompt_from_context_with_reason(
        &mut self,
        context: &NextTurnPromptContext,
        reason: &str,
    ) {
        self.record_project_trust_warning();
        let stable_prompt = self.compose_stable_system_prompt(context);

        let stable_hash = system_prompt_hash(stable_prompt.as_ref());
        if self.session.system_prompt_override {
            return;
        }
        self.session.pinned_prompt_context = Some(context.clone());
        if self.session.last_system_prompt_hash != Some(stable_hash) {
            self.session.system_prompt = stable_prompt;
            self.session.last_system_prompt_hash = Some(stable_hash);
            self.session.pending_prefix_change_reason = Some(reason.to_string());
            // A re-pinned header carries every workspace change; the delta
            // baseline restarts from it.
            self.session.context_update_baseline = None;
        }
    }

    /// New-user-turn header policy. Called once per submitted user turn,
    /// never mid-tool-loop.
    ///
    /// - When the explicit prompt inputs (model, mode, goal, route,
    ///   translation, verbosity) changed, that is a declared header change:
    ///   recompose and re-pin under a `change:<field>` reason.
    /// - Otherwise the pinned header stays byte-identical. If a fresh compose
    ///   would differ (workspace files, AGENTS.md, skills, memory drifted), the
    ///   delta is returned as a bounded `<context_update>` snapshot for the
    ///   caller to append as a user-role message *before* the user's message
    ///   — a normal history append, so the prefix still extends.
    /// - Returns `None` when nothing changed or a header re-pin absorbed it.
    fn refresh_pinned_header_for_turn(
        &mut self,
        context: &NextTurnPromptContext,
    ) -> Option<String> {
        self.record_project_trust_warning();
        if self.session.system_prompt_override {
            return None;
        }
        let explicit_reason = match self.session.pinned_prompt_context.as_ref() {
            None => Some("system".to_string()),
            Some(pinned) if pinned != context => {
                Some(explicit_prompt_context_change_reason(pinned, context))
            }
            Some(_) => None,
        };
        if let Some(reason) = explicit_reason {
            self.refresh_system_prompt_from_context_with_reason(context, &reason);
            return None;
        }

        let composed = self.compose_stable_system_prompt(context);
        let composed_hash = system_prompt_hash(composed.as_ref());
        if self.session.last_system_prompt_hash == Some(composed_hash) {
            return None;
        }
        let pinned_text =
            codewhale_core::prefix_cache::system_prompt_text(self.session.system_prompt.as_ref());
        let known_text = self
            .session
            .context_update_baseline
            .clone()
            .unwrap_or(pinned_text);
        let current_text = codewhale_core::prefix_cache::system_prompt_text(composed.as_ref());
        if known_text == current_text {
            return None;
        }
        let summary =
            codewhale_core::prefix_cache::context_update_message(&known_text, &current_text)?;
        self.session.context_update_baseline = Some(current_text);
        if let Some(pm) = self.session.prefix_stability.as_mut() {
            pm.note_context_update();
        }
        Some(summary)
    }

    /// Compose the stable system prompt for an explicit route, without
    /// touching session state.
    ///
    /// [`Self::refresh_system_prompt`] calls it for the installed route;
    /// `/preview-request` calls it for the route the *next* turn would use,
    /// which may be a different model with a different context window when
    /// auto routing is on. Extracting it is what lets a preview describe the
    /// next prompt exactly without mutating the session to find out.
    pub(super) fn compose_stable_system_prompt(
        &self,
        context: &NextTurnPromptContext,
    ) -> Option<SystemPrompt> {
        if let Some(rlm) = self.rlm_host.as_ref() {
            return Some(rlm.system_prompt.clone());
        }
        if let Some(child) = self.child_host.as_ref() {
            // The configured role is input to this same composer, and is
            // retained across route/mode/header refreshes. Volatile Native
            // contributions and work facts still append to Core Session.
            return Some(child.system_prompt.clone());
        }
        if self.api_config.runtime_chat_isolated {
            return Some(SystemPrompt::Text(ISOLATED_CHAT_SYSTEM_PROMPT.to_string()));
        }
        let user_memory_block = crate::native_memory::native_prompt_block_traced(
            self.config.memory_enabled,
            &self.config.memory_path,
            &self.config.workspace,
            &self.session.id,
        );
        let prompt_host = if self.config.terminal_chrome_enabled {
            prompts::PromptHost::Interactive
        } else {
            prompts::PromptHost::Headless
        };
        // Recomputed on each refresh (#5715): the prior session's checkpoint
        // may have settled or been resumed since construction.
        let recovery_hint = crate::session_manager::session_recovery_hint(
            &self.config.workspace,
            Some(self.session.id.as_str()),
        );
        let base =
            prompts::system_prompt_for_mode_with_context_skills_session_and_approval_for_host(
                &self.config.workspace,
                None,
                Some(&self.config.skills_dir),
                Some(&self.config.instructions),
                prompts::PromptSessionContext {
                    user_memory_block: user_memory_block.as_deref(),
                    goal_objective: context.goal_objective.as_deref(),
                    project_context_pack_enabled: self.config.project_context_pack_enabled,
                    locale_tag: &self.config.locale_tag,
                    translation_enabled: context.translation_enabled,
                    model_id: &context.model,
                    context_window_override: Some(
                        crate::route_budget::route_context_window_tokens(
                            context.provider,
                            &context.model,
                            context.route_limits,
                        ),
                    ),
                    verbosity: context.verbosity.as_deref(),
                    recovery_hint: recovery_hint.as_deref(),
                    skills_discovery_mode: self.config.skills_discovery_mode,
                    plugin_registry: Some(self.plugin_registry.as_ref()),
                    mode: context.mode,
                },
                prompt_host,
            );
        Some(base)
    }

    fn installed_next_turn_prompt_context(&self) -> NextTurnPromptContext {
        NextTurnPromptContext::for_planned_turn(
            self.api_provider,
            self.config.model.clone(),
            self.active_route_limits,
            self.current_mode,
            goal_objective_for_prompt(
                self.config.goal_objective.as_deref(),
                &self.config.goal_state,
            ),
            self.config.goal_status,
            self.config.goal_token_budget,
            self.config.translation_enabled,
            self.config.verbosity.clone(),
        )
    }
}

fn default_plugin_tools_dir() -> PathBuf {
    codewhale_config::codewhale_home()
        .unwrap_or_else(|_| {
            crate::config::effective_home_dir()
                .map_or_else(|| PathBuf::from(".codewhale"), |h| h.join(".codewhale"))
        })
        .join("tools")
}

fn plugin_tools_dir(tools_config: Option<&crate::config::ToolsConfig>) -> PathBuf {
    if let Some(tools_config) = tools_config
        && let Some(custom_dir) = tools_config.plugin_dir.as_deref()
    {
        return PathBuf::from(shellexpand::tilde(custom_dir).as_ref());
    }
    default_plugin_tools_dir()
}

/// Whether this process has not yet shown the user the notice keyed `key`.
fn first_notice(key: &str) -> bool {
    static NOTIFIED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    NOTIFIED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(key.to_string())
}

/// Load drop-in scripts and apply `[tools.overrides]`. Returns the tool names
/// this added and the overrides refused for naming a built-in (D4).
fn configure_plugin_tools(
    tool_registry: &mut crate::tools::ToolRegistry,
    tools_config: Option<&crate::config::ToolsConfig>,
) -> (std::collections::HashSet<String>, Vec<String>) {
    // Everything registered before the plugin directory loads is built in
    // (native and host-dynamic tools); no script tool may replace it (D4).
    let builtin_names: std::collections::HashSet<String> = tool_registry
        .names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    let plugin_dir = plugin_tools_dir(tools_config);
    let executor = crate::tools::plugin::PluginExecutor::for_engine();
    tool_registry.load_plugins_with_executor(&plugin_dir, executor.clone());

    let refused = match tools_config.and_then(|config| config.overrides.as_ref()) {
        Some(overrides) => tool_registry.apply_overrides_with_executor(
            overrides,
            &plugin_dir,
            &builtin_names,
            executor,
        ),
        None => Vec::new(),
    };

    let names_after: std::collections::HashSet<String> = tool_registry
        .names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    (&names_after - &builtin_names, refused)
}

fn system_prompt_hash(prompt: Option<&SystemPrompt>) -> u64 {
    let mut hasher = DefaultHasher::new();
    match prompt {
        Some(SystemPrompt::Text(text)) => {
            0u8.hash(&mut hasher);
            text.hash(&mut hasher);
        }
        Some(SystemPrompt::Blocks(blocks)) => {
            1u8.hash(&mut hasher);
            for block in blocks {
                block.block_type.hash(&mut hasher);
                block.text.hash(&mut hasher);
                if let Some(cache_control) = &block.cache_control {
                    cache_control.cache_type.hash(&mut hasher);
                }
            }
        }
        None => {
            2u8.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn normalized_goal_objective(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn sync_goal_state_from_host(
    goal_state: &SharedGoalState,
    objective: Option<&str>,
    token_budget: Option<u32>,
    status: GoalStatus,
) {
    match goal_state.lock() {
        Ok(mut state) => state.sync_from_host_status(objective, token_budget, status),
        Err(err) => tracing::warn!("goal state lock poisoned while syncing host goal: {err}"),
    }
}

fn goal_objective_for_prompt(
    configured_goal: Option<&str>,
    goal_state: &SharedGoalState,
) -> Option<String> {
    match goal_state.lock() {
        Ok(state) => {
            if let Some(objective) = state.objective() {
                // Preserve original behavior: return None (not fallback) when
                // objective exists but goal is inactive.
                return state.is_active().then(|| objective.to_string());
            }
        }
        Err(err) => tracing::warn!("goal state lock poisoned while building prompt: {err}"),
    }
    normalized_goal_objective(configured_goal)
}

// ── Mode & approval prompts as request-time runtime metadata ─────────
//
// Mode contracts and approval policies are not persisted in the session
// history and are not sent as extra system messages. Instead, each API
// request projects a transient user-role runtime metadata message at the
// tail. The stable system prompt remains byte-stable, stored history remains
// byte-stable, and strict chat-template providers never see a system message
// outside messages[0].

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolAskRuleDecision {
    Allow,
    Prompt(String),
    Block(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoReviewPlanDecision {
    NoChange,
    Allow,
    ForcePrompt(String),
    Block(String),
    /// Fallback hold routed to the model guardian in interactive Auto posture
    /// instead of a hard block.
    ConsultReviewer(String),
}

pub(crate) fn auto_review_run_origin_for_plan(detached_start: bool) -> RunOrigin {
    if detached_start {
        RunOrigin::Background
    } else {
        RunOrigin::Interactive
    }
}

pub(crate) fn auto_review_plan_decision_for_context(
    policy: &crate::core::authority::auto_review::AutoReviewPolicy,
    context: &crate::core::authority::auto_review::AutoReviewContext<'_>,
) -> (AutoReviewPlanDecision, Value) {
    let decision = policy.evaluate(context);
    let audit_event = policy.audit_event(context, &decision);
    let plan_decision = if context.approval_mode == ApprovalMode::Auto
        && context.tool_name == REQUEST_USER_INPUT_NAME
    {
        // A question executes no user work and changes no state. Auto-Review
        // reviews tool approvals only, so let the turn loop ask the user
        // instead of treating the question as an unknown external action.
        AutoReviewPlanDecision::Allow
    } else {
        match decision.action {
            crate::core::authority::auto_review::AutoReviewAction::Allow
                if context.approval_mode == ApprovalMode::Auto =>
            {
                AutoReviewPlanDecision::Allow
            }
            crate::core::authority::auto_review::AutoReviewAction::Allow => {
                AutoReviewPlanDecision::NoChange
            }
            crate::core::authority::auto_review::AutoReviewAction::AskUser
                if decision.built_in_safety_gate =>
            {
                // Name the built-in gate honestly.
                let reason = format!(
                    "Built-in safety gate requires approval: {}",
                    decision.reason
                );
                if matches!(
                    context.approval_mode,
                    ApprovalMode::Auto | ApprovalMode::Never
                ) {
                    // Auto-Review and Never are non-interactive for approval
                    // holds, so the floor fails closed instead of stranding
                    // the call on a modal neither posture opens.
                    AutoReviewPlanDecision::Block(reason)
                } else {
                    AutoReviewPlanDecision::ForcePrompt(reason)
                }
            }
            crate::core::authority::auto_review::AutoReviewAction::AskUser
                if context.approval_mode == ApprovalMode::Auto =>
            {
                AutoReviewPlanDecision::ConsultReviewer(decision.reason.clone())
            }
            crate::core::authority::auto_review::AutoReviewAction::AskUser => {
                AutoReviewPlanDecision::NoChange
            }
            crate::core::authority::auto_review::AutoReviewAction::Block => {
                AutoReviewPlanDecision::Block(format!(
                    "Auto-review policy blocked tool '{}': {}",
                    context.tool_name, decision.reason
                ))
            }
        }
    };
    (plan_decision, audit_event)
}

pub(super) fn exec_shell_ask_rule_decision(
    config: &EngineConfig,
    tool_name: &str,
    tool_input: &Value,
    workspace: &Path,
    approval_mode: ApprovalMode,
) -> Option<ToolAskRuleDecision> {
    exec_shell_ask_rule_decision_for_policy(
        &config.exec_policy_engine,
        tool_name,
        tool_input,
        workspace,
        approval_mode,
    )
}

/// Evaluate the persisted shell ask/allow/deny rules without requiring a full
/// [`EngineConfig`]. Headless protocol adapters use this seam so they enforce
/// the same sibling `permissions.toml` policy as the interactive engine.
pub(crate) fn exec_shell_ask_rule_decision_for_policy(
    exec_policy_engine: &codewhale_execpolicy::ExecPolicyEngine,
    tool_name: &str,
    tool_input: &Value,
    workspace: &Path,
    approval_mode: ApprovalMode,
) -> Option<ToolAskRuleDecision> {
    let policy_tool_name =
        crate::tools::canonical_action::canonical_action_alias(tool_name, tool_input);
    // Task tools that hand a command string to the shell answer to the same
    // shell deny and ask rules. Their own approval requirement stays: a shell
    // allow rule does not waive it.
    let runs_shell_command = matches!(policy_tool_name, "task_shell_start" | "task_gate_run");
    if policy_tool_name != "exec_shell" && !runs_shell_command {
        return None;
    }
    let command = tool_input.get("command").and_then(Value::as_str)?;
    let decision = tool_ask_rule_decision_for_context(
        exec_policy_engine,
        "exec_shell",
        command,
        None,
        workspace,
        approval_mode,
    );
    if runs_shell_command && matches!(decision, Some(ToolAskRuleDecision::Allow)) {
        return None;
    }
    decision
}

pub(super) fn file_tool_ask_rule_decision(
    config: &EngineConfig,
    tool_name: &str,
    tool_input: &Value,
    workspace: &Path,
    approval_mode: ApprovalMode,
) -> Option<ToolAskRuleDecision> {
    file_tool_ask_rule_decision_for_policy(
        &config.exec_policy_engine,
        tool_name,
        tool_input,
        workspace,
        approval_mode,
    )
}

/// Evaluate the persisted file ask/allow/deny rules without requiring a full
/// [`EngineConfig`]. This keeps protocol adapters on the canonical path and
/// preserves the all-targets-must-match rule for multi-file patches.
pub(crate) fn file_tool_ask_rule_decision_for_policy(
    exec_policy_engine: &codewhale_execpolicy::ExecPolicyEngine,
    tool_name: &str,
    tool_input: &Value,
    workspace: &Path,
    approval_mode: ApprovalMode,
) -> Option<ToolAskRuleDecision> {
    let policy_tool_name =
        crate::tools::canonical_action::canonical_action_alias(tool_name, tool_input);
    let paths = file_tool_permission_paths(policy_tool_name, tool_input)?;
    if paths.is_empty() {
        if matches!(policy_tool_name, "write_file" | "edit_file" | "apply_patch") {
            return Some(ToolAskRuleDecision::Block(
                "File write has no resolvable target; provide an explicit path or valid patch."
                    .to_string(),
            ));
        }
        return tool_ask_rule_decision_for_context(
            exec_policy_engine,
            policy_tool_name,
            "",
            None,
            workspace,
            approval_mode,
        );
    }

    let mut prompt: Option<String> = None;
    let mut all_allowed = true;
    for path in paths {
        match tool_ask_rule_decision_for_context(
            exec_policy_engine,
            policy_tool_name,
            "",
            Some(&path),
            workspace,
            approval_mode,
        ) {
            Some(ToolAskRuleDecision::Block(reason)) => {
                return Some(ToolAskRuleDecision::Block(reason));
            }
            Some(ToolAskRuleDecision::Prompt(reason)) => {
                prompt.get_or_insert(reason);
                all_allowed = false;
            }
            Some(ToolAskRuleDecision::Allow) => {}
            None => all_allowed = false,
        }
    }
    if let Some(prompt) = prompt {
        Some(ToolAskRuleDecision::Prompt(prompt))
    } else if all_allowed {
        Some(ToolAskRuleDecision::Allow)
    } else {
        None
    }
}

fn tool_ask_rule_decision_for_context(
    exec_policy_engine: &codewhale_execpolicy::ExecPolicyEngine,
    tool_name: &str,
    command: &str,
    path: Option<&str>,
    workspace: &Path,
    approval_mode: ApprovalMode,
) -> Option<ToolAskRuleDecision> {
    let cwd = workspace.to_string_lossy();
    let ask_for_approval = match approval_mode {
        ApprovalMode::Never => AskForApproval::Never,
        ApprovalMode::Auto | ApprovalMode::Bypass | ApprovalMode::Suggest => {
            AskForApproval::OnFailure
        }
    };
    let decision = exec_policy_engine
        .check(ExecPolicyContext {
            command,
            cwd: cwd.as_ref(),
            tool: Some(tool_name),
            path,
            ask_for_approval,
            sandbox_mode: None,
        })
        .ok()?;
    if !decision.allow {
        Some(ToolAskRuleDecision::Block(decision.reason().to_string()))
    } else if decision.requires_approval {
        Some(ToolAskRuleDecision::Prompt(decision.reason().to_string()))
    } else if decision.matched_action == Some(codewhale_execpolicy::PermissionAction::Allow) {
        // Count only. Never `matched_rule`, never `reason()`, never the
        // command or its argv: `auto_allow` patterns are user-authored command
        // strings.
        codewhale_telemetry::session_counters()
            .bump(codewhale_telemetry::Counter::ApprovalAutoAllowed);
        Some(ToolAskRuleDecision::Allow)
    } else {
        None
    }
}

/// Every path the file tool will act on. The tools fold `file_path` /
/// `filePath` onto `path` before executing, so each alias spelling is read
/// here too; a rule keyed on `path` would otherwise never see that target.
fn file_tool_permission_paths(tool_name: &str, input: &Value) -> Option<Vec<String>> {
    let path_arguments = || {
        let mut paths: Vec<String> = crate::tools::file::path_argument_keys()
            .filter_map(|key| string_field(input, key))
            .collect();
        paths.dedup();
        paths
    };
    match tool_name {
        "read_file" | "write_file" | "edit_file" | "file_search" | "grep_files" => {
            Some(path_arguments())
        }
        "list_dir" => {
            let paths = path_arguments();
            Some(if paths.is_empty() {
                vec![".".to_string()]
            } else {
                paths
            })
        }
        "apply_patch" => Some(apply_patch_permission_paths(
            &crate::tools::file::with_canonical_path_argument(input),
        )),
        _ => None,
    }
}

/// Target paths when a call is one of the canonical workspace file-write
/// tools (`write_file` / `edit_file` / `apply_patch`), `None` for any other
/// tool. Feeds the in-workspace write carve-out (#5185).
pub(crate) fn file_write_tool_target_paths(tool_name: &str, input: &Value) -> Option<Vec<String>> {
    let canonical = crate::tools::canonical_action::canonical_action_alias(tool_name, input);
    if !matches!(canonical, "write_file" | "edit_file" | "apply_patch") {
        return None;
    }
    file_tool_permission_paths(canonical, input)
}

fn string_field(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn apply_patch_permission_paths(input: &Value) -> Vec<String> {
    crate::tools::apply_patch::preflight_apply_patch(input)
        .map(|preflight| preflight.touched_files)
        .unwrap_or_default()
}

/// Spawn the engine in a background task
pub fn spawn_engine(config: EngineConfig, api_config: &Config) -> EngineHandle {
    let (engine, handle) = Engine::new(config, api_config);

    // Box the run future before supervision. An extra async wrapper embeds
    // the large engine state again in both its own and the supervisor's poll
    // frames, which can overflow an ordinary worker-thread stack.
    spawn_supervised(
        "engine-event-loop",
        std::panic::Location::caller(),
        Box::pin(engine.run()),
    );

    handle
}

/// Spawn a runtime-owned engine whose autonomous later turns resolve against
/// the manager's atomic config snapshot. This does not mutate an active turn.
pub(crate) fn spawn_engine_with_authoritative_route_config(
    mut config: EngineConfig,
    api_config: &Config,
    host_profile: EngineHostProfile,
    authoritative_route_config: Arc<parking_lot::RwLock<Config>>,
    model_client: Option<SharedModelClient>,
    subagent_manager: Option<SharedSubAgentManager>,
    execution_admission: CancellationToken,
) -> (EngineHandle, tokio::task::JoinHandle<()>) {
    // `model_client` replaces only the model I/O boundary (see
    // `Engine::new_with_model_client`); hosts pass `None` for the provider
    // client the route resolves.
    if host_profile.is_acp() {
        config.max_steps = config.max_steps.clamp(1, 50);
        config.goal_max_steps = None;
        config.subagents_enabled = false;
    }
    let (mut engine, mut handle) =
        Engine::new_admitted_with_subagent_manager(config, api_config, None, subagent_manager);
    if let Some(limits) = engine.subagent_execution_limits.as_mut() {
        limits.admission = execution_admission;
    }
    if let Some(client) = model_client {
        engine.model_client = Some(client);
        engine.model_client_injected = true;
        engine.codewhale_client_error = None;
        handle.client_preflight_required = false;
    }
    engine.host_profile = host_profile;
    engine.authoritative_route_config = Some(authoritative_route_config);

    let worker = spawn_supervised(
        "engine-event-loop",
        std::panic::Location::caller(),
        Box::pin(engine.run()),
    );

    (handle, worker)
}

#[cfg(test)]
pub(crate) struct MockEngineHandle {
    pub handle: EngineHandle,
    pub rx_op: mpsc::Receiver<Op>,
    rx_approval: mpsc::Receiver<ApprovalDecision>,
    rx_user_input: mpsc::Receiver<UserInputDecision>,
    pub rx_steer: mpsc::Receiver<handle::SteerInput>,
    pub tx_event: mpsc::Sender<Event>,
    pub cancel_token: CancellationToken,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MockApprovalEvent {
    Approved {
        id: String,
    },
    Denied {
        id: String,
    },
    TimedOut {
        id: String,
    },
    Unavailable {
        id: String,
    },
    RetryWithPolicy {
        id: String,
        policy: crate::sandbox::SandboxPolicy,
    },
}

#[cfg(test)]
impl MockEngineHandle {
    pub(crate) async fn recv_approval_event(&mut self) -> Option<MockApprovalEvent> {
        self.recv_approval_decision().await.map(|(event, _)| event)
    }

    /// The next decision and who the host said made it (`None` for a
    /// timeout or an unavailable request, which name their own cause).
    pub(crate) async fn recv_approval_decision(
        &mut self,
    ) -> Option<(
        MockApprovalEvent,
        Option<crate::approval_log::ApprovalDecider>,
    )> {
        Some(match self.rx_approval.recv().await? {
            ApprovalDecision::Approved { id, by, .. } => {
                (MockApprovalEvent::Approved { id }, Some(by))
            }
            ApprovalDecision::Denied { id, by, .. } => (MockApprovalEvent::Denied { id }, Some(by)),
            ApprovalDecision::TimedOut { id } => (MockApprovalEvent::TimedOut { id }, None),
            ApprovalDecision::Unavailable { id } => (MockApprovalEvent::Unavailable { id }, None),
            ApprovalDecision::RetryWithPolicy { id, policy, by } => {
                (MockApprovalEvent::RetryWithPolicy { id, policy }, Some(by))
            }
        })
    }

    pub(crate) async fn recv_user_input_submission(
        &mut self,
    ) -> Option<(String, UserInputResponse)> {
        match self.rx_user_input.recv().await? {
            UserInputDecision::Submitted {
                id,
                response,
                accepted,
            } => {
                accepted.send(true).ok()?;
                Some((id, response))
            }
            UserInputDecision::Cancelled { accepted, .. } => {
                let _ = accepted.send(true);
                None
            }
        }
    }

    pub(crate) async fn recv_user_input_cancellation(&mut self) -> Option<String> {
        match self.rx_user_input.recv().await? {
            UserInputDecision::Cancelled { id, accepted } => {
                accepted.send(true).ok()?;
                Some(id)
            }
            UserInputDecision::Submitted { accepted, .. } => {
                let _ = accepted.send(true);
                None
            }
        }
    }

    /// Model an Engine verdict that rejects this question's decision.
    pub(crate) async fn reject_user_input_decision(&mut self) -> Option<String> {
        let decision = self.rx_user_input.recv().await?;
        let id = match &decision {
            UserInputDecision::Submitted { id, .. } | UserInputDecision::Cancelled { id, .. } => {
                id.clone()
            }
        };
        decision.reject();
        Some(id)
    }

    /// Close the engine event stream without moving fields out of the handle,
    /// so failure-path tests can keep using the receiver helpers afterwards.
    pub(crate) fn close_event_stream(&mut self) {
        let (tx_event, _rx_event) = mpsc::channel(1);
        self.tx_event = tx_event;
    }
}

#[cfg(test)]
pub(crate) fn mock_engine_handle() -> MockEngineHandle {
    let (tx_op, rx_op) = mpsc::channel(32);
    let (tx_event, rx_event) = mpsc::channel(4 * streaming::MAX_TOOL_CALLS_PER_RESPONSE);
    let (tx_approval, rx_approval) = mpsc::channel(64);
    let (tx_user_input, rx_user_input) = mpsc::channel(32);
    let (tx_steer, rx_steer) = mpsc::channel(64);
    let cancel_token = CancellationToken::new();
    let shared_cancel_token = Arc::new(StdMutex::new(cancel_token.clone()));
    let cancel_reason: Arc<StdMutex<Option<CancelReason>>> = Arc::new(StdMutex::new(None));
    let shared_paused = Arc::new(StdMutex::new(false));
    let live_runtime_authority = Arc::new(StdMutex::new(LiveRuntimeAuthorityState::new(
        LiveRuntimeAuthority::from_fields(
            AppMode::Agent,
            false,
            false,
            false,
            ApprovalMode::Suggest,
            None,
        ),
    )));
    let compaction_cancellation = Arc::new(StdMutex::new(CompactionCancellationState::default()));
    let handle = EngineHandle {
        goal_state: new_shared_goal_state(),
        tx_op,
        rx_event: Arc::new(RwLock::new(rx_event)),
        cancel_token: shared_cancel_token,
        cancel_reason,
        tx_approval,
        tx_user_input,
        tx_steer,
        turn_controls: Arc::new(StdMutex::new(handle::TurnControls::default())),
        shared_paused,
        client_preflight_required: false,
        live_runtime_authority,
        compaction_cancellation,
        turn_heartbeat: turn_heartbeat::TurnHeartbeat::new(),
        subagent_manager: crate::tools::subagent::new_shared_subagent_manager(
            std::env::temp_dir(),
            1,
        ),
        terminal_worker_events: Some(Arc::new(TerminalEventDelivery::default())),
    };

    MockEngineHandle {
        handle,
        rx_op,
        rx_approval,
        rx_user_input,
        rx_steer,
        tx_event,
        cancel_token,
    }
}

/// The session state a turn installs before it writes `<turn_meta>`.
///
/// Production reads it back off `self` after installing it; `/preview-request`
/// supplies the values it *would* install, so an inspection can reproduce the
/// block exactly without writing any of them.
pub(crate) struct TurnMetadataSnapshot<'a> {
    pub(crate) prompt_context: &'a NextTurnPromptContext,
    pub(crate) system_prompt: Option<&'a SystemPrompt>,
    pub(crate) approval_mode: ApprovalMode,
    pub(crate) working_set: &'a crate::working_set::WorkingSet,
    pub(crate) policy_narrowing: Option<&'a PolicyNarrowingEvent>,
}

/// Immutable prompt facts for the next accepted turn.
///
/// Both production and `/preview-request` compose through this value. It owns
/// every per-turn field resolved by submit or route planning that can change
/// the stable system prompt, so a hypothetical route cannot accidentally
/// inherit the installed turn's goal, translation, verbosity, mode,
/// model, or context window. Workspace-scoped prompt inputs remain engine
/// configuration and are documented separately as snapshot dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NextTurnPromptContext {
    pub(crate) provider: ProviderKind,
    pub(crate) model: String,
    pub(crate) route_limits: Option<codewhale_config::route::RouteLimits>,
    pub(crate) mode: AppMode,
    pub(crate) goal_objective: Option<String>,
    pub(crate) goal_token_budget: Option<u32>,
    pub(crate) translation_enabled: bool,
    pub(crate) verbosity: Option<String>,
}

/// Name the explicit prompt inputs that differ between two contexts, for the
/// `change:<what>` prefix-pin reason.
pub(crate) fn explicit_prompt_context_change_reason(
    pinned: &NextTurnPromptContext,
    next: &NextTurnPromptContext,
) -> String {
    let mut fields = Vec::new();
    if pinned.provider != next.provider {
        fields.push("provider");
    }
    if pinned.model != next.model {
        fields.push("model");
    }
    if pinned.route_limits != next.route_limits {
        fields.push("route");
    }
    if pinned.mode != next.mode {
        fields.push("mode");
    }
    if pinned.goal_objective != next.goal_objective
        || pinned.goal_token_budget != next.goal_token_budget
    {
        fields.push("goal");
    }
    if pinned.translation_enabled != next.translation_enabled {
        fields.push("translation");
    }
    if pinned.verbosity != next.verbosity {
        fields.push("verbosity");
    }
    if fields.is_empty() {
        "system".to_string()
    } else {
        fields.join("+")
    }
}

impl NextTurnPromptContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn for_planned_turn(
        provider: ProviderKind,
        model: String,
        route_limits: Option<codewhale_config::route::RouteLimits>,
        mode: AppMode,
        goal_objective: Option<String>,
        goal_status: GoalStatus,
        goal_token_budget: Option<u32>,
        translation_enabled: bool,
        verbosity: Option<String>,
    ) -> Self {
        Self {
            provider,
            model,
            route_limits,
            mode,
            goal_objective: (goal_status == GoalStatus::Active)
                .then(|| normalized_goal_objective(goal_objective.as_deref()))
                .flatten(),
            goal_token_budget,
            translation_enabled,
            verbosity,
        }
    }
}

/// Grace period for cancelled turn-owned children to release their barrier
/// registration before the terminal turn event is emitted anyway (#6184).
/// Cooperative children settle in milliseconds; the bound exists so a child
/// parked on an await that never observes its cancel token cannot withhold
/// `TurnComplete` — a child still shutting down is strictly less harmful
/// than a turn that silently never finishes.
pub(crate) const FOREGROUND_CHILD_SETTLE_GRACE: Duration = Duration::from_secs(5);

/// Turn-scoped mailbox handle plus the machinery needed to close it exactly
/// once. Held by the engine (never by the child runtime) so the flush barrier
/// is owned by the same code that emits the terminal turn event.
pub(crate) struct TurnMailboxBarrier {
    pub(crate) mailbox: Mailbox,
    pub(crate) cancel_token: tokio_util::sync::CancellationToken,
    pub(crate) foreground_children: Arc<ForegroundChildRegistry>,
    pub(crate) flush_tx: tokio::sync::oneshot::Sender<()>,
    pub(crate) drain_handle: tokio::task::JoinHandle<()>,
    /// Bound on the cancelled-child join and on the mailbox-drainer flush
    /// inside the barrier (#6184).
    pub(crate) settle_grace: Duration,
}

fn terminal_turn_status_at_settlement(
    status: TurnOutcomeStatus,
    cancellation_requested: bool,
) -> TurnOutcomeStatus {
    if status == TurnOutcomeStatus::Completed && cancellation_requested {
        TurnOutcomeStatus::Interrupted
    } else {
        status
    }
}

impl TurnMailboxBarrier {
    /// Settle the foreground subtree before closing the turn's mailbox. The
    /// ordering is intentional: a terminal turn event must never be emitted
    /// while an owned child can still publish into this turn's shared state.
    ///
    /// The join is best-effort and bounded by `settle_grace` (#6184): a
    /// child parked on an await that never observes its cancel token must
    /// not withhold `TurnComplete`. Returns the labels of any children
    /// still registered when the join gave up — empty on a clean settle —
    /// so the caller can name them in the terminal turn event.
    pub(crate) async fn cancel_and_flush(self) -> Vec<String> {
        let unsettled = self.join_foreground_children().await;
        self.flush().await;
        unsettled
    }

    /// A normal answer closes this turn's UI mailbox without cancelling
    /// healthy children. Their manager registration, transcript, immutable
    /// usage owner and completion inbox survive this turn. Explicit stop,
    /// failed turns and budget stops still use `cancel_and_flush`.
    pub(crate) async fn continue_and_flush(self) {
        self.flush().await;
    }

    /// Wait for cancelled foreground children to release their
    /// registration, giving up at `settle_grace` or when a *new*
    /// cancellation lands mid-wait. The Esc path reaches this barrier with
    /// the turn token already cancelled, so the early-exit arm is only
    /// armed when it is not — otherwise every interrupted turn's receipt
    /// window would collapse to zero instead of merely being bounded.
    async fn join_foreground_children(&self) -> Vec<String> {
        let join = self.foreground_children.cancel_and_wait();
        tokio::pin!(join);
        let fresh_cancel = !self.cancel_token.is_cancelled();
        let gave_up = tokio::select! {
            biased;
            () = &mut join => false,
            () = self.cancel_token.cancelled(), if fresh_cancel => true,
            () = tokio::time::sleep(self.settle_grace) => true,
        };
        if !gave_up {
            return Vec::new();
        }
        let labels = self.foreground_children.unsettled_labels();
        tracing::warn!(
            unsettled_children = ?labels,
            "foreground child join exceeded its bound; sealing the turn mailbox with children still registered"
        );
        labels
    }

    /// Seal the turn mailbox and wait for the drainer *under a bound* (#6184).
    /// The drainer forwards into the event channel with an untimed send; a
    /// UI that has stopped draining parks it, and the flush signal cannot be
    /// observed from inside that send. The bound prevents this wait from
    /// withholding completion; it does not establish the cause of the
    /// hours-long freeze reported in #6184.
    async fn flush(self) {
        self.mailbox.seal();
        let _ = self.flush_tx.send(());
        let mut drain_handle = self.drain_handle;
        await_mailbox_drain_bounded(&mut drain_handle, self.settle_grace).await;
    }
}

/// Bound the mailbox drainer's exit (#6184).
///
/// A full event channel with a live, non-draining consumer can park a
/// forward. The flush signal remains pending until that forward returns.
/// Abort the drainer on expiry so best-effort delivery cannot indefinitely
/// withhold turn completion, matching the bounded child join above.
async fn await_mailbox_drain_bounded(
    drain_handle: &mut tokio::task::JoinHandle<()>,
    grace: Duration,
) {
    if tokio::time::timeout(grace, &mut *drain_handle)
        .await
        .is_err()
    {
        drain_handle.abort();
        tracing::warn!(
            grace_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX),
            "subagent-mailbox drainer exceeded its bound with the event channel not draining; aborted so the turn can settle"
        );
    }
}

/// Result of one turn tool-catalog build.
struct TurnToolBuild {
    /// One authority for executable, searchable, and initially active tools.
    surface: ToolSurfacePolicy,
    /// Names of the MCP-contributed tools in this build.
    mcp_tool_names: Vec<String>,
    /// What is known about the MCP contribution to this catalog.
    mcp: McpToolState,
    /// Route model installed into the child runtime, when sub-agent tools were
    /// available. This is an internal receipt, not a manifest field.
    #[cfg_attr(not(test), expect(dead_code))]
    subagent_runtime_model: Option<String>,
    /// Turn-scoped sub-agent mailbox and its flush barrier, when sub-agent
    /// wiring was live. The engine must seal, flush, and await this before it
    /// emits `TurnComplete`. Detached children never reopen this ordering
    /// boundary; their owner-scoped usage lease is the separate durable path.
    mailbox: Option<TurnMailboxBarrier>,
    /// Tools this build loaded from the plugin surface rather than the built-in
    /// registry builder. Carried out so the read-only request projection can
    /// tell `plugin` provenance from `builtin` instead of collapsing both.
    plugin_tool_names: std::collections::HashSet<String>,
}

/// The route a tool catalog is being shaped for.
///
/// A real turn installs its route before building the catalog, so this is
/// simply the installed route. `/preview-request` has a *planned* route that
/// is deliberately not installed, so it passes that one instead — otherwise
/// an auto-routed preview would report the previous route's tool budget.
#[derive(Clone)]
pub(crate) struct TurnRouteContext {
    pub(crate) provider: ProviderKind,
    pub(crate) model: String,
    pub(crate) capabilities: codewhale_config::route::RouteCapabilities,
    pub(crate) limits: Option<codewhale_config::route::RouteLimits>,
    /// Client for this exact route. Tool contexts use it only for
    /// provider-native helper capabilities; previews pass their throw-away
    /// planned client instead of inheriting the installed session client.
    pub(crate) client: Option<CodewhaleClient>,
    /// Route-scoped runtime config, captured by the planner. A preview must
    /// never construct child agents from the previously installed config.
    pub(crate) api_config: Box<crate::config::Config>,
    pub(crate) locale_tag: String,
    pub(crate) role_models: HashMap<String, crate::config::SubagentModelOverride>,
    pub(crate) auto_model: bool,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) reasoning_effort_auto: bool,
}

impl TurnRouteContext {
    pub(crate) fn capability_profile(&self) -> crate::model_profile::CapabilityProfile {
        crate::model_profile::resolved_capability_profile_for_route(
            self.provider,
            &self.model,
            self.capabilities,
            self.limits.unwrap_or_default(),
        )
    }
}

/// Whether a tool-catalog build may start or connect MCP servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpAccess {
    /// A real turn: create the pool if needed and connect every enabled
    /// server, exactly as before.
    Connect,
    /// An inspection: use only what is already connected, and report the
    /// tool surface as unavailable when that is not the whole picture.
    PassiveSnapshot,
}

impl McpAccess {
    fn may_connect(self) -> bool {
        matches!(self, Self::Connect)
    }
}

/// The MCP contribution to one tool-catalog build.
#[derive(Debug, Clone)]
pub(crate) enum McpToolState {
    /// MCP is off for this session; a turn would send no MCP tools.
    Disabled,
    /// The exact MCP tool set the next request would carry.
    Live {
        tools: Vec<Tool>,
        server_count: usize,
    },
    /// The exact set is not knowable without connecting, which an inspection
    /// must not do.
    Unavailable { reason: McpUnavailable },
}

impl McpToolState {
    pub(crate) fn tools(&self) -> &[Tool] {
        match self {
            Self::Live { tools, .. } => tools,
            Self::Disabled | Self::Unavailable { .. } => &[],
        }
    }

    /// Connected server count, or `None` when the state is unavailable.
    pub(crate) fn server_count(&self) -> Option<usize> {
        match self {
            Self::Disabled => Some(0),
            Self::Live { server_count, .. } => Some(*server_count),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Why a passive MCP snapshot could not describe the next turn exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpUnavailable {
    /// No pool exists yet: the first turn of the session would create and
    /// connect one.
    PoolNotStarted,
    /// An MCP config source changed since the pool last read it, so the next
    /// turn would reload before connecting.
    ConfigChangedSinceConnect,
    /// Some enabled servers are configured but not connected.
    ServersNotConnected { pending: usize },
}

impl McpUnavailable {
    /// Short, path-free explanation for the manifest.
    pub(crate) fn label(self) -> String {
        match self {
            Self::PoolNotStarted => {
                "MCP is enabled but no server has been connected in this session yet".to_string()
            }
            Self::ConfigChangedSinceConnect => {
                "an MCP configuration source changed since the last connect".to_string()
            }
            Self::ServersNotConnected { pending } => {
                format!("{pending} enabled MCP server(s) are not connected yet")
            }
        }
    }
}

/// Whether a tool-catalog build may establish sub-agent runtime side effects.
///
/// Both variants register exactly the same tools; only the runtime plumbing
/// differs (the structured fork snapshot and the spawned mailbox drainer),
/// which is what makes an offline inspection safe to run at any time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubAgentWiring {
    /// A real turn: wire the fork snapshot and the mailbox drainer.
    Live,
    /// An inspection: build the catalog, spawn nothing.
    Inert,
}

impl SubAgentWiring {
    fn is_live(self) -> bool {
        matches!(self, Self::Live)
    }
}

mod approval;
mod compaction;
mod context;
pub(crate) mod handle;
pub mod preview;
use crate::compaction::estimate_input_tokens_conservative;
#[cfg(test)]
pub(crate) use context::compact_tool_result_for_context;
pub(crate) use context::compact_tool_result_for_route;
/// Public so external hosts/wrappers can reuse the engine's input-budget math
/// (see `context_input_budget_for_route`'s doc) instead of re-deriving it.
pub use context::context_input_budget_for_route;
#[cfg(test)]
use context::route_context_budget_for_provider;
use context::{
    MAX_CONTEXT_RECOVERY_ATTEMPTS, context_overflow_exhausted_message,
    effective_max_output_tokens_for_route, extract_compaction_summary_prompt,
    is_context_length_error_message, is_image_input_rejection_message,
    route_context_budget_for_route, summarize_text,
};
#[cfg(test)]
use context::{context_input_budget_for_provider, effective_max_output_tokens};
mod dispatch;
mod lsp_hooks;
pub(crate) mod reviewer;
mod streaming;
mod token_estimate_cache;
pub(crate) mod tool_catalog;
pub(crate) mod tool_execution;
#[cfg(all(test, unix))]
pub(crate) use tool_execution::pin_replay_span_sequence;
mod tool_media;
mod tool_preparation;
mod tool_setup;
pub(crate) mod turn_budget;
pub(crate) mod turn_heartbeat;
pub(crate) mod turn_loop;
pub(crate) use approval::HumanDecision;
pub(crate) use dispatch::content_without_approval_note;
pub(crate) use token_estimate_cache::TokenEstimateCache;

pub(super) const MAX_PARALLEL_SHELL_EXEC: usize = 4;

#[cfg(test)]
pub(crate) fn default_active_native_tool_names() -> &'static [&'static str] {
    tool_catalog::DEFAULT_ACTIVE_NATIVE_TOOLS
}

use self::approval::{ApprovalDecision, ApprovalResult, UserInputDecision};
use self::dispatch::{
    ToolApprovalStamp, ToolExecGuard, ToolExecOutcome, ToolExecutionBatch, ToolExecutionPlan,
    caller_allowed_for_tool, caller_type_for_tool_use, final_tool_input,
    format_tool_error_with_schema, malformed_tool_arguments_error, malformed_tool_arguments_input,
    parse_tool_input, plan_tool_execution_batches, stamp_tool_result_approval,
};
#[cfg(test)]
use self::dispatch::{format_tool_error, should_parallelize_tool_batch};
#[cfg(test)]
use self::lsp_hooks::edited_paths_for_tool;
pub(crate) use self::streaming::FAKE_WRAPPER_NOTICE;
#[cfg(test)]
use self::streaming::TOOL_CALL_START_MARKERS;
#[cfg(test)]
use self::streaming::filter_tool_call_delta;
use self::streaming::{
    ContentBlockKind, StreamResume, StreamRetryBudget, ToolCallDeltaFilterState, ToolUseState,
    contains_fake_tool_wrapper, filter_tool_call_delta_with_state, flush_tool_call_delta_state,
    should_resume_after_network_drop, should_resume_after_sleep,
    should_resume_interactive_after_network_drop, should_transparently_retry_stream,
    sleep_gap_detected, stream_read_error_user_message,
};
#[cfg(test)]
use self::streaming::{
    MAX_STREAM_ERRORS_BEFORE_FAIL, MAX_STREAM_RETRIES, MAX_TRANSPARENT_STREAM_RETRIES,
};
use self::tool_catalog::{
    CODE_EXECUTION_TOOL_NAME, EXECUTE_TOOLS_TOOL_NAME, JS_EXECUTION_TOOL_NAME,
    REQUEST_USER_INPUT_NAME, ToolSurfacePolicy, active_tools_for_request,
    build_model_tool_catalog_with_surface, default_synthetic_catalog_tool_names,
    execute_code_execution_tool, is_tool_search_tool, maybe_hydrate_requested_deferred_tool,
    missing_tool_error_message,
};
#[cfg(test)]
use self::tool_catalog::{
    TOOL_SEARCH_NAME, active_tools_for_step, build_model_tool_catalog, ensure_advanced_tooling,
    execute_tool_search, initial_active_tools, preflight_requested_deferred_tool,
    should_default_defer_tool, tool_allowed, tool_catalog_consistency_issues, tool_denied,
};
pub(crate) use self::tool_execution::emit_tool_audit;
use self::tool_preparation::{prepare_tool_call, reprepare_tool_call_after_hook};
use crate::tools::js_execution::execute_js_execution_tool;

#[cfg(test)]
pub(crate) mod tests;

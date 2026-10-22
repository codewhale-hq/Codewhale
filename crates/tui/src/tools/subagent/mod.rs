//! Sub-agent spawning system.
//!
//! Provides tools to spawn background sub-agents, query their status,
//! and retrieve results. Sub-agents run with a filtered toolset and
//! inherit the workspace configuration from the main session.
//!
//! The model-facing creation surface is the `agent` tool. Narrow coordination
//! tools (`agents/list`, `agents/message`, `agents/followup`,
//! `agents/interrupt`, `agents/coordinate`, `agents/wait`) are retired from
//! the model catalog (#5462) — still registered and executable by name so
//! persisted transcripts replay, never advertised — and wrap the same runtime without restoring
//! the retired lifecycle theater. Older manager helpers remain executable for
//! persisted records and internal recovery.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
#[cfg(test)]
use tokio::sync::Mutex;
use tokio::sync::RwLock;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::client::CodewhaleClient;
use crate::config::{MAX_SUBAGENTS, SubagentModelOverride};
#[cfg(test)]
use crate::core::engine::ChildSurfaceProbe;
use crate::core::engine::tool_catalog::tool_denied;
#[cfg(test)]
use crate::core::engine::tool_catalog::{
    TOOL_SEARCH_NAME, apply_native_tool_deferral, ensure_advanced_tooling,
    execute_tool_search_with_cache, is_tool_search_tool,
};
use crate::core::events::{AgentProgressEventMeta, Event};
#[cfg(test)]
use crate::core::session::ToolActivationCache;
use crate::dependencies::{ExternalTool, Git};
/// Compatibility re-export: the closed role set lives in
/// [`crate::fleet::role`], the lightweight role surface every spawn path
/// consumes. Existing `tools::subagent::FleetRole` paths keep resolving.
pub use crate::fleet::role::FleetRole;
use crate::fleet::role::{
    FLEET_ROLE_SCHEMA_VALUES, VALID_ROLE_ALIASES, migrate_legacy_role_token, public_role_label,
};
use crate::llm_client::LlmError;
use crate::reasoning_preference::ReasoningEffort;
use crate::request_tuning::RequestTuning;
use crate::tools::canonical_action::{
    CANONICAL_ACTION_ALIASES, canonical_action_alias, is_action_family,
};
use crate::tools::handle::VarHandle;
#[cfg(test)]
use crate::tools::plan::PlanState;
use crate::tools::plan::SharedPlanState;
use crate::tools::registry::{AgentToolSurfaceOptions, ToolRegistry, ToolRegistryBuilder};
use crate::tools::shell::SharedShellManager;
#[cfg(test)]
use crate::tools::spec::RichToolResult;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use crate::tools::todo::SharedTodoList;
#[cfg(test)]
use crate::tools::todo::TodoList;
use crate::utils::spawn_supervised;
use crate::work_graph::{
    EvidenceKind, EvidenceRef, OperationIntent, OperationOwnerSnapshot, OwnerState,
    SharedWorkRuntime,
};
use crate::worker_profile::{
    ChildLaunchManifest, ModelRoute, ShellPolicy, ToolScope, WorkerRuntimeProfile,
    narrow_model_steps, narrow_optional_limit,
};
use codewhale_config::AppMode;
#[cfg(test)]
use codewhale_execpolicy::ApprovalMode;
#[cfg(test)]
use codewhale_models::MessageResponse;
use codewhale_models::{ContentBlock, Message, SystemPrompt, Tool, Usage};
use codewhale_models::{is_output_limit_stop_reason, stop_reason_detail};
use coord::{
    CoordinationClaimScope, CoordinationDetailMetrics, CoordinationHotPath, CoordinationLedger,
    DecisionRecord, DecisionStatus, PersistedWriteClaim, ReconciliationReceipt, WriteScopeClaim,
};

pub mod advisor;
pub(crate) mod budget_handback;
pub mod coord;
mod delivery;
pub(crate) mod engine;
mod governor;
mod lifecycle;
pub mod mailbox;
mod naming;
mod worktree;

pub use delivery::{DeliverableVerdict, DeliveryEvidence};

use worktree::{SubAgentWorktreeRequest, prepare_child_workspace};
#[cfg(test)]
use worktree::{create_isolated_worktree, git_repo_root};

#[allow(unused_imports)] // re-exported for hosts / tests; registration uses concrete types
pub use advisor::{
    AdvisorConfig, EmissionGuard, ToolCallPair, build_advisor_prompt, extract_tool_call_pairs,
    run_advisor_for_turn,
};
use codewhale_models::Role;
#[allow(unused_imports)] // re-exported for hosts / tests; registration uses concrete types
pub use coord::{
    AgentsCoordinateTool, AgentsFollowupTool, AgentsInterruptTool, AgentsListTool,
    AgentsMessageTool, AgentsWaitTool, CoordinationDetailProjection, register_coordination_tools,
};
#[allow(unused_imports)]
pub use mailbox::{Mailbox, MailboxEnvelope, MailboxMessage, MailboxReceiver};
pub(crate) use naming::explicit_nickname;
use naming::generated_whale_name_base;
pub(crate) use naming::localized_whale_display_names;
pub(crate) use naming::subagent_display_name;
pub(crate) use naming::subagent_result_display_name;
pub(crate) use naming::subagent_role_label;
#[allow(unused_imports)] // compatibility path; some consumers exist only in test builds today
pub use naming::{
    WHALE_NICKNAMES, assign_unique_whale_name_in_locale, whale_name_for_id_in_locale,
};
#[cfg(test)]
use naming::{
    WHALE_NICKNAMES_CA, WHALE_NICKNAMES_DE, WHALE_NICKNAMES_ES_419, WHALE_NICKNAMES_FR,
    WHALE_NICKNAMES_HI, WHALE_NICKNAMES_ID, WHALE_NICKNAMES_JA, WHALE_NICKNAMES_KO,
    WHALE_NICKNAMES_PT_BR, WHALE_NICKNAMES_RU, WHALE_NICKNAMES_UK, WHALE_NICKNAMES_VI,
    WHALE_NICKNAMES_ZH_HANT,
};

// === Constants ===

/// Global ownership table for cache-aware resident file sub-agents (#529).
/// Maps file path → agent id. Agents hold a lease on a file while running;
/// the lease is released when the agent reaches a terminal state.
static RESIDENT_LEASES: std::sync::OnceLock<
    parking_lot::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::OnceLock::new();
const MAX_RESIDENT_CONTEXT_BYTES: u64 = 64 * 1024;

/// Release all resident file leases held by `agent_id`. Called when an
/// agent transitions to a terminal state (completed, failed, cancelled).
fn release_resident_leases_for(agent_id: &str) {
    if let Some(lock) = RESIDENT_LEASES.get() {
        let mut guard = lock.lock();
        guard.retain(|_, owner| owner != agent_id);
    }
}

fn reserve_resident_lease(lease_key: &str, display_path: &str) -> Result<(), ToolError> {
    let leases = RESIDENT_LEASES.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
    let mut guard = leases.lock();
    if let Some(owner) = guard.get(lease_key) {
        return Err(ToolError::invalid_input(format!(
            "resident_file '{display_path}' is already leased by agent {owner}"
        )));
    }
    guard.insert(lease_key.to_string(), "pending".to_string());
    Ok(())
}

fn rollback_pending_resident_lease(file_path: &str) {
    if let Some(leases) = RESIDENT_LEASES.get() {
        let mut guard = leases.lock();
        if guard.get(file_path).is_some_and(|owner| owner == "pending") {
            guard.remove(file_path);
        }
    }
}

fn commit_resident_lease(file_path: &str, agent_id: &str) {
    if let Some(leases) = RESIDENT_LEASES.get() {
        let mut guard = leases.lock();
        if let Some(owner) = guard.get_mut(file_path)
            && owner == "pending"
        {
            *owner = agent_id.to_string();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResidentContext {
    display_path: String,
    lease_key: String,
    contents: String,
}

fn read_bounded_resident_context(
    context: &ToolContext,
    raw_path: &str,
) -> Result<ResidentContext, ToolError> {
    let path = crate::tools::spec::resolve_strict_authority_path(context, raw_path)?;
    let metadata = std::fs::metadata(&path).map_err(|error| {
        ToolError::invalid_input(format!(
            "resident_file '{}' is not a readable workspace file: {error}",
            raw_path
        ))
    })?;
    if !metadata.is_file() {
        return Err(ToolError::invalid_input(format!(
            "resident_file '{}' must name one regular workspace file",
            raw_path
        )));
    }
    if metadata.len() > MAX_RESIDENT_CONTEXT_BYTES {
        return Err(ToolError::invalid_input(format!(
            "resident_file '{}' is {} bytes; the bounded context limit is {} bytes",
            raw_path,
            metadata.len(),
            MAX_RESIDENT_CONTEXT_BYTES
        )));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| {
            file.take(MAX_RESIDENT_CONTEXT_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)
        })
        .map_err(|error| {
            ToolError::invalid_input(format!(
                "resident_file '{}' could not be read: {error}",
                raw_path
            ))
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_RESIDENT_CONTEXT_BYTES {
        return Err(ToolError::invalid_input(format!(
            "resident_file '{}' grew beyond the bounded {} byte context limit",
            raw_path, MAX_RESIDENT_CONTEXT_BYTES
        )));
    }
    let contents = String::from_utf8(bytes).map_err(|_| {
        ToolError::invalid_input(format!(
            "resident_file '{}' must contain UTF-8 text",
            raw_path
        ))
    })?;
    let workspace = context.workspace.canonicalize().map_err(|error| {
        ToolError::execution_failed(format!(
            "Failed to canonicalize resident workspace {}: {error}",
            context.workspace.display()
        ))
    })?;
    let relative = path.strip_prefix(&workspace).map_err(|_| {
        ToolError::permission_denied(format!(
            "resident_file escapes workspace: {}",
            path.display()
        ))
    })?;
    let display_path =
        normalize_claim_path(&relative.to_string_lossy()).map_err(ToolError::permission_denied)?;
    Ok(ResidentContext {
        display_path,
        // The lease table is process-wide, so a repo-relative path alone
        // would falsely collide across unrelated workspaces. The authority
        // resolver returned a canonical in-workspace file; use that exact
        // identity internally while keeping only the relative label visible.
        lease_key: path.to_string_lossy().into_owned(),
        contents,
    })
}

/// Positive child model-turn budgets are clamped to this hard ceiling. Zero is
/// the unbounded sentinel used by the default agent loop.
const MAX_SUBAGENT_STEPS: u32 = 2_000;
/// Default wall-clock budget for one child run, including model and tool work.
pub(crate) const DEFAULT_CHILD_WALL_TIME: Duration = Duration::from_secs(30 * 60);
const MAX_CHILD_WALL_TIME: Duration = Duration::from_secs(24 * 60 * 60);
/// Default wall-clock budget for a single sub-agent tool execution. The active
/// value travels on `SubAgentRuntime::tool_timeout` so a long-but-legitimate
/// tool (a large build, a slow shell command, a deep search) is not killed
/// mid-flight. Kept non-zero so `timeout(Duration::ZERO, ...)` can never fire
/// immediately. The per-step API timeout, streaming watchdogs, and heartbeat
/// floors remain the independent stall detectors. Derived from the shared
/// `DEFAULT_SUBAGENT_TOOL_TIMEOUT_SECS` so the heartbeat floor (which must sit
/// above this timeout, see `resolve_subagent_heartbeat_timeout_secs`) can never
/// drift from the value actually applied to a running tool.
const DEFAULT_TOOL_TIMEOUT: Duration =
    Duration::from_secs(crate::config::DEFAULT_SUBAGENT_TOOL_TIMEOUT_SECS);
const MIN_EVENT_CHANNEL_HEADROOM_FOR_ROUTINE_PROGRESS: usize = 32;

/// Format a step counter for sub-agent progress messages.
///
fn format_step_counter(steps: u32, max_steps: u32) -> String {
    if max_steps == 0 {
        format!("step {steps}")
    } else {
        format!("step {steps}/{max_steps}")
    }
}

fn resolve_max_steps(role: FleetRole, explicit: Option<u32>, configured: Option<u32>) -> u32 {
    narrow_model_steps(
        WorkerRuntimeProfile::default_max_steps(role),
        narrow_model_steps(configured.unwrap_or(0), explicit.unwrap_or(0)),
    )
    .min(MAX_SUBAGENT_STEPS)
}

/// A queued child whose launch slot never opened did no work: it is a
/// failure to start, not a run that used up its budget (#6015).
fn never_started_reason(queue_limit: Duration) -> String {
    format!(
        "never started: no sub-agent launch slot opened within {}s, so no work ran; start fewer agents at once or retry when running agents finish",
        queue_limit.as_secs()
    )
}

fn child_wall_time_exhausted_reason(limit: Duration) -> String {
    format!(
        "child wall-time budget exhausted (limit: {}s); partial work is preserved; narrow the task or have the operator raise the inherited limit",
        limit.as_secs()
    )
}

/// Render the child's resolved run budgets into its first task message (#6194).
///
/// A child that cannot see its limits cannot pace itself: it discovers the
/// wall clock only when the run is killed mid-request. The block rides inside
/// the task text — the same channel as the enforced write-scope line appended
/// at spawn — so the transcript artifact records exactly what the model was
/// told, and a continuation re-resolves its own values rather than inheriting
/// a stale copy.
fn child_runtime_budget_context(
    runtime: &SubAgentRuntime,
    max_steps: u32,
    work_max_steps: u32,
) -> String {
    let wall = match runtime.worker_profile.wall_deadline_ms {
        Some(deadline_ms) => {
            let remaining = codewhale_command_contract::elapsed::format_elapsed_ms(
                deadline_ms.saturating_sub(epoch_millis_now()),
            );
            match runtime.worker_profile.wall_time_secs {
                Some(total_secs) => format!(
                    "task work stops about {remaining} from now (total run budget {}); model and tool time count against it",
                    codewhale_command_contract::elapsed::format_elapsed_secs(total_secs)
                ),
                None => format!("task work stops about {remaining} from now"),
            }
        }
        None => "no wall-clock limit".to_string(),
    };
    let steps = if max_steps == 0 {
        "no per-run step cap".to_string()
    } else if work_max_steps < max_steps {
        format!(
            "{work_max_steps} model turns of task work (limit {max_steps}; the last turn stays reserved for a bounded hand-back report)"
        )
    } else {
        format!("{max_steps} model turns")
    };
    // One compaction policy for the whole tree: the child inherits the
    // parent session's, so it says here exactly what the parent's does.
    let context = if runtime.compaction.enabled {
        "There is no token budget and no per-step context cap: when the conversation nears the model's context window, the host compacts it automatically (older turns become a summary checkpoint) and the run continues."
    } else {
        "There is no token budget. Automatic context compaction is disabled for this session, so the model's own context window is the only context limit."
    };
    format!(
        "Runtime budget (host-enforced; you cannot raise it):\n- wall clock: {wall}.\n- model steps: {steps}.\n{context} When a limit is reached, task work stops where it stands and only a small reserved hand-back turn remains to report it. Commit or checkpoint work-in-progress early rather than holding it, and keep a current partial report ready."
    )
}

/// The parent session's compaction policy, re-resolved for the route this
/// child actually dispatches on. The parent's trigger is carried over as the
/// same fraction of the window (exactly, when the windows match), so a child
/// on a smaller or larger window compacts at the same relative pressure; the
/// opt-out, summarizer instructions, and retention budget are inherited
/// unchanged. Cost ownership follows the child's own runtime lease.
///
/// Known limitation: a runtime built without a parent session (direct
/// Workflow runs, tests) inherits `CompactionConfig::default()`, which is
/// enabled, rather than re-reading the operator's `auto_compact` setting.
#[cfg(test)]
fn child_compaction_config(
    runtime: &SubAgentRuntime,
    route: &crate::cost_status::EffectiveRouteEnvelope,
    image_input: crate::model_profile::SupportState,
) -> crate::compaction::CompactionConfig {
    let parent = &runtime.compaction;
    let route_limits = runtime.client.route_limits();
    let window = crate::route_budget::route_context_window_tokens(
        route.provider,
        &route.model,
        route_limits,
    );
    let token_threshold = match parent.effective_context_window.filter(|window| *window > 0) {
        Some(parent_window) if parent_window == window => parent.token_threshold,
        parent_window => crate::route_budget::compaction_threshold_for_route_at_percent(
            route.provider,
            &route.model,
            route_limits,
            parent_window.map_or(
                crate::context_budget::DEFAULT_COMPACTION_TRIGGER_PERCENT,
                |parent_window| parent.token_threshold as f64 * 100.0 / f64::from(parent_window),
            ),
        ),
    };
    crate::compaction::CompactionConfig {
        token_threshold,
        model: runtime.model.clone(),
        image_input,
        effective_context_window: Some(window),
        focus: None,
        runtime_cost_owner: runtime
            .runtime_usage_lease
            .as_ref()
            .map(|lease| lease.owner().to_string())
            .or_else(|| parent.runtime_cost_owner.clone()),
        workspace: Some(runtime.context.workspace.clone()),
        ..parent.clone()
    }
}

/// One-shot mid-run notice fired when any enforced budget is roughly
/// three-quarters consumed (#6194). Returns `None` while every bound still
/// has headroom or when no bound applies. This is visibility only: it changes
/// no limit and never interrupts a step.
fn child_budget_pacing_notice(
    started_at: Instant,
    deadline: Option<Instant>,
    steps: u32,
    work_max_steps: u32,
) -> Option<String> {
    let mut consumed = Vec::new();
    if let Some(deadline) = deadline {
        let total = deadline.saturating_duration_since(started_at);
        if total > Duration::ZERO && started_at.elapsed() >= total / 4 * 3 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            consumed.push(format!(
                "wall clock: ~{} remains of ~{}",
                codewhale_command_contract::elapsed::format_elapsed_ms(
                    u64::try_from(remaining.as_millis()).unwrap_or(u64::MAX)
                ),
                codewhale_command_contract::elapsed::format_elapsed_ms(
                    u64::try_from(total.as_millis()).unwrap_or(u64::MAX)
                ),
            ));
        }
    }
    if work_max_steps > 0 && u64::from(steps) * 4 >= u64::from(work_max_steps) * 3 {
        consumed.push(format!("model steps: {steps} of {work_max_steps} used"));
    }
    if consumed.is_empty() {
        return None;
    }
    Some(format!(
        "<codewhale:runtime_event kind=\"budget_pacing\" visibility=\"internal\">\n\
This is an internal runtime event, not user input. Roughly three quarters of a run budget is used:\n- {}\n\
Wrap up now: commit or checkpoint work-in-progress and prepare your final report instead of starting new multi-step work.\n\
</codewhale:runtime_event>",
        consumed.join("\n- ")
    ))
}
// Non-streaming sub-agents need enough response budget to carry large tool-call
// arguments, especially write_file content. The API bills generated tokens, not
// the requested ceiling.
const SUBAGENT_TRANSIENT_PROVIDER_MAX_RETRIES: u32 = 2;
const SUBAGENT_TRANSIENT_PROVIDER_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
/// Hard byte ceiling for any single tool result before it enters the child
/// context. Matches codex-rs `DEFAULT_OUTPUT_BYTES_CAP` (1 MiB). Applied
/// after spillover so the disk backup sees the full original bytes.
const SUBAGENT_TOOL_RESULT_BYTE_CAP: usize = 1_048_576; // 1 MiB
/// Default per-result token cap, matched against a conservative `bytes / 3`
/// estimate. Codex-rs `max_output_tokens` default at `shell_spec.rs:61`.
pub(crate) const SUBAGENT_TOOL_RESULT_TOKEN_CAP_DEFAULT: u32 = 10_000;
/// Conservative bytes-per-token factor (1 token ≈ up to 3–4 bytes on the
/// wire for English/code; each extra byte is safety margin).
const SUBAGENT_TOOL_RESULT_BYTES_PER_TOKEN: usize = 3;

/// Hard-truncate a tool result to the tighter of the byte cap (1 MiB) and the
/// token-cap estimate, appending `[truncated: true]` when either fires.
pub(crate) fn hard_cap_tool_result(
    mut content: String,
    max_output_tokens: std::num::NonZeroU32,
) -> String {
    let token_cap_bytes = max_output_tokens
        .get()
        .saturating_mul(SUBAGENT_TOOL_RESULT_BYTES_PER_TOKEN as u32)
        .min(SUBAGENT_TOOL_RESULT_BYTE_CAP as u32) as usize;
    let effective_cap = SUBAGENT_TOOL_RESULT_BYTE_CAP.min(token_cap_bytes);
    if content.len() <= effective_cap {
        return content;
    }
    // Truncate at a char boundary to avoid splitting a multi-byte UTF-8
    // sequence.
    let mut end = effective_cap;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    content.truncate(end);
    content.push_str("\n[truncated: true]");
    content
}
/// Per-step API-call timeout retry budget. A `create_message` call that
/// exceeds `step_api_timeout` is re-issued up to this many times (with
/// exponential backoff) before the step is interrupted with a preserved
/// checkpoint. Dogfooding showed a single 120s stall wiping out every child
/// in a fan-out one by one even though the provider call was live-but-slow,
/// so a timeout now gets the same retry dignity as a transient provider
/// error (kimi-code comparison, FINISH-0.9.4 entry #40).
const SUBAGENT_API_TIMEOUT_MAX_RETRIES: u32 = 5;
/// Initial backoff for a timed-out per-step API call; doubles per retry and
/// is capped at [`SUBAGENT_API_TIMEOUT_MAX_BACKOFF`].
const SUBAGENT_API_TIMEOUT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Cap for the per-step API-call timeout backoff.
const SUBAGENT_API_TIMEOUT_MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Jitter applied to the timeout backoff (0.2 = ±20%) so a fan-out of
/// children that all time out together does not re-fire in lockstep.
const SUBAGENT_API_TIMEOUT_BACKOFF_JITTER_FACTOR: f64 = 0.2;
/// Per-step LLM API call timeout. Each `create_message` request must complete
/// within this window or the attempt is treated as timed out (and retried up
/// to [`SUBAGENT_API_TIMEOUT_MAX_RETRIES`] times before interrupting the
/// step). Prevents a single stuck API call from blocking the sub-agent
/// indefinitely.
/// Legacy fallback for the per-step DeepSeek API timeout. The active timeout
/// now travels on `SubAgentRuntime::step_api_timeout` so users can override
/// it via `[subagents] api_timeout_secs` in `~/.codewhale/config.toml`. The
/// constant only exists for tests/stub runtimes that need a hard-coded
/// default; production runtimes set the field explicitly (#1806, #1808).
const DEFAULT_STEP_API_TIMEOUT: Duration =
    Duration::from_secs(crate::config::DEFAULT_SUBAGENT_API_TIMEOUT_SECS);
const COMPLETED_AGENT_RETENTION: Duration = Duration::from_secs(60 * 60);
const MAX_AGENT_WORKER_RECORDS: usize = 256;
const MAX_AGENT_WORKER_EVENTS_PER_RECORD: usize = 128;
const MAX_QUEUED_PARENT_MESSAGES: usize = 64;
/// Byte budget for the message tail retained in a [`SubAgentCheckpoint`]
/// (#3882). Checkpoints fire on every step of every worker and are cloned
/// into snapshots, projections, and `subagents.v1.json`; an unbounded
/// `messages` clone turns one large tool output into many resident copies
/// under Fleet fanout. The checkpoint keeps the most recent messages within
/// this budget (always at least the last one, so continuability is
/// preserved) and records how many older messages were omitted. Full tool
/// outputs remain recoverable from the spillover files on disk.
const SUBAGENT_CHECKPOINT_MESSAGE_BUDGET_BYTES: usize = 256 * 1024;
/// Byte budget for the message tail embedded in a `subagent_full_transcript`
/// handle (#3882). One handle is retained in memory per agent; the payload
/// keeps a bounded tail plus the true `message_count` so inspection stays
/// useful without pinning a whole unbounded transcript in RAM.
const SUBAGENT_TRANSCRIPT_MESSAGE_BUDGET_BYTES: usize = 1024 * 1024;
const SUBAGENT_TRANSCRIPT_ARTIFACT_SCHEMA_VERSION: u32 = 2;
const SUBAGENT_TRANSCRIPT_ARTIFACT_DIR: &str = "subagent-transcripts";
const SUBAGENT_STATE_SCHEMA_VERSION: u32 = 1;
const SUBAGENT_STATE_FILE: &str = "subagents.v1.json";
const SUBAGENT_STATE_LOCK_FILE: &str = "subagents.v1.lock";
const SUBAGENT_RESTART_REASON: &str = "Interrupted by process restart";
const SUBAGENT_SESSION_CLOSED_REASON: &str = "Interrupted: parent session closed";
#[cfg(test)]
const SUBAGENT_MODEL_WAIT_REASON: &str = "waiting for model response";
const SUBAGENT_QUEUED_LAUNCH_REASON: &str = "queued: waiting for a sub-agent launch slot";
/// Result text of a parent/operator Stop, before any preservation receipt.
const CANCELLED_BY_PARENT_RESULT: &str = "Cancelled by parent request.";
/// Queued-reason variant used while the rate-limit governor has paused new
/// sub-agent launches after sustained provider 429s.
const SUBAGENT_QUEUED_RATE_LIMIT_REASON: &str = "queued: waiting for provider rate-limit recovery";
/// While queued, probe the governor for a drained rate-limit window at this
/// period (see `acquire_queued_launch_permit`).
const LAUNCH_RECOVERY_PROBE_PERIOD: Duration = Duration::from_secs(5);
/// Placeholder probe period when the runtime has no governor: the probe
/// branch no-ops, and a queued child's wall-time deadline always fires first.
const LAUNCH_RECOVERY_PROBE_PERIOD_WITHOUT_GOVERNOR: Duration = Duration::from_secs(3600);
/// #freeze: minimum spacing between hot-path (per-step checkpoint) state
/// persists. `update_checkpoint` fires on every step of every agent; at high
/// fanout an unconditional full-fleet rewrite under the manager write lock
/// wedges the UI. Hot-path writes coalesce to at most one per this interval;
/// terminal/structural changes still persist immediately, and any terminal
/// write flushes the full in-memory fleet (including other agents' pending
/// checkpoints) to disk.
const SUBAGENT_PERSIST_DEBOUNCE: Duration = Duration::from_millis(1500);

/// #3803: minimum interval between write-locked `cleanup` runs triggered by the
/// sidebar refresh (`Op::ListSubAgents`). Cleanup auto-cancels stale agents
/// (heartbeat timeout, default 300s) and drops old finished records, so a 2s
/// floor keeps it responsive while preventing per-refresh write-lock contention
/// during a high-fanout burst.
pub const SUBAGENT_LIST_CLEANUP_MIN_INTERVAL: Duration = Duration::from_secs(2);

/// #freeze: lightweight perf counters for the sub-agent persist hot path,
/// gated behind `CODEWHALE_SUBAGENT_PERF_TRACE=1`. The atomic increments are
/// always cheap; only the structured `subagent_perf` log line is gated.
static SUBAGENT_PERSIST_WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static SUBAGENT_PERSIST_SKIPPED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn subagent_perf_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("CODEWHALE_SUBAGENT_PERF_TRACE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

const VALID_SUBAGENT_TYPES: &str = "general, explore, planner, reviewer, implement, test, advisor, custom \
     (legacy aliases remain accepted: worker, scout, builder, verifier, consultant, general-purpose, general_purpose, default, exploration/explorer, plan/planning/awaiter, review/code-review/code_review, implementer/implementation, verify/verification/validator/tester, oracle)";
const SUBAGENT_TYPE_DESCRIPTION: &str = "Fleet role for this delegated worker. general: full tool access for multi-step tasks. explore: fast read-only exploration. planner: grounded strategy with read-only probes. reviewer: reads and grades code. implement: lands focused code changes. test: runs tests/validation gates and reports evidence. advisor: read-only high-reasoning counsel for judgement calls and design critique. custom: the tools listed in allowed_tools on the parent's posture. Legacy aliases remain accepted at deserialization boundaries.";

// === Types ===

/// Assignment metadata for sub-agent orchestration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubAgentAssignment {
    pub objective: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_preset: Option<crate::extension_host::composition_scope::NativePresetRef>,
}

impl SubAgentAssignment {
    fn new(objective: String, role: Option<String>) -> Self {
        Self {
            objective,
            role,
            native_preset: None,
        }
    }
}

/// Role presentation: system prompts and config key lookup. The role itself
/// ([`FleetRole`], parsing, posture) lives in [`crate::fleet::role`]; this
/// impl stays on the agent tool because it renders prompt text the tool owns.
impl FleetRole {
    /// Pre-Fleet model-override key (`explorer_model` / `ni_model` tables).
    /// Not used for receipts or UI — only config key lookup.
    #[must_use]
    fn legacy_type_name(&self) -> &'static str {
        match self {
            Self::Worker => "general",
            Self::Scout => "explore",
            Self::Planner => "plan",
            Self::Reviewer => "review",
            Self::Builder => "implementer",
            Self::Verifier => "verifier",
            // Consultant is post-Fleet; it never had a pre-Fleet override table key.
            Self::Consultant => "consultant",
            Self::Custom => "custom",
        }
    }

    /// Get the system prompt for this Fleet role.
    #[must_use]
    pub fn system_prompt(&self) -> String {
        let role_intro = match self {
            Self::Worker => GENERAL_AGENT_INTRO,
            Self::Scout => EXPLORE_AGENT_INTRO,
            Self::Planner => PLAN_AGENT_INTRO,
            Self::Reviewer => REVIEW_AGENT_INTRO,
            Self::Builder => IMPLEMENTER_AGENT_INTRO,
            Self::Verifier => VERIFIER_AGENT_INTRO,
            Self::Consultant => CONSULTANT_AGENT_INTRO,
            Self::Custom => CUSTOM_AGENT_INTRO,
        };
        match self {
            Self::Scout => format!(
                "{role_intro}{}",
                crate::prompts::text::SUBAGENT_SCOUT_OUTPUT_FORMAT
            ),
            _ => format!("{role_intro}{SUBAGENT_OUTPUT_FORMAT}"),
        }
    }
}

/// Status of a sub-agent execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SubAgentStatus {
    Running,
    Completed,
    Interrupted(String),
    Failed(String),
    Cancelled,
    /// Worker stopped at an own/shared token, step, or wall-time budget.
    /// Partial output retains usage and deliverable receipts without claiming
    /// successful completion.
    BudgetExhausted,
}

/// Structured reason a non-running sub-agent needs parent action.
///
/// This is intentionally separate from `SubAgentStatus`: legacy surfaces keep
/// seeing `Interrupted`, while parent-visible projections get a concrete
/// question/action instead of a parked child task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubAgentNeedsInput {
    pub question: String,
}

/// Snapshot of sub-agent state for tool results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentResult {
    /// Provider-reported worker usage from the existing durable ledger. Missing
    /// pricing remains absent; this is never an estimate of a provider charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AgentRunUsage>,
    pub name: String,
    pub agent_id: String,
    pub context_mode: String,
    pub fork_context: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub agent_type: FleetRole,
    pub assignment: SubAgentAssignment,
    #[serde(default)]
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    pub status: SubAgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_status: Option<AgentWorkerStatus>,
    /// Effective non-secret runtime posture for Fleet-backed workers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_permissions: Option<codewhale_protocol::fleet::FleetEffectivePermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default)]
    pub spawn_depth: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_route: Option<ChildRouteReceipt>,
    pub result: Option<String>,
    pub steps_taken: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<SubAgentCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_input: Option<SubAgentNeedsInput>,
    pub duration_ms: u64,
    /// Live start timestamp for elapsed derivation at render (4b). The
    /// `duration_ms` above is a frozen snapshot; this `Instant` is the
    /// source of truth for ticking rows. `None` for deserialized or
    /// non-running agents, `Some` for live running children. Skipped in
    /// serialization so persisted agents remain correct.
    #[serde(skip)]
    pub started_at: Option<std::time::Instant>,
    /// `true` when this agent was loaded from a prior-session persisted
    /// state file rather than spawned in the current session (#405).
    /// Lets listings filter out historical noise by default while
    /// keeping the records reachable via `include_archived=true`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub from_prior_session: bool,
    /// Milliseconds since this running agent last showed the manager any
    /// progress, at snapshot time: the same clock the heartbeat reads when it
    /// auto-stops a stalled child (#6565). `None` once settled. Live-only,
    /// like `started_at`: never serialized, so a model-visible listing and a
    /// persisted record do not change every time they are read.
    #[serde(skip)]
    pub idle_ms: Option<u64>,
    /// The heartbeat bound the manager enforces on that clock: a running
    /// child idle this long is stopped. `None` once settled. Live-only.
    #[serde(skip)]
    pub heartbeat_timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChildRouteReceipt {
    pub requested_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_profile: Option<String>,
    pub resolved_profile_id: Option<String>,
    pub profile_origin: Option<String>,
    pub canonical_role: String,
    pub provider_id: String,
    pub model_id: String,
    pub route_source: String,
    /// Present when the dispatch could not run the route it was asked for:
    /// it names either the unusable pinned provider and the reason
    /// (#5529 mode 2) or the requested fast lane the router had no cheap
    /// sibling for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_note: Option<String>,
    pub requested_reasoning: String,
    pub effective_reasoning: Option<String>,
    pub runtime_version: String,
    pub runtime_build_sha: String,
}
struct RequestedChildRoute {
    requested_type: String,
    requested_profile: Option<String>,
    requested_reasoning: String,
}
/// Headless worker lifecycle states for sub-agent execution.
///
/// This is the TUI-independent state machine that future CLI/API/workflow
/// surfaces should consume. The legacy `SubAgentStatus` remains the
/// compatibility projection returned by sub-agent runs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkerStatus {
    Queued,
    Starting,
    Running,
    WaitingForUser,
    ModelWait,
    RunningTool,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl AgentWorkerStatus {
    /// Terminal worker statuses may be age-evicted from the run ledger (#4217).
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
        )
    }
}

/// Tool capability profile requested for a headless worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentWorkerToolProfile {
    /// Inherit the parent runtime registry for compatibility.
    Inherited,
    /// Use the listed tools only.
    Explicit(Vec<String>),
}

/// Declarative headless worker request derived from `agent`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentWorkerSpec {
    pub worker_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// Workflow run that launched this child, stamped before the worker is
    /// registered. Its terminal result belongs to that run's driver, so the
    /// parent turn must never have it synthesized a second time. Kept apart
    /// from `parent_run_id`, which the lineage walk and manifest owner check
    /// read. Absent on records written before the field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub objective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub agent_type: FleetRole,
    pub model: String,
    pub workspace: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    pub context_mode: String,
    pub fork_context: bool,
    pub tool_profile: AgentWorkerToolProfile,
    #[serde(default)]
    pub runtime_profile: WorkerRuntimeProfile,
    pub max_steps: u32,
    pub spawn_depth: u32,
    pub max_spawn_depth: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_route: Option<ChildRouteReceipt>,
    /// #414 launch authority and #4647 write contract, persisted as one record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_manifest: Option<ChildLaunchManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunFollowUpDelivery {
    pub delivered: bool,
    pub timestamp_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub interrupt: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub continued_from_checkpoint: bool,
}

/// Parent → child mail queued by `agents/message` / `agents/followup`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedParentMessage {
    pub text: String,
    pub queued_at_ms: u64,
    /// When true, delivery should also attempt a live wake (`followup`).
    pub wake: bool,
}

/// Receipt returned by queue / followup coordination helpers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentMailReceipt {
    pub agent_id: String,
    pub status: String,
    pub queue_depth: usize,
    pub woke: bool,
    pub continued_from_checkpoint: bool,
    /// Present when the child is interrupted_continuable and still has a
    /// checkpoint handle. With a runtime attached, `followup` resumes it and
    /// returns the current continuation; a manager-only call queues the note.
    pub continuation_handle: Option<String>,
    pub note: String,
}

/// Compact coordination projection for `agents/list`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentCoordSummary {
    pub agent_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_route: Option<ChildRouteReceipt>,
    pub status: String,
    pub steps_taken: u32,
    #[serde(default)]
    pub recent_progress: Vec<String>,
    #[serde(default)]
    pub queued_mail: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<String>,
    #[serde(default)]
    pub continuable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_claim: Option<PersistedWriteClaim>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_decisions: Vec<DecisionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunFollowUpTarget {
    #[serde(default = "default_agent_inspect_tool")]
    pub tool: String,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default)]
    pub accepted_statuses: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_delivery: Option<AgentRunFollowUpDelivery>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunTakeoverTarget {
    #[serde(default = "default_subagent_takeover_kind")]
    pub kind: String,
    #[serde(default)]
    pub supported: bool,
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsupported_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunArtifactRef {
    pub kind: String,
    pub name: String,
    pub target: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunUsage {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    /// Priced USD subtotal from the immutable per-response route audits, in
    /// microdollars. Absent means the worker has no authoritative USD receipt;
    /// it never means the worker was free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_microusd: Option<u64>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunVerificationSummary {
    pub status: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deliverables: Vec<DeliverableVerdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRunRecommendedAction {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub reason: String,
}

/// Structured headless worker event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentWorkerEvent {
    pub seq: u64,
    pub worker_id: String,
    pub status: AgentWorkerStatus,
    pub timestamp_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

/// Canonical headless worker record retained by `SubAgentManager`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentWorkerRecord {
    pub spec: AgentWorkerSpec,
    /// Root conversation that admitted this worker. Persisted independently
    /// of the optional paired `SubAgent` row so headless workers remain
    /// session-addressable without being guessed into a later conversation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_origin_turn_id: Option<String>,
    #[serde(default = "default_subagent_actor_kind")]
    pub actor_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    #[serde(default = "default_agent_run_follow_up")]
    pub follow_up: AgentRunFollowUpTarget,
    #[serde(default = "default_agent_run_takeover")]
    pub takeover: AgentRunTakeoverTarget,
    #[serde(default)]
    pub artifacts: Vec<AgentRunArtifactRef>,
    #[serde(default = "default_agent_run_usage")]
    pub usage: AgentRunUsage,
    /// Redacted provider-response identities already folded into `usage`.
    /// Persisting the same fingerprint as the session cost snapshot makes a
    /// retried delivery idempotent in both projections after reload.
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
    #[serde(default, skip_serializing_if = "is_false")]
    pub legacy_unreported_usage: bool,
    /// A decoded response omitted usable usage, or a logical provider call
    /// ended at a deadline or cancellation without decoded usage. This records
    /// missing coverage, never inferred billed tokens. An exact late receipt may
    /// resolve its attributed slot; overflow and legacy gaps stay conservative.
    /// Legacy records default to no positively known missing response.
    #[serde(default, skip_serializing_if = "is_false")]
    pub has_unreported_usage: bool,
    #[serde(default)]
    pub delivery_evidence: DeliveryEvidence,
    #[serde(default = "default_agent_run_verification")]
    pub verification: AgentRunVerificationSummary,
    #[serde(default = "default_agent_run_recommended_action")]
    pub recommended_action: AgentRunRecommendedAction,
    /// What this worker is waiting on a person for, derived from the live
    /// pending store on read (approvals C2). Never persisted: a restart ends
    /// every pending wait.
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub pending_request: Option<PendingRequestView>,
    pub status: AgentWorkerStatus,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub steps_taken: u32,
    /// This worker settled by being *parked* at the parent's turn end, not by
    /// asking a question (#5906). See
    /// [`SubAgentCheckpoint::parked_at_turn_end`]; the coordination liveness
    /// predicate reads this flag, and re-dispatching the record clears it.
    #[serde(default)]
    pub parked_at_turn_end: bool,
    #[serde(default)]
    pub events: VecDeque<AgentWorkerEvent>,
}

#[derive(Clone)]
pub(crate) struct CoordinationRegistrationSnapshot {
    worker_records: HashMap<String, AgentWorkerRecord>,
    coordination: CoordinationLedger,
    worker_origins: HashMap<String, WorkerOrigin>,
    worker_origin_schema: u32,
}

impl AgentWorkerRecord {
    /// Build a record exactly as the manager does. `pub(crate)` so the agents
    /// roster projection (#5479) can be tested against real records instead of
    /// a hand-rolled struct literal that would drift from this one.
    #[cfg(test)]
    pub(crate) fn new(spec: AgentWorkerSpec, now_ms: u64) -> Self {
        Self::new_for_session(spec, now_ms, String::new(), None)
    }

    fn new_for_session(
        spec: AgentWorkerSpec,
        now_ms: u64,
        owner_session_id: String,
        precomputed_evidence: Option<DeliveryEvidence>,
    ) -> Self {
        let run_id = agent_worker_run_id(&spec);
        let artifacts = default_subagent_artifacts(&run_id);
        let follow_up = follow_up_target_for_spec(&spec);
        let takeover = takeover_target_for_spec(&spec);
        // #6210: the async spawn path fingerprints pre-lock assuming write
        // capability; the resolved spec permission is authoritative, so a
        // baseline for a read-only worker is discarded, never stored.
        let delivery_evidence = match precomputed_evidence {
            Some(evidence) if spec.runtime_profile.permissions.write => evidence,
            Some(_) => DeliveryEvidence::default(),
            None => DeliveryEvidence::capture(&spec),
        };
        let mut verification = default_agent_run_verification();
        if let Some(manifest) = spec.launch_manifest.as_ref() {
            verification.deliverables = manifest
                .deliverables
                .iter()
                .map(|path| DeliverableVerdict {
                    path: path.clone(),
                    status: "pending".to_string(),
                    bytes: None,
                })
                .collect();
        }
        let recommended_action =
            recommended_action_for_worker_status(AgentWorkerStatus::Starting, &spec);
        Self {
            parent_run_id: spec.parent_run_id.clone(),
            spec,
            owner_session_id,
            runtime_origin_turn_id: None,
            actor_kind: default_subagent_actor_kind(),
            follow_up,
            takeover,
            artifacts,
            usage: default_agent_run_usage(),
            usage_source_fingerprints: BTreeSet::new(),
            missing_usage_sources: BTreeMap::new(),
            missing_usage_overflowed: false,
            legacy_unreported_usage: false,
            has_unreported_usage: false,
            delivery_evidence,
            verification,
            recommended_action,
            pending_request: None,
            status: AgentWorkerStatus::Starting,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            started_at_ms: None,
            completed_at_ms: None,
            latest_message: None,
            result_summary: None,
            error: None,
            steps_taken: 0,
            parked_at_turn_end: false,
            events: VecDeque::new(),
        }
    }
}

fn default_subagent_actor_kind() -> String {
    "subagent".to_string()
}

fn default_agent_inspect_tool() -> String {
    "handle_read".to_string()
}

fn default_subagent_takeover_kind() -> String {
    "local_subagent_session".to_string()
}

fn default_agent_run_follow_up() -> AgentRunFollowUpTarget {
    AgentRunFollowUpTarget {
        tool: default_agent_inspect_tool(),
        agent_id: String::new(),
        session_name: None,
        accepted_statuses: vec!["running".to_string(), "interrupted_continuable".to_string()],
        latest_delivery: None,
    }
}

fn default_agent_run_takeover() -> AgentRunTakeoverTarget {
    AgentRunTakeoverTarget {
        kind: default_subagent_takeover_kind(),
        supported: false,
        agent_id: String::new(),
        session_name: None,
        instructions: "No takeover target is available for this older record.".to_string(),
        unsupported_reason: Some("legacy_record_missing_agent_id".to_string()),
    }
}

fn default_agent_run_usage() -> AgentRunUsage {
    AgentRunUsage {
        status: "unknown".to_string(),
        input_tokens: None,
        output_tokens: None,
        total_tokens: None,
        cost_microusd: None,
        note: "Token usage is not yet reported by the sub-agent worker ledger.".to_string(),
    }
}

fn usage_total_tokens(usage: &Usage) -> u64 {
    u64::from(usage.input_tokens).saturating_add(u64::from(usage.output_tokens))
}

fn usage_has_reported_data(usage: &Usage) -> bool {
    usage.input_tokens > 0
        || usage.output_tokens > 0
        || usage.prompt_cache_hit_tokens.is_some()
        || usage.prompt_cache_miss_tokens.is_some()
        || usage.prompt_cache_write_tokens.is_some()
        || usage.reasoning_tokens.is_some()
        || usage.reasoning_replay_tokens.is_some()
        || usage.server_tool_use.is_some()
}

/// Convert an authoritative USD audit into the workflow IR's integer
/// microdollar receipt. Route coverage stays on the cost-status path; this
/// narrow projection deliberately preserves only a priced subtotal.
fn priced_usd_microusd(audit: &crate::pricing::TurnCostAudit) -> Option<u64> {
    if !audit.usd_priced {
        return None;
    }
    let usd = audit.estimate?.usd;
    let microusd = usd * 1_000_000.0;
    if !microusd.is_finite() || !(0.0..=(u64::MAX as f64)).contains(&microusd) {
        return None;
    }
    Some(microusd.round() as u64)
}

/// Immutable accounting ownership of one sub-agent dispatch.
///
/// Captured once, when the runtime is built (`SubAgentRuntime::new`: the
/// engine's per-turn runtime, its off-turn continuation runtime, the direct
/// Workflow runtime), and inherited unchanged by every `child_runtime` /
/// `background_runtime` descendant and by the child tool registry's guardian
/// runtime. It replaces the completion-time `cost_status::scope_token()` read
/// `record_provider_response_usage` used to perform: a provider response that
/// lands after `/new` or a session load settles against the session it was
/// dispatched from, never against whichever session is live when it arrives.
///
/// Runtime-owned dispatches (a `RuntimeUsageLease`) already carry their origin
/// inside the owner sink. This origin governs the ownerless paths: off-turn
/// continuations, direct Workflow runtimes, and their missing-usage receipts.
#[derive(Debug, Clone)]
pub(crate) struct SubAgentAccountingOrigin {
    /// Cost-scope generation at dispatch. The live pool rejects a stale token;
    /// it is never re-derived at completion.
    cost_scope: crate::cost_status::CostScopeToken,
    /// Root session of the dispatch: the tool context's state namespace, the
    /// same identity the terminal fan-in and coordination paths use. A receipt
    /// whose scope has been retired is appended to this session's durable
    /// late-usage sidecar.
    session_id: String,
}

impl SubAgentAccountingOrigin {
    fn capture(context: &ToolContext) -> Self {
        Self {
            cost_scope: crate::cost_status::scope_token(),
            session_id: context.state_namespace.clone(),
        }
    }

    /// Settle one reported provider response that has no runtime owner. A
    /// live origin scope charges the interactive pool exactly once per source
    /// id; a retired one appends the exact frozen receipt to the origin
    /// session's sidecar, labelled by the child run that dispatched it. Either
    /// way the replacement session never sees it.
    fn report_ownerless_usage(
        &self,
        agent_id: &str,
        source_id: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
        usage: &Usage,
    ) {
        crate::cost_status::report_effective_route_for_interactive_origin(
            self.cost_scope,
            &self.session_id,
            agent_id,
            source_id,
            route,
            usage,
        );
    }
}

/// Publish one child provider response into every projection that owns it.
/// The stable source id is the shared exactly-once key: runtime/session cost
/// and the durable worker record hash it with the same canonical function.
/// Accounting ownership is the runtime's dispatch-time
/// [`SubAgentAccountingOrigin`]; nothing here consults the live cost scope.
async fn record_provider_response_usage(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    source_id: &str,
    route: crate::cost_status::EffectiveRouteEnvelope,
    usage: &Usage,
) {
    let priced = report_provider_response_usage_origin(
        runtime,
        agent_id,
        source_id,
        &route,
        usage,
        crate::cost_status::RuntimeUsageMissingReason::SuccessWithoutUsage,
    );
    runtime
        .manager
        .write()
        .await
        .record_worker_routed_usage(agent_id, source_id, &route, usage, priced);
}
fn report_provider_response_usage_origin(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    source_id: &str,
    route: &crate::cost_status::EffectiveRouteEnvelope,
    usage: &Usage,
    missing_reason: crate::cost_status::RuntimeUsageMissingReason,
) -> Option<u64> {
    engine::ChildAccountingProjection::capture(runtime, agent_id).publish(
        source_id,
        route,
        usage,
        missing_reason,
    )
}

fn new_child_execution_id(agent_id: &str) -> String {
    format!("agent:{agent_id}:approval:{}", Uuid::new_v4())
}

/// One logical held child-tool call gets one guardian usage identity even if a
/// mailbox/monitor replays its receipt. Raw agent/tool ids can be model-owned,
/// so only this fixed-length digest crosses telemetry or persistence seams.
#[cfg(test)]
fn child_guardian_usage_source_id(agent_id: &str, tool_id: &str) -> String {
    use sha2::{Digest as _, Sha256};

    let mut digest = Sha256::new();
    for part in [
        b"codewhale:subagent-auto-review-guardian:v1".as_slice(),
        agent_id.as_bytes(),
        tool_id.as_bytes(),
    ] {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    format!(
        "subagent-guardian:{}",
        crate::hashing::hex_bytes(digest.finalize())
    )
}

fn refresh_usage_note(usage: &mut AgentRunUsage) {
    let worker_total = usage.total_tokens.unwrap_or(0);
    if worker_total > 0 {
        usage.status = "reported".to_string();
        usage.note = format!("Provider reported {worker_total} tokens for this worker.");
    } else if usage.status.is_empty() {
        *usage = default_agent_run_usage();
    }
}

fn default_agent_run_verification() -> AgentRunVerificationSummary {
    AgentRunVerificationSummary {
        status: "self_report_only".to_string(),
        summary:
            "No verified command or test receipt is attached; treat the result summary as a child self-report."
                .to_string(),
        deliverables: Vec::new(),
    }
}

fn default_agent_run_recommended_action() -> AgentRunRecommendedAction {
    AgentRunRecommendedAction {
        action: "inspect_transcript".to_string(),
        tool: Some(default_agent_inspect_tool()),
        reason: "Inspect the returned transcript handle if the child result needs audit detail."
            .to_string(),
    }
}

/// A person must decide something for this agent. No tool approves on the
/// model's behalf, so the action is to tell the user where to answer.
fn tell_user_recommended_action(
    spec: &AgentWorkerSpec,
    pending: &PendingRequestView,
) -> AgentRunRecommendedAction {
    let agent_ref = spec
        .session_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(&spec.worker_id);
    AgentRunRecommendedAction {
        action: "tell_user".to_string(),
        tool: None,
        reason: format!(
            "A decision is waiting in {agent_ref}: '{}' — {}. Tell the user; you cannot approve it.",
            pending.tool, pending.summary
        ),
    }
}

fn recommended_action_for_worker_status(
    status: AgentWorkerStatus,
    spec: &AgentWorkerSpec,
) -> AgentRunRecommendedAction {
    let agent_ref = spec
        .session_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(&spec.worker_id);
    match status {
        AgentWorkerStatus::Queued => AgentRunRecommendedAction {
            action: "continue_parent_work".to_string(),
            tool: None,
            reason: format!(
                "Worker {agent_ref} is queued in the background; continue coordinating and consume its completion event when it arrives."
            ),
        },
        AgentWorkerStatus::Starting
        | AgentWorkerStatus::Running
        | AgentWorkerStatus::ModelWait
        | AgentWorkerStatus::RunningTool => AgentRunRecommendedAction {
            action: "continue_parent_work".to_string(),
            tool: None,
            reason: format!(
                "Worker {agent_ref} is active in the background; continue parent work until its completion event arrives."
            ),
        },
        AgentWorkerStatus::WaitingForUser => AgentRunRecommendedAction {
            action: "inspect_or_replace".to_string(),
            tool: Some(default_agent_inspect_tool()),
            reason: format!(
                "Worker {agent_ref} needs parent action; inspect the transcript handle and open a replacement with agent if the task still matters."
            ),
        },
        AgentWorkerStatus::Completed => AgentRunRecommendedAction {
            action: "verify_self_report".to_string(),
            tool: Some("handle_read".to_string()),
            reason: format!(
                "Worker {agent_ref} completed; verify its self-report before treating side effects as fact."
            ),
        },
        AgentWorkerStatus::Failed => AgentRunRecommendedAction {
            action: "inspect_failure".to_string(),
            tool: Some(default_agent_inspect_tool()),
            reason: format!(
                "Worker {agent_ref} failed; inspect the transcript handle and decide whether to open a replacement."
            ),
        },
        AgentWorkerStatus::Cancelled => AgentRunRecommendedAction {
            action: "open_replacement_if_needed".to_string(),
            tool: Some("agent".to_string()),
            reason: format!(
                "Worker {agent_ref} was cancelled; open a replacement with agent only if the assignment still matters."
            ),
        },
        AgentWorkerStatus::Interrupted => AgentRunRecommendedAction {
            action: "inspect_or_replace".to_string(),
            tool: Some(default_agent_inspect_tool()),
            reason: format!(
                "Worker {agent_ref} was interrupted; inspect the transcript handle before deciding whether to re-dispatch."
            ),
        },
    }
}

fn agent_worker_run_id(spec: &AgentWorkerSpec) -> String {
    if spec.run_id.is_empty() {
        spec.worker_id.clone()
    } else {
        spec.run_id.clone()
    }
}

fn follow_up_target_for_spec(spec: &AgentWorkerSpec) -> AgentRunFollowUpTarget {
    AgentRunFollowUpTarget {
        tool: default_agent_inspect_tool(),
        agent_id: spec.worker_id.clone(),
        session_name: spec.session_name.clone(),
        accepted_statuses: vec!["running".to_string(), "interrupted_continuable".to_string()],
        latest_delivery: None,
    }
}

fn takeover_target_for_spec(spec: &AgentWorkerSpec) -> AgentRunTakeoverTarget {
    let agent_ref = spec
        .session_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(&spec.worker_id);
    AgentRunTakeoverTarget {
        kind: default_subagent_takeover_kind(),
        supported: true,
        agent_id: spec.worker_id.clone(),
        session_name: spec.session_name.clone(),
        instructions: format!(
            "Inspect agent '{agent_ref}' through the returned transcript_handle with handle_read; open a replacement with agent if the lane no longer fits."
        ),
        unsupported_reason: None,
    }
}

fn default_subagent_artifacts(run_id: &str) -> Vec<AgentRunArtifactRef> {
    vec![
        AgentRunArtifactRef {
            kind: "worker_events".to_string(),
            name: "worker_record.events".to_string(),
            target: run_id.to_string(),
            description: "Bounded structured lifecycle events retained on the worker record."
                .to_string(),
        },
        AgentRunArtifactRef {
            kind: "transcript".to_string(),
            name: "transcript_handle".to_string(),
            target: format!("agent:{run_id}"),
            description: "Open loads the complete private chat artifact, including the child's agent-owned todo_write working notes; use the bounded transcript_handle with handle_read for slices and artifact metadata."
                .to_string(),
        },
        AgentRunArtifactRef {
            kind: "receipt".to_string(),
            name: "result_summary".to_string(),
            target: run_id.to_string(),
            description: "Child final summary when present; verify before treating as fact."
                .to_string(),
        },
    ]
}

/// Whether a settled child's result is the parent turn's to receive.
///
/// A nested child reports to its own parent agent, and a workflow child
/// reports to its run's driver, which folds the result into the one workflow
/// receipt. Synthesizing either into the root turn delivered the same report
/// twice and billed it twice.
fn delivers_to_parent_turn(spec: &AgentWorkerSpec) -> bool {
    spec.parent_run_id.is_none() && spec.workflow_run_id.is_none()
}

fn normalize_worker_spec(mut spec: AgentWorkerSpec) -> AgentWorkerSpec {
    if spec.run_id.is_empty() {
        spec.run_id = spec.worker_id.clone();
    }
    canonicalize_persisted_advisory_role(&mut spec.role);
    spec
}

fn canonicalize_persisted_advisory_role(role: &mut Option<String>) {
    if role.as_deref().is_some_and(|role| {
        matches!(
            role.trim().to_ascii_lowercase().as_str(),
            "oracle" | "advisor"
        )
    }) {
        *role = Some(FleetRole::Consultant.as_str().to_string());
    }
}

fn worker_coordination_claim(
    spec: &AgentWorkerSpec,
) -> Result<Option<(WriteScopeClaim, bool)>, String> {
    if !spec.runtime_profile.permissions.write {
        return Ok(None);
    }
    let manifest = spec.launch_manifest.as_ref().ok_or_else(|| {
        format!(
            "write-capable worker '{}' requires a persisted ChildLaunchManifest",
            spec.worker_id
        )
    })?;
    if manifest.child_id != spec.worker_id {
        return Err(format!(
            "worker '{}' launch manifest belongs to '{}'",
            spec.worker_id, manifest.child_id
        ));
    }
    let normalize_paths = |values: &[String], field: &str| {
        if values.len() > 32 {
            return Err(format!(
                "worker '{}' {field} accepts at most 32 entries",
                spec.worker_id
            ));
        }
        let mut normalized = Vec::new();
        for value in values {
            let value = normalize_claim_path(value)?;
            if !normalized.contains(&value) {
                normalized.push(value);
            }
        }
        Ok(normalized)
    };
    let roots = normalize_paths(&manifest.writable_roots, "writable_roots")?;
    let exact_files = normalize_paths(&manifest.writable_files, "writable_files")?;
    if manifest.coordination_contracts.len() > 16 {
        return Err(format!(
            "worker '{}' coordination_contracts accepts at most 16 entries",
            spec.worker_id
        ));
    }
    let mut contracts = Vec::new();
    for contract in &manifest.coordination_contracts {
        let contract = contract.trim();
        if contract.is_empty() || contract.chars().count() > 128 {
            return Err(format!(
                "worker '{}' coordination contracts must be 1..=128 characters",
                spec.worker_id
            ));
        }
        if !contracts.iter().any(|existing| existing == contract) {
            contracts.push(contract.to_string());
        }
    }
    if roots.is_empty() && exact_files.is_empty() && contracts.is_empty() {
        return Err(format!(
            "write-capable worker '{}' requires a bounded root, exact file, or coordination contract",
            spec.worker_id
        ));
    }
    Ok(Some((
        WriteScopeClaim {
            owner: spec.worker_id.clone(),
            roots,
            exact_files,
            contracts,
        },
        manifest.worktree,
    )))
}

fn worker_tool_scope(tool_profile: &AgentWorkerToolProfile) -> ToolScope {
    match tool_profile {
        AgentWorkerToolProfile::Inherited => ToolScope::Inherit,
        AgentWorkerToolProfile::Explicit(tools) => ToolScope::Explicit(tools.clone()),
    }
}

fn worker_profile_from_spec(spec: &AgentWorkerSpec) -> WorkerRuntimeProfile {
    let mut profile = WorkerRuntimeProfile::for_role(spec.agent_type.clone());
    profile.tools = worker_tool_scope(&spec.tool_profile);
    profile.model = ModelRoute::Fixed(spec.model.clone());
    profile.max_spawn_depth = spec.max_spawn_depth;
    profile.spawn_depth = spec.spawn_depth;
    profile.max_steps = spec.max_steps.min(MAX_SUBAGENT_STEPS);
    profile.background = true;
    profile
}

fn worker_profile_for_spawn(
    runtime: &SubAgentRuntime,
    agent_type: &FleetRole,
    tool_profile: &AgentWorkerToolProfile,
    effective_model: &str,
    model_route: Option<ModelRoute>,
    custom_write_authority: bool,
) -> WorkerRuntimeProfile {
    let mut requested = WorkerRuntimeProfile::for_role(agent_type.clone());
    // Custom inherits the parent's effective posture by default (its explicit
    // tool list is the narrowing). The bounded write authority a spawning call
    // may pass is kept as an explicit, redundant grant for older callers.
    // Parent intersection below remains the hard ceiling.
    if *agent_type == FleetRole::Custom && custom_write_authority {
        requested.permissions.write = true;
        requested.shell = ShellPolicy::Full;
    }
    requested.tools = worker_tool_scope(tool_profile);
    requested.model = model_route.unwrap_or_else(|| ModelRoute::Fixed(effective_model.to_string()));
    requested.provider = Some(runtime.client.admitted_provider_identity().key.to_string());
    requested.max_spawn_depth = runtime.max_spawn_depth;
    requested.background = true;
    let mut profile = runtime.worker_profile.derive_child(&requested);
    profile.spawn_depth = runtime.spawn_depth;
    // Route binding already resolved the child tier, including an absent tier.
    // Authority intersection must not re-inherit a different parent setting.
    profile.reasoning_effort = runtime.reasoning_effort.clone();
    profile
}

fn normalize_worker_record(mut record: AgentWorkerRecord) -> AgentWorkerRecord {
    if record.has_unreported_usage
        && record.missing_usage_sources.is_empty()
        && !record.missing_usage_overflowed
    {
        record.legacy_unreported_usage = true;
    }
    record.spec = normalize_worker_spec(record.spec);
    if record.spec.runtime_profile == WorkerRuntimeProfile::default() {
        record.spec.runtime_profile = worker_profile_from_spec(&record.spec);
    }
    // Pre-migration profile depths were relative. Keep their smaller ceiling
    // rather than upgrading old persisted authority; align the current position
    // from the authoritative worker spec.
    record.spec.runtime_profile.spawn_depth = record.spec.spawn_depth;
    record.spec.max_spawn_depth = record
        .spec
        .max_spawn_depth
        .min(record.spec.runtime_profile.max_spawn_depth);
    record.spec.runtime_profile.max_spawn_depth = record.spec.max_spawn_depth;
    let run_id = agent_worker_run_id(&record.spec);
    if record.actor_kind.is_empty() {
        record.actor_kind = default_subagent_actor_kind();
    }
    if record.parent_run_id.is_none() {
        record.parent_run_id = record.spec.parent_run_id.clone();
    }
    if record.follow_up.agent_id.is_empty() {
        record.follow_up = follow_up_target_for_spec(&record.spec);
    } else if record.follow_up.tool != default_agent_inspect_tool() {
        record.follow_up.tool = default_agent_inspect_tool();
    }
    if record.takeover.agent_id.is_empty()
        || !record
            .takeover
            .instructions
            .contains(&default_agent_inspect_tool())
    {
        record.takeover = takeover_target_for_spec(&record.spec);
    }
    record.recommended_action = recommended_action_for_worker_status(record.status, &record.spec);
    if record.artifacts.is_empty() {
        record.artifacts = default_subagent_artifacts(&run_id);
    }
    if record.usage.status.is_empty() {
        record.usage = default_agent_run_usage();
    } else {
        refresh_usage_note(&mut record.usage);
    }
    if record.verification.status.is_empty() {
        record.verification = default_agent_run_verification();
    }
    record
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn current_git_branch(workspace: &Path) -> Option<String> {
    let branch = run_git(workspace, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    if branch.is_empty() {
        return None;
    }
    if branch != "HEAD" {
        return Some(branch.to_string());
    }

    let short_hash = run_git(workspace, &["rev-parse", "--short", "HEAD"])?;
    let short_hash = short_hash.trim();
    (!short_hash.is_empty()).then(|| format!("detached:{short_hash}"))
}

fn run_git(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Git::output(args, workspace).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).to_string())
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SubAgentSpawnOptions {
    /// Private captured caller receipt; never supplied by a tool request.
    pub origin: Option<WorkerOrigin>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub model_route: Option<ModelRoute>,
    pub child_route: Option<ChildRouteReceipt>,
    pub nickname: Option<String>,
    pub fork_context: bool,
    /// Per-tool-result output token cap for this child (#6282). `None` uses
    /// the runtime default (10k).
    pub max_output_tokens: Option<std::num::NonZeroU32>,
    /// Optional per-child model-turn override, clamped to the runtime ceiling.
    pub max_steps: Option<u32>,
    /// Optional per-child wall-clock override, clamped to the runtime ceiling.
    pub wall_time: Option<Duration>,
    pub write_claim: Option<WriteScopeClaim>,
    pub isolated_worktree: bool,
    pub expected_artifact: Option<String>,
    pub deliverables: Vec<String>,
    /// Source agent id this child continues, stamped into the ChildLaunchManifest
    /// for receipt traceability.
    pub resume_from_agent_id: Option<String>,
    /// Internal continuation, distinct from a deliberate transcript fork.
    /// Commits the old-to-new link with the worker before task scheduling.
    pub checkpoint_continuation: bool,
    /// Checkpoint resume: the claim comes from the coordination ledger and is
    /// already namespaced — skip re-namespacing in the spawn seam.
    pub claim_pre_namespaced: bool,
    /// Checkpoint resume: preserve the interrupted child's runtime posture
    /// instead of rebuilding it from the caller's role.
    pub preserve_runtime_profile: Option<WorkerRuntimeProfile>,
    /// Workflow run that owns this child; recorded on the worker spec.
    pub workflow_run_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowTaskSpawnResult {
    pub result: SubAgentResult,
    pub metadata: WorkflowTaskSpawnMetadata,
}

/// Workflow identity stamped onto children launched via `spawn_workflow_task`
/// (#4119). Lets panel/history render without parsing the child prompt.
#[derive(Debug, Clone)]
pub(crate) struct WorkflowTaskSpawnIdentity {
    pub workflow_run_id: String,
    pub workflow_phase_id: Option<String>,
    pub workflow_task_label: Option<String>,
    pub workflow_child_index: u32,
    /// Fingerprint of the exact-Fleet permission envelope this task was routed
    /// under, taken from the durable receipt.
    ///
    /// `None` for every non-Fleet task. When it is `Some`,
    /// [`spawn_workflow_task`] recomputes the fingerprint from the spawn input
    /// it actually built and refuses the launch on any difference. That check is
    /// what makes the Fleet's clamped authority a property of the child that
    /// runs rather than a value recorded next to it.
    pub fleet_authority_fingerprint: Option<String>,
    /// Host-only member captured from the exact Fleet snapshot. This value is
    /// never deserialized from tool input or looked up in the selected Fleet.
    pub exact_fleet_binding: Option<crate::fleet::exact::ExactMemberBinding>,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkflowTaskSpawnMetadata {
    pub child_route: ChildRouteReceipt,
    pub resolved_provider: String,
    pub resolved_model: String,
    pub route_source: String,
    pub requested_reasoning: Option<String>,
    pub effective_reasoning: Option<String>,
    pub resolved_role: Option<String>,
    pub resolved_profile: Option<String>,
    pub parent_task_id: Option<String>,
    pub depth: u32,
    /// Workflow run that launched this child (`None` for direct `agent` spawns).
    pub workflow_run_id: Option<String>,
    /// Active phase title/id when the child was admitted (`None` outside workflows).
    pub workflow_phase_id: Option<String>,
    /// Human label from the Workflow `task({ label })` option.
    pub workflow_task_label: Option<String>,
    /// 0-based admission order among children of this workflow run.
    pub workflow_child_index: Option<u32>,
    /// Source agent this child was continued from via `resume_from`, if any.
    pub resume_from_agent_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubAgentModelStrength {
    Same,
    Faster,
}

impl SubAgentModelStrength {
    fn parse(value: &str) -> Result<Self, ToolError> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "same" | "inherit" | "parent" | "current" => Ok(Self::Same),
            "faster" | "fast" | "smaller" | "small" | "lower" | "cheap" | "flash" => {
                Ok(Self::Faster)
            }
            _ => Err(ToolError::invalid_input(
                "model_strength must be one of: same, faster".to_string(),
            )),
        }
    }

    fn model_route(self) -> ModelRoute {
        match self {
            Self::Same => ModelRoute::Inherit,
            Self::Faster => ModelRoute::Faster,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubAgentThinking {
    Inherit,
    Auto,
    Effort(ReasoningEffort),
}

/// The `thinking` values the `agent` schema advertises. The schema enum and
/// the rejection message both read this list, so they cannot name different
/// levels. Every entry must parse; `thinking_schema_values_all_parse` pins it.
const SUBAGENT_THINKING_SCHEMA_VALUES: [&str; 9] = [
    "inherit", "auto", "off", "low", "medium", "high", "xhigh", "max", "ultra",
];

impl SubAgentThinking {
    fn parse(value: &str) -> Result<Self, ToolError> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "inherit" | "parent" | "same" | "current" => Ok(Self::Inherit),
            _ => ReasoningEffort::parse_strict(value)
                .map(|effort| match effort {
                    ReasoningEffort::Auto => Self::Auto,
                    effort => Self::Effort(effort),
                })
                .map_err(|_| {
                    ToolError::invalid_input(format!(
                        "thinking must be one of: {}",
                        SUBAGENT_THINKING_SCHEMA_VALUES.join(", ")
                    ))
                }),
        }
    }
}

/// Stable, non-secret label for the reasoning a caller *requested* (#4039).
///
/// `inherit`/`auto` are requests, not efforts: they must stay distinguishable
/// from the effort the route resolved to, or a row could claim the user asked
/// for something they only got.
pub(crate) fn subagent_thinking_label(thinking: SubAgentThinking) -> &'static str {
    match thinking {
        SubAgentThinking::Inherit => "inherit",
        SubAgentThinking::Auto => "auto",
        SubAgentThinking::Effort(effort) => effort.as_setting(),
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SubAgentInput {
    text: String,
    interrupt: bool,
    /// Live "queued for the next round" counter shared with the manager.
    /// Decremented when the child loop takes the input off its channel, so
    /// the rail's `· N queued` count is the truthful number of follow-ups the
    /// child has not yet folded into its next model round.
    pending: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl SubAgentInput {
    /// Mark this input as consumed by the child loop.
    fn mark_taken(&self) {
        if let Some(pending) = self.pending.as_ref() {
            // Saturating decrement as a compare-exchange loop: `fetch_update`
            // is deprecated from Rust 1.99 and `try_update` is newer than the MSRV.
            use std::sync::atomic::Ordering::{AcqRel, Acquire};
            let mut value = pending.load(Acquire);
            while value > 0 {
                match pending.compare_exchange_weak(value, value - 1, AcqRel, Acquire) {
                    Ok(_) => break,
                    Err(current) => value = current,
                }
            }
        }
    }
}

#[cfg(test)]
fn append_subagent_inputs_as_user_messages(
    messages: &mut Vec<Message>,
    pending_inputs: &mut VecDeque<SubAgentInput>,
) {
    while let Some(input) = pending_inputs.pop_front() {
        if !input.text.trim().is_empty() {
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: input.text,
                    cache_control: None,
                }],
            });
        }
    }
}

#[derive(Debug, Clone)]
struct SpawnRequest {
    session_name: Option<String>,
    prompt: String,
    /// Explicit bounded prerequisites relevant to this child. Persisted in the
    /// launch prompt; never populated from a parent transcript.
    dependencies: Vec<String>,
    /// Explicit bounded acceptance checks relevant to this child.
    acceptance: Vec<String>,
    agent_type: FleetRole,
    /// True when the caller supplied `type`/`agent_type` or `role` explicitly
    /// (vs the `Worker` default). A `profile` only sets the agent type when
    /// the caller did not, and conflicts are rejected only for explicit
    /// values.
    agent_type_explicit: bool,
    /// True only when the caller wrote the `type` field itself. `role` also
    /// sets `agent_type_explicit` (a role may be a type alias), but a role is
    /// a label for role resolution while `type` is a claim about what
    /// the child can do. Only the latter can contradict `write_authority`
    /// (#5123).
    agent_type_named: bool,
    /// Saved Fleet member selector or built-in role. Resolved from the trusted
    /// current roster at spawn time; parsing has no runtime access.
    profile: Option<String>,
    assignment: SubAgentAssignment,
    allowed_tools: Option<Vec<String>>,
    model: Option<String>,
    model_strength: SubAgentModelStrength,
    /// True when the caller supplied `model_strength` explicitly. An explicit
    /// strength outranks a configured role default; the parse-time
    /// default does not.
    model_strength_explicit: bool,
    thinking: SubAgentThinking,
    thinking_explicit: bool,
    /// Optional working directory for the child. Must canonicalize to a path
    /// inside the parent's workspace. For first-class git worktree isolation,
    /// use `worktree` instead of pre-creating a cwd by hand.
    cwd: Option<PathBuf>,
    /// Optional first-class git worktree isolation. When set, Codewhale
    /// creates a sibling worktree/branch and runs the child from that checkout.
    worktree: Option<SubAgentWorktreeRequest>,
    /// Optional file path for cache-aware resident mode (#529). When set,
    /// the child's prompt is prefixed with the file contents for prefix-cache
    /// locality. A global ownership table prevents two agents from holding
    /// a resident lease on the same file simultaneously.
    resident_file: Option<String>,
    /// `Some(true)`: seed the child with the parent's system prompt and
    /// message prefix before appending the child task. `Some(false)`: force a
    /// fresh isolated context. `None` keeps the child fresh; transcript
    /// inheritance is always an explicit caller decision.
    fork_context: Option<bool>,
    /// Legacy recursion budget for descendants. The model-facing child tool
    /// surface is leaf-only; this remains for persisted/internal records.
    max_depth: Option<u32>,
    /// Per-tool-result output token cap for this child (#6282). `None` uses
    /// the runtime default (10k); `Some(n)` clamps every tool result to at
    /// most n tokens before it enters the child's message history.
    max_output_tokens: Option<u32>,
    max_steps: Option<u32>,
    wall_time: Option<Duration>,
    /// Extra tool deny-list from the caller, unioned with the parent runtime's
    /// inherited deny-list. Deny always wins over allow (#4042).
    disallowed_tools: Option<Vec<String>>,
    /// When true (default), the child inherits the parent runtime's
    /// `disallowed_tools`. Legacy `false` is accepted but cannot remove any
    /// ancestor or operator restriction.
    inherit_disallowed_tools: bool,
    /// Declared child write authority. Not schema decoration: `ReadOnly`
    /// narrows the child worker profile's write permission before spawn, so a
    /// child declared read-only cannot run Suggest-level write tools
    /// (TUI-DOG-017 truthful-affordance gate).
    write_authority: Option<SpawnWriteAuthority>,
    /// Declared expected artifact. Surfaced to the child in its prompt so the
    /// contract the spawner declared is visible to the agent doing the work.
    expected_artifact: Option<String>,
    deliverables: Vec<String>,
    /// Expected mutation boundary. Write-capable launches without an explicit
    /// root, exact file, or named contract default to the parent workspace
    /// root (`"."`). Escalation outside the parent workspace is refused.
    write_roots: Vec<String>,
    exact_files: Vec<String>,
    coordination_contracts: Vec<String>,
    /// Optional settled child agent id or session name whose transcript this
    /// child should continue. When set, the source agent's messages are loaded
    /// and injected as the initial context (fork_context=true), preserving
    /// transcript lineage across role/profile transitions (explore → implement
    /// → verify). The source must be settled (not running), in the same
    /// workspace, and reachable by the spawning agent.
    resume_from: Option<String>,
    /// Detached children deliberately outlive the active parent turn. The
    /// default is foreground ownership: explicit cancellation stops the
    /// child's entire non-detached subtree before the turn becomes terminal.
    detached: bool,
}

/// Declared child write authority for a (deliberate) spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnWriteAuthority {
    ReadOnly,
    WorkspaceWrite,
    WorktreeWrite,
}

/// Which terminal states `resume_from_checkpoint_with_policy` may re-dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResumePolicy {
    /// The model-facing `agents/followup` contract: interrupted children only.
    InterruptedOnly,
    /// The operator-facing continue-a-fork contract: interrupted or completed
    /// children whose last checkpoint is continuable.
    InterruptedOrCompleted,
}

impl ResumePolicy {
    fn describe(self) -> &'static str {
        match self {
            Self::InterruptedOnly => "only interrupted children are resumable",
            Self::InterruptedOrCompleted => {
                "only interrupted or completed children can be continued"
            }
        }
    }
}

/// Receipt for a user-originated follow-up to a child agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserFollowUpOutcome {
    /// The child the user addressed.
    pub agent_id: String,
    /// The child that now carries the conversation: the same id when the
    /// message was delivered live, or the resumed fork's new id.
    pub target_agent_id: String,
    /// Whether the text actually reached a live loop.
    pub delivered: bool,
    /// Whether a new agent loop was re-dispatched from the checkpoint.
    pub resumed: bool,
    /// Human-readable delivery note (never a secret).
    pub note: String,
}

/// Durable recovery point for an interrupted sub-agent session.
///
/// `messages` is a byte-bounded tail (#3882), not the full history:
/// checkpoints fire per step and are cloned into snapshots/persistence, so an
/// unbounded clone multiplies large tool outputs under Fleet fanout.
/// `message_count` records the true total and `omitted_messages` how many of
/// the oldest were dropped from this snapshot; spilled tool outputs remain on
/// disk under the spillover directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubAgentCheckpoint {
    pub checkpoint_id: String,
    pub agent_id: String,
    pub continuation_handle: String,
    pub reason: String,
    pub continuable: bool,
    pub steps_taken: u32,
    pub message_count: usize,
    pub created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    /// Oldest messages omitted from `messages` to honor the checkpoint byte
    /// budget. `0` for records written before v0.8.67 (serde default).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_messages: usize,
    /// This checkpoint was written because the parent's turn ended while the
    /// child was still working — not because the child asked anyone anything
    /// (#5906).
    ///
    /// Both cases land on `AgentWorkerStatus::WaitingForUser` (the projection
    /// keys off `continuable`), and coordination has to tell them apart: a
    /// child that asked a question keeps its write claim, because a user can
    /// still answer it and the child will write again. A parked child is
    /// waiting for nothing — it is resumed as a *new* agent through
    /// `followup`, which registers a claim of its own — so its claim must
    /// stop gating peers the moment it parks. Carried on the checkpoint rather
    /// than sniffed out of the reason string, which would be a guess.
    #[serde(default, skip_serializing_if = "is_false")]
    pub parked_at_turn_end: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSubAgent {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_name: Option<String>,
    #[serde(default)]
    fork_context: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<PathBuf>,
    agent_type: FleetRole,
    prompt: String,
    assignment: SubAgentAssignment,
    #[serde(default)]
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nickname: Option<String>,
    status: SubAgentStatus,
    result: Option<String>,
    steps_taken: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checkpoint: Option<SubAgentCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    needs_input: Option<SubAgentNeedsInput>,
    duration_ms: u64,
    allowed_tools: Vec<String>,
    updated_at_ms: u64,
    /// Stable id of the manager / process boot that spawned this agent
    /// (#405). Lets a fresh manager filter out agents that were
    /// persisted by a prior session. Optional with `#[serde(default)]`
    /// for backward compatibility — older records lack the field and
    /// load with an empty string, which the manager treats as
    /// "from_prior_session" because it can't match any current id.
    #[serde(default)]
    session_boot_id: String,
    /// Root conversation that launched this child. Records written before
    /// this field existed deserialize to empty and are never eligible for
    /// completion synthesis (fail closed).
    #[serde(default)]
    owner_session_id: String,
}

/// Captured at admission, before execution cwd/worktree changes. A legacy
/// original marker is explicit recovery provenance, never a guessed child path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorkerOriginKind {
    LegacyOriginal,
    Direct,
    Fleet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkerOrigin {
    kind: WorkerOriginKind,
    owner_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_workspace: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<CoordinationClaimScope>,
}

impl WorkerOrigin {
    fn legacy(owner_session_id: String) -> Self {
        Self {
            kind: WorkerOriginKind::LegacyOriginal,
            owner_session_id,
            selected_workspace: None,
            scope: None,
        }
    }

    fn validate_shape(&self) -> Result<(), String> {
        if self.kind == WorkerOriginKind::LegacyOriginal
            && (self.selected_workspace.is_some() || self.scope.is_some())
        {
            return Err("legacy original worker cannot carry a new selected-root receipt".into());
        }
        match (&self.selected_workspace, &self.scope) {
            (None, None) => Ok(()),
            (Some(workspace), Some(scope))
                if workspace.is_absolute() && workspace.as_os_str().len() <= 32768 =>
            {
                scope.validate_shape()
            }
            _ => Err("worker origin has incomplete selected-root identity".into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSubAgentState {
    schema_version: u32,
    /// Monotonic in-process snapshot id. Concurrent background writers use it
    /// to prevent an older clone from publishing after a newer one.
    #[serde(default)]
    snapshot_sequence: u64,
    agents: Vec<PersistedSubAgent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    workers: Vec<AgentWorkerRecord>,
    #[serde(default)]
    coordination: CoordinationLedger,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    resume_targets: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worker_origins: Option<HashMap<String, WorkerOrigin>>,
}

impl Default for PersistedSubAgentState {
    fn default() -> Self {
        Self {
            schema_version: SUBAGENT_STATE_SCHEMA_VERSION,
            snapshot_sequence: 0,
            agents: Vec::new(),
            workers: Vec::new(),
            coordination: CoordinationLedger::default(),
            resume_targets: HashMap::new(),
            worker_origins: None,
        }
    }
}

impl PersistedSubAgentState {
    fn validate_worker_origins(&self) -> Result<(), String> {
        match (self.schema_version, self.worker_origins.as_ref()) {
            (1, None) => Ok(()),
            (2, Some(origins))
                if origins.len() == self.workers.len()
                    && origins.len() <= MAX_AGENT_WORKER_RECORDS =>
            {
                let mut seen = HashSet::new();
                for worker in &self.workers {
                    if !seen.insert(&worker.spec.worker_id) {
                        return Err("duplicate worker in origin journal".into());
                    }
                    let origin = origins.get(&worker.spec.worker_id).ok_or_else(|| {
                        "worker origin is absent from schema2 journal".to_string()
                    })?;
                    origin.validate_shape()?;
                    if origin.owner_session_id != worker.owner_session_id {
                        return Err("worker origin owner session does not match its record".into());
                    }
                }
                Ok(())
            }
            _ => Err(format!(
                "unsupported or incomplete sub-agent origin schema {}",
                self.schema_version
            )),
        }
    }
}

/// Default cap on sub-agent recursion depth. Override via
/// `[subagents] max_depth = N` in config.
///
/// Sourced from [`codewhale_config::DEFAULT_SPAWN_DEPTH`] so standalone
/// sub-agents and fleet workers share ONE recursion axis (no "two moving
/// targets"). Configured/requested depths clamp to
/// [`codewhale_config::MAX_SPAWN_DEPTH_CEILING`].
pub const DEFAULT_MAX_SPAWN_DEPTH: u32 = codewhale_config::DEFAULT_SPAWN_DEPTH;

/// Resolve a child runtime's `max_spawn_depth` from its (already-incremented)
/// `spawn_depth` and the model-supplied per-call `max_depth`, clamped to the
/// absolute [`codewhale_config::MAX_SPAWN_DEPTH_CEILING`].
///
/// Without the absolute clamp, `max_spawn_depth = spawn_depth + max_depth`
/// makes the recursion gate (`spawn_depth + 1 > max_spawn_depth`) reduce to
/// `1 > max_depth` at every level — always false when the model re-supplies
/// `max_depth >= 1` per spawn — so ring depth would grow to the global
/// admission cap instead of the intended 8-ring ceiling.
fn clamp_child_max_spawn_depth(child_spawn_depth: u32, requested_max_depth: u32) -> u32 {
    child_spawn_depth
        .saturating_add(requested_max_depth)
        .min(codewhale_config::MAX_SPAWN_DEPTH_CEILING)
}

/// Terminal-state notification emitted to the immediate parent's completion
/// inbox when one of its children finishes (issue #756). For root-spawned
/// agents and nested agents that inbox belongs to their configured Engine.
/// Carries the already-rendered
/// `<codewhale:subagent.done>` sentinel that the model expects in the
/// transcript per the constitution (`prompts/text.rs`, `BASE_PROMPT`).
#[derive(Debug, Clone)]
pub struct SubAgentCompletion {
    /// Root session that owned the child when it was launched. Completion
    /// channels outlive individual conversations, so consumers must compare
    /// this immutable owner with the active session before deduplicating or
    /// injecting the payload.
    pub owner_session_id: String,
    /// The completing child's agent id. Held for routing/logging — the
    /// engine's turn loop does not currently key on it (it just injects
    /// the payload), but downstream tooling and tests need the field.
    pub agent_id: String,
    /// Human summary on line 1, sentinel on line 2. Same payload shape as
    /// `Event::AgentComplete::result`.
    pub payload: String,
}

impl SubAgentCompletion {
    /// Terminal failures are marked inside the model-visible sentinel so the
    /// same fact survives channel fan-in, transcript persistence, and replay.
    #[must_use]
    pub fn is_high_priority_failure(&self) -> bool {
        // Only the runtime's final receipt classifies the outcome. A child
        // can quote failure events in an otherwise successful report.
        let Some(receipt) = self
            .payload
            .trim_end()
            .lines()
            .next_back()
            .and_then(|line| line.strip_prefix("<codewhale:subagent.done>"))
            .and_then(|line| line.strip_suffix("</codewhale:subagent.done>"))
        else {
            return false;
        };
        serde_json::from_str::<Value>(receipt).is_ok_and(|receipt| {
            matches!(
                receipt.get("event").and_then(Value::as_str),
                Some("subagent.failed" | "workflow.failed")
            )
        })
    }
}
#[derive(Default)]
pub(crate) struct TerminalEventDelivery {
    pending: std::sync::atomic::AtomicUsize,
    failed: std::sync::atomic::AtomicBool,
    settled: tokio::sync::Notify,
}

impl TerminalEventDelivery {
    pub(crate) fn mark_failed(&self) {
        self.failed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) async fn wait(&self) -> Result<()> {
        loop {
            let settled = self.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            if self.pending.load(std::sync::atomic::Ordering::Acquire) == 0 {
                break;
            }
            settled.await;
        }
        anyhow::ensure!(
            !self.failed.load(std::sync::atomic::Ordering::Acquire),
            "a background worker lifecycle event could not reach its receiver or durable journal"
        );
        Ok(())
    }
}

struct PendingTerminalEvent {
    owner: Arc<TerminalEventDelivery>,
    delivered: bool,
}

impl Drop for PendingTerminalEvent {
    fn drop(&mut self) {
        if !self.delivered {
            self.owner
                .failed
                .store(true, std::sync::atomic::Ordering::Release);
        }
        self.owner
            .pending
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        self.owner.settled.notify_waiters();
    }
}

/// Live-only sinks needed to publish one terminal child outcome.
///
/// This deliberately lives on [`SubAgent`] rather than the persisted worker
/// record: channels are process-local capabilities and must never cross a
/// restart boundary. Keeping the immediate-parent sender here lets explicit
/// Stop and stale cleanup use the same claim -> deliver -> commit path as a
/// natural task exit instead of aborting the only future that knew how to wake
/// the parent (#4408).
#[derive(Clone)]
struct SubAgentTerminalDeliveryContext {
    spawn_depth: u32,
    parent_completion_tx: Option<mpsc::Sender<SubAgentCompletion>>,
    mailbox: Option<Mailbox>,
    event_tx: Option<mpsc::Sender<Event>>,
    event_delivery: Option<Arc<TerminalEventDelivery>>,
    /// Shared session namespace (root session id), cloned down the spawn
    /// tree; over-budget final reports are spilled under it so the truncation
    /// footer can name a retrievable artifact.
    session_id: String,
}

impl SubAgentTerminalDeliveryContext {
    fn from_runtime(runtime: &SubAgentRuntime) -> Self {
        Self {
            spawn_depth: runtime.spawn_depth,
            parent_completion_tx: runtime.parent_completion_tx.clone(),
            mailbox: runtime.mailbox.clone(),
            event_tx: runtime.event_tx.clone(),
            event_delivery: runtime
                .execution_limits
                .as_ref()
                .map(|limits| limits.terminal_events.clone()),
            session_id: runtime.context.state_namespace.clone(),
        }
    }

    /// Publish to every live sink without blocking or awaiting while the
    /// manager owns the terminal claim. The public agent/worker states remain
    /// Running until all three sends have been attempted.
    fn deliver(&self, manager: &SubAgentManager, result: &SubAgentResult) {
        let report_ref = spill_subagent_final_report(&self.session_id, result);
        let completion = manager.completion_from_result_with_ref_for_session(
            &self.session_id,
            result,
            report_ref.as_deref(),
        );

        if self.spawn_depth > 0
            && let Some(tx) = self.parent_completion_tx.as_ref()
        {
            // A full inbox waits for capacity instead of dropping the wake
            // (#6560 D02-06): a nested parent has no terminal-results
            // synthesis to recover it, so a child finishing while the inbox
            // is full would otherwise never reach the parent's model.
            send_terminal_event(tx, completion.clone());
        }

        if let Some(mailbox) = self.mailbox.as_ref() {
            let _ = mailbox.send(terminal_mailbox_message(result, report_ref.as_deref()));
        }

        if let Some(event_tx) = self.event_tx.as_ref() {
            send_terminal_event_with_delivery(
                event_tx,
                Event::AgentComplete {
                    owner_session_id: self.session_id.clone(),
                    id: result.agent_id.clone(),
                    result: completion.payload,
                    outcome: Some(result.status.clone()),
                    parent_run_id: result.parent_run_id.clone(),
                    spawn_depth: Some(result.spawn_depth),
                    continuable: Some(subagent_checkpoint_is_continuable(result)),
                    usage: result.usage.clone(),
                    display_name: Some(subagent_result_display_name(result)),
                },
                self.event_delivery.as_ref(),
            );
        }
    }
}

/// Deliver a terminal sub-agent event (or child completion) the receiver
/// must not lose (#6184 H2, #6560 D02-06).
///
/// `try_send` dropped `AgentComplete` whenever the event channel was full,
/// leaving a ghost Running row that silenced every stall watchdog. The
/// terminal claim forbids awaiting here, so a full channel hands the event to
/// a task that waits for capacity; only a closed channel (no receiver left)
/// drops it. Progress events stay lossy by design. `AgentSpawned` also stays
/// lossy: delivered late it could land after the completion and resurrect a
/// Running row, while a lost one is recovered by the completion itself.
pub(crate) fn send_terminal_event<T: Send + 'static>(event_tx: &mpsc::Sender<T>, event: T) {
    send_terminal_event_with_delivery(event_tx, event, None);
}

fn send_terminal_event_with_delivery<T: Send + 'static>(
    event_tx: &mpsc::Sender<T>,
    event: T,
    delivery: Option<&Arc<TerminalEventDelivery>>,
) {
    let event = match event_tx.try_send(event) {
        Ok(()) => return,
        Err(mpsc::error::TrySendError::Closed(_)) => {
            if let Some(delivery) = delivery {
                delivery
                    .failed
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            return;
        }
        Err(mpsc::error::TrySendError::Full(event)) => event,
    };
    let pending = delivery.map(|owner| {
        owner
            .pending
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        PendingTerminalEvent {
            owner: owner.clone(),
            delivered: false,
        }
    });
    let tx = event_tx.clone();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let mut pending = pending;
                let delivered = tx.send(event).await.is_ok();
                if let Some(pending) = pending.as_mut() {
                    pending.delivered = delivered;
                }
            });
        }
        Err(_) => {
            std::thread::spawn(move || {
                let mut pending = pending;
                let delivered = tx.blocking_send(event).is_ok();
                if let Some(pending) = pending.as_mut() {
                    pending.delivered = delivered;
                }
            });
        }
    }
}

fn terminal_mailbox_message(result: &SubAgentResult, report_ref: Option<&str>) -> MailboxMessage {
    match &result.status {
        SubAgentStatus::Completed => {
            let (summary, _) =
                stamp_subagent_summary_with_ref(&summarize_subagent_result(result), report_ref);
            MailboxMessage::Completed {
                agent_id: result.agent_id.clone(),
                summary,
            }
        }
        SubAgentStatus::Interrupted(reason) => MailboxMessage::Interrupted {
            agent_id: result.agent_id.clone(),
            reason: reason.clone(),
        },
        SubAgentStatus::Failed(error) => MailboxMessage::Failed {
            agent_id: result.agent_id.clone(),
            error: error.clone(),
        },
        SubAgentStatus::Cancelled => MailboxMessage::Cancelled {
            agent_id: result.agent_id.clone(),
        },
        SubAgentStatus::BudgetExhausted => MailboxMessage::Failed {
            agent_id: result.agent_id.clone(),
            error: summarize_subagent_result(result),
        },
        SubAgentStatus::Running => MailboxMessage::Progress {
            agent_id: result.agent_id.clone(),
            status: "running".to_string(),
        },
    }
}

/// Parent transcript snapshot for opted-in context forks. Cold forks keep
/// their resolved top-level prompt; guarded same-route inheritance may reuse
/// the parent header. Resolved role, task and skill instructions remain in
/// the later system-role fork context. Client routing and child authority are
/// enforced independently of prompt text.
#[derive(Clone, Debug)]
pub struct SubAgentForkContext {
    pub messages: Vec<Message>,
    pub live_header: crate::prompt_zones::SharedLiveHeader<crate::route_receipt::TurnRouteReceipt>,
    /// Stable, To-do-free parent state captured once at turn start. History
    /// semantics stay exactly as they were: this text is not re-derived per
    /// spawn.
    pub structured_state_block: Option<String>,
    /// Where to read the spawning agent's To-do list *at spawn time*.
    ///
    /// The block above is captured before the turn's first tool call, so a
    /// `work_update` earlier in the same turn would otherwise hand the child a
    /// stale list (#3983). Only the To-do portion is refreshed.
    pub work_source: Option<crate::todo_snapshot::TodoSource>,
}

impl SubAgentForkContext {
    /// Resolve the fork-state block at the fork seam: the stable captured
    /// state, then the current To-do snapshot. Shown to the child once, in the
    /// context block stored in its own history — never re-sent.
    pub(crate) async fn with_resolved_state_block(&self) -> Self {
        let stable = self
            .structured_state_block
            .as_deref()
            .map(str::trim)
            .filter(|state| !state.is_empty())
            .map(str::to_string);
        let todo_body = match self.work_source.as_ref() {
            Some(source) => source.body().await,
            None => None,
        };
        let todo_section = todo_body
            .as_deref()
            .map(crate::todo_snapshot::fork_state_todo_section);

        let structured_state_block = match (stable, todo_section) {
            (Some(stable), Some(work)) => Some(format!("{stable}\n{work}")),
            (Some(stable), None) => Some(stable),
            (None, work) => work,
        };

        Self {
            messages: self.messages.clone(),
            live_header: Arc::clone(&self.live_header),
            structured_state_block,
            work_source: self.work_source.clone(),
        }
    }
}

/// Runtime configuration for spawning sub-agents.
///
/// Carries everything a child needs to (a) build its own tool registry —
/// including the manager so grandchildren can spawn — and (b) cooperate with
/// lifecycle cancellation and depth caps. `child_runtime()` links cancellation
/// tokens and turn ownership, while `background_runtime()` explicitly detaches
/// long-running `agent` sessions from the caller's turn token.
#[derive(Debug)]
pub(crate) struct ForegroundChildRegistry {
    state: std::sync::Mutex<ForegroundChildState>,
    settled: tokio::sync::watch::Sender<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForegroundSettlement {
    Cancel,
    Park,
}

#[derive(Debug)]
struct ForegroundChildEntry {
    token: CancellationToken,
    /// Model-facing child id (`agent_*`) kept so a bounded join can name the
    /// children it gives up on (#6184).
    label: String,
    parking_requested: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Debug, Default)]
struct ForegroundChildState {
    settlement: Option<ForegroundSettlement>,
    next_id: u64,
    children: HashMap<u64, ForegroundChildEntry>,
}

pub(crate) struct ForegroundChildRegistration {
    registry: std::sync::Weak<ForegroundChildRegistry>,
    id: u64,
    parking_requested: Arc<std::sync::atomic::AtomicBool>,
}

impl ForegroundChildRegistry {
    #[must_use]
    pub(crate) fn new() -> Self {
        let (settled, _) = tokio::sync::watch::channel(());
        Self {
            state: std::sync::Mutex::new(ForegroundChildState::default()),
            settled,
        }
    }

    pub(crate) fn register(
        self: &Arc<Self>,
        token: CancellationToken,
        label: &str,
    ) -> Result<ForegroundChildRegistration, ForegroundSettlement> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(settlement) = state.settlement {
            token.cancel();
            return Err(settlement);
        }
        let id = state.next_id;
        state.next_id = state.next_id.saturating_add(1);
        let parking_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        state.children.insert(
            id,
            ForegroundChildEntry {
                token,
                label: label.to_string(),
                parking_requested: Arc::clone(&parking_requested),
            },
        );
        Ok(ForegroundChildRegistration {
            registry: Arc::downgrade(self),
            id,
            parking_requested,
        })
    }

    #[must_use]
    pub(crate) fn active_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .children
            .len()
    }

    fn release(&self, id: u64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.children.remove(&id).is_some() {
            self.settled.send_replace(());
        }
    }

    /// Labels of children still holding their registration — the unsettled
    /// set a bounded join leaves behind. Diagnostic only: the caller names
    /// them in the terminal turn event rather than waiting forever (#6184).
    pub(crate) fn unsettled_labels(&self) -> Vec<String> {
        let mut labels = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .children
            .values()
            .map(|entry| entry.label.clone())
            .collect::<Vec<_>>();
        labels.sort();
        labels
    }

    /// Cancel every currently-owned child and wait until each task has
    /// released its registration. Multiple terminal paths share this barrier:
    /// only the first call issues cancellation, while all callers await the
    /// same settled set. A child registered after cancellation observes the
    /// latched state and is cancelled before it can reach a provider request.
    ///
    /// The wait itself is unbounded and does not observe any cancellation
    /// token: a child parked on an await that ignores its token would park
    /// the caller forever. Production callers must bound it —
    /// `TurnMailboxBarrier::cancel_and_flush` races this join against the
    /// turn's settle grace and a fresh cancellation (#6184).
    pub(crate) async fn cancel_and_wait(&self) {
        self.settle_and_wait(ForegroundSettlement::Cancel).await;
    }

    /// Park turn-owned children as resumable before cancelling their live
    /// tasks. The flag is stored before token cancellation, so every child
    /// projects the race as Interrupted with a checkpoint rather than a
    /// terminal Cancelled receipt.
    #[cfg(test)] // retained to verify recovery of legacy parked checkpoints
    pub(crate) async fn park_and_wait(&self) {
        self.settle_and_wait(ForegroundSettlement::Park).await;
    }

    async fn settle_and_wait(&self, requested: ForegroundSettlement) {
        use std::sync::atomic::Ordering;

        let mut settled = self.settled.subscribe();
        let tokens = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.settlement.is_some() {
                Vec::new()
            } else {
                state.settlement = Some(requested);
                state
                    .children
                    .values()
                    .map(|entry| {
                        if requested == ForegroundSettlement::Park {
                            entry.parking_requested.store(true, Ordering::Release);
                        }
                        entry.token.clone()
                    })
                    .collect::<Vec<_>>()
            }
        };
        for token in tokens {
            token.cancel();
        }

        loop {
            let is_settled = {
                let state = self
                    .state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.children.is_empty()
            };
            if is_settled {
                return;
            }
            // `watch` retains a version change that races this check, unlike a
            // plain notification created but not yet polled by this task.
            let _ = settled.changed().await;
        }
    }
}

impl ForegroundChildRegistration {
    pub(crate) fn parking_signal(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.parking_requested)
    }
}

impl Drop for ForegroundChildRegistration {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.release(self.id);
        }
    }
}

#[derive(Clone)]
pub(crate) struct SubAgentExecutionLimits {
    pub max_admitted: usize,
    pub heartbeat_timeout: Duration,
    pub default_max_steps: Option<u32>,
    pub default_wall_time: Option<Duration>,
    pub launch_gate: Arc<governor::DynamicGate>,
    pub terminal_events: Arc<TerminalEventDelivery>,
    pub admission: CancellationToken,
}

impl SubAgentExecutionLimits {
    pub(crate) fn new(
        max_agents: usize,
        max_admitted: usize,
        heartbeat_timeout: Duration,
        launch_concurrency: usize,
        default_max_steps: Option<u32>,
        default_wall_time: Option<Duration>,
    ) -> Self {
        let max_agents = max_agents.clamp(1, MAX_SUBAGENTS);
        Self {
            max_admitted: max_admitted.clamp(max_agents, crate::config::MAX_SUBAGENT_ADMISSION),
            heartbeat_timeout: if heartbeat_timeout.is_zero() {
                Duration::from_secs(crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS)
            } else {
                heartbeat_timeout
            },
            default_max_steps,
            default_wall_time,
            launch_gate: Arc::new(governor::DynamicGate::new(
                launch_concurrency.clamp(1, max_agents),
            )),
            terminal_events: Arc::new(TerminalEventDelivery::default()),
            admission: CancellationToken::new(),
        }
    }

    pub(crate) fn update(
        &mut self,
        max_agents: usize,
        max_admitted: usize,
        heartbeat_timeout: Duration,
        launch_concurrency: usize,
    ) {
        let max_agents = max_agents.clamp(1, MAX_SUBAGENTS);
        self.max_admitted = max_admitted.clamp(max_agents, crate::config::MAX_SUBAGENT_ADMISSION);
        self.heartbeat_timeout = if heartbeat_timeout.is_zero() {
            Duration::from_secs(crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS)
        } else {
            heartbeat_timeout
        };
        self.launch_gate
            .set_capacity(launch_concurrency.clamp(1, max_agents));
    }
}

#[derive(Clone)]
pub struct SubAgentRuntime {
    pub client: CodewhaleClient,
    /// Session `Config` snapshot, used for role-model defaults,
    /// provider-identity receipts, model routing, and inherited typed deny rules. The engine
    /// threads it in via [`SubAgentRuntime::with_api_config`];
    /// `child_runtime`/`background_runtime` clone the `Arc` so every
    /// descendant resolves the same session route.
    ///
    /// `None` for legacy/test runtimes that never threaded a config.
    pub api_config: Option<std::sync::Arc<crate::config::Config>>,
    pub model: String,
    /// Active UI/model locale used for generated human-facing worker names.
    /// Internal ids and session handles remain language-neutral.
    pub locale_tag: String,
    pub auto_model: bool,
    pub reasoning_effort: Option<String>,
    pub reasoning_effort_auto: bool,
    pub role_models: HashMap<String, SubagentModelOverride>,
    /// Operator-approved `provider/model` routes this child may move to when
    /// its role pin's first request is refused before any work. Set only by
    /// a role pin that declares them (`[subagents.roles.<role>]
    /// replacements`); every other route source, including exact Fleet
    /// bindings and task-level models, keeps it empty and stays exact.
    pub route_replacements: Vec<SubagentModelOverride>,
    /// Where this child's route came from, in operator words, and how to
    /// change it; for a saved-profile pin, also the parent route the child
    /// may take when that pin refuses authorization before any work. Set by
    /// the spawn that bound the route; descendants earn their own.
    pub(crate) route_origin: Option<Arc<SpawnRouteOrigin>>,
    pub context: ToolContext,
    pub allow_shell: bool,
    /// When true, Suggest-level file writes auto-accept for write-capable roles
    /// without full parent auto-approve. Shell/network/MCP still gated.
    /// Set for Workflow-spawned children and parent-approved root Operate
    /// workers.
    pub accept_edits: bool,
    /// Allow the built-in, non-custom verification tools after a root Operate
    /// worker start has crossed the parent's approval boundary. This is not a
    /// general shell grant: arbitrary commands and custom verifier programs
    /// remain blocked unless the parent session is auto-approved.
    pub accept_verification: bool,
    /// Native Agent-mode tool surface inherited from the parent turn. Carries
    /// feature/config-dependent families such as web search, patch, memory,
    /// vision, notify, and FIM so child catalogs stay in parity with the parent.
    pub agent_tool_surface_options: AgentToolSurfaceOptions,
    /// Capability contract inherited by descendants. `agent` derives a
    /// child profile from this before registering the worker record so parent,
    /// sub-agent, and fleet projections share one worker contract.
    pub worker_profile: WorkerRuntimeProfile,
    pub event_tx: Option<mpsc::Sender<Event>>,
    /// Manager handle so children can recurse via `agent`. All agents
    /// at every depth share the same manager.
    pub manager: SharedSubAgentManager,
    pub(crate) execution_limits: Option<SubAgentExecutionLimits>,
    /// Depth in the spawn tree. 0 = top-level user turn; 1 = direct child;
    /// etc. Children clone the parent runtime and increment this on spawn.
    pub spawn_depth: u32,
    /// Agent id that should be recorded as parent for any child spawned
    /// through this runtime's model-visible `agent` tool. `None` for the
    /// root engine; set to the running sub-agent id for nested spawns so UI
    /// surfaces can render the tree.
    pub parent_agent_id: Option<String>,
    /// Hard cap on recursion depth. A child whose `spawn_depth + 1` would
    /// exceed this is rejected at the spawn entry. Use `>` (strictly
    /// greater than) so equality is allowed — matches codex's pattern.
    pub max_spawn_depth: u32,
    /// Cooperative cancellation token. Direct `child_runtime()` callers derive
    /// a child token from the parent; explicitly detached model-visible
    /// `agent` starts use `background_runtime()` to replace it.
    pub cancel_token: CancellationToken,
    /// Turn-scoped ownership barrier for the complete foreground subtree.
    /// Every non-detached descendant registers with the same root barrier;
    /// explicitly detached runtimes clear it for their whole subtree.
    foreground_children: Option<Arc<ForegroundChildRegistry>>,
    /// Structured progress / lifecycle stream. Cloned across children so the
    /// whole spawn tree publishes into one ordered, fan-out-able mailbox.
    /// `None` only when no consumer is wired (legacy entry points / tests).
    pub mailbox: Option<Mailbox>,
    /// Lease on the durable runtime-turn accounting sink. Detached child
    /// runtimes clone this guard, keeping the sink alive after the parent UI
    /// mailbox closes until the final child response has been persisted.
    pub(crate) runtime_usage_lease: Option<crate::cost_status::RuntimeUsageLease>,
    /// Dispatch-time accounting ownership for every provider response this
    /// runtime or its descendants settle. Captured in [`Self::new`], cloned
    /// by [`Self::child_runtime`], never refreshed from the live cost scope.
    pub(crate) accounting_origin: SubAgentAccountingOrigin,
    /// Wakeup channel for this runtime's immediate parent (issue #756). For
    /// the engine's direct children this points at the engine turn loop. While
    /// a sub-agent is running, its tool registry swaps this for a local inbox
    /// so nested children report to their orchestrating sub-agent instead of
    /// flooding the root parent. `None` when no consumer is wired (tests /
    /// legacy paths).
    pub parent_completion_tx: Option<mpsc::Sender<SubAgentCompletion>>,
    /// Snapshot of the request prefix visible to an opt-in forked child.
    pub fork_context: Option<SubAgentForkContext>,
    /// The parent's MCP pool if available.
    pub mcp_pool: Option<std::sync::Arc<tokio::sync::Mutex<crate::mcp::McpPool>>>,
    /// Per-step DeepSeek API timeout for the child's `create_message` call.
    /// Resolved from `[subagents] api_timeout_secs` (clamped to 1..=3600) at
    /// engine construction so a slow but legitimate model turn does not
    /// false-timeout the child mid-thinking. `child_runtime()` and
    /// `background_runtime()` preserve the parent's value (#1806, #1808).
    pub step_api_timeout: Duration,
    /// Initial backoff between timed-out `create_message` retries. Defaults
    /// to [`SUBAGENT_API_TIMEOUT_INITIAL_BACKOFF`]; tests shrink it so the
    /// timeout-retry path runs in milliseconds. `child_runtime()` preserves
    /// the parent's value.
    pub(crate) api_timeout_retry_base_backoff: Duration,
    /// Wall-clock budget for a single tool execution within a sub-agent step.
    /// Defaults to `DEFAULT_TOOL_TIMEOUT`; the engine may override it so a long
    /// but legitimate tool run is not killed mid-flight. `child_runtime()`
    /// preserves the parent's value.
    pub tool_timeout: Duration,
    /// Per-result output cap in tokens (default 10k). `None` uses the
    /// compile-time default; `Some(n)` clamps every tool result to at
    /// most n tokens (conservatively estimated as n × 3 bytes) before it
    /// enters the child's message history.
    pub max_output_tokens: Option<std::num::NonZeroU32>,
    /// Default directory for Xiaomi MiMo speech/TTS tool outputs inherited by
    /// child registries. Keeps parent and sub-agent `speech` / `tts` tools on
    /// the same `[speech].output_dir` / env override.
    pub speech_output_dir: Option<PathBuf>,
    /// This runtime's **own** todo list. It is never shared with a parent or a
    /// sibling: `child_runtime()` allocates a fresh `SharedTodoList` for every
    /// spawned agent (#4810). Sharing the parent's `Arc` here let a child's
    /// `work_update` / `todo_write` replace the parent's Work checklist
    /// wholesale — a data-integrity bug, because the tools *replace* the list
    /// rather than merge into it, and because `WorkRuntime::matches_todos`
    /// routes any write on the parent `Arc` straight into the parent's work
    /// graph.
    ///
    /// Parent progress reaches a child as **immutable context only**, via the
    /// `fork_context` structured-state block (see `StructuredState::capture`),
    /// never as writable state. Child progress reaches the parent through the
    /// completion payload and delegate card, not by mutating the parent list.
    pub todos: SharedTodoList,
    /// Session mode of the orchestrating parent at spawn time (Wave 7 M4/M5).
    pub parent_mode: AppMode,
    /// The session's deterministic Auto-Review policy (configured allow/block
    /// rules plus the built-in safety floor), shared with every descendant.
    pub auto_review_policy: std::sync::Arc<crate::core::authority::auto_review::AutoReviewPolicy>,
    /// Whether the host can answer an approval prompt for a child (an
    /// interactive TUI). Headless hosts keep the fail-closed denial.
    pub parent_can_prompt: bool,
    /// Durable approval evidence inherited from the parent session. Legacy
    /// runtimes that do not install a store cannot open child approval prompts.
    approval_receipt_store: Option<Result<crate::approval_log::ApprovalReceiptStore, String>>,
    /// Shared rate-limit governor for the fleet that spawned this runtime.
    /// Stamped by the spawning manager in
    /// `spawn_background_with_assignment_options` (the single chokepoint all
    /// spawn variants funnel through), so every descendant LLM attempt
    /// reports 429s/successes to the fleet's adaptive scheduler; cloned into
    /// child runtimes. `None` for runtimes built outside a manager (tests,
    /// tool-only runtimes).
    pub(crate) governor: Option<Arc<governor::RateLimitGovernor>>,
    /// The parent session's automatic-compaction policy. Every descendant
    /// compacts its own history at the request boundary exactly like the
    /// parent turn loop through the captured canonical Engine route;
    /// `enabled == false` (the operator's opt-out) holds for the whole tree.
    pub(crate) compaction: crate::compaction::CompactionConfig,
}

impl SubAgentRuntime {
    /// Create a top-level runtime configuration for sub-agent execution.
    /// Use this from the engine when constructing the runtime that the
    /// parent's tool registry passes through. Children should derive their
    /// runtime via `Self::child_runtime` instead.
    #[must_use]
    pub fn new(
        client: CodewhaleClient,
        model: String,
        context: ToolContext,
        allow_shell: bool,
        event_tx: Option<mpsc::Sender<Event>>,
        manager: SharedSubAgentManager,
    ) -> Self {
        // Accounting ownership is fixed here, at dispatch, before any child
        // provider request exists; see `SubAgentAccountingOrigin`.
        let accounting_origin = SubAgentAccountingOrigin::capture(&context);
        Self {
            client,
            api_config: None,
            model,
            locale_tag: "en".to_string(),
            auto_model: false,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            role_models: HashMap::new(),
            route_replacements: Vec::new(),
            route_origin: None,
            context,
            allow_shell,
            accept_edits: false,
            accept_verification: false,
            agent_tool_surface_options: AgentToolSurfaceOptions::new(
                ShellPolicy::from_legacy_allow_shell(allow_shell),
            ),
            worker_profile: WorkerRuntimeProfile::for_role(FleetRole::Worker),
            event_tx,
            manager,
            execution_limits: None,
            spawn_depth: 0,
            parent_agent_id: None,
            max_spawn_depth: DEFAULT_MAX_SPAWN_DEPTH,
            cancel_token: CancellationToken::new(),
            foreground_children: None,
            mailbox: None,
            runtime_usage_lease: None,
            accounting_origin,
            parent_completion_tx: None,
            fork_context: None,
            mcp_pool: None,
            step_api_timeout: DEFAULT_STEP_API_TIMEOUT,
            api_timeout_retry_base_backoff: SUBAGENT_API_TIMEOUT_INITIAL_BACKOFF,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            max_output_tokens: None,
            speech_output_dir: None,
            todos: crate::tools::todo::new_shared_todo_list(),
            parent_mode: AppMode::Agent,
            auto_review_policy: std::sync::Arc::new(
                crate::core::authority::auto_review::AutoReviewPolicy::default(),
            ),
            parent_can_prompt: false,
            approval_receipt_store: None,
            compaction: crate::compaction::CompactionConfig::default(),
            // Stamped by the spawning manager in
            // `spawn_background_with_assignment_options`, so every descendant
            // LLM attempt reports 429s/successes to the fleet's rate-limit
            // governor. `None` for runtimes built outside a manager (tests,
            // tool-only runtimes).
            governor: None,
        }
    }

    /// Preserve the parent session mode for spawn-policy decisions.
    #[must_use]
    pub fn with_parent_mode(mut self, mode: AppMode) -> Self {
        self.parent_mode = mode;
        self
    }

    /// Install the parent session's append-only approval receipt store.
    #[must_use]
    pub(crate) fn with_approval_receipt_store(
        mut self,
        store: Result<crate::approval_log::ApprovalReceiptStore, String>,
    ) -> Self {
        self.approval_receipt_store = Some(store);
        self
    }

    /// Install the session's Auto-Review policy so children are gated exactly
    /// like the parent, and say whether the host can answer a prompt raised on
    /// a child's behalf. The posture itself travels on `context`, live.
    #[must_use]
    pub fn with_permission_posture(
        mut self,
        auto_review_policy: std::sync::Arc<crate::core::authority::auto_review::AutoReviewPolicy>,
        parent_can_prompt: bool,
    ) -> Self {
        self.auto_review_policy = auto_review_policy;
        self.parent_can_prompt = parent_can_prompt;
        self
    }

    /// Match generated worker display names to the active session language.
    #[must_use]
    pub fn with_locale_tag(mut self, locale_tag: impl Into<String>) -> Self {
        self.locale_tag = locale_tag.into();
        self
    }

    /// Bind the todo list this runtime writes to. The engine passes its own
    /// session list here so the *root* runtime and the turn tool registry
    /// agree on one list; spawned agents never inherit it, because
    /// `child_runtime()` allocates a fresh list per agent (#4810).
    #[must_use]
    pub fn with_todos(mut self, todos: SharedTodoList) -> Self {
        self.todos = todos;
        self
    }

    /// Stamp the fleet's rate-limit governor onto a root runtime. The manager
    /// owns the governor (and its launch gate), but only runtimes carrying it
    /// report 429s/successes; without this the whole descendant tree inherits
    /// `None` and the AIMD scheduler never observes provider throttling.
    #[must_use]
    pub(crate) fn with_fleet_governor(
        mut self,
        governor: Arc<governor::RateLimitGovernor>,
    ) -> Self {
        self.governor = Some(governor);
        self
    }

    /// Preserve the parent Agent-mode native tool surface for child registries.
    #[must_use]
    pub fn with_agent_tool_surface_options(mut self, options: AgentToolSurfaceOptions) -> Self {
        self.speech_output_dir = options.speech_output_dir.clone();
        self.agent_tool_surface_options = options;
        self
    }

    /// Attach an MCP pool so the subagent can execute MCP tools.
    #[must_use]
    pub fn with_mcp_pool(
        mut self,
        pool: Option<std::sync::Arc<tokio::sync::Mutex<crate::mcp::McpPool>>>,
    ) -> Self {
        self.mcp_pool = pool;
        self
    }

    /// Override the per-step DeepSeek API timeout (default
    /// `DEFAULT_STEP_API_TIMEOUT`). Called by the engine after reading
    /// `[subagents] api_timeout_secs`. Tests may use this to fail fast
    /// without waiting the default 600 seconds (#1806, #1808).
    #[must_use]
    pub fn with_step_api_timeout(mut self, timeout: Duration) -> Self {
        self.step_api_timeout = timeout;
        self
    }

    /// Shrink the timeout-retry backoff so tests covering the retry path do
    /// not wait out the production 1s..=30s backoff sequence.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_api_timeout_retry_base_backoff(mut self, backoff: Duration) -> Self {
        self.api_timeout_retry_base_backoff = backoff;
        self
    }

    /// Preserve the configured speech output directory for sub-agent tools.
    #[must_use]
    pub fn with_speech_output_dir(mut self, output_dir: Option<PathBuf>) -> Self {
        self.speech_output_dir = output_dir.clone();
        self.agent_tool_surface_options.speech_output_dir = output_dir;
        self
    }

    /// Attach the wakeup channel for this runtime's immediate parent. The
    /// engine uses this for direct children; running sub-agents replace it in
    /// the runtime handed to their nested `agent` tool so child completions are
    /// routed back to the sub-agent that spawned them.
    #[must_use]
    pub fn with_parent_completion_tx(mut self, tx: mpsc::Sender<SubAgentCompletion>) -> Self {
        self.parent_completion_tx = Some(tx);
        self
    }

    /// Attach the current parent request prefix for `fork_context` spawns.
    #[must_use]
    pub fn with_fork_context(mut self, context: SubAgentForkContext) -> Self {
        self.fork_context = Some(context);
        self
    }

    /// Attach a `Mailbox` so this runtime and its derived children publish
    /// structured `MailboxMessage` envelopes alongside the legacy `Event`
    /// stream. Pair with [`Self::with_cancel_token`] when the mailbox close
    /// token should match this runtime's cancellation token.
    #[must_use]
    #[allow(dead_code)] // wired by #128 (in-transcript cards) when it lands.
    pub fn with_mailbox(mut self, mailbox: Mailbox) -> Self {
        self.mailbox = Some(mailbox);
        self
    }

    /// Inherit the parent session's compaction policy, and bind descendants
    /// to the durable accounting owner it names (created by a runtime host).
    /// Interactive TUI turns have no owner and continue using mailbox
    /// delivery only.
    #[must_use]
    pub(crate) fn with_parent_compaction(
        mut self,
        compaction: &crate::compaction::CompactionConfig,
    ) -> Self {
        self.runtime_usage_lease = compaction
            .runtime_cost_owner
            .as_deref()
            .and_then(crate::cost_status::acquire_runtime_usage_lease);
        self.compaction = compaction.clone();
        self
    }

    /// Replace the cancellation token (e.g. when the engine constructs the
    /// runtime alongside a mailbox bound to the same token).
    #[must_use]
    #[allow(dead_code)] // wired by #128 alongside `with_mailbox`.
    pub fn with_cancel_token(mut self, token: CancellationToken) -> Self {
        self.context.cancel_token = Some(token.clone());
        self.cancel_token = token;
        self
    }

    /// Attach the turn-owned foreground-subtree registry. Engine-only wiring
    /// keeps the ownership boundary out of Fleet scheduling and persisted
    /// records.
    #[must_use]
    pub(crate) fn with_foreground_children(
        mut self,
        foreground_children: Arc<ForegroundChildRegistry>,
    ) -> Self {
        self.foreground_children = Some(foreground_children);
        self
    }

    /// Whether this captured runtime belongs to the parent's foreground subtree.
    pub(crate) fn has_foreground_ownership(&self) -> bool {
        self.foreground_children.is_some()
    }

    /// Override the maximum spawn depth (default `DEFAULT_MAX_SPAWN_DEPTH`).
    /// Used by config wiring (`[subagents] max_depth = N`) and tests.
    #[must_use]
    pub fn with_max_spawn_depth(mut self, max: u32) -> Self {
        self.max_spawn_depth = max.min(codewhale_config::MAX_SPAWN_DEPTH_CEILING);
        self.worker_profile.max_spawn_depth = self.max_spawn_depth;
        self
    }

    /// Attach raw role/type model overrides. Values are intentionally
    /// validated at spawn time so bad config fails before a partial spawn.
    #[must_use]
    pub fn with_role_models(mut self, role_models: HashMap<String, SubagentModelOverride>) -> Self {
        self.role_models = role_models;
        self
    }

    /// Attach the session `Config` for spawn-time role-model defaults, receipts,
    /// and model routing (see the [`api_config`](Self::api_config) field
    /// docs). Engine-only wiring; test and legacy runtimes may leave it unset.
    #[must_use]
    pub fn with_api_config(mut self, config: crate::config::Config) -> Self {
        self.api_config = Some(std::sync::Arc::new(config));
        self
    }

    #[must_use]
    pub(crate) fn with_execution_limits(mut self, limits: Option<SubAgentExecutionLimits>) -> Self {
        self.execution_limits = limits;
        self
    }

    /// Preserve whether the parent session is using per-turn model routing.
    #[must_use]
    pub fn with_auto_model(mut self, auto_model: bool) -> Self {
        self.auto_model = auto_model;
        self
    }

    /// Preserve the parent's thinking configuration. Child model strength is
    /// explicit on the `agent` call; this only controls reasoning effort.
    #[must_use]
    pub fn with_reasoning_effort(
        mut self,
        reasoning_effort: Option<String>,
        reasoning_effort_auto: bool,
    ) -> Self {
        self.reasoning_effort = reasoning_effort;
        self.reasoning_effort_auto = reasoning_effort_auto;
        self
    }

    /// Return a child runtime that is deliberately detached from the parent
    /// turn cancellation token and its foreground ownership barrier. Explicit
    /// agent cancellation signals this captured token and joins the same task.
    #[must_use]
    pub fn background_runtime(&self) -> Self {
        let mut runtime = self.child_runtime();
        let token = CancellationToken::new();
        runtime.cancel_token = token.clone();
        runtime.context.cancel_token = Some(token);
        runtime.foreground_children = None;
        runtime
    }

    fn foreground_child_registration(
        &self,
        agent_id: &str,
    ) -> Result<Option<ForegroundChildRegistration>> {
        if self.spawn_depth == 0 {
            return Ok(None);
        }
        let Some(registry) = self.foreground_children.as_ref() else {
            return Ok(None);
        };
        registry
            .register(self.cancel_token.clone(), agent_id)
            .map(Some)
            .map_err(|settlement| {
                anyhow!(
                    "parent turn is already settling foreground work as {settlement:?}; refusing to launch child {agent_id} after the ownership barrier closed"
                )
            })
    }

    /// Build a child runtime cloning this one, incrementing `spawn_depth`,
    /// and deriving a child cancellation token. Used at spawn entry to
    /// construct the runtime the new sub-agent will see.
    ///
    /// Children inherit the parent's approval state. A non-auto parent can
    /// still delegate read-only investigation, but approval-gated child tools
    /// are blocked by the sub-agent registry instead of being silently run
    /// without a prompt.
    #[must_use]
    pub fn child_runtime(&self) -> Self {
        let mut child_context = self.context.clone();
        let cancel_token = self.cancel_token.child_token();
        child_context.cancel_token = Some(cancel_token.clone());
        Self {
            client: self.client.clone(),
            api_config: self.api_config.clone(),
            model: self.model.clone(),
            locale_tag: self.locale_tag.clone(),
            auto_model: self.auto_model,
            reasoning_effort: self.reasoning_effort.clone(),
            reasoning_effort_auto: self.reasoning_effort_auto,
            role_models: self.role_models.clone(),
            // A descendant earns replacement authority only from its own pin.
            route_replacements: Vec::new(),
            route_origin: None,
            context: child_context,
            allow_shell: self.allow_shell,
            accept_edits: self.accept_edits,
            // A parent-approved Operate verification lease belongs to its
            // direct worker only; nested children must cross their own
            // approval boundary instead of silently inheriting it.
            accept_verification: self.accept_verification && self.spawn_depth == 0,
            agent_tool_surface_options: self.agent_tool_surface_options.clone(),
            worker_profile: self.worker_profile.clone(),
            event_tx: self.event_tx.clone(),
            manager: self.manager.clone(),
            execution_limits: self.execution_limits.clone(),
            spawn_depth: self.spawn_depth.saturating_add(1),
            parent_agent_id: self.parent_agent_id.clone(),
            max_spawn_depth: self
                .max_spawn_depth
                .min(self.worker_profile.max_spawn_depth),
            cancel_token,
            foreground_children: self.foreground_children.clone(),
            mailbox: self.mailbox.clone(),
            runtime_usage_lease: self.runtime_usage_lease.clone(),
            accounting_origin: self.accounting_origin.clone(),
            parent_completion_tx: self.parent_completion_tx.clone(),
            fork_context: self.fork_context.as_ref().map(|context| {
                let mut context = context.clone();
                let snapshot = context
                    .live_header
                    .lock()
                    .ok()
                    .and_then(|cell| cell.clone());
                context.live_header = Arc::new(std::sync::Mutex::new(snapshot));
                context
            }),
            mcp_pool: self.mcp_pool.clone(),
            step_api_timeout: self.step_api_timeout,
            api_timeout_retry_base_backoff: self.api_timeout_retry_base_backoff,
            tool_timeout: self.tool_timeout,
            max_output_tokens: self.max_output_tokens,
            speech_output_dir: self.speech_output_dir.clone(),
            // #4810: every spawned agent owns its todo list. Cloning the
            // parent `Arc` here made child `work_update` / `todo_write` replace
            // the parent's Work checklist (and, through
            // `WorkRuntime::matches_todos`, the parent's work graph), so a
            // worker could silently erase the supervisor's plan and its
            // siblings' progress. Parent todo state is still visible to an
            // opt-in forked child as immutable `fork_context` text.
            todos: crate::tools::todo::new_shared_todo_list(),
            // Inherit the fleet's rate-limit governor so every descendant
            // LLM attempt reports 429s/successes to the adaptive scheduler.
            governor: self.governor.clone(),
            parent_mode: self.parent_mode,
            auto_review_policy: Arc::clone(&self.auto_review_policy),
            parent_can_prompt: self.parent_can_prompt,
            approval_receipt_store: self.approval_receipt_store.clone(),
            compaction: self.compaction.clone(),
        }
    }

    /// Whether the next spawn would exceed the depth cap.
    #[must_use]
    pub fn would_exceed_depth(&self) -> bool {
        self.spawn_depth
            >= self
                .max_spawn_depth
                .min(self.worker_profile.max_spawn_depth)
    }
}

#[derive(Clone)]
struct SubAgentWorkLifecycle {
    work: SharedWorkRuntime,
    session_id: String,
    external: String,
}

impl SubAgentWorkLifecycle {
    fn register(runtime: &SubAgentRuntime, agent_id: &str, title: &str) -> Result<Option<Self>> {
        let Some(work) = runtime.context.runtime.work.clone() else {
            return Ok(None);
        };
        let session_id = runtime.context.state_namespace.clone();
        let external = format!("worker:{agent_id}");
        let lifecycle = Self {
            work,
            session_id,
            external,
        };
        if let Err(err) = lifecycle.work.register_operation(
            &lifecycle.session_id,
            OperationIntent::new(
                lifecycle.external.clone(),
                title,
                true,
                "agent",
                format!("agent:{agent_id}:spawn"),
            ),
        ) {
            // Bookkeeping must not veto dispatch (#6194): a transiently busy
            // To-do/Plan state leaves the spawn unbound, and every later
            // reconcile skips via the `Option<SubAgentWorkLifecycle>`.
            tracing::warn!(
                agent_id = %agent_id,
                error = %err,
                "sub-agent work-graph registration skipped; spawning unbound"
            );
            return Ok(None);
        }

        // A root Operate verification lease is provenance, not delegated
        // authority. Nested workers cannot inherit this bit.
        if runtime.accept_verification && runtime.spawn_depth == 1 {
            let reference = format!("operate-verification:{agent_id}");
            if let Err(err) = lifecycle.work.record_operation_approval(
                &lifecycle.session_id,
                &lifecycle.external,
                &reference,
                "agent",
                &format!("agent:{agent_id}:approval"),
            ) {
                let _ = lifecycle.reconcile_state(OwnerState::Failed, 1, None);
                return Err(anyhow!(
                    "failed to record sub-agent verification approval: {err}"
                ));
            }
        }
        Ok(Some(lifecycle))
    }

    fn reconcile_record(&self, record: &AgentWorkerRecord) -> Result<bool, String> {
        let Some(snapshot) = agent_worker_owner_snapshot(record) else {
            return Ok(false);
        };
        self.work.reconcile_operation(&self.session_id, snapshot)
    }

    fn reconcile_state(
        &self,
        state: OwnerState,
        seq: u64,
        output: Option<EvidenceRef>,
    ) -> Result<bool, String> {
        let observed_at = i64::try_from(epoch_millis_now()).unwrap_or(i64::MAX);
        let mut snapshot =
            OperationOwnerSnapshot::new(self.external.clone(), state, seq, observed_at);
        if let Some(output) = output {
            snapshot = snapshot.with_output(output);
        }
        self.work.reconcile_operation(&self.session_id, snapshot)
    }
}

/// Translate the persisted worker owner record after its manager has applied
/// restart recovery. A record without an event predates the lifecycle owner
/// protocol and cannot safely invent a sequence.
pub(crate) fn agent_worker_owner_snapshot(
    record: &AgentWorkerRecord,
) -> Option<OperationOwnerSnapshot> {
    let event = record.events.back()?;
    let output = record.result_summary.as_ref().and_then(|result| {
        EvidenceRef::new(
            EvidenceKind::Receipt {
                owner: "worker".to_string(),
            },
            format!("worker:{}:result", record.spec.worker_id),
            Some(u64::try_from(result.len()).unwrap_or(u64::MAX)),
            false,
        )
        .ok()
    });
    let observed_at = i64::try_from(event.timestamp_ms).unwrap_or(i64::MAX);
    let mut snapshot = OperationOwnerSnapshot::new(
        format!("worker:{}", record.spec.worker_id),
        owner_state_from_worker_status(record.status),
        event.seq,
        observed_at,
    );
    if let Some(output) = output {
        snapshot = snapshot.with_output(output);
    }
    Some(snapshot)
}

fn owner_state_from_worker_status(status: AgentWorkerStatus) -> OwnerState {
    match status {
        AgentWorkerStatus::Starting | AgentWorkerStatus::Running => OwnerState::Running,
        AgentWorkerStatus::Queued | AgentWorkerStatus::WaitingForUser => OwnerState::Waiting,
        AgentWorkerStatus::ModelWait | AgentWorkerStatus::RunningTool => OwnerState::Running,
        AgentWorkerStatus::Completed => OwnerState::Completed,
        AgentWorkerStatus::Failed | AgentWorkerStatus::Interrupted => OwnerState::Failed,
        AgentWorkerStatus::Cancelled => OwnerState::Cancelled,
    }
}

/// A running sub-agent instance.
pub struct SubAgent {
    pub id: String,
    pub session_name: String,
    pub fork_context: bool,
    pub agent_type: FleetRole,
    pub prompt: String,
    pub assignment: SubAgentAssignment,
    pub model: String,
    pub nickname: Option<String>,
    pub status: SubAgentStatus,
    pub result: Option<String>,
    pub steps_taken: u32,
    pub checkpoint: Option<SubAgentCheckpoint>,
    pub needs_input: Option<SubAgentNeedsInput>,
    pub started_at: Instant,
    pub last_activity_at: Instant,
    heartbeat_timeout: Option<Duration>,
    /// `None` = full registry inheritance, with approval-gated tools still
    /// blocked unless the parent runtime is auto-approved.
    /// `Some(list)` = explicit narrow allowlist (Custom agents, legacy).
    pub allowed_tools: Option<Vec<String>>,
    /// Stable id of the manager that spawned this agent (#405). Compared
    /// against the manager's `current_session_boot_id` to classify the
    /// agent as in-session vs prior-session at list time.
    pub session_boot_id: String,
    /// Immutable root conversation owner. Empty is a legacy/unattached value
    /// and must never match an active session.
    owner_session_id: String,
    pub workspace: PathBuf,
    /// Internal completion/cancellation arbitration bit. While set, the task
    /// has won the right to publish its terminal notifications, but the public
    /// status deliberately remains `Running` until those notifications are
    /// queued (#1961). Competing cancellation/interrupt paths must treat the
    /// claim as terminal ownership and leave the task to finalize.
    completion_claimed: bool,
    /// Process-local terminal fan-in sinks. Never serialized; restored agents
    /// have no live parent/mailbox/event consumers and are reconciled directly
    /// to interrupted state during load.
    terminal_delivery: Option<SubAgentTerminalDeliveryContext>,
    work_lifecycle: Option<SubAgentWorkLifecycle>,
    input_tx: Option<mpsc::UnboundedSender<SubAgentInput>>,
    task_handle: Option<JoinHandle<()>>,
    // Captured only for an actual admitted executor; never reconstructed on load.
    stop_token: Option<CancellationToken>,
    requested_stop: Option<(SubAgentStatus, Option<String>)>,
    // An unresolved forced-stop retains the exact existing capacity/barrier
    // guards until owner teardown; it cannot be replayed as completed work.
    unsettled_launch: Option<Vec<governor::DynamicGatePermit>>,
    unsettled_foreground: Option<ForegroundChildRegistration>,
}

impl SubAgent {
    /// Create a new sub-agent. The `id` is generated by the caller so that
    /// deterministic whale-naming can hash the ID before construction.
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: String,
        agent_type: FleetRole,
        prompt: String,
        assignment: SubAgentAssignment,
        model: String,
        nickname: Option<String>,
        allowed_tools: Option<Vec<String>>,
        input_tx: mpsc::UnboundedSender<SubAgentInput>,
        workspace: PathBuf,
        session_boot_id: String,
    ) -> Self {
        let session_name = id.clone();

        let started_at = Instant::now();
        Self {
            id,
            session_name,
            fork_context: false,
            agent_type,
            prompt,
            assignment,
            model,
            nickname,
            status: SubAgentStatus::Running,
            result: None,
            steps_taken: 0,
            checkpoint: None,
            needs_input: None,
            started_at,
            last_activity_at: started_at,
            heartbeat_timeout: None,
            allowed_tools,
            session_boot_id,
            owner_session_id: String::new(),
            workspace,
            completion_claimed: false,
            terminal_delivery: None,
            work_lifecycle: None,
            input_tx: Some(input_tx),
            task_handle: None,
            stop_token: None,
            requested_stop: None,
            unsettled_launch: None,
            unsettled_foreground: None,
        }
    }

    /// Get a snapshot of the current state.
    #[must_use]
    pub fn snapshot(&self) -> SubAgentResult {
        SubAgentResult {
            usage: None,
            name: self.session_name.clone(),
            agent_id: self.id.clone(),
            context_mode: if self.fork_context { "forked" } else { "fresh" }.to_string(),
            fork_context: self.fork_context,
            workspace: Some(self.workspace.clone()),
            git_branch: current_git_branch(&self.workspace),
            agent_type: self.agent_type.clone(),
            assignment: self.assignment.clone(),
            model: self.model.clone(),
            nickname: self.nickname.clone(),
            status: self.status.clone(),
            worker_status: None,
            runtime_permissions: None,
            parent_run_id: None,
            spawn_depth: 0,
            child_route: None,
            result: self.result.clone(),
            steps_taken: self.steps_taken,
            checkpoint: self.checkpoint.clone(),
            needs_input: self.needs_input.clone(),
            duration_ms: u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            started_at: Some(self.started_at),
            // Snapshots from the agent itself don't know the manager's
            // current boot id, so default to false. The manager fills
            // this in when it produces a snapshot via its own
            // `snapshot_for_listing` helper (#405).
            from_prior_session: false,
            idle_ms: None,
            heartbeat_timeout_ms: None,
        }
    }
}

/// Manager for active sub-agents.
struct CoordinationProcessLock {
    release: Option<std::sync::mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Marker prefix for a coordination-lock failure whose holder is THIS
/// process: an engine swap (model/provider switch) constructs the new
/// engine's manager while the old engine still holds the flock on its own
/// fd, and flock treats a second descriptor in the same process as a
/// conflict. This is a transient handover that self-heals on the next
/// projection retry (#5036) — UI surfaces must not present it as "another
/// Codewhale process" (owner report, 2026-08-04).
pub const COORDINATION_SAME_PROCESS_HANDOVER: &str =
    "handing delegated coordination between engines in this process";

/// Marker inside a lock-acquisition error meaning the flock wait expired
/// rather than that another session holds it. The status strip distinguishes
/// the two, because "a second session is open here" and "the claim timed out"
/// call for different responses.
pub const COORDINATION_LOCK_TIMEOUT_MARKER: &str =
    "timed out acquiring delegated coordination lock";

impl CoordinationProcessLock {
    fn acquire(state_root: &Path, exclusive: bool) -> Result<Self> {
        let requested_root = normalize_subagent_workspace(state_root);
        let lock_dir = requested_root.join(".codewhale").join("state");
        // Refuse a linked `.codewhale` or `state` before creating anything:
        // `create_dir_all` follows links and would populate the target.
        reject_root_relative_symlinks(&requested_root, &lock_dir.join(SUBAGENT_STATE_LOCK_FILE))?;
        fs::create_dir_all(&lock_dir)?;
        let ignore = lock_dir.join(".gitignore");
        let ignored = reject_root_relative_symlinks(&requested_root, &ignore).and_then(|()| {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&ignore)
            {
                Ok(mut file) => file.write_all(b"*\n").map_err(Into::into),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(error.into()),
            }
        });
        if let Err(error) = ignored {
            tracing::warn!(target: "subagent", %error, "could not write {}", ignore.display());
        }
        // Creating a missing root can change its canonical spelling on
        // Windows (for example by adding a `\\?\` prefix). Re-resolve both
        // sides before checking containment.
        let state_root = normalize_subagent_workspace(state_root);
        let lock_path = state_root
            .join(".codewhale")
            .join("state")
            .join(SUBAGENT_STATE_LOCK_FILE);
        reject_root_relative_symlinks(&state_root, &lock_path)?;
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let thread = std::thread::spawn(move || {
            let mut lock = fd_lock::RwLock::new(lock_file);
            let acquired = if exclusive {
                lock.try_write().map(|guard| {
                    let _ = ready_tx.send(Ok::<(), String>(()));
                    let _guard = guard;
                    let _ = release_rx.recv();
                })
            } else {
                lock.try_read().map(|guard| {
                    let _ = ready_tx.send(Ok::<(), String>(()));
                    let _guard = guard;
                    let _ = release_rx.recv();
                })
            };
            match acquired {
                Ok(()) => {}
                Err(error) => {
                    let _ = ready_tx.send(Err(error.to_string()));
                }
            }
        });
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                release: Some(release_tx),
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                let holder_pid = std::fs::read_to_string(&lock_path)
                    .ok()
                    .and_then(|contents| contents.trim().parse::<u32>().ok());
                if holder_pid == Some(std::process::id()) {
                    Err(anyhow!(
                        "{COORDINATION_SAME_PROCESS_HANDOVER} for {}: {error}",
                        state_root.display()
                    ))
                } else {
                    Err(anyhow!(
                        "another Codewhale process{} owns delegated coordination for {}: {error}",
                        holder_pid
                            .map(|pid| format!(" (pid {pid})"))
                            .unwrap_or_default(),
                        state_root.display()
                    ))
                }
            }
            Err(error) => {
                drop(release_tx);
                let _ = thread.join();
                Err(anyhow!(
                    "{COORDINATION_LOCK_TIMEOUT_MARKER} for {}: {error}",
                    state_root.display()
                ))
            }
        }
    }
}

impl Drop for CoordinationProcessLock {
    fn drop(&mut self) {
        self.release.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Only the captured Runtime owner admits roots. Held handles are not serialized;
/// a restored attributed claim must be re-admitted before it can authorize work.
struct AdmittedCoordinationRoot {
    lexical: PathBuf,
    receipt: CoordinationClaimScope,
    directory: Arc<fs::File>,
}

impl AdmittedCoordinationRoot {
    fn validate(&self) -> Result<(), String> {
        let (canonical, current) = crate::runtime_api::open_workspace_directory(&self.lexical)
            .map_err(|error| error.to_string())?;
        if canonical != self.receipt.canonical_root
            || !crate::fleet::files::same_file(&self.directory, &current)
                .map_err(|error| error.to_string())?
        {
            return Err("coordination workspace identity changed".into());
        }
        Ok(())
    }
}

pub struct SubAgentManager {
    agents: HashMap<String, SubAgent>,
    worker_records: HashMap<String, AgentWorkerRecord>,
    /// In-flight report allowances, not billed usage. Weak leases disappear
    /// on cancellation/abort without awaiting the manager lock or persisting
    /// a refundable reservation across process recovery.
    handback_reservations: HashMap<String, std::sync::Weak<u64>>,
    worker_event_seq: u64,
    persist_sequence: std::sync::atomic::AtomicU64,
    coordination: CoordinationLedger,
    admitted_coordination_roots: HashMap<PathBuf, AdmittedCoordinationRoot>,
    worker_origins: HashMap<String, WorkerOrigin>,
    worker_origin_schema: u32,
    #[allow(dead_code)] // Stored for future workspace-scoped operations
    workspace: PathBuf,
    /// Root that owns the delegated-agent ledger, complete transcript
    /// artifacts and coordination lock. Defaults to `workspace`; embedders may
    /// isolate it without changing child execution cwd or file authority.
    state_root: PathBuf,
    state_path: Option<PathBuf>,
    coordination_process_lock: std::sync::Mutex<Option<Arc<CoordinationProcessLock>>>,
    coordination_process_lock_required: bool,
    coordination_exclusive: bool,
    workspace_writer_locks: HashMap<PathBuf, Arc<CoordinationProcessLock>>,
    /// Configured default per-child model-turn budget (`[subagents]
    /// default_max_steps`, #5324). `None` keeps the unbounded Fleet default;
    /// an explicit spawn `max_steps` still wins. Zero means unbounded.
    max_steps: Option<u32>,
    /// Configured default per-child wall-clock budget (`[subagents]
    /// default_wall_time_secs`, #5324). `None` keeps
    /// `DEFAULT_CHILD_WALL_TIME`, which an explicit spawn `wall_time_secs`
    /// may raise up to `MAX_CHILD_WALL_TIME`; a configured value is also a
    /// ceiling on explicit requests.
    wall_time: Option<Duration>,
    max_agents: usize,
    max_admitted_agents: usize,
    running_heartbeat_timeout: Duration,
    /// Stable id assigned at manager construction (#405). Stamped on
    /// every agent the manager spawns; agents loaded from the
    /// persisted state file carry whatever id the prior session
    /// stamped (or empty for pre-#405 records). The manager classifies
    /// agents whose `session_boot_id` doesn't match this value as
    /// "from prior session" so listings can hide them by default.
    current_session_boot_id: String,
    /// Launch gate for direct (depth-1) sub-agent launches (#3095). Each
    /// permit is one actively executing direct child; further direct
    /// children spawn immediately but queue for a permit before starting,
    /// publishing a visible "queued" reason instead of bursting. Deeper
    /// descendants bypass the gate so a permit-holding parent waiting on
    /// its own children cannot deadlock the tree.
    ///
    /// The gate is a [`governor::DynamicGate`] rather than a
    /// `tokio::sync::Semaphore` so the rate-limit governor can shrink its
    /// capacity at runtime (even below the number of active children)
    /// without replacing the `Arc` — a semaphore swap silently fails while
    /// any child still holds a permit, which is why
    /// `update_runtime_limits` previously only applied launch-concurrency
    /// changes to an idle fleet.
    launch_gate: Arc<governor::DynamicGate>,
    /// Rate-limit aware scheduler feeding `launch_gate` (swarm-mode
    /// adaptive throttling). Sub-agent LLM attempts report 429s and
    /// successes through [`SubAgentRuntime::governor`]; the governor
    /// shrinks/pauses admissions on sustained 429s and recovers via AIMD.
    governor: Arc<governor::RateLimitGovernor>,
    /// #freeze: hot-path persist debounce bookkeeping (see
    /// `SUBAGENT_PERSIST_DEBOUNCE`). `last_persist_at` is the last time any
    /// state persist ran; `persist_pending` records that a hot-path write was
    /// coalesced away so a later flush (terminal write or shutdown) can
    /// capture the most recent checkpoint.
    last_persist_at: Option<Instant>,
    persist_pending: bool,
    /// #3803: last time `cleanup` ran. The sidebar refresh (`Op::ListSubAgents`)
    /// renders from a read-only `list()` snapshot and only runs the
    /// write-locked `cleanup` on a bounded cadence, so a UI refresh storm during
    /// a sub-agent fanout no longer contends for the write lock on every request.
    last_cleanup_at: Option<Instant>,
    /// Parent mail queued by `agents/message` without waking the child.
    /// `agents/followup` drains into `input_tx` when a live wake is possible.
    queued_mail: HashMap<String, VecDeque<QueuedParentMessage>>,
    /// Follow-ups handed to a running child's live input channel that the
    /// child has not yet taken at a round boundary. Read by the rail rows.
    pending_follow_ups: HashMap<String, Arc<std::sync::atomic::AtomicUsize>>,
    /// Test/observability: agent ids that received a live wake via followup.
    woken_agents: HashMap<String, bool>,
    /// Agent ids whose handle-store entries should be evicted on the next async
    /// drain. Populated by `cleanup()` when an agent record is retired; drained
    /// by async callers that hold the `HandleStore` lock (#3885).
    pending_handle_evictions: Vec<(String, String)>,
    /// Checkpoint-resume idempotency map: interrupted agent id -> the agent
    /// id of the session resumed from its checkpoint. A second followup on
    /// the same interrupted id returns the existing resumed target instead of
    /// spawning a duplicate agent loop (duplicate-resume guard).
    resume_targets: HashMap<String, String>,
    /// Isolated worktrees whose finished-worker removal is claimed and may be
    /// running on the blocking pool. Continuation refuses these paths until
    /// the removal settles, so a successor never starts in a directory that
    /// is about to be deleted. Shared so the blocking task releases its claim
    /// without the manager lock, even when the awaiting task was aborted.
    worktree_cleanups: Arc<std::sync::Mutex<HashSet<PathBuf>>>,
    /// Approval prompts raised on a child's behalf under Ask: the approval id
    /// the host sees (`agent:<agent_id>:approval:<execution UUID>`) → the
    /// waiting child. The engine
    /// routes the person's decision here; a decision for an id nobody is
    /// waiting on is dropped, never applied to a different call.
    child_approvals: HashMap<String, ChildPendingRequest>,
    /// Wake cursor for `agent wait` (approvals C2): approval ids already
    /// reported to the parent model as `needs_person`. A reported id keeps a
    /// later wait blocking instead of re-waking on the same request.
    reported_pending: HashSet<String>,
    /// Saved-profile routes (`provider/model`) whose first request this
    /// session refused authorization, with the short reason. In memory only:
    /// later spawns of the same pin go straight to the parent route instead
    /// of each failing first; a new session re-tries the pin.
    refused_routes: HashMap<String, String>,
    /// Children whose requests moved to another route before any work, by
    /// agent id, so the task's failure text blames the route that failed.
    /// Taken when the task settles.
    rerouted_requests: HashMap<String, ReroutedRequest>,
}

/// One child request waiting on a person (approvals C2). The store is the
/// authority for "waiting": status and `agent wait` derive from it rather
/// than from a progress event that can be skipped under lock contention.
#[derive(Debug)]
pub struct ChildPendingRequest {
    tx: tokio::sync::oneshot::Sender<ChildApprovalOutcome>,
    agent_id: String,
    tool_name: String,
    summary: String,
    requested_at: std::time::Instant,
}

/// What a waiting child needs from a person, as the parent model sees it.
/// Children cannot ask questions today, so `kind` is always
/// `needs_approval`; `needs_input` is reserved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRequestView {
    pub kind: String,
    pub approval_id: String,
    pub tool: String,
    pub summary: String,
}

/// Bound for the one-line summary carried in `needs_person` / status.
const PENDING_REQUEST_SUMMARY_MAX_CHARS: usize = 160;

fn one_line_pending_summary(reason: &str) -> String {
    let collapsed = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= PENDING_REQUEST_SUMMARY_MAX_CHARS {
        collapsed
    } else {
        let mut bounded: String = collapsed
            .chars()
            .take(PENDING_REQUEST_SUMMARY_MAX_CHARS.saturating_sub(1))
            .collect();
        bounded.push('…');
        bounded
    }
}

/// Approval keys for a child's held call: the normal exact/grouping scheme
/// (`tools/approval_cache.rs`), prefixed with the owning agent. A person's
/// "allow for this conversation" on a child card then covers that same
/// agent's later calls in the same family, and never the parent's calls or a
/// sibling agent's (approvals program C3). Denials keep the exact key.
#[must_use]
#[cfg(test)]
pub(crate) fn child_approval_keys(agent_id: &str, name: &str, input: &Value) -> (String, String) {
    let exact = crate::tools::approval_cache::build_approval_key(name, input).0;
    let grouping = crate::tools::approval_cache::build_approval_grouping_key(name, input).0;
    (
        format!("agent:{agent_id}:{exact}"),
        format!("agent:{agent_id}:{grouping}"),
    )
}

/// An answer to a child's approval prompt, preserving who made the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildApprovalOutcome {
    Approved {
        by: crate::approval_log::ApprovalDecider,
    },
    Denied {
        by: crate::approval_log::ApprovalDecider,
    },
    /// The host could not put the request in front of a person (for example
    /// it belongs to a conversation that is no longer shown). Recorded as
    /// `unavailable`, never as the person's denial.
    Unavailable,
}

/// Time a child's tool call has spent waiting on a person's approval
/// decision (CURRENT_DECISIONS §21). The per-tool timeout does not count it —
/// an approval gets a decision or stays pending until its agent's work ends,
/// never timed out into a deny. The wall-clock deadline still counts it.
#[derive(Debug, Default, Clone, Copy)]
struct PersonWaitState {
    waiting_since: Option<Instant>,
    completed: Duration,
}

impl PersonWaitState {
    fn paused_total(&self, now: Instant) -> Duration {
        self.completed
            + self
                .waiting_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
    }
}

/// One child's person-wait clock: the approval gate pauses it while it waits,
/// and [`run_tool_with_person_aware_timeout`] stops the tool timer meanwhile.
#[derive(Debug)]
pub(crate) struct PersonWaitClock(tokio::sync::watch::Sender<PersonWaitState>);

impl Default for PersonWaitClock {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(PersonWaitState::default()).0)
    }
}

impl PersonWaitClock {
    /// Pause the tool timer until the returned guard drops.
    fn pause(self: &Arc<Self>) -> PersonWaitPause {
        self.0.send_modify(|state| {
            state.waiting_since.get_or_insert_with(Instant::now);
        });
        PersonWaitPause(Arc::clone(self))
    }
}

pub(crate) struct PersonWaitPause(Arc<PersonWaitClock>);

impl Drop for PersonWaitPause {
    fn drop(&mut self) {
        self.0.0.send_modify(|state| {
            if let Some(since) = state.waiting_since.take() {
                state.completed += since.elapsed();
            }
        });
    }
}

/// Run one child tool call under `tool_timeout`, not counting time spent
/// waiting on a person's approval decision, and under the wall
/// `work_deadline`, which does count it (§21). `None` means a bound fired and
/// the call was dropped.
async fn run_tool_with_person_aware_timeout<F: std::future::Future>(
    tool_timeout: Duration,
    work_deadline: Option<Instant>,
    clock: &PersonWaitClock,
    future: F,
) -> Option<F::Output> {
    let mut changes = clock.0.subscribe();
    let started = Instant::now();
    let paused_before = clock.0.borrow().paused_total(started);
    tokio::pin!(future);
    loop {
        let now = Instant::now();
        let state = *changes.borrow_and_update();
        let tool_deadline = state.waiting_since.is_none().then(|| {
            started + tool_timeout + state.paused_total(now).saturating_sub(paused_before)
        });
        let deadline = match (tool_deadline, work_deadline) {
            (Some(tool), Some(work)) => Some(tool.min(work)),
            (tool, work) => tool.or(work),
        };
        if deadline.is_some_and(|deadline| now >= deadline) {
            return None;
        }
        tokio::select! {
            output = &mut future => return Some(output),
            () = async {
                match deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                    None => std::future::pending().await,
                }
            } => {}
            // The sender outlives this call (`clock` is borrowed), so this
            // only wakes on a pause or resume.
            _ = changes.changed() => {}
        }
    }
}

/// Tell hosts a child's approval wait ended, by identity, without the
/// back-pressure drop routine progress takes: the event carries the approval
/// id, so hosts retire exactly that card and pending entry. A terminal
/// `worker_status` marks a withdrawal whose agent has already ended; its
/// worker record is left alone.
#[cfg(test)]
async fn announce_child_approval_wait_ended(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    approval_id: &str,
    tool: &str,
    worker_status: AgentWorkerStatus,
    message: String,
) {
    if !worker_status.is_terminal()
        && let Ok(mut manager) = runtime.manager.try_write()
    {
        manager.touch(agent_id);
        manager.record_worker_event(
            agent_id,
            worker_status,
            Some(message.clone()),
            None,
            Some(tool.to_string()),
        );
    }
    if let Some(event_tx) = runtime.event_tx.as_ref() {
        let _ = event_tx
            .send(Event::AgentProgress {
                owner_session_id: runtime.context.state_namespace.clone(),
                id: agent_id.to_string(),
                status: message,
                activity: AgentProgressEventMeta::new(worker_status)
                    .with_tool(tool.to_string())
                    .with_approval_id(approval_id.to_string()),
                parent_run_id: runtime.parent_agent_id.clone(),
                spawn_depth: runtime.spawn_depth,
            })
            .await;
    }
}

impl SubAgentManager {
    /// Register a prompt raised for a child's held call. Returns the approval
    /// id to publish and the receiver the child awaits.
    pub fn register_child_approval(
        &mut self,
        agent_id: &str,
        tool_id: &str,
        tool_name: &str,
        reason: &str,
    ) -> Result<(String, tokio::sync::oneshot::Receiver<ChildApprovalOutcome>)> {
        // The host mints this before checkpointing the call. Approval, history,
        // audit, and artifacts share it; registration never allocates another.
        let prefix = format!("agent:{agent_id}:approval:");
        anyhow::ensure!(
            tool_id
                .strip_prefix(&prefix)
                .is_some_and(|id| Uuid::parse_str(id).is_ok()),
            "invalid child execution identity"
        );
        let std::collections::hash_map::Entry::Vacant(entry) =
            self.child_approvals.entry(tool_id.to_string())
        else {
            anyhow::bail!("child execution already has a pending approval");
        };
        let (tx, rx) = tokio::sync::oneshot::channel();
        entry.insert(ChildPendingRequest {
            tx,
            agent_id: agent_id.to_string(),
            tool_name: tool_name.to_string(),
            summary: one_line_pending_summary(reason),
            requested_at: std::time::Instant::now(),
        });
        Ok((tool_id.to_string(), rx))
    }

    /// Requests from `agent_id` still waiting on a person, oldest first.
    #[must_use]
    pub fn pending_requests_for_agent(&self, agent_id: &str) -> Vec<PendingRequestView> {
        let mut pending: Vec<(&String, &ChildPendingRequest)> = self
            .child_approvals
            .iter()
            .filter(|(_, request)| request.agent_id == agent_id)
            .collect();
        pending.sort_by_key(|(_, request)| request.requested_at);
        pending
            .into_iter()
            .map(|(id, request)| PendingRequestView {
                kind: "needs_approval".to_string(),
                approval_id: id.clone(),
                tool: request.tool_name.clone(),
                summary: request.summary.clone(),
            })
            .collect()
    }

    /// Whether any of `agent_ids` has a pending request not yet reported to
    /// the parent model by `agent wait`.
    #[must_use]
    pub fn has_unreported_needs_person(&self, agent_ids: &[String]) -> bool {
        self.child_approvals.iter().any(|(id, request)| {
            agent_ids.contains(&request.agent_id) && !self.reported_pending.contains(id)
        })
    }

    /// The unreported pending requests of `agent_ids` as `needs_person`
    /// entries, marking each reported so the next wait does not re-wake on it.
    pub fn take_unreported_needs_person(&mut self, agent_ids: &[String]) -> Vec<Value> {
        let mut entries = Vec::new();
        for agent_id in agent_ids {
            let name = self
                .agents
                .get(agent_id)
                .map(|agent| agent.session_name.clone())
                .unwrap_or_else(|| agent_id.clone());
            for request in self.pending_requests_for_agent(agent_id) {
                if !self.reported_pending.insert(request.approval_id.clone()) {
                    continue;
                }
                entries.push(json!({
                    "agent_id": agent_id,
                    "name": name,
                    "kind": request.kind,
                    "tool": request.tool,
                    "summary": request.summary,
                }));
            }
        }
        entries
    }

    /// Derive "waiting on a person" from the pending store (approvals C2):
    /// a pending request makes the record `waiting_for_user` with a
    /// `pending_request` and a tell-the-user action; a running agent whose
    /// recorded wait already ended (the post-wait progress was skipped under
    /// contention) is no longer reported as waiting.
    fn overlay_pending_request(&self, record: &mut AgentWorkerRecord) {
        let agent_id = record.spec.worker_id.clone();
        // Only a running agent can be waiting: a cancelled or finished one
        // reports its stored terminal status, never `tell_user` (approvals M1).
        if !self
            .agents
            .get(&agent_id)
            .is_some_and(|agent| agent.status == SubAgentStatus::Running)
        {
            return;
        }
        if let Some(pending) = self
            .pending_requests_for_agent(&agent_id)
            .into_iter()
            .next()
        {
            record.status = AgentWorkerStatus::WaitingForUser;
            record.recommended_action = tell_user_recommended_action(&record.spec, &pending);
            record.pending_request = Some(pending);
        } else if record.status == AgentWorkerStatus::WaitingForUser {
            record.status = AgentWorkerStatus::RunningTool;
            record.recommended_action =
                recommended_action_for_worker_status(record.status, &record.spec);
        }
    }

    /// Whether an approval id belongs to a child prompt (routing hint for the
    /// engine before it consults the map).
    #[must_use]
    pub fn is_child_approval_id(id: &str) -> bool {
        id.starts_with("agent:") && id.contains(":approval:")
    }

    /// Deliver a person's decision to the waiting child. Returns `false` when
    /// no child is waiting on that id (already answered, cancelled, or not a
    /// child prompt).
    pub fn resolve_child_approval(&mut self, id: &str, outcome: ChildApprovalOutcome) -> bool {
        if self.child_approvals.get(id).is_some_and(|request| {
            self.agents.get(&request.agent_id).is_some_and(|agent| {
                agent.requested_stop.is_some()
                    || agent
                        .stop_token
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled)
            })
        }) {
            // Do not drop the receiver before Core's cancelled branch appends
            // its decision. A late Allow cannot enter the cancelled inbox.
            return false;
        }
        self.reported_pending.remove(id);
        match self.child_approvals.remove(id) {
            Some(request) => request.tx.send(outcome).is_ok(),
            None => false,
        }
    }

    /// Forget a prompt the child stopped waiting for (cancellation).
    pub fn cancel_child_approval(&mut self, id: &str) {
        self.child_approvals.remove(id);
        self.reported_pending.remove(id);
    }

    /// Forget projections after the owned executor settles. This map never
    /// writes a decision receipt; Core's awaited approval path is authoritative.
    fn withdraw_child_approvals_for_agent(&mut self, agent_id: &str) {
        let ids: Vec<String> = self
            .child_approvals
            .iter()
            .filter(|(_, request)| request.agent_id == agent_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.cancel_child_approval(&id);
        }
    }

    /// Number of child prompts currently awaiting a person.
    #[must_use]
    #[cfg(test)]
    pub fn pending_child_approvals(&self) -> usize {
        self.child_approvals.len()
    }

    /// Create a new manager for sub-agents.
    #[must_use]
    pub fn new(workspace: PathBuf, max_agents: usize) -> Self {
        let state_root = workspace.clone();
        Self::new_with_state_root(workspace, state_root, max_agents)
    }

    /// Create a manager whose delegated control-plane state is rooted
    /// separately from its execution workspace.
    #[must_use]
    pub fn new_with_state_root(workspace: PathBuf, state_root: PathBuf, max_agents: usize) -> Self {
        // The governor owns the launch gate it schedules, so manager builders
        // and the runtime limiter adjust capacity through the pair.
        let (governor, launch_gate) = governor::RateLimitGovernor::new(max_agents.max(1));
        Self {
            agents: HashMap::new(),
            worker_records: HashMap::new(),
            handback_reservations: HashMap::new(),
            worker_event_seq: 0,
            persist_sequence: std::sync::atomic::AtomicU64::new(0),
            coordination: CoordinationLedger::default(),
            admitted_coordination_roots: HashMap::new(),
            worker_origins: HashMap::new(),
            worker_origin_schema: SUBAGENT_STATE_SCHEMA_VERSION,
            workspace,
            state_root,
            state_path: None,
            coordination_process_lock: std::sync::Mutex::new(None),
            coordination_process_lock_required: false,
            coordination_exclusive: false,
            workspace_writer_locks: HashMap::new(),
            max_steps: None,
            wall_time: None,
            max_agents,
            max_admitted_agents: max_agents,
            running_heartbeat_timeout: Duration::from_secs(
                crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS,
            ),
            // Fresh boot id per manager. Used by #405 to classify
            // re-loaded persisted agents as "prior session".
            current_session_boot_id: format!("boot_{}", &Uuid::new_v4().to_string()[..12]),
            // Default launch concurrency = the full agent cap; the gate only
            // throttles when a lower `launch_concurrency` is configured.
            launch_gate,
            governor,
            last_persist_at: None,
            persist_pending: false,
            last_cleanup_at: None,
            queued_mail: HashMap::new(),
            pending_follow_ups: HashMap::new(),
            woken_agents: HashMap::new(),
            pending_handle_evictions: Vec::new(),
            resume_targets: HashMap::new(),
            worktree_cleanups: Arc::default(),
            child_approvals: HashMap::new(),
            reported_pending: HashSet::new(),
            refused_routes: HashMap::new(),
            rerouted_requests: HashMap::new(),
        }
    }

    pub(crate) fn admit_coordination_workspace(
        &mut self,
        lexical: PathBuf,
        canonical_root: PathBuf,
        directory: Arc<fs::File>,
    ) -> Result<(), String> {
        if let Some(existing) = self.admitted_coordination_roots.get(&lexical) {
            if existing.receipt.canonical_root != canonical_root
                || !crate::fleet::files::same_file(&existing.directory, &directory)
                    .map_err(|error| error.to_string())?
            {
                return Err("coordination workspace identity changed".into());
            }
            return Ok(());
        }
        if self.admitted_coordination_roots.len() >= 64 {
            return Err("coordination workspace scope limit reached".into());
        }
        #[cfg(unix)]
        let (platform, volume, index) = {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = directory.metadata().map_err(|error| error.to_string())?;
            ("unix", metadata.dev(), metadata.ino())
        };
        #[cfg(windows)]
        let (platform, volume, index) = {
            let identity = crate::plugins::windows_file_identity(&directory)
                .map_err(|error| error.to_string())?;
            ("windows", u64::from(identity.volume), identity.index)
        };
        #[cfg(not(any(unix, windows)))]
        return Err("coordination workspace identity is unsupported on this platform".into());
        #[cfg(any(unix, windows))]
        {
            if self.coordination_exclusive
                && normalize_subagent_workspace(&lexical)
                    != normalize_subagent_workspace(&self.state_root)
                && !self.workspace_writer_locks.contains_key(&canonical_root)
            {
                let lock = CoordinationProcessLock::acquire(&lexical, true)
                    .map_err(|error| error.to_string())?;
                self.workspace_writer_locks
                    .insert(canonical_root.clone(), Arc::new(lock));
            }
            let receipt = CoordinationClaimScope {
                canonical_root,
                platform: platform.into(),
                volume,
                index,
            };
            receipt.validate_shape()?;
            self.admitted_coordination_roots.insert(
                lexical.clone(),
                AdmittedCoordinationRoot {
                    lexical,
                    receipt,
                    directory,
                },
            );
            Ok(())
        }
    }

    fn admitted_coordination_root(&self, workspace: &Path) -> Option<&AdmittedCoordinationRoot> {
        self.admitted_coordination_roots
            .values()
            .filter(|root| workspace.starts_with(&root.lexical))
            .max_by_key(|root| root.lexical.components().count())
    }

    fn capture_worker_origin(
        &self,
        workspace: &Path,
        owner_session_id: &str,
        kind: WorkerOriginKind,
    ) -> Result<WorkerOrigin, String> {
        if let Some(root) = self.admitted_coordination_root(workspace) {
            root.validate()?;
            return Ok(WorkerOrigin {
                kind,
                owner_session_id: owner_session_id.into(),
                selected_workspace: Some(root.lexical.clone()),
                scope: Some(root.receipt.clone()),
            });
        }
        // The historical single-root owner is already bound to this manager.
        // Do not use execution cwd/worktree as the origin of a new scope.
        coordination_workspace_prefix(&self.workspace, workspace)?;
        Ok(WorkerOrigin {
            kind,
            owner_session_id: owner_session_id.into(),
            selected_workspace: None,
            scope: None,
        })
    }

    pub(crate) fn capture_fleet_worker_origin(
        &self,
        workspace: &Path,
    ) -> Result<WorkerOrigin, String> {
        self.capture_worker_origin(workspace, "", WorkerOriginKind::Fleet)
    }

    fn validate_worker_origin(&self, origin: &WorkerOrigin) -> Result<(), String> {
        origin.validate_shape()?;
        if let (Some(workspace), Some(receipt)) = (&origin.selected_workspace, &origin.scope) {
            let admitted = self
                .admitted_coordination_roots
                .get(workspace)
                .filter(|root| &root.receipt == receipt)
                .ok_or_else(|| "worker origin requires exact owner re-admission".to_string())?;
            admitted.validate()?;
        }
        Ok(())
    }

    fn coordination_workspace_in_origin(
        &self,
        origin: &WorkerOrigin,
        execution_workspace: &Path,
        isolated: bool,
    ) -> Result<PathBuf, String> {
        self.validate_worker_origin(origin)?;
        if isolated {
            return Ok(execution_workspace.to_path_buf());
        }
        if let (Some(selected), Some(scope)) = (&origin.selected_workspace, &origin.scope) {
            let relative = execution_workspace
                .strip_prefix(selected)
                .or_else(|_| execution_workspace.strip_prefix(&scope.canonical_root))
                .map_err(|_| {
                    "worker execution workspace is outside its captured origin".to_string()
                })?;
            if relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            }) {
                return Err("worker execution workspace is not normalized".into());
            }
            return Ok(selected.join(relative));
        }
        coordination_workspace_prefix(&self.workspace, execution_workspace)?;
        Ok(execution_workspace.to_path_buf())
    }

    fn capture_direct_worker_origin(
        &self,
        workspace: &Path,
        owner_session: &str,
    ) -> Result<WorkerOrigin, String> {
        self.capture_worker_origin(workspace, owner_session, WorkerOriginKind::Direct)
    }

    fn capture_direct_worker_origin_for_runtime(
        &self,
        workspace: &Path,
        owner_session: &str,
        parent_agent_id: Option<&str>,
    ) -> Result<WorkerOrigin, String> {
        let Some(parent_id) = parent_agent_id else {
            return self.capture_direct_worker_origin(workspace, owner_session);
        };
        // The parent's worktree is an execution target, not another admission.
        // Only the actual live parent in this actor and session can pass its
        // original root receipt to a nested child.
        let parent = self
            .agents
            .get(parent_id)
            .filter(|parent| {
                self.agent_is_owned_by_session(parent, owner_session)
                    && parent.status == SubAgentStatus::Running
            })
            .ok_or_else(|| {
                "nested worker parent is not live in the originating session".to_string()
            })?;
        let record = self
            .worker_records
            .get(&parent.id)
            .filter(|record| record.owner_session_id == owner_session)
            .ok_or_else(|| "nested worker parent has no matching owner record".to_string())?;
        let mut origin = self
            .worker_origins
            .get(&record.spec.worker_id)
            .filter(|origin| origin.owner_session_id == owner_session)
            .cloned()
            .ok_or_else(|| "nested worker parent has no captured origin".to_string())?;
        self.validate_worker_origin(&origin)?;
        origin.kind = WorkerOriginKind::Direct;
        Ok(origin)
    }

    fn continuation_worker_origin(
        &self,
        source: &str,
        owner_session: &str,
        parent_agent_id: Option<&str>,
        incoming: &WorkerOrigin,
    ) -> Result<WorkerOrigin> {
        let source_id = self.ensure_caller_controls_descendant_for_session(
            owner_session,
            source,
            parent_agent_id,
            "agent/resume_from",
        )?;
        let source_agent = self
            .agents
            .get(&source_id)
            .ok_or_else(|| anyhow!("continuation source is absent"))?;
        if source_agent.status == SubAgentStatus::Running {
            return Err(anyhow!("continuation source is still running"));
        }
        let saved = self
            .worker_origins
            .get(&source_id)
            .ok_or_else(|| anyhow!("continuation source has no captured worker origin"))?;
        self.validate_worker_origin(saved)
            .map_err(anyhow::Error::msg)?;
        self.validate_worker_origin(incoming)
            .map_err(anyhow::Error::msg)?;
        if saved.owner_session_id != owner_session
            || incoming.owner_session_id != owner_session
            || self
                .worker_records
                .get(&source_id)
                .is_none_or(|record| record.owner_session_id != owner_session)
        {
            return Err(anyhow!(
                "continuation source belongs to another owner session"
            ));
        }
        if saved == incoming {
            return Ok(saved.clone());
        }
        // Schema 1 records have an explicit original-owner marker. A newly
        // captured direct receipt can represent that same owner after restart;
        // accept it only against the actual held original directory admission.
        // Preserve the source marker, rather than upgrading recovery authority.
        if saved.kind == WorkerOriginKind::LegacyOriginal
            && incoming.kind == WorkerOriginKind::Direct
        {
            let original = self
                .admitted_coordination_roots
                .get(&self.workspace)
                .ok_or_else(|| {
                    anyhow!("legacy continuation requires held original-root admission")
                })?;
            original.validate().map_err(anyhow::Error::msg)?;
            if incoming.selected_workspace.as_ref() == Some(&self.workspace)
                && incoming.scope.as_ref() == Some(&original.receipt)
            {
                return Ok(saved.clone());
            }
        }
        Err(anyhow!(
            "continuation cannot replace its captured worker origin"
        ))
    }

    fn check_worker_origin_capacity(
        &self,
        worker_id: Option<&str>,
        origin: &WorkerOrigin,
    ) -> Result<(), String> {
        let scoped = self.worker_origin_schema == 2
            || origin
                .selected_workspace
                .as_ref()
                .is_some_and(|root| root != &self.workspace);
        if scoped
            && self.worker_records.len() >= MAX_AGENT_WORKER_RECORDS
            && worker_id.is_none_or(|id| !self.worker_records.contains_key(id))
            && self
                .worker_records
                .values()
                .filter(|record| !record.status.is_terminal())
                .count()
                >= MAX_AGENT_WORKER_RECORDS
        {
            return Err("worker origin journal active capacity reached".into());
        }
        Ok(())
    }

    fn install_worker_origin(&mut self, worker_id: &str, origin: WorkerOrigin) {
        if origin
            .selected_workspace
            .as_ref()
            .is_some_and(|root| root != &self.workspace)
        {
            self.worker_origin_schema = 2;
        }
        self.worker_origins.insert(worker_id.into(), origin);
    }

    fn prune_worker_origins(&mut self) {
        self.worker_origins
            .retain(|id, _| self.worker_records.contains_key(id));
    }

    fn origin_workspace_for_owner(&self, owner: &str) -> Result<&Path, String> {
        if let Some(origin) = self.worker_origins.get(owner) {
            self.validate_worker_origin(origin)?;
            return Ok(origin
                .selected_workspace
                .as_deref()
                .unwrap_or(&self.workspace));
        }
        if self.worker_origin_schema == 2 {
            return Err("worker origin is absent from schema2 owner state".into());
        }
        Ok(&self.workspace)
    }

    fn original_coordination_root(&self) -> &Path {
        self.admitted_coordination_root(&self.workspace)
            .map_or(self.workspace.as_path(), |root| {
                root.receipt.canonical_root.as_path()
            })
    }

    fn coordination_scope_for_workspace(
        &self,
        workspace: &Path,
    ) -> Result<Option<CoordinationClaimScope>, String> {
        let Some(root) = self.admitted_coordination_root(workspace) else {
            // Historical single-root callers retain their confined namespace.
            coordination_workspace_prefix(&self.workspace, workspace)?;
            return Ok(None);
        };
        if root
            .receipt
            .canonical_root
            .starts_with(self.original_coordination_root())
        {
            Ok(None)
        } else {
            Ok(Some(root.receipt.clone()))
        }
    }

    fn coordination_prefix(&self, workspace: &Path) -> Result<String, String> {
        if let Some(root) = self.admitted_coordination_root(workspace) {
            let relative = workspace
                .strip_prefix(&root.lexical)
                .map_err(|error| error.to_string())?;
            let physical = root.receipt.canonical_root.join(relative);
            let base = if physical.starts_with(self.original_coordination_root()) {
                self.original_coordination_root()
            } else {
                root.receipt.canonical_root.as_path()
            };
            return normalize_claim_path(
                &physical
                    .strip_prefix(base)
                    .map_err(|error| error.to_string())?
                    .to_string_lossy(),
            );
        }
        coordination_workspace_prefix(&self.workspace, workspace)
    }

    fn claim_root(&self, claim: &PersistedWriteClaim) -> Result<&Path, String> {
        if let Some(scope) = self.coordination.claim_scope(claim.sequence) {
            let admitted = self
                .admitted_coordination_roots
                .values()
                .find(|root| &root.receipt == scope)
                .ok_or_else(|| {
                    "restored coordination workspace requires owner re-admission".to_string()
                })?;
            return Ok(admitted.receipt.canonical_root.as_path());
        }
        Ok(self.original_coordination_root())
    }

    /// Set the number of direct children that may execute concurrently
    /// before further launches queue (#3095). Clamped to `1..=max_agents`.
    /// Applied to the live gate capacity, so this also takes effect when
    /// called after children have started. Routed through the governor so a
    /// rate-limit pause is not silently lifted by a limit change.
    #[must_use]
    pub fn with_launch_concurrency(self, limit: usize) -> Self {
        let limit = limit.clamp(1, self.max_agents);
        self.governor.set_max_capacity(limit);
        self
    }

    /// The rate-limit governor backing [`Self::launch_gate`]; exposed so the
    /// engine can stamp it onto root runtimes, `GET /v1/agent-runs` can report
    /// it (addendum F5), and tests can drive the adaptive scheduler.
    #[must_use]
    pub(crate) fn rate_limit_governor(&self) -> Arc<governor::RateLimitGovernor> {
        Arc::clone(&self.governor)
    }

    /// Set the total queued + running admission ceiling for this manager.
    /// The value is always at least the instantaneous concurrency cap.
    #[must_use]
    pub fn with_admission_limit(mut self, max_admitted: usize) -> Self {
        self.max_admitted_agents =
            max_admitted.clamp(self.max_agents, crate::config::MAX_SUBAGENT_ADMISSION);
        self
    }

    /// Set the configured default per-child model-turn budget applied when a
    /// spawn carries no explicit `max_steps` (#5324). `None` keeps the
    /// unbounded default; a positive explicit/configured value still caps it.
    #[must_use]
    pub fn with_default_max_steps(mut self, max_steps: Option<u32>) -> Self {
        self.max_steps = max_steps;
        self
    }

    /// Set the configured default per-child wall-clock budget applied when a
    /// spawn carries no explicit `wall_time_secs` (#5324). `None` keeps
    /// `DEFAULT_CHILD_WALL_TIME`.
    #[must_use]
    pub fn with_default_wall_time(mut self, wall_time: Option<Duration>) -> Self {
        self.wall_time = wall_time;
        self
    }

    /// Return the boot id this manager stamps on agents it spawns.
    /// Exposed for tests; internal callers use the field directly.
    #[cfg(test)]
    pub fn session_boot_id(&self) -> &str {
        &self.current_session_boot_id
    }

    #[cfg(test)]
    pub fn record_coordination_decision(
        &mut self,
        decision: DecisionRecord,
    ) -> Result<DecisionRecord, String> {
        let workspace = self.workspace.clone();
        self.record_coordination_decision_in_workspace(decision, &workspace)
    }

    fn record_coordination_decision_in_workspace(
        &mut self,
        mut decision: DecisionRecord,
        workspace: &Path,
    ) -> Result<DecisionRecord, String> {
        self.ensure_coordination_process_lock()?;
        if let Some(root) = self.admitted_coordination_root(workspace) {
            root.validate()?;
        }
        let scope = self.coordination_scope_for_workspace(workspace)?;
        let prefix = self.coordination_prefix(workspace)?;
        for value in &mut decision.scope {
            if let Some(path) = value.strip_prefix("path:") {
                *value = format!("path:{}", namespace_coordination_path(&prefix, path)?);
            }
        }
        self.coordination.record_decision_in_scope(decision, scope)
    }

    pub(crate) fn stamp_coordination_sequence_for_session(
        &mut self,
        sequence: u64,
        active_session_id: &str,
    ) -> Result<(), String> {
        if active_session_id.is_empty() {
            return Err("active session id is empty".to_string());
        }
        self.coordination
            .record_sessions
            .insert(sequence, active_session_id.to_string());
        Ok(())
    }

    fn coordination_record_is_owned_by_session(
        &self,
        active_session_id: &str,
        sequence: u64,
        owner: Option<&str>,
    ) -> bool {
        self.coordination
            .record_sessions
            .get(&sequence)
            .is_some_and(|session_id| session_id == active_session_id)
            || owner
                .is_some_and(|owner| self.agent_id_is_owned_by_session(owner, active_session_id))
    }

    pub(crate) fn coordination_decision_is_owned_by_session(
        &self,
        active_session_id: &str,
        decision_id: &str,
    ) -> bool {
        self.coordination.decisions.iter().any(|record| {
            record.decision_id == decision_id
                && self.coordination_record_is_owned_by_session(
                    active_session_id,
                    record.sequence,
                    Some(&record.owner),
                )
        })
    }

    pub fn update_coordination_decision(
        &mut self,
        decision_id: &str,
        status: DecisionStatus,
        owner: &str,
        expected_version: u32,
    ) -> Result<DecisionRecord, String> {
        self.ensure_coordination_process_lock()?;
        self.coordination
            .update_decision_status(decision_id, status, owner, expected_version)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reconcile_coordination(
        &mut self,
        subject: String,
        owner: String,
        input_decisions: Vec<String>,
        outcome: String,
        evidence_handles: Vec<String>,
        candidate_handles: Vec<String>,
        retry_count: u32,
        retry_limit: u32,
        reviewer_evidence_handles: Vec<String>,
        verifier_evidence_handles: Vec<String>,
        verification_outcome: String,
    ) -> Result<ReconciliationReceipt, String> {
        self.ensure_coordination_process_lock()?;
        let expected_owner = self.nearest_common_fan_in_owner(&input_decisions)?;
        if owner != expected_owner {
            return Err(format!(
                "neutral fan-in must be owned by nearest common Planner/release owner '{expected_owner}', not '{owner}'"
            ));
        }
        let reviewer_ids = self.validate_reconciliation_role_evidence(
            &reviewer_evidence_handles,
            FleetRole::Reviewer,
            "Reviewer",
        )?;
        let verifier_ids = self.validate_reconciliation_role_evidence(
            &verifier_evidence_handles,
            FleetRole::Verifier,
            "Verifier",
        )?;
        if !reviewer_ids.is_disjoint(&verifier_ids) {
            return Err(
                "Reviewer and Verifier evidence must come from distinct workers".to_string(),
            );
        }
        let candidate_owners = input_decisions
            .iter()
            .filter_map(|decision_id| {
                self.coordination
                    .decisions
                    .iter()
                    .find(|decision| &decision.decision_id == decision_id)
                    .map(|decision| decision.owner.as_str())
            })
            .collect::<BTreeSet<_>>();
        if reviewer_ids
            .iter()
            .chain(verifier_ids.iter())
            .any(|worker| worker == &owner || candidate_owners.contains(worker.as_str()))
        {
            return Err(
                "Reviewer/Verifier evidence workers must be independent of the neutral owner and candidate authors"
                    .to_string(),
            );
        }
        self.coordination.reconcile(
            subject,
            owner,
            input_decisions,
            outcome,
            evidence_handles,
            candidate_handles,
            retry_count,
            retry_limit,
            reviewer_evidence_handles,
            verifier_evidence_handles,
            verification_outcome,
        )
    }

    fn validate_reconciliation_role_evidence(
        &self,
        handles: &[String],
        expected: FleetRole,
        label: &str,
    ) -> Result<BTreeSet<String>, String> {
        if handles.is_empty() {
            return Err(format!("neutral fan-in requires {label} evidence"));
        }
        let mut workers = BTreeSet::new();
        for handle in handles {
            let reference = handle
                .strip_prefix("agent:")
                .and_then(|rest| rest.split([':', '@', '#']).next())
                .unwrap_or(handle)
                .trim();
            let Some((worker_id, record)) = self.worker_record_by_ref(reference) else {
                return Err(format!(
                    "{label} evidence handle '{handle}' does not identify a persisted worker"
                ));
            };
            let role_matches = record.spec.agent_type == expected
                || record.spec.role.as_deref().is_some_and(|role| {
                    role.trim().eq_ignore_ascii_case(label)
                        || (expected == FleetRole::Reviewer
                            && role.trim().eq_ignore_ascii_case("reviewer"))
                        || (expected == FleetRole::Verifier
                            && role.trim().eq_ignore_ascii_case("verifier"))
                });
            if !role_matches || record.status != AgentWorkerStatus::Completed {
                return Err(format!(
                    "{label} evidence worker '{worker_id}' must have the {label} role and completed status"
                ));
            }
            workers.insert(worker_id);
        }
        Ok(workers)
    }

    fn nearest_common_fan_in_owner(&self, input_decisions: &[String]) -> Result<String, String> {
        let decision_owners = input_decisions
            .iter()
            .map(|decision_id| {
                self.coordination
                    .decisions
                    .iter()
                    .find(|decision| &decision.decision_id == decision_id)
                    .map(|decision| decision.owner.clone())
                    .ok_or_else(|| {
                        format!("reconciliation references unknown decision '{decision_id}'")
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if decision_owners.len() < 2 {
            return Err("neutral fan-in requires at least two input decisions".to_string());
        }

        let ancestry = decision_owners
            .iter()
            .map(|owner| self.worker_ancestry(owner))
            .collect::<Vec<_>>();
        let Some(first) = ancestry.first() else {
            return Ok("root".to_string());
        };
        for candidate in first {
            if decision_owners.contains(candidate)
                || !ancestry
                    .iter()
                    .skip(1)
                    .all(|chain| chain.contains(candidate))
            {
                continue;
            }
            if candidate == "root" || self.worker_is_fan_in_owner(candidate) {
                return Ok(candidate.clone());
            }
        }
        Ok("root".to_string())
    }

    fn worker_ancestry(&self, owner: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut cursor = Some(owner.to_string());
        while let Some(reference) = cursor.take() {
            let Some((worker_id, record)) = self.worker_record_by_ref(&reference) else {
                break;
            };
            if chain.contains(&worker_id) {
                break;
            }
            chain.push(worker_id);
            cursor = record.parent_run_id.clone();
        }
        if !chain.iter().any(|entry| entry == "root") {
            chain.push("root".to_string());
        }
        chain
    }

    fn worker_record_by_ref(&self, reference: &str) -> Option<(String, &AgentWorkerRecord)> {
        self.worker_records
            .get(reference)
            .map(|record| (reference.to_string(), record))
            .or_else(|| {
                self.worker_records.iter().find_map(|(worker_id, record)| {
                    (record.spec.run_id == reference).then(|| (worker_id.clone(), record))
                })
            })
    }

    fn worker_is_fan_in_owner(&self, reference: &str) -> bool {
        self.worker_record_by_ref(reference)
            .is_some_and(|(_, record)| {
                record.spec.agent_type == FleetRole::Planner
                    || record.spec.role.as_deref().is_some_and(|role| {
                        matches!(
                            role.trim().to_ascii_lowercase().as_str(),
                            "planner" | "manager" | "operator" | "release-owner"
                        )
                    })
            })
    }

    #[must_use]
    pub fn coordination_detail_projection(
        &self,
        subject: Option<&str>,
        limit: usize,
    ) -> CoordinationDetailProjection {
        let limit = limit.clamp(1, coord::COORDINATION_RECORD_LIMIT);
        let matches_subject = |value: &str| subject.is_none_or(|subject| value == subject);
        let decisions = self
            .coordination
            .decisions
            .iter()
            .rev()
            .filter(|decision| matches_subject(&decision.subject))
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let reconciliations = self
            .coordination
            .reconciliations
            .iter()
            .rev()
            .filter(|receipt| matches_subject(&receipt.subject))
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let claims = self
            .coordination
            .write_claims
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let projections = self
            .coordination
            .projections
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let contentions = self
            .coordination
            .contentions
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        let active_owners = self.active_coordination_owners();
        let mut hot_path_counts = std::collections::BTreeMap::<String, usize>::new();
        for record in self
            .coordination
            .write_claims
            .iter()
            .filter(|record| active_owners.contains(&record.claim.owner))
        {
            for root in &record.claim.roots {
                *hot_path_counts.entry(root.clone()).or_default() += 1;
            }
            for file in &record.claim.exact_files {
                *hot_path_counts.entry(file.clone()).or_default() += 1;
            }
        }
        let mut hottest_paths = hot_path_counts.into_iter().collect::<Vec<_>>();
        hottest_paths.sort_by(|(path_a, count_a), (path_b, count_b)| {
            count_b.cmp(count_a).then_with(|| path_a.cmp(path_b))
        });
        hottest_paths.truncate(limit.min(8));
        CoordinationDetailProjection {
            schema_version: self.coordination.schema_version,
            sequence: self.coordination.sequence,
            decisions,
            write_claims: claims,
            reconciliations,
            context_projections: projections,
            contentions,
            metrics: CoordinationDetailMetrics {
                hottest_paths: hottest_paths
                    .into_iter()
                    .map(|(path, active_claims)| CoordinationHotPath {
                        path,
                        active_claims,
                    })
                    .collect(),
                package_or_module_growth: None,
                route_or_cost: None,
                note: "growth and route/cost stay null when the coordination ledger has no authoritative source".to_string(),
            },
            bounded: true,
            limit,
            process_lock_held: {
                // Retry acquire on each projection so a session that lost the
                // flock at construction recovers once the previous owner exits.
                let _ = self.coordination_process_lock_status();
                self.holds_coordination_process_lock()
            },
            process_lock_note: if self.holds_coordination_process_lock() {
                None
            } else {
                self.coordination_process_lock_status().err()
            },
        }
    }

    #[must_use]
    pub(crate) fn coordination_detail_projection_for_session(
        &self,
        active_session_id: &str,
        subject: Option<&str>,
        limit: usize,
    ) -> CoordinationDetailProjection {
        let mut projection = self.coordination_detail_projection(subject, limit);
        let record_matches = |sequence: u64, owner: Option<&str>| {
            self.coordination_record_is_owned_by_session(active_session_id, sequence, owner)
        };

        projection
            .decisions
            .retain(|record| record_matches(record.sequence, Some(&record.owner)));
        let visible_decision_ids = projection
            .decisions
            .iter()
            .map(|record| record.decision_id.clone())
            .collect::<HashSet<_>>();
        projection
            .write_claims
            .retain(|record| record_matches(record.sequence, Some(record.claim.owner.as_str())));
        projection.reconciliations.retain(|record| {
            record_matches(record.sequence, Some(&record.owner))
                && record
                    .input_decisions
                    .iter()
                    .all(|decision_id| visible_decision_ids.contains(decision_id))
        });
        projection.context_projections.retain_mut(|record| {
            if !record_matches(record.sequence, Some(&record.child_id)) {
                return false;
            }
            record
                .decision_ids
                .retain(|decision_id| visible_decision_ids.contains(decision_id));
            true
        });
        projection.contentions.retain(|record| {
            record_matches(record.sequence, Some(&record.claimant))
                && self.agent_id_is_owned_by_session(&record.conflicting_owner, active_session_id)
        });

        let mut hot_path_counts = std::collections::BTreeMap::<String, usize>::new();
        for record in &projection.write_claims {
            for root in &record.claim.roots {
                *hot_path_counts.entry(root.clone()).or_default() += 1;
            }
            for file in &record.claim.exact_files {
                *hot_path_counts.entry(file.clone()).or_default() += 1;
            }
        }
        let mut hottest_paths = hot_path_counts.into_iter().collect::<Vec<_>>();
        hottest_paths.sort_by(|(path_a, count_a), (path_b, count_b)| {
            count_b.cmp(count_a).then_with(|| path_a.cmp(path_b))
        });
        hottest_paths.truncate(projection.limit.min(8));
        projection.metrics.hottest_paths = hottest_paths
            .into_iter()
            .map(|(path, active_claims)| CoordinationHotPath {
                path,
                active_claims,
            })
            .collect();
        projection.sequence = projection
            .decisions
            .iter()
            .map(|record| record.sequence)
            .chain(projection.write_claims.iter().map(|record| record.sequence))
            .chain(
                projection
                    .reconciliations
                    .iter()
                    .map(|record| record.sequence),
            )
            .chain(
                projection
                    .context_projections
                    .iter()
                    .map(|record| record.sequence),
            )
            .chain(projection.contentions.iter().map(|record| record.sequence))
            .max()
            .unwrap_or_default();
        projection
    }

    #[must_use]
    #[cfg(test)]
    pub fn inspect_coordination(&self, subject: Option<&str>, limit: usize) -> Value {
        serde_json::to_value(self.coordination_detail_projection(subject, limit))
            .expect("typed coordination projection is serializable")
    }

    #[must_use]
    pub(crate) fn inspect_coordination_for_session(
        &self,
        active_session_id: &str,
        subject: Option<&str>,
        limit: usize,
    ) -> Value {
        serde_json::to_value(self.coordination_detail_projection_for_session(
            active_session_id,
            subject,
            limit,
        ))
        .expect("typed coordination projection is serializable")
    }

    pub fn expand_write_claim(
        &mut self,
        owner: &str,
        roots: Vec<String>,
        exact_files: Vec<String>,
        contracts: Vec<String>,
    ) -> Result<PersistedWriteClaim, String> {
        self.ensure_coordination_process_lock()?;
        let Some(existing) = self
            .coordination
            .write_claims
            .iter()
            .find(|record| record.claim.owner == owner)
            .cloned()
        else {
            return Err(format!("agent '{owner}' has no write claim to expand"));
        };
        self.validate_coordination_claim_root(owner, &existing)?;
        let mut claim = existing.claim;
        let (roots, exact_files) = self.namespace_claim_paths_for_owner(
            owner,
            existing.isolated_worktree,
            roots,
            exact_files,
        )?;
        for root in roots {
            let root = normalize_claim_path(&root)?;
            if !claim.roots.contains(&root) {
                claim.roots.push(root);
            }
        }
        for file in exact_files {
            let file = normalize_claim_path(&file)?;
            if !claim.exact_files.contains(&file) {
                claim.exact_files.push(file);
            }
        }
        for contract in contracts
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            if !claim.contracts.contains(&contract) {
                claim.contracts.push(contract);
            }
        }
        let origin = self
            .worker_origins
            .get(owner)
            .cloned()
            .unwrap_or_else(|| WorkerOrigin::legacy(String::new()));
        let execution_workspace = self
            .worker_records
            .get(owner)
            .map(|record| record.spec.workspace.as_path())
            .or_else(|| {
                self.agents
                    .get(owner)
                    .map(|agent| agent.workspace.as_path())
            })
            .unwrap_or(self.workspace.as_path());
        let workspace = self.coordination_workspace_in_origin(
            &origin,
            execution_workspace,
            existing.isolated_worktree,
        )?;
        self.register_claim_with_presence(claim, existing.isolated_worktree, &workspace)
    }

    /// Release write claims whose owner is no longer a live claimant.
    ///
    /// The operator-of-record path for #5562: `coordinate inspect` shows the
    /// standing claims, and `coordinate release` clears the stale ones after
    /// the manager agrees no live claimant is being touched. An optional owner
    /// restricts the sweep to one agent; live claims are never removed.
    pub(crate) fn release_stale_write_claims(
        &mut self,
        owner: Option<String>,
    ) -> Result<Vec<String>, String> {
        self.ensure_coordination_process_lock()?;
        let active = self.active_coordination_owners();
        let released = self
            .coordination
            .release_stale_claims(owner.as_deref(), |candidate| active.contains(candidate))?;
        self.persist_state_synchronously()
            .map_err(|error| error.to_string())?;
        Ok(released)
    }

    fn validate_coordination_claim_root(
        &self,
        owner: &str,
        claim: &PersistedWriteClaim,
    ) -> Result<(), String> {
        self.origin_workspace_for_owner(owner)?;
        if !claim.isolated_worktree {
            let workspace = self
                .worker_records
                .get(owner)
                .map(|record| record.spec.workspace.as_path())
                .or_else(|| {
                    self.agents
                        .get(owner)
                        .map(|agent| agent.workspace.as_path())
                })
                .unwrap_or(self.workspace.as_path());
            let origin = self
                .worker_origins
                .get(owner)
                .cloned()
                .unwrap_or_else(|| WorkerOrigin::legacy(String::new()));
            let workspace = self.coordination_workspace_in_origin(&origin, workspace, false)?;
            let expected = self.coordination_scope_for_workspace(&workspace)?;
            if self.coordination.claim_scope(claim.sequence) != expected.as_ref() {
                return Err("write claim root does not match its originating worker".into());
            }
            self.claim_root(claim)?;
            if let Some(root) = self.admitted_coordination_root(&workspace) {
                root.validate()?;
            }
        }
        Ok(())
    }

    fn validate_write_scope(&self, owner: &str, paths: &[String]) -> Result<(), String> {
        let Some(claim) = self
            .coordination
            .write_claims
            .iter()
            .find(|record| record.claim.owner == owner)
        else {
            return Err(format!(
                "agent '{owner}' has no registered write claim; declare scope at launch before mutation"
            ));
        };
        self.validate_coordination_claim_root(owner, claim)?;
        let (_, paths) = self.namespace_claim_paths_for_owner(
            owner,
            claim.isolated_worktree,
            Vec::new(),
            paths.to_vec(),
        )?;
        if let Some(path) = paths.iter().find(|path| !claim.claim.contains_path(path)) {
            return Err(format!(
                "write '{path}' is outside agent '{owner}' scope (roots: {:?}, files: {:?}); expand it first with agent action=claim",
                claim.claim.roots, claim.claim.exact_files
            ));
        }
        Ok(())
    }

    fn shared_write_claim(&self, owner: &str) -> Option<&PersistedWriteClaim> {
        self.coordination
            .write_claims
            .iter()
            .find(|record| record.claim.owner == owner && !record.isolated_worktree)
    }

    /// The live peers whose shared-checkout write claims gate `owner` — the
    /// names the peer gate must surface so a refused child knows
    /// what it is waiting on and what to cancel.
    ///
    /// Liveness is the load-bearing part. Claims outlive the agents that
    /// registered them, so a workspace accumulates one per builder that ever
    /// ran: six completed agents left four standing claims in testing. Counting
    /// those made every later builder look contended by children that had long
    /// since finished, and the contention only ever grew. A terminal owner
    /// cannot write anything, so its claim cannot contend.
    ///
    /// The same single predicate as claim admission (#5562): an agent restored
    /// `Running` from a prior session (crash leftover) and an owner whose agent
    /// record is not loaded are both non-live.
    ///
    /// Worktree-isolated peers are excluded too: they write into their own
    /// checkout and can never contend for these paths.
    fn live_peer_shared_write_claim_owners(&self, owner: &str) -> Vec<String> {
        self.coordination
            .write_claims
            .iter()
            .filter(|record| record.claim.owner != owner && !record.isolated_worktree)
            .filter(|record| self.is_live_coordination_owner(&record.claim.owner))
            // A claim whose every named path is gone cannot describe a peer
            // still writing here, so it must not gate command execution
            // either (#5906) — the reported incident lost a whole session's
            // shell to a claim on a deleted worktree.
            .filter(|record| !self.claim_names_only_missing_paths(record))
            .map(|record| record.claim.owner.clone())
            .collect()
    }

    /// Classify an agent by its `session_boot_id`: `true` when the
    /// agent was either (a) loaded from disk with no id, or (b) carries
    /// a different id than the manager's current boot. Filters
    /// listing output by default (#405).
    fn is_from_prior_session(&self, agent: &SubAgent) -> bool {
        agent.session_boot_id.is_empty() || agent.session_boot_id != self.current_session_boot_id
    }

    #[must_use]
    fn with_state_path(mut self, path: PathBuf) -> Self {
        self.state_path = Some(path);
        self
    }

    fn require_coordination_process_lock(mut self) -> Self {
        self.coordination_process_lock_required = true;
        *self
            .coordination_process_lock
            .get_mut()
            .expect("coordination lock slot poisoned") =
            CoordinationProcessLock::acquire(&self.state_root, self.coordination_exclusive)
                .ok()
                .map(Arc::new);
        self
    }

    /// A failed acquisition is never terminal: the previous owner may have
    /// exited since the last attempt, and the flock is held on an open fd —
    /// deleting the lock file does not release it, so the only correct
    /// recovery is to re-attempt acquisition on each use (#5036).
    fn ensure_coordination_process_lock(&self) -> Result<(), String> {
        if !self.coordination_process_lock_required {
            return Ok(());
        }
        let mut slot = self
            .coordination_process_lock
            .lock()
            .expect("coordination lock slot poisoned");
        if slot.is_some() {
            return Ok(());
        }
        match CoordinationProcessLock::acquire(&self.state_root, self.coordination_exclusive) {
            Ok(lock) => {
                *slot = Some(Arc::new(lock));
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// Whether this process currently owns the workspace coordination flock.
    /// Read-only inspection of the in-process slot — does not attempt acquire.
    #[must_use]
    pub fn holds_coordination_process_lock(&self) -> bool {
        if !self.coordination_process_lock_required {
            // Tests and non-durable managers do not require the flock.
            return true;
        }
        self.coordination_process_lock
            .lock()
            .expect("coordination lock slot poisoned")
            .is_some()
    }

    /// Probe lock ownership for UI/inspect surfaces. Retries acquire when the
    /// slot is empty so a session that started without the lock can recover
    /// after the previous owner exits (#2.6 / #5036).
    pub fn coordination_process_lock_status(&self) -> Result<(), String> {
        self.ensure_coordination_process_lock()
    }

    #[must_use]
    pub fn with_running_heartbeat_timeout(mut self, timeout: Duration) -> Self {
        self.running_heartbeat_timeout = if timeout.is_zero() {
            Duration::from_secs(crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS)
        } else {
            timeout
        };
        self
    }

    /// Apply live runtime limits. The launch gate is a
    /// [`governor::DynamicGate`], so the new launch concurrency applies to
    /// the live capacity immediately — children already holding permits keep
    /// running, and no admission above the new capacity is granted until the
    /// active count drains. Always returns `true`.
    pub fn update_runtime_limits(
        &mut self,
        max_agents: usize,
        max_admitted_agents: usize,
        running_heartbeat_timeout: Duration,
        launch_concurrency: usize,
    ) -> bool {
        self.max_agents = max_agents.clamp(1, crate::config::MAX_SUBAGENTS);
        self.max_admitted_agents =
            max_admitted_agents.clamp(self.max_agents, crate::config::MAX_SUBAGENT_ADMISSION);
        self.running_heartbeat_timeout = if running_heartbeat_timeout.is_zero() {
            Duration::from_secs(crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS)
        } else {
            running_heartbeat_timeout
        };
        let launch_concurrency = launch_concurrency.clamp(1, self.max_agents);
        // Routed through the governor so a rate-limit pause (gate capacity 0)
        // is not silently lifted by a runtime limit change.
        self.governor.set_max_capacity(launch_concurrency);
        true
    }

    /// Build the [`PersistedSubAgentState`] snapshot from the current fleet.
    ///
    /// This is a cheap clone operation that runs under the caller's lock.
    /// The returned payload is fully owned and safe to move to a background
    /// thread for disk I/O.
    fn build_persist_payload(&self) -> Result<Option<(PathBuf, PersistedSubAgentState)>> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(None);
        };
        let path = checked_subagent_state_path(&self.state_root, path)?;
        let now_ms = epoch_millis_now();
        let mut agents = Vec::with_capacity(self.agents.len());
        for agent in self.agents.values() {
            agents.push(PersistedSubAgent {
                id: agent.id.clone(),
                session_name: Some(agent.session_name.clone()),
                fork_context: agent.fork_context,
                workspace: Some(agent.workspace.clone()),
                agent_type: agent.agent_type.clone(),
                prompt: agent.prompt.clone(),
                assignment: agent.assignment.clone(),
                model: agent.model.clone(),
                // Generated whale names are locale-derived presentation, not
                // durable identity. Persist only an explicit custom nickname;
                // legacy generated values are discarded again on load.
                nickname: agent
                    .nickname
                    .clone()
                    .filter(|name| generated_whale_name_base(&agent.id, name).is_none()),
                status: agent.status.clone(),
                result: agent.result.clone(),
                steps_taken: agent.steps_taken,
                checkpoint: agent.checkpoint.clone(),
                needs_input: agent.needs_input.clone(),
                duration_ms: u64::try_from(agent.started_at.elapsed().as_millis())
                    .unwrap_or(u64::MAX),
                // Backward-compat: Vec on disk. None → empty vec; Some(list) → list.
                // Reload converts empty vec back to None (full inheritance).
                allowed_tools: agent.allowed_tools.clone().unwrap_or_default(),
                updated_at_ms: now_ms,
                session_boot_id: agent.session_boot_id.clone(),
                owner_session_id: agent.owner_session_id.clone(),
            });
        }
        agents.sort_by(|a, b| a.id.cmp(&b.id));

        let payload = PersistedSubAgentState {
            schema_version: self.worker_origin_schema,
            snapshot_sequence: self
                .persist_sequence
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .saturating_add(1),
            agents,
            workers: self.sorted_worker_records(),
            coordination: self.coordination.clone(),
            resume_targets: self.resume_targets.clone(),
            worker_origins: (self.worker_origin_schema == 2).then(|| self.worker_origins.clone()),
        };
        payload
            .validate_worker_origins()
            .map_err(anyhow::Error::msg)?;
        Ok(Some((path, payload)))
    }

    /// Persist the current fleet state to disk.
    ///
    /// #freeze: JSON serialization runs cheaply under the caller's lock; the
    /// expensive disk I/O (`write_json_atomic`) is spawned onto a background
    /// thread so the caller's write lock is released before touching the
    /// filesystem.
    ///
    /// Returns a [`std::thread::JoinHandle`] that resolves when the disk write
    /// completes.  Callers may `.join()` for synchronous semantics or drop it
    /// for fire-and-forget.
    fn persist_state(&self) -> Result<std::thread::JoinHandle<()>> {
        self.ensure_coordination_process_lock()
            .map_err(anyhow::Error::msg)?;
        let Some((path, payload)) = self.build_persist_payload()? else {
            // Nothing to persist — return a no-op handle.
            return Ok(std::thread::spawn(|| {}));
        };
        let state_root = self.state_root.clone();
        let writer_locks = self
            .coordination_process_lock
            .lock()
            .expect("coordination lock slot poisoned")
            .iter()
            .cloned()
            .chain(self.workspace_writer_locks.values().cloned())
            .collect::<Vec<_>>();
        // Spawn disk I/O off the write-lock hot path.  `payload` is fully
        // owned (cloned from `self.agents`) so it is `Send` and safe to move.
        let handle = std::thread::spawn(move || {
            let _writer_locks = writer_locks;
            if let Err(err) = write_json_atomic(&state_root, &path, &payload) {
                tracing::warn!(target: "subagent", ?err, "failed to persist sub-agent state");
            }
        });
        Ok(handle)
    }

    fn persist_state_synchronously(&self) -> Result<()> {
        self.ensure_coordination_process_lock()
            .map_err(anyhow::Error::msg)?;
        let Some((path, payload)) = self.build_persist_payload()? else {
            return Ok(());
        };
        write_json_atomic(&self.state_root, &path, &payload)
    }

    /// Fire-and-forget persist — logs errors, drops the join handle.
    fn persist_state_best_effort(&self) {
        if let Err(err) = self.persist_state() {
            // Must not be `eprintln!` — raw stderr inside the alt-screen
            // leaks into the buffer and produces the scroll-demon
            // regression (#1085). Routed through tracing so the
            // file-backed subscriber in `runtime_log` captures it.
            tracing::warn!(target: "subagent", ?err, "failed to persist sub-agent state");
        } else {
            // Join handle is dropped here — disk I/O proceeds in background.
        }
    }

    /// #freeze: persist on the hot per-step checkpoint path, coalesced to at
    /// most one disk write per `SUBAGENT_PERSIST_DEBOUNCE`. A skipped write
    /// sets `persist_pending` so the next terminal persist (which always
    /// rewrites the full fleet) or `flush_pending_persist` captures it.
    fn persist_state_debounced(&mut self) {
        let now = Instant::now();
        let due = match self.last_persist_at {
            Some(last) => now.duration_since(last) >= SUBAGENT_PERSIST_DEBOUNCE,
            None => true,
        };
        if due {
            self.last_persist_at = Some(now);
            self.persist_pending = false;
            self.persist_state_best_effort();
            let writes =
                SUBAGENT_PERSIST_WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if subagent_perf_enabled() {
                let skipped = SUBAGENT_PERSIST_SKIPPED.load(std::sync::atomic::Ordering::Relaxed);
                tracing::info!(
                    target: "subagent_perf",
                    writes,
                    skipped,
                    agents = self.agents.len(),
                    "checkpoint persist (debounced write)"
                );
            }
        } else {
            self.persist_pending = true;
            SUBAGENT_PERSIST_SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// #freeze: force a persist if a hot-path write was previously coalesced
    /// away. Call on graceful shutdown / session teardown so the most recent
    /// intermediate checkpoint is not lost.
    ///
    /// Unlike `persist_state`, this performs disk I/O **synchronously** to
    /// guarantee data is flushed before the process exits.
    pub fn flush_pending_persist(&mut self) {
        if let Err(error) = self.ensure_coordination_process_lock() {
            tracing::warn!(target: "subagent", %error, "skipping persist without state-root coordination lock");
            return;
        }
        if self.persist_pending {
            self.last_persist_at = Some(Instant::now());
            self.persist_pending = false;
            // Synchronous disk I/O — safe because we are shutting down and no
            // callers depend on releasing the write lock quickly.
            if let Ok(Some((path, payload))) = self.build_persist_payload()
                && let Err(err) = write_json_atomic(&self.state_root, &path, &payload)
            {
                tracing::warn!(target: "subagent", ?err, "failed to flush pending sub-agent state");
            }
        }
    }

    fn load_state(&mut self) -> Result<()> {
        let Some(path) = self.state_path.as_ref() else {
            return Ok(());
        };
        let path = checked_subagent_state_path(&self.state_root, path)?;

        // If canonical path doesn't exist, try legacy .deepseek/ path for one-time
        // migration. The next persist will write to the canonical .codewhale/ path.
        let path = if path.exists() {
            path
        } else {
            let legacy = checked_subagent_state_path(
                &self.state_root,
                &Path::new(".deepseek")
                    .join("state")
                    .join(SUBAGENT_STATE_FILE),
            )?;
            if legacy.exists() {
                tracing::info!(
                    target: "subagent",
                    "loading sub-agent state from legacy path for migration: {}",
                    legacy.display()
                );
                legacy
            } else {
                return Ok(());
            }
        };

        let raw = read_subagent_state_file(&self.state_root, &path)?;
        let state = serde_json::from_str::<PersistedSubAgentState>(&raw)?;
        state
            .validate_worker_origins()
            .map_err(anyhow::Error::msg)?;
        let origins = state.worker_origins.clone().unwrap_or_else(|| {
            state
                .workers
                .iter()
                .map(|worker| {
                    (
                        worker.spec.worker_id.clone(),
                        WorkerOrigin::legacy(worker.owner_session_id.clone()),
                    )
                })
                .collect()
        });

        let mut coordination = state.coordination;
        coordination
            .validate_replay()
            .map_err(|error| anyhow!("Invalid coordination ledger: {error}"))?;
        self.agents.clear();
        self.worker_records.clear();
        self.worker_origins = origins;
        self.worker_origin_schema = state.schema_version;
        self.persist_sequence.store(
            state.snapshot_sequence,
            std::sync::atomic::Ordering::Relaxed,
        );
        self.coordination = coordination;
        self.resume_targets = state.resume_targets;
        for persisted in state.agents {
            let nickname = persisted
                .nickname
                .filter(|name| generated_whale_name_base(&persisted.id, name).is_none());
            let mut status = persisted.status;
            if matches!(status, SubAgentStatus::Running) {
                status = SubAgentStatus::Interrupted(SUBAGENT_RESTART_REASON.to_string());
            }

            let started_at = instant_from_duration(Duration::from_millis(persisted.duration_ms));
            // Empty vec on disk → None (full inheritance, v0.6.6 default).
            // Non-empty vec → Some(list) (preserves narrow scope from older sessions).
            let allowed_tools = if persisted.allowed_tools.is_empty() {
                None
            } else {
                Some(persisted.allowed_tools)
            };
            let mut assignment = persisted.assignment;
            canonicalize_persisted_advisory_role(&mut assignment.role);
            let agent = SubAgent {
                id: persisted.id.clone(),
                session_name: persisted
                    .session_name
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| persisted.id.clone()),
                fork_context: persisted.fork_context,
                workspace: persisted
                    .workspace
                    .unwrap_or_else(|| self.workspace.clone()),
                agent_type: persisted.agent_type,
                prompt: persisted.prompt,
                assignment,
                model: if persisted.model.is_empty() {
                    "unknown".to_string()
                } else {
                    persisted.model
                },
                // v0.8.68 and earlier persisted generated whale text. It may
                // have been chosen under a different UI language, so never
                // replay it into a new session. Explicit custom names survive.
                nickname,
                status,
                result: persisted.result,
                steps_taken: persisted.steps_taken,
                checkpoint: persisted.checkpoint,
                needs_input: persisted.needs_input,
                started_at,
                last_activity_at: started_at,
                heartbeat_timeout: None,
                allowed_tools,
                // Empty string when loading pre-#405 records; the
                // manager treats that the same as a non-matching id —
                // i.e. agent classified as prior-session.
                session_boot_id: persisted.session_boot_id,
                owner_session_id: persisted.owner_session_id,
                completion_claimed: false,
                terminal_delivery: None,
                work_lifecycle: None,
                input_tx: None,
                task_handle: None,
                stop_token: None,
                requested_stop: None,
                unsettled_launch: None,
                unsettled_foreground: None,
            };
            self.agents.insert(persisted.id, agent);
        }
        for worker in state.workers {
            let worker = normalize_worker_record(worker);
            self.worker_event_seq = self.worker_event_seq.max(
                worker
                    .events
                    .iter()
                    .map(|event| event.seq)
                    .max()
                    .unwrap_or(0),
            );
            self.worker_records
                .insert(worker.spec.worker_id.clone(), worker);
        }
        self.reconcile_orphaned_workers_after_restart();
        self.prune_worker_records();

        Ok(())
    }

    /// No in-process task survives a manager restart. Reconcile every worker
    /// status that requires a live executor to `Interrupted`, matching the
    /// existing top-level agent restoration above. Terminal receipts and
    /// waiting-for-user records remain unchanged, and the status guard makes
    /// repeated reconciliation idempotent (#4408).
    fn reconcile_orphaned_workers_after_restart(&mut self) -> usize {
        let orphaned = self
            .worker_records
            .values()
            .filter(|record| {
                matches!(
                    record.status,
                    AgentWorkerStatus::Queued
                        | AgentWorkerStatus::Starting
                        | AgentWorkerStatus::Running
                        | AgentWorkerStatus::ModelWait
                        | AgentWorkerStatus::RunningTool
                )
            })
            .map(|record| (record.spec.worker_id.clone(), record.steps_taken))
            .collect::<Vec<_>>();
        for (worker_id, steps_taken) in &orphaned {
            self.record_worker_event(
                worker_id,
                AgentWorkerStatus::Interrupted,
                Some(SUBAGENT_RESTART_REASON.to_string()),
                Some(*steps_taken),
                None,
            );
        }
        orphaned.len()
    }

    /// Finalize the live fleet when the parent session closes without a
    /// process restart (#5372). A session switch in the same process keeps
    /// the manager (and its coordination ledger) alive, so running children
    /// and their write claims would otherwise survive into the next
    /// conversation and keep blocking new writers.
    ///
    /// Loaded prior-session agents are already `Interrupted` and loaded
    /// workers are already terminal (reconciled on `load_state`), so only the
    /// currently-live fleet is finalized here. Write claims owned by absent
    /// or prior-session owners stay put — a sibling session's live claim must
    /// never be released by this process.
    #[cfg(test)]
    pub fn finalize_session_close(&mut self) -> usize {
        self.finalize_session_close_inner(None)
    }

    pub(crate) fn finalize_session_close_for_session(&mut self, active_session_id: &str) -> usize {
        self.finalize_session_close_inner(Some(active_session_id))
    }

    fn finalize_session_close_inner(&mut self, active_session_id: Option<&str>) -> usize {
        let reason = SUBAGENT_SESSION_CLOSED_REASON;
        let running_agent_ids = self
            .agents
            .values()
            .filter(|agent| agent.status == SubAgentStatus::Running)
            .filter(|agent| {
                active_session_id
                    .is_none_or(|session_id| self.agent_is_owned_by_session(agent, session_id))
            })
            .map(|agent| agent.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut finalized = 0usize;
        for agent_id in &running_agent_ids {
            let Some(snapshot) = self.agents.get(agent_id).map(SubAgent::snapshot) else {
                continue;
            };
            let mut terminal = snapshot;
            terminal.status = SubAgentStatus::Interrupted(reason.to_string());
            terminal.result = Some(reason.to_string());
            terminal.needs_input = None;
            if self.finish_terminal_result(agent_id, terminal, true, true) {
                finalized += 1;
            }
        }
        let draining = self
            .agents
            .values()
            .filter(|agent| agent.status == SubAgentStatus::Running && agent.stop_token.is_some())
            .map(|agent| agent.id.clone())
            .collect::<std::collections::HashSet<_>>();
        // Headless workers keep no agent entry, and a waiting-for-user worker
        // may survive its agent's transition above, so reconcile every live
        // (non-terminal) worker record directly.
        let live_worker_ids = self
            .worker_records
            .values()
            .filter(|record| !record.status.is_terminal())
            .filter(|record| !draining.contains(&record.spec.worker_id))
            .filter(|record| {
                active_session_id.is_none_or(|session_id| {
                    !session_id.is_empty() && record.owner_session_id == session_id
                })
            })
            .map(|record| (record.spec.worker_id.clone(), record.steps_taken))
            .collect::<Vec<_>>();
        for (worker_id, steps_taken) in &live_worker_ids {
            self.record_worker_event(
                worker_id,
                AgentWorkerStatus::Interrupted,
                Some(reason.to_string()),
                Some(*steps_taken),
                None,
            );
            finalized += 1;
        }
        // Release write claims owned by the fleet being finalized. Claims from
        // absent owners (sibling sessions) and prior-session records stay put.
        let finalized_owners = running_agent_ids
            .into_iter()
            .filter(|id| !draining.contains(id))
            .chain(live_worker_ids.into_iter().map(|(id, _)| id))
            .collect::<std::collections::HashSet<_>>();
        let mut released = 0usize;
        self.coordination.write_claims.retain(|record| {
            if finalized_owners.contains(&record.claim.owner) {
                released += 1;
                false
            } else {
                true
            }
        });
        self.coordination.prune_claim_scopes();
        // Drop finalized agents from the in-memory roster so they can never
        // be counted active again. Prior-session archive rows remain.
        self.agents.retain(|id, _| !finalized_owners.contains(id));
        let _ = self.persist_state_synchronously();
        tracing::debug!(
            target: "subagent",
            finalized,
            released,
            "finalized sub-agent fleet on session close"
        );
        finalized
    }

    fn sorted_worker_records(&self) -> Vec<AgentWorkerRecord> {
        let mut workers: Vec<_> = self.worker_records.values().cloned().collect();
        workers.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| a.spec.worker_id.cmp(&b.spec.worker_id))
        });
        workers
    }

    fn prune_worker_records(&mut self) {
        while self.worker_records.len() > MAX_AGENT_WORKER_RECORDS {
            let oldest_terminal = self
                .worker_records
                .values()
                .filter(|record| record.status.is_terminal())
                .min_by(|a, b| {
                    a.updated_at_ms
                        .cmp(&b.updated_at_ms)
                        .then_with(|| a.spec.worker_id.cmp(&b.spec.worker_id))
                })
                .map(|record| record.spec.worker_id.clone());
            let Some(worker_id) = oldest_terminal else {
                // Active launch identity is never a retention casualty. The
                // live population may temporarily exceed the history cap and
                // is compacted after terminal records become reclaimable.
                break;
            };
            self.worker_records.remove(&worker_id);
        }
        self.prune_worker_origins();
    }

    #[cfg(test)]
    pub fn register_worker(&mut self, spec: AgentWorkerSpec) {
        self.register_worker_for_session(spec, "", None);
    }

    fn register_worker_for_session(
        &mut self,
        spec: AgentWorkerSpec,
        owner_session_id: &str,
        precomputed_evidence: Option<DeliveryEvidence>,
    ) {
        self.register_worker_for_session_with_origin(
            spec,
            owner_session_id,
            precomputed_evidence,
            None,
        );
    }

    pub(crate) fn register_worker_for_session_with_origin(
        &mut self,
        spec: AgentWorkerSpec,
        owner_session_id: &str,
        precomputed_evidence: Option<DeliveryEvidence>,
        runtime_origin_turn_id: Option<String>,
    ) {
        let worker_id = spec.worker_id.clone();
        let now_ms = epoch_millis_now();
        let mut record = AgentWorkerRecord::new_for_session(
            normalize_worker_spec(spec),
            now_ms,
            owner_session_id.to_string(),
            precomputed_evidence,
        );
        record.runtime_origin_turn_id = runtime_origin_turn_id;
        self.push_worker_event(
            &mut record,
            AgentWorkerStatus::Starting,
            Some("starting".to_string()),
            None,
            None,
            now_ms,
        );
        self.worker_origins.insert(
            worker_id.clone(),
            WorkerOrigin::legacy(owner_session_id.into()),
        );
        self.worker_records.insert(worker_id, record);
        self.prune_worker_records();
    }

    /// Validate a Fleet/headless worker's persisted launch claim before its
    /// durable task lease is committed. A contention is still receipted in the
    /// live coordination ledger, but a successful preflight does not reserve
    /// anything until `register_worker_with_coordination` runs under the same
    /// manager write lock.
    #[cfg(test)]
    pub fn preflight_worker_coordination(&mut self, spec: &AgentWorkerSpec) -> Result<(), String> {
        let origin = self.capture_fleet_worker_origin(&spec.workspace)?;
        self.preflight_worker_coordination_in_origin(spec, &origin)
    }

    pub(crate) fn preflight_worker_coordination_in_origin(
        &mut self,
        spec: &AgentWorkerSpec,
        origin: &WorkerOrigin,
    ) -> Result<(), String> {
        self.validate_worker_origin(origin)?;
        self.check_worker_origin_capacity(Some(&spec.worker_id), origin)?;
        let origin_workspace = self.coordination_workspace_in_origin(
            origin,
            &spec.workspace,
            worker_coordination_claim(spec)?
                .as_ref()
                .is_some_and(|(_, isolated)| *isolated),
        )?;
        let Some((claim, isolated_worktree)) = worker_coordination_claim(spec)? else {
            return Ok(());
        };
        // Isolated-worktree builders mutate their own checkout, not the
        // shared workspace, so they must not contend for the per-workspace
        // coordination process lock (#5036).
        if !isolated_worktree {
            self.ensure_coordination_process_lock()?;
        }
        let claim = self.namespace_write_claim(&origin_workspace, isolated_worktree, claim)?;
        let scope = if isolated_worktree {
            None
        } else {
            self.coordination_scope_for_workspace(&origin_workspace)?
        };
        let active_owners = self.admissible_coordination_owners();
        let mut probe = self.coordination.clone();
        match probe.register_claim_in_scope(
            claim.clone(),
            isolated_worktree,
            scope,
            Some(self.original_coordination_root()),
            |owner| active_owners.contains(owner),
        ) {
            Ok(_) => Ok(()),
            Err(error) => {
                // Re-run against the authoritative ledger so a real overlap
                // remains visible after restart. Invalid/schema failures do
                // not mutate either ledger.
                let coordination_before = self.coordination.clone();
                let _ =
                    self.register_claim_with_presence(claim, isolated_worktree, &origin_workspace);
                if let Err(persist_error) = self.persist_state_synchronously() {
                    self.coordination = coordination_before;
                    return Err(format!(
                        "{error}; additionally failed to persist contention receipt: {persist_error}"
                    ));
                }
                Err(error)
            }
        }
    }

    /// Commit a preflighted Fleet/headless worker, its bounded write claim, and
    /// its minimal accepted-decision projection as one in-memory projection.
    /// Callers hold the manager write lock from preflight through this method,
    /// so the second validation cannot race another registration.
    #[cfg(test)]
    pub fn register_worker_with_coordination(
        &mut self,
        spec: AgentWorkerSpec,
    ) -> Result<(), String> {
        let origin = self.capture_fleet_worker_origin(&spec.workspace)?;
        self.register_worker_with_coordination_in_origin(spec, origin)
    }

    pub(crate) fn register_worker_with_coordination_in_origin(
        &mut self,
        mut spec: AgentWorkerSpec,
        origin: WorkerOrigin,
    ) -> Result<(), String> {
        self.validate_worker_origin(&origin)?;
        self.check_worker_origin_capacity(Some(&spec.worker_id), &origin)?;
        let origin_workspace = self.coordination_workspace_in_origin(
            &origin,
            &spec.workspace,
            worker_coordination_claim(&spec)?
                .as_ref()
                .is_some_and(|(_, isolated)| *isolated),
        )?;
        let previous_origins = self.worker_origins.clone();
        let previous_schema = self.worker_origin_schema;
        let claim = worker_coordination_claim(&spec)?;
        // Same isolation rule as `preflight_worker_coordination` (#5036).
        let isolated_worktree = claim
            .as_ref()
            .is_some_and(|(_, isolated_worktree)| *isolated_worktree);
        if !isolated_worktree {
            self.ensure_coordination_process_lock()?;
        }
        let previous_worker_records = self.worker_records.clone();
        let previous_coordination = self.coordination.clone();
        let persisted_claim = claim
            .map(|(claim, isolated_worktree)| {
                let claim =
                    self.namespace_write_claim(&origin_workspace, isolated_worktree, claim)?;
                self.register_claim_with_presence(claim, isolated_worktree, &origin_workspace)
            })
            .transpose()?;

        let mut capabilities = match &spec.tool_profile {
            AgentWorkerToolProfile::Inherited => Vec::new(),
            AgentWorkerToolProfile::Explicit(tools) => tools.clone(),
        };
        capabilities.push(spec.agent_type.as_str().to_string());
        if let Some(role) = spec.role.as_ref()
            && !capabilities.contains(role)
        {
            capabilities.push(role.clone());
        }
        let original_root = self.original_coordination_root().to_path_buf();
        let claim_root = persisted_claim
            .as_ref()
            .map(|claim| {
                if claim.isolated_worktree {
                    Ok(origin.scope.as_ref().map_or_else(
                        || original_root.clone(),
                        |scope| scope.canonical_root.clone(),
                    ))
                } else {
                    self.claim_root(claim).map(Path::to_path_buf)
                }
            })
            .transpose()?;
        let (projection, _) = self.coordination.project_relevant_decisions_in_scope(
            &spec.worker_id,
            persisted_claim.as_ref().map(|record| &record.claim),
            &capabilities,
            claim_root.as_deref(),
            Some(&original_root),
        );
        if !projection.is_empty() {
            spec.objective.push_str("\n\n");
            spec.objective.push_str(&projection);
            if let Some(manifest) = spec.launch_manifest.as_mut() {
                manifest.prompt = spec.objective.clone();
            }
        }
        let worker_id_for_log = spec.worker_id.clone();
        self.register_worker_for_session(spec, &origin.owner_session_id, None);
        self.install_worker_origin(&worker_id_for_log, origin);
        if isolated_worktree && self.ensure_coordination_process_lock().is_err() {
            // This process does not own the durable coordination state and an
            // isolated-worktree builder must not be blocked on it: register
            // in memory only and leave the durable ledger to the lock owner.
            // The persist-layer lock check stays intact (#5036).
            tracing::warn!(
                target: "subagent",
                worker = %worker_id_for_log,
                "registered isolated-worktree worker in memory only; delegated coordination lock is owned elsewhere"
            );
            return Ok(());
        }
        if let Err(error) = self.persist_state_synchronously() {
            self.worker_records = previous_worker_records;
            self.coordination = previous_coordination;
            self.worker_origins = previous_origins;
            self.worker_origin_schema = previous_schema;
            return Err(format!(
                "failed to persist Fleet coordination launch record: {error}"
            ));
        }
        Ok(())
    }

    pub(crate) fn coordination_registration_snapshot(&self) -> CoordinationRegistrationSnapshot {
        CoordinationRegistrationSnapshot {
            worker_records: self.worker_records.clone(),
            coordination: self.coordination.clone(),
            worker_origins: self.worker_origins.clone(),
            worker_origin_schema: self.worker_origin_schema,
        }
    }

    pub(crate) fn restore_coordination_registration_snapshot(
        &mut self,
        snapshot: CoordinationRegistrationSnapshot,
    ) -> Result<(), String> {
        self.worker_records = snapshot.worker_records;
        self.coordination = snapshot.coordination;
        self.worker_origins = snapshot.worker_origins;
        self.worker_origin_schema = snapshot.worker_origin_schema;
        self.persist_state_synchronously()
            .map_err(|error| format!("failed to persist Fleet coordination rollback: {error}"))
    }

    /// The single definition of a live coordination claimant.
    ///
    /// One predicate, evaluated per owner id by every caller: claim admission
    /// (`active_coordination_owners` delegates below), the shared-checkout peer
    /// gate (`live_peer_shared_write_claim_owners`), and stale-claim release.
    /// The admission and gate sides used to disagree, which let a claim from a
    /// crashed prior session (an agent record restored as `Running` with a
    /// different boot id, or an owner whose record was never loaded at all)
    /// keep looking like a live writer forever — every later builder/worker
    /// was then denied all command execution (issue #5562: "stale write-claims
    /// persist forever and cascade-lock other agents"). A claim cannot contend
    /// for an owner that cannot write.
    fn is_live_coordination_owner(&self, id: &str) -> bool {
        self.agents.get(id).is_some_and(|agent| {
            agent.status == SubAgentStatus::Running && !self.is_from_prior_session(agent)
        }) || self.worker_records.get(id).is_some_and(|record| {
            !record.status.is_terminal()
                // A headless worker (no paired agent entry) that reaches
                // WaitingForUser can never be answered: no user path will
                // resume or finalize it, so counting it live leaks its claim
                // as a permanent gate on every later writer. A *paired*
                // waiting child keeps its claim — the user can still answer
                // and the child will write again.
                && !(record.status == AgentWorkerStatus::WaitingForUser
                    && !self.agents.contains_key(id))
                // A child parked at its parent's turn end is not a child that
                // asked a question (#5906). Nothing will answer it: the
                // recovery path is `followup`, whose continuation registers
                // its own claim. Counting the parked record
                // live left its claim standing forever — refusing the resume
                // itself, every later write-capable spawn overlapping the
                // scope, and (through the shared-checkout peer gate) all
                // command execution for those peers.
                && !record.parked_at_turn_end
                && !self
                    .agents
                    .get(id)
                    .is_some_and(|agent| self.is_from_prior_session(agent))
        })
    }

    /// Resolve a normalized claim path against this manager's coordination
    /// root. The ledger owns no workspace, so it asks through this.
    fn coordination_claim_path_exists(workspace: &Path, path: &str) -> bool {
        // `.` is the coordination root itself and is always present.
        path == "." || workspace.join(path).exists()
    }

    /// Whether a standing claim's every recorded path is gone from disk.
    ///
    /// A claim on a deleted worktree cannot describe work anyone is still
    /// doing, yet it kept refusing every overlapping claimant (#5906): the
    /// reported incident claimed a worktree later removed with
    /// `git worktree remove`, and the claim outlived the checkout it named.
    /// See [`PersistedWriteClaim::present_at_claim`] for why the answer is
    /// "vanished since", not "absent now".
    fn claim_names_only_missing_paths(&self, record: &PersistedWriteClaim) -> bool {
        let Ok(root) = self.claim_root(record) else {
            return false;
        };
        record.names_only_vanished_paths(|path| Self::coordination_claim_path_exists(root, path))
    }

    /// Register a write claim against the live admission set and stamp which
    /// of its paths existed at that moment.
    ///
    /// The one door every production claim goes through, so the admission
    /// answer and the presence stamp cannot drift apart: the ledger has no
    /// filesystem, and a record registered without the stamp would silently
    /// opt out of vanished-path staleness forever (#5906).
    fn register_claim_with_presence(
        &mut self,
        claim: WriteScopeClaim,
        isolated_worktree: bool,
        workspace: &Path,
    ) -> Result<PersistedWriteClaim, String> {
        let admissible = self.admissible_coordination_owners();
        let scope = if isolated_worktree {
            None
        } else {
            self.coordination_scope_for_workspace(workspace)?
        };
        let original_root = self.original_coordination_root().to_path_buf();
        let mut record = self.coordination.register_claim_in_scope(
            claim,
            isolated_worktree,
            scope,
            Some(&original_root),
            |owner| admissible.contains(owner),
        )?;
        let owner = record.claim.owner.clone();
        if !isolated_worktree {
            record.present_at_claim = record
                .claim
                .roots
                .iter()
                .chain(record.claim.exact_files.iter())
                .filter(|path| {
                    self.claim_root(&record)
                        .is_ok_and(|root| Self::coordination_claim_path_exists(root, path))
                })
                .cloned()
                .collect();
        }
        if let Some(persisted) = self
            .coordination
            .write_claims
            .iter_mut()
            .find(|persisted| persisted.claim.owner == owner)
        {
            persisted
                .present_at_claim
                .clone_from(&record.present_at_claim);
        }
        Ok(record)
    }

    /// The owners whose standing claims may refuse a new claimant: live by
    /// [`Self::is_live_coordination_owner`] *and* still naming something that
    /// exists on disk.
    ///
    /// Admission only. [`Self::active_coordination_owners`] stays the answer
    /// for `coordinate release`, which *deletes* records — a live owner whose
    /// tree is merely not created yet must not lose its claim permanently to
    /// a sweep it never asked for.
    fn admissible_coordination_owners(&self) -> std::collections::HashSet<String> {
        let mut owners = self.active_coordination_owners();
        owners.retain(|owner| {
            !self
                .coordination
                .write_claims
                .iter()
                .filter(|record| record.claim.owner == *owner)
                .any(|record| self.claim_names_only_missing_paths(record))
        });
        owners
    }

    // Worker records carry no boot id of their own, so the per-id predicate
    // consults the paired agent when one exists; delegating keeps admission,
    // the peer gate, and stale-claim release on one liveness definition.
    fn active_coordination_owners(&self) -> std::collections::HashSet<String> {
        self.agents
            .keys()
            .chain(self.worker_records.keys())
            .filter(|id| self.is_live_coordination_owner(id))
            .cloned()
            .collect()
    }

    fn namespace_write_claim(
        &self,
        workspace: &Path,
        isolated_worktree: bool,
        mut claim: WriteScopeClaim,
    ) -> Result<WriteScopeClaim, String> {
        if isolated_worktree {
            return Ok(claim);
        }
        let prefix = self.coordination_prefix(workspace)?;
        claim.roots = claim
            .roots
            .iter()
            .map(|path| namespace_coordination_path(&prefix, path))
            .collect::<Result<Vec<_>, _>>()?;
        claim.exact_files = claim
            .exact_files
            .iter()
            .map(|path| namespace_coordination_path(&prefix, path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(claim)
    }

    fn namespace_claim_paths_for_owner(
        &self,
        owner: &str,
        isolated_worktree: bool,
        roots: Vec<String>,
        exact_files: Vec<String>,
    ) -> Result<(Vec<String>, Vec<String>), String> {
        if isolated_worktree {
            return Ok((roots, exact_files));
        }
        let origin = self
            .worker_origins
            .get(owner)
            .cloned()
            .unwrap_or_else(|| WorkerOrigin::legacy(String::new()));
        self.origin_workspace_for_owner(owner)?;
        let execution_workspace = self
            .worker_records
            .get(owner)
            .map(|record| record.spec.workspace.as_path())
            .or_else(|| {
                self.agents
                    .get(owner)
                    .map(|agent| agent.workspace.as_path())
            })
            .unwrap_or(self.workspace.as_path());
        let workspace =
            self.coordination_workspace_in_origin(&origin, execution_workspace, false)?;
        let prefix = self.coordination_prefix(&workspace)?;
        let roots = roots
            .iter()
            .map(|path| namespace_coordination_path(&prefix, path))
            .collect::<Result<Vec<_>, _>>()?;
        let exact_files = exact_files
            .iter()
            .map(|path| namespace_coordination_path(&prefix, path))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((roots, exact_files))
    }

    pub(crate) fn worker_records_for_workspace(
        &self,
        selected_workspace: &Path,
    ) -> Result<Vec<(AgentWorkerRecord, bool)>, String> {
        let selected = self.admitted_coordination_roots.get(selected_workspace);
        if let Some(selected) = selected {
            selected.validate()?;
        }
        let mut visible = Vec::new();
        for record in self.sorted_worker_records() {
            let origin = self
                .worker_origins
                .get(&record.spec.worker_id)
                .ok_or_else(|| "worker origin is missing from the owner journal".to_string())?;
            let matches = match (&origin.selected_workspace, &origin.scope, selected) {
                (Some(workspace), Some(scope), Some(selected)) => {
                    workspace == &selected.lexical && scope == &selected.receipt
                }
                (None, None, _) => selected_workspace == self.workspace,
                _ => false,
            };
            if !matches {
                continue;
            }
            self.validate_worker_origin(origin)?;
            if origin.owner_session_id != record.owner_session_id {
                return Err("worker origin owner session changed".into());
            }
            visible.push((record, origin.kind == WorkerOriginKind::Fleet));
        }
        Ok(visible)
    }

    pub(crate) fn fleet_worker_records_for_workspace(
        &self,
        selected_workspace: &Path,
    ) -> Result<Vec<AgentWorkerRecord>, String> {
        Ok(self
            .worker_records_for_workspace(selected_workspace)?
            .into_iter()
            .filter_map(|(record, fleet)| {
                let legacy_original = self
                    .worker_origins
                    .get(&record.spec.worker_id)
                    .is_some_and(|origin| origin.kind == WorkerOriginKind::LegacyOriginal);
                (fleet || legacy_original).then_some(record)
            })
            .collect())
    }

    pub fn list_worker_records(&self) -> Vec<AgentWorkerRecord> {
        self.sorted_worker_records()
    }

    /// Empty-owner legacy records fail closed. Headless records keep their own
    /// immutable owner so they can remain visible without requiring a paired
    /// `SubAgent` row.
    pub(crate) fn list_worker_records_for_session(
        &self,
        active_session_id: &str,
    ) -> Vec<AgentWorkerRecord> {
        self.sorted_worker_records()
            .into_iter()
            .filter(|record| {
                !active_session_id.is_empty() && record.owner_session_id == active_session_id
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn coordination_snapshot(&self) -> CoordinationLedger {
        self.coordination.clone()
    }

    pub fn get_worker_record(&self, worker_id: &str) -> Option<AgentWorkerRecord> {
        self.worker_records
            .get(worker_id)
            .cloned()
            .map(|mut record| {
                self.overlay_pending_request(&mut record);
                record
            })
    }

    pub(crate) fn get_worker_record_for_session(
        &self,
        active_session_id: &str,
        worker_id: &str,
    ) -> Option<AgentWorkerRecord> {
        self.worker_records.get(worker_id).and_then(|record| {
            (!active_session_id.is_empty() && record.owner_session_id == active_session_id).then(
                || {
                    let mut record = record.clone();
                    self.overlay_pending_request(&mut record);
                    record
                },
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn replace_registered_worker_spec_for_test(
        &mut self,
        spec: AgentWorkerSpec,
    ) -> Result<(), String> {
        let worker_id = spec.worker_id.clone();
        let record = self
            .worker_records
            .get_mut(&worker_id)
            .ok_or_else(|| format!("Fleet worker {worker_id} has no registered launch spec"))?;
        record.spec = spec;
        self.persist_state_synchronously()
            .map_err(|error| format!("failed to persist test worker spec: {error}"))
    }

    /// Persist the next exact launch generation for a Fleet restart without
    /// changing the worker's authority, route, prompt, or retained lifecycle
    /// evidence. The Fleet manager validates the full spec against the task
    /// lease before calling this; this local check makes generation the only
    /// field that can change through this narrow seam.
    pub(crate) fn advance_registered_worker_generation(
        &mut self,
        spec: AgentWorkerSpec,
    ) -> Result<(), String> {
        self.ensure_coordination_process_lock()?;
        let worker_id = spec.worker_id.clone();
        let previous = self
            .worker_records
            .get(&worker_id)
            .cloned()
            .ok_or_else(|| format!("Fleet worker {worker_id} has no registered launch spec"))?;
        let old_generation = previous
            .spec
            .launch_manifest
            .as_ref()
            .map(|manifest| manifest.generation)
            .ok_or_else(|| format!("Fleet worker {worker_id} has no persisted launch manifest"))?;
        let new_generation = spec
            .launch_manifest
            .as_ref()
            .map(|manifest| manifest.generation)
            .ok_or_else(|| {
                format!("Fleet worker {worker_id} replacement has no launch manifest")
            })?;
        if new_generation != old_generation.saturating_add(1) {
            return Err(format!(
                "Fleet worker {worker_id} restart generation must advance from {old_generation} to {}",
                old_generation.saturating_add(1)
            ));
        }
        let mut expected = previous.spec.clone();
        expected
            .launch_manifest
            .as_mut()
            .expect("old launch manifest checked above")
            .generation = new_generation;
        if expected != spec {
            return Err(format!(
                "Fleet worker {worker_id} restart may change only its launch generation"
            ));
        }

        let record = self
            .worker_records
            .get_mut(&worker_id)
            .expect("worker record checked above");
        record.spec = spec;
        record.updated_at_ms = epoch_millis_now();
        if let Err(error) = self.persist_state_synchronously() {
            self.worker_records.insert(worker_id, previous);
            return Err(format!(
                "failed to persist Fleet restart launch generation: {error}"
            ));
        }
        Ok(())
    }

    /// Reconcile an externally executed Fleet worker with the shared headless
    /// lifecycle projection. This is idempotent and makes old write claims stop
    /// participating in active-overlap checks once the durable Fleet task is
    /// terminal.
    pub fn project_external_worker_status(
        &mut self,
        worker_id: &str,
        status: AgentWorkerStatus,
        message: Option<String>,
    ) -> bool {
        let Some(record) = self.worker_records.get(worker_id) else {
            return false;
        };
        if record.status == status {
            return false;
        }
        self.record_worker_event(worker_id, status, message, None, None);
        self.persist_state_best_effort();
        true
    }

    fn record_worker_routed_usage(
        &mut self,
        worker_id: &str,
        source_id: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
        usage: &Usage,
        priced_cost_microusd: Option<u64>,
    ) {
        let fingerprint = crate::cost_status::usage_source_fingerprint(source_id);
        if self
            .worker_records
            .get(worker_id)
            .and_then(|record| record.missing_usage_sources.get(&fingerprint))
            .is_some_and(|coverage| {
                !coverage.route_sha256.is_empty() && !coverage.matches_route(route)
            })
        {
            return;
        }
        self.record_worker_usage(worker_id, source_id, usage, priced_cost_microusd);
    }
    fn record_worker_usage(
        &mut self,
        worker_id: &str,
        source_id: &str,
        usage: &Usage,
        priced_cost_microusd: Option<u64>,
    ) {
        if !usage_has_reported_data(usage) {
            self.record_worker_missing_usage(
                worker_id,
                source_id,
                crate::cost_status::MissingUsageCoverage {
                    reason: crate::cost_status::RuntimeUsageMissingReason::SuccessWithoutUsage,
                    money_metered: true,
                    route_sha256: String::new(),
                },
            );
            return;
        }
        let now_ms = epoch_millis_now();
        let total_delta = usage_total_tokens(usage);
        let Some(record) = self.worker_records.get_mut(worker_id) else {
            return;
        };
        let source_fingerprint = crate::cost_status::usage_source_fingerprint(source_id);
        let promoted = record
            .missing_usage_sources
            .remove(&source_fingerprint)
            .is_some();
        if !record.usage_source_fingerprints.insert(source_fingerprint) && !promoted {
            return;
        }
        record.has_unreported_usage = record.legacy_unreported_usage
            || record.missing_usage_overflowed
            || !record.missing_usage_sources.is_empty();
        record.updated_at_ms = now_ms;
        record.usage.input_tokens = Some(
            record
                .usage
                .input_tokens
                .unwrap_or(0)
                .saturating_add(u64::from(usage.input_tokens)),
        );
        record.usage.output_tokens = Some(
            record
                .usage
                .output_tokens
                .unwrap_or(0)
                .saturating_add(u64::from(usage.output_tokens)),
        );
        record.usage.total_tokens = Some(
            record
                .usage
                .total_tokens
                .unwrap_or(0)
                .saturating_add(total_delta),
        );
        if let Some(cost_microusd) = priced_cost_microusd {
            record.usage.cost_microusd = Some(
                record
                    .usage
                    .cost_microusd
                    .unwrap_or(0)
                    .saturating_add(cost_microusd),
            );
        }
        refresh_usage_note(&mut record.usage);
        self.persist_state_debounced();
    }

    fn record_worker_missing_usage(
        &mut self,
        worker_id: &str,
        source_id: &str,
        coverage: crate::cost_status::MissingUsageCoverage,
    ) {
        let Some(record) = self.worker_records.get_mut(worker_id) else {
            return;
        };
        let fingerprint = crate::cost_status::usage_source_fingerprint(source_id);
        if record.usage_source_fingerprints.contains(&fingerprint) {
            return;
        }
        if record.missing_usage_sources.len() < crate::cost_status::MAX_MISSING_USAGE_SOURCES {
            record.usage_source_fingerprints.insert(fingerprint.clone());
            record.missing_usage_sources.insert(fingerprint, coverage);
        } else {
            record.missing_usage_overflowed = true;
        }
        record.has_unreported_usage = true;
        record.updated_at_ms = epoch_millis_now();
        self.persist_state_debounced();
    }

    /// Persist a pre-work route replacement in the worker's existing route
    /// receipt: the effective provider/model, the replacement source
    /// (`role.replacement` for an approved route, `session.fallback` for a
    /// refused saved-profile pin), and a note naming the original route,
    /// reason and outcome.
    ///
    /// `next` is the runtime the child's requests now use; its route origin
    /// is kept so a failure on that route names it.
    fn record_route_replacement(
        &mut self,
        worker_id: &str,
        next: &SubAgentRuntime,
        source: SpawnRouteSource,
        note: String,
    ) {
        let provider_id = runtime_provider_id(next);
        let model_id = next.model.clone();
        if let Some(origin) = next.route_origin.clone() {
            self.rerouted_requests.insert(
                worker_id.to_string(),
                ReroutedRequest {
                    model: model_id.clone(),
                    provider: next.client.api_provider(),
                    origin,
                },
            );
        }
        if let Some(record) = self.worker_records.get_mut(worker_id) {
            record.spec.model.clone_from(&model_id);
            if let Some(route) = record.spec.child_route.as_mut() {
                route.provider_id = provider_id;
                route.model_id = model_id;
                route.route_source = source.as_str().to_string();
                route.fallback_note = Some(note);
            }
            record.updated_at_ms = epoch_millis_now();
            self.persist_state_debounced();
        }
    }

    /// Remember a saved-profile route that refused authorization this session.
    fn record_refused_route(&mut self, route: &str, reason: &str) {
        self.refused_routes
            .insert(route.to_string(), reason.chars().take(160).collect());
    }

    /// Why this session already knows `route` refuses authorization, if it does.
    fn refused_route(&self, route: &str) -> Option<String> {
        self.refused_routes.get(route).cloned()
    }

    #[cfg(test)]
    fn mark_worker_unreported_usage(&mut self, worker_id: &str) {
        if let Some(record) = self.worker_records.get_mut(worker_id) {
            record.has_unreported_usage = true;
            record.legacy_unreported_usage = true;
            record.updated_at_ms = epoch_millis_now();
            self.persist_state_debounced();
        }
    }

    fn push_worker_event(
        &mut self,
        record: &mut AgentWorkerRecord,
        status: AgentWorkerStatus,
        message: Option<String>,
        step: Option<u32>,
        tool_name: Option<String>,
        now_ms: u64,
    ) {
        self.worker_event_seq = self.worker_event_seq.saturating_add(1);
        record.events.push_back(AgentWorkerEvent {
            seq: self.worker_event_seq,
            worker_id: record.spec.worker_id.clone(),
            status,
            timestamp_ms: now_ms,
            message,
            step,
            tool_name,
        });
        while record.events.len() > MAX_AGENT_WORKER_EVENTS_PER_RECORD {
            record.events.pop_front();
        }
    }

    fn record_worker_event(
        &mut self,
        worker_id: &str,
        status: AgentWorkerStatus,
        message: Option<String>,
        step: Option<u32>,
        tool_name: Option<String>,
    ) {
        let now_ms = epoch_millis_now();
        let Some(mut record) = self.worker_records.remove(worker_id) else {
            return;
        };
        record.status = status;
        record.recommended_action = recommended_action_for_worker_status(status, &record.spec);
        record.updated_at_ms = now_ms;
        record.latest_message = message.clone();
        if matches!(
            status,
            AgentWorkerStatus::Starting | AgentWorkerStatus::Running
        ) && record.started_at_ms.is_none()
        {
            record.started_at_ms = Some(now_ms);
        }
        // A worker that is executing again is not parked, whatever it was
        // before. Re-dispatch under the same id therefore restores its claim's
        // standing without a second authority deciding that (#5906).
        if matches!(
            status,
            AgentWorkerStatus::Queued
                | AgentWorkerStatus::Starting
                | AgentWorkerStatus::Running
                | AgentWorkerStatus::ModelWait
                | AgentWorkerStatus::RunningTool
        ) {
            record.parked_at_turn_end = false;
        }
        if matches!(
            status,
            AgentWorkerStatus::Completed
                | AgentWorkerStatus::Failed
                | AgentWorkerStatus::Cancelled
                | AgentWorkerStatus::Interrupted
        ) {
            record.completed_at_ms = Some(now_ms);
        }
        if let Some(step) = step {
            record.steps_taken = step;
        }
        self.push_worker_event(&mut record, status, message, step, tool_name, now_ms);
        self.worker_records.insert(worker_id.to_string(), record);
        self.reconcile_worker_lifecycle(worker_id);
    }

    fn reconcile_worker_lifecycle(&self, worker_id: &str) {
        let Some(lifecycle) = self
            .agents
            .get(worker_id)
            .and_then(|agent| agent.work_lifecycle.clone())
        else {
            return;
        };
        let Some(record) = self.worker_records.get(worker_id) else {
            return;
        };
        if let Err(err) = lifecycle.reconcile_record(record) {
            tracing::warn!(
                target: "subagent",
                worker_id,
                ?err,
                "failed to reconcile sub-agent Work lifecycle"
            );
        }
    }

    /// Store a verification computed off the lock. Re-checks `checked` so a
    /// racing `ensure_worker_delivery_verified` cannot overwrite a stored
    /// verdict (#6210). Returns whether this call stored.
    fn store_delivery_verification(
        &mut self,
        worker_id: &str,
        verification: AgentRunVerificationSummary,
    ) -> bool {
        if let Some(record) = self.worker_records.get_mut(worker_id)
            && !record.delivery_evidence.checked
        {
            record.verification = verification;
            record.delivery_evidence.checked = true;
            return true;
        }
        false
    }

    /// Claim `workspace` for removing `worker_id`'s unchanged isolated
    /// worktree. Taken under the manager lock, so it is ordered against
    /// continuation, which checks [`Self::worktree_cleanup_pending`] under the
    /// write lock. `None` when another running agent works in that directory
    /// or a removal of it is already claimed.
    fn claim_worktree_cleanup(
        &self,
        worker_id: &str,
        workspace: &Path,
    ) -> Option<WorktreeCleanupClaim> {
        let shared = self.agents.iter().any(|(id, agent)| {
            id != worker_id
                && agent.status == SubAgentStatus::Running
                && agent.workspace == workspace
        });
        if shared {
            return None;
        }
        let mut cleanups = self
            .worktree_cleanups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cleanups
            .insert(workspace.to_path_buf())
            .then(|| WorktreeCleanupClaim {
                cleanups: Arc::clone(&self.worktree_cleanups),
                workspace: workspace.to_path_buf(),
            })
    }

    fn worktree_cleanup_pending(&self, workspace: &Path) -> bool {
        self.worktree_cleanups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(workspace)
    }

    /// Snapshot everything delivery verification needs. `None` when there is
    /// no record, verification already ran, or the result is not terminal.
    /// Pure reads for the read lock in `ensure_worker_delivery_verified`
    /// (#6210).
    fn delivery_verification_inputs(
        &self,
        worker_id: &str,
        result: &SubAgentResult,
    ) -> Option<delivery::DeliveryVerificationInputs> {
        let record = self.worker_records.get(worker_id)?;
        if record.delivery_evidence.checked || result.status == SubAgentStatus::Running {
            return None;
        }
        let deliverables: Vec<String> = record
            .spec
            .launch_manifest
            .as_ref()
            .map(|manifest| manifest.deliverables.clone())
            .unwrap_or_default();
        let allowed = deliverables
            .iter()
            .map(|path| {
                record.spec.runtime_profile.permissions.write
                    && self
                        .validate_write_scope(worker_id, std::slice::from_ref(path))
                        .is_ok()
            })
            .collect();
        Some(delivery::DeliveryVerificationInputs {
            evidence: record.delivery_evidence.clone(),
            workspace: record.spec.workspace.clone(),
            result_text: result.result.clone().unwrap_or_default(),
            write_perm: record.spec.runtime_profile.permissions.write,
            deliverables,
            allowed,
            // Only an interrupted worker with a continuable checkpoint can be
            // resumed in the same workspace; every other terminal state is final.
            remove_worktree_if_unchanged: record
                .spec
                .launch_manifest
                .as_ref()
                .is_some_and(|manifest| manifest.worktree)
                && !matches!(result.status, SubAgentStatus::Interrupted(_)),
        })
    }

    fn complete_worker_from_result(&mut self, worker_id: &str, result: &SubAgentResult) {
        // Delivery verification is deferred: `ensure_worker_delivery_verified`
        // computes it off the lock before the terminal commit (natural and
        // panic paths) or heals it on the next detail read (#6210).
        let status = worker_status_from_subagent_result(result);
        let message = match &result.status {
            SubAgentStatus::Completed => Some("completed".to_string()),
            SubAgentStatus::Failed(err) => Some(err.clone()),
            SubAgentStatus::Interrupted(reason) => Some(reason.clone()),
            SubAgentStatus::Cancelled => Some("cancelled".to_string()),
            SubAgentStatus::BudgetExhausted => Some(
                result
                    .checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.reason.clone())
                    .unwrap_or_else(|| "token budget exhausted".to_string()),
            ),
            SubAgentStatus::Running => Some("running".to_string()),
        };
        if let Some(record) = self.worker_records.get_mut(worker_id) {
            record.result_summary = result.result.clone();
            record.steps_taken = result.steps_taken;
            // #5906: carry the park/ask distinction from the checkpoint onto
            // the durable record, which is what coordination reads.
            record.parked_at_turn_end = result
                .checkpoint
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.parked_at_turn_end);
            match &result.status {
                SubAgentStatus::Failed(err) => record.error = Some(err.clone()),
                SubAgentStatus::BudgetExhausted => record.error = message.clone(),
                _ => {}
            }
        }
        self.record_worker_event(worker_id, status, message, Some(result.steps_taken), None);
    }

    pub fn cancel_agent(&mut self, agent_ref: &str) -> Result<SubAgentResult> {
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        let mut terminal = {
            let agent = self
                .agents
                .get(&agent_id)
                .ok_or_else(|| anyhow!("Agent {agent_id} not found"))?;
            if agent.status != SubAgentStatus::Running {
                let snapshot = self.snapshot_for_listing(agent);
                return Ok(self.terminalize_settled_worker_on_cancel(&agent_id, snapshot));
            }
            if agent.completion_claimed {
                // Still running, with its terminal transition already claimed:
                // mid-flight, not settled. Stealing it here would record a
                // second terminal outcome for one child.
                return Ok(self.snapshot_for_listing(agent));
            }
            self.snapshot_for_listing(agent)
        };
        terminal.status = SubAgentStatus::Cancelled;
        terminal.result = Some(CANCELLED_BY_PARENT_RESULT.to_string());
        terminal.needs_input = None;
        if !self.finish_terminal_result(&agent_id, terminal, true, true) {
            return self.get_result(&agent_id);
        }
        self.get_result(&agent_id)
    }

    /// Append the work-preservation receipt to a child this process just
    /// cancelled (addendum F4). Returns the refreshed snapshot, or `None` when
    /// the child is no longer a fresh Stop (already noted, or re-terminalized).
    pub(crate) fn append_cancel_preservation_note(
        &mut self,
        agent_id: &str,
        note: &str,
    ) -> Option<SubAgentResult> {
        let agent = self.agents.get_mut(agent_id)?;
        if agent.status != SubAgentStatus::Cancelled
            || agent.result.as_deref() != Some(CANCELLED_BY_PARENT_RESULT)
        {
            return None;
        }
        agent.result = Some(format!("{CANCELLED_BY_PARENT_RESULT} {note}"));
        self.persist_state_best_effort();
        self.agents
            .get(agent_id)
            .map(|agent| self.snapshot_for_listing(agent))
    }

    /// Write-scoped descendants of `ancestor` that the same Stop cancelled and
    /// that carry no preservation receipt yet (F4 per-descendant receipts).
    /// Read-only descendants are skipped: they have no baseline to inventory.
    fn freshly_cancelled_writing_descendants(&self, ancestor: &str) -> Vec<String> {
        self.agents
            .values()
            .filter(|agent| {
                agent.id != ancestor
                    && agent.status == SubAgentStatus::Cancelled
                    && agent.result.as_deref() == Some(CANCELLED_BY_PARENT_RESULT)
                    && self
                        .worker_records
                        .get(&agent.id)
                        .is_some_and(|record| record.spec.runtime_profile.permissions.write)
                    && self
                        .ensure_caller_controls_descendant(
                            &agent.id,
                            Some(ancestor),
                            "agent/cancel",
                        )
                        .is_ok()
            })
            .map(|agent| agent.id.clone())
            .collect()
    }

    /// Terminalize a child that already left `Running` but whose worker record
    /// never reached a terminal status — a child parked at the parent's turn
    /// end, or one waiting on an answer the parent has now decided not to give
    /// (#5906).
    ///
    /// Cancel is the path that actually clears a live owner's claim, and before
    /// this it was a no-op on exactly the records that leak: `cancel_agent`
    /// returned the snapshot untouched because the agent was no longer
    /// `Running`, so the non-terminal worker record kept the write claim
    /// standing and every overlapping spawn stayed refused. Terminalizing the
    /// record releases the claim through the one liveness predicate rather
    /// than reaching into the ledger behind its back.
    fn terminalize_settled_worker_on_cancel(
        &mut self,
        agent_id: &str,
        snapshot: SubAgentResult,
    ) -> SubAgentResult {
        let already_terminal = self
            .worker_records
            .get(agent_id)
            .is_none_or(|record| record.status.is_terminal());
        if already_terminal {
            return snapshot;
        }
        if let Some(agent) = self.agents.get_mut(agent_id) {
            // The question is withdrawn with the work: leaving it would keep
            // offering a resume the operator just declined.
            agent.needs_input = None;
        }
        self.record_worker_event(
            agent_id,
            AgentWorkerStatus::Cancelled,
            Some("cancelled by parent while parked or awaiting input".to_string()),
            None,
            None,
        );
        self.persist_state_best_effort();
        self.agents
            .get(agent_id)
            .map_or(snapshot, |agent| self.snapshot_for_listing(agent))
    }

    pub(crate) fn cancel_agent_for_session(
        &mut self,
        active_session_id: &str,
        agent_ref: &str,
    ) -> Result<SubAgentResult> {
        // Resolution and mutation share this manager write borrow, so an alias
        // cannot be rebound between the owner check and the exact-id action.
        let source = self.resolve_agent_ref_for_session(active_session_id, agent_ref)?;
        let agent_id = self.continuation_target(&source)?;
        let descendants = self
            .agents
            .values()
            .filter(|agent| {
                self.agent_is_owned_by_session(agent, active_session_id)
                    && self
                        .ensure_caller_controls_descendant(
                            &agent.id,
                            Some(&agent_id),
                            "agent/cancel",
                        )
                        .is_ok()
            })
            .map(|agent| agent.id.clone())
            .collect::<Vec<_>>();
        for descendant in descendants {
            self.cancel_agent(&descendant)?;
        }
        self.cancel_agent(&agent_id)
    }

    /// Queue parent mail without waking the child (`agents/message`).
    pub fn queue_parent_message(
        &mut self,
        agent_ref: &str,
        text: String,
        wake: bool,
    ) -> Result<ParentMailReceipt> {
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        let status = self
            .agents
            .get(&agent_id)
            .map(|agent| subagent_status_name(&agent.status).to_string())
            .ok_or_else(|| anyhow!("Agent {agent_id} not found"))?;
        let entry = QueuedParentMessage {
            text,
            queued_at_ms: epoch_millis_now(),
            wake,
        };
        let queue = self.queued_mail.entry(agent_id.clone()).or_default();
        if queue.len() >= MAX_QUEUED_PARENT_MESSAGES {
            return Err(anyhow!(
                "Agent {agent_id} already has {MAX_QUEUED_PARENT_MESSAGES} queued parent messages; wait for it to read them before sending more"
            ));
        }
        queue.push_back(entry);
        let queue_depth = queue.len();
        Ok(ParentMailReceipt {
            agent_id,
            status,
            queue_depth,
            woke: false,
            continued_from_checkpoint: false,
            continuation_handle: None,
            note: "queued without wake".to_string(),
        })
    }

    /// Queue-only message entrypoint for live children. Terminal records are
    /// immutable receipts, not mailboxes, so the message surface fails closed
    /// instead of acknowledging undeliverable work.
    pub fn queue_running_parent_message(
        &mut self,
        agent_ref: &str,
        text: String,
    ) -> Result<ParentMailReceipt> {
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        let status = self
            .agents
            .get(&agent_id)
            .map(|agent| agent.status.clone())
            .ok_or_else(|| anyhow!("Agent {agent_id} not found"))?;
        if status != SubAgentStatus::Running {
            return Err(anyhow!(
                "Cannot queue a parent message for agent {agent_id}: status is {} (only running children accept messages)",
                subagent_status_name(&status)
            ));
        }
        self.queue_parent_message(&agent_id, text, false)
    }

    pub(crate) fn queue_running_parent_message_for_session(
        &mut self,
        active_session_id: &str,
        agent_ref: &str,
        text: String,
    ) -> Result<ParentMailReceipt> {
        let agent_id = self.resolve_agent_ref_for_session(active_session_id, agent_ref)?;
        self.queue_running_parent_message(&agent_id, text)
    }

    /// Queue mail and attempt a live wake (`agents/followup`).
    pub fn followup_child(&mut self, agent_ref: &str, text: String) -> Result<ParentMailReceipt> {
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        let status = self
            .agents
            .get(&agent_id)
            .map(|agent| agent.status.clone())
            .ok_or_else(|| anyhow!("Agent {agent_id} not found"))?;
        if matches!(
            status,
            SubAgentStatus::Completed
                | SubAgentStatus::Failed(_)
                | SubAgentStatus::Cancelled
                | SubAgentStatus::BudgetExhausted
        ) {
            return Err(anyhow!(
                "Cannot follow up agent {agent_id}: status is {} and the child cannot resume",
                subagent_status_name(&status)
            ));
        }
        let mut receipt = self.queue_parent_message(&agent_id, text.clone(), true)?;
        let has_input_tx = self
            .agents
            .get(&agent_id)
            .is_some_and(|agent| agent.input_tx.is_some());
        let continuation_handle = self.agents.get(&agent_id).and_then(|agent| {
            agent.checkpoint.as_ref().and_then(|cp| {
                (cp.continuable && !cp.messages.is_empty()).then(|| cp.continuation_handle.clone())
            })
        });
        let continuable = continuation_handle.is_some();

        match status {
            SubAgentStatus::Running if has_input_tx => {
                let pending = self.queued_mail.remove(&agent_id).unwrap_or_default();
                let input_tx = self
                    .agents
                    .get(&agent_id)
                    .and_then(|agent| agent.input_tx.clone());
                let mut pending = pending.into_iter();
                let mut undelivered = VecDeque::new();
                let mut delivered = 0_usize;
                if let Some(tx) = input_tx {
                    let counter =
                        Arc::clone(self.pending_follow_ups.entry(agent_id.clone()).or_default());
                    while let Some(mail) = pending.next() {
                        counter.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                        if tx
                            .send(SubAgentInput {
                                text: mail.text.clone(),
                                interrupt: false,
                                pending: Some(Arc::clone(&counter)),
                            })
                            .is_ok()
                        {
                            delivered = delivered.saturating_add(1);
                        } else {
                            counter.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                            undelivered.push_back(mail);
                            undelivered.extend(pending);
                            break;
                        }
                    }
                }
                if !undelivered.is_empty() {
                    self.queued_mail.insert(agent_id.clone(), undelivered);
                }
                receipt.woke = delivered > 0;
                receipt.queue_depth = self
                    .queued_mail
                    .get(&agent_id)
                    .map(VecDeque::len)
                    .unwrap_or(0);
                receipt.continuation_handle = None;
                receipt.note = if receipt.woke && receipt.queue_depth == 0 {
                    self.woken_agents.insert(agent_id.clone(), true);
                    "queued and delivered to running child".to_string()
                } else if receipt.woke {
                    self.woken_agents.insert(agent_id.clone(), true);
                    format!(
                        "partially delivered to running child; {} message(s) remain queued after the live input channel closed",
                        receipt.queue_depth
                    )
                } else {
                    "queued; running child's live input channel is closed".to_string()
                };
                if receipt.woke
                    && let Some(record) = self.worker_records.get_mut(&agent_id)
                {
                    record.follow_up.latest_delivery = Some(AgentRunFollowUpDelivery {
                        delivered: true,
                        timestamp_ms: epoch_millis_now(),
                        message_preview: Some(truncate_preview(&text, 120)),
                        reason: None,
                        interrupt: false,
                        continued_from_checkpoint: false,
                    });
                }
            }
            SubAgentStatus::Running => {
                receipt.woke = false;
                receipt.note =
                    "queued; running child has no live input channel (likely stale handle)"
                        .to_string();
            }
            SubAgentStatus::Interrupted(_) => {
                // Manager-level followup is queue-only; checkpoint resume is
                // wired at the tool layer (`agents/followup`), which
                // re-dispatches a fresh agent loop seeded with the checkpoint
                // messages when a runtime is attached. This arm keeps the
                // honest queue-only receipt for callers without a runtime.
                receipt.woke = false;
                receipt.continued_from_checkpoint = false;
                receipt.continuation_handle = continuation_handle.clone();
                receipt.note = if continuable {
                    format!(
                        "queued; child is interrupted_continuable — attach a runtime to agents/followup to resume from checkpoint, or re-dispatch using continuation_handle={}",
                        continuation_handle.as_deref().unwrap_or("<missing>")
                    )
                } else {
                    "queued; child is interrupted without a continuable checkpoint".to_string()
                };
                if let Some(record) = self.worker_records.get_mut(&agent_id) {
                    record.follow_up.latest_delivery = Some(AgentRunFollowUpDelivery {
                        delivered: false,
                        timestamp_ms: epoch_millis_now(),
                        message_preview: Some(truncate_preview(&text, 120)),
                        reason: Some(receipt.note.clone()),
                        interrupt: false,
                        continued_from_checkpoint: false,
                    });
                }
            }
            other => {
                receipt.woke = false;
                receipt.note = format!(
                    "queued; child status is {} — no live wake performed",
                    subagent_status_name(&other)
                );
            }
        }
        Ok(receipt)
    }

    pub(crate) fn followup_child_for_session(
        &mut self,
        active_session_id: &str,
        agent_ref: &str,
        text: String,
    ) -> Result<ParentMailReceipt> {
        let agent_id = self.resolve_agent_ref_for_session(active_session_id, agent_ref)?;
        self.followup_child(&agent_id, text)
    }

    /// Follow the persisted continuation chain without treating deliberate
    /// `start + resume_from` forks as continuation targets. Every hop retains
    /// the original root conversation; malformed or cyclic state fails closed.
    fn continuation_target(&self, agent_id: &str) -> Result<String> {
        let source = self
            .agents
            .get(agent_id)
            .ok_or_else(|| anyhow!("Agent not found"))?;
        let owner = &source.owner_session_id;
        let mut target = agent_id.to_string();
        let mut seen = HashSet::new();
        while let Some(next) = self.resume_targets.get(&target) {
            if !seen.insert(target.clone()) {
                return Err(anyhow!("Invalid agent continuation lineage: cycle"));
            }
            let successor = self
                .agents
                .get(next)
                .ok_or_else(|| anyhow!("Agent continuation target is no longer retained"))?;
            if owner.is_empty() || successor.owner_session_id != *owner {
                return Err(anyhow!(
                    "Agent continuation target is outside the active session"
                ));
            }
            target.clone_from(next);
        }
        Ok(target)
    }

    fn continuation_source(&self, agent_id: &str) -> Option<String> {
        self.resume_targets.iter().find_map(|(source, target)| {
            (target == agent_id && self.continuation_target(source).is_ok()).then(|| source.clone())
        })
    }

    pub(super) fn continuation_target_for_caller(
        &self,
        active_session_id: &str,
        agent_ref: &str,
        caller: Option<&str>,
        action: &str,
    ) -> Result<(String, String)> {
        let source = self.ensure_caller_controls_descendant_for_session(
            active_session_id,
            agent_ref,
            caller,
            action,
        )?;
        let target = self.continuation_target(&source)?;
        self.ensure_caller_controls_descendant_for_session(
            active_session_id,
            &target,
            caller,
            action,
        )?;
        Ok((source, target))
    }

    /// Resume an `interrupted_continuable` child by re-dispatching a fresh
    /// agent loop seeded with the checkpoint message tail and the follow-up
    /// text (checkpoint-based continuation).
    ///
    /// The interrupted terminal record stays immutable — receipts are never
    /// rewritten — so the resumed session runs under a new agent id, matching
    /// the existing "re-dispatch using checkpoint {handle}" guidance. Step and
    /// token allowances come from the saved worker and its lineage; a new
    /// runtime cannot reset the original child's remaining limits.
    ///
    /// Callers must hold the manager write lock (same contract as
    /// `spawn_background_with_assignment_options`); `manager_handle` is passed
    /// through to the spawn machinery, which takes no further lock.
    pub(crate) fn resume_from_checkpoint(
        &mut self,
        manager_handle: SharedSubAgentManager,
        runtime: SubAgentRuntime,
        agent_ref: &str,
        followup_text: &str,
    ) -> Result<SubAgentResult> {
        self.resume_from_checkpoint_with_policy(
            manager_handle,
            runtime,
            agent_ref,
            followup_text,
            ResumePolicy::InterruptedOnly,
        )
    }

    pub(crate) fn resume_from_checkpoint_for_session(
        &mut self,
        active_session_id: &str,
        manager_handle: SharedSubAgentManager,
        runtime: SubAgentRuntime,
        agent_ref: &str,
        followup_text: &str,
    ) -> Result<SubAgentResult> {
        let agent_id = self.resolve_agent_ref_for_session(active_session_id, agent_ref)?;
        self.resume_from_checkpoint(manager_handle, runtime, &agent_id, followup_text)
    }

    /// Continue a child on its own fork from the user's side of the shell.
    ///
    /// This is the operator-facing twin of `agents/followup`: a running child
    /// receives the text on its live input channel; an interrupted *or
    /// completed* child whose last checkpoint is continuable is re-dispatched
    /// from that checkpoint under a new agent id (the terminal record stays an
    /// immutable receipt and `resume_targets` links the fork). Failed,
    /// cancelled, and budget-exhausted children cannot be continued.
    pub(crate) fn continue_child_from_user(
        &mut self,
        manager_handle: SharedSubAgentManager,
        runtime: Option<SubAgentRuntime>,
        agent_ref: &str,
        text: &str,
    ) -> Result<UserFollowUpOutcome> {
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        let status = self
            .agents
            .get(&agent_id)
            .map(|agent| agent.status.clone())
            .ok_or_else(|| anyhow!("Agent {agent_id} not found"))?;
        match status {
            SubAgentStatus::Running => {
                let receipt = self.followup_child(&agent_id, text.to_string())?;
                Ok(UserFollowUpOutcome {
                    agent_id: agent_id.clone(),
                    target_agent_id: agent_id,
                    delivered: receipt.woke,
                    resumed: false,
                    note: receipt.note,
                })
            }
            SubAgentStatus::Interrupted(_) | SubAgentStatus::Completed => {
                let Some(runtime) = runtime else {
                    return Err(anyhow!(
                        "Cannot continue agent {agent_id}: no runtime is available to resume it"
                    ));
                };
                let resumed = self.resume_from_checkpoint_with_policy(
                    manager_handle,
                    runtime,
                    &agent_id,
                    text,
                    ResumePolicy::InterruptedOrCompleted,
                )?;
                let target = resumed.agent_id.clone();
                Ok(UserFollowUpOutcome {
                    agent_id,
                    delivered: true,
                    resumed: true,
                    note: format!("continued from checkpoint as {target}"),
                    target_agent_id: target,
                })
            }
            other => Err(anyhow!(
                "Cannot continue agent {agent_id}: status is {} and the child cannot resume",
                subagent_status_name(&other)
            )),
        }
    }

    pub(crate) fn continue_child_from_user_for_session(
        &mut self,
        active_session_id: &str,
        manager_handle: SharedSubAgentManager,
        runtime: Option<SubAgentRuntime>,
        agent_ref: &str,
        text: &str,
    ) -> Result<UserFollowUpOutcome> {
        let agent_id = self.resolve_agent_ref_for_session(active_session_id, agent_ref)?;
        self.continue_child_from_user(manager_handle, runtime, &agent_id, text)
    }

    fn resume_from_checkpoint_with_policy(
        &mut self,
        manager_handle: SharedSubAgentManager,
        runtime: SubAgentRuntime,
        agent_ref: &str,
        followup_text: &str,
        policy: ResumePolicy,
    ) -> Result<SubAgentResult> {
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        let target = self.continuation_target(&agent_id)?;
        if target != agent_id {
            match self.agents.get(&target).map(|agent| &agent.status) {
                Some(SubAgentStatus::Running) => {
                    self.followup_child(&target, followup_text.to_string())?;
                    return self.get_result(&target);
                }
                Some(SubAgentStatus::Interrupted(_)) => {
                    return self.resume_from_checkpoint_with_policy(
                        manager_handle,
                        runtime,
                        &target,
                        followup_text,
                        policy,
                    );
                }
                Some(SubAgentStatus::Completed)
                    if policy == ResumePolicy::InterruptedOrCompleted =>
                {
                    return self.resume_from_checkpoint_with_policy(
                        manager_handle,
                        runtime,
                        &target,
                        followup_text,
                        policy,
                    );
                }
                _ => return self.get_result(&target),
            }
        }
        let saved_identity = self.worker_records.get(&agent_id).map(|record| {
            (
                record.spec.parent_run_id.clone(),
                record.spec.spawn_depth,
                record.spec.max_spawn_depth,
            )
        });
        let (
            agent_type,
            resume_prompt,
            assignment,
            allowed_tools,
            model,
            fork_context,
            workspace,
            claim,
            preserved_profile,
            child_route,
        ) = {
            let agent = self
                .agents
                .get(&agent_id)
                .ok_or_else(|| anyhow!("Agent {agent_id} not found"))?;
            let resumable = match policy {
                ResumePolicy::InterruptedOnly => {
                    matches!(agent.status, SubAgentStatus::Interrupted(_))
                }
                ResumePolicy::InterruptedOrCompleted => matches!(
                    agent.status,
                    SubAgentStatus::Interrupted(_) | SubAgentStatus::Completed
                ),
            };
            if !resumable {
                return Err(anyhow!(
                    "Cannot resume agent {agent_id}: status is {} ({})",
                    subagent_status_name(&agent.status),
                    policy.describe()
                ));
            }
            let checkpoint = agent
                .checkpoint
                .as_ref()
                .filter(|cp| cp.continuable && !cp.messages.is_empty())
                .ok_or_else(|| {
                    let continuable = agent.checkpoint.as_ref().is_some_and(|cp| cp.continuable);
                    let messages = agent
                        .checkpoint
                        .as_ref()
                        .map(|cp| cp.messages.len())
                        .unwrap_or(0);
                    anyhow!(
                        "Agent {agent_id} has no continuable checkpoint to resume from (continuable={continuable}, messages={messages})"
                    )
                })?;
            // Restore the interrupted child's write claim so the resumed loop
            // stays inside the coordination ledger with the original bounded
            // scope instead of inheriting the caller's unchecked write surface.
            // The ledger claim is already namespaced and carries the isolation
            // flag; both are passed through to the spawn seam.
            let claim = self
                .coordination
                .write_claims
                .iter()
                .find(|record| record.claim.owner == agent_id)
                .map(|record| (record.claim.clone(), record.isolated_worktree));
            // Preserve the interrupted child's runtime posture (read_only /
            // denied tools / shell) instead of rebuilding from the caller's
            // role, which could widen the resumed child's authority.
            let preserved_profile = self
                .worker_records
                .get(&agent_id)
                .map(|record| record.spec.runtime_profile.clone());
            let child_route = self
                .worker_records
                .get(&agent_id)
                .and_then(|record| record.spec.child_route.clone());
            (
                agent.agent_type.clone(),
                build_resume_prompt(&agent.prompt, checkpoint, followup_text),
                agent.assignment.clone(),
                agent.allowed_tools.clone(),
                agent.model.clone(),
                agent.fork_context,
                agent.workspace.clone(),
                claim,
                preserved_profile,
                child_route,
            )
        };
        let mut runtime = runtime.background_runtime();
        if let Some((parent, depth, max_depth)) = saved_identity {
            runtime.parent_agent_id = parent;
            runtime.spawn_depth = depth;
            runtime.max_spawn_depth = runtime.max_spawn_depth.min(max_depth);
        }
        if runtime.spawn_depth > runtime.max_spawn_depth {
            return Err(anyhow!(
                "Cannot resume agent {agent_id}: sub-agent depth limit reached (current {}, max {})",
                runtime.spawn_depth,
                runtime.max_spawn_depth
            ));
        }
        // A finished worker's unchanged worktree may be mid-removal on the
        // blocking pool (an interrupt can win after that removal started).
        // A successor started now could lose its directory underneath it.
        if self.worktree_cleanup_pending(&workspace) {
            return Err(anyhow!(
                "Cannot resume agent {agent_id}: its workspace {} is being checked for removal as an unchanged isolated worktree; retry the follow-up in a moment.",
                workspace.display()
            ));
        }
        // A checkpoint outlives its workspace (a finished worker's unchanged
        // worktree is removed, a directory is deleted or unmounted).
        // Resuming into the missing directory would start a child whose
        // every tool fails.
        if !workspace.is_dir() {
            return Err(anyhow!(
                "Cannot resume agent {agent_id}: its workspace {} no longer exists (for example, an isolated worktree with no changes is removed when its agent finishes); start a new agent for the follow-up work.",
                workspace.display()
            ));
        }
        let preserved_origin = self
            .worker_origins
            .get(&agent_id)
            .cloned()
            .ok_or_else(|| anyhow!("continuation source has no captured worker origin"))?;
        self.validate_worker_origin(&preserved_origin)
            .map_err(anyhow::Error::msg)?;
        runtime.context.workspace = workspace;
        // Rebind the child's saved provider pin (#6046). The resumed runtime
        // is derived from the caller, whose active provider can differ from
        // the provider the child was spawned under; without this rebind the
        // saved model is validated against the caller's provider and rejected
        // as a foreign model for a direct provider. `bind_spawn_provider` is
        // a no-op when the pin already matches, so same-provider children are
        // unaffected. Borrow the receipt: it is moved into the spawn options
        // below.
        if let Some(saved_route) = child_route.as_ref() {
            bind_spawn_provider(&mut runtime, &saved_route.provider_id).map_err(|error| {
                anyhow!(
                    "Cannot resume agent {agent_id} on its saved provider '{}': {error}",
                    saved_route.provider_id
                )
            })?;
        }
        // Resume skips the spawn's route binding, so rebuild where the saved
        // route came from; a failure then names that source, not a guess.
        runtime.route_origin = child_route
            .as_ref()
            .and_then(route_origin_from_receipt)
            .map(Arc::new);
        // A child moved before any work (session fallback or approved
        // replacement) resumes on the route its receipt names; the launch
        // model would pair the old pin's model with the new provider.
        let model = child_route
            .as_ref()
            .map_or(model, |route| route.model_id.clone());
        let saved_manifest = self
            .worker_records
            .get(&agent_id)
            .and_then(|record| record.spec.launch_manifest.as_ref());
        let options = SubAgentSpawnOptions {
            origin: Some(preserved_origin),
            name: None, // the old session name stays owned by the terminal record
            model: Some(model),
            model_route: None,
            child_route,
            nickname: None,
            fork_context,
            write_claim: claim.as_ref().map(|(claim, _)| claim.clone()),
            isolated_worktree: claim
                .as_ref()
                .map(|(_, isolated)| *isolated)
                .unwrap_or(false),
            claim_pre_namespaced: claim.is_some(),
            preserve_runtime_profile: preserved_profile,
            expected_artifact: saved_manifest
                .and_then(|manifest| manifest.expected_artifact.clone()),
            deliverables: saved_manifest
                .map(|manifest| manifest.deliverables.clone())
                .unwrap_or_default(),
            resume_from_agent_id: Some(agent_id.clone()),
            checkpoint_continuation: true,
            ..Default::default()
        };
        let resumed = self.spawn_background_with_assignment_options(
            manager_handle,
            runtime,
            agent_type,
            resume_prompt,
            assignment,
            allowed_tools,
            options,
            // Checkpoint resume replays under the write lock; the baseline
            // captures inline there, as before (#6210).
            None,
        )?;
        Ok(resumed)
    }

    /// Interrupt a child, preserve checkpoint, fail closed on root/self.
    pub fn interrupt_child(
        &mut self,
        agent_ref: &str,
        caller_agent_id: Option<&str>,
        reason: String,
    ) -> Result<(SubAgentResult, SubAgentResult)> {
        if agent_ref.trim().eq_ignore_ascii_case("root") {
            return Err(anyhow!(
                "Refusing to interrupt root. agents/interrupt fails closed on the root session."
            ));
        }
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        self.ensure_caller_controls_descendant(&agent_id, caller_agent_id, "agents/interrupt")?;

        let prior = self.get_result_by_ref(&agent_id)?;
        if prior.status != SubAgentStatus::Running
            || self
                .agents
                .get(&agent_id)
                .is_some_and(|agent| agent.completion_claimed)
        {
            return Ok((prior.clone(), prior));
        }

        // Build a continuable checkpoint from the latest stored checkpoint or a
        // minimal placeholder so interrupt never drops recoverability silently.
        let checkpoint = {
            let agent = self
                .agents
                .get(&agent_id)
                .ok_or_else(|| anyhow!("Agent {agent_id} not found"))?;
            agent.checkpoint.clone().unwrap_or_else(|| {
                build_subagent_checkpoint(&agent_id, &reason, &[], agent.steps_taken, true)
            })
        };

        let mut terminal = prior.clone();
        terminal.status = SubAgentStatus::Interrupted(reason.clone());
        terminal.result = Some(reason);
        terminal.steps_taken = checkpoint.steps_taken;
        terminal.checkpoint = Some(checkpoint);
        terminal.needs_input = None;
        if !self.finish_terminal_result(&agent_id, terminal, true, true) {
            return Ok((prior, self.get_result(&agent_id)?));
        }
        let snapshot = self.get_result(&agent_id)?;
        Ok((prior, snapshot))
    }

    pub(crate) fn interrupt_child_for_session(
        &mut self,
        active_session_id: &str,
        agent_ref: &str,
        caller_agent_id: Option<&str>,
        reason: String,
    ) -> Result<(SubAgentResult, SubAgentResult)> {
        let (_, agent_id) = self.continuation_target_for_caller(
            active_session_id,
            agent_ref,
            caller_agent_id,
            "agents/interrupt",
        )?;
        self.interrupt_child(&agent_id, caller_agent_id, reason)
    }

    /// Follow-ups per running child that were handed to its live input
    /// channel but not yet taken at a round boundary. Only non-zero entries.
    #[cfg(test)]
    pub fn queued_follow_up_counts(&self) -> HashMap<String, usize> {
        self.queued_follow_up_counts_inner(None)
    }

    pub(crate) fn queued_follow_up_counts_for_session(
        &self,
        active_session_id: &str,
    ) -> HashMap<String, usize> {
        self.queued_follow_up_counts_inner(Some(active_session_id))
    }

    fn queued_follow_up_counts_inner(
        &self,
        active_session_id: Option<&str>,
    ) -> HashMap<String, usize> {
        self.pending_follow_ups
            .iter()
            .filter_map(|(agent_id, counter)| {
                if active_session_id.is_some_and(|session_id| {
                    !self.agent_id_is_owned_by_session(agent_id, session_id)
                }) {
                    return None;
                }
                let count = counter.load(std::sync::atomic::Ordering::Acquire);
                (count > 0).then(|| (agent_id.clone(), count))
            })
            .collect()
    }

    pub(crate) fn list_coordination_summaries_for_session(
        &self,
        active_session_id: &str,
        include_archived: bool,
        recent_limit: usize,
    ) -> Vec<AgentCoordSummary> {
        self.list_filtered_for_session(active_session_id, include_archived)
            .into_iter()
            .filter_map(|snap| {
                self.coordination_summary_for_session(
                    active_session_id,
                    &snap.agent_id,
                    recent_limit,
                )
                .ok()
            })
            .collect()
    }

    pub fn coordination_summary_for(
        &self,
        agent_ref: &str,
        recent_limit: usize,
    ) -> Result<AgentCoordSummary> {
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        let snap = self.get_result_by_ref(&agent_id)?;
        let record = self.worker_records.get(&agent_id);
        let recent_progress = record
            .map(|r| {
                r.events
                    .iter()
                    .rev()
                    .filter_map(|ev| ev.message.clone())
                    .take(recent_limit)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            })
            .unwrap_or_default();
        let queued_mail = self
            .queued_mail
            .get(&agent_id)
            .map(VecDeque::len)
            .unwrap_or(0);
        let continuable = subagent_checkpoint_is_continuable(&snap);
        let write_claim = self
            .coordination
            .write_claims
            .iter()
            .find(|claim| claim.claim.owner == agent_id)
            .cloned();
        let accepted_decisions = self
            .coordination
            .decisions
            .iter()
            .rev()
            .filter(|decision| {
                decision.owner == agent_id && decision.status == DecisionStatus::Accepted
            })
            .take(recent_limit)
            .cloned()
            .collect();
        Ok(AgentCoordSummary {
            agent_id: snap.agent_id.clone(),
            name: snap.name.clone(),
            parent_run_id: record.and_then(|r| r.parent_run_id.clone()),
            child_route: record.and_then(|r| r.spec.child_route.clone()),
            status: subagent_status_name(&snap.status).to_string(),
            steps_taken: snap.steps_taken,
            recent_progress,
            queued_mail,
            checkpoint_id: snap.checkpoint.as_ref().map(|c| c.checkpoint_id.clone()),
            continuable,
            write_claim,
            accepted_decisions,
        })
    }

    pub(crate) fn coordination_summary_for_session(
        &self,
        active_session_id: &str,
        agent_ref: &str,
        recent_limit: usize,
    ) -> Result<AgentCoordSummary> {
        let agent_id = self.resolve_agent_ref_for_session(active_session_id, agent_ref)?;
        self.coordination_summary_for(&agent_id, recent_limit)
    }

    #[cfg(test)] // coord list/wait surfaces; wired when agents/list hosts go live
    pub fn queued_mail_depth(&self, agent_id: &str) -> Option<usize> {
        self.queued_mail.get(agent_id).map(VecDeque::len)
    }

    #[cfg(test)] // followup honesty probe for coordination tools
    pub fn child_was_woken(&self, agent_id: &str) -> bool {
        self.woken_agents.get(agent_id).copied().unwrap_or(false)
    }

    /// Fingerprint of recent progress for activity waits.
    pub fn activity_fingerprint(&self, agent_id: &str) -> Option<u64> {
        let agent = self.agents.get(agent_id)?;
        let record = self.worker_records.get(agent_id);
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        subagent_status_name(&agent.status).hash(&mut hasher);
        agent.steps_taken.hash(&mut hasher);
        if let Some(record) = record {
            record.events.len().hash(&mut hasher);
            if let Some(last) = record.events.back() {
                last.seq.hash(&mut hasher);
                last.message.hash(&mut hasher);
            }
        }
        let queued = self
            .queued_mail
            .get(agent_id)
            .map(VecDeque::len)
            .unwrap_or(0);
        queued.hash(&mut hasher);
        Some(hasher.finish())
    }

    /// Test helper: seed a running child with a live input channel.
    #[cfg(test)]
    pub fn insert_test_running_agent(&mut self, name: &str, workspace: &Path) -> String {
        self.insert_test_running_agent_with_input(name, workspace).0
    }

    #[cfg(test)]
    pub fn assign_test_session_owner(&mut self, agent_id: &str, owner_session_id: &str) {
        self.agents
            .get_mut(agent_id)
            .expect("test agent")
            .owner_session_id = owner_session_id.to_string();
        if let Some(record) = self.worker_records.get_mut(agent_id) {
            record.owner_session_id = owner_session_id.to_string();
        }
        if let Some(origin) = self.worker_origins.get_mut(agent_id) {
            origin.owner_session_id = owner_session_id.to_string();
        }
    }

    /// Test helper exposing the receiving side so delivery and provenance can
    /// be verified rather than inferred from a still-present sender handle.
    #[cfg(test)]
    fn insert_test_running_agent_with_input(
        &mut self,
        name: &str,
        workspace: &Path,
    ) -> (String, mpsc::UnboundedReceiver<SubAgentInput>) {
        let agent_id = format!("agent_{name}");
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let mut agent = SubAgent::new(
            agent_id.clone(),
            FleetRole::Worker,
            "test".to_string(),
            SubAgentAssignment::new("test".to_string(), None),
            "test-model".to_string(),
            None,
            None,
            input_tx,
            workspace.to_path_buf(),
            self.current_session_boot_id.clone(),
        );
        agent.session_name = name.to_string();
        agent.status = SubAgentStatus::Running;
        // `ToolContext::new` uses this deterministic namespace in unit tests.
        // Tests exercising real conversation boundaries override it through
        // `assign_test_session_owner`.
        agent.owner_session_id = "workspace".to_string();
        // Make the test agent live for 4a liveness (handle required, otherwise
        // list_filtered hides it as phantom). Use try_current + leaked runtime
        // fallback so sync tests also work.
        let handle = if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async {
                std::future::pending::<()>().await;
            })
        } else {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime");
            let h = rt.spawn(async {
                std::future::pending::<()>().await;
            });
            std::mem::forget(rt);
            h
        };
        agent.task_handle = Some(handle);
        self.agents.insert(agent_id.clone(), agent);
        let spec = AgentWorkerSpec {
            worker_id: agent_id.clone(),
            run_id: agent_id.clone(),
            parent_run_id: Some("parent_session".to_string()),
            workflow_run_id: None,
            session_name: Some(name.to_string()),
            objective: "test".to_string(),
            role: None,
            agent_type: FleetRole::Worker,
            model: "test-model".to_string(),
            workspace: workspace.to_path_buf(),
            git_branch: None,
            context_mode: "fresh".to_string(),
            fork_context: false,
            tool_profile: AgentWorkerToolProfile::Inherited,
            runtime_profile: WorkerRuntimeProfile::default(),
            max_steps: WorkerRuntimeProfile::default().max_steps,
            spawn_depth: 1,
            max_spawn_depth: 3,
            child_route: None,
            launch_manifest: None,
        };
        self.register_worker_for_session(spec, "workspace", None);
        (agent_id, input_rx)
    }

    /// Test helper: seed a current-session direct child whose future terminal
    /// result is eligible for automatic parent delivery.
    #[cfg(test)]
    pub fn insert_test_running_direct_child(&mut self, name: &str, workspace: &Path) -> String {
        let agent_id = self.insert_test_running_agent(name, workspace);
        if let Some(record) = self.worker_records.get_mut(&agent_id) {
            record.parent_run_id = None;
            record.spec.parent_run_id = None;
        }
        agent_id
    }

    #[cfg(test)]
    pub(crate) fn insert_test_running_direct_child_in_origin(
        &mut self,
        name: &str,
        execution_workspace: &Path,
        selected_workspace: &Path,
        owner_session: &str,
    ) -> Result<String, String> {
        let origin = self.capture_direct_worker_origin(selected_workspace, owner_session)?;
        let id = self.insert_test_running_direct_child(name, execution_workspace);
        self.assign_test_session_owner(&id, owner_session);
        self.install_worker_origin(&id, origin);
        self.persist_state_synchronously()
            .map_err(|error| error.to_string())?;
        Ok(id)
    }

    /// Test helper: seed a settled direct child whose result has not yet been
    /// delivered to the parent.
    #[cfg(test)]
    pub fn insert_test_terminal_direct_child(&mut self, name: &str, workspace: &Path) -> String {
        let agent_id = self.insert_test_running_direct_child(name, workspace);
        if let Some(agent) = self.agents.get_mut(&agent_id) {
            agent.status = SubAgentStatus::Completed;
            agent.result = Some("test terminal result".to_string());
        }
        if let Some(record) = self.worker_records.get_mut(&agent_id) {
            record.status = AgentWorkerStatus::Completed;
        }
        agent_id
    }

    /// Test helper: seed an interrupted_continuable child with a checkpoint.
    #[cfg(test)]
    pub fn insert_test_interrupted_continuable_agent(
        &mut self,
        name: &str,
        workspace: &Path,
        messages: Vec<codewhale_models::Message>,
    ) -> (String, String) {
        let agent_id = self.insert_test_running_agent(name, workspace);
        let checkpoint = build_subagent_checkpoint(&agent_id, "test_interrupt", &messages, 1, true);
        let handle = checkpoint.continuation_handle.clone();
        if let Some(agent) = self.agents.get_mut(&agent_id) {
            agent.status = SubAgentStatus::Interrupted("test interrupt".to_string());
            agent.checkpoint = Some(checkpoint);
            agent.input_tx = None;
            agent.task_handle = None;
        }
        (agent_id, handle)
    }

    /// Count running agents. Test-only since the governor harvest (#6055):
    /// the live launch-concurrency path reads the gate, not this count.
    #[cfg(test)]
    pub fn running_count(&self) -> usize {
        self.admitted_count()
    }

    pub(crate) fn running_count_for_session(&self, active_session_id: &str) -> usize {
        self.running_count_for_owner(active_session_id, None)
    }

    pub(crate) fn running_count_for_parent(&self, session: &str, parent: &str) -> usize {
        self.running_count_for_owner(session, Some(parent))
    }

    fn running_count_for_owner(&self, active_session_id: &str, parent: Option<&str>) -> usize {
        self.agents
            .values()
            .filter(|agent| self.agent_is_owned_by_session(agent, active_session_id))
            .filter(|agent| self.worker_is_child_of(&agent.id, parent))
            .filter(|agent| {
                agent.status == SubAgentStatus::Running
                    && agent.task_handle.is_some()
                    && !self.running_heartbeat_timed_out(agent)
            })
            .count()
    }

    /// Count live sub-agents that have been admitted, including queued
    /// workers waiting on the launch gate.
    pub fn admitted_count(&self) -> usize {
        self.agents
            .values()
            .filter(|agent| {
                // Exclude non-running statuses
                if agent.status != SubAgentStatus::Running {
                    return false;
                }
                // Exclude persisted agents with no task_handle (they're not actually running)
                if agent.task_handle.is_none() {
                    return false;
                }
                // Keep recently finished handles counted until the terminal
                // status update has reconciled. Otherwise a fanout burst can
                // refill the cap before the UI/state catches up (#2211).
                !self.running_heartbeat_timed_out(agent)
            })
            .count()
    }

    /// Count admitted workers that are currently waiting for the launch gate.
    pub fn queued_count(&self) -> usize {
        self.agents
            .values()
            .filter(|agent| {
                agent.status == SubAgentStatus::Running
                    && agent.task_handle.is_some()
                    && !self.running_heartbeat_timed_out(agent)
                    && self
                        .worker_records
                        .get(&agent.id)
                        .is_some_and(|record| record.status == AgentWorkerStatus::Queued)
            })
            .count()
    }

    /// Count admitted workers not currently in the queued launch state.
    pub fn active_count(&self) -> usize {
        self.admitted_count().saturating_sub(self.queued_count())
    }

    /// Live tasks owned by this session, including queued/stalled tasks until
    /// their real terminal transition. A heartbeat projection is not proof that
    /// the task or its completion delivery has settled.
    pub(crate) fn live_count_for_session(&self, active_session_id: &str) -> usize {
        self.live_count_for_owner(active_session_id, None)
    }

    pub(crate) fn live_count_for_parent(&self, session: &str, parent: &str) -> usize {
        self.live_count_for_owner(session, Some(parent))
    }

    fn worker_is_child_of(&self, worker: &str, parent: Option<&str>) -> bool {
        parent.is_none_or(|parent| {
            self.worker_records
                .get(worker)
                .is_some_and(|record| record.parent_run_id.as_deref() == Some(parent))
        })
    }

    fn live_count_for_owner(&self, active_session_id: &str, parent: Option<&str>) -> usize {
        self.agents
            .values()
            .filter(|agent| {
                agent.status == SubAgentStatus::Running
                    && agent.task_handle.is_some()
                    && self.agent_is_owned_by_session(agent, active_session_id)
                    && self.worker_is_child_of(&agent.id, parent)
            })
            .count()
    }

    fn check_admission_capacity(&self) -> Result<()> {
        let admitted = self.admitted_count();
        if admitted >= self.max_admitted_agents {
            return Err(anyhow!(
                "Sub-agent admission limit reached (max_admitted {}, admitted {}, running {}, queued {}). Wait for queued/running agents to finish, cancel unneeded agents, or raise [subagents] max_admitted for this Workflow.",
                self.max_admitted_agents,
                admitted,
                self.active_count(),
                self.queued_count()
            ));
        }
        Ok(())
    }

    fn running_heartbeat_timed_out(&self, agent: &SubAgent) -> bool {
        agent.status == SubAgentStatus::Running
            && agent.task_handle.is_some()
            && agent.requested_stop.is_none()
            && agent.last_activity_at.elapsed()
                >= agent
                    .heartbeat_timeout
                    .unwrap_or(self.running_heartbeat_timeout)
    }

    pub fn touch(&mut self, agent_id: &str) -> bool {
        let Some(agent) = self.agents.get_mut(agent_id) else {
            return false;
        };
        if agent.status != SubAgentStatus::Running {
            return false;
        }
        agent.last_activity_at = Instant::now();
        true
    }

    pub(crate) fn touch_for_session(&mut self, active_session_id: &str, agent_id: &str) -> bool {
        if !self.agent_id_is_owned_by_session(agent_id, active_session_id) {
            return false;
        }
        self.touch(agent_id)
    }

    /// Spawn a new background sub-agent with explicit assignment and display
    /// metadata.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_background_with_assignment_options(
        &mut self,
        manager_handle: SharedSubAgentManager,
        mut runtime: SubAgentRuntime,
        agent_type: FleetRole,
        mut prompt: String,
        assignment: SubAgentAssignment,
        allowed_tools: Option<Vec<String>>,
        options: SubAgentSpawnOptions,
        precomputed_delivery_evidence: Option<DeliveryEvidence>,
    ) -> Result<SubAgentResult> {
        if runtime
            .execution_limits
            .as_ref()
            .is_some_and(|limits| limits.admission.is_cancelled())
        {
            return Err(anyhow!("Runtime worker admission is closed"));
        }
        let mut origin = match options.origin.as_ref() {
            Some(origin) => origin.clone(),
            None => self
                .capture_direct_worker_origin_for_runtime(
                    &runtime.context.workspace,
                    &runtime.context.state_namespace,
                    runtime.parent_agent_id.as_deref(),
                )
                .map_err(anyhow::Error::msg)?,
        };
        self.validate_worker_origin(&origin)
            .map_err(anyhow::Error::msg)?;
        self.check_worker_origin_capacity(None, &origin)
            .map_err(anyhow::Error::msg)?;
        if origin.owner_session_id != runtime.context.state_namespace {
            return Err(anyhow!("captured worker origin belongs to another session"));
        }
        if let Some(source) = options.resume_from_agent_id.as_deref() {
            origin = self.continuation_worker_origin(
                source,
                &runtime.context.state_namespace,
                runtime.parent_agent_id.as_deref(),
                &origin,
            )?;
        }
        let coordination_workspace = self
            .coordination_workspace_in_origin(
                &origin,
                &runtime.context.workspace,
                options.isolated_worktree,
            )
            .map_err(anyhow::Error::msg)?;
        // Every manager-spawned runtime carries the fleet governor, so the
        // spawned agent and its whole descendant tree report 429s/successes
        // to the adaptive scheduler. Runtimes built outside a manager (tests,
        // tool-only runtimes) keep `governor: None`.
        runtime.governor = Some(Arc::clone(&self.governor));

        self.cleanup(COMPLETED_AGENT_RETENTION);

        self.check_admission_capacity()?;
        if let Some(limits) = runtime.execution_limits.as_ref() {
            let admitted = self.live_count_for_session(&runtime.context.state_namespace);
            if admitted >= limits.max_admitted {
                return Err(anyhow!(
                    "Sub-agent route admission limit reached (max_admitted {}, admitted {}). Wait for queued/running agents to finish or cancel unneeded agents.",
                    limits.max_admitted,
                    admitted
                ));
            }
        }
        runtime.max_spawn_depth = runtime
            .max_spawn_depth
            .min(runtime.worker_profile.max_spawn_depth);
        if runtime.spawn_depth > runtime.max_spawn_depth {
            return Err(anyhow!(
                "Sub-agent depth limit reached (child depth {}, absolute max {})",
                runtime.spawn_depth,
                runtime.max_spawn_depth
            ));
        }
        // A continuation/fork keeps the source's allowance as well as its
        // current parent's allowance. The manifest edge preserves this after
        // reload and for descendant accounting.
        if let Some(preserved) = options.preserve_runtime_profile.as_ref() {
            runtime.spawn_depth = runtime.spawn_depth.max(preserved.spawn_depth);
            runtime.max_spawn_depth = runtime.max_spawn_depth.min(preserved.max_spawn_depth);
            runtime.worker_profile.max_steps =
                narrow_model_steps(runtime.worker_profile.max_steps, preserved.max_steps);
            runtime.worker_profile.wall_time_secs = narrow_optional_limit(
                runtime.worker_profile.wall_time_secs,
                preserved.wall_time_secs,
            );
            runtime.worker_profile.wall_deadline_ms = narrow_optional_limit(
                runtime.worker_profile.wall_deadline_ms,
                preserved.wall_deadline_ms,
            );
        }
        // #6282: spawn options narrow the per-result output cap.
        runtime.max_output_tokens =
            narrow_optional_limit(runtime.max_output_tokens, options.max_output_tokens);
        let mut max_steps = narrow_model_steps(
            runtime.worker_profile.max_steps,
            resolve_max_steps(
                agent_type.clone(),
                options.max_steps,
                runtime
                    .execution_limits
                    .as_ref()
                    .map_or(self.max_steps, |limits| limits.default_max_steps),
            ),
        );
        if let Some(source) = options
            .resume_from_agent_id
            .as_deref()
            .and_then(|id| self.worker_records.get(id))
        {
            if source.spec.max_steps > 0 {
                let remaining = source.spec.max_steps.saturating_sub(source.steps_taken);
                if remaining == 0 {
                    return Err(anyhow!(
                        "child step budget exhausted; continuation cannot reset its model-turn allowance"
                    ));
                }
                max_steps = narrow_model_steps(max_steps, remaining);
            }
            // A source's absolute position is part of its authority. A fork
            // requested by root cannot turn a depth-2 leaf into a depth-1
            // worker and thereby regain a generation of delegation.
            runtime.spawn_depth = runtime
                .spawn_depth
                .max(source.spec.spawn_depth)
                .max(source.spec.runtime_profile.spawn_depth);
            runtime.max_spawn_depth = runtime.max_spawn_depth.min(source.spec.max_spawn_depth);
        }
        if runtime.spawn_depth > runtime.max_spawn_depth {
            return Err(anyhow!(
                "Sub-agent depth limit reached (child depth {}, absolute inherited max {})",
                runtime.spawn_depth,
                runtime.max_spawn_depth
            ));
        }
        // The built-in 30-minute clock is a default, not a ceiling: an explicit
        // `wall_time_secs` may raise it up to MAX_CHILD_WALL_TIME. An operator
        // `[subagents] default_wall_time_secs` stays a ceiling, and inherited
        // profiles and deadlines still only narrow.
        let default_wall_time = runtime
            .execution_limits
            .as_ref()
            .map_or(self.wall_time, |limits| limits.default_wall_time);
        let wall_time = options
            .wall_time
            .unwrap_or_else(|| default_wall_time.unwrap_or(DEFAULT_CHILD_WALL_TIME))
            .min(default_wall_time.unwrap_or(MAX_CHILD_WALL_TIME))
            .min(
                runtime
                    .worker_profile
                    .wall_time_secs
                    .map(Duration::from_secs)
                    .unwrap_or(MAX_CHILD_WALL_TIME),
            )
            .min(MAX_CHILD_WALL_TIME);
        let now_ms = epoch_millis_now();
        let requested_deadline =
            now_ms.saturating_add(u64::try_from(wall_time.as_millis()).unwrap_or(u64::MAX));
        let source_deadline = options
            .resume_from_agent_id
            .as_deref()
            .and_then(|id| self.worker_records.get(id))
            .and_then(|record| record.spec.runtime_profile.wall_deadline_ms);
        // Parent, saved-run and source deadlines: a hard ceiling that also
        // bounds a work clock restarted at launch (see `run_subagent_task_inner`).
        let wall_ceiling_ms =
            narrow_optional_limit(runtime.worker_profile.wall_deadline_ms, source_deadline);
        let deadline_ms = wall_ceiling_ms.map_or(requested_deadline, |deadline| {
            deadline.min(requested_deadline)
        });
        if deadline_ms <= now_ms {
            return Err(anyhow!(
                "child wall-time budget exhausted; continuation cannot reset its deadline"
            ));
        }
        let wall_time = Duration::from_millis(deadline_ms - now_ms);
        let continuation_from = if options.checkpoint_continuation {
            let source = options
                .resume_from_agent_id
                .as_deref()
                .ok_or_else(|| anyhow!("Checkpoint continuation requires a source agent"))?;
            let source =
                self.resolve_agent_ref_for_session(&runtime.context.state_namespace, source)?;
            if self.resume_targets.contains_key(&source) {
                return Err(anyhow!(
                    "Source already has a continuation; address it with followup"
                ));
            }
            Some(source)
        } else {
            None
        };

        if let Some(model) = options.model.as_deref() {
            runtime.model = model.to_string();
        }
        let effective_model = runtime.model.clone();
        let agent_id = format!("agent_{}", &Uuid::new_v4().to_string()[..8]);
        // Admission into the turn-owned barrier happens before worker records,
        // mailbox events, or task scheduling. Once turn settlement closes the
        // registry, a racing descendant is refused rather than escaping the
        // one-shot join and publishing after TurnComplete.
        let foreground_child_registration = runtime.foreground_child_registration(&agent_id)?;
        let active_names: std::collections::HashSet<String> = self
            .agents
            .values()
            .filter_map(|a| a.nickname.clone())
            .collect();
        let nickname = options.nickname.or_else(|| {
            Some(assign_unique_whale_name_in_locale(
                &agent_id,
                &active_names,
                &runtime.locale_tag,
            ))
        });
        let tools = build_allowed_tools(&agent_type, allowed_tools, runtime.allow_shell)?;
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let mut agent = SubAgent::new(
            agent_id.clone(),
            agent_type.clone(),
            prompt.clone(),
            assignment.clone(),
            effective_model,
            nickname,
            tools.clone(),
            input_tx,
            runtime.context.workspace.clone(),
            self.current_session_boot_id.clone(),
        );
        if let Some(name) = options
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            // Names are scoped to the live session: a completed worker
            // hydrated from a previous session's ledger is invisible to
            // `status`/`peek`/`followup`, so it must not reserve the name
            // either (cloud-agent e2e, 2026-08-30: a fresh `exec` in the same
            // workspace could not spawn `worker-a` again). Settled workers
            // release their names the same way: a retry after cancel must
            // not fail "already in use" (#6313). Name lookup prefers the
            // live holder, so a reused name never resolves ambiguously.
            if let Some(existing) = self.agents.values().find(|existing| {
                existing.session_name == name
                    && self.agent_is_owned_by_session(existing, &runtime.context.state_namespace)
                    && !self.is_from_prior_session(existing)
                    && !subagent_status_is_settled(&existing.status)
            }) {
                // #3020: Include elapsed time so the parent can distinguish a
                // live worker from a stale/failed earlier spawn (#2656).
                let elapsed = existing.started_at.elapsed();
                let since = format!(
                    "{} ago",
                    codewhale_command_contract::elapsed::format_elapsed_secs(elapsed.as_secs())
                );
                return Err(anyhow!(
                    "Sub-agent session name '{name}' is already in use by agent_id '{}' \
                     (status: {}, started {since}). \
                     Wait for its completion event, or open a new agent with a different name.",
                    existing.id,
                    subagent_status_name(&existing.status)
                ));
            }
            agent.session_name = name.to_string();
        }
        agent.fork_context = options.fork_context;
        let agent_id = agent.id.clone();
        let started_at = agent.started_at;
        let tool_profile = match tools.clone() {
            Some(tools) => AgentWorkerToolProfile::Explicit(tools),
            None => AgentWorkerToolProfile::Inherited,
        };
        let mut runtime_profile = match options.preserve_runtime_profile.clone() {
            Some(preserved) => {
                // Resuming retains the saved posture only within the current
                // caller's typed permission, shell, tool and denial ceiling.
                let mut preserved = runtime.worker_profile.derive_child(&preserved);
                // Resume the recorded child tier rather than the caller's
                // current tier. Older manifests omitted it; the child receipt
                // already records the actual resolved value, including None.
                if let Some(route) = options.child_route.as_ref() {
                    preserved.reasoning_effort = route.effective_reasoning.clone();
                }
                runtime.reasoning_effort = preserved.reasoning_effort.clone();
                runtime.reasoning_effort_auto = false;
                runtime.worker_profile = preserved.clone();
                preserved
            }
            None => {
                let profile = worker_profile_for_spawn(
                    &runtime,
                    &agent_type,
                    &tool_profile,
                    &agent.model,
                    options.model_route.clone(),
                    options.write_claim.is_some(),
                );
                runtime.worker_profile = profile.clone();
                profile
            }
        };
        let write_capable = runtime_profile.permissions.write;
        let delivery_paths =
            delivery::declared_paths(&options.deliverables, options.expected_artifact.as_deref())
                .map_err(anyhow::Error::msg)?;
        for path in &delivery_paths {
            delivery::safe_deliverable_path(&agent.workspace, path).map_err(anyhow::Error::msg)?;
            let claimed_path = if options.claim_pre_namespaced && !options.isolated_worktree {
                let prefix = self
                    .coordination_prefix(&coordination_workspace)
                    .map_err(anyhow::Error::msg)?;
                namespace_coordination_path(&prefix, path).map_err(anyhow::Error::msg)?
            } else {
                path.clone()
            };
            if !write_capable
                || !options
                    .write_claim
                    .as_ref()
                    .is_some_and(|claim| claim.contains_path(&claimed_path))
            {
                return Err(anyhow!(
                    "deliverable {path:?} is outside the worker write scope; declare an exact_files entry or a containing write_roots path"
                ));
            }
        }
        if write_capable {
            // Isolated-worktree children mutate their own checkout, so they
            // do not contend for the shared-workspace process lock (#5036).
            if !options.isolated_worktree {
                self.ensure_coordination_process_lock()
                    .map_err(anyhow::Error::msg)?;
            }
            if self.coordination_process_lock_required && self.state_path.is_none() {
                return Err(anyhow!(
                    "write-capable sub-agent launch requires a durable coordination state path"
                ));
            }
        }
        let runtime_origin_turn_id = if runtime.context.runtime.active_thread_id.as_deref()
            == Some(runtime.context.state_namespace.as_str())
        {
            runtime
                .runtime_usage_lease
                .as_ref()
                .map(|lease| lease.owner().to_string())
        } else {
            None
        };
        let durable_registration = write_capable
            || runtime_origin_turn_id.is_some()
            || continuation_from.is_some()
            || origin
                .selected_workspace
                .as_ref()
                .is_some_and(|root| root != &self.workspace);
        let durable_launch_snapshot = durable_registration.then(|| {
            (
                self.worker_records.clone(),
                self.coordination.clone(),
                self.worker_origins.clone(),
                self.worker_origin_schema,
            )
        });
        let persisted_claim = if write_capable {
            options
                .write_claim
                .clone()
                .map(|mut claim| {
                    claim.owner = agent_id.clone();
                    let claim = if options.claim_pre_namespaced {
                        claim
                    } else {
                        self.namespace_write_claim(
                            &coordination_workspace,
                            options.isolated_worktree,
                            claim,
                        )?
                    };
                    self.register_claim_with_presence(
                        claim,
                        options.isolated_worktree,
                        &coordination_workspace,
                    )
                })
                .transpose()
        } else {
            Ok(None)
        };
        let persisted_claim = match persisted_claim {
            Ok(claim) => claim,
            Err(error) => {
                if let Err(persist_error) = self.persist_state_synchronously() {
                    if let Some((worker_records, coordination, origins, schema)) =
                        durable_launch_snapshot.as_ref()
                    {
                        self.worker_records = worker_records.clone();
                        self.coordination = coordination.clone();
                        self.worker_origins = origins.clone();
                        self.worker_origin_schema = *schema;
                    }
                    return Err(anyhow!(
                        "{error}; additionally failed to persist contention receipt: {persist_error}"
                    ));
                }
                return Err(anyhow!(error));
            }
        };
        let mut projection_capabilities = tools.clone().unwrap_or_else(|| {
            ["Bash", "File", "Git", "Run", "Web"]
                .into_iter()
                .map(str::to_string)
                .collect()
        });
        projection_capabilities.push(agent_type.as_str().to_string());
        if let Some(role) = assignment.role.as_ref()
            && !projection_capabilities.contains(role)
        {
            projection_capabilities.push(role.clone());
        }
        let original_root = self.original_coordination_root().to_path_buf();
        let claim_root = persisted_claim
            .as_ref()
            .map(|claim| {
                if claim.isolated_worktree {
                    Ok(origin.scope.as_ref().map_or_else(
                        || original_root.clone(),
                        |scope| scope.canonical_root.clone(),
                    ))
                } else {
                    self.claim_root(claim).map(Path::to_path_buf)
                }
            })
            .transpose()
            .map_err(anyhow::Error::msg)?;
        let (decision_projection, _) = self.coordination.project_relevant_decisions_in_scope(
            &agent_id,
            persisted_claim.as_ref().map(|record| &record.claim),
            &projection_capabilities,
            claim_root.as_deref(),
            Some(&original_root),
        );
        if !decision_projection.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&decision_projection);
            agent.prompt = prompt.clone();
        }
        if let Some(claim) = persisted_claim.as_ref().map(|record| &record.claim) {
            prompt.push_str(&format!(
                "\n\nWrite scope (enforced; coordination-root-relative): roots={:?}; exact_files={:?}; contracts={:?}. Expand it with agent action=claim before mutating anything outside this scope.",
                claim.roots, claim.exact_files, claim.contracts
            ));
            agent.prompt = prompt.clone();
        }
        runtime.max_spawn_depth = runtime.max_spawn_depth.min(runtime_profile.max_spawn_depth);
        runtime_profile.max_spawn_depth = runtime.max_spawn_depth;
        runtime_profile.spawn_depth = runtime.spawn_depth;
        runtime_profile.max_steps = max_steps;
        runtime_profile.wall_time_secs = Some(
            wall_time
                .as_secs()
                .saturating_add(u64::from(wall_time.subsec_nanos() > 0)),
        );
        runtime_profile.wall_deadline_ms = Some(deadline_ms);
        runtime.worker_profile = runtime_profile.clone();
        let worker_spec = AgentWorkerSpec {
            worker_id: agent_id.clone(),
            run_id: agent_id.clone(),
            parent_run_id: runtime.parent_agent_id.clone(),
            workflow_run_id: options.workflow_run_id.clone(),
            session_name: Some(agent.session_name.clone()),
            objective: assignment.objective.clone(),
            role: assignment.role.clone(),
            agent_type: agent_type.clone(),
            model: agent.model.clone(),
            workspace: agent.workspace.clone(),
            git_branch: current_git_branch(&agent.workspace),
            context_mode: if options.fork_context {
                "forked"
            } else {
                "fresh"
            }
            .to_string(),
            fork_context: options.fork_context,
            tool_profile,
            runtime_profile: runtime_profile.clone(),
            max_steps,
            spawn_depth: runtime.spawn_depth,
            max_spawn_depth: runtime.max_spawn_depth,
            child_route: options.child_route.clone(),
            launch_manifest: Some(ChildLaunchManifest {
                owner_session: runtime
                    .parent_agent_id
                    .clone()
                    .unwrap_or_else(|| "root".to_string()),
                child_id: agent_id.clone(),
                profile: runtime_profile,
                prompt: prompt.clone(),
                cwd: Some(agent.workspace.display().to_string()),
                worktree: options.isolated_worktree,
                writable_roots: persisted_claim
                    .as_ref()
                    .map(|record| record.claim.roots.clone())
                    .unwrap_or_default(),
                writable_files: persisted_claim
                    .as_ref()
                    .map(|record| record.claim.exact_files.clone())
                    .unwrap_or_default(),
                coordination_contracts: persisted_claim
                    .as_ref()
                    .map(|record| record.claim.contracts.clone())
                    .unwrap_or_default(),
                expected_artifact: options.expected_artifact.clone(),
                deliverables: delivery_paths.clone(),
                resume_identity: Some(agent.session_name.clone()),
                generation: 1,
                resume_from_agent_id: options.resume_from_agent_id.clone(),
            }),
        };
        agent.work_lifecycle =
            match SubAgentWorkLifecycle::register(&runtime, &agent_id, &assignment.objective) {
                Ok(lifecycle) => lifecycle,
                Err(error) => {
                    if let Some((worker_records, coordination, origins, schema)) =
                        durable_launch_snapshot.as_ref()
                    {
                        self.worker_records = worker_records.clone();
                        self.coordination = coordination.clone();
                        self.worker_origins = origins.clone();
                        self.worker_origin_schema = *schema;
                    }
                    return Err(error);
                }
            };
        agent.owner_session_id = runtime.context.state_namespace.clone();
        agent.heartbeat_timeout = runtime
            .execution_limits
            .as_ref()
            .map(|limits| limits.heartbeat_timeout);
        agent.stop_token = Some(runtime.cancel_token.clone());
        agent.terminal_delivery = Some(SubAgentTerminalDeliveryContext::from_runtime(&runtime));
        self.register_worker_for_session_with_origin(
            worker_spec,
            &runtime.context.state_namespace,
            precomputed_delivery_evidence,
            runtime_origin_turn_id,
        );
        self.install_worker_origin(&agent_id, origin);

        // Shared-workspace writers may execute only after their exact worker
        // identity and claim are durably replayable. Persist a Starting record
        // while the manager write lock still excludes the child; then launch.
        // A crash can therefore leave an interrupted owner, never an accepted
        // edit with no durable scope/identity record. Checkpoint continuations
        // commit their source link in this same snapshot, even for read-only
        // workers, so a crash cannot preserve the worker but lose its lineage.
        if durable_registration {
            if let Some(source) = continuation_from.as_ref() {
                self.resume_targets.insert(source.clone(), agent_id.clone());
            }
            self.agents.insert(agent_id.clone(), agent);
            let persist_result = self.persist_state_synchronously();
            agent = self
                .agents
                .remove(&agent_id)
                .expect("pre-launch agent remains registered under manager lock");
            if let Err(error) = persist_result {
                if let Some(source) = continuation_from.as_ref() {
                    self.resume_targets.remove(source);
                }
                let (worker_records, coordination, origins, schema) = durable_launch_snapshot
                    .expect("durable launch captured a registration snapshot");
                self.worker_records = worker_records;
                self.coordination = coordination;
                self.worker_origins = origins;
                self.worker_origin_schema = schema;
                if let Some(lifecycle) = agent.work_lifecycle.as_ref() {
                    let _ = lifecycle.reconcile_state(OwnerState::Failed, 1, None);
                }
                return Err(anyhow!(
                    "failed to durably register write-capable sub-agent before launch: {error}"
                ));
            }
        }

        if let Some(event_tx) = runtime.event_tx.clone() {
            let _ = event_tx.try_send(Event::AgentSpawned {
                owner_session_id: runtime.context.state_namespace.clone(),
                id: agent_id.clone(),
                prompt: prompt.clone(),
                worker_status: self
                    .worker_record_by_ref(&agent_id)
                    .map(|(_, record)| record.status),
                parent_run_id: runtime.parent_agent_id.clone(),
                spawn_depth: runtime.spawn_depth,
                // The model the child was actually installed with. Read here
                // rather than from session state so a later `/model` switch
                // cannot rewrite a launched child's attribution.
                model: agent.model.clone(),
                // Route provenance is resolved on the workflow spawn seam
                // (`WorkflowTaskSpawnMetadata`), not on this path, so it is
                // honestly absent rather than guessed. The model — the half
                // that determines billing — is present either way.
                route_source: None,
                display_name: Some(subagent_display_name(
                    &agent_id,
                    agent.nickname.as_deref(),
                    Some(agent.session_name.as_str()),
                    &subagent_role_label(
                        options.child_route.as_ref(),
                        agent.assignment.role.as_deref(),
                        &agent.agent_type,
                    ),
                )),
            });
        }

        let launch_gate = (runtime.spawn_depth == 1).then(|| self.launch_gate.clone());
        let task = SubAgentTask {
            manager_handle,
            runtime,
            agent_id: agent_id.clone(),
            agent_type,
            prompt,
            assignment,
            allowed_tools: tools,
            fork_context: options.fork_context,
            started_at,
            max_steps,
            wall_time,
            wall_ceiling_ms,
            input_rx,
            launch_gate,
            _foreground_child_registration: foreground_child_registration,
        };
        let handle = spawn_supervised(
            "subagent-task",
            std::panic::Location::caller(),
            run_subagent_task(task),
        );
        agent.task_handle = Some(handle);
        self.agents.insert(agent_id.clone(), agent);
        self.record_worker_event(
            &agent_id,
            AgentWorkerStatus::Running,
            Some("running".to_string()),
            None,
            None,
        );
        self.persist_state_best_effort();

        let agent = self
            .agents
            .get(&agent_id)
            .expect("agent should exist after spawn");
        Ok(self.snapshot_for_listing(agent))
    }

    /// Get the current snapshot for an agent.
    pub fn get_result(&self, agent_id: &str) -> Result<SubAgentResult> {
        let agent = self
            .agents
            .get(agent_id)
            .ok_or_else(|| anyhow!("Agent {agent_id} not found"))?;
        Ok(self.snapshot_for_listing(agent))
    }

    pub fn get_result_by_ref(&self, agent_ref: &str) -> Result<SubAgentResult> {
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        self.get_result(&agent_id)
    }

    /// Get an agent snapshot only when it belongs to the active root session.
    ///
    /// The error deliberately does not distinguish a foreign agent from a
    /// missing one. User- and model-facing callers must not gain an existence
    /// oracle for another conversation by guessing a durable id or name.
    pub(crate) fn get_result_by_ref_for_session(
        &self,
        active_session_id: &str,
        agent_ref: &str,
    ) -> Result<SubAgentResult> {
        let agent_id = self.resolve_agent_ref_for_session(active_session_id, agent_ref)?;
        self.get_result(&agent_id)
    }

    #[cfg(test)]
    pub fn terminal_results_excluding(
        &self,
        delivered_ids: &std::collections::HashSet<String>,
    ) -> Vec<SubAgentResult> {
        self.terminal_results_excluding_inner(None, delivered_ids)
    }

    /// Return terminal direct-child results owned by the active root session.
    ///
    /// The manager and its persisted roster can survive `SyncSession`; exact
    /// owner matching prevents a completed child from the previous
    /// conversation being synthesized into the new turn. Empty legacy owners
    /// do not match and therefore fail closed.
    pub(crate) fn terminal_results_excluding_for_session(
        &self,
        active_session_id: &str,
        delivered_ids: &std::collections::HashSet<String>,
    ) -> Vec<SubAgentResult> {
        self.terminal_results_excluding_inner(Some(active_session_id), delivered_ids)
    }

    fn terminal_results_excluding_inner(
        &self,
        active_session_id: Option<&str>,
        delivered_ids: &std::collections::HashSet<String>,
    ) -> Vec<SubAgentResult> {
        let mut results = self
            .agents
            .values()
            .filter(|agent| agent.status != SubAgentStatus::Running)
            .filter(|agent| agent.session_boot_id == self.current_session_boot_id)
            .filter(|agent| {
                active_session_id.is_none_or(|session_id| agent.owner_session_id == session_id)
            })
            .filter(|agent| {
                self.worker_records
                    .get(&agent.id)
                    .is_none_or(|record| delivers_to_parent_turn(&record.spec))
            })
            .filter(|agent| !delivered_ids.contains(&agent.id))
            .map(|agent| self.snapshot_for_listing(agent))
            .collect::<Vec<_>>();
        results.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        results
    }

    /// Whether a direct child can still inject a completion into the parent
    /// before its next provider request.
    ///
    /// This is deliberately read-only and conservative: a current-session
    /// direct child is eligible while it is still running, and remains
    /// eligible after settling until its terminal result has been delivered.
    /// Preview uses this predicate instead of draining either completion
    /// channel, so inspecting the request cannot consume or claim the state it
    /// is reporting.
    pub(crate) fn may_transform_next_parent_request_for_session(
        &self,
        active_session_id: &str,
        delivered_ids: &std::collections::HashSet<String>,
    ) -> bool {
        self.may_transform_next_parent_request_inner(Some(active_session_id), delivered_ids)
    }

    fn may_transform_next_parent_request_inner(
        &self,
        active_session_id: Option<&str>,
        delivered_ids: &std::collections::HashSet<String>,
    ) -> bool {
        self.agents.values().any(|agent| {
            agent.session_boot_id == self.current_session_boot_id
                && active_session_id
                    .is_none_or(|session_id| self.agent_is_owned_by_session(agent, session_id))
                && self
                    .worker_records
                    .get(&agent.id)
                    .is_none_or(|record| delivers_to_parent_turn(&record.spec))
                && !delivered_ids.contains(&agent.id)
        })
    }

    /// Resolve either a durable agent id or a model-facing session name.
    fn resolve_agent_ref(&self, agent_ref: &str) -> Result<String> {
        self.resolve_agent_ref_inner(agent_ref, None)
    }

    fn resolve_agent_ref_for_session(
        &self,
        active_session_id: &str,
        agent_ref: &str,
    ) -> Result<String> {
        self.resolve_agent_ref_inner(agent_ref, Some(active_session_id))
            .map_err(|_| anyhow!("Agent not found in the active session"))
    }

    fn resolve_agent_ref_inner(
        &self,
        agent_ref: &str,
        active_session_id: Option<&str>,
    ) -> Result<String> {
        let agent_ref = agent_ref.trim();
        if let Some(agent) = self.agents.get(agent_ref)
            && active_session_id
                .is_none_or(|session_id| self.agent_is_owned_by_session(agent, session_id))
        {
            return Ok(agent.id.clone());
        }

        let mut matches = self
            .agents
            .values()
            .filter(|agent| agent.session_name == agent_ref)
            .filter(|agent| {
                active_session_id
                    .is_none_or(|session_id| self.agent_is_owned_by_session(agent, session_id))
            })
            .collect::<Vec<_>>();
        // A reused name shadows settled holders: the live worker wins, and
        // settled records stay id-addressable (#6313).
        if matches
            .iter()
            .any(|agent| !subagent_status_is_settled(&agent.status))
        {
            matches.retain(|agent| !subagent_status_is_settled(&agent.status));
        }
        let matches = matches
            .iter()
            .map(|agent| agent.id.clone())
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [id] => Ok(id.clone()),
            [] => Err(anyhow!("Agent session {agent_ref} not found")),
            _ => Err(anyhow!(
                "Agent session name '{agent_ref}' is ambiguous; use an agent_id"
            )),
        }
    }

    fn agent_is_owned_by_session(&self, agent: &SubAgent, active_session_id: &str) -> bool {
        !active_session_id.is_empty() && agent.owner_session_id == active_session_id
    }

    fn agent_id_is_owned_by_session(&self, agent_id: &str, active_session_id: &str) -> bool {
        self.agents
            .get(agent_id)
            .is_some_and(|agent| self.agent_is_owned_by_session(agent, active_session_id))
    }

    /// Resolve a hierarchy mutation target and prove that it is a strict
    /// descendant of the calling agent. Root registries carry no caller id
    /// (or the literal `root`) and retain authority over every child. This
    /// prevents a child from messaging, waking, or interrupting a sibling or
    /// ancestor and then claiming the ownership that target released.
    pub(super) fn ensure_caller_controls_descendant(
        &self,
        agent_ref: &str,
        caller_agent_id: Option<&str>,
        action: &str,
    ) -> Result<String> {
        let agent_id = self.resolve_agent_ref(agent_ref)?;
        let Some(caller) = caller_agent_id
            .map(str::trim)
            .filter(|caller| !caller.is_empty() && *caller != "root")
        else {
            return Ok(agent_id);
        };
        let caller_identity = if self.agents.contains_key(caller) {
            self.continuation_target(caller)?
        } else {
            // Internal headless parents can own a worker record without a
            // paired SubAgent projection. A missing caller still fails closed.
            self.worker_record_by_ref(caller)
                .map(|(id, _)| id)
                .ok_or_else(|| anyhow!("Calling agent not found"))?
        };
        if caller_identity == self.continuation_target(&agent_id)? {
            return Err(anyhow!(
                "Refusing {action} on self (agent_id '{agent_id}'); child coordination authority is limited to strict descendants."
            ));
        }

        let mut cursor = agent_id.clone();
        let mut visited = std::collections::HashSet::new();
        while visited.insert(cursor.clone()) {
            let Some((_, record)) = self.worker_record_by_ref(&cursor) else {
                break;
            };
            let Some(parent_ref) = record
                .parent_run_id
                .as_deref()
                .or(record.spec.parent_run_id.as_deref())
            else {
                break;
            };
            if parent_ref == "root" {
                break;
            }
            // The original parent edge is immutable. A resumed parent owns
            // the same descendants through the explicit continuation map;
            // deliberate start+resume_from forks grant no such authority.
            let parent_identity = if self.agents.contains_key(parent_ref) {
                self.continuation_target(parent_ref)?
            } else {
                parent_ref.to_string()
            };
            if parent_identity == caller_identity {
                return Ok(agent_id);
            }
            let Some((parent_id, _)) = self.worker_record_by_ref(parent_ref) else {
                break;
            };
            cursor = parent_id;
        }

        Err(anyhow!(
            "Refusing {action} from agent '{caller}' to '{agent_id}'; a child may control only its own descendants."
        ))
    }

    pub(super) fn ensure_caller_controls_descendant_for_session(
        &self,
        active_session_id: &str,
        agent_ref: &str,
        caller_agent_id: Option<&str>,
        action: &str,
    ) -> Result<String> {
        let agent_id = self.resolve_agent_ref_for_session(active_session_id, agent_ref)?;
        if let Some(caller) = caller_agent_id
            .map(str::trim)
            .filter(|caller| !caller.is_empty() && *caller != "root")
        {
            self.resolve_agent_ref_for_session(active_session_id, caller)?;
        }
        self.ensure_caller_controls_descendant(&agent_id, caller_agent_id, action)
    }

    /// List all agents and their status.
    #[must_use]
    /// Snapshot a single agent and tag it with the manager's
    /// classification. The bare `SubAgent::snapshot` defaults
    /// `from_prior_session` to `false`; only the manager knows the
    /// matching boot id, so listing goes through here.
    fn snapshot_for_listing(&self, agent: &SubAgent) -> SubAgentResult {
        let mut snap = agent.snapshot();
        snap.started_at = Some(agent.started_at);
        snap.from_prior_session = self.is_from_prior_session(agent);
        if agent.status == SubAgentStatus::Running {
            let millis =
                |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
            snap.idle_ms = Some(millis(agent.last_activity_at.elapsed()));
            snap.heartbeat_timeout_ms = Some(millis(
                agent
                    .heartbeat_timeout
                    .unwrap_or(self.running_heartbeat_timeout),
            ));
        }
        if let Some(record) = self.worker_records.get(&agent.id) {
            snap.usage = Some(record.usage.clone());
            snap.worker_status = Some(record.status);
            snap.runtime_permissions = Some(crate::fleet::role::fleet_effective_permissions(
                &record.spec.agent_type,
                &record.spec.runtime_profile,
                None,
                None,
            ));
            snap.parent_run_id = record
                .parent_run_id
                .clone()
                .or_else(|| record.spec.parent_run_id.clone());
            snap.spawn_depth = record.spec.spawn_depth;
            snap.child_route = record.spec.child_route.clone();
        }
        snap
    }

    /// List all agents currently held by the manager, regardless of
    /// session origin. Use [`Self::list_filtered`] in user-facing tool
    /// paths so prior-session agents stay hidden by default (#405).
    #[cfg(test)]
    pub fn list(&self) -> Vec<SubAgentResult> {
        self.agents
            .values()
            .map(|agent| self.snapshot_for_listing(agent))
            .collect()
    }

    pub(crate) fn list_for_session(&self, active_session_id: &str) -> Vec<SubAgentResult> {
        self.agents
            .values()
            .filter(|agent| self.agent_is_owned_by_session(agent, active_session_id))
            .map(|agent| self.snapshot_for_listing(agent))
            .collect()
    }

    /// Legacy test-global projection for boot-origin filtering (#405).
    /// Production user/model surfaces call `list_filtered_for_session`, which
    /// first requires exact non-empty root-conversation ownership. Its
    /// `include_archived` option includes only that conversation's archived
    /// rows and never makes a foreign or ownerless record visible.
    #[cfg(test)]
    pub fn list_filtered(&self, include_archived: bool) -> Vec<SubAgentResult> {
        self.list_filtered_inner(None, include_archived)
    }

    pub(crate) fn list_filtered_for_session(
        &self,
        active_session_id: &str,
        include_archived: bool,
    ) -> Vec<SubAgentResult> {
        self.list_filtered_inner(Some(active_session_id), include_archived)
    }

    fn list_filtered_inner(
        &self,
        active_session_id: Option<&str>,
        include_archived: bool,
    ) -> Vec<SubAgentResult> {
        self.agents
            .values()
            .filter(|agent| {
                active_session_id
                    .is_none_or(|session_id| self.agent_is_owned_by_session(agent, session_id))
            })
            .filter(|agent| {
                if include_archived {
                    return true;
                }
                // Live roster: only actually running children (4a). In the
                // legacy unscoped projection, prior-boot Running stays visible
                // for recovery; the scoped production projection has already
                // excluded every foreign conversation above. This
                // excludes completed/failed/cancelled and children that
                // never started (no task_handle) or timed out — same root
                // as the phantom watch entry. Prior-session Running stays
                // visible for recovery even without a handle (persisted
                // without task). Current-session terminals stay visible
                // for result fetch; prior-session terminals hide by default.
                if agent.status == SubAgentStatus::Running {
                    if self.is_from_prior_session(agent) {
                        return true;
                    }
                    return agent.task_handle.is_some() && !self.running_heartbeat_timed_out(agent);
                }
                !self.is_from_prior_session(agent)
            })
            .map(|agent| self.snapshot_for_listing(agent))
            .collect()
    }

    /// Follow both delegation and continuation edges. A set makes malformed
    /// persisted cycles finite and ensures shared descendants count once.
    fn lineage_ancestors(&self, worker_id: &str) -> BTreeSet<String> {
        let mut ancestors = BTreeSet::new();
        let mut pending = vec![worker_id.to_string()];
        while let Some(id) = pending.pop() {
            if !ancestors.insert(id.clone()) {
                continue;
            }
            if let Some(record) = self.worker_records.get(&id) {
                pending.extend(record.spec.parent_run_id.iter().cloned());
                pending.extend(
                    record
                        .spec
                        .launch_manifest
                        .as_ref()
                        .and_then(|manifest| manifest.resume_from_agent_id.as_ref())
                        .cloned(),
                );
            }
        }
        ancestors
    }

    /// Ancestors of live workers are pinned: continuation lookup and
    /// descendant control both walk this lineage, so a completed parent must
    /// outlive every reference a running descendant still holds. Keyed to
    /// liveness, not age, so the retention is bounded by real work.
    fn lineage_records_in_use(&self) -> BTreeSet<String> {
        self.worker_records
            .values()
            .filter(|record| !record.status.is_terminal())
            .flat_map(|record| self.lineage_ancestors(&record.spec.worker_id))
            .collect()
    }

    /// Clean up stale running agents and completed agents older than the
    /// given duration. Returns the number of running agents auto-cancelled
    /// during this pass.
    pub fn cleanup(&mut self, max_age: Duration) -> usize {
        self.cleanup_inner(None, max_age)
    }

    pub(crate) fn cleanup_for_session(
        &mut self,
        active_session_id: &str,
        max_age: Duration,
    ) -> usize {
        self.cleanup_inner(Some(active_session_id), max_age)
    }

    fn cleanup_inner(&mut self, active_session_id: Option<&str>, max_age: Duration) -> usize {
        let before = self.agents.len();
        let before_workers = self.worker_records.len();
        let scoped_agent_owners = self
            .agents
            .iter()
            .filter(|(_, agent)| {
                active_session_id
                    .is_none_or(|session_id| self.agent_is_owned_by_session(agent, session_id))
            })
            .map(|(agent_id, agent)| (agent_id.clone(), agent.owner_session_id.clone()))
            .collect::<HashMap<_, _>>();
        let scoped_agent_ids = scoped_agent_owners.keys().cloned().collect::<HashSet<_>>();
        let scoped_worker_owners = self
            .worker_records
            .iter()
            .filter(|(_, record)| {
                active_session_id.is_none_or(|session_id| {
                    !session_id.is_empty() && record.owner_session_id == session_id
                })
            })
            .map(|(worker_id, record)| (worker_id.clone(), record.owner_session_id.clone()))
            .collect::<HashMap<_, _>>();
        let scoped_worker_ids = scoped_worker_owners.keys().cloned().collect::<HashSet<_>>();
        let mut transcript_candidates = scoped_agent_ids
            .iter()
            .chain(scoped_worker_ids.iter())
            .cloned()
            .collect::<Vec<_>>();
        transcript_candidates.sort();
        transcript_candidates.dedup();
        let mut auto_cancelled = 0;
        let timeout = self.running_heartbeat_timeout;
        let stale_agent_ids = self
            .agents
            .values()
            .filter(|agent| scoped_agent_ids.contains(&agent.id))
            .filter(|agent| {
                agent.status == SubAgentStatus::Running
                    && !agent.completion_claimed
                    && agent.requested_stop.is_none()
                    && agent.last_activity_at.elapsed()
                        >= agent.heartbeat_timeout.unwrap_or(timeout)
            })
            .map(|agent| agent.id.clone())
            .collect::<Vec<_>>();
        for agent_id in stale_agent_ids {
            let timeout = self
                .agents
                .get(&agent_id)
                .and_then(|agent| agent.heartbeat_timeout)
                .unwrap_or(timeout);
            let orphan = self
                .agents
                .get(&agent_id)
                .is_some_and(|agent| agent.task_handle.is_none());
            if let Some(agent) = self.agents.get(&agent_id) {
                tracing::warn!(
                    target: "subagent",
                    agent_id = %agent.id,
                    timeout_secs = timeout.as_secs(),
                    orphan,
                    "auto-cancelling stale sub-agent with no manager-visible progress"
                );
            }
            let Some(mut terminal) = self.agents.get(&agent_id).map(SubAgent::snapshot) else {
                continue;
            };
            // Orphans have no live executor — Interrupted is more honest than
            // Cancelled (nothing was stopped; the process was already gone).
            if orphan {
                terminal.status = SubAgentStatus::Interrupted(
                    "No live executor; marked terminal locally after heartbeat timeout".to_string(),
                );
                terminal.result = Some(format!(
                    "Marked terminal after {}s with no live task handle (local-only; durable coordination may be owned elsewhere).",
                    timeout.as_secs()
                ));
            } else {
                terminal.status = SubAgentStatus::Cancelled;
                terminal.result = Some(format!(
                    "Auto-cancelled after {}s without sub-agent progress.",
                    timeout.as_secs()
                ));
            }
            terminal.needs_input = None;
            // Cleanup batches stale transitions and persists the final fleet
            // snapshot once below. Spawning one unordered background write
            // per child could let an earlier partial snapshot rename last and
            // resurrect a cancelled worker after restart.
            // persist_after_commit=true still best-efforts; lock loss only
            // skips disk — in-memory terminal state always commits.
            if self.finish_terminal_result(&agent_id, terminal, true, true) {
                auto_cancelled += 1;
            }
        }
        // Deliberately NOT here: a pass that terminalized every Running agent
        // without a live task handle whenever this process lacked the
        // coordination flock (#2.6). Two Codewhale sessions in one workspace is
        // ordinary usage, and failure to append to a shared bookkeeping ledger
        // is not evidence that live work has stopped. Liveness is decided by
        // the heartbeat above — which is actual evidence — never by who owns
        // the flock (owner report, 2026-08-04). See `docs/architecture/
        // delegated-coordination.md` for what lock loss legitimately costs.

        // The retained lineage owns continuation lookup and descendant
        // control. Keep the agents projection while live work references it.
        let lineage_records = self.lineage_records_in_use();
        self.agents.retain(|agent_id, agent| {
            if lineage_records.contains(agent_id) {
                return true;
            }
            if active_session_id.is_some() && !scoped_agent_ids.contains(agent_id) {
                return true;
            }
            if agent.status == SubAgentStatus::Running {
                true
            } else {
                agent.started_at.elapsed() < max_age
            }
        });
        // #4217: age-evict terminal worker ledger entries. Agents already drop
        // after `max_age`, but worker_records previously only had an LRU cap of
        // 256 — long-lived sessions rewrote multi-MB subagents.v1.json forever.
        // Running / starting / waiting records are always preserved.
        let now_ms = epoch_millis_now();
        let max_age_ms = max_age.as_millis() as u64;
        self.worker_records.retain(|worker_id, record| {
            if active_session_id.is_some() && !scoped_worker_ids.contains(worker_id) {
                return true;
            }
            if !record.status.is_terminal() {
                return true;
            }
            let anchor_ms = record.completed_at_ms.unwrap_or(record.updated_at_ms);
            now_ms.saturating_sub(anchor_ms) < max_age_ms
        });
        self.prune_worker_origins();
        let previous_links = self.resume_targets.len();
        self.resume_targets.retain(|source, target| {
            self.agents.contains_key(source)
                || self.worker_records.contains_key(source)
                || self.agents.contains_key(target)
                || self.worker_records.contains_key(target)
        });
        // The transcript artifact follows the same retention lifecycle as the
        // worker ledger. Keep it while either the agent or worker record is
        // inspectable; once both age out, remove the one deterministic file so
        // long-lived workspaces do not accumulate silent transcript copies.
        // Also queue handle-store eviction for the same fully-retired agents
        // (#3885): handles pinned in memory per-agent are freed on the next
        // async drain by callers that hold the SharedHandleStore lock.
        for agent_id in transcript_candidates {
            if self.agents.contains_key(&agent_id) || self.worker_records.contains_key(&agent_id) {
                continue;
            }
            if let Err(err) = remove_subagent_transcript_artifact(&self.state_root, &agent_id) {
                tracing::warn!(
                    target: "subagent",
                    ?err,
                    agent_id,
                    "failed to remove expired sub-agent transcript artifact"
                );
            }
            let owner_session_id = scoped_agent_owners
                .get(&agent_id)
                .cloned()
                .or_else(|| scoped_worker_owners.get(&agent_id).cloned())
                .unwrap_or_default();
            self.pending_handle_evictions
                .push((agent_id, owner_session_id));
        }
        if self.agents.len() != before
            || auto_cancelled > 0
            || self.worker_records.len() != before_workers
            || self.resume_targets.len() != previous_links
        {
            self.persist_state_best_effort();
        }
        self.last_cleanup_at = Some(Instant::now());
        auto_cancelled
    }

    /// #3803: whether enough time has elapsed since the last `cleanup` that the
    /// next sidebar refresh should run the write-locked cleanup again. Every
    /// other refresh renders from the read-only `list()` snapshot, so a UI
    /// refresh storm during a fanout does not take the write lock per request.
    #[must_use]
    pub fn cleanup_due(&self, min_interval: Duration) -> bool {
        self.last_cleanup_at
            .is_none_or(|last| last.elapsed() >= min_interval)
    }

    #[must_use]
    pub(crate) fn drain_pending_handle_evictions_for_session(
        &mut self,
        active_session_id: &str,
    ) -> Vec<String> {
        let pending = std::mem::take(&mut self.pending_handle_evictions);
        let (owned, foreign): (Vec<_>, Vec<_>) = pending
            .into_iter()
            .partition(|(_, owner_session_id)| owner_session_id == active_session_id);
        self.pending_handle_evictions = foreign;
        owned.into_iter().map(|(agent_id, _)| agent_id).collect()
    }

    /// Claim terminal delivery if this task is still the running owner.
    ///
    /// The claim excludes cancellation while deliberately leaving the public
    /// status `Running`. `run_subagent_task` can therefore queue completion to
    /// the parent before the running-child gate closes (#1961). The winning
    /// finisher performs only non-awaiting channel sends while it owns the
    /// manager guard, then commits the terminal projections.
    fn claim_terminal_delivery(&mut self, agent_id: &str) -> bool {
        let Some(agent) = self.agents.get_mut(agent_id) else {
            return false;
        };
        if agent.status != SubAgentStatus::Running || agent.completion_claimed {
            return false;
        }
        agent.completion_claimed = true;
        true
    }

    /// Own, publish, and commit one terminal outcome.
    ///
    /// Claiming first makes natural completion, explicit Stop, coordination
    /// interrupt, and stale cleanup race on one bit. The winning path attempts
    /// every live fan-in send while both public projections still read
    /// Running, then commits the matching agent and worker terminal states.
    /// Late task output and repeated Stop calls cannot publish a second result.
    fn finish_terminal_result(
        &mut self,
        agent_id: &str,
        mut result: SubAgentResult,
        abort_task: bool,
        persist_after_commit: bool,
    ) -> bool {
        if result.status == SubAgentStatus::Running || result.agent_id != agent_id {
            return false;
        }
        if abort_task
            && let Some(agent) = self.agents.get_mut(agent_id)
            && let Some(token) = agent.stop_token.as_ref()
        {
            if agent.status != SubAgentStatus::Running
                || agent.completion_claimed
                || agent.requested_stop.is_some()
            {
                return false;
            }
            // Keep task/claims/fan-in ownership while Core persists its decision
            // and projects the final checkpoint. The same task performs commit.
            agent.requested_stop = Some((result.status, result.result));
            token.cancel();
            agent.input_tx = None;
            return true;
        }
        if !self.claim_terminal_delivery(agent_id) {
            return false;
        }
        if let Some((status, message)) = self
            .agents
            .get_mut(agent_id)
            .and_then(|agent| agent.requested_stop.take())
        {
            // A failed owned join is uncertain, not a durable Cancelled receipt.
            if !matches!(result.status, SubAgentStatus::Failed(_)) {
                result.status = status;
                result.result = message;
                result.needs_input = None;
            }
        }
        result.child_route = self
            .worker_records
            .get(agent_id)
            .and_then(|record| record.spec.child_route.clone());

        // Nothing a finished agent asked for is pending any more.
        self.withdraw_child_approvals_for_agent(agent_id);
        if abort_task
            && let Some(handle) = self
                .agents
                .get_mut(agent_id)
                .and_then(|agent| agent.task_handle.take())
        {
            handle.abort();
        }

        let delivery = self
            .agents
            .get(agent_id)
            .and_then(|agent| agent.terminal_delivery.clone());
        if let Some(delivery) = delivery {
            delivery.deliver(self, &result);
        }

        let session_closed = matches!(&result.status, SubAgentStatus::Interrupted(reason)
            if reason == SUBAGENT_SESSION_CLOSED_REASON);
        let committed =
            self.update_from_result_with_persist(agent_id, result, persist_after_commit);
        if committed && session_closed {
            // The asynchronous close request retained these exact claims while
            // its task drained. Retire only after the same terminal commit.
            self.coordination
                .write_claims
                .retain(|record| record.claim.owner != agent_id);
            self.coordination.prune_claim_scopes();
            self.persist_state_best_effort();
        }
        committed
    }

    /// Project the same measured receipt for live fan-in and channel recovery.
    /// The existing worker ledger already persists usage and lineage; no cached
    /// aggregate is stored or folded back into a worker's own usage.
    pub(crate) fn completion_from_result_with_ref_for_session(
        &self,
        owner_session_id: &str,
        result: &SubAgentResult,
        report_ref: Option<&str>,
    ) -> SubAgentCompletion {
        let verification = self
            .worker_records
            .get(&result.agent_id)
            .filter(|record| record.owner_session_id == owner_session_id)
            .map(|record| &record.verification);
        let usage = self
            .completion_usage(owner_session_id, result)
            .unwrap_or_else(|| {
                json!({
                    "scope": "unavailable", "own": completion_own_tokens(None),
                    "descendants": null, "subtree": null,
                })
            });
        subagent_completion_with_verification(
            owner_session_id,
            result,
            report_ref,
            verification,
            Some(&usage),
        )
    }

    fn completion_usage(&self, owner_session_id: &str, result: &SubAgentResult) -> Option<Value> {
        if owner_session_id.is_empty() {
            return None;
        }
        let owned = self
            .worker_records
            .iter()
            .filter(|(id, record)| {
                record.owner_session_id == owner_session_id
                    && id.as_str() == record.spec.worker_id.as_str()
            })
            .map(|(id, record)| (id.as_str(), record))
            .collect::<HashMap<_, _>>();
        let own = owned.get(result.agent_id.as_str())?;
        let mut children = HashMap::<&str, Vec<&str>>::new();
        for (id, record) in &owned {
            let source = record.spec.launch_manifest.as_ref().filter(|manifest| {
                // This manifest field identifies the immediate launch owner,
                // unlike the record's root-conversation owner_session_id.
                manifest.owner_session == record.spec.parent_run_id.as_deref().unwrap_or("root")
                    && manifest.child_id.as_str() == *id
            });
            for parent in record
                .spec
                .parent_run_id
                .as_deref()
                .into_iter()
                .chain(source.and_then(|manifest| manifest.resume_from_agent_id.as_deref()))
            {
                if owned.contains_key(parent) {
                    children.entry(parent).or_default().push(*id);
                }
            }
        }
        // Older persisted continuations may have only the durable successor
        // map. Never follow a missing/foreign record as a bridge into a tree.
        for (source, target) in &self.resume_targets {
            if owned.contains_key(source.as_str()) && owned.contains_key(target.as_str()) {
                children.entry(source).or_default().push(target);
            }
        }
        let mut visited = HashSet::new();
        let mut pending = vec![result.agent_id.as_str()];
        let mut descendants = Vec::new();
        let mut active_descendants = 0;
        let mut unreported_descendants = 0;
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let record = owned[id];
            if id != result.agent_id {
                descendants.push(&record.usage);
                active_descendants += usize::from(!record.status.is_terminal());
                unreported_descendants += usize::from(record.has_unreported_usage);
            }
            pending.extend(children.get(id).into_iter().flatten().copied());
        }
        let mut descendant_totals = completion_token_totals(&descendants, active_descendants);
        descendant_totals["unreported_usage_workers"] = json!(unreported_descendants);
        descendants.push(&own.usage);
        let mut subtree_totals = completion_token_totals(&descendants, active_descendants);
        subtree_totals["unreported_usage_workers"] =
            json!(unreported_descendants + usize::from(own.has_unreported_usage));
        let mut own_tokens = completion_own_tokens(Some(&own.usage));
        // A later measured response cannot make earlier missing usage known.
        // Keep the numeric subtotal and report its incomplete coverage beside it.
        own_tokens["has_unreported_usage"] = json!(own.has_unreported_usage);
        Some(json!({
            "scope": "retained_subtree",
            "own": own_tokens,
            "descendants": descendant_totals,
            "subtree": subtree_totals,
        }))
    }

    /// Commit a claimed natural task result.
    ///
    /// Returns `true` only when the prior claim still owns the terminal
    /// transition. External notification is deliberately queued between
    /// [`Self::claim_terminal_delivery`] and this commit.
    #[cfg(test)]
    fn update_from_result(&mut self, agent_id: &str, result: SubAgentResult) -> bool {
        self.update_from_result_with_persist(agent_id, result, true)
    }

    fn update_from_result_with_persist(
        &mut self,
        agent_id: &str,
        result: SubAgentResult,
        persist_after_commit: bool,
    ) -> bool {
        let Some(agent) = self.agents.get_mut(agent_id) else {
            return false;
        };
        if agent.status != SubAgentStatus::Running || !agent.completion_claimed {
            return false;
        }
        agent.status = result.status.clone();
        agent.assignment = result.assignment.clone();
        agent.result = result.result.clone();
        agent.steps_taken = result.steps_taken;
        agent.checkpoint = result.checkpoint.clone();
        agent.needs_input = result.needs_input.clone();
        if result.status != SubAgentStatus::Running {
            agent.input_tx = None;
        }
        agent.completion_claimed = false;
        agent.task_handle = None;
        agent.stop_token = None;
        agent.requested_stop = None;
        drop(agent.unsettled_launch.take());
        drop(agent.unsettled_foreground.take());
        agent.terminal_delivery = None;
        release_resident_leases_for(agent_id);
        self.complete_worker_from_result(agent_id, &result);
        if persist_after_commit {
            self.persist_state_best_effort();
        }
        true
    }

    fn update_checkpoint(&mut self, agent_id: &str, checkpoint: SubAgentCheckpoint) -> bool {
        let Some(agent) = self.agents.get_mut(agent_id) else {
            return false;
        };
        agent.steps_taken = checkpoint.steps_taken;
        agent.checkpoint = Some(checkpoint);
        agent.last_activity_at = Instant::now();
        // #freeze: hot per-step path — coalesce the full-fleet persist so 20
        // agents stepping concurrently do not serialize the whole fleet (with
        // full transcripts) to disk under the write lock on every step.
        self.persist_state_debounced();
        true
    }
}

/// Thread-safe wrapper for `SubAgentManager`.
pub type SharedSubAgentManager = Arc<RwLock<SubAgentManager>>;

pub(crate) const CHILD_STOP_SETTLE_GRACE: Duration =
    crate::core::engine::FOREGROUND_CHILD_SETTLE_GRACE;

/// Observe the same owned task; timeout keeps its Running/uncertain projection.
/// Never hold the manager lock while Core needs it to project its checkpoint.
pub(crate) async fn settle_requested_child(
    manager: &SharedSubAgentManager,
    snapshot: SubAgentResult,
) -> SubAgentResult {
    let deadline = tokio::time::Instant::now() + CHILD_STOP_SETTLE_GRACE;
    let mut latest = snapshot;
    loop {
        let Ok(manager) = tokio::time::timeout_at(deadline, manager.read()).await else {
            return latest;
        };
        latest = manager.get_result(&latest.agent_id).unwrap_or(latest);
        drop(manager);
        if latest.status != SubAgentStatus::Running || tokio::time::Instant::now() >= deadline {
            return latest;
        }
        tokio::time::sleep(SUBAGENT_WAIT_CHECK_INTERVAL).await;
    }
}

pub(crate) async fn settle_requested_children_for_session(
    manager: &SharedSubAgentManager,
    session: &str,
    parent: Option<&str>,
) {
    let deadline = tokio::time::Instant::now() + CHILD_STOP_SETTLE_GRACE;
    loop {
        let live = {
            let Ok(manager) = tokio::time::timeout_at(deadline, manager.read()).await else {
                return;
            };
            parent.map_or_else(
                || manager.live_count_for_session(session),
                |parent| manager.live_count_for_parent(session, parent),
            )
        };
        if live == 0 || tokio::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(SUBAGENT_WAIT_CHECK_INTERVAL).await;
    }
}

/// Load persisted worker records from an explicit delegated-agent state root.
#[cfg(test)]
pub fn load_persisted_agent_worker_records_with_state_root(
    workspace: &Path,
    state_root: &Path,
) -> Result<Vec<AgentWorkerRecord>> {
    let mut manager =
        SubAgentManager::new_with_state_root(workspace.to_path_buf(), state_root.to_path_buf(), 1)
            .with_state_path(default_state_path(state_root)?);
    manager.load_state()?;
    Ok(manager.list_worker_records())
}

/// Model-facing session projection returned by the v0.8.33 sub-agent API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentSessionProjection {
    pub name: String,
    pub agent_id: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resumed_as: Option<String>,
    pub status: String,
    pub terminal: bool,
    pub context_mode: String,
    pub fork_context: bool,
    pub prefix_cache: SubAgentPrefixCacheProjection,
    pub transcript_handle: VarHandle,
    #[serde(default = "default_agent_run_follow_up")]
    pub follow_up: AgentRunFollowUpTarget,
    #[serde(default = "default_agent_run_takeover")]
    pub takeover: AgentRunTakeoverTarget,
    #[serde(default)]
    pub artifacts: Vec<AgentRunArtifactRef>,
    #[serde(default = "default_agent_run_usage")]
    pub usage: AgentRunUsage,
    #[serde(default = "default_agent_run_verification")]
    pub verification: AgentRunVerificationSummary,
    pub snapshot: SubAgentResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<SubAgentCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_input: Option<SubAgentNeedsInput>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub continuable: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub needs_continuation: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub timed_out_with_checkpoint: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_record: Option<AgentWorkerRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleet_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_route: Option<ChildRouteReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentPrefixCacheProjection {
    pub mode: String,
    pub parent_prefix: String,
    pub deepseek_prefix_cache_reuse: String,
}

fn subagent_prefix_cache_projection(snapshot: &SubAgentResult) -> SubAgentPrefixCacheProjection {
    if snapshot.fork_context {
        SubAgentPrefixCacheProjection {
            mode: "forked".to_string(),
            parent_prefix: "preserved_byte_identical_when_available".to_string(),
            deepseek_prefix_cache_reuse: "optimized_for_existing_parent_prefill".to_string(),
        }
    } else {
        SubAgentPrefixCacheProjection {
            mode: "fresh".to_string(),
            parent_prefix: "not_inherited".to_string(),
            deepseek_prefix_cache_reuse: "independent_child_prefill".to_string(),
        }
    }
}

fn subagent_checkpoint_is_continuable(snapshot: &SubAgentResult) -> bool {
    matches!(snapshot.status, SubAgentStatus::Interrupted(_))
        && snapshot
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.continuable && !checkpoint.messages.is_empty())
}

async fn subagent_session_projection(
    manager: &SharedSubAgentManager,
    snapshot: SubAgentResult,
    timed_out: bool,
    context: &ToolContext,
    worker_record: Option<AgentWorkerRecord>,
) -> SubAgentSessionProjection {
    // Deferred-verification backstop (#6210): terminal commits outside the
    // natural/panic epilogues (Stop, interrupt, session close, stale cleanup)
    // leave verification pending, so the first detail read heals it before
    // the projection reports it. Re-fetch under the session gate afterwards.
    let worker_record = if snapshot.status != SubAgentStatus::Running
        && worker_record
            .as_ref()
            .is_some_and(|record| !record.delivery_evidence.checked)
    {
        ensure_worker_delivery_verified(manager, &snapshot.agent_id, &snapshot).await;
        manager
            .read()
            .await
            .get_worker_record_for_session(&context.state_namespace, &snapshot.agent_id)
            .or(worker_record)
    } else {
        worker_record
    };
    let transcript_session_id = format!("agent:{}", snapshot.agent_id);
    let continuable = subagent_checkpoint_is_continuable(&snapshot);
    let transcript_payload = json!({
        "kind": "subagent_session_snapshot",
        "agent_id": snapshot.agent_id.clone(),
        "name": snapshot.name.clone(),
        "status": subagent_status_name(&snapshot.status),
        "context_mode": snapshot.context_mode.clone(),
        "fork_context": snapshot.fork_context,
        "result": snapshot.result.clone(),
        "steps_taken": snapshot.steps_taken,
        "duration_ms": snapshot.duration_ms,
        "assignment": snapshot.assignment.clone(),
        "checkpoint": snapshot.checkpoint.clone(),
        "needs_input": snapshot.needs_input.clone(),
        "needs_continuation": continuable,
        "timed_out_with_checkpoint": timed_out && continuable,
        "snapshot": snapshot.clone(),
    });
    let transcript_handle = {
        let mut store = context.runtime.handle_store.lock().await;
        let full_transcript_lookup = VarHandle {
            kind: "var_handle".to_string(),
            session_id: transcript_session_id.clone(),
            name: "full_transcript".to_string(),
            type_name: String::new(),
            length: 0,
            repr_preview: String::new(),
            sha256: String::new(),
        };
        if snapshot.status != SubAgentStatus::Running
            && let Some(record) = store.get(&full_transcript_lookup)
        {
            record.handle.clone()
        } else {
            store.insert_json(transcript_session_id, "transcript", transcript_payload)
        }
    };
    let run_id = worker_record
        .as_ref()
        .map(|record| agent_worker_run_id(&record.spec))
        .unwrap_or_else(|| snapshot.agent_id.clone());
    let follow_up = worker_record
        .as_ref()
        .map(|record| record.follow_up.clone())
        .unwrap_or_else(|| AgentRunFollowUpTarget {
            tool: default_agent_inspect_tool(),
            agent_id: snapshot.agent_id.clone(),
            session_name: Some(snapshot.name.clone()),
            accepted_statuses: vec!["running".to_string(), "interrupted_continuable".to_string()],
            latest_delivery: None,
        });
    let takeover = worker_record
        .as_ref()
        .map(|record| record.takeover.clone())
        .unwrap_or_else(|| AgentRunTakeoverTarget {
            kind: default_subagent_takeover_kind(),
            supported: true,
            agent_id: snapshot.agent_id.clone(),
            session_name: Some(snapshot.name.clone()),
            instructions: format!(
                "Inspect agent '{}' through the returned transcript_handle with handle_read; open a replacement with agent if the lane no longer fits.",
                snapshot.agent_id
            ),
            unsupported_reason: None,
        });
    let artifacts = worker_record
        .as_ref()
        .map(|record| record.artifacts.clone())
        .unwrap_or_else(|| default_subagent_artifacts(&run_id));
    let usage = worker_record
        .as_ref()
        .map(|record| record.usage.clone())
        .unwrap_or_else(default_agent_run_usage);
    let verification = worker_record
        .as_ref()
        .map(|record| record.verification.clone())
        .unwrap_or_else(default_agent_run_verification);
    // Status must stay coherent with the continuation flags below. An
    // Interrupted snapshot that carries a continuable checkpoint
    // (`continuable`/`needs_continuation` true, `terminal` true) means the
    // worker is parked waiting for the parent to act, so it must project as
    // `waiting_for_user` rather than a bare `interrupted`. When a worker
    // record exists its status was already derived via
    // `worker_status_from_subagent_result`; mirror that derivation when there
    // is no record so both paths agree on the "needs parent action" signal.
    let status = worker_record
        .as_ref()
        .map(|record| agent_worker_status_name(record.status))
        .unwrap_or_else(|| agent_worker_status_name(worker_status_from_subagent_result(&snapshot)))
        .to_string();

    SubAgentSessionProjection {
        name: snapshot.name.clone(),
        agent_id: snapshot.agent_id.clone(),
        run_id,
        resumed_from: None,
        resumed_as: None,
        status,
        terminal: snapshot.status != SubAgentStatus::Running,
        context_mode: snapshot.context_mode.clone(),
        fork_context: snapshot.fork_context,
        prefix_cache: subagent_prefix_cache_projection(&snapshot),
        transcript_handle,
        follow_up,
        takeover,
        artifacts,
        usage,
        verification,
        checkpoint: snapshot.checkpoint.clone(),
        needs_input: snapshot.needs_input.clone(),
        continuable: subagent_checkpoint_is_continuable(&snapshot),
        needs_continuation: continuable,
        snapshot,
        timed_out,
        timed_out_with_checkpoint: timed_out && continuable,
        fleet_profile: worker_record
            .as_ref()
            .and_then(|record| record.spec.child_route.as_ref())
            .and_then(|receipt| receipt.resolved_profile_id.clone()),
        child_route: worker_record
            .as_ref()
            .and_then(|record| record.spec.child_route.clone()),
        worker_record,
    }
}

/// Append-only, per-run backing store for the worker's complete structured
/// message stream. The in-memory `full_transcript` handle deliberately keeps a
/// bounded tail; this artifact is the durable source used by the TUI's Open
/// action when the conversation is larger than that tail.
///
/// Compaction replaces the worker's live history but never rewrites this
/// record: the pre-compaction messages stay, the checkpoint the model now sees
/// is appended after them, and later live messages continue the same index
/// sequence.
struct SubAgentTranscriptArtifactWriter {
    state_root: PathBuf,
    path: PathBuf,
    relative_path: PathBuf,
    /// Message records in the artifact.
    persisted_messages: usize,
    /// Prefix of the current live history already in the artifact.
    synced_live_messages: usize,
    last_system_prompt: Option<SystemPrompt>,
}

impl SubAgentTranscriptArtifactWriter {
    async fn for_runtime(runtime: &SubAgentRuntime, agent_id: &str) -> Result<Self> {
        let state_root = runtime.manager.read().await.state_root.clone();
        Self::create(&state_root, agent_id)
    }

    fn create(state_root: &Path, agent_id: &str) -> Result<Self> {
        let state_root = normalize_subagent_workspace(state_root);
        let relative_path = subagent_transcript_artifact_relative_path(agent_id);
        let path = checked_subagent_transcript_artifact_path(&state_root, agent_id)?;
        let header = json!({
            "kind": "subagent_transcript_header",
            "schema_version": SUBAGENT_TRANSCRIPT_ARTIFACT_SCHEMA_VERSION,
            "agent_id": agent_id,
        });
        create_private_subagent_transcript(&state_root, &path, &json_line(&header)?)?;
        Ok(Self {
            state_root,
            path,
            relative_path,
            persisted_messages: 0,
            synced_live_messages: 0,
            last_system_prompt: None,
        })
    }

    fn unsynced_messages<'a>(&self, messages: &'a [Message]) -> Result<&'a [Message]> {
        messages.get(self.synced_live_messages..).ok_or_else(|| {
            anyhow!(
                "sub-agent transcript history shrank from {} to {} messages",
                self.synced_live_messages,
                messages.len()
            )
        })
    }

    fn sync_messages(&mut self, messages: &[Message], durable: bool) -> Result<()> {
        let unsynced = self.unsynced_messages(messages)?;
        self.append_messages(unsynced, durable)?;
        self.synced_live_messages = messages.len();
        Ok(())
    }

    fn sync_projected_messages(&mut self, messages: &[Message], durable: bool) -> Result<()> {
        let unsynced = crate::image_attach::safe_tool_result_message_projection(
            self.unsynced_messages(messages)?,
        );
        self.append_messages(&unsynced, durable)?;
        self.synced_live_messages = messages.len();
        Ok(())
    }

    /// Record a compaction pass: persist whatever of the replaced history is
    /// not yet on disk, append the checkpoint the model sees from now on, and
    /// continue syncing live messages after the replacement history.
    fn record_compaction(&mut self, replaced: &[Message], replacement: &[Message]) -> Result<()> {
        self.sync_messages(replaced, false)?;
        let checkpoints = replacement
            .iter()
            .filter(|message| crate::compaction::is_wire_compaction_checkpoint_message(message))
            .cloned()
            .collect::<Vec<_>>();
        self.append_messages(&checkpoints, false)?;
        self.synced_live_messages = replacement.len();
        Ok(())
    }

    fn append_messages(&mut self, messages: &[Message], durable: bool) -> Result<()> {
        let mut encoded = Vec::new();
        for (offset, message) in messages.iter().enumerate() {
            encoded.extend(json_line(&json!({
                "kind": "message",
                "index": self.persisted_messages + offset,
                "message": message,
            }))?);
        }
        if !encoded.is_empty() || durable {
            append_private_subagent_transcript(&self.state_root, &self.path, &encoded, durable)?;
        }
        self.persisted_messages += messages.len();
        Ok(())
    }

    fn metadata(&self, complete: bool) -> Value {
        json!({
            "kind": "subagent_transcript_jsonl",
            "schema_version": SUBAGENT_TRANSCRIPT_ARTIFACT_SCHEMA_VERSION,
            "relative_path": self.relative_path,
            "persisted_messages": self.persisted_messages,
            "complete": complete,
            "contains_session_content": true,
        })
    }
}

fn json_line(value: &Value) -> Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(value)?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn subagent_transcript_artifact_relative_path(agent_id: &str) -> PathBuf {
    let digest = crate::hashing::sha256_hex(agent_id.as_bytes());
    Path::new(".codewhale")
        .join("state")
        .join(SUBAGENT_TRANSCRIPT_ARTIFACT_DIR)
        .join(format!("{digest}.jsonl"))
}

fn checked_subagent_transcript_artifact_path(state_root: &Path, agent_id: &str) -> Result<PathBuf> {
    checked_subagent_state_path(
        state_root,
        &subagent_transcript_artifact_relative_path(agent_id),
    )
}

/// Read the complete structured worker chat for the TUI Open action. The path
/// is derived from `agent_id` rather than accepted from handle JSON, so a
/// corrupted or model-supplied payload cannot redirect the reader outside the
/// manager state root.
pub fn load_subagent_transcript_artifact(
    state_root: &Path,
    agent_id: &str,
) -> Result<Vec<Message>> {
    let state_root = normalize_subagent_workspace(state_root);
    let path = checked_subagent_transcript_artifact_path(&state_root, agent_id)?;
    let raw = read_subagent_state_file(&state_root, &path)?;
    let mut lines = raw.lines();
    let header_line = lines
        .next()
        .ok_or_else(|| anyhow!("sub-agent transcript artifact is empty"))?;
    let header: Value = serde_json::from_str(header_line)?;
    if header.get("kind").and_then(Value::as_str) != Some("subagent_transcript_header")
        || !matches!(
            header.get("schema_version").and_then(Value::as_u64),
            Some(1 | 2)
        )
        || header.get("agent_id").and_then(Value::as_str) != Some(agent_id)
    {
        return Err(anyhow!(
            "sub-agent transcript artifact header does not match agent {agent_id}"
        ));
    }

    let mut messages = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let record: Value = serde_json::from_str(line)?;
        if record.get("kind").and_then(Value::as_str) == Some("system_prompt") {
            anyhow::ensure!(
                header.get("schema_version").and_then(Value::as_u64) == Some(2),
                "system prompt record requires transcript schema 2"
            );
            anyhow::ensure!(
                record.get("at_message_index").and_then(Value::as_u64)
                    == Some(messages.len() as u64),
                "system prompt record has an invalid message boundary"
            );
            let _: SystemPrompt = serde_json::from_value(
                record
                    .get("system_prompt")
                    .cloned()
                    .ok_or_else(|| anyhow!("system prompt record has no prompt"))?,
            )?;
            continue;
        }
        if record.get("kind").and_then(Value::as_str) != Some("message") {
            return Err(anyhow!("unknown sub-agent transcript artifact record"));
        }
        let index = record
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| anyhow!("sub-agent transcript message is missing its index"))?;
        if index != messages.len() {
            return Err(anyhow!(
                "sub-agent transcript message index {index} does not follow {}",
                messages.len()
            ));
        }
        let message = serde_json::from_value::<Message>(
            record
                .get("message")
                .cloned()
                .ok_or_else(|| anyhow!("sub-agent transcript record is missing its message"))?,
        )?;
        messages.push(message);
    }
    Ok(messages)
}

fn remove_subagent_transcript_artifact(state_root: &Path, agent_id: &str) -> Result<bool> {
    let state_root = normalize_subagent_workspace(state_root);
    let path = checked_subagent_transcript_artifact_path(&state_root, agent_id)?;
    reject_state_path_symlinks(&state_root, &path)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err(anyhow!(
            "sub-agent transcript artifact is not a regular file: {}",
            path.display()
        ));
    }
    fs::remove_file(path)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) fn write_subagent_transcript_artifact_for_test(
    state_root: &Path,
    agent_id: &str,
    messages: &[Message],
) -> Result<PathBuf> {
    let mut writer = SubAgentTranscriptArtifactWriter::create(state_root, agent_id)?;
    writer.sync_messages(messages, true)?;
    Ok(writer.path)
}

fn default_state_path(state_root: &Path) -> Result<PathBuf> {
    let state_root = normalize_subagent_workspace(state_root);
    // Canonical post-rebrand state path. On first run the file won't exist yet;
    // write_json_atomic creates parent directories. Legacy .deepseek/state/ data
    // is migrated on load (see load_state).
    checked_subagent_state_path(
        &state_root,
        &Path::new(".codewhale")
            .join("state")
            .join(SUBAGENT_STATE_FILE),
    )
}

fn checked_subagent_state_path(state_root: &Path, path: &Path) -> Result<PathBuf> {
    let state_root = normalize_subagent_workspace(state_root);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        state_root.join(path)
    };
    let file_name = absolute
        .file_name()
        .ok_or_else(|| anyhow!("sub-agent state path must include a file name"))?;
    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow!("sub-agent state path must include a parent directory"))?;
    // Resolve through the deepest existing ancestor so Windows short/long names
    // and verbatim (`\\?\`) spellings match the already-canonical state root.
    // A missing parent used to fall back to lexical normalization and could keep
    // an 8.3 component (for example `RUNNER~1`) that no longer prefixes the root.
    let parent = resolve_path_for_containment(parent)?;
    let state_path = parent.join(file_name);
    // The state root may itself sit behind a link the user created: see
    // `effective_state_root`. Both checks below must use the same root, or a
    // relocated state directory fails containment and then fails the walk.
    let state_root = effective_state_root(&state_root, &state_path);
    if !path_is_within_state_root(&state_path, &state_root) {
        return Err(anyhow!(
            "sub-agent state path must stay within state root: {}",
            state_path.display()
        ));
    }
    reject_root_relative_symlinks(&state_root, &state_path)?;
    Ok(state_path)
}

/// The state root in effect for an already-resolved child path.
///
/// `default_state_path` and the transcript artifact writer both build
/// `.codewhale/state/...` under the state root. A user who keeps application
/// state on another volume makes `<state_root>/.codewhale` a junction (or a
/// symlink), which resolves the child outside the state root while landing
/// exactly where that user put the state. When the link exists and the child
/// resolves inside its target, the target is the root the containment check
/// and the link walk must use; otherwise the state root is returned unchanged.
///
/// A child that resolves anywhere else keeps the stricter state root, so this
/// re-roots one known, user-owned link and does not widen the boundary.
fn effective_state_root(state_root: &Path, state_path: &Path) -> PathBuf {
    let linked = state_root.join(codewhale_config::CODEWHALE_APP_DIR);
    let Ok(metadata) = fs::symlink_metadata(&linked) else {
        return state_root.to_path_buf();
    };
    // Windows junctions are reparse points without the symlink tag, so the
    // shared predicate is required here; `FileType::is_symlink` alone would
    // miss exactly the case this exists for.
    if !crate::plugins::metadata_is_link_or_reparse(&metadata) {
        return state_root.to_path_buf();
    }
    match resolve_path_for_containment(&linked) {
        Ok(resolved) if path_is_within_state_root(state_path, &resolved) => resolved,
        _ => state_root.to_path_buf(),
    }
}

/// [`reject_root_relative_symlinks`] for a path that may sit under a state
/// root the user relocated behind a link. Every guard that runs after
/// `checked_subagent_state_path` must reach the same root decision it did, or
/// a relocated root passes that check and fails the write; roots outside this
/// module's state layout come back unchanged from [`effective_state_root`].
fn reject_state_path_symlinks(state_root: &Path, path: &Path) -> Result<()> {
    let root = effective_state_root(state_root, path);
    reject_root_relative_symlinks(&root, path)
}

/// Canonicalize `path` when it exists; otherwise canonicalize the deepest
/// existing ancestor and re-append the missing tail. Keeps containment checks
/// on the same identity spelling as [`normalize_subagent_workspace`].
fn resolve_path_for_containment(path: &Path) -> Result<PathBuf> {
    let mut missing = Vec::new();
    let mut current = path.to_path_buf();
    loop {
        match current.canonicalize() {
            Ok(canonical) => {
                let mut resolved = canonical;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = current.file_name() else {
                    return Ok(normalize_path_components(path));
                };
                missing.push(name.to_os_string());
                let Some(parent) = current.parent() else {
                    return Ok(normalize_path_components(path));
                };
                if parent.as_os_str().is_empty() || parent == current.as_path() {
                    return Ok(normalize_path_components(path));
                }
                current = parent.to_path_buf();
            }
            Err(err) => return Err(err.into()),
        }
    }
}

fn path_is_within_state_root(candidate: &Path, root: &Path) -> bool {
    if candidate.starts_with(root) {
        return true;
    }

    // Align both sides onto the same identity spelling before the containment
    // walk. Deepest-existing-ancestor resolve expands Windows 8.3 aliases
    // (`RUNNER~1` → `runneradmin`) and adds a `\\?\` prefix on both the state
    // root and the checked child. Compare after that; still a real containment
    // check (no loose string prefix / no skipped gate).
    let candidate = match resolve_path_for_containment(candidate) {
        Ok(path) => path,
        Err(_) => candidate.to_path_buf(),
    };
    let root = match resolve_path_for_containment(root) {
        Ok(path) => path,
        Err(_) => root.to_path_buf(),
    };
    if candidate.starts_with(&root) {
        return true;
    }

    // Residual alias pairs after resolve are mainly a Windows concern; skip the
    // extra canonicalize walk on Unix so containment stays cheap on hot paths.
    #[cfg(windows)]
    if let Ok(root_canon) = root.canonicalize() {
        let mut cursor = candidate.clone();
        loop {
            if let Ok(canon) = cursor.canonicalize()
                && (canon == root_canon || canon.starts_with(&root_canon))
            {
                return true;
            }
            if !cursor.pop() {
                break;
            }
        }
    }

    #[cfg(windows)]
    {
        windows_components_within(
            &strip_windows_verbatim_prefix(&candidate),
            &strip_windows_verbatim_prefix(&root),
        )
    }
    #[cfg(not(windows))]
    {
        let _ = (candidate, root);
        false
    }
}

#[cfg(windows)]
fn windows_components_within(candidate: &Path, root: &Path) -> bool {
    let mut root_components = root.components();
    let mut candidate_components = candidate.components();
    loop {
        match (root_components.next(), candidate_components.next()) {
            (None, _) => return true,
            (Some(_), None) => return false,
            (Some(r), Some(c)) => {
                use std::os::windows::ffi::OsStrExt as _;
                let fold = |value: &std::ffi::OsStr| {
                    value
                        .encode_wide()
                        .map(|unit| {
                            if (u32::from(b'A')..=u32::from(b'Z')).contains(&(unit as u32)) {
                                unit + (u32::from(b'a') - u32::from(b'A')) as u16
                            } else {
                                unit
                            }
                        })
                        .collect::<Vec<_>>()
                };
                if fold(r.as_os_str()) != fold(c.as_os_str()) {
                    return false;
                }
            }
        }
    }
}

#[cfg(windows)]
fn strip_windows_verbatim_prefix(path: &Path) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy();
    let stripped = raw
        .strip_prefix(r"\\?\")
        .or_else(|| raw.strip_prefix("//?/"))
        .unwrap_or(raw.as_ref());
    PathBuf::from(stripped)
}

fn normalize_subagent_workspace(workspace: &Path) -> PathBuf {
    // Use the same deepest-existing-ancestor resolve as containment checks so a
    // not-yet-created state root still shares the Windows long-path / `\\?\`
    // spelling of paths resolved under it (for example transcript artifacts).
    match resolve_path_for_containment(workspace) {
        Ok(resolved) => resolved,
        Err(_) => {
            let absolute = if workspace.is_absolute() {
                workspace.to_path_buf()
            } else {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join(workspace)
            };
            normalize_path_components(&absolute)
        }
    }
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

fn reject_root_relative_symlinks(root: &Path, path: &Path) -> Result<()> {
    // Walk the caller-supplied spellings with symlink_metadata. Do not
    // canonicalize `path` first — canonicalize follows symlinks and would
    // erase the components this check exists to reject (including a symlink
    // leaf that still resolves inside the state root).
    //
    // Windows `\\?\` / 8.3 identity is aligned by callers
    // (`normalize_subagent_workspace`, `checked_subagent_state_path`, etc.)
    // before they invoke us. If strip_prefix still fails, fall back to
    // resolving the *root* and the path *parent* only, then rejoin the leaf
    // so a symlink leaf stays visible to the walk.
    let (walk_root, relative) = match path.strip_prefix(root) {
        Ok(relative) => (root.to_path_buf(), relative.to_path_buf()),
        Err(_) => {
            let resolved_root = match resolve_path_for_containment(root) {
                Ok(resolved) => resolved,
                Err(_) => root.to_path_buf(),
            };
            let aligned_path = match (path.parent(), path.file_name()) {
                (Some(parent), Some(name)) => {
                    let parent = match resolve_path_for_containment(parent) {
                        Ok(resolved) => resolved,
                        Err(_) => parent.to_path_buf(),
                    };
                    parent.join(name)
                }
                _ => path.to_path_buf(),
            };
            let relative = match aligned_path.strip_prefix(&resolved_root) {
                Ok(relative) => relative.to_path_buf(),
                Err(_) => {
                    #[cfg(windows)]
                    {
                        let path_cmp = strip_windows_verbatim_prefix(&aligned_path);
                        let root_cmp = strip_windows_verbatim_prefix(&resolved_root);
                        path_cmp
                            .strip_prefix(&root_cmp)
                            .map(|relative| relative.to_path_buf())
                            .map_err(|_| {
                                anyhow!(
                                    "sub-agent state path must stay within state root: {}",
                                    path.display()
                                )
                            })?
                    }
                    #[cfg(not(windows))]
                    {
                        return Err(anyhow!(
                            "sub-agent state path must stay within state root: {}",
                            path.display()
                        ));
                    }
                }
            };
            (resolved_root, relative)
        }
    };
    let mut current = walk_root;
    for component in relative.components() {
        current.push(component.as_os_str());
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            return Err(anyhow!(
                "sub-agent state path must not traverse symlinks: {}",
                current.display()
            ));
        }
    }
    Ok(())
}

/// Maximum bytes read from a sub-agent state or transcript artifact file.
/// Generous for long session histories; anything larger is corrupt.
const MAX_SUBAGENT_STATE_BYTES: u64 = 16 * 1024 * 1024;

fn read_subagent_state_file(state_root: &Path, path: &Path) -> Result<String> {
    let state_root = normalize_subagent_workspace(state_root);
    reject_state_path_symlinks(&state_root, path)?;
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() || !file_type.is_file() {
        return Err(anyhow!(
            "sub-agent state path must be a regular file: {}",
            path.display()
        ));
    }

    let file = open_subagent_state_file(path)?;
    let mut raw = String::new();
    file.take(MAX_SUBAGENT_STATE_BYTES + 1)
        .read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_SUBAGENT_STATE_BYTES {
        return Err(anyhow!(
            "sub-agent state file {} exceeds the 16 MiB limit",
            path.display()
        ));
    }
    Ok(raw)
}

#[cfg(unix)]
fn open_subagent_state_file(path: &Path) -> Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(Into::into)
}

#[cfg(not(unix))]
fn open_subagent_state_file(path: &Path) -> Result<fs::File> {
    fs::File::open(path).map_err(Into::into)
}

fn prepare_subagent_transcript_parent(state_root: &Path, path: &Path) -> Result<()> {
    reject_state_path_symlinks(state_root, path)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("sub-agent transcript artifact must have a parent directory"))?;
    fs::create_dir_all(parent)?;
    // Re-check after creation so a pre-existing component cannot redirect the
    // private transcript outside the state root.
    reject_state_path_symlinks(state_root, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn create_private_subagent_transcript(state_root: &Path, path: &Path, bytes: &[u8]) -> Result<()> {
    prepare_subagent_transcript_parent(state_root, path)?;
    let mut file = open_private_subagent_transcript(path, false)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn append_private_subagent_transcript(
    state_root: &Path,
    path: &Path,
    bytes: &[u8],
    durable: bool,
) -> Result<()> {
    reject_state_path_symlinks(state_root, path)?;
    let mut file = open_private_subagent_transcript(path, true)?;
    if !bytes.is_empty() {
        file.write_all(bytes)?;
    }
    if durable {
        file.sync_all()?;
    }
    Ok(())
}

#[cfg(unix)]
fn open_private_subagent_transcript(path: &Path, append: bool) -> Result<fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut options = fs::OpenOptions::new();
    options
        .write(true)
        .append(append)
        .create(!append)
        .truncate(!append)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600);
    let file = options.open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(unix))]
fn open_private_subagent_transcript(path: &Path, append: bool) -> Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .append(append)
        .create(!append)
        .truncate(!append)
        .open(path)
        .map_err(Into::into)
}

pub(crate) fn epoch_millis_now() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        Err(_) => 0,
    }
}

/// Compact preview for follow-up delivery receipts (sibling coord surface).
fn truncate_preview(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn instant_from_duration(duration: Duration) -> Instant {
    Instant::now()
        .checked_sub(duration)
        .unwrap_or_else(Instant::now)
}

/// `persist_state_best_effort` fires a fresh thread per call, so multiple
/// persists of the same `state.json` can be in flight at once; the publish
/// sequence below keeps an older snapshot from replacing a newer one.
static STATE_PUBLISH_SEQUENCES: std::sync::OnceLock<parking_lot::Mutex<HashMap<PathBuf, u64>>> =
    std::sync::OnceLock::new();

fn write_json_atomic(state_root: &Path, path: &Path, value: &PersistedSubAgentState) -> Result<()> {
    let state_root = normalize_subagent_workspace(state_root);
    reject_state_path_symlinks(&state_root, path)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_string_pretty(value)?;
    let publish_sequences =
        STATE_PUBLISH_SEQUENCES.get_or_init(|| parking_lot::Mutex::new(HashMap::new()));
    let mut published = publish_sequences.lock();
    if published
        .get(path)
        .is_some_and(|sequence| *sequence > value.snapshot_sequence)
    {
        return Ok(());
    }
    crate::utils::write_atomic(path, payload.as_bytes())?;
    published.insert(path.to_path_buf(), value.snapshot_sequence);
    Ok(())
}

/// Create a shared sub-agent manager with a configurable limit.
#[cfg(test)]
#[must_use]
pub fn new_shared_subagent_manager(workspace: PathBuf, max_agents: usize) -> SharedSubAgentManager {
    new_shared_subagent_manager_with_timeout(
        workspace,
        max_agents,
        max_agents,
        Duration::from_secs(crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS),
        max_agents,
    )
}

#[must_use]
pub(crate) fn new_shared_worker_authority(workspace: PathBuf) -> SharedSubAgentManager {
    new_shared_subagent_manager_with_timeout(
        workspace,
        MAX_SUBAGENTS,
        MAX_SUBAGENTS,
        Duration::from_secs(crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS),
        MAX_SUBAGENTS,
    )
}

/// Create a shared sub-agent manager with configurable concurrency and stale
/// running-agent heartbeat timeout.
#[must_use]
pub fn new_shared_subagent_manager_with_timeout(
    workspace: PathBuf,
    max_agents: usize,
    max_admitted_agents: usize,
    running_heartbeat_timeout: Duration,
    launch_concurrency: usize,
) -> SharedSubAgentManager {
    let state_root = workspace.clone();
    new_shared_subagent_manager_with_state_root_and_timeout(
        workspace,
        state_root,
        max_agents,
        max_admitted_agents,
        running_heartbeat_timeout,
        launch_concurrency,
        None,
        None,
    )
}

pub(crate) fn new_shared_runtime_subagent_manager(
    workspace: PathBuf,
    workers: usize,
) -> Result<SharedSubAgentManager> {
    type RuntimeManagerRegistry =
        parking_lot::Mutex<HashMap<PathBuf, (usize, std::sync::Weak<RwLock<SubAgentManager>>)>>;
    static MANAGERS: std::sync::OnceLock<RuntimeManagerRegistry> = std::sync::OnceLock::new();
    let workers = workers.clamp(1, MAX_SUBAGENTS);
    let state_path = default_state_path(&workspace)?;
    let key = checked_subagent_state_path(&workspace, &state_path)?;
    let mut managers = MANAGERS
        .get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
        .lock();
    managers.retain(|_, (_, manager)| manager.strong_count() > 0);
    if let Some((capacity, manager)) = managers.get(&key)
        && let Some(manager) = manager.upgrade()
    {
        anyhow::ensure!(
            *capacity == workers,
            "Runtime worker authority already has aggregate capacity {capacity}; requested {workers}. Attach to its owner or stop it before changing workers."
        );
        return Ok(manager);
    }
    let mut manager = SubAgentManager::new(workspace.clone(), workers)
        .with_admission_limit(workers)
        .with_launch_concurrency(workers)
        .with_state_path(state_path);
    manager.coordination_exclusive = true;
    manager = manager.require_coordination_process_lock();
    manager
        .ensure_coordination_process_lock()
        .map_err(anyhow::Error::msg)?;
    manager.load_state()?;
    let manager = Arc::new(RwLock::new(manager));
    managers.insert(key, (workers, Arc::downgrade(&manager)));
    Ok(manager)
}

/// Create a shared sub-agent manager with an explicit control-plane state root.
///
/// `workspace` remains the child execution and file-authority root. The worker
/// ledger, complete transcript artifacts and coordination lock live under
/// `state_root/.codewhale/state`. Distinct state roots intentionally do not
/// share write claims; an embedding host that runs them against the same
/// workspace must provide any required cross-session write coordination.
#[allow(clippy::too_many_arguments)] // legacy open constructor
#[must_use]
pub fn new_shared_subagent_manager_with_state_root_and_timeout(
    workspace: PathBuf,
    state_root: PathBuf,
    max_agents: usize,
    max_admitted_agents: usize,
    running_heartbeat_timeout: Duration,
    launch_concurrency: usize,
    default_max_steps: Option<u32>,
    default_wall_time: Option<Duration>,
) -> SharedSubAgentManager {
    let max_agents = max_agents.clamp(1, MAX_SUBAGENTS);
    let state_path = match default_state_path(&state_root) {
        Ok(path) => Some(path),
        Err(err) => {
            tracing::warn!(target: "subagent", ?err, "failed to resolve sub-agent state path");
            None
        }
    };
    let manager = if state_root == workspace {
        SubAgentManager::new(workspace, max_agents)
    } else {
        SubAgentManager::new_with_state_root(workspace, state_root, max_agents)
    };
    let mut manager = manager
        .with_admission_limit(max_admitted_agents)
        .with_running_heartbeat_timeout(running_heartbeat_timeout)
        .with_launch_concurrency(launch_concurrency)
        .with_default_max_steps(default_max_steps)
        .with_default_wall_time(default_wall_time);
    if let Some(state_path) = state_path {
        manager = manager.with_state_path(state_path);
    }
    manager = manager.require_coordination_process_lock();
    if let Err(error) = manager.ensure_coordination_process_lock() {
        tracing::warn!(target: "subagent", %error, "this session cannot append to the shared coordination ledger");
    }
    // Loading is a read. It was previously gated behind the *write* flock,
    // which left a second session in the same workspace blind to every
    // decision and write claim already recorded — and holding a default,
    // empty ledger that would be written straight over the real one the
    // moment the first session exited and the flock became acquirable
    // (owner report, 2026-08-04). Read unconditionally; the write gate on
    // `persist_state`/`persist_state_synchronously` still checks this
    // manager's coordinator admission before publishing.
    if let Err(err) = manager.load_state() {
        // Routed through tracing instead of stderr — see comment in
        // `persist_state_best_effort` above.
        tracing::warn!(target: "subagent", ?err, "failed to load sub-agent state");
    }
    Arc::new(RwLock::new(manager))
}

// === Tool Implementations ===

/// Start a child agent task through a single simplified model-facing surface.
pub struct AgentTool {
    manager: SharedSubAgentManager,
    runtime: SubAgentRuntime,
    /// Last projection fingerprint per agent, used to throttle repeat
    /// peek/status calls that observe no change (#4097). Std mutex: locked
    /// only for brief map reads/writes, never across an await.
    inspect_memo: Arc<std::sync::Mutex<HashMap<String, PeekMemo>>>,
}

/// Fingerprint of the last peek/status response for one agent (#4097).
#[derive(Debug, Clone, Copy)]
struct PeekMemo {
    fingerprint: u64,
    at: Instant,
}

impl AgentTool {
    #[must_use]
    pub fn new(manager: SharedSubAgentManager, runtime: SubAgentRuntime) -> Self {
        Self {
            manager,
            runtime,
            inspect_memo: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentToolAction {
    Start,
    Roster,
    Status,
    Peek,
    Message,
    Followup,
    Interrupt,
    Wait,
    Cancel,
    /// Expand this caller's write claim before mutating outside it (#5462).
    ///
    /// The one coordination action that had no equivalent on `agent`: write
    /// scope could only be widened through `agents/coordinate action=claim`,
    /// so retiring that tool from the catalog without this action would have
    /// left fail-closed write enforcement with no in-band way to satisfy it.
    Claim,
    /// Clear write claims whose owner is no longer a live claimant (#5906).
    ///
    /// The contention refusal and the shared-checkout peer gate both told the
    /// caller to run `agents/coordinate action=release`, a tool that is no
    /// longer in any catalog — so the one remediation named by the only error
    /// that needs it was unreachable from the surface that raised it. Like
    /// `claim`, this adds no authority: `release_stale_write_claims` never
    /// removes a live claimant's claim, whoever asks.
    Release,
}

fn parse_agent_tool_action(input: &Value) -> Result<AgentToolAction, ToolError> {
    let Some(action) = optional_input_str(input, &["action", "op"])? else {
        return Ok(AgentToolAction::Start);
    };
    match action.trim().to_ascii_lowercase().as_str() {
        "" | "start" | "spawn" | "run" => Ok(AgentToolAction::Start),
        "roster" | "members" | "profiles" => Ok(AgentToolAction::Roster),
        "status" | "list" | "inspect" => Ok(AgentToolAction::Status),
        "peek" | "progress" => Ok(AgentToolAction::Peek),
        "message" | "queue_message" => Ok(AgentToolAction::Message),
        "followup" | "follow_up" | "steer" => Ok(AgentToolAction::Followup),
        "interrupt" | "pause" => Ok(AgentToolAction::Interrupt),
        "wait" | "join" | "await" | "block" => Ok(AgentToolAction::Wait),
        "cancel" | "stop" | "abort" => Ok(AgentToolAction::Cancel),
        "claim" => Ok(AgentToolAction::Claim),
        "release" | "release_claim" => Ok(AgentToolAction::Release),
        other => Err(ToolError::invalid_input(format!(
            "Invalid agent action '{other}'. Use start, roster, status, peek, message, followup, interrupt, wait, claim, release, or cancel."
        ))),
    }
}

/// Translate `agent{action:"claim", ...}` into the `agents/coordinate` wire.
///
/// Two things this function exists to get right, both of which fail *silently*
/// when they are wrong:
///
/// 1. The coordinate wire key is `roots`, not `write_roots`
///    ([`AgentsCoordinateTool::execute`] reads `roots`). A translation that
///    forwarded `write_roots` would hand `expand_write_claim` three empty
///    lists, which returns the unchanged claim with `Ok` — a successful
///    receipt for an expansion that never happened, and then a fail-closed
///    write refusal the model cannot explain.
/// 2. An all-empty claim is refused here. `expand_write_claim` treats it as a
///    no-op success for the same reason, so without this check a call that
///    forgot its scope would read as "claim granted".
///
/// The three field names are the ones `agent action=start` already uses for
/// write scope (`write_roots` advertised; `exact_files` and
/// `coordination_contracts` parse-accepted), so one vocabulary describes a
/// child's scope whether it is declared at launch or widened later.
fn agent_claim_coordinate_input(input: &Value) -> Result<Value, ToolError> {
    let roots = parse_coordination_paths(input, "write_roots")?;
    let exact_files = parse_coordination_paths(input, "exact_files")?;
    let contracts = parse_bounded_strings(input, "coordination_contracts", 16)?;
    if roots.is_empty() && exact_files.is_empty() && contracts.is_empty() {
        return Err(ToolError::invalid_input(
            "agent action=claim needs at least one scope entry: write_roots, exact_files, or coordination_contracts. An empty claim would report success while expanding nothing."
                .to_string(),
        ));
    }
    Ok(json!({
        "action": "claim",
        "roots": roots,
        "exact_files": exact_files,
        "contracts": contracts,
    }))
}

fn parse_agent_ref(input: &Value) -> Result<Option<String>, ToolError> {
    Ok(
        optional_input_str(input, &["agent_id", "id", "session_name", "name"])?
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    )
}

/// #5186: whether an `agent action=start` call asks for exactly a canonical
/// read-only Fleet role and nothing that could widen it. Those spawns run
/// without an approval modal in the default posture because the child's own
/// captured `ChildAuthority` enforces read-only behavior from the inside.
///
/// Anything this parser cannot prove read-only stays gated: no role token
/// (defaults to `worker`), an unparseable role, a `profile`
/// reference, a conflicting type/role pair, or an explicit write authority.
fn start_requests_read_only_role(input: &Value) -> bool {
    // A parameter this function cannot even read is not proof of anything,
    // so a type error fails closed into the approval modal. `execute` then
    // refuses the call outright with the named-parameter error.
    // Provisioning a git worktree creates a branch and a checkout on disk,
    // so it is never read-only whatever the role.
    if !matches!(parse_optional_worktree_request(input), Ok(None)) {
        return false;
    }
    let read = |keys: &[&str]| optional_input_str(input, keys).map(|v| v.map(str::to_string));
    let Ok(profile) = read(&["profile", "fleet_profile", "roster_profile"]) else {
        return false;
    };
    if profile.is_some() {
        return false;
    }
    let Ok(type_input) = read(&["type", "agent_type", "agent_name"]) else {
        return false;
    };
    let Ok(role_input) = read(&["role", "agent_role"]) else {
        return false;
    };
    let type_input = type_input.as_deref();
    let role_input = role_input.as_deref();
    let parsed_type = type_input.and_then(FleetRole::from_str);
    let parsed_role = role_input.and_then(FleetRole::from_str);
    let role = match (parsed_type, parsed_role) {
        (Some(from_type), Some(from_role)) if from_type == from_role => from_type,
        // A second token that does not parse as a canonical role may be a
        // role name resolved later — fail closed like `profile`.
        (Some(from_type), None) if role_input.is_none() => from_type,
        (None, Some(from_role)) if type_input.is_none() => from_role,
        _ => return false,
    };
    let Ok(write_authority) = read(&["write_authority", "writeAuthority"]) else {
        return false;
    };
    if write_authority.is_some_and(|authority| !authority.trim().eq_ignore_ascii_case("read_only"))
    {
        return false;
    }
    matches!(
        role,
        FleetRole::Scout
            | FleetRole::Planner
            | FleetRole::Reviewer
            | FleetRole::Verifier
            | FleetRole::Consultant
    )
}

/// The `agent` tool's model-facing description. Role names here must be the
/// canonical `FLEET_ROLE_SCHEMA_VALUES` the `type` enum accepts: the legacy
/// aliases (worker, scout, builder, verifier, consultant) still parse at the
/// deserialization boundary but a strict gateway rejects them against the
/// declared enum (#5940). `agent_tool_description_names_only_schema_roles`
/// pins this. Field descriptions own lifecycle and scope details; do not
/// repeat them here on every request.
static AGENT_TOOL_DESCRIPTION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let description = concat!(
        "Start with action=start and prompt; returns an agent_id immediately. Read-only roles need no extra fields. Healthy children continue after an ordinary parent response; detached=true also opts out of parent-turn cancellation. ",
        "Use Workflow for dependencies; use multiple starts for independent tasks. ",
        "type selects the Fleet role: general (full tool access for multi-step tasks), explore (fast read-only exploration), planner (grounded strategy, read-only probes), reviewer (reads and grades code), implement (lands focused code changes), test (runs tests and reports evidence), advisor (read-only design counsel), or custom (allowed_tools on the parent's posture). ",
        "profile selects a saved member or built-in role. Saved profile and manual role pins are exact; model/strength choose unpinned routes; thinking overrides the tier. ",
        "Use action=roster for resolved roles, models, reasoning, context and cost evidence; it makes no provider request. ",
        "max_steps and wall_time_secs only narrow inherited limits; the worker reserves one tools-disabled partial-report turn and falls back to recorded work when reporting is unavailable. There is no token budget: runs are never stopped by token accounting. ",
        "Prefer type=implement for write work and type=test (or the Run tool with action=\"verifiers\") after writes settle — dispatch is not completion. ",
        "action=claim widens your own enforced write scope: pass write_roots (and optionally exact_files, coordination_contracts) before mutating anything a fail-closed write refusal named. It records a durable claim receipt and fails on contention with a peer claim; it never touches another agent's scope. ",
        "Action contract: start requires prompt; message/followup require targets and a message; followup accepts one id, agent_ids or all_parked=true and returns continuation mappings. peek/interrupt/cancel require a target; claim requires scope entries; status is compact and paginated unless an addressed detail is requested. ",
        "In Operate, a child the main session starts edits files and runs the Run tool's built-in checks without a second approval; any other shell command follows the session's approval settings. ",
        "Legacy action=status|peek|cancel remain for compatibility."
    );
    format!("{description} {}", subagent_followup_recovery("<agent_id>"))
});

#[async_trait]
impl ToolSpec for AgentTool {
    fn name(&self) -> &'static str {
        "agent"
    }

    fn description(&self) -> &'static str {
        AGENT_TOOL_DESCRIPTION.as_str()
    }

    /// One schema advertises the runtime's routing, scope, lifecycle and
    /// narrowing-budget contract. Legacy aliases remain accepted for replay.
    fn input_schema(&self) -> Value {
        // Generic provider sanitization prunes required fields unless the
        // same schema node declares them. Presence tests inside `not` need
        // those property stubs too, or every followup branch becomes false.
        let field_present = |field: &str| {
            json!({
                "properties": {(field): {}}, "required": [field]
            })
        };
        let target_required = json!([field_present("agent_id"), field_present("name")]);
        let followup_targets = json!([
            {
                "anyOf": target_required.clone(),
                "not": {"anyOf": [field_present("agent_ids"), field_present("all_parked")]}
            },
            {
                "properties": {"agent_ids": {"uniqueItems": true}},
                "required": ["agent_ids"],
                "not": {"anyOf": [field_present("agent_id"), field_present("name"), field_present("all_parked")]}
            },
            {
                "properties": {"all_parked": {"const": true}},
                "required": ["all_parked"],
                "not": {"anyOf": [field_present("agent_id"), field_present("name"), field_present("agent_ids")]}
            }
        ]);
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["start", "roster", "status", "peek", "message", "followup", "interrupt", "wait", "claim", "release", "cancel"],
                    "description": "start launches a worker and returns immediately. Healthy workers continue after an ordinary parent response. roster lists roles with their resolved routes and capability/cost evidence. status/peek inspect running or retained workers. message queues a note without waking a running child. followup delivers notes to a running child or continues an interrupted child from its checkpoint; use the returned agent_id for subsequent waits/messages. Retrying followup on the original interrupted id reuses its successor. Bulk targets return individual continuation mappings and errors. interrupt stops the current turn while preserving the child checkpoint. wait only observes; see until. claim widens your own enforced write scope (see write_roots). release clears write claims whose owner is no longer running; pass agent_id to clear one, omit it to sweep. cancel permanently cancels a running child."
                },
                "until": {
                    "type": "string",
                    "enum": ["completion", "all", "activity"],
                    "description": "For action=wait. A wait blocks until one child settles or the timeout (default 30s, max 120s) elapses; on timeout the receipt reports timed_out=true with any already-settled children, and full results still arrive as completion sentinels. completion (default) returns when any one child settles. all returns only once every child running at call time has settled, with each outcome — the fan-out join: start the batch, make one wait, then synthesize. activity also returns on progress."
                },
                "agent_id": {
                    "type": "string",
                    "description": "Agent id or session name for any action except start and unscoped status/wait."
                },
                "agent_ids": {
                    "type": "array", "minItems": 1, "maxItems": 32,
                    "items": {"type": "string", "minLength": 1},
                    "description": "For followup, a bounded batch of ids. Choose exactly one target form: agent_id/name, agent_ids, or all_parked=true."
                },
                "all_parked": {
                    // Kimi MFJS cannot represent boolean const or array
                    // uniqueness. Keep those action-dependent constraints
                    // above, where the existing provider pass drops them
                    // with a limitation note. Runtime validation still owns
                    // target selection for every provider.
                    "type": "boolean",
                    "description": "For followup, continue all parked children you control, with individual mappings and errors. Mutually exclusive with ids."
                },
                "detail": {
                    "type": "boolean",
                    "description": "For addressed status/peek with agent_id, include a bounded page of detailed messages/events. Unscoped status always stays compact."
                },
                "offset": {"type": "integer", "minimum": 0, "description": "Status/detail page offset; use the returned next_offset."},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20, "description": "Maximum rows per status/detail page, default 20."},
                "message": {
                    "type": "string", "minLength": 1,
                    "description": "Parent note for action=message or action=followup. message queues only; followup wakes a running child or continues a parked child from its checkpoint."
                },
                "name": {
                    "type": "string",
                    "description": "For action=start, optional stable session name. For status/peek/cancel, accepted as an alias for agent_id."
                },
                "prompt": {
                    "type": "string",
                    "description": "The focused task to give the worker. A read-only role needs no write scope; a write-capable role defaults to the parent workspace unless narrowed with write_roots."
                },
                "detached": {
                    "type": "boolean",
                    "description": "Default children continue after ordinary parent turn completion and remain explicitly cancellable. true additionally opts this subtree out of parent-turn cancellation; its own budgets still apply."
                },
                "type": {
                    "type": "string",
                    "enum": FLEET_ROLE_SCHEMA_VALUES,
                    "description": SUBAGENT_TYPE_DESCRIPTION
                },
                "profile": {
                    "type": "string",
                    "description": "Saved member id or built-in role from action=roster. Saved instructions, route and depth bounds apply; conflicting model/type pins are refused. thinking may override the saved tier. A role:NAME selector matching several members is refused as ambiguous — list exact member ids with action=roster and pass one."
                },
                "allowed_tools": {
                    "type": "array",
                    "items": {"type": "string", "minLength": 1},
                    "description": "Tool names for this child. Required and non-empty for type=custom, which is refused without it; for other roles narrows the inherited registry. Blank entries are dropped and duplicates merged."
                },
                "model": {
                    "type": "string",
                    "description": "For an unpinned role, choose an exact model or provider/model from roster models, plus the session model. Roster member rows show each role's resolved route, i.e. which models each role accepts. A selected Fleet constrains task choices to those routes; saved profile and manual role pins remain exact. With no selected models, current-provider overrides remain available."
                },
                "model_strength": {
                    "type": "string",
                    "enum": ["same", "faster"],
                    "description": "For an unpinned role: same inherits the session model; faster requests its provider's faster candidate. An explicit task model precedes strength. Saved profile and manual role pins refuse strength changes. Inspect roster for resolved routes."
                },
                "thinking": {
                    "type": "string",
                    "enum": SUBAGENT_THINKING_SCHEMA_VALUES,
                    "description": "Requested reasoning effort, normalized to the selected route's supported values. inherit uses role defaults then session effort; auto considers this task."
                },
                "worktree": {
                    "type": "boolean",
                    "description": "When true, create a fresh git worktree and branch for this child before it starts. Use for parallel edit tasks that must not collide with the parent checkout."
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory for this child. With worktree=true it becomes the worktree root; otherwise the child's workspace. Pass it when the parent directory holds several checkouts and the spawn names no single repository."
                },
                "write_roots": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Repo-relative directory trees a write-capable agent may mutate. On action=start: the scope this child claims, defaulting to the parent workspace ('.') when omitted. On action=claim: the trees to add to your own enforced scope, which you must do before mutating anything outside it. Paths outside the parent workspace are refused."
                },
                "max_output_tokens": {
                    "type": "integer", "minimum": 1,
                    "description": "Per-tool-result output token cap for this child (#6282). Overrides the 10k default; narrower values conserve context but may truncate tool results with [truncated: true] markers. Values above u32::MAX are rejected."
                },
                "max_steps": {
                    "type": "integer", "minimum": 1, "maximum": MAX_SUBAGENT_STEPS,
                    "description": "Maximum model turns for this run, only narrowing role/operator/parent limits. Continuation retains its remaining allowance."
                },
                "wall_time_secs": {
                    "type": "integer", "minimum": 1, "maximum": MAX_CHILD_WALL_TIME.as_secs(),
                    "description": "Whole-run wall time (queue, model, tools); default 1800, may exceed that but never inherited or operator limits. Continuation keeps the clock."
                },
                "deliverables": {
                    "type": "array", "maxItems": 16,
                    "items": {"type": "string", "minLength": 1},
                    "description": "Repo-relative files expected at completion, checked for scope, regular-file existence and nonempty contents."
                },
                "expected_artifact": {
                    "type": "string",
                    "description": "Human-readable expected outcome; declare file paths separately in deliverables for runtime checks."
                },
                "write_authority": {
                    "type": "string", "enum": ["read_only", "workspace_write", "worktree_write"],
                    "description": "Optional typed narrowing of the child's write permissions. read_only admits no write scope; worktree_write requires worktree isolation. type=custom declaring write_roots, exact_files, or coordination_contracts must pass workspace_write or worktree_write explicitly; custom without scopes defaults to read_only."
                },
                "exact_files": {
                    "type": "array", "items": {"type": "string", "minLength": 1},
                    "description": "Repo-relative files the write-capable child claims, or files added by action=claim. Prefer disjoint files when peers own neighboring paths; files beneath a claimed write_root bind that root to just those files, so peers can share one root with disjoint outputs."
                },
                "coordination_contracts": {
                    "type": "array", "items": {"type": "string", "minLength": 1},
                    "description": "Named coordination contracts claimed by the child or added by action=claim. Peer contention is refused."
                },
                "fork_context": {
                    "type": "boolean",
                    "description": "For start: true = child starts from your conversation prefix (cache-shared); default fresh. Never grants permissions."
                },
                "resume_from": {
                    "type": "string",
                    "description": "Settled child agent_id or session name to fork into a new independent worker; each start makes another, so use action=followup to continue parked work. Its full transcript becomes the new child's context, continuing the lineage under a new role or profile (e.g. explore → implementer → verifier). Refused with fork_context=false, or for cross-workspace or missing sources."
                }
            },
            "dependentSchemas": {
                "action": {
                    "anyOf": [
                        {
                            "properties": {
                                "action": {"const": "start"},
                                "prompt": {}
                            },
                            "required": ["prompt"]
                        },
                        {
                            "properties": {"action": {"const": "roster"}}
                        },
                        {
                            "properties": {"action": {"const": "status"}}
                        },
                        {
                            "properties": {"action": {"const": "peek"}},
                            "anyOf": target_required.clone()
                        },
                        {
                            "properties": {
                                "action": {"const": "message"},
                                "message": {}
                            },
                            "required": ["message"],
                            "anyOf": target_required.clone()
                        },
                        {
                            "properties": {
                                "action": {"const": "followup"},
                                "message": {}
                            },
                            "required": ["message"],
                            "oneOf": followup_targets
                        },
                        {
                            "properties": {"action": {"const": "interrupt"}},
                            "anyOf": target_required.clone()
                        },
                        {
                            "properties": {"action": {"const": "wait"}}
                        },
                        {
                            // `claim` names no agent: it always widens the
                            // caller's own scope. Which of the three scope
                            // lists carries the entries is left to the call —
                            // `execute` refuses an empty claim rather than
                            // silently succeeding with no expansion.
                            "properties": {"action": {"const": "claim"}}
                        },
                        {
                            // `release` names no *required* target: with
                            // `agent_id` it clears that one stale claim, and
                            // without it sweeps every claim whose owner is no
                            // longer live. Neither form can touch a live
                            // claimant's claim.
                            "properties": {"action": {"const": "release"}}
                        },
                        {
                            "properties": {"action": {"const": "cancel"}},
                            "anyOf": target_required
                        }
                    ]
                }
            },
            // Model-facing calls choose an action explicitly so the matching
            // dependent schema is always active. The parser still defaults a
            // missing action to start for stored and programmatic legacy calls.
            "required": ["action"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![
            ToolCapability::ExecutesCode,
            ToolCapability::RequiresApproval,
        ]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    /// #3801: status and peek are read-only queries — no approval needed.
    /// #4097: wait passively observes children — also read-only.
    /// #5186: starting an explicitly read-only role no longer modals in the
    /// default posture; the child's own posture gates keep it read-only from
    /// the inside. Write-capable spawns keep their gate.
    fn approval_requirement_for(&self, input: &Value) -> ApprovalRequirement {
        match parse_agent_tool_action(input) {
            Ok(
                AgentToolAction::Roster
                | AgentToolAction::Status
                | AgentToolAction::Peek
                | AgentToolAction::Wait,
            ) => ApprovalRequirement::Auto,
            Ok(AgentToolAction::Start) if start_requests_read_only_role(input) => {
                ApprovalRequirement::Auto
            }
            // #5462: `agents/coordinate` declared `Auto` because gating a
            // coordination record deadlocks autonomous fan-in — a child that
            // must widen its scope to proceed cannot raise a modal in a
            // parent UI nobody is watching. Inheriting the action must
            // inherit that reasoning, not quietly upgrade it to `Required`.
            // Authority is unchanged: `claim` can only widen the *caller's
            // own* scope, and contention with a peer claim still fails.
            Ok(AgentToolAction::Claim) => ApprovalRequirement::Auto,
            // Same reasoning, and the same bounded authority: `release` only
            // drops claims whose owner is already not a live claimant, and a
            // child that is refused every write until a dead claim clears
            // cannot raise a modal in a parent UI nobody is watching (#5906).
            Ok(AgentToolAction::Release) => ApprovalRequirement::Auto,
            _ => ApprovalRequirement::Required,
        }
    }

    /// #3801: only explicit `detached=true` starts durable work that should not
    /// hold the global tool-exec write lock while the child spins up. Foreground
    /// starts still return promptly, but stay owned by their active turn.
    fn starts_detached_for(&self, input: &Value) -> bool {
        matches!(parse_agent_tool_action(input), Ok(AgentToolAction::Start))
            && input.get("detached").and_then(Value::as_bool) == Some(true)
    }

    /// #3801: Read-only `agent` actions (status, peek) can safely run in
    /// parallel batches.
    fn supports_parallel_for(&self, input: &Value) -> bool {
        matches!(
            parse_agent_tool_action(input),
            Ok(AgentToolAction::Roster) | Ok(AgentToolAction::Status) | Ok(AgentToolAction::Peek)
        )
    }

    /// #3801: status/peek actions are read-only queries of manager state.
    /// #4097: wait only observes child lifecycle — read-only as well.
    fn is_read_only_for(&self, input: &Value) -> bool {
        matches!(
            parse_agent_tool_action(input),
            Ok(AgentToolAction::Roster
                | AgentToolAction::Status
                | AgentToolAction::Peek
                | AgentToolAction::Wait)
        )
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let action = parse_agent_tool_action(&input)?;
        match action {
            AgentToolAction::Start => {
                let remote_set = !matches!(input.get("remote"), None | Some(Value::Null));
                let runtime_is_local = match input.get("runtime") {
                    None | Some(Value::Null) => true,
                    Some(Value::String(value)) => {
                        matches!(value.trim().to_ascii_lowercase().as_str(), "" | "local")
                    }
                    Some(_) => false,
                };
                if remote_set || !runtime_is_local {
                    return Err(ToolError::invalid_input(
                        "Cloud agent runs are no longer started from the agent tool; omit runtime and remote to run a local child.",
                    ));
                }
            }
            AgentToolAction::Roster => {
                let mut runtime = self.runtime.clone();
                refresh_spawn_route_sources(&mut runtime);
                let roster = spawn_roster(&runtime);
                let mut members = Vec::new();
                for role in FleetRole::all() {
                    members.push(resolved_role_roster_entry(&runtime, &roster, &role).await);
                }
                let mut profiles = Vec::new();
                for member in roster
                    .members()
                    .iter()
                    .filter(|member| member.origin != crate::fleet::roster::ProfileOrigin::BuiltIn)
                    .take(64)
                {
                    profiles.push(resolved_profile_roster_entry(&runtime, &roster, member).await);
                }
                let mut model_rows = Vec::new();
                let selected_models =
                    crate::fleet::members::fleet_models(&runtime.context.workspace);
                if let Ok(models) = &selected_models {
                    for model in models.iter().take(64) {
                        let selector = format!("{}/{}", model.provider, model.model);
                        model_rows.push(resolved_spawn_roster_entry(
                            &runtime, &roster,
                            json!({"provider": model.provider, "model": model.model, "roles": model.roles,
                                "selector": {"model": selector}}),
                            json!({"prompt": "Preview selected model.", "type": "general", "model": selector}),
                            false,
                        ).await);
                    }
                }
                let payload = json!({
                    "action": "roster",
                    "count": members.len(),
                    "total_count": members.len(),
                    "truncated": false,
                    "members": members,
                    "models": model_rows,
                    "model_total_count": selected_models.as_ref().map_or(0, Vec::len),
                    "model_load_error": selected_models.err().map(|error| error.to_string()),
                    "model_help": "Choose an exact model selector with an unpinned type. The session model is allowed for unpinned roles. Model rows describe the selected route independently of role pins. Empty models retains current-provider model/strength choices. Manual role and saved profile pins remain exact.",
                    "profiles": profiles,
                    "profile_count": profiles.len(),
                    "profile_total_count": roster.members().iter().filter(|member| member.origin != crate::fleet::roster::ProfileOrigin::BuiltIn).count(),
                    "profile_load_error": roster.load_error(),
                    "profile_load_issues": roster.profile_load_issues().iter().take(64).collect::<Vec<_>>(),
                    "profile_load_issue_count": roster.profile_load_issues().len(),
                    "selector_help": "Use type:<role> for its posture and configured role pin, or profile:<member_id> for a saved member with its instructions and exact route. Explicit profiles precede manual role pins, then unique saved role pins. model/model_strength choose only unpinned routes; thinking may override the saved tier.",
                });
                let mut result = ToolResult::json(&payload)
                    .map_err(|error| ToolError::execution_failed(error.to_string()))?;
                result.metadata = Some(json!({
                    "action": "roster",
                    "count": payload["count"],
                }));
                return Ok(result);
            }
            AgentToolAction::Status | AgentToolAction::Peek => {
                return inspect_agent_from_input(
                    &input,
                    self.manager.clone(),
                    context,
                    matches!(action, AgentToolAction::Peek),
                    Some(&self.inspect_memo),
                )
                .await;
            }
            AgentToolAction::Message => {
                return AgentsMessageTool::new(self.manager.clone())
                    .with_optional_caller(self.runtime.parent_agent_id.clone())
                    .execute(input, context)
                    .await;
            }
            AgentToolAction::Followup => {
                return AgentsFollowupTool::new(self.manager.clone())
                    .with_optional_caller(self.runtime.parent_agent_id.clone())
                    .with_runtime(self.runtime.clone())
                    .execute(input, context)
                    .await;
            }
            AgentToolAction::Interrupt => {
                return AgentsInterruptTool::new(self.manager.clone())
                    .with_optional_caller(self.runtime.parent_agent_id.clone())
                    .execute(input, context)
                    .await;
            }
            AgentToolAction::Wait => {
                // Shared with `agents/wait` so `until` (completion | all |
                // activity) means the same thing on both surfaces.
                return coord::dispatch_wait(&input, self.manager.clone(), context).await;
            }
            AgentToolAction::Cancel => {
                return cancel_agent_from_input(
                    &input,
                    self.manager.clone(),
                    context,
                    self.runtime.parent_agent_id.as_deref(),
                )
                .await;
            }
            AgentToolAction::Claim => {
                return AgentsCoordinateTool::new(
                    self.manager.clone(),
                    self.runtime.parent_agent_id.clone(),
                )
                .execute(agent_claim_coordinate_input(&input)?, context)
                .await;
            }
            AgentToolAction::Release => {
                let mut coordinate_input = json!({"action": "release"});
                if let Some(agent_ref) = parse_agent_ref(&input)? {
                    // Targeted release accepts a session name like every other
                    // targeted action; the ledger keys claims by agent id, and
                    // a headless worker id that resolves to no agent row is
                    // passed through unchanged.
                    let owner = {
                        let manager = self.manager.read().await;
                        manager
                            .resolve_agent_ref(&agent_ref)
                            .unwrap_or_else(|_| agent_ref.clone())
                    };
                    coordinate_input["owner"] = json!(owner);
                }
                return AgentsCoordinateTool::new(
                    self.manager.clone(),
                    self.runtime.parent_agent_id.clone(),
                )
                .execute(coordinate_input, context)
                .await;
            }
        }
        touch_running_shell_owners(
            &self.manager,
            &context.execution.shell_manager,
            &context.state_namespace,
        )
        .await;
        let verbose = input
            .get("verbose")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let (snapshot, _spawn_metadata) = spawn_subagent_from_input(
            input,
            self.manager.clone(),
            self.runtime.clone(),
            false,
            None,
        )
        .await?;
        let worker_record = {
            let manager = self.manager.read().await;
            manager.get_worker_record_for_session(&context.state_namespace, &snapshot.agent_id)
        };
        let projection =
            subagent_session_projection(&self.manager, snapshot, false, context, worker_record)
                .await;
        let mut value = serde_json::to_value(&projection)
            .map_err(|e| ToolError::execution_failed(e.to_string()))?;
        compact_spawn_receipt(&mut value, verbose);
        let mut tool_result =
            ToolResult::json(&value).map_err(|e| ToolError::execution_failed(e.to_string()))?;
        let metadata = json!({
            "action": "start",
            "agent_id": projection.agent_id,
            "status": projection.status,
            "terminal": projection.terminal,
            "context_mode": projection.context_mode,
            "prefix_cache": projection.prefix_cache,
            "child_route": value["child_route"],
        });
        tool_result.metadata = Some(metadata);
        Ok(tool_result)
    }
}

/// A spawn receipt is an acknowledgement, not an archive: the full projection
/// carried the complete child prompt (launch_manifest inside `worker_record`)
/// plus a duplicated `snapshot` — ~12KB per spawn (morning-report issue #4).
/// Same compaction contract as unscoped status (9fa5e04e6): strip the heavy
/// keys, say so, and let `verbose: true` restore the old shape.
fn compact_spawn_receipt(value: &mut Value, verbose: bool) {
    if verbose {
        return;
    }
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.remove("snapshot");
    // Context mode already states whether the prefix is inherited. These
    // repeated descriptions otherwise crowd out the effective budget receipt.
    object.remove("prefix_cache");
    object.remove("fork_context");
    object.remove("fleet_profile");
    if object.get("run_id") == object.get("agent_id") {
        object.remove("run_id");
    }
    if let Some(name) = object.get("name").and_then(Value::as_str) {
        let name = lifecycle::text_preview(name, 64);
        object.insert("name".into(), json!(name));
    }
    if let Some(route) = object.get_mut("child_route") {
        *route = lifecycle::compact_child_route(std::mem::take(route));
    }
    if let Some(follow_up) = object.get_mut("follow_up").and_then(Value::as_object_mut) {
        follow_up.remove("session_name");
    }
    if let Some(usage) = object.get_mut("usage").and_then(Value::as_object_mut) {
        usage.remove("note");
    }
    if let Some(profile) = object
        .get("worker_record")
        .and_then(|worker| worker.pointer("/spec/runtime_profile"))
    {
        let mut limits = serde_json::Map::new();
        for key in [
            "spawn_depth",
            "max_spawn_depth",
            "max_steps",
            "wall_time_secs",
            "wall_deadline_ms",
        ] {
            if let Some(value) = profile.get(key).filter(|value| !value.is_null()) {
                limits.insert(key.to_string(), value.clone());
            }
        }
        object.insert("effective_limits".to_string(), Value::Object(limits));
    }
    if let Some(paths) = object
        .get("worker_record")
        .and_then(|worker| worker.pointer("/spec/launch_manifest/deliverables"))
        .filter(|paths| paths.as_array().is_some_and(|paths| !paths.is_empty()))
        .cloned()
    {
        object.insert("deliverables".to_string(), paths);
    }
    object.remove("worker_record");
    object.remove("checkpoint");
    object.remove("artifacts");
    object.remove("takeover");
    object.remove("transcript_handle");
    object.remove("verification");
    object.insert("compact".to_string(), json!(true));
    object.insert(
        "compact_note".to_string(),
        json!("Inspect with agent_id, detail=true; start verbose: true."),
    );
    // Declared output paths can be numerous and long. Keep every path that
    // fits verbatim and make omissions explicit; addressed detail has them all.
    let mut omitted = 0;
    while serde_json::to_vec(&object)
        .is_ok_and(|bytes| bytes.len() > lifecycle::COMPACT_SPAWN_BYTES)
    {
        if object
            .get_mut("deliverables")
            .and_then(Value::as_array_mut)
            .and_then(Vec::pop)
            .is_none()
        {
            break;
        }
        omitted += 1;
        object.insert("deliverables_omitted".into(), json!(omitted));
    }
}

/// Repeat peek/status calls on an unchanged running child inside this window
/// return a compact "no change" nudge instead of a full projection (#4097).
const PEEK_UNCHANGED_THROTTLE_WINDOW: Duration = Duration::from_secs(30);

/// Read bounded supervision facts, or an explicitly addressed diagnostic page.
async fn inspect_agent_from_input(
    input: &Value,
    manager: SharedSubAgentManager,
    context: &ToolContext,
    peek: bool,
    inspect_memo: Option<&Arc<std::sync::Mutex<HashMap<String, PeekMemo>>>>,
) -> Result<ToolResult, ToolError> {
    let include_archived =
        parse_optional_bool(input, &["include_archived", "includeArchived"])?.unwrap_or(false);

    let agent_ref = parse_agent_ref(input)?;
    let detail = parse_optional_bool(input, &["detail", "verbose"])?.unwrap_or(false);
    let (offset, limit) = lifecycle::page(input)?;
    if agent_ref.is_none() || !detail {
        touch_running_shell_owners(
            &manager,
            &context.execution.shell_manager,
            &context.state_namespace,
        )
        .await;
        let mut manager = manager.write().await;
        manager.cleanup_for_session(&context.state_namespace, COMPLETED_AGENT_RETENTION);
        let payload = if let Some(agent_ref) = agent_ref.as_ref() {
            let source = manager
                .resolve_agent_ref_for_session(&context.state_namespace, agent_ref)
                .map_err(|error| ToolError::invalid_input(error.to_string()))?;
            let target = manager
                .continuation_target(&source)
                .map_err(|error| ToolError::invalid_input(error.to_string()))?;
            let agent = manager.agents.get(&target).expect("resolved agent");
            let mut row = lifecycle::compact_row(&manager, agent);
            row["addressed_agent_id"] = json!(source);
            row["action"] = json!(if peek { "peek" } else { "status" });
            if agent.status == SubAgentStatus::Running
                && let Some(memo_map) = inspect_memo
            {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                agent.steps_taken.hash(&mut hasher);
                agent.result.is_some().hash(&mut hasher);
                agent.needs_input.is_some().hash(&mut hasher);
                agent.checkpoint.is_some().hash(&mut hasher);
                manager.activity_fingerprint(&target).hash(&mut hasher);
                let fingerprint = hasher.finish();
                let now = Instant::now();
                let mut memo_map = memo_map
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let unchanged = memo_map.get(&target).is_some_and(|memo| {
                    memo.fingerprint == fingerprint
                        && now.duration_since(memo.at) < PEEK_UNCHANGED_THROTTLE_WINDOW
                });
                memo_map.insert(
                    target.clone(),
                    PeekMemo {
                        fingerprint,
                        at: now,
                    },
                );
                if unchanged {
                    row["unchanged"] = json!(true);
                    row["hint"] = json!(
                        "No change. Use one bounded wait, continue other work, or answer the user; children keep running and report when done."
                    );
                }
            }
            row["detail_hint"] =
                json!("Use detail=true with agent_id for a bounded diagnostic page.");
            row
        } else {
            lifecycle::compact_roster(
                &manager,
                input,
                &context.state_namespace,
                include_archived,
                peek,
            )?
        };
        let evicted = manager.drain_pending_handle_evictions_for_session(&context.state_namespace);
        drop(manager);
        if !evicted.is_empty() {
            let mut store = context.runtime.handle_store.lock().await;
            for id in evicted {
                store.evict_session(&format!("agent:{id}"));
            }
        }
        return lifecycle::status_result(payload, peek);
    }

    if let Some(agent_ref) = parse_agent_ref(input)? {
        let (snapshot, worker_record, evicted_ids, compact) = {
            touch_running_shell_owners(
                &manager,
                &context.execution.shell_manager,
                &context.state_namespace,
            )
            .await;
            let mut manager = manager.write().await;
            manager.cleanup_for_session(&context.state_namespace, COMPLETED_AGENT_RETENTION);
            let evicted_ids =
                manager.drain_pending_handle_evictions_for_session(&context.state_namespace);
            let source = manager
                .resolve_agent_ref_for_session(&context.state_namespace, &agent_ref)
                .map_err(|error| ToolError::invalid_input(error.to_string()))?;
            let target = manager
                .continuation_target(&source)
                .map_err(|error| ToolError::invalid_input(error.to_string()))?;
            let snapshot = manager
                .get_result_by_ref_for_session(&context.state_namespace, &target)
                .map_err(|error| ToolError::invalid_input(error.to_string()))?;
            let worker_record =
                manager.get_worker_record_for_session(&context.state_namespace, &snapshot.agent_id);
            let mut compact = lifecycle::compact_row(
                &manager,
                manager.agents.get(&target).expect("resolved agent"),
            );
            compact["addressed_agent_id"] = json!(source);
            (snapshot, worker_record, evicted_ids, compact)
        };
        // Evict retired handles outside the manager lock (#3885).
        if !evicted_ids.is_empty() {
            let mut store = context.runtime.handle_store.lock().await;
            for agent_id in &evicted_ids {
                store.evict_session(&format!("agent:{agent_id}"));
            }
        }

        let mut projection =
            subagent_session_projection(&manager, snapshot, false, context, worker_record).await;
        projection.resumed_from = compact
            .get("resumed_from")
            .and_then(Value::as_str)
            .map(str::to_string);
        projection.resumed_as = compact
            .get("resumed_as")
            .and_then(Value::as_str)
            .map(str::to_string);
        let value = serde_json::to_value(&projection)
            .map_err(|error| ToolError::execution_failed(error.to_string()))?;
        let payload = lifecycle::bounded_detail(value, compact, offset, limit);
        return lifecycle::status_result(payload, peek);
    }

    unreachable!("unaddressed status returned through the compact roster")
}

/// Keep a running child alive while one of its tracked background shell jobs
/// is still active. Shell output is progress owned by the child even before
/// the next model-visible completion event is emitted.
async fn touch_running_shell_owners(
    manager: &SharedSubAgentManager,
    shell_manager: &SharedShellManager,
    active_session_id: &str,
) {
    let owner_ids = {
        let Ok(mut shell_manager) = shell_manager.lock() else {
            return;
        };
        shell_manager.running_owner_agent_ids_for_session(active_session_id)
    };
    if owner_ids.is_empty() {
        return;
    }
    let mut manager = manager.write().await;
    for owner_id in owner_ids {
        manager.touch_for_session(active_session_id, &owner_id);
    }
}

async fn cancel_agent_from_input(
    input: &Value,
    manager: SharedSubAgentManager,
    context: &ToolContext,
    caller_agent_id: Option<&str>,
) -> Result<ToolResult, ToolError> {
    let agent_ref = parse_agent_ref(input)?.ok_or_else(|| ToolError::missing_field("agent_id"))?;
    let (snapshot, worker_record) = {
        let mut manager = manager.write().await;
        manager
            .continuation_target_for_caller(
                &context.state_namespace,
                &agent_ref,
                caller_agent_id,
                "agent/cancel",
            )
            .map_err(|err| ToolError::invalid_input(err.to_string()))?;
        let snapshot = manager
            .cancel_agent_for_session(&context.state_namespace, &agent_ref)
            .map_err(|err| ToolError::invalid_input(err.to_string()))?;
        let worker_record =
            manager.get_worker_record_for_session(&context.state_namespace, &snapshot.agent_id);
        (snapshot, worker_record)
    };
    let snapshot = settle_requested_child(&manager, snapshot).await;
    let snapshot = preserve_cancelled_work(&manager, snapshot).await;
    let worker_record = manager
        .read()
        .await
        .get_worker_record_for_session(&context.state_namespace, &snapshot.agent_id)
        .or(worker_record);
    let projection =
        subagent_session_projection(&manager, snapshot, false, context, worker_record).await;
    let mut tool_result = ToolResult::json(&projection)
        .map_err(|err| ToolError::execution_failed(err.to_string()))?;
    tool_result.metadata = Some(json!({
        "action": "cancel",
        "status": projection.status,
        "terminal": projection.terminal,
        "agent_id": projection.agent_id,
    }));
    Ok(tool_result)
}

/// Bounds for `agent(action="wait")` (#4097). The default is short so a
/// `wait` does not make the session deaf: the turn is blocked for at most
/// this long and user messages have nowhere to land. Results arrive
/// automatically as `<codewhale:subagent.done>` sentinels, so ending the
/// turn and staying reachable is the preferred default — only `wait` when
/// you must join before continuing.
const SUBAGENT_WAIT_DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Runtime floor is 1s (schema advertises 5) so tests can exercise the
/// timeout path without multi-second sleeps.
const SUBAGENT_WAIT_MIN_TIMEOUT_SECS: u64 = 1;
const SUBAGENT_WAIT_MAX_TIMEOUT_SECS: u64 = 120;
/// Internal state-check cadence while blocked. Invisible to the model — the
/// #4097 anti-pattern is model-visible polling that burns turns and tokens,
/// not a cheap in-process timer.
const SUBAGENT_WAIT_CHECK_INTERVAL: Duration = Duration::from_millis(250);

/// `agent(action="wait")`: block until a running child settles (leaves
/// `Running` — completed, failed, cancelled, interrupted/needs-input, or
/// budget-exhausted), then return a compact summary. Full child results are
/// still delivered as `<codewhale:subagent.done>` sentinels by the runtime;
/// this call only provides the legitimate "join" the model previously faked
/// with peek→sleep loops (#4097).
///
/// With `agent_id`, waits for that child specifically. Without it, waits for
/// the next child to settle (returning every child that settled while
/// blocked). Returns immediately when nothing is running. Cancel-safe: the
/// engine turn's cancel token interrupts the block, and no lock is held
/// across an await.
///
/// This is the `until=completion` arm only. `until=all` — the fan-out join
/// that blocks for *every* watched child — lives in
/// [`coord::dispatch_wait`]; both arms are reachable from `agents/wait` and
/// from `agent(action="wait")`.
async fn wait_for_subagents_from_input(
    input: &Value,
    manager: SharedSubAgentManager,
    context: &ToolContext,
) -> Result<ToolResult, ToolError> {
    let timeout_secs = parse_optional_u64(input, &["timeout_secs", "timeout"])?
        .unwrap_or(SUBAGENT_WAIT_DEFAULT_TIMEOUT_SECS)
        .clamp(
            SUBAGENT_WAIT_MIN_TIMEOUT_SECS,
            SUBAGENT_WAIT_MAX_TIMEOUT_SECS,
        );
    let timeout = Duration::from_secs(timeout_secs);
    let agent_ref = parse_agent_ref(input)?;

    // Resolve the watch set up front so a bad reference fails immediately
    // instead of blocking for the full timeout.
    let watched: Vec<String> = {
        let manager = manager.read().await;
        if let Some(agent_ref) = &agent_ref {
            let snapshot = manager
                .get_result_by_ref_for_session(&context.state_namespace, agent_ref)
                .map_err(|err| ToolError::invalid_input(err.to_string()))?;
            if snapshot.status != SubAgentStatus::Running {
                let running = manager.running_count_for_session(&context.state_namespace);
                drop(manager);
                return wait_result_payload(&[snapshot], &[], running, 0, false).await;
            }
            vec![snapshot.agent_id]
        } else {
            manager
                .list_filtered_for_session(&context.state_namespace, false)
                .into_iter()
                .filter(|snapshot| snapshot.status == SubAgentStatus::Running)
                .map(|snapshot| snapshot.agent_id)
                .collect()
        }
    };

    if watched.is_empty() {
        let payload = json!({
            "action": "wait",
            "settled": [],
            "running": 0,
            "note": "No running sub-agents; nothing to wait for.",
        });
        let mut tool_result = ToolResult::json(&payload)
            .map_err(|err| ToolError::execution_failed(err.to_string()))?;
        tool_result.metadata = Some(json!({ "action": "wait", "settled": 0, "running": 0 }));
        return Ok(tool_result);
    }

    let started = Instant::now();
    let cancelled = async {
        match &context.cancel_token {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(cancelled);

    loop {
        let (settled, running) = {
            let manager = manager.read().await;
            let mut settled = Vec::new();
            for agent_id in &watched {
                if let Ok(snapshot) =
                    manager.get_result_by_ref_for_session(&context.state_namespace, agent_id)
                    && snapshot.status != SubAgentStatus::Running
                {
                    settled.push(snapshot);
                }
            }
            (
                settled,
                manager.running_count_for_session(&context.state_namespace),
            )
        };

        if !settled.is_empty() || running == 0 {
            return wait_result_payload(
                &settled,
                &[],
                running,
                started.elapsed().as_millis(),
                false,
            )
            .await;
        }
        // A watched child is blocked on a person (approvals C2): return now
        // so the parent can tell the user, instead of waiting out the
        // timeout. Reported ids do not re-wake a later wait.
        let needs_person = take_new_needs_person(&manager, &watched).await;
        if !needs_person.is_empty() {
            return wait_result_payload(
                &[],
                &needs_person,
                running,
                started.elapsed().as_millis(),
                false,
            )
            .await;
        }
        if started.elapsed() >= timeout {
            return wait_result_payload(&[], &[], running, started.elapsed().as_millis(), true)
                .await;
        }

        tokio::select! {
            () = &mut cancelled => {
                return Ok(ToolResult::success(
                    "Wait interrupted by user cancellation before any sub-agent settled.",
                ));
            }
            () = tokio::time::sleep(SUBAGENT_WAIT_CHECK_INTERVAL) => {}
        }
    }
}

/// Compact `action=wait` result. Deliberately not a full projection: the
/// runtime's completion sentinels (and a follow-up peek on a settled child)
/// carry the full payload; duplicating it here would double token cost.
/// The watched agents' pending requests not yet reported to the parent,
/// marked reported. Takes the write lock only when there is something new.
pub(super) async fn take_new_needs_person(
    manager: &SharedSubAgentManager,
    watched: &[String],
) -> Vec<Value> {
    if !manager.read().await.has_unreported_needs_person(watched) {
        return Vec::new();
    }
    manager.write().await.take_unreported_needs_person(watched)
}

/// Note for a wait that returned because a person must decide something.
pub(super) const NEEDS_PERSON_WAIT_NOTE: &str = "A child agent is blocked on a decision only the user can make (see needs_person). Tell the user which agent is waiting and what it wants to run — they answer on that agent's approval card; you cannot approve it. Do not replace or re-dispatch the agent for this.";

async fn wait_result_payload(
    settled: &[SubAgentResult],
    needs_person: &[Value],
    running: usize,
    waited_ms: u128,
    timed_out: bool,
) -> Result<ToolResult, ToolError> {
    let settled_entries: Vec<Value> = settled
        .iter()
        .map(|snapshot| {
            json!({
                "agent_id": snapshot.agent_id,
                "name": snapshot.name,
                "status": subagent_status_name(&snapshot.status),
            })
        })
        .collect();
    let note = if !needs_person.is_empty() {
        NEEDS_PERSON_WAIT_NOTE
    } else if timed_out {
        "Wait timed out with children still running. Do not poll — wait again (until=\"all\" blocks for the whole batch), continue independent work, or end your turn; results arrive automatically as <codewhale:subagent.done> sentinels."
    } else if settled_entries.is_empty() {
        "No sub-agents are running anymore."
    } else {
        "Full results arrive as <codewhale:subagent.done> sentinels — read those before synthesizing; do not re-peek settled children unless you need the full projection."
    };
    let mut payload = json!({
        "action": "wait",
        "settled": settled_entries,
        "running": running,
        "waited_ms": u64::try_from(waited_ms).unwrap_or(u64::MAX),
        "timed_out": timed_out,
        "note": note,
    });
    if !needs_person.is_empty() {
        payload["needs_person"] = json!(needs_person);
    }
    let mut tool_result =
        ToolResult::json(&payload).map_err(|err| ToolError::execution_failed(err.to_string()))?;
    tool_result.metadata = Some(json!({
        "action": "wait",
        "settled": settled.len(),
        "running": running,
        "timed_out": timed_out,
        "needs_person": needs_person.len(),
    }));
    Ok(tool_result)
}

async fn spawn_subagent_from_input(
    input: Value,
    manager: SharedSubAgentManager,
    mut runtime: SubAgentRuntime,
    verified_posture_denials: bool,
    workflow_identity: Option<&WorkflowTaskSpawnIdentity>,
) -> Result<(SubAgentResult, WorkflowTaskSpawnMetadata), ToolError> {
    let exact_fleet_binding =
        workflow_identity.and_then(|identity| identity.exact_fleet_binding.as_ref());
    apply_session_spawn_defaults(&mut runtime);
    if exact_fleet_binding.is_none() {
        refresh_spawn_route_sources(&mut runtime);
    }
    let mut spawn_request = parse_spawn_request(&input)?;
    if !verified_posture_denials {
        // Keep caller restrictions in the inherited context before adding
        // structural role sentinels to the worker profile. A read-only role
        // exception must never erase an explicit ancestor/operator denial.
        merge_spawn_disallowed_tools(&mut runtime.context.disallowed_tools, &spawn_request);
    }
    let requested_route = RequestedChildRoute {
        requested_type: spawn_request.agent_type.as_str().to_string(),
        requested_profile: spawn_request.profile.clone(),
        requested_reasoning: subagent_thinking_label(spawn_request.thinking).to_string(),
    };
    let profile_member = match exact_fleet_binding {
        Some(binding) => {
            let member = binding.spawn_profile();
            apply_spawn_profile(&mut spawn_request, &member)?;
            Some(member)
        }
        None => resolve_spawn_route_profile(&runtime, &mut spawn_request, &spawn_roster(&runtime))?,
    };
    // Role resolution runs before classification so the write contract sees
    // the effective role: read-only roles stay ergonomic, and a write-capable
    // role with no declared scope claims the workspace root ('.'), which the
    // coordination ledger then arbitrates against live peers.
    validate_spawn_write_contract(&mut spawn_request, false)?;

    if runtime.would_exceed_depth() {
        return Err(ToolError::execution_failed(format!(
            "Sub-agent depth limit reached (current depth {}, max {}). \
             Increase via [subagents] max_depth in config.toml.",
            runtime.spawn_depth, runtime.max_spawn_depth
        )));
    }

    let mut child_runtime = if spawn_request.detached {
        runtime.background_runtime()
    } else {
        runtime.child_runtime()
    };
    // #6282: an explicit per-result output cap only narrows the inherited
    // cap — a spawn request can never widen it. Zero is rejected at parse
    // time, so a positive value translates cleanly.
    if let Some(n) = spawn_request
        .max_output_tokens
        .and_then(std::num::NonZeroU32::new)
    {
        child_runtime.max_output_tokens =
            narrow_optional_limit(child_runtime.max_output_tokens, Some(n));
    }
    // File reads, git and transcript loads below run on the blocking pool,
    // not the async worker (#6561 D02-09).
    let resident_context = match spawn_request.resident_file.clone() {
        Some(file_path) => {
            let context = runtime.context.clone();
            Some(
                spawn_prep_blocking(move || read_bounded_resident_context(&context, &file_path))
                    .await?,
            )
        }
        None => None,
    };
    let effective_prompt = assemble_spawn_prompt(&spawn_request, resident_context.as_ref());
    let (model_route, route_source, fallback_note) = bind_spawn_model_route(
        &mut child_runtime,
        &spawn_request,
        profile_member.as_ref(),
        true,
        // Exact-bound spawns refuse provider substitution: their route is
        // preflighted and "will not guess or fall back".
        exact_fleet_binding.is_none(),
    )
    .await?;
    // The pause is per provider route, so it is checked against the route
    // the child was just bound to, not the parent's: a 429 on the parent's
    // provider must not refuse a child headed to another provider or a local
    // runtime (and a child bound to a paused route is refused either way).
    if let Some(remaining) =
        crate::retry_status::rate_limit_remaining(&child_runtime.client.rate_limit_scope())
    {
        let seconds = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
        return Err(ToolError::execution_failed(format!(
            "Provider is rate-limiting; sub-agent spawning is paused for {seconds}s. \
             Wait for the current backoff window before starting new agent work."
        )));
    }
    if let Some(binding) = exact_fleet_binding {
        let provider = child_runtime.api_config.as_ref().map_or_else(
            || child_runtime.client.api_provider().as_str().to_string(),
            |_| {
                child_runtime
                    .client
                    .admitted_provider_identity()
                    .key
                    .to_string()
            },
        );
        if provider != binding.route.provider_id
            || child_runtime.model != binding.route.wire_model
            || codewhale_workflow::EndpointIdentity::from_base_url(child_runtime.client.base_url())
                != binding.route.endpoint
        {
            return Err(ToolError::permission_denied(
                "The exact Fleet route changed between preflight and spawn; restart the workflow with the current provider configuration.",
            ));
        }
    }
    let effective_model = child_runtime.model.clone();
    let child_route = mint_child_route_receipt(
        &requested_route,
        &spawn_request,
        profile_member.as_ref(),
        &child_runtime,
        effective_model.clone(),
        route_source.as_str(),
        fallback_note.as_deref(),
    )?;

    if spawn_request.worktree.is_some() {
        let manager_guard = manager.read().await;
        manager_guard
            .check_admission_capacity()
            .map_err(|err| ToolError::execution_failed(err.to_string()))?;
    }
    // Every later refusal (resume_from, resident lease, admission, a name
    // already in use) would otherwise leave the new checkout and its branch
    // behind, one more per failed attempt. Disarmed once the child is live.
    // The guard is armed inside the blocking task, so a spawn future dropped
    // while git creates the worktree still removes it when the task ends.
    let origin_guard = manager.clone().read_owned().await;
    let origin_workspace = runtime.context.workspace.clone();
    let origin_session = runtime.context.state_namespace.clone();
    let origin_parent = runtime.parent_agent_id.clone();
    let captured_origin = codewhale_app_server::daemon_socket::owner_work(move || {
        origin_guard
            .capture_direct_worker_origin_for_runtime(
                &origin_workspace,
                &origin_session,
                origin_parent.as_deref(),
            )
            .map_err(anyhow::Error::msg)
    })
    .await
    .map_err(|error| ToolError::execution_failed(error.to_string()))?;
    let (child_workspace, mut pending_worktree) = {
        let parent_workspace = runtime.context.workspace.clone();
        let cwd = spawn_request.cwd.clone();
        let worktree = spawn_request.worktree.clone();
        let session_name = spawn_request.session_name.clone();
        let agent_type = spawn_request.agent_type.clone();
        spawn_prep_blocking(move || {
            let child_workspace = prepare_child_workspace(
                &parent_workspace,
                cwd.as_deref(),
                worktree.as_ref(),
                session_name.as_deref(),
                &agent_type,
            )?;
            let pending = PendingChildWorktree(
                child_workspace
                    .as_ref()
                    .filter(|_| worktree.is_some())
                    .cloned(),
            );
            Ok((child_workspace, pending))
        })
        .await?
    };

    child_runtime.max_spawn_depth = child_max_spawn_depth_for_spawn(
        child_runtime.max_spawn_depth,
        child_runtime.spawn_depth,
        match (
            spawn_request.max_depth,
            profile_member
                .as_ref()
                .and_then(|member| member.profile.delegation.max_spawn_depth),
        ) {
            (Some(task), Some(profile)) => Some(task.min(profile)),
            (task, profile) => task.or(profile),
        },
    );
    if let Some(workspace) = child_workspace {
        child_runtime.context.workspace = workspace.clone();
        // A worktree child gets a distinct workspace-scoped plugin catalog.
        // Reusing the parent's registry here would leak workspace plugins (and
        // their authority receipts) across the exact isolation boundary the
        // child requested. User-global roots remain available through the
        // registry's frozen pre-dotenv discovery context.
        if let Some(parent_plugins) = child_runtime.context.plugin_registry.as_ref() {
            child_runtime.context.plugin_registry =
                Some(parent_plugins.rediscover_for_workspace(&workspace));
        }
    }
    // Denials are an ancestor/operator ceiling. The legacy inherit=false
    // spelling is accepted for saved calls, but cannot widen that ceiling.
    merge_spawn_disallowed_tools(
        &mut child_runtime.worker_profile.denied_tools,
        &spawn_request,
    );
    apply_spawn_write_authority(&mut child_runtime, &spawn_request);
    let write_capable = spawn_request_is_write_capable(&spawn_request);
    let write_claim = write_capable.then(|| WriteScopeClaim {
        owner: String::new(),
        roots: spawn_request.write_roots.clone(),
        exact_files: spawn_request.exact_files.clone(),
        contracts: spawn_request.coordination_contracts.clone(),
    });
    let mut spawn_metadata = WorkflowTaskSpawnMetadata {
        resolved_provider: child_route.provider_id.clone(),
        resolved_model: child_route.model_id.clone(),
        route_source: child_route.route_source.clone(),
        requested_reasoning: Some(child_route.requested_reasoning.clone()),
        effective_reasoning: child_route.effective_reasoning.clone(),
        resolved_role: Some(child_route.canonical_role.clone()),
        resolved_profile: child_route.resolved_profile_id.clone(),
        child_route: child_route.clone(),
        parent_task_id: child_runtime.parent_agent_id.clone(),
        depth: child_runtime.spawn_depth,
        workflow_run_id: None,
        workflow_phase_id: None,
        workflow_task_label: None,
        workflow_child_index: None,
        resume_from_agent_id: None,
    };

    // #4647: a child receives only its explicit objective/dependencies/
    // acceptance and relevant accepted-decision projection by default. Forking
    // a parent transcript is still available, but only through an explicit
    // `fork_context: true` request.
    let fork_context = spawn_request.fork_context.unwrap_or(false);

    // resolve_resume_from: look up the source agent, validate it is settled
    // and lives in the same workspace, then load its transcript so the new
    // child inherits the full lineage (issue #425).
    let (fork_context, resume_from_agent_id) = if let Some(ref source_ref) =
        spawn_request.resume_from
    {
        // Validate: fork_context=false is incompatible with resume_from.
        if spawn_request.fork_context == Some(false) {
            return Err(ToolError::invalid_input(
                "resume_from requires fork_context to be true or unset; \
                 explicit fork_context=false conflicts with transcript continuation."
                    .to_string(),
            ));
        }
        let (source_agent_id, source_workspace, source_state_root, checkpoint_messages) = {
            let manager_read = manager.read().await;
            let source_id = manager_read
                .resolve_agent_ref_for_session(&runtime.context.state_namespace, source_ref)
                .map_err(|_| {
                    ToolError::invalid_input(format!(
                        "resume_from: agent or session '{source_ref}' not found. \
                     Use agent action=status to list available agents."
                    ))
                })?;
            manager_read
                .ensure_caller_controls_descendant_for_session(
                    &runtime.context.state_namespace,
                    &source_id,
                    runtime.parent_agent_id.as_deref(),
                    "agent/resume_from",
                )
                .map_err(|err| ToolError::invalid_input(err.to_string()))?;
            let source = manager_read.agents.get(&source_id).ok_or_else(|| {
                ToolError::invalid_input(format!("resume_from: agent '{source_id}' not found"))
            })?;
            if source.status == SubAgentStatus::Running {
                return Err(ToolError::invalid_input(format!(
                    "resume_from: agent '{source_id}' (session '{}') is still running. \
                     Only settled agents (completed, interrupted, failed, cancelled) \
                     may be used as a resume source. Use action=wait to block until \
                     it settles, or action=interrupt to stop it.",
                    source.session_name
                )));
            }
            // Capture checkpoint messages now while holding the read lock; used
            // as a fallback when the transcript artifact is unavailable.
            let checkpoint_messages = source
                .checkpoint
                .as_ref()
                .filter(|cp| cp.continuable && !cp.messages.is_empty())
                .map(|cp| cp.messages.clone())
                .unwrap_or_default();
            (
                source_id,
                source.workspace.clone(),
                manager_read.state_root.clone(),
                checkpoint_messages,
            )
        };
        // Cross-workspace resume remains unsupported because execution
        // authority and inherited context belong to the source workspace,
        // even when the transcript artifact itself has a separate state root.
        let parent_workspace = normalize_subagent_workspace(&runtime.context.workspace);
        let source_workspace_normalized = normalize_subagent_workspace(&source_workspace);
        if parent_workspace != source_workspace_normalized {
            return Err(ToolError::invalid_input(format!(
                "resume_from: source agent '{source_agent_id}' lives in a different \
                 workspace ({}) than this agent ({}). Cross-workspace continuation \
                 is not supported.",
                source_workspace.display(),
                runtime.context.workspace.display()
            )));
        }
        // Load the full transcript from the on-disk artifact. Fall back to the
        // checkpoint messages for legacy records that predate transcript
        // artifacts or for agents whose artifacts were cleaned up.
        let transcript_agent_id = source_agent_id.clone();
        let messages = spawn_prep_blocking(move || {
            Ok(load_subagent_transcript_artifact(&source_state_root, &transcript_agent_id).ok())
        })
        .await?
        .unwrap_or(checkpoint_messages);
        let resume_ctx = SubAgentForkContext {
            messages,
            // Resume forks a settled agent: nothing is live in the
            // provider cache, so inheritance stays off by construction.
            live_header: crate::prompt_zones::new_live_header_cell(),
            structured_state_block: None,
            work_source: None,
        };
        child_runtime.fork_context = Some(resume_ctx);
        spawn_metadata.resume_from_agent_id = Some(source_agent_id.clone());
        (true, Some(source_agent_id))
    } else {
        (fork_context, None)
    };

    let resident_lease = resident_context
        .as_ref()
        .map(|resident| (resident.lease_key.clone(), resident.display_path.clone()));
    if let Some((lease_key, display_path)) = resident_lease.as_ref() {
        reserve_resident_lease(lease_key, display_path)?;
    }
    // #6210: fingerprint the delivery baseline BEFORE the manager write
    // lock. The child workspace is final here (worktree creation already
    // ran); write capability resolves under the lock, so capture assuming
    // write and let registration discard it for readers. Skipped only when
    // the parent ceiling already denies writes — `derive_child` intersects,
    // so that child is definitely read-only. A join failure falls back to
    // the inline capture, exactly as before.
    let precomputed_delivery_evidence = if child_runtime.worker_profile.permissions.write {
        let workspace = child_runtime.context.workspace.clone();
        tokio::task::spawn_blocking(move || DeliveryEvidence::capture_for_handle(&workspace, true))
            .await
            .ok()
    } else {
        None
    };
    let mut manager_guard = manager.write().await;

    let result = manager_guard.spawn_background_with_assignment_options(
        Arc::clone(&manager),
        child_runtime,
        spawn_request.agent_type,
        effective_prompt,
        spawn_request.assignment,
        spawn_request.allowed_tools,
        SubAgentSpawnOptions {
            origin: Some(captured_origin),
            name: spawn_request.session_name.clone(),
            model: Some(effective_model),
            model_route: Some(model_route),
            child_route: Some(child_route),
            // A workflow child goes by its task label everywhere: the label is
            // its explicit nickname, so every snapshot, event and host reads
            // the same name (#6565).
            nickname: workflow_identity
                .and_then(|identity| identity.workflow_task_label.as_deref())
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_string),
            fork_context,
            max_output_tokens: spawn_request
                .max_output_tokens
                .and_then(std::num::NonZeroU32::new),
            max_steps: spawn_request.max_steps,
            wall_time: spawn_request.wall_time,
            write_claim,
            isolated_worktree: spawn_request.worktree.is_some(),
            expected_artifact: spawn_request.expected_artifact.clone(),
            deliverables: spawn_request.deliverables.clone(),
            resume_from_agent_id: resume_from_agent_id.clone(),
            checkpoint_continuation: false,
            claim_pre_namespaced: false,
            preserve_runtime_profile: None,
            workflow_run_id: workflow_identity.map(|identity| identity.workflow_run_id.clone()),
        },
        precomputed_delivery_evidence,
    );
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            if let Some((lease_key, _)) = resident_lease.as_ref() {
                rollback_pending_resident_lease(lease_key);
            }
            return Err(ToolError::execution_failed(format!(
                "Failed to spawn sub-agent: {error}"
            )));
        }
    };

    if let Some((lease_key, _)) = resident_lease.as_ref() {
        commit_resident_lease(lease_key, &result.agent_id);
    }
    pending_worktree.0 = None;

    Ok((result, spawn_metadata))
}

/// Run synchronous spawn preparation (file reads, git) on the blocking pool.
async fn spawn_prep_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        ToolError::execution_failed(format!("sub-agent spawn preparation failed: {error}"))
    })?
}

/// An isolated worktree created for a spawn that has not started yet. If the
/// spawn fails, or its future is dropped, the checkout and its new branch are
/// removed. Removal runs git and deletes a directory, so inside a runtime it
/// goes to the blocking pool rather than stall an async worker.
struct PendingChildWorktree(Option<PathBuf>);

impl Drop for PendingChildWorktree {
    fn drop(&mut self) {
        let Some(worktree) = self.0.take() else {
            return;
        };
        let remove = move || worktree::remove_unstarted_worktree(&worktree);
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn_blocking(remove);
            }
            Err(_) => remove(),
        }
    }
}

const CHILD_ROUTE_RECEIPT_MAX_BYTES: usize = 1024;

fn assemble_spawn_prompt(request: &SpawnRequest, resident: Option<&ResidentContext>) -> String {
    let prompt = match resident {
        Some(resident) => format!(
            "<!-- resident_file: {} -->\n```\n{}\n```\n\n{}",
            resident.display_path, resident.contents, request.prompt
        ),
        None => request.prompt.clone(),
    };
    let prompt = match request.expected_artifact.as_deref() {
        Some(artifact) => {
            format!("{prompt}\n\nExpected artifact (declared by the spawner): {artifact}")
        }
        None => prompt,
    };
    let prompt = if request.deliverables.is_empty() {
        prompt
    } else {
        format!(
            "{prompt}\n\nRequired file deliverables (checked at completion): {}",
            request.deliverables.join(", ")
        )
    };
    if request.dependencies.is_empty() && request.acceptance.is_empty() {
        return prompt;
    }
    let lines = |items: &[String]| {
        items
            .iter()
            .map(|item| format!("- {}", item.chars().take(256).collect::<String>()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "{prompt}\n\nDelegation contract (bounded):\nDependencies:\n{}\nAcceptance:\n{}",
        lines(&request.dependencies),
        lines(&request.acceptance),
    )
}

fn mint_child_route_receipt(
    requested_route: &RequestedChildRoute,
    request: &SpawnRequest,
    member: Option<&crate::fleet::profile::AgentProfile>,
    runtime: &SubAgentRuntime,
    model_id: String,
    route_source: &str,
    fallback_note: Option<&str>,
) -> Result<ChildRouteReceipt, ToolError> {
    // Identity comes from the same member snapshot used to bind this child.
    let canonical_role = request
        .assignment
        .role
        .as_deref()
        .map(public_role_label)
        .unwrap_or_else(|| request.agent_type.as_str().to_string());
    let provider_id = runtime.client.admitted_provider_identity().key.to_string();
    let receipt = ChildRouteReceipt {
        requested_type: requested_route.requested_type.clone(),
        requested_profile: requested_route.requested_profile.clone(),
        resolved_profile_id: member.map(|member| member.id.clone()),
        profile_origin: member.map(|member| member.origin.to_string()),
        canonical_role,
        provider_id,
        model_id,
        route_source: route_source.to_string(),
        fallback_note: fallback_note.map(str::to_string),
        requested_reasoning: requested_route.requested_reasoning.clone(),
        effective_reasoning: runtime.reasoning_effort.clone(),
        runtime_version: env!("CARGO_PKG_VERSION").to_string(),
        // CI builds embed the full 40-hex GITHUB_SHA; a full-length field
        // pushed real receipts past 384 bytes (394 with the sha, 386 on some
        // legitimate routes without it), breaking admission. Twelve hex chars
        // plus the version identify the build without bloating every receipt.
        runtime_build_sha: option_env!("CODEWHALE_BUILD_COMMIT")
            .map(|sha| sha.get(..12).unwrap_or(sha).to_string())
            .unwrap_or_else(|| "unknown".to_string()),
    };
    let length = serde_json::to_vec(&receipt)
        .map_err(|error| ToolError::execution_failed(error.to_string()))?
        .len();
    if length > CHILD_ROUTE_RECEIPT_MAX_BYTES {
        return Err(ToolError::invalid_input(format!(
            "resolved child route receipt is {length} bytes; the {CHILD_ROUTE_RECEIPT_MAX_BYTES}-byte limit prevents an oversized admission record"
        )));
    }
    Ok(receipt)
}

fn apply_spawn_write_authority(runtime: &mut SubAgentRuntime, request: &SpawnRequest) {
    if request.write_authority != Some(SpawnWriteAuthority::ReadOnly) {
        return;
    }
    // `read_only` must be an executable posture, not just metadata. Normally
    // write-capable identities also inherit Full shell, which could mutate the
    // workspace without a scope-aware claim under Auto/Full Access. Narrow it
    // to the existing classifier-bounded inspection shell, not a file-only
    // surface. A parent with no shell stays shell-less; verifier keeps its
    // deliberate test runner. The grant and executor enforce the same boundary.
    runtime.worker_profile.permissions.write = false;
    if matches!(
        request.agent_type,
        FleetRole::Worker | FleetRole::Builder | FleetRole::Custom
    ) {
        runtime.worker_profile.shell = runtime.worker_profile.shell.min_with(ShellPolicy::ReadOnly);
    }
}

fn spawn_request_is_write_capable(request: &SpawnRequest) -> bool {
    match request.agent_type {
        FleetRole::Worker | FleetRole::Builder => {
            request.write_authority != Some(SpawnWriteAuthority::ReadOnly)
        }
        FleetRole::Custom => matches!(
            request.write_authority,
            Some(SpawnWriteAuthority::WorkspaceWrite | SpawnWriteAuthority::WorktreeWrite)
        ),
        FleetRole::Scout
        | FleetRole::Planner
        | FleetRole::Reviewer
        | FleetRole::Verifier
        | FleetRole::Consultant => false,
    }
}

/// A root Operate dispatch has already crossed the approval boundary on the
/// `agent` call. Delegate Suggest-level file edits and the bounded built-in
/// verification surfaces so a normal message can produce verified work.
/// Arbitrary shell and custom verifier commands still follow the active
/// permission posture.
fn apply_session_spawn_defaults(runtime: &mut SubAgentRuntime) {
    if runtime.spawn_depth == 0 && runtime.parent_mode == AppMode::Operate {
        runtime.accept_edits = true;
        runtime.accept_verification = true;
    }
}

/// Spawn one Workflow `task(...)` through the same path as the public `agent`
/// tool. Keeping this adapter inside the sub-agent module prevents the
/// Workflow driver from copying Fleet roster/profile/depth/budget semantics.
///
/// `identity` is stamped onto the returned spawn metadata so panel/history
/// consumers can render workflow children without parsing prompt text (#4119).
pub(crate) async fn spawn_workflow_task(
    request: codewhale_workflow_js::TaskRequest,
    manager: SharedSubAgentManager,
    mut runtime: SubAgentRuntime,
    identity: WorkflowTaskSpawnIdentity,
) -> Result<WorkflowTaskSpawnResult, ToolError> {
    match (
        identity.exact_fleet_binding.as_ref(),
        identity.fleet_authority_fingerprint.as_deref(),
    ) {
        (Some(binding), Some(fingerprint))
            if request.profile.as_deref() == Some(binding.member_id.as_str())
                && request.role.as_deref() == Some(binding.member_role.as_str())
                && binding.authority.fingerprint() == fingerprint => {}
        (None, None) => {}
        _ => {
            return Err(ToolError::permission_denied(
                "The exact Fleet member and its authority receipt do not match at spawn.",
            ));
        }
    }
    // Capture identity fallbacks before consuming `request` fields into the
    // agent-tool input JSON.
    let request_label = request
        .label
        .as_ref()
        .map(|label| label.trim())
        .filter(|label| !label.is_empty())
        .map(str::to_string);
    let request_phase = request
        .phase
        .as_ref()
        .map(|phase| phase.trim())
        .filter(|phase| !phase.is_empty())
        .map(str::to_string);
    let mut input = json!({
        "prompt": request.description,
        "worktree": request.worktree,
    });
    if let Some(value) = request.cwd {
        input["cwd"] = json!(value);
    }
    if let Some(value) = request.write_authority {
        input["write_authority"] = json!(value);
    }
    if !request.write_roots.is_empty() {
        input["write_roots"] = json!(request.write_roots);
    }
    if !request.exact_files.is_empty() {
        input["exact_files"] = json!(request.exact_files);
    }
    if !request.coordination_contracts.is_empty() {
        input["coordination_contracts"] = json!(request.coordination_contracts);
    }
    if !request.dependencies.is_empty() {
        input["dependencies"] = json!(request.dependencies);
    }
    if !request.acceptance.is_empty() {
        input["acceptance"] = json!(request.acceptance);
    }
    if let Some(value) = request.subagent_type {
        input["type"] = json!(value);
    }
    if let Some(value) = request.role {
        input["role"] = json!(value);
    }
    if let Some(value) = request.profile {
        input["profile"] = json!(value);
    }
    if let Some(value) = request.model {
        input["model"] = json!(value);
    }
    if let Some(value) = request.model_strength {
        input["model_strength"] = json!(value);
    }
    if let Some(value) = request.thinking {
        input["thinking"] = json!(value);
    }
    if let Some(value) = request.allowed_tools {
        input["allowed_tools"] = json!(value);
    }
    // A host-derived ceiling (e.g. an exact Fleet member's
    // `network_tool = false`) reaches the child as a deny list, which the child
    // registry applies to both the model-visible surface and the call guard.
    if !request.disallowed_tools.is_empty() {
        input["disallowed_tools"] = json!(request.disallowed_tools);
    }
    if let Some(value) = request.max_depth {
        input["max_depth"] = json!(value);
    }
    if let Some(value) = request.max_steps {
        input["max_steps"] = json!(value);
    }
    if let Some(value) = request.wall_time_secs {
        input["wall_time_secs"] = json!(value);
    }
    // An exact-Fleet task must arrive here carrying the envelope its receipt
    // names, and the input just built is the last place both are visible. A
    // missing, stale, or mismatched fingerprint fails the spawn closed rather
    // than launching a child whose surface nobody can tie back to a decision.
    if let Some(expected) = identity.fleet_authority_fingerprint.as_deref() {
        // `spawn_workflow_task` reports `ToolError`, so the verification's
        // `anyhow` failure is mapped onto the variant that matches what just
        // happened: the child's surface could not be authorized, so it is
        // denied rather than launched.
        verify_fleet_authority_input(expected, &input)
            .map_err(|err| ToolError::permission_denied(err.to_string()))?;
    }
    // Workflow children inherit the parent tool surface and auto-accept
    // Suggest-level file edits for write-capable roles. Shell / network / MCP
    // still require parent auto-approve (or fail closed).
    runtime.accept_edits = true;
    // Prefer the identity values the driver stamped; fall back to task options.
    // The label is resolved before the spawn so the child carries it as its
    // name from the first event on (#6565).
    let mut identity = identity;
    identity.workflow_task_label = identity
        .workflow_task_label
        .take()
        .filter(|label| !label.trim().is_empty())
        .or(request_label);
    let (result, mut metadata) = spawn_subagent_from_input(
        input,
        manager,
        runtime,
        identity.fleet_authority_fingerprint.is_some(),
        Some(&identity),
    )
    .await?;
    let workflow_task_label = identity.workflow_task_label;
    let workflow_phase_id = identity
        .workflow_phase_id
        .filter(|phase| !phase.trim().is_empty())
        .or(request_phase);
    metadata.workflow_run_id = Some(identity.workflow_run_id);
    metadata.workflow_phase_id = workflow_phase_id;
    metadata.workflow_task_label = workflow_task_label;
    metadata.workflow_child_index = Some(identity.workflow_child_index);
    Ok(WorkflowTaskSpawnResult { result, metadata })
}

/// Check the spawn input against the fingerprint on the Fleet receipt.
///
/// The fingerprint is produced by `ChildAuthority::fingerprint` and names every
/// field of the envelope. Only four of them survive into the spawn input as
/// distinct keys — `write_authority`, `max_depth`, `allowed_tools`,
/// `disallowed_tools` — and those are exactly the four this function can and
/// does verify. The remaining fields (`tools`, `network`, `shell`, `posture`)
/// are *derivations* the Fleet used to compute those four, so a divergence in
/// any of them shows up in one of the four; verifying the wire form is
/// therefore the stronger check, not the weaker one, because it is the value
/// the child is actually constructed from.
///
/// Fails closed in every ambiguous case: an unparseable fingerprint is a
/// refusal, not a pass.
fn verify_fleet_authority_input(expected: &str, input: &Value) -> Result<()> {
    let fields: std::collections::HashMap<&str, &str> = expected
        .split(';')
        .filter_map(|part| part.split_once('='))
        .collect();
    if !expected.starts_with("v1;") || fields.len() < 8 {
        return Err(anyhow!(
            "Fleet authority fingerprint `{expected}` is not a form this build understands; \
             refusing the spawn rather than launching an unverified child"
        ));
    }

    let listed = |key: &str| -> String {
        let mut values: Vec<String> = input
            .get(key)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        values.sort();
        values.dedup();
        values.join(",")
    };

    let actual_allow = match input.get("allowed_tools") {
        None | Some(Value::Null) => "inherit".to_string(),
        Some(Value::Array(list)) if list.is_empty() => "none".to_string(),
        Some(_) => listed("allowed_tools"),
    };
    let actual_write = input
        .get("write_authority")
        .and_then(Value::as_str)
        .unwrap_or("read_only");
    let actual_depth = input
        .get("max_depth")
        .and_then(Value::as_u64)
        .map(|depth| depth.to_string())
        .unwrap_or_default();

    for (key, actual) in [
        ("write", actual_write.to_string()),
        ("depth", actual_depth),
        ("allow", actual_allow),
        ("deny", listed("disallowed_tools")),
    ] {
        let expected_value = fields.get(key).copied().unwrap_or_default();
        if expected_value != actual {
            return Err(anyhow!(
                "Fleet authority mismatch at the spawn boundary: the receipt names {key}=`{expected_value}` \
                 but the child would be constructed with `{actual}`. Refusing the spawn — a Fleet \
                 ceiling that does not reach the runtime is not a ceiling."
            ));
        }
    }
    Ok(())
}

// === Sub-agent Execution ===

/// Build the system prompt for a sub-agent.
///
/// Starts with the per-type prompt (`FleetRole::system_prompt`) and
/// appends a one-line role overlay when `assignment.role` is set. The
/// full role library — TOML overlays from `~/.deepseek/roles/`, the
/// `/roles` slash command, model overrides per role — lands in 0.6.7.
/// For 0.6.6 we just don't drop the role on the floor: the model sees
/// "You are operating in the role of `{name}`." as a final line so its
/// behavior reflects the user's choice.
fn build_subagent_system_prompt(agent_type: &FleetRole, assignment: &SubAgentAssignment) -> String {
    let base = agent_type.system_prompt();
    let mut prompt = match assignment.role.as_deref() {
        Some(role) if !role.trim().is_empty() => {
            format!(
                "{base}\n\nYou are operating in the role of `{}`.",
                role.trim()
            )
        }
        _ => base,
    };
    // Sub-agents are background workers: the orchestrating agent is their only
    // caller. They never talk to the end user.
    prompt.push_str(
        "\n\nYou are a background sub-agent: every instruction comes from the orchestrating agent, not a human. Never address the end user or ask them questions — do the assigned work and report results back to the orchestrator.",
    );
    // C1: write-capable children must return PASS/FAIL evidence, not only a diff.
    if write_capable_child_needs_verify_contract(agent_type) {
        prompt.push_str(WRITE_CHILD_VERIFY_CONTRACT);
    }
    prompt
}

/// True when the child role is expected to mutate the workspace and therefore
/// must end with structured verify evidence (Operate Phase 1 / C1).
///
/// Scout/planner/reviewer/verifier stay read-oriented; builder/custom/worker
/// are write-capable defaults. Explicit read-only spawn authority is enforced
/// separately in the tool registry — the prompt still asks write-role children
/// for evidence when they did edit.
fn write_capable_child_needs_verify_contract(agent_type: &FleetRole) -> bool {
    matches!(
        agent_type,
        FleetRole::Builder | FleetRole::Custom | FleetRole::Worker
    )
}

fn build_subagent_system_prompt_with_skills(
    agent_type: &FleetRole,
    assignment: &SubAgentAssignment,
    context: &ToolContext,
) -> String {
    let mut prompt = build_subagent_system_prompt(agent_type, assignment);
    let catalog = subagent_skill_catalog(context);
    if !catalog.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&catalog);
    }
    prompt
}

/// Render the same workspace/plugin-qualified Skill authority available to
/// `load_skill`, without leaking a mutable source or staged filesystem path.
/// Every fresh and nested child derives this from its inherited ToolContext;
/// forked children receive it at system precedence as well.
fn subagent_skill_catalog(context: &ToolContext) -> String {
    let mode = context.skills_discovery_mode;
    let registry = context
        .skills_dir
        .as_deref()
        .map_or_else(
            || {
                crate::skills::discover_in_workspace_with_mode_and_plugins(
                    &context.workspace,
                    mode,
                    context.plugin_registry.as_deref(),
                )
            },
            |skills_dir| {
                crate::skills::discover_for_workspace_and_dir_with_mode_and_plugins(
                    &context.workspace,
                    skills_dir,
                    mode,
                    context.plugin_registry.as_deref(),
                )
            },
        )
        .into_enabled();
    if registry.list().is_empty() {
        return String::new();
    }
    let mut output = String::from(
        "## Skills\n\nUse `load_skill` with an exact name before applying a Skill. Catalog entries are workspace-scoped snapshots; plugin entries are revalidated at use.\n",
    );
    for skill in registry
        .list()
        .iter()
        .filter(|skill| skill.invocation.model_invocable())
    {
        let source = match &skill.source {
            crate::skills::SkillSource::Native => "native workspace catalog".to_string(),
            crate::skills::SkillSource::Plugin {
                plugin_id,
                plugin_name,
                authority,
                ..
            } => format!(
                "reviewed plugin {plugin_name} id={plugin_id} generation={} content={}",
                authority.state_generation,
                &authority.content_hash[..authority.content_hash.len().min(12)]
            ),
        };
        use std::fmt::Write as _;
        let _ = writeln!(
            output,
            "- `{}`: {} ({source})",
            skill.name,
            skill.description.replace(['\n', '\r'], " ")
        );
    }
    output
}

fn subagent_request_system_prompt(subagent_system_prompt: &str) -> SystemPrompt {
    // Cold or different-route forks keep their own resolved role prompt.
    // Exact-route cache inheritance is admitted later by the common Engine;
    // the role/task framing remains recorded in the fork history.
    SystemPrompt::Text(subagent_system_prompt.to_string())
}

#[cfg(test)]
fn build_initial_subagent_messages(
    prompt: &str,
    assignment: &SubAgentAssignment,
    agent_type: &FleetRole,
    fork_context: Option<&SubAgentForkContext>,
) -> Vec<Message> {
    let system_prompt = build_subagent_system_prompt(agent_type, assignment);
    build_initial_subagent_messages_with_system(
        prompt,
        assignment,
        agent_type,
        &system_prompt,
        fork_context.cloned(),
    )
}

pub(crate) const SUBAGENT_CONTEXT_OPEN: &str = "<codewhale:subagent_context>";

fn build_initial_subagent_messages_with_system(
    prompt: &str,
    assignment: &SubAgentAssignment,
    agent_type: &FleetRole,
    subagent_system_prompt: &str,
    fork_context: Option<SubAgentForkContext>,
) -> Vec<Message> {
    let mut messages = Vec::new();

    if let Some(context) = fork_context {
        // Resolution already made this child's isolated copy of the parent.
        messages = context.messages;
        if let Some(state) = context
            .structured_state_block
            .as_deref()
            .map(str::trim)
            .filter(|state| !state.is_empty())
        {
            messages.push(system_text_message(format!(
                "<codewhale:fork_state>\n{state}\n</codewhale:fork_state>"
            )));
        }

        messages.push(system_text_message(format!(
            "{SUBAGENT_CONTEXT_OPEN}\n{}\n</codewhale:subagent_context>",
            subagent_system_prompt
        )));
    }

    messages.push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: build_assignment_prompt(prompt, assignment, agent_type),
            cache_control: None,
        }],
    });

    messages
}

/// Whether an agent's current To-do snapshot is worth publishing to the
/// mailbox (#4810).
///
/// Two silences are deliberate: an unchanged list is not news, and an agent
/// that has never stated any work says nothing at all rather than announcing
/// an empty list. A list that goes from non-empty to empty *is* published —
/// that is a real transition the card must reflect instead of showing stale
/// items.
#[cfg(test)]
fn work_state_worth_publishing(
    last_published: Option<&crate::tools::todo::TodoListSnapshot>,
    current: &crate::tools::todo::TodoListSnapshot,
) -> bool {
    match last_published {
        Some(last) => last != current,
        None => !current.is_empty(),
    }
}

fn system_text_message(text: String) -> Message {
    Message {
        role: Role::System,
        content: vec![ContentBlock::Text {
            text,
            cache_control: None,
        }],
    }
}

struct SubAgentTask {
    manager_handle: SharedSubAgentManager,
    runtime: SubAgentRuntime,
    agent_id: String,
    agent_type: FleetRole,
    prompt: String,
    assignment: SubAgentAssignment,
    /// `None` = full registry inheritance. `Some(list)` = explicit narrow.
    /// Approval-gated tools still require an auto-approved parent runtime.
    allowed_tools: Option<Vec<String>>,
    fork_context: bool,
    started_at: Instant,
    max_steps: u32,
    /// Wall-clock budget for the child's work. A child that waits for a
    /// launch slot gets it in full from the moment it launches, bounded by
    /// `wall_ceiling_ms`; the queue wait itself is bounded by the same length.
    wall_time: Duration,
    /// Inherited absolute deadline (epoch ms) from the parent, saved-run or
    /// source record, which a restarted work clock never passes.
    wall_ceiling_ms: Option<u64>,
    input_rx: mpsc::UnboundedReceiver<SubAgentInput>,
    /// Interactive launch gate (#3095). `Some` only for direct (depth-1)
    /// children: the task acquires a permit before its first model step and
    /// holds it until completion, so a fanout burst beyond the limit queues
    /// with a visible reason instead of executing all at once.
    launch_gate: Option<Arc<governor::DynamicGate>>,
    /// Releases the parent turn's settlement barrier after this turn-owned
    /// child or descendant has completed its terminal fan-in.
    _foreground_child_registration: Option<ForegroundChildRegistration>,
}

/// Spawn the child task body under a panic guard.
///
/// Children run under `spawn_supervised`, which catch_unwinds a panic so the
/// parent process survives — and that used to be where the story ended: the
/// terminal commit at the end of `run_subagent_task_inner` never ran, so the
/// panicked child stayed `Running` and its write claim kept gating live peers
/// until heartbeat auto-cancel (indefinitely, while a lingering shell kept
/// the heartbeat fresh). The guard catches the panic first, commits a crash
/// terminal result through the same `finish_terminal_result` arbitration every
/// other terminal path uses, then resumes unwinding so the supervisor still
/// logs the panic and writes its crash dump.
async fn run_subagent_task(task: SubAgentTask) {
    let agent_id = task.agent_id.clone();
    let manager_handle = task.manager_handle.clone();
    supervise_subagent_task_body(manager_handle, agent_id, run_subagent_task_inner(task)).await;
}

async fn supervise_subagent_task_body(
    manager_handle: SharedSubAgentManager,
    agent_id: String,
    body: impl std::future::Future<Output = ()> + Send,
) {
    use futures_util::FutureExt;
    let panicked = std::panic::AssertUnwindSafe(body).catch_unwind().await;
    let Err(panic) = panicked else {
        return;
    };
    let message = crate::utils::panic_message(&*panic);
    let mut result = match manager_handle.read().await.get_result(&agent_id) {
        Ok(result) => result,
        Err(err) => {
            tracing::error!(
                target: "subagent",
                agent_id = %agent_id,
                ?err,
                "panicked task no longer has a manager record"
            );
            std::panic::resume_unwind(panic);
        }
    };
    result.status = SubAgentStatus::Failed(format!("sub-agent task panicked: {message}"));
    result.result = None;
    result.needs_input = None;
    // Verification precedes the write lock like the natural epilogue (#6210).
    ensure_worker_delivery_verified(&manager_handle, &agent_id, &result).await;
    {
        let mut manager = manager_handle.write().await;
        // Arbitrated exactly like the natural terminal commit: when a
        // cancel or another terminal outcome already won, this is a
        // no-op rather than a second result.
        manager.finish_terminal_result(&agent_id, result, false, true);
    }
    std::panic::resume_unwind(panic);
}

/// The deterministic fallback makes no additional request. An earlier
/// reporting attempt may have timed out, so do not claim it never happened.
fn budget_partial_result(
    result: SubAgentResult,
    cause: &str,
    preservation_note: Option<&str>,
) -> SubAgentResult {
    let note = match preservation_note {
        Some(note) => format!(
            "Reporting allowance was unavailable or expired. This deterministic fallback makes no further model request. {note}"
        ),
        None => "Reporting allowance was unavailable or expired. This deterministic fallback makes no further model request.".to_string(),
    };
    budget_partial_result_with_note(result, cause, &note)
}

/// Cancelling keeps the work (addendum F4, fleet-5). A Stop used to end a
/// write-scoped child with only "Cancelled by parent request.", leaving the
/// files it had changed unnamed and uncheckpointed. After a fresh
/// Running -> Cancelled transition this runs the same inventory and
/// isolated-worktree checkpoint a budget death gets, off the manager lock,
/// and appends it to the child's result. Read-only children have no
/// delivery baseline and are returned unchanged.
///
/// A Stop cascades to the child's descendants (`cancel_agent_for_session`),
/// so each write-scoped descendant stopped with it gets its own receipt too:
/// the work a grandchild left is named on the grandchild's record instead of
/// vanishing behind the parent's single line.
pub(crate) async fn preserve_cancelled_work(
    manager: &SharedSubAgentManager,
    snapshot: SubAgentResult,
) -> SubAgentResult {
    if snapshot.status != SubAgentStatus::Cancelled
        || snapshot.result.as_deref() != Some(CANCELLED_BY_PARENT_RESULT)
    {
        return snapshot;
    }
    let descendants = manager
        .read()
        .await
        .freshly_cancelled_writing_descendants(&snapshot.agent_id);
    for descendant in descendants {
        if let Some(note) =
            budget_work_preservation_note(manager, &descendant, "cancelled with its parent").await
        {
            manager
                .write()
                .await
                .append_cancel_preservation_note(&descendant, &note);
        }
    }
    let Some(note) =
        budget_work_preservation_note(manager, &snapshot.agent_id, "cancelled by parent").await
    else {
        return snapshot;
    };
    manager
        .write()
        .await
        .append_cancel_preservation_note(&snapshot.agent_id, &note)
        .unwrap_or(snapshot)
}

/// Inventory the workspace changes a budget-killed worker left behind, for
/// the preservation receipt in its terminal result (#5529). The spawn-time
/// delivery baseline makes `changed_paths` name exactly what this worker
/// touched — committed or not — so a wall-time or token death is never a
/// silent loss. Returns `None` when no write-scoped baseline exists (a
/// read-only worker cannot have left file work) or git cannot answer.
async fn budget_work_preservation_note(
    manager: &SharedSubAgentManager,
    agent_id: &str,
    cause: &str,
) -> Option<String> {
    let (evidence, workspace, isolated_worktree) = manager
        .read()
        .await
        .worker_records
        .get(agent_id)
        .map(|record| {
            (
                record.delivery_evidence.clone(),
                record.spec.workspace.clone(),
                record
                    .spec
                    .launch_manifest
                    .as_ref()
                    .is_some_and(|manifest| manifest.worktree),
            )
        })?;
    let display = workspace.display().to_string();
    let agent = agent_id.to_string();
    let cause = cause.to_string();
    let (changed, checkpoint) = tokio::task::spawn_blocking(move || {
        let changed = evidence.changed_paths(&workspace)?;
        let checkpoint =
            evidence.checkpoint_uncommitted(&changed, &agent, &cause, isolated_worktree);
        Some((changed, checkpoint))
    })
    .await
    .ok()??;
    Some(if changed.is_empty() {
        format!("No workspace changes were recorded under {display}.")
    } else {
        const MAX_LISTED_PATHS: usize = 12;
        let listed = changed
            .iter()
            .take(MAX_LISTED_PATHS)
            .cloned()
            .collect::<Vec<_>>();
        let suffix = if changed.len() > listed.len() {
            format!(" and {} more", changed.len() - listed.len())
        } else {
            String::new()
        };
        let inventory = format!(
            "The worker left {} workspace change(s) under {display}: {}{suffix}.",
            changed.len(),
            listed.join(", ")
        );
        let disposition = match checkpoint {
            delivery::BudgetCheckpointOutcome::Committed { sha } => format!(
                " They are checkpointed in commit {sha} on the worker branch; review before merging or re-dispatching."
            ),
            delivery::BudgetCheckpointOutcome::Clean => {
                " The worker committed before death; the changes are already in the worker tree."
                    .to_string()
            }
            delivery::BudgetCheckpointOutcome::Failed { reason } => format!(
                " Checkpoint commit failed ({reason}); the files survive on disk; salvage them or re-dispatch the remaining task."
            ),
            delivery::BudgetCheckpointOutcome::SkippedNonIsolated => {
                " The files survive on disk; salvage them or re-dispatch the remaining task."
                    .to_string()
            }
        };
        format!("{inventory}{disposition}")
    })
}

/// Deferred delivery verification (#6210). Snapshots inputs under a read
/// lock, runs the git-subprocess + fingerprint compute in `spawn_blocking`
/// with no lock held, then stores under a follow-up write lock. Idempotent
/// via `DeliveryEvidence.checked`: concurrent callers may duplicate the
/// read-only computation, but only the first store wins.
///
/// A racing terminal commit can land between the compute and the store; the
/// stored verdict is then based on this call's result text rather than the
/// committed one. The deliverable-presence verdicts are text-independent, so
/// only the claimed-path comparison can differ, and only in that race.
async fn ensure_worker_delivery_verified(
    manager: &SharedSubAgentManager,
    worker_id: &str,
    result: &SubAgentResult,
) {
    let held = manager.clone().read_owned().await;
    let worker = worker_id.to_string();
    let result = result.clone();
    let snapshot = codewhale_app_server::daemon_socket::owner_work(move || {
        let Some(inputs) = held.delivery_verification_inputs(&worker, &result) else {
            return Ok(None);
        };
        let cleanup = inputs
            .remove_worktree_if_unchanged
            .then(|| held.claim_worktree_cleanup(&worker, &inputs.workspace))
            .flatten();
        Ok(Some((inputs, cleanup)))
    })
    .await
    .ok()
    .flatten();
    let Some((inputs, cleanup)) = snapshot else {
        return;
    };
    let shared = Arc::clone(manager);
    let worker = worker_id.to_string();
    let verification = tokio::task::spawn_blocking(move || {
        let mut verification = delivery::compute_delivery_verification(&inputs);
        if let Some(claim) = cleanup {
            let changed = inputs.evidence.changed_paths(&inputs.workspace);
            if remove_finished_worktree(&shared, &worker, claim, changed.as_ref()) {
                verification
                    .summary
                    .push_str(" The worker's isolated worktree changed nothing and was removed.");
            }
        }
        verification
    })
    .await
    .ok();
    let Some(verification) = verification else {
        return;
    };
    manager
        .write()
        .await
        .store_delivery_verification(worker_id, verification);
}

/// A claimed removal of a finished worker's isolated worktree; dropping it
/// releases the claim, also when the removal panics.
struct WorktreeCleanupClaim {
    cleanups: Arc<std::sync::Mutex<HashSet<PathBuf>>>,
    workspace: PathBuf,
}

impl Drop for WorktreeCleanupClaim {
    fn drop(&mut self) {
        self.cleanups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.workspace);
    }
}

/// Remove `worker_id`'s unchanged isolated worktree under `claim`. Runs on
/// the blocking pool. The inputs were read before the terminal commit, and an
/// interrupt can win after that: the worker is then continuable again, so its
/// workspace is kept. The claim keeps a continuation from starting until this
/// decision and the removal are done. The manager lock is released before git
/// runs.
fn remove_finished_worktree(
    manager: &SharedSubAgentManager,
    worker_id: &str,
    claim: WorktreeCleanupClaim,
    changed: Option<&BTreeSet<String>>,
) -> bool {
    let continuable = manager
        .blocking_read()
        .agents
        .get(worker_id)
        .is_some_and(|agent| matches!(agent.status, SubAgentStatus::Interrupted(_)));
    let removed = !continuable && worktree::remove_unchanged_worktree(&claim.workspace, changed);
    drop(claim);
    removed
}

fn budget_partial_result_with_note(
    mut result: SubAgentResult,
    cause: &str,
    handback_note: &str,
) -> SubAgentResult {
    let text = result.result.take().filter(|text| !text.trim().is_empty());
    let partial = match text {
        Some(text) => text.chars().take(4_000).collect::<String>(),
        None => result.checkpoint.as_ref().map_or_else(
            || {
                "No assistant text was recorded; inspect the checkpoint for completed tool work."
                    .to_string()
            },
            |checkpoint| budget_handback::fallback_partial_text(&checkpoint.messages),
        ),
    };
    let measured = result
        .usage
        .as_ref()
        .and_then(|usage| usage.total_tokens)
        .map(|tokens| format!("Measured worker usage: {tokens} input + output tokens."))
        .unwrap_or_else(|| "Worker token usage has not been reported.".to_string());
    result.status = SubAgentStatus::BudgetExhausted;
    result.result = Some(format!(
        "{cause}. {measured} {handback_note}\nPartial output:\n{partial}"
    ));
    let checkpoint = result.checkpoint.get_or_insert_with(|| {
        build_subagent_checkpoint(&result.agent_id, cause, &[], result.steps_taken, false)
    });
    checkpoint.reason = cause.to_string();
    checkpoint.continuable = false;
    result.needs_input = None;
    result
}

#[allow(clippy::too_many_lines)]
async fn run_subagent_task_inner(mut task: SubAgentTask) {
    // `spawn_background_with_assignment_options` installs this before the task
    // is scheduled. Keep this fallback for internal/test task launchers so a
    // manually-created worker still owns the same terminal fan-in contract.
    {
        let delivery = SubAgentTerminalDeliveryContext::from_runtime(&task.runtime);
        let mut manager = task.manager_handle.write().await;
        if let Some(agent) = manager.agents.get_mut(&task.agent_id)
            && agent.status == SubAgentStatus::Running
            && !agent.completion_claimed
            && agent.terminal_delivery.is_none()
        {
            agent.owner_session_id = delivery.session_id.clone();
            agent.terminal_delivery = Some(delivery);
        }
    }

    let mut deadline = (task.started_at + task.wall_time).min(
        task.runtime.worker_profile.wall_deadline_ms.map_or(
            task.started_at + task.wall_time,
            |deadline| {
                Instant::now() + Duration::from_millis(deadline.saturating_sub(epoch_millis_now()))
            },
        ),
    );
    // One effective deadline, visible to both layers. The inner stages read
    // `worker_profile.wall_deadline_ms` (work deadline, reserved hand-back),
    // while an authored task wall time used to narrow the budget only through
    // the outer backstop below. The backstop must not race the deadline
    // (#6277), so fold the task wall time into the profile before the run:
    // enforcement, the model-visible budget block, and the receipt then all
    // describe the same limit.
    let effective_deadline_ms = epoch_millis_now().saturating_add(
        u64::try_from(
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis(),
        )
        .unwrap_or(u64::MAX),
    );
    task.runtime.worker_profile.wall_deadline_ms = Some(
        task.runtime
            .worker_profile
            .wall_deadline_ms
            .map_or(effective_deadline_ms, |existing| {
                existing.min(effective_deadline_ms)
            }),
    );

    // Interactive launch gate (#3095): direct children acquire a permit
    // before their first model step so a fanout burst beyond the limit
    // queues visibly instead of executing all at once. The permit is held
    // for the lifetime of the task. The queue wait is bounded by the same
    // deadline, and a child that never gets a slot fails as "never started"
    // rather than as a run that exhausted its budget. A child that does get a
    // slot after waiting starts its work clock then (#6015), still bounded by
    // any inherited deadline. Cancellation while queued is handled by
    // the admitted child actor before it emits Started/Starting.
    let mut _launch_permits = Vec::new();
    let mut launch_wait_timed_out = false;
    let mut launched_from_queue = false;
    let owner_gate = (task.runtime.spawn_depth == 1)
        .then(|| {
            task.runtime
                .execution_limits
                .as_ref()
                .map(|limits| &limits.launch_gate)
        })
        .flatten();
    for gate in owner_gate.into_iter().chain(task.launch_gate.as_ref()) {
        match Arc::clone(gate).try_acquire() {
            Some(permit) => _launch_permits.push(permit),
            None => {
                match tokio::time::timeout_at(
                    deadline.into(),
                    acquire_queued_launch_permit(&task, Arc::clone(gate), deadline),
                )
                .await
                {
                    Ok(permit) => {
                        if let Some(permit) = permit {
                            launched_from_queue = true;
                            _launch_permits.push(permit);
                        } else {
                            break;
                        }
                    }
                    Err(_) => {
                        launch_wait_timed_out = true;
                        break;
                    }
                }
            }
        }
    }
    let queue_limit = deadline.saturating_duration_since(task.started_at);
    let mut work_started_at = task.started_at;
    if launched_from_queue {
        let now = Instant::now();
        let now_ms = epoch_millis_now();
        let restarted_ms =
            now_ms.saturating_add(u64::try_from(task.wall_time.as_millis()).unwrap_or(u64::MAX));
        let deadline_ms = task
            .wall_ceiling_ms
            .map_or(restarted_ms, |ceiling| ceiling.min(restarted_ms));
        deadline = now + Duration::from_millis(deadline_ms.saturating_sub(now_ms));
        task.runtime.worker_profile.wall_deadline_ms = Some(deadline_ms);
        work_started_at = now;
        // Continuation reads the saved deadline, so save the restarted one;
        // otherwise a child that launched late would be refused as out of
        // budget while most of its work budget was left.
        let mut manager = task.manager_handle.write().await;
        if let Some(record) = manager.worker_records.get_mut(&task.agent_id) {
            record.spec.runtime_profile.wall_deadline_ms = Some(deadline_ms);
            record.updated_at_ms = now_ms;
            manager.persist_state_debounced();
        }
    }

    let turn_end_parking = task
        ._foreground_child_registration
        .as_ref()
        .map(ForegroundChildRegistration::parking_signal);
    // Every inner stage bounds itself by `deadline` (step requests by the
    // work deadline, and the reserved hand-back turn by the hard deadline),
    // so the inner run can always produce its own receipt: the specific stop
    // cause, the hand-back outcome, and preserved usage. This outer backstop
    // exists only for an inner stage that overruns anyway; racing it at the
    // same instant as the inner deadline replaced real receipts with the
    // generic error below (#6277). The grace keeps the anti-hang guarantee
    // while letting the inner receipt win.
    let backstop = deadline + Duration::from_secs(30);
    let effective_limit = deadline.saturating_duration_since(work_started_at);
    let mut backstop_budget = false;
    let result = if launch_wait_timed_out {
        task.runtime.cancel_token.cancel();
        Err(anyhow!(never_started_reason(queue_limit)))
    } else {
        let child = engine::run_child_agent(
            &task.runtime,
            task.agent_id.clone(),
            task.agent_type,
            task.prompt,
            task.assignment,
            task.allowed_tools,
            task.fork_context,
            task.started_at,
            task.max_steps,
            turn_end_parking,
            task.input_rx,
        );
        tokio::pin!(child);
        tokio::select! {
            result = &mut child => result,
            () = async {
                tokio::select! {
                    () = task.runtime.cancel_token.cancelled() => {},
                    () = tokio::time::sleep_until(backstop.into()) => {},
                }
            } => {
                // Signal first, then keep polling the exact future that owns
                // Core's approval receipt and actor join; never reopen work.
                backstop_budget = Instant::now() >= backstop
                    && !task.runtime.cancel_token.is_cancelled();
                task.runtime.cancel_token.cancel();
                // Core's driver owns a five-second cooperative join. One extra
                // second allows its cleanup; an earlier setup stall has no
                // admitted tool, and an abandoned actor retains unresolved Asked
                // evidence rather than claiming a durable Cancelled outcome.
                tokio::time::timeout(CHILD_STOP_SETTLE_GRACE + Duration::from_secs(1), &mut child)
                    .await.unwrap_or_else(|_| Err(anyhow!(engine::UnsettledChildCancellation(format!(
                        "{}; child cancellation did not settle",
                        child_wall_time_exhausted_reason(effective_limit)
                    )))))
            }
        }
    };

    let agent_id = task.agent_id.clone();
    // A request that moved before any work failed (if it failed) on its new
    // route: blame that route, not the one it left.
    let rerouted = task
        .manager_handle
        .write()
        .await
        .rerouted_requests
        .remove(&agent_id);
    let unsettled = result
        .as_ref()
        .is_err_and(|error| error.is::<engine::UnsettledChildCancellation>());
    let annotate_failure = |error: &str| match rerouted.as_ref() {
        Some(rerouted) => annotate_child_model_error_with_origin(
            error,
            &rerouted.model,
            rerouted.provider,
            &ModelRoute::Inherit,
            Some(&rerouted.origin),
        ),
        None => annotate_child_model_error_with_origin(
            error,
            &task.runtime.model,
            task.runtime.client.api_provider(),
            &task.runtime.worker_profile.model,
            task.runtime.route_origin.as_deref(),
        ),
    };
    let failure_error = result.as_ref().err().map(|err| {
        crate::logging::warn(format!(
            "sub-agent {} model request failed: {err:#}",
            task.agent_id
        ));
        annotate_failure(&subagent_failure_message(err))
    });

    if unsettled {
        // The same actor was force-stopped without a settled receipt. Keep
        // claims, admission capacity, and the existing foreground registration;
        // publish uncertainty on the same record, never a fake terminal result.
        let note = failure_error.clone().expect("unsettled error");
        let mut manager = task.manager_handle.write().await;
        if let Some(agent) = manager.agents.get_mut(&agent_id) {
            agent.result = Some(note.clone());
            agent.input_tx = None;
            agent.requested_stop.get_or_insert_with(|| {
                (
                    SubAgentStatus::Interrupted(note.clone()),
                    Some(note.clone()),
                )
            });
            agent.unsettled_launch = Some(std::mem::take(&mut _launch_permits));
            agent.unsettled_foreground = task._foreground_child_registration.take();
        }
        if let Some(record) = manager.worker_records.get_mut(&agent_id) {
            record.error = Some(note.clone());
        }
        manager.record_worker_event(
            &agent_id,
            AgentWorkerStatus::Running,
            Some(note),
            None,
            None,
        );
        manager.persist_state_best_effort();
        return;
    }

    // #5529: a wall-time task error must still name the work the child left
    // on disk. The inventory takes blocking git/fs reads, so it runs before
    // the manager write lock below is taken.
    let preservation_note = if backstop_budget
        || failure_error
            .as_deref()
            .is_some_and(|error| error.contains("wall-time budget exhausted"))
    {
        budget_work_preservation_note(
            &task.runtime.manager,
            &agent_id,
            failure_error
                .as_deref()
                .unwrap_or("wall-time budget exhausted"),
        )
        .await
    } else {
        None
    };

    // The terminal result is built before the write lock: `get_result` takes
    // `&self`, the error transforms are pure, and no live writer can change
    // the agent's text between here and the commit — only a racing terminal
    // claim, which leaves this epilogue with nothing to commit either way.
    // Verification runs here too, so the fan-in completion and the terminal
    // persist both carry fresh verdicts with no git subprocess under the lock
    // (#6210).
    let terminal = match result {
        Ok(result) if backstop_budget => budget_partial_result(
            result,
            &child_wall_time_exhausted_reason(effective_limit),
            preservation_note.as_deref(),
        ),
        Ok(mut result) => {
            // Core already owns the typed terminal status and redacted error.
            // Attach the exact request route here, just as for setup failures.
            match &mut result.status {
                SubAgentStatus::Failed(error) | SubAgentStatus::Interrupted(error) => {
                    *error = annotate_failure(error);
                }
                _ => {}
            }
            result
        }
        Err(_) => {
            let mut result = match task.manager_handle.read().await.get_result(&agent_id) {
                Ok(result) => result,
                Err(err) => {
                    tracing::error!(
                        target: "subagent",
                        agent_id = %agent_id,
                        ?err,
                        "failed task no longer has a manager record"
                    );
                    return;
                }
            };
            let error = failure_error
                .clone()
                .expect("failed task should carry annotated error");
            if error.contains("wall-time budget exhausted") && !error.contains("did not settle") {
                budget_partial_result(result, &error, preservation_note.as_deref())
            } else {
                result.status = SubAgentStatus::Failed(error);
                result.result = None;
                result.needs_input = None;
                result
            }
        }
    };
    ensure_worker_delivery_verified(&task.manager_handle, &agent_id, &terminal).await;
    // Every terminal path — successful/fatal model exit, explicit Stop,
    // coordination interrupt, and stale cleanup — arbitrates and publishes
    // through `finish_terminal_result`. Cancellation that already won leaves
    // this late epilogue with no claim and therefore no duplicate fan-in.
    let terminal_committed = {
        let mut manager = task.manager_handle.write().await;
        manager.finish_terminal_result(&agent_id, terminal, false, true)
    };
    if !terminal_committed {
        tracing::debug!(
            target: "subagent",
            agent_id = %agent_id,
            "suppressing late task completion after another terminal outcome won"
        );
    }
}

/// Queued-row reason (addendum F5): why the child waits — a free slot, or the
/// rate-limit governor's pause/throttle — and when it gives up waiting (as
/// an end time). The work budget starts at launch (#6015); the queue wait is
/// bounded separately so saturation cannot keep a child waiting forever.
fn queued_launch_reason(task: &SubAgentTask, deadline: Instant) -> String {
    let now = Instant::now();
    let governor_line = task
        .runtime
        .governor
        .as_ref()
        .and_then(|governor| governor.snapshot(now).status_line());
    let base = match governor_line {
        Some(line)
            if task
                .runtime
                .governor
                .as_ref()
                .is_some_and(|governor| governor.is_paused(now)) =>
        {
            format!("{SUBAGENT_QUEUED_RATE_LIMIT_REASON} — {line}")
        }
        Some(line) => format!("{SUBAGENT_QUEUED_LAUNCH_REASON} — {line}"),
        None => SUBAGENT_QUEUED_LAUNCH_REASON.to_string(),
    };
    format!(
        "{base} {}",
        queued_budget_note(
            deadline.saturating_duration_since(now),
            chrono::Local::now()
        )
    )
}

/// The wall-budget half of a queued reason. The row is republished only when
/// the governor state changes, so a "N minutes left" count would freeze at its
/// first value while the budget drained; the absolute end time stays true.
fn queued_budget_note(remaining: Duration, now: chrono::DateTime<chrono::Local>) -> String {
    let ends_at = chrono::Duration::from_std(remaining)
        .ok()
        .and_then(|remaining| now.checked_add_signed(remaining))
        .unwrap_or(now);
    format!(
        "(stops waiting at {}; its work budget starts at launch)",
        ends_at.format("%H:%M")
    )
}

/// The part of a queued reason that changes with governor state, not time.
fn queued_reason_cause(reason: &str) -> &str {
    reason.split(" (").next().unwrap_or(reason)
}

async fn acquire_queued_launch_permit(
    task: &SubAgentTask,
    gate: Arc<governor::DynamicGate>,
    deadline: Instant,
) -> Option<governor::DynamicGatePermit> {
    // When the governor has paused launches over sustained provider 429s,
    // surface the reason in the queued status instead of the generic
    // "waiting for a launch slot" message.
    let mut queued_reason = queued_launch_reason(task, deadline);
    record_queued_launch_progress(task, &queued_reason).await;
    // While queued, periodically probe the governor: if a rate-limit pause
    // outlives its window (the in-flight fleet finished before any success
    // could lift the pause), the probe resumes launches instead of leaving
    // the queue frozen until each child's wall-time deadline.
    let mut recovery_probe = tokio::time::interval(if task.runtime.governor.is_some() {
        LAUNCH_RECOVERY_PROBE_PERIOD
    } else {
        LAUNCH_RECOVERY_PROBE_PERIOD_WITHOUT_GOVERNOR
    });
    // Hold the acquire future across select iterations. Re-creating it on
    // every probe tick would leave one stale queue entry per tick per queued
    // child inside the gate (purged only by the next grant wave), which adds
    // up over a long pause with a full swarm queue.
    let mut acquire_permit = std::pin::pin!(gate.acquire());
    loop {
        tokio::select! {
            biased;
            () = task.runtime.cancel_token.cancelled() => {
                // No Cancelled progress event here: turn-end parking cancels
                // queued children through the same token and must project as
                // Interrupted-with-checkpoint, and genuine cancellation is
                // reported by the admitted child actor's first-step cancel check.
                return None;
            }
            _ = recovery_probe.tick() => {
                if let Some(governor) = task.runtime.governor.as_ref() {
                    governor.recover_if_window_drained(Instant::now());
                }
                // F5: re-publish when the governor state behind the queue
                // changed (paused, throttled, recovered).
                let reason = queued_launch_reason(task, deadline);
                if queued_reason_cause(&reason) != queued_reason_cause(&queued_reason) {
                    record_queued_launch_progress(task, &reason).await;
                    queued_reason = reason;
                }
                // If the probe lifted a pause it raised the gate capacity,
                // which grants queued waiters; the pinned `acquire_permit`
                // below observes the grant on the next poll.
            }
            permit = &mut acquire_permit => {
                return Some(permit);
            }
        }
    }
}

async fn record_queued_launch_progress(task: &SubAgentTask, queued_reason: &str) {
    {
        let mut manager = task.runtime.manager.write().await;
        manager.touch(&task.agent_id);
        manager.record_worker_event(
            &task.agent_id,
            AgentWorkerStatus::Queued,
            Some(queued_reason.to_string()),
            None,
            None,
        );
    }
    emit_agent_progress(
        task.runtime.event_tx.as_ref(),
        &task.runtime.context.state_namespace,
        &task.agent_id,
        queued_reason.to_string(),
        AgentProgressEventMeta::new(AgentWorkerStatus::Queued),
        task.runtime.parent_agent_id.clone(),
        task.runtime.spawn_depth,
    );
    if let Some(mailbox) = task.runtime.mailbox.as_ref() {
        let _ = mailbox.send(MailboxMessage::progress(&task.agent_id, queued_reason));
    }
}

/// Notify this runtime's immediate parent that the child finished (issue
/// #756). Root-spawned children send to the engine turn loop. Nested children
/// send to the parent sub-agent's local inbox, which is swapped into the
/// runtime used by that parent's `agent` tool. Returns `true` if a send was
/// attempted, `false` if this is the engine itself or no channel is wired.
/// Skips silently when the channel sender has no receiver — the receiver may
/// have ended because the parent turn/agent already completed.
#[cfg(test)]
pub(crate) fn emit_parent_completion(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    payload: &str,
) -> bool {
    if runtime.spawn_depth == 0 {
        return false;
    }
    let Some(tx) = runtime.parent_completion_tx.as_ref() else {
        return false;
    };
    let _ = tx.try_send(SubAgentCompletion {
        owner_session_id: runtime.context.state_namespace.clone(),
        agent_id: agent_id.to_string(),
        payload: payload.to_string(),
    });
    true
}

#[cfg(test)]
pub(crate) fn subagent_completion_from_result(result: &SubAgentResult) -> SubAgentCompletion {
    subagent_completion_from_result_with_ref(result, None)
}

/// Completion builder that names the persisted full report in the truncation
/// footer when `report_ref` is available; see `spill_subagent_final_report`.
#[cfg(test)]
pub(crate) fn subagent_completion_from_result_with_ref(
    result: &SubAgentResult,
    report_ref: Option<&str>,
) -> SubAgentCompletion {
    subagent_completion_from_result_with_ref_for_session("", result, report_ref)
}

/// Result-only fixture builder. Production delivery and recovery use the
/// manager's builder so the persisted ledger supplies descendant coverage.
#[cfg(test)]
pub(crate) fn subagent_completion_from_result_with_ref_for_session(
    owner_session_id: &str,
    result: &SubAgentResult,
    report_ref: Option<&str>,
) -> SubAgentCompletion {
    subagent_completion_with_verification(owner_session_id, result, report_ref, None, None)
}

fn completion_own_tokens(usage: Option<&AgentRunUsage>) -> Value {
    json!({
        "input_tokens": usage.and_then(|usage| usage.input_tokens),
        "output_tokens": usage.and_then(|usage| usage.output_tokens),
        "total_tokens": usage.and_then(|usage| usage.total_tokens),
    })
}

/// Fixed-size coverage, not a sum of each worker's shared-budget expenditure.
/// Unknown usage stays null when nothing was reported; an empty subtree is an
/// actual zero. A partial sum is explicitly labelled known, with its coverage.
fn completion_token_totals(usages: &[&AgentRunUsage], active_workers: usize) -> Value {
    let mut totals = json!({"workers": usages.len(), "active_workers": active_workers});
    for (field, values) in [
        (
            "input_tokens",
            usages
                .iter()
                .map(|usage| usage.input_tokens)
                .collect::<Vec<_>>(),
        ),
        (
            "output_tokens",
            usages
                .iter()
                .map(|usage| usage.output_tokens)
                .collect::<Vec<_>>(),
        ),
        (
            "total_tokens",
            usages
                .iter()
                .map(|usage| usage.total_tokens)
                .collect::<Vec<_>>(),
        ),
    ] {
        let reported_workers = values.iter().flatten().count();
        let sum = values
            .into_iter()
            .flatten()
            .try_fold(0_u64, u64::checked_add);
        totals[field] = json!({
            "known": sum.filter(|_| reported_workers > 0 || usages.is_empty()),
            "reported_workers": reported_workers,
            "overflow": sum.is_none(),
        });
    }
    totals
}

fn subagent_completion_with_verification(
    owner_session_id: &str,
    result: &SubAgentResult,
    report_ref: Option<&str>,
    verification: Option<&AgentRunVerificationSummary>,
    usage: Option<&Value>,
) -> SubAgentCompletion {
    let raw = summarize_subagent_result(result);
    let mut evidence_truncated = false;
    let evidence_block = match &result.status {
        SubAgentStatus::Failed(_)
        | SubAgentStatus::BudgetExhausted
        | SubAgentStatus::Cancelled
        | SubAgentStatus::Interrupted(_) => None,
        _ => result
            .result
            .as_deref()
            .and_then(extract_evidence_block)
            .map(|block| {
                let (clipped, ev_trunc) = clip_evidence_block(&block);
                evidence_truncated = ev_trunc;
                clipped
            })
            .filter(|evidence| !evidence.trim().is_empty()),
    };
    let summary_source = evidence_block
        .as_ref()
        .map(|_| strip_evidence_block(&raw))
        .unwrap_or(raw);
    let (summary, truncated) = stamp_subagent_summary_with_ref(&summary_source, report_ref);
    let summary_truncated = truncated || evidence_truncated;
    let sentinel = match &result.status {
        SubAgentStatus::Failed(error) => subagent_failed_sentinel(result, error),
        SubAgentStatus::BudgetExhausted => subagent_failed_sentinel(
            result,
            result
                .checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.reason.as_str())
                .unwrap_or("child token budget exhausted"),
        ),
        _ => subagent_done_sentinel(&result.agent_id, result, summary_truncated),
    };
    let sentinel = {
        // Both existing terminal sentinel forms carry the same delivery receipt.
        let opening = sentinel.find('>').expect("terminal sentinel opening");
        let closing = sentinel.rfind("</").expect("terminal sentinel closing");
        let mut payload: Value =
            serde_json::from_str(&sentinel[opening + 1..closing]).expect("terminal sentinel JSON");
        if let Some(verification) = verification {
            payload["verification"] = json!(verification);
        }
        payload["usage"] = usage.cloned().unwrap_or_else(|| {
            json!({
                "scope": "worker_only",
                "own": completion_own_tokens(result.usage.as_ref()),
                "descendants": null,
                "subtree": null,
            })
        });
        format!("{}{payload}{}", &sentinel[..=opening], &sentinel[closing..])
    };
    let payload = match evidence_block {
        Some(evidence) => format!("{summary}\n{evidence}\n{sentinel}"),
        None => format!("{summary}\n{sentinel}"),
    };
    SubAgentCompletion {
        owner_session_id: owner_session_id.to_string(),
        agent_id: result.agent_id.clone(),
        payload,
    }
}

const SUBAGENT_EVIDENCE_CHAR_BUDGET: usize = 4_000;

fn clip_evidence_block(block: &str) -> (String, bool) {
    let total = block.chars().count();
    if total <= SUBAGENT_EVIDENCE_CHAR_BUDGET {
        return (block.to_string(), false);
    }
    let clipped: String = block.chars().take(SUBAGENT_EVIDENCE_CHAR_BUDGET).collect();
    (format!("{clipped}…"), true)
}

fn extract_evidence_block(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let markers = ["### evidence", "## evidence", "evidence:"];
    for marker in markers {
        let Some(start) = lower.find(marker) else {
            continue;
        };
        let block = &text[start..];
        let tail = &block[marker.len()..];
        let end = tail
            .find("\n### ")
            .or_else(|| tail.find("\n## "))
            .or_else(|| tail.to_ascii_lowercase().find("\ngaps"))
            .or_else(|| tail.to_ascii_lowercase().find("\nnext"))
            .unwrap_or(tail.len());
        let extracted = format!("{}{}", &block[..marker.len()], &tail[..end])
            .trim()
            .to_string();
        if !extracted.is_empty() {
            return Some(extracted);
        }
    }
    None
}

fn strip_evidence_block(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let markers = ["### evidence", "## evidence", "evidence:"];
    for marker in markers {
        let Some(start) = lower.find(marker) else {
            continue;
        };
        let block = &text[start..];
        let tail = &block[marker.len()..];
        let end = tail
            .find("\n### ")
            .or_else(|| tail.find("\n## "))
            .or_else(|| tail.to_ascii_lowercase().find("\ngaps"))
            .or_else(|| tail.to_ascii_lowercase().find("\nnext"))
            .unwrap_or(tail.len());
        let mut without = format!("{}{}", &text[..start], &block[marker.len() + end..]);
        without = without.trim().to_string();
        return without;
    }
    text.trim().to_string()
}

/// Build a `<codewhale:subagent.done>` JSON sentinel for a successful child.
/// Intended to surface in the parent's transcript so the model recognizes
/// child completion.
///
/// Keep this payload deliberately lean. The human summary is emitted on the
/// line immediately before the sentinel; duplicating it here bloats the next
/// parent request's cache-miss tail. Wall-clock duration is useful UI
/// telemetry, but it is volatile and not useful for model coordination.
///
/// `truncated` reflects whether the previous-line summary was length-gated by
/// `stamp_subagent_summary` (issue #2652); it surfaces as `summary_kind` so
/// the parent model can tell a complete self-report from a clipped one and
/// verify material claims accordingly.
fn subagent_done_sentinel(agent_id: &str, res: &SubAgentResult, truncated: bool) -> String {
    let mut payload = json!({
        "agent_id": agent_id,
        // Whale name — a stable, human-friendly handle the orchestrator can use
        // to refer to this child in its own reasoning/output.
        "name": res.nickname,
        "agent_type": res.agent_type.as_str(),
        "status": subagent_status_name(&res.status),
        "summary_location": "previous_line",
        // issue #2652: lets the parent branch on whether the previous-line
        // summary is the full child report or a head+tail excerpt.
        "summary_kind": if truncated { "truncated" } else { "complete" },
    });
    if let Some(needs_input) = res.needs_input.clone() {
        payload["needs_input"] = json!(needs_input);
    }
    if let Some(child_route) = res.child_route.clone() {
        payload["child_route"] = json!(child_route);
    }
    format!("<codewhale:subagent.done>{payload}</codewhale:subagent.done>")
}

fn subagent_failure_class(status: &SubAgentStatus, error: &str) -> &'static str {
    let error = error.to_ascii_lowercase();
    if error.contains("no assistant text") || error.contains("without returning a final summary") {
        "empty_turn"
    } else if error.contains("step budget exhausted") {
        "step_budget"
    } else if error.contains("wall-time budget exhausted")
        || error == "child wall-time work budget exhausted"
    {
        "wall_time_budget"
    } else if matches!(status, SubAgentStatus::BudgetExhausted) {
        "budget_exhausted"
    } else if error.contains("authorization failed")
        || error.contains("usage limit")
        || error.contains("quota")
    {
        "auth_or_quota"
    } else if error.contains("timed out") || error.contains("timeout") {
        "timeout"
    } else {
        "runtime_error"
    }
}

/// Build the distinct high-priority failure event carried to the owning
/// parent. The human-readable, already-sanitized reason stays on the previous
/// line; this sentinel carries only bounded routing and recovery metadata.
fn subagent_failed_sentinel(res: &SubAgentResult, error: &str) -> String {
    let transcript_handle = format!("agent:{}/full_transcript", res.agent_id);
    let payload = json!({
        "event": "subagent.failed",
        "priority": "high",
        "agent_id": res.agent_id,
        "name": res.nickname.as_deref().unwrap_or(&res.name),
        "agent_type": res.agent_type.as_str(),
        "status": subagent_status_name(&res.status),
        "failure_class": subagent_failure_class(&res.status, error),
        "steps": res.steps_taken,
        "elapsed_ms": res.duration_ms,
        "transcript_handle": transcript_handle,
        "error_location": "previous_line",
        "child_route": res.child_route,
    });
    format!("<codewhale:subagent.done>{payload}</codewhale:subagent.done>")
}

/// Preserve the child's fail-closed incomplete-response contract in Core.
pub(crate) fn incomplete_subagent_response_failure(stop_reason: Option<&str>) -> String {
    let reason = stop_reason_detail(stop_reason);
    if is_output_limit_stop_reason(stop_reason) {
        format!(
            "Sub-agent model output was truncated (provider stop reason `{reason}`); no partial tool call was executed and any partial text was preserved for diagnostics."
        )
    } else {
        format!(
            "Sub-agent model response was incomplete (provider stop reason `{reason}`); no partial tool call or final result was accepted, and any partial text was preserved for diagnostics."
        )
    }
}

#[allow(clippy::too_many_arguments)]
async fn insert_subagent_full_transcript_handle(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    agent_type: &FleetRole,
    assignment: &SubAgentAssignment,
    status: &SubAgentStatus,
    result: Option<&String>,
    checkpoint: Option<&SubAgentCheckpoint>,
    transcript_artifact: Option<&mut SubAgentTranscriptArtifactWriter>,
    messages: &[Message],
    steps_taken: u32,
    duration_ms: u64,
    fork_context: bool,
) -> VarHandle {
    // Byte-bound the retained transcript (#3882): the handle store keeps this
    // payload resident per agent, and the checkpoint already carries its own
    // bounded message tail — embedding it verbatim would duplicate that tail
    // inside one payload. Keep checkpoint metadata, drop its messages, and
    // record how much of the true history the bounded tail omits.
    let (bounded_messages, omitted_messages) =
        bounded_tail_messages(messages, SUBAGENT_TRANSCRIPT_MESSAGE_BUDGET_BYTES);
    let checkpoint_meta = checkpoint.map(|checkpoint| SubAgentCheckpoint {
        omitted_messages: checkpoint.message_count,
        messages: Vec::new(),
        ..checkpoint.clone()
    });
    let transcript_artifact = transcript_artifact.map(|writer| {
        let synced =
            match writer.sync_projected_messages(messages, *status != SubAgentStatus::Running) {
                Ok(()) => true,
                Err(err) => {
                    tracing::warn!(
                        target: "subagent",
                        ?err,
                        agent_id,
                        "failed to persist complete sub-agent transcript artifact"
                    );
                    false
                }
            };
        writer.metadata(synced && writer.synced_live_messages == messages.len())
    });
    let payload = json!({
        "kind": "subagent_full_transcript",
        "agent_id": agent_id,
        "agent_type": agent_type.as_str(),
        "status": subagent_status_name(status),
        "context_mode": if fork_context { "forked" } else { "fresh" },
        "fork_context": fork_context,
        "result": result,
        "steps_taken": steps_taken,
        "duration_ms": duration_ms,
        "assignment": assignment,
        "checkpoint": checkpoint_meta,
        "message_count": messages.len(),
        "omitted_messages": omitted_messages,
        "messages_complete": omitted_messages == 0,
        "messages": bounded_messages,
        "complete_transcript_artifact": transcript_artifact,
    });
    let mut store = runtime.context.runtime.handle_store.lock().await;
    store.insert_json(format!("agent:{agent_id}"), "full_transcript", payload)
}

/// Publish the inspectable worker transcript while a child is still running.
///
/// The sidebar's Open action is intentionally backed by the same
/// `full_transcript` handle before and after completion. Keeping a separate
/// live-only snapshot name meant Open could only show a compact status card
/// until the worker stopped, which is exactly when observing it is least
/// useful.
#[allow(clippy::too_many_arguments)]
async fn publish_live_subagent_transcript(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    agent_type: &FleetRole,
    assignment: &SubAgentAssignment,
    result: Option<&String>,
    checkpoint: Option<&SubAgentCheckpoint>,
    transcript_artifact: Option<&mut SubAgentTranscriptArtifactWriter>,
    messages: &[Message],
    steps_taken: u32,
    started_at: Instant,
    fork_context: bool,
) {
    let duration_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    insert_subagent_full_transcript_handle(
        runtime,
        agent_id,
        agent_type,
        assignment,
        &SubAgentStatus::Running,
        result,
        checkpoint,
        transcript_artifact,
        messages,
        steps_taken,
        duration_ms,
        fork_context,
    )
    .await;
}

/// Bound a sub-agent tool result before it enters `messages` (#3882).
///
/// The root engine applies spillover in `turn_loop.rs`; the sub-agent loop
/// bypassed it, so one multi-MB build log became many resident copies across
/// child messages, checkpoints, transcript handles, and persistence — the
/// Fleet fanout memory blow-up. Over-threshold content (successes AND
/// errors: sub-agent error output is routinely a full build log, so the root
/// loop's pass-errors-through rationale does not hold here) is written to the
/// shared spillover directory and replaced inline by a bounded head plus a
/// footer naming the on-disk path.
///
/// Returns the (possibly bounded) content and the spillover path when one was
/// written. Spillover write failures degrade to passing the original content
/// through, mirroring `apply_spillover`.
#[cfg(test)]
fn bound_subagent_tool_result(
    agent_id: &str,
    tool_id: &str,
    tool_name: &str,
    session_id: &str,
    success: bool,
    content: String,
) -> (String, Option<PathBuf>) {
    let spill_id = format!("sa_{agent_id}_{tool_id}");
    let mut result = if success {
        ToolResult::success(content)
    } else {
        ToolResult::error(content)
    };
    let path = crate::tools::truncate::apply_spillover_with_artifact_including_errors(
        &mut result,
        &spill_id,
        tool_name,
        session_id,
    );
    (result.content, path)
}

/// Rough serialized size of one message, used for checkpoint/transcript byte
/// budgets. Exact JSON size via serde; unserializable messages (should not
/// happen) count as 1 KiB so they still consume budget.
fn approximate_message_bytes(message: &Message) -> usize {
    let mut counter = crate::utils::CountingWriter::new();
    serde_json::to_writer(&mut counter, message).map_or(1024, |()| counter.count())
}

/// Keep the most recent messages, after `safe_tool_result_message_projection`,
/// whose combined approximate size fits `budget_bytes`. Always keeps at least
/// the final message (even if it alone exceeds the budget) so a non-empty
/// history stays continuable. Returns the retained tail and how many older
/// messages were omitted.
fn bounded_tail_messages(messages: &[Message], budget_bytes: usize) -> (Vec<Message>, usize) {
    let mut kept_rev: Vec<Message> = Vec::new();
    let mut used = 0usize;
    for message in messages.iter().rev() {
        let projected =
            crate::image_attach::safe_tool_result_message_projection(std::slice::from_ref(message));
        let size: usize = projected.iter().map(approximate_message_bytes).sum();
        if !kept_rev.is_empty() && used.saturating_add(size) > budget_bytes {
            break;
        }
        used = used.saturating_add(size);
        kept_rev.extend(projected);
    }
    kept_rev.reverse();
    let omitted = messages.len().saturating_sub(kept_rev.len());
    (kept_rev, omitted)
}

fn build_subagent_checkpoint(
    agent_id: &str,
    reason: impl Into<String>,
    messages: &[Message],
    steps_taken: u32,
    continuable: bool,
) -> SubAgentCheckpoint {
    let created_at_ms = epoch_millis_now();
    let checkpoint_id = format!("{agent_id}:step:{steps_taken}:ts:{created_at_ms}");
    let (bounded_messages, omitted_messages) =
        bounded_tail_messages(messages, SUBAGENT_CHECKPOINT_MESSAGE_BUDGET_BYTES);
    SubAgentCheckpoint {
        checkpoint_id: checkpoint_id.clone(),
        agent_id: agent_id.to_string(),
        continuation_handle: format!("agent:{agent_id}:checkpoint:{checkpoint_id}"),
        reason: reason.into(),
        continuable,
        steps_taken,
        message_count: messages.len(),
        created_at_ms,
        messages: bounded_messages,
        omitted_messages,
        parked_at_turn_end: false,
    }
}

async fn checkpoint_subagent_progress(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    reason: impl Into<String>,
    messages: &[Message],
    steps_taken: u32,
    continuable: bool,
) -> SubAgentCheckpoint {
    let checkpoint =
        build_subagent_checkpoint(agent_id, reason, messages, steps_taken, continuable);
    let mut manager = runtime.manager.write().await;
    manager.update_checkpoint(agent_id, checkpoint.clone());
    checkpoint
}

/// Render a checkpoint's message tail as readable prose for re-seeding a
/// resumed session. Text and tool blocks are shown; images and internal
/// thinking blocks are summarized/omitted (thinking was already consumed by
/// the interrupted model step).
fn checkpoint_messages_to_text(messages: &[Message]) -> String {
    let mut lines = Vec::new();
    for message in messages {
        let mut parts: Vec<String> = Vec::new();
        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. } => parts.push(text.clone()),
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => parts.push(format!("[tool use {name} (id {id}): {input}]")),
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } => {
                    let verdict = if is_error == &Some(true) {
                        "error"
                    } else {
                        "ok"
                    };
                    parts.push(format!(
                        "[tool result {tool_use_id} ({verdict}): {content}]"
                    ))
                }
                ContentBlock::ServerToolUse {
                    id, name, input, ..
                } => parts.push(format!("[server tool use {name} (id {id}): {input}]")),
                ContentBlock::ToolSearchToolResult {
                    tool_use_id,
                    content,
                    ..
                } => parts.push(format!("[tool search result {tool_use_id}: {content}]")),
                ContentBlock::CodeExecutionToolResult {
                    tool_use_id,
                    content,
                    ..
                } => parts.push(format!("[code execution result {tool_use_id}: {content}]")),
                ContentBlock::ImageUrl { .. } => parts.push("[image]".to_string()),
                ContentBlock::Thinking { .. } => {}
            }
        }
        if parts.is_empty() {
            continue;
        }
        lines.push(format!("[{}]\n{}", message.role, parts.join("\n")));
    }
    lines.join("\n\n")
}

/// Build the prompt for a resumed session: the original objective plus the
/// checkpoint context tail and the parent's follow-up as the latest
/// instruction.
fn build_resume_prompt(
    original_prompt: &str,
    checkpoint: &SubAgentCheckpoint,
    followup_text: &str,
) -> String {
    format!(
        "{original_prompt}\n\n\
         [RESUMED SESSION — checkpoint {}]\n\
         This session was interrupted after {} step(s). The prior conversation tail follows; \
         continue the work from where it left off. The parent follow-up below is the latest \
         instruction.\n\n\
         --- prior conversation (tail) ---\n{}\n--- end prior conversation ---\n\n\
         Parent follow-up: {followup_text}",
        checkpoint.checkpoint_id,
        checkpoint.steps_taken,
        checkpoint_messages_to_text(&checkpoint.messages),
    )
}

fn needs_input_for_interrupted_checkpoint(
    reason: &str,
    checkpoint: &SubAgentCheckpoint,
) -> SubAgentNeedsInput {
    SubAgentNeedsInput {
        question: format!(
            "Sub-agent interrupted before completion ({reason}). Re-dispatch this worker or provide explicit follow-up using checkpoint {}.",
            checkpoint.continuation_handle
        ),
    }
}

fn subagent_transient_provider_retry_delay(retry_number: u32) -> Duration {
    let multiplier = 1u32
        .checked_shl(retry_number.saturating_sub(1))
        .unwrap_or(4);
    SUBAGENT_TRANSIENT_PROVIDER_INITIAL_BACKOFF.saturating_mul(multiplier.min(4))
}

/// Deterministic exponential backoff for a timed-out per-step API call
/// (`retry_number` is 1-based): `initial_backoff`, ×2, ×4, …, capped at
/// [`SUBAGENT_API_TIMEOUT_MAX_BACKOFF`].
fn subagent_api_timeout_retry_base_delay(retry_number: u32, initial_backoff: Duration) -> Duration {
    let multiplier = 1u32
        .checked_shl(retry_number.saturating_sub(1))
        .unwrap_or(u32::MAX);
    initial_backoff
        .saturating_mul(multiplier)
        .min(SUBAGENT_API_TIMEOUT_MAX_BACKOFF)
}

/// Timeout retry backoff with ±20% jitter (UUID v4 entropy, the same idiom
/// as `llm_client::RetryConfig::delay_for_attempt`) so a fan-out of children
/// whose calls all time out together does not re-fire in lockstep.
fn subagent_api_timeout_retry_delay(retry_number: u32, initial_backoff: Duration) -> Duration {
    let base = subagent_api_timeout_retry_base_delay(retry_number, initial_backoff);
    let bytes = *Uuid::new_v4().as_bytes();
    let sample = u16::from_le_bytes([bytes[0], bytes[1]]);
    let random_factor = f64::from(sample) / f64::from(u16::MAX); // 0.0 to 1.0
    let jitter = base.as_secs_f64()
        * SUBAGENT_API_TIMEOUT_BACKOFF_JITTER_FACTOR
        * (2.0 * random_factor - 1.0); // -20% to +20%
    Duration::from_secs_f64((base.as_secs_f64() + jitter).max(0.0))
}

#[derive(Debug, Clone, Copy)]
struct RetryableSubAgentProviderFailure {
    label: &'static str,
    checkpoint_reason: &'static str,
    delay: Duration,
}

fn retryable_subagent_provider_failure(
    error: &anyhow::Error,
    retry_number: u32,
) -> Option<RetryableSubAgentProviderFailure> {
    if let Some(LlmError::RateLimited { retry_after, .. }) = error.downcast_ref::<LlmError>() {
        return Some(RetryableSubAgentProviderFailure {
            label: "rate-limited provider response",
            checkpoint_reason: "api_rate_limited",
            // Honor the provider's `Retry-After` when present. Without it,
            // back off exponentially with full jitter (capped at 120s) so a
            // fan-out of children 429'd by the same provider response does
            // not retry in lockstep (thundering herd).
            delay: retry_after.unwrap_or_else(|| governor::rate_limit_retry_delay(retry_number)),
        });
    }

    if is_transient_subagent_provider_error(error) {
        return Some(RetryableSubAgentProviderFailure {
            label: "transient provider failure",
            checkpoint_reason: "api_transient_provider_failure",
            delay: subagent_transient_provider_retry_delay(retry_number),
        });
    }

    None
}

fn is_transient_subagent_provider_error(error: &anyhow::Error) -> bool {
    // Durable account, model, and policy refusals outrank message text: a
    // quota error mentioning "429" is not a transient rate limit. Retain the
    // transport fallback for generic/parse errors and relay HTTP 400s, which
    // can carry an upstream timeout rather than an invalid request.
    let message = format!("{error:#}").to_ascii_lowercase();
    if crate::llm_client::image_count_rejection(&message).is_some() {
        return false;
    }
    match error.downcast_ref::<LlmError>() {
        Some(
            LlmError::QuotaExhausted(_)
            | LlmError::AuthenticationError(_)
            | LlmError::AuthorizationError(_)
            | LlmError::ModelError(_)
            | LlmError::ContentPolicyError(_)
            | LlmError::ContextLengthError(_),
        ) => return false,
        Some(error) if error.is_retryable() => return true,
        _ => {}
    }

    [
        "did not receive response headers",
        "response headers",
        "stream request",
        "request timed out",
        "operation timed out",
        "deadline has elapsed",
        "connection reset",
        "connection closed",
        "connection aborted",
        "temporarily unavailable",
        "bad gateway",
        "gateway timeout",
        "service unavailable",
        "rate limited",
        "rate_limit",
        "rate_limited",
        "too many requests",
        "429",
        "502",
        "503",
        "504",
        // Body/decode failures are transport-class: the provider accepted the
        // request and died mid-response (a DeepSeek stream decode error killed
        // a 141s scout with zero retries — morning-report issue #7). One
        // same-prompt retry is cheap next to re-planning the child.
        "error decoding response body",
        "failed to read chat api response body",
        "failed to parse chat api json",
        "unexpected end of file",
        "incomplete message",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn record_agent_progress(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    activity: AgentProgressEventMeta,
    message: impl Into<String>,
) {
    let message = message.into();
    if let Ok(mut manager) = runtime.manager.try_write() {
        manager.touch(agent_id);
        manager.record_worker_event(
            agent_id,
            activity.worker_status,
            Some(message.clone()),
            activity.step,
            activity.tool_name.clone(),
        );
    }
    emit_agent_progress(
        runtime.event_tx.as_ref(),
        &runtime.context.state_namespace,
        agent_id,
        message,
        activity,
        runtime.parent_agent_id.clone(),
        runtime.spawn_depth,
    );
}

/// Bound on the nested-agent completion inbox (#6147): one completion per
/// terminated nested child, drained by the parent agent's turn loop. A full
/// inbox delays a completion rather than dropping it; see
/// [`send_terminal_event`].
#[cfg(test)]
const CHILD_COMPLETION_CHANNEL_CAPACITY: usize = 64;

#[cfg(test)]
fn runtime_for_nested_agent_tools(
    runtime: &SubAgentRuntime,
    parent_agent_id: &str,
    fork_context: SubAgentForkContext,
) -> (SubAgentRuntime, mpsc::Receiver<SubAgentCompletion>) {
    let (child_completion_tx, child_completion_rx) =
        mpsc::channel::<SubAgentCompletion>(CHILD_COMPLETION_CHANNEL_CAPACITY);
    let runtime_for_tools = runtime
        .clone()
        .with_parent_completion_tx(child_completion_tx)
        .with_fork_context(fork_context);
    let runtime_for_tools = SubAgentRuntime {
        parent_agent_id: Some(parent_agent_id.to_string()),
        ..runtime_for_tools
    };
    (runtime_for_tools, child_completion_rx)
}

#[cfg(test)]
fn child_completion_runtime_message(completions: &[SubAgentCompletion]) -> Message {
    let mut text = String::from(
        "<codewhale:runtime_event kind=\"child_subagent_completion\" visibility=\"internal\">\n\
This is an internal runtime event, not user input. One or more child sub-agents \
you spawned have finished. Treat each child summary as an unverified self-report: \
if you rely on it, cite the child agent_id and the EVIDENCE lines it provided, \
and distinguish that from evidence you personally verified. A sentinel marked \
event=subagent.failed is high priority: inspect its failure_class and transcript_handle, \
then re-plan dependent work before claiming completion.\n",
    );
    for completion in completions {
        text.push_str("\n--- child sub-agent completion ---\n");
        text.push_str("agent_id: ");
        text.push_str(&completion.agent_id);
        text.push('\n');
        text.push_str(&completion.payload);
        text.push('\n');
    }
    text.push_str("</codewhale:runtime_event>");

    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text,
            cache_control: None,
        }],
    }
}

pub(crate) fn subagent_followup_recovery(agent_id: &str) -> String {
    format!(
        "Continue this child with agent(action=\"followup\", agent_id=\"{agent_id}\", message=\"Continue the assignment.\"). The response identifies its current continuation; the original receipt remains intact."
    )
}

fn subagent_cancellation_projection(
    agent_id: &str,
    messages: &[Message],
    steps: u32,
    latest_checkpoint: Option<&SubAgentCheckpoint>,
    turn_end_parking: Option<&Arc<std::sync::atomic::AtomicBool>>,
) -> (
    SubAgentStatus,
    Option<String>,
    Option<SubAgentCheckpoint>,
    Option<SubAgentNeedsInput>,
    AgentWorkerStatus,
    &'static str,
) {
    let parking_requested =
        turn_end_parking.is_some_and(|signal| signal.load(std::sync::atomic::Ordering::Acquire));
    if parking_requested {
        let recovery = subagent_followup_recovery(agent_id);
        let reason = format!("Work was parked with a continuable checkpoint. {recovery}");
        let mut checkpoint = build_subagent_checkpoint(agent_id, &reason, messages, steps, true);
        // Parked, not asking: nothing will answer this child, and its write
        // claim must stop gating peers immediately (#5906).
        checkpoint.parked_at_turn_end = true;
        let needs_input = SubAgentNeedsInput { question: recovery };
        return (
            SubAgentStatus::Interrupted(reason.clone()),
            Some(reason),
            Some(checkpoint),
            Some(needs_input),
            AgentWorkerStatus::Interrupted,
            "parked at parent turn end",
        );
    }

    (
        SubAgentStatus::Cancelled,
        None,
        latest_checkpoint.cloned(),
        None,
        AgentWorkerStatus::Cancelled,
        "cancelled",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum SubAgentLoopBoundary {
    Continue,
    Cancellation,
    StepLimit,
}

#[cfg(test)]
fn subagent_loop_boundary(
    max_steps: u32,
    steps: u32,
    cancellation_requested: bool,
) -> SubAgentLoopBoundary {
    if cancellation_requested {
        SubAgentLoopBoundary::Cancellation
    } else if max_steps > 0 && steps >= max_steps {
        SubAgentLoopBoundary::StepLimit
    } else {
        SubAgentLoopBoundary::Continue
    }
}

#[allow(clippy::too_many_arguments)]
async fn cancelled_subagent_result(
    runtime: &SubAgentRuntime,
    agent_id: &str,
    agent_type: &FleetRole,
    assignment: &SubAgentAssignment,
    messages: &[Message],
    steps: u32,
    max_steps: u32,
    latest_checkpoint: Option<&SubAgentCheckpoint>,
    turn_end_parking: Option<&Arc<std::sync::atomic::AtomicBool>>,
    transcript_artifact: &mut Option<SubAgentTranscriptArtifactWriter>,
    started_at: Instant,
    fork_context_enabled: bool,
    progress_suffix: &str,
) -> SubAgentResult {
    let (status, result, checkpoint, needs_input, worker_status, progress) =
        subagent_cancellation_projection(
            agent_id,
            messages,
            steps,
            latest_checkpoint,
            turn_end_parking,
        );
    record_agent_progress(
        runtime,
        agent_id,
        AgentProgressEventMeta::new(worker_status).with_step(steps),
        format!(
            "{}: {progress}{progress_suffix}",
            format_step_counter(steps, max_steps)
        ),
    );
    let duration_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    insert_subagent_full_transcript_handle(
        runtime,
        agent_id,
        agent_type,
        assignment,
        &status,
        result.as_ref(),
        checkpoint.as_ref(),
        transcript_artifact.as_mut(),
        messages,
        steps,
        duration_ms,
        fork_context_enabled,
    )
    .await;
    SubAgentResult {
        usage: None,
        name: agent_id.to_string(),
        agent_id: agent_id.to_string(),
        context_mode: if fork_context_enabled {
            "forked"
        } else {
            "fresh"
        }
        .to_string(),
        fork_context: fork_context_enabled,
        workspace: Some(runtime.context.workspace.clone()),
        git_branch: current_git_branch(&runtime.context.workspace),
        agent_type: agent_type.clone(),
        assignment: assignment.clone(),
        model: runtime.model.clone(),
        nickname: None,
        status,
        worker_status: None,
        runtime_permissions: None,
        parent_run_id: runtime.parent_agent_id.clone(),
        spawn_depth: runtime.spawn_depth,
        child_route: None,
        result,
        steps_taken: steps,
        checkpoint,
        needs_input,
        duration_ms,
        started_at: Some(started_at),
        from_prior_session: false,
        idle_ms: None,
        heartbeat_timeout_ms: None,
    }
}

/// First non-empty string among `keys`, refusing a wrong type.
///
/// A key that is absent, `null`, or an empty/blank string falls through to
/// the next spelling exactly as before. Anything else present under one of
/// these names is a type error naming the parameter: silently dropping a
/// value the caller supplied is how a declared restriction evaporates.
fn optional_input_str<'a>(input: &'a Value, keys: &[&str]) -> Result<Option<&'a str>, ToolError> {
    for key in keys {
        let Some(value) = input.get(*key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let text = value
            .as_str()
            .ok_or_else(|| codewhale_tools::type_mismatch(key, value, "a string"))?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        return Ok(Some(trimmed));
    }
    Ok(None)
}

/// First present value among `keys`, treating `null` as absent.
fn aliased_value<'a, 'k>(input: &'a Value, keys: &'k [&'k str]) -> Option<(&'k str, &'a Value)> {
    keys.iter().find_map(|key| {
        input
            .get(*key)
            .filter(|value| !value.is_null())
            .map(|value| (*key, value))
    })
}

/// Optional aliased u64, refusing a wrong type instead of dropping it.
fn parse_optional_u64(input: &Value, keys: &[&str]) -> Result<Option<u64>, ToolError> {
    let Some((key, value)) = aliased_value(input, keys) else {
        return Ok(None);
    };
    codewhale_tools::json_nonnegative_integer(value)
        .map(Some)
        .ok_or_else(|| codewhale_tools::type_mismatch(key, value, "a non-negative integer"))
}

/// Optional array of tool names, refusing a wrong type instead of dropping it.
///
/// A deny-list handed over as a bare string used to vanish without a word,
/// which silently *widened* the child's authority. Entries are trimmed and
/// de-duplicated; blanks are dropped.
fn parse_tool_name_list(input: &Value, key: &str) -> Result<Option<Vec<String>>, ToolError> {
    let Some(value) = input.get(key).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let array = value
        .as_array()
        .ok_or_else(|| codewhale_tools::type_mismatch(key, value, "an array of tool names"))?;
    let mut tools: Vec<String> = Vec::new();
    for item in array {
        let tool = item
            .as_str()
            .ok_or_else(|| codewhale_tools::type_mismatch(&format!("{key}[]"), item, "a string"))?;
        let trimmed = tool.trim();
        if !trimmed.is_empty() && !tools.iter().any(|existing| existing == trimmed) {
            tools.push(trimmed.to_string());
        }
    }
    Ok(Some(tools))
}

fn parse_text_or_items(
    input: &Value,
    text_keys: &[&str],
    items_key: &str,
    required_field: &str,
) -> Result<String, ToolError> {
    let text = optional_input_str(input, text_keys)?.map(str::to_string);
    let items = parse_items_text(input, items_key)?;
    match (text, items) {
        (Some(_), Some(_)) => Err(ToolError::invalid_input(format!(
            "Provide either {required_field} text or {items_key}, but not both"
        ))),
        (Some(text), None) => Ok(text),
        (None, Some(items)) => Ok(items),
        (None, None) => Err(ToolError::missing_field(required_field)),
    }
}

fn parse_items_text(input: &Value, key: &str) -> Result<Option<String>, ToolError> {
    let Some(items) = input.get(key) else {
        return Ok(None);
    };
    let array = items
        .as_array()
        .ok_or_else(|| ToolError::invalid_input(format!("'{key}' must be an array")))?;
    if array.is_empty() {
        return Err(ToolError::invalid_input(format!("'{key}' cannot be empty")));
    }

    let mut lines = Vec::new();
    for item in array {
        let object = item
            .as_object()
            .ok_or_else(|| ToolError::invalid_input("each item must be an object"))?;
        let item_type = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("text")
            .trim();
        let rendered = match item_type {
            "text" => object
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .ok_or_else(|| ToolError::invalid_input("text item requires non-empty text"))?,
            "mention" => {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| ToolError::invalid_input("mention item requires name"))?;
                let path = object
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| ToolError::invalid_input("mention item requires path"))?;
                format!("[mention:${name}]({path})")
            }
            "skill" => {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| ToolError::invalid_input("skill item requires name"))?;
                let path = object
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| ToolError::invalid_input("skill item requires path"))?;
                format!("[skill:${name}]({path})")
            }
            "local_image" => {
                let path = object
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| ToolError::invalid_input("local_image item requires path"))?;
                format!("[local_image:{path}]")
            }
            "image" => {
                let url = object
                    .get("image_url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .ok_or_else(|| ToolError::invalid_input("image item requires image_url"))?;
                format!("[image:{url}]")
            }
            _ => object
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "[input]".to_string()),
        };
        lines.push(rendered);
    }

    Ok(Some(lines.join("\n")))
}

fn parse_spawn_request(input: &Value) -> Result<SpawnRequest, ToolError> {
    let prompt = parse_text_or_items(
        input,
        &["prompt", "message", "objective"],
        "items",
        "prompt",
    )?;
    let dependencies = parse_bounded_strings(input, "dependencies", 8)?;
    let acceptance = parse_bounded_strings(input, "acceptance", 8)?;
    let session_name = optional_input_str(input, &["name", "session_name"])?
        .map(validate_session_name)
        .transpose()?;

    let type_input = optional_input_str(input, &["type", "agent_type", "agent_name"])?;
    let role_input = optional_input_str(input, &["role", "agent_role"])?;

    let parsed_type = type_input
        .map(|kind| {
            FleetRole::from_str(kind).ok_or_else(|| {
                ToolError::invalid_input(format!(
                    "Invalid sub-agent type '{kind}'. Use: {VALID_SUBAGENT_TYPES}"
                ))
            })
        })
        .transpose()?;

    // Role may be either a FleetRole alias (reviewer → FleetRole::Reviewer)
    // or a Fleet role name resolved later (`resolve_spawn_role`). Type aliases
    // still set agent_type; non-alias roles resolve as the profile key (#4177).
    let parsed_role_type = role_input.and_then(FleetRole::from_str);
    let role_is_type_alias = parsed_role_type.is_some();

    if let (Some(type_kind), Some(role_kind)) = (&parsed_type, &parsed_role_type)
        && type_kind != role_kind
    {
        return Err(ToolError::invalid_input(
            "Fleet role conflicts with the explicit legacy agent type".to_string(),
        ));
    }

    let agent_type_explicit = parsed_type.is_some() || parsed_role_type.is_some();
    let agent_type_named = parsed_type.is_some();
    let agent_type = parsed_type
        .or(parsed_role_type)
        .unwrap_or(FleetRole::Worker);

    // Fleet role token: the raw role only when it is not a descriptive type
    // alias. Type aliases remain local FleetRole vocabulary and must not be
    // promoted into profile keys.
    let fleet_role_token = match role_input {
        Some(raw) if !role_is_type_alias => {
            let token = validate_role_name(raw)?;
            Some(token)
        }
        _ => None,
    };

    // Assignment role label: the canonical spelling of whichever type/role
    // alias was given (both agree past the conflict check above), otherwise
    // the raw Fleet role token that `resolve_spawn_role` resolves later.
    let role = if agent_type_explicit {
        Some(agent_type.as_str().to_string())
    } else {
        fleet_role_token.clone()
    };

    let mut profile = optional_input_str(input, &["profile", "fleet_profile", "roster_profile"])?
        .map(validate_profile_name)
        .transpose()?;
    // When the caller declared a non-type Fleet role, use it as the profile
    // key so `resolve_spawn_role` is the single role resolution path.
    // Descriptive FleetRole aliases (worker/review/plan/verify/...) keep
    // profile=None; promoting those aliases to profile keys made valid direct
    // agent calls fail because several are not role ids (#4177).
    if profile.is_none() {
        profile = fleet_role_token.clone();
    }

    let allowed_tools = parse_tool_name_list(input, "allowed_tools")?;

    let cwd = parse_optional_cwd(input)?;
    let worktree = parse_optional_worktree_request(input)?;
    let model = parse_optional_subagent_model(input, "model")?;
    let explicit_model_strength = optional_input_str(input, &["model_strength", "modelStrength"])?
        .map(SubAgentModelStrength::parse)
        .transpose()?;
    let model_strength_explicit = explicit_model_strength.is_some();
    // Fleet is predictable before setup: every role inherits the active model.
    // A cheaper sibling is an explicit routing choice through model_strength,
    // a saved Fleet profile, or a concrete model override.
    let model_strength = explicit_model_strength.unwrap_or(SubAgentModelStrength::Same);
    let requested_thinking =
        optional_input_str(input, &["thinking", "reasoning_effort", "reasoningEffort"])?;
    let thinking_explicit = requested_thinking.is_some();
    let thinking = requested_thinking
        .map(SubAgentThinking::parse)
        .transpose()?
        .unwrap_or(SubAgentThinking::Inherit);
    let resident_file = optional_input_str(input, &["resident_file"])?.map(str::to_string);
    let detached = parse_optional_bool(input, &["detached"])?.unwrap_or(false);
    let fork_context =
        parse_optional_bool(input, &["fork_context", "forkContext", "inherit_context"])?;
    let max_depth = parse_optional_u64(input, &["max_depth", "maxDepth", "max_spawn_depth"])?
        .map(|depth| {
            let ceiling = codewhale_config::MAX_SPAWN_DEPTH_CEILING;
            u32::try_from(depth)
                .map_err(|_| {
                    ToolError::invalid_input(format!("max_depth must be between 0 and {ceiling}"))
                })
                .and_then(|depth| {
                    if depth <= ceiling {
                        Ok(depth)
                    } else {
                        Err(ToolError::invalid_input(format!(
                            "max_depth must be between 0 and {ceiling}"
                        )))
                    }
                })
        })
        .transpose()?;
    let max_output_tokens = parse_optional_bounded_limit(
        input,
        &["max_output_tokens", "maxOutputTokens"],
        u32::MAX as u64,
    )?
    .map(|n| n as u32);
    let max_steps = parse_optional_bounded_limit(
        input,
        &["max_steps", "maxSteps"],
        u64::from(MAX_SUBAGENT_STEPS),
    )?
    .map(|steps| u32::try_from(steps).expect("validated model-turn range"));
    let wall_time = parse_optional_bounded_limit(
        input,
        &["wall_time_secs", "wallTimeSecs"],
        MAX_CHILD_WALL_TIME.as_secs(),
    )?
    .map(Duration::from_secs);

    // #4042: optional caller-supplied tool deny-list (unioned with the parent's
    // inherited deny-list) and the legacy inheritance flag (cannot relax policy).
    let disallowed_tools = parse_disallowed_tools(input)?;
    let inherit_disallowed_tools = parse_optional_bool(
        input,
        &["inherit_disallowed_tools", "inheritDisallowedTools"],
    )?
    .unwrap_or(true);

    // Deliberate delegation contract: when `deliberate=true`, require the
    // model to declare task type (or profile), workspace policy, expected
    // artifact, and write authority. The declared values are
    // parsed and ENFORCED whenever present (deliberate or not): declaring
    // authority the runtime ignores would be a false affordance
    // (TUI-DOG-017).
    let deliberate = parse_optional_bool(input, &["deliberate"])?.unwrap_or(false);
    let workspace_policy_str = optional_input_str(input, &["workspace_policy", "workspacePolicy"])?;
    let expected_artifact = optional_input_str(input, &["expected_artifact", "expectedArtifact"])?
        .map(str::trim)
        .filter(|artifact| !artifact.is_empty())
        .map(str::to_string);
    if input
        .get("deliverables")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items
                .iter()
                .any(|item| item.as_str().is_some_and(|path| path.trim().is_empty()))
        })
    {
        return Err(ToolError::invalid_input(
            "deliverables paths must be non-empty",
        ));
    }
    let deliverables = delivery::declared_paths(
        &parse_bounded_strings(input, "deliverables", delivery::MAX_DELIVERABLES)?,
        expected_artifact.as_deref(),
    )
    .map_err(ToolError::invalid_input)?;
    let write_authority_str = optional_input_str(input, &["write_authority", "writeAuthority"])?;
    if deliberate {
        let has_type = agent_type_explicit || profile.is_some();
        let mut missing = Vec::new();
        if !has_type {
            missing.push("type (or profile)");
        }
        if workspace_policy_str.is_none() && worktree.is_none() {
            missing.push("workspace_policy (or worktree=true)");
        }
        if expected_artifact.is_none() && deliverables.is_empty() {
            missing.push("expected_artifact");
        }
        if write_authority_str.is_none() {
            missing.push("write_authority");
        }
        if !missing.is_empty() {
            return Err(ToolError::invalid_input(format!(
                "deliberate spawn requires: {}. Missing: {}.",
                "type/profile, workspace_policy, expected_artifact, write_authority",
                missing.join(", ")
            )));
        }
    }
    // Enforce the declared workspace policy: `worktree` materializes a real
    // worktree request (the separate `worktree` field is the mechanism that
    // actually creates one), and `shared` must not contradict an explicit
    // worktree ask.
    let worktree = match workspace_policy_str
        .map(|policy| policy.trim().to_ascii_lowercase())
        .as_deref()
    {
        None => worktree,
        Some("worktree") => worktree.or(Some(SubAgentWorktreeRequest {
            branch: None,
            path: None,
            base_ref: None,
        })),
        Some("shared") => {
            if worktree.is_some() {
                return Err(ToolError::invalid_input(
                    "workspace_policy 'shared' conflicts with worktree isolation options; \
                     use workspace_policy 'worktree' or drop the worktree fields.",
                ));
            }
            worktree
        }
        Some(other) => {
            return Err(ToolError::invalid_input(format!(
                "Invalid workspace_policy '{other}'. Use shared or worktree."
            )));
        }
    };
    let write_authority = match write_authority_str
        .map(|auth| auth.trim().to_ascii_lowercase())
        .as_deref()
    {
        None => None,
        Some("read_only") => Some(SpawnWriteAuthority::ReadOnly),
        Some("workspace_write") => Some(SpawnWriteAuthority::WorkspaceWrite),
        Some("worktree_write") => Some(SpawnWriteAuthority::WorktreeWrite),
        Some(other) => {
            return Err(ToolError::invalid_input(format!(
                "Invalid write_authority '{other}'. Use read_only, workspace_write, or worktree_write."
            )));
        }
    };
    if write_authority == Some(SpawnWriteAuthority::WorktreeWrite) && worktree.is_none() {
        return Err(ToolError::invalid_input(
            "write_authority 'worktree_write' requires worktree isolation \
             (workspace_policy 'worktree' or worktree=true).",
        ));
    }
    let write_roots = parse_coordination_paths(input, "write_roots")?;
    let exact_files = parse_coordination_paths(input, "exact_files")?;
    let coordination_contracts = parse_bounded_strings(input, "coordination_contracts", 16)?;
    let resume_from = optional_input_str(input, &["resume_from", "resumeFrom"])?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let prompt_only_general = agent_type == FleetRole::Worker
        && !agent_type_explicit
        && profile.is_none()
        && role_input.is_none()
        && type_input.is_none();
    let unresolved_profile = profile.is_some();
    let mut request = SpawnRequest {
        session_name,
        prompt: prompt.clone(),
        dependencies,
        acceptance,
        agent_type,
        agent_type_explicit,
        agent_type_named,
        profile,
        assignment: SubAgentAssignment::new(prompt, role),
        allowed_tools,
        model,
        model_strength,
        model_strength_explicit,
        thinking,
        thinking_explicit,
        cwd,
        worktree,
        resident_file,
        fork_context,
        max_depth,
        max_output_tokens,
        max_steps,
        wall_time,
        disallowed_tools,
        inherit_disallowed_tools,
        write_authority,
        expected_artifact,
        deliverables,
        write_roots,
        exact_files,
        coordination_contracts,
        resume_from,
        detached,
    };
    // A role profile may resolve the parse-time General placeholder to a
    // read-only scout/reviewer or to a write-capable manager/builder. Defer
    // classification until resolve_spawn_role runs; all profile-less requests
    // can be validated immediately.
    if !unresolved_profile {
        validate_spawn_write_contract(&mut request, prompt_only_general)?;
    }
    Ok(request)
}

fn validate_spawn_write_contract(
    request: &mut SpawnRequest,
    allow_prompt_only_general: bool,
) -> Result<(), ToolError> {
    if matches!(
        request.agent_type,
        FleetRole::Scout
            | FleetRole::Planner
            | FleetRole::Reviewer
            | FleetRole::Verifier
            | FleetRole::Consultant
    ) && request
        .write_authority
        .is_some_and(|authority| authority != SpawnWriteAuthority::ReadOnly)
    {
        return Err(ToolError::invalid_input(format!(
            "{} is a read-only role and cannot declare write-capable authority",
            request.agent_type.as_str()
        )));
    }
    // #5123: `type=builder` plus read_only authority used to parse, then
    // silently clamp write/shell off — the child self-BLOCKED as a "builder"
    // holding only read-only inspection tools, after burning a turn
    // discovering it. Fail closed at spawn instead.
    //
    // Two narrowings keep this to the actual lie:
    //
    // 1. Only `type` counts, not `role`. `role: "advisor"` is a role name
    //    copied into `profile` as a resolution key, and the role is not resolved
    //    until `resolve_spawn_role`, so the role says nothing here about write
    //    capability. `role: "implementer"` is a type alias but still an
    //    identity — a Fleet role and its authority posture are independent, and
    //    an acceptance workflow must be able to resolve `implementer` to its
    //    saved profile while narrowing that child to the read-only tool set.
    //
    // 2. Only `builder` counts, not `worker`. Builder is the explicitly
    //    write-capable role, and it is the one the #5123 transcript shows
    //    BLOCKED — the worker in that same transcript ran fine. Worker is the
    //    unnamed default (it renders as "general") whose capability comes from
    //    authority rather than from its name, so a read-only worker is an
    //    ordinary general-purpose child, not a contradiction. The release QA
    //    contract calls exactly that set — worker, scout, reviewer, verifier —
    //    the four canonical read-only Fleet roles.
    //
    // Widening past either narrowing rejected every read-only Workflow leaf and
    // the canonical read-only worker along with it.
    if request.agent_type_named
        && request.agent_type == FleetRole::Builder
        && request.write_authority == Some(SpawnWriteAuthority::ReadOnly)
    {
        return Err(ToolError::invalid_input(format!(
            "{} implies write capability; write_authority=read_only is a contradiction. \
            Use type=explore (or another read-only role), or set write_authority to \
             workspace_write / worktree_write.",
            request.agent_type.as_str()
        )));
    }
    let declares_scope = !request.write_roots.is_empty()
        || !request.exact_files.is_empty()
        || !request.coordination_contracts.is_empty();
    if request.write_authority == Some(SpawnWriteAuthority::ReadOnly) && declares_scope {
        return Err(ToolError::invalid_input(
            "read_only authority cannot declare write_roots, exact_files, or coordination_contracts"
                .to_string(),
        ));
    }
    if request.agent_type == FleetRole::Custom && request.write_authority.is_none() {
        if declares_scope {
            return Err(ToolError::invalid_input(
                "custom write scopes require explicit workspace_write or worktree_write authority"
                    .to_string(),
            ));
        }
        request.write_authority = Some(SpawnWriteAuthority::ReadOnly);
    }
    let write_capable = spawn_request_is_write_capable(request);
    if write_capable
        && request.write_roots.is_empty()
        && request.exact_files.is_empty()
        && request.coordination_contracts.is_empty()
    {
        if !request.deliverables.is_empty() {
            request.exact_files = request.deliverables.clone();
        } else if request.write_authority.is_some() || !allow_prompt_only_general {
            // Default write scope to the parent workspace root. Escalation
            // outside the workspace is still refused by path normalization
            // when an explicit scope is declared.
            request.write_roots = vec![".".to_string()];
        } else {
            // A prompt-only/general launch remains ergonomic but is not
            // silently granted the whole repository. It starts read-only
            // until the caller supplies an explicit write-capable identity
            // or mutation claim.
            request.write_authority = Some(SpawnWriteAuthority::ReadOnly);
        }
    }
    Ok(())
}

fn parse_bounded_strings(input: &Value, key: &str, limit: usize) -> Result<Vec<String>, ToolError> {
    let Some(value) = input.get(key) else {
        return Ok(Vec::new());
    };
    let Some(items) = value.as_array() else {
        return Err(ToolError::invalid_input(format!(
            "{key} must be an array of strings"
        )));
    };
    if items.len() > limit {
        return Err(ToolError::invalid_input(format!(
            "{key} accepts at most {limit} entries"
        )));
    }
    let mut result = Vec::new();
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(ToolError::invalid_input(format!(
                "{key} must contain only strings"
            )));
        };
        let text = text.trim();
        if text.chars().count() > 512 {
            return Err(ToolError::invalid_input(format!(
                "{key} entries must be at most 512 characters"
            )));
        }
        if !text.is_empty() && !result.iter().any(|existing| existing == text) {
            result.push(text.to_string());
        }
    }
    Ok(result)
}

fn parse_coordination_paths(input: &Value, key: &str) -> Result<Vec<String>, ToolError> {
    parse_bounded_strings(input, key, 32)?
        .into_iter()
        .map(|path| normalize_claim_path(&path).map_err(ToolError::invalid_input))
        .collect()
}

fn normalize_claim_path(path: &str) -> Result<String, String> {
    let path = path.replace('\\', "/");
    let trimmed = path.trim();
    if trimmed.len() > 4096 || trimmed.chars().any(|ch| matches!(ch, '\0' | '\r' | '\n')) {
        return Err("write scope path must be one bounded repo-relative line".to_string());
    }
    if trimmed.is_empty() || trimmed == "." {
        return Ok(".".to_string());
    }
    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return Err(format!(
            "write scope path must be repo-relative without traversal: {path}"
        ));
    }
    let mut components = Vec::new();
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => {
                let value = value.to_string_lossy().nfc().collect::<String>();
                if !value.is_empty() {
                    components.push(value);
                }
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "write scope path must be repo-relative without traversal: {path}"
                ));
            }
        }
    }
    if components.is_empty() {
        Ok(".".to_string())
    } else {
        Ok(components.join("/"))
    }
}

fn coordination_workspace_prefix(
    manager_workspace: &Path,
    worker_workspace: &Path,
) -> Result<String, String> {
    let manager_workspace = normalize_subagent_workspace(manager_workspace);
    let worker_workspace = normalize_subagent_workspace(worker_workspace);
    let relative = worker_workspace
        .strip_prefix(&manager_workspace)
        .map_err(|_| {
            format!(
                "shared writer workspace '{}' must remain inside coordination root '{}'",
                worker_workspace.display(),
                manager_workspace.display()
            )
        })?;
    normalize_claim_path(&relative.to_string_lossy())
}

fn namespace_coordination_path(prefix: &str, path: &str) -> Result<String, String> {
    let path = normalize_claim_path(path)?;
    match (prefix, path.as_str()) {
        (".", path) | (path, ".") => Ok(path.to_string()),
        (prefix, path) => normalize_claim_path(&format!("{prefix}/{path}")),
    }
}

fn validate_session_name(name: &str) -> Result<String, ToolError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ToolError::invalid_input("name cannot be blank"));
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err(ToolError::invalid_input(
            "name must not contain whitespace; use letters, numbers, '-', '_', or '.'",
        ));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(ToolError::invalid_input(
            "name may only contain ASCII letters, numbers, '-', '_', or '.'",
        ));
    }
    Ok(trimmed.to_string())
}

/// Validate a bounded human Fleet selector. Resolution owns normalization so
/// the route receipt can preserve the safe spelling the caller actually used
/// (`DeepSeek V4 Flash`, `role:scout`, or an exact member id).
fn validate_profile_name(value: &str) -> Result<String, ToolError> {
    validate_roster_selector(value, "profile")
}

fn validate_role_name(value: &str) -> Result<String, ToolError> {
    validate_roster_selector(value, "role")
}

fn validate_roster_selector(value: &str, field: &str) -> Result<String, ToolError> {
    const MAX_SELECTOR_CHARS: usize = 128;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ToolError::invalid_input(format!("{field} cannot be blank")));
    }
    if trimmed.chars().count() > MAX_SELECTOR_CHARS {
        return Err(ToolError::invalid_input(format!(
            "{field} must be at most {MAX_SELECTOR_CHARS} characters"
        )));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ToolError::invalid_input(format!(
            "{field} must not contain control characters or newlines"
        )));
    }
    Ok(trimmed.to_string())
}

/// Re-read current saved role defaults and overlay the session's live explicit
/// subagent settings. Removed profile pins cannot survive as a launch snapshot.
fn refresh_spawn_route_sources(runtime: &mut SubAgentRuntime) {
    let Some(config) = runtime.api_config.as_deref() else {
        return;
    };
    let mut role_models = spawn_roster(runtime).model_overrides();
    role_models.extend(config.subagent_model_overrides());
    runtime.role_models = role_models;
}

fn spawn_roster(runtime: &SubAgentRuntime) -> crate::fleet::roster::FleetRoster {
    let config = runtime
        .api_config
        .as_deref()
        .map(crate::config::Config::fleet_config)
        .unwrap_or_default();
    crate::fleet::identity::load_effective_roster(
        &config,
        &runtime.context.workspace,
        runtime.context.plugin_registry.as_deref(),
    )
}

/// Explicit profiles win; otherwise a manual role pin precedes a saved role's
/// complete member route. Keep provider pins out of the provider-less model map.
fn resolve_spawn_route_profile(
    runtime: &SubAgentRuntime,
    request: &mut SpawnRequest,
    roster: &crate::fleet::roster::FleetRoster,
) -> Result<Option<crate::fleet::profile::AgentProfile>, ToolError> {
    let member = resolve_spawn_profile(request, roster)?;
    if member.is_some() || configured_manual_spawn_model(runtime, request)?.is_some() {
        return Ok(member);
    }
    // A role the caller never wrote is Codewhale's own default
    // (`FleetRole::Worker` -> "general"), not a request. Refusing an ambiguous
    // pin is right when the caller named a role, type or profile — it stops us
    // silently choosing one of several providers for them. But for a
    // prompt-only `agent(action=start, ...)` it turned our default into a hard
    // failure whenever two members happened to share role `general`, blocking
    // the spawn at the tool boundary over a selector the caller never asked
    // for (#6244). In that case an ambiguous pin means "no usable pin": fall
    // through to the session/operator route the spawn would have taken anyway.
    let requested_role = request.assignment.role.as_deref();
    let role = requested_role.unwrap_or_else(|| request.agent_type.as_str());
    let role_was_requested = requested_role.is_some() || request.agent_type_explicit;
    let member =
        match crate::fleet::worker_runtime::resolve_pinned_role_profile(roster.members(), role) {
            Ok(member) => member,
            Err(crate::fleet::identity::FleetSelectorError::Ambiguous { .. })
                if !role_was_requested =>
            {
                None
            }
            Err(error) => return Err(ToolError::invalid_input(error.to_string())),
        };
    let Some(member) = member else {
        return Ok(None);
    };
    request.profile = Some(format!("member:{}", member.id));
    // Reuse explicit-profile trust, posture, reasoning, and instruction binding.
    resolve_spawn_profile(request, roster)
}

/// Resolve saved identity through the same trusted roster as durable Fleet.
/// The role posture and the parent's policy remain the authority ceiling.
fn resolve_spawn_profile(
    request: &mut SpawnRequest,
    roster: &crate::fleet::roster::FleetRoster,
) -> Result<Option<crate::fleet::profile::AgentProfile>, ToolError> {
    if let Some(error) = roster.load_error() {
        return Err(ToolError::execution_failed(error.to_string()));
    }
    let Some(selector) = request.profile.as_deref() else {
        return Ok(None);
    };
    let member = roster
        .resolve_member(selector)
        .map_err(|error| ToolError::invalid_input(error.to_string()))?;
    let Some(member) = member else {
        if !roster.is_exact_selection() && FleetRole::from_str(selector).is_some() {
            resolve_spawn_role(request)?;
            return Ok(None);
        }
        let available = roster
            .members()
            .iter()
            .take(16)
            .map(|member| crate::fleet::identity::bounded_identity_field(&member.id))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ToolError::invalid_input(format!(
            "Unknown Fleet role/profile '{selector}'. Available members: {available}. Use action=roster."
        )));
    };
    if member.origin == crate::fleet::roster::ProfileOrigin::BuiltIn {
        resolve_spawn_role(request)?;
        return Ok(None);
    }
    apply_spawn_profile(request, member)?;
    Ok(Some(member.clone()))
}

/// Apply an already resolved member through the common posture, pin and
/// instruction checks. Exact Workflows supply their frozen member here;
/// ordinary Agent calls still resolve through the selected roster above.
fn apply_spawn_profile(
    request: &mut SpawnRequest,
    member: &crate::fleet::profile::AgentProfile,
) -> Result<(), ToolError> {
    if member.native_preset.is_none()
        && let Some(authority) = member.plugin_authority.as_ref()
    {
        crate::plugins::registry::verify_plugin_component_authority(
            authority, crate::plugins::activation::PluginActivationCapability::Agents,
        ).map_err(|reason| ToolError::permission_denied(format!(
            "Plugin Agent profile '{}' is unavailable: {reason}. Reload and review the bundle before retrying.", member.id
        )))?;
    }
    let role = crate::fleet::worker_runtime::roster_member_agent_type(member);
    if request.agent_type_explicit && request.agent_type != role {
        return Err(ToolError::invalid_input(format!(
            "profile '{}' implies type {}; conflicting explicit type '{}'",
            member.id,
            role.as_str(),
            request.agent_type.as_str(),
        )));
    }
    if let Some(pinned) = member
        .profile
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty() && !model.eq_ignore_ascii_case("auto"))
        && request.model_strength_explicit
    {
        return Err(ToolError::invalid_input(format!(
            "Fleet profile '{}' pins model '{pinned}'; model_strength cannot change an exact saved route.",
            member.id
        )));
    }
    // Compare any task selector only after the exact provider is bound, so
    // a qualified selector can prove it names this same provider/model pair.
    request.agent_type = role;
    request.profile = Some(member.id.clone());
    request.assignment.role = Some(member.profile.role.name.clone());
    request.assignment.native_preset = member.native_preset.clone();
    if !request.thinking_explicit
        && let Some(effort) =
            crate::fleet::worker_runtime::effective_fleet_reasoning_effort(Some(member))
    {
        request.thinking = SubAgentThinking::parse(&effort).map_err(|_| {
            ToolError::invalid_input(format!(
                "Fleet profile '{}' has invalid reasoning_effort '{effort}'",
                member.id
            ))
        })?;
    }
    crate::fleet::worker_runtime::append_agent_profile_prompt(&mut request.prompt, member);
    Ok(())
}

fn bind_profile_provider(
    runtime: &mut SubAgentRuntime,
    member: Option<&crate::fleet::profile::AgentProfile>,
) -> Result<MemberProviderBind, ToolError> {
    let Some(provider_id) = crate::fleet::worker_runtime::explicit_fleet_provider_id(member) else {
        return Ok(MemberProviderBind::Bound);
    };
    if provider_pin_matches_session(runtime, &provider_id) {
        return Ok(MemberProviderBind::Bound);
    }
    if member
        .and_then(|member| member.profile.model.as_deref())
        .is_none_or(|model| model.trim().is_empty() || model.eq_ignore_ascii_case("auto"))
    {
        return Err(ToolError::invalid_input(
            "A saved cross-provider route must pin an exact model as well as its provider.",
        ));
    }
    try_bind_spawn_provider(runtime, &provider_id)
}

/// Outcome of binding a saved profile's provider pin (#5529 mode 2).
enum MemberProviderBind {
    /// No pin, a session-identical pin, or bound successfully.
    Bound,
    /// The pin names a real provider whose client cannot be built
    /// (missing credentials, unusable route). The dispatch may fall back
    /// to the session route loudly instead of failing. Unknown provider
    /// ids stay a hard fail-closed error — running elsewhere on a typo'd
    /// pin would be guessing, not falling back.
    Unavailable { provider_id: String, reason: String },
}

fn bind_spawn_provider(runtime: &mut SubAgentRuntime, provider_id: &str) -> Result<(), ToolError> {
    match try_bind_spawn_provider(runtime, provider_id)? {
        MemberProviderBind::Bound => Ok(()),
        MemberProviderBind::Unavailable {
            provider_id,
            reason,
        } => Err(ToolError::execution_failed(format!(
            "Saved provider '{provider_id}' is unavailable: {reason}"
        ))),
    }
}

fn try_bind_spawn_provider(
    runtime: &mut SubAgentRuntime,
    provider_id: &str,
) -> Result<MemberProviderBind, ToolError> {
    if provider_pin_matches_session(runtime, provider_id) {
        return Ok(MemberProviderBind::Bound);
    }
    let config = runtime.api_config.as_deref().ok_or_else(|| ToolError::execution_failed(
        "An exact provider choice needs the session Config; the child cannot safely change providers without it."
    ))?;
    let identity = config
        .resolve_provider_pin_identity(provider_id)
        .map_err(ToolError::invalid_input)?;
    let mut scoped = config.clone();
    scoped
        .scope_to_provider_identity(&identity)
        .map_err(ToolError::invalid_input)?;
    match CodewhaleClient::new(&scoped) {
        Ok(client) => {
            // #5529 mode 2: a bound client with unresolvable credentials
            // dies on its first request; probe servability now so the
            // dispatch can fall back loudly instead. Read-only probe: real
            // requests still own secret migration.
            if let Err(error) = scoped.active_route_api_key_read_only() {
                return Ok(MemberProviderBind::Unavailable {
                    provider_id: provider_id.to_string(),
                    reason: format!(
                        "credentials unresolvable: {}",
                        runtime.client.redact_model_bound_text(&error.to_string())
                    ),
                });
            }
            runtime.client = client;
            runtime.api_config = Some(Arc::new(scoped));
            Ok(MemberProviderBind::Bound)
        }
        Err(error) => Ok(MemberProviderBind::Unavailable {
            provider_id: provider_id.to_string(),
            reason: runtime.client.redact_model_bound_text(&error.to_string()),
        }),
    }
}

fn provider_pin_matches_session(runtime: &SubAgentRuntime, provider_id: &str) -> bool {
    let provider_id = provider_id.trim();
    let captured = runtime.client.admitted_provider_identity();
    if let Some(config) = runtime.api_config.as_ref() {
        if config.verify_provider_identity(captured).is_err() {
            return false;
        }
        return config
            .resolve_provider_pin_identity(provider_id)
            .is_ok_and(|pinned| pinned == *captured);
    }
    // With no Config, only an ordinary captured built-in's exact or released
    // alias spelling is reusable. Named custom, root-only and legacy Cloud
    // provenance require its table receipt and cannot borrow transport identity.
    if captured.provider == crate::config::ProviderKind::Custom
        || captured.migrated_legacy_ollama_cloud_route
        || captured.persisted_id() != Some(captured.key.as_str())
    {
        return false;
    }
    codewhale_config::descriptors::compatibility_for_selector(provider_id)
        .is_some_and(|row| row.id == captured.key.as_str() && row.kind == captured.provider)
}

fn enforce_fleet_member_route_requirements(
    member: Option<&crate::fleet::profile::AgentProfile>,
    runtime: &SubAgentRuntime,
    model: &str,
) -> Result<(), ToolError> {
    let Some(member) = member else {
        return Ok(());
    };
    if member.requires.is_empty() {
        return Ok(());
    }
    let member_id = crate::fleet::identity::FleetMemberIdentity::from_member(member).member_id;

    let candidate = runtime.client.resolve_model_route(model)
    .map_err(|error| {
        ToolError::execution_failed(format!(
            "Fleet member '{member_id}' requirements could not be checked against its exact child route: {}",
            crate::safe_label::safe_error_text(&error.to_string())
        ))
    })?;
    let provider_id = runtime.client.admitted_provider_identity().key.to_string();
    let provider_id = crate::safe_label::SafeLabel::identifier(&provider_id);
    let model_id = crate::safe_label::SafeLabel::catalog_model(candidate.wire_model_id().as_str());

    for requirement in &member.requires {
        match crate::fleet::store::MemberCapability::parse(requirement) {
            Some(crate::fleet::store::MemberCapability::Vision) => {
                let state = candidate.capabilities().image_input;
                if !state.is_supported() {
                    let state = match state {
                        codewhale_config::route::CapabilityState::Unsupported => "unsupported",
                        codewhale_config::route::CapabilityState::Unknown => "unknown",
                        codewhale_config::route::CapabilityState::Supported => unreachable!(),
                    };
                    return Err(ToolError::execution_failed(format!(
                        "Fleet member '{member_id}' requires vision, but exact route {provider_id}/{model_id} has image_input={state}. Codewhale will not reroute a capability-bound member; pin an exact route with verified image_input support."
                    )));
                }
            }
            None => {
                let requirement = crate::fleet::identity::bounded_identity_field(requirement);
                return Err(ToolError::execution_failed(format!(
                    "Fleet member '{member_id}' has unknown capability requirement '{}'; valid values: {}",
                    requirement,
                    crate::fleet::store::MemberCapability::VOCABULARY.join(", ")
                )));
            }
        }
    }
    Ok(())
}
/// Resolve the built-in compatibility role when no saved member is selected.
/// Saved profile identity and route checks are owned by `resolve_spawn_profile`.
fn resolve_spawn_role(request: &mut SpawnRequest) -> Result<(), ToolError> {
    let Some(profile_id) = request.profile.clone() else {
        return Ok(());
    };
    let Some(role) = FleetRole::from_str(&profile_id) else {
        return Err(ToolError::invalid_input(format!(
            "Unknown Fleet role/profile '{profile_id}'. Fleet profiles are roles: {VALID_ROLE_ALIASES}."
        )));
    };
    if request.agent_type_explicit && request.agent_type != role {
        return Err(ToolError::invalid_input(format!(
            "profile '{}' implies type {}; conflicting explicit type '{}'",
            profile_id,
            role.as_str(),
            request.agent_type.as_str()
        )));
    }
    request.agent_type = role.clone();
    // Record the canonical role id after resolution.
    request.profile = Some(role.as_str().to_string());
    // Surface the role in prompts and ledger records.
    request.assignment.role = Some(role.as_str().to_string());
    Ok(())
}

/// The active parent's posture, expressed as the upper bound for a child's
/// requested role policy.
///
/// Read off the live parent runtime rather than assumed: this is the value
/// that makes "a spawn cannot widen what the operator is currently allowed
/// to do" true at runtime instead of on paper.
#[must_use]
pub(crate) fn session_permission_ceiling(
    runtime: &SubAgentRuntime,
) -> codewhale_workflow::PermissionCeiling {
    codewhale_workflow::PermissionCeiling {
        write: runtime.worker_profile.permissions.write,
        network_tool: runtime.worker_profile.permissions.network
            && runtime.agent_tool_surface_options.web_search_enabled,
        shell: crate::fleet::role::session_shell_ceiling(
            runtime.worker_profile.shell,
            runtime.allow_shell,
        ),
        delegation_depth: runtime
            .max_spawn_depth
            .min(runtime.worker_profile.max_spawn_depth)
            .saturating_sub(runtime.spawn_depth),
        // The parent side never withholds the coarse tool bit: fine-grained
        // inherited ToolScope/deny rules are intersected again by the spawn
        // runtime. This value only captures the dimensions represented here.
        tools: true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpawnRouteSource {
    AgentProfileModel,
    RolePin,
    TaskModel,
    TaskModelStrength,
    RoleDefault,
    RunModel,
    /// The profile's pinned provider was unusable, so the dispatch fell
    /// back to the session route loudly (receipt note names the pin and
    /// the reason) instead of failing (#5529 mode 2).
    SessionFallback,
    /// The role pin's first request was refused before any work and the
    /// child moved to an operator-approved replacement route.
    RoleReplacement,
}

impl SpawnRouteSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::AgentProfileModel => "agent_profile.model",
            Self::RolePin => "role.pin",
            Self::TaskModel => "task.model",
            Self::TaskModelStrength => "task.model_strength",
            Self::RoleDefault => "role.default",
            Self::RunModel => "run.model",
            Self::SessionFallback => "session.fallback",
            Self::RoleReplacement => "role.replacement",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        [
            Self::AgentProfileModel,
            Self::RolePin,
            Self::TaskModel,
            Self::TaskModelStrength,
            Self::RoleDefault,
            Self::RunModel,
            Self::SessionFallback,
            Self::RoleReplacement,
        ]
        .into_iter()
        .find(|source| source.as_str() == value)
    }
}

/// `provider/model` of the route a runtime would send its next request on,
/// named the way route receipts name it.
fn runtime_route_label(runtime: &SubAgentRuntime) -> String {
    format!("{}/{}", runtime_provider_id(runtime), runtime.model)
}

fn runtime_provider_id(runtime: &SubAgentRuntime) -> String {
    runtime.client.admitted_provider_identity().key.to_string()
}

/// The parent's route, captured before a spawn binds its own, so a saved
/// profile pin that refuses authorization can run where the conversation
/// already runs. No new provider ever receives the task this way.
#[derive(Clone)]
pub(crate) struct ParentRoute {
    client: CodewhaleClient,
    api_config: Option<Arc<crate::config::Config>>,
    model: String,
    reasoning_effort: Option<String>,
    reasoning_effort_auto: bool,
    label: String,
}

impl ParentRoute {
    fn capture(runtime: &SubAgentRuntime) -> Self {
        Self {
            client: runtime.client.clone(),
            api_config: runtime.api_config.clone(),
            model: runtime.model.clone(),
            reasoning_effort: runtime.reasoning_effort.clone(),
            reasoning_effort_auto: runtime.reasoning_effort_auto,
            label: runtime_route_label(runtime),
        }
    }

    /// Move only the request route; role, grants, tools and budgets stay.
    fn install(&self, runtime: &mut SubAgentRuntime) {
        runtime.client = self.client.clone();
        runtime.api_config.clone_from(&self.api_config);
        runtime.model.clone_from(&self.model);
        runtime.reasoning_effort.clone_from(&self.reasoning_effort);
        runtime.reasoning_effort_auto = self.reasoning_effort_auto;
    }
}

/// Where a child's route came from, in operator words, and how to change it.
/// Failure text and fallback notes read it, so an error names the real
/// source (a saved profile, a config pin, a task argument) instead of
/// guessing from the resolved model.
pub(crate) struct SpawnRouteOrigin {
    /// e.g. `saved agent profile "reviewer" pins xai/grok-4.6`.
    source: String,
    /// The exact way to change it.
    change: String,
    /// `provider/model` the spawn bound: the refused-route cache key.
    route: String,
    /// Present only when a refused first request may run on the parent route.
    parent: Option<ParentRoute>,
}

fn saved_profile_change_hint(profile_id: &str, profile_file: Option<&std::path::Path>) -> String {
    let editor = format!("change it in /fleet members (Enter on \"{profile_id}\")");
    match profile_file.filter(|path| !path.as_os_str().is_empty()) {
        Some(path) => format!("{editor} or edit `provider`/`model` in {}", path.display()),
        None => editor,
    }
}

/// The saved profile a route came from: its id and, when known, its file.
type RouteProfile<'a> = (&'a str, Option<&'a std::path::Path>);

fn spawn_route_origin(
    source: SpawnRouteSource,
    role: &str,
    profile: Option<RouteProfile<'_>>,
    route: String,
    fallback_note: Option<&str>,
) -> SpawnRouteOrigin {
    let (source, change) = match (source, profile) {
        (SpawnRouteSource::AgentProfileModel, Some((id, file))) => (
            format!("saved agent profile \"{id}\" pins {route}"),
            saved_profile_change_hint(id, file),
        ),
        (SpawnRouteSource::RolePin, _) => (
            format!("config.toml role pin for \"{role}\" routes {route}"),
            format!(
                "edit `model` under [subagents.roles.{role}] in config.toml, or declare approved `replacements` there"
            ),
        ),
        (SpawnRouteSource::TaskModel, _) => (
            format!("explicit child model override (the task's `model` argument) routes {route}"),
            "omit `model` or choose a route listed by agent action=roster".to_string(),
        ),
        (SpawnRouteSource::TaskModelStrength, _) => (
            format!("the task's `model_strength` routes {route}"),
            "omit `model_strength` to inherit the parent route".to_string(),
        ),
        (SpawnRouteSource::RoleDefault, _) => (
            format!("the configured default model for role \"{role}\" routes {route}"),
            format!("edit the \"{role}\" default under [subagents] in config.toml"),
        ),
        // The route the child moved to after its own was refused or
        // unusable: blame this route, and still point at the original.
        (SpawnRouteSource::SessionFallback | SpawnRouteSource::RoleReplacement, _) => (
            match fallback_note {
                Some(note) => format!("{route}, taken because {note}"),
                None => format!("fallback route {route}"),
            },
            format!(
                "{route} failed too; check that provider's sign-in, credits, or access, and fix the original route the note names"
            ),
        ),
        _ => (
            format!("inherited from the parent route {route}"),
            "the parent route itself was refused; check that provider's sign-in, credits, or access"
                .to_string(),
        ),
    };
    SpawnRouteOrigin {
        source,
        change,
        route,
        parent: None,
    }
}

/// A resumed child's route origin, rebuilt from its saved route receipt: the
/// resume does not re-run the spawn's route binding, but its failures must
/// still name the real source instead of guessing from the resolved model.
fn route_origin_from_receipt(receipt: &ChildRouteReceipt) -> Option<SpawnRouteOrigin> {
    let source = SpawnRouteSource::parse(&receipt.route_source)?;
    Some(spawn_route_origin(
        source,
        &receipt.canonical_role,
        receipt.resolved_profile_id.as_deref().map(|id| (id, None)),
        format!("{}/{}", receipt.provider_id, receipt.model_id),
        receipt.fallback_note.as_deref(),
    ))
}

/// The route a child's requests moved to before any work (a refused
/// saved-profile pin on the parent route, or an approved replacement), so a
/// failure there is reported against that route, not the one it left.
pub(crate) struct ReroutedRequest {
    model: String,
    provider: crate::config::ProviderKind,
    origin: Arc<SpawnRouteOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpawnModelSelection {
    model_route: ModelRoute,
    source: SpawnRouteSource,
}

/// Resolve the child model once, with receipt-grade precedence provenance:
/// Manual pins precede task choices; inherited defaults remain the last fallback.
/// Saved member pins are bound before this unpinned task/default fallback.
fn resolve_spawn_model_selection(
    runtime: &SubAgentRuntime,
    request: &SpawnRequest,
) -> Result<SpawnModelSelection, ToolError> {
    if let Some(pin) = configured_manual_spawn_model(runtime, request)? {
        return Ok(SpawnModelSelection {
            model_route: ModelRoute::Fixed(pin.model),
            source: SpawnRouteSource::RolePin,
        });
    }
    if let Some(model) = request.model.as_deref() {
        let model = normalize_bound_subagent_model(model, "model", &runtime.client)?;
        return Ok(SpawnModelSelection {
            model_route: ModelRoute::Fixed(model),
            source: SpawnRouteSource::TaskModel,
        });
    }
    if request.model_strength_explicit {
        return Ok(SpawnModelSelection {
            model_route: request.model_strength.model_route(),
            source: SpawnRouteSource::TaskModelStrength,
        });
    }
    if let Some(model) = configured_model_for_role_or_type(
        runtime,
        request.assignment.role.as_deref(),
        &request.agent_type,
    )? {
        return Ok(SpawnModelSelection {
            model_route: ModelRoute::Fixed(model),
            source: SpawnRouteSource::RoleDefault,
        });
    }
    if request.model_strength == SubAgentModelStrength::Faster {
        return Ok(SpawnModelSelection {
            model_route: ModelRoute::Faster,
            source: SpawnRouteSource::RoleDefault,
        });
    }
    Ok(SpawnModelSelection {
        model_route: ModelRoute::Inherit,
        source: SpawnRouteSource::RunModel,
    })
}

/// Resolve an explicit task choice against the existing selected Fleet's routes.
/// No selection preserves current-provider overrides; a broken selection is an
/// error, never an empty shortlist. Provider identity is matched before model
/// validation, so a deliberately saved cross-provider route cannot be guessed.
fn bind_shortlisted_task_model(
    runtime: &mut SubAgentRuntime,
    request: &SpawnRequest,
) -> Result<Option<String>, ToolError> {
    let Some(requested) = request.model.as_deref() else {
        return Ok(None);
    };
    let models = crate::fleet::members::fleet_models(&runtime.context.workspace)
        .map_err(|error| ToolError::execution_failed(error.to_string()))?;
    if models.is_empty() {
        return Ok(None);
    }
    let session_provider = runtime.client.admitted_provider_identity().key.to_string();
    // Exact wire choices precede alias convenience, including the session
    // fast path. A case-distinct shortlisted declaration must not select the
    // parent's differently cased model or become falsely ambiguous.
    let session_exact = requested == runtime.model
        || qualified_spawn_model(runtime, requested, &session_provider)
            == Some(runtime.model.as_str());
    if session_exact {
        return Ok(Some(runtime.model.clone()));
    }
    let find_candidates = |allow_aliases: bool| {
        let matches = |provider: &str, requested: &str, model: &str| {
            requested == model
                || (allow_aliases && spawn_model_ids_match(runtime, provider, requested, model))
        };
        let qualified = models
            .iter()
            .filter(|model| {
                qualified_spawn_model(runtime, requested, &model.provider)
                    .is_some_and(|requested| matches(&model.provider, requested, &model.model))
            })
            .collect::<Vec<_>>();
        let mut candidates = if qualified.is_empty() {
            models
                .iter()
                .filter(|model| matches(&model.provider, requested, &model.model))
                .collect::<Vec<_>>()
        } else {
            qualified
        };
        if allow_aliases {
            // Projection preserves case-distinct rows, but undeclared model
            // aliases of one exact provider still represent one choice.
            let mut seen: Vec<(&str, &str)> = Vec::new();
            candidates.retain(|model| {
                if seen.iter().any(|(provider, id)| {
                    id.eq_ignore_ascii_case(&model.model)
                        && spawn_provider_ids_match(runtime, provider, &model.provider)
                }) {
                    return false;
                }
                seen.push((&model.provider, &model.model));
                true
            });
        }
        candidates
    };
    let mut candidates = find_candidates(false);
    if candidates.is_empty() {
        // Ordinary aliases retain the historical current-route preference.
        if requested_spawn_model_matches_pin(
            runtime,
            requested,
            &runtime.model,
            Some(&session_provider),
        ) {
            return Ok(Some(runtime.model.clone()));
        }
        candidates = find_candidates(true);
    }
    let selected = match candidates.as_slice() {
        [model] => *model,
        [] => {
            let choices = models
                .iter()
                .take(32)
                .map(|model| {
                    crate::fleet::identity::bounded_identity_field(&format!(
                        "{}/{}",
                        model.provider, model.model
                    ))
                })
                .chain(std::iter::once(
                    crate::fleet::identity::bounded_identity_field(&format!(
                        "{session_provider}/{}",
                        runtime.model
                    )),
                ))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ToolError::invalid_input(format!(
                "Requested model is outside the selected Fleet. Available routes: {choices}. Use action=roster; saved profile pins remain authoritative."
            )));
        }
        _ => {
            return Err(ToolError::invalid_input(
                "That model is shortlisted on multiple providers. Use the exact provider/model selector from action=roster.",
            ));
        }
    };
    bind_spawn_provider(runtime, &selected.provider)?;
    Ok(Some(selected.model.clone()))
}

/// Bind discovery and execution through the same provider/model/effort path.
/// This performs no inference and reserves no child or workspace resources.
async fn bind_spawn_model_route(
    runtime: &mut SubAgentRuntime,
    request: &SpawnRequest,
    member: Option<&crate::fleet::profile::AgentProfile>,
    apply_role_pins: bool,
    allow_provider_fallback: bool,
) -> Result<(ModelRoute, SpawnRouteSource, Option<String>), ToolError> {
    // #5529 mode 2: a profile pin to an unusable provider falls back to the
    // session route loudly instead of failing the dispatch. Exact-bound
    // spawns refuse the substitution (their route is preflighted), and
    // task-level pins stay exact — only saved-profile rot falls back.
    let mut member = member;
    let mut fallback_note = None;
    // Replacement authority belongs to one role pin, never to a descendant
    // that inherited this runtime or chose its route another way.
    runtime.route_replacements.clear();
    runtime.route_origin = None;
    let parent_route = ParentRoute::capture(runtime);
    match bind_profile_provider(runtime, member)? {
        MemberProviderBind::Bound => {}
        MemberProviderBind::Unavailable {
            provider_id,
            reason,
        } if allow_provider_fallback => {
            let reason: String = reason.chars().take(160).collect();
            fallback_note = Some(format!(
                "pinned provider '{provider_id}' unavailable ({reason}); fell back to the session route"
            ));
            member = None;
        }
        MemberProviderBind::Unavailable {
            provider_id,
            reason,
        } => {
            return Err(ToolError::execution_failed(format!(
                "Saved provider '{provider_id}' is unavailable: {reason}"
            )));
        }
    }
    let mut shortlisted = false;
    let mut manual_pin = None;
    let mut selection = if let Some(member) = member
        && let Some(model) = member
            .profile
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty() && !model.eq_ignore_ascii_case("auto"))
    {
        SpawnModelSelection {
            model_route: ModelRoute::Fixed(normalize_bound_subagent_model(
                model,
                "profile.model",
                &runtime.client,
            )?),
            source: SpawnRouteSource::AgentProfileModel,
        }
    } else if apply_role_pins && let Some(pin) = configured_manual_spawn_model(runtime, request)? {
        if let Some(provider) = pin.provider.as_deref() {
            bind_spawn_provider(runtime, provider)?;
        }
        let selection = SpawnModelSelection {
            model_route: ModelRoute::Fixed(normalize_bound_subagent_model(
                &pin.model,
                "role.model",
                &runtime.client,
            )?),
            source: SpawnRouteSource::RolePin,
        };
        runtime.route_replacements = configured_route_replacements(runtime, request)?;
        manual_pin = Some(pin);
        selection
    } else if let Some(model) = bind_shortlisted_task_model(runtime, request)? {
        shortlisted = true;
        SpawnModelSelection {
            model_route: ModelRoute::Fixed(normalize_bound_subagent_model(
                &model,
                "model",
                &runtime.client,
            )?),
            source: SpawnRouteSource::TaskModel,
        }
    } else if !apply_role_pins && let Some(model) = request.model.as_deref() {
        // Model rows disclose the shortlist's exact route, independently of a
        // role. Actual starts and role/profile rows always apply role pins.
        SpawnModelSelection {
            model_route: ModelRoute::Fixed(normalize_bound_subagent_model(
                model,
                "model",
                &runtime.client,
            )?),
            source: SpawnRouteSource::TaskModel,
        }
    } else {
        resolve_spawn_model_selection(runtime, request)?
    };
    let providerless = crate::fleet::worker_runtime::explicit_fleet_provider_id(member).is_none()
        && !shortlisted
        && manual_pin.as_ref().is_none_or(|pin| pin.provider.is_none());
    let unresolved_pin = selection.model_route.clone();
    resolve_fixed_spawn_model_route(runtime, &mut selection, providerless)?;
    if matches!(
        selection.source,
        SpawnRouteSource::AgentProfileModel | SpawnRouteSource::RolePin
    ) {
        validate_spawn_pin_request(
            runtime,
            request,
            member,
            manual_pin.as_ref(),
            &unresolved_pin,
            &selection,
        )?;
    }
    let route = resolve_subagent_assignment_route(
        runtime,
        None,
        &request.agent_type,
        selection.model_route,
        request.thinking,
    )
    .await;
    // A fast lane the router could not serve is a fallback too: the note rides
    // the receipt field the pinned-provider fallback already uses (#5529 mode
    // 2) without relabelling where the model came from.
    if fallback_note.is_none() {
        fallback_note = route.route_note.clone();
    }
    let model = ensure_subagent_model_for_provider(runtime, &route.model_route, route.model)?;
    if let Some(rebound) = runtime
        .client
        .rebound_for_model_protocol(runtime.api_config.as_deref(), &model)
        .map_err(|err| {
            ToolError::execution_failed(format!(
                "Pod dispatch could not bind the wire protocol for model {model:?}: {err:#}"
            ))
        })?
    {
        runtime.client = rebound;
    }
    enforce_fleet_member_route_requirements(member, runtime, &model)?;
    runtime.model = model;
    runtime.reasoning_effort = route.reasoning_effort;
    runtime.reasoning_effort_auto = false;
    let source = if fallback_note.is_some() {
        SpawnRouteSource::SessionFallback
    } else {
        selection.source
    };
    let bound = runtime_route_label(runtime);
    let role = request
        .assignment
        .role
        .as_deref()
        .unwrap_or_else(|| request.agent_type.as_str());
    let mut origin = spawn_route_origin(
        source,
        role,
        member.map(|member| (member.id.as_str(), Some(member.source.as_path()))),
        bound.clone(),
        fallback_note.as_deref(),
    );
    if source == SpawnRouteSource::AgentProfileModel && bound != parent_route.label {
        // A saved-profile pin that refuses authorization before any work may
        // run on the parent route instead (the #5529 mode 2 rule, extended
        // to the first request). Exact-bound spawns and capability-bound
        // members stay exact, and the failure then says so.
        let known_refusal = runtime.manager.read().await.refused_route(&bound);
        if !allow_provider_fallback {
            origin
                .change
                .push_str("; this exact Fleet binding forbids falling back to the parent route");
        } else if member.is_some_and(|member| !member.requires.is_empty()) {
            origin
                .change
                .push_str("; a capability-bound member is never moved to another route");
        } else if let Some(reason) = known_refusal {
            // This session already saw the pin refuse: go straight to the
            // parent route instead of failing first again.
            let note: String = format!(
                "{}, which failed authorization earlier this session ({reason}); ran on {} instead",
                origin.source, parent_route.label
            )
            .chars()
            .take(480)
            .collect();
            crate::logging::warn(format!("sub-agent route fallback: {note}"));
            parent_route.install(runtime);
            runtime.route_origin = Some(Arc::new(spawn_route_origin(
                SpawnRouteSource::SessionFallback,
                role,
                None,
                parent_route.label.clone(),
                Some(&note),
            )));
            return Ok((
                ModelRoute::Inherit,
                SpawnRouteSource::SessionFallback,
                Some(note),
            ));
        } else {
            origin.parent = Some(parent_route);
        }
    }
    runtime.route_origin = Some(Arc::new(origin));
    Ok((route.model_route, source, fallback_note))
}

fn validate_spawn_pin_request(
    runtime: &SubAgentRuntime,
    request: &SpawnRequest,
    member: Option<&crate::fleet::profile::AgentProfile>,
    manual_pin: Option<&SubagentModelOverride>,
    unresolved_pin: &ModelRoute,
    selection: &SpawnModelSelection,
) -> Result<(), ToolError> {
    let ModelRoute::Fixed(pinned) = &selection.model_route else {
        return Ok(());
    };
    if request.model_strength_explicit {
        return Err(ToolError::invalid_input(format!(
            "{} pins model '{pinned}'; model_strength cannot change an exact saved route.",
            selection.source.as_str(),
        )));
    }
    let Some(requested) = request.model.as_deref() else {
        return Ok(());
    };
    let provider = runtime.client.admitted_provider_identity().key.to_string();
    let matches = |model: &str| {
        requested_spawn_model_matches_pin(runtime, requested, model, Some(&provider))
            || member.is_some_and(|member| {
                requested_spawn_model_matches_pin(
                    runtime,
                    requested,
                    model,
                    member.profile.provider.as_deref(),
                )
            })
            || manual_pin.is_some_and(|pin| {
                requested_spawn_model_matches_pin(
                    runtime,
                    requested,
                    model,
                    pin.provider.as_deref(),
                )
            })
    };
    if matches(pinned)
        || matches!(unresolved_pin, ModelRoute::Fixed(model) if matches(model))
        || member
            .and_then(|member| member.profile.model.as_deref())
            .is_some_and(matches)
        || manual_pin.is_some_and(|pin| matches(&pin.model))
    {
        return Ok(());
    }
    Err(ToolError::invalid_input(format!(
        "{} pins route '{provider}/{pinned}'; the requested model conflicts with that route. Omit the task model or change the saved pin.",
        selection.source.as_str(),
    )))
}

async fn resolved_role_roster_entry(
    runtime: &SubAgentRuntime,
    roster: &crate::fleet::roster::FleetRoster,
    role: &FleetRole,
) -> Value {
    resolved_spawn_roster_entry(
        runtime,
        roster,
        json!({
            "member_id": role.as_str(), "role": role.as_str(), "description": role.description(),
            "selector": {"type": role.as_str()},
        }),
        json!({"prompt": "Preview role defaults.", "type": role.as_str()}),
        true,
    )
    .await
}

async fn resolved_profile_roster_entry(
    runtime: &SubAgentRuntime,
    roster: &crate::fleet::roster::FleetRoster,
    member: &crate::fleet::profile::AgentProfile,
) -> Value {
    let mut entry = serde_json::to_value(crate::fleet::identity::FleetMemberIdentity::from_member(
        member,
    ))
    .expect("member identity serializes");
    entry["selector"] = json!({"profile": member.id});
    resolved_spawn_roster_entry(
        runtime,
        roster,
        entry,
        json!({"prompt": "Preview saved profile.", "profile": member.id}),
        true,
    )
    .await
}

async fn resolved_spawn_roster_entry(
    runtime: &SubAgentRuntime,
    roster: &crate::fleet::roster::FleetRoster,
    mut entry: Value,
    input: Value,
    apply_role_pins: bool,
) -> Value {
    // Exactly the spawn's parser/profile/provider/model binding, without admission or inference.
    let request = parse_spawn_request(&input).and_then(|mut request| {
        let member = if apply_role_pins {
            resolve_spawn_route_profile(runtime, &mut request, roster)?
        } else {
            None
        };
        Ok((request, member))
    });
    let mut child = runtime.child_runtime();
    let resolved = match request {
        Ok((request, member)) => {
            bind_spawn_model_route(&mut child, &request, member.as_ref(), apply_role_pins, true)
                .await
        }
        Err(error) => Err(error),
    };
    match resolved {
        Ok((_, source, fallback_note)) => {
            let envelope = child
                .client
                .effective_route_envelope(&child.model, chrono::Utc::now())
                .sanitized_for_persistence();
            let limits = child.client.route_limits();
            // Inspect nonzero text classes through the existing route audit;
            // this does not record usage or claim a future task's total cost.
            let audit = envelope.audit(&codewhale_models::Usage {
                input_tokens: 1,
                output_tokens: 1,
                ..Default::default()
            });
            let cost_class = match audit.estimate {
                Some(estimate) if estimate.is_positive() => "paid",
                Some(estimate) if estimate.is_finite_nonnegative() => "free",
                _ if audit.unpriced_reason
                    == Some(crate::pricing::UnpricedReason::NotMoneyMetered) =>
                {
                    "not_money_metered"
                }
                _ => "unknown",
            };
            let capabilities = crate::fleet::capability_badges::resolve_route_capability_badges(
                Some(&envelope.provider_identity),
                &child.model,
            );
            entry["route"] = json!({
                "provider": envelope.provider_identity,
                "model": envelope.model,
                "openrouter_vendor": envelope.openrouter_vendor,
                "source": source.as_str(),
                "reasoning_effort": child.reasoning_effort,
                "context_window": limits.and_then(|limits| limits.context_tokens),
                "max_output": limits.and_then(|limits| limits.output_tokens),
                "capability_badges": capabilities.as_ref().map(|facts| &facts.badges),
                "capability_source": capabilities.as_ref().map(|facts| facts.provenance),
                "cost_class": cost_class,
                "cost_basis": "current uncached text input/output rates; other token classes, tools and future usage may differ",
                "unpriced_reason": audit.unpriced_reason.map(|reason| reason.label()),
                "reachability": "unverified",
            });
            if let Some(note) = fallback_note {
                // A pin this session saw refuse authorization previews the
                // parent route it will actually run on, and says why.
                entry["route"]["fallback_note"] = json!(note);
            }
        }
        Err(error) => {
            entry["route"] = Value::Null;
            entry["route_error"] =
                json!(runtime.client.redact_model_bound_text(&error.to_string()));
        }
    }
    entry
}

/// Resolve caller/config model pins to the child provider's exact wire id
/// before a child reserves worktree or concurrency resources. Provider-less
/// pins also receive the conservative known-foreign check; explicit provider
/// pairs keep their deliberate route intent. Inherited/strength routes stay
/// unchanged.
///
/// #5099: the known-foreign check distinguishes who asked for the model. An
/// explicit `task.model` is the caller's deliberate pin and still fails with
/// the pin-vs-inherit error. A provider-less DEFAULT the session never chose
/// (a role/type default) must not hard-fail the spawn —
/// the child inherits the session route instead of colliding with a foreign
/// provider's bare model id, and the downgrade is logged.
fn resolve_fixed_spawn_model_route(
    runtime: &SubAgentRuntime,
    selection: &mut SpawnModelSelection,
    providerless: bool,
) -> Result<(), ToolError> {
    if !matches!(
        selection.source,
        SpawnRouteSource::TaskModel
            | SpawnRouteSource::RolePin
            | SpawnRouteSource::RoleDefault
            | SpawnRouteSource::AgentProfileModel
    ) {
        return Ok(());
    }
    let ModelRoute::Fixed(model) = &selection.model_route else {
        return Ok(());
    };
    let provider = runtime.client.api_provider();
    let candidate = runtime.client.resolve_model_route(model);
    let declared = candidate.as_ref().is_ok_and(|candidate| {
        candidate
            .applied_limit_overrides()
            .iter()
            .any(|entry| entry.source == codewhale_config::route::OverrideSource::UserModelMetadata)
    });
    if providerless
        && !declared
        && let Err(reason) = crate::route_runtime::validate_unpinned_model_provider(
            provider,
            model,
            runtime.client.base_url(),
        )
    {
        if matches!(
            selection.source,
            SpawnRouteSource::TaskModel
                | SpawnRouteSource::RolePin
                | SpawnRouteSource::AgentProfileModel
        ) {
            return Err(ToolError::invalid_input(reason));
        }
        tracing::warn!(
            model = %model,
            source = selection.source.as_str(),
            session_provider = %provider.as_str(),
            "provider-less spawn default is foreign to the session route; \
             the child inherits the session route instead ({reason})"
        );
        selection.model_route = ModelRoute::Inherit;
        selection.source = SpawnRouteSource::RunModel;
        return Ok(());
    }
    let candidate = candidate.map_err(|error| ToolError::invalid_input(error.to_string()))?;
    selection.model_route = ModelRoute::Fixed(candidate.wire_model_id().as_str().to_string());
    Ok(())
}

/// Effective absolute `max_spawn_depth` for a child, combining the inherited
/// runtime budget and the caller's `max_depth` request. The inherited budget
/// is an immutable absolute boundary: an explicit request may never widen a
/// child past the depth the root/session selected. A request only narrows —
/// the effective depth is the minimum of the inherited budget and the clamped
/// request (#5253).
fn child_max_spawn_depth_for_spawn(
    inherited: u32,
    child_spawn_depth: u32,
    requested: Option<u32>,
) -> u32 {
    match requested {
        Some(requested) => inherited.min(clamp_child_max_spawn_depth(child_spawn_depth, requested)),
        None => inherited,
    }
}

/// Optional aliased boolean, refusing a wrong type instead of dropping it.
///
/// `{"deliberate": "true"}` used to coerce to the default `false` and skip
/// the whole deliberate-delegation contract in silence.
fn parse_optional_bool(input: &Value, names: &[&str]) -> Result<Option<bool>, ToolError> {
    let Some((name, value)) = aliased_value(input, names) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| codewhale_tools::type_mismatch(name, value, "a boolean"))
}

fn merge_spawn_disallowed_tools(inherited: &mut Vec<String>, request: &SpawnRequest) {
    let _ = request.inherit_disallowed_tools; // compatibility input; never grants authority
    if let Some(caller) = request.disallowed_tools.as_ref() {
        for rule in caller {
            if !inherited.contains(rule) {
                inherited.push(rule.clone());
            }
        }
    }
}

/// Parse an optional caller-supplied `disallowed_tools` array (#4042). Mirrors
/// the `allowed_tools` parsing: trimmed, de-duplicated, non-empty-only. Returns
/// `None` when the key is absent or yields no usable entries so the union merge
/// in `spawn_subagent_from_input` only runs when there is something to add.
fn parse_disallowed_tools(input: &Value) -> Result<Option<Vec<String>>, ToolError> {
    Ok(parse_tool_name_list(input, "disallowed_tools")?.filter(|tools| !tools.is_empty()))
}

fn parse_optional_bounded_limit(
    input: &Value,
    names: &[&str],
    maximum: u64,
) -> Result<Option<u64>, ToolError> {
    let mut limit = None;
    for name in names {
        if let Some(value) = input.get(*name) {
            // Whole-number floats (`900.0`) are integers too: some providers
            // serialize every JSON number as a float.
            let parsed = codewhale_tools::json_nonnegative_integer(value)
                .filter(|value| *value > 0 && *value <= maximum)
                .ok_or_else(|| {
                    ToolError::invalid_input(if maximum == u64::MAX {
                        format!("{name} must be an integer greater than zero; omit it to inherit")
                    } else {
                        format!("{name} must be between 1 and {maximum}; omit it to inherit")
                    })
                })?;
            limit = narrow_optional_limit(limit, Some(parsed));
        }
    }
    Ok(limit)
}

#[cfg(test)]
fn with_default_fork_context(mut input: Value, default: bool) -> Value {
    let Some(object) = input.as_object_mut() else {
        return input;
    };
    if !object.contains_key("fork_context")
        && !object.contains_key("forkContext")
        && !object.contains_key("inherit_context")
    {
        object.insert("fork_context".to_string(), Value::Bool(default));
    }
    input
}

// Strip only the known provider qualifier; a slash inside a wire ID is not
// evidence that it names another provider.
fn qualified_spawn_model<'a>(
    runtime: &SubAgentRuntime,
    requested: &'a str,
    provider: &str,
) -> Option<&'a str> {
    let (prefix, model) = requested.split_once('/')?;
    spawn_provider_ids_match(runtime, prefix, provider).then_some(model)
}

fn spawn_provider_ids_match(runtime: &SubAgentRuntime, requested: &str, provider: &str) -> bool {
    if let Some(config) = runtime.api_config.as_deref() {
        let (Ok(requested), Ok(pinned)) = (
            config.resolve_provider_pin_identity(requested),
            config.resolve_provider_pin_identity(provider),
        ) else {
            return false;
        };
        requested.provider == pinned.provider
            && requested.key == pinned.key
            && requested.migrated_legacy_ollama_cloud_route
                == pinned.migrated_legacy_ollama_cloud_route
    } else {
        // With no Config, only exact names and known built-in aliases are
        // comparable. A named custom provider's spelling is its identity.
        requested.trim() == provider.trim()
            || crate::config::ProviderKind::parse(requested)
                .filter(|provider| *provider != crate::config::ProviderKind::Custom)
                .is_some_and(|requested| {
                    Some(requested) == crate::config::ProviderKind::parse(provider)
                })
    }
}

fn declared_spawn_model_for_provider(
    runtime: &SubAgentRuntime,
    provider: &str,
    model: &str,
) -> bool {
    if provider_pin_matches_session(runtime, provider) {
        return is_declared_subagent_model(&runtime.client, model);
    }
    let Some(config) = runtime.api_config.as_deref() else {
        return false;
    };
    let Ok(identity) = config.resolve_provider_pin_identity(provider) else {
        return false;
    };
    // Other providers bind from this config snapshot, while the active
    // provider above keeps its already-frozen endpoint and declarations.
    crate::route_runtime::resolve_runtime_route_for_identity(config, &identity, Some(model))
        .is_ok_and(|route| {
            route.model == model
                && route
                    .candidate
                    .applied_limit_overrides()
                    .iter()
                    .any(|entry| {
                        entry.source == codewhale_config::route::OverrideSource::UserModelMetadata
                    })
        })
}

fn spawn_model_ids_match(
    runtime: &SubAgentRuntime,
    provider: &str,
    requested: &str,
    model: &str,
) -> bool {
    requested == model
        || (requested.eq_ignore_ascii_case(model)
            && !declared_spawn_model_for_provider(runtime, provider, requested)
            && !declared_spawn_model_for_provider(runtime, provider, model))
}

fn requested_spawn_model_matches_pin(
    runtime: &SubAgentRuntime,
    requested: &str,
    model: &str,
    provider: Option<&str>,
) -> bool {
    let requested = requested.trim();
    let model = model.trim();
    let session_provider = runtime.client.admitted_provider_identity().key.to_string();
    let provider = provider
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .unwrap_or(&session_provider);
    spawn_model_ids_match(runtime, provider, requested, model)
        || qualified_spawn_model(runtime, requested, provider)
            .is_some_and(|requested| spawn_model_ids_match(runtime, provider, requested, model))
}

// Reuse the bound client's immutable route snapshot before legacy name
// normalization can turn an exact declaration into a different wire ID.
fn is_declared_subagent_model(client: &CodewhaleClient, model: &str) -> bool {
    client.resolve_model_route(model).is_ok_and(|candidate| {
        candidate.wire_model_id().as_str() == model
            && candidate.applied_limit_overrides().iter().any(|entry| {
                entry.source == codewhale_config::route::OverrideSource::UserModelMetadata
            })
    })
}

fn normalize_bound_subagent_model(
    value: &str,
    field: &str,
    client: &CodewhaleClient,
) -> Result<String, ToolError> {
    let model = value.trim();
    if is_declared_subagent_model(client, model) {
        return Ok(model.to_string());
    }
    normalize_requested_subagent_model(value, field, client.api_provider())
}

pub(crate) fn normalize_requested_subagent_model(
    value: &str,
    field: &str,
    provider: crate::config::ProviderKind,
) -> Result<String, ToolError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ToolError::invalid_input(format!("{field} cannot be blank")));
    }
    // #3018: Use provider-aware validation so non-DeepSeek providers can
    // accept their own model IDs instead of failing with "Expected a
    // DeepSeek model id".
    let normalized =
        crate::config::requested_model_for_provider(provider, trimmed).ok_or_else(|| {
            let valid_names = crate::provider_lake::all_catalog_models_for_provider(provider);
            let valid_hint = if valid_names.is_empty() {
                String::new()
            } else {
                format!(" (accepted: {})", valid_names.join(", "))
            };
            ToolError::invalid_input(format!(
                "Invalid {field} '{trimmed}' for provider {}{valid_hint}",
                provider_name_for_error(provider)
            ))
        })?;
    crate::config::validate_route(provider, &normalized).map_err(ToolError::invalid_input)?;
    Ok(normalized)
}

fn provider_name_for_error(provider: crate::config::ProviderKind) -> &'static str {
    // Reuse the canonical picker/status label so every provider is named
    // concretely (DeepSeek, Sakana, Zhipu, …) instead of collapsing the long
    // tail to "this provider", and so error copy stays in sync with the model
    // picker labels (#4049).
    provider.provider().display_name()
}

pub(crate) fn configured_model_for_role_or_type(
    runtime: &SubAgentRuntime,
    role: Option<&str>,
    agent_type: &FleetRole,
) -> Result<Option<String>, ToolError> {
    let Some((key, pin)) = configured_role_model_override(&runtime.role_models, role, agent_type)
    else {
        return Ok(None);
    };
    if let Some(provider) = pin.provider.as_deref()
        && !provider_pin_matches_session(runtime, provider)
    {
        return Err(ToolError::invalid_input(format!(
            "subagents.{key}.model has an explicit provider that is not bound to this child; an exact role pin requires the current session Config."
        )));
    }
    normalize_bound_subagent_model(
        &pin.model,
        &format!("subagents.{key}.model"),
        &runtime.client,
    )
    .map(Some)
}

fn configured_manual_spawn_model(
    runtime: &SubAgentRuntime,
    request: &SpawnRequest,
) -> Result<Option<SubagentModelOverride>, ToolError> {
    let Some(config) = runtime.api_config.as_deref() else {
        return Ok(None);
    };
    let overrides = config.subagent_model_overrides();
    let Some((key, pin)) = configured_role_model_override(
        &overrides,
        request.assignment.role.as_deref(),
        &request.agent_type,
    ) else {
        return Ok(None);
    };
    if pin.model.trim().is_empty() || pin.model.chars().any(char::is_control) {
        return Err(ToolError::invalid_input(format!(
            "subagents.{key}.model must name one nonblank model without control characters"
        )));
    }
    if let Some(provider) = pin.provider.as_deref() {
        if provider.trim().is_empty() || pin.model.trim().eq_ignore_ascii_case("auto") {
            return Err(ToolError::invalid_input(format!(
                "subagents.{key}.model must pair a nonblank provider with an exact model"
            )));
        }
        config
            .resolve_provider_pin_identity(provider)
            .map_err(ToolError::invalid_input)?;
    } else if pin.model.trim().eq_ignore_ascii_case("auto") {
        return Ok(None);
    }
    Ok(Some(pin.clone()))
}

/// Upper bound on declared replacement routes: each is tried at most once, so
/// this also bounds the extra first requests one refused pin can cost.
const MAX_ROUTE_REPLACEMENTS: usize = 3;

/// The replacement routes declared beside the role pin this spawn resolved.
/// Misconfiguration fails loud at spawn, not at the failure it would cover.
fn configured_route_replacements(
    runtime: &SubAgentRuntime,
    request: &SpawnRequest,
) -> Result<Vec<SubagentModelOverride>, ToolError> {
    let Some(config) = runtime.api_config.as_deref() else {
        return Ok(Vec::new());
    };
    let overrides = config.subagent_model_overrides();
    let Some((key, _)) = configured_role_model_override(
        &overrides,
        request.assignment.role.as_deref(),
        &request.agent_type,
    ) else {
        return Ok(Vec::new());
    };
    let replacements = config.subagent_route_replacements(&key);
    if replacements.len() > MAX_ROUTE_REPLACEMENTS {
        return Err(ToolError::invalid_input(format!(
            "subagents.roles.{key}.replacements lists {} routes; at most {MAX_ROUTE_REPLACEMENTS} are allowed",
            replacements.len()
        )));
    }
    for route in &replacements {
        let Some(provider) = route.provider.as_deref().filter(|p| !p.trim().is_empty()) else {
            return Err(ToolError::invalid_input(format!(
                "subagents.roles.{key}.replacements entries must name `provider/model`, so the provider that may receive the task is explicit; got {:?}",
                route.model
            )));
        };
        if route.model.trim().is_empty()
            || route.model.trim().eq_ignore_ascii_case("auto")
            || route.model.chars().any(char::is_control)
        {
            return Err(ToolError::invalid_input(format!(
                "subagents.roles.{key}.replacements entry for {provider:?} must name one exact model"
            )));
        }
        config
            .resolve_provider_pin_identity(provider)
            .map_err(ToolError::invalid_input)?;
    }
    Ok(replacements)
}

/// Why a first-request refusal may move a child to an approved replacement
/// route, or `None` when it must not. Typed only: an arbitrary message that
/// mentions credits is not quota evidence. Content-policy, context-length and
/// invalid-request refusals would recur on any route (or would shop content
/// to another provider), and Codewhale's own permission denials never reach
/// this seam as provider errors.
fn route_replacement_reason(error: &anyhow::Error) -> Option<&'static str> {
    match error.downcast_ref::<LlmError>()? {
        LlmError::QuotaExhausted(_) => Some("quota exhausted"),
        LlmError::AuthenticationError(_) => Some("credentials rejected"),
        LlmError::AuthorizationError(_) => Some("provider refused authorization"),
        LlmError::ModelError(_) => Some("model unavailable"),
        _ => None,
    }
}

/// Why a saved-profile pin's first-request refusal may move the child to the
/// parent route, or `None`. Only typed authorization and credit refusals
/// qualify: transient failures keep the normal retry path, and content or
/// model refusals are not a reason to change routes.
fn pin_refusal_reason(error: &anyhow::Error) -> Option<&'static str> {
    match error.downcast_ref::<LlmError>()? {
        LlmError::QuotaExhausted(_)
        | LlmError::AuthenticationError(_)
        | LlmError::AuthorizationError(_) => route_replacement_reason(error),
        _ => None,
    }
}

/// Bind `base` to one approved replacement route. Only the request route
/// changes: role, grants, tool scope, budgets and workspace stay `base`'s.
fn replacement_route_runtime(
    base: &SubAgentRuntime,
    route: &SubagentModelOverride,
) -> Result<SubAgentRuntime, String> {
    let mut next = base.clone();
    let provider = route.provider.as_deref().unwrap_or_default();
    match try_bind_spawn_provider(&mut next, provider).map_err(|error| error.to_string())? {
        MemberProviderBind::Bound => {}
        MemberProviderBind::Unavailable {
            provider_id,
            reason,
        } => return Err(format!("{provider_id} unavailable: {reason}")),
    }
    let model = normalize_bound_subagent_model(&route.model, "replacement", &next.client)
        .map_err(|error| error.to_string())?;
    let model = ensure_subagent_model_for_provider(&next, &ModelRoute::Fixed(model.clone()), model)
        .map_err(|error| error.to_string())?;
    if let Some(rebound) = next
        .client
        .rebound_for_model_protocol(next.api_config.as_deref(), &model)
        .map_err(|error| format!("{error:#}"))?
    {
        next.client = rebound;
    }
    next.model = model;
    Ok(next)
}

/// One alias/default precedence for manual Config pins and legacy role defaults.
pub(crate) fn configured_role_model_override<'a>(
    overrides: &'a HashMap<String, SubagentModelOverride>,
    role: Option<&str>,
    agent_type: &FleetRole,
) -> Option<(String, &'a SubagentModelOverride)> {
    let mut keys = Vec::new();
    let mut push_key = |key: String| {
        if !keys.contains(&key) {
            keys.push(key);
        }
    };
    if let Some(role) = role.map(str::trim).filter(|role| !role.is_empty()) {
        let normalized = role.to_ascii_lowercase();
        push_key(
            migrate_legacy_role_token(&normalized)
                .unwrap_or(normalized.as_str())
                .to_string(),
        );
        push_key(normalized);
    }
    push_key(agent_type.as_str().to_string());
    if agent_type.legacy_type_name() != agent_type.as_str() {
        push_key(agent_type.legacy_type_name().to_string());
    }
    // `[subagents.models].oracle` shipped before the public role was renamed.
    // Keep both historical advisory keys readable, but never expose them in
    // schemas, prompts, receipts, or newly serialized role values.
    if *agent_type == FleetRole::Consultant {
        push_key("oracle".to_string());
        push_key("advisor".to_string());
    }
    push_key("default".to_string());

    for key in keys {
        if let Some(pin) = overrides.get(&key) {
            return Some((key, pin));
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SubAgentResolvedRoute {
    pub(crate) model_route: ModelRoute,
    pub(crate) model: String,
    pub(crate) reasoning_effort: Option<String>,
    pub(crate) tuning: RequestTuning,
    /// Set when the requested route could not be served as asked and the
    /// dispatch fell back to the parent model — today, a `Faster`/`Auto` lane
    /// the router has no cheap sibling for. The spawn receipt carries it to
    /// the operator, so the fallback is visible instead of silent.
    pub(crate) route_note: Option<String>,
}

impl SubAgentResolvedRoute {
    fn new(
        model_route: ModelRoute,
        model: String,
        reasoning_effort: Option<String>,
    ) -> SubAgentResolvedRoute {
        let tuning = subagent_request_tuning(reasoning_effort.as_deref());
        SubAgentResolvedRoute {
            model_route,
            model,
            reasoning_effort,
            tuning,
            route_note: None,
        }
    }
}

pub(crate) async fn resolve_subagent_assignment_route(
    runtime: &SubAgentRuntime,
    configured_model: Option<String>,
    agent_type: &FleetRole,
    requested_model_route: ModelRoute,
    requested_thinking: SubAgentThinking,
) -> SubAgentResolvedRoute {
    let model_route = assignment_model_route(configured_model.as_deref(), requested_model_route);
    worker_profile_subagent_assignment_route(runtime, &model_route, requested_thinking, agent_type)
}

fn assignment_model_route(
    configured_model: Option<&str>,
    requested_model_route: ModelRoute,
) -> ModelRoute {
    if let Some(model) = configured_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        return ModelRoute::Fixed(model.to_string());
    }

    requested_model_route
}

fn subagent_request_tuning(reasoning_effort: Option<&str>) -> RequestTuning {
    RequestTuning {
        reasoning_effort: reasoning_effort.map(ReasoningEffort::from_setting),
        max_output_tokens: None,
    }
}

/// Candidate pair for explicit sub-agent strength routing, derived from the
/// active provider and the already provider-resolved parent model.
fn subagent_router_candidates(runtime: &SubAgentRuntime) -> crate::model_routing::RouterCandidates {
    crate::model_routing::provider_router_candidates(runtime.client.api_provider(), &runtime.model)
}

#[cfg(test)]
fn fallback_subagent_assignment_route(
    runtime: &SubAgentRuntime,
    configured_model: Option<String>,
    requested_model_route: ModelRoute,
    requested_thinking: SubAgentThinking,
) -> SubAgentResolvedRoute {
    let model_route = assignment_model_route(configured_model.as_deref(), requested_model_route);
    worker_profile_subagent_assignment_route(
        runtime,
        &model_route,
        requested_thinking,
        &FleetRole::Worker,
    )
}

/// Operator-visible model for the active provider when inherit/faster routing
/// must not cross namespaces (#3227, subagent route validation 2026-07-07).
///
/// Enumerates through the catalog-backed [`crate::provider_lake`] facade rather
/// than the raw legacy `model_completion_names_for_provider` table (#4116 /
/// #4188). The facade prefers live Models.dev, then the offline bundled
/// snapshot, and only then the legacy hardcoded table for Codewhale-only /
/// unbundled providers. This consumer only reads the first entry.
fn operator_model_for_subagent(runtime: &SubAgentRuntime) -> String {
    let provider = runtime.client.api_provider();
    if is_declared_subagent_model(&runtime.client, &runtime.model)
        || crate::config::validate_route(provider, &runtime.model).is_ok()
    {
        return runtime.model.clone();
    }
    crate::provider_lake::all_catalog_models_for_provider(provider)
        .into_iter()
        .next()
        .unwrap_or_else(|| runtime.model.clone())
}

/// Reject or remap a resolved sub-agent model so it matches the runtime
/// provider before spawn. Explicit fixed pins fail fast; inherit/faster/auto
/// fall back to the operator route instead of cross-wiring namespaces.
pub(crate) fn ensure_subagent_model_for_provider(
    runtime: &SubAgentRuntime,
    model_route: &ModelRoute,
    model: String,
) -> Result<String, ToolError> {
    let provider = runtime.client.api_provider();
    if is_declared_subagent_model(&runtime.client, &model)
        || crate::config::validate_route(provider, &model).is_ok()
    {
        return Ok(model);
    }
    match model_route {
        ModelRoute::Inherit | ModelRoute::Faster | ModelRoute::Auto => {
            Ok(operator_model_for_subagent(runtime))
        }
        ModelRoute::Fixed(_) => Err(ToolError::invalid_input(
            crate::config::validate_route(provider, &model).unwrap_err(),
        )),
    }
}

fn worker_profile_subagent_assignment_route(
    runtime: &SubAgentRuntime,
    model_route: &ModelRoute,
    requested_thinking: SubAgentThinking,
    agent_type: &FleetRole,
) -> SubAgentResolvedRoute {
    let candidates = subagent_router_candidates(runtime);
    let mut requested_fast_lane = false;
    let mut route_note = None;
    let model = match model_route {
        ModelRoute::Fixed(model) => model.clone(),
        ModelRoute::Faster | ModelRoute::Auto => {
            requested_fast_lane = true;
            match candidates.cheap.clone() {
                Some(cheap) => cheap,
                None => {
                    // Staying on the parent is correct; staying there quietly
                    // is what cost money, so the route says why.
                    route_note = Some(crate::model_routing::missing_fast_sibling_note(
                        runtime.client.api_provider(),
                        &runtime.model,
                    ));
                    runtime.model.clone()
                }
            }
        }
        ModelRoute::Inherit => runtime.model.clone(),
    };

    let role_reasoning_default =
        WorkerRuntimeProfile::for_role(agent_type.clone()).reasoning_effort;
    let reasoning_effort = subagent_reasoning_effort_for_request(
        runtime,
        &model,
        requested_fast_lane,
        requested_thinking,
        role_reasoning_default.as_deref(),
    );

    let mut resolved = SubAgentResolvedRoute::new(model_route.clone(), model, reasoning_effort);
    resolved.route_note = route_note;
    resolved
}

fn subagent_reasoning_effort_for_request(
    runtime: &SubAgentRuntime,
    model: &str,
    requested_fast_lane: bool,
    requested_thinking: SubAgentThinking,
    role_reasoning_default: Option<&str>,
) -> Option<String> {
    let normalize = |effort: ReasoningEffort| {
        effort.normalize_for_route_identity(
            runtime.client.api_provider(),
            runtime.client.admitted_provider_identity().key.as_str(),
            runtime.client.base_url(),
            model,
        )
    };
    match requested_thinking {
        SubAgentThinking::Effort(effort) => Some(normalize(effort).as_setting().to_string()),
        SubAgentThinking::Auto => Some(
            normalize(auto_subagent_reasoning_effort())
                .as_setting()
                .to_string(),
        ),
        // A role default is child intent, not inherited parent state. It wins
        // whenever the caller left thinking at `inherit`, while explicit
        // Effort/Auto requests above still take precedence. Route
        // normalization remains the final capability ceiling.
        SubAgentThinking::Inherit if role_reasoning_default.is_some() => role_reasoning_default
            .map(ReasoningEffort::from_setting)
            .map(normalize)
            .map(|effort| effort.as_setting().to_string()),
        SubAgentThinking::Inherit if requested_fast_lane => {
            // Faster/explore lane: cheaper reasoning by default. The OpenAI Codex
            // (GPT-5.5) adapter has no true "off" on the wire (it collapses off
            // to low), so we resolve Low honestly for that provider instead of
            // emitting an off that is silently rewritten. Explicit thinking
            // passed by the caller already won via the arms above.
            let provider = runtime.client.api_provider();
            let effort = if matches!(provider, crate::config::ProviderKind::OpenaiCodex) {
                ReasoningEffort::Low
            } else {
                ReasoningEffort::Off
            };
            Some(normalize(effort).as_setting().to_string())
        }
        SubAgentThinking::Inherit => fallback_subagent_reasoning_effort(runtime, model),
    }
}

fn fallback_subagent_reasoning_effort(runtime: &SubAgentRuntime, model: &str) -> Option<String> {
    let normalize = |effort: ReasoningEffort| {
        effort.normalize_for_route_identity(
            runtime.client.api_provider(),
            runtime.client.admitted_provider_identity().key.as_str(),
            runtime.client.base_url(),
            model,
        )
    };
    // `reasoning_effort_auto` is the flag; a literal `"auto"` tier is the same
    // request by another spelling. A fixed-model Fleet subprocess arrives with
    // the string and no flag, so treating only the flag as Auto left `"auto"`
    // to travel raw to a provider that has no such tier.
    let requested_auto = runtime.reasoning_effort_auto
        || runtime
            .reasoning_effort
            .as_deref()
            .is_some_and(|effort| ReasoningEffort::from_setting(effort) == ReasoningEffort::Auto);
    if requested_auto {
        Some(
            normalize(auto_subagent_reasoning_effort())
                .as_setting()
                .to_string(),
        )
    } else {
        runtime
            .reasoning_effort
            .as_deref()
            .map(ReasoningEffort::from_setting)
            .map(normalize)
            .map(|effort| effort.as_setting().to_string())
    }
}

fn auto_subagent_reasoning_effort() -> ReasoningEffort {
    crate::auto_reasoning::select()
}

fn parse_optional_subagent_model(input: &Value, key: &str) -> Result<Option<String>, ToolError> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(ToolError::invalid_input(format!("{key} cannot be blank")));
            }
            // #3018: Basic parsing only — provider-aware validation is deferred
            // to the spawn path where the runtime's ProviderKind is available.
            Ok(Some(trimmed.to_string()))
        }
        Some(_) => Err(ToolError::invalid_input(format!("{key} must be a string"))),
    }
}

/// Extract an optional `cwd: String` from spawn input and convert to a
/// `PathBuf`. Empty / absent → `None`. Workspace-boundary check happens
/// at spawn time (the parent's workspace is known there, not here).
fn parse_optional_cwd(input: &Value) -> Result<Option<PathBuf>, ToolError> {
    let raw = input.get("cwd").and_then(|v| v.as_str()).map(str::trim);
    match raw {
        None | Some("") => Ok(None),
        Some(s) => Ok(Some(PathBuf::from(s))),
    }
}

fn parse_optional_worktree_request(
    input: &Value,
) -> Result<Option<SubAgentWorktreeRequest>, ToolError> {
    let worktree_flag =
        parse_optional_bool(input, &["worktree", "isolate_worktree", "isolateWorktree"])?;
    let isolation = optional_input_str(input, &["isolation"])?
        .map(|value| value.trim().to_ascii_lowercase().replace(['_', '-'], ""));
    let isolation_wants_worktree = match isolation.as_deref() {
        None | Some("") | Some("none") | Some("shared") => false,
        Some("worktree") | Some("gitworktree") => true,
        Some(other) => {
            return Err(ToolError::invalid_input(format!(
                "isolation must be 'worktree' or 'none' (got '{other}')"
            )));
        }
    };

    let branch = optional_input_str(
        input,
        &[
            "worktree_branch",
            "worktreeBranch",
            "branch_name",
            "branchName",
            "branch",
        ],
    )?
    .map(str::to_string);
    let path = optional_input_str(
        input,
        &[
            "worktree_path",
            "worktreePath",
            "worktree_dir",
            "worktreeDir",
        ],
    )?
    .map(PathBuf::from);
    let base_ref = optional_input_str(
        input,
        &["worktree_base", "worktreeBase", "base_ref", "baseRef"],
    )?
    .map(str::to_string);

    let has_worktree_details = branch.is_some() || path.is_some() || base_ref.is_some();
    if worktree_flag == Some(false) && (isolation_wants_worktree || has_worktree_details) {
        return Err(ToolError::invalid_input(
            "worktree=false conflicts with worktree isolation options".to_string(),
        ));
    }
    if worktree_flag.unwrap_or(false) || isolation_wants_worktree || has_worktree_details {
        Ok(Some(SubAgentWorktreeRequest {
            branch,
            path,
            base_ref,
        }))
    } else {
        Ok(None)
    }
}

fn build_assignment_prompt(
    prompt: &str,
    assignment: &SubAgentAssignment,
    agent_type: &FleetRole,
) -> String {
    let role = assignment
        .role
        .as_deref()
        .map(|token| {
            FleetRole::from_str(token)
                .map(|role| role.as_str())
                .unwrap_or(token)
        })
        .unwrap_or("default");
    format!(
        "Assignment metadata:\n- objective: {}\n- role: {}\n- resolved_type: {}\n\nTask:\n{}",
        assignment.objective,
        role,
        agent_type.as_str(),
        prompt
    )
}

fn worker_status_from_subagent_status(status: &SubAgentStatus) -> AgentWorkerStatus {
    match status {
        SubAgentStatus::Running => AgentWorkerStatus::Running,
        SubAgentStatus::Completed => AgentWorkerStatus::Completed,
        SubAgentStatus::Failed(_) => AgentWorkerStatus::Failed,
        SubAgentStatus::Cancelled => AgentWorkerStatus::Cancelled,
        SubAgentStatus::BudgetExhausted => AgentWorkerStatus::Failed,
        SubAgentStatus::Interrupted(_) => AgentWorkerStatus::Interrupted,
    }
}

pub fn agent_worker_status_name(status: AgentWorkerStatus) -> &'static str {
    match status {
        AgentWorkerStatus::Queued => "queued",
        AgentWorkerStatus::Starting => "starting",
        AgentWorkerStatus::Running => "running",
        AgentWorkerStatus::WaitingForUser => "waiting_for_user",
        AgentWorkerStatus::ModelWait => "model_wait",
        AgentWorkerStatus::RunningTool => "running_tool",
        AgentWorkerStatus::Completed => "completed",
        AgentWorkerStatus::Failed => "failed",
        AgentWorkerStatus::Cancelled => "cancelled",
        AgentWorkerStatus::Interrupted => "interrupted",
    }
}

fn worker_status_from_subagent_result(result: &SubAgentResult) -> AgentWorkerStatus {
    if subagent_checkpoint_is_continuable(result) {
        AgentWorkerStatus::WaitingForUser
    } else {
        worker_status_from_subagent_status(&result.status)
    }
}

pub(crate) fn subagent_progress_tool_display_name(name: &str) -> &str {
    match name {
        "exec_shell"
        | "exec_shell_wait"
        | "exec_shell_interact"
        | "exec_shell_cancel"
        | "exec_wait"
        | "exec_interact"
        | "task_shell_start"
        | "task_shell_wait" => "bash",
        _ => name,
    }
}

fn emit_agent_progress(
    event_tx: Option<&mpsc::Sender<Event>>,
    owner_session_id: &str,
    agent_id: &str,
    status: String,
    activity: AgentProgressEventMeta,
    parent_run_id: Option<String>,
    spawn_depth: u32,
) {
    if let Some(event_tx) = event_tx {
        if event_tx.max_capacity() > MIN_EVENT_CHANNEL_HEADROOM_FOR_ROUTINE_PROGRESS
            && event_tx.capacity() <= MIN_EVENT_CHANNEL_HEADROOM_FOR_ROUTINE_PROGRESS
            && routine_agent_progress_can_preserve_event_headroom(activity.worker_status)
        {
            return;
        }
        let _ = event_tx.try_send(Event::AgentProgress {
            owner_session_id: owner_session_id.to_string(),
            id: agent_id.to_string(),
            status,
            activity,
            parent_run_id,
            spawn_depth,
        });
    }
}

fn routine_agent_progress_can_preserve_event_headroom(status: AgentWorkerStatus) -> bool {
    matches!(
        status,
        AgentWorkerStatus::Running | AgentWorkerStatus::ModelWait | AgentWorkerStatus::RunningTool
    )
}

// === Tool Registry Helpers ===

/// Role-only approval posture; the registry allow/deny and runtime envelope
/// remain the authoritative per-tool checks.
///
/// `Auto` is safe for every role. `Suggest` requires a write-capable role;
/// `Required` requires the full-shell posture. A Custom worker may describe
/// either posture, but its explicit allowlist and the runtime envelope still
/// narrow what it can execute. Read-only bash operations are classified Auto,
/// so Scouts and Reviewers do not gain general shell authority through this
/// branch.
/// Intersect an explicit parent scope with the child's requested subset.
///
/// A child can narrow an explicit parent scope, never widen it. Omitting the
/// child list inherits the parent's exact scope. A parent with the default
/// role-defined scope may accept a child list, but the role and envelope gates
/// below remain authoritative at visibility and dispatch time.
fn intersect_explicit_tool_scope(
    parent: &ToolScope,
    child: Option<Vec<String>>,
) -> Option<Vec<String>> {
    let ToolScope::Explicit(parent) = parent else {
        return child;
    };
    let Some(child) = child else {
        return Some(parent.clone());
    };
    Some(
        child
            .into_iter()
            .filter(|name| explicit_scope_permits(parent, name))
            .collect(),
    )
}

/// Family, legacy action, and lowercase primitive spellings are equivalent.
/// This is intentionally case-insensitive so a saved `File`/`Bash` scope and
/// the current `read`/`write`/`edit`/`bash` names describe the same authority.
fn explicit_scope_permits(parent: &[String], name: &str) -> bool {
    parent
        .iter()
        .any(|allowed| policy_tool_name_matches(allowed, name))
}

fn policy_tool_name_matches(rule: &str, name: &str) -> bool {
    let empty = Value::Null;
    let semantic_rule = canonical_action_alias(rule, &empty);
    let semantic_name = canonical_action_alias(name, &empty);
    rule.eq_ignore_ascii_case(name)
        || semantic_rule.eq_ignore_ascii_case(semantic_name)
        || CANONICAL_ACTION_ALIASES.iter().any(|(family, _, alias)| {
            rule.eq_ignore_ascii_case(family) && semantic_name.eq_ignore_ascii_case(alias)
                || name.eq_ignore_ascii_case(family) && semantic_rule.eq_ignore_ascii_case(alias)
        })
}

/// A test probe over the admitted Core registry and batch pipeline. This
/// fixture does not contain a child executor, guardian or permission gate.
#[cfg(test)]
struct ChildCoreProbe {
    gate_runtime: SubAgentRuntime,
    authority: Arc<engine::ChildAuthority>,
    registry: ToolRegistry,
}
#[cfg(test)]
impl std::ops::Deref for ChildCoreProbe {
    type Target = engine::ChildAuthority;
    fn deref(&self) -> &Self::Target {
        &self.authority
    }
}
#[cfg(test)]
impl std::ops::DerefMut for ChildCoreProbe {
    fn deref_mut(&mut self) -> &mut Self::Target {
        Arc::make_mut(&mut self.authority)
    }
}
#[cfg(test)]
impl ChildCoreProbe {
    fn new(
        runtime: SubAgentRuntime,
        role: FleetRole,
        scope: Option<Vec<String>>,
        todos: SharedTodoList,
        plan: SharedPlanState,
    ) -> Self {
        let mut probe = Self::new_with_owner(
            runtime,
            role,
            "agent_unknown".into(),
            "child probe".into(),
            scope,
            todos,
            plan,
        );
        probe.enforce_write_claim = false;
        probe
    }
    fn new_with_owner(
        runtime: SubAgentRuntime,
        role: FleetRole,
        owner: String,
        label: String,
        scope: Option<Vec<String>>,
        todos: SharedTodoList,
        plan: SharedPlanState,
    ) -> Self {
        let authority = engine::ChildAuthority::capture(runtime, role, owner, label, scope);
        let mut admitted = authority.runtime.clone();
        admitted.worker_profile.shell = authority.grant.shell_policy();
        admitted.agent_tool_surface_options.shell_policy = authority.grant.shell_policy();
        admitted.parent_agent_id = Some(authority.owner_agent_id.clone());
        let mut registry = authority
            .tool_registry_builder(admitted, todos, plan)
            .build(authority.context());
        registry.remove_tool("create_goal");
        registry.remove_tool("update_goal");
        let forbidden = registry
            .all()
            .iter()
            .filter(|spec| is_machine_control_tool(spec.name()))
            .map(|spec| spec.name().to_owned())
            .collect::<Vec<_>>();
        for name in forbidden {
            registry.remove_tool(&name);
        }
        Self {
            gate_runtime: authority.runtime.clone(),
            authority,
            registry,
        }
    }
    fn posture_permits_tool(&self, name: &str, input: Option<&Value>) -> bool {
        self.authority
            .posture_permits_tool(&self.registry, name, input)
    }
    fn grant_blocks_tool(&self, name: &str) -> bool {
        self.authority.grant_blocks_tool(&self.registry, name)
    }
    fn envelope_refusal(&self, name: &str, input: &Value) -> Option<String> {
        let spec = self.registry.get(name)?;
        crate::tools::execution_envelope::enforce_execution_envelope(
            name,
            input,
            spec.as_ref(),
            self.execution_envelope(),
            self.bounded_readonly_bash_evidence(name, input),
        )
        .err()
    }
    fn envelope_permits(&self, name: &str, input: &Value) -> bool {
        self.authority.envelope_permits(&self.registry, name, input)
    }
    fn delegation_refusal(&self, name: &str, input: &Value) -> Option<String> {
        (!self.authority.delegated_call(&self.registry, name, input))
            .then(|| format!("{name} requires the same Core approval decision"))
    }
    fn tools_for_model(&self, role: &FleetRole) -> Vec<Tool> {
        self.authority.tools_for_model(&self.registry, role)
    }
    fn deferred_catalog_for_model(&self, _role: &FleetRole) -> Vec<Tool> {
        crate::core::engine::Engine::probe_child_catalog(&self.authority, &self.registry)
    }
    fn unavailable_allowed_tools(&self) -> Vec<String> {
        self.grant
            .scope
            .iter()
            .flatten()
            .filter(|name| !is_tool_search_tool(name) && !self.registry.contains(name))
            .cloned()
            .collect()
    }
    async fn execute_full(
        &self,
        agent: &str,
        id: &str,
        name: &str,
        input: Value,
    ) -> Result<RichToolResult> {
        let catalog = self.tools_for_model(&self.agent_type);
        let warm = catalog
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let mut surface = crate::core::engine::ChildSurfaceProbe::new(catalog, &warm);
        self.execute_from_surface(agent, id, &mut surface, name, input)
            .await
    }
    async fn execute(&self, agent: &str, name: &str, input: Value) -> Result<String> {
        self.execute_full(agent, &new_child_execution_id(agent), name, input)
            .await
            .map(|answer| answer.result.content)
    }
    async fn execute_from_surface(
        &self,
        agent: &str,
        id: &str,
        surface: &mut crate::core::engine::ChildSurfaceProbe,
        name: &str,
        input: Value,
    ) -> Result<RichToolResult> {
        let mut captured = self.authority.as_ref().clone();
        captured.runtime = self.gate_runtime.clone();
        captured.owner_agent_id = agent.to_owned();
        let captured = Arc::new(captured);
        let mut registry = ToolRegistry::new(captured.context());
        registry.register_all(self.registry.all());
        let execution_id = if SubAgentManager::is_child_approval_id(id)
            && id.starts_with(&format!("agent:{agent}:approval:"))
        {
            id.to_owned()
        } else {
            captured.new_execution_id()
        };
        crate::core::engine::Engine::probe_child_call(
            captured,
            registry,
            surface,
            crate::core::engine::ChildProbeCall {
                id: id.to_owned(),
                execution_id,
                name: name.to_owned(),
                input,
            },
        )
        .await
    }
}

/// A child admission refusal is a typed permission denial, not an opaque
/// execution error — the worker loop's no-progress guard (#6015) can only
/// count denials it can name.
/// Whether `name` is a desktop / computer-control surface.
///
/// Desktop control is the most machine-wide capability Codewhale can grant —
/// it types on the user's screen — so children never inherit it: `execute_full`
/// refuses the whole family at dispatch and the child registry build removes
/// it from the catalog, so a tool that would be denied never appears on the
/// wire (#6296, #6298). Classification is by MCP naming
/// (`mcp_<server>_<tool>`): the two shipped computer-use surfaces plus the
/// generic `computer-use` / `computer_use` markers a third-party desktop
/// server carries in its server id.
pub(crate) fn is_machine_control_tool(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("mcp_") else {
        return false;
    };
    rest.starts_with("codewhale-cu_")
        || rest.starts_with("codewhale--cu_")
        || rest.contains("computer-use")
        || rest.contains("computer_use")
}

fn admission_denied(message: impl Into<String>) -> anyhow::Error {
    ToolError::permission_denied(message).into()
}

fn is_unbounded_shell_run(name: &str, input: &Value) -> bool {
    canonical_action_alias(name, input) == "exec_shell"
}

/// Whether this exact call is a shell run the agent read-only classifier
/// proves mutation-free, or native read-only mode enforces — the contention carve-out.
///
/// This shares its classifier with `bounded_readonly_bash_evidence` but not
/// its role restriction, deliberately: the envelope helper's carve-out
/// *grants* shell to inspection roles that otherwise lack it, so it is scoped
/// to them; the contention gate grants nothing — shell authority, posture, and
/// the envelope have already settled by the time it runs — and asks only
/// whether this call can collide with a live peer's writes. A proven
/// read-only `ls` cannot, whichever role runs it. An explicit read_only=true
/// call likewise cannot: the shared bash executor requires native read-only
/// enforcement or refuses the call before process spawn.
fn proven_readonly_shell_run(name: &str, input: &Value) -> bool {
    is_unbounded_shell_run(name, input) && crate::tools::shell::agent_readonly_bash_input(input)
}

/// Parameter names whose *value* is a location to reach, rather than content.
///
/// Deliberately a name list and not a scan of every string: a network-denied
/// child may perfectly well write a file whose body mentions `https://`, or grep
/// for one. Refusing that would be a false positive with no security value. The
/// question this guard asks is "is this call *addressed* somewhere remote", and
/// only a field that names a destination can answer it.
const URL_BEARING_FIELDS: &[&str] = &[
    "url",
    "urls",
    "uri",
    "href",
    "link",
    "endpoint",
    "base_url",
    "target",
    "pr",
    "pull_request",
];

/// Whether a string addresses a remote location over a network scheme.
///
/// Scheme-anchored, so a workspace path, a bare filename, or a `git@` remote is
/// not mistaken for one. `file:` is deliberately absent: it names no host.
fn is_network_url(value: &str) -> bool {
    let value = value.trim();
    let lowered = value.to_ascii_lowercase();
    ["http://", "https://", "ws://", "wss://", "ftp://"]
        .iter()
        .any(|scheme| lowered.starts_with(scheme))
}

/// Whether any URL-bearing field in `input` addresses the network.
///
/// Scans the top level and one level of nesting (objects and arrays), which
/// covers every shape the tool schemas here actually use — `{"url": ...}`,
/// `{"urls": [...]}`, `{"source": {"url": ...}}` — without recursing into
/// arbitrary model-supplied content.
fn carries_network_url(input: &Value) -> bool {
    fn field_reaches(key: &str, value: &Value) -> bool {
        if !URL_BEARING_FIELDS.contains(&key) {
            return false;
        }
        match value {
            Value::String(text) => is_network_url(text),
            Value::Array(items) => items
                .iter()
                .any(|item| item.as_str().is_some_and(is_network_url)),
            _ => false,
        }
    }

    let Some(object) = input.as_object() else {
        return false;
    };
    object.iter().any(|(key, value)| {
        field_reaches(key, value)
            || match value {
                Value::Object(nested) => nested
                    .iter()
                    .any(|(nested_key, nested_value)| field_reaches(nested_key, nested_value)),
                Value::Array(items) => items.iter().any(|item| {
                    item.as_object().is_some_and(|nested| {
                        nested.iter().any(|(nested_key, nested_value)| {
                            field_reaches(nested_key, nested_value)
                        })
                    })
                }),
                _ => false,
            }
    })
}

/// Refuse a call that reaches the network through a tool the name deny list
/// does not consider a network tool.
///
/// The deny list is keyed on names, and that is sufficient for any tool whose
/// whole purpose is the network — those are simply absent. It is not sufficient
/// for a *mostly local* tool that reaches out only for certain inputs, because
/// such a tool calls the fetch path itself, inside the process, under its own
/// name. Two live examples, both real escapes before this guard existed:
///
/// - `rlm{action:"open", url:...}` calls `FetchUrlTool::execute` directly.
/// - `review{target:"https://github.com/o/r/pull/1"}` shells out to `gh pr
///   diff`, which is the network by way of a subprocess.
///
/// Both are now unreachable by name as well (`rlm_open` is denied outright;
/// `review`'s local forms are the ones worth keeping), so this is the layer that
/// catches the *next* one — a tool that grows a `url` field after this list was
/// written. It fails closed and names the posture, so the refusal reads as a
/// contract rather than a malfunction.
fn reject_network_reaching_input(name: &str, input: &Value) -> Result<()> {
    // Judged per segment, so a gh or npm read inside a pipeline or chain is
    // still a network read (#6015).
    let shell_network_read = matches!(name, "bash" | "Bash" | "exec_shell")
        && input
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| {
                !codewhale_execpolicy::command_safety::readonly_network_reads(command).is_empty()
            });
    if !shell_network_read && !carries_network_url(input) {
        return Ok(());
    }
    Err(anyhow!(
        "Tool {name} was called with a network address, but this agent runs with no network \
         capability (`network_tool = false`). Local sources are still available; a remote one \
         needs a member whose saved ceiling grants network tools."
    ))
}

/// Refuse the unbounded forms of the verification surface for a write-denied
/// child.
///
/// A read-only member keeps `Run` / `run_tests` / `run_verifiers` on purpose:
/// running the checks is what a verifier is *for*, and removing them would make
/// the role useless. But both tools accept an escape hatch that is not
/// verification at all — `run_verifiers` takes `commands`, an array of arbitrary
/// `program` + `args` pairs, and `run_tests` takes `args`, a raw cargo argv.
/// `{"program": "bash", "args": ["-lc", "rm -rf src"]}` is exactly the raw shell
/// that [`crate::fleet::role::RAW_SHELL_DENYLIST`] just removed, re-entered
/// through the one door that was left open for honest reasons.
///
/// So the tools stay and the arbitrary arguments go. The default form — the one
/// the deny list's comment actually promises is bounded — keeps working.
fn reject_unbounded_verification(name: &str, input: &Value, shell: bool) -> Result<()> {
    use crate::tools::execution_envelope::{VerificationBound, classify_verification};

    match classify_verification(canonical_action_alias(name, input), input) {
        None | Some(VerificationBound::Default) => Ok(()),
        // A pure test selection is what the shipped `verifier` role exists to
        // run. It starts a process, so it costs shell authority — and nothing
        // else, because `write` is not what a test filter needs.
        Some(VerificationBound::Filter) if shell => Ok(()),
        Some(VerificationBound::Filter) => Err(anyhow!(
            "Tool {name} was called with test-selection arguments, which start a test process, \
             and this agent has no shell authority. Drop `args` to run the default verification \
             gate."
        )),
        Some(VerificationBound::Unbounded) => Err(anyhow!(
            "Tool {name} was called with operator-supplied commands or arguments that can name a \
             program or redirect what runs, which spawns arbitrary programs and can mutate the \
             workspace. This agent runs read-only, so only the built-in verification gates and \
             test-selection arguments are available. Drop `commands`, drop the redirecting flag, \
             or use a write-capable role."
        )),
    }
}

fn is_internal_coordination_state_tool(name: &str) -> bool {
    matches!(
        name,
        "agent"
            | "agents/list"
            | "agents/message"
            | "agents/followup"
            | "agents/interrupt"
            | "agents/coordinate"
            | "agents/wait"
            | "work_update"
            | "checklist_add"
            | "checklist_update"
            | "checklist_write"
            | "todo_add"
            | "todo_update"
            | "todo_write"
    )
}

/// Raw target paths of a write-capable call, before claim normalization:
/// the patch preflight's touched files for `apply_patch`/`File.patch`, else
/// the `path`/`output_path` field. Feeds the child write carve-out (#5186);
/// claim enforcement keeps using the normalized [`mutation_paths`].
fn raw_mutation_target_paths(name: &str, input: &Value) -> Vec<String> {
    if name == "apply_patch"
        || (name == "File" && input.get("action").and_then(Value::as_str) == Some("patch"))
    {
        let mut patch_input = input.clone();
        if let Some(object) = patch_input.as_object_mut() {
            object.remove("action");
        }
        return crate::tools::apply_patch::preflight_apply_patch(&patch_input)
            .map(|preflight| preflight.touched_files)
            .unwrap_or_default();
    }
    input
        .get("path")
        .or_else(|| input.get("output_path"))
        .and_then(Value::as_str)
        .map(|path| vec![path.to_string()])
        .unwrap_or_default()
}

fn mutation_paths(name: &str, input: &Value) -> Result<Vec<String>> {
    let raw_paths = if name == "apply_patch"
        || (name == "File" && input.get("action").and_then(Value::as_str) == Some("patch"))
    {
        let mut patch_input = input.clone();
        if let Some(object) = patch_input.as_object_mut() {
            object.remove("action");
        }
        crate::tools::apply_patch::preflight_apply_patch(&patch_input)
            .map_err(|err| anyhow!(err.to_string()))?
            .touched_files
    } else if let Some(path) = input
        .get("path")
        .or_else(|| input.get("output_path"))
        .and_then(Value::as_str)
    {
        vec![path.to_string()]
    } else {
        Vec::new()
    };
    raw_paths
        .into_iter()
        .map(|path| normalize_claim_path(&path).map_err(anyhow::Error::msg))
        .collect()
}

fn reject_subagent_terminal_takeover(name: &str, input: &Value) -> Result<()> {
    let wants_interactive_shell = matches!(name, "bash" | "Bash" | "exec_shell")
        && input
            .get("action")
            .and_then(Value::as_str)
            .is_none_or(|action| action == "run")
        && input
            .get("interactive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if wants_interactive_shell {
        return Err(anyhow!(
            "Sub-agents run in the background and cannot use Bash with interactive=true \
             because that would take over the parent TUI terminal. Use Bash without \
             interactive, or with background=true / tty=true instead."
        ));
    }
    Ok(())
}

/// Resolve the effective allowed-tools list for a child.
///
/// **v0.6.6 default: full inheritance.** Returning `Ok(None)` means the
/// child sees the same tool surface as the parent's Agent mode — every
/// family including `with_subagent_tools` so it can recurse. The narrowing
/// path (`Ok(Some(list))`) is only used by:
/// - `Custom` agent types (which require an explicit list).
/// - Callers that pass `explicit_tools` (advanced / legacy use).
///
/// `allow_shell = false` no longer narrows the tool LIST — the child's
/// registry simply doesn't register shell tools, which has the same
/// effect without papering over the parent's choice with a deny-list.
fn build_allowed_tools(
    agent_type: &FleetRole,
    explicit_tools: Option<Vec<String>>,
    _allow_shell: bool,
) -> Result<Option<Vec<String>>> {
    if let Some(tools) = explicit_tools {
        let mut deduped = Vec::new();
        for tool in tools {
            let name = tool.trim();
            if !name.is_empty() && !deduped.iter().any(|existing: &String| existing == name) {
                deduped.push(name.to_string());
            }
        }
        if matches!(agent_type, FleetRole::Custom) && deduped.is_empty() {
            return Err(anyhow!(
                "Custom sub-agent requires a non-empty allowed_tools list"
            ));
        }
        return Ok(Some(deduped));
    }

    if matches!(agent_type, FleetRole::Custom) {
        return Err(anyhow!(
            "Custom sub-agent requires a non-empty allowed_tools list"
        ));
    }

    // Default: full registry inheritance from the parent. The child sees every
    // tool the parent has, including the sub-agent management family. The
    // registry execution guard still blocks approval-gated tools unless the
    // parent runtime is auto-approved.
    Ok(None)
}

/// Render a sub-agent model failure with its full error chain. `to_string()`
/// on an anyhow error prints only the outermost context (for Codex children
/// that is the bare "Responses API request failed"), discarding the HTTP
/// status, sanitized body snippet, and error class carried by the source
/// `LlmError` — the exact masking reported in #3884. The alternate format
/// walks the chain, and the downcast prefixes a stable class tag so failure
/// records distinguish auth/rate-limit/invalid-request/model/server/network
/// failures at a glance.
fn subagent_failure_message(err: &anyhow::Error) -> String {
    let class = match err.downcast_ref::<LlmError>() {
        Some(LlmError::RateLimited { .. }) => Some("rate_limited"),
        Some(LlmError::QuotaExhausted(_)) => Some("quota_exhausted"),
        Some(LlmError::ServerError { .. }) => Some("server"),
        Some(LlmError::NetworkError(_)) | Some(LlmError::Timeout(_)) => Some("network"),
        Some(LlmError::AuthenticationError(_)) | Some(LlmError::AuthorizationError(_)) => {
            Some("auth")
        }
        Some(LlmError::InvalidRequest { .. }) => Some("invalid_request"),
        Some(LlmError::ModelError(_)) => Some("model"),
        Some(LlmError::ContentPolicyError(_)) => Some("content_policy"),
        Some(LlmError::ContextLengthError(_)) => Some("context_length"),
        Some(LlmError::ParseError(_)) | Some(LlmError::Other(_)) | None => None,
    };
    match class {
        Some(class) => format!("[{class}] {err:#}"),
        None => format!("{err:#}"),
    }
}

/// Human label for how a child's model was selected, so a launch failure can
/// name the route that produced the failing model — inherited from the parent,
/// a faster same-family sibling, or an explicit id (#4049).
fn route_source_label(route: &ModelRoute) -> String {
    match route {
        ModelRoute::Inherit => "inherited from the parent/session model".to_string(),
        ModelRoute::Faster => "faster same-family sibling of the parent model".to_string(),
        ModelRoute::Auto => "auto (legacy route, treated as a faster sibling)".to_string(),
        ModelRoute::Fixed(id) => format!("explicit model id `{id}`"),
    }
}

/// When a child agent fails because its model is unavailable under the current
/// access profile, a bare provider 403/404 (classified `Authorization` or
/// `State`) or a spent-balance quota refusal (classified `RateLimit`) is
/// unactionable. Annotate it so the parent knows which provider and route
/// produced the failing model and how to recover (#2653, #4049) without
/// re-classifying the underlying error. Short-lived rate limits and errors
/// unrelated to model availability pass through unchanged.
#[cfg(test)]
fn annotate_child_model_error(
    err: &str,
    model: &str,
    provider: crate::config::ProviderKind,
    route: &ModelRoute,
) -> String {
    annotate_child_model_error_with_origin(err, model, provider, route, None)
}

/// [`annotate_child_model_error`] naming the route's real source when the
/// spawn recorded one: a saved profile, a config pin, or a task argument,
/// with the exact way to change it. Only a task `model` argument is ever
/// called an explicit child model override.
fn annotate_child_model_error_with_origin(
    err: &str,
    model: &str,
    provider: crate::config::ProviderKind,
    route: &ModelRoute,
    origin: Option<&SpawnRouteOrigin>,
) -> String {
    let hint = || match origin {
        Some(origin) => format!(
            "{err}\n(provider `{}` · requested model `{model}` · route: {} — {})",
            provider_name_for_error(provider),
            origin.source,
            origin.change,
        ),
        None => format!(
            "{err}\n(provider `{}` · requested model `{model}` · route: {} — \
             the model may be unavailable under the current access profile; {} before retrying)",
            provider_name_for_error(provider),
            route_source_label(route),
            if matches!(route, ModelRoute::Fixed(_)) {
                // Fixed covers task `model` arguments, saved profiles and
                // config pins alike; without an origin, name them all.
                "change the child's pinned model (the task's `model` argument, a saved agent profile in /fleet members, or a config.toml role pin) or adjust child-agent model config"
            } else {
                "check that provider's access or adjust child-agent model config"
            },
        ),
    };
    let lower = err.to_ascii_lowercase();
    match crate::error_taxonomy::classify_error_message(err) {
        crate::error_taxonomy::ErrorCategory::Authorization => hint(),
        crate::error_taxonomy::ErrorCategory::RateLimit
            if crate::error_taxonomy::is_spent_balance_message(err) =>
        {
            hint()
        }
        crate::error_taxonomy::ErrorCategory::State if lower.contains("model") => hint(),
        _ => {
            // #3020 (#2653): Provider rejections like "Model Not Exist" or
            // "does not exist or you do not have access" often classify as
            // `Internal` rather than `Authorization`/`State`.  Catch these
            // patterns in the raw error text and annotate anyway.
            if lower.contains("model not exist")
                || lower.contains("model_not_found")
                || lower.contains("model") && lower.contains("does not exist")
                || lower.contains("no such model")
                || lower.contains("invalid model")
            {
                hint()
            } else {
                err.to_string()
            }
        }
    }
}

/// Char budget above which a sub-agent summary is treated as a large dump and
/// head+tail truncated. The floor mirrors `TOOL_RESULT_SENT_CHAR_BUDGET` in
/// `crates/tui/src/client/chat.rs`, but the effective budget follows the same
/// adaptive resolver as every other tool result so an explicit
/// `tool_result_max_bytes` opt-in is honored instead of silently capped.
const SUBAGENT_SUMMARY_CHAR_BUDGET: usize = 12_000;

fn subagent_summary_char_budget() -> usize {
    crate::tools::large_output_router::WorkshopConfig::active_tool_result_max_bytes()
        .map(|bytes| bytes.clamp(SUBAGENT_SUMMARY_CHAR_BUDGET, 2 * 1024 * 1024))
        .unwrap_or(SUBAGENT_SUMMARY_CHAR_BUDGET)
}

/// One-line provenance suffix reinforcing that a sub-agent summary is a
/// self-report (issue #2652). Appended only when the summary was NOT
/// length-truncated, so every summary carries exactly one boundary marker.
const SUBAGENT_SELF_REPORT_NOTE: &str = "\n[Sub-agent self-report — re-verify material claims (read changed files, \
run the relevant tests) before relying on it.]";

/// Stamp a sub-agent summary with a provenance/clip marker (issue #2652).
///
/// Returns `(stamped_summary, truncated)`:
/// - When the raw summary is within the budget, append the soft self-report
///   note and report `truncated: false`.
/// - When it exceeds the budget, keep a head+tail slice and stamp it with the
///   existing `[Output truncated ...]` vocabulary (reused from tool-output
///   truncation). When `report_ref` names the persisted full report (see
///   `spill_subagent_final_report`), the footer points at it so the elided
///   middle stays retrievable via `retrieve_tool_result`; with no ref the
///   footer stays honest that the middle cannot be retrieved. Report
///   `truncated: true` either way.
///
/// Every summary therefore gets exactly one boundary marker, never both.
#[cfg(test)]
fn stamp_subagent_summary(raw: &str) -> (String, bool) {
    stamp_subagent_summary_with_ref(raw, None)
}

/// The ref-aware stamper; see `stamp_subagent_summary`.
fn stamp_subagent_summary_with_ref(raw: &str, report_ref: Option<&str>) -> (String, bool) {
    let total = raw.chars().count();
    let budget = subagent_summary_char_budget();
    if total <= budget {
        return (format!("{raw}{SUBAGENT_SELF_REPORT_NOTE}"), false);
    }
    // Spend the whole budget on content: two-thirds head, one-third tail. The
    // old fixed 4,000 + 4,000 cut a 12,001-char report down to 8,000.
    let head_chars = budget * 2 / 3;
    let tail_chars = budget - head_chars;
    let chars: Vec<char> = raw.chars().collect();
    let head: String = chars.iter().take(head_chars).collect();
    let tail: String = chars
        .iter()
        .skip(total.saturating_sub(tail_chars))
        .collect();
    let omitted = total.saturating_sub(head_chars).saturating_sub(tail_chars);
    let retrieval = match report_ref {
        Some(reference) => format!(
            "the full report is retained as artifact {reference} — read the elided middle ({omitted} \
chars) with retrieve_tool_result using that ref (mode=lines/query/bytes). Re-verify material claims \
before relying on them."
        ),
        None => format!(
            "the elided middle ({omitted} chars) is not in the spillover store and cannot be \
retrieved via retrieve_tool_result. Re-open the child or read changed files directly to verify \
material claims."
        ),
    };
    let stamped = format!(
        "{head}\n\n[Sub-agent summary truncated: {head_chars} + {tail_chars} of {total} \
chars shown. This is the child's self-report; {retrieval}]\n\n{tail}",
    );
    (stamped, true)
}

/// Persist a final report that exceeds the summary budget so the truncated
/// summary can name a retrievable artifact instead of dropping the elided
/// middle. Returns the `retrieve_tool_result` ref on success; write failures
/// degrade to no ref, mirroring `apply_spillover`'s passthrough posture. The
/// artifact lands under the shared session root (`state_namespace`), which
/// every agent in the spawn tree clones, so the parent and any sibling can
/// resolve it. Same-id writes carry identical bytes, so a synthesized
/// re-delivery of the same terminal result is idempotent.
pub(crate) fn spill_subagent_final_report(
    session_id: &str,
    result: &SubAgentResult,
) -> Option<String> {
    let raw = summarize_subagent_result(result);
    if raw.chars().count() <= subagent_summary_char_budget() {
        return None;
    }
    let artifact_id = format!("art_sa_{}_report", result.agent_id);
    crate::artifacts::write_session_artifact(session_id, &artifact_id, &raw)
        .ok()
        .map(|_| artifact_id)
}

fn summarize_subagent_result(result: &SubAgentResult) -> String {
    if let Some(needs_input) = result.needs_input.as_ref() {
        return format!("Needs input: {}", needs_input.question);
    }
    match (&result.status, result.result.as_ref()) {
        (SubAgentStatus::Completed, Some(text)) => text.clone(),
        (SubAgentStatus::Completed, None) => "Completed (no final summary returned)".to_string(),
        (SubAgentStatus::Interrupted(error), _) => format!("Interrupted: {error}"),
        (SubAgentStatus::Cancelled, _) => "Cancelled".to_string(),
        (SubAgentStatus::BudgetExhausted, Some(text)) => format!(
            "Child budget exhausted before finishing; partial output preserved below.\n{text}"
        ),
        (SubAgentStatus::BudgetExhausted, None) => {
            "Child budget exhausted before returning a final summary; inspect the checkpoint and split the work.".to_string()
        }
        (SubAgentStatus::Failed(error), _) => format!("Failed: {error}"),
        (SubAgentStatus::Running, _) => "Running".to_string(),
    }
}

pub(crate) fn subagent_status_name(status: &SubAgentStatus) -> &'static str {
    match status {
        SubAgentStatus::Running => "running",
        SubAgentStatus::Completed => "completed",
        SubAgentStatus::Interrupted(_) => "interrupted",
        SubAgentStatus::Failed(_) => "failed",
        SubAgentStatus::Cancelled => "cancelled",
        SubAgentStatus::BudgetExhausted => "budget_exhausted",
    }
}

/// Settled beyond resume: the worker will never run again under this record.
/// `Interrupted` is excluded — an interrupted child continues from its
/// checkpoint, so its name stays reserved (#6313).
pub(crate) fn subagent_status_is_settled(status: &SubAgentStatus) -> bool {
    matches!(
        status,
        SubAgentStatus::Completed
            | SubAgentStatus::Failed(_)
            | SubAgentStatus::Cancelled
            | SubAgentStatus::BudgetExhausted
    )
}

use crate::prompts::text::SUBAGENT_OUTPUT_FORMAT;

const GENERAL_AGENT_INTRO: &str = concat!(
    "You are a trusted Fleet worker. Your job is to complete the one task you were given, end-to-end, and report back concisely.\n",
    "Stay inside the assigned scope; put adjacent work under RISKS/BLOCKERS.\n",
    "For genuinely multi-step work, track progress with `todo_write`; skip it for short, focused tasks.\n",
    "**Stop quickly on failure**: if the same tool call fails 2 times in a row, stop retrying and return what you have so far with a one-line note explaining what's missing. Do not loop on impossible queries (e.g. external API unreachable, rate-limited, or returning empty).\n",
    "For builder or repair-style work, keep going within the assigned scope; checkpoint before broadening the task or after repeated failures instead of forcing a tiny tool-call cap.\n\n"
);

const EXPLORE_AGENT_INTRO: &str = concat!(
    "You are a trusted Fleet scout (role: `scout`). Your job is to map the relevant code quickly and stay strictly read-only.\n",
    "Default to `EFFORT: quick`: stop at decisive evidence, not at a number — there is no per-agent call cap to save.\n",
    "Orient first: confirm the workspace/project root, read relevant AGENTS.md/README guidance when the tree is unfamiliar, then search only the likely scope.\n",
    "Use `read` for bounded file reads and `bash` only for the allowed read-only inspection subset: navigation/rg, safe Git reads (for example `git log -n 5`), and read-only GitHub views such as `gh issue view`. Builds, tests, writes, and shell control actions are unavailable.\n",
    "Use your private `todo_write` list as editable working notes when useful; it is agent-owned state, not permission to write project files. Those tool calls remain in the complete transcript artifact returned to the parent.\n",
    "Honor QUESTION, SCOPE, ALREADY_KNOWN, and STOP_CONDITION. Do not repeat ALREADY_KNOWN work unless evidence contradicts it; do not broaden once QUESTION is answered.\n",
    "Your value is compressed evidence: cite `path:line-range` for each finding and stop once evidence is sufficient. Return partial findings if the next step would be speculative or duplicative.\n",
    "CHANGES will almost always be \"None.\" for a scout.\n\n"
);

const PLAN_AGENT_INTRO: &str = concat!(
    "You are a trusted Fleet planner (role: `planner`). Your job is to produce a grounded, prioritized plan, not patches.\n",
    "Read enough code to avoid guessing; each step names its artifact and verification.\n",
    "Use `read` for bounded file reads and `bash` only for the allowed read-only inspection subset: navigation/rg, safe Git reads (for example `git log -n 5`), and read-only GitHub views such as `gh issue view`. Builds, tests, writes, and shell control actions are unavailable.\n",
    "Use todo_write for concrete To-do progress; explain key trade-offs in the plan you return.\n",
    "CHANGES should list plan artifacts only, not future speculative edits.\n\n"
);

const REVIEW_AGENT_INTRO: &str = concat!(
    "You are an adversarial Fleet reviewer (role: `reviewer`). Assume the change is broken until the evidence proves otherwise: actively try to refute the claims made about it, and stay strictly read-only.\n",
    "Read the diff/files, grep sibling patterns/tests, hunt regressions, missing tests, unhandled edge cases, and quiet behavior changes, then order EVIDENCE by severity.\n",
    "Use `read` for bounded file reads and `bash` only for the allowed read-only navigation/rg, safe Git, and read-only GitHub evidence subset; builds, tests, writes, and shell control actions are unavailable.\n",
    "Use your private `todo_write` list as editable working notes when useful; it is agent-owned state, not permission to write project files. Those tool calls remain in the complete transcript artifact returned to the parent.\n",
    "Use BLOCKER/MAJOR/MINOR/NIT and include path:line-range plus suggested fix.\n",
    "You may use more tool calls than quick exploration, but stop after decisive evidence instead of widening the review forever.\n",
    "If nothing survives your attack, say plainly in SUMMARY that no MAJOR+ issues exist — a clean verdict earned adversarially is a real result, not a failure.\n",
    "CHANGES will almost always be \"None.\" for a reviewer.\n\n"
);

const CUSTOM_AGENT_INTRO: &str = concat!(
    "You are a trusted custom Fleet worker (role: `custom`) with a narrowed tool registry. Your job is to stay tightly scoped to the assigned objective.\n",
    "Use only tools available at runtime; put missing capabilities under BLOCKERS and stop.\n\n"
);

const IMPLEMENTER_AGENT_INTRO: &str = concat!(
    "You are a trusted Fleet builder (role: `builder`). Your job is to land the assigned change with minimal surrounding edits.\n",
    "Use `edit` for precise unique replacements, `write` for whole-file changes, and discover `apply_patch` for unified multi-file patches when needed.\n",
    "Run relevant verification after edit batches; write needed tests with the implementation.\n",
    "You are not limited to a scout-style 3-5 tool-call cap. Checkpoint before expanding scope or after repeated failures, then continue only inside the assigned brief.\n",
    "CHANGES is load-bearing: list every modified file with a one-line why.\n",
    "Before finishing, end with a VERDICT block: PASS or FAIL, the exact commands you ran (or why verification was impossible), and brief evidence. A diff alone is not completion.\n\n"
);

/// Spawn-time contract for write-capable children: they must return PASS/FAIL
/// evidence, not just a patch. Injected into the child system prompt for
/// implementer/builder and other write-authority roles (Operate C1).
const WRITE_CHILD_VERIFY_CONTRACT: &str = concat!(
    "\n\n## Verify-before-return (write child)\n",
    "You are a write-capable worker. Completing file edits is not enough.\n",
    "1. Run the repository's relevant checks for the change you made (tests, lint, typecheck, or the acceptance criteria in your brief).\n",
    "2. End your final message with a structured evidence block:\n",
    "   VERDICT: PASS | FAIL\n",
    "   COMMANDS: <exact commands run, one per line, or NONE with reason>\n",
    "   EVIDENCE: <exit codes, failing assertion, or concise proof of PASS>\n",
    "3. Do not claim PASS without command or inspection evidence. If you cannot run checks, report FAIL or BLOCKED with the blocker — never invent success.\n",
);

const CONSULTANT_AGENT_INTRO: &str = concat!(
    "You are a trusted Fleet consultant (role: `consultant`). You are asked for judgement, not for labour.\n",
    "You are read-only and have no shell. Read the workspace and the public web to ground your advice, then give counsel.\n",
    "Lead with your actual recommendation, not a survey of options. If you would do something different from what was proposed, say so first and say why.\n",
    "Name what the asker appears not to have considered: the failure mode, the constraint, the cheaper alternative, the reason this is harder than it looks.\n",
    "Distinguish what you verified by reading from what you are inferring. An unverified hunch is still useful — labelled as one.\n",
    "If the question is underspecified in a way that changes the answer, say which detail decides it rather than answering both ways at length.\n",
    "CHANGES will always be \"None.\" for a consultant.\n\n"
);

const VERIFIER_AGENT_INTRO: &str = concat!(
    "You are a trusted Fleet verifier (role: `verifier`). Your job is to run the requested gates with your bounded validation tools — Run tests/verifiers (pass `cwd` when the checks live in a subdirectory), Git fetch for remote refs, Git merge_tree for merge results — and report results. You never write: patching the workspace is denied. Unbounded shell forms are refused; use the verification surface. If a probe the task needs is refused, report the blocked probe to the parent instead of working around it.\n",
    "Report PASS/FAIL/FLAKY at the top of SUMMARY with exact command evidence.\n",
    "Capture failing assertion and file:line; put obvious fixes under RISKS.\n",
    "You may use more tool calls than quick exploration, but stop after decisive pass/fail evidence.\n",
    "CHANGES will almost always be \"None.\" for a verifier.\n\n"
);

// === Tests ===

#[cfg(test)]
mod budget_handback_tests;
#[cfg(test)]
mod completion_usage_tests;
#[cfg(test)]
mod delivery_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod limits_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use tests::kimi_general_child_request_tools_fixture;

#[cfg(test)]
#[test]
fn configured_model_subagent_keeps_exact_id_and_negative_capability() {
    let _env = crate::test_support::lock_test_env();
    let mut runtime = tests::stub_runtime();
    let mut config = crate::config::Config {
        provider: Some("deepseek".into()),
        custom_models: Some(vec![
            toml::from_str(
                r#"
            provider = "deepseek"
            base_url = "https://api.deepseek.com"
            id = "deepseek-v4pro"
            modalities = { input = ["text"], output = ["text"] }
            tool_call = false
            limit = { context = 4096, output = 64 }
        "#,
            )
            .unwrap(),
        ]),
        ..crate::config::Config::default()
    }
    .with_legacy_root(Some("configured-model-local-fixture".into()), None);
    config
        .set_provider_base_url_override(
            &config.test_identity_for_kind(crate::config::ProviderKind::Deepseek),
            Some("https://api.deepseek.com".into()),
        )
        .unwrap();
    runtime.client = CodewhaleClient::new(&config).unwrap();
    config.custom_models.as_mut().unwrap()[0]
        .modalities
        .as_mut()
        .unwrap()
        .input
        .push("image".into());
    runtime.api_config = Some(std::sync::Arc::new(config));
    let mut selection = SpawnModelSelection {
        model_route: ModelRoute::Fixed("deepseek-v4pro".into()),
        source: SpawnRouteSource::TaskModel,
    };
    resolve_fixed_spawn_model_route(&runtime, &mut selection, true).unwrap();
    assert_eq!(
        selection.model_route,
        ModelRoute::Fixed("deepseek-v4pro".into())
    );
    let candidate = runtime
        .client
        .resolve_model_route("deepseek-v4pro")
        .unwrap();
    assert_eq!(
        candidate.capabilities().image_input,
        codewhale_config::route::CapabilityState::Unsupported
    );
    assert_eq!(
        candidate.capabilities().native_tool_calls,
        codewhale_config::route::CapabilityState::Unsupported
    );
    let member = crate::fleet::profile::AgentProfile {
        native_preset: None,
        id: "metadata-fixture".into(),
        display_name: None,
        description: None,
        requires: vec!["vision".into()],
        profile: codewhale_config::FleetProfile::default(),
        source: std::path::PathBuf::new(),
        origin: crate::fleet::profile::ProfileOrigin::Config,
        plugin_authority: None,
    };
    let error = enforce_fleet_member_route_requirements(Some(&member), &runtime, "deepseek-v4pro")
        .unwrap_err();
    assert!(error.to_string().contains("unsupported"));
}

#[cfg(test)]
#[tokio::test]
async fn configured_model_subagent_full_bind_preserves_task_profile_and_role_ids() {
    let _env = crate::test_support::lock_test_env();
    let workspace = tempfile::tempdir().unwrap();
    for (base, id) in [
        ("https://api.deepseek.com", "deepseek-v4pro"),
        ("https://models.example.test/v1", "preview-fixture-v9"),
    ] {
        let mut config = crate::config::Config {
            provider: Some("deepseek".into()),
            custom_models: Some(vec![
                toml::from_str(
                    r#"
                provider = "deepseek"
                base_url = "https://api.deepseek.com"
                id = "deepseek-v4pro"
                limit = { context = 96000, output = 8000 }
                cost = { input = 0.4, output = 1.6 }
                modalities = { input = ["text"], output = ["text"] }
            "#,
                )
                .unwrap(),
            ]),
            ..crate::config::Config::default()
        }
        .with_legacy_root(Some("configured-model-local-fixture".into()), None);
        config
            .set_provider_base_url_override(
                &config.test_identity_for_kind(crate::config::ProviderKind::Deepseek),
                Some(base.into()),
            )
            .unwrap();
        let declaration = &mut config.custom_models.as_mut().unwrap()[0];
        declaration.base_url = base.into();
        declaration.id = id.into();
        for source in ["task", "profile", "role", "default"] {
            let mut runtime = tests::stub_runtime();
            runtime.context = ToolContext::new(workspace.path().to_path_buf());
            runtime.client = CodewhaleClient::new(&config).unwrap();
            runtime.api_config = Some(std::sync::Arc::new(config.clone()));
            let mut member = crate::fleet::profile::AgentProfile {
                native_preset: None,
                id: "metadata-fixture".into(),
                display_name: None,
                description: None,
                requires: Vec::new(),
                profile: codewhale_config::FleetProfile::default(),
                source: std::path::PathBuf::new(),
                origin: crate::fleet::profile::ProfileOrigin::Config,
                plugin_authority: None,
            };
            let mut input = json!({"prompt": "fixture", "type": "reviewer"});
            match source {
                "task" => input["model"] = json!(id),
                "profile" => member.profile.model = Some(id.into()),
                "role" => {
                    std::sync::Arc::make_mut(runtime.api_config.as_mut().unwrap()).subagents = Some(
                        toml::from_str(&format!("[roles.reviewer]\nmodel = '{id}'\n")).unwrap(),
                    );
                }
                "default" => {
                    runtime.role_models.insert("reviewer".into(), id.into());
                }
                _ => unreachable!(),
            }
            let request = parse_spawn_request(&input).unwrap();
            let (route, _, _) = bind_spawn_model_route(
                &mut runtime,
                &request,
                (source == "profile").then_some(&member),
                true,
                true,
            )
            .await
            .unwrap();
            assert_eq!(route, ModelRoute::Fixed(id.into()), "{base} {source}");
            assert_eq!(runtime.model, id, "{source}");
            assert_eq!(
                runtime.client.route_limits().unwrap().context_tokens,
                Some(96000)
            );
            assert!(is_declared_subagent_model(&runtime.client, id));
            let envelope = runtime
                .client
                .effective_route_envelope(id, chrono::Utc::now());
            assert_eq!(envelope.model, id);
            let quote = serde_json::to_value(envelope.provider_live_pricing.unwrap()).unwrap();
            assert_eq!(quote["input_per_million"], "0.4");
            assert_eq!(quote["output_per_million"], "1.6");
        }
        config
            .set_provider_base_url_override(
                &config.test_identity_for_kind(crate::config::ProviderKind::Deepseek),
                Some("https://different.example.test/v1".into()),
            )
            .unwrap();
        let wrong_endpoint = CodewhaleClient::new(&config).unwrap();
        assert!(!is_declared_subagent_model(&wrong_endpoint, id));
        assert!(
            normalize_bound_subagent_model(id, "model", &wrong_endpoint)
                .map_or(true, |normalized| normalized != id)
        );
    }
}

#[cfg(test)]
mod declared_shortlist_tests {
    use super::*;
    use crate::config::{Config, ProviderKind};
    use crate::fleet::members::{FleetModelChange, add_fleet_model};

    fn runtime_for(config: &Config, workspace: &std::path::Path, current: &str) -> SubAgentRuntime {
        let mut config = config.clone();
        config.default_text_model = Some(current.into());
        config
            .set_provider_model_override(
                &config.test_identity_for_kind(ProviderKind::Deepseek),
                Some(current.into()),
            )
            .unwrap();
        let mut runtime = tests::stub_runtime();
        runtime.context = ToolContext::new(workspace.to_path_buf());
        runtime.client = CodewhaleClient::new(&config).unwrap();
        runtime.model = current.into();
        runtime.api_config = Some(Arc::new(config));
        runtime
    }

    #[test]
    fn configured_model_selected_pod_preserves_case_identity_and_saved_pins() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(exercise_selected_pod());
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn shortlisted_case_distinct_named_providers_remain_ambiguous() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
        let mut runtime = tests::stub_runtime();
        runtime.context = ToolContext::new(root.path().to_path_buf());
        for (provider, model, endpoint) in [
            ("TeamA", "model-x", "http://127.0.0.1:1/v1"),
            ("teama", "Model-X", "http://127.0.0.1:2/v1"),
        ] {
            Arc::make_mut(runtime.api_config.as_mut().unwrap())
                .providers
                .get_or_insert_with(Default::default)
                .custom
                .insert(
                    provider.into(),
                    crate::config::ProviderConfig {
                        kind: Some("openai-compatible".into()),
                        base_url: Some(endpoint.into()),
                        api_key: Some("fixture-key".into()),
                        model: Some(model.into()),
                        ..Default::default()
                    },
                );
            add_fleet_model(root.path(), provider, model, &[]).unwrap();
        }
        assert_eq!(
            crate::fleet::members::fleet_models(root.path())
                .unwrap()
                .len(),
            2
        );
        let original_model = runtime.model.clone();
        let original_endpoint = runtime.client.base_url().to_string();
        let request =
            parse_spawn_request(&json!({"prompt":"fixture", "type":"reviewer", "model":"MODEL-X"}))
                .unwrap();
        let error = bind_shortlisted_task_model(&mut runtime, &request)
            .expect_err("model aliases cannot choose between distinct named providers");
        assert!(error.to_string().contains("multiple providers"), "{error}");
        assert_eq!(runtime.model, original_model);
        assert_eq!(runtime.client.base_url(), original_endpoint);
    }

    async fn exercise_selected_pod() {
        let _env = crate::test_support::lock_test_env();
        let root = tempfile::tempdir().unwrap();
        let _home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("state"));
        let mut config: Config = toml::from_str(include_str!(
            "../../../../config/tests/fixtures/custom_models.toml"
        ))
        .unwrap();
        config
            .set_provider_api_key_override(
                &config.test_identity_for_kind(ProviderKind::Deepseek),
                Some("fixture-key".into()),
            )
            .unwrap();
        config
            .set_provider_base_url_override(
                &config.test_identity_for_kind(ProviderKind::Openrouter),
                Some("https://models.example.test/v1".into()),
            )
            .unwrap();
        let upper = "Preview-fixture";
        let lower = "preview-fixture";
        let declaration = config.custom_models.as_mut().unwrap().first_mut().unwrap();
        declaration.id = upper.into();
        let mut second = declaration.clone();
        second.id = lower.into();
        second.limit.as_mut().unwrap().context = Some(112000);
        second.cost.as_mut().unwrap().output = Some(2.0);
        config.custom_models.as_mut().unwrap().push(second);

        // Build the selected Fleet through the same mutation API the picker uses.
        for model in [upper, lower, "deepseek-v4-flash", "DeepSeek-V4-Flash"] {
            assert!(matches!(
                add_fleet_model(root.path(), "deepseek", model, &[]).unwrap(),
                FleetModelChange::Added { .. }
            ));
        }
        let models = crate::fleet::members::fleet_models(root.path()).unwrap();
        assert_eq!(models.len(), 4, "projection must preserve exact wire IDs");

        // Exercise both the session fast path and two shortlisted
        // declarations, with bare and provider-qualified selectors.
        for current in [upper, "deepseek-v4-pro"] {
            for (model, context, output_rate) in [(upper, 96000, "1.6"), (lower, 112000, "2")] {
                for requested in [model.to_string(), format!("deepseek/{model}")] {
                    let mut runtime = runtime_for(&config, root.path(), current);
                    // Matching must retain the client's frozen declarations.
                    Arc::make_mut(runtime.api_config.as_mut().unwrap()).custom_models = None;
                    let request = parse_spawn_request(
                        &json!({"prompt":"fixture", "type":"reviewer", "model":requested}),
                    )
                    .unwrap();
                    let (route, _, _) =
                        bind_spawn_model_route(&mut runtime, &request, None, true, true)
                            .await
                            .unwrap();
                    assert_eq!(route, ModelRoute::Fixed(model.into()));
                    assert_eq!(runtime.model, model);
                    assert_eq!(
                        runtime.client.route_limits().unwrap().context_tokens,
                        Some(context)
                    );
                    let envelope = runtime
                        .client
                        .effective_route_envelope(model, chrono::Utc::now());
                    assert_eq!(envelope.model, model);
                    let quote =
                        serde_json::to_value(envelope.provider_live_pricing.unwrap()).unwrap();
                    assert_eq!(quote["output_per_million"], output_rate);
                }
            }
        }

        // Profile and manual role pins must reject a different
        // declared ID even when its spelling differs only by case.
        for (source, pin_provider) in [
            ("profile", Some("deepseek")),
            ("profile", None),
            ("profile", Some("")),
            ("profile", Some(" ")),
            ("role", None),
        ] {
            for requested_model in [upper, lower] {
                for requested in [
                    requested_model.to_string(),
                    format!("deepseek/{requested_model}"),
                    format!("DEEPSEEK/{requested_model}"),
                ] {
                    let mut runtime = runtime_for(&config, root.path(), upper);
                    let member = crate::fleet::profile::AgentProfile {
                        native_preset: None,
                        id: "case-pin".into(),
                        display_name: None,
                        description: None,
                        requires: Vec::new(),
                        profile: codewhale_config::FleetProfile {
                            provider: pin_provider.map(str::to_string),
                            model: Some(upper.into()),
                            ..Default::default()
                        },
                        source: std::path::PathBuf::new(),
                        origin: crate::fleet::profile::ProfileOrigin::Config,
                        plugin_authority: None,
                    };
                    if source == "role" {
                        Arc::make_mut(runtime.api_config.as_mut().unwrap()).subagents = Some(
                            toml::from_str(&format!("[roles.reviewer]\nmodel = '{upper}'\n"))
                                .unwrap(),
                        );
                    }
                    let request = parse_spawn_request(
                        &json!({"prompt":"fixture", "type":"reviewer", "model":requested}),
                    )
                    .unwrap();
                    let result = bind_spawn_model_route(
                        &mut runtime,
                        &request,
                        (source == "profile").then_some(&member),
                        true,
                        true,
                    )
                    .await;
                    if requested_model == upper {
                        assert_eq!(result.unwrap().0, ModelRoute::Fixed(upper.into()));
                    } else {
                        assert!(
                            result
                                .unwrap_err()
                                .to_string()
                                .contains("conflicts with that route")
                        );
                    }
                    assert_eq!(runtime.model, upper);
                }
            }
        }

        let mut runtime = runtime_for(&config, root.path(), upper);
        let request = parse_spawn_request(
            &json!({"prompt":"fixture", "type":"reviewer", "model":"PREVIEW-FIXTURE"}),
        )
        .unwrap();
        assert!(
            bind_spawn_model_route(&mut runtime, &request, None, true, true)
                .await
                .unwrap_err()
                .to_string()
                .contains("outside the selected Fleet")
        );
        assert!(
            !declared_spawn_model_for_provider(&runtime, "openrouter", lower),
            "same endpoint on another provider cannot inherit declarations"
        );

        // An endpoint mismatch cannot inherit declarations or their
        // limits. The provider's ordinary admission rules still apply.
        let mut wrong = config.clone();
        wrong
            .set_provider_base_url_override(
                &wrong.test_identity_for_kind(ProviderKind::Deepseek),
                Some("https://other.example.test/v1".into()),
            )
            .unwrap();
        let mut runtime = runtime_for(&wrong, root.path(), "deepseek-v4-pro");
        assert!(!declared_spawn_model_for_provider(
            &runtime, "deepseek", lower
        ));
        let request =
            parse_spawn_request(&json!({"prompt":"fixture", "type":"reviewer", "model":lower}))
                .unwrap();
        assert!(
            bind_spawn_model_route(&mut runtime, &request, None, true, true)
                .await
                .is_err()
        );

        // Duplicate ordinary aliases still allow convenient casing.
        let mut runtime = runtime_for(&config, root.path(), "deepseek-v4-pro");
        let request = parse_spawn_request(
            &json!({"prompt":"fixture", "type":"reviewer", "model":"DEEPSEEK-V4-FLASH"}),
        )
        .unwrap();
        assert_eq!(
            bind_spawn_model_route(&mut runtime, &request, None, true, true)
                .await
                .unwrap()
                .0,
            ModelRoute::Fixed("deepseek-v4-flash".into())
        );
    }
}

#[cfg(test)]
#[test]
fn parse_spawn_request_reads_max_output_tokens_and_rejects_zero_and_excessive() {
    // Valid value round-trips.
    let req = parse_spawn_request(&json!({
        "prompt": "test",
        "max_output_tokens": 500
    }))
    .unwrap();
    assert_eq!(req.max_output_tokens, Some(500));

    // Zero is rejected (bounded to 1..=u32::MAX).
    let err = parse_spawn_request(&json!({
        "prompt": "test",
        "max_output_tokens": 0
    }))
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("max_output_tokens"),
        "expected rejection of zero; got: {msg}"
    );

    // Value exceeding u32::MAX is rejected.
    let err = parse_spawn_request(&json!({
        "prompt": "test",
        "max_output_tokens": 5_000_000_000u64
    }))
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("max_output_tokens"),
        "expected rejection of excessive; got: {msg}"
    );
}

#[cfg(test)]
mod owner_scoped_coordination_tests {
    use super::*;

    fn admit(manager: &mut SubAgentManager, lexical: &Path) {
        let (canonical, file) = crate::runtime_api::open_workspace_directory(lexical).unwrap();
        manager
            .admit_coordination_workspace(lexical.to_path_buf(), canonical, Arc::new(file))
            .unwrap();
    }

    pub(super) fn writer(id: &str, workspace: &Path, root: &str) -> AgentWorkerSpec {
        let mut spec = super::tests::make_worker_spec(id, workspace.to_path_buf());
        spec.agent_type = FleetRole::Builder;
        spec.runtime_profile = WorkerRuntimeProfile::for_role(FleetRole::Builder);
        spec.launch_manifest = Some(ChildLaunchManifest {
            owner_session: "owner".into(),
            child_id: id.into(),
            profile: spec.runtime_profile.clone(),
            prompt: spec.objective.clone(),
            cwd: Some(workspace.display().to_string()),
            worktree: false,
            writable_roots: vec![root.into()],
            writable_files: Vec::new(),
            coordination_contracts: Vec::new(),
            expected_artifact: None,
            deliverables: Vec::new(),
            resume_identity: Some(id.into()),
            generation: 1,
            resume_from_agent_id: None,
        });
        spec
    }

    #[test]
    fn one_actor_keeps_original_claim_bytes_and_admits_disjoint_owner_roots() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let original = root.join("original");
        let selected = root.join("selected");
        fs::create_dir_all(original.join("src")).unwrap();
        fs::create_dir_all(selected.join("src")).unwrap();
        let authority = new_shared_runtime_subagent_manager(original.clone(), 2).unwrap();
        let mut manager = authority.try_write().unwrap();
        admit(&mut manager, &original);
        admit(&mut manager, &selected);
        let independent =
            SubAgentManager::new(selected.clone(), 2).require_coordination_process_lock();
        assert!(independent.ensure_coordination_process_lock().is_err());
        assert!(independent.persist_state_synchronously().is_err());
        manager
            .preflight_worker_coordination(&writer("original", &original, "src"))
            .unwrap();
        manager
            .register_worker_with_coordination(writer("original", &original, "src"))
            .unwrap();
        assert_eq!(manager.coordination.schema_version, 1);
        assert!(manager.coordination.claim_scopes.is_none());
        manager
            .preflight_worker_coordination(&writer("selected", &selected, "src"))
            .unwrap();
        manager
            .register_worker_with_coordination(writer("selected", &selected, "src"))
            .unwrap();
        assert_eq!(manager.coordination.schema_version, 2);
        assert_eq!(manager.coordination.write_claims.len(), 2);
        assert_eq!(manager.max_agents, 2);
        assert_eq!(manager.state_root, original);
        manager
            .validate_write_scope("selected", &["src/output.txt".into()])
            .unwrap();
        assert!(
            manager
                .validate_write_scope("selected", &["outside.txt".into()])
                .is_err()
        );
        assert!(
            manager
                .coordination
                .write_claims
                .iter()
                .all(|claim| claim.claim.roots == ["src"])
        );
    }

    #[test]
    fn restored_attribution_requires_matching_held_owner_readmission() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let original = root.join("original");
        let selected = root.join("selected");
        fs::create_dir_all(&original).unwrap();
        fs::create_dir_all(&selected).unwrap();
        let mut old = SubAgentManager::new(original.clone(), 2);
        admit(&mut old, &original);
        admit(&mut old, &selected);
        old.register_worker_with_coordination(writer("selected", &selected, "."))
            .unwrap();
        let encoded = serde_json::to_vec(&old.coordination).unwrap();
        let mut next = SubAgentManager::new(original.clone(), 2);
        next.coordination = serde_json::from_slice(&encoded).unwrap();
        next.coordination.validate_replay().unwrap();
        next.worker_records = old.worker_records.clone();
        next.worker_origins = old.worker_origins.clone();
        next.worker_origin_schema = old.worker_origin_schema;
        assert!(
            next.validate_write_scope("selected", &["output.txt".into()])
                .is_err()
        );
        admit(&mut next, &original);
        admit(&mut next, &selected);
        next.validate_write_scope("selected", &["output.txt".into()])
            .unwrap();
        fs::rename(&selected, root.join("retired")).unwrap();
        fs::create_dir(&selected).unwrap();
        assert!(
            next.validate_write_scope("selected", &["output.txt".into()])
                .unwrap_err()
                .contains("identity changed")
        );
        assert!(
            next.expand_write_claim("selected", vec!["new".into()], Vec::new(), Vec::new())
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn aliased_and_nested_selected_roots_cannot_evade_file_overlap() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let original = root.join("original");
        let selected = root.join("selected");
        fs::create_dir_all(&original).unwrap();
        fs::create_dir_all(selected.join("src")).unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&selected, &alias).unwrap();
        let mut manager = SubAgentManager::new(original.clone(), 3);
        admit(&mut manager, &original);
        admit(&mut manager, &selected);
        admit(&mut manager, &alias);
        admit(&mut manager, &selected.join("src"));
        manager
            .register_worker_with_coordination(writer("first", &selected, "src"))
            .unwrap();
        assert!(
            manager
                .preflight_worker_coordination(&writer("alias", &alias, "src"))
                .is_err()
        );
        assert!(
            manager
                .preflight_worker_coordination(&writer("nested", &selected.join("src"), "."))
                .is_err()
        );
        assert_eq!(manager.coordination.write_claims.len(), 1);
    }
}

#[cfg(test)]
mod captured_worker_origin_tests {
    use super::*;

    fn roots() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let original = root.join("original");
        let selected = root.join("selected");
        let execution = root.join("isolated-execution");
        for path in [&original, &selected, &execution] {
            fs::create_dir_all(path.join("src")).unwrap();
        }
        (temporary, original, selected, execution)
    }
    fn admit(manager: &mut SubAgentManager, workspace: &Path) {
        let (canonical, file) = crate::runtime_api::open_workspace_directory(workspace).unwrap();
        manager
            .admit_coordination_workspace(workspace.to_path_buf(), canonical, Arc::new(file))
            .unwrap();
    }
    fn isolated_writer(id: &str, workspace: &Path) -> AgentWorkerSpec {
        let mut worker = super::owner_scoped_coordination_tests::writer(id, workspace, "src");
        worker.launch_manifest.as_mut().unwrap().worktree = true;
        worker
    }
    fn payload(manager: &SubAgentManager) -> PersistedSubAgentState {
        PersistedSubAgentState {
            schema_version: manager.worker_origin_schema,
            workers: manager.sorted_worker_records(),
            coordination: manager.coordination.clone(),
            worker_origins: (manager.worker_origin_schema == 2)
                .then(|| manager.worker_origins.clone()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn legacy_resume_preserves_saved_origin_only_for_verified_original_owner() {
        let (_temporary, original, selected, execution) = roots();
        let mut manager = SubAgentManager::new(original.clone(), 2);
        admit(&mut manager, &original);
        admit(&mut manager, &selected);
        let source = manager
            .insert_test_running_direct_child_in_origin(
                "legacy",
                &execution,
                &original,
                "owner-session",
            )
            .unwrap();
        manager
            .cancel_agent_for_session("owner-session", &source)
            .unwrap();
        let legacy = WorkerOrigin::legacy("owner-session".into());
        manager
            .worker_origins
            .insert(source.clone(), legacy.clone());
        let incoming = manager
            .capture_direct_worker_origin(&original, "owner-session")
            .unwrap();
        assert_ne!(legacy, incoming);
        assert_eq!(
            manager
                .continuation_worker_origin(&source, "owner-session", None, &incoming)
                .unwrap(),
            legacy
        );
        let another = manager
            .capture_direct_worker_origin(&selected, "owner-session")
            .unwrap();
        assert!(
            manager
                .continuation_worker_origin(&source, "owner-session", None, &another)
                .is_err()
        );
        assert!(
            manager
                .continuation_worker_origin(&source, "foreign-session", None, &incoming)
                .is_err()
        );
        assert!(
            manager
                .continuation_worker_origin(
                    &source,
                    "owner-session",
                    Some("absent-parent"),
                    &incoming
                )
                .is_err()
        );
        manager.admitted_coordination_roots.remove(&original);
        assert!(
            manager
                .continuation_worker_origin(&source, "owner-session", None, &incoming)
                .is_err()
        );
        admit(&mut manager, &original);
        fs::rename(&original, original.with_extension("replaced")).unwrap();
        fs::create_dir(&original).unwrap();
        assert!(
            manager
                .continuation_worker_origin(&source, "owner-session", None, &incoming)
                .is_err()
        );
    }

    #[test]
    fn captured_origin_survives_isolated_execution_and_exact_registration_rollback() {
        let (_temporary, original, selected, execution) = roots();
        let mut manager = SubAgentManager::new(original.clone(), 2);
        admit(&mut manager, &original);
        admit(&mut manager, &selected);
        let origin = manager.capture_fleet_worker_origin(&selected).unwrap();
        let before = manager.coordination_registration_snapshot();
        let spec = isolated_writer("isolated", &execution);
        manager
            .preflight_worker_coordination_in_origin(&spec, &origin)
            .unwrap();
        manager
            .register_worker_with_coordination_in_origin(spec, origin.clone())
            .unwrap();
        assert_eq!(manager.worker_records["isolated"].spec.workspace, execution);
        assert_eq!(manager.worker_origins["isolated"], origin);
        assert!(
            manager
                .worker_records_for_workspace(&original)
                .unwrap()
                .is_empty()
        );
        let records = manager.worker_records_for_workspace(&selected).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].1, "Fleet membership remains required");
        payload(&manager).validate_worker_origins().unwrap();
        manager
            .restore_coordination_registration_snapshot(before)
            .unwrap();
        assert!(manager.worker_records.is_empty() && manager.worker_origins.is_empty());
        assert_eq!(
            manager.worker_origin_schema, 1,
            "uncommitted admission rollback restores original schema"
        );
    }

    #[test]
    fn durable_origin_schema_requires_complete_matching_map_before_state_install() {
        let (_temporary, original, selected, execution) = roots();
        let mut manager = SubAgentManager::new(original.clone(), 2);
        admit(&mut manager, &original);
        admit(&mut manager, &selected);
        let origin = manager.capture_fleet_worker_origin(&selected).unwrap();
        manager
            .register_worker_with_coordination_in_origin(
                isolated_writer("saved", &execution),
                origin,
            )
            .unwrap();
        let valid = payload(&manager);
        valid.validate_worker_origins().unwrap();
        let encoded = serde_json::to_vec(&valid).unwrap();
        let decoded: PersistedSubAgentState = serde_json::from_slice(&encoded).unwrap();
        decoded.validate_worker_origins().unwrap();
        let mut bad = decoded.clone();
        bad.worker_origins = None;
        assert!(bad.validate_worker_origins().is_err());
        bad = decoded.clone();
        bad.worker_origins
            .as_mut()
            .unwrap()
            .get_mut("saved")
            .unwrap()
            .owner_session_id = "foreign".into();
        assert!(bad.validate_worker_origins().is_err());
        bad = decoded.clone();
        let orphan = bad.worker_origins.as_ref().unwrap()["saved"].clone();
        bad.worker_origins
            .as_mut()
            .unwrap()
            .insert("orphan".into(), orphan);
        assert!(bad.validate_worker_origins().is_err());
        bad = decoded;
        bad.schema_version = 1;
        assert!(
            bad.validate_worker_origins().is_err(),
            "schema1 never silently consumes schema2 origins"
        );
        let saved_path = original.join("captured-origin-state.json");
        let mut receiver = SubAgentManager::new(original.clone(), 2);
        receiver.state_path = Some(saved_path.clone());
        receiver.worker_records = manager.worker_records.clone();
        receiver.worker_origins = manager.worker_origins.clone();
        receiver.worker_origin_schema = 2;
        let retained = receiver.worker_origins.clone();
        let mut invalid = valid.clone();
        invalid.worker_origins = None;
        let invalid_bytes = serde_json::to_vec(&invalid).unwrap();
        fs::write(&saved_path, &invalid_bytes).unwrap();
        assert!(receiver.load_state().is_err());
        assert_eq!(
            receiver.worker_origins, retained,
            "failed load must not install incomplete state"
        );
        assert_eq!(
            fs::read(&saved_path).unwrap(),
            invalid_bytes,
            "unsupported history stays recoverable"
        );
        fs::write(&saved_path, &encoded).unwrap();
        receiver.load_state().unwrap();
        assert_eq!(receiver.worker_origin_schema, 2);
        assert_eq!(receiver.worker_origins, retained);
        assert!(
            receiver.validate_worker_origin(&retained["saved"]).is_err(),
            "disk load alone never provides a held owner admission"
        );
        admit(&mut receiver, &original);
        admit(&mut receiver, &selected);
        receiver.validate_worker_origin(&retained["saved"]).unwrap();
        let old = serde_json::to_value(PersistedSubAgentState::default()).unwrap();
        assert!(old.get("worker_origins").is_none());
        manager.worker_records.clear();
        manager.prune_worker_origins();
        let retired = payload(&manager);
        assert_eq!(retired.schema_version, 2);
        assert!(retired.worker_origins.as_ref().unwrap().is_empty());
        retired.validate_worker_origins().unwrap();
    }

    #[tokio::test]
    async fn direct_origin_survives_execution_cwd_and_owner_session_refuses_cross_control() {
        let (_temporary, original, selected, execution) = roots();
        let mut manager = SubAgentManager::new(original.clone(), 2);
        admit(&mut manager, &original);
        admit(&mut manager, &selected);
        let id = manager
            .insert_test_running_direct_child_in_origin(
                "direct",
                &execution,
                &selected,
                "owner-session",
            )
            .unwrap();
        assert!(
            manager
                .worker_records_for_workspace(&original)
                .unwrap()
                .is_empty()
        );
        let nested_origin = manager
            .capture_direct_worker_origin_for_runtime(&execution, "owner-session", Some(&id))
            .unwrap();
        assert_eq!(nested_origin, manager.worker_origins[&id]);
        assert!(
            manager
                .capture_direct_worker_origin_for_runtime(&execution, "foreign-session", Some(&id))
                .is_err()
        );
        assert!(
            manager
                .capture_direct_worker_origin_for_runtime(
                    &execution,
                    "owner-session",
                    Some("absent")
                )
                .is_err()
        );
        let rows = manager.worker_records_for_workspace(&selected).unwrap();
        assert_eq!(rows[0].0.spec.workspace, execution);
        assert!(!rows[0].1);
        assert!(
            manager
                .cancel_agent_for_session("foreign-session", &id)
                .is_err()
        );
        manager
            .cancel_agent_for_session("owner-session", &id)
            .unwrap();
        assert!(
            manager.worker_records_for_workspace(&selected).unwrap()[0]
                .0
                .status
                .is_terminal()
        );
        assert!(
            manager
                .capture_direct_worker_origin_for_runtime(&execution, "owner-session", Some(&id))
                .is_err(),
            "settled parent cannot admit another nested worker"
        );
        assert_eq!(
            manager.worker_origins[&id].owner_session_id,
            "owner-session"
        );
        fs::rename(&selected, selected.with_extension("retired")).unwrap();
        fs::create_dir(&selected).unwrap();
        assert!(
            manager
                .worker_records_for_workspace(&selected)
                .unwrap_err()
                .contains("identity changed")
        );
    }

    #[cfg(unix)]
    #[test]
    fn alias_origin_is_captured_before_canonical_execution_and_restart_requires_readmission() {
        let (temporary, original, selected, _execution) = roots();
        let alias = temporary.path().canonicalize().unwrap().join("alias");
        std::os::unix::fs::symlink(&selected, &alias).unwrap();
        let mut manager = SubAgentManager::new(original.clone(), 2);
        admit(&mut manager, &original);
        admit(&mut manager, &alias);
        let origin = manager.capture_fleet_worker_origin(&alias).unwrap();
        let canonical_cwd = alias.join("src").canonicalize().unwrap();
        let spec =
            super::owner_scoped_coordination_tests::writer("alias-worker", &canonical_cwd, ".");
        manager
            .register_worker_with_coordination_in_origin(spec, origin.clone())
            .unwrap();
        assert_eq!(
            manager.worker_origins["alias-worker"]
                .selected_workspace
                .as_ref(),
            Some(&alias)
        );
        let saved = payload(&manager);
        let mut next = SubAgentManager::new(original.clone(), 2);
        next.worker_records = saved
            .workers
            .into_iter()
            .map(|worker| (worker.spec.worker_id.clone(), worker))
            .collect();
        next.worker_origins = saved.worker_origins.unwrap();
        next.worker_origin_schema = 2;
        assert!(next.validate_worker_origin(&origin).is_err());
        admit(&mut next, &original);
        admit(&mut next, &alias);
        next.validate_worker_origin(&origin).unwrap();
        assert_eq!(next.worker_records_for_workspace(&alias).unwrap().len(), 1);
    }

    #[test]
    fn path_decisions_use_physical_origin_while_named_contracts_remain_global() {
        let (_temporary, original, selected, _execution) = roots();
        let mut manager = SubAgentManager::new(original.clone(), 2);
        admit(&mut manager, &original);
        admit(&mut manager, &selected);
        for (id, root, scope) in [
            ("original-path", &original, "path:src"),
            ("selected-path", &selected, "path:src"),
            ("global-contract", &original, "contract:public-api"),
        ] {
            manager
                .record_coordination_decision_in_workspace(
                    DecisionRecord {
                        decision_id: id.into(),
                        subject: id.into(),
                        status: DecisionStatus::Accepted,
                        owner: "root".into(),
                        scope: vec![scope.into()],
                        constraints: vec![id.into()],
                        evidence_handles: Vec::new(),
                        version: 1,
                        sequence: 0,
                    },
                    root,
                )
                .unwrap();
        }
        let mut writer =
            super::owner_scoped_coordination_tests::writer("selected-writer", &selected, "src");
        writer
            .launch_manifest
            .as_mut()
            .unwrap()
            .coordination_contracts
            .push("public-api".into());
        let origin = manager.capture_fleet_worker_origin(&selected).unwrap();
        manager
            .register_worker_with_coordination_in_origin(writer, origin)
            .unwrap();
        let objective = &manager.worker_records["selected-writer"].spec.objective;
        assert!(objective.contains("selected-path") && objective.contains("global-contract"));
        assert!(!objective.contains("original-path"));
        let decision = manager
            .coordination
            .decisions
            .iter()
            .find(|decision| decision.decision_id == "selected-path")
            .unwrap()
            .clone();
        let before = manager.coordination.claim_scope(decision.sequence).cloned();
        let after = manager
            .update_coordination_decision(
                "selected-path",
                DecisionStatus::Superseded,
                "root",
                decision.version,
            )
            .unwrap();
        assert_eq!(
            manager.coordination.claim_scope(after.sequence),
            before.as_ref()
        );
        assert!(
            manager
                .coordination
                .claim_scope(decision.sequence)
                .is_none()
        );
        manager.coordination.validate_replay().unwrap();
    }
}

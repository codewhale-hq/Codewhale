//! Sub-agent concurrency/timeout limits and their clamp resolvers.
//!
//! Pure numeric/string limit constants plus the two private clamp helpers that
//! operate solely on them. Extracted verbatim from `config.rs`; the constants
//! are re-exported via `pub use subagent_limits::*;` (preserving each item's
//! `pub`/`pub(crate)` visibility) and the resolvers are pulled back into
//! `config.rs` with a private `use`, so no new external surface is created
//! (#3311).

/// Temporary high-throughput default while the shared-context cutover makes
/// agent fanout cheap. This should eventually be governed by API/backpressure
/// budgets rather than memory-driven count throttles.
pub const DEFAULT_MAX_SUBAGENTS: usize = 64;
/// User-configurable ceiling for concurrent sub-agent execution. Keep this
/// above the default so operators can opt into larger API-bound fanout without
/// code changes while the full resource budget gate lands.
pub const MAX_SUBAGENTS: usize = 128;
/// Upper bound for queued + running sub-agent admissions. This is deliberately
/// higher than the instantaneous concurrency cap so Workflow-style fanout can
/// opt into large bounded populations without unbounded queue growth.
pub const MAX_SUBAGENT_ADMISSION: usize = 1024;
/// Default per-step DeepSeek API timeout for sub-agent requests, in seconds.
/// Raised from the legacy 120s: a live-but-slow reasoning call routinely
/// outlasts two minutes, and a timed-out attempt is now retried with backoff
/// before the step interrupts, so the default should only trip on genuinely
/// stuck calls (FINISH-0.9.4 entry #40). Applies when `[subagents]
/// api_timeout_secs` is unset (#1806, #1808).
pub const DEFAULT_SUBAGENT_API_TIMEOUT_SECS: u64 = 600;
/// Minimum accepted `[subagents] api_timeout_secs`. Anything lower (including
/// `0`, which would otherwise produce an immediate timeout footgun) clamps
/// up to this value before the runtime sees it.
pub const MIN_SUBAGENT_API_TIMEOUT_SECS: u64 = 1;
/// Maximum accepted `[subagents] api_timeout_secs` (60 minutes). The cap
/// keeps a misconfigured per-step timeout from masking real model/network
/// hangs forever.
pub const MAX_SUBAGENT_API_TIMEOUT_SECS: u64 = 3600;
/// Default wall-clock budget for a single sub-agent tool execution, in
/// seconds. This is the single source of truth for the default:
/// `tools::subagent::DEFAULT_TOOL_TIMEOUT` derives from it, so the heartbeat
/// floor below and the timeout actually applied to a running tool can never
/// drift apart.
pub const DEFAULT_SUBAGENT_TOOL_TIMEOUT_SECS: u64 = 300;
/// Default wall-clock interval without manager-visible sub-agent progress
/// before a running child can be auto-cancelled to release its slot (#2614).
pub const DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS: u64 = 300;
/// Minimum accepted `[subagents] heartbeat_timeout_secs`.
pub const MIN_SUBAGENT_HEARTBEAT_TIMEOUT_SECS: u64 = 30;
/// Maximum accepted `[subagents] heartbeat_timeout_secs` (1 hour).
pub const MAX_SUBAGENT_HEARTBEAT_TIMEOUT_SECS: u64 = 3600;
/// Default per-SSE-chunk idle timeout, in seconds.
pub const DEFAULT_STREAM_CHUNK_TIMEOUT_SECS: u64 = 900;
/// Minimum accepted stream chunk timeout.
pub const MIN_STREAM_CHUNK_TIMEOUT_SECS: u64 = 1;
/// Maximum accepted stream chunk timeout.
pub const MAX_STREAM_CHUNK_TIMEOUT_SECS: u64 = 3600;
/// Default TCP/TLS connect timeout for the model HTTP client, in seconds.
pub const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 30;
/// Minimum accepted connect timeout.
pub const MIN_CONNECT_TIMEOUT_SECS: u64 = 1;
/// Maximum accepted connect timeout.
pub const MAX_CONNECT_TIMEOUT_SECS: u64 = 300;
pub(crate) const STREAM_CHUNK_TIMEOUT_ENV: &str = "CODEWHALE_STREAM_IDLE_TIMEOUT_SECS";
/// Legacy alias for [`STREAM_CHUNK_TIMEOUT_ENV`].
pub(crate) const LEGACY_STREAM_CHUNK_TIMEOUT_ENV: &str = "DEEPSEEK_STREAM_IDLE_TIMEOUT_SECS";
/// R1: env override for `[tui].max_model_steps`.
pub(crate) const MAX_MODEL_STEPS_ENV: &str = "CODEWHALE_MAX_MODEL_STEPS";
/// R1: env override for `[tui].turn_wall_clock_secs`.
pub(crate) const TURN_WALL_CLOCK_ENV: &str = "CODEWHALE_TURN_WALL_CLOCK_SECS";

pub(crate) fn resolve_subagent_api_timeout_secs(raw: Option<u64>) -> u64 {
    let raw = raw.unwrap_or(DEFAULT_SUBAGENT_API_TIMEOUT_SECS);
    if raw == 0 {
        return DEFAULT_SUBAGENT_API_TIMEOUT_SECS;
    }
    raw.clamp(MIN_SUBAGENT_API_TIMEOUT_SECS, MAX_SUBAGENT_API_TIMEOUT_SECS)
}

pub(crate) fn resolve_subagent_heartbeat_timeout_secs(
    raw: Option<u64>,
    api_timeout_secs: u64,
    tool_timeout_secs: u64,
) -> u64 {
    let raw = raw.unwrap_or(DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS);
    let configured = if raw == 0 {
        DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS
    } else {
        raw.clamp(
            MIN_SUBAGENT_HEARTBEAT_TIMEOUT_SECS,
            MAX_SUBAGENT_HEARTBEAT_TIMEOUT_SECS,
        )
    };
    let min_for_api = api_timeout_secs.saturating_add(30).clamp(
        MIN_SUBAGENT_HEARTBEAT_TIMEOUT_SECS,
        MAX_SUBAGENT_HEARTBEAT_TIMEOUT_SECS,
    );
    // A single tool execution may legitimately run up to `tool_timeout_secs`
    // without the child touching its progress heartbeat (activity is recorded
    // at step boundaries, not mid-tool), so the floor must also sit above the
    // tool timeout. Deriving it from `api_timeout_secs` alone let a low
    // `[subagents] api_timeout_secs` pull the floor under a long tool, and
    // cleanup then killed a legitimately-working child (2026-08-04 sub-agent
    // hunt, finding 4).
    let min_for_tool = tool_timeout_secs.saturating_add(30).clamp(
        MIN_SUBAGENT_HEARTBEAT_TIMEOUT_SECS,
        MAX_SUBAGENT_HEARTBEAT_TIMEOUT_SECS,
    );
    configured.max(min_for_api).max(min_for_tool)
}

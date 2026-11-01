//! Process-wide session accumulators.
//!
//! Atomics rather than a lock, because every bump happens on a hot path and
//! must cost nothing measurable. Every bump also happens at a **call site**,
//! never inside a conditionally-entered handler: a counter sited above an early
//! return is a landmine, and the natural future optimization — hoisting the
//! guard to the caller — silently zeroes it for exactly the users who do not
//! use that feature.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::event::{Counters, Errors, Event, ModelCounters, TurnWall};

/// One session's accumulated counts.
#[derive(Debug, Default)]
pub struct SessionCounters {
    turns: AtomicU32,
    tool_calls: AtomicU32,
    fleet_dispatch: AtomicU32,
    workflow_run: AtomicU32,
    subagent_spawn: AtomicU32,
    mcp_server_connected: AtomicU32,
    memory_search: AtomicU32,
    approval_modal_shown: AtomicU32,
    approval_auto_allowed: AtomicU32,
    command_palette_open: AtomicU32,

    auth_preflight_failed: AtomicU32,
    provider_http_4xx: AtomicU32,
    provider_http_5xx: AtomicU32,
    tool_denied_by_policy: AtomicU32,
    tool_timeout: AtomicU32,
    network_error: AtomicU32,

    turn_wall: Mutex<TurnWall>,
    /// Sorted, deduplicated `ProviderKind::as_str()` values. `&'static str`
    /// only: a `String` here would be the seam through which a customer's
    /// `[providers.<name>]` table key reaches the wire.
    providers: Mutex<Vec<&'static str>>,
    models: Mutex<Vec<ModelSlot>>,
    tools: Mutex<Vec<(&'static str, u32, u32)>>,
}

#[derive(Debug)]
struct ModelSlot {
    provider: &'static str,
    model: String,
    counters: ModelCounters,
    turn_wall: TurnWall,
}

/// At most this many distinct provider and model pairs per session; later
/// pairs are folded into the provider's `other` row.
const MAX_MODEL_ROWS: usize = 16;

/// How one model turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    /// Completed.
    Ok,
    /// Failed.
    Failed,
    /// Interrupted by the user or the runtime.
    Interrupted,
}

/// Token counts for one model turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnTokens {
    /// Input tokens, including cache reads.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write: u64,
}

/// Which counter to bump. A closed enum so a call site cannot invent a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counter {
    /// A model turn completed.
    Turns,
    /// A tool call was executed.
    ToolCalls,
    /// A fleet dispatch started.
    FleetDispatch,
    /// A `workflow_run` action was parsed and executed.
    WorkflowRun,
    /// A sub-agent was spawned.
    SubagentSpawn,
    /// An MCP server reached `connected`.
    McpServerConnected,
    /// A native-memory search ran.
    MemorySearch,
    /// An approval modal was shown.
    ApprovalModalShown,
    /// An approval was granted by an auto-allow rule.
    ApprovalAutoAllowed,
    /// The command palette was opened.
    CommandPaletteOpen,
}

/// Which error counter to bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCounter {
    /// Credential preflight rejected the route.
    AuthPreflightFailed,
    /// Provider responded 4xx.
    ProviderHttp4xx,
    /// Provider responded 5xx.
    ProviderHttp5xx,
    /// A tool call was denied by policy.
    ToolDeniedByPolicy,
    /// A tool call timed out.
    ToolTimeout,
    /// A request failed below HTTP.
    NetworkError,
}

impl SessionCounters {
    /// Bump a feature counter by one.
    pub fn bump(&self, counter: Counter) {
        self.slot(counter).fetch_add(1, Ordering::Relaxed);
    }

    /// Bump an error counter by one.
    pub fn bump_error(&self, counter: ErrorCounter) {
        self.error_slot(counter).fetch_add(1, Ordering::Relaxed);
    }

    /// Record one turn's wall-clock time in the histogram.
    pub fn observe_turn_secs(&self, secs: u64) {
        if let Ok(mut wall) = self.turn_wall.lock() {
            wall.observe_secs(secs);
        }
    }

    /// Record that a provider was routed to.
    ///
    /// Takes a `ProviderKind` **by value**, never a `&str`: the four
    /// persistence and label accessors that look like the natural seam all
    /// return the customer's own `[providers.<name>]` table key when the route
    /// is custom, and `/status` already prints it. `ProviderKind::Custom`
    /// yields the literal `"custom"` and nothing else.
    pub fn record_provider(&self, provider: codewhale_config::ProviderKind) {
        let name = provider.as_str();
        if let Ok(mut providers) = self.providers.lock()
            && !providers.contains(&name)
        {
            providers.push(name);
            providers.sort_unstable();
        }
    }

    /// Record one model turn: its provider, model, outcome, tokens and wall time.
    pub fn record_model_turn(
        &self,
        provider: codewhale_config::ProviderKind,
        model: &str,
        outcome: TurnOutcome,
        tokens: TurnTokens,
        secs: u64,
    ) {
        let provider = provider.as_str();
        let mut model = crate::event::reported_model_id(model);
        let Ok(mut models) = self.models.lock() else {
            return;
        };
        let known = models
            .iter()
            .any(|slot| slot.provider == provider && slot.model == model);
        if !known && models.len() >= MAX_MODEL_ROWS {
            model = "other".to_string();
        }
        let index = match models
            .iter()
            .position(|slot| slot.provider == provider && slot.model == model)
        {
            Some(index) => index,
            None => {
                models.push(ModelSlot {
                    provider,
                    model,
                    counters: ModelCounters::default(),
                    turn_wall: TurnWall::default(),
                });
                models.len() - 1
            }
        };
        let slot = &mut models[index];
        let counters = &mut slot.counters;
        match outcome {
            TurnOutcome::Ok => counters.turns_ok = counters.turns_ok.saturating_add(1),
            TurnOutcome::Failed => counters.turns_failed = counters.turns_failed.saturating_add(1),
            TurnOutcome::Interrupted => {
                counters.turns_interrupted = counters.turns_interrupted.saturating_add(1);
            }
        }
        counters.input_tokens = counters.input_tokens.saturating_add(tokens.input);
        counters.output_tokens = counters.output_tokens.saturating_add(tokens.output);
        counters.cache_read_tokens = counters.cache_read_tokens.saturating_add(tokens.cache_read);
        counters.cache_write_tokens = counters
            .cache_write_tokens
            .saturating_add(tokens.cache_write);
        slot.turn_wall.observe_secs(secs);
    }

    /// Record one tool call by name. `is_mcp` marks an MCP pool tool.
    pub fn record_tool_call(&self, name: &str, is_mcp: bool, failed: bool) {
        let tool = crate::event::reported_tool_name(name, is_mcp);
        let Ok(mut tools) = self.tools.lock() else {
            return;
        };
        let index = match tools.iter().position(|(known, _, _)| *known == tool) {
            Some(index) => index,
            None => {
                tools.push((tool, 0, 0));
                tools.len() - 1
            }
        };
        let row = &mut tools[index];
        row.1 = row.1.saturating_add(1);
        if failed {
            row.2 = row.2.saturating_add(1);
        }
    }

    /// The `model_usage` and `tool_usage` events for this session so far.
    #[must_use]
    pub fn usage_events(&self) -> Vec<Event> {
        let mut events = Vec::new();
        if let Ok(models) = self.models.lock() {
            events.extend(models.iter().map(|slot| Event::ModelUsage {
                provider: slot.provider.to_string(),
                model: slot.model.clone(),
                counters: slot.counters,
                turn_wall: slot.turn_wall,
            }));
        }
        if let Ok(tools) = self.tools.lock() {
            events.extend(tools.iter().map(|(tool, calls, errors)| Event::ToolUsage {
                tool: (*tool).to_string(),
                calls: *calls,
                errors: *errors,
            }));
        }
        events
    }

    /// Snapshot the feature counters.
    #[must_use]
    pub fn counters(&self) -> Counters {
        Counters {
            turns: self.turns.load(Ordering::Relaxed),
            tool_calls: self.tool_calls.load(Ordering::Relaxed),
            fleet_dispatch: self.fleet_dispatch.load(Ordering::Relaxed),
            workflow_run: self.workflow_run.load(Ordering::Relaxed),
            subagent_spawn: self.subagent_spawn.load(Ordering::Relaxed),
            mcp_server_connected: self.mcp_server_connected.load(Ordering::Relaxed),
            memory_search: self.memory_search.load(Ordering::Relaxed),
            approval_modal_shown: self.approval_modal_shown.load(Ordering::Relaxed),
            approval_auto_allowed: self.approval_auto_allowed.load(Ordering::Relaxed),
            command_palette_open: self.command_palette_open.load(Ordering::Relaxed),
        }
    }

    /// Snapshot the error counters.
    #[must_use]
    pub fn errors(&self) -> Errors {
        Errors {
            auth_preflight_failed: self.auth_preflight_failed.load(Ordering::Relaxed),
            provider_http_4xx: self.provider_http_4xx.load(Ordering::Relaxed),
            provider_http_5xx: self.provider_http_5xx.load(Ordering::Relaxed),
            tool_denied_by_policy: self.tool_denied_by_policy.load(Ordering::Relaxed),
            tool_timeout: self.tool_timeout.load(Ordering::Relaxed),
            network_error: self.network_error.load(Ordering::Relaxed),
        }
    }

    /// Snapshot the turn wall-clock histogram.
    #[must_use]
    pub fn turn_wall(&self) -> TurnWall {
        self.turn_wall.lock().map(|wall| *wall).unwrap_or_default()
    }

    /// Snapshot the sorted, deduplicated provider set.
    #[must_use]
    pub fn providers(&self) -> Vec<String> {
        self.providers
            .lock()
            .map(|providers| providers.iter().map(|name| (*name).to_string()).collect())
            .unwrap_or_default()
    }

    fn slot(&self, counter: Counter) -> &AtomicU32 {
        match counter {
            Counter::Turns => &self.turns,
            Counter::ToolCalls => &self.tool_calls,
            Counter::FleetDispatch => &self.fleet_dispatch,
            Counter::WorkflowRun => &self.workflow_run,
            Counter::SubagentSpawn => &self.subagent_spawn,
            Counter::McpServerConnected => &self.mcp_server_connected,
            Counter::MemorySearch => &self.memory_search,
            Counter::ApprovalModalShown => &self.approval_modal_shown,
            Counter::ApprovalAutoAllowed => &self.approval_auto_allowed,
            Counter::CommandPaletteOpen => &self.command_palette_open,
        }
    }

    fn error_slot(&self, counter: ErrorCounter) -> &AtomicU32 {
        match counter {
            ErrorCounter::AuthPreflightFailed => &self.auth_preflight_failed,
            ErrorCounter::ProviderHttp4xx => &self.provider_http_4xx,
            ErrorCounter::ProviderHttp5xx => &self.provider_http_5xx,
            ErrorCounter::ToolDeniedByPolicy => &self.tool_denied_by_policy,
            ErrorCounter::ToolTimeout => &self.tool_timeout,
            ErrorCounter::NetworkError => &self.network_error,
        }
    }
}

/// Classify a provider HTTP status into the 4xx or 5xx counter, if either
/// applies. Captured from `status.as_u16() / 100` **before** the error is
/// built — every `LlmError` variant carries the raw provider body verbatim.
#[must_use]
pub fn http_status_counter(status: u16) -> Option<ErrorCounter> {
    match status / 100 {
        4 => Some(ErrorCounter::ProviderHttp4xx),
        5 => Some(ErrorCounter::ProviderHttp5xx),
        _ => None,
    }
}

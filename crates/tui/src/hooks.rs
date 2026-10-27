//! Lifecycle hooks for the Codewhale TUI and Runtime API threads.
//!
//! Hooks fire in the interactive TUI (`crates/tui/src/tui/`), the engine turn
//! loop, and Runtime API threads (`runtime_threads.rs`). Which surface fires
//! which event is the scope table in `docs/HOOKS.md`. The unrelated
//! `crates/hooks` event-sink crate is a different mechanism and shares no
//! configuration.
//!
//! Hooks execute user-defined shell commands at:
//! - Session start/end
//! - Tool call before/after
//! - Mode changes
//! - Message submission
//! - Error events
//! - Turn completion
//! - Sub-agent spawn/completion
//! - `exec_shell` environment collection
//!
//! Configuration is done via `[[hooks.hooks]]` in config.toml. See
//! `docs/HOOKS.md` for the per-event contract.

pub(crate) mod authority;
pub(crate) mod config;
mod executor;

#[allow(unused_imports)]
pub use config::{PROJECT_HOOKS_TEMPLATE, workspace_allows_project_hooks};
#[allow(unused_imports)]
pub(crate) use executor::HookCaller;
pub(crate) use executor::{
    HOOK_CONTEXT_AGGREGATE_MAX_CHARS, HOOK_EXECUTION_RECEIPT_MAX_BYTES, HOOK_LABEL_MAX_CHARS,
    generic_unavailable_detail, parse_tool_call_before_stdout, sanitize_hook_denial_reason,
    sanitize_hook_label, sanitize_hook_line, sanitize_hook_text,
};
#[cfg(test)]
pub(crate) use executor::{
    HOOK_MESSAGE_SUBMIT_PAYLOAD_MAX_BYTES, HOOK_TEXT_FIELD_MAX_CHARS, message_submit_payload,
};
pub use executor::{
    HookContext, HookExecutor, HookResult, MessageSubmitOutcome, ToolCallDecision,
    TurnEndPayloadInput, TurnEndTotals, turn_end_payload,
};

pub(crate) use executor::shell_env_keys;

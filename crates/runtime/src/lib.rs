//! Codewhale runtime: the headless half of the runtime/TUI split.
//!
//! Modules move here from `crates/tui/src` by explicit path, in the order
//! `docs/design/TUI_DECONSTRUCTION.md` records. The TUI calls them by their
//! `codewhale_runtime::<module>` paths; there is no alias block.
//!
//! This crate never depends on the terminal UI: no `codewhale-tui`, ratatui
//! or crossterm (`scripts/check-command-crate-boundaries.py`).

// Copied verbatim from the TUI crate root: moved code relies on it.
#![allow(clippy::uninlined_format_args)]

pub use codewhale_command_contract::elapsed;

pub mod active_workshop;
pub mod artifacts;
pub mod audit;
pub mod browser;
pub mod child_env;
pub mod context_budget;
pub mod continual_harness;
pub mod credentials;
pub mod delivery_files;
pub mod dsh_credentials;
pub mod external_credentials;
pub mod fast_hash;
pub mod features;
pub mod goal_loop;
pub mod hashing;
pub mod host_terminal;
pub mod lane_control;
pub mod llm_response_cache;
pub mod logging;
pub mod media_originals;
pub mod model_context;
pub mod native_memory;
pub mod private_log;
pub mod process_tree;
pub mod prompt_zones;
pub mod regex_cache;
pub mod resource_telemetry;
pub mod retry_status;
pub mod runtime_log;
pub mod runtime_policy;
pub mod safe_label;
pub mod session_diagnostics;
pub mod session_tree;
pub mod skill_state;
pub mod sleep_guard;
pub mod stall_report;
pub mod startup_trace;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod tool_history_repair;
pub mod tool_output_receipts;
pub mod turn_budget;
pub mod working_set;
pub mod workspace_discovery;

pub const CODEWHALE_MAIN_STACK_BYTES: usize = 32 * 1024 * 1024;

pub fn resolve_state_from_environment() {
    #[cfg(any(test, feature = "test-support"))]
    test_support::resolve_state_from_environment();
}

pub mod catalog_cache;

pub mod codex_model_roster;

pub mod config;

pub mod config_keys;

pub mod config_persistence;

pub mod oauth;

pub mod settings;
pub mod workspace_lease;
pub mod workspace_trust;

//! Terminal UI (TUI) module for `DeepSeek` CLI.

// The rendering layer runs inside the alt-screen. Raw stdio prints
// produce the scroll demon (see `runtime_log` for full context). Use
// `tracing::*` for diagnostics — `runtime_log` captures it to disk.
// `ui::run_event_loop` legitimately prints a post-exit resume hint
// AFTER `LeaveAlternateScreen`; that single site uses
// `#[allow(clippy::print_stdout)]` locally.
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

// === Submodules ===

pub mod active_cell;
pub(crate) mod agent_details;
pub(crate) mod agent_focus;
pub(crate) mod agent_roster;
pub mod ambient_life;
pub mod app;
pub mod approval;
pub mod auto_router;
pub mod automation_panel;
mod automation_routing;
pub mod background_finished;
pub mod background_indicator;
pub mod backtrack;
pub mod behavioral_tips;
pub mod clipboard;
pub mod color_compat;
pub mod command_palette;
pub mod composer_chrome;
pub mod composer_ui;
pub mod context_inspector;
pub mod context_menu;
pub(crate) mod control_socket;
pub(crate) mod coordination_detail;
pub mod display_refresh;
pub mod external_editor;
pub mod feedback_picker;
pub mod file_frecency;
pub mod file_mention;
pub mod file_picker;
pub mod file_picker_relevance;
pub mod file_tree;
pub mod focus_texture;
pub mod footer_hints;
pub mod footer_ui;
pub mod format_helpers;
pub mod frame_rate_limiter;
pub mod gate_receipts;
pub mod git_mention;
#[cfg(test)]
pub(crate) mod golden_harness;
pub mod herdr;
pub mod history;
pub mod hot_tail;
pub mod hotbar;
pub mod hover_hit;
pub mod hover_layer;
pub mod infoline;
pub mod key_actions;
pub mod key_shortcuts;
pub mod launch_resume_confirm;
pub mod live_transcript;
mod mcp_routing;
pub(crate) mod mention_completion;
pub mod model_picker;
pub mod motion;
pub mod mouse_ui;
pub mod notifications;
pub mod ocean;
pub mod onboarding;
pub mod output_rows_cache;
pub mod pager;
pub mod paste;
pub(crate) mod pending_requests;
pub mod persistence_actor;
pub mod pet_watch;
pub mod phase_strip;
pub(crate) mod plan_handoff;
pub(crate) mod plugin_suggestions;
pub mod prompt_suggestion;
pub mod provider_picker;
pub mod redaction_gate;
pub mod selection;
pub mod session_boot;
pub mod session_metrics;
pub mod session_picker;
pub mod settings_picker;
pub mod setup;
mod shell_job_routing;
pub mod shell_key_routing;
pub mod sidebar;
pub mod slash_menu;
pub mod startup_defaults;
pub mod startup_input;
pub mod streaming_thinking;
mod subagent_routing;
#[cfg(test)]
pub(crate) mod test_support;
pub mod theme_picker;
pub mod tideline;
mod tool_routing;
pub mod transcript;
pub mod transcript_cache;
pub mod ui;
pub mod underwater;
pub mod user_input;
pub mod views;
pub mod vim_mode;
pub mod whales;
pub mod widgets;
pub mod window_control;
pub mod work_surface;
pub mod workspace_context;
pub mod worktree_manager;

// === Re-exports ===

pub use app::{InitialInput, TuiOptions};
pub use ui::run_tui;

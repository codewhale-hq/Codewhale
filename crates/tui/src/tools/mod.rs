//! Tool system modules and re-exports.

// Tools run inside the TUI alt-screen runtime. Raw `print!` / `eprintln!`
// inside this module tree leaks into ratatui's diff-renderer buffer and
// produces the "scroll demon" regression (#1085 / v0.8.27 follow-up).
// Route status/error reporting through `tracing::*` instead — the
// `runtime_log` subscriber captures it to `~/.deepseek/logs/`.
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

pub(crate) mod activity;
pub mod apply_patch;
pub mod approval_cache;
pub mod approval_summary;
pub mod arg_repair;
pub mod automation;
pub mod canonical_action;
pub mod cargo_failure_summary;
pub mod codemode;
pub mod dev_server_readiness;
pub mod diagnostics;
pub mod diff_format;
pub mod dynamic;
pub mod execution_envelope;
pub mod file;
pub mod file_search;
pub mod finance;

pub mod echolocation;
pub mod fetch_url;
pub mod file_tool;
pub mod fim;
pub mod git;
pub mod git_history;
pub mod git_tool;
pub mod github;
pub mod goal;
pub mod handle;
pub mod harness;
pub mod image_ocr;
pub mod js_execution;
pub mod large_output_router;
pub mod lsp;
pub mod mcp_registry;
pub mod native_memory;
pub mod notify;
pub mod pandoc;
pub(crate) mod pdf;
pub mod plan;
pub mod plugin;
pub mod project;
pub mod read_media;
pub mod registry;
pub mod remember;
pub mod request_plugin_install;
mod resource_admission;
pub mod revert_turn;
pub mod review;
pub(crate) mod review_host;
pub mod review_hunks;
pub(crate) mod review_pr;
pub mod rlm;
pub mod run_tool;
pub mod runtime_mcp;
mod rust_format;
pub mod schema_canonicalize;
pub mod schema_sanitize;
pub mod search;
pub mod send_later;
pub mod session;
pub mod shell;
pub(crate) mod shell_output;
pub mod skill;
pub mod spec;
pub mod speech;
pub mod subagent;
mod syntax_check;
pub mod tasks;
#[cfg(not(target_env = "ohos"))]
pub mod terminal_session;
pub mod test_runner;
pub mod todo;
pub mod tool_call_budget;
pub mod tool_result_retrieval;
pub mod truncate;
pub mod tui_help;
pub mod user_input;
pub mod validate_data;
pub mod verifier;
pub mod verify;
pub mod web;
pub mod web_run;
pub mod web_search;
pub mod web_tool;
pub mod workflow;
pub mod workflow_plan_approval;

pub use registry::{AgentToolSurfaceOptions, ToolRegistry, ToolRegistryBuilder};
pub use review::ReviewOutput;
pub use spec::ToolContext;
pub use user_input::UserInputResponse;

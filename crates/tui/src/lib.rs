//! Codewhale TUI library — single-binary entry point.

#![allow(clippy::uninlined_format_args)]

use std::collections::{BTreeSet, HashMap};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
#[cfg(test)]
use clap::CommandFactory;
use clap::{Args, Parser, Subcommand, ValueEnum};
use tempfile::NamedTempFile;
use wait_timeout::ChildExt;

use crate::dependencies::ExternalTool;

mod acp_server;
pub mod agent_roster;
mod approval_log;
mod artifacts;
mod audit;
mod auto_reasoning;
mod automation_manager;
mod child_env;
mod client;
pub mod cloud_dispatch;
mod codex_model_cache;
mod commands;
mod compaction;
mod composer_history;
mod composer_stash;
pub mod computer_meter;
mod config;
pub mod config_keys;
mod config_persistence;
#[cfg(test)]
mod conformance;
mod context_report;
mod core;
mod cost_status;
mod credentials;
/// Guarded installation I/O reusing Engine's confined file primitives.
pub mod delivery_files;
mod dependencies;
pub mod dispatch_runner;
mod doctor;
mod doctor_fix;
mod dsh_credentials;
mod error_taxonomy;
mod eval;
mod extension_host;
mod external_credentials;
mod features;
mod fleet;
mod fs_confined;
mod git_status;
mod keybinding_table;
use crate::fleet::executor::exec_stream_final_answer_excerpt;
mod hooks;
mod image_attach;
mod import_claude;
mod integrations;
mod lane_control;
mod llm_client;
mod local_ollama;
mod logging;
mod lsp;
mod mcp;
mod mcp_server;
mod model_inventory;
mod model_profile;
mod model_relevance;
mod model_routing;
mod models_dev_live;
pub use models_dev_live::maybe_load_persisted_cache;
mod network_policy;
mod notify;
mod oauth;
mod operate;
mod plugins;
mod pricing;
mod process_tree;
mod profile_constitution;
mod project_context;
mod project_context_cache;
mod prompts;
mod provider_catalog_live;
mod provider_lake;
pub use provider_lake::all_catalog_models_for_provider;
mod provider_readiness;
mod purge;
pub mod reasoning_preference;
mod receipts;
mod remote_control;
mod remote_setup;
pub mod repl;
mod repo_law;
mod request_manifest;
mod request_tuning;
mod resource_telemetry;
pub mod rlm;
mod route_billing;
mod route_budget;
pub mod route_preferences;
mod route_receipt;
mod route_runtime;
mod runtime_api;
mod runtime_chat_relay;
mod runtime_handoff;
mod runtime_log;
mod runtime_policy;
mod runtime_threads;
mod sandbox;
mod scorecard;
mod session_diagnostics;
// Acceptance matrix for #2934 / #4397. Test-only: the table documents the
// contract for reviewers and is enforced by the tests beside it, so it does
// not need to exist in a shipped binary.
#[cfg(test)]
#[path = "main/tests.rs"]
mod doctor_loader_tests;
#[cfg(test)]
mod session_control_acceptance;
mod session_export;
mod session_manager;
mod session_peek;
mod session_projection;
mod session_reconcile;
mod session_resume;
mod session_secret_scrub;
mod settings;
mod shell_dispatcher;
mod skills;
mod snapshot;
mod startup_trace;
mod superfast;
mod task_manager;
mod telemetry_notice;
#[cfg(test)]
mod test_support;
// TLS bootstrap and platform client builders live in codewhale-release;
// `crate::tls::*` keeps resolving for every caller.
use codewhale_release::tls;
// Runtime split path alias: modules that moved to `crates/runtime` keep
// resolving as `crate::<module>` inside this crate. One block, no per-item
// re-exports; the split deletes it by rewriting these paths to
// `codewhale_runtime::` (docs/design/TUI_DECONSTRUCTION.md).
use codewhale_runtime::{
    context_budget, continual_harness, fast_hash, goal_loop, hashing, host_terminal,
    llm_response_cache, media_originals, model_context, native_memory, prompt_zones, regex_cache,
    retry_status, safe_label, session_tree, skill_state, sleep_guard, tool_history_repair,
    workspace_discovery,
};
mod todo_snapshot;
mod tool_inspection;
mod tool_output_receipts;
mod tools;
mod tui;
/// Portable, dependency-free dot-whale core and conformance helpers.
/// The terminal and external renderers share this implementation.
pub use tui::ambient_life::pet_sim as pet;
mod turn_route_plan;
mod utils;
mod vision;
mod voice;
mod work_graph;
mod worker_profile;
mod working_set;
mod workspace_trust;

use crate::config::{
    Config, DEFAULT_MAX_SUBAGENTS, DEFAULT_TEXT_MODEL, MAX_SUBAGENTS, effective_home_dir,
    initialize_cloud_facts,
};
use crate::eval::{EvalHarness, EvalHarnessConfig, ScenarioStepKind};
use crate::features::{Feature, render_feature_table};
use crate::llm_client::LlmClient;
use crate::mcp::{
    McpCommandAvailability, McpPool, McpServerConfig, McpServerOAuthConfig,
    is_relative_stdio_path_arg,
};
#[cfg(test)]
use crate::session_manager::create_saved_session;
use crate::session_manager::{SessionManager, truncate_id};
use crate::tui::app::ScreenMode;
use crate::tui::history::{summarize_tool_args, summarize_tool_output};
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message, MessageRequest, SystemPrompt};

#[cfg(windows)]
fn configure_windows_console_utf8() {
    use windows::Win32::System::Console::{SetConsoleCP, SetConsoleOutputCP};

    const CP_UTF8: u32 = 65001;
    // SAFETY: integer argument only; failures discarded.
    unsafe {
        let _ = SetConsoleCP(CP_UTF8);
        let _ = SetConsoleOutputCP(CP_UTF8);
    }
}

#[cfg(not(windows))]
fn configure_windows_console_utf8() {}

fn install_rustls_crypto_provider() {
    crate::tls::ensure_rustls_crypto_provider();
}

mod runtime_options;
pub use runtime_options::RuntimeOptions;

// The canonical executable owns process arguments. This decoder handles only
// delegated Engine commands and their command-specific interactive flags.
#[derive(Parser, Debug)]
#[command(name = "codewhale", bin_name = "codewhale", version = env!("CODEWHALE_BUILD_VERSION"))]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[command(flatten)]
    options: RuntimeOptions,
    /// Initial prompt to submit in the interactive TUI.
    #[arg(short, long, value_name = "PROMPT", num_args = 1..)]
    prompt: Vec<String>,
    /// Resume a previous session by ID or prefix.
    #[arg(short, long, conflicts_with = "continue_session")]
    resume: Option<String>,
    /// Continue the most recent session in this workspace.
    #[arg(short = 'c', long = "continue", conflicts_with = "resume")]
    continue_session: bool,
    /// Recover the exact durable Resume/Fork intent without repeating its mutation.
    #[arg(long, global = true, value_name = "KEY")]
    operation_key: Option<String>,
}

impl std::ops::Deref for Cli {
    type Target = RuntimeOptions;
    fn deref(&self) -> &Self::Target {
        &self.options
    }
}

#[derive(Subcommand, Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Run system diagnostics and check configuration
    Doctor(DoctorArgs),
    /// Summarize failure signals from a local JSONL session log without raw content
    SessionDiagnostics(SessionDiagnosticsArgs),
    #[command(about = "Install a local, GitHub or version-pinned npm plugin bundle")]
    Install { source: String },
    /// Bootstrap MCP config and/or skills directories
    Setup(SetupArgs),
    /// Generate a remote Codewhale agent deploy bundle (cloud + chat bridge)
    RemoteSetup(remote_setup::RemoteSetupArgs),
    /// List saved sessions, or export one as a full-fidelity archive
    Sessions {
        /// Maximum number of sessions to display
        #[arg(short, long, default_value = "20")]
        limit: usize,
        /// Search sessions by title
        #[arg(short, long)]
        search: Option<String>,
        #[command(subcommand)]
        command: Option<SessionsCommand>,
    },
    /// Show what a session did: files changed, commands run, web and MCP
    /// calls, agents, approvals, and failures, read from its saved record
    #[command(visible_alias = "receipt")]
    Receipts {
        /// Session id or unique prefix, or a Runtime thread id (thr_...).
        /// Omit it (or pass --last) for the most recently updated one.
        #[arg(value_name = "SESSION_ID")]
        id: Option<String>,
        /// Use the most recently updated session or thread
        #[arg(long, conflicts_with = "id")]
        last: bool,
        /// Limit to one turn: a thread's turn id, or a session's turn number
        #[arg(long, value_name = "TURN")]
        turn: Option<String>,
        /// Output format
        #[arg(long, value_enum, default_value = "md")]
        format: receipts::ReceiptFormat,
    },
    /// Create default AGENTS.md in current directory
    Init,
    /// Sign in to your Codewhale account (use the `codewhale` CLI).
    Login {
        /// Legacy provider-key flag: rejected with a redirect to `auth set`.
        #[arg(long, hide = true)]
        api_key: Option<String>,
    },
    /// Remove the saved API key
    Logout,
    /// Manage provider authentication flows.
    Auth(TuiAuthArgs),
    /// List cached models, or update catalogs for configured providers
    Models(ModelsArgs),
    /// Generate speech audio with Xiaomi MiMo TTS models
    #[command(visible_alias = "tts")]
    Speech(SpeechArgs),
    /// Run a non-interactive prompt. Use --auto for agent-with-tools mode.
    Exec(ExecArgs),
    /// Manage local Agent fleet runs and workers (`fleet` is a compatibility alias)
    #[command(name = "fleet")]
    Fleet(FleetArgs),
    /// Internal model-free Workflow tool dispatcher used by Lane Runtime.
    #[command(name = "workflow-tool", hide = true)]
    WorkflowTool(WorkflowToolArgs),
    /// Run a code review over a git diff
    Review(ReviewArgs),
    /// Open the TUI pre-seeded with a GitHub PR's title, body, and diff
    Pr {
        /// PR number
        #[arg(value_name = "NUMBER")]
        number: u32,
        /// Repository in `owner/name` form. Defaults to the current
        /// workspace's `gh` config (i.e. the repo gh thinks you're in).
        #[arg(short = 'R', long)]
        repo: Option<String>,
        /// Skip `gh pr checkout` even if gh is available. By default
        /// the working tree is left as-is — checkout is opt-in via
        /// `--checkout` because dirty trees fail it loudly.
        #[arg(long, default_value_t = false)]
        checkout: bool,
    },
    /// Apply a patch file (or stdin) to the working tree
    Apply(ApplyArgs),
    /// Run the offline evaluation harness (no network/LLM calls)
    Eval(EvalArgs),
    /// Score a run's token/cache/cost from recorded turns; flag regressions vs a baseline
    Scorecard(ScorecardArgs),
    /// Manage MCP servers
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Inspect feature flags
    Features(FeaturesCli),
    /// Connect third-party harnesses through Codewhale (currently: DeepSeek Harness `dsh`)
    Integrations {
        #[command(subcommand)]
        command: IntegrationsCommand,
    },
    /// Run a command inside the sandbox
    Sandbox(SandboxArgs),
    /// Run a local server (e.g. MCP)
    Serve(ServeArgs),
    /// Resume a previous session by ID (use --last for most recent)
    Resume {
        /// Conversation/session id (UUID or prefix)
        #[arg(value_name = "SESSION_ID")]
        session_id: Option<String>,
        /// Continue the most recent session in this workspace without a picker
        #[arg(long = "last", default_value_t = false, conflicts_with = "session_id")]
        last: bool,
    },
    /// Fork a previous session by ID (use --last for most recent)
    Fork {
        /// Conversation/session id (UUID or prefix)
        #[arg(value_name = "SESSION_ID")]
        session_id: Option<String>,
        /// Fork the most recent session in this workspace without a picker
        #[arg(long = "last", default_value_t = false, conflicts_with = "session_id")]
        last: bool,
    },
}

/// Subcommands of `codewhale sessions`. Without one, the command falls back
/// to listing sessions.
#[derive(Subcommand, Debug, Clone)]
enum SessionsCommand {
    /// List saved sessions (default when no subcommand is given)
    List {
        /// Maximum number of sessions to display
        #[arg(short, long, default_value = "20")]
        limit: usize,
        /// Search sessions by title
        #[arg(short, long)]
        search: Option<String>,
    },
    /// Mask credentials that older builds stored in saved sessions' tool
    /// output. Reports what it would change unless `--apply` is given. Run it
    /// while no Codewhale session is open.
    ScrubSecrets {
        /// Rewrite the affected session files (default: report only)
        #[arg(long, default_value_t = false)]
        apply: bool,
    },
    /// Export a session as a full-fidelity tar.xz archive (complete context:
    /// system prompt, messages, tool calls and results, plus artifacts)
    Export {
        /// Session id (or unambiguous id prefix) to export
        #[arg(value_name = "SESSION_ID")]
        id: String,
        /// Destination .tar.xz path (default: codewhale-session-<id>.tar.xz)
        #[arg(short, long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Exclude the session artifacts directory from the archive
        #[arg(long, default_value_t = false)]
        skip_artifacts: bool,
        /// xz compression preset, 0 (fastest) through 9 (smallest)
        #[arg(long, default_value_t = session_export::DEFAULT_XZ_COMPRESSION_LEVEL)]
        compression: u32,
        /// Overwrite the destination file if it already exists
        #[arg(long, default_value_t = false)]
        force: bool,
    },
}

#[derive(Args, Debug, Clone)]
#[command(after_help = "\
Examples:
  codewhale exec \"explain this function\"
  codewhale exec --auto \"list crates/ with ls\"
  codewhale exec --auto --output-format stream-json \"fix the failing test\"

Plain `codewhale exec` is a one-shot model response: one Engine turn with the
same system prompt as every other run, and no tools. Use `--auto` for
non-interactive agent-with-tools execution. Tools are offered only with
`--auto`, `--yolo`, `--allowed-tools`, or when resuming a session; limits such
as `--max-turns`, `--disallowed-tools` or `--sandbox`, and the output format,
never add tools. A reply cut off at the provider's output limit is continued
in the same turn, at most 8 model steps unless `--max-turns` says otherwise.
`--auto` does not change the sandbox posture or elevate a denied tool. Use `--sandbox danger-full-access`
or `--allow-sandbox-elevation` to explicitly authorize sandbox elevation.
")]
struct ExecArgs {
    /// Override model for this run
    #[arg(long)]
    model: Option<String>,
    /// Override the provider for this run (e.g. `deepseek`, `openrouter`).
    /// Non-secret identifier only — credentials still resolve from the
    /// environment/config. Fleet uses this to launch a worker on its
    /// profile-pinned provider even when the parent session is on another
    /// one (#4093).
    #[arg(long)]
    provider: Option<String>,
    /// Override reasoning/thinking effort for this run.
    /// Accepted values: auto, off, low, medium, high, max.
    #[arg(long = "reasoning-effort", value_name = "EFFORT")]
    reasoning_effort: Option<String>,
    /// Enable agent-with-tools mode with automatic tool approvals. This does
    /// not authorize sandbox elevation.
    #[arg(long, default_value_t = false)]
    auto: bool,
    /// Sandbox policy for this exec run; independent from --auto.
    #[arg(long, value_name = "POLICY")]
    sandbox: Option<String>,
    /// Explicitly allow a denied tool to retry with danger-full-access.
    #[arg(long, default_value_t = false)]
    allow_sandbox_elevation: bool,
    /// Emit machine-readable JSON output
    #[arg(long, default_value_t = false, conflicts_with = "output_format")]
    json: bool,
    /// Resume a previous session by ID or prefix
    #[arg(long, value_name = "SESSION_ID", conflicts_with_all = ["session_id", "continue_session"])]
    resume: Option<String>,
    /// Resume a previous session by ID or prefix
    #[arg(long = "session-id", value_name = "SESSION_ID", conflicts_with_all = ["resume", "continue_session"])]
    session_id: Option<String>,
    /// Continue the most recent session for this workspace
    #[arg(long = "continue", default_value_t = false, conflicts_with_all = ["resume", "session_id"])]
    continue_session: bool,
    /// Output format for exec mode
    #[arg(long, value_enum, default_value_t = ExecOutputFormat::Text)]
    output_format: ExecOutputFormat,
    /// Comma-separated list of canonical tools to allow (all others denied).
    /// Names are case-insensitive: Bash, File, Git, Run, etc.
    #[arg(long, value_delimiter = ',')]
    allowed_tools: Option<Vec<String>>,
    /// Comma-separated list of tools to deny (deny wins over allow).
    #[arg(long, value_delimiter = ',')]
    disallowed_tools: Option<Vec<String>>,
    /// Maximum number of model steps before the run ends. Omitted means unlimited.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    max_turns: Option<u32>,
    /// Maximum number of tool calls admitted in one model turn. Omitted means unlimited.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    max_tool_calls: Option<u32>,
    /// Shut down when the parent closes this process's stdin. Fleet workers
    /// pass this automatically: a dead manager must not leave detached workers
    /// spending forever (R7).
    #[arg(long, default_value_t = false)]
    parent_death_watch: bool,
    /// Extra text appended to the system prompt for this run.
    #[arg(long)]
    append_system_prompt: Option<String>,
    /// Internal Fleet worker authority envelope. Non-secret, versioned JSON.
    #[arg(long, value_name = "JSON", hide = true)]
    tool_authority_json: Option<String>,
    /// Fire the configured hooks in this run (opt-in). On the headless path
    /// the engine-side events are `tool_call_before` — which may still deny
    /// a call — and `shell_env`. A hook `ask` resolves fail-closed because
    /// nothing can prompt headlessly. Fleet worker subprocesses never fire
    /// operator hooks.
    #[arg(long, default_value_t = false)]
    hooks: bool,
    /// Read the prompt from a file (`-` reads stdin) instead of argv, for
    /// prompts past the OS per-argument limit (~128 KiB on Linux).
    #[arg(long, value_name = "PATH", conflicts_with = "prompt")]
    prompt_file: Option<PathBuf>,
    /// Prompt to send to the model. Taken literally, including a lone `-`;
    /// use `--prompt-file -` to read stdin.
    #[arg(
        value_name = "PROMPT",
        required_unless_present = "prompt_file",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    prompt: Vec<String>,
}

#[derive(Args, Debug, Clone)]
struct WorkflowToolArgs {
    /// Authority provenance stamped by the public `workflow run` command.
    #[arg(long, value_name = "SOURCE")]
    approval_source: String,
    /// Exact Workflow tool input serialized as one JSON object.
    #[arg(long, value_name = "JSON")]
    input_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExecOutputFormat {
    Text,
    #[value(name = "stream-json")]
    StreamJson,
}

#[derive(Args, Debug, Clone)]
struct TuiAuthArgs {
    #[command(subcommand)]
    command: TuiAuthCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum TuiAuthCommand {
    /// Sign in to xAI/Grok with an SSH-friendly device code; run again to switch accounts.
    #[command(name = "xai-device")]
    XaiDevice,
    /// Sign in with ChatGPT for Codex subscription access; run again to switch accounts.
    #[command(name = "chatgpt")]
    Chatgpt,
    /// Revoke Codewhale-owned ChatGPT tokens. Codex CLI consent is unchanged.
    #[command(name = "chatgpt-revoke")]
    ChatgptRevoke,
    #[command(name = "claude", alias = "anthropic")]
    Claude,
    #[command(name = "claude-revoke")]
    ClaudeRevoke,
    /// Sign in to OrcaRouter with OAuth 2.0 + PKCE; run again to switch accounts.
    #[command(name = "orcarouter")]
    Orcarouter,
    /// Revoke the saved OrcaRouter credential. The OrcaRouter console also
    /// revokes every key it issued to this app in one click.
    #[command(name = "orcarouter-revoke")]
    OrcarouterRevoke,
    /// Sign in to a provider contributed by an enabled, reviewed plugin.
    PluginLogin {
        #[arg(long)]
        provider: String,
    },
    /// Remove credentials for one plugin provider without changing trust.
    PluginLogout {
        #[arg(long)]
        provider: String,
    },
}

const CODEWHALE_TOOL_SURFACE_ENV: &str = "CODEWHALE_TOOL_SURFACE";
const SHELL_ONLY_EXEC_TOOLS: &[&str] = &["bash"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecToolSurface {
    ShellOnly,
}

fn exec_tool_surface_from_env() -> Option<ExecToolSurface> {
    std::env::var(CODEWHALE_TOOL_SURFACE_ENV)
        .ok()
        .and_then(|value| {
            if should_warn_unknown_exec_tool_surface(&value) {
                eprintln!(
                    "warning: unrecognized {CODEWHALE_TOOL_SURFACE_ENV}; leaving exec tool surface unchanged. Use `shell-only`, `full`, or `native-tools`."
                );
            }
            parse_exec_tool_surface(&value)
        })
}

fn parse_exec_tool_surface(value: &str) -> Option<ExecToolSurface> {
    match value.trim().to_ascii_lowercase().as_str() {
        "shell-only" | "shell_only" | "shell" => Some(ExecToolSurface::ShellOnly),
        "full" | "native-tools" | "native_tools" | "" => None,
        _ => None,
    }
}

fn should_warn_unknown_exec_tool_surface(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    !matches!(
        normalized.as_str(),
        "" | "shell-only" | "shell_only" | "shell" | "full" | "native-tools" | "native_tools"
    )
}

fn normalize_exec_tool_names(tools: &[String]) -> Vec<String> {
    tools
        .iter()
        .map(|name| name.to_ascii_lowercase().trim().to_string())
        .collect()
}

fn shell_only_exec_allowed_tools() -> Vec<String> {
    SHELL_ONLY_EXEC_TOOLS
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

/// #6510: whether an `exec` run is offered tools. Plain exec is one Engine
/// turn with no tools; only a flag that grants tool authority opens a
/// surface: `--auto`/`--yolo`, an explicit `--allowed-tools` list, a Fleet
/// authority envelope, or the launcher's tool-surface env. A resumed session
/// keeps its surface, because its history can carry tool calls and results
/// that a zero-tool request cannot replay.
///
/// Limits (`--max-turns`, `--max-tool-calls`, `--disallowed-tools`,
/// `--sandbox`, `--allow-sandbox-elevation`), prompt and hook opt-ins
/// (`--append-system-prompt`, `--hooks`) and the output format never grant
/// tools. They used to, so `exec --max-turns 1 "hi"` silently became a
/// tool-using agent.
fn exec_grants_tool_surface(
    args: &ExecArgs,
    yolo: bool,
    resuming: bool,
    env_tool_surface: bool,
) -> bool {
    args.auto
        || yolo
        || resuming
        || args.allowed_tools.is_some()
        || args.tool_authority_json.is_some()
        || env_tool_surface
}

/// Flags that only shape a tool surface, passed on a run that has none.
fn exec_tool_flags_without_grant(args: &ExecArgs) -> Vec<&'static str> {
    [
        (args.max_tool_calls.is_some(), "--max-tool-calls"),
        (args.disallowed_tools.is_some(), "--disallowed-tools"),
        (args.sandbox.is_some(), "--sandbox"),
        (args.allow_sandbox_elevation, "--allow-sandbox-elevation"),
        (args.hooks, "--hooks"),
    ]
    .into_iter()
    .filter_map(|(passed, flag)| passed.then_some(flag))
    .collect()
}

fn resolve_exec_allowed_tools(
    cli_allowed_tools: Option<&[String]>,
    env_tool_surface: Option<ExecToolSurface>,
) -> Option<Vec<String>> {
    if let Some(tools) = cli_allowed_tools {
        return Some(normalize_exec_tool_names(tools));
    }

    env_tool_surface.map(|ExecToolSurface::ShellOnly| shell_only_exec_allowed_tools())
}

#[derive(Args, Debug, Clone)]
struct FleetArgs {
    #[command(subcommand)]
    command: FleetCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum FleetCommand {
    /// Initialize the local fleet ledger for this workspace
    Init,
    /// Create a run from a task spec and start the foreground manager loop
    Run(FleetRunArgs),
    /// List durable fleet runs from this workspace's ledger
    List,
    /// Show queued/running/completed/failed/stale fleet counts
    Status,
    /// Inspect one worker's status, heartbeat, latest event, and artifacts
    Inspect {
        /// Worker id printed by `codewhale fleet run`
        worker_id: String,
    },
    /// Print bounded log artifacts for one worker
    Logs {
        /// Worker id printed by `codewhale fleet run`
        worker_id: String,
    },
    /// List artifact refs for one worker
    Artifacts {
        /// Worker id printed by `codewhale fleet run`
        worker_id: String,
    },
    /// Interrupt a running worker task and record a terminal cancellation
    Interrupt {
        /// Worker id printed by `codewhale fleet run`
        worker_id: String,
    },
    /// Restart the latest task for a worker
    Restart {
        /// Worker id printed by `codewhale fleet run`
        worker_id: String,
    },
    /// Resume a run from durable ledger state, reconciling orphaned/stale leases
    Resume {
        /// Run id printed by `codewhale fleet run`
        run_id: String,
        /// Seconds without heartbeat before a leased task is treated as stale
        #[arg(long, default_value_t = 300)]
        stale_after_seconds: u64,
    },
    /// Stop all queued and running fleet work
    Stop {
        /// Confirm stopping all queued and running fleet tasks
        #[arg(long, required = true)]
        all: bool,
    },
    /// Render a redacted fleet alert payload without sending it
    AlertDryRun(FleetAlertDryRunArgs),
}

#[derive(Args, Debug, Clone)]
struct FleetRunArgs {
    /// JSON or TOML task spec to enqueue
    #[arg(value_name = "TASK_SPEC")]
    task_spec: PathBuf,
    /// Maximum local workers to lease concurrently
    #[arg(long, default_value_t = 4)]
    max_workers: usize,
    /// Seconds without heartbeat before a running task is counted stale
    #[arg(long, default_value_t = 300)]
    stale_after_seconds: u64,
    /// Schedule once and return instead of staying in the manager loop
    #[arg(long, hide = true, default_value_t = false)]
    once: bool,
    /// Validate the spec (shape, roster members, profiles, model routes)
    /// without creating a run or starting any worker
    #[arg(long, default_value_t = false)]
    check: bool,
}

#[derive(Args, Debug, Clone)]
struct FleetAlertDryRunArgs {
    /// Alert event class to render
    #[arg(long, value_enum)]
    event: FleetAlertEventArg,
    /// fleet run id
    #[arg(long)]
    run_id: String,
    /// Worker id, when the event belongs to one worker
    #[arg(long)]
    worker_id: Option<String>,
    /// Task id, when the event belongs to one task
    #[arg(long)]
    task_id: Option<String>,
    /// Short human-readable reason for the alert
    #[arg(long, default_value = "manual fleet alert dry-run")]
    reason: String,
    /// Status label to include in the payload
    #[arg(long)]
    status: Option<String>,
    /// Adapter payload shape to render
    #[arg(long, value_enum, default_value_t = FleetAlertAdapterArg::Slack)]
    adapter: FleetAlertAdapterArg,
    /// Environment variable containing the Slack webhook URL
    #[arg(long, default_value = "CODEWHALE_FLEET_SLACK_WEBHOOK")]
    slack_webhook_env: String,
    /// Environment variable containing the generic webhook URL
    #[arg(long, default_value = "CODEWHALE_FLEET_WEBHOOK_URL")]
    webhook_url_env: String,
    /// Optional environment variable containing the generic webhook secret
    #[arg(long)]
    webhook_secret_env: Option<String>,
    /// Environment variable containing the PagerDuty routing key
    #[arg(long, default_value = "CODEWHALE_FLEET_PAGERDUTY_ROUTING_KEY")]
    pagerduty_routing_key_env: String,
    /// PagerDuty severity to render
    #[arg(long, default_value = "error")]
    pagerduty_severity: String,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum FleetAlertEventArg {
    Stale,
    RestartExhausted,
    NeedsHuman,
    BudgetExceeded,
    VerifierFailed,
    RunCompleted,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum FleetAlertAdapterArg {
    Slack,
    Webhook,
    PagerDuty,
}

/// Spawn a tokio task that listens for terminating signals (SIGINT
/// always; SIGTERM and SIGHUP on Unix) and, on receipt, restores the
/// terminal modes and exits with the conventional 128 + signal code.
/// Multiple deliveries are tolerated: once the cleanup runs, a second
/// signal short-circuits to plain exit so a stuck cleanup can never
/// trap a frustrated user pressing Ctrl+C repeatedly.
///
/// See the call site in `main` for the rationale (#1583).
///
/// Registration is synchronous, before the spawn: a `tokio::spawn`ed task does
/// not run until the scheduler first polls it, so registering the signal
/// streams *inside* it leaves a window — unbounded under load — where SIGINT
/// still has its default disposition and kills the process outright. That is
/// the very outcome this handler exists to prevent, and it produced a real
/// terminated-by-signal exit (no code, no terminal restore, no `session_end`).
/// After this function returns, the signals are armed.
fn spawn_signal_cleanup_task() {
    let mut signals = TerminatingSignals::register();
    tokio::spawn(async move {
        let exit_code = signals.wait().await;
        // A serving Runtime API gets a bounded window to end its open event
        // streams with a typed `stream.end`, so clients can tell a Runtime
        // that stopped from a dropped connection. A second signal skips it.
        tokio::select! {
            () = runtime_api::drain_for_signal_exit() => {}
            _ = signals.wait() => {}
        }
        // If we get here a fatal signal arrived. Restore the terminal
        // and exit. A second signal during cleanup re-enters this
        // path and aborts via `std::process::exit` directly.
        static CLEANED_UP: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if !CLEANED_UP.swap(true, std::sync::atomic::Ordering::SeqCst) {
            #[cfg(unix)]
            crate::tools::shell::abort_pending_persistent_process_groups_for_exit();
            #[cfg(unix)]
            crate::process_tree::kill_contained_trees_for_exit();
            crate::tui::ui::emergency_restore_terminal();
            // Nothing async survives the `exit` below, so this is the last
            // chance to say how the session ended. `record_blocking` is one
            // `O_APPEND` write with no lock: taking the compaction lock here
            // would let a second Codewhale process sharing CODEWHALE_HOME hang
            // Ctrl-C, and the second-signal short-circuit below has to stay
            // reachable. A no-op unless this process was armed.
            //
            // The class is stated, not derived: `RunTerminationReason::Canceled`
            // also exits 130, so `exit_code` cannot tell a signal from an
            // Esc-cancelled turn.
            record_signal_session_end();
        }
        std::process::exit(exit_code);
    });
}

/// When this process's armed telemetry session began. Set once, at arming, and
/// read from both the ordinary teardown and the signal path.
static TELEMETRY_SESSION_START: std::sync::OnceLock<std::time::Instant> =
    std::sync::OnceLock::new();

/// Build `session_end` from what this process actually accumulated.
///
/// The exit class is read from the process-wide atomic and never derived from
/// an exit code: `RunTerminationReason::Canceled` maps to 130, the same value
/// the SIGINT path uses, so a code-based derivation would report every
/// Esc-cancelled turn as a signal.
///
/// The cold-start bucket is `None` unless the interactive event loop actually
/// began, which is what keeps it absent rather than invented on the surfaces
/// that have no event loop.
fn telemetry_session_end() -> codewhale_telemetry::Event {
    let counters = codewhale_telemetry::session_counters();
    codewhale_telemetry::Event::SessionEnd {
        duration_bucket: codewhale_telemetry::DurationBucket::from_secs(
            TELEMETRY_SESSION_START
                .get()
                .map_or(0, |start| start.elapsed().as_secs()),
        ),
        exit_class: codewhale_telemetry::exit_class(),
        cold_start_bucket: crate::startup_trace::cold_start_ms()
            .map(codewhale_telemetry::ColdStartBucket::from_millis),
        providers: counters.providers(),
        counters: counters.counters(),
        errors: counters.errors(),
        turn_wall: counters.turn_wall(),
    }
}

/// Close the session synchronously, from the signal handler.
///
/// A no-op unless this process was armed.
fn record_signal_session_end() {
    codewhale_telemetry::set_exit_class(codewhale_telemetry::ExitClass::Signal);
    codewhale_telemetry::record_blocking(telemetry_session_end());
}

/// Terminating-signal streams, registered up front and awaited later.
///
/// Splitting registration from the await is the point: the OS disposition
/// changes when `register` returns, not when the waiting task is first polled.
#[cfg(unix)]
struct TerminatingSignals {
    sigint: Option<tokio::signal::unix::Signal>,
    sigterm: Option<tokio::signal::unix::Signal>,
    sighup: Option<tokio::signal::unix::Signal>,
}

#[cfg(unix)]
impl TerminatingSignals {
    /// Install the handlers. Failing to install any individual stream is
    /// non-fatal: we still want the others to work.
    fn register() -> Self {
        use tokio::signal::unix::{SignalKind, signal};
        Self {
            sigint: signal(SignalKind::interrupt()).ok(),
            sigterm: signal(SignalKind::terminate()).ok(),
            sighup: signal(SignalKind::hangup()).ok(),
        }
    }

    /// Resolve with 128 + signal number for whichever arrives first. The
    /// fallback never-resolving future keeps `select!` well-typed when a
    /// stream failed to register.
    async fn wait(&mut self) -> i32 {
        tokio::select! {
            _ = async { match self.sigint.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending::<()>().await, } } => 130,
            _ = async { match self.sigterm.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending::<()>().await, } } => 143,
            _ = async { match self.sighup.as_mut() { Some(s) => { s.recv().await; }, None => std::future::pending::<()>().await, } } => 129,
        }
    }
}

/// Windows: `ctrl_c` covers both Ctrl+C and Ctrl+Break (CTRL_C_EVENT /
/// CTRL_BREAK_EVENT). Console-close, logoff, and shutdown events are not
/// currently routed through tokio.
#[cfg(not(unix))]
struct TerminatingSignals {
    ctrl_c: Option<tokio::signal::windows::CtrlC>,
}

#[cfg(not(unix))]
impl TerminatingSignals {
    fn register() -> Self {
        Self {
            ctrl_c: tokio::signal::windows::ctrl_c().ok(),
        }
    }

    async fn wait(&mut self) -> i32 {
        match self.ctrl_c.as_mut() {
            Some(s) => {
                s.recv().await;
            }
            None => std::future::pending::<()>().await,
        }
        130
    }
}

fn join_prompt_parts(parts: &[String]) -> String {
    parts.join(" ")
}

/// Maximum bytes accepted for an exec prompt read from `--prompt-file` or
/// stdin. Far past argv's per-argument ceiling; the model's context window
/// stays the real limit (#6688).
const MAX_EXEC_PROMPT_BYTES: u64 = 64 * 1024 * 1024;

/// The effective exec prompt: argv words, or the body of `--prompt-file`
/// (`-` = stdin). A positional `-` stays literal text: raw-prompt callers
/// such as cloud dispatch pass a job prompt verbatim as argv. Runs before
/// any model call so a missing or empty source fails loudly (#6688).
fn resolve_exec_prompt(args: &ExecArgs) -> Result<String> {
    let Some(path) = &args.prompt_file else {
        return Ok(join_prompt_parts(&args.prompt));
    };
    let prompt = if path.as_os_str() == "-" {
        if args.parent_death_watch {
            bail!("--parent-death-watch owns stdin; pass the prompt with --prompt-file <PATH>");
        }
        let stdin = io::stdin();
        if stdin.is_terminal() {
            bail!("--prompt-file - reads stdin, but stdin is a terminal; pipe the prompt in.");
        }
        read_capped_text(stdin.lock(), MAX_EXEC_PROMPT_BYTES, "exec prompt on stdin")?
    } else {
        let file = std::fs::File::open(path)
            .with_context(|| format!("failed to open --prompt-file {}", path.display()))?;
        read_capped_text(file, MAX_EXEC_PROMPT_BYTES, "--prompt-file")
            .with_context(|| format!("failed to read --prompt-file {}", path.display()))?
    };
    if prompt.trim().is_empty() {
        let source = if path.as_os_str() == "-" {
            "stdin".to_string()
        } else {
            path.display().to_string()
        };
        bail!("exec prompt from {source} is empty");
    }
    Ok(prompt)
}

fn resolve_exec_model(config: &Config, explicit_model: Option<&str>) -> String {
    explicit_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
        .or_else(exec_model_env_override)
        .unwrap_or_else(|| config.default_model())
}

fn apply_exec_provider_override(config: &mut Config, provider_arg: &str) -> Result<()> {
    let provider_arg = provider_arg.trim();
    if provider_arg.is_empty() {
        return Ok(());
    }
    if config
        .providers
        .as_ref()
        .and_then(|providers| providers.custom_provider_config(provider_arg))
        .is_some()
    {
        config.provider = Some(provider_arg.to_string());
        return Ok(());
    }
    if let Some(provider) = crate::config::ProviderKind::parse(provider_arg) {
        config.provider = Some(provider.as_str().to_string());
        return Ok(());
    }
    bail!(
        "Unrecognized --provider {provider_arg:?}. Known providers: {} \
         or a configured [providers.<name>] custom provider",
        crate::config::ProviderKind::names_hint()
    );
}

fn exec_model_env_override() -> Option<String> {
    let read = || {
        ["CODEWHALE_MODEL", "DEEPSEEK_MODEL"]
            .into_iter()
            .find_map(|key| {
                std::env::var(key)
                    .ok()
                    .map(|model| model.trim().to_string())
                    .filter(|model| !model.is_empty())
            })
    };
    #[cfg(test)]
    {
        crate::test_support::with_test_env_lock(read)
    }
    #[cfg(not(test))]
    {
        read()
    }
}

fn top_level_prompt_initial_input(parts: &[String]) -> Option<tui::InitialInput> {
    (!parts.is_empty()).then(|| tui::InitialInput::Submit(join_prompt_parts(parts)))
}

fn resolve_exec_resume_session_id(args: &ExecArgs, workspace: &Path) -> Result<Option<String>> {
    if let Some(id) = args.resume.as_ref().or(args.session_id.as_ref()) {
        return Ok(Some(id.clone()));
    }
    if !args.continue_session {
        return Ok(None);
    }
    latest_session_id_for_workspace(workspace)?.map_or_else(
        || {
            bail!(
                "No saved sessions found for workspace {}. Use `codewhale sessions` to list sessions, or pass `codewhale exec --resume <SESSION_ID> ...`.",
                workspace.display()
            )
        },
        |id| Ok(Some(id)),
    )
}

fn load_exec_resume_session(session_id: &str) -> Result<session_manager::SavedSession> {
    match SessionManager::default_location()
        .context("could not open session manager for resume")?
        .attach_session_by_prefix(session_id)
    {
        Ok((recovery, lease)) => {
            // This exec run owns the session until it exits.
            lease.commit();
            Ok(recovery.session)
        }
        // Resuming a session a TUI has open would give its document two
        // writers; the TUI's next autosave would drop this run's turns.
        Err(error) if error.kind() == io::ErrorKind::ResourceBusy => {
            bail!(exec_resume_busy_error(session_id))
        }
        Err(error) => Err(error).with_context(|| exec_resume_load_error(session_id)),
    }
}

fn exec_resume_busy_error(session_id: &str) -> String {
    format!(
        "session {} is open in another Codewhale window. Continue it there, or run \
         `codewhale fork <SESSION_ID>` and resume the copy.",
        exec_stream_session_ref(session_id)
    )
}

/// The typed `--resume` value stays redacted in every output mode: exec runs
/// in CI logs, and a mistyped or pasted value can be a secret.
fn exec_resume_load_error(session_id: &str) -> String {
    format!(
        "could not load session {}. Run `codewhale sessions` to list ids.",
        exec_stream_session_ref(session_id)
    )
}

/// Select the route for `exec --resume` before any engine/client is built.
///
/// Precedence is intentionally field-aware:
/// - no explicit `--provider` or `--model`: restore the saved provider/model;
/// - explicit `--provider`: keep that route and use its configured/default model
///   unless `--model` is also present;
/// - explicit `--model` alone: restore the saved provider, then use that model.
fn resolve_exec_resume_route(
    config: &mut Config,
    saved: &session_manager::SavedSession,
    explicit_provider: bool,
    explicit_model: Option<&str>,
) -> Result<String> {
    if !explicit_provider {
        let saved_provider_identity = saved
            .metadata
            .model_provider_id
            .as_deref()
            .filter(|identity| !identity.trim().is_empty())
            .unwrap_or(&saved.metadata.model_provider);
        let identity = config
            .resolve_persisted_provider_identity(
                Some(&saved.metadata.model_provider),
                saved.metadata.model_provider_id.as_deref(),
            )
            .map_err(anyhow::Error::msg)
            .with_context(|| {
                format!(
                    "saved session provider '{}' is unavailable; Codewhale will not fall back",
                    saved_provider_identity
                )
            })?;
        config
            .scope_to_provider_identity(&identity)
            .map_err(anyhow::Error::msg)?;
    }

    if let Some(model) = explicit_model {
        return Ok(resolve_exec_model(config, Some(model)));
    }
    if explicit_provider {
        return Ok(resolve_exec_model(config, None));
    }
    Ok(saved.metadata.model.clone())
}

/// Fold the dispatcher-forwarded launch overrides (`CODEWHALE_PROVIDER` /
/// `CODEWHALE_MODEL`, set by `codewhale --provider X --model Y exec ...`)
/// into the explicit route signals `exec --resume`/`--continue` honour.
///
/// Exec-level flags win when both are present; either source counts as
/// "the user named a route for this run", so a resume must not silently
/// restore the saved provider/model over it.
fn exec_resume_route_overrides(
    exec_provider: Option<&str>,
    exec_model: Option<&str>,
    launch_provider: Option<&str>,
    launch_model: Option<&str>,
) -> (bool, Option<String>) {
    let non_empty = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let explicit_provider =
        non_empty(exec_provider).is_some() || non_empty(launch_provider).is_some();
    let explicit_model = non_empty(exec_model).or_else(|| non_empty(launch_model));
    (explicit_provider, explicit_model)
}

#[derive(Args, Debug, Clone, Default)]
struct SetupArgs {
    /// Initialize MCP configuration at the configured path
    #[arg(long, default_value_t = false)]
    mcp: bool,
    /// Initialize skills directory and an example skill
    #[arg(long, default_value_t = false)]
    skills: bool,
    /// Initialize tools directory with a self-describing example script
    #[arg(long, default_value_t = false)]
    tools: bool,
    /// Initialize plugins directory with a self-describing example
    #[arg(long, default_value_t = false)]
    plugins: bool,
    /// Initialize MCP config, skills, tools, and plugins
    #[arg(long, default_value_t = false)]
    all: bool,
    /// Create a local workspace skills directory (./skills)
    #[arg(long, default_value_t = false)]
    local: bool,
    /// Overwrite existing template files
    #[arg(long, default_value_t = false)]
    force: bool,
    /// Print a compact, read-only status report (no network calls)
    #[arg(long, default_value_t = false, conflicts_with_all = ["mcp", "skills", "tools", "plugins", "all", "local", "clean"])]
    status: bool,
    /// Remove crash checkpoints while preserving unsent offline input
    #[arg(long, default_value_t = false, conflicts_with_all = ["mcp", "skills", "tools", "plugins", "all", "local", "status"])]
    clean: bool,
}

#[derive(Args, Debug, Clone, Default)]
struct DoctorArgs {
    /// Emit machine-readable structural JSON output (always offline)
    #[arg(long, default_value_t = false)]
    json: bool,
    /// Emit only the diagnostic context source map as JSON
    #[arg(long, default_value_t = false, conflicts_with = "json")]
    context_json: bool,
    /// Opt in to probing a local provider endpoint (may start a local service)
    #[arg(
        long,
        default_value_t = false,
        conflicts_with_all = ["json", "context_json"]
    )]
    probe_local: bool,
    /// Opt in to probing the configured hosted provider API
    #[arg(
        long,
        default_value_t = false,
        conflicts_with_all = ["json", "context_json"]
    )]
    probe_api: bool,
    /// Opt in to contacting the release service for an update check
    #[arg(
        long,
        default_value_t = false,
        conflicts_with_all = ["json", "context_json"]
    )]
    check_updates: bool,
    /// Opt in to starting enabled MCP servers and checking process/protocol reachability
    #[arg(
        long,
        default_value_t = false,
        conflicts_with_all = ["json", "context_json"]
    )]
    probe_mcp: bool,
    /// Opt in to a credential-free transport probe of the selected search provider
    #[arg(
        long,
        default_value_t = false,
        conflicts_with_all = ["json", "context_json"]
    )]
    probe_search: bool,
    /// Plan and apply automatic repairs with consent (#5552)
    #[arg(
        long,
        default_value_t = false,
        conflicts_with_all = ["json", "context_json"]
    )]
    fix: bool,
    /// Apply the planned repairs without prompting (requires --fix)
    #[arg(long, default_value_t = false, requires = "fix")]
    yes: bool,
    /// Repair the saved-session store now: re-index recoverable sessions,
    /// unbind dead thread links, set aside (never delete) what nothing uses
    #[arg(
        long,
        default_value_t = false,
        conflicts_with_all = ["json", "context_json"]
    )]
    repair_sessions: bool,
    /// With --repair-sessions: report what would be repaired, change nothing
    #[arg(long, default_value_t = false, requires = "repair_sessions")]
    dry_run: bool,
}

#[derive(Args, Debug, Clone)]
struct SessionDiagnosticsArgs {
    /// JSONL session log to inspect
    #[arg(value_name = "JSONL")]
    path: PathBuf,
    /// Emit machine-readable JSON with redacted source handles
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct ScorecardArgs {
    /// JSON file with the recorded turns to score: an array of
    /// `{ "turn_id", "provider", "model", "billing_surface", "usage": {…} }`.
    /// `turn_end` hooks emit this route provenance plus `created_at`; persisted
    /// runtime exports may instead use `id`, `effective_provider`,
    /// `effective_model`, and `effective_billing_surface`.
    /// Shell-only hook rows marked `model_backed: false` are excluded. Legacy
    /// rows without provider remain readable but their cost is unavailable.
    #[arg(long, value_name = "FILE")]
    input: PathBuf,
    /// Optional baseline scorecard-metrics JSON to compare against. When set,
    /// the command exits non-zero if any metric regresses past the threshold.
    #[arg(long, value_name = "FILE")]
    baseline: Option<PathBuf>,
    /// Regression threshold, in percent increase over the baseline.
    #[arg(long, default_value_t = 5.0, value_parser = parse_regression_threshold)]
    threshold: f64,
    /// Emit machine-readable JSON instead of the human summary.
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct EvalArgs {
    /// Intentionally fail a specific step (list, read, search, edit, patch, shell)
    #[arg(long, value_name = "STEP")]
    fail_step: Option<String>,
    /// Shell command to run during the exec step
    #[arg(long, default_value = "printf eval-harness")]
    shell_command: String,
    /// Token that must appear in shell output for validation
    #[arg(long, default_value = "eval-harness")]
    shell_expect_token: String,
    /// Maximum characters stored per step output summary
    #[arg(long, default_value_t = 240)]
    max_output_chars: usize,
    /// Emit machine-readable JSON output
    #[arg(long, default_value_t = false)]
    json: bool,
    /// Append one JSONL fixture line per step to `<DIR>/<scenario>.jsonl`.
    /// Mock LLM tests can later replay these fixtures.
    #[arg(long, value_name = "DIR")]
    record: Option<PathBuf>,
}

#[derive(Args, Debug, Clone, Default)]
struct ModelsArgs {
    /// Print models as pretty JSON
    #[arg(long, default_value_t = false)]
    json: bool,
    /// Refresh catalogs for all configured providers (no inference requests)
    #[arg(long, visible_alias = "refresh")]
    update: bool,
    /// Limit listing or refresh to this exact provider identity
    #[arg(long, value_name = "ID")]
    provider: Option<String>,
}

#[derive(Args, Debug, Clone)]
struct SpeechArgs {
    /// Text to synthesize. This is sent as the assistant message content.
    #[arg(value_name = "TEXT")]
    text: String,

    /// Output audio path. Defaults to `speech.<format>` in `--output-dir`,
    /// `[speech].output_dir`, or the current directory.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,

    /// Directory for the default `speech.<format>` output file when `-o`/`--output` is omitted.
    #[arg(long = "output-dir", value_name = "DIR")]
    output_dir: Option<PathBuf>,

    /// TTS model. Defaults to built-in voices, or is inferred from --voice-prompt/--clone-voice.
    #[arg(long)]
    model: Option<String>,

    /// Built-in voice ID, or a data:audio/...;base64,... URI for voice clone.
    #[arg(long)]
    voice: Option<String>,

    /// Natural language style instruction; not spoken verbatim.
    #[arg(long)]
    instruction: Option<String>,

    /// Voice design prompt. Implies mimo-v2.5-tts-voicedesign when --model is omitted.
    #[arg(long = "voice-prompt")]
    voice_prompt: Option<String>,

    /// MP3/WAV sample used for voice cloning. Implies mimo-v2.5-tts-voiceclone when --model is omitted.
    #[arg(long = "clone-voice", value_name = "FILE")]
    clone_voice: Option<PathBuf>,

    /// Output audio format requested from the API
    #[arg(long, default_value = "wav")]
    format: String,

    /// Emit machine-readable JSON output
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct ReviewArgs {
    /// Review staged changes instead of the working tree
    #[arg(long, conflicts_with_all = ["pr", "base"])]
    staged: bool,
    /// Review GitHub pull request #N instead of a local diff (fetched via `gh`)
    #[arg(long, conflicts_with_all = ["staged", "base", "path"])]
    pr: Option<u32>,
    /// Repository in `owner/name` form for --pr. Defaults to the current
    /// workspace's `gh` config (i.e. the repo gh thinks you're in).
    #[arg(long, requires = "pr")]
    repo: Option<String>,
    /// Post the review to the pull request (one COMMENT review with inline
    /// line comments plus a summary). Requires --pr; without it the review
    /// is only printed locally.
    #[arg(long, requires = "pr")]
    post: bool,
    /// Base ref to diff against (e.g. origin/main)
    #[arg(long, conflicts_with_all = ["staged", "pr"])]
    base: Option<String>,
    /// Limit diff to a specific path
    #[arg(long)]
    path: Option<PathBuf>,
    /// Override model for this review
    #[arg(long)]
    model: Option<String>,
    /// Override the provider route for this review (e.g. `zai`, `openrouter`,
    /// `deepseek`). Non-secret identifier only — credentials still resolve
    /// from the environment/config. Use it to disambiguate a `--model` that
    /// more than one configured route offers (which otherwise hard-errors
    /// with "available from configured provider route(s): ...").
    #[arg(long)]
    provider: Option<String>,
    /// Maximum diff characters; an oversized diff is refused, never truncated
    #[arg(long, default_value_t = 200_000)]
    max_chars: usize,
    /// Maximum complete PR review passes. Values above 1 explicitly authorize
    /// additional model requests; the default preserves single-pass behavior.
    #[arg(long, default_value_t = 1)]
    max_passes: usize,
    /// Write a durable pre-push review receipt after a successful review
    #[arg(long, default_value_t = false)]
    write_receipt: bool,
    /// Validate the current diff against a durable review receipt without calling a model
    #[arg(long, default_value_t = false)]
    check_receipt: bool,
    /// Override where the review receipt is written or read
    #[arg(long)]
    receipt_path: Option<PathBuf>,
    /// Emit machine-readable JSON output
    #[arg(long, default_value_t = false)]
    json: bool,
}

#[derive(Args, Debug, Clone)]
struct ApplyArgs {
    /// Patch file to apply (defaults to stdin)
    #[arg(value_name = "PATCH_FILE")]
    patch_file: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
struct ServeArgs {
    /// Start MCP server over stdio
    #[arg(long)]
    mcp: bool,
    /// Start runtime HTTP/SSE API server
    #[arg(long)]
    http: bool,
    /// Start runtime HTTP/SSE API server with the built-in mobile control page
    #[arg(long)]
    mobile: bool,
    /// Start the embedded loopback-only browser client and open it
    #[arg(long)]
    web: bool,
    /// Show a QR code for the mobile URL in the terminal (requires --mobile)
    #[arg(long, requires = "mobile")]
    qr: bool,
    /// Start ACP server over stdio for editor clients such as Zed
    #[arg(long)]
    acp: bool,
    /// Bind host for HTTP server (default loopback; mobile is always loopback-only)
    #[arg(long)]
    host: Option<String>,
    /// Bind port for HTTP server
    #[arg(long, default_value_t = 7878)]
    port: u16,
    /// Background task worker count (1-8)
    #[arg(long, default_value_t = 2)]
    workers: usize,
    /// Additional CORS origin to allow (repeatable). Stacks on top of the
    /// built-in defaults (localhost:3000, localhost:1420, tauri://localhost).
    /// Also reads `CODEWHALE_CORS_ORIGINS` (comma-separated), then
    /// `DEEPSEEK_CORS_ORIGINS` as an alias, and `[runtime_api] cors_origins`
    /// from `config.toml`. Whalescale#255.
    #[arg(long = "cors-origin", value_name = "URL")]
    cors_origin: Vec<String>,
    /// Require this bearer token for `/v1/*` runtime API routes. Also reads
    /// `CODEWHALE_RUNTIME_TOKEN` when omitted, then `DEEPSEEK_RUNTIME_TOKEN`
    /// as an alias.
    #[arg(long = "auth-token", value_name = "TOKEN")]
    auth_token: Option<String>,
    /// Disable runtime API auth when no token is configured. Only use on a trusted loopback.
    #[arg(long = "insecure")]
    insecure_no_auth: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ServeBindHost {
    host: String,
}

fn resolve_serve_bind_host(_mobile: bool, host: Option<String>) -> ServeBindHost {
    ServeBindHost {
        host: host.unwrap_or_else(|| "127.0.0.1".to_string()),
    }
}

fn validate_serve_mode_selection(
    mcp: bool,
    http: bool,
    mobile: bool,
    web: bool,
    acp: bool,
) -> Result<bool> {
    if http && mobile {
        bail!("--http and --mobile are mutually exclusive; choose one");
    }
    if web && (http || mobile) {
        bail!("--web is mutually exclusive with --http and --mobile");
    }
    let http_selected = http || mobile || web;
    let selected_modes = [mcp, http_selected, acp]
        .into_iter()
        .filter(|selected| *selected)
        .count();
    if selected_modes != 1 {
        bail!("Choose exactly one server mode: --mcp, --http/--mobile/--web, or --acp");
    }
    Ok(http_selected)
}

#[derive(Subcommand, Debug, Clone)]
enum McpCommand {
    /// List configured MCP servers
    List,
    /// Create a template MCP config at the configured path
    Init {
        /// Overwrite an existing MCP config file
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Connect to MCP servers and report status (does not attach to a
    /// running session)
    Connect {
        /// Optional server name to connect to
        #[arg(value_name = "SERVER")]
        server: Option<String>,
    },
    /// List tools discovered from MCP servers
    Tools {
        /// Optional server name to list tools for
        #[arg(value_name = "SERVER")]
        server: Option<String>,
    },
    /// Add an MCP server entry
    Add {
        /// Server name
        name: String,
        /// Command to launch stdio server
        #[arg(long, conflicts_with = "url")]
        command: Option<String>,
        /// URL for streamable HTTP/SSE server
        #[arg(long, conflicts_with = "command")]
        url: Option<String>,
        /// Explicit URL transport override. Use "sse" for legacy SSE endpoints.
        #[arg(long, requires = "url")]
        transport: Option<String>,
        /// Environment variable containing a bearer token for URL-based servers
        #[arg(long, requires = "url")]
        bearer_token_env_var: Option<String>,
        /// OAuth client ID for servers that do not support dynamic registration
        #[arg(long, requires = "url")]
        oauth_client_id: Option<String>,
        /// OAuth resource parameter to append to the authorization URL
        #[arg(long, requires = "url")]
        oauth_resource: Option<String>,
        /// OAuth scope to request during login. Repeat or comma-separate.
        #[arg(long = "scope", requires = "url", value_delimiter = ',')]
        scopes: Vec<String>,
        /// Arguments for command-based servers
        #[arg(long = "arg", allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Authenticate to a URL-based MCP server using OAuth
    Login {
        /// Server name
        name: String,
        /// OAuth scope to request. Repeat or comma-separate; defaults to config/discovery.
        #[arg(long = "scope", value_delimiter = ',')]
        scopes: Vec<String>,
    },
    /// Delete stored OAuth credentials for a URL-based MCP server
    Logout {
        /// Server name
        name: String,
    },
    /// Remove an MCP server entry
    Remove {
        /// Server name
        name: String,
    },
    /// Enable an MCP server
    Enable {
        /// Server name
        name: String,
    },
    /// Disable an MCP server
    Disable {
        /// Server name
        name: String,
    },
    /// Validate MCP config and required servers
    Validate,
    /// Register this Codewhale binary as a local MCP stdio server.
    ///
    /// This adds a config entry that runs `codewhale serve --mcp` (stdio protocol).
    /// For the HTTP/SSE runtime API, use `codewhale serve --http` directly instead.
    #[command(
        name = "add-self",
        long_about = "Register this Codewhale binary as a local MCP stdio server.\n\nAdds a config entry to ~/.codewhale/mcp.json that launches `codewhale serve --mcp`\nvia the stdio transport. Other Codewhale sessions (or any MCP client) can then\ndiscover and call tools exposed by this server.\n\nUse `codewhale serve --http` instead if you need the HTTP/SSE runtime API."
    )]
    AddSelf {
        /// Server name in mcp.json (default: "codewhale")
        #[arg(long, default_value = "codewhale")]
        name: String,
        /// Workspace directory for the MCP server
        #[arg(long)]
        workspace: Option<String>,
    },
}

/// The `codewhale mcp` subcommands with their one-line descriptions, for the
/// dispatcher's `codewhale mcp --help`.
///
/// That help exits in the dispatcher's parser, before this crate's parser
/// runs, and the dispatcher only sees forwarded arguments. Reading the list
/// off `McpCommand` keeps it from naming a subcommand that does not exist
/// or missing one that does.
#[must_use]
pub fn mcp_subcommand_help() -> String {
    use std::fmt::Write as _;

    let command = McpCommand::augment_subcommands(clap::Command::new("mcp"));
    let width = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name().len())
        .max()
        .unwrap_or(0);
    let mut help = String::from("Commands:\n");
    for subcommand in command.get_subcommands() {
        let about = subcommand
            .get_about()
            .map(ToString::to_string)
            .unwrap_or_default();
        let _ = writeln!(help, "  {:<width$}  {about}", subcommand.get_name());
    }
    help.push_str("\nRun `codewhale mcp <COMMAND> --help` for a command's options.");
    help
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum IntegrationsCommand {
    /// Official DeepSeek Harness (`dsh`) connected through Codewhale
    Dsh {
        #[command(subcommand)]
        command: DshIntegrationCommand,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub(crate) enum DshIntegrationCommand {
    /// Detect dsh and report the integration state without writing anything
    Status {
        /// Emit machine-readable JSON
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show exactly what `connect`/`update` would write, without writing it
    Plan {
        #[arg(long, default_value_t = false)]
        json: bool,
        /// DSH profile the overlay targets (`web` or `headless`)
        #[arg(long, default_value = "web")]
        profile: String,
        /// Mirror Codewhale full access as DSH danger-full-access (only when Codewhale itself runs with full access)
        #[arg(long, default_value_t = false)]
        allow_full_access: bool,
        /// Record the Codewhale palette (skin) decision for the bundle profile; applied via DSH's `overrideTokens`, never through the overlay
        #[arg(long, default_value_t = false)]
        skin: bool,
    },
    /// Write the overlay and receipt under $CODEWHALE_HOME/integrations/dsh
    Connect {
        #[arg(long, default_value = "web")]
        profile: String,
        #[arg(long, default_value_t = false)]
        allow_full_access: bool,
        #[arg(long, default_value_t = false)]
        skin: bool,
        /// Confirm the disclosed plan without an interactive prompt (required when stdin is not a terminal)
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Re-derive the overlay from the current Codewhale route
    Update {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long, default_value_t = false)]
        allow_full_access: bool,
        /// Turn the bundle-profile skin on/off (`--skin false`; defaults to the previous choice)
        #[arg(long)]
        skin: Option<bool>,
        /// Turn the ambient ocean scene behind the DSH web UI on/off (`--ocean false`; defaults to the previous choice, initially on; needs the skin)
        #[arg(long)]
        ocean: Option<bool>,
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Run dsh with the Codewhale overlay; extra args go to the dsh app
    Launch {
        /// Override the recorded profile (`web` or `headless`)
        #[arg(long)]
        profile: Option<String>,
        /// Print the exact command instead of running it
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Keep the overlay but refuse launches
    Disable,
    /// Allow launches again
    Enable,
    /// Delete Codewhale-owned files only; $DSH_HOME is never touched
    Remove {
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Documented DSH plugin path: install the Codewhale bundle into a dedicated `codewhale` DSH profile via `dsh plugin add` (pnpm required)
    InstallBundle {
        /// Which shipped DSH app the dedicated profile boots (`web` or `headless`)
        #[arg(long, default_value = "web")]
        app: String,
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// `dsh plugin --profile codewhale remove codewhale-dsh-bundle`, then delete only Codewhale-owned bundle files
    RemoveBundle {
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
}

#[derive(Args, Debug, Clone)]
struct FeaturesCli {
    #[command(subcommand)]
    command: FeaturesSubcommand,
}

#[derive(Subcommand, Debug, Clone)]
enum FeaturesSubcommand {
    /// List known feature flags and their state
    List,
}

#[derive(Args, Debug, Clone)]
struct SandboxArgs {
    #[command(subcommand)]
    command: SandboxCommand,
}

#[derive(Subcommand, Debug, Clone)]
enum SandboxCommand {
    /// Run a command with sandboxing
    Run {
        /// Sandbox policy (danger-full-access, read-only, external-sandbox, workspace-write)
        #[arg(long, default_value = "workspace-write")]
        policy: String,
        /// Allow outbound network access
        #[arg(long)]
        network: bool,
        /// Additional writable roots (repeatable)
        #[arg(long, value_name = "PATH")]
        writable_root: Vec<PathBuf>,
        /// Exclude TMPDIR from writable paths
        #[arg(long)]
        exclude_tmpdir: bool,
        /// Exclude /tmp from writable paths
        #[arg(long)]
        exclude_slash_tmp: bool,
        /// Command working directory
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Timeout in milliseconds
        #[arg(long, default_value_t = 60_000)]
        timeout_ms: u64,
        /// Command and arguments to run
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },
}

/// Pre-clap seam feeding `apply_process_hardening` (#5723): resolve only the
/// *startup* sandbox posture — `CODEWHALE_SANDBOX_MODE` /
/// `DEEPSEEK_SANDBOX_MODE` first, then the config file's `sandbox_mode` key —
/// so the irreversible `PR_SET_NO_NEW_PRIVS` decision can honor a
/// `danger-full-access` launch.
///
/// This is deliberately a narrow single-key read, not a config-system
/// reorder: the full pipeline (clap flags such as `--config`/`exec
/// --sandbox`, profiles, managed and project overlays) cannot run before
/// process hardening, which must land before Tokio and any worker threads.
/// What the seam cannot see keeps the hardened default — fail-closed. The
/// one deliberate gap in the other direction: a later-resolved override that
/// *tightens* a config-file `danger-full-access` (e.g. managed requirements)
/// leaves the flag off; `CODEWHALE_NO_NEW_PRIVS=1` remains the explicit
/// override that forces it on in any posture. The env path override
/// (`CODEWHALE_CONFIG_PATH`) is honored through `resolve_load_config_path`;
/// only clap-parsed paths are invisible here.
fn resolve_startup_sandbox_mode_for_hardening() -> Option<String> {
    if let Ok(value) =
        std::env::var("CODEWHALE_SANDBOX_MODE").or_else(|_| std::env::var("DEEPSEEK_SANDBOX_MODE"))
    {
        return Some(value);
    }
    let path = crate::config::resolve_load_config_path(None)
        .ok()
        .flatten()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let doc = raw.parse::<toml::Value>().ok()?;
    doc.get("sandbox_mode")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

/// Run a delegated Engine command in the canonical CLI process. Startup facts
/// are typed; `args` contains only command arguments, without a binary name.
pub fn run(options: RuntimeOptions, args: Vec<String>) -> std::process::ExitCode {
    match run_with_args(options, args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            for cause in err.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            std::process::ExitCode::FAILURE
        }
    }
}

/// Internal implementation that mirrors the old `main()` but takes explicit
/// args instead of reading `std::env::args()`. Startup facts are captured by
/// the canonical CLI and applied before config/discovery or worker startup.
fn run_with_args(options: RuntimeOptions, args: Vec<String>) -> Result<()> {
    let args: Vec<String> = std::iter::once("codewhale".to_string())
        .chain(args)
        .collect();
    // Match the dispatcher entrypoint: Unix shells and supervisors may inherit
    // SIGPIPE ignored, which turns short pipelines such as `codewhale doctor |
    // head` into BrokenPipe panics once this delegated TUI binary prints.
    // SAFETY: first call at startup; no threads or handlers yet.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    startup_trace::mark_process_start();
    configure_windows_console_utf8();
    install_rustls_crypto_provider();
    // The TUI is the terminal host for every mode this binary runs
    // (interactive, exec, serve): runtime code reaches raw mode and
    // notification delivery only through this port.
    crate::tui::ui::install_host_terminal();

    // ── Process hardening (#2183) ─────────────────────────────────────────
    // MUST run before Tokio is booted and before any threads are spawned.
    // See crates/tui/src/sandbox/process_hardening.rs for ordering rationale.
    // The startup-posture read is the narrow seam documented above: a startup
    // resolved to danger-full-access skips PR_SET_NO_NEW_PRIVS (#5723), every
    // other outcome keeps it.
    let startup_sandbox_mode = resolve_startup_sandbox_mode_for_hardening();
    crate::sandbox::process_hardening::apply_process_hardening(startup_sandbox_mode.as_deref());

    if args.get(1).is_some_and(|arg| arg == "pet") {
        let root = crate::tui::pet_watch::owner::directory()?;
        match args.get(2).map(String::as_str) {
            Some("serve") if args.len() == 3 => {
                return crate::tui::pet_watch::owner::serve(
                    root,
                    std::env::var("CODEWHALE_PET_PORT")
                        .ok()
                        .map(|p| p.parse::<u16>())
                        .transpose()?
                        .unwrap_or(4633),
                );
            }
            _ => anyhow::bail!("Usage: codewhale pet serve"),
        }
    }

    // ── Fatal-signal terminal guard (#5424) ───────────────────────────────
    // Abort-class deaths (stack overflow, allocation failure, double panic)
    // skip the panic hook AND every Drop guard, leaving mouse capture and
    // the kitty keyboard stack leaking into the user's shell. A classic
    // sigaction handler restores the terminal and stamps a marker before
    // re-raising. Also before any threads exist.
    crate::tui::ui::fatal_signal_guard::install_fatal_signal_guard();

    // Set up process panic hook before anything else — writes crash dumps
    // to the selected profile's crashes/ even before tokio is up,
    // and restores the terminal so a panicked TUI doesn't leave the user's
    // shell stuck in alt-screen mode.
    let orig_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Restore the terminal first so the panic message itself, plus the
        // user's shell after exit, are visible. Best-effort — we may not be
        // in raw / alt-screen mode if the panic happens pre-TUI. Shared
        // with the signal handler installed below so both exit paths leave
        // the terminal in the same well-defined state.
        crate::tui::ui::emergency_restore_terminal();

        let msg = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            format!("{:?}", panic_info.payload())
        };
        let location = panic_info
            .location()
            .map(|loc| loc.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        tracing::error!(target: "panic", "Process panicked at {location}: {msg}");

        // Telemetry, if and only if this process was armed. This hook is
        // installed before `Cli::parse()` and long before any config is
        // resolved, so it cannot consult a resolved value — but it can consult
        // a `OnceLock` that is by construction empty until resolution
        // completes. A user who never opted in panics without writing a byte
        // and without creating a directory.
        //
        // The site is allowlist-reduced and `msg` is deliberately not read: a
        // slicing panic embeds the entire string being sliced, and this tree
        // slices user and model text in dozens of places.
        codewhale_telemetry::set_exit_class(codewhale_telemetry::ExitClass::Panic);
        if let Some(site) = panic_info
            .location()
            .map(|loc| codewhale_telemetry::reduce_panic_site(loc.file(), loc.line(), loc.column()))
        {
            codewhale_telemetry::record_blocking(codewhale_telemetry::Event::Panic { site });
        }
        // Write crash dump best-effort
        if let Ok(home) = codewhale_config::codewhale_home() {
            let crash_dir = home.join("crashes");
            let _ = std::fs::create_dir_all(&crash_dir);
            use chrono::Utc;
            let ts = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
            let path = crash_dir.join(format!("{ts}-process-panic.log"));
            let contents =
                format!("Process panicked\nLocation: {location}\nTimestamp: {ts}\nPanic: {msg}\n",);
            let _ = std::fs::write(&path, contents);
        }
        // Invoke the original hook (prints to stderr, etc.)
        orig_hook(panic_info);
    }));

    // Parse and freeze every startup authority before Tokio or any other
    // worker thread exists. A workspace `.env` is intentionally a narrow
    // credential convenience surface: it must never redirect product state,
    // configuration, MCP, trust, sandbox, executable lookup, or plugin
    // discovery. Plugin discovery therefore runs first, and the loader below
    // admits only built-in provider credential names from a stable file read.
    let cli = match Cli::try_parse_from(args) {
        Ok(mut cli) => {
            cli.options = options.merge(cli.options)?;
            cli
        }
        Err(e) => {
            e.exit();
        }
    };
    // #5098: project-scope fleet agent profiles (`.codewhale/agents/*.toml`)
    // join the dispatch roster under the same trust decision as the rest of
    // project-level config — `--no-project-config` opts the layer out for
    // every roster read in this process.
    crate::fleet::roster::set_project_agent_profiles_enabled(!cli.no_project_config);
    let workspace = resolve_workspace(&cli);
    let mut plugin_discovery = None;
    let mut plugin_registry = None;
    let (cli, command) = prepare_cli_startup(
        cli,
        || {
            let discovery = crate::plugins::PluginDiscoveryContext::capture_pre_dotenv();
            plugin_registry = Some(discovery.registry_for_workspace(&workspace));
            plugin_discovery = Some(discovery);
        },
        || warn_on_workspace_dotenv_result(&workspace),
    );
    let plugin_discovery = plugin_discovery
        .expect("plugin discovery initialization must precede workspace dotenv loading");
    let plugin_registry = plugin_registry
        .expect("plugin discovery initialization must precede workspace dotenv loading");

    crate::plugins::providers::install_startup_registry(plugin_registry.clone());

    // The interactive runtime intentionally carries a large state machine:
    // terminal rendering, modal dispatch, provider setup, and fleet/workflow
    // events all share one async owner. Debug builds retain enough stack
    // temporaries that nesting a modal event over the TUI loop can exceed the
    // platform main-thread default (8 MiB on macOS). Give that owner an
    // explicit stack while keeping process hardening and the global panic hook
    // above this boundary, before Tokio or any worker thread exists.
    //
    // 16 MiB stopped being enough: in a debug build the deepest measured
    // chain — the event-loop poll stack down to the trust-confirm engine
    // respawn (`handle_view_events` → `apply_command_result` →
    // `spawn_tui_engine` → `Engine::new` → `CodewhaleClient::new` →
    // `resolve_runtime_route` → `Config::clone`) — consumed ~16.5 MiB and
    // aborted on the guard page (the plugin_toml_binary cucumber acceptance
    // on the ubuntu CI leg). The fat frames are the debug poll functions of
    // the giant top-level futures (`run_async_main_inner` ~5.5 MiB, `run_tui`
    // ~3.2 MiB, `handle_view_events` ~2.1 MiB), so any small addition to them
    // re-tips a zero-margin stack. 32 MiB restores real headroom; the cost is
    // address space only, since thread stacks commit lazily.
    let runtime_thread = std::thread::Builder::new()
        .name("codewhale-main".to_string())
        .stack_size(codewhale_runtime::CODEWHALE_MAIN_STACK_BYTES)
        .spawn(move || run_async_main(cli, command, plugin_discovery, plugin_registry))
        .context("Failed to start the Codewhale runtime thread")?;
    match runtime_thread.join() {
        Ok(result) => result,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|value| (*value).to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic payload".to_string());
            Err(anyhow!("Codewhale runtime thread panicked: {message}"))
        }
    }
}

fn run_async_main(
    cli: Cli,
    command: Option<Commands>,
    plugin_discovery: Arc<crate::plugins::PluginDiscoveryContext>,
    plugin_registry: Arc<crate::plugins::PluginRegistry>,
) -> Result<()> {
    build_runtime(command.as_ref())?.block_on(run_async_main_inner(
        cli,
        command,
        plugin_discovery,
        plugin_registry,
    ))
}

/// Build the runtime that owns every async task in this binary.
///
/// `#[tokio::main]` used to expand here, which left every worker thread on
/// tokio's 2 MiB default while only the `codewhale-main` owner thread above
/// received `CODEWHALE_MAIN_STACK_BYTES`. The engine does not run on that owner
/// thread — `core::engine::spawn_engine` hands `Engine::run` to
/// `utils::spawn_supervised`, a bare `tokio::spawn` — so the explicit stack
/// never applied where the depth actually is.
///
/// A debug-build `agent` dispatch (turn_loop -> FuturesUnordered ->
/// execute_rich_full_with_context -> AgentTool::execute -> spawn_subagent_from_input)
/// measured a stack high-water mark between 2.25 and 2.5 MiB and aborted the
/// whole process on the guard page. A Rust stack overflow is not a panic: it
/// raises SIGABRT, so `spawn_supervised`'s `catch_unwind` cannot see it and the
/// process dies with 134 mid-dispatch.
///
/// Build the runtime that owns every async task in this binary.
///
/// `command` selects the worker-count policy: read-only diagnostic commands
/// run on a small fixed pool instead of tokio's one-worker-per-CPU default
/// (see [`diagnostic_worker_count`]). Interactive sessions and servers keep
/// the default sizing unchanged.
///
/// `#[tokio::main]` used to expand here, which left every worker thread on
/// tokio's 2 MiB default while only the `codewhale-main` owner thread above
/// received `CODEWHALE_MAIN_STACK_BYTES`. The engine does not run on that owner
/// thread — `core::engine::spawn_engine` hands `Engine::run` to
/// `utils::spawn_supervised`, a bare `tokio::spawn` — so the explicit stack
/// never applied where the depth actually is.
///
/// A debug-build `agent` dispatch (turn_loop -> FuturesUnordered ->
/// execute_rich_full_with_context -> AgentTool::execute -> spawn_subagent_from_input)
/// measured a stack high-water mark between 2.25 and 2.5 MiB and aborted the
/// whole process on the guard page. A Rust stack overflow is not a panic: it
/// raises SIGABRT, so `spawn_supervised`'s `catch_unwind` cannot see it and the
/// process dies with 134 mid-dispatch.
///
/// This is behavior-identical to the old `#[tokio::main]` expansion apart from
/// the stack size, and it makes the knob greppable.
pub(crate) fn build_runtime(command: Option<&Commands>) -> Result<tokio::runtime::Runtime> {
    let mut builder = tokio_runtime_builder();
    if let Some(workers) = diagnostic_worker_count(command) {
        builder.worker_threads(workers);
    }
    builder
        .build()
        .context("Failed to build the Codewhale Tokio runtime")
}

/// Number of async workers to request from tokio.
///
/// Unset means tokio's default: one worker per CPU. That default is right for
/// interactive sessions and long-running servers, but short-lived offline
/// commands gain nothing from a full-CPU pool — they pay thread spawn, stack
/// reservation, and teardown futex traffic for capacity they never use (perf
/// attribution: pthread_create under `Builder::build` dominates init samples).
const DIAGNOSTIC_WORKER_CAP: usize = 2;

fn diagnostic_worker_count(command: Option<&Commands>) -> Option<usize> {
    let capped = match command {
        // Read-only diagnostic surfaces (doctor family).
        Some(
            Commands::Doctor(_)
            | Commands::Eval(_)
            | Commands::SessionDiagnostics(_)
            | Commands::Sessions { .. }
            | Commands::Receipts { .. },
        ) => true,
        // Only the read-only status report; mutating setup keeps defaults.
        Some(Commands::Setup(args)) => args.status,
        _ => false,
    };
    capped.then_some(DIAGNOSTIC_WORKER_CAP)
}

fn tokio_runtime_builder() -> tokio::runtime::Builder {
    let mut builder = tokio::runtime::Builder::new_multi_thread();
    builder
        .enable_all()
        .thread_stack_size(codewhale_runtime::CODEWHALE_MAIN_STACK_BYTES);
    builder
}

/// Which product surface this process is serving.
///
/// A function of the parsed subcommand, never of the executable: this one
/// binary serves at least five surfaces, so `current_exe()` would label all of
/// them the same.
fn telemetry_surface(command: Option<&Commands>) -> codewhale_telemetry::Surface {
    telemetry_surface_with(command, embedded_surface_override())
}

/// The injectable form of [`telemetry_surface`].
///
/// The declared surface is a parameter rather than an environment read inside
/// the match, so the "only the server branch consults it" rule can be proven
/// without a test mutating process-wide environment — which would race every
/// other test in this binary.
fn telemetry_surface_with(
    command: Option<&Commands>,
    declared: Option<codewhale_telemetry::Surface>,
) -> codewhale_telemetry::Surface {
    use codewhale_telemetry::Surface;
    match command {
        None | Some(Commands::Resume { .. } | Commands::Fork { .. } | Commands::Pr { .. }) => {
            Surface::Tui
        }
        Some(Commands::Exec(_)) => Surface::Exec,
        Some(Commands::Serve(args)) => {
            if args.mcp {
                Surface::McpServer
            } else {
                declared.unwrap_or(Surface::Serve)
            }
        }
        Some(_) => Surface::Cli,
    }
}

/// The surface name an embedding client declared for a server it started.
///
/// An API server is the one surface a third party legitimately *starts*, so it
/// is the one surface an embedder may name. The editor extension, for instance,
/// runs `codewhale serve --http` and declares itself, so its sessions are not
/// counted as anonymous `serve` traffic from nobody in particular.
///
/// Three properties are deliberate:
///
/// 1. **`serve` only.** The variable is inherited by any process this one
///    starts, and an agent running a shell command is running `codewhale`
///    descendants with this environment. A nested `codewhale exec` that
///    inherited the label would report itself as the embedder — wrong, and
///    invisible. Consulting it only in the branch that *is* the embedder's own
///    server bounds that to a process the embedder actually started.
/// 2. **`tui` is refused.** Nothing starts the interactive terminal UI on a
///    user's behalf, and reporting a server as `tui` would put it in the one
///    surface whose numbers are the terminal's. `mcp` is decided before this is
///    consulted, so a server that *is* an MCP server still says so.
/// 3. **Unrecognised is not an error.** A name outside [`Surface::ALL`] is
///    ignored and the caller falls back, rather than failing a startup over a
///    field that is only a label. The value is not "sanitised" into a variant
///    it resembles; it is dropped.
fn embedded_surface_override() -> Option<codewhale_telemetry::Surface> {
    embedded_surface_override_from(std::env::var("CODEWHALE_TELEMETRY_SURFACE").ok())
}

/// The injectable form of [`embedded_surface_override`], used by tests.
///
/// Separate from the environment read for the same reason
/// `load_setup_state_for_decision_at` is separate from
/// `load_setup_state_for_decision`: a test that mutated process-wide
/// environment for this would race every other test in the binary.
fn embedded_surface_override_from(raw: Option<String>) -> Option<codewhale_telemetry::Surface> {
    use codewhale_telemetry::Surface;
    let surface = Surface::parse(raw?.trim())?;
    if surface == Surface::Tui {
        return None;
    }
    Some(surface)
}

/// How this session was started, for `session_start`.
fn telemetry_session_source(command: Option<&Commands>) -> codewhale_telemetry::SessionSource {
    use codewhale_telemetry::SessionSource;
    match command {
        None | Some(Commands::Pr { .. }) => SessionSource::Interactive,
        Some(Commands::Resume { .. }) => SessionSource::Resume,
        Some(Commands::Fork { .. }) => SessionSource::Fork,
        Some(Commands::Serve(_)) => SessionSource::Api,
        Some(_) => SessionSource::Unknown,
    }
}

/// Read-only commands must not create telemetry state as a side effect.
// Sessions export only projects local records to an explicit output. Like
// listing, it must not initialize telemetry or interactive session state.
fn telemetry_command_is_read_only(command: Option<&Commands>) -> bool {
    matches!(
        command,
        Some(
            Commands::Doctor(_)
                | Commands::SessionDiagnostics(_)
                | Commands::Sessions { .. }
                | Commands::Receipts { .. }
        )
    ) || matches!(command, Some(Commands::Setup(args)) if args.status)
}

/// Resolve the emit predicate and arm, once, before anything can record.
///
/// This is the read that v1 of the design was missing entirely:
/// `resolve_runtime_options` had no non-test caller in this crate, so neither
/// `telemetry = false` in the config file nor `CODEWHALE_TELEMETRY=0` was ever
/// consulted by a process that would have emitted.
///
/// `CliRuntimeOverrides::default()` is correct here. The dispatcher has already
/// applied the kill-switch floor and forwarded the *resolved* value through
/// `CODEWHALE_TELEMETRY`, which `EnvRuntimeOverrides::load()` picks up — and
/// re-reading `CODEWHALE_TELEMETRY` inside the telemetry crate would fork
/// `parse_bool`, the `DEEPSEEK_TELEMETRY` alias, and the floor into a second
/// source of truth.
fn arm_telemetry_with_setup(
    config_path: Option<PathBuf>,
    surface: codewhale_telemetry::Surface,
    source: codewhale_telemetry::SessionSource,
    setup_override: Option<&codewhale_config::SetupState>,
) {
    let Ok(store) = codewhale_config::ConfigStore::load(config_path) else {
        return;
    };
    let resolved = store
        .config
        .resolve_runtime_options(&codewhale_config::CliRuntimeOverrides::default());
    let setup = if let Some(setup) = setup_override {
        setup.clone()
    } else {
        let Some(setup) = codewhale_telemetry::load_setup_state_for_decision() else {
            // An existing unreadable privacy record may contain a decline.
            // Failing closed is safer than replacing it with default-on.
            return;
        };
        setup
    };
    let codewhale_telemetry::TelemetryDecision::Enabled(consent) =
        codewhale_telemetry::decide(&resolved, &setup, surface)
    else {
        return;
    };
    codewhale_telemetry::init(consent.with_config_path(Some(store.path().to_path_buf())));
    let _ = TELEMETRY_SESSION_START.set(std::time::Instant::now());
    codewhale_telemetry::record(codewhale_telemetry::Event::SessionStart { source });
}

fn arm_telemetry(cli: &Cli, command: Option<&Commands>) {
    if telemetry_command_is_read_only(command) {
        return;
    }
    arm_telemetry_with_setup(
        cli.config.clone(),
        telemetry_surface(command),
        telemetry_session_source(command),
        None,
    );
}

/// Non-secret account label (email, plan) of the Codewhale-owned
/// subscription sign-in stored in `generation`, for `codewhale auth status`
/// and `auth list`. Reads only that Codewhale-owned file: no refresh, no
/// network, no external CLI file. `Err` carries a fixed, token-free reason
/// the sign-in is unusable (missing, unreadable, no usable entry).
pub fn owned_oauth_account_label(
    provider: codewhale_config::ProviderKind,
    generation: &str,
) -> Result<Option<String>> {
    let provider = match provider {
        codewhale_config::ProviderKind::OpenaiCodex => oauth::OAuthProvider::Chatgpt,
        codewhale_config::ProviderKind::Xai => oauth::OAuthProvider::Xai,
        _ => bail!("provider has no subscription sign-in"),
    };
    oauth::owned_account_label_for_generation(provider, generation)
}

/// Whether a resolved route is the canonical public ChatGPT API endpoint.
/// CLI diagnostics use the same destination check as inference dispatch.
#[must_use]
pub fn is_official_chatgpt_api_base(base_url: &str) -> bool {
    pricing::is_official_chatgpt_api(base_url)
}

/// Compatibility command to explicitly enable usage under the current policy.
/// This optional choice never arms the current process.
pub fn accept_telemetry_notice(config_path: Option<PathBuf>, version: u32) -> Result<String> {
    if version != codewhale_telemetry::NOTICE_VERSION {
        anyhow::bail!(
            "read `codewhale config telemetry`, then explicitly accept current notice version {}",
            codewhale_config::TELEMETRY_NOTICE_VERSION
        );
    }
    set_telemetry_preference(config_path, true)
}

/// Set the durable usage preference through the same transition as Settings.
/// Enabling affects the next launch; disabling immediately erases queued usage.
pub fn set_telemetry_preference(config_path: Option<PathBuf>, enabled: bool) -> Result<String> {
    let applied = crate::telemetry_notice::apply_persistent_preference(config_path, enabled);
    let message = applied.message(codewhale_localization::Locale::En);
    if applied.is_error() {
        anyhow::bail!("{message}");
    }
    Ok(message)
}

/// Close the armed session and flush, bounded.
///
/// Short CLI (`config`, `doctor`, `auth`, …) records `session_end`, seals the
/// local queue within a much smaller deadline, and returns. The 3s network
/// flush is a TUI/exec concern: a hung TLS handshake must not hold
/// `codewhale config list`. A configured endpoint remains buffered for the
/// next interactive session; an explicitly empty endpoint writes its local
/// dry-run batch immediately.
async fn finish_telemetry(outcome: &Result<()>, surface: codewhale_telemetry::Surface) {
    if !codewhale_telemetry::is_armed() {
        return;
    }
    // Only escalate: the panic hook and the signal path have already spoken if
    // they ran, and a stated class must not be overwritten by an inferred one.
    if outcome.is_err()
        && codewhale_telemetry::exit_class() == codewhale_telemetry::ExitClass::Clean
    {
        codewhale_telemetry::set_exit_class(codewhale_telemetry::ExitClass::Error);
    }
    codewhale_telemetry::record(telemetry_session_end());
    if surface == codewhale_telemetry::Surface::Cli {
        let persistence = codewhale_telemetry::persist_local_blocking();
        logging::info(format!(
            "telemetry local persistence outcome={persistence:?}"
        ));
        return;
    }
    // `shutdown_blocking` parks a thread waiting on the writer, so it goes to
    // the blocking pool, and it is bounded there. The persistence actor's
    // unbounded `let _ = task.await` next door is not a pattern to copy here: a
    // hung TLS handshake would hold the process open past the last frame.
    let _ = tokio::time::timeout(
        codewhale_telemetry::SHUTDOWN_FLUSH_TIMEOUT,
        tokio::task::spawn_blocking(|| {
            codewhale_telemetry::shutdown_blocking(codewhale_telemetry::SHUTDOWN_FLUSH_TIMEOUT)
        }),
    )
    .await;
}

async fn run_async_main_inner(
    cli: Cli,
    command: Option<Commands>,
    plugin_discovery: Arc<crate::plugins::PluginDiscoveryContext>,
    plugin_registry: Arc<crate::plugins::PluginRegistry>,
) -> Result<()> {
    // Install signal handlers that restore the terminal before the process
    // exits. Without this, Ctrl+C delivered while raw mode / kitty keyboard
    // enhancement / alt-screen are active (or in the brief windows around
    // startup and teardown where they're being toggled) leaves the user's shell
    // receiving raw CSI sequences like `^[[>5u` until they run `reset` (#1583).
    //
    // Once the TUI's raw mode is engaged the terminal driver delivers Ctrl+C as
    // the byte 0x03 rather than SIGINT, so the in-TUI key handler — not this
    // handler — is what processes user interrupts during normal operation. This
    // handler exists for the gaps: pre-TUI subcommands (--version, doctor,
    // login, …), the moments around enable_raw_mode / disable_raw_mode, the
    // external-editor suspend path, and SIGTERM / SIGHUP from the OS.
    //
    // It goes up before arming and before the notice: arming is the first
    // externally observable thing this process does (it creates the telemetry
    // buffer), and the notice is the first thing that can sit waiting on a
    // human. A Ctrl-C in either window must still restore the terminal and exit
    // 130 rather than kill the process outright. Recording a `session_end` from
    // the signal path is a no-op until `arm_telemetry` runs, so installing
    // ahead of it collects nothing.
    spawn_signal_cleanup_task();

    // A due interactive disclosure belongs to the native TUI. Presentation
    // does not gate the default-on policy; unreadable privacy state does.
    // Settings can disable this session and erase its pending aggregates.
    let surface = telemetry_surface(command.as_ref());
    let telemetry_notice_plan = if surface == codewhale_telemetry::Surface::Tui {
        crate::telemetry_notice::plan_if_due(
            cli.config.clone(),
            telemetry_session_source(command.as_ref()),
        )
    } else {
        crate::telemetry_notice::TelemetryNoticePlan::NotDue
    };
    let should_arm_before_dispatch = surface != codewhale_telemetry::Surface::Tui
        || telemetry_notice_plan.should_arm_before_tui();
    let pending_telemetry_notice = telemetry_notice_plan.into_pending();
    if should_arm_before_dispatch {
        arm_telemetry(&cli, command.as_ref());
    }
    let outcome = run_async_main_dispatch(
        cli,
        command,
        plugin_discovery,
        plugin_registry,
        pending_telemetry_notice,
    )
    .await;
    finish_telemetry(&outcome, surface).await;
    outcome
}

async fn run_async_main_dispatch(
    cli: Cli,
    command: Option<Commands>,
    plugin_discovery: Arc<crate::plugins::PluginDiscoveryContext>,
    plugin_registry: Arc<crate::plugins::PluginRegistry>,
    mut pending_telemetry_notice: Option<crate::telemetry_notice::PendingTelemetryNotice>,
) -> Result<()> {
    logging::set_verbose(cli.verbose || logging::env_requests_verbose_logging());
    anyhow::ensure!(
        cli.operation_key.is_none()
            || matches!(
                command.as_ref(),
                Some(Commands::Resume { .. } | Commands::Fork { .. })
            ),
        "--operation-key is an exact read-only recovery of an explicit resume/fork intent"
    );

    // Install any user prompt overrides from the config directory before an
    // engine can compose a system prompt. The override cells are
    // first-call-wins; doing this once here keeps every downstream turn
    // consistent. Missing files are a no-op (bundled defaults). See #3638.
    crate::prompts::load_prompt_overrides_from_config_home();

    // Plugins own one read-only discovery snapshot per process. Initialize it
    // before the subcommand match so plain launch, resume, fork, exec, serve,
    // and every other runtime surface use the same plugin trust decision
    // (#3916, #4399). Discovery never enables, trusts, executes, or persists a
    // bundle.

    // Handle subcommands first
    if let Some(command) = command {
        return match command {
            Commands::Doctor(args) => {
                let config = match load_doctor_config_from_cli(&cli, &args) {
                    Ok(config) => config,
                    Err(error) if args.json => return run_doctor_json_config_error(&error),
                    Err(error) => bail!(doctor_config_error_text(&error)),
                };
                let workspace = resolve_workspace(&cli);
                if args.repair_sessions {
                    return run_doctor_repair_sessions(args.dry_run);
                }
                if args.context_json {
                    run_doctor_context_json(&config, &workspace)
                } else if args.json {
                    run_doctor_json(
                        &config,
                        &workspace,
                        cli.config.as_deref(),
                        plugin_registry.as_ref(),
                    )
                } else {
                    let probes = crate::doctor::DoctorProbeRequest {
                        check_updates: args.check_updates,
                        probe_api: args.probe_api,
                        probe_local: args.probe_local,
                        probe_mcp: args.probe_mcp,
                        probe_search: args.probe_search,
                    };
                    run_doctor(
                        &config,
                        &workspace,
                        cli.config.as_deref(),
                        effective_config_profile(&cli).as_deref(),
                        probes,
                        plugin_registry.as_ref(),
                    )
                    .await;
                    if args.fix {
                        let plan = crate::doctor_fix::plan_fixes(
                            &config,
                            &workspace,
                            plugin_registry.as_ref(),
                        );
                        crate::doctor_fix::print_fix_plan(&plan);
                        if !plan.is_empty() && (args.yes || crate::doctor_fix::confirm_fix(&plan)) {
                            let results = crate::doctor_fix::apply_fixes(&plan);
                            crate::doctor_fix::print_apply_results(&results);
                        }
                    }
                    Ok(())
                }
            }
            Commands::SessionDiagnostics(args) => run_session_diagnostics(args),
            Commands::Install { source } => {
                let config = load_config_from_cli(&cli)?;
                crate::plugins::mutation::install_from_cli(
                    source,
                    config,
                    (*plugin_registry).clone(),
                )
                .await
            }
            Commands::Setup(args) => {
                let config = load_config_from_cli(&cli)?;
                let workspace = resolve_workspace(&cli);
                run_setup(&config, &workspace, args, plugin_registry.as_ref())
            }
            Commands::RemoteSetup(args) => remote_setup::run_remote_setup(args),
            Commands::Sessions {
                command,
                limit,
                search,
            } => match command {
                None => list_sessions(limit, search),
                Some(SessionsCommand::List { limit, search }) => list_sessions(limit, search),
                Some(SessionsCommand::ScrubSecrets { apply }) => {
                    run_sessions_scrub_secrets(
                        apply,
                        cli.config.clone(),
                        effective_config_profile(&cli),
                    )
                    .await
                }
                Some(SessionsCommand::Export {
                    id,
                    output,
                    skip_artifacts,
                    compression,
                    force,
                }) => {
                    run_sessions_export(&id, output.as_deref(), skip_artifacts, compression, force)
                }
            },
            Commands::Receipts {
                id,
                last,
                turn,
                format,
            } => receipts::run_receipts_command(
                if last { None } else { id.as_deref() },
                turn.as_deref(),
                format,
            ),
            Commands::Init => init_project(),
            Commands::Login { api_key } => run_login(api_key),
            Commands::Logout => run_logout(),
            Commands::Auth(args) => match args.command {
                TuiAuthCommand::XaiDevice => run_xai_device_auth(cli.config.as_deref()).await,
                TuiAuthCommand::Claude => run_claude_auth(cli.config.as_deref()).await,
                TuiAuthCommand::ClaudeRevoke => {
                    crate::oauth::revoke_owned_login(
                        crate::oauth::OAuthProvider::Claude,
                        cli.config.as_deref(),
                        None,
                    )?;
                    println!(
                        "Removed Codewhale's saved Claude sign-in. Manage remote access in Claude account settings."
                    );
                    Ok(())
                }
                TuiAuthCommand::Chatgpt => run_chatgpt_pkce_auth(cli.config.as_deref()).await,
                TuiAuthCommand::ChatgptRevoke => run_chatgpt_pkce_revoke(cli.config.as_deref()),
                TuiAuthCommand::Orcarouter => run_orcarouter_pkce_auth(cli.config.as_deref()).await,
                TuiAuthCommand::OrcarouterRevoke => run_orcarouter_revoke(cli.config.as_deref()),
                TuiAuthCommand::PluginLogin { provider } => {
                    let entry = plugin_auth_entry_from_cli(&cli, &provider).await?;
                    crate::oauth::plugin_oauth_login(
                        provider,
                        entry.base_url.clone().unwrap(),
                        entry.oauth.clone().unwrap(),
                        entry.plugin_authority.clone().unwrap(),
                    )
                    .await
                }
                TuiAuthCommand::PluginLogout { provider } => {
                    let entry = plugin_auth_entry_from_cli(&cli, &provider).await?;
                    let policy = crate::plugins::activation::extension_host_policy_enabled();
                    tokio::task::spawn_blocking(move || {
                        let _scope = crate::plugins::activation::PolicyScope::propagate(policy);
                        crate::plugins::registry::verify_plugin_component_authority(
                            entry.plugin_authority.as_ref().unwrap(),
                            crate::plugins::activation::PluginActivationCapability::Providers,
                        )
                        .map_err(anyhow::Error::msg)?;
                        crate::oauth::plugin_oauth_logout(
                            &provider,
                            entry.base_url.as_deref().unwrap(),
                            entry.oauth.as_ref().unwrap(),
                        )
                    })
                    .await?
                }
            },
            Commands::Models(args) => {
                let config = load_config_from_cli(&cli)?;
                initialize_cloud_facts(&config);
                run_models(&config, args).await
            }
            Commands::Speech(args) => {
                let config = load_config_from_cli(&cli)?;
                run_speech(&config, args).await
            }
            Commands::Exec(args) => {
                let config = load_config_from_cli(&cli)?;
                let plugin_registry = policy_current_registry(plugin_registry);
                let workspace = cli.workspace.clone().unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                });
                let mut config = config.clone();
                // #4641: `--no-project-config` skips the workspace-specific
                // `[workspace]`/`[projects]` user-config overlay so a headless
                // launch (e.g. a future Verifiers harness) sees a reproducible
                // config surface that depends only on the explicit `--config`.
                if !cli.no_project_config {
                    merge_user_workspace_config(&mut config, cli.config.clone(), &workspace);
                }
                if let Some(sandbox) = args.sandbox.as_deref() {
                    let _ = parse_sandbox_policy(sandbox, true, Vec::new(), false, false)?;
                    config.sandbox_mode = Some(sandbox.to_ascii_lowercase());
                }
                // Honour CODEWHALE_BASE_URL (forwarded by the CLI dispatcher
                // from --base-url) or the user-set legacy DEEPSEEK_BASE_URL.
                if let Ok(env_url) = std::env::var("CODEWHALE_BASE_URL")
                    .or_else(|_| std::env::var("DEEPSEEK_BASE_URL"))
                {
                    let trimmed = env_url.trim();
                    if !trimmed.is_empty() {
                        let identity = config
                            .active_provider_identity()
                            .map_err(anyhow::Error::msg)?;
                        config
                            .set_provider_base_url_override(&identity, Some(trimmed.to_string()))
                            .map_err(anyhow::Error::msg)?;
                    }
                }
                // Honour `--provider` (#4093): a Fleet worker whose profile pins
                // a provider launches on that provider even when the parent
                // session is on another one. This sets ONLY the non-secret
                // provider identity (`config.provider`); credentials/base URL
                // still resolve from the worker's own env/config, and the
                // endpoint above stays in the previously active provider's
                // own table, so a different pinned provider never reads it.
                // Must precede model
                // resolution so an `auto`/default model resolves to the
                // overridden provider's default.
                let explicit_provider = args
                    .provider
                    .as_deref()
                    .map(str::trim)
                    .filter(|provider| !provider.is_empty());
                if let Some(provider_arg) = explicit_provider {
                    apply_exec_provider_override(&mut config, provider_arg)?;
                }
                let explicit_reasoning = args
                    .reasoning_effort
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                if let Some(reasoning_arg) = explicit_reasoning {
                    config.reasoning_effort = normalize_cli_reasoning_effort(reasoning_arg)?;
                    config.reasoning_effort_inferred_from_legacy_alias = false;
                }
                initialize_cloud_facts(&config);
                // #6705: OpenCode Zen's per-model wire comes from its
                // Models.dev catalog. Seed the persisted snapshot (disk only,
                // no network) so exec routes the models the picker offers
                // instead of only the ones compiled into this build. This runs
                // unconditionally, as in the interactive path: the final route
                // is not known yet (a selected Fleet operator or `--resume` can
                // still move it onto Zen, and both resolve through the lake).
                // The read and JSON parse are blocking, so they leave the
                // runtime's worker threads.
                if let Err(error) =
                    tokio::task::spawn_blocking(crate::models_dev_live::maybe_load_persisted_cache)
                        .await
                {
                    tracing::warn!(
                        target: "models_dev_live",
                        %error,
                        "persisted Models.dev cache load did not complete; keeping bundled"
                    );
                }
                let prompt = resolve_exec_prompt(&args)?;
                let resume_session_id = resolve_exec_resume_session_id(&args, &workspace)?;
                validate_exec_tool_authority_resume(
                    args.tool_authority_json.as_deref(),
                    resume_session_id.is_some(),
                )?;
                let resume_session = resume_session_id
                    .as_deref()
                    .map(load_exec_resume_session)
                    .transpose()?;
                let explicit_model = args
                    .model
                    .as_deref()
                    .map(str::trim)
                    .filter(|model| !model.is_empty());
                if resume_session.is_none() {
                    let explicit_route_override = explicit_provider.is_some()
                        || explicit_model.is_some()
                        || crate::config::explicit_launch_provider_override().is_some()
                        || crate::config::explicit_launch_model_override().is_some();
                    apply_selected_fleet_operator_for_launch(
                        &mut config,
                        &workspace,
                        explicit_route_override,
                        explicit_reasoning.is_some(),
                    )?;
                }
                // The `codewhale` dispatcher refuses `--provider`/`--model`
                // after `exec` and forwards the top-level flags as
                // `CODEWHALE_PROVIDER` / `CODEWHALE_MODEL` instead, so a
                // resume must treat those launch overrides as explicit or it
                // silently restores the saved route (cloud-agent e2e,
                // 2026-08-30).
                let (resume_explicit_provider, resume_explicit_model) = exec_resume_route_overrides(
                    explicit_provider,
                    explicit_model,
                    crate::config::explicit_launch_provider_override().as_deref(),
                    crate::config::explicit_launch_model_override().as_deref(),
                );
                let model = if let Some(saved) = resume_session.as_ref() {
                    resolve_exec_resume_route(
                        &mut config,
                        saved,
                        resume_explicit_provider,
                        resume_explicit_model.as_deref(),
                    )?
                } else {
                    resolve_exec_model(&config, explicit_model)
                };
                let force_configured_route = should_force_configured_exec_route(
                    resume_session.is_some(),
                    explicit_provider,
                    explicit_model,
                );
                // A launcher can forward `--yolo` to this binary via the
                // CODEWHALE_YOLO env var (which the config loader folds into
                // `config.yolo`), not as a CLI flag. Honour either source.
                let yolo = cli.yolo || config.yolo.unwrap_or(false);
                let env_tool_surface = exec_tool_surface_from_env();
                // #6510: every exec runs on the Engine — one turn loop, one
                // prompt authority (BASE_PROMPT, AGENTS.md, skills). Without
                // a tool grant the run is a one-shot answer on a zero-tool
                // surface.
                let tool_surface_requested = exec_grants_tool_surface(
                    &args,
                    yolo,
                    resume_session_id.is_some(),
                    env_tool_surface.is_some(),
                );
                {
                    if args.parent_death_watch {
                        spawn_parent_death_watch();
                    }
                    let identity = config
                        .active_provider_identity()
                        .map_err(anyhow::Error::msg)?;
                    let max_subagents = cli.max_subagents.map_or_else(
                        || config.max_subagents_for_provider(&identity),
                        |value| value.clamp(1, MAX_SUBAGENTS),
                    );
                    let auto_mode = args.auto || yolo;
                    // A zero-tool run only spends model steps on output-limit
                    // continuations; without `--max-turns` those would run to
                    // the turn wall clock, so it gets a small default ceiling.
                    let max_turns = exec_max_steps(
                        args.max_turns
                            .or((!tool_surface_requested).then_some(ONE_SHOT_DEFAULT_MAX_STEPS)),
                    );
                    if !tool_surface_requested {
                        let ignored = exec_tool_flags_without_grant(&args);
                        if !ignored.is_empty() {
                            eprintln!(
                                "codewhale exec: {} only apply to tools, and this run offers none; add --auto or --allowed-tools to run with tools.",
                                ignored.join(", ")
                            );
                        }
                    }
                    let allowed_tools = if tool_surface_requested {
                        resolve_exec_allowed_tools(args.allowed_tools.as_deref(), env_tool_surface)
                    } else {
                        Some(Vec::new())
                    };
                    let disallowed_tools = args
                        .disallowed_tools
                        .as_deref()
                        .map(normalize_exec_tool_names);
                    run_exec_agent(
                        &config,
                        &model,
                        &prompt,
                        workspace,
                        max_subagents,
                        auto_mode,
                        args.allow_sandbox_elevation,
                        args.sandbox.as_deref(),
                        auto_mode,
                        args.json,
                        resume_session,
                        force_configured_route,
                        args.output_format,
                        max_turns,
                        args.max_tool_calls,
                        allowed_tools,
                        disallowed_tools,
                        args.append_system_prompt.clone(),
                        args.tool_authority_json.clone(),
                        args.hooks,
                        std::sync::Arc::clone(&plugin_registry),
                        !tool_surface_requested,
                    )
                    .await
                }
            }
            Commands::Fleet(args) => {
                let config = load_config_from_cli(&cli)?;
                let workspace = resolve_workspace(&cli);
                run_fleet_command(&workspace, &config, args).await
            }
            Commands::WorkflowTool(args) => {
                run_workflow_tool_command(&cli, args, std::sync::Arc::clone(&plugin_registry)).await
            }
            Commands::Review(args) => {
                let config = load_config_from_cli(&cli)?;
                run_review(&config, args).await
            }
            Commands::Pr {
                number,
                repo,
                checkout,
            } => {
                let config = load_config_from_cli(&cli)?;
                run_pr(
                    &cli,
                    &config,
                    number,
                    repo.as_deref(),
                    checkout,
                    pending_telemetry_notice.take(),
                    Arc::clone(&plugin_registry),
                )
                .await
            }
            Commands::Apply(args) => run_apply(args),
            Commands::Eval(args) => run_eval(args),
            Commands::Scorecard(args) => run_scorecard(args),
            Commands::Mcp { command } => {
                let config = load_config_from_cli(&cli)?;
                let workspace = resolve_workspace(&cli);
                run_mcp_command(&config, &workspace, command, plugin_registry.as_ref()).await
            }
            Commands::Features(command) => {
                let config = load_config_from_cli(&cli)?;
                run_features_command(&config, command)
            }
            Commands::Integrations { command } => {
                // Identity derivation is structural: credential-bearing
                // environment values never enter this path.
                let config = load_structural_config_from_cli(&cli)?;
                let workspace = resolve_workspace(&cli);
                integrations::cli::run(&config, &workspace, command)
            }
            Commands::Sandbox(args) => run_sandbox_command(args),
            Commands::Serve(args) => {
                let workspace = cli.workspace.clone().unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                });
                let http_selected = validate_serve_mode_selection(
                    args.mcp,
                    args.http,
                    args.mobile,
                    args.web,
                    args.acp,
                )?;
                if args.mcp {
                    mcp_server::run_mcp_server(workspace).await
                } else if http_selected {
                    let (mut config, config_profile) =
                        load_config_from_cli_with_effective_profile(&cli)?;
                    initialize_cloud_facts(&config);
                    let explicit_route_override =
                        crate::config::explicit_launch_provider_override().is_some()
                            || crate::config::explicit_launch_model_override().is_some();
                    apply_selected_fleet_operator_for_launch(
                        &mut config,
                        &workspace,
                        explicit_route_override,
                        false,
                    )?;
                    let cors_origins = resolve_cors_origins(&config, &args.cors_origin);
                    let bind_host = resolve_serve_bind_host(args.mobile, args.host);
                    if args.web && bind_host.host != "127.0.0.1" {
                        bail!("Codewhale web is loopback-only and must bind to 127.0.0.1");
                    }
                    runtime_api::run_http_server(
                        config,
                        workspace,
                        std::sync::Arc::clone(&plugin_discovery),
                        runtime_api::RuntimeApiOptions {
                            host: bind_host.host,
                            port: args.port,
                            workers: args.workers.clamp(1, 8),
                            cors_origins,
                            auth_token: args.auth_token,
                            insecure_no_auth: args.insecure_no_auth,
                            mobile: args.mobile,
                            web: args.web,
                            show_qr: args.qr,
                            config_path: cli.config.clone(),
                            config_profile,
                            control_frontend: cli.control_frontend.clone(),
                        },
                    )
                    .await
                } else if args.acp {
                    let (config, config_profile) =
                        load_config_from_cli_with_effective_profile(&cli)?;
                    initialize_cloud_facts(&config);
                    let model = config.default_model();
                    acp_server::run_acp_server(
                        config,
                        model,
                        workspace,
                        std::sync::Arc::clone(&plugin_discovery),
                        cli.config.clone(),
                        config_profile,
                    )
                    .await
                } else {
                    unreachable!("server mode count checked above")
                }
            }
            Commands::Resume { session_id, last } => {
                tui::ui::require_interactive_terminal(
                    io::stdin().is_terminal(),
                    io::stdout().is_terminal(),
                )?;
                let config = load_config_from_cli(&cli)?;
                let workspace = resolve_workspace(&cli);
                let resume_id = resolve_session_id(session_id, last, &workspace)?;
                run_interactive(
                    &cli,
                    &config,
                    Some(resume_id),
                    None,
                    pending_telemetry_notice.take(),
                    std::sync::Arc::clone(&plugin_registry),
                )
                .await
            }
            Commands::Fork { session_id, last } => {
                tui::ui::require_interactive_terminal(
                    io::stdin().is_terminal(),
                    io::stdout().is_terminal(),
                )?;
                let config = load_config_from_cli(&cli)?;
                let plugin_registry = policy_current_registry(plugin_registry);
                let prepared = prepare_interactive_config(&cli, &config, true)?;
                let source_id = resolve_session_id(session_id, last, &prepared.workspace)?;
                let (prepared, new_session_id) = prepare_mounted_session(
                    &cli,
                    prepared,
                    source_id,
                    MountedHistoryIntent::Fork,
                    std::sync::Arc::clone(&plugin_registry),
                )
                .await?;
                run_interactive_prepared(
                    &cli,
                    prepared,
                    Some(new_session_id),
                    None,
                    None,
                    pending_telemetry_notice.take(),
                    std::sync::Arc::clone(&plugin_registry),
                )
                .await
            }
        };
    }

    tui::ui::require_interactive_terminal(io::stdin().is_terminal(), io::stdout().is_terminal())?;

    // Top-level prompt mode: submit the initial prompt, then keep the TUI alive
    // for follow-up messages. Use `codewhale exec` for explicit non-interactive
    // one-shot behavior (#2370).
    let config = load_config_from_cli(&cli)?;
    if let Some(initial_input) = top_level_prompt_initial_input(&cli.prompt) {
        return run_interactive(
            &cli,
            &config,
            None,
            Some(initial_input),
            pending_telemetry_notice.take(),
            std::sync::Arc::clone(&plugin_registry),
        )
        .await;
    }

    // Handle session resume. Plain `codewhale` starts fresh: interrupted
    // snapshots are preserved for explicit resume, but never auto-attached.
    let mut startup_notice = None;
    let resume_session_id = if cli.continue_session {
        let workspace = resolve_workspace(&cli);
        resolve_continue_session_id(
            &workspace,
            io::stdin().is_terminal() && io::stdout().is_terminal(),
        )
    } else if let Some(id) = cli.resume.clone() {
        Some(id)
    } else if !cli.fresh {
        let workspace = resolve_workspace(&cli);
        preserve_interrupted_checkpoint_for_explicit_resume(&workspace);
        // Opt-in auto-resume (#2934). Off by default, so the historical
        // "plain `codewhale` starts fresh" behaviour is unchanged unless the
        // user asked for something else. The decision never resumes an
        // archived, unreadable, or foreign-workspace session; every fallback
        // carries a receipt rather than silently starting blank.
        let (session_id, notice) = resolve_auto_resume(&workspace);
        startup_notice = notice;
        session_id
    } else {
        None
    };

    // Default: Interactive TUI
    // --yolo starts in YOLO mode (auto-approve; shell enabled)
    run_interactive_with_notice(
        &cli,
        &config,
        resume_session_id,
        None,
        startup_notice,
        pending_telemetry_notice.take(),
        plugin_registry,
    )
    .await
}

/// Resolve the opt-in auto-resume setting into a session id plus a receipt.
///
/// Deliberately scoped to the plain interactive launch. `codewhale "do X"`
/// (top-level prompt) and `codewhale exec` are not covered: silently prefixing
/// a one-shot task with a prior conversation would change what is sent to the
/// model, which is not a layout preference the user opted into.
fn resolve_auto_resume(workspace: &Path) -> (Option<String>, Option<String>) {
    use crate::session_resume::{ResumeRequest, decide_auto_resume};

    let enabled = crate::settings::Settings::load_persisted()
        .map(|settings| settings.session_auto_resume)
        .unwrap_or(false);
    if !enabled {
        return (None, None);
    }
    let Ok(manager) = SessionManager::default_location() else {
        return (None, None);
    };
    let decision = decide_auto_resume(true, &ResumeRequest::default(), workspace, &manager);
    (
        decision.session_id().map(str::to_string),
        decision.status_message(),
    )
}

fn prepare_cli_startup(
    cli: Cli,
    initialize_plugins: impl FnOnce(),
    load_dotenv: impl FnOnce(),
) -> (Cli, Option<Commands>) {
    initialize_plugins();
    let command = cli.command.clone();
    let should_load_dotenv = match command.as_ref() {
        Some(Commands::Doctor(args)) => args.probe_api || args.probe_local,
        _ => true,
    };
    if should_load_dotenv {
        load_dotenv();
    }
    (cli, command)
}

const MAX_WORKSPACE_DOTENV_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Default)]
struct WorkspaceDotenvReport {
    path: PathBuf,
    loaded: BTreeSet<String>,
    ignored: BTreeSet<String>,
}

/// Load the narrow, data-plane subset of a workspace `.env` before Tokio.
///
/// Repository content is not product authority. In particular, a committed
/// `.env` must not be able to redirect `CODEWHALE_HOME`, config/profile files,
/// MCP servers, plugin trust, executable lookup, sandbox/approval posture, or
/// network destinations. Shell-exported values and config/CLI arguments remain
/// the explicit surfaces for those controls.
fn warn_on_workspace_dotenv_result(workspace: &Path) {
    match load_workspace_dotenv_credentials(workspace) {
        Ok(Some(report)) if !report.ignored.is_empty() => {
            eprintln!(
                "Codewhale ignored non-credential settings in {}: {}. Use config.toml, CLI flags, or the launching shell for control settings.",
                report.path.display(),
                display_env_key_set(&report.ignored)
            );
        }
        Ok(_) => {}
        Err(error) => {
            // The error intentionally contains no file contents or parsed
            // values. A malformed or unsafe workspace file fails closed while
            // shell/config credentials remain available.
            eprintln!("Codewhale did not load workspace .env: {error}");
        }
    }
}

fn display_env_key_set(keys: &BTreeSet<String>) -> String {
    const MAX_DISPLAYED: usize = 12;
    let mut labels = keys
        .iter()
        .take(MAX_DISPLAYED)
        .map(|key| {
            if key
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
            {
                key.as_str()
            } else {
                "<invalid-name>"
            }
        })
        .collect::<Vec<_>>();
    if keys.len() > MAX_DISPLAYED {
        labels.push("...");
    }
    labels.join(", ")
}

fn load_workspace_dotenv_credentials(workspace: &Path) -> Result<Option<WorkspaceDotenvReport>> {
    let Some(path) = find_workspace_dotenv(workspace)? else {
        return Ok(None);
    };
    load_workspace_dotenv_credentials_from_path(&path).map(Some)
}

/// The nearest `.env` from the resolved launch workspace (`--workspace`, else
/// the current directory) up to its repository root. Searching from the
/// process directory instead would load another tree's credentials when the
/// two differ.
fn find_workspace_dotenv(workspace: &Path) -> Result<Option<PathBuf>> {
    let start = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir()
            .context("could not resolve the current workspace")?
            .join(workspace)
    };
    let boundary = start
        .ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor.join(".git")).is_ok())
        .unwrap_or(start.as_path());

    for ancestor in start.ancestors() {
        let candidate = ancestor.join(".env");
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => return Ok(Some(candidate)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(anyhow!(
                    "could not inspect {}: {error}",
                    candidate.display()
                ));
            }
        }
        if ancestor == boundary {
            break;
        }
    }
    Ok(None)
}

fn load_workspace_dotenv_credentials_from_path(path: &Path) -> Result<WorkspaceDotenvReport> {
    let contents = read_stable_workspace_dotenv(path)?;
    let text = std::str::from_utf8(&contents)
        .map_err(|_| anyhow!("{} is not valid UTF-8", path.display()))?;
    if dotenv_has_variable_expansion(text) {
        bail!(
            "{} uses variable expansion; workspace .env values must be literal to prevent ambient-secret substitution",
            path.display()
        );
    }

    let mut report = WorkspaceDotenvReport {
        path: path.to_path_buf(),
        ..WorkspaceDotenvReport::default()
    };
    let entries = dotenvy::from_read_iter(std::io::Cursor::new(contents))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| anyhow!("{} could not be parsed safely", path.display()))?;
    for entry in entries {
        let (key, value) = entry;
        if !is_workspace_dotenv_credential_key(&key) {
            report.ignored.insert(key);
            continue;
        }
        if std::env::var_os(&key).is_some() {
            continue;
        }

        // SAFETY: this loader runs synchronously in `main` before the runtime
        // owner or Tokio workers are spawned. No concurrent environment reader
        // exists inside Codewhale, and later startup code treats this process
        // environment as immutable.
        unsafe { std::env::set_var(&key, value) };
        report.loaded.insert(key);
    }
    Ok(report)
}

fn is_workspace_dotenv_credential_key(key: &str) -> bool {
    codewhale_config::provider::providers_sorted_for_display()
        .into_iter()
        .any(|provider| provider.env_vars().contains(&key))
        || matches!(
            key,
            "DEEPSEEK_SEARCH_API_KEY"
                | "SOFYA_API_KEY"
                | "SERPLY_API_KEY"
                | "METASO_API_KEY"
                | "BAIDU_SEARCH_API_KEY"
                | "TAVILY_API_KEY"
                | "DEEPSEEK_SANDBOX_API_KEY"
        )
}

fn dotenv_has_variable_expansion(contents: &str) -> bool {
    let mut escaped = false;
    let mut single_quoted = false;
    let mut double_quoted = false;
    let mut comment = false;

    for ch in contents.chars() {
        if comment {
            // Reject expansion markers even in comments. This is deliberately
            // conservative, and ignoring other comment text prevents an
            // unmatched quote there from changing how the next line is read.
            if ch == '$' {
                return true;
            }
            if ch == '\n' {
                comment = false;
                escaped = false;
            }
            continue;
        }
        if single_quoted {
            if ch == '\'' {
                single_quoted = false;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '\'' && !double_quoted {
            single_quoted = true;
            continue;
        }
        if ch == '"' {
            double_quoted = !double_quoted;
            continue;
        }
        if ch == '#' && !double_quoted {
            comment = true;
            continue;
        }
        if ch == '$' {
            return true;
        }
    }
    false
}

fn read_stable_workspace_dotenv(path: &Path) -> Result<Vec<u8>> {
    let mut file = open_workspace_dotenv_without_following_links(path)?;
    let metadata = file
        .metadata()
        .map_err(|error| anyhow!("could not inspect {}: {error}", path.display()))?;
    if !metadata.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    if workspace_dotenv_has_multiple_links(&file, &metadata)? {
        bail!(
            "{} has multiple filesystem links, not a unique workspace-owned file",
            path.display()
        );
    }
    if metadata.len() > MAX_WORKSPACE_DOTENV_BYTES {
        bail!(
            "{} exceeds the {} byte workspace .env limit",
            path.display(),
            MAX_WORKSPACE_DOTENV_BYTES
        );
    }

    let mut contents = Vec::with_capacity(metadata.len() as usize);
    (&mut file)
        .take(MAX_WORKSPACE_DOTENV_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|error| anyhow!("could not read {}: {error}", path.display()))?;
    if contents.len() as u64 > MAX_WORKSPACE_DOTENV_BYTES {
        bail!(
            "{} exceeds the {} byte workspace .env limit",
            path.display(),
            MAX_WORKSPACE_DOTENV_BYTES
        );
    }
    Ok(contents)
}

#[cfg(unix)]
fn workspace_dotenv_has_multiple_links(
    _file: &std::fs::File,
    metadata: &std::fs::Metadata,
) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;

    Ok(metadata.nlink() > 1)
}

#[cfg(windows)]
fn workspace_dotenv_has_multiple_links(
    file: &std::fs::File,
    _metadata: &std::fs::Metadata,
) -> Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a live kernel handle for the already-open `.env`;
    // `information` remains writable for the duration of this synchronous
    // call. No path lookup or re-open occurs here.
    unsafe {
        GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information)
            .map_err(|error| anyhow!("could not inspect workspace .env link count: {error}"))?;
    }
    Ok(information.nNumberOfLinks > 1)
}

#[cfg(not(any(unix, windows)))]
fn workspace_dotenv_has_multiple_links(
    _file: &std::fs::File,
    _metadata: &std::fs::Metadata,
) -> Result<bool> {
    Ok(false)
}

#[cfg(unix)]
fn open_workspace_dotenv_without_following_links(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        // `O_NONBLOCK` is inert for regular files but prevents a FIFO named
        // `.env` from hanging startup before the metadata check can reject it.
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| anyhow!("could not securely open {}: {error}", path.display()))
}

#[cfg(windows)]
fn open_workspace_dotenv_without_following_links(path: &Path) -> Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|error| anyhow!("could not securely open {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| anyhow!("could not inspect {}: {error}", path.display()))?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!(
            "{} is a reparse point, not a workspace-owned file",
            path.display()
        );
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_workspace_dotenv_without_following_links(path: &Path) -> Result<std::fs::File> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| anyhow!("could not inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "{} is a symbolic link, not a workspace-owned file",
            path.display()
        );
    }
    std::fs::File::open(path)
        .map_err(|error| anyhow!("could not securely open {}: {error}", path.display()))
}

/// Run the offline evaluation harness (no network/LLM calls).
fn run_eval(args: EvalArgs) -> Result<()> {
    let fail_step = match args.fail_step.as_deref() {
        Some(value) => ScenarioStepKind::parse(value)
            .map(Some)
            .ok_or_else(|| anyhow!("invalid --fail-step '{value}'"))?,
        None => None,
    };

    let config = EvalHarnessConfig {
        fail_step,
        shell_command: args.shell_command,
        shell_expect_token: args.shell_expect_token,
        max_output_chars: args.max_output_chars,
        record_dir: args.record.clone(),
        ..EvalHarnessConfig::default()
    };

    let harness = EvalHarness::new(config);
    let run = harness.run().context("evaluation harness failed")?;
    let report = run.to_report();

    if args.json {
        let json = serde_json::to_string_pretty(&report)?;
        println!("{json}");
    } else {
        println!("Offline Eval Harness");
        println!("scenario: {}", report.scenario_name);
        println!("workspace: {}", report.workspace_root.display());
        println!("success: {}", report.metrics.success);
        println!("steps: {}", report.metrics.steps);
        println!("tool_errors: {}", report.metrics.tool_errors);
        println!("duration_ms: {}", report.metrics.duration.as_millis());

        if !report.metrics.per_tool.is_empty() {
            println!("per_tool:");
            for (kind, stats) in &report.metrics.per_tool {
                println!(
                    "  {} invocations={} errors={} duration_ms={}",
                    kind.tool_name(),
                    stats.invocations,
                    stats.errors,
                    stats.total_duration.as_millis()
                );
            }
        }

        let failed_steps: Vec<_> = report.steps.iter().filter(|s| !s.success).collect();
        if !failed_steps.is_empty() {
            println!("failed_steps:");
            for step in failed_steps {
                let error = step.error.as_deref().unwrap_or("unknown error");
                println!(
                    "  {} tool={} error={}",
                    step.kind.tool_name(),
                    step.tool_name,
                    error
                );
            }
        }
    }

    if report.metrics.success {
        Ok(())
    } else {
        bail!("offline evaluation harness reported failure")
    }
}

/// A regression gate threshold must be a finite percentage: `NaN` compares
/// false against every change (a gate that always passes) and infinity
/// disables the gate outright.
fn parse_regression_threshold(raw: &str) -> Result<f64, String> {
    let value: f64 = raw
        .trim()
        .parse()
        .map_err(|_| format!("`{raw}` is not a number"))?;
    if !value.is_finite() {
        return Err(format!("`{raw}` is not a finite percentage"));
    }
    Ok(value)
}

/// Score a run's token/cache/cost from recorded turns and (optionally) flag
/// regressions against a committed baseline. Offline: reads recorded usage from
/// a JSON file, reuses the pricing layer, never calls a model. Exits non-zero
/// when a baseline is supplied and a metric regresses past the threshold, so it
/// can be wired as a release gate (#3388).
fn run_scorecard(args: ScorecardArgs) -> Result<()> {
    use crate::scorecard::{RecordedTurn, Scorecard, ScorecardMetrics};

    let raw = std::fs::read_to_string(&args.input)
        .with_context(|| format!("failed to read scorecard input {}", args.input.display()))?;
    let recorded: Vec<RecordedTurn> = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse scorecard input {}", args.input.display()))?;

    let card = Scorecard::from_recorded_turns(&recorded);

    let regressions = match &args.baseline {
        Some(path) => {
            let baseline_raw = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read baseline {}", path.display()))?;
            let baseline: ScorecardMetrics = serde_json::from_str(&baseline_raw)
                .with_context(|| format!("failed to parse baseline {}", path.display()))?;
            card.metrics.regressions_against(&baseline, args.threshold)
        }
        None => Vec::new(),
    };

    if args.json {
        let out = serde_json::json!({
            "per_turn": card.per_turn,
            "metrics": card.metrics,
            "regressions": regressions,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print!("{}", card.to_summary());
        for r in &regressions {
            println!(
                "REGRESSION {}: baseline {:.4} -> current {:.4} (+{:.1}%)",
                r.metric, r.baseline, r.current, r.pct_increase
            );
        }
    }

    if regressions.is_empty() {
        Ok(())
    } else {
        bail!(
            "{} metric(s) regressed past the {:.1}% threshold",
            regressions.len(),
            args.threshold
        )
    }
}

async fn run_fleet_command(workspace: &Path, config: &Config, args: FleetArgs) -> Result<()> {
    use crate::fleet::alerts::{
        FleetAlertAdapterConfig, FleetAlertConfig, FleetAlertDispatcher, FleetAlertEvent,
        FleetEnvSecretResolver,
    };
    use crate::fleet::control as fleet_control;
    use crate::fleet::executor::FleetExecutor;
    use crate::fleet::manager::{FleetManager, FleetStatusSnapshot, FleetWorkerInspection};
    use codewhale_lane::{ControlOperation, ControlSurface};
    use codewhale_protocol::fleet::{FleetAlertEventClass, FleetArtifactKind, FleetRunId};

    // Every label and every row below comes from the shared Fleet control
    // surface, so `codewhale fleet …` and `/fleet …` cannot drift in how they
    // describe the same durable ledger (#1888, #4022).
    fn print_status(status: &FleetStatusSnapshot) {
        println!("{}", fleet_control::render_fleet_status_snapshot(status));
    }

    fn print_inspection(inspection: &FleetWorkerInspection) {
        println!("{}", fleet_control::render_inspection(inspection));
    }

    fn print_artifacts(inspection: &FleetWorkerInspection) {
        println!("{}", fleet_control::render_artifacts(inspection));
    }

    /// Print one shared control receipt on the CLI surface.
    fn emit_fleet_receipt(receipt: &codewhale_lane::ControlReceipt) -> Result<()> {
        if receipt.is_error() {
            eprintln!("{}", receipt.render());
            let detail = receipt
                .failure
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| receipt.outcome.as_str().to_string());
            bail!("{}: {detail}", receipt.operation_id);
        }
        println!("{}", receipt.render());
        Ok(())
    }

    fn print_logs(workspace: &Path, inspection: &FleetWorkerInspection) -> Result<()> {
        let mut printed = false;
        for artifact in inspection
            .artifacts
            .iter()
            .filter(|artifact| matches!(artifact.kind, FleetArtifactKind::Log))
        {
            let path = workspace.join(&artifact.path);
            println!("== {} ==", artifact.path.display());
            let contents = std::fs::read_to_string(&path)
                .with_context(|| format!("reading Fleet log {}", path.display()))?;
            let preview: String = contents.chars().take(16 * 1024).collect();
            // Worker logs can contain captured terminal bytes (a child TUI's
            // mouse-tracking handshake, SGR, OSC). Printing them raw would
            // re-arm mouse reporting in the caller's shell and leave it
            // executing escape fragments after this command exits.
            let mut safe_preview = String::with_capacity(preview.len());
            crate::tui::osc8::strip_ansi_into(&preview, &mut safe_preview);
            print!("{safe_preview}");
            if contents.chars().count() > preview.chars().count() {
                println!("\n[truncated]");
            } else if !preview.ends_with('\n') {
                println!();
            }
            printed = true;
        }
        if !printed {
            println!("logs: none");
        }
        Ok(())
    }

    fn alert_event_class(arg: FleetAlertEventArg) -> FleetAlertEventClass {
        match arg {
            FleetAlertEventArg::Stale => FleetAlertEventClass::Stale,
            FleetAlertEventArg::RestartExhausted => FleetAlertEventClass::RestartExhausted,
            FleetAlertEventArg::NeedsHuman => FleetAlertEventClass::NeedsHuman,
            FleetAlertEventArg::BudgetExceeded => FleetAlertEventClass::BudgetExceeded,
            FleetAlertEventArg::VerifierFailed => FleetAlertEventClass::VerifierFailed,
            FleetAlertEventArg::RunCompleted => FleetAlertEventClass::RunCompleted,
        }
    }

    fn alert_status(class: FleetAlertEventClass, override_status: Option<String>) -> String {
        if let Some(status) = override_status {
            return status;
        }
        match class {
            FleetAlertEventClass::Stale => "stale",
            FleetAlertEventClass::RestartExhausted => "failed",
            FleetAlertEventClass::NeedsHuman => "needs_human",
            FleetAlertEventClass::BudgetExceeded => "budget_exceeded",
            FleetAlertEventClass::VerifierFailed => "verifier_failed",
            FleetAlertEventClass::RunCompleted => "completed",
        }
        .to_string()
    }

    fn alert_adapter(args: &FleetAlertDryRunArgs) -> FleetAlertAdapterConfig {
        match args.adapter {
            FleetAlertAdapterArg::Slack => FleetAlertAdapterConfig::Slack {
                webhook_env: args.slack_webhook_env.clone(),
                channel: None,
            },
            FleetAlertAdapterArg::Webhook => FleetAlertAdapterConfig::Webhook {
                url_env: args.webhook_url_env.clone(),
                secret_env: args.webhook_secret_env.clone(),
            },
            FleetAlertAdapterArg::PagerDuty => FleetAlertAdapterConfig::PagerDuty {
                routing_key_env: args.pagerduty_routing_key_env.clone(),
                severity: args.pagerduty_severity.clone(),
            },
        }
    }

    let fleet_config = config.fleet_config();
    // `fleet run --check` must not conjure the ledger or the sub-agent state it
    // would write to, so it validates before either is opened below.
    if let FleetCommand::Run(run_args) = &args.command
        && run_args.check
    {
        initialize_cloud_facts(config);
        let check = FleetManager::check_task_spec_path_in(
            workspace,
            fleet_config,
            config.default_model(),
            config.clone(),
            &run_args.task_spec,
        )?;
        println!(
            "Fleet spec ok: {} ({} task{}). Nothing was created or launched.",
            run_args.task_spec.display(),
            check.task_count,
            if check.task_count == 1 { "" } else { "s" }
        );
        for warning in &check.warnings {
            println!("warning: {warning}");
        }
        return Ok(());
    }

    let identity = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;

    let max_subagents = config.max_subagents_for_provider(&identity);
    let coordination_manager = crate::tools::subagent::new_shared_subagent_manager_with_timeout(
        workspace.to_path_buf(),
        max_subagents,
        config
            .max_admitted_subagents_for_provider(&identity)
            .max(max_subagents),
        Duration::from_secs(config.subagent_heartbeat_timeout_secs_for_provider(&identity)),
        config.launch_concurrency_for_provider(&identity),
    );
    // Probe the durable ledger *before* opening the manager: FleetManager::open
    // creates `.codewhale/fleet.jsonl` as a side effect, so a later probe would
    // always find a ledger and the CLI would report availability differently
    // from the slash surface for the same workspace (#4022).
    let fleet_context = fleet_control::fleet_control_context(workspace);
    // Probing is not enough on its own: `FleetManager::open` *creates* the
    // ledger, and it used to run for every subcommand before this match. That
    // made `codewhale fleet status` in a ledgerless workspace print
    // "no_fleet_ledger" while simultaneously creating the file it said was
    // missing — and the next invocation then reported an empty ledger as if a
    // Fleet had existed all along. Refuse the control verbs here, before the
    // manager exists, so the CLI and `/fleet` agree and neither surface
    // conjures the store it is reporting on (#4022).
    if let Some(operation) = match &args.command {
        FleetCommand::List => Some(ControlOperation::FleetList),
        FleetCommand::Status => Some(ControlOperation::FleetStatus),
        FleetCommand::Interrupt { .. } => Some(ControlOperation::FleetInterrupt),
        FleetCommand::Resume { .. } => Some(ControlOperation::FleetResume),
        _ => None,
    } {
        let descriptor = operation.descriptor();
        let availability = descriptor.availability(ControlSurface::Cli, fleet_context);
        if !availability.is_available() {
            return emit_fleet_receipt(&codewhale_lane::ControlReceipt::unavailable(
                descriptor,
                ControlSurface::Cli,
                availability,
            ));
        }
    }

    // The configured route is the operator: fleet workers without a
    // task/profile model pin inherit the session's active model.
    let manager = FleetManager::open(workspace)?
        .with_exec_config(fleet_config.exec.clone())
        .with_fleet_config(fleet_config)
        .with_sub_agent_manager(coordination_manager)
        .with_session_model(config.default_model())
        .with_route_config(config.clone());
    match args.command {
        FleetCommand::Init => {
            println!("Fleet ledger: {}", manager.ledger_path().display());
            Ok(())
        }
        FleetCommand::Run(args) => {
            initialize_cloud_facts(config);
            let max_workers = args.max_workers.clamp(1, 128);
            let manager =
                manager.with_stale_after(Duration::from_secs(args.stale_after_seconds.max(1)));
            let report = manager.create_run_from_task_spec_path(&args.task_spec, max_workers)?;
            println!(
                "Fleet run: {} tasks={} leased={} queued={}",
                report.run_id.0, report.task_count, report.leased, report.queued
            );
            for warning in &report.warnings {
                println!("warning: {warning}");
            }
            println!("workers:");
            for worker_id in &report.worker_ids {
                println!("  {worker_id}");
            }
            if args.once {
                print_status(&manager.run_status(&report.run_id)?);
                return Ok(());
            }
            println!(
                "manager loop running; use `codewhale fleet status`, `inspect`, `interrupt`, or `stop --all` from another terminal."
            );
            let mut executor = FleetExecutor::new(workspace)
                .with_sessions_dir(session_manager::default_sessions_dir()?);
            let codewhale_binary = fleet::executor::configured_codewhale_binary();
            let status = manager
                .run_to_completion(
                    &report.run_id,
                    max_workers,
                    &mut executor,
                    &codewhale_binary,
                    None,
                    Duration::from_secs(2),
                )
                .await?;
            print_status(&status);
            Ok(())
        }
        FleetCommand::List => emit_fleet_receipt(&fleet_control::execute_fleet_control_with(
            ControlSurface::Cli,
            workspace,
            fleet_context,
            &manager,
            ControlOperation::FleetList,
            None,
        )),
        FleetCommand::Status => emit_fleet_receipt(&fleet_control::execute_fleet_control_with(
            ControlSurface::Cli,
            workspace,
            fleet_context,
            &manager,
            ControlOperation::FleetStatus,
            None,
        )),
        FleetCommand::Inspect { worker_id } => {
            print_inspection(&manager.inspect_worker(&worker_id)?);
            Ok(())
        }
        FleetCommand::Logs { worker_id } => {
            let inspection = manager.inspect_worker(&worker_id)?;
            print_logs(workspace, &inspection)
        }
        FleetCommand::Artifacts { worker_id } => {
            let inspection = manager.inspect_worker(&worker_id)?;
            print_artifacts(&inspection);
            Ok(())
        }
        FleetCommand::Interrupt { worker_id } => {
            emit_fleet_receipt(&fleet_control::execute_fleet_control_with(
                ControlSurface::Cli,
                workspace,
                fleet_context,
                &manager,
                ControlOperation::FleetInterrupt,
                Some(&worker_id),
            ))
        }
        FleetCommand::Restart { worker_id } => {
            let report = manager.restart_worker(&worker_id)?;
            print_inspection(&report.inspection);
            println!(
                "manager loop running for restarted run {}; use `codewhale fleet status`, `inspect`, `interrupt`, or `stop --all` from another terminal.",
                report.run_id.0
            );
            let mut executor = FleetExecutor::new(workspace)
                .with_sessions_dir(session_manager::default_sessions_dir()?);
            let codewhale_binary = fleet::executor::configured_codewhale_binary();
            let status = manager
                .run_to_completion(
                    &report.run_id,
                    report.max_workers,
                    &mut executor,
                    &codewhale_binary,
                    None,
                    Duration::from_secs(2),
                )
                .await?;
            print_status(&status);
            Ok(())
        }
        FleetCommand::Resume {
            run_id,
            stale_after_seconds,
        } => {
            let manager = manager.with_stale_after(Duration::from_secs(stale_after_seconds.max(1)));
            emit_fleet_receipt(&fleet_control::execute_fleet_control_with(
                ControlSurface::Cli,
                workspace,
                fleet_context,
                &manager,
                ControlOperation::FleetResume,
                Some(&run_id),
            ))
        }
        FleetCommand::Stop { all } => {
            if !all {
                bail!("pass --all to stop all Fleet work");
            }
            let stopped = manager.stop_all()?;
            println!("stopped: {stopped}");
            Ok(())
        }
        FleetCommand::AlertDryRun(args) => {
            let class = alert_event_class(args.event);
            let adapter = alert_adapter(&args);
            let event = FleetAlertEvent {
                class,
                run_id: FleetRunId::from(args.run_id.clone()),
                worker_id: args.worker_id.clone(),
                task_id: args.task_id.clone(),
                status: alert_status(class, args.status.clone()),
                reason: args.reason.clone(),
            };
            let dispatcher = FleetAlertDispatcher::new(
                FleetAlertConfig::dry_run_for_adapter(adapter),
                FleetEnvSecretResolver,
            );
            let deliveries = dispatcher.dispatch(&event)?;
            for delivery in deliveries {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&delivery.redacted_payload)?
                );
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStatus {
    Created,
    Overwritten,
    SkippedExists,
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory for {}", parent.display()))?;
    }
    Ok(())
}

fn write_template_file(path: &Path, contents: &str, force: bool) -> Result<WriteStatus> {
    ensure_parent_dir(path)?;

    if path.exists() && !force {
        return Ok(WriteStatus::SkippedExists);
    }

    let status = if path.exists() {
        WriteStatus::Overwritten
    } else {
        WriteStatus::Created
    };

    std::fs::write(path, contents)
        .with_context(|| format!("Failed to write template at {}", path.display()))?;

    Ok(status)
}

fn init_mcp_config(path: &Path, force: bool) -> Result<WriteStatus> {
    Ok(match crate::mcp::init_config(path, force)? {
        crate::mcp::McpWriteStatus::Created => WriteStatus::Created,
        crate::mcp::McpWriteStatus::Overwritten => WriteStatus::Overwritten,
        crate::mcp::McpWriteStatus::SkippedExists => WriteStatus::SkippedExists,
    })
}

fn skills_template(name: &str) -> String {
    format!(
        "\
---\n\
name: {name}\n\
description: Quick repo diagnostics and setup guidance\n\
allowed-tools: diagnostics, list_dir, read_file, grep_files, git_status, git_diff\n\
---\n\n\
When this skill is active:\n\
1. Run the diagnostics tool to report workspace and sandbox status.\n\
2. Skim key project files (README.md, Cargo.toml, AGENTS.md) before editing.\n\
3. Prefer small, validated changes and summarize what you verified.\n\
"
    )
}

fn init_skills_dir(skills_dir: &Path, force: bool) -> Result<(PathBuf, WriteStatus)> {
    std::fs::create_dir_all(skills_dir)
        .with_context(|| format!("Failed to create skills dir {}", skills_dir.display()))?;

    let skill_name = "getting-started";
    let skill_path = skills_dir.join(skill_name).join("SKILL.md");
    ensure_parent_dir(&skill_path)?;

    let status = write_template_file(&skill_path, &skills_template(skill_name), force)?;
    Ok((skill_path, status))
}

fn tools_readme_template() -> &'static str {
    "# Local tools\n\n\
     Drop self-describing scripts here so they can be discovered by\n\
     `codewhale setup --status` and surfaced in `codewhale doctor`.\n\n\
     When `[tools.plugin_dir]` is set in config.toml (or when the default\n\
     `~/.codewhale/tools/` directory exists), they are auto-discovered and\n\
     registered as model-visible tools.\n\n\
     Each script should start with a frontmatter-style header so the\n\
     description is visible without executing the file and the agent knows\n\
     the tool name, description, and input schema:\n\n\
     ```\n\
     # name: my-tool\n\
     # description: One-line summary of what this tool does\n\
     # usage: my-tool [args...]\n\
     ```\n\n\
     The directory is intentionally not auto-loaded into the agent's tool\n\
     catalog. Wire individual tools through MCP, hooks, or skills when you\n\
     want them available inside a session.\n"
}

fn tools_example_script() -> &'static str {
    "#!/usr/bin/env sh\n\
     # name: example\n\
     # description: Print a confirmation that local tool discovery works\n\
     # usage: example [name]\n\
     printf 'codewhale local tool ok: %s\\n' \"${1:-world}\"\n"
}

fn init_tools_dir(tools_dir: &Path, force: bool) -> Result<(PathBuf, WriteStatus, WriteStatus)> {
    std::fs::create_dir_all(tools_dir)
        .with_context(|| format!("Failed to create tools dir {}", tools_dir.display()))?;

    let readme_path = tools_dir.join("README.md");
    let readme_status = write_template_file(&readme_path, tools_readme_template(), force)?;

    let example_path = tools_dir.join("example.sh");
    let example_status = write_template_file(&example_path, tools_example_script(), force)?;

    Ok((tools_dir.to_path_buf(), readme_status, example_status))
}

fn plugins_readme_template() -> &'static str {
    "# Local plugins\n\n\
     Each Codewhale plugin bundle lives in its own subdirectory with a\n\
     versioned `plugin.toml`. User bundles live here; workspace bundles live\n\
     under `<workspace>/.codewhale/plugins/`. Both are discovered read-only,\n\
     untrusted, and disabled by default.\n\n\
     A v0.9.1 bundle layout looks like:\n\n\
     ```\n\
     plugins/\n\
       my-plugin/\n\
         plugin.toml\n\
         skills/\n\
           my-skill/SKILL.md\n\
     ```\n\n\
     Run `/plugin validate`, `/plugin show <name>`, then `/plugin enable <name>`.\n\
     Enablement opens a content- and capability-bound trust review;\n\
     confirm the displayed `/plugin trust` command to create an owner-only,\n\
     content-addressed runtime snapshot, then enable the bundle. Remote MCP\n\
     authentication must name environment sources; never store secret values\n\
     in `plugin.toml`.\n\n\
     Codewhale activates declarative Skills, MCP servers, Commands, Agent\n\
     profiles, and Hooks through their existing engines. LSP, native\n\
     extensions, filesystem grants, and lifecycle mutation stay inventoried\n\
     and inactive; a mixed bundle can still activate supported components.\n\
     Marketplace catalogs, install, update, and uninstall all feed this same\n\
     disabled-and-untrusted review path; none grants automatic trust. Codewhale\n\
     does not scan other applications for ambient plugins.\n"
}

fn plugin_example_manifest_template() -> &'static str {
    "schema_version = 1\n\n\
     [plugin]\n\
     name = \"example\"\n\
     version = \"0.1.0\"\n\
     description = \"Starter Codewhale plugin bundle\"\n\n\
     [skills]\n\
     path = \"skills\"\n"
}

fn plugin_example_skill_template() -> &'static str {
    "---\n\
     name: hello\n\
     description: Explain that the example plugin bundle is active.\n\
     ---\n\n\
     Tell the user this instruction came from the namespaced\n\
     `example:hello` plugin skill. Do not perform side effects.\n"
}

fn init_plugins_dir(
    plugins_dir: &Path,
    force: bool,
) -> Result<(
    PathBuf,
    PathBuf,
    PathBuf,
    WriteStatus,
    WriteStatus,
    WriteStatus,
)> {
    std::fs::create_dir_all(plugins_dir)
        .with_context(|| format!("Failed to create plugins dir {}", plugins_dir.display()))?;

    let readme_path = plugins_dir.join("README.md");
    let readme_status = write_template_file(&readme_path, plugins_readme_template(), force)?;

    let manifest_path = plugins_dir.join("example").join("plugin.toml");
    ensure_parent_dir(&manifest_path)?;
    let manifest_status =
        write_template_file(&manifest_path, plugin_example_manifest_template(), force)?;

    let skill_path = plugins_dir
        .join("example")
        .join("skills")
        .join("hello")
        .join("SKILL.md");
    ensure_parent_dir(&skill_path)?;
    let skill_status = write_template_file(&skill_path, plugin_example_skill_template(), force)?;

    Ok((
        readme_path,
        manifest_path,
        skill_path,
        readme_status,
        manifest_status,
        skill_status,
    ))
}

/// Resolve the user-supplied CORS origins for `codewhale serve --http`.
///
/// Sources, in priority order (later sources extend earlier ones):
/// 1. `--cors-origin URL` flags (repeatable)
/// 2. `CODEWHALE_CORS_ORIGINS` env var (comma-separated),
///    then `DEEPSEEK_CORS_ORIGINS` as an alias
/// 3. `[runtime_api] cors_origins = [...]` in `config.toml`
///
/// The runtime API always allows the built-in dev defaults
/// (localhost:3000, localhost:1420, tauri://localhost). User entries are
/// appended on top — empty strings are skipped, and duplicates are deduped
/// while preserving first-seen order. Whalescale#255 / #561.
fn resolve_cors_origins(config: &Config, flag_origins: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: &str| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        if !out.iter().any(|existing| existing == trimmed) {
            out.push(trimmed.to_string());
        }
    };
    for o in flag_origins {
        push(o);
    }
    if let Ok(env_value) =
        std::env::var("CODEWHALE_CORS_ORIGINS").or_else(|_| std::env::var("DEEPSEEK_CORS_ORIGINS"))
    {
        for piece in env_value.split(',') {
            push(piece);
        }
    }
    if let Some(rt) = &config.runtime_api
        && let Some(list) = &rt.cors_origins
    {
        for o in list {
            push(o);
        }
    }
    out
}

fn deepseek_home_dir() -> PathBuf {
    codewhale_config::codewhale_home().unwrap_or_else(|_| {
        crate::config::effective_home_dir()
            .map_or_else(|| PathBuf::from(".codewhale"), |h| h.join(".codewhale"))
    })
}

/// Resolve the default tools directory. Mirrors `default_skills_dir` shape.
fn default_tools_dir() -> PathBuf {
    deepseek_home_dir().join("tools")
}

/// Resolve the default plugins directory.
fn default_plugins_dir() -> PathBuf {
    deepseek_home_dir().join("plugins")
}

/// Default location for crash/offline-queue checkpoints managed by the TUI.
fn default_checkpoints_dir() -> PathBuf {
    deepseek_home_dir().join("sessions").join("checkpoints")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CleanPlan {
    targets: Vec<PathBuf>,
}

fn collect_clean_targets(checkpoints_dir: &Path) -> CleanPlan {
    // Unsent input is not regenerable. Preserve legacy and per-session queue
    // files by name even if their contents are malformed or from a newer
    // schema; cleanup must not erase drafts it cannot currently decode.
    let mut targets: Vec<PathBuf> = std::fs::read_dir(checkpoints_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "json"))
                .filter(|p| {
                    p.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| !crate::session_manager::is_offline_queue_file(name))
                })
                .collect()
        })
        .unwrap_or_default();
    targets.sort();
    CleanPlan { targets }
}

fn execute_clean_plan(plan: &CleanPlan) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::with_capacity(plan.targets.len());
    for path in &plan.targets {
        std::fs::remove_file(path)
            .with_context(|| format!("Failed to remove {}", path.display()))?;
        removed.push(path.clone());
    }
    Ok(removed)
}

fn run_setup(
    config: &Config,
    workspace: &Path,
    args: SetupArgs,
    plugins: &crate::plugins::PluginRegistry,
) -> Result<()> {
    if args.status {
        return run_setup_status(config, workspace, plugins);
    }
    if args.clean {
        return run_setup_clean(&default_checkpoints_dir(), args.force);
    }

    use codewhale_palette as palette;
    use colored::Colorize;

    let (aqua_r, aqua_g, aqua_b) = palette::WHALE_ACTION_RGB;
    let (sky_r, sky_g, sky_b) = palette::WHALE_ACTION_RGB;

    let any_explicit = args.mcp || args.skills || args.tools || args.plugins;
    let run_mcp = args.mcp || args.all || !any_explicit;
    let run_skills = args.skills || args.all || !any_explicit;
    let run_tools = args.tools || args.all;
    let run_plugins = args.plugins || args.all;

    println!(
        "{}",
        "Codewhale Setup".truecolor(aqua_r, aqua_g, aqua_b).bold()
    );
    println!("{}", "==============".truecolor(sky_r, sky_g, sky_b));
    println!("Workspace: {}", crate::utils::display_path(workspace));

    if run_mcp {
        let mcp_path = config.mcp_config_path();
        let status = init_mcp_config(&mcp_path, args.force)?;
        match status {
            WriteStatus::Created => {
                println!("  ✓ Created MCP config at {}", mcp_path.display());
            }
            WriteStatus::Overwritten => {
                println!("  ✓ Overwrote MCP config at {}", mcp_path.display());
            }
            WriteStatus::SkippedExists => {
                println!("  · MCP config already exists at {}", mcp_path.display());
            }
        }
        println!(
            "    Next: edit the file, then run `codewhale mcp list` or `codewhale mcp tools`."
        );
    }

    if run_skills {
        let skills_dir = if args.local {
            workspace.join("skills")
        } else {
            config.skills_dir()
        };
        let (skill_path, status) = init_skills_dir(&skills_dir, args.force)?;
        match status {
            WriteStatus::Created => {
                println!("  ✓ Created example skill at {}", skill_path.display());
            }
            WriteStatus::Overwritten => {
                println!("  ✓ Overwrote example skill at {}", skill_path.display());
            }
            WriteStatus::SkippedExists => {
                println!(
                    "  · Example skill already exists at {}",
                    skill_path.display()
                );
            }
        }
        if args.local {
            println!(
                "    Local skills dir enabled for this workspace: {}",
                crate::utils::display_path(&skills_dir)
            );
        } else {
            println!(
                "    Skills dir: {}",
                crate::utils::display_path(&skills_dir)
            );
        }
        println!("    Next: run the TUI and use `/skills` then `/skill getting-started`.");
    }

    if run_tools {
        let tools_dir = default_tools_dir();
        let (dir, readme_status, example_status) = init_tools_dir(&tools_dir, args.force)?;
        report_write_status("Tools README", &dir.join("README.md"), readme_status);
        report_write_status("Example tool", &dir.join("example.sh"), example_status);
        println!("    Tools dir: {}", crate::utils::display_path(&dir));
        println!("    Next: drop scripts here; surface them via skills/MCP when ready.");
    }

    if run_plugins {
        let plugins_dir = default_plugins_dir();
        let (readme_path, manifest_path, skill_path, readme_status, manifest_status, skill_status) =
            init_plugins_dir(&plugins_dir, args.force)?;
        report_write_status("Plugins README", &readme_path, readme_status);
        report_write_status("Example plugin manifest", &manifest_path, manifest_status);
        report_write_status("Example plugin skill", &skill_path, skill_status);
        println!(
            "    Plugins dir: {}",
            crate::utils::display_path(&plugins_dir)
        );
        println!("    Next: run `/plugin validate`, review `example`, then trust and enable it.");
    }

    let sandbox =
        crate::sandbox::get_platform_sandbox_with_bwrap_preference(config.prefers_bwrap());
    if let Some(kind) = sandbox {
        println!("  ✓ Sandbox available: {kind}");
    } else {
        println!("  · Sandbox not available on this platform (best-effort only).");
    }

    Ok(())
}

fn report_write_status(label: &str, path: &Path, status: WriteStatus) {
    match status {
        WriteStatus::Created => {
            println!("  ✓ Created {label} at {}", path.display());
        }
        WriteStatus::Overwritten => {
            println!("  ✓ Overwrote {label} at {}", path.display());
        }
        WriteStatus::SkippedExists => {
            println!("  · {label} already exists at {}", path.display());
        }
    }
}

/// Source of the resolved API key, used only by static doctor/setup reports.
///
/// These reports must not migrate a legacy secret store or acquire a
/// write-capable credential handle just to label a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiKeySource {
    ConfigDeclared,
    EnvDeclared,
    ExternalAuthDeclared,
    SecretStoreUnprobed,
    SecretStoreUnavailable,
    OAuth,
    ExternalConsent,
    NoAuth,
    LocalRuntime,
    Unknown,
}

/// What structural diagnostics can truthfully say about credential
/// availability without consulting environment values or durable stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CredentialAvailability {
    Present,
    NotRequired,
    Unknown,
    NotProbed,
    Unavailable,
}

impl CredentialAvailability {
    fn label(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::NotRequired => "not_required",
            Self::Unknown => "unknown",
            Self::NotProbed => "not_probed",
            Self::Unavailable => "unavailable",
        }
    }

    fn certifies_ready(self) -> bool {
        matches!(self, Self::Present | Self::NotRequired)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CredentialDiagnostic {
    source: ApiKeySource,
    availability: CredentialAvailability,
}

impl CredentialDiagnostic {
    const fn new(source: ApiKeySource, availability: CredentialAvailability) -> Self {
        Self {
            source,
            availability,
        }
    }
}

fn resolve_credential_diagnostic(config: &Config) -> CredentialDiagnostic {
    let Ok(identity) = config.active_provider_identity() else {
        return CredentialDiagnostic::new(
            ApiKeySource::Unknown,
            CredentialAvailability::Unavailable,
        );
    };
    let provider = identity.provider;
    let base_url = config.base_url_for_route(&identity);
    let auth_mode = config.auth_mode_for_provider(&identity);
    if crate::config::auth_mode_disables_api_key(auth_mode.as_deref()) {
        return CredentialDiagnostic::new(
            ApiKeySource::NoAuth,
            CredentialAvailability::NotRequired,
        );
    }
    if !crate::config::auth_mode_requires_api_key(auth_mode.as_deref())
        && (crate::config::provider_route_is_keyless_self_hosted(provider, &base_url)
            || crate::config::base_url_uses_local_host(&base_url))
    {
        return CredentialDiagnostic::new(
            ApiKeySource::LocalRuntime,
            CredentialAvailability::NotRequired,
        );
    }
    let custom_endpoint = config.provider_uses_custom_endpoint(&identity);
    if !custom_endpoint && provider == crate::config::ProviderKind::OpenaiCodex {
        return config
            .external_credential_consent_status(&identity)
            .filter(|status| status.route_state == "active")
            .map_or_else(
                || {
                    CredentialDiagnostic::new(
                        ApiKeySource::OAuth,
                        CredentialAvailability::NotProbed,
                    )
                },
                |_| {
                    CredentialDiagnostic::new(
                        ApiKeySource::ExternalConsent,
                        CredentialAvailability::NotProbed,
                    )
                },
            );
    }
    if !custom_endpoint
        && provider == crate::config::ProviderKind::Xai
        && auth_mode
            .as_deref()
            .is_some_and(crate::oauth::auth_mode_uses_xai_oauth)
    {
        return config
            .external_credential_consent_status(&identity)
            .filter(|status| status.route_state == "active")
            .map_or_else(
                || {
                    CredentialDiagnostic::new(
                        ApiKeySource::OAuth,
                        CredentialAvailability::NotProbed,
                    )
                },
                |_| {
                    CredentialDiagnostic::new(
                        ApiKeySource::ExternalConsent,
                        CredentialAvailability::NotProbed,
                    )
                },
            );
    }
    let provider_config = config.provider_config();
    let provider_config_key_kind = provider_config
        .and_then(|entry| entry.api_key.as_deref())
        .map(crate::config::classify_config_api_key_value);
    // DeepSeek-CN shares `[providers.deepseek]`'s key (the two identities
    // used to share the top-level `api_key`, #6394).
    let root_key_kind = (identity.key.as_str()
        == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id)
        .then(|| {
            config
                .builtin_provider_identity(crate::config::ProviderKind::Deepseek)
                .ok()
        })
        .flatten()
        .and_then(|owner| config.provider_config_for(&owner))
        .and_then(|entry| entry.api_key.as_deref())
        .map(crate::config::classify_config_api_key_value);

    if matches!(
        provider_config_key_kind,
        Some(crate::config::ConfigApiKeyValueKind::Literal)
    ) || matches!(
        root_key_kind,
        Some(crate::config::ConfigApiKeyValueKind::Literal)
    ) {
        CredentialDiagnostic::new(
            ApiKeySource::ConfigDeclared,
            CredentialAvailability::Present,
        )
    } else if config
        .provider_config()
        .and_then(|entry| entry.api_key_env.as_deref())
        .is_some_and(|name| !name.trim().is_empty())
    {
        CredentialDiagnostic::new(ApiKeySource::EnvDeclared, CredentialAvailability::NotProbed)
    } else if config
        .provider_config()
        .and_then(|entry| entry.auth.as_ref())
        .is_some()
    {
        CredentialDiagnostic::new(
            ApiKeySource::ExternalAuthDeclared,
            CredentialAvailability::NotProbed,
        )
    } else if matches!(
        provider_config_key_kind,
        Some(crate::config::ConfigApiKeyValueKind::SecretStoreSentinel)
    ) || matches!(
        root_key_kind,
        Some(crate::config::ConfigApiKeyValueKind::SecretStoreSentinel)
    ) {
        if config.should_skip_secret_store_for_provider(&identity) {
            return CredentialDiagnostic::new(
                ApiKeySource::SecretStoreUnavailable,
                CredentialAvailability::Unavailable,
            );
        }
        // The sentinel is a declaration that runtime resolution should use
        // the secret-store layer, never a literal key. Doctor does not read it.
        CredentialDiagnostic::new(
            ApiKeySource::SecretStoreUnprobed,
            CredentialAvailability::NotProbed,
        )
    } else if !config.should_skip_secret_store_for_provider(&identity) {
        // No literal config declaration was found, but this route can continue
        // through the durable store and ambient provider environment. Ordinary
        // doctor deliberately does not inspect either source.
        CredentialDiagnostic::new(
            ApiKeySource::SecretStoreUnprobed,
            CredentialAvailability::NotProbed,
        )
    } else {
        CredentialDiagnostic::new(ApiKeySource::Unknown, CredentialAvailability::Unknown)
    }
}

#[cfg(test)]
fn resolve_api_key_source(config: &Config) -> ApiKeySource {
    resolve_credential_diagnostic(config).source
}

#[cfg(test)]
fn provider_config_table_key(provider: crate::config::ProviderKind) -> String {
    format!(
        "providers.{}",
        codewhale_config::descriptors::compatibility_for_kind(provider).config_key
    )
}

fn count_dir_entries(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(std::result::Result::ok).count())
        .unwrap_or(0)
}

fn skills_count_for(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    crate::skills::SkillRegistry::discover(dir).len()
}

fn run_setup_status(
    config: &Config,
    workspace: &Path,
    plugins: &crate::plugins::PluginRegistry,
) -> Result<()> {
    use codewhale_palette as palette;
    use colored::Colorize;

    let (aqua_r, aqua_g, aqua_b) = palette::WHALE_ACTION_RGB;
    let (sky_r, sky_g, sky_b) = palette::WHALE_ACTION_RGB;

    println!(
        "{}",
        "Codewhale Status".truecolor(aqua_r, aqua_g, aqua_b).bold()
    );
    println!("{}", "===============".truecolor(sky_r, sky_g, sky_b));
    println!("workspace: {}", workspace.display());

    let credential = resolve_credential_diagnostic(config);
    match credential.source {
        ApiKeySource::ConfigDeclared => println!(
            "  {} api_key: literal config value structurally present",
            "✓".truecolor(aqua_r, aqua_g, aqua_b)
        ),
        ApiKeySource::EnvDeclared => println!(
            "  {} api_key: environment source declared (value not inspected)",
            "·".dimmed()
        ),
        ApiKeySource::ExternalAuthDeclared => println!(
            "  {} api_key: external auth source declared (value not inspected)",
            "·".dimmed()
        ),
        ApiKeySource::SecretStoreUnprobed => println!(
            "  {} api_key: secret store eligible (store not probed)",
            "·".dimmed()
        ),
        ApiKeySource::SecretStoreUnavailable => println!(
            "  {} api_key: secret-store sentinel declared, but this route cannot use that store",
            "!".truecolor(sky_r, sky_g, sky_b)
        ),
        ApiKeySource::OAuth => println!(
            "  {} oauth: Codewhale-owned route selected (token availability not probed)",
            "·".dimmed()
        ),
        ApiKeySource::ExternalConsent => println!(
            "  {} oauth: external read-only consent configured (credential file not probed)",
            "·".dimmed()
        ),
        ApiKeySource::NoAuth => println!(
            "  {} api_key: disabled for this route",
            "✓".truecolor(aqua_r, aqua_g, aqua_b)
        ),
        ApiKeySource::LocalRuntime => println!(
            "  {} api_key: not required for this local runtime",
            "✓".truecolor(aqua_r, aqua_g, aqua_b)
        ),
        ApiKeySource::Unknown => println!(
            "  {} api_key: unknown (credential environment and durable stores not inspected)",
            "·".dimmed()
        ),
    }
    println!(
        "  · credential availability: {}",
        credential.availability.label()
    );
    println!(
        "  · base_url: {}",
        crate::doctor::structural_url_authority(&config.active_route_base_url())
    );
    let model = config
        .default_text_model
        .clone()
        .unwrap_or_else(|| DEFAULT_TEXT_MODEL.to_string());
    println!("  · default_text_model: {model}");
    let (default_mode, default_mode_source) = doctor_runtime_default_mode();
    println!("  · default_mode: {default_mode} ({default_mode_source})");

    let mcp_path = config.mcp_config_path();
    let project_mcp_path = crate::mcp::workspace_mcp_config_path(workspace);
    let mcp_count =
        match crate::mcp::load_config_with_workspace_and_plugins(&mcp_path, workspace, plugins) {
            Ok(cfg) => cfg.servers.len(),
            Err(_) => 0,
        };
    let mcp_present = if mcp_path.exists() { "" } else { "  (missing)" };
    let project_mcp_present = if project_mcp_path.exists() {
        ""
    } else {
        "  (missing)"
    };
    println!(
        "  · mcp servers: {mcp_count} from {}{mcp_present} + {}{project_mcp_present}",
        mcp_path.display(),
        project_mcp_path.display()
    );

    let skills_dir = config.skills_dir();
    println!(
        "  · skills: {} at {}",
        skills_count_for(&skills_dir),
        crate::utils::display_path(&skills_dir)
    );

    let tools_dir = default_tools_dir();
    let tools_present = if tools_dir.exists() {
        ""
    } else {
        "  (missing — run `setup --tools`)"
    };
    println!(
        "  · tools: {} entries at {}{tools_present}",
        if tools_dir.exists() {
            count_dir_entries(&tools_dir)
        } else {
            0
        },
        crate::utils::display_path(&tools_dir)
    );

    let plugins_dir = default_plugins_dir();
    let plugins_present = if plugins_dir.exists() {
        ""
    } else {
        "  (missing — run `setup --plugins`)"
    };
    println!(
        "  · plugins: {} entries at {}{plugins_present}",
        if plugins_dir.exists() {
            count_dir_entries(&plugins_dir)
        } else {
            0
        },
        crate::utils::display_path(&plugins_dir)
    );

    let sandbox =
        crate::sandbox::get_platform_sandbox_with_bwrap_preference(config.prefers_bwrap());
    match sandbox {
        Some(kind) => println!(
            "  {} sandbox: {kind}",
            "✓".truecolor(aqua_r, aqua_g, aqua_b)
        ),
        None => println!(
            "  {} sandbox: unavailable (commands run best-effort)",
            "!".truecolor(sky_r, sky_g, sky_b)
        ),
    }

    println!("  {} {}", "·".dimmed(), dotenv_status_line(workspace));

    println!();
    println!("Run `codewhale doctor --json` for a machine-readable check.");
    Ok(())
}

fn dotenv_status_line(workspace: &Path) -> String {
    let dotenv = workspace.join(".env");
    if dotenv.exists() {
        return format!(
            ".env present at {} (literal provider credentials only)",
            dotenv.display()
        );
    }

    if workspace.join(".env.example").exists() {
        return ".env not present in workspace (run `cp .env.example .env` and edit)".to_string();
    }

    ".env not present in workspace".to_string()
}

fn run_setup_clean(checkpoints_dir: &Path, force: bool) -> Result<()> {
    use colored::Colorize;

    if !checkpoints_dir.exists() {
        println!(
            "Nothing to clean — checkpoints dir does not exist: {}",
            checkpoints_dir.display()
        );
        return Ok(());
    }

    let plan = collect_clean_targets(checkpoints_dir);
    if plan.targets.is_empty() {
        println!(
            "Nothing to clean — no checkpoint files in {}",
            checkpoints_dir.display()
        );
        return Ok(());
    }

    if !force {
        println!(
            "Would remove {} checkpoint file(s) (use --force to apply):",
            plan.targets.len()
        );
        for path in &plan.targets {
            println!("  · {}", path.display());
        }
        return Ok(());
    }

    let removed = execute_clean_plan(&plan)?;
    println!("{}", "Cleaned checkpoints:".bold());
    for path in &removed {
        println!("  ✓ {}", path.display());
    }
    Ok(())
}

fn run_session_diagnostics(args: SessionDiagnosticsArgs) -> Result<()> {
    let contents = std::fs::read_to_string(&args.path).with_context(|| {
        format!(
            "read session diagnostic JSONL from {}",
            crate::utils::display_path(&args.path)
        )
    })?;
    let summary = crate::session_diagnostics::analyze_session_failure_jsonl(&contents);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        println!(
            "{}",
            crate::session_diagnostics::format_redacted_failure_summary(&summary)
        );
    }
    Ok(())
}

/// Live API checks are explicit. Local endpoints have a separate opt-in because
/// an HTTP request can wake a desktop-managed daemon (notably Ollama.app).
fn doctor_should_probe_api(
    provider: crate::config::ProviderKind,
    base_url: &str,
    probes: crate::doctor::DoctorProbeRequest,
) -> bool {
    let local = crate::config::provider_route_is_keyless_self_hosted(provider, base_url)
        || crate::config::base_url_uses_local_host(base_url);
    probes.should_probe_api(local)
}

/// Providers whose credential *presence* `codewhale doctor` reports.
///
/// The retired Antigravity identity is a non-runnable tombstone kept only so
/// legacy tables deserialize and clear; doctor never advertises it as a slot.
fn doctor_api_key_providers() -> impl Iterator<Item = crate::config::ProviderKind> {
    crate::config::ProviderKind::all()
        .iter()
        .copied()
        .filter(|provider| *provider != crate::config::ProviderKind::Antigravity)
}

/// Doctor must never turn credential inspection into a refresh/write path.
/// OAuth connectivity is exercised by an ordinary user request instead;
/// doctor limits itself to non-mutating readiness inspection.
fn doctor_should_probe_auth(config: &Config) -> bool {
    let Ok(identity) = config.active_provider_identity() else {
        return false;
    };
    let provider = identity.provider;
    if provider == crate::config::ProviderKind::OpenaiCodex
        && !config.provider_uses_custom_endpoint(&identity)
    {
        return false;
    }
    let auth_mode = config.auth_mode_for_provider(&identity);
    if provider == crate::config::ProviderKind::Xai
        && auth_mode
            .as_deref()
            .is_some_and(crate::oauth::auth_mode_uses_xai_oauth)
    {
        return false;
    }
    !(provider == crate::config::ProviderKind::Moonshot
        && auth_mode
            .as_deref()
            .is_some_and(crate::config::auth_mode_uses_kimi_imported_token))
}

/// Run system diagnostics
async fn run_doctor(
    config: &Config,
    workspace: &Path,
    config_path_override: Option<&Path>,
    profile: Option<&str>,
    probes: crate::doctor::DoctorProbeRequest,
    plugins: &crate::plugins::PluginRegistry,
) {
    use codewhale_palette as palette;
    use colored::Colorize;

    let (accent_r, accent_g, accent_b) = palette::WHALE_HUMAN_RGB;
    let (sky_r, sky_g, sky_b) = palette::WHALE_ACTION_RGB;
    let (aqua_r, aqua_g, aqua_b) = palette::WHALE_ACTION_RGB;
    let (red_r, red_g, red_b) = palette::WHALE_ERROR_RGB;

    println!(
        "{}",
        "codewhale Doctor"
            .truecolor(accent_r, accent_g, accent_b)
            .bold()
    );
    println!("{}", "==================".truecolor(sky_r, sky_g, sky_b));
    // The answer comes before the detail (U7). A requested live probe runs
    // first so that answer can include it; the probe line is the only thing
    // printed while that check is in flight.
    let (verdict_state, _) = doctor_setup_state(config, workspace);
    let identity = config.active_provider_identity().ok();
    let api_target = doctor_api_target(config);
    let live_api_requested = identity.as_ref().is_some_and(|identity| {
        doctor_should_probe_api(identity.provider, &api_target.base_url, probes)
    });
    // The opt-in live check runs once, before the verdict, so the verdict,
    // the credential lines and API Connectivity all report the same result.
    let live_probe = if doctor_should_probe_auth(config) && live_api_requested {
        println!("{} Testing connection...", "·".dimmed());
        // Resolve a credential through the diagnostic-only store first, then
        // probe with an in-memory clone. Constructing the normal client from
        // the original config could otherwise trigger its legacy secret-store
        // migration while a user merely asks doctor to test connectivity.
        Some(match config.with_read_only_api_key_for_diagnostic() {
            Ok(diagnostic_config) => test_api_connectivity(&diagnostic_config).await,
            Err(error) => Err(error),
        })
    } else {
        None
    };
    // Presence and variable name only; the value is never held here.
    let env_key_source = crate::config::active_provider_env_api_key_source(config);
    let verdict = doctor_verdict(
        &verdict_state,
        identity
            .as_ref()
            .map_or("unavailable", |identity| identity.key.as_str()),
        &DoctorVerdictFacts {
            onboarded: crate::tui::onboarding::is_onboarded(),
            env_key_source: env_key_source.clone(),
            live_probe: live_probe.as_ref().map(Result::is_ok),
            live_probe_timed_out: live_probe
                .as_ref()
                .is_some_and(|result| result.as_ref().is_err_and(doctor_probe_timed_out)),
        },
    );
    println!("{}", verdict.truecolor(aqua_r, aqua_g, aqua_b).bold());
    println!();

    // Version info
    println!("{}", "Version Information:".bold());
    println!("  codewhale: {}", env!("CODEWHALE_BUILD_VERSION"));
    // A release binary needs no Rust toolchain; this line describes the host,
    // not the build, so a missing rustc must not read as a fault.
    println!("  host rustc: {}", rustc_version());
    println!();

    println!("{}", "Updates:".bold());
    crate::doctor::print_update_report(probes).await;
    println!();

    // Configuration summary
    let doctor_paths = match crate::doctor::DoctorPathReport::resolve(config_path_override) {
        Ok(paths) => paths,
        Err(error) => {
            println!("{}", "Resolved User Paths:".bold());
            println!(
                "  {} unavailable: {error:#}",
                "✗".truecolor(red_r, red_g, red_b)
            );
            return;
        }
    };
    println!("{}", "Configuration:".bold());
    let config_path = &doctor_paths.config;

    if tokio::fs::try_exists(config_path).await.unwrap_or(false) {
        println!(
            "  {} config.toml found at {}",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(config_path)
        );
        // Secret hygiene: name the keys, never the values. Plain-text config
        // is not a secret store.
        if let Ok(raw) = tokio::fs::read_to_string(config_path).await {
            let flagged = crate::doctor::config_credential_shaped_keys(&raw);
            if !flagged.is_empty() {
                println!(
                    "  {} credential-shaped value(s) in config.toml ({}): move them to the secret backend, then scrub the file — config.toml is plain text",
                    "!".truecolor(sky_r, sky_g, sky_b),
                    flagged.join(", ")
                );
            }
        }
        // Legacy top-level `base_url` / `api_key` (#6394): which moves the
        // file still has pending, which pair disagrees and which side is in
        // use. Sources only, never values.
        for note in &config.legacy_root.notes {
            let pending = !matches!(
                note,
                codewhale_config::legacy_root::LegacyRootNote::Conflict { .. }
            );
            println!(
                "  {} {note}{}",
                "!".truecolor(sky_r, sky_g, sky_b),
                if pending {
                    " (read that way already; `codewhale config migrate` updates the file)"
                } else {
                    ""
                }
            );
        }
    } else {
        println!(
            "  {} config.toml not found at {} (using defaults/env)",
            "!".truecolor(sky_r, sky_g, sky_b),
            crate::utils::display_path(config_path)
        );
    }
    println!("  workspace: {}", crate::utils::display_path(workspace));
    println!("  {}", doctor_search_provider_line(config));

    println!();
    println!("{}", "Resolved User Paths (read-only):".bold());
    for (label, path) in doctor_paths.entries() {
        println!("  · {label}: {}", crate::utils::display_path(path));
    }

    let secret_backend = codewhale_secrets::diagnose_secret_backend();
    println!();
    println!("{}", "Secret Backend (structural only):".bold());
    for line in crate::doctor::secret_backend_human_lines(&secret_backend) {
        println!("  · {line}");
    }

    println!();
    println!("{}", "Sessions:".bold());
    println!(
        "  · {}",
        crate::session_reconcile::last_run(&doctor_paths.sessions).map_or_else(
            || "no session repair has run yet (it runs at launch; `codewhale doctor --repair-sessions` runs it now)".to_string(),
            |summary| summary.doctor_detail()
        )
    );

    // State root (v0.8.44)
    println!();
    println!("{}", "State Root:".bold());
    let (code_home, legacy_home) = doctor_state_roots();
    let active_root = if code_home.exists() {
        &code_home
    } else if legacy_home.exists() {
        &legacy_home
    } else {
        &code_home
    };
    println!("  active: {}", crate::utils::display_path(active_root));
    if active_root != &code_home {
        println!(
            "  note: legacy {} found; start Codewhale once to trigger safe migration where available.",
            crate::utils::display_path(&legacy_home)
        );
    }
    if legacy_home.exists() && code_home.exists() {
        println!(
            "  dual roots: {} (primary) + {} (legacy)",
            crate::utils::display_path(&code_home),
            crate::utils::display_path(&legacy_home)
        );
    }
    let legacy_state_report = doctor_legacy_state_report(&code_home, &legacy_home);
    let session_recovery = doctor_session_recovery_report(
        &code_home,
        &legacy_home,
        codewhale_config::codewhale_home_is_explicit(),
    );
    print_doctor_legacy_state_report(
        &legacy_state_report,
        &session_recovery,
        (aqua_r, aqua_g, aqua_b),
        (sky_r, sky_g, sky_b),
    );
    print_doctor_stored_secrets_report(
        config_path_override.map(Path::to_path_buf),
        profile.map(str::to_owned),
    )
    .await;

    let (setup_state, setup_source) = doctor_setup_state(config, workspace);
    print_doctor_setup_report(
        config,
        workspace,
        &setup_state,
        setup_source,
        (aqua_r, aqua_g, aqua_b),
        (sky_r, sky_g, sky_b),
    );
    print_doctor_fleet_roster_layers(config, workspace);

    // Check API keys
    println!();
    println!("{}", "API Keys:".bold());

    // Per-provider state: env + config file only (no values printed).
    // Keep doctor/status prompt-free and credential-value-free even for
    // unsigned rebuilt binaries.
    for provider in doctor_api_key_providers() {
        let slot = provider.as_str();
        let provider_identity = config.builtin_provider_identity(provider).ok();
        let provider_config = provider_identity
            .as_ref()
            .and_then(|identity| config.provider_config_for(identity));
        let config_declared = provider_config.is_some_and(|entry| {
            entry.api_key.as_deref().is_some_and(|key| {
                crate::config::classify_config_api_key_value(key)
                    == crate::config::ConfigApiKeyValueKind::Literal
            })
        });
        let declared_env = provider_config
            .and_then(|entry| entry.api_key_env.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let env_source_declared = declared_env.is_some();
        // Presence only: name the variable that holds a key, never its value.
        let env_set = declared_env
            .filter(|name| {
                std::env::var_os(name)
                    .is_some_and(|value| !value.to_string_lossy().trim().is_empty())
            })
            .or_else(|| crate::config::provider_env_api_key_var(provider));
        let icon = if config_declared || env_source_declared || env_set.is_some() {
            "·".truecolor(aqua_r, aqua_g, aqua_b)
        } else {
            "·".dimmed()
        };
        println!(
            "  {} {slot}: env_source={}, config_source={}",
            icon,
            match env_set {
                Some(var) => doctor_env_key_label(var),
                None if env_source_declared => "declared (value not inspected)".to_string(),
                None => "not inspected".to_string(),
            },
            if config_declared {
                "declared (value not inspected)"
            } else {
                "not declared"
            }
        );
    }
    println!("  · credential precedence is unchanged; doctor does not inspect credential values");
    println!();
    println!(
        "{}",
        "External credential consent (configuration only):".bold()
    );
    for line in doctor_external_credential_consent_lines(config) {
        println!("  {line}");
    }

    println!();
    println!(
        "{}",
        "DeepSeek Harness integration (read-only detection):".bold()
    );
    for line in doctor_dsh_integration_lines(config, workspace) {
        println!("  {line}");
    }

    let credential = resolve_credential_diagnostic(config);
    let source_label = match credential.source {
        ApiKeySource::ConfigDeclared => "literal config value structurally present",
        ApiKeySource::EnvDeclared => "environment source declared; value not inspected",
        ApiKeySource::ExternalAuthDeclared => {
            "external auth source declared; credential not resolved"
        }
        ApiKeySource::SecretStoreUnprobed => "secret store eligible; store not probed",
        ApiKeySource::SecretStoreUnavailable => {
            "secret-store sentinel declared, but this route cannot use that store"
        }
        ApiKeySource::OAuth => "OAuth route configured; token availability not probed",
        ApiKeySource::ExternalConsent => "external consent configured; token file not read",
        ApiKeySource::NoAuth => "no-auth route",
        ApiKeySource::LocalRuntime => "local runtime; credentials not required",
        ApiKeySource::Unknown => "unknown; credential environment and stores not inspected",
    };
    match env_key_source.as_deref() {
        Some(source) => println!(
            "  {} active provider credential source: {}",
            "·".dimmed(),
            doctor_env_key_label(source)
        ),
        None => println!(
            "  {} active provider credential source: {source_label}",
            "·".dimmed()
        ),
    }
    println!(
        "  · active provider credential availability: {}",
        credential.availability.label()
    );
    match &live_probe {
        Some(Ok(())) => println!(
            "  {} active provider credential: accepted by the live API check",
            "✓".truecolor(aqua_r, aqua_g, aqua_b)
        ),
        // #6889: no answer in time says nothing about the credential.
        Some(Err(error)) if doctor_probe_timed_out(error) => println!(
            "  {} active provider credential: not confirmed, the live API check got no answer in time (see API Connectivity)",
            "·".dimmed()
        ),
        Some(Err(_)) => println!(
            "  {} active provider credential: live API check failed (see API Connectivity)",
            "✗".truecolor(red_r, red_g, red_b)
        ),
        None => {}
    }

    // API connectivity test
    println!();
    println!("{}", "API Connectivity:".bold());
    // Configured-vs-active honesty (DGF-01): doctor describes the route a
    // session launched NOW would resolve. It cannot see inside an already
    // running session, which keeps the route it resolved at its own launch.
    println!(
        "  · scope: configured route — what a session launched now would use; a running session keeps the route it resolved at launch (its TUI header shows the live route)"
    );
    println!("  · provider: {}", api_target.provider);
    println!(
        "  · base_url: {}",
        crate::doctor::structural_url_authority(&api_target.base_url)
    );
    match api_target.resolution {
        DoctorModelResolution::Resolved => {
            println!("  · model: {} (resolved)", api_target.model);
        }
        DoctorModelResolution::ConfiguredOnly => {
            println!(
                "  · model: {} (configured; route resolution unavailable)",
                api_target.model
            );
        }
    }
    let tls_status = doctor_tls_status(config);
    if !tls_status.certificate_verification {
        println!("  ! {}", tls_status.message);
        println!("    Prefer SSL_CERT_FILE with a trusted custom CA bundle when possible.");
    }
    let strict_tool_mode = doctor_strict_tool_mode_status(config);
    let strict_icon = match strict_tool_mode.status {
        "ready" => "✓".truecolor(aqua_r, aqua_g, aqua_b),
        "fallback_non_beta" | "custom_endpoint" => "!".truecolor(sky_r, sky_g, sky_b),
        _ => "·".dimmed(),
    };
    println!(
        "  {} strict_tool_mode: {}",
        strict_icon, strict_tool_mode.message
    );
    if let Some(recommended) = strict_tool_mode.recommended_base_url.as_deref() {
        println!(
            "    Use the {} endpoint for DeepSeek strict schemas.",
            crate::doctor::structural_url_authority(recommended)
        );
    }
    let capability = identity
        .as_ref()
        .map(|identity| crate::config::provider_capability(identity.provider, &api_target.model));
    if let Some(alias) = capability
        .as_ref()
        .and_then(|capability| capability.alias_deprecation.as_ref())
    {
        println!(
            "  ! model alias {} retires {}; switch to {}",
            alias.alias, alias.retirement_date, alias.replacement
        );
    }
    let endpoint_is_local = identity.as_ref().is_some_and(|identity| {
        crate::config::provider_route_is_keyless_self_hosted(
            identity.provider,
            &api_target.base_url,
        ) || crate::config::base_url_uses_local_host(&api_target.base_url)
    });
    if let Some(connectivity_result) = &live_probe {
        match connectivity_result {
            Ok(()) => {
                println!(
                    "  {} API connection successful",
                    "✓".truecolor(aqua_r, aqua_g, aqua_b)
                );
            }
            Err(e) => {
                let error_msg = e.to_string();
                let timed_out = doctor_probe_timed_out(e);
                println!(
                    "  {} {}",
                    "✗".truecolor(red_r, red_g, red_b),
                    if timed_out {
                        "API check got no answer in time"
                    } else {
                        "API connection failed"
                    }
                );
                let names_status =
                    |status| crate::mcp::oauth::text_names_http_status(&error_msg, status);
                let provider = identity
                    .as_ref()
                    .map(|identity| identity.provider)
                    .unwrap_or(crate::config::ProviderKind::Deepseek);
                if names_status("401") || error_msg.contains("Unauthorized") {
                    println!(
                        "    Invalid API key. Check `codewhale auth status`, {}, or config.toml",
                        doctor_provider_key_place(provider)
                    );
                } else if names_status("403") || error_msg.contains("Forbidden") {
                    println!(
                        "    API key lacks permissions. Verify the {} key is active.",
                        provider.provider().display_name()
                    );
                } else if timed_out
                    || error_msg.contains("timeout")
                    || error_msg.contains("Timeout")
                {
                    for line in doctor_timeout_recovery_lines(config) {
                        println!("    {line}");
                    }
                } else if error_msg.contains("dns") || error_msg.contains("resolve") {
                    println!("    DNS resolution failed. Check your network connection");
                } else if error_msg.contains("connect") {
                    println!("    Connection failed. Check firewall settings or try again");
                } else if crate::doctor::is_keyless_ds4_route(config) {
                    println!("    {error_msg}");
                } else {
                    println!(
                        "    Error details omitted because provider failures can contain credential material."
                    );
                }
            }
        }
    } else if !doctor_should_probe_auth(config) {
        println!(
            "  {} Live OAuth connectivity not checked by non-mutating doctor",
            "·".dimmed()
        );
        println!(
            "    Doctor never refreshes or rewrites credentials; exercise the route with a normal request."
        );
    } else {
        if endpoint_is_local {
            println!(
                "  {} Live connectivity not checked for this local endpoint",
                "·".dimmed()
            );
            println!(
                "    Run `codewhale doctor --probe-local` to opt in; the request may start a local service."
            );
        } else {
            println!(
                "  {} Live hosted connectivity not checked (offline default)",
                "·".dimmed()
            );
            println!("    Run `codewhale doctor --probe-api` to opt in.");
        }
    }

    println!();
    println!("{}", "Search Provider Reachability:".bold());
    let search_probe = crate::doctor::doctor_search_probe(config, probes).await;
    for line in crate::doctor::doctor_search_probe_lines(&search_probe) {
        println!("  {line}");
    }

    // MCP configuration
    println!();
    println!("{}", "MCP Servers (configuration only):".bold());
    println!("  · Static check only; no server process was started.");
    let features = config.features();
    if features.enabled(Feature::Mcp) {
        println!(
            "  {} MCP feature flag enabled",
            "✓".truecolor(aqua_r, aqua_g, aqua_b)
        );
    } else {
        println!(
            "  {} MCP feature flag disabled",
            "!".truecolor(sky_r, sky_g, sky_b)
        );
    }

    let mcp_config_path = config.mcp_config_path();
    let project_mcp_config_path = crate::mcp::workspace_mcp_config_path(workspace);
    if mcp_config_path.exists() {
        println!(
            "  {} MCP config found at {}",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(&mcp_config_path)
        );
    } else {
        println!(
            "  {} MCP config not found at {}",
            "·".dimmed(),
            crate::utils::display_path(&mcp_config_path)
        );
    }
    if project_mcp_config_path.exists() {
        println!(
            "  {} Project MCP config found at {}",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(&project_mcp_config_path)
        );
    } else {
        println!(
            "  {} Project MCP config not found at {}",
            "·".dimmed(),
            crate::utils::display_path(&project_mcp_config_path)
        );
    }

    match crate::mcp::load_config_with_workspace_and_plugins(&mcp_config_path, workspace, plugins) {
        Ok(cfg) if cfg.servers.is_empty() => {
            println!("  {} 0 merged server(s) configured", "·".dimmed());
            if !mcp_config_path.exists() && !project_mcp_config_path.exists() {
                println!("    Run `codewhale mcp init` or add `.codewhale/mcp.json`.");
            }
        }
        Ok(cfg) => {
            println!(
                "  {} {} merged server(s) configured",
                "·".dimmed(),
                cfg.servers.len()
            );
            let duplicate_computer_use = crate::mcp::duplicate_computer_use_servers(&cfg);
            for (name, server) in &cfg.servers {
                let status = doctor_check_mcp_server(server);
                let icon = match &status {
                    McpServerDoctorStatus::Ok(detail) => {
                        format!(
                            "  {} {name}: configuration valid; {}",
                            "✓".truecolor(aqua_r, aqua_g, aqua_b),
                            detail
                        )
                    }
                    McpServerDoctorStatus::Warning(detail) => {
                        format!(
                            "  {} {name}: configuration warning; {}",
                            "!".truecolor(sky_r, sky_g, sky_b),
                            detail
                        )
                    }
                    McpServerDoctorStatus::Error(detail) => {
                        format!(
                            "  {} {name}: configuration invalid; {}",
                            "✗".truecolor(red_r, red_g, red_b),
                            detail
                        )
                    }
                };
                println!("{icon}");
                if !server.is_enabled() {
                    println!("      disabled; live health not checked");
                } else {
                    println!(
                        "      process/protocol/backend: not checked; `codewhale mcp validate` explicitly starts and initializes configured servers"
                    );
                }
            }
            for (name, _) in &duplicate_computer_use {
                println!(
                    "  {} {}",
                    "!".truecolor(sky_r, sky_g, sky_b),
                    duplicate_computer_use_warning(name).trim_start()
                );
            }
            if probes.should_probe_mcp() {
                println!();
                println!(
                    "  {} Live MCP probe enabled: starting enabled servers; backend tool health remains untested.",
                    "!".truecolor(sky_r, sky_g, sky_b)
                );
                match crate::mcp::McpPool::from_config_path_with_workspace_and_plugins(
                    &mcp_config_path,
                    workspace,
                    std::sync::Arc::new(plugins.clone()),
                ) {
                    Ok(pool) => {
                        let mut pool =
                            pool.with_backend(crate::mcp::McpBackend::from_config(config));
                        let errors = pool.connect_all().await;
                        let failed = errors
                            .iter()
                            .map(|(name, _)| name.as_str())
                            .collect::<std::collections::BTreeSet<_>>();
                        for (name, server) in &cfg.servers {
                            if !server.is_enabled() {
                                continue;
                            }
                            if failed.contains(name.as_str()) {
                                println!(
                                    "      {} {name}: process/protocol unreachable; error details omitted",
                                    "✗".truecolor(red_r, red_g, red_b)
                                );
                            } else {
                                println!(
                                    "      {} {name}: process reachable and protocol initialized; backend tool health not checked",
                                    "✓".truecolor(aqua_r, aqua_g, aqua_b)
                                );
                            }
                        }
                    }
                    Err(_) => println!(
                        "      {} live MCP probe could not load merged configuration; details omitted",
                        "✗".truecolor(red_r, red_g, red_b)
                    ),
                }
            } else {
                println!(
                    "    Use codewhale doctor --probe-mcp to opt in to live process/protocol checks; it may start configured servers."
                );
            }
        }
        Err(_) => {
            println!(
                "  {} MCP configuration could not be loaded; details omitted",
                "✗".truecolor(red_r, red_g, red_b)
            );
        }
    }

    // Skills configuration
    println!();
    println!("{}", "Skills:".bold());
    let global_skills_dir = config.skills_dir();
    let agents_skills_dir = workspace.join(".agents").join("skills");
    let local_skills_dir = workspace.join("skills");
    let agents_global_skills_dir = crate::skills::agents_global_skills_dir();
    // #432: cross-tool skill discovery dirs. Presence is reported here
    // even though they sit lower in the precedence chain so users can
    // see at a glance whether a `.opencode/skills/`, `.claude/skills/`,
    // `.cursor/skills/`, or global agentskills.io directory is contributing
    // to the merged catalogue.
    let opencode_skills_dir = workspace.join(".opencode").join("skills");
    let claude_skills_dir = workspace.join(".claude").join("skills");
    let selected_skills_dir = if agents_skills_dir.exists() {
        agents_skills_dir.clone()
    } else if local_skills_dir.exists() {
        local_skills_dir.clone()
    } else if config.skills_dir.is_none()
        && let Some(global_agents) = agents_global_skills_dir.as_ref()
        && global_agents.exists()
    {
        global_agents.clone()
    } else {
        global_skills_dir.clone()
    };

    let describe_dir = |dir: &Path| -> usize {
        std::fs::read_dir(dir)
            .map(|entries| entries.filter_map(std::result::Result::ok).count())
            .unwrap_or(0)
    };

    if local_skills_dir.exists() {
        println!(
            "  {} local skills dir found at {} ({} items)",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(&local_skills_dir),
            describe_dir(&local_skills_dir)
        );
    } else {
        println!(
            "  {} local skills dir not found at {}",
            "·".dimmed(),
            crate::utils::display_path(&local_skills_dir)
        );
    }

    if agents_skills_dir.exists() {
        println!(
            "  {} .agents skills dir found at {} ({} items)",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(&agents_skills_dir),
            describe_dir(&agents_skills_dir)
        );
    } else {
        println!(
            "  {} .agents skills dir not found at {}",
            "·".dimmed(),
            crate::utils::display_path(&agents_skills_dir)
        );
    }

    if let Some(agents_global_skills_dir) = agents_global_skills_dir.as_ref() {
        if agents_global_skills_dir.exists() {
            println!(
                "  {} global .agents skills dir found at {} ({} items)",
                "✓".truecolor(aqua_r, aqua_g, aqua_b),
                crate::utils::display_path(agents_global_skills_dir),
                describe_dir(agents_global_skills_dir)
            );
        } else {
            println!(
                "  {} global .agents skills dir not found at {}",
                "·".dimmed(),
                crate::utils::display_path(agents_global_skills_dir)
            );
        }
    }

    if global_skills_dir.exists() {
        println!(
            "  {} global skills dir found at {} ({} items)",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(&global_skills_dir),
            describe_dir(&global_skills_dir)
        );
    } else {
        println!(
            "  {} global skills dir not found at {}",
            "·".dimmed(),
            crate::utils::display_path(&global_skills_dir)
        );
    }

    // #432: only print interop dirs when they're populated — empty
    // .opencode/.claude folders are common and would just clutter
    // the report with false-positive "absent" lines.
    if opencode_skills_dir.exists() {
        println!(
            "  {} .opencode skills dir found at {} ({} items)",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(&opencode_skills_dir),
            describe_dir(&opencode_skills_dir)
        );
    }
    if claude_skills_dir.exists() {
        println!(
            "  {} .claude skills dir found at {} ({} items)",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(&claude_skills_dir),
            describe_dir(&claude_skills_dir)
        );
    }

    println!(
        "  {} selected skills dir: {}",
        "·".dimmed(),
        crate::utils::display_path(&selected_skills_dir)
    );
    if !agents_skills_dir.exists()
        && !local_skills_dir.exists()
        && !agents_global_skills_dir
            .as_ref()
            .is_some_and(|dir| dir.exists())
        && !global_skills_dir.exists()
    {
        println!("    Run `codewhale setup --skills` (or add --local for ./skills).");
    }

    // Tools directory
    println!();
    println!("{}", "Tools:".bold());
    let tools_dir = default_tools_dir();
    if tools_dir.exists() {
        let count = count_dir_entries(&tools_dir);
        println!(
            "  {} tools dir found at {} ({} items)",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(&tools_dir),
            count
        );
    } else {
        println!(
            "  {} tools dir not found at {}",
            "·".dimmed(),
            crate::utils::display_path(&tools_dir)
        );
        println!("    Run `codewhale setup --tools` to scaffold a starter dir.");
    }

    // Plugins directory
    println!();
    println!("{}", "Plugins:".bold());
    let plugins_dir = default_plugins_dir();
    if plugins_dir.exists() {
        let count = count_dir_entries(&plugins_dir);
        println!(
            "  {} plugins dir found at {} ({} items)",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            crate::utils::display_path(&plugins_dir),
            count
        );
    } else {
        println!(
            "  {} plugins dir not found at {}",
            "·".dimmed(),
            crate::utils::display_path(&plugins_dir)
        );
        println!("    Run `codewhale setup --plugins` to scaffold a starter dir.");
    }

    // Storage surfaces (#422 / #440 / #500)
    println!();
    println!("{}", "Storage:".bold());
    if let Some(spillover_root) = crate::tools::truncate::spillover_root() {
        let (present, count) = if spillover_root.is_dir() {
            (true, count_dir_entries(&spillover_root))
        } else {
            (false, 0)
        };
        if present {
            println!(
                "  {} tool-output spillover at {} ({} file{})",
                "✓".truecolor(aqua_r, aqua_g, aqua_b),
                crate::utils::display_path(&spillover_root),
                count,
                if count == 1 { "" } else { "s" }
            );
        } else {
            println!(
                "  {} tool-output spillover dir not yet created at {}",
                "·".dimmed(),
                crate::utils::display_path(&spillover_root)
            );
        }
    }
    let stash = crate::composer_stash::diagnostic_stash_report();
    if let Some(stash_path) = stash.path.as_ref() {
        if let Some(error) = stash.error.as_deref() {
            println!(
                "  {} composer stash was not inspected at {}: {error}",
                "!".truecolor(sky_r, sky_g, sky_b),
                crate::utils::display_path(stash_path),
            );
        } else if stash.present {
            println!(
                "  {} composer stash at {} ({} parked draft{})",
                "✓".truecolor(aqua_r, aqua_g, aqua_b),
                crate::utils::display_path(stash_path),
                stash.count,
                if stash.count == 1 { "" } else { "s" }
            );
        } else {
            println!(
                "  {} composer stash empty (Ctrl+G or Ctrl+S in the composer to park a draft)",
                "·".dimmed()
            );
        }
    } else if let Some(error) = stash.error.as_deref() {
        println!(
            "  {} composer stash was not inspected: {error}",
            "!".truecolor(sky_r, sky_g, sky_b),
        );
    }

    // Tool dependencies — probe external binaries that individual
    // tools rely on (Python for code_execution, pdftotext for PDF
    // reading) so users see explicit ✓/✗ rather than the tool failing
    // at execution time with "program not found". New in v0.8.31.
    println!();
    println!("{}", "Tool Dependencies:".bold());

    match crate::dependencies::resolve_python_interpreter() {
        Some(name) => println!(
            "  {} Python: {} → code_execution tool registered",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            name
        ),
        None => {
            println!(
                "  {} Python: not found (tried {:?})",
                "✗".truecolor(red_r, red_g, red_b),
                crate::dependencies::PYTHON_CANDIDATES,
            );
            println!("    code_execution tool is NOT advertised to the model on this install.");
            println!("    Install Python 3 and ensure one of those names is on PATH:");
            match std::env::consts::OS {
                "macos" => {
                    println!("      brew install python@3.12   (or download from python.org)")
                }
                "linux" => println!(
                    "      sudo apt install python3    (Debian/Ubuntu) — or your distro's equivalent"
                ),
                "windows" => {
                    println!("      winget install Python.Python.3   (or download from python.org)")
                }
                other => println!("      install Python 3 for {other} from python.org"),
            }
        }
    }

    match crate::dependencies::resolve_node() {
        Some(_) => println!(
            "  {} Node.js: present → js_execution tool registered",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
        ),
        None => {
            println!(
                "  {} Node.js: not found (tried `node`)",
                "✗".truecolor(red_r, red_g, red_b),
            );
            println!("    js_execution tool is NOT advertised to the model on this install.");
            println!("    Install Node 18+ and ensure `node` is on PATH:");
            match std::env::consts::OS {
                "macos" => println!("      brew install node   (or download from nodejs.org)"),
                "linux" => println!(
                    "      sudo apt install nodejs    (Debian/Ubuntu) — or your distro's equivalent"
                ),
                "windows" => {
                    println!("      winget install OpenJS.NodeJS   (or download from nodejs.org)")
                }
                other => println!("      install Node.js for {other} from nodejs.org"),
            }
        }
    }

    {
        // The runtime the TypeScript extension host would use, resolved the
        // same way the host launcher does (`[extension_host] runtime`), and
        // with the host on, the OS sandbox it would get, planned (on Linux,
        // bwrap-probed) the way a launch plans it. Both run child processes,
        // so they stay off the async runtime. Known limit: the runtime probes
        // have no timeout, so a runtime binary that hangs on `--version`
        // stalls doctor here.
        let options = crate::extension_host::ExtensionHostOptions::from_config(
            config.extension_host.as_ref(),
        );
        let native_enabled = config
            .features()
            .enabled(crate::features::Feature::ExtensionHost);
        let mcp_enabled =
            crate::mcp::McpBackend::from_config(config) == crate::mcp::McpBackend::Host;
        let enabled = native_enabled || mcp_enabled;
        let tier = if mcp_enabled {
            crate::extension_host::tier::HostTier::Builtin
        } else {
            crate::extension_host::tier::HostTier::Plugin
        };
        let resolution = tokio::task::spawn_blocking(move || {
            let resolution = crate::dependencies::resolve_extension_host_runtime(
                options.runtime,
                options.node_override.as_deref(),
                options.bun_override.as_deref(),
            );
            let sandbox = resolution
                .selected
                .as_ref()
                .filter(|_| enabled)
                .map(|runtime| crate::extension_host::planned_sandbox(&options, runtime, tier));
            (resolution, sandbox)
        })
        .await;
        let state = if enabled {
            ""
        } else {
            " (unused: Native extensions and Host MCP are off)"
        };
        let failed = if enabled {
            "✗".truecolor(red_r, red_g, red_b)
        } else {
            "·".dimmed()
        };
        match resolution {
            Ok((resolution, sandbox)) => match &resolution.selected {
                Some(runtime) => {
                    println!(
                        "  {} Extension host runtime: {}{state}",
                        "✓".truecolor(aqua_r, aqua_g, aqua_b),
                        resolution.summary(),
                    );
                    println!(
                        "    Node remains the default; Bun default eligibility awaits measured runtime and platform isolation gates."
                    );
                    if native_enabled {
                        println!(
                            "    Native admission requires a verified OS sandbox; an unsandboxed pinned Builtin host does not qualify Native extensions."
                        );
                    }
                    println!(
                        "    {}",
                        crate::extension_host::supervisor::MemoryEnforcement::planned(runtime.kind)
                            .describe(crate::extension_host::supervisor::HOST_MEMORY_CAP)
                    );
                    match sandbox {
                        Some(Ok(sandbox)) => println!("    {sandbox}"),
                        Some(Err(error)) => {
                            println!("    host sandbox not determined: {error}")
                        }
                        None => {}
                    }
                }
                None => println!(
                    "  {failed} Extension host runtime: {}{state}",
                    resolution.failure(),
                ),
            },
            Err(error) => println!(
                "  {failed} Extension host runtime: the runtime probe did not finish ({error}){state}"
            ),
        }
    }

    match crate::dependencies::resolve_pandoc() {
        Some(_) => println!(
            "  {} pandoc: present → pandoc_convert tool registered",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
        ),
        None => {
            println!("  {} pandoc: not found (optional)", "·".dimmed(),);
            println!(
                "    pandoc_convert tool is NOT advertised to the model. Install pandoc 2.15+ to enable:"
            );
            match std::env::consts::OS {
                "macos" => println!("      brew install pandoc"),
                "linux" => println!(
                    "      release package from pandoc.org/installing.html (distro packages may predate 2.15)"
                ),
                "windows" => {
                    println!("      winget install JohnMacFarlane.Pandoc")
                }
                other => println!("      install pandoc for {other} from pandoc.org"),
            }
        }
    }

    match crate::dependencies::resolve_tesseract() {
        Some(_) => {
            if cfg!(target_os = "macos") {
                println!(
                    "  {} OCR: macOS Vision + tesseract available → image_ocr/read_file screenshot OCR enabled",
                    "✓".truecolor(aqua_r, aqua_g, aqua_b),
                );
            } else {
                println!(
                    "  {} tesseract: present → image_ocr/read_file screenshot OCR enabled",
                    "✓".truecolor(aqua_r, aqua_g, aqua_b),
                );
            }
        }
        None => {
            if cfg!(target_os = "macos") {
                println!(
                    "  {} OCR: macOS Vision available → image_ocr/read_file screenshot OCR enabled",
                    "✓".truecolor(aqua_r, aqua_g, aqua_b),
                );
                println!(
                    "    tesseract not found (optional; install only for alternate OCR packs)."
                );
            } else {
                println!("  {} tesseract: not found (optional)", "·".dimmed(),);
                println!(
                    "    image_ocr tool is NOT advertised to the model. Install tesseract to enable:"
                );
                match std::env::consts::OS {
                    "macos" => println!("      brew install tesseract"),
                    "linux" => println!(
                        "      sudo apt install tesseract-ocr    (Debian/Ubuntu) — or your distro's equivalent"
                    ),
                    "windows" => println!("      winget install UB-Mannheim.TesseractOCR"),
                    other => {
                        println!("      install tesseract for {other} from tesseract-ocr.github.io")
                    }
                }
            }
        }
    }

    // PDF text extraction is an optional integration. Codewhale itself stays
    // a single required executable; file and web tools report a typed
    // failed `binary_unavailable` result when Poppler is not installed.
    match crate::dependencies::resolve_pdftotext() {
        Some(_) => println!(
            "  {} pdftotext: available → PDF text extraction enabled",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
        ),
        None => {
            println!(
                "  {} pdftotext: not found (optional; PDF text reads fail as `binary_unavailable`)",
                "·".dimmed(),
            );
            match std::env::consts::OS {
                "macos" => println!("    Install via: brew install poppler"),
                "linux" => {
                    println!("    Install via: sudo apt install poppler-utils   (Debian/Ubuntu)")
                }
                "windows" => println!(
                    "    Install Poppler for Windows from https://blog.alivate.com.au/poppler-windows/"
                ),
                _ => {}
            }
        }
    }

    // Terminal-quirk overrides currently active. Mirrors the env
    // signals checked by `Settings::apply_env_overrides` so users
    // can see at a glance which a11y/compat overrides fired.
    println!();
    println!("{}", "Terminal Quirks:".bold());
    let term_program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let term_program_lc = term_program.to_ascii_lowercase();
    let mut any_quirk = false;
    if matches!(term_program.as_str(), "vscode" | "ghostty") {
        println!(
            "  {} TERM_PROGRAM={} → low_motion + fancy_animations=false (auto)",
            "•".truecolor(sky_r, sky_g, sky_b),
            term_program
        );
        any_quirk = true;
    }
    if term_program == "Termius"
        || std::env::var_os("SSH_CLIENT").is_some_and(|v| !v.is_empty())
        || std::env::var_os("SSH_TTY").is_some_and(|v| !v.is_empty())
    {
        println!(
            "  {} SSH/Termius session → low_motion + fancy_animations=false (auto, #1433)",
            "•".truecolor(sky_r, sky_g, sky_b)
        );
        any_quirk = true;
    }
    if term_program_lc.contains("ptyxis")
        || std::env::var_os("PTYXIS_VERSION").is_some_and(|v| !v.is_empty())
    {
        println!(
            "  {} Ptyxis detected → synchronized_output=off (auto, v0.8.31)",
            "•".truecolor(sky_r, sky_g, sky_b)
        );
        any_quirk = true;
    }
    if crate::settings::detected_legacy_windows_console_host() {
        println!(
            "  {} legacy Windows console host → low_motion + fancy_animations=false + bracketed_paste=false + synchronized_output=off (auto)",
            "•".truecolor(sky_r, sky_g, sky_b)
        );
        any_quirk = true;
    }
    if !any_quirk {
        println!(
            "  {} no env-driven terminal-quirk overrides active",
            "·".dimmed()
        );
    }

    // Platform and sandbox checks
    println!();
    println!("{}", "Platform:".bold());
    println!("  OS: {}", std::env::consts::OS);
    println!("  Arch: {}", std::env::consts::ARCH);

    let sandbox =
        crate::sandbox::get_platform_sandbox_with_bwrap_preference(config.prefers_bwrap());
    if let Some(kind) = sandbox {
        println!(
            "  {} sandbox available: {}",
            "✓".truecolor(aqua_r, aqua_g, aqua_b),
            kind
        );
    } else {
        println!(
            "  {} sandbox not available (commands run best-effort)",
            "!".truecolor(sky_r, sky_g, sky_b)
        );
    }

    println!();
    println!("{}", verdict.truecolor(aqua_r, aqua_g, aqua_b).bold());
}

/// Human-facing facts the verdict uses beyond the setup-state record. None of
/// these change structural Setup/Fleet readiness (the JSON contract): a set
/// environment variable is reported, never certified, until a live probe.
#[derive(Debug, Clone, Default)]
struct DoctorVerdictFacts {
    /// The TUI's own first-run receipt (`.onboarded`). TUI onboarding finishes
    /// after its key gate but never fills the `/setup` wizard's language and
    /// constitution steps, so `first_run_ready()` alone misreads it.
    onboarded: bool,
    /// Name of the env place holding the active provider's key, from
    /// `config::active_provider_env_api_key_source`; never the value.
    env_key_source: Option<String>,
    /// Outcome of the opt-in live API check; `None` when it did not run.
    live_probe: Option<bool>,
    /// The live check failed by running out of time, not by being refused
    /// (#6889). A model the provider is still loading looks exactly like this.
    live_probe_timed_out: bool,
}

/// `set via <VAR> (value not shown; not checked offline)`.
fn doctor_env_key_label(source: &str) -> String {
    format!("set via {source} (value not shown; not checked offline)")
}

/// Where a rejected key might live, named without reading it. A route that
/// binds `api_key_env` is named by that variable; otherwise the provider's
/// first ambient variable.
fn doctor_provider_key_place(provider: crate::config::ProviderKind) -> String {
    provider
        .provider()
        .env_vars()
        .first()
        .copied()
        .unwrap_or("the provider environment variable")
        .to_string()
}

/// Doctor's one-line answer: ready, or the single next step (U7). Readiness
/// starts from the setup lane's record. Offline doctor never reads the secret
/// store, so a stored key is "not checked", not missing; "Not ready" is kept
/// for a missing route, a key nothing can account for, or a failed probe.
fn doctor_verdict(
    state: &codewhale_config::SetupState,
    provider: &str,
    facts: &DoctorVerdictFacts,
) -> String {
    use codewhale_config::StepStatus;
    const PROBE_HINT: &str = "run `codewhale doctor --probe-api` to verify";
    match facts.live_probe {
        Some(false) if facts.live_probe_timed_out => {
            return format!(
                "Not confirmed: the live {provider} API check got no answer in time → the model may still be loading, so wait a minute and run `codewhale doctor --probe-api` again; see API Connectivity below."
            );
        }
        Some(false) => {
            return format!(
                "Not ready: the live {provider} API check failed → see API Connectivity below; `codewhale auth set --provider {provider}` replaces a rejected key."
            );
        }
        Some(true) => return format!("Ready: the live {provider} API check passed."),
        None => {}
    }
    let env_ready = facts.env_key_source.as_deref().map(|source| {
        format!("Ready: {provider} key is set via {source} (not checked offline; {PROBE_HINT}).")
    });
    // NeedsAction means a named route exists but its key is missing, unchecked
    // or failed. Configured routes can be used without a prior probe.
    // `first_run_ready` accepts NeedsAction (a failed key still reaches the
    // wizard's ready screen), so check the provider first.
    match state.status(codewhale_config::SetupStep::ProviderModel) {
        StepStatus::Verified if state.first_run_ready() || facts.onboarded => {
            "Ready: setup is complete.".to_string()
        }
        StepStatus::Configured | StepStatus::Verified
            if state.first_run_ready() || facts.onboarded =>
        {
            format!("Ready: setup is complete (saved key not checked offline; {PROBE_HINT}).")
        }
        StepStatus::Configured | StepStatus::Verified => env_ready.unwrap_or_else(|| {
            "Not ready: first-run setup is unfinished → run `codewhale setup`.".to_string()
        }),
        StepStatus::NeedsAction => env_ready.unwrap_or_else(|| {
            // A derived NeedsAction only means offline doctor cannot see the
            // stored key; onboarding already gated on one. A NeedsAction the
            // setup lane persisted is a real missing or failed key.
            if state.inherited && facts.onboarded {
                format!(
                    "Ready: onboarding is complete (saved {provider} key not checked offline; {PROBE_HINT})."
                )
            } else {
                format!(
                    "Not ready: the {provider} route has no verified key → save one with /provider in Codewhale or `codewhale auth set --provider {provider}`; `codewhale doctor --probe-api` checks a key already saved."
                )
            }
        }),
        _ => env_ready.unwrap_or_else(|| {
            "Not ready: no model provider set up → run /provider in Codewhale, or `codewhale setup`."
                .to_string()
        }),
    }
}

#[cfg(test)]
mod doctor_verdict_tests {
    #[test]
    fn a_fresh_home_is_not_ready_and_names_the_provider_step() {
        let verdict = super::doctor_verdict(
            &codewhale_config::SetupState::default(),
            "deepseek",
            &super::DoctorVerdictFacts::default(),
        );
        assert!(verdict.starts_with("Not ready"), "{verdict}");
        assert!(verdict.contains("/provider"), "{verdict}");
    }

    #[test]
    fn a_route_without_a_verified_key_is_not_called_missing() {
        // A fresh home derives NeedsAction for the default route because
        // doctor does not read saved keys; the verdict must not claim there is
        // no provider, and names both the headless fix and the probe.
        use codewhale_config::{SetupState, SetupStep, StepEntry, StepStatus};
        let mut state = SetupState::default();
        state.set_step(
            SetupStep::ProviderModel,
            StepEntry::new(StepStatus::NeedsAction, true, "inherited"),
        );
        let verdict =
            super::doctor_verdict(&state, "deepseek", &super::DoctorVerdictFacts::default());
        assert!(verdict.starts_with("Not ready"), "{verdict}");
        assert!(!verdict.contains("no model provider"), "{verdict}");
        assert!(
            verdict.contains("`codewhale auth set --provider deepseek`"),
            "{verdict}"
        );
        assert!(verdict.contains("--probe-api"), "{verdict}");
    }

    #[test]
    fn finished_setup_with_an_unconfirmed_key_is_not_ready() {
        use codewhale_config::{
            ConstitutionChoice, RuntimePostureSource, SetupState, SetupStep, StepEntry, StepStatus,
        };
        let mut state = SetupState::default();
        state.set_step(
            SetupStep::Language,
            StepEntry::new(StepStatus::Verified, true, "0.10.1"),
        );
        state.set_step(
            SetupStep::ProviderModel,
            StepEntry::new(StepStatus::NeedsAction, true, "0.10.1"),
        );
        state.runtime_posture_source = RuntimePostureSource::Confirmed;
        state.constitution_choice = ConstitutionChoice::Bundled;
        assert!(state.first_run_ready(), "fixture must be wizard-ready");
        let verdict =
            super::doctor_verdict(&state, "deepseek", &super::DoctorVerdictFacts::default());
        assert!(verdict.starts_with("Not ready"), "{verdict}");
        assert!(verdict.contains("/provider"), "{verdict}");

        state.set_step(
            SetupStep::ProviderModel,
            StepEntry::new(StepStatus::Verified, true, "0.10.1"),
        );
        assert_eq!(
            super::doctor_verdict(&state, "deepseek", &super::DoctorVerdictFacts::default()),
            "Ready: setup is complete."
        );
    }

    fn facts(
        onboarded: bool,
        env_key_source: Option<&str>,
        live_probe: Option<bool>,
    ) -> super::DoctorVerdictFacts {
        super::DoctorVerdictFacts {
            onboarded,
            env_key_source: env_key_source.map(str::to_string),
            live_probe,
            live_probe_timed_out: false,
        }
    }

    #[test]
    fn completed_tui_onboarding_is_not_unfinished_setup() {
        // TUI onboarding records the route step only; never the wizard's
        // language/constitution steps.
        use codewhale_config::{SetupState, SetupStep, StepEntry, StepStatus};
        let mut state = SetupState::default();
        state.set_step(
            SetupStep::ProviderModel,
            StepEntry::new(StepStatus::Configured, true, "0.10.1"),
        );
        assert!(!state.first_run_ready(), "fixture is not wizard-ready");
        let verdict = super::doctor_verdict(&state, "openai", &facts(true, None, None));
        assert!(verdict.starts_with("Ready: setup is complete"), "{verdict}");
        assert!(verdict.contains("--probe-api"), "{verdict}");
        assert!(!verdict.contains("unfinished"), "{verdict}");

        // Without the receipt (or a key) the wizard's verdict stands.
        let verdict = super::doctor_verdict(&state, "openai", &facts(false, None, None));
        assert!(
            verdict.contains("first-run setup is unfinished"),
            "{verdict}"
        );
    }

    #[test]
    fn onboarded_home_without_a_record_is_ready_but_a_recorded_failure_is_not() {
        use codewhale_config::{InheritedConfigFacts, SetupState};
        let derived = SetupState::derive_inherited(&InheritedConfigFacts {
            has_provider_route: true,
            ..Default::default()
        });
        let verdict = super::doctor_verdict(&derived, "openai", &facts(true, None, None));
        assert!(
            verdict.starts_with("Ready: onboarding is complete"),
            "{verdict}"
        );

        let mut recorded = derived.clone();
        recorded.inherited = false;
        let verdict = super::doctor_verdict(&recorded, "openai", &facts(true, None, None));
        assert!(verdict.starts_with("Not ready"), "{verdict}");
    }

    #[test]
    fn a_set_env_key_is_ready_and_named_for_any_provider() {
        use codewhale_config::{InheritedConfigFacts, SetupState};
        // Structural derivation keeps NeedsAction: env never certifies it.
        let derived = SetupState::derive_inherited(&InheritedConfigFacts {
            has_provider_route: true,
            ..Default::default()
        });
        let verdict = super::doctor_verdict(
            &derived,
            "anthropic",
            &facts(false, Some("ANTHROPIC_API_KEY"), None),
        );
        assert_eq!(
            verdict,
            "Ready: anthropic key is set via ANTHROPIC_API_KEY (not checked offline; run `codewhale doctor --probe-api` to verify)."
        );
    }

    #[test]
    fn the_live_probe_result_decides_the_verdict() {
        use codewhale_config::{SetupState, SetupStep, StepEntry, StepStatus};
        let mut state = SetupState::default();
        state.set_step(
            SetupStep::ProviderModel,
            StepEntry::new(StepStatus::NeedsAction, true, "0.10.1"),
        );
        let verdict = super::doctor_verdict(&state, "openai", &facts(false, None, Some(true)));
        assert!(
            verdict.starts_with("Ready: the live openai API check passed"),
            "{verdict}"
        );

        let verdict = super::doctor_verdict(
            &state,
            "openai",
            &facts(true, Some("OPENAI_API_KEY"), Some(false)),
        );
        assert!(verdict.starts_with("Not ready"), "{verdict}");
        assert!(verdict.contains("API Connectivity"), "{verdict}");
    }

    /// #6889: a live check that ran out of time has not shown the key or the
    /// route to be wrong, so the verdict does not say "Not ready" or point at
    /// replacing the key.
    #[test]
    fn a_live_probe_that_timed_out_is_not_reported_as_a_rejected_key() {
        let state = codewhale_config::SetupState::default();
        let verdict = super::doctor_verdict(
            &state,
            "openai",
            &super::DoctorVerdictFacts {
                live_probe_timed_out: true,
                ..facts(true, Some("OPENAI_API_KEY"), Some(false))
            },
        );
        assert!(verdict.starts_with("Not confirmed"), "{verdict}");
        assert!(verdict.contains("may still be loading"), "{verdict}");
        assert!(verdict.contains("--probe-api"), "{verdict}");
        assert!(!verdict.contains("auth set"), "{verdict}");

        let timed_out = anyhow::Error::new(crate::llm_client::LlmError::Timeout(
            super::DOCTOR_PROBE_TIMEOUT,
        ))
        .context("live check");
        assert!(super::doctor_probe_timed_out(&timed_out));
        let refused = anyhow::Error::new(crate::llm_client::LlmError::ServerError {
            status: 502,
            message: "resources busy".to_string(),
        });
        assert!(!super::doctor_probe_timed_out(&refused));
        assert!(!super::doctor_probe_timed_out(&anyhow::anyhow!(
            "connect timeout"
        )));
    }

    #[test]
    fn env_key_source_names_the_providers_own_variable_without_its_value() {
        let _lock = crate::test_support::lock_test_env();
        let _cli = crate::test_support::EnvVarGuard::remove(codewhale_config::CLI_API_KEY_ENV);
        let _openai = crate::test_support::EnvVarGuard::set("OPENAI_API_KEY", "MUST-NOT-BE-SHOWN");
        let config = crate::config::Config {
            provider: Some("openai".to_string()),
            ..Default::default()
        };
        let source = crate::config::active_provider_env_api_key_source(&config)
            .expect("ambient key present");
        assert_eq!(source, "OPENAI_API_KEY");
        let label = super::doctor_env_key_label(&source);
        assert_eq!(
            label,
            "set via OPENAI_API_KEY (value not shown; not checked offline)"
        );
        assert!(!label.contains("MUST-NOT-BE-SHOWN"));
        // The structural contract is untouched: env presence is not readiness.
        assert!(
            !super::resolve_credential_diagnostic(&config)
                .availability
                .certifies_ready()
        );

        let _openai = crate::test_support::EnvVarGuard::remove("OPENAI_API_KEY");
        assert_eq!(
            crate::config::active_provider_env_api_key_source(&config),
            None
        );
    }
}

const DOCTOR_LEGACY_STATE_ITEMS: &[&str] = &[
    "sessions",
    "tasks",
    "skills",
    "slop_ledger",
    "trophies",
    "catalog",
    "review-receipts",
    "config.toml",
    "settings.toml",
    "mcp.json",
];
const DOCTOR_SESSION_RECOVERY_HUMAN_SAMPLE_LIMIT: usize = 20;
const DOCTOR_SESSION_RECOVERY_JSON_SAMPLE_LIMIT: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorLegacyStateStatus {
    PrimaryOnly,
    LegacyOnly,
    Both,
    Absent,
}

impl DoctorLegacyStateStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryOnly => "primary_only",
            Self::LegacyOnly => "legacy_only",
            Self::Both => "both",
            Self::Absent => "absent",
        }
    }
}

#[derive(Debug, Clone)]
struct DoctorLegacyStateEntry {
    name: &'static str,
    primary_path: PathBuf,
    legacy_path: PathBuf,
    primary_present: bool,
    legacy_present: bool,
    status: DoctorLegacyStateStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorSessionRecoveryStatus {
    Isolated,
    NoLegacySessions,
    MigrationPending,
    MigrationIncomplete,
    MigrationComplete,
    ScanFailed,
}

impl DoctorSessionRecoveryStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::NoLegacySessions => "no_legacy_sessions",
            Self::MigrationPending => "migration_pending",
            Self::MigrationIncomplete => "migration_incomplete",
            Self::MigrationComplete => "migration_complete",
            Self::ScanFailed => "scan_failed",
        }
    }
}

#[derive(Debug, Clone)]
struct DoctorRecoverableSessionEntry {
    name: PathBuf,
    source_path: PathBuf,
    destination_path: PathBuf,
}

#[derive(Debug, Clone)]
struct DoctorSessionRecoveryReport {
    status: DoctorSessionRecoveryStatus,
    primary_sessions_path: PathBuf,
    legacy_sessions_path: PathBuf,
    codewhale_home_is_explicit: bool,
    legacy_session_file_count: usize,
    already_present_file_count: usize,
    recoverable_file_count: usize,
    /// Bounded filename/path sample; the total is `recoverable_file_count`.
    recoverable: Vec<DoctorRecoverableSessionEntry>,
    error: Option<String>,
}

impl DoctorSessionRecoveryReport {
    fn needs_attention(&self) -> bool {
        matches!(
            self.status,
            DoctorSessionRecoveryStatus::MigrationPending
                | DoctorSessionRecoveryStatus::MigrationIncomplete
                | DoctorSessionRecoveryStatus::ScanFailed
        )
    }
}

fn doctor_legacy_state_status(
    primary_present: bool,
    legacy_present: bool,
) -> DoctorLegacyStateStatus {
    match (primary_present, legacy_present) {
        (true, false) => DoctorLegacyStateStatus::PrimaryOnly,
        (false, true) => DoctorLegacyStateStatus::LegacyOnly,
        (true, true) => DoctorLegacyStateStatus::Both,
        (false, false) => DoctorLegacyStateStatus::Absent,
    }
}

fn doctor_state_roots() -> (PathBuf, PathBuf) {
    let code_home =
        codewhale_config::codewhale_home().unwrap_or_else(|_| PathBuf::from("~/.codewhale"));
    let legacy_home = if codewhale_config::codewhale_home_is_explicit() {
        code_home.join(codewhale_config::LEGACY_APP_DIR)
    } else {
        codewhale_config::legacy_deepseek_home().unwrap_or_else(|_| PathBuf::from("~/.deepseek"))
    };
    (code_home, legacy_home)
}

fn doctor_legacy_state_report(
    primary_root: &Path,
    legacy_root: &Path,
) -> Vec<DoctorLegacyStateEntry> {
    DOCTOR_LEGACY_STATE_ITEMS
        .iter()
        .copied()
        .map(|name| {
            let primary_path = primary_root.join(name);
            let legacy_path = legacy_root.join(name);
            let primary_present = primary_path.exists();
            let legacy_present = legacy_path.exists();
            let status = doctor_legacy_state_status(primary_present, legacy_present);
            DoctorLegacyStateEntry {
                name,
                primary_path,
                legacy_path,
                primary_present,
                legacy_present,
                status,
            }
        })
        .collect()
}

/// Compare legacy and primary session filenames without opening session files.
///
/// This is deliberately separate from `SessionManager::default_location()`:
/// constructing the manager can trigger the additive legacy migration, while
/// doctor must remain a read-only diagnostic. Session history is stored as
/// top-level JSON files. Directories (including `checkpoints`) and symlinks
/// observed during the scan are ignored, so the diagnostic does not
/// intentionally traverse checkpoint internals or link targets. These checks
/// are best-effort observations, not a race-free no-follow guarantee.
/// A matching filename is only a regular-file counterpart check: doctor does
/// not parse or compare session descriptors.
fn doctor_session_recovery_report(
    primary_root: &Path,
    legacy_root: &Path,
    codewhale_home_is_explicit: bool,
) -> DoctorSessionRecoveryReport {
    let primary_sessions_path = primary_root.join("sessions");
    let legacy_sessions_path = legacy_root.join("sessions");
    let mut report = DoctorSessionRecoveryReport {
        status: DoctorSessionRecoveryStatus::NoLegacySessions,
        primary_sessions_path,
        legacy_sessions_path,
        codewhale_home_is_explicit,
        legacy_session_file_count: 0,
        already_present_file_count: 0,
        recoverable_file_count: 0,
        recoverable: Vec::new(),
        error: None,
    };

    if codewhale_home_is_explicit {
        report.status = DoctorSessionRecoveryStatus::Isolated;
        return report;
    }

    let legacy_root_is_present =
        match doctor_session_directory_is_safe(legacy_root, "legacy state root") {
            Ok(present) => present,
            Err(error) => {
                report.status = DoctorSessionRecoveryStatus::ScanFailed;
                report.error = Some(error);
                return report;
            }
        };
    if !legacy_root_is_present {
        return report;
    }
    if let Err(error) = doctor_session_directory_is_safe(primary_root, "primary state root") {
        report.status = DoctorSessionRecoveryStatus::ScanFailed;
        report.error = Some(error);
        return report;
    }

    let legacy_sessions_are_present = match doctor_session_directory_is_safe(
        &report.legacy_sessions_path,
        "legacy sessions root",
    ) {
        Ok(present) => present,
        Err(error) => {
            report.status = DoctorSessionRecoveryStatus::ScanFailed;
            report.error = Some(error);
            return report;
        }
    };
    if !legacy_sessions_are_present {
        return report;
    }
    let primary_sessions_are_present = match doctor_session_directory_is_safe(
        &report.primary_sessions_path,
        "primary sessions root",
    ) {
        Ok(present) => present,
        Err(error) => {
            report.status = DoctorSessionRecoveryStatus::ScanFailed;
            report.error = Some(error);
            return report;
        }
    };

    let entries = match std::fs::read_dir(&report.legacy_sessions_path) {
        Ok(entries) => entries,
        Err(err) => {
            report.status = DoctorSessionRecoveryStatus::ScanFailed;
            report.error = Some(format!(
                "could not inspect legacy session filenames at {}: {err}",
                crate::utils::display_path(&report.legacy_sessions_path)
            ));
            return report;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                report.status = DoctorSessionRecoveryStatus::ScanFailed;
                report.error = Some(format!(
                    "could not inspect an entry under {}: {err}",
                    crate::utils::display_path(&report.legacy_sessions_path)
                ));
                return report;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) => {
                report.status = DoctorSessionRecoveryStatus::ScanFailed;
                report.error = Some(format!(
                    "could not inspect legacy session entry metadata under {}: {err}",
                    crate::utils::display_path(&report.legacy_sessions_path)
                ));
                return report;
            }
        };
        if !file_type.is_file() || entry.path().extension().is_none_or(|ext| ext != "json") {
            continue;
        }

        report.legacy_session_file_count += 1;
        let name = PathBuf::from(entry.file_name());
        let destination_path = report.primary_sessions_path.join(&name);
        match std::fs::symlink_metadata(&destination_path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                report.already_present_file_count += 1;
            }
            Ok(metadata) => {
                report.status = DoctorSessionRecoveryStatus::ScanFailed;
                let shape = if metadata.file_type().is_symlink() {
                    "destination session entry is a symlink"
                } else {
                    "destination session entry is not a regular file"
                };
                report.error = Some(format!(
                    "could not inspect destination session metadata at {}: {shape}",
                    crate::utils::display_path(&destination_path)
                ));
                return report;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                report.recoverable_file_count += 1;
                record_doctor_recoverable_session(
                    &mut report.recoverable,
                    DoctorRecoverableSessionEntry {
                        source_path: entry.path(),
                        destination_path,
                        name,
                    },
                );
            }
            Err(err) => {
                report.status = DoctorSessionRecoveryStatus::ScanFailed;
                report.error = Some(format!(
                    "could not inspect destination metadata at {}: {err}",
                    crate::utils::display_path(&destination_path)
                ));
                return report;
            }
        }
    }

    report.status = if report.legacy_session_file_count == 0 {
        DoctorSessionRecoveryStatus::NoLegacySessions
    } else if report.recoverable_file_count == 0 {
        DoctorSessionRecoveryStatus::MigrationComplete
    } else if primary_sessions_are_present {
        DoctorSessionRecoveryStatus::MigrationIncomplete
    } else {
        DoctorSessionRecoveryStatus::MigrationPending
    };
    report
}

/// Validate a session-state directory from observed metadata.
///
/// `doctor` only compares top-level filenames. It rejects a state-root or
/// sessions-root symlink observed during inspection rather than using it for a
/// recovery suggestion. This is a best-effort observation, not a race-free
/// no-follow guarantee. Missing paths are normal on a fresh install and are
/// reported as `false`.
fn doctor_session_directory_is_safe(path: &Path, label: &str) -> std::result::Result<bool, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "could not inspect {label} at {}: {error}",
                crate::utils::display_path(path)
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "could not inspect {label} at {}: path is a symlink",
            crate::utils::display_path(path)
        ));
    }
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "could not inspect {label} at {}: path is not a directory",
            crate::utils::display_path(path)
        ));
    }
    Ok(true)
}

/// Keep the report bounded while preserving a deterministic, lexical sample.
/// `read_dir` order is platform- and filesystem-dependent, so retaining the
/// first entries encountered would make the JSON and human receipts drift.
fn record_doctor_recoverable_session(
    recoverable: &mut Vec<DoctorRecoverableSessionEntry>,
    entry: DoctorRecoverableSessionEntry,
) {
    let insert_at = recoverable
        .binary_search_by(|existing| existing.name.cmp(&entry.name))
        .unwrap_or_else(|index| index);
    if recoverable.len() == DOCTOR_SESSION_RECOVERY_JSON_SAMPLE_LIMIT
        && insert_at == recoverable.len()
    {
        return;
    }
    recoverable.insert(insert_at, entry);
    if recoverable.len() > DOCTOR_SESSION_RECOVERY_JSON_SAMPLE_LIMIT {
        recoverable.pop();
    }
}

fn legacy_state_needs_attention(entry: &DoctorLegacyStateEntry) -> bool {
    entry.name != "sessions"
        && matches!(
            entry.status,
            DoctorLegacyStateStatus::LegacyOnly | DoctorLegacyStateStatus::Both
        )
}

fn print_doctor_legacy_state_report(
    report: &[DoctorLegacyStateEntry],
    session_recovery: &DoctorSessionRecoveryReport,
    ok_rgb: (u8, u8, u8),
    warn_rgb: (u8, u8, u8),
) {
    use colored::Colorize;

    let attention: Vec<_> = report
        .iter()
        .filter(|entry| legacy_state_needs_attention(entry))
        .collect();
    if attention.is_empty()
        && !session_recovery.needs_attention()
        && session_recovery.status != DoctorSessionRecoveryStatus::Isolated
    {
        println!(
            "  {} legacy state: no known .deepseek entries need migration",
            "✓".truecolor(ok_rgb.0, ok_rgb.1, ok_rgb.2)
        );
    } else if !attention.is_empty() {
        println!(
            "  {} legacy state needs review:",
            "!".truecolor(warn_rgb.0, warn_rgb.1, warn_rgb.2)
        );
        for entry in attention {
            match entry.status {
                DoctorLegacyStateStatus::LegacyOnly => {
                    println!(
                        "    {} {} exists but {} is missing",
                        "!".truecolor(warn_rgb.0, warn_rgb.1, warn_rgb.2),
                        crate::utils::display_path(&entry.legacy_path),
                        crate::utils::display_path(&entry.primary_path),
                    );
                }
                DoctorLegacyStateStatus::Both => {
                    println!(
                        "    {} {} exists alongside primary {}; legacy data may still need review",
                        "!".truecolor(warn_rgb.0, warn_rgb.1, warn_rgb.2),
                        crate::utils::display_path(&entry.legacy_path),
                        crate::utils::display_path(&entry.primary_path),
                    );
                }
                DoctorLegacyStateStatus::PrimaryOnly | DoctorLegacyStateStatus::Absent => {}
            }
        }
        println!(
            "    Start Codewhale once to trigger safe migration where available, then rerun `codewhale doctor`."
        );
    }

    print_doctor_session_recovery_report(session_recovery, ok_rgb, warn_rgb);
}

fn print_doctor_session_recovery_report(
    report: &DoctorSessionRecoveryReport,
    ok_rgb: (u8, u8, u8),
    warn_rgb: (u8, u8, u8),
) {
    use colored::Colorize;

    match report.status {
        DoctorSessionRecoveryStatus::Isolated => {
            println!(
                "  {} legacy sessions: ambient ~/.deepseek/sessions was not inspected because CODEWHALE_HOME is set",
                "·".dimmed()
            );
            println!(
                "    This preserves the explicit home boundary. To inspect the default home, use a separate shell with CODEWHALE_HOME unset and rerun `codewhale doctor`."
            );
        }
        DoctorSessionRecoveryStatus::NoLegacySessions => {
            println!(
                "  {} legacy sessions: no top-level session JSON files found",
                "✓".truecolor(ok_rgb.0, ok_rgb.1, ok_rgb.2)
            );
        }
        DoctorSessionRecoveryStatus::MigrationComplete => {
            println!(
                "  {} legacy sessions: all {} filename(s) have regular-file counterparts under {}; descriptor contents were not compared and legacy originals remain preserved",
                "✓".truecolor(ok_rgb.0, ok_rgb.1, ok_rgb.2),
                report.legacy_session_file_count,
                crate::utils::display_path(&report.primary_sessions_path),
            );
        }
        DoctorSessionRecoveryStatus::MigrationPending
        | DoctorSessionRecoveryStatus::MigrationIncomplete => {
            let label = if report.status == DoctorSessionRecoveryStatus::MigrationIncomplete {
                "migration is incomplete"
            } else {
                "migration has not completed"
            };
            println!(
                "  {} legacy sessions: {label}; {} recoverable file(s) are absent from {}",
                "!".truecolor(warn_rgb.0, warn_rgb.1, warn_rgb.2),
                report.recoverable_file_count,
                crate::utils::display_path(&report.primary_sessions_path),
            );
            for entry in report
                .recoverable
                .iter()
                .take(DOCTOR_SESSION_RECOVERY_HUMAN_SAMPLE_LIMIT)
            {
                println!(
                    "    {} {} -> {}",
                    "·".dimmed(),
                    crate::utils::display_path(&entry.source_path),
                    crate::utils::display_path(&entry.destination_path),
                );
            }
            if report.recoverable_file_count > DOCTOR_SESSION_RECOVERY_HUMAN_SAMPLE_LIMIT {
                println!(
                    "    · {} more filename(s); `codewhale doctor --json` includes a bounded metadata-only sample",
                    report.recoverable_file_count - DOCTOR_SESSION_RECOVERY_HUMAN_SAMPLE_LIMIT
                );
            }
            println!("    Safe recovery:");
            println!(
                "      1. Back up {} and {} (if present).",
                crate::utils::display_path(&report.legacy_sessions_path),
                crate::utils::display_path(&report.primary_sessions_path),
            );
            println!(
                "      2. Close other Codewhale processes, then run `codewhale sessions`; migration adds only missing files, never overwrites primary files, and leaves legacy originals in place."
            );
            println!(
                "      3. Rerun `codewhale doctor`. If filenames remain, keep both backups and report only the listed source/destination names."
            );
        }
        DoctorSessionRecoveryStatus::ScanFailed => {
            println!(
                "  {} legacy sessions: recovery diagnostic could not complete",
                "!".truecolor(warn_rgb.0, warn_rgb.1, warn_rgb.2)
            );
            if let Some(error) = report.error.as_deref() {
                println!("    {error}");
            }
            println!(
                "    Keep both session directories unchanged, back them up, fix path permissions or shape, and rerun `codewhale doctor` before attempting migration."
            );
        }
    }
    if report.status != DoctorSessionRecoveryStatus::Isolated {
        println!(
            "    Doctor inspected filenames and filesystem metadata only; it did not read chat contents, traverse checkpoints, or modify session files."
        );
    }
}

fn doctor_session_recovery_json(report: &DoctorSessionRecoveryReport) -> serde_json::Value {
    use serde_json::json;

    let recoverable: Vec<_> = report
        .recoverable
        .iter()
        .take(DOCTOR_SESSION_RECOVERY_JSON_SAMPLE_LIMIT)
        .map(|entry| {
            json!({
                "name": entry.name.display().to_string(),
                "source_path": entry.source_path.display().to_string(),
                "destination_path": entry.destination_path.display().to_string(),
            })
        })
        .collect();

    json!({
        "status": report.status.as_str(),
        "needs_attention": report.needs_attention(),
        "read_only": true,
        "chat_contents_read": false,
        "checkpoint_internals_scanned": false,
        "session_descriptors_compared": false,
        "counterpart_check": "top_level_filename_and_regular_file_only",
        "codewhale_home_is_explicit": report.codewhale_home_is_explicit,
        "legacy_sessions_path": report.legacy_sessions_path.display().to_string(),
        "primary_sessions_path": report.primary_sessions_path.display().to_string(),
        "legacy_session_file_count": report.legacy_session_file_count,
        "already_present_file_count": report.already_present_file_count,
        "recoverable_file_count": report.recoverable_file_count,
        "recoverable_files": recoverable,
        "recoverable_files_truncated": report.recoverable_file_count > report.recoverable.len(),
        "error": report.error,
        "recovery_command": if report.needs_attention() && report.status != DoctorSessionRecoveryStatus::ScanFailed {
            Some("codewhale sessions")
        } else {
            None
        },
    })
}

fn doctor_legacy_state_json(
    primary_root: &Path,
    legacy_root: &Path,
    report: &[DoctorLegacyStateEntry],
    session_recovery: &DoctorSessionRecoveryReport,
) -> serde_json::Value {
    use serde_json::json;

    let legacy_only = report
        .iter()
        .filter(|entry| entry.status == DoctorLegacyStateStatus::LegacyOnly)
        .count();
    let both = report
        .iter()
        .filter(|entry| entry.status == DoctorLegacyStateStatus::Both)
        .count();
    let entries: Vec<_> = report
        .iter()
        .map(|entry| {
            json!({
                "name": entry.name,
                "primary_path": entry.primary_path.display().to_string(),
                "legacy_path": entry.legacy_path.display().to_string(),
                "primary_present": entry.primary_present,
                "legacy_present": entry.legacy_present,
                "status": entry.status.as_str(),
            })
        })
        .collect();

    json!({
        "primary_root": primary_root.display().to_string(),
        "legacy_root": legacy_root.display().to_string(),
        "needs_attention": report.iter().any(legacy_state_needs_attention) || session_recovery.needs_attention(),
        "legacy_only_count": legacy_only,
        "dual_present_count": both,
        "session_recovery": doctor_session_recovery_json(session_recovery),
        "entries": entries,
    })
}

fn doctor_setup_state(
    config: &Config,
    workspace: &Path,
) -> (codewhale_config::SetupState, &'static str) {
    if let Ok(Some(state)) = codewhale_config::SetupState::load() {
        return (state, "persisted");
    }

    (
        codewhale_config::SetupState::derive_inherited(&doctor_inherited_setup_facts(
            config, workspace,
        )),
        "derived",
    )
}

fn doctor_inherited_setup_facts(
    config: &Config,
    workspace: &Path,
) -> codewhale_config::InheritedConfigFacts {
    let user_constitution = codewhale_config::UserConstitution::load().ok();
    let user_constitution_validity = user_constitution.as_ref().map_or(
        codewhale_config::ConstitutionValidity::Unknown,
        codewhale_config::UserConstitutionLoad::validity,
    );
    let has_user_constitution = user_constitution
        .as_ref()
        .is_some_and(|loaded| !matches!(loaded, codewhale_config::UserConstitutionLoad::Missing));
    let has_expert_override = codewhale_config::codewhale_home()
        .ok()
        .map(|home| home.join(Path::new(crate::prompts::CONSTITUTION_OVERRIDE_FILE)))
        .is_some_and(|path| path.exists());

    codewhale_config::InheritedConfigFacts {
        language: None,
        has_provider_route: !config.default_model().trim().is_empty(),
        has_credentials_or_local_runtime: doctor_has_credentials_or_local_runtime(config),
        trust_chosen: !crate::tui::onboarding::needs_trust(workspace),
        has_expert_override,
        has_user_constitution,
        user_constitution_validity,
    }
}

fn doctor_has_credentials_or_local_runtime(config: &Config) -> bool {
    resolve_credential_diagnostic(config)
        .availability
        .certifies_ready()
}

fn print_doctor_setup_report(
    config: &Config,
    workspace: &Path,
    state: &codewhale_config::SetupState,
    source: &str,
    ok_rgb: (u8, u8, u8),
    warn_rgb: (u8, u8, u8),
) {
    use colored::Colorize;

    let credential = resolve_credential_diagnostic(config);
    // Setup completion is persisted independently from credential probing.
    // Ordinary doctor deliberately does not read environment values or the
    // durable secret store, so `not_probed` must not erase a completed lane.
    let first_run_ready = state.first_run_ready();
    let update_ready = state.update_ready(crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION);
    let operate_ready = state.operate_ready();
    let first_run_icon = if first_run_ready {
        "✓".truecolor(ok_rgb.0, ok_rgb.1, ok_rgb.2)
    } else {
        "!".truecolor(warn_rgb.0, warn_rgb.1, warn_rgb.2)
    };
    let update_icon = if update_ready {
        "✓".truecolor(ok_rgb.0, ok_rgb.1, ok_rgb.2)
    } else {
        "!".truecolor(warn_rgb.0, warn_rgb.1, warn_rgb.2)
    };
    let operate_icon = if operate_ready {
        "✓".truecolor(ok_rgb.0, ok_rgb.1, ok_rgb.2)
    } else {
        "!".truecolor(warn_rgb.0, warn_rgb.1, warn_rgb.2)
    };

    println!();
    println!("{}", "Setup State:".bold());
    println!("  · source: {source}");
    println!(
        "  · credential: source={}, availability={}",
        doctor_api_key_source_label(credential.source),
        credential.availability.label()
    );
    println!(
        "  {first_run_icon} first-run: {}",
        doctor_ready_label(first_run_ready)
    );
    // An update checkpoint only means something once a prior setup exists;
    // on a fresh home it is a stale version number with nothing to update.
    if first_run_ready {
        println!(
            "  {update_icon} update checkpoint {}: {}",
            crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
            doctor_ready_label(update_ready)
        );
    }
    println!(
        "  {operate_icon} operate/fleet: {}",
        doctor_ready_label(operate_ready)
    );
    println!(
        "  · constitution autonomy: {} (guidance only)",
        doctor_constitution_autonomy_preference_id()
    );
    println!(
        "  · runtime posture: {}",
        doctor_runtime_posture_line(config, workspace)
    );
    println!(
        "  · control socket: {}",
        doctor_control_socket_posture_line(config)
    );
    let consistency = doctor_setup_consistency(state, source);
    if consistency["status"] == "inconsistent" {
        let issues = consistency["issues"]
            .as_array()
            .map(|issues| {
                issues
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        println!(
            "  {} consistency: half-applied setup detected ({issues}) — {}",
            "!".truecolor(warn_rgb.0, warn_rgb.1, warn_rgb.2),
            consistency["repair"].as_str().unwrap_or("/setup"),
        );
    }
    println!(
        "  · next actions: /constitution (standing law), /setup report (readiness), /setup provider or /provider setup <name> (provider credentials), /model (route), /config (runtime posture), /setup fleet (Operate/Fleet readiness), /fleet setup (explicit profile authoring), /setup hotbar (optional shortcuts), /setup tools (Tools/MCP readiness), /setup remote (remote runtime on-ramp), /setup persistence (path review)"
    );
    for step in codewhale_config::SetupStep::ALL {
        let entry = state.steps.get(&step);
        let required = entry.is_some_and(|entry| entry.required);
        let version = entry.and_then(|entry| entry.version.as_deref());
        let result = entry.and_then(|entry| entry.result.as_deref());
        let required_label = if required { "required" } else { "optional" };
        let version_label = version.unwrap_or("unversioned");
        let result_label = result.unwrap_or("no result");
        println!(
            "    · {}: {} ({required_label}, {version_label}, {result_label})",
            setup_step_id(step),
            setup_status_id(state.status(step))
        );
    }
}

/// #5098: print every profile id that exists in more than one roster layer
/// so a personal/config edit that loses to project is visible without
/// opening `/fleet`.
fn print_doctor_fleet_roster_layers(config: &Config, workspace: &Path) {
    use colored::Colorize;

    let roster =
        crate::fleet::identity::load_effective_roster(&config.fleet_config(), workspace, None);
    println!();
    println!("{}", "Fleet roster layers:".bold());
    if let Some(error) = roster.load_error() {
        println!("  ! {error}");
        return;
    }
    let lines = roster.doctor_layer_lines();
    if lines.is_empty() {
        println!("  · no profile id is defined in more than one layer");
        return;
    }
    for line in lines {
        if let Some(layer) = line.strip_prefix("  ") {
            println!("      {layer}");
        } else {
            println!("  · {line}");
        }
    }
}

fn doctor_ready_label(ready: bool) -> &'static str {
    if ready { "ready" } else { "needs action" }
}

/// Detect half-applied setup persistence (#3410).
///
/// The setup transaction writes `constitution.json` and `setup_state.json`
/// together, so a persisted state that points at a user-global constitution
/// which is missing or unusable on disk means a write was interrupted or a
/// file was removed out-of-band. Stale `.tmp*` files in `$CODEWHALE_HOME`
/// are the other fingerprint of an interrupted atomic write.
fn doctor_setup_consistency(
    state: &codewhale_config::SetupState,
    source: &str,
) -> serde_json::Value {
    use serde_json::json;

    let mut issues: Vec<&'static str> = Vec::new();

    if source == "persisted"
        && matches!(
            state.constitution_source,
            codewhale_config::ConstitutionSource::UserGlobal
        )
    {
        match codewhale_config::UserConstitution::load() {
            Ok(codewhale_config::UserConstitutionLoad::Missing) => {
                issues.push("setup_state_points_at_missing_user_constitution");
            }
            Ok(codewhale_config::UserConstitutionLoad::Empty) => {
                issues.push("user_constitution_empty");
            }
            Ok(codewhale_config::UserConstitutionLoad::Invalid(_)) => {
                issues.push("user_constitution_invalid");
            }
            Ok(codewhale_config::UserConstitutionLoad::Unreadable(_)) | Err(_) => {
                issues.push("user_constitution_unreadable");
            }
            Ok(codewhale_config::UserConstitutionLoad::Loaded(_)) => {}
        }
    }

    if doctor_home_has_stale_setup_temp_files() {
        issues.push("stale_setup_temp_files_in_codewhale_home");
    }

    json!({
        "status": if issues.is_empty() { "consistent" } else { "inconsistent" },
        "issues": issues,
        "repair": "/constitution to rebuild standing law, /setup to re-run the checkpoint",
    })
}

fn doctor_home_has_stale_setup_temp_files() -> bool {
    let Ok(home) = codewhale_config::codewhale_home() else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(&home) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_name().to_string_lossy().starts_with(".tmp")
            && entry.file_type().is_ok_and(|kind| kind.is_file())
    })
}

fn doctor_constitution_autonomy_preference() -> codewhale_config::AutonomyPreference {
    codewhale_config::UserConstitution::load()
        .ok()
        .and_then(|load| {
            load.constitution()
                .map(|constitution| constitution.autonomy_preference)
        })
        .unwrap_or(codewhale_config::AutonomyPreference::Unspecified)
}

fn doctor_constitution_autonomy_preference_id() -> &'static str {
    autonomy_preference_id(doctor_constitution_autonomy_preference())
}

fn autonomy_preference_id(preference: codewhale_config::AutonomyPreference) -> &'static str {
    match preference {
        codewhale_config::AutonomyPreference::Unspecified => "unspecified",
        codewhale_config::AutonomyPreference::Cautious => "cautious",
        codewhale_config::AutonomyPreference::Balanced => "balanced",
        codewhale_config::AutonomyPreference::Autonomous => "autonomous",
    }
}

fn doctor_runtime_default_mode() -> (String, &'static str) {
    match crate::settings::Settings::load_read_only() {
        Ok(settings) => (settings.default_mode, "settings"),
        Err(_) => (crate::settings::Settings::default().default_mode, "default"),
    }
}

/// TUI settings posture used when `config.approval_policy` is unset.
/// Doctor must surface this separately so a saved Full Access baseline is not
/// misreported as the config default `approval_policy=on-request`.
fn doctor_runtime_permission_posture() -> (String, &'static str) {
    match crate::settings::Settings::load_read_only() {
        Ok(settings) => match settings.permission_posture {
            Some(posture) => (posture, "settings"),
            None => ("unset".to_string(), "default"),
        },
        Err(_) => ("unset".to_string(), "default"),
    }
}

fn doctor_runtime_posture_line(config: &Config, workspace: &Path) -> String {
    let (default_mode, default_mode_source) = doctor_runtime_default_mode();
    let (permission_posture, permission_posture_source) = doctor_runtime_permission_posture();
    let approval = config.approval_policy.as_deref().unwrap_or("on-request");
    let approval_source = if config.approval_policy.is_some() {
        "config"
    } else {
        "default"
    };
    let allow_shell = config.interactive_allow_shell();
    let allow_shell_source = if config.allow_shell.is_some() {
        "config"
    } else {
        "interactive default"
    };
    let sandbox = config.sandbox_mode.as_deref().unwrap_or("mode-derived");
    let sandbox_source = if config.sandbox_mode.is_some() {
        "config"
    } else {
        "default"
    };
    let network = config
        .network
        .as_ref()
        .map_or("prompt", |policy| policy.default.as_str());
    let network_source = if config.network.is_some() {
        "config"
    } else {
        "default"
    };
    let trust = if crate::tui::onboarding::needs_trust(workspace) {
        "workspace not elevated"
    } else {
        "workspace trusted"
    };
    let (telemetry_on, telemetry_source) = doctor_runtime_telemetry(config);
    let telemetry = if telemetry_on { "on" } else { "off" };

    format!(
        "default_mode={default_mode} ({default_mode_source}), permission_posture={permission_posture} ({permission_posture_source}), approval_policy={approval} ({approval_source}), allow_shell={allow_shell} ({allow_shell_source}), sandbox={sandbox} ({sandbox_source}), network.default={network} ({network_source}), telemetry={telemetry} ({telemetry_source}), trust={trust}"
    )
}

/// Doctor posture for the per-session control socket, enabled via
/// `[control_socket].enabled` (false = off, the default). Report the
/// resolved state and, when enabled, where the socket appears for the
/// running session.
fn doctor_control_socket_posture_line(config: &Config) -> String {
    let enabled = config
        .control_socket
        .as_ref()
        .is_some_and(|socket| socket.enabled);
    if enabled {
        "control_socket=on (sessions/<id>/control.sock per running session)".to_string()
    } else {
        "control_socket=off (default)".to_string()
    }
}

/// Resolved telemetry consent and where it came from (#5441).
///
/// Report the durable opt-out and environment preference without arming or
/// touching telemetry state. Notice presentation is not a permission gate.
fn doctor_runtime_telemetry(config: &Config) -> (bool, &'static str) {
    let (on, source) = codewhale_config::resolved_telemetry_consent(config.telemetry);
    if !on {
        return (false, source.as_str());
    }
    match codewhale_telemetry::load_setup_state_for_decision() {
        Some(state) if state.telemetry_opted_out() => (false, "recorded_opt_out"),
        Some(_) => (true, source.as_str()),
        None => (false, "privacy_state_unreadable"),
    }
}

fn doctor_operate_fleet_report_json(config: &Config, workspace: &Path) -> serde_json::Value {
    use serde_json::json;

    let identity = match config.active_provider_identity() {
        Ok(identity) => identity,
        Err(_) => {
            return json!({"provider": config.provider, "ready": false, "route_error": "provider_identity_unavailable", "auth": {"availability": "unavailable", "source": "unknown"}});
        }
    };
    // Doctor reports configured routing posture only. In particular it must
    // never consume an external-file grant merely to label Fleet readiness.
    let credential = resolve_credential_diagnostic(config);
    let has_credentials_or_local = credential.availability.certifies_ready();
    let subagents_enabled = config.subagents_enabled_for_provider(&identity);
    let disabled_reason = if subagents_enabled {
        None
    } else {
        Some(
            config
                .subagents_disabled_reason()
                .unwrap_or("disabled for active provider"),
        )
    };
    let max_subagents = config.max_subagents_for_provider(&identity);
    let launch_concurrency = config.launch_concurrency_for_provider(&identity);
    let max_admitted = config.max_admitted_subagents_for_provider(&identity);
    let max_spawn_depth = config.subagent_max_spawn_depth_for_provider(&identity);
    let roster =
        crate::fleet::identity::load_effective_roster(&config.fleet_config(), workspace, None);
    let mut built_in_members = 0usize;
    let mut plugin_members = 0usize;
    let mut config_members = 0usize;
    let mut personal_members = 0usize;
    let mut workspace_members = 0usize;
    let mut claude_members = 0usize;
    for member in roster.members() {
        match member.origin {
            crate::fleet::roster::ProfileOrigin::BuiltIn => built_in_members += 1,
            crate::fleet::roster::ProfileOrigin::Plugin => plugin_members += 1,
            crate::fleet::roster::ProfileOrigin::Config => config_members += 1,
            crate::fleet::roster::ProfileOrigin::Personal => personal_members += 1,
            crate::fleet::roster::ProfileOrigin::Workspace => workspace_members += 1,
            crate::fleet::roster::ProfileOrigin::ClaudeCode => claude_members += 1,
        }
    }
    let roster_members = roster.members().len();
    let custom_members =
        plugin_members + config_members + personal_members + workspace_members + claude_members;
    let roster_ready = roster.load_error().is_none() && roster_members > 0;
    let runtime_ready =
        subagents_enabled && max_subagents > 0 && launch_concurrency > 0 && max_spawn_depth > 0;
    let multi_layer: Vec<serde_json::Value> = roster
        .multi_layer_report()
        .into_iter()
        .map(|entry| {
            json!({
                "id": entry.id,
                "effective": entry.effective.to_string(),
                "effective_path": entry.effective_path.display().to_string(),
                "layers": entry
                    .layers
                    .iter()
                    .map(|layer| {
                        json!({
                            "origin": layer.origin.to_string(),
                            "path": layer.source.display().to_string(),
                            "wins": layer.wins,
                        })
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    let model_pin_drift = doctor_model_pin_drift(config, workspace, &roster);

    json!({
        "ready": has_credentials_or_local && runtime_ready && roster_ready,
        "model_pin_drift": model_pin_drift,
        "provider": {
            "id": identity.key.as_str(),
            "auth": {
                "present_or_local": has_credentials_or_local,
                "source": doctor_api_key_source_label(credential.source),
                "availability": credential.availability.label(),
            },
        },
        "worker_runtime": {
            "ready": runtime_ready,
            "enabled": subagents_enabled,
            "disabled_reason": disabled_reason,
            "max_subagents": max_subagents,
            "launch_concurrency": launch_concurrency,
            "max_admitted": max_admitted,
            "max_spawn_depth": max_spawn_depth,
            "host_enforced_workflow_receipts": true,
        },
        "roster": {
            "ready": roster_ready,
            "error": roster.load_error(),
            "total": roster_members,
            "built_in": built_in_members,
            "config": config_members,
            "personal": personal_members,
            "workspace": workspace_members,
            "claude": claude_members,
            "custom": custom_members,
            "starter_roster_available": built_in_members > 0,
            "readiness_rule": "built-in starter roster or custom roster",
            "multi_layer": multi_layer,
        },
        "concurrency": {
            "launch_concurrency": launch_concurrency,
            "max_subagents": max_subagents,
            "max_admitted": max_admitted,
            "plan_limit_probed": false,
        },
    })
}

/// Warning-only model-pin drift surfacing (#6035). A pin is flagged only
/// when a FRESH cached live roster for that provider route exists and does
/// not list the pinned wire id — stale, failed, or absent rosters cannot
/// prove drift, and bundled catalog rows say nothing about what the account
/// currently serves. The id may still answer (soft deprecation) or be served
/// by other providers on their own routes, so this never rewrites the pin.
fn doctor_model_pin_drift(
    config: &Config,
    workspace: &Path,
    roster: &crate::fleet::roster::FleetRoster,
) -> serde_json::Value {
    use serde_json::json;
    use std::collections::BTreeMap;

    // One row per affected route: (provider, model) -> pin owners.
    let mut pins: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();

    for entry in crate::fleet::store::list_fleets(workspace) {
        if entry.parse_error.is_some() {
            continue;
        }
        let Ok((fleet, scope)) = crate::fleet::store::load_fleet_at(&entry.path) else {
            continue;
        };
        let owner = format!("fleet:{} ({})", fleet.name, scope.label());
        if let Some(operator) = fleet.operator.as_ref() {
            pins.entry((operator.provider.clone(), operator.model.clone()))
                .or_default()
                .push(format!("{owner} operator"));
        }
        for member in &fleet.members {
            if let (Some(provider), Some(model)) = (member.provider.as_ref(), member.model.as_ref())
            {
                pins.entry((provider.clone(), model.clone()))
                    .or_default()
                    .push(format!("{owner} member:{}", member.id));
            }
        }
    }
    for member in roster.members() {
        if let (Some(provider), Some(model)) = (
            member.profile.provider.as_ref(),
            member.profile.model.as_ref(),
        ) {
            pins.entry((provider.clone(), model.clone()))
                .or_default()
                .push(format!("agent:{}", member.id));
        }
    }

    let mut unverifiable = 0usize;
    let drifted = pins
        .iter()
        .filter_map(|((provider, model), owners)| {
            let Some(missing) = crate::provider_catalog_live::pin_missing_from_fresh_roster(
                config, provider, model,
            ) else {
                unverifiable += 1;
                return None;
            };
            missing.then(|| {
                json!({
                    "provider": provider,
                    "model": model,
                    "owners": owners,
                    "message": format!(
                        "pinned id `{model}` is absent from {provider}'s current live roster; \
                         the id may still answer (soft deprecation) or be served by other \
                         providers on their own routes — the pin is left unchanged"
                    ),
                })
            })
        })
        .collect::<Vec<_>>();

    json!({
        "checked": pins.len(),
        "unverifiable": unverifiable,
        "drifted": drifted,
    })
}

fn doctor_provider_model_report_json(config: &Config) -> serde_json::Value {
    use serde_json::json;

    let identity = match config.active_provider_identity() {
        Ok(identity) => identity,
        Err(_) => {
            return json!({"provider": config.provider, "ready": false, "route_error": "provider_identity_unavailable", "auth": {"availability": "unavailable", "source": "unknown"}});
        }
    };
    let provider = identity.provider;
    let credential = resolve_credential_diagnostic(config);
    let auth_present_or_local = credential.availability.certifies_ready();
    let credential_help = provider.provider().credential_help();
    let credential_url = credential_help
        .credential_url
        .map(crate::doctor::structural_url_authority);
    let credential_docs_url = credential_help
        .docs_url
        .map(crate::doctor::structural_url_authority);

    json!({
        "provider": {
            "id": identity.key.as_str(),
            "display": identity.compatibility().map_or(identity.key.as_str(), |row| row.label),
        },
        "model": {
            "resolved": config.default_model(),
        },
        "auth": {
            "present_or_local": auth_present_or_local,
            "source": doctor_api_key_source_label(credential.source),
            "availability": credential.availability.label(),
            "env_vars": provider.provider().env_vars(),
            "credential_mode": credential_help.acquisition.as_str(),
            "credential_url": credential_url,
            "credential_docs_url": credential_docs_url,
            "credential_guidance": credential_help.guidance,
            "oauth_only": credential_help.acquisition
                == codewhale_config::provider::CredentialAcquisition::OAuth,
        },
        "health": {
            "live_validation": false,
            "next_action": if auth_present_or_local {
                "/model"
            } else {
                "/setup provider or /provider setup <name>"
            },
        },
    })
}

fn doctor_dsh_integration_report(
    config: &Config,
    workspace: &Path,
) -> anyhow::Result<crate::integrations::dsh::DshStatusReport> {
    use crate::integrations::dsh;
    let paths = dsh::DshPaths::from_process()?;
    let detection = dsh::detect::detect(&dsh::DetectEnv::from_process(), &dsh::ProcessRunner);
    let identity = dsh::codewhale_route_identity(config, workspace);
    dsh::compute_status(
        &paths,
        detection,
        identity,
        false,
        dsh::bundle_availability_now(),
    )
}

fn doctor_dsh_integration_lines(config: &Config, workspace: &Path) -> Vec<String> {
    match doctor_dsh_integration_report(config, workspace) {
        Ok(report) => {
            let mut lines = vec![
                format!("state: {}", report.state.label()),
                crate::integrations::dsh::status_line(&report),
                format!(
                    "owned files: {} (overlay {})",
                    crate::utils::display_path(&report.paths_root),
                    if report.overlay_present {
                        "present"
                    } else {
                        "absent"
                    }
                ),
            ];
            if !report.shadowing_namespaces.is_empty() {
                lines.push(format!(
                    "dsh settings.yaml sections that can shadow the overlay: {}",
                    report.shadowing_namespaces.join(", ")
                ));
            }
            lines
        }
        Err(error) => vec![format!("unavailable: {error}")],
    }
}

fn doctor_dsh_integration_json(config: &Config, workspace: &Path) -> serde_json::Value {
    match doctor_dsh_integration_report(config, workspace) {
        Ok(report) => serde_json::json!({
            "state": report.state.label(),
            "summary": crate::integrations::dsh::status_line(&report),
            "dsh_version": report.detection.version,
            "compatibility": report.detection.compatibility.label(),
            "overlay_present": report.overlay_present,
            "shadowing_namespaces": report.shadowing_namespaces,
        }),
        Err(error) => serde_json::json!({ "state": "unavailable", "error": error.to_string() }),
    }
}

fn doctor_external_credential_consent_statuses(
    config: &Config,
) -> Vec<codewhale_config::ExternalCredentialConsentStatus> {
    [
        crate::config::ProviderKind::OpenaiCodex,
        crate::config::ProviderKind::Xai,
        crate::config::ProviderKind::Deepseek,
    ]
    .into_iter()
    .filter_map(|provider| {
        let identity = config.builtin_provider_identity(provider).ok()?;
        config.external_credential_consent_status(&identity)
    })
    .collect()
}

fn doctor_external_credential_consent_lines(config: &Config) -> Vec<String> {
    doctor_external_credential_consent_statuses(config)
        .into_iter()
        .flat_map(|status| {
            let mut lines = vec![
                format!(
                    "{}: access={}, provider={}, source={}, owner={}, path={}, version={}, state={}, ambient_path_changed={}",
                    status.provider,
                    status.access.as_str(),
                    status.provider,
                    status.source.as_str(),
                    status.owner,
                    codewhale_config::quote_os_path(&status.path),
                    status.consent_version,
                    status.route_state,
                    status.ambient_path_changed,
                ),
                format!("  semantics: {}", status.semantics),
                format!("  revoke: {}", status.revoke_command),
            ];
            if let Some(warning) = status.ambient_path_warning() {
                lines.push(format!("  {warning}"));
            }
            lines
        })
        .collect()
}

fn doctor_external_credential_consent_json(config: &Config) -> serde_json::Value {
    serde_json::Value::Array(
        doctor_external_credential_consent_statuses(config)
            .into_iter()
            .map(|status| {
                serde_json::json!({
                    "provider": status.provider,
                    "access": status.access.as_str(),
                    "source": status.source.as_str(),
                    "owner": status.owner,
                    "path": codewhale_config::quote_os_path(&status.path),
                    "consent_version": status.consent_version,
                    "scope_valid": status.scope_valid,
                    "ambient_path_changed": status.ambient_path_changed,
                    "ambient_path_warning": status.ambient_path_warning(),
                    "route_state": status.route_state,
                    "semantics": status.semantics,
                    "revoke_command": status.revoke_command,
                })
            })
            .collect(),
    )
}

fn doctor_setup_report_json(config: &Config, workspace: &Path) -> serde_json::Value {
    use serde_json::json;

    let (state, source) = doctor_setup_state(config, workspace);
    let (default_mode, default_mode_source) = doctor_runtime_default_mode();
    let (permission_posture, permission_posture_source) = doctor_runtime_permission_posture();
    let approval_policy = config.approval_policy.as_deref().unwrap_or("on-request");
    let approval_policy_source = if config.approval_policy.is_some() {
        "config"
    } else {
        "default"
    };
    let allow_shell = config.interactive_allow_shell();
    let allow_shell_source = if config.allow_shell.is_some() {
        "config"
    } else {
        "interactive_default"
    };
    let sandbox_mode = config.sandbox_mode.as_deref().unwrap_or("mode-derived");
    let sandbox_mode_source = if config.sandbox_mode.is_some() {
        "config"
    } else {
        "default"
    };
    let network_default = config
        .network
        .as_ref()
        .map_or("prompt", |policy| policy.default.as_str());
    let network_source = if config.network.is_some() {
        "config"
    } else {
        "default"
    };
    let (telemetry_value, telemetry_source) = doctor_runtime_telemetry(config);
    let workspace_trusted = !crate::tui::onboarding::needs_trust(workspace);
    let credential = resolve_credential_diagnostic(config);
    let credential_ready = credential.availability.certifies_ready();
    let steps: Vec<_> = codewhale_config::SetupStep::ALL
        .into_iter()
        .map(|step| {
            let entry = state.steps.get(&step);
            json!({
                "step": setup_step_id(step),
                "status": setup_status_id(state.status(step)),
                "required": entry.is_some_and(|entry| entry.required),
                "version": entry.and_then(|entry| entry.version.clone()),
                "result": entry.and_then(|entry| entry.result.clone()),
            })
        })
        .collect();

    json!({
        "source": source,
        "schema_version": state.schema_version,
        "inherited": state.inherited,
        "checkpoint_version": crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
        "first_run_ready": state.first_run_ready(),
        "update_ready": state.update_ready(crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION),
        "operate_ready": state.operate_ready(),
        "credential": {
            "ready": credential_ready,
            "source": doctor_api_key_source_label(credential.source),
            "availability": credential.availability.label(),
        },
        "constitution": {
            "choice": constitution_choice_id(state.constitution_choice),
            "source": constitution_source_id(state.constitution_source),
            "validity": constitution_validity_id(state.constitution_validity),
            "checkpoint_completed_for": state.constitution_checkpoint_completed_for.clone(),
            "language": state.constitution_language.clone(),
            "preview_hash_present": state.constitution_preview_hash.is_some(),
            "preview_version": state.constitution_preview_version,
            "autonomy_preference": doctor_constitution_autonomy_preference_id(),
        },
        "runtime_posture_source": runtime_posture_source_id(state.runtime_posture_source),
        "runtime_posture": {
            "source": runtime_posture_source_id(state.runtime_posture_source),
            "default_mode": {
                "value": default_mode,
                "source": default_mode_source,
            },
            "permission_posture": {
                "value": permission_posture,
                "source": permission_posture_source,
            },
            "approval_policy": {
                "value": approval_policy,
                "source": approval_policy_source,
            },
            "allow_shell": {
                "value": allow_shell,
                "source": allow_shell_source,
            },
            "sandbox_mode": {
                "value": sandbox_mode,
                "source": sandbox_mode_source,
            },
            "network_default": {
                "value": network_default,
                "source": network_source,
            },
            "telemetry": {
                "value": telemetry_value,
                "source": telemetry_source,
            },
            "workspace_trust": {
                "trusted": workspace_trusted,
                "source": "workspace",
            },
        },
        "provider_model": doctor_provider_model_report_json(config),
        "operate_fleet": doctor_operate_fleet_report_json(config, workspace),
        "consistency": doctor_setup_consistency(&state, source),
        "next_actions": {
            "constitution": "/constitution",
            "setup_report": "/setup report",
            "provider_model": "/setup provider, /provider setup <name>, or /model",
            "runtime_posture": "/config",
            "operate_fleet": "/setup fleet (readiness), /fleet setup (explicit profile authoring)",
            "hotbar": "/setup hotbar",
            "tools_mcp": "/setup tools",
            "remote_runtime": "/setup remote",
            "persistence": "/setup persistence",
        },
        "steps": steps,
    })
}

fn setup_step_id(step: codewhale_config::SetupStep) -> &'static str {
    match step {
        codewhale_config::SetupStep::Language => "language",
        codewhale_config::SetupStep::ProviderModel => "provider_model",
        codewhale_config::SetupStep::TrustSandbox => "trust_sandbox",
        codewhale_config::SetupStep::ToolsMcp => "tools_mcp",
        codewhale_config::SetupStep::Hotbar => "hotbar",
        codewhale_config::SetupStep::RemoteRuntime => "remote_runtime",
        codewhale_config::SetupStep::Persistence => "persistence",
        codewhale_config::SetupStep::Constitution => "constitution",
        codewhale_config::SetupStep::OperateFleet => "operate_fleet",
        codewhale_config::SetupStep::Verification => "verification",
    }
}

fn setup_status_id(status: codewhale_config::StepStatus) -> &'static str {
    match status {
        codewhale_config::StepStatus::NotStarted => "not_started",
        codewhale_config::StepStatus::Recommended => "recommended",
        codewhale_config::StepStatus::Optional => "optional",
        codewhale_config::StepStatus::Deferred => "deferred",
        codewhale_config::StepStatus::InProgress => "in_progress",
        codewhale_config::StepStatus::Configured => "configured",
        codewhale_config::StepStatus::Verified => "verified",
        codewhale_config::StepStatus::NeedsAction => "needs_action",
        codewhale_config::StepStatus::Failed => "failed",
        codewhale_config::StepStatus::Skipped => "skipped",
    }
}

fn constitution_choice_id(choice: codewhale_config::ConstitutionChoice) -> &'static str {
    match choice {
        codewhale_config::ConstitutionChoice::Unset => "unset",
        codewhale_config::ConstitutionChoice::Bundled => "bundled",
        codewhale_config::ConstitutionChoice::GuidedCustom => "guided_custom",
        codewhale_config::ConstitutionChoice::ExpertOverride => "expert_override",
        codewhale_config::ConstitutionChoice::Deferred => "deferred",
    }
}

fn constitution_source_id(source: codewhale_config::ConstitutionSource) -> &'static str {
    match source {
        codewhale_config::ConstitutionSource::Bundled => "bundled",
        codewhale_config::ConstitutionSource::UserGlobal => "user_global",
        codewhale_config::ConstitutionSource::ExpertOverride => "expert_override",
    }
}

fn constitution_validity_id(validity: codewhale_config::ConstitutionValidity) -> &'static str {
    match validity {
        codewhale_config::ConstitutionValidity::Unknown => "unknown",
        codewhale_config::ConstitutionValidity::Valid => "valid",
        codewhale_config::ConstitutionValidity::Invalid => "invalid",
        codewhale_config::ConstitutionValidity::Empty => "empty",
        codewhale_config::ConstitutionValidity::Unreadable => "unreadable",
    }
}

fn runtime_posture_source_id(source: codewhale_config::RuntimePostureSource) -> &'static str {
    match source {
        codewhale_config::RuntimePostureSource::Unset => "unset",
        codewhale_config::RuntimePostureSource::Inherited => "inherited",
        codewhale_config::RuntimePostureSource::Confirmed => "confirmed",
    }
}

/// Emit a bounded, secret-redacted JSON failure when configuration cannot be
/// loaded or validated. Invalid configuration must not be forced through the
/// normal doctor report because its route/capability facts would be misleading.
/// `codewhale doctor --repair-sessions [--dry-run]` (#6144).
fn run_doctor_repair_sessions(dry_run: bool) -> Result<()> {
    let manager = session_manager::SessionManager::default_location()?;
    let summary = crate::session_reconcile::reconcile(
        &manager,
        &crate::session_reconcile::ReconcileOptions {
            dry_run,
            ..Default::default()
        },
    )?;
    let report = serde_json::to_string_pretty(&summary)?;
    if summary.skipped_concurrent {
        println!("Another Codewhale process is repairing the session store; try again shortly.");
    } else if dry_run {
        println!("Session repair (dry run — nothing changed):");
    } else {
        println!("Session repair:");
    }
    println!("{report}");
    Ok(())
}

const DOCTOR_CONFIG_ERROR_OMITTED: &str = "configuration validation failed; details omitted because configuration errors may contain credential material";

/// Human doctor text for a config load failure. Plain value/profile
/// validation errors are shown with their fix; anything else (parse errors,
/// credential fields) stays suppressed because it may echo secret material.
fn doctor_config_error_text(error: &anyhow::Error) -> String {
    let Some(diagnostic) = crate::config::SafeConfigDiagnostic::find_in(error) else {
        return format!("doctor {DOCTOR_CONFIG_ERROR_OMITTED}");
    };
    let mut text = format!(
        "doctor configuration validation failed: {}",
        diagnostic.display_message()
    );
    if let Some(fix) = diagnostic.fix() {
        text.push_str("\nfix: ");
        text.push_str(fix);
    }
    text
}

fn run_doctor_json_config_error(error: &anyhow::Error) -> Result<()> {
    let diagnostic = crate::config::SafeConfigDiagnostic::find_in(error);
    let safe_message = diagnostic.map(crate::config::SafeConfigDiagnostic::display_message);
    let report = serde_json::json!({
        "status": "error",
        "error": {
            "kind": "config_validation",
            "message": safe_message.as_deref().unwrap_or(DOCTOR_CONFIG_ERROR_OMITTED),
            "fix": diagnostic.and_then(crate::config::SafeConfigDiagnostic::fix),
        },
    });
    println!("{}", serde_json::to_string_pretty(&report)?);

    // Keep stderr generic: the actionable, redacted error is already on
    // stdout, and Rust's Result termination must never redisclose a secret.
    bail!("doctor configuration validation failed; see JSON output")
}

/// Machine-readable counterpart to `run_doctor`. This report is always
/// structural and offline; live probe flags conflict with `--json`.
fn run_doctor_json(
    config: &Config,
    workspace: &Path,
    config_path_override: Option<&Path>,
    plugins: &crate::plugins::PluginRegistry,
) -> Result<()> {
    use serde_json::json;

    let doctor_paths = crate::doctor::DoctorPathReport::resolve(config_path_override)?;
    let config_path = &doctor_paths.config;
    let secret_backend = codewhale_secrets::diagnose_secret_backend();

    let credential = resolve_credential_diagnostic(config);

    let mcp_config_path = config.mcp_config_path();
    let project_mcp_config_path = crate::mcp::workspace_mcp_config_path(workspace);
    let mcp_present = mcp_config_path.exists();
    let project_mcp_present = project_mcp_config_path.exists();
    let mcp_summary = match crate::mcp::load_config_with_workspace_and_plugins(
        &mcp_config_path,
        workspace,
        plugins,
    ) {
        Ok(cfg) => {
            let servers: Vec<serde_json::Value> = cfg
                .servers
                .iter()
                .map(|(name, server)| doctor_mcp_server_json(name, server))
                .collect();
            json!({
                "config_path": mcp_config_path.display().to_string(),
                "present": mcp_present,
                "project_config_path": project_mcp_config_path.display().to_string(),
                "project_present": project_mcp_present,
                "probe_scope": "configuration",
                "live_health_checked": false,
                "servers": servers,
            })
        }
        Err(_) => json!({
            "config_path": mcp_config_path.display().to_string(),
            "present": mcp_present,
            "project_config_path": project_mcp_config_path.display().to_string(),
            "project_present": project_mcp_present,
            "probe_scope": "configuration",
            "live_health_checked": false,
            "servers": [],
            "error": "configuration_unavailable_details_omitted",
        }),
    };

    let global_skills_dir = config.skills_dir();
    let agents_skills_dir = workspace.join(".agents").join("skills");
    let local_skills_dir = workspace.join("skills");
    let agents_global_skills_dir = crate::skills::agents_global_skills_dir();
    // #432: cross-tool skill discovery dirs surface in the JSON
    // report so external dashboards can see whether any
    // `.opencode/skills/`, `.claude/skills/`, `.cursor/skills/`, or
    // global agentskills.io content is contributing to the merged catalogue.
    let opencode_skills_dir = workspace.join(".opencode").join("skills");
    let claude_skills_dir = workspace.join(".claude").join("skills");
    let selected_skills_dir = if agents_skills_dir.exists() {
        agents_skills_dir.clone()
    } else if local_skills_dir.exists() {
        local_skills_dir.clone()
    } else if config.skills_dir.is_none()
        && let Some(global_agents) = agents_global_skills_dir.as_ref()
        && global_agents.exists()
    {
        global_agents.clone()
    } else {
        global_skills_dir.clone()
    };
    let agents_global_summary = agents_global_skills_dir
        .as_ref()
        .map(|path| {
            json!({
                "path": path.display().to_string(),
                "present": path.exists(),
                "count": skills_count_for(path),
            })
        })
        .unwrap_or_else(|| {
            json!({
                "path": null,
                "present": false,
                "count": 0,
            })
        });

    let tools_dir = default_tools_dir();
    let plugins_dir = default_plugins_dir();

    // Memory feature state (#489). Operators ask "is memory on?" and
    // "where does it live?" — surface both here so the question can be
    // answered without booting the TUI. Both inputs are checked: the
    // config flag and the env-var override that the runtime would
    // honour. (The dedicated `Config::memory_enabled()` accessor lives
    // on the memory-MVP branch (#518); this duplicates the same logic
    // until the two PRs land and it can be replaced with a single
    // method call.)
    let memory_path = config.memory_path();
    let memory_enabled_env = std::env::var("CODEWHALE_MEMORY")
        .or_else(|_| std::env::var("DEEPSEEK_MEMORY"))
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "on" | "true" | "yes" | "y" | "enabled"
            )
        })
        .unwrap_or(false);
    let memory_summary = json!({
        // The MVP feature is opt-in by default; this defaults to false
        // on branches without the [memory] section in `Config`.
        "enabled": memory_enabled_env,
        "path": memory_path.display().to_string(),
        "file_present": memory_path.exists(),
    });
    let api_target = doctor_api_target(config);
    let strict_tool_mode = doctor_strict_tool_mode_status(config);
    let tls_status = doctor_tls_status(config);
    let (code_home, legacy_home) = doctor_state_roots();
    let legacy_state_report = doctor_legacy_state_report(&code_home, &legacy_home);
    let session_recovery = doctor_session_recovery_report(
        &code_home,
        &legacy_home,
        codewhale_config::codewhale_home_is_explicit(),
    );

    let stash = crate::composer_stash::diagnostic_stash_report();
    let report = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "config_path": config_path.display().to_string(),
        "config_present": config_path.exists(),
        "sessions": {
            "last_repair": crate::session_reconcile::last_run(&doctor_paths.sessions),
        },
        "paths": doctor_paths,
        "secret_backend": secret_backend,
        "workspace": workspace.display().to_string(),
        "legacy_state": doctor_legacy_state_json(
            &code_home,
            &legacy_home,
            &legacy_state_report,
            &session_recovery,
        ),
        "setup": doctor_setup_report_json(config, workspace),
        "api_key": {
            "source": doctor_api_key_source_label(credential.source),
            "availability": credential.availability.label(),
        },
        "external_credentials": doctor_external_credential_consent_json(config),
        "dsh_integration": doctor_dsh_integration_json(config, workspace),
        "base_url": crate::doctor::structural_url_authority(&api_target.base_url),
        "default_text_model": api_target.model,
        // DGF-01: this report describes the route a session launched now
        // would resolve; a running session keeps its launch-time route.
        "route_scope": "configured_at_launch",
        "model_resolution": match api_target.resolution {
            DoctorModelResolution::Resolved => "resolved",
            DoctorModelResolution::ConfiguredOnly => "configured_unresolved",
        },
        "route": doctor_route_report(config),
        "strict_tool_mode": doctor_strict_tool_mode_report_json(&strict_tool_mode),
        "tls": {
            "certificate_verification": tls_status.certificate_verification,
            "insecure_skip_tls_verify": tls_status.insecure_skip_tls_verify,
            "provider": tls_status.provider,
            "message": tls_status.message,
        },
        "search_provider": doctor_search_provider_json(config),
        "memory": memory_summary,
        "mcp": mcp_summary,
        "skills": {
            "selected": selected_skills_dir.display().to_string(),
            "global": {
                "path": global_skills_dir.display().to_string(),
                "present": global_skills_dir.exists(),
                "count": skills_count_for(&global_skills_dir),
            },
            "agents": {
                "path": agents_skills_dir.display().to_string(),
                "present": agents_skills_dir.exists(),
                "count": skills_count_for(&agents_skills_dir),
            },
            "agents_global": agents_global_summary,
            "local": {
                "path": local_skills_dir.display().to_string(),
                "present": local_skills_dir.exists(),
                "count": skills_count_for(&local_skills_dir),
            },
            "opencode": {
                "path": opencode_skills_dir.display().to_string(),
                "present": opencode_skills_dir.exists(),
                "count": skills_count_for(&opencode_skills_dir),
            },
            "claude": {
                "path": claude_skills_dir.display().to_string(),
                "present": claude_skills_dir.exists(),
                "count": skills_count_for(&claude_skills_dir),
            },
        },
        "tools": {
            "path": tools_dir.display().to_string(),
            "present": tools_dir.exists(),
            "count": if tools_dir.exists() { count_dir_entries(&tools_dir) } else { 0 },
        },
        "plugins": {
            "path": plugins_dir.display().to_string(),
            "present": plugins_dir.exists(),
            "count": if plugins_dir.exists() { count_dir_entries(&plugins_dir) } else { 0 },
        },
        "storage": {
            "spillover": {
                "path": crate::tools::truncate::spillover_root()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                "present": crate::tools::truncate::spillover_root()
                    .is_some_and(|p| p.is_dir()),
                "count": crate::tools::truncate::spillover_root()
                    .filter(|p| p.is_dir())
                    .map(|p| count_dir_entries(&p))
                    .unwrap_or(0),
            },
            "stash": {
                "path": stash
                    .path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                "present": stash.present,
                "count": stash.count,
                "error": stash.error,
            },
        },
        "sandbox": match crate::sandbox::get_platform_sandbox_with_bwrap_preference(
            config.prefers_bwrap(),
        ) {
            Some(kind) => json!({"available": true, "kind": kind.to_string()}),
            None => json!({"available": false, "kind": null}),
        },
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "api_connectivity": {
            "checked": false,
            "status": "not_probed",
            "note": "JSON doctor is offline; use `codewhale doctor --probe-api` or `--probe-local` for an explicit live check.",
        },
        "capability": provider_capability_report(config),
    });

    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn run_doctor_context_json(config: &Config, workspace: &Path) -> Result<()> {
    let report = crate::context_report::build_headless_context_report(config, workspace);
    println!("{}", crate::context_report::context_report_json(&report));
    Ok(())
}

/// Build the `capability` section for the machine-readable doctor report.
///
/// Returns a JSON value with the resolved provider, resolved model, context
/// window, max output, thinking support, cache telemetry support, and request
/// payload mode.
fn provider_capability_report(config: &Config) -> serde_json::Value {
    use serde_json::json;

    let identity = match config.active_provider_identity() {
        Ok(identity) => identity,
        Err(_) => {
            return json!({"provider": config.provider, "ready": false, "route_error": "provider_identity_unavailable", "auth": {"availability": "unavailable", "source": "unknown"}});
        }
    };
    let provider = identity.provider;
    let configured_model = config.default_model();
    let route_result = crate::route_runtime::resolve_runtime_route_for_identity(
        config,
        &identity,
        Some(&configured_model),
    );
    let route_error = route_result
        .is_err()
        .then_some("route_resolution_failed_details_omitted");
    let route = route_result.ok();
    let resolved_model = route
        .as_ref()
        .map_or(configured_model.as_str(), |route| route.model.as_str());
    // Wire-aware so a custom provider's `wire = "responses" | "anthropic"`
    // reports the payload mode the client will actually speak instead of the
    // static Chat default.
    let cap = crate::config::provider_capability_with_wire(
        provider,
        resolved_model,
        config.provider_wire_dialect(&identity),
    );
    let route_profile = route.as_ref().map(|route| {
        crate::model_profile::resolved_capability_profile_for_route(
            provider,
            resolved_model,
            route.candidate.capabilities(),
            route.candidate.limits(),
        )
    });
    let context_window = route
        .as_ref()
        .map_or(cap.context_window, |route| route.context_window.tokens);
    let context_window_source = route.as_ref().map_or(
        crate::route_runtime::ContextWindowSource::Fallback.label(),
        |route| route.context_window.source.label(),
    );
    // `null` when neither the resolved route nor the compatibility matrix
    // publishes an output ceiling — doctor must not invent one.
    let max_output = route_profile
        .as_ref()
        .and_then(|profile| profile.max_output)
        .or(cap.max_output);
    let is_exact_kimi_code_k3 = route.as_ref().is_some_and(|route| {
        crate::config::is_exact_kimi_code_k3_route(
            provider,
            &route.candidate.endpoint().base_url,
            route.candidate.wire_model_id().as_str(),
        )
    });
    let thinking_supported = is_exact_kimi_code_k3
        || route_profile
            .as_ref()
            .map_or(cap.thinking_supported, |profile| {
                profile.supports_reasoning()
            });
    let cache_telemetry_supported = route_profile
        .as_ref()
        .map_or(cap.cache_telemetry_supported, |profile| {
            profile.prompt_caching.is_supported()
        });
    let request_payload_mode = route_profile
        .as_ref()
        .map_or(cap.request_payload_mode, |profile| {
            profile.request_payload_mode
        });
    let alias_deprecation = config.active_deepseek_alias_deprecation();

    json!({
        "resolved_provider": identity.key.as_str(),
        "resolved_model": resolved_model,
        "context_window": context_window,
        "context_window_source": context_window_source,
        "max_output": max_output,
        "thinking_supported": thinking_supported,
        "cache_telemetry_supported": cache_telemetry_supported,
        "request_payload_mode": serde_json::to_value(request_payload_mode).unwrap_or_default(),
        "route_error": route_error,
        "alias_deprecation": alias_deprecation,
    })
}

fn doctor_route_report(config: &Config) -> serde_json::Value {
    use serde_json::json;

    let target = doctor_api_target(config);
    let identity = match config.active_provider_identity() {
        Ok(identity) => identity,
        Err(_) => {
            return json!({"provider": config.provider, "ready": false, "route_error": "provider_identity_unavailable", "auth": {"availability": "unavailable", "source": "unknown"}});
        }
    };
    let provider = identity.provider;
    let redacted_base_url = crate::doctor::structural_url_authority(&target.base_url);
    let route_result = crate::route_runtime::resolve_runtime_route_for_identity(
        config,
        &identity,
        Some(&target.model),
    );
    let route_error = route_result
        .is_err()
        .then_some("route_resolution_failed_details_omitted");
    let context_window = route_result
        .ok()
        .map(|route| {
        json!({
            "tokens": route.context_window.tokens,
            "source": route.context_window.source.label(),
        })
    })
    .unwrap_or_else(|| {
        json!({
            "tokens": crate::config::provider_capability(provider, &target.model).context_window,
            "source": crate::route_runtime::ContextWindowSource::Fallback.label(),
        })
    });

    let route_identity =
        crate::config::moonshot_k3_route_display_name(&target.base_url, &target.model);
    let credential = resolve_credential_diagnostic(config);

    json!({
        "provider": target.provider,
        "provider_source": doctor_provider_source(config),
        "provider_config_table": doctor_provider_config_table(&identity),
        "model": target.model,
        "route_identity": route_identity,
        "wire_protocol": doctor_wire_protocol(provider),
        "base_url": {
            "redacted": redacted_base_url,
            "class": doctor_base_url_class(provider, &target.base_url),
            "fingerprint": crate::utils::redacted_identifier_for_log(&target.base_url),
        },
        "auth": {
            "scheme": doctor_auth_scheme(config),
            "source": doctor_api_key_source_label(credential.source),
            "availability": credential.availability.label(),
        },
        "context_window": context_window,
        "route_error": route_error,
    })
}

fn doctor_provider_config_table(identity: &crate::config::ProviderIdentity) -> String {
    format!(
        "providers.{}",
        identity
            .compatibility()
            .map_or(identity.key.as_str(), |row| row.config_key)
    )
}

fn doctor_provider_source(config: &Config) -> &'static str {
    if config
        .provider
        .as_ref()
        .is_some_and(|provider| !provider.trim().is_empty())
    {
        "config"
    } else {
        "default"
    }
}

fn doctor_wire_protocol(provider: crate::config::ProviderKind) -> &'static str {
    let policy = provider.provider().wire_policy();
    match policy.fixed() {
        Some(codewhale_config::provider::WireFormat::ChatCompletions) => "chat_completions",
        Some(codewhale_config::provider::WireFormat::Responses) => "responses",
        Some(codewhale_config::provider::WireFormat::AnthropicMessages) => "anthropic_messages",
        None => "model_aware",
    }
}

fn doctor_base_url_class(provider: crate::config::ProviderKind, base_url: &str) -> &'static str {
    let normalized = base_url.trim_end_matches('/').to_ascii_lowercase();
    if normalized.starts_with("http://localhost")
        || normalized.starts_with("http://127.0.0.1")
        || normalized.starts_with("http://[::1]")
    {
        return "local";
    }
    if normalized
        == codewhale_config::descriptors::compatibility_for_kind(provider)
            .base_url
            .trim_end_matches('/')
            .to_ascii_lowercase()
    {
        "default"
    } else {
        "custom"
    }
}

fn doctor_auth_scheme(config: &Config) -> &'static str {
    let Ok(identity) = config.active_provider_identity() else {
        return "unavailable";
    };
    let provider = identity.provider;
    if crate::config::auth_mode_disables_api_key(
        config.auth_mode_for_provider(&identity).as_deref(),
    ) {
        "none"
    } else if provider == crate::config::ProviderKind::Anthropic {
        "x-api-key"
    } else if provider == crate::config::ProviderKind::XiaomiMimo
        && doctor_xiaomi_mimo_base_url_uses_token_plan(&config.base_url_for_route(&identity))
    {
        "api-key"
    } else if provider == crate::config::ProviderKind::XiaomiMimo {
        // The alternate MiMo scheme depends on a credential prefix. Ordinary
        // doctor does not read credentials merely to make this label precise.
        "unknown"
    } else if matches!(
        provider,
        crate::config::ProviderKind::Sglang
            | crate::config::ProviderKind::Vllm
            | crate::config::ProviderKind::Ollama
    ) {
        "optional_bearer"
    } else {
        "bearer"
    }
}

fn doctor_xiaomi_mimo_base_url_uses_token_plan(base_url: &str) -> bool {
    let normalized = base_url.trim_end_matches('/');
    [
        crate::config::XIAOMI_MIMO_TOKEN_PLAN_CN_BASE_URL,
        crate::config::XIAOMI_MIMO_TOKEN_PLAN_SGP_BASE_URL,
        crate::config::XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL,
    ]
    .iter()
    .any(|candidate| normalized.eq_ignore_ascii_case(candidate.trim_end_matches('/')))
}

fn doctor_api_key_source_label(source: ApiKeySource) -> &'static str {
    match source {
        ApiKeySource::ConfigDeclared => "config_declared",
        ApiKeySource::EnvDeclared => "env_declared",
        ApiKeySource::ExternalAuthDeclared => "external_auth_declared",
        ApiKeySource::SecretStoreUnprobed => "secret_store_unprobed",
        ApiKeySource::SecretStoreUnavailable => "secret_store_unavailable",
        ApiKeySource::OAuth => "oauth_unprobed",
        ApiKeySource::ExternalConsent => "external_consent",
        ApiKeySource::NoAuth => "none",
        ApiKeySource::LocalRuntime => "local_runtime",
        ApiKeySource::Unknown => "unknown",
    }
}

fn doctor_search_provider_line(config: &Config) -> String {
    let search_provider = config.search_provider_resolution();
    let switch_hint = if matches!(
        (search_provider.provider, search_provider.source),
        (
            crate::config::SearchProvider::Firecrawl,
            crate::config::SearchProviderSource::Default
        )
    ) {
        "; set [search] provider = \"baidu\" | \"metaso\" | \"volcengine\" for China"
    } else {
        ""
    };
    // Missing-key is stdout-only (never JSON) and only applies when the
    // operator pinned Tavily: autodetect (`tavily key`) cannot reach this line
    // without a key signal, so it never reports a missing key.
    let missing_key = if search_provider.provider == crate::config::SearchProvider::Tavily
        && matches!(
            search_provider.source,
            crate::config::SearchProviderSource::Config
                | crate::config::SearchProviderSource::EnvOverride
        )
        && !search_provider_has_tavily_key(config)
    {
        "; missing TAVILY_API_KEY or [search] api_key"
    } else {
        ""
    };
    // Firecrawl search works without a key. Say so, or a default with no
    // credential reads as a broken setup. Stdout-only, like the missing-key note.
    let keyless = if search_provider.provider == crate::config::SearchProvider::Firecrawl
        && !search_provider_has_firecrawl_key(config)
    {
        "; works without an API key (limited quota; set FIRECRAWL_API_KEY or [search] api_key to raise it)"
    } else {
        ""
    };

    format!(
        "search_provider: {} (source: {}{}){}{}",
        search_provider.provider.as_str(),
        search_provider.source.as_str(),
        switch_hint,
        missing_key,
        keyless
    )
}

/// Whether `web_search` would send a Firecrawl key. Mirrors the adapter:
/// `[search] api_key` shadows `FIRECRAWL_API_KEY`, and a blank value is no key.
fn search_provider_has_firecrawl_key(config: &Config) -> bool {
    let env_key = std::env::var("FIRECRAWL_API_KEY").ok();
    config
        .search
        .as_ref()
        .and_then(|search| search.api_key.as_deref())
        .or(env_key.as_deref())
        .is_some_and(|key| !key.trim().is_empty())
}

/// Whether *any* Tavily key is reachable: the dedicated env var, or a
/// non-empty generic `[search] api_key`. Deliberately not prefix-gated — an
/// explicit `provider = "tavily"` accepts any non-empty generic key.
fn search_provider_has_tavily_key(config: &Config) -> bool {
    crate::config::tavily_env_key().is_some()
        || config
            .search
            .as_ref()
            .and_then(|search| search.api_key.as_deref())
            .is_some_and(|key| !key.trim().is_empty())
}

fn doctor_search_provider_json(config: &Config) -> serde_json::Value {
    use serde_json::json;

    let search_provider = config.search_provider_resolution();
    json!({
        "provider": search_provider.provider.as_str(),
        "source": search_provider.source.as_str(),
        "reachability": "not_checked",
        "reachability_reason": "offline_json",
    })
}

/// Whether the model in a [`DoctorApiTarget`] is the wire id the engine
/// resolver produced, or only the raw configured value because resolution
/// failed. Doctor never prints resolution error details — the JSON route
/// report already redacts them for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoctorModelResolution {
    Resolved,
    ConfiguredOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoctorApiTarget {
    provider: String,
    base_url: String,
    model: String,
    resolution: DoctorModelResolution,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoctorStrictToolModeStatus {
    enabled: bool,
    status: &'static str,
    function_strict_sent: bool,
    message: String,
    recommended_base_url: Option<String>,
}

fn doctor_api_target(config: &Config) -> DoctorApiTarget {
    let identity = config.active_provider_identity().ok();
    // Report the model through the same resolver the live client uses at
    // session launch (`client.rs` → `resolve_runtime_route`), so doctor's
    // answer matches what a session started now would actually serve —
    // saved provider models, alias normalization, and roster preference
    // included — instead of re-deriving a config default that can diverge
    // from the engine (DGF-01, dogfood 2026-08-02).
    let (model, resolution) = match identity
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("provider_identity_unavailable"))
        .and_then(|identity| {
            crate::route_runtime::resolve_runtime_route_for_identity(config, identity, None)
                .map_err(anyhow::Error::msg)
        }) {
        Ok(route) => (route.model.clone(), DoctorModelResolution::Resolved),
        Err(_) => (
            config.default_model(),
            DoctorModelResolution::ConfiguredOnly,
        ),
    };
    DoctorApiTarget {
        provider: identity.as_ref().map_or_else(
            || {
                config
                    .provider
                    .clone()
                    .unwrap_or_else(|| "unavailable".to_string())
            },
            |identity| identity.key.to_string(),
        ),
        base_url: identity
            .as_ref()
            .map_or_else(String::new, |identity| config.base_url_for_route(identity)),
        model,
        resolution,
    }
}

fn doctor_strict_tool_mode_status(config: &Config) -> DoctorStrictToolModeStatus {
    if !config.strict_tool_mode.unwrap_or(false) {
        return DoctorStrictToolModeStatus {
            enabled: false,
            status: "disabled",
            function_strict_sent: false,
            message: "disabled".to_string(),
            recommended_base_url: None,
        };
    }

    let target = doctor_api_target(config);
    match known_deepseek_base_url_kind(&target.base_url) {
        Some(DeepSeekBaseUrlKind::Beta) => DoctorStrictToolModeStatus {
            enabled: true,
            status: "ready",
            function_strict_sent: true,
            message: "enabled; DeepSeek strict schemas use the beta endpoint".to_string(),
            recommended_base_url: None,
        },
        Some(DeepSeekBaseUrlKind::NonBeta) => {
            let recommended = recommended_strict_base_url(config, &target.base_url);
            DoctorStrictToolModeStatus {
                enabled: true,
                status: "fallback_non_beta",
                function_strict_sent: false,
                message:
                    "enabled, but function.strict is stripped for this non-beta DeepSeek endpoint"
                        .to_string(),
                recommended_base_url: Some(recommended.to_string()),
            }
        }
        None => DoctorStrictToolModeStatus {
            enabled: true,
            status: "custom_endpoint",
            function_strict_sent: true,
            message: "enabled; function.strict will be sent to this custom endpoint".to_string(),
            recommended_base_url: None,
        },
    }
}

fn doctor_strict_tool_mode_report_json(status: &DoctorStrictToolModeStatus) -> serde_json::Value {
    serde_json::json!({
        "enabled": status.enabled,
        "status": status.status,
        "function_strict_sent": status.function_strict_sent,
        "message": status.message,
        "recommended_base_url": status
            .recommended_base_url
            .as_deref()
            .map(crate::doctor::structural_url_authority),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DoctorTlsStatus {
    certificate_verification: bool,
    insecure_skip_tls_verify: bool,
    provider: String,
    message: String,
}

fn doctor_tls_status(config: &Config) -> DoctorTlsStatus {
    let provider = config.active_provider_identity().map_or_else(
        |_| {
            config
                .provider
                .clone()
                .unwrap_or_else(|| "unavailable".to_string())
        },
        |identity| identity.key.to_string(),
    );
    let insecure_skip_tls_verify = config.insecure_skip_tls_verify();
    let message = if insecure_skip_tls_verify {
        format!(
            "TLS certificate verification cannot be disabled for provider {provider}; use SSL_CERT_FILE with a trusted custom CA bundle"
        )
    } else {
        "TLS certificate verification enabled".to_string()
    };
    DoctorTlsStatus {
        certificate_verification: true,
        insecure_skip_tls_verify,
        provider,
        message,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeepSeekBaseUrlKind {
    Beta,
    NonBeta,
}

fn known_deepseek_base_url_kind(base_url: &str) -> Option<DeepSeekBaseUrlKind> {
    let normalized = base_url.trim_end_matches('/');
    if normalized.eq_ignore_ascii_case("https://api.deepseek.com/beta")
        || normalized.eq_ignore_ascii_case("https://api.deepseeki.com/beta")
    {
        Some(DeepSeekBaseUrlKind::Beta)
    } else if normalized.eq_ignore_ascii_case("https://api.deepseek.com")
        || normalized.eq_ignore_ascii_case("https://api.deepseek.com/v1")
        || normalized.eq_ignore_ascii_case("https://api.deepseeki.com")
        || normalized.eq_ignore_ascii_case("https://api.deepseeki.com/v1")
    {
        Some(DeepSeekBaseUrlKind::NonBeta)
    } else {
        None
    }
}

fn recommended_strict_base_url(_config: &Config, _base_url: &str) -> &'static str {
    crate::config::DEFAULT_DEEPSEEK_BASE_URL
}

fn doctor_timeout_recovery_lines(config: &Config) -> Vec<String> {
    let target = doctor_api_target(config);
    let mut lines = vec![
        format!(
            "Connection timed out while reaching {}.",
            crate::doctor::structural_url_authority(&target.base_url)
        ),
        // #6889: some providers load a model on its first call, which can
        // take longer than this check waits.
        "If the key and endpoint are right, the model may still be loading: a first call to a model that has not been used recently can take a minute or two. Wait and run the check again."
            .to_string(),
    ];

    match config
        .active_provider_identity()
        .ok()
        .map(|identity| identity.provider)
    {
        Some(crate::config::ProviderKind::Deepseek)
            if target.base_url.contains("api.deepseek.com")
                && !target.base_url.contains("api.deepseeki.com") =>
        {
            lines.push(
                "If this is a custom DeepSeek-compatible endpoint, set its HTTPS base URL in ~/.codewhale/config.toml and rerun `codewhale doctor`."
                    .to_string(),
            );
        }
        Some(crate::config::ProviderKind::Deepseek) => {
            lines.push(
                "If this is a custom DeepSeek-compatible endpoint, confirm it serves `/v1/models` and `/v1/chat/completions` over HTTPS."
                    .to_string(),
            );
        }
        _ => {
            lines.push(
                "Confirm the configured provider endpoint is reachable and OpenAI-compatible for `/v1/models` and `/v1/chat/completions`."
                    .to_string(),
            );
        }
    }

    lines.push(
        "Run `codewhale doctor --json` and include `base_url`, `default_text_model`, and `api_connectivity` when filing an issue."
            .to_string(),
    );
    lines
}

fn run_features_command(config: &Config, command: FeaturesCli) -> Result<()> {
    match command.command {
        FeaturesSubcommand::List => {
            print!("{}", render_feature_table(&config.features()));
            Ok(())
        }
    }
}

async fn run_models(config: &Config, args: ModelsArgs) -> Result<()> {
    crate::provider_lake::run_models(config, args.update, args.provider.as_deref(), args.json).await
}

async fn run_speech(config: &Config, args: SpeechArgs) -> Result<()> {
    use crate::client::{CodewhaleClient, SpeechSynthesisRequest};
    use crate::config::ProviderKind;
    use crate::tools::speech::{
        DEFAULT_VOICE, SpeechPreparation, SpeechSurface, SpeechVoice, describe_speech_voice,
        encode_voice_clone_sample_data_uri, prepare_speech_format, prepare_speech_options,
    };

    let SpeechArgs {
        text,
        output,
        output_dir,
        model,
        voice,
        instruction,
        voice_prompt,
        clone_voice,
        format,
        json: json_output,
    } = args;

    let identity = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    if identity.provider != ProviderKind::XiaomiMimo {
        bail!(
            "`speech` requires provider = \"xiaomi-mimo\" (current: {}). Run with `--provider xiaomi-mimo` or set it in config.",
            identity.key.as_str()
        );
    }

    if text.trim().is_empty() {
        bail!("Speech text cannot be empty");
    }
    let features = config.features();
    let workspace = std::env::current_dir()?;
    let context = crate::tools::spec::ToolContext::new(workspace).with_features(features);
    let adapter_error = |error| match error {
        crate::tools::spec::ToolError::InvalidInput { message } => anyhow::anyhow!(message),
        error => anyhow::anyhow!(error),
    };
    let plan = prepare_speech_options(
        SpeechPreparation {
            model: model.as_deref(),
            voice: voice.as_deref(),
            instruction,
            voice_prompt,
            has_clone_path: clone_voice.is_some(),
            surface: SpeechSurface::Cli,
        },
        &context,
    )
    .await
    .map_err(adapter_error)?;
    let model = plan.model;
    let instruction = plan.instruction;
    // Clone sample/voice data stays inside the existing Core/provider path.
    let voice = match plan.voice {
        SpeechVoice::Clone => Some(encode_voice_clone_sample_data_uri(
            clone_voice
                .as_deref()
                .context("speech adapter requested an uncaptured clone sample")?,
        )?),
        SpeechVoice::Omit => None,
        SpeechVoice::Raw => Some(voice.context("speech adapter requested an uncaptured voice")?),
        SpeechVoice::Default => Some(DEFAULT_VOICE.to_string()),
    };
    let format = prepare_speech_format(&format, SpeechSurface::Cli, &context)
        .await
        .map_err(adapter_error)?;
    let output = output.unwrap_or_else(|| {
        output_dir
            .or_else(|| config.speech_output_dir())
            .unwrap_or_default()
            .join(format!("speech.{format}"))
    });

    let client = CodewhaleClient::new(config)?;
    let response = client
        .synthesize_speech(SpeechSynthesisRequest {
            model: model.clone(),
            text,
            instruction,
            audio_format: format.clone(),
            voice,
        })
        .await?;

    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create output directory {}", parent.display()))?;
    }
    tokio::fs::write(&output, &response.audio_bytes)
        .await
        .with_context(|| format!("Failed to write audio file {}", output.display()))?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "mode": "speech",
                "success": true,
                "model": response.model,
                "format": response.audio_format,
                "output": output.display().to_string(),
                "bytes": response.audio_bytes.len(),
                "voice": response.voice.as_deref().map(describe_speech_voice),
                "transcript": response.transcript,
            }))?
        );
    } else {
        println!(
            "Generated speech: {} ({} bytes, model: {}, format: {})",
            output.display(),
            response.audio_bytes.len(),
            response.model,
            response.audio_format
        );
    }

    Ok(())
}

#[cfg(test)]
#[path = "tests/speech_host_cli.rs"]
mod speech_host_cli_tests;

#[cfg(test)]
mod speech_cli_tests {
    use super::*;
    use crate::tools::speech::{
        default_speech_output_name, infer_speech_model, normalize_speech_format,
    };

    #[test]
    fn normalizes_documented_speech_formats() {
        assert_eq!(normalize_speech_format("WAV").as_deref(), Some("wav"));
        assert_eq!(normalize_speech_format("pcm16").as_deref(), Some("pcm16"));
        assert_eq!(normalize_speech_format("pcm").as_deref(), Some("pcm16"));
        assert_eq!(normalize_speech_format("flac"), None);
    }

    #[test]
    fn default_speech_output_tracks_requested_format() {
        assert_eq!(
            PathBuf::from(default_speech_output_name("mp3")),
            PathBuf::from("speech.mp3")
        );
        assert_eq!(
            PathBuf::from("audio").join(default_speech_output_name("pcm")),
            PathBuf::from("audio").join("speech.pcm16")
        );
    }

    #[test]
    fn speech_command_parses_cli_passthrough_smoke() {
        let cli = Cli::try_parse_from([
            "codewhale-tui",
            "speech",
            "hello",
            "--model",
            "tts",
            "--format",
            "pcm",
            "--output-dir",
            "audio",
            "--voice",
            "Mia",
        ])
        .expect("speech command parses");

        let Some(Commands::Speech(args)) = cli.command else {
            panic!("expected speech command");
        };
        assert_eq!(args.text, "hello");
        assert_eq!(
            infer_speech_model(args.model.as_deref(), false, false),
            "mimo-v2.5-tts"
        );
        assert_eq!(
            normalize_speech_format(&args.format).as_deref(),
            Some("pcm16")
        );
        assert_eq!(args.output_dir, Some(PathBuf::from("audio")));
        assert_eq!(args.voice.as_deref(), Some("Mia"));
    }
}

/// Test API connectivity by making a minimal request
async fn test_api_connectivity(config: &Config) -> Result<()> {
    use crate::client::CodewhaleClient;
    use codewhale_models::{ContentBlock, Message, MessageRequest};

    let client = CodewhaleClient::new(config)?;
    let model = client.model().to_string();

    if crate::doctor::is_keyless_ds4_route(config) {
        return crate::doctor::probe_ds4_models(config).await;
    }

    // Minimal request: single word prompt, 1 max token
    let request = MessageRequest {
        model: model.clone(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
                cache_control: None,
            }],
        }],
        max_tokens: 1,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        // This is a one-token transport probe, not a reasoning task.
        reasoning_effort: Some("off".to_string()),
        stream: Some(false),
        temperature: None,
        top_p: None,
    };

    // Use tokio timeout to catch hanging requests. The timeout is typed so
    // the report can tell "no answer yet" from a refusal (#6889).
    match tokio::time::timeout(DOCTOR_PROBE_TIMEOUT, client.create_message(request)).await {
        Ok(Ok(_response)) => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow::Error::new(crate::llm_client::LlmError::Timeout(
            DOCTOR_PROBE_TIMEOUT,
        ))),
    }
}

/// How long the opt-in live check waits for its one-token answer.
const DOCTOR_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Whether the live check ran out of time instead of being refused (#6889).
/// A provider that loads a model on its first call can take longer than the
/// check waits, so this means "no answer yet", not a rejected key or a route
/// that does not work.
fn doctor_probe_timed_out(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<crate::llm_client::LlmError>(),
            Some(crate::llm_client::LlmError::Timeout(_))
        )
    })
}

fn rustc_version() -> String {
    // `RustC::available()` resolves the tool once, capturing the `--version`
    // banner as a side effect of the probe; reuse it instead of launching a
    // second rustc process (each launch loads libLLVM).
    if !crate::dependencies::RustC::available() {
        return "not installed (only needed to build from source)".to_string();
    }
    crate::dependencies::rustc_version_banner().unwrap_or_else(|| "unknown".to_string())
}

/// List saved sessions
fn sessions_resume_command() -> &'static str {
    "codewhale resume"
}

/// Newest files per category (sessions/checkpoints and Runtime receipts) inspected;
/// `codewhale sessions scrub-secrets` covers every file.
const DOCTOR_SECRET_SCAN_FILES: usize = 50;

/// B1 finding: sessions written before tool output was redacted at the
/// transcript boundary can still hold live credentials. Report, never
/// rewrite — scrubbing is the explicit `scrub-secrets` command. Doctor is a
/// read-only diagnostic, so this resolves the sessions directory with the
/// read-path resolver: it never creates the home or migrates a legacy tree.
async fn print_doctor_stored_secrets_report(config_path: Option<PathBuf>, profile: Option<String>) {
    use colored::Colorize;

    let scan = tokio::task::spawn_blocking(move || -> Result<_> {
        let sessions_dir = codewhale_config::resolve_state_dir("sessions")?;
        let runtime = runtime_threads::RuntimeThreadManagerConfig::from_task_data_dir(
            task_manager::default_tasks_dir(),
        );
        let mut unreadable = Vec::new();
        let files =
            session_secret_scrub::session_files(&sessions_dir, &runtime.data_dir, &mut unreadable);
        let total = files.len();
        let checked = session_secret_scrub::doctor_files(files, DOCTOR_SECRET_SCAN_FILES);
        let secrets = session_secret_scrub::configured_secrets(config_path, profile.as_deref())?;
        let mut report = session_secret_scrub::scrub_files(&checked, None, &secrets)?;
        report.unreadable.extend(unreadable);
        Ok((report, total))
    })
    .await;
    println!();
    println!("{}", "Stored Sessions:".bold());
    match scan {
        Ok(Ok((report, total))) => println!("{}", doctor_stored_secrets_summary(&report, total)),
        _ => println!("  ! stored credential scan could not be completed"),
    }
}

fn doctor_stored_secrets_summary(
    report: &session_secret_scrub::ScrubReport,
    total: usize,
) -> String {
    let scope = if total > report.files_scanned {
        format!(
            "{} of {total}; newest up to {DOCTOR_SECRET_SCAN_FILES} per category: sessions/checkpoints and Runtime receipts",
            report.files_scanned
        )
    } else {
        format!("{total}")
    };
    let mut lines = Vec::new();
    if report.flagged_files.is_empty() && report.unreadable.is_empty() {
        lines.push(format!(
            "  ✓ no credentials found in stored tool output ({scope} files)"
        ));
    }
    if !report.flagged_files.is_empty() {
        lines.push(format!(
            "  ✗ {} files hold credentials in stored tool output ({scope} checked)",
            report.flagged_files.len()
        ));
        lines.push(format!(
            "    fix: `{}` to review, then `--apply` to mask them; rotate any exposed credential",
            session_secret_scrub::SCRUB_COMMAND
        ));
    }
    if !report.unreadable.is_empty() {
        lines.push(format!(
            "  ! scan incomplete: {} files or directories could not be read or parsed ({scope} checked)",
            report.unreadable.len()
        ));
        for path in &report.unreadable {
            lines.push(format!("    {}", path.display()));
        }
    }
    lines.join("\n")
}

async fn run_sessions_scrub_secrets(
    apply: bool,
    config_path: Option<PathBuf>,
    profile: Option<String>,
) -> Result<()> {
    #[cfg(test)]
    let ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(ticket);
        run_sessions_scrub_secrets_blocking(apply, config_path, profile.as_deref())
    })
    .await?
}

fn run_sessions_scrub_secrets_blocking(
    apply: bool,
    config_path: Option<PathBuf>,
    profile: Option<&str>,
) -> Result<()> {
    let manager = session_manager::SessionManager::default_location()?;
    let runtime = runtime_threads::RuntimeThreadManagerConfig::from_task_data_dir(
        task_manager::default_tasks_dir(),
    );
    let mut unreadable = Vec::new();
    let files = session_secret_scrub::session_files(
        manager.sessions_dir(),
        &runtime.data_dir,
        &mut unreadable,
    );
    let secrets = session_secret_scrub::configured_secrets(config_path, profile)?;
    let mut report =
        session_secret_scrub::scrub_files(&files, apply.then_some(&manager), &secrets)?;
    report.unreadable.extend(unreadable);
    let affected = report.flagged_files.len();
    if affected == 0 && report.unreadable.is_empty() && report.busy.is_empty() {
        println!(
            "No stored credentials found in tool output across {} files.",
            report.files_scanned
        );
    } else if affected > 0 {
        let verb = if apply { "Scrubbed" } else { "Found" };
        println!(
            "{verb} {} credential-bearing tool results in {affected} of {} session files:",
            report.flagged_tool_results, report.files_scanned
        );
        for path in &report.flagged_files {
            println!("  {}", path.display());
        }
        if !apply {
            println!(
                "Re-run with `{} --apply` to mask them (close open Codewhale sessions first). \
                 Rotate any credential that was exposed: redaction cannot un-leak it.",
                session_secret_scrub::SCRUB_COMMAND
            );
        }
    }
    if !report.unreadable.is_empty() {
        println!(
            "{} files or directories could not be read or parsed and were left untouched.",
            report.unreadable.len()
        );
        for path in &report.unreadable {
            println!("  {}", path.display());
        }
    }
    if !report.busy.is_empty() {
        println!(
            "{} credential-bearing files were skipped because their session or Runtime store is open. Close the session or Runtime server and re-run `{} --apply`:",
            report.busy.len(),
            session_secret_scrub::SCRUB_COMMAND
        );
        for path in &report.busy {
            println!("  {}", path.display());
        }
    }
    Ok(())
}

fn list_sessions(limit: usize, search: Option<String>) -> Result<()> {
    use codewhale_palette as palette;
    use colored::Colorize;
    use session_manager::{SessionManager, format_session_line};

    let (action_r, action_g, action_b) = palette::WHALE_ACTION_RGB;
    let (human_r, human_g, human_b) = palette::WHALE_HUMAN_RGB;
    let (sky_r, sky_g, sky_b) = palette::WHALE_ACTION_RGB;
    let (aqua_r, aqua_g, aqua_b) = palette::WHALE_ACTION_RGB;

    let manager = SessionManager::default_location()?;

    let sessions = if let Some(query) = search {
        manager.search_sessions(&query)?
    } else {
        manager.list_sessions()?
    };

    if sessions.is_empty() {
        println!("{}", "No sessions found.".truecolor(sky_r, sky_g, sky_b));
        println!(
            "Start a new session with: {}",
            "codewhale".truecolor(human_r, human_g, human_b)
        );
        return Ok(());
    }

    println!(
        "{}",
        "Saved Sessions"
            .truecolor(action_r, action_g, action_b)
            .bold()
    );
    println!("{}", "==============".truecolor(sky_r, sky_g, sky_b));
    println!();

    for (i, session) in sessions.iter().take(limit).enumerate() {
        let line = format_session_line(session);
        if i == 0 {
            println!("  {} {}", "*".truecolor(aqua_r, aqua_g, aqua_b), line);
        } else {
            println!("    {line}");
        }
    }

    let total = sessions.len();
    if total > limit {
        println!();
        println!(
            "  {} more session(s). Use --limit to show more.",
            total - limit
        );
    }

    println!();
    println!(
        "Resume with: {} {}",
        sessions_resume_command().truecolor(action_r, action_g, action_b),
        "<session-id>".dimmed()
    );
    println!(
        "Continue latest in this workspace: {}",
        "codewhale --continue".truecolor(action_r, action_g, action_b)
    );

    Ok(())
}

/// Export one saved session as a full-fidelity `tar.xz` archive
/// (`session_export`). Prefers an exact session id; falls back to an
/// unambiguous id prefix like the resume flow.
fn run_sessions_export(
    id: &str,
    output: Option<&Path>,
    skip_artifacts: bool,
    compression: u32,
    force: bool,
) -> Result<()> {
    use session_export::{SessionArchiveOptions, default_archive_file_name, write_session_archive};

    let manager = SessionManager::default_location()?;
    let session = match manager.load_session_snapshot(id) {
        Ok(session) => session,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            manager.load_session_snapshot(&manager.resolve_session_id_prefix(id)?)?
        }
        Err(error) => return Err(error.into()),
    };

    let output_path = output.map_or_else(
        || PathBuf::from(default_archive_file_name(&session.metadata)),
        Path::to_path_buf,
    );
    let summary = write_session_archive(
        &session,
        Some(manager.sessions_dir()),
        &output_path,
        SessionArchiveOptions {
            include_artifacts: !skip_artifacts,
            compression_level: compression,
            overwrite: force,
        },
    )?;

    use codewhale_localization::{MessageId, resolve_locale, tr};
    let settings = crate::settings::Settings::load_read_only().unwrap_or_default();
    let locale = resolve_locale(&settings.locale);
    println!(
        "{} {} → {}",
        tr(locale, MessageId::SessionArchiveExported),
        truncate_id(&session.metadata.id),
        summary.output.display()
    );
    println!(
        "  {}: {} / {} / {}",
        tr(locale, MessageId::SessionArchiveSizes),
        summary.members.len(),
        format_bytes(summary.total_member_bytes()),
        format_bytes(summary.compressed_bytes())
    );
    if !summary.includes_artifacts && !skip_artifacts {
        println!("  {}", tr(locale, MessageId::SessionArchiveNoArtifacts));
    }
    println!("  {}", tr(locale, MessageId::SessionArchiveRestoreHint));
    Ok(())
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes >= KIB * KIB {
        format!("{:.1} MiB", bytes / (KIB * KIB))
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Initialize a new project with AGENTS.md
fn init_project() -> Result<()> {
    use codewhale_palette as palette;
    use colored::Colorize;
    use project_context::create_default_agents_md;

    let (sky_r, sky_g, sky_b) = palette::WHALE_ACTION_RGB;
    let (aqua_r, aqua_g, aqua_b) = palette::WHALE_ACTION_RGB;
    let (red_r, red_g, red_b) = palette::WHALE_ERROR_RGB;

    let workspace = std::env::current_dir()?;
    let agents_path = workspace.join("AGENTS.md");

    if agents_path.exists() {
        println!(
            "{} AGENTS.md already exists at {}",
            "!".truecolor(sky_r, sky_g, sky_b),
            agents_path.display()
        );
        return Ok(());
    }

    match create_default_agents_md(&workspace) {
        Ok(path) => {
            println!(
                "{} Created {}",
                "✓".truecolor(aqua_r, aqua_g, aqua_b),
                path.display()
            );
            println!();
            println!("Edit this file to customize how the AI agent works with your project.");
            println!("The instructions will be loaded automatically when you run codewhale.");
        }
        Err(e) => {
            println!(
                "{} Failed to create AGENTS.md: {}",
                "✗".truecolor(red_r, red_g, red_b),
                e
            );
        }
    }

    Ok(())
}

fn resolve_workspace(cli: &Cli) -> PathBuf {
    cli.workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

async fn plugin_auth_entry_from_cli(
    cli: &Cli,
    provider: &str,
) -> Result<crate::config::ProviderConfig> {
    let path = cli.config.clone();
    let profile = effective_config_profile(cli);
    let options = cli.options.clone();
    let provider = provider.to_owned();
    let policy = crate::plugins::activation::extension_host_policy_enabled();
    tokio::task::spawn_blocking(move || {
        let _scope = crate::plugins::activation::PolicyScope::propagate(policy);
        let config = load_config_with_cli_preferences(path, profile.as_deref(), &options)?;
        crate::plugins::providers::plugin_auth_entry(&config, &provider)
    })
    .await?
}

fn load_config_from_cli(cli: &Cli) -> Result<Config> {
    load_config_from_cli_with_effective_profile(cli).map(|(config, _)| config)
}

/// Doctor is a structural report unless the user explicitly asks it to probe
/// a provider endpoint. Keep credential-bearing environment values out of the
/// regular diagnostic configuration so an unrelated renderer or error path
/// cannot disclose them.
fn load_doctor_config_from_cli(cli: &Cli, args: &DoctorArgs) -> Result<Config> {
    if args.probe_api || args.probe_local {
        return load_config_from_cli(cli);
    }
    load_structural_config_from_cli(cli)
}

fn load_structural_config_from_cli(cli: &Cli) -> Result<Config> {
    let profile = effective_config_profile(cli);
    let mut config = Config::load_structural(cli.config.clone(), profile.as_deref())?;
    if let Ok(settings) = crate::settings::Settings::load_read_only() {
        apply_saved_reasoning_preference(&mut config, &settings);
    }
    cli.options.apply_features(&mut config)?;
    install_extension_host_boot_config(&config);
    Ok(config)
}

/// Select the plugin activation policy (v5, or v6 with the experimental
/// extension host) and the host's runtime settings, once per process, before
/// any plugin discovery. Later config reloads never flip either.
fn install_extension_host_boot_config(config: &Config) {
    let enabled = config
        .features()
        .enabled(crate::features::Feature::ExtensionHost);
    crate::plugins::activation::install_extension_host_policy(enabled);
    // Builtin MCP uses the configured runtime without enabling Native plugins.
    // Configuration is lazy: it does not start an unused host.
    crate::extension_host::configure_with_handle(
        crate::extension_host::ExtensionHostOptions::from_config(config.extension_host.as_ref()),
        tokio::runtime::Handle::try_current().ok(),
    );
    if enabled {
        // The host asks the command table whether a built-in command answers
        // to a name before it admits an extension command; without this it
        // refuses every extension command.
        crate::extension_host::command::install_builtin_commands(std::sync::Arc::new(
            crate::commands::BuiltinCommandNames,
        ));
        // Per-plugin settings (`[plugins."<name>".config]`) and the file
        // `/plugin reload` re-reads them from.
        crate::extension_host::install_plugin_settings(
            config.plugins.as_ref(),
            config.loaded_config_path.clone(),
        );
    }
}

fn effective_config_profile(cli: &Cli) -> Option<String> {
    cli.profile
        .clone()
        .or_else(|| std::env::var("CODEWHALE_PROFILE").ok())
        .or_else(|| std::env::var("DEEPSEEK_PROFILE").ok())
}

fn load_config_from_cli_with_effective_profile(cli: &Cli) -> Result<(Config, Option<String>)> {
    let profile = effective_config_profile(cli);
    let config =
        load_config_with_cli_preferences(cli.config.clone(), profile.as_deref(), &cli.options)?;
    Ok((config, profile))
}

fn load_config_with_cli_preferences(
    path: Option<PathBuf>,
    profile: Option<&str>,
    options: &RuntimeOptions,
) -> Result<Config> {
    let mut config = Config::load(path, profile)?;
    // Config loading is shared by diagnostics and mutating runtimes. Read the
    // saved preference without migrating or creating state here; interactive
    // startup performs any permitted migration later through `Settings::load`.
    if let Ok(settings) = crate::settings::Settings::load_read_only() {
        apply_saved_reasoning_preference(&mut config, &settings);
    }
    options.apply_features(&mut config)?;
    install_extension_host_boot_config(&config);
    // Install the foreign-instruction opt-in before anything can load project
    // context. This is the single funnel every runtime goes through — TUI,
    // exec, ACP, and the app-server passthrough all resolve config here — so
    // the loader never has to be handed the setting at each of its call sites.
    install_foreign_instruction_imports(&config);
    Ok(config)
}

/// Apply the selected v2 Fleet's operator to a fresh root session.
///
/// Provider/model are one atomic route: any explicit launch override for
/// either half keeps the caller's full route and bypasses the saved operator.
/// Reasoning is independent, so an explicit reasoning flag keeps its value
/// while the Fleet may still select the provider/model pair. Resumes call
/// this helper only for fresh sessions and therefore retain their saved route.
fn apply_selected_fleet_operator_for_launch(
    config: &mut Config,
    workspace: &Path,
    explicit_route_override: bool,
    explicit_reasoning_override: bool,
) -> Result<bool> {
    if explicit_route_override {
        return Ok(false);
    }
    let Some(selected) = crate::fleet::store::resolve_selected_fleet(workspace).map_err(|_| {
        anyhow!(
            "Selected Fleet is missing or unreadable; inspect /fleet and repair or clear the selection."
        )
    })?
    else {
        return Ok(false);
    };
    let fleet_name = crate::safe_label::SafeLabel::phrase(&selected.name);
    let (fleet, _) = crate::fleet::store::load_fleet_at(&selected.path).map_err(|_| {
        anyhow!(
            "selected Fleet '{}' ({}) is invalid or unreadable; inspect /fleet and repair or clear the selection.",
            fleet_name,
            selected.scope.label()
        )
    })?;
    let Some(operator) = fleet.operator.as_ref() else {
        return Ok(false);
    };
    let provider_id = operator.provider.trim();
    let model_id = operator.model.trim();
    if provider_id.is_empty() || model_id.is_empty() {
        bail!(
            "selected Fleet '{}' has an incomplete operator route; provider and model must both be non-empty",
            fleet_name
        );
    }
    let safe_provider_id = crate::safe_label::SafeLabel::identifier(provider_id);
    let safe_model_id = crate::safe_label::SafeLabel::catalog_model(model_id);

    let identity = config
        .resolve_provider_pin_identity(provider_id)
        .map_err(|error| {
            anyhow!(
                "selected Fleet '{}' operator provider '{}' is unavailable: {}",
                fleet_name,
                safe_provider_id,
                crate::safe_label::safe_error_text(&error)
            )
        })?;
    let resolved =
        crate::route_runtime::resolve_runtime_route_for_identity(config, &identity, Some(model_id))
            .map_err(|error| {
                anyhow!(
                    "selected Fleet '{}' operator route {}/{} is invalid: {}",
                    fleet_name,
                    safe_provider_id,
                    safe_model_id,
                    crate::safe_label::safe_error_text(&error)
                )
            })?;
    let mut selected_config = *resolved.config;
    selected_config.fleet_operator_route_applied = true;
    selected_config.fleet_operator_reasoning_applied = false;
    if !explicit_reasoning_override
        && let Some(reasoning) = operator
            .reasoning
            .as_deref()
            .map(str::trim)
            .filter(|reasoning| !reasoning.is_empty())
        && let Some(reasoning) = normalize_cli_reasoning_effort(reasoning).map_err(|error| {
            anyhow!(
                "selected Fleet '{}' has invalid operator reasoning: {}",
                fleet_name,
                crate::safe_label::safe_error_text(&error.to_string())
            )
        })?
    {
        selected_config.reasoning_effort = Some(reasoning);
        selected_config.reasoning_effort_inferred_from_legacy_alias = false;
        selected_config.fleet_operator_reasoning_applied = true;
    }
    *config = selected_config;
    Ok(true)
}

/// Resolve `project_instruction_imports` into the loader's opt-in set.
///
/// Unrecognized names are reported rather than dropped: a typo in this key
/// silently means "import nothing", which is exactly the failure mode a user
/// would not notice.
fn install_foreign_instruction_imports(config: &Config) {
    let (imports, unknown) = crate::project_context::ForeignInstructionImports::from_config(
        &config.project_instruction_imports,
    );
    for name in unknown {
        tracing::warn!(
            target: "project_context",
            value = %name,
            "Ignoring unknown project_instruction_imports entry; expected one of              claude, cursor, cline, windsurf, gemini, copilot, muse, all"
        );
    }
    crate::project_context::set_foreign_instruction_imports(imports);
}

/// Apply the same reasoning-preference precedence as interactive `App`
/// construction to non-TUI runtimes.
///
/// `/model` and the config editor persist this preference in `settings.toml`.
/// Exec, review, workflow, ACP, and runtime-thread launches all begin with a
/// `Config`, so copying the saved value here keeps those entry points from
/// silently falling back to a route classifier or an older config.toml value.
fn apply_saved_reasoning_preference(config: &mut Config, settings: &crate::settings::Settings) {
    let Some(reasoning_effort) = settings.reasoning_effort.as_ref() else {
        return;
    };
    config.reasoning_effort = Some(reasoning_effort.clone());
    config.reasoning_effort_inferred_from_legacy_alias = false;
}

fn run_login(api_key: Option<String>) -> Result<()> {
    if api_key.is_some() {
        bail!(
            "`login --api-key` is not account sign-in. \
             Use `codewhale login` for the Codewhale account, \
             or `codewhale auth set --provider <id>` for a provider key."
        );
    }
    bail!(
        "This binary's `login` command does not store provider keys. \
         Use the `codewhale` CLI: `codewhale login` for the Codewhale account device flow, \
         or `codewhale auth set --provider <id>` for a provider key."
    );
}

fn run_logout() -> Result<()> {
    config::clear_api_key()?;
    println!("Cleared saved API key.");
    Ok(())
}

async fn run_xai_device_auth(config_path: Option<&Path>) -> Result<()> {
    let pending = crate::oauth::login(crate::oauth::OAuthProvider::Xai).await?;
    let activation = crate::oauth::activate_login(pending, config_path, None)?;
    println!("{}", activation.summary(codewhale_localization::Locale::En));
    println!(
        "xAI OAuth is ready; activated {} via {}",
        codewhale_config::quote_os_path(&activation.auth_path),
        codewhale_config::quote_os_path(&activation.config_path)
    );
    println!(
        "To switch accounts later, run `codewhale auth xai-device` again (or `/auth xai-device` in Codewhale) and approve with the other account. Restart open Codewhale sessions after a shell login."
    );
    Ok(())
}

async fn run_claude_auth(config_path: Option<&Path>) -> Result<()> {
    let pending = crate::oauth::login(crate::oauth::OAuthProvider::Claude).await?;
    let path = config_path.map(Path::to_path_buf);
    let activation = tokio::task::spawn_blocking(move || {
        crate::oauth::activate_login(pending, path.as_deref(), None)
    })
    .await
    .context("Claude activation worker failed")??;
    println!("{}", activation.summary(codewhale_localization::Locale::En));
    println!(
        "Use `codewhale --provider anthropic` or `/provider anthropic`. Sign out with `codewhale auth claude-revoke`."
    );
    Ok(())
}

async fn run_chatgpt_pkce_auth(config_path: Option<&Path>) -> Result<()> {
    let config = Config::load(config_path.map(Path::to_path_buf), None)?;
    let pending =
        crate::oauth::login_with_config(crate::oauth::OAuthProvider::Chatgpt, &config).await?;
    let activation = crate::oauth::activate_login(pending, config_path, None)?;
    println!("{}", activation.summary(codewhale_localization::Locale::En));
    if let Some(warning) = activation.env_override_warning(codewhale_localization::Locale::En) {
        println!("{warning}");
    }
    println!(
        "ChatGPT OAuth is ready; activated {} via {}",
        codewhale_config::quote_os_path(&activation.auth_path),
        codewhale_config::quote_os_path(&activation.config_path)
    );
    println!(
        "To switch ChatGPT accounts or workspaces, run `CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt` in a shell and restart open Codewhale sessions. `/auth chatgpt` reauthorizes the selected account."
    );
    let mut selected = Config::load(config_path.map(Path::to_path_buf), None)?;
    selected.provider = Some(config::ProviderKind::OpenaiCodex.as_str().to_string());
    match crate::codex_model_cache::update_from_chatgpt(&selected).await {
        Ok(roster) => println!(
            "{} ChatGPT models available. Use `codewhale models --provider openai-codex` to list them.",
            roster.models.len()
        ),
        Err(_) => println!(
            "Sign-in is saved. Model discovery is unavailable; retry `codewhale models --update --provider openai-codex`."
        ),
    }
    Ok(())
}

fn run_chatgpt_pkce_revoke(config_path: Option<&Path>) -> Result<()> {
    crate::oauth::revoke_owned_login(crate::oauth::OAuthProvider::Chatgpt, config_path, None)?;
    println!("Removed Codewhale's saved ChatGPT sign-in.");
    Ok(())
}

/// OrcaRouter account sign-in: OAuth 2.0 + PKCE on a loopback redirect,
/// exchanged for a durable `sk-orca-...` key.
///
/// This is the "OrcaRouter - Auth" entry point. It never replaces the
/// API-key path (`codewhale auth set --provider orcarouter`); both land in the
/// same credential slot and are independently usable.
async fn run_orcarouter_pkce_auth(config_path: Option<&Path>) -> Result<()> {
    let inputs = crate::oauth::OrcaLoginInputs::from_env();
    if inputs.auth_base == crate::oauth::ORCAROUTER_AUTH_BASE
        && std::env::var_os("ORCA_AUTH_BASE_URL").is_none()
    {
        println!(
            "Signing in to OrcaRouter at {} (consent is granted on the OrcaRouter site).",
            crate::oauth::ORCAROUTER_AUTH_BASE
        );
    }
    let api_base = inputs.api_base.clone();
    if api_base != crate::oauth::ORCAROUTER_API_BASE {
        println!("OrcaRouter inference and model discovery will use {api_base}.");
    }
    let mut challenge = crate::oauth::cli_challenge_writer()?;
    let credential = tokio::task::spawn_blocking(move || {
        crate::oauth::orcarouter_pkce_login(&inputs, challenge.as_mut())
    })
    .await
    .context("OrcaRouter PKCE login worker failed")??;
    let saved = crate::oauth::activate_orcarouter_credential(&credential, config_path)?;
    println!(
        "OrcaRouter is ready; stored the key in {}",
        saved.describe()
    );
    if !credential.scope_satisfies_purpose() {
        println!(
            "Note: OrcaRouter granted scope \"{}\"; this client asked for \"{}\". The narrower grant is reused as-is.",
            credential.granted_scope(),
            crate::oauth::ORCAROUTER_SCOPE
        );
    }
    println!(
        "Revoke access any time at https://www.orcarouter.ai/console/authorized-apps. To switch accounts, run `codewhale auth orcarouter` again."
    );
    Ok(())
}

/// Clear the saved OrcaRouter credential from the secret store and config.
fn run_orcarouter_revoke(config_path: Option<&Path>) -> Result<()> {
    let mut store = codewhale_config::ConfigStore::load(config_path.map(Path::to_path_buf))?;
    let Some(secrets) = crate::config::credential_secret_store() else {
        anyhow::bail!("no credential store is available in this environment");
    };
    let provider = codewhale_config::ProviderKind::Orcarouter;
    codewhale_config::credentials::clear_provider_api_key(&mut store, &secrets, provider)?;
    println!("Removed Codewhale's saved OrcaRouter credential.");
    Ok(())
}

fn resolve_session_id(session_id: Option<String>, last: bool, workspace: &Path) -> Result<String> {
    if last {
        return latest_session_id_for_workspace(workspace)?.ok_or_else(|| {
            anyhow!(
                "No saved sessions found for workspace {}. Use `codewhale sessions` to list all sessions, or `codewhale resume <SESSION_ID>` to resume one explicitly.",
                workspace.display()
            )
        });
    }
    if let Some(id) = session_id {
        return Ok(id);
    }
    pick_session_id()
}

fn latest_session_id_for_workspace(workspace: &Path) -> std::io::Result<Option<String>> {
    let manager = SessionManager::default_location()?;
    Ok(manager
        .get_latest_session_for_workspace(workspace)?
        .map(|session| session.id))
}

/// A local mounted intent uses the same durable owner operation as HTTP. It
/// acquires only an inactive store, drains it, then lets the existing UI/session
/// leases acquire that exact binding. A live owner or a handoff race refuses;
/// no remote UI transport or parallel autosave writer is introduced here.
#[derive(Clone, Copy)]
enum MountedHistoryIntent {
    Resume,
    Fork,
}

async fn prepare_mounted_session(
    cli: &Cli,
    mut prepared: PreparedInteractiveConfig,
    selector: String,
    intent: MountedHistoryIntent,
    plugins: Arc<crate::plugins::PluginRegistry>,
) -> Result<(PreparedInteractiveConfig, String)> {
    use crate::runtime_api::thread_history::{
        history_owner_work, lookup_thread_history_operation_in_runtime,
        mutate_thread_history_in_runtime, recover_thread_history_operation_in_runtime,
        saved_document_digest, session_goal_digest,
    };
    use crate::runtime_threads::{RuntimeThreadManager, RuntimeThreadManagerConfig};
    use codewhale_protocol::{
        CanonicalHistoryOptions, CanonicalHistorySource, CanonicalThreadMutation,
        CanonicalThreadMutationRequest, CanonicalThreadOperationKind,
        CanonicalThreadOperationLookup, CanonicalThreadOperationRecovery,
        CanonicalThreadOperationStatus, MAX_CANONICAL_HISTORY_BYTES,
    };

    if let Some(key) = cli.operation_key.as_ref() {
        anyhow::ensure!(
            !key.is_empty() && key.len() <= 128 && !key.chars().any(char::is_control),
            "invalid retained canonical operation key; no mutation was attempted"
        );
    }
    let sessions_dir = crate::session_manager::default_sessions_dir()?;
    let read_dir = sessions_dir.clone();
    let selection_workspace = prepared.workspace.clone();
    let (saved, source_digest, source_goal_digest) = history_owner_work(move || {
        let manager = SessionManager::new(read_dir)?;
        let id = if selector == "latest" {
            manager
                .get_latest_session_for_workspace(&selection_workspace)?
                .ok_or_else(|| anyhow!("No saved sessions found for this workspace"))?
                .id
        } else {
            manager.resolve_session_id_prefix(&selector)?
        };
        // Capture the complete document and separate local goal under the
        // same existing inactive-session guard; neither read repairs history.
        let _source_lease = manager.reserve_session_for_external_write(&id)?;
        let saved = manager.load_session_snapshot_bounded(&id, MAX_CANONICAL_HISTORY_BYTES)?;
        // Project grants were captured for this launch before awaiting. The
        // existing restore changes App workspace, so it cannot silently apply
        // that snapshot to a different saved project. Match the same physical/
        // config equality used by workspace overlays without changing either
        // selected lexical path or merging a second project after the await.
        let launch_scope = selection_workspace.canonicalize().unwrap_or_else(|_| selection_workspace.clone());
        let saved_scope = saved.metadata.workspace.canonicalize().unwrap_or_else(|_| saved.metadata.workspace.clone());
        anyhow::ensure!(
            paths_equal_for_config(&launch_scope, &saved_scope),
            "selected session belongs to workspace {}; captured launch project differs; retry with --workspace {} before any canonical operation",
            saved.metadata.workspace.display(), saved.metadata.workspace.display(),
        );
        let digest = saved_document_digest(&saved)?;
        if let Some(binding) = saved.metadata.runtime_store.as_ref() {
            // Never let open_for_session recover a missing saved store here:
            // an empty replacement cannot prove the original durable history.
            binding.validate_existing_store()?;
        }
        let goal_digest = session_goal_digest(&manager.load_session_goal(&id)?)?;
        Ok((saved, digest, goal_digest))
    })
    .await
    .with_context(|| {
        cli.operation_key.as_ref().map_or_else(
            || "selected saved history could not be captured; no mutation attempted".to_string(),
            |key| {
                format!("retained operation {key}; source selection failed; no mutation attempted")
            },
        )
    })?;
    let source_id = saved.metadata.id.clone();
    let source_title = saved.metadata.title.clone();
    if cli.operation_key.is_none() {
        let identity = prepared
            .config
            .resolve_persisted_provider_identity(
                Some(&saved.metadata.model_provider),
                saved.metadata.model_provider_id.as_deref(),
            )
            .map_err(anyhow::Error::msg)?;
        // Same exact route projection as mounted restore; policy and credentials
        // come only from this already captured merged Config.
        let route = crate::route_runtime::resolve_runtime_route_for_identity(
            &prepared.config,
            &identity,
            Some(&saved.metadata.model),
        )
        .map_err(anyhow::Error::msg)?;
        prepared.config = *route.config;
    }
    let task_config = crate::task_manager::TaskManagerConfig::from_runtime(
        &prepared.config,
        saved.metadata.workspace.clone(),
        Some(saved.metadata.model.clone()),
        None,
    );
    let manager_config = RuntimeThreadManagerConfig::for_session(task_config.data_dir, &source_id);
    let owner_config = prepared.config.clone();
    let owner_workspace = saved.metadata.workspace.clone();
    let prior_binding = saved.metadata.runtime_store.clone();
    let recovery = cli.operation_key.is_some();
    let runtime = history_owner_work(move || {
        // Recheck the captured binding immediately before the actual factory.
        if let Some(binding) = prior_binding.as_ref() {
            binding.validate_existing_store()?;
        }
        match prior_binding.as_ref() {
            Some(binding) => RuntimeThreadManager::open_existing_session(
                owner_config,
                owner_workspace,
                manager_config,
                plugins,
                binding,
            ),
            None if recovery => RuntimeThreadManager::open_existing_session_unbound(
                owner_config,
                owner_workspace,
                manager_config,
                plugins,
            ),
            None => RuntimeThreadManager::open_for_session(
                owner_config,
                owner_workspace,
                manager_config,
                plugins,
                None,
            ),
        }
        .map(Arc::new)
    })
    .await
    .with_context(|| cli.operation_key.as_ref().map_or_else(
        || "inactive canonical owner could not be acquired; no history mutation attempted".to_string(),
        |key| format!("retained operation {key}; existing owner could not be acquired; no mutation attempted"),
    ))?;
    let binding = runtime.session_store_binding();
    let operation_key = cli
        .operation_key
        .clone()
        .unwrap_or_else(|| format!("mounted-history:{}", uuid::Uuid::new_v4()));
    let profile = effective_config_profile(cli);
    let selected_workspace = saved.metadata.workspace.clone();
    let outcome = tokio::time::timeout(Duration::from_secs(30), async {
        if recovery {
            // Lookup uses only the retained key, before a current document can
            // be proposed as a different request. This lookup only verifies
            // the historical outcome; explicit prepared recovery follows below.
            let lookup = CanonicalThreadOperationLookup {
                version: 1, operation_key: operation_key.clone(),
                expected_data_dir: binding.data_dir.clone(),
                expected_execution_scope: binding.execution_scope.clone(),
                workspace: selected_workspace.clone(),
            };
            let mut status = lookup_thread_history_operation_in_runtime(
                &runtime, &sessions_dir, lookup.clone(),
            ).await?;
            if let CanonicalThreadOperationStatus::Pending { receipt, association } = &status {
                let expected_kind = match intent {
                    MountedHistoryIntent::Resume => CanonicalThreadOperationKind::Resume,
                    MountedHistoryIntent::Fork => CanonicalThreadOperationKind::Fork,
                };
                anyhow::ensure!(
                    association.kind == expected_kind
                        && association.source_session_id.as_deref() == Some(source_id.as_str()),
                    "retained operation {operation_key} targets thread {} / session {}, but its original action/source does not match this selected intent; no recovery or automatic attach",
                    receipt.runtime_thread_id, receipt.session_id,
                );
                // An explicit retained-key invocation may finish only that
                // already-prepared owner operation. No current source/body is
                // proposed, and the owner rechecks the complete association.
                status = recover_thread_history_operation_in_runtime(
                    &runtime, &sessions_dir,
                    CanonicalThreadOperationRecovery {
                        operation: lookup, association: association.clone(),
                    },
                ).await?;
            }
            match status {
                CanonicalThreadOperationStatus::Committed { receipt, association } => {
                    let expected_kind = match intent {
                        MountedHistoryIntent::Resume => CanonicalThreadOperationKind::Resume,
                        MountedHistoryIntent::Fork => CanonicalThreadOperationKind::Fork,
                    };
                    anyhow::ensure!(
                        association.kind == expected_kind
                            && association.source_session_id.as_deref() == Some(source_id.as_str()),
                        "retained operation {operation_key} targets thread {} / session {}, but its original action/source does not match this selected intent; no automatic attach",
                        receipt.runtime_thread_id, receipt.session_id,
                    );
                    Ok(receipt)
                }
                CanonicalThreadOperationStatus::Pending { receipt, .. } => bail!(
                    "retained operation {operation_key} is pending for thread {} / session {}; outcome remains uncertain; no new intent or automatic attach",
                    receipt.runtime_thread_id, receipt.session_id,
                ),
                CanonicalThreadOperationStatus::Absent => bail!(
                    "retained operation {operation_key} is absent from this exact owner; no mutation or new identity was attempted"
                ),
            }
        } else {
            let source = CanonicalHistorySource::SavedSession {
                session: serde_json::to_value(&saved)?,
                expected_document_digest: source_digest,
            };
            let options = CanonicalHistoryOptions {
                expected_session_goal_digest: Some(source_goal_digest),
                ..Default::default()
            };
            let mutation = match intent {
                MountedHistoryIntent::Resume => CanonicalThreadMutation::Resume { source, options },
                MountedHistoryIntent::Fork => CanonicalThreadMutation::Fork { source, selected_entry_id: None, options },
            };
            mutate_thread_history_in_runtime(
                &runtime, &sessions_dir, cli.config.as_deref(), profile.as_deref(),
                CanonicalThreadMutationRequest {
                    version: 1, operation_key: operation_key.clone(),
                    expected_data_dir: binding.data_dir.clone(),
                    expected_execution_scope: binding.execution_scope.clone(),
                    workspace: selected_workspace.clone(), mutation,
                },
            ).await
        }
    }).await.context("canonical history deadline expired; outcome uncertain")
        .and_then(|result| result);
    let shutdown = tokio::time::timeout(Duration::from_secs(30), runtime.shutdown_and_wait())
        .await
        .context("canonical owner shutdown deadline expired; mounted handoff refused")
        .and_then(|result| result);
    drop(runtime);
    let receipt = match (outcome, shutdown) {
        (Ok(receipt), Ok(())) => receipt,
        (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error).with_context(|| {
            format!("canonical history operation {operation_key}; retained outcome must be recovered using this exact key")
        }),
        (Err(error), Err(shutdown)) => return Err(error).with_context(|| {
            format!("canonical history operation {operation_key}; retained outcome may be uncertain; owner handoff also refused: {shutdown:#}")
        }),
    };
    anyhow::ensure!(
        receipt.version == 1
            && receipt.operation_key == operation_key
            && receipt.data_dir == binding.data_dir
            && receipt.execution_scope == binding.execution_scope,
        "canonical history operation {operation_key}: receipt does not match the acquired owner; mounted handoff refused"
    );
    let result_id = receipt.session_id.clone();
    let verify_id = result_id.clone();
    let committed = history_owner_work(move || {
        let manager = SessionManager::new(sessions_dir)?;
        let committed =
            manager.load_session_snapshot_bounded(&verify_id, MAX_CANONICAL_HISTORY_BYTES)?;
        anyhow::ensure!(
            committed.metadata.id == verify_id
                && committed.metadata.runtime_store.as_ref() == Some(&binding)
                && committed.metadata.workspace == selected_workspace,
            "committed saved session lost its exact owner binding; mounted handoff refused"
        );
        Ok(committed)
    })
    .await
    .with_context(|| format!("canonical history operation {operation_key}; committed target handoff failed; retain this exact key"))?;
    let identity = prepared
        .config
        .resolve_persisted_provider_identity(
            Some(&committed.metadata.model_provider),
            committed.metadata.model_provider_id.as_deref(),
        )
        .map_err(anyhow::Error::msg)
        .with_context(|| {
            format!(
                "canonical history operation {operation_key}; committed route admission refused"
            )
        })?;
    let route = crate::route_runtime::resolve_runtime_route_for_identity(
        &prepared.config,
        &identity,
        Some(&committed.metadata.model),
    )
    .map_err(anyhow::Error::msg)
    .with_context(|| {
        format!("canonical history operation {operation_key}; committed route projection refused")
    })?;
    prepared.config = *route.config;
    if recovery {
        println!(
            "Recovered canonical history operation {operation_key}: thread {} / session {}",
            receipt.runtime_thread_id, result_id
        );
    } else if matches!(intent, MountedHistoryIntent::Fork) {
        let label = if source_title.trim().is_empty() {
            "session".to_string()
        } else {
            format!("\"{}\"", source_title.trim())
        };
        println!(
            "Forked {label} ({}) → new session {}",
            truncate_id(&source_id),
            truncate_id(&result_id)
        );
    }
    Ok((prepared, result_id))
}

fn pick_session_id() -> Result<String> {
    let manager = SessionManager::default_location()?;
    let sessions = manager.list_sessions()?;
    if sessions.is_empty() {
        bail!("No saved sessions found.");
    }

    println!("Select a session to resume:");
    for (idx, session) in sessions.iter().enumerate() {
        println!("  {:>2}. {} ({})", idx + 1, session.title, session.id);
    }
    print!("Enter a number (or press Enter to cancel): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();
    if input.is_empty() {
        bail!("No session selected.");
    }
    let idx: usize = input
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid input"))?;
    let session = sessions
        .get(idx.saturating_sub(1))
        .ok_or_else(|| anyhow::anyhow!("Selection out of range"))?;
    Ok(session.id.clone())
}

async fn run_review(config: &Config, args: ReviewArgs) -> Result<()> {
    initialize_cloud_facts(config);
    use crate::client::CodewhaleClient;

    // Resolved before anything is fetched or billed so an unknown
    // `--provider` fails fast with the provider vocabulary hint.
    let (config, force_configured_route) = review_execution_route(config, &args)?;
    let config = &config;
    validate_review_receipt_args(&args)?;

    if args.pr.is_some() && !is_command_available("gh") {
        bail!(
            "`gh` CLI not found on PATH. Install GitHub CLI \
             (https://cli.github.com) and authenticate (`gh auth login`) \
             so `codewhale review --pr` can fetch the pull request."
        );
    }
    // Fetched before the diff so a missing/hidden PR fails before any model
    // route is resolved or billed.
    let pr_view = match args.pr {
        Some(number) => Some((number, run_gh_pr_view(number, args.repo.as_deref())?)),
        None => None,
    };
    let diff = collect_diff(
        &args,
        pr_view.as_ref().map(|(_, view)| view),
        &std::env::current_dir()?,
    )?;
    if diff.trim().is_empty() {
        bail!("No diff to review.");
    }
    if args.check_receipt {
        return run_review_receipt_check(&diff, &args, pr_view.as_ref().map(|(_, view)| view));
    }

    let pr_plan = pr_view
        .as_ref()
        .map(|(_, view)| {
            crate::tools::review::plan_pr_review(&diff, view, args.max_chars, args.max_passes)
        })
        .transpose()?;
    let review_workspace = std::env::current_dir()?;
    // A stateless CLI review has no Engine session or Native caller to invent.
    let review_context =
        crate::tools::spec::ToolContext::new(&review_workspace).with_features(config.features());
    // #6510: one review prompt authority. Plain diffs and PRs both ask for
    // the structured review contract; a plain diff renders it locally.
    let system = SystemPrompt::Text(crate::tools::review::review_system_prompt().to_string());
    let prompts = if let (Some((number, view)), Some(plan)) = (&pr_view, &pr_plan) {
        crate::tools::review_host::pr_prompts(*number, view, plan, &review_context).await?
    } else {
        vec![if review_context
            .features
            .enabled(crate::features::Feature::ReviewHost)
        {
            crate::tools::review_host::cli_prompt(&diff, &review_context).await?
        } else {
            format!("Review the following diff and provide feedback:\n\n{diff}\n\nEnd of diff.")
        }]
    };
    let model = resolve_review_model(config, args.model.as_deref());
    let route_input = prompts
        .iter()
        .max_by_key(|prompt| prompt.chars().count())
        .expect("review has at least one prompt");
    let route = resolve_cli_exec_route(config, &model, route_input, force_configured_route).await?;
    let execution_config = config_for_cli_route(config, &route)?;
    let route_provider = execution_config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?
        .key
        .to_string();
    let model = route.model.clone();
    let client = CodewhaleClient::new(&execution_config)?;
    let request_route = client.effective_route_envelope(&model, chrono::Utc::now());
    let planned_passes = prompts.len();
    let mut usage = codewhale_models::Usage::default();
    let mut publication = if args.post {
        ReviewPublication::NotAttempted
    } else {
        ReviewPublication::NotRequested
    };
    let report_failure =
        |usage: &codewhale_models::Usage, completed_passes, publication, message| {
            report_review_failure(
                &args,
                &route_provider,
                &model,
                usage,
                completed_passes,
                planned_passes,
                publication,
                message,
            )
        };
    let mut accumulator = pr_plan
        .as_ref()
        .map(crate::tools::review::PrReviewAccumulator::new);
    // Visible-text reserve for this exact route/model (#6285). A reasoning route
    // shares one `max_tokens` allowance between hidden reasoning and visible
    // text, so the review request has to keep room for the review itself.
    let review_allowance = client.effective_max_output_tokens(&request_route.model);
    let review_reserve_percent =
        crate::route_budget::review_visible_text_reserve_percent(&request_route.model);
    let review_reserve_tokens = crate::route_budget::review_visible_text_reserve_tokens(
        &request_route.model,
        review_allowance,
    );
    let mut output = String::new();
    let mut review_stop_reason = None;
    for (index, user_prompt) in prompts.into_iter().enumerate() {
        let reasoning_effort = route.reasoning_effort.and_then(|effort| {
            review_reasoning_effort_value_for_prompt(
                &execution_config,
                &model,
                effort,
                review_reserve_percent,
            )
        });
        let request = MessageRequest {
            model: model.clone(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: user_prompt,
                    cache_control: None,
                }],
            }],
            max_tokens: client.effective_max_output_tokens(&request_route.model),
            system: Some(system.clone()),
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort,
            stream: Some(false),
            temperature: None,
            top_p: None,
        };
        let response = match client.create_message(request).await {
            Ok(response) => response,
            Err(error) => {
                return report_failure(
                    &usage,
                    index,
                    publication,
                    format!(
                        "{}; no partial review was accepted or posted",
                        crate::tools::review::request_failure_message(
                            index + 1,
                            planned_passes,
                            &error
                        )
                    ),
                );
            }
        };
        crate::tools::review::add_review_usage(&mut usage, &response.usage);
        review_stop_reason = response.stop_reason.clone();
        if codewhale_models::is_incomplete_stop_reason(review_stop_reason.as_deref()) {
            // #6285: budget exhaustion is an infrastructure outcome, not a
            // review verdict. The PR was never judged, so the message must not
            // read as findings and must name what was and was not reviewed.
            let reasoning_tokens = usage
                .reasoning_tokens
                .map_or_else(|| "unknown".to_string(), |tokens| tokens.to_string());
            let reserve_clause = if review_reserve_percent == 0 {
                "no reasoning reserve was needed for this model".to_string()
            } else {
                "the pass's reasoning level was capped to fit that reserve".to_string()
            };
            return report_failure(
                &usage,
                index,
                publication,
                format!(
                    "Review pass {}/{} exhausted its output allowance and produced no review text. This is an infrastructure/budget outcome, not a review result: the provider stopped with reason `{}` after reporting {} of {} requested output tokens as reasoning; this model's review reserve is {} tokens ({review_reserve_percent}% of the allowance) and {reserve_clause}. Coverage: {}. Reviewed so far: {} of {} planned pass(es). The partial review was not accepted or posted.",
                    index + 1,
                    planned_passes,
                    codewhale_models::stop_reason_detail(review_stop_reason.as_deref()),
                    reasoning_tokens,
                    review_allowance,
                    review_reserve_tokens,
                    pr_review_unreviewed_note(pr_plan.as_ref(), index),
                    index,
                    planned_passes,
                ),
            );
        }
        let mut pass_output = String::new();
        for block in response.content {
            if let ContentBlock::Text { text, .. } = block {
                pass_output.push_str(&text);
            }
        }
        if let (Some(plan), Some(accumulator)) = (&pr_plan, accumulator.as_mut()) {
            if let Err(error) = accumulator.accept(&plan.passes[index], pass_output) {
                return report_failure(&usage, index, publication, error.to_string());
            }
        } else {
            output = pass_output;
        }
    }
    let (structured, coverage) = if let Some(accumulator) = accumulator {
        let (review, content, coverage) = match accumulator.finish(&diff) {
            Ok(complete) => complete,
            Err(error) => {
                return report_failure(&usage, planned_passes, publication, error.to_string());
            }
        };
        output = content;
        (Some(review), Some(coverage))
    } else {
        // A plain diff is structured when the model kept the JSON contract;
        // otherwise its prose is the report, exactly as before.
        (
            crate::tools::review::ReviewOutput::from_structured_str(&output),
            None,
        )
    };
    // Presentation is prepared before publication. Any peer failure retains the
    // completed provider usage and cannot cause a partial post or Rust fallback.
    let rendered_report = if args.post || !args.json {
        let posted = args
            .post
            .then(|| pr_view.as_ref().expect("post requires PR"))
            .map(|(number, view)| (*number, view));
        match render_review_report_for_context(
            structured.as_ref(),
            &output,
            posted,
            &review_context,
        )
        .await
        {
            Ok(body) => Some(body),
            Err(error) => {
                return report_failure(&usage, planned_passes, publication, error.to_string());
            }
        }
    } else {
        None
    };
    let finalized = (|| -> Result<_> {
        if let Some((number, view)) = &pr_view {
            let cwd = std::env::current_dir().context(
                "Failed to resolve the current directory before final PR revision check",
            )?;
            crate::tools::review_pr::ensure_current(*number, args.repo.as_deref(), &cwd, view)?;
        }
        if args.post {
            let (number, view) = pr_view
                .as_ref()
                .expect("--post requires --pr (enforced by clap)");
            let review = structured
                .as_ref()
                .expect("structured output exists for PR reviews");
            post_pr_review(
                *number,
                view,
                args.repo.as_deref(),
                review,
                &diff,
                rendered_report
                    .as_deref()
                    .expect("posted review has prepared body"),
                &mut publication,
            )
            .context("PR review publication failed")?;
        }
        let receipt = if args.write_receipt {
            let parsed_output = structured
                .clone()
                .unwrap_or_else(|| crate::tools::review::ReviewOutput::from_str(&output));
            let mut receipt = crate::tools::review::build_review_receipt(
                review_target_label(&args),
                &diff,
                &route_provider,
                &model,
                &parsed_output,
                &output,
                Vec::new(),
            );
            if let Some(coverage) = coverage {
                crate::tools::review::attach_pr_review_coverage(&mut receipt, coverage)
                    .context("Failed to attach PR coverage to review receipt")?;
            }
            let path =
                crate::tools::review::write_review_receipt(&receipt, args.receipt_path.as_deref())
                    .context("Failed to write review receipt")?;
            Some((path, receipt))
        } else {
            None
        };
        Ok(receipt)
    })();
    let receipt = match finalized {
        Ok(receipt) => receipt,
        Err(error) => {
            return report_failure(&usage, planned_passes, publication, error.to_string());
        }
    };
    if args.json {
        let payload = serde_json::json!({
            "mode": "review",
            "provider": route_provider,
            "model": model,
            "success": true,
            "publication": publication.as_str(),
            "content": output,
            "pr": pr_view.as_ref().map(|(number, view)| serde_json::json!({
                "number": number,
                "url": view.url,
                "title": view.title,
                "head_sha": view.head_sha,
            })),
            "review": structured,
            "stop_reason": review_stop_reason,
            "usage": usage,
            "review_passes": pr_plan.as_ref().map(|plan| plan.passes.len()),
            "complete": pr_plan
                .as_ref()
                .is_none_or(|plan| plan.manifest.skipped_files.is_empty()),
            "skipped_files": pr_plan.as_ref().map(|plan| &plan.manifest.skipped_files),
            "receipt_path": receipt
                .as_ref()
                .map(|(path, _)| path.display().to_string()),
            "receipt": receipt.as_ref().map(|(_, receipt)| receipt),
        });
        let payload = match serde_json::to_string_pretty(&payload) {
            Ok(payload) => payload,
            Err(error) => {
                return report_failure(
                    &usage,
                    planned_passes,
                    publication,
                    format!("Failed to serialize completed review output: {error}"),
                );
            }
        };
        println!("{payload}");
    } else if pr_view.is_some() {
        println!(
            "{}",
            rendered_report
                .as_deref()
                .expect("local PR review has prepared report")
        );
        if let Some((path, _)) = receipt {
            eprintln!("Review receipt written: {}", path.display());
        }
    } else {
        println!(
            "{}",
            rendered_report
                .as_deref()
                .expect("plain review has prepared report")
        );
        if let Some((path, _)) = receipt {
            eprintln!("Review receipt written: {}", path.display());
        }
    }
    if let Some(plan) = &pr_plan
        && !plan.manifest.skipped_files.is_empty()
    {
        // #6285 AC3: the partial review above is real findings, already
        // printed and posted — but the gate must not pass on unread
        // files. The exit still fails, naming what the gate did not read
        // and the remedy, so it reads as limits rather than as "this PR
        // failed review".
        bail!(
            "Partial PR review: {} pass(es) completed, but the gate did not read: {}. Raise --max-chars/--max-passes or shrink the PR, then re-run; publication: {}",
            plan.passes.len(),
            crate::tools::review::format_skipped_files(&plan.manifest.skipped_files),
            publication.as_str()
        );
    }
    Ok(())
}

/// Criterion 4 (no silent caps): whatever a budget stop leaves unread is
/// named by file, never silently dropped — including files the plan itself
/// skipped before the first pass ran (#6285 AC4). A free function so tests
/// can pin the note without running a review.
fn pr_review_unreviewed_note(
    plan: Option<&crate::tools::review::PrReviewPlan>,
    completed_passes: usize,
) -> String {
    let Some(plan) = plan else {
        return "the entire diff".to_string();
    };
    let mut unread: Vec<String> = Vec::new();
    for pass in plan.manifest.passes.iter().skip(completed_passes) {
        for file in &pass.files {
            if !unread.iter().any(|seen| seen == file) {
                unread.push(file.clone());
            }
        }
    }
    let mut clauses = Vec::new();
    if !unread.is_empty() {
        clauses.push(format!(
            "{} file(s) were never read: {}",
            unread.len(),
            unread.join(", ")
        ));
    }
    if !plan.manifest.skipped_files.is_empty() {
        clauses.push(format!(
            "the plan never scheduled: {}",
            crate::tools::review::format_skipped_files(&plan.manifest.skipped_files)
        ));
    }
    if clauses.is_empty() {
        "no planned file was left unread".to_string()
    } else {
        clauses.join("; ")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReviewPublication {
    NotRequested,
    NotAttempted,
    Uncertain,
    Posted,
}

impl ReviewPublication {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
            Self::NotAttempted => "not_attempted",
            Self::Uncertain => "uncertain",
            Self::Posted => "posted",
        }
    }
}

fn review_failure_payload(
    provider: &str,
    model: &str,
    usage: &codewhale_models::Usage,
    completed_passes: usize,
    planned_passes: usize,
    publication: ReviewPublication,
    message: &str,
) -> serde_json::Value {
    serde_json::json!({
        "mode": "review",
        "provider": provider,
        "model": model,
        "success": false,
        "complete": false,
        // #6285: a run that never produced findings is an infrastructure or
        // budget outcome. Keeping it explicit in the payload stops CI and
        // operators from reading `success: false` as "this PR failed review".
        "outcome": "infrastructure",
        "review_verdict": "not_produced",
        "publication": publication.as_str(),
        "error": message,
        "usage": usage,
        "completed_review_passes": completed_passes,
        "planned_review_passes": planned_passes,
    })
}

fn report_review_failure(
    args: &ReviewArgs,
    provider: &str,
    model: &str,
    usage: &codewhale_models::Usage,
    completed_passes: usize,
    planned_passes: usize,
    publication: ReviewPublication,
    message: impl Into<String>,
) -> Result<()> {
    let message = message.into();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&review_failure_payload(
                provider,
                model,
                usage,
                completed_passes,
                planned_passes,
                publication,
                &message,
            ))?
        );
    }
    let usage = serde_json::to_string(usage)?;
    let publication = publication.as_str();
    bail!(
        "{message}; publication: {publication}; completed review passes: {completed_passes}/{planned_passes}; accumulated usage: {usage}"
    )
}

/// Apply `codewhale review --provider <name>` and decide whether the route is
/// authoritative (no cross-provider inventory inference).
///
/// This mirrors `codewhale exec --provider` (#4093): the flag sets ONLY the
/// non-secret provider identity, and pinning the route is what lets a model
/// offered by more than one configured route resolve instead of hard-erroring
/// in `resolve_cli_auto_route`.
fn review_execution_route(config: &Config, args: &ReviewArgs) -> Result<(Config, bool)> {
    let explicit_provider = non_empty_flag(args.provider.as_deref());
    let explicit_model = non_empty_flag(args.model.as_deref());
    let mut resolved = config.clone();
    if let Some(provider_arg) = explicit_provider {
        apply_exec_provider_override(&mut resolved, provider_arg)?;
    }
    let force_configured_route =
        should_force_configured_exec_route(false, explicit_provider, explicit_model);
    Ok((resolved, force_configured_route))
}

fn non_empty_flag(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn resolve_review_model(config: &Config, explicit_model: Option<&str>) -> String {
    explicit_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| config.default_model())
}

fn validate_review_receipt_args(args: &ReviewArgs) -> Result<()> {
    if args.receipt_path.is_some() && !args.write_receipt && !args.check_receipt {
        bail!("--receipt-path requires --write-receipt or --check-receipt");
    }
    if args.write_receipt && args.check_receipt {
        bail!("--write-receipt and --check-receipt are mutually exclusive");
    }
    if args.pr.is_none() && args.max_passes != 1 {
        bail!("--max-passes applies only to --pr reviews");
    }
    if !(1..=crate::tools::review::MAX_REVIEW_PASSES).contains(&args.max_passes) {
        bail!(
            "--max-passes must be from 1 to {}",
            crate::tools::review::MAX_REVIEW_PASSES
        );
    }
    Ok(())
}

fn run_review_receipt_check(
    diff: &str,
    args: &ReviewArgs,
    pr_view: Option<&GhPullRequest>,
) -> Result<()> {
    let (path, receipt) = if let Some(path) = args.receipt_path.as_ref() {
        (
            path.clone(),
            crate::tools::review::read_review_receipt(path)
                .with_context(|| format!("failed to read review receipt {}", path.display()))?,
        )
    } else {
        crate::tools::review::latest_review_receipt_for_diff(diff)?.ok_or_else(|| {
            anyhow!(
                "No review receipt found for the current diff. Run `codewhale review --write-receipt` first, or pass --receipt-path."
            )
        })?
    };
    let mut validation =
        crate::tools::review::validate_review_receipt_for_diff(diff, &receipt, Some(path.clone()));
    if validation.passed
        && pr_view
            .is_some_and(|view| !crate::tools::review::receipt_matches_pr_revision(&receipt, view))
    {
        validation.passed = false;
        validation.reason =
            "review receipt does not match the current PR base/head revision".into();
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "mode": "review_receipt_check",
                "success": validation.passed,
                "validation": review_receipt_validation_public_json(&validation),
            }))?
        );
    } else if validation.passed {
        println!("Review receipt valid: {}", path.display());
    }

    if !validation.passed {
        bail!("Review receipt check failed: {}", validation.reason);
    }
    Ok(())
}

fn review_receipt_validation_public_json(
    validation: &crate::tools::review::ReviewReceiptValidation,
) -> serde_json::Value {
    let unresolved_risk = validation.unresolved_risk.as_ref();
    serde_json::json!({
        "passed": validation.passed,
        "status": review_receipt_validation_status(validation),
        "diff_fingerprint": validation.diff_fingerprint.as_str(),
        "receipt_fingerprint": validation.receipt_fingerprint.as_deref(),
        "unresolved": unresolved_risk.is_some_and(|risk| risk.unresolved),
        "risk_level": unresolved_risk.map(|risk| risk.level.as_str()),
    })
}

fn review_receipt_validation_status(
    validation: &crate::tools::review::ReviewReceiptValidation,
) -> &'static str {
    if validation.passed {
        "valid"
    } else if validation
        .receipt_fingerprint
        .as_deref()
        .is_some_and(|fingerprint| fingerprint != validation.diff_fingerprint.as_str())
    {
        "diff_mismatch"
    } else if validation
        .unresolved_risk
        .as_ref()
        .is_some_and(|risk| risk.unresolved)
    {
        "unresolved_risk"
    } else if validation
        .reason
        .starts_with("unsupported review receipt schema version")
    {
        "unsupported_schema"
    } else if validation.reason.starts_with("review receipt check ") {
        "check_failed"
    } else {
        "invalid"
    }
}

/// `codewhale pr <N>` (#451) — fetch a GitHub PR via `gh`, format
/// title + body + diff as the composer's first message, and launch
/// the interactive TUI. Falls back gracefully if `gh` is missing.
async fn run_pr(
    cli: &Cli,
    config: &Config,
    number: u32,
    repo: Option<&str>,
    checkout: bool,
    pending_telemetry_notice: Option<crate::telemetry_notice::PendingTelemetryNotice>,
    plugin_registry: Arc<crate::plugins::PluginRegistry>,
) -> Result<()> {
    if !is_command_available("gh") {
        bail!(
            "`gh` CLI not found on PATH. Install GitHub CLI \
             (https://cli.github.com) and authenticate (`gh auth login`) \
             so `codewhale pr <N>` can fetch PR metadata and the diff."
        );
    }

    let view = run_gh_pr_view(number, repo)?;
    let diff = run_gh_pr_diff(number, repo, &view)?;

    if checkout {
        match run_gh_pr_checkout(number, repo) {
            Ok(()) => eprintln!("Checked out PR #{number} into the current workspace."),
            Err(err) => eprintln!(
                "warning: gh pr checkout #{number} failed ({err}). Continuing without checkout."
            ),
        }
    }

    let context = crate::tools::spec::ToolContext::new(std::env::current_dir()?)
        .with_features(config.features());
    let prompt = if context
        .features
        .enabled(crate::features::Feature::ReviewHost)
    {
        crate::tools::review_host::interactive(number, &view, &diff, &context).await?
    } else {
        format_pr_prompt(number, &view, &diff)
    };
    let resume_session_id = if cli.continue_session {
        let workspace = resolve_workspace(cli);
        latest_session_id_for_workspace(&workspace).ok().flatten()
    } else {
        cli.resume.clone()
    };
    run_interactive(
        cli,
        config,
        resume_session_id,
        Some(tui::InitialInput::Prefill(prompt)),
        pending_telemetry_notice,
        plugin_registry,
    )
    .await
}

/// Return true if `name` resolves to an executable on the current `PATH`.
///
/// Walks `$PATH` directly instead of probing with `--version`. The
/// previous implementation invoked `Command::new(name).arg("--version")`,
/// which fails on the Ubuntu CI runner because `/bin/sh` is `dash` —
/// `dash --version` exits with status 2 ("invalid option") even though
/// `sh` is plainly on PATH. macOS happens to ship bash as `sh`, which
/// does honor `--version`, so the bug was invisible locally and only
/// surfaced in CI logs.
///
/// Windows: also checks the `.exe` extension when `name` doesn't have
/// one, matching the platform's PATHEXT lookup behavior for the common
/// case.
fn is_command_available(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            // PATHEXT gives `.exe`/`.cmd`/`.bat` etc. priority — we only
            // probe `.exe` because that's the case that actually trips
            // up the negative case (`gh` resolves as `gh.exe`).
            if candidate.extension().is_none() && candidate.with_extension("exe").is_file() {
                return true;
            }
        }
    }
    false
}

use crate::tools::review_pr::GhPullRequest;

fn run_gh_pr_view(number: u32, repo: Option<&str>) -> Result<GhPullRequest> {
    crate::tools::review_pr::fetch_view(number, repo, &std::env::current_dir()?)
}

fn run_gh_pr_diff(number: u32, repo: Option<&str>, view: &GhPullRequest) -> Result<String> {
    crate::tools::review_pr::fetch_diff(number, repo, &std::env::current_dir()?, view)
}

fn run_gh_pr_checkout(number: u32, repo: Option<&str>) -> Result<()> {
    let mut cmd = crate::dependencies::Gh::command()
        .ok_or_else(|| anyhow::anyhow!("gh not found on PATH"))?;
    cmd.arg("pr").arg("checkout").arg(number.to_string());
    if let Some(r) = repo {
        cmd.arg("--repo").arg(r);
    }
    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run `gh pr checkout`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("gh pr checkout #{number} failed: {stderr}");
    }
    Ok(())
}

/// Resolve the `owner/name` repository `gh` believes the current workspace
/// belongs to. `gh api` needs an explicit repository path, unlike `gh pr`
/// which infers it from the working directory.
fn run_gh_repo_name() -> Result<String> {
    let mut cmd = crate::dependencies::Gh::command()
        .ok_or_else(|| anyhow::anyhow!("gh not found on PATH"))?;
    cmd.arg("repo")
        .arg("view")
        .arg("--json")
        .arg("nameWithOwner")
        .arg("--jq")
        .arg(".nameWithOwner");
    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run `gh repo view`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("gh repo view failed: {stderr}");
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if name.is_empty() {
        bail!("`gh repo view` returned an empty repository name");
    }
    Ok(name)
}

/// Pick a fence long enough to wrap `replacement` without the replacement's
/// own backticks closing the block early.
fn suggestion_fence(replacement: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for ch in replacement.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.saturating_add(1).max(3))
}

/// GitHub turns any fenced `suggestion` block in a review comment into a
/// one-click commit. Model prose is not vetted for that, so a fence the model
/// wrote inside its own explanation is downgraded to a plain code block:
/// only the `replacement` this function validated against the diff hunks may
/// ever be committable.
fn neutralize_model_suggestion_fences(prose: &str) -> String {
    prose
        .split('\n')
        .map(neutralize_suggestion_fence_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Neutralize one line's fence if it opens a `suggestion` block.
///
/// The fence may sit behind leading whitespace or behind a blockquote cue or
/// list marker (`- ```suggestion`, `> ```suggestion`, `1. ```suggestion`).
/// Whether GitHub renders those container-nested blocks applicable is
/// unverified, so all of those shapes are treated as live and rewritten.
fn neutralize_suggestion_fence_line(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];
    let (prefix, fence) = split_container_prefix(trimmed);
    let fence_char = match fence.chars().next() {
        Some(ch @ ('`' | '~')) => ch,
        _ => return line.to_string(),
    };
    let ticks = fence.chars().take_while(|ch| *ch == fence_char).count();
    if ticks < 3 {
        return line.to_string();
    }
    let info = fence[ticks..].trim();
    if info.to_ascii_lowercase().starts_with("suggestion") {
        let fence: String = std::iter::repeat_n(fence_char, ticks).collect();
        format!("{indent}{prefix}{fence}text")
    } else {
        line.to_string()
    }
}

/// Split the blockquote cues and list markers off the front of a line,
/// returning `(prefix_to_preserve, rest)`. Nesting is followed (`> - `),
/// but a line of ordinary prose is left untouched — the prefix only matters
/// when what follows it is a fence.
fn split_container_prefix(trimmed: &str) -> (&str, &str) {
    let mut rest = trimmed;
    while let Some(after) = strip_container_token(rest) {
        rest = after;
    }
    let split = trimmed.len() - rest.len();
    trimmed.split_at(split)
}

/// Strip one container token (`> ` blockquote cue, `-`/`*`/`+` bullet, or an
/// ordered-list marker) plus its trailing whitespace, or `None` when the
/// line does not start with one.
fn strip_container_token(rest: &str) -> Option<&str> {
    if let Some(after) = rest.strip_prefix('>') {
        return Some(after.trim_start_matches([' ', '\t']));
    }
    if let Some(after) = rest
        .strip_prefix(['-', '*', '+'])
        .filter(|after| after.starts_with([' ', '\t']))
    {
        return Some(after.trim_start_matches([' ', '\t']));
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        let after_marker = &rest[digits..];
        if let Some(after) = after_marker
            .strip_prefix(['.', ')'])
            .filter(|after| after.starts_with([' ', '\t']))
        {
            return Some(after.trim_start_matches([' ', '\t']));
        }
    }
    None
}

/// Map structured review issues to GitHub inline-review-comment payloads.
///
/// Anchors are checked against the diff's actual hunks, not just its file
/// lists: GitHub 422s the *entire* review when one comment lands outside a
/// hunk, so a model-estimated line that misses now drops a single comment.
/// Issues without a locatable position stay in the summary body instead.
///
/// SAFETY: `title` and `description` are raw model text interpolated into a
/// comment body. A model-written ```suggestion fence there would become a
/// one-click-mergeable block that bypasses every span and size check, so
/// both fields pass through `neutralize_model_suggestion_fences` — only a
/// replacement validated against the diff hunks may ever be committable.
/// (`severity` is safe unneutralized: it is normalized to a fixed
/// error/warning/info vocabulary before it reaches here.)
fn inline_issue_comments(
    review: &crate::tools::review::ReviewOutput,
    hunks: &crate::tools::review_hunks::DiffHunks,
    plan: &mut InlineReviewPlan,
) -> Vec<serde_json::Value> {
    review
        .issues
        .iter()
        .filter_map(|issue| {
            let (Some(path), Some(line)) = (
                crate::tools::review::normalize_review_path(issue.path.as_deref()),
                issue.line,
            ) else {
                // No position at all: the summary body is the only home.
                return None;
            };
            if !hunks.contains_line(&path, line) {
                plan.note_unanchorable(hunks, &path);
                return None;
            }
            Some(serde_json::json!({
                "path": path,
                "line": line,
                "side": "RIGHT",
                "body": format!(
                    "**[{}] {}**\n\n{}",
                    issue.severity.to_uppercase(),
                    neutralize_model_suggestion_fences(&issue.title),
                    neutralize_model_suggestion_fences(&issue.description)
                ),
            }))
        })
        .collect()
}

/// Render one structured suggestion as an inline review comment.
///
/// A committable ```` ```suggestion ```` block is emitted only when **both**
/// safety conditions hold:
///
/// 1. the model supplied literal `replacement` code (not prose), and
/// 2. every line of the replaced span is a RIGHT-side line inside a diff hunk
///    (never a deleted LEFT-side line, which GitHub rejects), and the span is
///    small enough to be a mechanical fix.
///
/// Otherwise the comment degrades to prose at the same anchor — a wrong
/// committable suggestion is worse than prose because it is one click from
/// being merged. With no valid anchor at all the suggestion stays in the
/// summary body and `None` is returned.
fn inline_suggestion_comment(
    suggestion: &crate::tools::review::ReviewSuggestion,
    hunks: &crate::tools::review_hunks::DiffHunks,
    plan: &mut InlineReviewPlan,
) -> Option<serde_json::Value> {
    // The committable decision lives in `resolve_suggestion_anchor` so the
    // review receipt records exactly what this path emits for the same diff.
    let (path, start, end, committable) =
        match crate::tools::review::resolve_suggestion_anchor(suggestion, hunks) {
            crate::tools::review::SuggestionAnchor::Anchored {
                path,
                start,
                end,
                committable,
            } => (path, start, end, committable),
            crate::tools::review::SuggestionAnchor::Unanchorable { path } => {
                plan.note_unanchorable(hunks, &path);
                return None;
            }
            crate::tools::review::SuggestionAnchor::NoPosition => return None,
        };
    let prose = if suggestion.suggestion.is_empty() {
        "Suggested change.".to_string()
    } else {
        neutralize_model_suggestion_fences(&suggestion.suggestion)
    };

    let Some(replacement) = suggestion.replacement.as_deref().filter(|_| committable) else {
        // Degradation path: keep the finding, drop the one-click apply.
        plan.degraded_to_prose += 1;
        return Some(serde_json::json!({
            "path": path,
            "line": end,
            "side": "RIGHT",
            "body": prose,
        }));
    };

    let fence = suggestion_fence(replacement);
    let body = format!("{prose}\n\n{fence}suggestion\n{replacement}\n{fence}");
    let mut comment = serde_json::json!({
        "path": path,
        "line": end,
        "side": "RIGHT",
        "body": body,
    });
    if start < end {
        comment["start_line"] = serde_json::json!(start);
        comment["start_side"] = serde_json::json!("RIGHT");
    }
    Some(comment)
}

/// The inline-comment payload for one review, plus a truthful count of what
/// did not survive anchoring. Findings are never silently discarded: the
/// counts are reported on stderr next to the posted review.
#[derive(Debug, Default)]
struct InlineReviewPlan {
    comments: Vec<serde_json::Value>,
    /// Findings whose file is in the diff but whose line is not inside any
    /// hunk — a model-estimated line number that missed.
    dropped_out_of_hunk: usize,
    /// Findings pointing at a file this diff does not touch at all.
    dropped_untouched_file: usize,
    /// Suggestions posted as prose because committing them was not safe.
    degraded_to_prose: usize,
}

impl InlineReviewPlan {
    fn note_unanchorable(&mut self, hunks: &crate::tools::review_hunks::DiffHunks, path: &str) {
        if hunks.touches_path(path) {
            self.dropped_out_of_hunk += 1;
        } else {
            self.dropped_untouched_file += 1;
        }
    }

    /// One-line receipt, or `None` when every finding landed as intended.
    fn receipt(&self) -> Option<String> {
        if self.dropped_out_of_hunk == 0
            && self.dropped_untouched_file == 0
            && self.degraded_to_prose == 0
        {
            return None;
        }
        Some(format!(
            "note: {} finding(s) had no line inside a diff hunk, {} pointed at a file \
             outside the diff (both stay in the summary body), and {} suggestion(s) \
             posted as prose instead of a committable block",
            self.dropped_out_of_hunk, self.dropped_untouched_file, self.degraded_to_prose
        ))
    }
}

/// Every inline comment for a review: issues first, then suggestions (which
/// carry the committable ```` ```suggestion ```` blocks).
fn plan_inline_review_comments(
    review: &crate::tools::review::ReviewOutput,
    diff: &str,
) -> InlineReviewPlan {
    let hunks = crate::tools::review_hunks::DiffHunks::parse(diff);
    let mut plan = InlineReviewPlan::default();
    let mut comments = inline_issue_comments(review, &hunks, &mut plan);
    comments.extend(
        review
            .suggestions
            .iter()
            .filter_map(|suggestion| inline_suggestion_comment(suggestion, &hunks, &mut plan)),
    );
    plan.comments = comments;
    plan
}

/// The local report for a plain-diff review (#6510). The diff is reviewed
/// under the one structured review prompt; when the model kept the JSON
/// contract the report is rendered Markdown, otherwise its prose is printed
/// verbatim, as before.
fn plain_diff_review_report(
    structured: Option<&crate::tools::review::ReviewOutput>,
    output: &str,
) -> String {
    structured.map_or_else(
        || output.to_string(),
        |review| render_review_markdown(review, None),
    )
}

/// Render a structured review as Markdown. `posted` names the PR the body is
/// being published to; `None` is the local report (plain diffs and unposted
/// PR reviews), where suggestion fences stay live.
fn render_review_markdown(
    review: &crate::tools::review::ReviewOutput,
    posted: Option<(u32, &GhPullRequest)>,
) -> String {
    let mut body = String::new();
    body.push_str("## Codewhale review\n\n");
    if !review.summary.is_empty() {
        body.push_str(review.summary.trim());
        body.push_str("\n\n");
    }
    if !review.issues.is_empty() {
        body.push_str("### Findings\n\n");
        for issue in &review.issues {
            let location = match (&issue.path, issue.line) {
                (Some(path), Some(line)) => format!("`{}:{line}`", path.trim()),
                (Some(path), None) => format!("`{}`", path.trim()),
                _ => String::new(),
            };
            if location.is_empty() {
                body.push_str(&format!(
                    "- **[{}] {}**\n",
                    issue.severity.to_uppercase(),
                    issue.title
                ));
            } else {
                body.push_str(&format!(
                    "- **[{}] {}** ({location})\n",
                    issue.severity.to_uppercase(),
                    issue.title
                ));
            }
            if !issue.description.is_empty() {
                body.push_str(&format!("  {}\n", issue.description));
            }
        }
        body.push('\n');
    }
    if !review.suggestions.is_empty() {
        body.push_str("### Suggestions\n\n");
        for suggestion in &review.suggestions {
            let location = match (&suggestion.path, suggestion.line) {
                (Some(path), Some(line)) => format!("`{}:{line}`", path.trim()),
                (Some(path), None) => format!("`{}`", path.trim()),
                _ => String::new(),
            };
            if location.is_empty() {
                body.push_str(&format!("- {}\n", suggestion.suggestion));
            } else {
                body.push_str(&format!("- {location} — {}\n", suggestion.suggestion));
            }
            // The replacement is the computed fix itself; rendering only the
            // prose used to compute and validate it, then throw it away.
            // Show it as a fenced block indented into the list item. In the
            // local report the fence is live — the user asked for the fix and
            // this block is the artifact to apply. In a *posted* PR body it
            // degrades to a plain code block: GitHub must only ever be handed
            // a one-click suggestion block this pipeline validated against
            // the diff hunks (the inline suggestion comments), never one the
            // summary duplicated from a suggestion that failed anchoring or
            // the span gates.
            if let Some(replacement) = suggestion
                .replacement
                .as_deref()
                .filter(|replacement| !replacement.trim().is_empty())
            {
                let fence = suggestion_fence(replacement);
                let info = if posted.is_some() {
                    "text"
                } else {
                    "suggestion"
                };
                body.push_str(&format!("\n  {fence}{info}\n"));
                for line in replacement.split('\n') {
                    body.push_str(&format!("  {line}\n"));
                }
                body.push_str(&format!("  {fence}\n"));
            }
        }
        body.push('\n');
    }
    if !review.overall_assessment.is_empty() {
        body.push_str("### Assessment\n\n");
        body.push_str(review.overall_assessment.trim());
        body.push_str("\n\n");
    }
    if let Some((number, view)) = posted {
        body.push_str(&review_advisory_footer(number, view));
    }
    body
}

async fn render_review_report_for_context(
    review: Option<&crate::tools::review::ReviewOutput>,
    output: &str,
    posted: Option<(u32, &GhPullRequest)>,
    context: &crate::tools::spec::ToolContext,
) -> Result<String> {
    if !context
        .features
        .enabled(crate::features::Feature::ReviewHost)
    {
        return Ok(match posted {
            Some(posted) => review.map_or_else(
                || output.to_string(),
                |review| render_review_markdown(review, Some(posted)),
            ),
            None => plain_diff_review_report(review, output),
        });
    }
    let mut body =
        crate::tools::review_host::report(review, output, posted.is_some(), context).await?;
    if let Some((number, view)) = posted {
        body.push_str(&review_advisory_footer(number, view));
    }
    Ok(body)
}

fn review_advisory_footer(number: u32, view: &GhPullRequest) -> String {
    format!(
        "---\n*Advisory review by Codewhale (`codewhale review --pr {number} --post`, \
             head `{head}`). Line-specific findings are also posted as inline review \
             comments; mechanical fixes arrive as committable suggestions you can \
             apply from the Files tab. CODEOWNERS approval still governs merge.*\n",
        head = if view.head_sha.is_empty() {
            "unknown"
        } else {
            view.head_sha.as_str()
        }
    )
}

/// Post one COMMENT review: summary body plus inline comments anchored to the
/// PR head SHA. Never approves or requests changes — the review is advisory
/// and posts alongside CODEOWNERS, like `claude-review.yml`.
fn run_gh_post_pr_review(
    repo: &str,
    number: u32,
    body: &str,
    commit_id: &str,
    comments: &[serde_json::Value],
    publication: &mut ReviewPublication,
) -> Result<()> {
    let mut payload = serde_json::json!({
        "body": body,
        "event": "COMMENT",
    });
    if !commit_id.is_empty() {
        payload["commit_id"] = serde_json::json!(commit_id);
    }
    if !comments.is_empty() {
        payload["comments"] = serde_json::json!(comments);
    }
    let mut cmd = crate::dependencies::Gh::command()
        .ok_or_else(|| anyhow::anyhow!("gh not found on PATH"))?;
    cmd.arg("api")
        .arg("--method")
        .arg("POST")
        .arg(format!("repos/{repo}/pulls/{number}/reviews"))
        .arg("--input")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to run `gh api`: {e}"))?;
    *publication = ReviewPublication::Uncertain;
    if let Some(stdin) = child.stdin.as_mut() {
        use std::io::Write;
        stdin
            .write_all(serde_json::to_string(&payload)?.as_bytes())
            .map_err(|e| anyhow::anyhow!("Failed to write review payload: {e}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| anyhow::anyhow!("Failed to wait for `gh api`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("gh api POST repos/{repo}/pulls/{number}/reviews failed: {stderr}");
    }
    *publication = ReviewPublication::Posted;
    Ok(())
}

/// Publish a completed PR review exactly once: resolve the repository, render
/// the summary, and include every comment whose position the diff confirms.
/// A failed request can have an uncertain remote outcome, so reconciliation
/// and any retry stay under the caller's control rather than risking a duplicate.
fn post_pr_review(
    number: u32,
    view: &GhPullRequest,
    repo: Option<&str>,
    review: &crate::tools::review::ReviewOutput,
    diff: &str,
    body: &str,
    publication: &mut ReviewPublication,
) -> Result<()> {
    crate::tools::review_pr::ensure_current(number, repo, &std::env::current_dir()?, view)?;
    let repo_name = match repo.map(str::trim).filter(|repo| !repo.is_empty()) {
        Some(repo) => repo.to_string(),
        None => run_gh_repo_name()?,
    };
    let plan = plan_inline_review_comments(review, diff);
    if let Some(receipt) = plan.receipt() {
        eprintln!("{receipt}");
    }
    let body = neutralize_model_suggestion_fences(body);
    run_gh_post_pr_review(
        &repo_name,
        number,
        &body,
        &view.head_sha,
        &plan.comments,
        publication,
    )
}

/// Both the CLI review and interactive composer receive the complete diff.
/// Collection and review-budget checks must fail before any partial review.
fn format_pr_prompt(number: u32, view: &GhPullRequest, diff: &str) -> String {
    let diff_section = crate::tools::review_pr::model_diff(diff);
    let body = if view.body.trim().is_empty() {
        "(no description)".to_string()
    } else {
        view.body.trim().to_string()
    };
    let title = if view.title.trim().is_empty() {
        format!("(PR #{number})")
    } else {
        view.title.trim().to_string()
    };
    let branches = match (view.base.is_empty(), view.head.is_empty()) {
        (false, false) => format!("{} ← {}", view.base, view.head),
        (false, true) => view.base.clone(),
        (true, false) => view.head.clone(),
        _ => "(unknown)".to_string(),
    };
    format!(
        "Review PR #{number} — {title}\n\
         \n\
         URL: {url}\n\
         Branches: {branches}\n\
         Revision: {head_sha} (base {base_sha}); {changed_files} file patches.\n\
         Binary changes are represented by metadata; their contents are not semantically inspected. Exact binary object IDs remain in the review evidence.\n\
         \n\
         ## Description\n\
         \n\
         {body}\n\
         \n\
         ## Diff\n\
         \n\
         ```diff\n\
         {diff_section}\n\
         ```\n",
        head_sha = view.head_sha,
        base_sha = view.base_sha,
        changed_files = view.changed_files,
        url = if view.url.is_empty() {
            "(unavailable)"
        } else {
            view.url.as_str()
        },
    )
}

fn collect_diff(
    args: &ReviewArgs,
    pr_view: Option<&GhPullRequest>,
    workspace: &std::path::Path,
) -> Result<String> {
    let diff = if let Some(number) = args.pr {
        run_gh_pr_diff(
            number,
            args.repo.as_deref(),
            pr_view.context("PR snapshot is required")?,
        )?
    } else {
        let mut cmd = crate::dependencies::Git::review_command(workspace)?;
        // Review repository content without executing its diff drivers.
        cmd.current_dir(workspace)
            .arg("diff")
            .args(crate::dependencies::Git::REVIEW_DIFF_ARGS);
        if args.staged {
            cmd.arg("--cached");
        }
        if let Some(base) = &args.base {
            cmd.arg(format!("{base}...HEAD"));
        }
        if let Some(path) = &args.path {
            cmd.arg("--").arg(path);
        }

        ensure_review_workspace_is_git_repo(workspace)?;
        let output = cmd
            .output()
            .map_err(|e| anyhow::anyhow!("Failed to run git diff. Is git installed? ({e})"))?;
        if !output.status.success() {
            bail!(
                "git diff failed: {}",
                first_stderr_line(&String::from_utf8_lossy(&output.stderr))
            );
        }
        String::from_utf8_lossy(&output.stdout).to_string()
    };
    if args.pr.is_none() {
        ensure_local_review_diff_fits(&diff, args.max_chars)?;
    }
    Ok(diff)
}

/// Outside a work tree `git diff` prints its whole `--no-index` usage; say
/// what is actually wrong instead. Only git's own "not a git repository"
/// becomes that one line; any other failure (dubious ownership, permissions,
/// a corrupt repository) keeps git's stderr, which names the fix.
fn ensure_review_workspace_is_git_repo(workspace: &std::path::Path) -> Result<()> {
    let output = crate::dependencies::Git::review_command(workspace)?
        .current_dir(workspace)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run git. Is git installed? ({e})"))?;
    if output.status.success() {
        if String::from_utf8_lossy(&output.stdout).trim() == "true" {
            return Ok(());
        }
        // Inside `.git` or a bare repository: a repository, but no work tree.
        bail!(
            "Not inside a git work tree (cwd: {}); run review from a checkout, not a bare repository or .git directory",
            workspace.display()
        );
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.to_ascii_lowercase().contains("not a git repository") {
        bail!("Not inside a git repository (cwd: {})", workspace.display());
    }
    bail!(
        "git could not read the repository at {}: {}",
        workspace.display(),
        stderr.trim()
    );
}

fn first_stderr_line(stderr: &str) -> &str {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("(no error output)")
}

fn ensure_local_review_diff_fits(diff: &str, max_chars: usize) -> Result<()> {
    let chars = diff.chars().count();
    if chars > max_chars {
        bail!(
            "Complete local diff requires {chars} characters, exceeding the review limit of {max_chars}. No review was run and no receipt was written or accepted. Select an explicit --path scope, or increase --max-chars only if the selected model can accept the complete input."
        );
    }
    Ok(())
}

fn review_target_label(args: &ReviewArgs) -> String {
    let mut label = if let Some(number) = args.pr {
        format!("pr:{number}")
    } else if args.staged {
        "staged".to_string()
    } else if let Some(base) = args
        .base
        .as_deref()
        .map(str::trim)
        .filter(|base| !base.is_empty())
    {
        format!("base:{base}")
    } else {
        "working-tree".to_string()
    };
    if let Some(path) = &args.path {
        label.push(' ');
        label.push_str(path.to_string_lossy().as_ref());
    }
    label
}

fn run_apply(args: ApplyArgs) -> Result<()> {
    let patch = if let Some(path) = args.patch_file {
        std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read patch {}: {}", path.display(), e))?
    } else {
        read_patch_from_stdin()?
    };
    if patch.trim().is_empty() {
        bail!("Patch is empty.");
    }

    let mut tmp = NamedTempFile::new()?;
    tmp.write_all(patch.as_bytes())?;
    let tmp_path = tmp.path().to_path_buf();

    let output = crate::dependencies::Git::command()
        .ok_or_else(|| anyhow::anyhow!("git not found on PATH"))?
        .arg("apply")
        .arg("--whitespace=nowarn")
        .arg(&tmp_path)
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run git apply: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git apply failed: {}", stderr.trim());
    }
    println!("Applied patch successfully.");
    Ok(())
}

/// Maximum bytes read for a patch on stdin. Generous for large diffs;
/// anything larger is not a patch.
const MAX_STDIN_PATCH_BYTES: u64 = 16 * 1024 * 1024;

fn read_patch_from_stdin() -> Result<String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        bail!("No patch file provided and stdin is empty.");
    }
    read_capped_text(stdin.lock(), MAX_STDIN_PATCH_BYTES, "patch on stdin")
}

/// Read UTF-8 text, refusing more than `limit` bytes with a message that
/// names the limit in bytes.
fn read_capped_text(reader: impl Read, limit: u64, what: &str) -> Result<String> {
    let mut buffer = String::new();
    reader.take(limit + 1).read_to_string(&mut buffer)?;
    if buffer.len() as u64 > limit {
        bail!("{what} exceeds the {limit}-byte limit");
    }
    Ok(buffer)
}

/// Warning for a user MCP server that duplicates the enabled built-in
/// Computer Use bundle. Advisory: the entry is never removed.
fn duplicate_computer_use_warning(name: &str) -> String {
    format!(
        "  warning: `{name}` launches the same Computer Use plugin as the enabled built-in computer-use bundle; every Computer Use tool is advertised twice (~2.5k extra tokens per request) with separate consent state. Remove or disable `{name}` in mcp.json, or disable the built-in bundle, to keep one."
    )
}

/// Credential-safe launch summary used by the actual `mcp list` path.
fn mcp_server_listing(command: Option<&str>, args: &[String], url: Option<&str>) -> String {
    use codewhale_secrets::sanitize::{
        is_sensitive_key_name, redact_url_for_display, sanitize_text,
    };
    let flag_name = |arg: &str| arg.trim_start_matches('-').to_string();
    let mut shown = Vec::with_capacity(args.len());
    let mut mask_next = false;
    for arg in args {
        if mask_next {
            shown.push("***".to_string());
            mask_next = false;
            continue;
        }
        if arg.starts_with('-') {
            if let Some((flag, _)) = arg.split_once('=') {
                if is_sensitive_key_name(&flag_name(flag)) {
                    shown.push(format!("{flag}=***"));
                    continue;
                }
            } else if is_sensitive_key_name(&flag_name(arg)) {
                mask_next = true;
            }
        }
        // Each argument on its own, so a masked value never swallows the
        // arguments after it.
        shown.push(sanitize_text(arg));
    }
    match (command, url) {
        (Some(command), _) if shown.is_empty() => sanitize_text(command),
        (Some(command), _) => format!("{} {}", sanitize_text(command), shown.join(" ")),
        (None, Some(url)) => sanitize_text(&redact_url_for_display(url)),
        (None, None) => "unknown".to_string(),
    }
}

/// Printed after `mcp connect` and `mcp validate` succeed. docs/MCP.md
/// § Connection Lifecycle already states that these commands inspect their
/// own process's pool and never attach transports to a running TUI or exec
/// session; the success line alone reads as a real fix for the running
/// session otherwise (issue #6828).
const MCP_OWN_PROCESS_NOTE: [&str; 2] = [
    "Note: this command ran in its own process; it does not attach to a running TUI or exec session.",
    "In a running session, use in-session discovery: search for the server name or an mcp_<server>_ tool name, or call one of its tools directly.",
];

fn print_mcp_own_process_note() {
    for line in MCP_OWN_PROCESS_NOTE {
        println!("{line}");
    }
}

#[cfg(test)]
mod mcp_own_process_note_tests {
    use super::MCP_OWN_PROCESS_NOTE;

    #[test]
    fn mcp_own_process_note_states_the_session_boundary_and_recovery() {
        let note = MCP_OWN_PROCESS_NOTE.join("\n");
        assert!(
            note.contains("own process") && note.contains("does not attach"),
            "the connect/validate note must name the process boundary: {note}"
        );
        assert!(
            note.contains("search for the server name"),
            "the note must point at in-session discovery: {note}"
        );
    }
}

async fn run_mcp_command(
    config: &Config,
    workspace: &Path,
    command: McpCommand,
    plugins: &crate::plugins::PluginRegistry,
) -> Result<()> {
    let config_path = config.mcp_config_path();
    let network_policy = config.network.clone().map(|network| {
        crate::network_policy::NetworkPolicyDecider::with_default_audit(network.into_runtime())
    });
    match command {
        McpCommand::Init { force } => {
            let status = init_mcp_config(&config_path, force)?;
            match status {
                WriteStatus::Created => {
                    println!("Created MCP config at {}", config_path.display());
                }
                WriteStatus::Overwritten => {
                    println!("Overwrote MCP config at {}", config_path.display());
                }
                WriteStatus::SkippedExists => {
                    println!(
                        "MCP config already exists at {} (use --force to overwrite)",
                        config_path.display()
                    );
                }
            }
            println!("Edit the file, then run `codewhale mcp list` or `codewhale mcp tools`.");
            Ok(())
        }
        McpCommand::List => {
            let cfg = crate::mcp::load_config_with_workspace_and_plugins(
                &config_path,
                workspace,
                plugins,
            )?;
            if cfg.servers.is_empty() {
                println!(
                    "No MCP servers configured in {} or {}",
                    config_path.display(),
                    crate::mcp::workspace_mcp_config_path(workspace).display()
                );
                return Ok(());
            }
            println!("MCP servers ({}):", cfg.servers.len());
            let duplicate_computer_use = crate::mcp::duplicate_computer_use_servers(&cfg);
            for (name, server) in cfg.servers {
                let status = if server.enabled && !server.disabled {
                    "enabled"
                } else {
                    "disabled"
                };
                let auth_status = crate::mcp::oauth::auth_status_for_server(
                    &name,
                    &server,
                    network_policy.as_ref(),
                )
                .await;
                let auth = if auth_status == crate::mcp::oauth::McpAuthStatus::Unsupported {
                    String::new()
                } else {
                    format!(
                        " auth={}",
                        auth_status
                            .to_string()
                            .to_ascii_lowercase()
                            .replace(' ', "-")
                    )
                };
                let cmd_str = mcp_server_listing(
                    server.command.as_deref(),
                    &server.args,
                    server.url.as_deref(),
                );
                let required = if server.required { " required" } else { "" };
                println!("  - {name} [{status}{required}{auth}] {cmd_str}");
            }
            for (name, _) in &duplicate_computer_use {
                println!("{}", duplicate_computer_use_warning(name));
            }
            Ok(())
        }
        McpCommand::Connect { server } => {
            let mut pool = McpPool::from_config_path_with_workspace_and_plugins(
                &config_path,
                workspace,
                std::sync::Arc::new(plugins.clone()),
            )?
            .with_backend(crate::mcp::McpBackend::from_config(config));
            if let Some(name) = server {
                if let Err(err) = pool.get_or_connect(&name).await {
                    if crate::mcp::oauth::error_looks_auth_required(&err) {
                        let hint = crate::mcp::oauth::auth_required_login_hint(&name);
                        return Err(err).context(hint);
                    }
                    return Err(err);
                }
                println!("Connected to MCP server: {name}");
                print_mcp_own_process_note();
            } else {
                let errors = pool.connect_all().await;
                if errors.is_empty() {
                    println!("Connected to all configured MCP servers.");
                    print_mcp_own_process_note();
                } else {
                    for (name, err) in errors {
                        eprintln!("Failed to connect {name}: {err:#}");
                        if crate::mcp::oauth::error_looks_auth_required(&err) {
                            eprintln!("  {}", crate::mcp::oauth::auth_required_login_hint(&name));
                        }
                    }
                }
            }
            Ok(())
        }
        McpCommand::Tools { server } => {
            let mut pool = McpPool::from_config_path_with_workspace_and_plugins(
                &config_path,
                workspace,
                std::sync::Arc::new(plugins.clone()),
            )?
            .with_backend(crate::mcp::McpBackend::from_config(config));
            if let Some(name) = server {
                let conn = match pool.get_or_connect(&name).await {
                    Ok(conn) => conn,
                    Err(err) => {
                        if crate::mcp::oauth::error_looks_auth_required(&err) {
                            let hint = crate::mcp::oauth::auth_required_login_hint(&name);
                            return Err(err).context(hint);
                        }
                        return Err(err);
                    }
                };
                if conn.tools().is_empty() {
                    println!("No tools found for MCP server: {name}");
                } else {
                    println!("Tools for {name}:");
                    for tool in conn.tools() {
                        println!(
                            "  - {}{}",
                            tool.name,
                            crate::mcp::format_mcp_tool_description(tool.description.as_deref())
                        );
                    }
                }
            } else {
                let errors = pool.connect_all().await;
                for (name, err) in errors {
                    eprintln!("Failed to connect {name}: {err:#}");
                    if crate::mcp::oauth::error_looks_auth_required(&err) {
                        eprintln!("  {}", crate::mcp::oauth::auth_required_login_hint(&name));
                    }
                }
                let tools = pool.all_tools();
                if tools.is_empty() {
                    println!("No MCP tools discovered.");
                } else {
                    println!("MCP tools:");
                    for (name, tool) in tools {
                        println!(
                            "  - {}{}",
                            name,
                            crate::mcp::format_mcp_tool_description(tool.description.as_deref())
                        );
                    }
                }
            }
            Ok(())
        }
        McpCommand::Add {
            name,
            command,
            url,
            transport,
            bearer_token_env_var,
            oauth_client_id,
            oauth_resource,
            scopes,
            args,
        } => {
            if command.is_none() && url.is_none() {
                bail!("Provide either --command or --url for `mcp add`.");
            }
            if let Some(transport) = transport.as_deref()
                && !transport.trim().eq_ignore_ascii_case("sse")
            {
                bail!("Unsupported MCP transport '{transport}'. Supported values: sse");
            }
            let added_server = McpServerConfig {
                command,
                args,
                env: std::collections::HashMap::new(),
                cwd: None,
                url,
                transport,
                connect_timeout: None,
                execute_timeout: None,
                read_timeout: None,
                disabled: false,
                enabled: true,
                required: false,
                enabled_tools: Vec::new(),
                disabled_tools: Vec::new(),
                headers: std::collections::HashMap::new(),
                env_headers: std::collections::HashMap::new(),
                bearer_token_env_var,
                scopes,
                oauth: oauth_client_id.map(|client_id| McpServerOAuthConfig {
                    client_id: Some(client_id),
                }),
                oauth_resource,
                reviewed_plugin: None,
                runtime_added: false,
                allow_private_network: false,
            };
            let can_suggest_oauth = added_server.url.is_some()
                && added_server.bearer_token_env_var.is_none()
                && added_server
                    .headers
                    .keys()
                    .all(|key| !key.trim().eq_ignore_ascii_case("authorization"))
                && added_server
                    .env_headers
                    .keys()
                    .all(|key| !key.trim().eq_ignore_ascii_case("authorization"));
            crate::mcp::mutate_config(&config_path, None, |cfg| {
                cfg.servers.insert(name.clone(), added_server.clone());
                Ok(())
            })?;
            println!("Added MCP server '{name}' in {}", config_path.display());
            if can_suggest_oauth
                && crate::mcp::oauth::oauth_login_support(&added_server, network_policy.as_ref())
                    .await
                    .is_ok_and(|support| support.is_some())
            {
                println!(
                    "OAuth is available for '{name}'. Run `codewhale mcp login {name}` to authenticate."
                );
            }
            Ok(())
        }
        McpCommand::Login { name, scopes } => {
            let cfg = crate::mcp::load_config_with_workspace_and_plugins(
                &config_path,
                workspace,
                plugins,
            )?;
            let server = cfg
                .servers
                .get(&name)
                .ok_or_else(|| anyhow!("MCP server '{name}' not found"))?;
            let explicit_scopes = (!scopes.is_empty()).then_some(scopes);
            crate::mcp::oauth::perform_oauth_login_for_server(
                &name,
                server,
                explicit_scopes,
                config.mcp_oauth_callback_port,
                config.mcp_oauth_callback_url.as_deref(),
                network_policy.as_ref(),
            )
            .await?;
            println!("Stored OAuth credentials for MCP server '{name}'.");
            Ok(())
        }
        McpCommand::Logout { name } => {
            let cfg = crate::mcp::load_config_with_workspace_and_plugins(
                &config_path,
                workspace,
                plugins,
            )?;
            let server = cfg
                .servers
                .get(&name)
                .ok_or_else(|| anyhow!("MCP server '{name}' not found"))?;
            if crate::mcp::oauth::delete_oauth_tokens_for_server(&name, server)? {
                println!(
                    "Deleted locally stored OAuth credentials for MCP server '{name}'. That clears this machine only; the provider may keep its grant, and the next login forces the consent screen."
                );
            } else {
                println!("No stored OAuth credentials found for MCP server '{name}'.");
            }
            Ok(())
        }
        McpCommand::Remove { name } => {
            crate::mcp::remove_server_config(&config_path, &name)?;
            println!("Removed MCP server '{name}'");
            Ok(())
        }
        McpCommand::Enable { name } => {
            crate::mcp::set_server_enabled(&config_path, &name, true)?;
            println!("Enabled MCP server '{name}'");
            Ok(())
        }
        McpCommand::Disable { name } => {
            crate::mcp::set_server_enabled(&config_path, &name, false)?;
            println!("Disabled MCP server '{name}'");
            Ok(())
        }
        McpCommand::Validate => {
            let mut pool = McpPool::from_config_path_with_workspace_and_plugins(
                &config_path,
                workspace,
                std::sync::Arc::new(plugins.clone()),
            )?
            .with_backend(crate::mcp::McpBackend::from_config(config));
            let errors = pool.connect_all().await;
            if errors.is_empty() {
                println!("MCP config is valid. All enabled servers connected.");
                print_mcp_own_process_note();
                return Ok(());
            }
            eprintln!("MCP validation failed:");
            for (name, err) in errors {
                eprintln!("  - {name}: {err:#}");
            }
            bail!("one or more MCP servers failed validation");
        }
        McpCommand::AddSelf { name, workspace } => {
            let exe_path = std::env::current_exe()
                .map_err(|e| anyhow!("Cannot resolve current binary path: {e}"))?;
            let exe_str = exe_path.to_string_lossy().to_string();

            let mut args = Vec::with_capacity(if workspace.is_some() { 4 } else { 2 });
            if let Some(ref ws) = workspace {
                args.push("--workspace".to_string());
                args.push(ws.clone());
            }
            args.push("serve".to_string());
            args.push("--mcp".to_string());

            crate::mcp::mutate_config(&config_path, None, |cfg| {
                if cfg.servers.contains_key(&name) {
                    bail!(
                        "MCP server '{name}' already exists in {}. Use `codewhale mcp remove {name}` first, or choose a different --name.",
                        config_path.display()
                    );
                }
                cfg.servers.insert(
                    name.clone(),
                    McpServerConfig {
                        command: Some(exe_str.clone()),
                        args,
                        env: std::collections::HashMap::new(),
                        cwd: None,
                        url: None,
                        transport: None,
                        connect_timeout: None,
                        execute_timeout: None,
                        read_timeout: None,
                        disabled: false,
                        enabled: true,
                        required: false,
                        enabled_tools: Vec::new(),
                        disabled_tools: Vec::new(),
                        headers: std::collections::HashMap::new(),
                        env_headers: std::collections::HashMap::new(),
                        bearer_token_env_var: None,
                        scopes: Vec::new(),
                        oauth: None,
                        oauth_resource: None,
                        reviewed_plugin: None,
                        runtime_added: false,
                        allow_private_network: false,
                    },
                );
                Ok(())
            })?;
            println!(
                "Registered Codewhale as MCP server '{name}' in {}",
                config_path.display()
            );
            println!("  command: {exe_str}");
            println!(
                "  args:    {}serve --mcp",
                workspace.map_or(String::new(), |ws| format!("--workspace {ws} "))
            );
            println!();
            println!("Tip: Use `codewhale mcp validate` to test the connection.");
            println!("     Use `codewhale serve --http` for the HTTP/SSE runtime API instead.");
            Ok(())
        }
    }
}

/// Diagnostic status for an MCP server entry.
#[derive(Debug)]
enum McpServerDoctorStatus {
    Ok(String),
    Warning(String),
    Error(String),
}

impl McpServerDoctorStatus {
    fn legacy_status(&self) -> &'static str {
        match self {
            Self::Ok(_) => "ok",
            Self::Warning(_) => "warning",
            Self::Error(_) => "error",
        }
    }

    fn configuration_status(&self) -> &'static str {
        match self {
            Self::Ok(_) => "valid",
            Self::Warning(_) => "warning",
            Self::Error(_) => "invalid",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Ok(detail) | Self::Warning(detail) | Self::Error(detail) => detail,
        }
    }
}

/// Inspect command availability without starting the configured MCP server.
fn doctor_mcp_command_status(server: &McpServerConfig) -> McpCommandAvailability {
    if server.url.is_some() {
        return McpCommandAvailability::NotApplicable;
    }
    match server.command.as_deref() {
        Some("") => McpCommandAvailability::Missing,
        Some(_) | None => McpCommandAvailability::NotChecked,
    }
}

fn doctor_mcp_server_json(name: &str, server: &McpServerConfig) -> serde_json::Value {
    use serde_json::json;

    let status = doctor_check_mcp_server(server);
    json!({
        "name": name,
        "enabled": server.enabled && !server.disabled,
        // Compatibility field retained for existing doctor JSON consumers.
        // Its scope is now explicit in `checks.configuration` below.
        "status": status.legacy_status(),
        "detail": status.detail(),
        "transport": if server.url.is_some() { "http" } else { "stdio" },
        "endpoint": server.url.as_deref().map(crate::doctor::structural_url_authority),
        "command_configured": server.command.is_some(),
        "args_count": server.args.len(),
        "env_count": server.env.len(),
        "headers_count": server.headers.len(),
        "env_headers_count": server.env_headers.len(),
        "check_scope": "configuration",
        "checks": {
            "configuration": {
                "status": status.configuration_status(),
                "detail": status.detail(),
            },
            "command": {
                "status": doctor_mcp_command_status(server).as_str(),
            },
            "process_reachable": {
                "status": "not_checked",
            },
            "protocol_initialized": {
                "status": "not_checked",
            },
            "backend_tool_health": {
                "status": "not_checked",
            },
        },
    })
}

/// Check an MCP server config entry for common issues.
fn doctor_check_mcp_server(server: &McpServerConfig) -> McpServerDoctorStatus {
    // No command or URL — incomplete entry.
    if server.command.is_none() && server.url.is_none() {
        return McpServerDoctorStatus::Error("no command or url configured".to_string());
    }

    // URL-based server: omit userinfo, query, and fragment entirely.
    if let Some(ref url) = server.url {
        let authority = crate::doctor::structural_url_authority(url);
        return if authority.starts_with("unparseable") {
            McpServerDoctorStatus::Warning(
                "HTTP/SSE server URL is invalid; configured value omitted".to_string(),
            )
        } else {
            McpServerDoctorStatus::Ok(format!("HTTP/SSE server at {authority}"))
        };
    }

    // Command-based: validate command path exists.
    let cmd = server.command.as_deref().unwrap_or("");
    if cmd.is_empty() {
        return McpServerDoctorStatus::Error("empty command".to_string());
    }

    if server.cwd.is_none() {
        if is_relative_stdio_path_arg(cmd) {
            return McpServerDoctorStatus::Warning(
                "stdio server uses a relative command without cwd; command value omitted"
                    .to_string(),
            );
        }
        if server
            .args
            .iter()
            .any(|arg| is_relative_stdio_path_arg(arg) && !is_scoped_npm_package_arg(cmd, arg))
        {
            return McpServerDoctorStatus::Warning(
                "stdio server uses a relative path argument without cwd; argument values omitted"
                    .to_string(),
            );
        }
    }

    McpServerDoctorStatus::Ok(format!(
        "stdio server configured (command omitted; {} argument(s), {} environment binding(s))",
        server.args.len(),
        server.env.len()
    ))
}

/// `@scope/package@version` is an npm package spec, not a relative filesystem
/// path, even though it contains `/`. Keep this exception tied to the npx
/// launcher so similarly shaped arguments to other commands retain the
/// relative-path warning.
fn is_scoped_npm_package_arg(command: &str, argument: &str) -> bool {
    let launcher = Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command);
    if !launcher.eq_ignore_ascii_case("npx") && !launcher.eq_ignore_ascii_case("npx.cmd") {
        return false;
    }

    let Some(scoped) = argument.strip_prefix('@') else {
        return false;
    };
    let Some((scope, package_and_version)) = scoped.split_once('/') else {
        return false;
    };
    if scope.is_empty()
        || package_and_version.is_empty()
        || package_and_version.contains('/')
        || package_and_version.contains('\\')
    {
        return false;
    }

    let (package, version) = match package_and_version.split_once('@') {
        Some((package, version)) => (package, Some(version)),
        None => (package_and_version, None),
    };
    let valid_name = |value: &str| {
        !value.is_empty()
            && !value.starts_with(['.', '_'])
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    };
    valid_name(scope)
        && valid_name(package)
        && version.is_none_or(|value| {
            !value.is_empty()
                && !value.contains(['@', '/', '\\'])
                && !value.chars().any(char::is_whitespace)
        })
}

fn run_sandbox_command(args: SandboxArgs) -> Result<()> {
    use crate::sandbox::{CommandSpec, SandboxManager};

    let SandboxCommand::Run {
        policy,
        network,
        writable_root,
        exclude_tmpdir,
        exclude_slash_tmp,
        cwd,
        timeout_ms,
        command,
    } = args.command;

    let policy = parse_sandbox_policy(
        &policy,
        network,
        writable_root,
        exclude_tmpdir,
        exclude_slash_tmp,
    )?;
    let cwd = cwd.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let timeout = Duration::from_millis(timeout_ms.clamp(1000, 600_000));

    let (program, args) = command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("Command is required"))?;
    let spec =
        CommandSpec::program(program, args.to_vec(), cwd.clone(), timeout).with_policy(policy);
    let manager = SandboxManager::new();
    let exec_env = manager.prepare(&spec);

    let mut cmd = Command::new(exec_env.program());
    cmd.args(exec_env.args())
        .current_dir(&exec_env.cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    child_env::apply_to_command(&mut cmd, child_env::string_map_env(&exec_env.env));
    // Lead a process group so the timeout ends everything the command started.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to run command: {e}"))?;
    // The sandbox run is the tree's lifetime: dropping `tree` on return ends
    // anything the command left running, as `contained_output` does.
    let tree = match crate::process_tree::ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Failed to contain the sandboxed command: {error}");
        }
    };
    let stdout_handle = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdout unavailable"))?;
    let stderr_handle = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("stderr unavailable"))?;

    // Output streams straight through instead of being buffered whole: a
    // command's output size is unbounded, so only a bounded stderr tail is
    // kept for sandbox-denial detection.
    const STDERR_TAIL_BYTES: usize = 64 * 1024;
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let stdout_done = done_tx.clone();
    std::thread::spawn(move || {
        let mut reader = stdout_handle;
        let _ = io::copy(&mut reader, &mut io::stdout());
        let _ = stdout_done.send(());
    });
    let stderr_tail = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let tail = Arc::clone(&stderr_tail);
    std::thread::spawn(move || {
        let mut reader = stderr_handle;
        let mut chunk = [0_u8; 8192];
        loop {
            let read = match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            let _ = io::stderr().write_all(&chunk[..read]);
            if let Ok(mut tail) = tail.lock() {
                tail.extend_from_slice(&chunk[..read]);
                let excess = tail.len().saturating_sub(STDERR_TAIL_BYTES);
                tail.drain(..excess);
            }
        }
        let _ = done_tx.send(());
    });

    let timeout = exec_env.timeout;
    let deadline = Instant::now() + timeout;
    let Some(status) = child.wait_timeout(timeout)? else {
        let _ = tree.kill();
        let _ = child.kill();
        let _ = child.wait();
        bail!("Command timed out after {}ms", timeout.as_millis());
    };
    // A descendant may still hold the output pipes. Let it finish inside the
    // same budget, then end the tree so the drain always reaches EOF.
    let mut drained = 0;
    while drained < 2 {
        match done_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(()) => drained += 1,
            Err(_) => break,
        }
    }
    if drained < 2 {
        let _ = tree.kill();
        while drained < 2 && done_rx.recv_timeout(Duration::from_secs(1)).is_ok() {
            drained += 1;
        }
    }

    let stderr = stderr_tail
        .lock()
        .map(|tail| tail.clone())
        .unwrap_or_default();
    let stderr_str = String::from_utf8_lossy(&stderr);
    let exit_code = status.code().unwrap_or(-1);
    let sandbox_type = exec_env.sandbox_type;
    if SandboxManager::was_denied(sandbox_type, exit_code, &stderr_str) {
        eprintln!(
            "{}",
            SandboxManager::denial_message(sandbox_type, &stderr_str)
        );
    }
    if !status.success() {
        bail!("Command failed with exit code {exit_code}");
    }
    Ok(())
}

fn parse_sandbox_policy(
    policy: &str,
    network: bool,
    writable_root: Vec<PathBuf>,
    exclude_tmpdir: bool,
    exclude_slash_tmp: bool,
) -> Result<crate::sandbox::SandboxPolicy> {
    use crate::sandbox::SandboxPolicy;

    match policy {
        "danger-full-access" => Ok(SandboxPolicy::DangerFullAccess),
        "read-only" => Ok(SandboxPolicy::ReadOnly),
        "external-sandbox" => Ok(SandboxPolicy::ExternalSandbox {
            network_access: network,
        }),
        "workspace-write" => Ok(SandboxPolicy::WorkspaceWrite {
            writable_roots: writable_root,
            network_access: network,
            exclude_tmpdir,
            exclude_slash_tmp,
        }),
        other => bail!("Unknown sandbox policy: {other}"),
    }
}

/// Screen the interactive TUI starts on.
///
/// `tui.alternate_screen` is the existing knob and keeps its existing
/// vocabulary: `auto`/`always` stay on the alternate screen (the default),
/// while `never` now selects the full-height inline viewport instead of being
/// parsed and ignored. `/inline` and `/fullscreen` move it at runtime.
fn startup_screen_mode(_cli: &Cli, config: &Config) -> ScreenMode {
    config
        .tui
        .as_ref()
        .and_then(|tui| tui.alternate_screen.as_deref())
        .and_then(ScreenMode::parse)
        .unwrap_or_default()
}

/// The user's mouse-capture preference with the screen factored out: the
/// CLI flags, then `tui.mouse_capture`, then the host default. Which screen
/// the session is on decides whether it applies — see
/// [`ScreenMode::mouse_capture`], which startup and the runtime switch share.
fn mouse_capture_preference(cli: &Cli, config: &Config) -> bool {
    let terminal_emulator = std::env::var("TERMINAL_EMULATOR").ok();
    let wt_session = std::env::var("WT_SESSION").ok().filter(|s| !s.is_empty());
    let conemu_pid = std::env::var("ConEmuPID").ok().filter(|s| !s.is_empty());
    mouse_capture_preference_with(
        cli,
        config,
        terminal_emulator.as_deref(),
        wt_session.as_deref(),
        conemu_pid.as_deref(),
    )
}

fn mouse_capture_preference_with(
    cli: &Cli,
    config: &Config,
    terminal_emulator: Option<&str>,
    wt_session: Option<&str>,
    conemu_pid: Option<&str>,
) -> bool {
    if cli.no_mouse_capture {
        return false;
    }
    if cli.mouse_capture {
        return true;
    }
    config
        .tui
        .as_ref()
        .and_then(|tui| tui.mouse_capture)
        .unwrap_or_else(|| default_mouse_capture_enabled(terminal_emulator, wt_session, conemu_pid))
}

#[cfg(test)]
fn should_use_mouse_capture_with(
    cli: &Cli,
    config: &Config,
    use_alt_screen: bool,
    terminal_emulator: Option<&str>,
    wt_session: Option<&str>,
    conemu_pid: Option<&str>,
) -> bool {
    let mode = if use_alt_screen {
        ScreenMode::Fullscreen
    } else {
        ScreenMode::Inline
    };
    mode.mouse_capture(mouse_capture_preference_with(
        cli,
        config,
        terminal_emulator,
        wt_session,
        conemu_pid,
    ))
}

/// Whether to enable terminal mouse capture by default for this platform/host.
///
/// On Windows the default depends on the host: Windows Terminal (which sets
/// `WT_SESSION`) and ConEmu/Cmder (which set `ConEmuPID`) handle mouse-mode
/// reporting cleanly, so default-on there gives users in-app text selection
/// and keeps the application's selection clamped to the transcript area
/// (#1169). Legacy conhost (CMD without either env var) stays default-off
/// because its mouse-mode reporting can leak SGR escape sequences as raw
/// text into the composer (#878 / #898).
///
/// Off elsewhere only for JetBrains' JediTerm, which advertises mouse
/// support but forwards the same SGR escape sequences as raw input. The
/// user can still opt back in with `[tui] mouse_capture = true` in
/// `~/.codewhale/config.toml` or `--mouse-capture`.
fn default_mouse_capture_enabled(
    terminal_emulator: Option<&str>,
    wt_session: Option<&str>,
    conemu_pid: Option<&str>,
) -> bool {
    if cfg!(windows) {
        return wt_session.is_some() || conemu_pid.is_some();
    }
    if matches!(terminal_emulator, Some(t) if t.eq_ignore_ascii_case("JetBrains-JediTerm")) {
        return false;
    }
    true
}

/// A loadable crash-recovery checkpoint candidate: session content, file
/// age, and which slot it came from (per-session file or the legacy single
/// slot).
struct RecentCheckpoint {
    session: session_manager::SavedSession,
    age: std::time::Duration,
    source: session_manager::CheckpointSource,
}

const CHECKPOINT_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// Load all recent crash-recovery checkpoints, pruning stale ones first.
///
/// Candidates are the per-session checkpoint files plus the legacy
/// single-slot `checkpoints/latest.json` (compatibility read). Files older
/// than 24 hours are removed; unreadable files are skipped. The result is
/// sorted most recent first.
fn load_recent_checkpoints(manager: &session_manager::SessionManager) -> Vec<RecentCheckpoint> {
    let refs = manager.list_checkpoints().unwrap_or_default();
    let mut recent = Vec::new();
    for checkpoint_ref in refs {
        // A session open in another terminal refreshes its own checkpoint
        // mid-turn. It is not interrupted: promoting or clearing it would
        // take that session's only crash-recovery record while it runs. The
        // legacy slot is checked by the session it names, before anything
        // prunes, promotes or migrates it over that session's live state.
        let owner = match &checkpoint_ref.source {
            session_manager::CheckpointSource::Session(id) => Some(id.clone()),
            session_manager::CheckpointSource::Legacy => {
                manager.legacy_checkpoint_origin().ok().flatten()
            }
        };
        if owner.is_some_and(|id| manager.is_session_live_anywhere(&id)) {
            continue;
        }
        let Ok(age) = std::time::SystemTime::now().duration_since(checkpoint_ref.modified) else {
            continue;
        };
        if age > CHECKPOINT_MAX_AGE {
            let _ = match &checkpoint_ref.source {
                session_manager::CheckpointSource::Session(id) => {
                    manager.clear_session_checkpoint(id)
                }
                session_manager::CheckpointSource::Legacy => manager.clear_legacy_checkpoint(),
            };
            continue;
        }
        let loaded = match &checkpoint_ref.source {
            session_manager::CheckpointSource::Session(id) => manager.load_session_checkpoint(id),
            session_manager::CheckpointSource::Legacy => manager.load_legacy_checkpoint(),
        };
        let Ok(Some(session)) = loaded else {
            continue;
        };
        recent.push(RecentCheckpoint {
            session,
            age,
            source: checkpoint_ref.source,
        });
    }
    // `list_checkpoints` sorts newest-first already; keep it explicit here so
    // selection does not silently depend on the manager's ordering.
    recent.sort_by_key(|c| c.age);
    recent
}

fn checkpoint_age_label(age: std::time::Duration) -> String {
    if age.as_secs() < 60 {
        format!("{}s ago", age.as_secs())
    } else if age.as_secs() < 3600 {
        format!("{}m ago", age.as_secs() / 60)
    } else {
        format!("{}h ago", age.as_secs() / 3600)
    }
}

/// Check for a crash-recovery checkpoint and return the session ID if explicit
/// recovery was requested *and* the checkpoint belongs to the current
/// workspace.
///
/// Candidates are all per-session checkpoint files plus the legacy
/// single-slot `checkpoints/latest.json`; each must be younger than 24 hours
/// **and its workspace must match the resolved launch workspace after
/// canonicalisation** — the newest matching candidate wins. If no candidate
/// matches, a one-line notice points at `codewhale sessions`, and nothing is
/// auto-loaded: another workspace's checkpoint file is never touched (it may
/// belong to a live session there).
/// Resolve the session `--continue` attaches to.
///
/// Recovery promotes the newest same-workspace checkpoint to a session file
/// and clears the checkpoint. That is only correct when the TUI can actually
/// start: a non-TTY launch fails `require_interactive_terminal` later and must
/// not consume the crash record on the way out, or the next real `--continue`
/// has nothing left to recover. Without a terminal, only the latest saved
/// session is considered (and the launch still fails the TTY check).
fn resolve_continue_session_id(launch_workspace: &Path, interactive: bool) -> Option<String> {
    if interactive {
        recover_interrupted_checkpoint_for_resume(launch_workspace).or_else(|| {
            latest_session_id_for_workspace(launch_workspace)
                .ok()
                .flatten()
        })
    } else {
        latest_session_id_for_workspace(launch_workspace)
            .ok()
            .flatten()
    }
}

fn recover_interrupted_checkpoint_for_resume(launch_workspace: &Path) -> Option<String> {
    let manager = session_manager::SessionManager::default_location().ok()?;
    let candidates = load_recent_checkpoints(&manager);
    if candidates.is_empty() {
        return None;
    }

    // Refuse to silently restore a session from another workspace. Compare
    // against the resolved launch workspace, not the shell cwd, so callers
    // using `--workspace` cannot accidentally recover a checkpoint from the
    // directory their shell happened to be in.
    let (matching, mismatched): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|candidate| {
        session_manager::workspace_scope_matches(
            &candidate.session.metadata.workspace,
            launch_workspace,
        )
    });

    let Some(best) = matching.into_iter().next() else {
        if let Some(newest) = mismatched.first() {
            eprintln!(
                "Note: an interrupted session from another workspace ({}) is \
                 available. Run `codewhale sessions` to list saved sessions. Starting \
                 fresh in {}.",
                newest.session.metadata.workspace.display(),
                launch_workspace.display(),
            );
        }
        return None;
    };

    let session_id = best.session.metadata.id.clone();

    // Take the session's live lease before promoting or clearing anything:
    // the liveness filter above is a check, and another terminal can attach
    // between it and these writes. The TUI's own attach then finds the lease
    // already held by this process. Losing the race leaves every file alone.
    match manager.reserve_session_for_attach(&session_id) {
        Ok(lease) => lease.commit(),
        Err(_) => return None,
    }

    // Persist the checkpoint as a regular session so the TUI can load it by
    // id — unless a newer regular session file for the same id already
    // exists (e.g. `--continue` ran before and the session advanced since).
    // A stale checkpoint must never overwrite newer durable session state.
    if !saved_session_is_newer(&manager, &best.session)
        && manager.save_session(&best.session).is_err()
    {
        return None;
    }

    match &best.source {
        session_manager::CheckpointSource::Session(id) => {
            // Consume the per-session checkpoint now that it is recovered.
            let _ = manager.clear_session_checkpoint(id);
        }
        session_manager::CheckpointSource::Legacy => {
            // Migrate the legacy slot to a per-session file (never
            // overwriting an existing one) and leave `latest.json` in place
            // so an older binary can still find it; its writer is already
            // gone and the file ages out within 24 hours.
            let _ = manager.write_session_checkpoint_if_absent(&best.session);
        }
    }

    let age_str = checkpoint_age_label(best.age);
    eprintln!("Recovered interrupted session ({age_str}). Use --fresh to start fresh.",);

    Some(session_id)
}

/// Whether a regular session file for the checkpoint's id already exists and
/// is at least as recent as the checkpoint. When it is, persisting the
/// checkpoint over it would replace newer durable state with older in-flight
/// state.
fn saved_session_is_newer(
    manager: &session_manager::SessionManager,
    checkpoint: &session_manager::SavedSession,
) -> bool {
    manager
        .load_session(&checkpoint.metadata.id)
        .is_ok_and(|existing| existing.metadata.updated_at >= checkpoint.metadata.updated_at)
}

/// Preserve an interrupted checkpoint on a normal fresh launch without
/// attaching it to the new TUI instance. This keeps "open another codewhale in
/// the same folder" from re-entering the previous in-flight session while still
/// leaving an explicit resume path.
///
/// Only the newest recent checkpoint drives the notice. The legacy
/// single-slot file is persisted as a regular session and consumed (today's
/// behavior for that slot); per-session checkpoint files are persisted but
/// left in place — they may belong to a live session in another terminal,
/// and `--continue` reads them directly.
fn preserve_interrupted_checkpoint_for_explicit_resume(launch_workspace: &Path) {
    let Some(manager) = session_manager::SessionManager::default_location().ok() else {
        return;
    };
    let Some(newest) = load_recent_checkpoints(&manager).into_iter().next() else {
        return;
    };

    let session_workspace = newest.session.metadata.workspace.clone();
    // #4479: removed save_session call — checkpoint should not be auto-promoted to session
    if newest.source == session_manager::CheckpointSource::Legacy {
        // Migrate legacy single-slot checkpoint to per-session format
        // before clearing the legacy file, or the data is unrecoverable.
        let _ = manager.save_checkpoint(&newest.session);
        let _ = manager.clear_legacy_checkpoint();
    }

    let age_str = checkpoint_age_label(newest.age);
    if session_manager::workspace_scope_matches(&session_workspace, launch_workspace) {
        eprintln!(
            "Found an in-flight session snapshot ({age_str}). Starting a new \
             session. Run `codewhale --continue` to resume it."
        );
    } else {
        eprintln!(
            "Note: an interrupted session from another workspace ({}) is \
             available. Run `codewhale sessions` to list saved sessions. Starting \
             fresh in {}.",
            session_workspace.display(),
            launch_workspace.display(),
        );
    }
}

/// Load project-level config from `$WORKSPACE/.codewhale/config.toml`, with
/// legacy `$WORKSPACE/.deepseek/config.toml` fallback, then apply its fields as
/// overrides on top of the global config (#485).
/// Only explicitly set fields in the project file are applied; everything
/// else falls back to the global value.
#[cfg(test)]
fn merge_project_config(config: &mut Config, workspace: &Path) {
    merge_project_config_with_approval_baseline(config, workspace, None)
        .expect("project config applies");
}

/// A project config that exists but cannot be applied is an error, not an
/// absent file: it may be the thing tightening approval, sandbox or shell for
/// this workspace, and launching on the looser user baseline without it would
/// fail open. The reason never quotes file contents.
fn project_config_unusable(path: &Path, reason: &str) -> anyhow::Error {
    anyhow!(
        "Project config {} could not be applied ({reason}), so its approval, sandbox and shell restrictions are not in effect. Fix the file, or launch with --no-project-config to ignore it.",
        path.display()
    )
}

/// Apply project config while evaluating approval tightening against the
/// user's effective interactive baseline. `Config::approval_policy` remains
/// authoritative when present; the saved TUI posture is used only when the
/// root config leaves approval unset.
fn merge_project_config_with_approval_baseline(
    config: &mut Config,
    workspace: &Path,
    saved_permission_posture: Option<&str>,
) -> Result<()> {
    // When the workspace is the user's home directory, the project-scope
    // config file is also the global config file. Skip the merge to avoid
    // redundant processing and a misleading "project-scope config key
    // ignored" warning on every launch from ~.
    if let Some(home) = effective_home_dir()
        && let (Ok(w), Ok(h)) = (
            std::fs::canonicalize(workspace),
            std::fs::canonicalize(&home),
        )
        && w == h
    {
        return Ok(());
    }

    // v0.8.44: prefer .codewhale/config.toml, fall back to .deepseek/
    let primary = workspace
        .join(codewhale_config::CODEWHALE_APP_DIR)
        .join("config.toml");
    let (path, raw) = match read_project_config_file(&primary) {
        Ok(Some(raw)) => (primary, raw),
        Ok(None) => {
            let legacy = workspace
                .join(codewhale_config::LEGACY_APP_DIR)
                .join("config.toml");
            match read_project_config_file(&legacy) {
                Ok(Some(raw)) => (legacy, raw),
                Ok(None) => return Ok(()),
                Err(err) => return Err(project_config_unusable(&legacy, &err.to_string())),
            }
        }
        Err(err) => return Err(project_config_unusable(&primary, &err.to_string())),
    };
    let project: toml::Value = toml::from_str(&raw).map_err(|err| {
        // Position only: the parser's message can quote the offending value.
        let reason = err.span().map_or_else(
            || "invalid TOML".to_string(),
            |span| {
                let prefix = &raw.as_bytes()[..span.start.min(raw.len())];
                let line = prefix.iter().filter(|byte| **byte == b'\n').count() + 1;
                format!("invalid TOML at line {line}")
            },
        );
        project_config_unusable(&path, &reason)
    })?;
    let Some(table) = project.as_table() else {
        return Err(project_config_unusable(&path, "not a TOML table"));
    };

    // #417: dangerous keys are denied at project scope. A malicious
    // `<workspace>/.deepseek/config.toml` could otherwise:
    // * `api_key` / `base_url` / `provider` — exfiltrate prompts to a
    //   look-alike endpoint by swapping the user's credentials and
    //   target host with project-controlled values.
    // * `mcp_config_path` — point the loader at an MCP config that
    //   spawns arbitrary stdio servers under the user's identity.
    // * `mcp_oauth_callback_*` — choose local OAuth redirect listener
    //   behavior for user-owned MCP credentials.
    //
    // The overlay path is non-interactive; users can't visually
    // confirm a rogue project config is hijacking these. We surface
    // a stderr warning on first encounter so a user who *did* expect
    // the override has a chance to notice the deny instead of silent
    // discard.
    const DENY_AT_PROJECT_SCOPE: &[&str] = &[
        "api_key",
        "base_url",
        "provider",
        "mcp_config_path",
        "mcp_oauth_callback_port",
        "mcp_oauth_callback_url",
        // The auto-approved `note` tool appends to `notes_path`, so a
        // project value would be a write target the user never reviewed.
        "notes_path",
    ];
    for key in DENY_AT_PROJECT_SCOPE {
        if table.contains_key(*key) {
            eprintln!(
                "warning: project-scope config key `{key}` is ignored — \
                 set it in `~/.codewhale/config.toml` instead. \
                 (See #417 for the deny-list rationale.)"
            );
        }
    }

    // String fields a project may legitimately override (model,
    // approval/sandbox tightening, reasoning effort).
    if !config.environment_model_applied
        && let Some(model) = table.get("model").and_then(toml::Value::as_str)
        && !model.is_empty()
    {
        config.default_text_model = Some(model.to_string());
        let identity = config
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;
        config
            .set_provider_model_override(&identity, Some(model.to_string()))
            .map_err(anyhow::Error::msg)?;
        config.remembered_selection_scope = Some(false);
    }
    if let Some(v) = table.get("reasoning_effort").and_then(toml::Value::as_str)
        && !v.is_empty()
    {
        config.reasoning_effort = Some(v.to_string());
    }

    if let Some(v) = table.get("approval_policy").and_then(toml::Value::as_str)
        && !v.is_empty()
    {
        let saved_approval_baseline =
            crate::config::approval_policy_baseline_from_permission_posture(
                saved_permission_posture,
            );
        let approval_baseline = config
            .approval_policy
            .as_deref()
            .or(saved_approval_baseline);
        if codewhale_config::project_approval_policy_is_allowed(approval_baseline, v) {
            config.approval_policy = Some(v.to_string());
        } else {
            eprintln!(
                "warning: project-scope `approval_policy = \"{v}\"` is ignored — \
                 project config can only tighten the user's approval policy. \
                 (See #417.)"
            );
        }
    }

    if let Some(v) = table.get("sandbox_mode").and_then(toml::Value::as_str)
        && !v.is_empty()
    {
        if codewhale_config::project_sandbox_mode_is_allowed(config.sandbox_mode.as_deref(), v) {
            config.sandbox_mode = Some(v.to_string());
        } else {
            eprintln!(
                "warning: project-scope `sandbox_mode = \"{v}\"` is ignored — \
                 project config can only tighten the user's sandbox mode. \
                 (See #417.)"
            );
        }
    }

    // Numeric / bool fields that benefit from per-project overrides.
    if let Some(v) = table.get("max_subagents").and_then(toml::Value::as_integer)
        && v > 0
    {
        config.max_subagents = Some((v as usize).clamp(1, crate::config::MAX_SUBAGENTS));
    }
    if let Some(v) = table.get("allow_shell").and_then(toml::Value::as_bool) {
        if v {
            eprintln!(
                "warning: project-scope `allow_shell = true` is ignored — \
                 enable shell from user config for this workspace instead. \
                 (See #417.)"
            );
        } else {
            config.allow_shell = Some(false);
        }
    }

    if table.contains_key("instructions") {
        eprintln!(
            "warning: project-scope `instructions` is ignored — \
             configure instruction files from user config instead. \
             (See #417.)"
        );
    }
    Ok(())
}

/// Maximum bytes read from a project config file. Configs are kilobytes.
const MAX_PROJECT_CONFIG_BYTES: u64 = 1024 * 1024;

fn read_project_config_file(path: &Path) -> io::Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "project-scope config must not be a symlink",
        ));
    }
    if !file_type.is_file() {
        return Ok(None);
    }

    let file = open_project_config_file(path)?;
    let mut raw = String::new();
    file.take(MAX_PROJECT_CONFIG_BYTES + 1)
        .read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_PROJECT_CONFIG_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("project config {} exceeds the 1 MiB limit", path.display()),
        ));
    }
    Ok(Some(raw))
}

#[cfg(unix)]
fn open_project_config_file(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_project_config_file(path: &Path) -> io::Result<std::fs::File> {
    std::fs::File::open(path)
}

fn merge_user_workspace_config(
    config: &mut Config,
    config_path: Option<PathBuf>,
    workspace: &Path,
) {
    if config.managed_config_path.is_some() || config.requirements_path.is_some() {
        return;
    }
    let allow_shell_before = config.allow_shell;
    let allow_shell_from_env = std::env::var_os("CODEWHALE_ALLOW_SHELL").is_some()
        || std::env::var_os("DEEPSEEK_ALLOW_SHELL").is_some();
    let path = match crate::config::resolve_load_config_path(config_path) {
        Ok(Some(path)) => path,
        Ok(None) => return,
        Err(error) => {
            tracing::error!(
                error = %error,
                "failed to resolve workspace config overlay; refusing to substitute another file"
            );
            return;
        }
    };
    let raw = match read_user_config_file(&path) {
        Ok(Some(raw)) => raw,
        Ok(None) => return,
        Err(error) => {
            eprintln!(
                "warning: could not read user config at {}: {error}. \
                 Ignoring it — `[workspace]`/`[projects]` grants (e.g. `allow_shell`) \
                 revert to defaults for this session. Fix or remove the file to \
                 restore them.",
                path.display()
            );
            return;
        }
    };
    let doc = match toml::from_str::<toml::Value>(&raw) {
        Ok(doc) => doc,
        Err(error) => {
            eprintln!(
                "warning: could not parse user config at {}: {error}. \
                 Ignoring it — `[workspace]`/`[projects]` grants (e.g. `allow_shell`) \
                 revert to defaults for this session. Fix the TOML syntax to \
                 restore them.",
                path.display()
            );
            return;
        }
    };
    merge_user_workspace_config_from_doc(config, &doc, workspace);
    if allow_shell_from_env {
        config.allow_shell = allow_shell_before;
    }
}

fn read_user_config_file(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match std::fs::symlink_metadata(path) {
                Err(metadata_error) if metadata_error.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(metadata_error) => Err(metadata_error),
                _ => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn merge_user_workspace_config_from_doc(config: &mut Config, doc: &toml::Value, workspace: &Path) {
    for table_name in ["workspace", "projects"] {
        let Some(entries) = doc.get(table_name).and_then(toml::Value::as_table) else {
            continue;
        };
        for (raw_path, entry) in entries {
            if !workspace_config_path_matches(raw_path, workspace) {
                continue;
            }
            if let Some(allow_shell) = entry.get("allow_shell").and_then(toml::Value::as_bool) {
                config.allow_shell = Some(allow_shell);
            }
        }
    }
}

fn workspace_config_path_matches(raw_path: &str, workspace: &Path) -> bool {
    let configured = crate::config::expand_path(raw_path);
    let configured = configured.canonicalize().unwrap_or(configured);
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    paths_equal_for_config(&configured, &workspace)
}

#[cfg(windows)]
fn paths_equal_for_config(left: &Path, right: &Path) -> bool {
    normalize_windows_config_path_for_compare(left)
        == normalize_windows_config_path_for_compare(right)
}

#[cfg(not(windows))]
fn paths_equal_for_config(left: &Path, right: &Path) -> bool {
    left == right
}

#[cfg(windows)]
fn normalize_windows_config_path_for_compare(path: &Path) -> String {
    normalize_windows_config_path_str(&path.to_string_lossy())
}

#[cfg(any(windows, test))]
fn normalize_windows_config_path_str(path: &str) -> String {
    let mut normalized = path.replace('/', "\\");
    if let Some(rest) = normalized.strip_prefix(r"\\?\UNC\") {
        normalized = format!("\\\\{rest}");
    } else if let Some(rest) = normalized.strip_prefix(r"\\?\") {
        normalized = rest.to_string();
    }
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    normalized.to_ascii_lowercase()
}

/// Startup discovery runs before config can select the extension-host policy,
/// so it judged every reviewed Native plugin `CapabilitiesChanged`. Call this
/// after the config load to re-judge under the installed policy, as
/// `/plugin reload` does, so trusted host plugins survive a restart. With the
/// flag off it returns the startup snapshot unchanged.
///
/// Known limits: plugin-declared providers keep the startup snapshot, and the
/// `mcp`, `doctor`, `setup`, `pr`, `review` and workflow-tool subcommands do
/// not call this yet.
fn policy_current_registry(
    registry: Arc<crate::plugins::PluginRegistry>,
) -> Arc<crate::plugins::PluginRegistry> {
    if crate::plugins::activation::extension_host_policy_enabled() {
        let workspace = registry.workspace().to_path_buf();
        registry.rediscover_for_workspace(&workspace)
    } else {
        registry
    }
}

fn interactive_tui_allow_shell(yolo: bool, config: &Config) -> bool {
    yolo || config.interactive_allow_shell()
}

async fn run_interactive(
    cli: &Cli,
    config: &Config,
    resume_session_id: Option<String>,
    initial_input: Option<tui::InitialInput>,
    pending_telemetry_notice: Option<crate::telemetry_notice::PendingTelemetryNotice>,
    plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
) -> Result<()> {
    run_interactive_with_notice(
        cli,
        config,
        resume_session_id,
        initial_input,
        None,
        pending_telemetry_notice,
        plugin_registry,
    )
    .await
}

/// As [`run_interactive`], but carrying a one-line startup receipt to show in
/// the transcript — used by auto-resume to explain why it did or did not
/// reattach to a previous session (#2934).
async fn run_interactive_with_notice(
    cli: &Cli,
    config: &Config,
    resume_session_id: Option<String>,
    initial_input: Option<tui::InitialInput>,
    startup_notice: Option<String>,
    pending_telemetry_notice: Option<crate::telemetry_notice::PendingTelemetryNotice>,
    plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
) -> Result<()> {
    tui::ui::require_interactive_terminal(io::stdin().is_terminal(), io::stdout().is_terminal())?;
    let plugin_registry = policy_current_registry(plugin_registry);
    let prepared = prepare_interactive_config(cli, config, resume_session_id.is_some())?;
    let (prepared, resume_session_id) = if let Some(selector) = resume_session_id {
        let (prepared, id) = prepare_mounted_session(
            cli,
            prepared,
            selector,
            MountedHistoryIntent::Resume,
            Arc::clone(&plugin_registry),
        )
        .await?;
        (prepared, Some(id))
    } else {
        (prepared, None)
    };
    run_interactive_prepared(
        cli,
        prepared,
        resume_session_id,
        initial_input,
        startup_notice,
        pending_telemetry_notice,
        plugin_registry,
    )
    .await
}

struct PreparedInteractiveConfig {
    config: Config,
    workspace: PathBuf,
}

fn prepare_interactive_config(
    cli: &Cli,
    config: &Config,
    resuming: bool,
) -> Result<PreparedInteractiveConfig> {
    let workspace = cli
        .workspace
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Merge project-level config from $WORKSPACE/.codewhale/config.toml
    // or legacy $WORKSPACE/.deepseek/config.toml
    // unless --no-project-config was passed (#485).
    let mut merged_config = config.clone();
    merge_user_workspace_config(&mut merged_config, cli.config.clone(), &workspace);
    if !cli.no_project_config {
        let saved_permission_posture = crate::settings::Settings::load_persisted()
            .ok()
            .and_then(|settings| settings.permission_posture);
        merge_project_config_with_approval_baseline(
            &mut merged_config,
            &workspace,
            saved_permission_posture.as_deref(),
        )?;
    }
    if !resuming {
        let explicit_route_override = crate::config::explicit_launch_provider_override().is_some()
            || crate::config::explicit_launch_model_override().is_some();
        apply_selected_fleet_operator_for_launch(
            &mut merged_config,
            &workspace,
            explicit_route_override,
            false,
        )?;
    }
    Ok(PreparedInteractiveConfig {
        config: merged_config,
        workspace,
    })
}

async fn run_interactive_prepared(
    cli: &Cli,
    prepared: PreparedInteractiveConfig,
    resume_session_id: Option<String>,
    initial_input: Option<tui::InitialInput>,
    startup_notice: Option<String>,
    pending_telemetry_notice: Option<crate::telemetry_notice::PendingTelemetryNotice>,
    plugin_registry: Arc<crate::plugins::PluginRegistry>,
) -> Result<()> {
    let PreparedInteractiveConfig { config, workspace } = prepared;
    let config = &config;
    let initial_input = if cli.remote_control {
        Some(tui::InitialInput::RemoteControl)
    } else {
        initial_input
    };
    initialize_cloud_facts(config);

    if !cli.skip_onboarding {
        match crate::config::ensure_config_file_exists(cli.config.clone()) {
            Ok(Some(path)) => logging::info(format!(
                "Created first-run config file at {}",
                path.display()
            )),
            Ok(None) => {}
            Err(err) => logging::warn(format!("Failed to create first-run config file: {err}")),
        }
    }

    // v0.8.44: migrate config from ~/.deepseek/ to ~/.codewhale/ on first
    // launch. Non-fatal — existing installs keep working either way.
    match codewhale_config::migrate_config_if_needed() {
        Ok(Some(migration)) => {
            eprintln!("{}", migration.user_notice());
        }
        Ok(None) => {}
        Err(err) => logging::warn(format!("Config migration skipped: {err}")),
    }

    let model = config.default_model();
    let identity = config.active_provider_identity().ok();
    let max_subagents = cli.max_subagents.map_or_else(
        || {
            identity.as_ref().map_or(DEFAULT_MAX_SUBAGENTS, |identity| {
                config.max_subagents_for_provider(identity)
            })
        },
        |value| value.clamp(1, MAX_SUBAGENTS),
    );
    let screen_mode = startup_screen_mode(cli, config);
    let mouse_capture_preference = mouse_capture_preference(cli, config);
    let use_mouse_capture = screen_mode.mouse_capture(mouse_capture_preference);
    let use_bracketed_paste = crate::settings::Settings::load()
        .map(|s| s.effective_bracketed_paste())
        .unwrap_or_else(|_| !crate::settings::detected_legacy_windows_console_host());

    // Auto-install bundled system skills (e.g. skill-creator) on first launch.
    // Errors are non-fatal: log a warning and continue.
    let skills_dir = config.skills_dir();
    if let Err(e) = crate::skills::install_system_skills(&skills_dir) {
        logging::warn(format!("Failed to install system skills: {e}"));
    }

    startup_trace::mark("interactive_config");

    // Seed ProviderLake from the secret-free Models.dev disk cache before any
    // picker/inventory read, then kick a best-effort background refresh (#4187).
    // Failures are quiet: bundled catalog rows always remain available.
    crate::models_dev_live::maybe_load_persisted_cache();
    crate::models_dev_live::spawn_background_refresh();
    // Best-effort per-provider catalog refresh: fetches the active provider's
    // own /v1/models endpoint and merges live rows into the provider lake
    // alongside the Models.dev snapshot. Currently active for TelecomJS, whose
    // model list is not covered by the Models.dev catalog.
    crate::client::CodewhaleClient::spawn_active_provider_catalog_refresh(config);

    // Boot janitors — snapshot prune (7-day default), spillover prune
    // (#422), and managed-session cleanup (v0.8.44) — are best-effort disk
    // hygiene. On a large ~/.codewhale they were the dominant startup cost
    // (a git object walk plus thousands of stat/read calls), so they run on
    // a blocking worker while the TUI brings up its first frame (#3757).
    // All three were already documented as non-fatal.
    let snapshots = config.snapshots_config();
    let janitor_snapshots_enabled = snapshots.enabled;
    let janitor_max_age = snapshots.max_age();
    let janitor_workspace = workspace.clone();
    // Session cleanup races session restore: skip it entirely when a session
    // is being resumed/continued this launch (the just-resumed session could
    // be pruned before its first save bumps `updated_at`). It runs next
    // clean launch. When we do run it, exclude the explicit resume id too.
    let janitor_resume_id = resume_session_id.clone();
    let janitor_skip_session_cleanup = resume_session_id.is_some() || cli.continue_session;
    tokio::task::spawn_blocking(move || {
        if janitor_snapshots_enabled {
            session_manager::prune_workspace_snapshots(&janitor_workspace, janitor_max_age);
        }

        match crate::tools::truncate::prune_older_than(crate::tools::truncate::SPILLOVER_MAX_AGE) {
            Ok(0) => {}
            Ok(n) => tracing::debug!(
                target: "spillover",
                "boot prune removed {n} spillover file(s)"
            ),
            Err(err) => tracing::warn!(
                target: "spillover",
                ?err,
                "spillover prune skipped on boot"
            ),
        }

        if !janitor_skip_session_cleanup
            && let Ok(manager) = session_manager::SessionManager::default_location()
        {
            let _ = manager.cleanup_old_sessions_keeping(janitor_resume_id.as_deref());
        }

        // Cloud-dispatch orphan reconciliation: a previous TUI could quit
        // (or crash) with a detached cloud-agent runner in flight, leaving
        // an active job record — and possibly a billing sandbox — behind.
        // Stale active jobs are failed and torn down, then any
        // dispatch-labeled sandbox whose job no longer needs it is deleted
        // by label. Best effort: one bounded listing call when credentials
        // exist, fail-closed quiet otherwise, never fatal.
        if let Ok(store) = crate::cloud_dispatch::CloudJobStore::from_env() {
            let receipt = crate::dispatch_runner::startup_reconcile(
                &store,
                &crate::cloud_dispatch::LiveDaytonaLauncher,
            );
            if !receipt.is_empty() {
                logging::info(receipt);
            }
        }
    });

    // A launcher can forward `--yolo` to this binary via the CODEWHALE_YOLO
    // env var (config.yolo), not as a CLI flag. Honour either.
    let yolo = cli.yolo || config.yolo.unwrap_or(false);

    tui::run_tui(
        config,
        tui::TuiOptions {
            model,
            workspace,
            config_path: cli.config.clone(),
            config_profile: effective_config_profile(cli),
            allow_shell: interactive_tui_allow_shell(yolo, config),
            screen_mode,
            use_mouse_capture,
            mouse_capture_preference,
            use_bracketed_paste,
            skills_dir,
            memory_path: config.memory_path(),
            notes_path: config.notes_path(),
            mcp_config_path: config.mcp_config_path(),
            use_memory: config.memory_enabled(),
            start_in_agent_mode: yolo,
            skip_onboarding: cli.skip_onboarding,
            yolo, // YOLO mode auto-approves all tool executions
            resume_session_id,
            initial_input,
            startup_notice,
            max_subagents,
        },
        plugin_registry,
        pending_telemetry_notice,
    )
    .await
}

#[derive(Debug)]
struct CliAutoRoute {
    provider: crate::config::ProviderIdentity,
    model: String,
    reasoning_effort: Option<crate::reasoning_preference::ReasoningEffort>,
    /// Whether the runtime should continue resolving reasoning per prompt.
    ///
    /// This is independent from `auto_model`: an Auto model can carry a fixed
    /// saved effort, while a fixed Fleet model can still request Auto effort.
    auto_controls_reasoning: bool,
    auto_model: bool,
}

fn cli_reasoning_effort_value(
    config: &Config,
    model: &str,
    effort: crate::reasoning_preference::ReasoningEffort,
) -> Option<String> {
    let identity = config.active_provider_identity().ok()?;
    effort
        .api_value_for_route(
            identity.provider,
            &config.base_url_for_route(&identity),
            model,
        )
        .map(str::to_string)
}

fn cli_reasoning_effort_value_for_prompt(
    config: &Config,
    model: &str,
    effort: crate::reasoning_preference::ReasoningEffort,
) -> Option<String> {
    let resolved = if effort == crate::reasoning_preference::ReasoningEffort::Auto {
        crate::auto_reasoning::select()
    } else {
        effort
    };
    cli_reasoning_effort_value(config, model, resolved)
}

/// Review-pass reasoning effort: resolve `Auto` from the prompt exactly as the
/// ordinary CLI path does, then bound the result so hidden reasoning cannot
/// consume the whole shared output allowance (#6285).
fn review_reasoning_effort_value_for_prompt(
    config: &Config,
    model: &str,
    effort: crate::reasoning_preference::ReasoningEffort,
    reserve_percent: u32,
) -> Option<String> {
    let resolved = if effort == crate::reasoning_preference::ReasoningEffort::Auto {
        crate::auto_reasoning::select()
    } else {
        effort
    };
    let bounded = crate::tools::review::bounded_review_reasoning_effort(resolved, reserve_percent);
    cli_reasoning_effort_value(config, model, bounded)
}

fn normalize_cli_reasoning_effort(value: &str) -> Result<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "inherit" | "parent" | "same" | "current" | "default" | "unset"
    ) {
        return Ok(None);
    }
    crate::reasoning_preference::ReasoningEffort::parse_strict(trimmed)
        .map(|effort| Some(effort.as_setting().to_string()))
        .map_err(anyhow::Error::msg)
}

fn config_for_cli_route(config: &Config, route: &CliAutoRoute) -> Result<Config> {
    let mut execution_config = config.clone();
    execution_config
        .scope_to_provider_identity(&route.provider)
        .map_err(anyhow::Error::msg)?;
    execution_config.set_provider_model_override(&route.provider, Some(route.model.clone()))?;
    if route.provider.provider == crate::config::ProviderKind::Deepseek {
        execution_config.default_text_model = Some(route.model.clone());
    }
    Ok(execution_config)
}

async fn resolve_cli_auto_route(
    config: &Config,
    model: &str,
    prompt: &str,
) -> Result<CliAutoRoute> {
    let active = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    if model.trim().eq_ignore_ascii_case("auto") {
        let selection =
            model_routing::resolve_auto_route_with_inventory(config, prompt, "", "auto", "auto")
                .await?;
        let preference = config
            .reasoning_effort()
            .filter(|_| config.reasoning_effort_is_explicit())
            .map(crate::reasoning_preference::ReasoningEffort::from_setting);
        let (reasoning_effort, auto_controls_reasoning) =
            model_routing::resolve_auto_model_reasoning(preference, selection.reasoning_effort);
        Ok(CliAutoRoute {
            provider: selection.provider,
            model: selection.model,
            reasoning_effort,
            auto_controls_reasoning,
            auto_model: true,
        })
    } else {
        if let Some(selection) = model_routing::resolve_explicit_route_with_inventory(config, model)
        {
            let auto_controls_reasoning = matches!(
                selection.reasoning_effort,
                Some(crate::reasoning_preference::ReasoningEffort::Auto)
            );
            return Ok(CliAutoRoute {
                provider: selection.provider,
                model: selection.model,
                reasoning_effort: selection.reasoning_effort,
                auto_controls_reasoning,
                auto_model: false,
            });
        }

        let candidate_providers = model_routing::explicit_route_candidate_providers(config, model);
        if !candidate_providers.is_empty() && !candidate_providers.contains(&active) {
            let providers = candidate_providers
                .iter()
                .map(|provider| provider.key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "model `{model}` is available from configured provider route(s): {providers}. \
                 Pass `--provider <provider>` with `--model {model}` to choose one explicitly. \
                 In the TUI, use `/provider`, `/model`, or `/setup` to resolve the route before sending."
            );
        }

        // When --model is not `auto`, fall back to the reasoning_effort
        // declared in the user's config.toml. The previous hard-coded `None`
        // silently dropped the user's setting on every non-auto-route exec
        // call, which (for example) prevented vllm + Qwen3 users from
        // disabling thinking via `reasoning_effort = "off"` and caused
        // 30+ second SSE idle timeouts on trivial prompts.
        let reasoning_effort = config
            .reasoning_effort()
            .map(crate::reasoning_preference::ReasoningEffort::from_setting);
        Ok(CliAutoRoute {
            provider: active,
            model: model.to_string(),
            auto_controls_reasoning: matches!(
                reasoning_effort,
                Some(crate::reasoning_preference::ReasoningEffort::Auto)
            ),
            reasoning_effort,
            auto_model: false,
        })
    }
}

async fn resolve_cli_exec_route(
    config: &Config,
    model: &str,
    prompt: &str,
    force_configured_route: bool,
) -> Result<CliAutoRoute> {
    if force_configured_route && !model.trim().eq_ignore_ascii_case("auto") {
        let reasoning_effort = config
            .reasoning_effort()
            .map(crate::reasoning_preference::ReasoningEffort::from_setting);
        return Ok(CliAutoRoute {
            provider: config
                .active_provider_identity()
                .map_err(anyhow::Error::msg)?,
            model: model.to_string(),
            auto_controls_reasoning: matches!(
                reasoning_effort,
                Some(crate::reasoning_preference::ReasoningEffort::Auto)
            ),
            reasoning_effort,
            auto_model: false,
        });
    }
    resolve_cli_auto_route(config, model, prompt).await
}

fn should_force_configured_exec_route(
    resuming: bool,
    explicit_provider: Option<&str>,
    explicit_model: Option<&str>,
) -> bool {
    // A configured/default model belongs to the configured provider route.
    // Cross-provider inventory inference is reserved for an explicit model
    // override without an explicit provider. Resume remains route-authoritative
    // even when its model is overridden because it restores the saved provider.
    resuming || explicit_provider.is_some() || explicit_model.is_none()
}

fn exec_stream_provider_route(
    identity: &crate::config::ProviderIdentity,
) -> (String, Option<String>) {
    let provider = identity.persisted_kind().to_string();
    let provider_id = if identity.provider == crate::config::ProviderKind::Custom {
        identity.persisted_id().map(str::to_string)
    } else {
        None
    };
    (provider, provider_id)
}

#[derive(serde::Serialize)]
struct ExecStreamMeta {
    receipt_kind: &'static str,
    provider: String,
    /// Exact configured provider-table id, when one selected the route.
    /// `None` deliberately distinguishes the legacy idless root custom route
    /// from literal `[providers.custom]`, whose exact id is `"custom"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    provider_id: Option<String>,
    model: String,
    route_source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_hit_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_miss_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_write_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_tokens: Option<u32>,
    /// Resolved output ceiling the route actually requested (post-catalogue).
    #[serde(skip_serializing_if = "Option::is_none")]
    codewhale_max_output_tokens: Option<u32>,
    /// Provenance of that ceiling: `documented`, `uncatalogued`, or
    /// `route-declared`.
    #[serde(skip_serializing_if = "Option::is_none")]
    codewhale_max_output_tokens_source: Option<&'static str>,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_count: Option<u32>,
    approval_posture: String,
    sandbox_posture: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    binary_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_sha256: Option<String>,
    prompt_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_catalog_sha256: Option<String>,
    input_analysis: ExecStreamInputAnalysis,
    /// Real character count of the visible final answer, before any bound.
    visible_final_answer_chars: usize,
    /// Bounded, secret-redacted excerpt of the visible final answer (see
    /// [`exec_stream_final_answer_excerpt`]). Omitted when the run produced
    /// no visible answer.
    #[serde(skip_serializing_if = "String::is_empty")]
    visible_final_answer_excerpt: String,
    session_id: String,
    resume_command: String,
    workspace: String,
    message_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    termination_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Default, Clone, serde::Serialize, PartialEq, Eq)]
struct ExecStreamInputAnalysis {
    estimated_request_tokens: usize,
    estimated_message_content_tokens: usize,
    estimated_system_tokens: usize,
    estimated_framing_tokens: usize,
    user_message_count: usize,
    assistant_message_count: usize,
    tool_message_count: usize,
    tool_use_count: usize,
    tool_result_count: usize,
    text_chars: usize,
    thinking_chars: usize,
    tool_use_input_chars: usize,
    tool_result_chars: usize,
    text_estimated_tokens: usize,
    thinking_estimated_tokens: usize,
    tool_use_input_estimated_tokens: usize,
    tool_result_estimated_tokens: usize,
}

#[derive(serde::Serialize)]
#[serde(tag = "type")]
// Keep receipts flat for stable JSONL consumers. Boxing the whole tool_result
// payload would introduce a nested object and break the stream schema.
#[allow(clippy::large_enum_variant)]
enum ExecStreamEvent {
    #[serde(rename = "status")]
    Status { message: String },
    #[serde(rename = "content")]
    Content { content: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        name: String,
        id: String,
        input: serde_json::Value,
        started_at: String,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        id: String,
        name: String,
        output: String,
        status: String,
        started_at: String,
        completed_at: String,
        duration_ms: u64,
        side_effect_status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error_category: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        truncated: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        result_metadata: Option<serde_json::Value>,
    },
    /// A sub-agent was launched, and the model it was launched on.
    ///
    /// Without this, a delegated child is invisible to anything reading the
    /// stream: a parent turn on one route could spawn children billed on
    /// another and the only place it surfaced was the invoice. That is not
    /// hypothetical — the `Fast` loadout re-priced scout children onto a
    /// cheaper sibling until it was fixed, and nothing in the output said so.
    #[serde(rename = "agent_spawned")]
    AgentSpawned {
        id: String,
        model: String,
        spawn_depth: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_run_id: Option<String>,
        /// Why the child got this route, when the spawn path resolved one.
        #[serde(skip_serializing_if = "Option::is_none")]
        route_source: Option<String>,
    },
    #[serde(rename = "sandbox_denied")]
    SandboxDenied {
        tool_id: String,
        tool_name: String,
        reason: String,
        outcome: String,
    },
    #[serde(rename = "workflow_event")]
    WorkflowEvent {
        run_id: String,
        event: serde_json::Value,
    },
    #[serde(rename = "session_capture")]
    SessionCapture {
        /// Redacted fingerprint for logs/forensics, the same value the
        /// terminal `metadata.session_id` carries; never the recoverable id.
        content: String,
        /// The real saved-session id a caller can resolve via
        /// `GET /v1/sessions/{id}` to read the worker's full transcript. This
        /// is the only place the exec stream carries the raw id: `metadata`
        /// stays fingerprint-only so a captured terminal receipt is safe to
        /// log on its own.
        saved_session_id: String,
    },
    #[serde(rename = "service_released")]
    #[cfg(unix)]
    ServiceReleased {
        task_id: String,
        pid: u32,
        process_group_id: u32,
        ownership: String,
    },
    /// Per-model-call usage receipt. Field names mirror the terminal
    /// `metadata` receipt (`prompt_cache_hit_tokens` is the provider's
    /// cache-read count, `prompt_cache_write_tokens` the cache-creation
    /// count). Optional fields are omitted — never emitted as null or zero —
    /// when the provider does not report them; the whole event is skipped
    /// for model calls whose provider reported no usage at all.
    #[serde(rename = "turn_usage")]
    TurnUsage {
        /// 1-based index of the model call within this exec run.
        turn: u32,
        input_tokens: u32,
        output_tokens: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_hit_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_miss_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        prompt_cache_write_tokens: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_replay_tokens: Option<u32>,
        duration_ms: u64,
    },
    #[serde(rename = "metadata")]
    Metadata { meta: Box<ExecStreamMeta> },
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "error")]
    Error { error: String },
}

fn exec_sandbox_elevation_authorized(
    allow_sandbox_elevation: bool,
    explicit_sandbox: Option<&str>,
) -> bool {
    allow_sandbox_elevation
        || explicit_sandbox.is_some_and(|policy| policy.eq_ignore_ascii_case("danger-full-access"))
}

fn emit_exec_stream_event(event: &ExecStreamEvent) -> Result<()> {
    let mut line = serde_json::to_string(&exec_stream_value(event)?)?;
    line.push('\n');
    write_exec_stdout(&line)
}

/// Headless `exec` ignores SIGPIPE while it runs, because it writes to pipes
/// it does not own: a stdio MCP server, LSP, hook or shell child that exits
/// early must fail that one write with `EPIPE`, not kill the run with no
/// output. Under the default disposition, an MCP server whose interpreter
/// could not start (a broken `node` on PATH for the built-in Computer Use
/// plugin) made `exec --auto` exit 141 before printing anything.
fn ignore_sigpipe_for_headless_exec() {
    // SAFETY: a plain disposition change with no handler. Children still start
    // with SIGPIPE at SIG_DFL: the standard library resets it before exec.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// Write exec output to stdout. SIGPIPE is ignored during exec (see
/// [`ignore_sigpipe_for_headless_exec`]), so a reader that closed stdout
/// (`codewhale exec ... | head -1`) surfaces here as `BrokenPipe`. End the
/// process the way the default disposition would have (#4030) instead of
/// panicking inside `print!`.
fn write_exec_stdout(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => {
            // SAFETY: restores the default disposition and re-raises the
            // signal the write would have delivered without SIG_IGN.
            #[cfg(unix)]
            unsafe {
                libc::signal(libc::SIGPIPE, libc::SIG_DFL);
                libc::raise(libc::SIGPIPE);
            }
            std::process::exit(141);
        }
        result => result.map_err(Into::into),
    }
}

/// Process exit code `codewhale exec` uses when a turn ends on a retryable
/// infrastructure failure (provider/transport) rather than a genuine task
/// failure. 75 is `EX_TEMPFAIL` from sysexits.h — "temporary failure; the
/// invocation is expected to succeed on retry" — so bench harnesses and
/// supervisors can distinguish retryable infra exits from genuine task
/// failures (exit 1) without parsing the stream-json metadata.
const EXEC_EXIT_RETRYABLE_INFRA: i32 = 75; // EX_TEMPFAIL

/// Map a terminal exec error category to the process exit code.
///
/// `network` / `timeout` mean the provider connection dropped or stalled
/// after every in-session retry budget was exhausted: the task itself
/// neither passed nor failed, and re-running the same command is safe.
/// `rate_limit` is deliberately NOT mapped to the retryable code — the same
/// category also covers quota exhaustion, which a blind retry would hammer.
fn exec_failure_exit_code(error_category: Option<&str>) -> i32 {
    match error_category {
        Some("network" | "timeout") => EXEC_EXIT_RETRYABLE_INFRA,
        _ => 1,
    }
}

/// Should a mid-turn engine error event force the final exec summary into
/// failure? Only non-recoverable envelopes do. Recoverable warnings (stream
/// stall notices, transient retry noise) are emitted on the stream for
/// visibility, but the terminal `TurnComplete` event carries the
/// authoritative turn outcome — a warning must never fail a run whose turn
/// later completes.
fn exec_error_event_is_fatal(envelope: &crate::error_taxonomy::ErrorEnvelope) -> bool {
    !envelope.recoverable
}

fn exec_stream_value(event: &ExecStreamEvent) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(event)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("schema_version".to_string(), serde_json::json!(1));
        object.insert(
            "schema".to_string(),
            serde_json::json!("codewhale.exec-stream"),
        );
    }
    Ok(value)
}

fn tool_error_receipt_category(error: &crate::tools::spec::ToolError) -> &'static str {
    use crate::tools::spec::ToolError;
    match error {
        ToolError::InvalidInput { .. } => "invalid_input",
        ToolError::MissingField { .. } => "missing_field",
        ToolError::PathEscape { .. } => "path_escape",
        ToolError::ExecutionFailed { .. } => "execution_failed",
        ToolError::Timeout { .. } => "timeout",
        ToolError::Cancelled { .. } => "cancelled",
        ToolError::NotAvailable { .. } => "not_available",
        ToolError::PermissionDenied { .. } => "permission_denied",
    }
}

fn tool_artifact_receipt(metadata: Option<&serde_json::Value>) -> Option<serde_json::Value> {
    let object = metadata?.as_object()?;
    let mut artifact = serde_json::Map::new();
    for key in [
        "artifact_id",
        "artifact_path",
        "artifact_relative_path",
        "artifact_byte_size",
        "spillover_path",
        "content_digest",
        "original_byte_count",
        "retained_head_bytes",
        "retained_tail_bytes",
    ] {
        if let Some(value) = object.get(key) {
            artifact.insert(key.to_string(), value.clone());
        }
    }
    (!artifact.is_empty()).then_some(serde_json::Value::Object(artifact))
}

fn current_binary_sha256() -> Option<String> {
    let bytes = std::fs::read(std::env::current_exe().ok()?).ok()?;
    Some(format!("sha256:{}", crate::hashing::sha256_hex(&bytes)))
}

async fn run_workflow_tool_command(
    cli: &Cli,
    args: WorkflowToolArgs,
    plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
) -> Result<()> {
    match run_workflow_tool_command_inner(cli, args, plugin_registry).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = emit_exec_stream_event(&ExecStreamEvent::Error {
                error: format!("{error:#}"),
            });
            exit_workflow_tool_failure();
        }
    }
}

async fn run_workflow_tool_command_inner(
    cli: &Cli,
    args: WorkflowToolArgs,
    plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
) -> Result<()> {
    use crate::tools::spec::ToolSpec;

    if args.approval_source != "explicit-workflow-command" {
        bail!("workflow-tool requires --approval-source explicit-workflow-command");
    }
    let input: serde_json::Value = serde_json::from_str(&args.input_json)
        .context("--input-json must be a valid Workflow tool input object")?;
    if !input.is_object() {
        bail!("--input-json must be a JSON object");
    }
    if !input
        .get("action")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|action| action.eq_ignore_ascii_case("run"))
    {
        bail!("workflow-tool accepts only action=run");
    }

    let workspace = resolve_workspace(cli);
    let mut config = load_config_from_cli(cli)?;
    merge_user_workspace_config(&mut config, cli.config.clone(), &workspace);
    if let Ok(env_url) =
        std::env::var("CODEWHALE_BASE_URL").or_else(|_| std::env::var("DEEPSEEK_BASE_URL"))
    {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() {
            let identity = config
                .active_provider_identity()
                .map_err(anyhow::Error::msg)?;
            config
                .set_provider_base_url_override(&identity, Some(trimmed.to_string()))
                .map_err(anyhow::Error::msg)?;
        }
    }

    initialize_cloud_facts(&config);
    let model = resolve_exec_model(&config, None);
    let route = resolve_cli_exec_route(
        &config,
        &model,
        "Run a checked-in Workflow through the host runtime",
        true,
    )
    .await?;
    let execution_config = config_for_cli_route(&config, &route)?;
    let route_identity = execution_config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)
        .context("workflow terminal route lost its exact provider identity")?;
    let (route_provider, route_provider_id) = exec_stream_provider_route(&route_identity);
    let workflow_input_sha256 = format!(
        "sha256:{}",
        crate::hashing::sha256_hex(&serde_json::to_vec(&input)?)
    );
    let tool_id = format!("workflow_host_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let tool_started = Instant::now();
    let tool_started_at = chrono::Utc::now().to_rfc3339();

    emit_exec_stream_event(&ExecStreamEvent::ToolUse {
        name: "workflow".to_string(),
        id: tool_id.clone(),
        input: input.clone(),
        started_at: tool_started_at.clone(),
    })?;

    let (event_tx, event_rx) = tokio::sync::mpsc::channel(1024);
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let event_forwarder = tokio::spawn(forward_direct_workflow_events(event_rx, stop_rx));
    let (tool, context) = match build_direct_workflow_tool(
        &execution_config,
        &route,
        &workspace,
        event_tx,
        plugin_registry,
    )
    .await
    {
        Ok(built) => built,
        Err(err) => {
            let _ = stop_tx.send(());
            let _ = event_forwarder.await;
            exit_workflow_tool_error(&tool_id, err.to_string());
        }
    };

    let result = tool.execute(input, &context).await;
    drop(tool);
    let _ = stop_tx.send(());
    event_forwarder
        .await
        .context("workflow event forwarder task failed")??;

    let result = match result {
        Ok(result) => result,
        Err(err) => {
            let error = err.to_string();
            exit_workflow_tool_error(&tool_id, error);
        }
    };

    let workflow_status =
        direct_workflow_status(&result.content).unwrap_or_else(|| "unknown".to_string());
    let completed = result.success && workflow_status == "completed";
    emit_exec_stream_event(&ExecStreamEvent::ToolResult {
        id: tool_id,
        name: "workflow".to_string(),
        output: result.content.clone(),
        status: if completed { "success" } else { "error" }.to_string(),
        started_at: tool_started_at,
        completed_at: chrono::Utc::now().to_rfc3339(),
        duration_ms: u64::try_from(tool_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        side_effect_status: result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("side_effect_status"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        error_category: (!completed).then(|| "tool_error".to_string()),
        truncated: result
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("truncated"))
            .and_then(serde_json::Value::as_bool),
        artifact: tool_artifact_receipt(result.metadata.as_ref()),
        result_metadata: result.metadata.clone(),
    })?;
    emit_exec_stream_event(&ExecStreamEvent::Metadata {
        meta: Box::new(ExecStreamMeta {
            receipt_kind: "terminal",
            provider: route_provider,
            provider_id: route_provider_id,
            // No parent/operator model call occurs on this host-owned path;
            // child model/provider usage remains attributable in typed task
            // receipts rather than being misreported as one root model.
            model: "host-workflow".to_string(),
            route_source: "host_workflow".to_string(),
            input_tokens: None,
            output_tokens: None,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            prompt_cache_write_tokens: None,
            reasoning_tokens: None,
            codewhale_max_output_tokens: None,
            codewhale_max_output_tokens_source: None,
            duration_ms: u64::try_from(tool_started.elapsed().as_millis()).unwrap_or(u64::MAX),
            retry_count: None,
            approval_posture: "explicit_workflow_command".to_string(),
            sandbox_posture: "configured".to_string(),
            binary_sha256: current_binary_sha256(),
            config_sha256: None,
            prompt_sha256: workflow_input_sha256,
            tool_catalog_sha256: None,
            input_analysis: ExecStreamInputAnalysis::default(),
            visible_final_answer_chars: result.content.chars().count(),
            visible_final_answer_excerpt: exec_stream_final_answer_excerpt(&result.content),
            session_id: String::new(),
            resume_command: String::new(),
            workspace: workspace.display().to_string(),
            message_count: 0,
            status: Some(workflow_status.clone()),
            termination_reason: Some(if completed { "resolved" } else { "tool_error" }.to_string()),
            error_category: (!completed).then(|| "tool".to_string()),
            error: (!completed)
                .then(|| format!("workflow run ended with terminal status {workflow_status}")),
        }),
    })?;
    if !completed {
        let error = format!("workflow run ended with terminal status {workflow_status}");
        emit_exec_stream_event(&ExecStreamEvent::Error {
            error: error.clone(),
        })?;
        exit_workflow_tool_failure();
    }
    emit_exec_stream_event(&ExecStreamEvent::Done)?;
    Ok(())
}

fn exit_workflow_tool_failure() -> ! {
    let _ = io::stdout().flush();
    std::process::exit(1)
}

fn exit_workflow_tool_error(tool_id: &str, error: String) -> ! {
    let now = chrono::Utc::now().to_rfc3339();
    let _ = emit_exec_stream_event(&ExecStreamEvent::ToolResult {
        id: tool_id.to_string(),
        name: "workflow".to_string(),
        output: error.clone(),
        status: "error".to_string(),
        started_at: now.clone(),
        completed_at: now,
        duration_ms: 0,
        side_effect_status: "unknown".to_string(),
        error_category: Some("execution_failed".to_string()),
        truncated: None,
        artifact: None,
        result_metadata: None,
    });
    let _ = emit_exec_stream_event(&ExecStreamEvent::Error { error });
    exit_workflow_tool_failure()
}

async fn initialize_direct_workflow_mcp_pool(
    config: &Config,
    workspace: &Path,
    network_policy: Option<crate::network_policy::NetworkPolicyDecider>,
    plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
) -> Option<(
    std::sync::Arc<tokio::sync::Mutex<crate::mcp::McpPool>>,
    Vec<(String, String)>,
)> {
    if !config.features().enabled(Feature::Mcp) {
        return None;
    }
    let mut pool = crate::mcp::McpPool::from_config_path_with_workspace_and_plugins(
        &config.mcp_config_path(),
        workspace,
        plugin_registry,
    )
    .unwrap_or_else(|error| {
        tracing::debug!("No MCP config for direct Workflow runtime: {error:#}");
        crate::mcp::McpPool::new(crate::mcp::McpConfig::default())
    });
    pool = pool.with_backend(crate::mcp::McpBackend::from_config(config));
    if let Some(policy) = network_policy {
        pool = pool.with_network_policy(policy);
    }
    let failures = pool
        .connect_all()
        .await
        .into_iter()
        .map(|(server, error)| (server, format!("{error:#}")))
        .collect();
    Some((std::sync::Arc::new(tokio::sync::Mutex::new(pool)), failures))
}

async fn build_direct_workflow_tool(
    config: &Config,
    route: &CliAutoRoute,
    workspace: &Path,
    event_tx: tokio::sync::mpsc::Sender<crate::core::events::Event>,
    plugin_registry: std::sync::Arc<crate::plugins::PluginRegistry>,
) -> Result<(
    crate::tools::workflow::WorkflowTool,
    crate::tools::ToolContext,
)> {
    use std::sync::Arc;

    use crate::client::CodewhaleClient;
    use crate::core::authority::shell_policy_for_mode;
    use crate::tools::AgentToolSurfaceOptions;
    use crate::tools::goal::new_shared_goal_state;
    use crate::tools::subagent::{SubAgentRuntime, new_shared_subagent_manager_with_timeout};
    use crate::tools::todo::new_shared_todo_list;
    use codewhale_config::AppMode;
    use codewhale_execpolicy::ApprovalMode;

    let identity = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    if !config.subagents_enabled_for_provider(&identity) {
        bail!(
            "Workflow dispatch requires sub-agents for provider {} ({})",
            identity.key.as_str(),
            config
                .subagents_disabled_reason()
                .unwrap_or("provider-specific sub-agent configuration disabled it")
        );
    }

    let yolo = config.yolo.unwrap_or(false);
    let mode = AppMode::Operate;
    let allow_shell = yolo || config.allow_shell();
    let shell_policy = shell_policy_for_mode(mode, allow_shell);
    let trusted = crate::workspace_trust::WorkspaceTrust::load_for(workspace);
    let mut context = crate::tools::ToolContext::with_auto_approve(
        workspace.to_path_buf(),
        yolo,
        config.notes_path(),
        config.mcp_config_path(),
        yolo,
    )
    .with_features(config.features())
    .with_skills_config(
        config.skills_dir(),
        crate::skills::SkillDiscoveryMode::from_config(&config.skills_config()),
    )
    .with_plugin_registry(std::sync::Arc::clone(&plugin_registry))
    .with_shell_policy(shell_policy)
    .with_trusted_external_paths(trusted.paths().to_vec())
    .with_elevated_sandbox_policy(crate::core::authority::sandbox_policy_for_turn(
        mode,
        if yolo {
            ApprovalMode::Bypass
        } else {
            ApprovalMode::Suggest
        },
        config.sandbox_mode.as_deref(),
        workspace,
        crate::core::authority::SandboxNetworkAccess::from_config(config.sandbox_network_access),
    ));
    let network_policy = config.network.clone().map(|network| {
        crate::network_policy::NetworkPolicyDecider::with_default_audit(network.into_runtime())
    });
    if let Some(policy) = network_policy.as_ref() {
        context = context.with_network_policy(policy.clone());
    }
    if config.memory_enabled() {
        context.memory_path = Some(config.memory_path());
    }
    context.search_provider = config.search_provider();
    context.search_api_key = config
        .search
        .as_ref()
        .and_then(|search| search.api_key.clone());
    context.search_base_url = config
        .search
        .as_ref()
        .and_then(|search| search.base_url.clone());
    if let Some(backend) = crate::sandbox::backend::create_backend(config)? {
        context = context.with_sandbox_backend(Arc::from(backend));
    }

    let max_subagents = config.max_subagents_for_provider(&identity);
    let manager = new_shared_subagent_manager_with_timeout(
        workspace.to_path_buf(),
        max_subagents,
        config
            .max_admitted_subagents_for_provider(&identity)
            .max(max_subagents),
        Duration::from_secs(config.subagent_heartbeat_timeout_secs_for_provider(&identity)),
        config.launch_concurrency_for_provider(&identity),
    );
    let roster = Arc::new(crate::fleet::identity::load_effective_roster(
        &config.fleet_config(),
        workspace,
        Some(plugin_registry.as_ref()),
    ));
    let mut role_models = roster.model_overrides();
    role_models.extend(config.subagent_model_overrides());

    let features = config.features();
    let mut surface = AgentToolSurfaceOptions::new(shell_policy);
    surface.apply_patch_enabled = features.enabled(Feature::ApplyPatch);
    surface.web_search_enabled = features.enabled(Feature::WebSearch);
    surface.memory_tool_enabled = config.memory_enabled();
    surface.vision_config = features
        .enabled(Feature::VisionModel)
        .then(|| config.vision_model_config())
        .flatten();
    surface.speech_output_dir = config.speech_output_dir();
    surface.goal_state = Some(new_shared_goal_state());

    let client = CodewhaleClient::new(config)?;
    // A FIXED model with `reasoning_effort = auto` (the shape a Fleet worker
    // subprocess launches with: `--model <exact> --reasoning-effort auto`) is
    // still Auto. Deriving the auto flag from `route.auto_model` alone left it
    // raw AND non-auto: the runtime carried the literal string `"auto"` while
    // nothing was allowed to resolve it. Auto is a reasoning decision, not a
    // model decision — it does not require `--model auto`.
    let reasoning_effort_auto = route.auto_controls_reasoning;
    let reasoning_effort = route
        .reasoning_effort
        .and_then(|effort| cli_reasoning_effort_value(config, &route.model, effort));
    let mcp_pool = if let Some((pool, failures)) =
        initialize_direct_workflow_mcp_pool(config, workspace, network_policy, plugin_registry)
            .await
    {
        for (server, error) in failures {
            tracing::warn!(
                server = %server,
                error = %error,
                "direct Workflow runtime could not connect MCP server"
            );
        }
        Some(pool)
    } else {
        None
    };
    let fleet_governor = manager.read().await.rate_limit_governor();
    let runtime = SubAgentRuntime::new(
        client,
        route.model.clone(),
        context.clone(),
        allow_shell,
        Some(event_tx),
        manager.clone(),
    )
    .with_fleet_governor(fleet_governor)
    .with_locale_tag(
        codewhale_localization::resolve_locale(
            &crate::settings::Settings::load_persisted()
                .unwrap_or_default()
                .locale,
        )
        .tag(),
    )
    .with_role_models(role_models)
    .with_api_config(config.clone())
    .with_auto_model(route.auto_model)
    .with_reasoning_effort(reasoning_effort, reasoning_effort_auto)
    .with_agent_tool_surface_options(surface)
    .with_max_spawn_depth(config.subagent_max_spawn_depth_for_provider(&identity))
    .with_step_api_timeout(Duration::from_secs(
        config.subagent_api_timeout_secs_for_provider(&identity),
    ))
    .with_speech_output_dir(config.speech_output_dir())
    .with_mcp_pool(mcp_pool)
    .with_todos(new_shared_todo_list())
    .with_parent_mode(mode);

    Ok((
        crate::tools::workflow::WorkflowTool::new(manager, runtime).with_explicit_cli_approval(),
        context,
    ))
}

async fn forward_direct_workflow_events(
    mut event_rx: tokio::sync::mpsc::Receiver<crate::core::events::Event>,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    loop {
        tokio::select! {
            biased;
            event = event_rx.recv() => match event {
                Some(event) => emit_direct_workflow_event(event)?,
                None => return Ok(()),
            },
            _ = &mut stop_rx => {
                while let Ok(event) = event_rx.try_recv() {
                    emit_direct_workflow_event(event)?;
                }
                return Ok(());
            }
        }
    }
}

fn emit_direct_workflow_event(event: crate::core::events::Event) -> Result<()> {
    if let crate::core::events::Event::WorkflowUi { run_id, event, .. } = event {
        emit_exec_stream_event(&ExecStreamEvent::WorkflowEvent { run_id, event })?;
    }
    Ok(())
}

fn direct_workflow_status(content: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()?
        .get("status")?
        .as_str()
        .map(str::to_ascii_lowercase)
}

fn exec_stream_input_analysis(
    messages: &[Message],
    system: Option<&SystemPrompt>,
) -> ExecStreamInputAnalysis {
    let mut analysis = ExecStreamInputAnalysis {
        estimated_request_tokens: crate::compaction::estimate_input_tokens_conservative(
            messages, system,
        ),
        estimated_message_content_tokens: crate::compaction::estimate_tokens(messages),
        estimated_system_tokens: exec_stream_estimate_system_tokens(system),
        estimated_framing_tokens: messages.len().saturating_mul(12).saturating_add(48),
        ..ExecStreamInputAnalysis::default()
    };

    for message in messages {
        match message.role.as_str() {
            "user" => analysis.user_message_count += 1,
            "assistant" => analysis.assistant_message_count += 1,
            "tool" => analysis.tool_message_count += 1,
            _ => {}
        }

        for block in &message.content {
            match block {
                ContentBlock::Text { text, .. } => {
                    exec_stream_add_text_estimate(
                        text,
                        &mut analysis.text_chars,
                        &mut analysis.text_estimated_tokens,
                    );
                }
                ContentBlock::Thinking { thinking, .. } => {
                    exec_stream_add_text_estimate(
                        thinking,
                        &mut analysis.thinking_chars,
                        &mut analysis.thinking_estimated_tokens,
                    );
                }
                ContentBlock::ToolUse { input, .. } | ContentBlock::ServerToolUse { input, .. } => {
                    analysis.tool_use_count += 1;
                    exec_stream_add_json_estimate(
                        input,
                        &mut analysis.tool_use_input_chars,
                        &mut analysis.tool_use_input_estimated_tokens,
                    );
                }
                ContentBlock::ToolResult {
                    content,
                    content_blocks,
                    ..
                } => {
                    analysis.tool_result_count += 1;
                    exec_stream_add_text_estimate(
                        content,
                        &mut analysis.tool_result_chars,
                        &mut analysis.tool_result_estimated_tokens,
                    );
                    if let Some(blocks) = content_blocks {
                        exec_stream_add_json_estimate(
                            blocks,
                            &mut analysis.tool_result_chars,
                            &mut analysis.tool_result_estimated_tokens,
                        );
                    }
                }
                ContentBlock::ToolSearchToolResult { content, .. }
                | ContentBlock::CodeExecutionToolResult { content, .. } => {
                    analysis.tool_result_count += 1;
                    exec_stream_add_json_estimate(
                        content,
                        &mut analysis.tool_result_chars,
                        &mut analysis.tool_result_estimated_tokens,
                    );
                }
                ContentBlock::ImageUrl { .. } => {}
            }
        }
    }

    analysis
}

fn exec_stream_add_text_estimate(text: &str, chars: &mut usize, tokens: &mut usize) {
    *chars = chars.saturating_add(text.chars().count());
    *tokens = tokens.saturating_add(crate::compaction::estimate_text_tokens_conservative(text));
}

fn exec_stream_add_json_estimate<T: serde::Serialize>(
    value: &T,
    chars: &mut usize,
    tokens: &mut usize,
) {
    let text = serde_json::to_string(value).unwrap_or_default();
    exec_stream_add_text_estimate(&text, chars, tokens);
}

fn exec_stream_estimate_system_tokens(system: Option<&SystemPrompt>) -> usize {
    match system {
        Some(SystemPrompt::Text(text)) => {
            crate::compaction::estimate_text_tokens_conservative(text)
        }
        Some(SystemPrompt::Blocks(blocks)) => blocks
            .iter()
            .map(|block| crate::compaction::estimate_text_tokens_conservative(&block.text))
            .sum(),
        None => 0,
    }
}

fn exec_saved_session_line(session_id: &str) -> String {
    format!("session: {}", truncate_id(session_id))
}

fn exec_resumed_session_line(session_id: &str) -> String {
    format!("resumed session: {}", truncate_id(session_id))
}

fn exec_stream_session_ref(session_id: &str) -> String {
    crate::utils::redacted_identifier_for_log(session_id)
}

/// Resume hint for the terminal `metadata` receipt. `metadata` carries only
/// the session fingerprint, so the hint names the `session_capture` field
/// that holds the recoverable id instead of pretending to redact one.
fn exec_stream_resume_hint(session_id: &str) -> String {
    if session_id.trim().is_empty() {
        String::new()
    } else {
        "codewhale exec --resume <session_capture.saved_session_id>".to_string()
    }
}

/// The final visible assistant reply for the terminal receipt: the text
/// blocks of the last assistant-like message after the current user prompt.
/// Tool results also use the user role, so they must not start a new turn.
/// A resumed session can be synchronized before its new prompt is accepted;
/// without current output its old answer must never become a new deliverable.
fn exec_stream_final_answer_text(
    messages: &[Message],
    current_turn_has_output: bool,
) -> Option<String> {
    if !current_turn_has_output {
        return None;
    }
    let turn_start = messages.iter().rposition(|message| {
        message.role == Role::User
            && !message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
    })?;
    let text = messages
        .iter()
        .skip(turn_start + 1)
        .rev()
        .find(|message| message.role.is_assistant_like())?
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

#[derive(Clone, Copy)]
struct PersistedProviderRoute<'a> {
    kind: &'a str,
    id: Option<&'a str>,
}

fn persist_exec_session(
    messages: &[Message],
    model: &str,
    provider_route: PersistedProviderRoute<'_>,
    workspace: &Path,
    system_prompt: &Option<SystemPrompt>,
    session_id: Option<&str>,
    total_tokens: u64,
    session_manager: Option<&SessionManager>,
) -> Result<String> {
    let default_manager;
    let manager = if let Some(manager) = session_manager {
        manager
    } else {
        default_manager = SessionManager::default_location()
            .context("could not open session manager for save")?;
        &default_manager
    };
    let mut saved = if let Some(id) = session_id.filter(|id| !id.trim().is_empty()) {
        match manager.load_session(id) {
            Ok(existing) => session_manager::update_session(
                existing,
                messages,
                total_tokens,
                system_prompt.as_ref(),
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                session_manager::create_saved_session_with_id_and_mode(
                    id.to_string(),
                    messages,
                    model,
                    workspace,
                    total_tokens,
                    system_prompt.as_ref(),
                    Some("exec"),
                )
            }
            Err(err) => return Err(err).context("could not load existing exec session"),
        }
    } else {
        session_manager::create_saved_session_with_mode(
            messages,
            model,
            workspace,
            total_tokens,
            system_prompt.as_ref(),
            Some("exec"),
        )
    };
    stamp_exec_session_metadata(
        &mut saved,
        model,
        provider_route.kind,
        provider_route.id,
        workspace,
    );
    let id = saved.metadata.id.clone();
    manager
        .save_session(&saved)
        .context("could not save exec session")?;
    Ok(id)
}

fn stamp_exec_session_metadata(
    saved: &mut session_manager::SavedSession,
    model: &str,
    model_provider_kind: &str,
    model_provider_id: Option<&str>,
    workspace: &Path,
) {
    saved.metadata.model = model.to_string();
    saved
        .metadata
        .set_model_provider_route(model_provider_kind, model_provider_id);
    saved.metadata.workspace = workspace.to_path_buf();
    saved.metadata.mode = Some("exec".to_string());
}

#[derive(serde::Serialize)]
struct ExecToolEntry {
    name: String,
    success: bool,
    output: String,
}

#[derive(serde::Serialize)]
struct ExecOutcome {
    kind: String,
    outcome: String,
    tool_name: String,
    reason: String,
}

#[derive(serde::Serialize, Default)]
struct ExecSummary {
    mode: String,
    provider: String,
    model: String,
    prompt: String,
    output: String,
    tools: Vec<ExecToolEntry>,
    outcomes: Vec<ExecOutcome>,
    status: Option<String>,
    termination_reason: Option<String>,
    error_category: Option<String>,
    error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    released_services: Vec<crate::tools::shell::PersistentServiceReceipt>,
    /// One-shot (`mode: "one-shot"`) receipt fields kept from the pre-#6510
    /// direct-call path: whether the turn completed without error, and its
    /// provider-reported usage. Absent on agent receipts.
    #[serde(skip_serializing_if = "Option::is_none")]
    success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<codewhale_models::Usage>,
}

impl ExecSummary {
    /// Fill the one-shot receipt fields from the settled turn: success means a
    /// completed status with no error; usage is what the provider reported.
    fn record_one_shot_outcome(&mut self, usage: Option<codewhale_models::Usage>) {
        self.success = Some(self.error.is_none() && self.status.as_deref() == Some("completed"));
        self.usage = usage;
    }
}

fn validate_exec_tool_authority_resume(
    tool_authority_json: Option<&str>,
    resuming: bool,
) -> Result<()> {
    if tool_authority_json.is_some() && resuming {
        bail!(
            "Fleet tool authority cannot be combined with exec --resume, --session-id, or --continue"
        );
    }
    Ok(())
}

fn exec_network_policy(
    config: &Config,
    outer_network_access: Option<bool>,
) -> Option<crate::network_policy::NetworkPolicyDecider> {
    // Fleet caps are an outer authority boundary: user configuration may
    // narrow them further, but it may never widen an explicit network denial.
    if outer_network_access == Some(false) {
        // A Fleet denial is an outer authority the user's document may never
        // widen, so mark the decider authoritative: a mid-session re-read of
        // that document folds onto it instead of replacing it.
        return Some(
            crate::network_policy::NetworkPolicyDecider::new(
                crate::network_policy::NetworkPolicy {
                    default: crate::network_policy::DecisionToml::Deny,
                    ..crate::network_policy::NetworkPolicy::default()
                },
                None,
            )
            .with_authoritative(),
        );
    }
    config.network.clone().map(|toml_cfg| {
        crate::network_policy::NetworkPolicyDecider::with_default_audit(toml_cfg.into_runtime())
    })
}

fn apply_fleet_engine_feature_caps(
    features: &mut crate::features::Features,
    fleet_authority_active: bool,
    outer_network_access: Option<bool>,
    shell_authority: crate::tools::spec::ToolShellAuthority,
) {
    if fleet_authority_active {
        features.disable(crate::features::Feature::Subagents);
        features.disable(crate::features::Feature::Mcp);
        if shell_authority != crate::tools::spec::ToolShellAuthority::ReadOnly {
            features.disable(crate::features::Feature::ShellTool);
        }
    }
    if outer_network_access == Some(false) {
        features.disable(crate::features::Feature::WebSearch);
    }
}

/// Resolve the optional headless safety budget without imposing a hidden
/// default. Benchmarks and other long-running exec callers continue until the
/// model finishes unless they opt into a finite `--max-turns` value.
/// Watch stdin for EOF — the manager closed the pipe because it died — and
/// terminate our own process group. Fleet workers run in their own session
/// (setsid, host.rs), so nothing else kills them after a parent crash; the
/// same guard makes a task-timeout worker stop its own tree deterministically
/// (R7). Windows workers are reaped by the host's Job Object instead, so the
/// watcher is a Unix-only concern.
fn spawn_parent_death_watch() {
    #[cfg(not(unix))]
    {
        return;
    }
    #[cfg(unix)]
    std::thread::Builder::new()
        .name("parent-death-watch".to_string())
        .spawn(|| {
            use std::io::Read as _;
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 512];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => continue,
                }
            }
            tracing::info!(
                target: "fleet",
                "parent stdin closed; terminating worker process group"
            );
            // SAFETY: kill is async-signal-safe; 0 targets this process's own
            // group, which after setsid is exactly the worker tree.
            unsafe { libc::kill(0, libc::SIGTERM) };
        })
        .expect("spawn parent-death watch thread");
}

// The non-interactive exec agent assembly lives in `exec_agent`; the
// glob re-export keeps the dispatch and test references unchanged (#5586).
mod exec_agent;
pub(crate) use exec_agent::*;

#[cfg(test)]
mod serve_bind_host_tests {
    use super::*;

    #[test]
    fn http_defaults_to_loopback() {
        assert_eq!(
            resolve_serve_bind_host(false, None),
            ServeBindHost {
                host: "127.0.0.1".to_string(),
            }
        );
    }

    #[test]
    fn mobile_defaults_to_loopback() {
        assert_eq!(
            resolve_serve_bind_host(true, None),
            ServeBindHost {
                host: "127.0.0.1".to_string(),
            }
        );
    }

    #[test]
    fn mobile_respects_explicit_loopback_host() {
        assert_eq!(
            resolve_serve_bind_host(true, Some("127.0.0.1".to_string())),
            ServeBindHost {
                host: "127.0.0.1".to_string(),
            }
        );
    }

    #[test]
    fn http_and_mobile_are_mutually_exclusive() {
        let err = validate_serve_mode_selection(false, true, true, false, false).unwrap_err();
        assert!(
            err.to_string()
                .contains("--http and --mobile are mutually exclusive")
        );
    }

    #[test]
    fn web_is_a_distinct_loopback_runtime_mode() {
        assert!(validate_serve_mode_selection(false, false, false, true, false).unwrap());
        let err = validate_serve_mode_selection(false, true, false, true, false).unwrap_err();
        assert!(err.to_string().contains("--web is mutually exclusive"));
        assert_eq!(
            resolve_serve_bind_host(false, None),
            ServeBindHost {
                host: "127.0.0.1".to_string(),
            }
        );
    }
}

#[cfg(test)]
#[path = "tests/exec_exit_semantics.rs"]
mod exec_exit_semantics_tests;
#[cfg(test)]
mod doctor_legacy_state_tests {
    use super::*;
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn doctor_reports_unreadable_files_without_clean_marker() {
        let tmp = TempDir::new().unwrap();
        let broken = tmp.path().join("broken.json");
        fs::write(&broken, "{\"access_token\": unfinished").unwrap();
        let mut report =
            session_secret_scrub::scrub_files(std::slice::from_ref(&broken), None, &[]).unwrap();
        let summary = doctor_stored_secrets_summary(&report, 1);
        assert!(summary.contains("scan incomplete") && summary.contains("broken.json"));
        assert!(!summary.contains('✓') && !summary.contains("no credentials found"));
        report.flagged_files.push(tmp.path().join("dirty.json"));
        let summary = doctor_stored_secrets_summary(&report, 2);
        assert!(summary.contains("hold credentials") && summary.contains("scan incomplete"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scrub_does_not_block_tokio_worker() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let _env = lock_test_env();
        let tmp = TempDir::new().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", tmp.path());
        let _runtime = EnvVarGuard::set("CODEWHALE_RUNTIME_DIR", tmp.path().join("runtime"));
        let manager = session_manager::SessionManager::default_location().unwrap();
        fs::write(manager.sessions_dir().join("dirty.json"), serde_json::json!({"type":"tool_result", "content":"sk-ant-oat01-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefghij"}).to_string()).unwrap();
        let (locked_tx, locked_rx) = mpsc::channel();
        let progressed = Arc::new(AtomicBool::new(false));
        let progress_for_lock = progressed.clone();
        let holder = std::thread::spawn(move || {
            manager
                .with_session_file_lock("dirty", || {
                    locked_tx.send(()).unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    Ok(progress_for_lock.load(Ordering::SeqCst))
                })
                .unwrap()
                .unwrap()
        });
        locked_rx.recv().unwrap();
        let tick = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            progressed.store(true, Ordering::SeqCst);
        });
        run_sessions_scrub_secrets(true, None, None).await.unwrap();
        assert!(
            holder.join().unwrap(),
            "the timer must run while the scrub waits on the file lock"
        );
        tick.await.unwrap();
    }

    struct EnvVarRestore {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarRestore {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarRestore {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => env::set_var(self.key, value),
                    None => env::remove_var(self.key),
                }
            }
        }
    }

    fn roots(tmp: &TempDir) -> (PathBuf, PathBuf) {
        (tmp.path().join(".codewhale"), tmp.path().join(".deepseek"))
    }

    fn entry<'a>(report: &'a [DoctorLegacyStateEntry], name: &str) -> &'a DoctorLegacyStateEntry {
        report
            .iter()
            .find(|entry| entry.name == name)
            .expect("legacy state entry should exist")
    }

    #[test]
    fn doctor_legacy_state_report_marks_unmigrated_legacy_entries() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        fs::create_dir_all(legacy_root.join("sessions")).expect("legacy sessions");
        fs::create_dir_all(legacy_root.join("tasks")).expect("legacy tasks");
        fs::create_dir_all(&primary_root).expect("primary root");
        fs::write(legacy_root.join("config.toml"), "api_key = 'old'").expect("legacy config");

        let report = doctor_legacy_state_report(&primary_root, &legacy_root);
        let session_recovery = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(
            entry(&report, "sessions").status,
            DoctorLegacyStateStatus::LegacyOnly
        );
        assert_eq!(
            entry(&report, "config.toml").status,
            DoctorLegacyStateStatus::LegacyOnly
        );
        assert_eq!(
            entry(&report, "skills").status,
            DoctorLegacyStateStatus::Absent
        );

        let json =
            doctor_legacy_state_json(&primary_root, &legacy_root, &report, &session_recovery);
        assert_eq!(json["needs_attention"], true);
        assert_eq!(json["legacy_only_count"], 3);
        assert_eq!(json["dual_present_count"], 0);
    }

    #[test]
    fn doctor_legacy_state_report_marks_dual_present_entries() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        fs::create_dir_all(primary_root.join("sessions")).expect("primary sessions");
        fs::create_dir_all(legacy_root.join("sessions")).expect("legacy sessions");
        fs::write(primary_root.join("mcp.json"), "{}").expect("primary mcp");
        fs::write(legacy_root.join("mcp.json"), "{}").expect("legacy mcp");

        let report = doctor_legacy_state_report(&primary_root, &legacy_root);
        let session_recovery = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(
            entry(&report, "sessions").status,
            DoctorLegacyStateStatus::Both
        );
        assert_eq!(
            entry(&report, "mcp.json").status,
            DoctorLegacyStateStatus::Both
        );

        let json =
            doctor_legacy_state_json(&primary_root, &legacy_root, &report, &session_recovery);
        assert_eq!(json["needs_attention"], true);
        assert_eq!(json["legacy_only_count"], 0);
        assert_eq!(json["dual_present_count"], 2);
    }

    #[test]
    fn doctor_legacy_state_report_is_clear_when_only_primary_exists() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        fs::create_dir_all(primary_root.join("sessions")).expect("primary sessions");
        fs::write(primary_root.join("settings.toml"), "default_mode = 'ask'")
            .expect("primary settings");

        let report = doctor_legacy_state_report(&primary_root, &legacy_root);
        let session_recovery = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(
            entry(&report, "sessions").status,
            DoctorLegacyStateStatus::PrimaryOnly
        );
        assert!(!report.iter().any(legacy_state_needs_attention));

        let json =
            doctor_legacy_state_json(&primary_root, &legacy_root, &report, &session_recovery);
        assert_eq!(json["needs_attention"], false);
        assert_eq!(json["legacy_only_count"], 0);
        assert_eq!(json["dual_present_count"], 0);
    }

    #[test]
    fn doctor_legacy_state_report_is_clear_when_neither_root_exists() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);

        let report = doctor_legacy_state_report(&primary_root, &legacy_root);
        let session_recovery = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert!(
            report
                .iter()
                .all(|entry| entry.status == DoctorLegacyStateStatus::Absent)
        );
        assert!(!report.iter().any(legacy_state_needs_attention));

        let json =
            doctor_legacy_state_json(&primary_root, &legacy_root, &report, &session_recovery);
        assert_eq!(json["needs_attention"], false);
        assert_eq!(json["legacy_only_count"], 0);
        assert_eq!(json["dual_present_count"], 0);
    }

    #[test]
    fn doctor_reports_incomplete_session_migration_without_mutating_files() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        let primary_sessions = primary_root.join("sessions");
        let legacy_sessions = legacy_root.join("sessions");
        fs::create_dir_all(&primary_sessions).expect("primary sessions");
        fs::create_dir_all(legacy_sessions.join("checkpoints")).expect("legacy checkpoints");
        fs::write(primary_sessions.join("already-there.json"), b"primary")
            .expect("primary session");
        fs::write(legacy_sessions.join("already-there.json"), b"legacy")
            .expect("legacy matching session");
        fs::write(
            legacy_sessions.join("recover-me.json"),
            b"not parsed by doctor",
        )
        .expect("legacy recoverable session");
        fs::write(
            legacy_sessions.join("checkpoints").join("latest.json"),
            b"checkpoint not inspected",
        )
        .expect("legacy checkpoint");

        let legacy_before = fs::read(legacy_sessions.join("recover-me.json"))
            .expect("read legacy fixture before diagnostic");
        let report = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(
            report.status,
            DoctorSessionRecoveryStatus::MigrationIncomplete
        );
        assert_eq!(report.legacy_session_file_count, 2);
        assert_eq!(report.already_present_file_count, 1);
        assert_eq!(report.recoverable_file_count, 1);
        assert_eq!(report.recoverable.len(), 1);
        assert_eq!(report.recoverable[0].name, PathBuf::from("recover-me.json"));
        assert!(
            !primary_sessions.join("recover-me.json").exists(),
            "doctor must not copy a recoverable session"
        );
        assert_eq!(
            fs::read(legacy_sessions.join("recover-me.json"))
                .expect("legacy file remains after diagnostic"),
            legacy_before,
            "doctor must not rewrite or delete the legacy source"
        );

        let json = doctor_session_recovery_json(&report);
        assert_eq!(json["needs_attention"], true);
        assert_eq!(json["read_only"], true);
        assert_eq!(json["chat_contents_read"], false);
        assert_eq!(json["checkpoint_internals_scanned"], false);
        assert_eq!(json["recoverable_file_count"], 1);
        assert_eq!(json["recovery_command"], "codewhale sessions");
        assert_eq!(json["recoverable_files"][0]["name"], "recover-me.json");
        let serialized = json.to_string();
        assert!(
            !serialized.contains("not parsed by doctor"),
            "the report must not expose session contents"
        );
        assert!(
            !serialized.contains("checkpoint not inspected"),
            "the report must not expose checkpoint contents"
        );
    }

    #[test]
    fn doctor_treats_preserved_legacy_sessions_as_complete_by_filename() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        let primary_sessions = primary_root.join("sessions");
        let legacy_sessions = legacy_root.join("sessions");
        fs::create_dir_all(&primary_sessions).expect("primary sessions");
        fs::create_dir_all(&legacy_sessions).expect("legacy sessions");
        fs::write(primary_sessions.join("same-name.json"), b"primary").expect("primary session");
        fs::write(legacy_sessions.join("same-name.json"), b"legacy").expect("legacy session");

        let report = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(
            report.status,
            DoctorSessionRecoveryStatus::MigrationComplete
        );
        assert!(!report.needs_attention());
        assert_eq!(report.recoverable_file_count, 0);
        assert!(report.recoverable.is_empty());
        assert_eq!(report.already_present_file_count, 1);
        let json = doctor_session_recovery_json(&report);
        assert_eq!(json["session_descriptors_compared"], false);
        assert_eq!(
            json["counterpart_check"],
            "top_level_filename_and_regular_file_only"
        );
    }

    #[test]
    fn doctor_bounds_recoverable_session_filename_samples() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        let legacy_sessions = legacy_root.join("sessions");
        fs::create_dir_all(&legacy_sessions).expect("legacy sessions");
        for index in 0..DOCTOR_SESSION_RECOVERY_JSON_SAMPLE_LIMIT {
            fs::write(
                legacy_sessions.join(format!("late-{index:03}.json")),
                b"fixture",
            )
            .expect("legacy session fixture");
        }
        fs::write(legacy_sessions.join("early-000.json"), b"fixture")
            .expect("earliest legacy session fixture");
        fs::write(legacy_sessions.join("early-001.json"), b"fixture")
            .expect("second earliest legacy session fixture");
        let total = DOCTOR_SESSION_RECOVERY_JSON_SAMPLE_LIMIT + 2;

        let report = doctor_session_recovery_report(&primary_root, &legacy_root, false);
        let json = doctor_session_recovery_json(&report);

        assert_eq!(report.recoverable_file_count, total);
        assert_eq!(
            report.recoverable.len(),
            DOCTOR_SESSION_RECOVERY_JSON_SAMPLE_LIMIT
        );
        assert_eq!(
            json["recoverable_files"].as_array().map(Vec::len),
            Some(DOCTOR_SESSION_RECOVERY_JSON_SAMPLE_LIMIT)
        );
        assert_eq!(
            report.recoverable.first().map(|entry| entry.name.as_path()),
            Some(Path::new("early-000.json")),
            "the bounded sample must not depend on read_dir order"
        );
        assert_eq!(
            report.recoverable.last().map(|entry| entry.name.as_path()),
            Some(Path::new("late-097.json")),
            "the bounded sample must retain the lexical prefix"
        );
        assert_eq!(json["recoverable_files_truncated"], true);
    }

    #[test]
    fn doctor_session_recovery_fails_closed_on_an_unreadable_path_shape() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        fs::create_dir_all(&legacy_root).expect("legacy root");
        fs::write(legacy_root.join("sessions"), b"not a directory")
            .expect("invalid legacy sessions path");

        let report = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(report.status, DoctorSessionRecoveryStatus::ScanFailed);
        assert!(report.needs_attention());
        assert!(report.error.as_deref().is_some_and(|error| {
            error.contains("legacy sessions root") && error.contains("not a directory")
        }));
    }

    #[test]
    fn doctor_session_recovery_rejects_a_non_directory_legacy_state_root() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        fs::write(&legacy_root, b"not a state directory").expect("invalid legacy root");

        let report = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(report.status, DoctorSessionRecoveryStatus::ScanFailed);
        assert!(report.error.as_deref().is_some_and(|error| {
            error.contains("legacy state root") && error.contains("not a directory")
        }));
    }

    #[test]
    fn doctor_session_recovery_rejects_a_non_directory_primary_state_root() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        fs::create_dir_all(legacy_root.join("sessions")).expect("legacy sessions");
        fs::write(&primary_root, b"not a state directory").expect("invalid primary root");

        let report = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(report.status, DoctorSessionRecoveryStatus::ScanFailed);
        assert!(report.error.as_deref().is_some_and(|error| {
            error.contains("primary state root") && error.contains("not a directory")
        }));
    }

    #[test]
    fn doctor_session_recovery_rejects_a_non_directory_primary_sessions_root() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        fs::create_dir_all(legacy_root.join("sessions")).expect("legacy sessions");
        fs::create_dir_all(&primary_root).expect("primary root");
        fs::write(primary_root.join("sessions"), b"not a sessions directory")
            .expect("invalid primary sessions path");

        let report = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(report.status, DoctorSessionRecoveryStatus::ScanFailed);
        assert!(report.error.as_deref().is_some_and(|error| {
            error.contains("primary sessions root") && error.contains("not a directory")
        }));
    }

    #[cfg(unix)]
    #[test]
    fn doctor_session_recovery_rejects_a_symlinked_legacy_sessions_root() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        let external_sessions = tmp.path().join("external-sessions");
        fs::create_dir_all(&external_sessions).expect("external sessions");
        fs::write(
            external_sessions.join("must-not-be-enumerated.json"),
            b"session contents must stay unread",
        )
        .expect("external session fixture");
        fs::create_dir_all(&legacy_root).expect("legacy root");
        symlink(&external_sessions, legacy_root.join("sessions"))
            .expect("symlinked legacy sessions root");

        let report = doctor_session_recovery_report(&primary_root, &legacy_root, false);

        assert_eq!(report.status, DoctorSessionRecoveryStatus::ScanFailed);
        assert!(report.needs_attention());
        assert_eq!(report.legacy_session_file_count, 0);
        assert!(report.recoverable.is_empty());
        assert!(
            report
                .error
                .as_deref()
                .is_some_and(|error| error.contains("legacy sessions root")
                    && error.contains("path is a symlink"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn doctor_session_recovery_rejects_symlinked_primary_root_and_sessions_root() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        let external_primary = tmp.path().join("external-primary");
        fs::create_dir_all(external_primary.join("sessions")).expect("external primary");
        fs::create_dir_all(legacy_root.join("sessions")).expect("legacy sessions");
        symlink(&external_primary, &primary_root).expect("symlinked primary root");

        let root_report = doctor_session_recovery_report(&primary_root, &legacy_root, false);
        assert_eq!(root_report.status, DoctorSessionRecoveryStatus::ScanFailed);
        assert!(root_report.error.as_deref().is_some_and(|error| {
            error.contains("primary state root") && error.contains("path is a symlink")
        }));

        fs::remove_file(&primary_root).expect("remove primary root symlink");
        fs::create_dir_all(&primary_root).expect("primary root");
        symlink(&external_primary, primary_root.join("sessions"))
            .expect("symlinked primary sessions root");

        let sessions_report = doctor_session_recovery_report(&primary_root, &legacy_root, false);
        assert_eq!(
            sessions_report.status,
            DoctorSessionRecoveryStatus::ScanFailed
        );
        assert!(sessions_report.error.as_deref().is_some_and(|error| {
            error.contains("primary sessions root") && error.contains("path is a symlink")
        }));
    }

    #[test]
    fn explicit_codewhale_home_skips_session_recovery_scan() {
        let tmp = TempDir::new().expect("tempdir");
        let (primary_root, legacy_root) = roots(&tmp);
        fs::create_dir_all(legacy_root.join("sessions")).expect("legacy sessions");
        fs::write(legacy_root.join("sessions").join("ambient.json"), b"legacy")
            .expect("legacy session");

        let report = doctor_session_recovery_report(&primary_root, &legacy_root, true);

        assert_eq!(report.status, DoctorSessionRecoveryStatus::Isolated);
        assert!(report.codewhale_home_is_explicit);
        assert_eq!(report.legacy_session_file_count, 0);
        assert_eq!(report.recoverable_file_count, 0);
        assert!(report.recoverable.is_empty());
        assert!(!report.needs_attention());
    }

    #[test]
    fn doctor_state_roots_ignore_ambient_legacy_home_when_codewhale_home_is_explicit() {
        let _env_lock = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let explicit_home = tmp.path().join("isolated-codewhale");
        let ambient_legacy = tmp.path().join(".deepseek");
        fs::create_dir_all(&ambient_legacy).expect("ambient legacy root");
        fs::write(
            ambient_legacy.join("config.toml"),
            "provider = 'deepseek'\n",
        )
        .expect("ambient legacy config");
        let _home = EnvVarRestore::set("HOME", tmp.path());
        let _codewhale_home = EnvVarRestore::set("CODEWHALE_HOME", &explicit_home);

        let (primary_root, legacy_root) = doctor_state_roots();
        let report = doctor_legacy_state_report(&primary_root, &legacy_root);
        let session_recovery = doctor_session_recovery_report(
            &primary_root,
            &legacy_root,
            codewhale_config::codewhale_home_is_explicit(),
        );

        assert_eq!(primary_root, explicit_home);
        assert_eq!(
            legacy_root,
            primary_root.join(codewhale_config::LEGACY_APP_DIR)
        );
        assert!(
            report
                .iter()
                .all(|entry| entry.status == DoctorLegacyStateStatus::Absent),
            "doctor must not report ambient legacy state when CODEWHALE_HOME is explicit"
        );
        assert!(!report.iter().any(legacy_state_needs_attention));
        assert_eq!(
            session_recovery.status,
            DoctorSessionRecoveryStatus::Isolated
        );
        assert!(session_recovery.recoverable.is_empty());
    }
}

#[cfg(test)]
mod doctor_setup_state_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn prepare_env(tmp: &TempDir) -> (crate::test_support::EnvVarGuard, PathBuf) {
        let codewhale_home = tmp.path().join(".codewhale");
        fs::create_dir_all(&codewhale_home).expect("codewhale home");
        (
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str()),
            codewhale_home,
        )
    }

    fn provider_step(report: &serde_json::Value) -> &serde_json::Value {
        report["steps"]
            .as_array()
            .expect("steps array")
            .iter()
            .find(|step| step["step"] == "provider_model")
            .expect("provider/model step")
    }

    #[test]
    fn doctor_setup_consistency_flags_missing_user_constitution() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let (_home_guard, _codewhale_home) = prepare_env(&tmp);
        let _key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");

        let state = codewhale_config::SetupState {
            constitution_source: codewhale_config::ConstitutionSource::UserGlobal,
            ..Default::default()
        };
        state.save().expect("persist setup state");

        let report = doctor_setup_report_json(&Config::default(), &workspace);

        assert_eq!(report["source"], "persisted");
        assert_eq!(report["consistency"]["status"], "inconsistent");
        let issues = report["consistency"]["issues"].to_string();
        assert!(
            issues.contains("setup_state_points_at_missing_user_constitution"),
            "{issues}"
        );
    }

    #[test]
    fn doctor_setup_consistency_flags_stale_temp_files() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let (_home_guard, codewhale_home) = prepare_env(&tmp);
        let _key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::write(codewhale_home.join(".tmpAbC123"), b"orphaned atomic write")
            .expect("stale temp file");

        let report = doctor_setup_report_json(&Config::default(), &workspace);

        assert_eq!(report["consistency"]["status"], "inconsistent");
        let issues = report["consistency"]["issues"].to_string();
        assert!(
            issues.contains("stale_setup_temp_files_in_codewhale_home"),
            "{issues}"
        );
    }

    #[test]
    fn doctor_setup_consistency_reports_consistent_for_clean_home() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let (_home_guard, _codewhale_home) = prepare_env(&tmp);
        let _key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");

        let report = doctor_setup_report_json(&Config::default(), &workspace);

        assert_eq!(report["consistency"]["status"], "consistent");
        assert_eq!(
            report["consistency"]["issues"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            0
        );
    }

    #[test]
    fn doctor_setup_report_json_derives_state_without_sidecar() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let (_home_guard, _codewhale_home) = prepare_env(&tmp);
        let _key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");

        let report = doctor_setup_report_json(&Config::default(), &workspace);

        assert_eq!(report["source"], "derived");
        assert_eq!(report["inherited"], true);
        assert_eq!(report["next_actions"]["constitution"], "/constitution");
        assert_eq!(report["next_actions"]["setup_report"], "/setup report");
        assert_eq!(
            report["next_actions"]["provider_model"],
            "/setup provider, /provider setup <name>, or /model"
        );
        assert_eq!(report["next_actions"]["runtime_posture"], "/config");
        assert_eq!(
            report["next_actions"]["operate_fleet"],
            "/setup fleet (readiness), /fleet setup (explicit profile authoring)"
        );
        assert_eq!(report["next_actions"]["hotbar"], "/setup hotbar");
        assert_eq!(report["next_actions"]["tools_mcp"], "/setup tools");
        assert_eq!(report["next_actions"]["remote_runtime"], "/setup remote");
        assert_eq!(report["next_actions"]["persistence"], "/setup persistence");
        assert_eq!(
            report["checkpoint_version"],
            crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION
        );
        assert_eq!(report["update_ready"], false);
        assert_eq!(report["operate_ready"], false);
        assert_eq!(
            report["operate_fleet"]["concurrency"]["plan_limit_probed"],
            false
        );
        assert_eq!(
            report["operate_fleet"]["roster"]["readiness_rule"],
            "built-in starter roster or custom roster"
        );
        assert_eq!(report["provider_model"]["provider"]["id"], "deepseek");
        assert_eq!(report["provider_model"]["provider"]["display"], "DeepSeek");
        assert_eq!(
            report["provider_model"]["model"]["resolved"],
            crate::config::DEFAULT_TEXT_MODEL
        );
        assert_eq!(
            report["provider_model"]["auth"]["source"],
            "secret_store_unprobed"
        );
        assert_eq!(
            report["provider_model"]["auth"]["availability"],
            "not_probed"
        );
        assert_eq!(
            report["provider_model"]["auth"]["credential_url"],
            "https://platform.deepseek.com"
        );
        assert_eq!(
            report["provider_model"]["auth"]["credential_mode"],
            "api_key"
        );
        assert_eq!(
            report["provider_model"]["auth"]["env_vars"][0],
            "DEEPSEEK_API_KEY"
        );
        assert_eq!(report["provider_model"]["health"]["live_validation"], false);
        assert_eq!(report["constitution"]["source"], "bundled");
        assert_eq!(report["constitution"]["autonomy_preference"], "unspecified");
        assert_eq!(report["runtime_posture"]["source"], "unset");
        assert_eq!(report["runtime_posture"]["default_mode"]["value"], "agent");
        assert_eq!(
            report["runtime_posture"]["approval_policy"]["value"],
            "on-request"
        );
        assert_eq!(report["runtime_posture"]["allow_shell"]["value"], true);
        assert_eq!(
            report["runtime_posture"]["sandbox_mode"]["value"],
            "mode-derived"
        );
        assert_eq!(
            report["runtime_posture"]["network_default"]["value"],
            "prompt"
        );
        assert_eq!(provider_step(&report)["status"], "needs_action");
    }

    #[test]
    fn doctor_setup_provider_model_json_covers_cn_codex_and_local_matrix() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let (_home_guard, _codewhale_home) = prepare_env(&tmp);
        let _home = crate::test_support::EnvVarGuard::set("HOME", tmp.path());
        let _userprofile = crate::test_support::EnvVarGuard::set("USERPROFILE", tmp.path());
        let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _codex_key = crate::test_support::EnvVarGuard::remove("OPENAI_CODEX_ACCESS_TOKEN");
        let _codex_legacy_key = crate::test_support::EnvVarGuard::remove("CODEX_ACCESS_TOKEN");
        let codex_auth_path = tmp.path().join("external-codex-auth.json");
        let codex_auth_raw = serde_json::json!({
            "tokens": {
                "access_token": crate::test_support::future_test_jwt("doctor"),
                "account_id": "acct-doctor-read-only",
                "refresh_token": "must-never-be-used",
                "unknown": {"preserve": true}
            }
        })
        .to_string();
        fs::write(&codex_auth_path, &codex_auth_raw).expect("Codex auth trap fixture");
        let _codex_auth =
            crate::test_support::EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &codex_auth_path);
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");

        let cn_config = Config {
            provider: Some("deepseek-cn".to_string()),
            ..Config::default()
        };
        let cn_report = doctor_setup_report_json(&cn_config, &workspace);
        assert_eq!(cn_report["provider_model"]["provider"]["id"], "deepseek-cn");
        assert_eq!(
            cn_report["provider_model"]["provider"]["display"],
            "DeepSeek (legacy alias)"
        );
        assert_eq!(
            cn_report["provider_model"]["auth"]["env_vars"][0],
            "DEEPSEEK_API_KEY"
        );
        assert_eq!(
            cn_report["provider_model"]["auth"]["credential_url"],
            "https://platform.deepseek.com"
        );
        assert_eq!(cn_report["provider_model"]["auth"]["oauth_only"], false);
        assert_eq!(
            cn_report["provider_model"]["health"]["live_validation"],
            false
        );

        let codex_config = Config {
            provider: Some("openai-codex".to_string()),
            ..Config::default()
        };
        crate::external_credentials::reset_side_effect_trap();
        let codex_report = doctor_setup_report_json(&codex_config, &workspace);
        assert_eq!(
            codex_report["provider_model"]["provider"]["id"],
            crate::config::ProviderKind::OpenaiCodex.as_str()
        );
        assert!(codex_report["provider_model"]["auth"]["credential_url"].is_null());
        assert_eq!(
            codex_report["provider_model"]["auth"]["credential_mode"],
            "oauth"
        );
        assert_eq!(codex_report["provider_model"]["auth"]["oauth_only"], true);
        assert_eq!(
            codex_report["provider_model"]["health"]["next_action"],
            "/setup provider or /provider setup <name>"
        );
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (0, 0),
            "doctor must not stat or read external credentials without consent"
        );

        let mut consent = codewhale_config::ExternalCredentialConsentToml::read_only(
            codewhale_config::ProviderKind::OpenaiCodex,
            codewhale_config::ExternalCredentialSource::CodexCli,
            codex_auth_path.clone(),
        );
        let codex_read_only = Config {
            provider: Some("openai-codex".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                openai_codex: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    external_credentials: Some(consent.clone()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Config::default()
        };
        let changed_ambient_path = tmp.path().join("new-ambient-codex-auth.json");
        let _changed_codex_auth =
            crate::test_support::EnvVarGuard::set("OPENAI_CODEX_AUTH_FILE", &changed_ambient_path);
        crate::external_credentials::reset_side_effect_trap();
        let codex_read_only_report = doctor_setup_report_json(&codex_read_only, &workspace);
        assert_eq!(
            codex_read_only_report["provider_model"]["auth"]["present_or_local"],
            false
        );
        assert_eq!(
            codex_read_only_report["provider_model"]["auth"]["source"],
            "external_consent"
        );
        let status_json = doctor_external_credential_consent_json(&codex_read_only);
        let codex_status = status_json
            .as_array()
            .and_then(|rows| rows.first())
            .expect("Codex structural status");
        assert_eq!(codex_status["access"], "read_only");
        assert_eq!(codex_status["provider"], "openai-codex");
        assert_eq!(codex_status["source"], "codex_cli");
        assert_eq!(codex_status["route_state"], "active");
        assert_eq!(codex_status["ambient_path_changed"], true);
        assert!(
            codex_status["ambient_path_warning"]
                .as_str()
                .is_some_and(|warning| warning.contains("remains pinned"))
        );
        assert_eq!(
            codex_status["revoke_command"],
            "codewhale auth external-revoke --provider openai-codex"
        );
        let human = doctor_external_credential_consent_lines(&codex_read_only).join("\n");
        assert!(human.contains("path="), "{human}");
        assert!(human.contains("version=1"), "{human}");
        assert!(human.contains("no refresh, identity-provider or discovery requests"));
        assert!(human.contains("normal requests to the explicitly selected provider"));
        assert!(human.contains("consent remains pinned"), "{human}");
        assert!(
            human.contains(&codewhale_config::quote_os_path(&codex_auth_path)),
            "{human}"
        );
        assert!(!human.contains(&changed_ambient_path.display().to_string()));
        assert_eq!(
            crate::external_credentials::complete_side_effect_trap_counts(),
            (0, 0, 0, 0, 0),
            "doctor consent status is structural and must not inspect the file"
        );
        assert_eq!(
            fs::read_to_string(&codex_auth_path).expect("unchanged Codex auth fixture"),
            codex_auth_raw
        );

        consent.access = codewhale_config::ExternalCredentialAccess::Managed;
        let codex_managed = Config {
            provider: Some("openai-codex".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                openai_codex: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    external_credentials: Some(consent),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Config::default()
        };
        crate::external_credentials::reset_side_effect_trap();
        let codex_managed_report = doctor_setup_report_json(&codex_managed, &workspace);
        assert_eq!(
            codex_managed_report["provider_model"]["auth"]["present_or_local"],
            false
        );
        assert_eq!(
            crate::external_credentials::side_effect_trap_counts(),
            (0, 0),
            "unsupported managed mode must fail before external I/O"
        );
        assert_eq!(
            fs::read_to_string(&codex_auth_path).expect("unchanged managed auth fixture"),
            codex_auth_raw
        );

        let local_config = Config {
            provider: Some("ollama".to_string()),
            ..Config::default()
        };
        let local_report = doctor_setup_report_json(&local_config, &workspace);
        assert_eq!(local_report["provider_model"]["provider"]["id"], "ollama");
        assert_eq!(
            local_report["provider_model"]["auth"]["present_or_local"],
            true
        );
        assert!(local_report["provider_model"]["auth"]["credential_url"].is_null());
        assert_eq!(
            local_report["provider_model"]["auth"]["credential_mode"],
            "local_optional"
        );
        assert_eq!(local_report["provider_model"]["auth"]["oauth_only"], false);
        assert_eq!(
            local_report["provider_model"]["health"]["next_action"],
            "/model"
        );

        let kimi_config = Config {
            provider: Some("moonshot".to_string()),
            ..Config::default()
        };
        let kimi_report = doctor_setup_report_json(&kimi_config, &workspace);
        assert_eq!(
            kimi_report["provider_model"]["auth"]["credential_url"],
            "https://platform.kimi.ai"
        );
        assert_eq!(
            kimi_report["provider_model"]["auth"]["credential_docs_url"],
            "https://platform.kimi.ai"
        );
        assert_eq!(
            kimi_report["provider_model"]["auth"]["credential_mode"],
            "api_key"
        );
        assert!(
            kimi_report["provider_model"]["auth"]["credential_guidance"]
                .as_str()
                .is_some_and(|guidance| guidance.contains("OAuth is not available"))
        );
    }

    #[test]
    fn doctor_setup_report_json_uses_persisted_state() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let (_home_guard, _codewhale_home) = prepare_env(&tmp);
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let mut state = codewhale_config::SetupState::default();
        state.set_step(
            codewhale_config::SetupStep::Language,
            codewhale_config::StepEntry::new(
                codewhale_config::StepStatus::Verified,
                true,
                crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
            ),
        );
        state.set_step(
            codewhale_config::SetupStep::ProviderModel,
            codewhale_config::StepEntry::new(
                codewhale_config::StepStatus::Verified,
                true,
                crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
            )
            .with_result("deepseek/deepseek-chat"),
        );
        state.set_step(
            codewhale_config::SetupStep::TrustSandbox,
            codewhale_config::StepEntry::new(
                codewhale_config::StepStatus::Verified,
                true,
                crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
            ),
        );
        state
            .complete_constitution_checkpoint(
                crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
                codewhale_config::ConstitutionChoice::Bundled,
            )
            .set_step(
                codewhale_config::SetupStep::Constitution,
                codewhale_config::StepEntry::new(
                    codewhale_config::StepStatus::Verified,
                    true,
                    crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
                ),
            );
        state.runtime_posture_source = codewhale_config::RuntimePostureSource::Confirmed;
        state.save().expect("persist setup state");
        codewhale_config::UserConstitution {
            autonomy_preference: codewhale_config::AutonomyPreference::Balanced,
            ..Default::default()
        }
        .save()
        .expect("persist user constitution");
        let config = Config {
            approval_policy: Some("never".to_string()),
            allow_shell: Some(false),
            sandbox_mode: Some("read-only".to_string()),
            network: Some(crate::config::NetworkPolicyToml {
                default: "deny".to_string(),
                ..Default::default()
            }),
            ..Config::default()
        }
        .with_legacy_root(Some("TEST-STRUCTURAL-LITERAL".to_string()), None);

        let report = doctor_setup_report_json(&config, &workspace);

        assert_eq!(report["source"], "persisted");
        assert_eq!(report["first_run_ready"], true);
        assert_eq!(report["update_ready"], true);
        assert_eq!(report["operate_ready"], false);
        assert_eq!(report["constitution"]["choice"], "bundled");
        assert_eq!(
            report["constitution"]["checkpoint_completed_for"],
            crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION
        );
        assert_eq!(report["constitution"]["autonomy_preference"], "balanced");
        assert_eq!(report["runtime_posture_source"], "confirmed");
        assert_eq!(report["runtime_posture"]["source"], "confirmed");
        assert_eq!(
            report["runtime_posture"]["approval_policy"]["value"],
            "never"
        );
        assert_eq!(
            report["runtime_posture"]["approval_policy"]["source"],
            "config"
        );
        assert_eq!(report["runtime_posture"]["allow_shell"]["value"], false);
        assert_eq!(report["runtime_posture"]["allow_shell"]["source"], "config");
        assert_eq!(
            report["runtime_posture"]["sandbox_mode"]["value"],
            "read-only"
        );
        assert_eq!(
            report["runtime_posture"]["sandbox_mode"]["source"],
            "config"
        );
        assert_eq!(
            report["runtime_posture"]["network_default"]["value"],
            "deny"
        );
        assert_eq!(
            report["runtime_posture"]["network_default"]["source"],
            "config"
        );
        assert_eq!(provider_step(&report)["result"], "deepseek/deepseek-chat");

        let unprobed_config = Config { ..config.clone() }
            .with_legacy_root(Some(crate::config::API_KEYRING_SENTINEL.to_string()), None);
        let unprobed_report = doctor_setup_report_json(&unprobed_config, &workspace);
        assert_eq!(unprobed_report["credential"]["ready"], false);
        assert_eq!(unprobed_report["credential"]["availability"], "not_probed");
        assert_eq!(unprobed_report["first_run_ready"], true);
        assert_eq!(unprobed_report["update_ready"], true);
    }

    #[test]
    fn doctor_reports_settings_permission_posture_when_approval_policy_unset() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let (_home_guard, codewhale_home) = prepare_env(&tmp);
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::write(
            codewhale_home.join("settings.toml"),
            "permission_posture = \"full-access\"\n",
        )
        .expect("write settings.toml");

        let config = Config::default();
        assert!(config.approval_policy.is_none());

        let line = doctor_runtime_posture_line(&config, &workspace);
        assert!(
            line.contains("permission_posture=full-access (settings)"),
            "text doctor should report saved settings posture: {line}"
        );
        assert!(
            line.contains("approval_policy=on-request (default)"),
            "text doctor should keep unset config approval_policy default: {line}"
        );

        let report = doctor_setup_report_json(&config, &workspace);
        assert_eq!(
            report["runtime_posture"]["permission_posture"]["value"],
            "full-access"
        );
        assert_eq!(
            report["runtime_posture"]["permission_posture"]["source"],
            "settings"
        );
        assert_eq!(
            report["runtime_posture"]["approval_policy"]["value"],
            "on-request"
        );
        assert_eq!(
            report["runtime_posture"]["approval_policy"]["source"],
            "default"
        );
    }

    /// Doctor must distinguish a configured preference from current processor
    /// consent, and accurately report the default-off posture.
    #[test]
    fn doctor_reports_resolved_telemetry_with_its_source() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let (_home_guard, _codewhale_home) = prepare_env(&tmp);
        let _telemetry_env = crate::test_support::EnvVarGuard::remove("CODEWHALE_TELEMETRY");
        let _telemetry_alias_env = crate::test_support::EnvVarGuard::remove("DEEPSEEK_TELEMETRY");
        let _telemetry_floor =
            crate::test_support::EnvVarGuard::remove("CODEWHALE_TELEMETRY_FLOOR");
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");

        // Nothing configured anywhere: the shipped default applies and is
        // named, in both the text line and the JSON posture section.
        let config = Config::default();
        assert!(config.telemetry.is_none());
        let line = doctor_runtime_posture_line(&config, &workspace);
        assert!(
            line.contains("telemetry=on (default)"),
            "doctor line should name the defaulted consent: {line}"
        );
        let report = doctor_setup_report_json(&config, &workspace);
        assert_eq!(report["runtime_posture"]["telemetry"]["value"], true);
        assert_eq!(report["runtime_posture"]["telemetry"]["source"], "default");

        let configured_on = Config {
            telemetry: Some(true),
            ..Config::default()
        };
        assert_eq!(doctor_runtime_telemetry(&configured_on), (true, "config"));
        let mut accepted = codewhale_config::SetupState::default();
        accepted.record_telemetry_notice("3", true);
        accepted.save().expect("old acceptance");
        assert_eq!(doctor_runtime_telemetry(&configured_on), (true, "config"));
        accepted.record_telemetry_notice(codewhale_config::TELEMETRY_NOTICE_VERSION, true);
        accepted.save().expect("current acceptance");
        assert_eq!(doctor_runtime_telemetry(&configured_on), (true, "config"));
        {
            let _env_on = crate::test_support::EnvVarGuard::set("CODEWHALE_TELEMETRY", "true");
            assert_eq!(doctor_runtime_telemetry(&Config::default()), (true, "env"));
        }

        // A persisted opt-out is reported as the config file's decision.
        let config = Config {
            telemetry: Some(false),
            ..Config::default()
        };
        let line = doctor_runtime_posture_line(&config, &workspace);
        assert!(
            line.contains("telemetry=off (config)"),
            "doctor line should name the persisted opt-out: {line}"
        );
        let report = doctor_setup_report_json(&config, &workspace);
        assert_eq!(report["runtime_posture"]["telemetry"]["value"], false);
        assert_eq!(report["runtime_posture"]["telemetry"]["source"], "config");
    }

    #[test]
    fn doctor_setup_report_json_fails_closed_without_operate_receipts() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().expect("tempdir");
        let (_home_guard, _codewhale_home) = prepare_env(&tmp);
        let workspace = tmp.path().join("workspace");
        fs::create_dir_all(&workspace).expect("workspace");
        let mut state = codewhale_config::SetupState::default();
        state.set_step(
            codewhale_config::SetupStep::Language,
            codewhale_config::StepEntry::new(
                codewhale_config::StepStatus::Verified,
                true,
                crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
            ),
        );
        state.set_step(
            codewhale_config::SetupStep::ProviderModel,
            codewhale_config::StepEntry::new(
                codewhale_config::StepStatus::Verified,
                true,
                crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
            ),
        );
        state.runtime_posture_source = codewhale_config::RuntimePostureSource::Confirmed;
        state.complete_constitution_checkpoint(
            crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
            codewhale_config::ConstitutionChoice::Bundled,
        );
        state.set_step(
            codewhale_config::SetupStep::OperateFleet,
            codewhale_config::StepEntry::new(
                codewhale_config::StepStatus::Verified,
                false,
                crate::tui::setup::CONSTITUTION_CHECKPOINT_VERSION,
            )
            .with_result(
                "provider=ready, runtime=ready, roster=ready, concurrency=plan limit not probed",
            ),
        );
        state.save().expect("persist setup state");

        let config = Config {
            ..Config::default()
        }
        .with_legacy_root(Some("TEST-STRUCTURAL-LITERAL".to_string()), None);
        let report = doctor_setup_report_json(&config, &workspace);

        assert_eq!(report["first_run_ready"], true);
        assert_eq!(report["operate_ready"], false);
        assert_eq!(
            report["operate_fleet"]["concurrency"]["plan_limit_probed"],
            false
        );
        assert!(
            report["operate_fleet"]["roster"]["built_in"]
                .as_u64()
                .is_some_and(|count| count > 0)
        );
        let operate_step = report["steps"]
            .as_array()
            .expect("steps array")
            .iter()
            .find(|step| step["step"] == "operate_fleet")
            .expect("operate/fleet step");
        assert_eq!(operate_step["status"], "verified");
        assert!(
            operate_step["result"]
                .as_str()
                .is_some_and(|result| result.contains("plan limit not probed"))
        );
    }
}

#[cfg(test)]
mod doctor_endpoint_tests {
    use super::*;

    #[test]
    fn doctor_api_target_reports_default_endpoint() {
        let config = Config::default();

        let target = doctor_api_target(&config);

        assert_eq!(target.provider, "deepseek");
        assert_eq!(target.base_url, crate::config::DEFAULT_DEEPSEEK_BASE_URL);
        assert_eq!(target.model, crate::config::DEFAULT_TEXT_MODEL);
        assert_eq!(target.resolution, DoctorModelResolution::Resolved);
    }

    #[test]
    fn doctor_api_target_falls_back_to_configured_model_when_resolution_fails() {
        // `custom` with no custom provider table cannot resolve an identity;
        // doctor must fall back to the raw configured model and say so
        // instead of presenting an unresolved value as the engine's route.
        let config = Config {
            provider: Some("custom".to_string()),
            ..Default::default()
        };

        let target = doctor_api_target(&config);

        assert_eq!(target.resolution, DoctorModelResolution::ConfiguredOnly);
        assert_eq!(target.model, config.default_model());
    }

    #[test]
    fn doctor_api_target_routes_deepseek_cn_alias_to_beta_endpoint() {
        let config = Config {
            provider: Some("deepseek-cn".to_string()),
            ..Default::default()
        };

        let target = doctor_api_target(&config);

        assert_eq!(target.provider, "deepseek-cn");
        assert_eq!(target.base_url, crate::config::DEFAULT_DEEPSEEKCN_BASE_URL);
        assert_eq!(target.base_url, crate::config::DEFAULT_DEEPSEEK_BASE_URL);
        assert_eq!(target.model, crate::config::DEFAULT_TEXT_MODEL);
        assert_eq!(target.resolution, DoctorModelResolution::Resolved);
    }

    #[test]
    fn strict_tool_mode_doctor_reports_disabled_by_default() {
        let config = Config::default();

        let status = doctor_strict_tool_mode_status(&config);

        assert!(!status.enabled);
        assert_eq!(status.status, "disabled");
        assert!(!status.function_strict_sent);
        assert!(status.recommended_base_url.is_none());
    }

    #[test]
    fn doctor_known_base_urls_are_ascii_case_insensitive() {
        assert!(doctor_xiaomi_mimo_base_url_uses_token_plan(
            "HTTPS://TOKEN-PLAN-CN.XIAOMIMIMO.COM/V1/"
        ));
        assert_eq!(
            known_deepseek_base_url_kind("HTTPS://API.DEEPSEEK.COM/BETA/"),
            Some(DeepSeekBaseUrlKind::Beta)
        );
        assert_eq!(
            known_deepseek_base_url_kind("HTTPS://API.DEEPSEEK.COM/V1/"),
            Some(DeepSeekBaseUrlKind::NonBeta)
        );
    }

    #[test]
    fn strict_tool_mode_doctor_accepts_default_beta_endpoint() {
        let config = Config {
            strict_tool_mode: Some(true),
            ..Default::default()
        };

        let status = doctor_strict_tool_mode_status(&config);

        assert!(status.enabled);
        assert_eq!(status.status, "ready");
        assert!(status.function_strict_sent);
        assert!(status.message.contains("beta endpoint"));
        assert!(status.recommended_base_url.is_none());
    }

    #[test]
    fn strict_tool_mode_doctor_warns_for_non_beta_deepseek_endpoint() {
        let config = Config {
            strict_tool_mode: Some(true),
            ..Default::default()
        }
        .with_legacy_root(None, Some("https://api.deepseek.com".to_string()));

        let status = doctor_strict_tool_mode_status(&config);

        assert_eq!(status.status, "fallback_non_beta");
        assert!(!status.function_strict_sent);
        assert_eq!(
            status.recommended_base_url.as_deref(),
            Some(crate::config::DEFAULT_DEEPSEEK_BASE_URL)
        );
        assert_eq!(
            doctor_strict_tool_mode_report_json(&status)["recommended_base_url"],
            "https://api.deepseek.com"
        );
    }

    #[test]
    fn strict_tool_mode_doctor_accepts_deepseek_cn_alias_default_endpoint() {
        let config = Config {
            provider: Some("deepseek-cn".to_string()),
            strict_tool_mode: Some(true),
            ..Default::default()
        };

        let status = doctor_strict_tool_mode_status(&config);

        assert_eq!(status.status, "ready");
        assert!(status.function_strict_sent);
        assert!(status.message.contains("beta endpoint"));
        assert!(status.recommended_base_url.is_none());
    }

    #[test]
    fn strict_tool_mode_doctor_marks_custom_endpoint_as_forwarded() {
        let config = Config {
            provider: Some("vllm".to_string()),
            strict_tool_mode: Some(true),
            ..Default::default()
        };

        let status = doctor_strict_tool_mode_status(&config);

        assert_eq!(status.status, "custom_endpoint");
        assert!(status.function_strict_sent);
        assert!(status.message.contains("custom endpoint"));
    }

    #[test]
    fn doctor_tls_status_reports_verification_enabled_by_default() {
        let status = doctor_tls_status(&Config::default());

        assert!(status.certificate_verification);
        assert!(!status.insecure_skip_tls_verify);
        assert_eq!(status.provider, "deepseek");
        assert!(status.message.contains("enabled"));
    }

    #[test]
    fn doctor_tls_status_warns_when_active_provider_skips_verification() {
        let mut providers = crate::config::ProvidersConfig::default();
        providers.openai.insecure_skip_tls_verify = Some(true);
        let config = Config {
            provider: Some("openai".to_string()),
            providers: Some(providers),
            ..Default::default()
        };

        let status = doctor_tls_status(&config);

        assert!(status.certificate_verification);
        assert!(status.insecure_skip_tls_verify);
        assert_eq!(status.provider, "openai");
        assert!(status.message.contains("cannot be disabled"));
        assert!(status.message.contains("SSL_CERT_FILE"));
    }

    #[test]
    fn provider_capability_report_exposes_alias_deprecation_for_deepseek_chat() {
        let mut config = Config {
            default_text_model: Some("deepseek-chat".to_string()),
            ..Default::default()
        };
        crate::config::normalize_model_config_for_test(&mut config);

        let report = provider_capability_report(&config);

        assert_eq!(report["resolved_model"], "deepseek-v4-flash");
        assert_eq!(report["context_window"], 1_000_000);
        assert_eq!(report["thinking_supported"], true);
        assert_eq!(report["alias_deprecation"]["alias"], "deepseek-chat");
        assert_eq!(
            report["alias_deprecation"]["replacement"],
            "deepseek-v4-flash"
        );
        assert_eq!(
            report["alias_deprecation"]["retirement_utc"],
            "2026-07-24T15:59:00Z"
        );
    }

    #[test]
    fn provider_capability_report_preserves_custom_deepseek_alias_namespace() {
        let mut config = Config {
            default_text_model: Some("deepseek-chat".to_string()),
            ..Default::default()
        }
        .with_legacy_root(None, Some("https://models.example/v1".to_string()));
        crate::config::normalize_model_config_for_test(&mut config);

        let report = provider_capability_report(&config);

        assert_eq!(report["resolved_model"], "deepseek-chat");
        assert!(report["alias_deprecation"].is_null());
    }

    #[test]
    fn provider_capability_report_leaves_canonical_flash_alias_metadata_null() {
        let config = Config {
            default_text_model: Some("deepseek-v4-flash".to_string()),
            ..Default::default()
        };

        let report = provider_capability_report(&config);

        assert_eq!(report["resolved_model"], "deepseek-v4-flash");
        assert!(report["alias_deprecation"].is_null());
    }

    /// The vendor reversed the planned retirement; Pro remains its own route.
    #[test]
    fn provider_capability_report_preserves_v4_pro_without_retirement() {
        let mut config = Config {
            default_text_model: Some("deepseek-v4-pro".to_string()),
            ..Default::default()
        }
        .with_legacy_root(
            None,
            Some(crate::config::DEFAULT_DEEPSEEK_BASE_URL.to_string()),
        );
        crate::config::normalize_model_config_for_test(&mut config);
        let report = provider_capability_report(&config);
        assert_eq!(report["resolved_model"], "deepseek-v4-pro");
        assert!(report["alias_deprecation"].is_null());
        assert!(
            crate::config::provider_capability(
                crate::config::ProviderKind::Deepseek,
                "deepseek-v4-pro"
            )
            .alias_deprecation
            .is_none()
        );
    }

    /// A custom endpoint owns the same model strings; DeepSeek's retirement is
    /// not a claim CodeWhale may make about someone else's host.
    #[test]
    fn provider_capability_report_leaves_custom_v4_pro_namespace_untouched() {
        let mut config = Config {
            default_text_model: Some("deepseek-v4-pro".to_string()),
            ..Default::default()
        }
        .with_legacy_root(None, Some("https://models.example/v1".to_string()));
        crate::config::normalize_model_config_for_test(&mut config);

        let report = provider_capability_report(&config);

        assert_eq!(report["resolved_model"], "deepseek-v4-pro");
        assert!(report["alias_deprecation"].is_null());
    }

    #[test]
    fn doctor_route_report_exposes_tokenhub_openai_compatible_route_without_secret() {
        let mut providers = crate::config::ProvidersConfig::default();
        providers.openai.api_key = Some("tokenhub-secret-value".to_string());
        providers.openai.base_url = Some("https://tokenhub.tencentmaas.com/v1".to_string());
        providers.openai.model = Some("deepseek-ai/DeepSeek-V4-Pro".to_string());
        let config = Config {
            provider: Some("openai".to_string()),
            providers: Some(providers),
            ..Default::default()
        };

        let report = doctor_route_report(&config);
        let serialized = report.to_string();

        assert_eq!(report["provider"], "openai");
        assert_eq!(report["provider_source"], "config");
        assert_eq!(report["provider_config_table"], "providers.openai");
        assert_eq!(report["model"], "deepseek-ai/DeepSeek-V4-Pro");
        assert_eq!(report["wire_protocol"], "chat_completions");
        assert_eq!(
            report["base_url"]["redacted"],
            "https://tokenhub.tencentmaas.com"
        );
        assert_eq!(report["base_url"]["class"], "custom");
        assert_eq!(report["auth"]["scheme"], "bearer");
        assert_eq!(report["auth"]["source"], "config_declared");
        assert!(
            report["base_url"]["fingerprint"]
                .as_str()
                .is_some_and(|value| value.starts_with("<redacted:"))
        );
        assert!(!serialized.contains("tokenhub-secret-value"));
    }

    #[test]
    fn doctor_route_report_exposes_siliconflow_cn_provider_route() {
        let mut providers = crate::config::ProvidersConfig::default();
        providers.siliconflow_cn.api_key = Some("sf-cn-secret-value".to_string());
        providers.siliconflow_cn.base_url =
            Some(crate::config::DEFAULT_SILICONFLOW_CN_BASE_URL.to_string());
        providers.siliconflow_cn.model = Some(crate::config::DEFAULT_SILICONFLOW_MODEL.to_string());
        let config = Config {
            provider: Some("siliconflow-CN".to_string()),
            providers: Some(providers),
            ..Default::default()
        };

        let report = doctor_route_report(&config);
        let serialized = report.to_string();

        assert_eq!(report["provider"], "siliconflow-CN");
        assert_eq!(report["provider_config_table"], "providers.siliconflow_cn");
        assert_eq!(report["model"], crate::config::DEFAULT_SILICONFLOW_MODEL);
        assert_eq!(
            report["base_url"]["redacted"],
            crate::doctor::structural_url_authority(crate::config::DEFAULT_SILICONFLOW_CN_BASE_URL)
        );
        assert_eq!(report["base_url"]["class"], "default");
        assert_eq!(report["auth"]["scheme"], "bearer");
        assert_eq!(report["auth"]["source"], "config_declared");
        assert!(!serialized.contains("sf-cn-secret-value"));
    }

    #[test]
    fn doctor_route_report_names_kimi_code_context_provenance() {
        let config = Config {
            provider: Some("moonshot".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                moonshot: crate::config::ProviderConfig {
                    api_key: Some("kimi-plan-secret".to_string()),
                    base_url: Some(crate::config::DEFAULT_KIMI_CODE_BASE_URL.to_string()),
                    model: Some(crate::config::KIMI_CODE_K3_MODEL.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        let report = doctor_route_report(&config);
        let serialized = report.to_string();

        assert_eq!(report["context_window"]["tokens"], 262_144);
        assert_eq!(
            report["context_window"]["source"],
            "static Kimi Code safe floor"
        );
        assert!(!serialized.contains("kimi-plan-secret"));
    }

    #[test]
    fn provider_capability_report_uses_exact_kimi_code_route_facts() {
        let config = Config {
            provider: Some("moonshot".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                moonshot: crate::config::ProviderConfig {
                    api_key: Some("kimi-plan-secret".to_string()),
                    base_url: Some(crate::config::DEFAULT_KIMI_CODE_BASE_URL.to_string()),
                    model: Some(crate::config::KIMI_CODE_K3_MODEL.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        let report = provider_capability_report(&config);

        assert_eq!(report["resolved_model"], crate::config::KIMI_CODE_K3_MODEL);
        assert_eq!(report["context_window"], 262_144);
        assert_eq!(
            report["context_window_source"],
            "static Kimi Code safe floor"
        );
        assert_eq!(report["thinking_supported"], true);
    }

    #[test]
    fn provider_capability_report_honors_kimi_code_context_override() {
        let config = Config {
            provider: Some("moonshot".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                moonshot: crate::config::ProviderConfig {
                    api_key: Some("kimi-plan-secret".to_string()),
                    base_url: Some(crate::config::DEFAULT_KIMI_CODE_BASE_URL.to_string()),
                    model: Some(crate::config::KIMI_CODE_K3_MODEL.to_string()),
                    context_window: Some(1_048_576),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        let report = provider_capability_report(&config);

        assert_eq!(
            report["resolved_model"],
            crate::config::KIMI_CODE_K3_MODEL,
            "the configured window must preserve Kimi Code's bare wire id"
        );
        assert_eq!(report["context_window"], 1_048_576);
        assert_eq!(report["context_window_source"], "configured");
        assert_eq!(report["thinking_supported"], true);
    }

    #[test]
    fn provider_capability_report_uses_direct_moonshot_k3_route_facts() {
        let config = Config {
            provider: Some("moonshot".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                moonshot: crate::config::ProviderConfig {
                    api_key: Some("moonshot-secret".to_string()),
                    base_url: Some(crate::config::DEFAULT_MOONSHOT_BASE_URL.to_string()),
                    model: Some("kimi-k3".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };

        let report = provider_capability_report(&config);

        assert_eq!(report["resolved_model"], "kimi-k3");
        assert_eq!(report["context_window"], 1_048_576);
        assert_eq!(report["context_window_source"], "catalog");
        assert_eq!(report["max_output"], 1_048_576);
        assert_eq!(report["thinking_supported"], true);
    }

    #[test]
    fn doctor_search_provider_line_includes_firecrawl_default_source_and_switch_hint() {
        let _guard = crate::test_support::lock_test_env();
        // A Default pin means all three Tavily-resolution signals are absent.
        let prev_code = std::env::var_os("CODEWHALE_SEARCH_PROVIDER");
        let prev = std::env::var_os("DEEPSEEK_SEARCH_PROVIDER");
        let prev_tavily = std::env::var_os("TAVILY_API_KEY");
        unsafe {
            std::env::remove_var("CODEWHALE_SEARCH_PROVIDER");
            std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER");
            std::env::remove_var("TAVILY_API_KEY");
        }

        let line = doctor_search_provider_line(&Config::default());

        match prev_code {
            Some(value) => unsafe { std::env::set_var("CODEWHALE_SEARCH_PROVIDER", value) },
            None => unsafe { std::env::remove_var("CODEWHALE_SEARCH_PROVIDER") },
        }
        match prev {
            Some(value) => unsafe { std::env::set_var("DEEPSEEK_SEARCH_PROVIDER", value) },
            None => unsafe { std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER") },
        }
        match prev_tavily {
            Some(value) => unsafe { std::env::set_var("TAVILY_API_KEY", value) },
            None => unsafe { std::env::remove_var("TAVILY_API_KEY") },
        }
        assert!(line.contains("search_provider: firecrawl"));
        assert!(line.contains("source: default"));
        assert!(line.contains("[search] provider"));
        assert!(line.contains("provider = \"baidu\""));
        assert!(!line.contains("missing"), "got `{line}`");
    }

    #[test]
    fn doctor_search_provider_line_reports_autodetected_tavily_key_without_missing_key() {
        let _guard = crate::test_support::lock_test_env();
        let prev_code = std::env::var_os("CODEWHALE_SEARCH_PROVIDER");
        let prev = std::env::var_os("DEEPSEEK_SEARCH_PROVIDER");
        let prev_tavily = std::env::var_os("TAVILY_API_KEY");
        unsafe {
            std::env::remove_var("CODEWHALE_SEARCH_PROVIDER");
            std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER");
            std::env::set_var("TAVILY_API_KEY", "tvly-test-doctor");
        }

        let config = Config::default();
        let line = doctor_search_provider_line(&config);
        let report = doctor_search_provider_json(&config);

        match prev_code {
            Some(value) => unsafe { std::env::set_var("CODEWHALE_SEARCH_PROVIDER", value) },
            None => unsafe { std::env::remove_var("CODEWHALE_SEARCH_PROVIDER") },
        }
        match prev {
            Some(value) => unsafe { std::env::set_var("DEEPSEEK_SEARCH_PROVIDER", value) },
            None => unsafe { std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER") },
        }
        match prev_tavily {
            Some(value) => unsafe { std::env::set_var("TAVILY_API_KEY", value) },
            None => unsafe { std::env::remove_var("TAVILY_API_KEY") },
        }

        assert_eq!(line, "search_provider: tavily (source: tavily key)");
        assert_eq!(report["provider"], "tavily");
        assert_eq!(report["source"], "tavily key");
        assert!(
            report.get("missing_key").is_none(),
            "missing-key is stdout-only: {report}"
        );
        // Autodetect is runtime-only; nothing was written to the config view.
        assert_eq!(
            config.search.as_ref().and_then(|search| search.provider),
            None
        );
    }

    #[test]
    fn doctor_search_provider_line_reports_explicit_tavily_missing_key() {
        let _guard = crate::test_support::lock_test_env();
        let prev_code = std::env::var_os("CODEWHALE_SEARCH_PROVIDER");
        let prev = std::env::var_os("DEEPSEEK_SEARCH_PROVIDER");
        let prev_tavily = std::env::var_os("TAVILY_API_KEY");
        unsafe {
            std::env::remove_var("CODEWHALE_SEARCH_PROVIDER");
            std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER");
            std::env::remove_var("TAVILY_API_KEY");
        }
        let config = Config {
            search: Some(crate::config::SearchConfig {
                provider: Some(crate::config::SearchProvider::Tavily),
                base_url: None,
                api_key: None,
                native: None,
            }),
            ..Default::default()
        };

        let line = doctor_search_provider_line(&config);

        // The env-override arm of the same rule.
        unsafe { std::env::set_var("CODEWHALE_SEARCH_PROVIDER", "tavily") };
        let env_line = doctor_search_provider_line(&Config::default());

        match prev_code {
            Some(value) => unsafe { std::env::set_var("CODEWHALE_SEARCH_PROVIDER", value) },
            None => unsafe { std::env::remove_var("CODEWHALE_SEARCH_PROVIDER") },
        }
        match prev {
            Some(value) => unsafe { std::env::set_var("DEEPSEEK_SEARCH_PROVIDER", value) },
            None => unsafe { std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER") },
        }
        match prev_tavily {
            Some(value) => unsafe { std::env::set_var("TAVILY_API_KEY", value) },
            None => unsafe { std::env::remove_var("TAVILY_API_KEY") },
        }

        assert_eq!(
            line,
            "search_provider: tavily (source: config); missing TAVILY_API_KEY or [search] api_key"
        );
        assert_eq!(
            env_line,
            "search_provider: tavily (source: env override); missing TAVILY_API_KEY or [search] api_key"
        );
        // Missing-key is stdout-only.
        assert!(
            doctor_search_provider_json(&config)
                .get("missing_key")
                .is_none()
        );
    }

    #[test]
    fn doctor_search_provider_json_reports_config_source() {
        let _guard = crate::test_support::lock_test_env();
        let prev = std::env::var_os("DEEPSEEK_SEARCH_PROVIDER");
        unsafe { std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER") };
        let config = Config {
            search: Some(crate::config::SearchConfig {
                provider: Some(crate::config::SearchProvider::DuckDuckGo),
                base_url: None,
                api_key: None,
                native: None,
            }),
            ..Default::default()
        };

        let report = doctor_search_provider_json(&config);

        match prev {
            Some(value) => unsafe { std::env::set_var("DEEPSEEK_SEARCH_PROVIDER", value) },
            None => unsafe { std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER") },
        }
        assert_eq!(report["provider"], "duckduckgo");
        assert_eq!(report["source"], "config");
        assert_eq!(report["reachability"], "not_checked");
        assert_eq!(report["reachability_reason"], "offline_json");
    }

    #[test]
    fn doctor_search_provider_json_reports_env_override_source() {
        let _guard = crate::test_support::lock_test_env();
        let prev = std::env::var_os("DEEPSEEK_SEARCH_PROVIDER");
        unsafe { std::env::set_var("DEEPSEEK_SEARCH_PROVIDER", "tavily") };

        let report = doctor_search_provider_json(&Config::default());

        match prev {
            Some(value) => unsafe { std::env::set_var("DEEPSEEK_SEARCH_PROVIDER", value) },
            None => unsafe { std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER") },
        }
        assert_eq!(report["provider"], "tavily");
        assert_eq!(report["source"], "env override");
        assert_eq!(report["reachability"], "not_checked");
    }

    #[test]
    fn doctor_search_provider_line_says_firecrawl_needs_no_key_until_one_is_set() {
        use crate::test_support::EnvVarGuard;
        let _guard = crate::test_support::lock_test_env();
        let _env = [
            "CODEWHALE_SEARCH_PROVIDER",
            "DEEPSEEK_SEARCH_PROVIDER",
            "TAVILY_API_KEY",
            "FIRECRAWL_API_KEY",
        ]
        .map(EnvVarGuard::remove);
        let with_config_key = |api_key: &str| Config {
            search: Some(crate::config::SearchConfig {
                provider: Some(crate::config::SearchProvider::Firecrawl),
                base_url: None,
                api_key: Some(api_key.to_string()),
                native: None,
            }),
            ..Default::default()
        };

        let keyless = doctor_search_provider_line(&Config::default());
        let config_key = doctor_search_provider_line(&with_config_key("fc-test"));
        let env_key = {
            let _key = EnvVarGuard::set("FIRECRAWL_API_KEY", "fc-test");
            // web_search lets a blank `[search] api_key` shadow the env key.
            let shadowed = doctor_search_provider_line(&with_config_key(" "));
            assert!(shadowed.contains("without an API key"), "got `{shadowed}`");
            doctor_search_provider_line(&Config::default())
        };

        assert_eq!(
            keyless,
            "search_provider: firecrawl (source: default; set [search] provider = \"baidu\" | \"metaso\" | \"volcengine\" for China); works without an API key (limited quota; set FIRECRAWL_API_KEY or [search] api_key to raise it)"
        );
        assert_eq!(config_key, "search_provider: firecrawl (source: config)");
        assert!(!env_key.contains("without an API key"), "got `{env_key}`");
        assert!(
            doctor_search_provider_json(&Config::default())
                .get("keyless")
                .is_none(),
            "the note is stdout-only"
        );
    }

    #[test]
    fn doctor_search_provider_line_omits_switch_hint_when_bing_is_configured() {
        let _guard = crate::test_support::lock_test_env();
        let prev = std::env::var_os("DEEPSEEK_SEARCH_PROVIDER");
        unsafe { std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER") };
        let config = Config {
            search: Some(crate::config::SearchConfig {
                provider: Some(crate::config::SearchProvider::Bing),
                base_url: None,
                api_key: None,
                native: None,
            }),
            ..Default::default()
        };

        let line = doctor_search_provider_line(&config);

        match prev {
            Some(value) => unsafe { std::env::set_var("DEEPSEEK_SEARCH_PROVIDER", value) },
            None => unsafe { std::env::remove_var("DEEPSEEK_SEARCH_PROVIDER") },
        }
        assert!(line.contains("search_provider: bing"));
        assert!(line.contains("source: config"));
        assert!(!line.contains("[search] provider"));
    }

    #[test]
    fn timeout_recovery_keeps_default_deepseek_users_on_default_endpoint() {
        let config = Config::default();

        let text = doctor_timeout_recovery_lines(&config).join("\n");

        assert!(text.contains("api.deepseek.com"));
        assert!(text.contains("custom DeepSeek-compatible endpoint"));
        assert!(!text.contains("provider = \"deepseek-cn\""));
        assert!(text.contains("codewhale doctor --json"));
    }

    #[test]
    fn timeout_recovery_for_custom_provider_checks_openai_compatibility() {
        let config = Config {
            provider: Some("vllm".to_string()),
            ..Default::default()
        };

        let text = doctor_timeout_recovery_lines(&config).join("\n");

        assert!(text.contains("/v1/models"));
        assert!(text.contains("/v1/chat/completions"));
        assert!(!text.contains("api.deepseeki.com"));
        // #6889: a timeout may be a model that is still loading.
        assert!(text.contains("the model may still be loading"), "{text}");
    }
}

#[cfg(test)]
mod terminal_mode_tests {
    use super::*;
    use clap::Parser;

    include!("lib/terminal_mode_test_cases_01.rs");
    include!("lib/terminal_mode_test_cases_02.rs");
    include!("lib/terminal_mode_test_cases_03.rs");
}

#[cfg(test)]
mod interactive_startup_tests {
    use super::*;

    #[test]
    fn interactive_tui_defaults_agent_shell_to_approval_gated_on() {
        let default_config = Config::default();
        assert!(
            interactive_tui_allow_shell(false, &default_config),
            "interactive Agent mode should expose shell tools by default so approvals can gate commands"
        );

        let disabled = Config {
            allow_shell: Some(false),
            ..Config::default()
        };
        assert!(
            !interactive_tui_allow_shell(false, &disabled),
            "explicit allow_shell=false still hides shell tools"
        );

        assert!(
            interactive_tui_allow_shell(true, &disabled),
            "YOLO forces shell access for its no-guardrails contract"
        );
    }
}

#[cfg(test)]
mod mounted_history_tests {
    use super::*;
    use crate::runtime_threads::{
        RuntimeThreadManager, RuntimeThreadManagerConfig, ThreadListFilter,
    };
    use crate::session_manager::{SavedSession, SessionGoalState, SessionWorkState};
    use crate::session_tree::{SessionEntryKind, SessionJournal};
    use crate::test_support::{EnvVarGuard, lock_test_env};

    fn config() -> Config {
        let mut config = Config {
            provider: Some("CaseRoute".into()),
            allow_shell: Some(false),
            providers: Some(crate::config::ProvidersConfig {
                custom: std::collections::HashMap::from([(
                    "CaseRoute".into(),
                    crate::config::ProviderConfig {
                        kind: Some("openai-compatible".into()),
                        base_url: Some("http://127.0.0.1:1/v1".into()),
                        model: Some("mounted-fixture".into()),
                        api_key: Some("owned-no-call-fixture".into()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }),
            ..Config::default()
        };
        config.set_feature("mcp", false).unwrap();
        config.set_feature("subagents", false).unwrap();
        config
    }

    fn cli(workspace: &Path) -> Cli {
        let mut cli = Cli::try_parse_from(["codewhale", "--no-project-config"]).unwrap();
        cli.options.workspace = Some(workspace.to_path_buf());
        let path = workspace.parent().unwrap().join("owned-config.toml");
        std::fs::write(&path, "allow_shell = false\n").unwrap();
        cli.options.config = Some(path);
        cli
    }

    fn source(manager: &SessionManager, workspace: &Path) -> Result<(SavedSession, PathBuf)> {
        let mut journal = SessionJournal::new();
        let root = journal.append(SessionEntryKind::User {
            text: "root request".into(),
        });
        let active = journal.append(SessionEntryKind::Assistant {
            text: "active answer".into(),
        });
        journal.branch_to(&root).map_err(anyhow::Error::msg)?;
        journal.append(SessionEntryKind::Assistant {
            text: "unselected branch must survive".into(),
        });
        journal.branch_to(&active).map_err(anyhow::Error::msg)?;
        let mut saved = create_saved_session(
            &journal.to_messages(),
            "mounted-fixture",
            workspace,
            41,
            Some(&SystemPrompt::Text("original bounded guidance".into())),
        );
        saved
            .metadata
            .set_model_provider_route("custom", Some("CaseRoute"));
        saved.metadata.mode = Some("agent".into());
        saved.metadata.cost.session_cost_usd = 1.25;
        saved.metadata.cost.priced_turns = 2;
        saved.metadata.cost.coverage_recorded = true;
        saved
            .metadata
            .cost
            .route_receipts
            .insert("owned-redacted-route-receipt".into());
        saved.work_state = Some(SessionWorkState {
            plan: crate::tools::plan::PlanSnapshot {
                title: Some("Keep the original work".into()),
                objective: Some("Full history across branches".into()),
                ..Default::default()
            },
            ..Default::default()
        });
        saved.leaf_id = journal.leaf_id.clone();
        saved.journal = Some(journal);
        let path = manager.save_session(&saved)?;
        // Use the protected on-disk projection as the exact test source.
        let saved = manager.load_session_snapshot_bounded(
            &saved.metadata.id,
            codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES,
        )?;
        Ok((saved, path))
    }

    fn manager_config(config: &Config, workspace: &Path, id: &str) -> RuntimeThreadManagerConfig {
        RuntimeThreadManagerConfig::for_session(
            crate::task_manager::TaskManagerConfig::from_runtime(
                config,
                workspace.to_path_buf(),
                None,
                None,
            )
            .data_dir,
            id,
        )
    }

    fn plugins(workspace: &Path) -> Arc<crate::plugins::PluginRegistry> {
        Arc::new(crate::plugins::PluginRegistry::empty(workspace))
    }

    #[tokio::test]
    async fn mounted_resume_preserves_saved_identity_graph_goal_cost_and_releases_both_leases()
    -> Result<()> {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let manager = SessionManager::default_location()?;
        let (original, _) = source(&manager, &workspace)?;
        let goal: SessionGoalState = serde_json::from_value(serde_json::json!({
            "schema_version":1,"objective":"Resume the same objective","status":"paused",
            "tokens_used":17,"continuation_count":2,"goal_id":"owned-goal"
        }))?;
        manager.save_session_goal(&original.metadata.id, Some(&goal))?;
        let cli = cli(&workspace);
        let (prepared, id) = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            original.metadata.id.clone(),
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await?;
        assert_eq!(
            id, original.metadata.id,
            "resume must keep the original saved/cost scope"
        );
        let resumed = manager
            .load_session_snapshot_bounded(&id, codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES)?;
        assert_eq!(resumed.journal, original.journal);
        assert_eq!(resumed.messages, original.messages);
        assert_eq!(resumed.work_state, original.work_state);
        assert_eq!(resumed.system_prompt, original.system_prompt);
        assert_eq!(
            serde_json::to_value(&resumed.metadata.cost)?,
            serde_json::to_value(&original.metadata.cost)?
        );
        assert_eq!(
            serde_json::to_value(manager.load_session_goal(&id)?)?,
            serde_json::to_value(Some(&goal))?
        );
        assert_eq!(
            resumed.metadata.model_provider_id.as_deref(),
            Some("CaseRoute")
        );
        assert_eq!(
            prepared
                .config
                .active_provider_identity()
                .unwrap()
                .key
                .as_str(),
            "CaseRoute"
        );
        let binding = resumed
            .metadata
            .runtime_store
            .as_ref()
            .context("canonical saved binding")?;
        let runtime = RuntimeThreadManager::open_existing_session(
            prepared.config.clone(),
            workspace.clone(),
            manager_config(&prepared.config, &workspace, &id),
            plugins(&workspace),
            binding,
        )?;
        let rows = runtime
            .list_threads(ThreadListFilter::IncludeArchived, None)
            .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id.as_deref(), Some(id.as_str()));
        assert_eq!(rows[0].model_provider_id.as_deref(), Some("CaseRoute"));
        runtime.shutdown_and_wait().await?;
        drop(runtime);
        // These are the actual process/session guards mounted startup acquires.
        drop(manager.reserve_session_for_attach(&id)?);
        let (_, same_id) = prepare_mounted_session(
            &cli,
            prepared,
            id.clone(),
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await?;
        assert_eq!(
            same_id, id,
            "a later invocation reuses the unique canonical holder"
        );
        Ok(())
    }

    #[tokio::test]
    async fn mounted_forks_keep_every_branch_and_source_bytes_with_distinct_durable_ids()
    -> Result<()> {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let manager = SessionManager::default_location()?;
        let (original, path) = source(&manager, &workspace)?;
        let before = std::fs::read(&path)?;
        let source_goal: SessionGoalState = serde_json::from_value(serde_json::json!({
            "schema_version":1,"objective":"Continue the original local objective","status":"active",
            "token_budget":100000,"tokens_used":17,"time_used_seconds":9,"elapsed_seconds":20,
            "continuation_count":3,"goal_id":"owned-fork-goal","last_gap_fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "repeated_gap_count":1,"last_gap_pass":2
        }))?;
        manager.save_session_goal(&original.metadata.id, Some(&source_goal))?;
        let mut fork_goal = source_goal.clone();
        fork_goal.status = crate::session_manager::SessionGoalStatus::Paused;
        fork_goal.pause_reason = None;
        let cli = cli(&workspace);
        let mut ids = Vec::new();
        for _ in 0..2 {
            let (prepared, id) = prepare_mounted_session(
                &cli,
                prepare_interactive_config(&cli, &config(), true)?,
                original.metadata.id.clone(),
                MountedHistoryIntent::Fork,
                plugins(&workspace),
            )
            .await?;
            let fork = manager.load_session_snapshot_bounded(
                &id,
                codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES,
            )?;
            let old = original.journal.as_ref().unwrap();
            let full = fork.journal.as_ref().unwrap();
            assert_eq!(
                full.entries, old.entries,
                "inactive branches cannot collapse into the current message projection"
            );
            assert_eq!(full.leaf_id, old.leaf_id);
            assert_eq!(full.spawn_depth, old.spawn_depth + 1);
            assert_eq!(fork.messages, original.messages);
            assert_eq!(
                fork.metadata.parent_session_id.as_deref(),
                Some(original.metadata.id.as_str())
            );
            assert_eq!(
                fork.metadata.model_provider_id.as_deref(),
                Some("CaseRoute")
            );
            assert_eq!(fork.work_state, original.work_state);
            assert_eq!(
                serde_json::to_value(&fork.metadata.cost)?,
                serde_json::to_value(&original.metadata.cost)?
            );
            assert_eq!(
                prepared
                    .config
                    .active_provider_identity()
                    .unwrap()
                    .key
                    .as_str(),
                "CaseRoute"
            );
            assert_eq!(
                serde_json::to_value(manager.load_session_goal(&id)?)?,
                serde_json::to_value(Some(&fork_goal))?,
                "actual local goal continuation is copied and paused without a provider turn"
            );
            assert_eq!(
                serde_json::to_value(manager.load_session_goal(&original.metadata.id)?)?,
                serde_json::to_value(Some(&source_goal))?,
                "fork cannot consume or rewrite the source's local goal"
            );
            drop(manager.reserve_session_for_attach(&id)?);
            assert_ne!(id, original.metadata.id);
            ids.push(id);
        }
        assert_ne!(
            ids[0], ids[1],
            "new user invocations must not reuse the preceding fork operation"
        );
        assert_eq!(
            std::fs::read(path)?,
            before,
            "fork never rewrites the source document"
        );
        Ok(())
    }

    #[tokio::test]
    async fn mounted_handoff_refuses_live_saved_session_without_copying_history() -> Result<()> {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let manager = SessionManager::default_location()?;
        let (saved, path) = source(&manager, &workspace)?;
        let before = std::fs::read(&path)?;
        let lease = manager.hold_live_lease_elsewhere(&saved.metadata.id);
        let cli = cli(&workspace);
        let error = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            saved.metadata.id,
            MountedHistoryIntent::Fork,
            plugins(&workspace),
        )
        .await
        .err()
        .context("live source must refuse")?;
        assert!(format!("{error:#}").contains("open"), "{error:#}");
        assert_eq!(manager.list_sessions()?.len(), 1);
        assert_eq!(std::fs::read(path)?, before);
        drop(lease);
        Ok(())
    }

    #[tokio::test]
    async fn mounted_handoff_refuses_held_runtime_owner_before_history_mutation() -> Result<()> {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let manager = SessionManager::default_location()?;
        let (mut saved, _) = source(&manager, &workspace)?;
        let runtime = RuntimeThreadManager::open_for_session(
            config(),
            workspace.clone(),
            manager_config(&config(), &workspace, &saved.metadata.id),
            plugins(&workspace),
            None,
        )?;
        saved.metadata.runtime_store = Some(runtime.session_store_binding());
        let path = manager.save_session(&saved)?;
        let before = std::fs::read(&path)?;
        let cli = cli(&workspace);
        assert!(
            prepare_mounted_session(
                &cli,
                prepare_interactive_config(&cli, &config(), true)?,
                saved.metadata.id,
                MountedHistoryIntent::Resume,
                plugins(&workspace),
            )
            .await
            .is_err(),
            "a second actual owner cannot acquire the held store"
        );
        assert!(
            runtime
                .list_threads(ThreadListFilter::IncludeArchived, None)
                .await?
                .is_empty()
        );
        assert_eq!(manager.list_sessions()?.len(), 1);
        assert_eq!(std::fs::read(path)?, before);
        runtime.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test]
    async fn mounted_missing_bound_store_refuses_without_recovered_empty_replacement() -> Result<()>
    {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let manager = SessionManager::default_location()?;
        let (mut saved, _) = source(&manager, &workspace)?;
        let runtime = RuntimeThreadManager::open_for_session(
            config(),
            workspace.clone(),
            manager_config(&config(), &workspace, &saved.metadata.id),
            plugins(&workspace),
            None,
        )?;
        let binding = runtime.session_store_binding();
        runtime.shutdown_and_wait().await?;
        drop(runtime);
        saved.metadata.runtime_store = Some(binding.clone());
        let path = manager.save_session(&saved)?;
        let before = std::fs::read(&path)?;
        std::fs::rename(
            &binding.data_dir,
            root.join("retained-missing-store-evidence"),
        )?;
        let cli = cli(&workspace);
        assert!(
            prepare_mounted_session(
                &cli,
                prepare_interactive_config(&cli, &config(), true)?,
                saved.metadata.id,
                MountedHistoryIntent::Resume,
                plugins(&workspace),
            )
            .await
            .is_err()
        );
        assert!(
            !binding.data_dir.exists(),
            "mounted resume cannot recover into an empty store"
        );
        assert_eq!(manager.list_sessions()?.len(), 1);
        assert_eq!(std::fs::read(path)?, before);
        Ok(())
    }

    #[tokio::test]
    async fn mounted_handoff_reuses_captured_project_config_after_source_changes() -> Result<()> {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let project = workspace.join(codewhale_config::CODEWHALE_APP_DIR);
        std::fs::create_dir(&project)?;
        let project_path = project.join("config.toml");
        std::fs::write(&project_path, "allow_shell = false\n")?;
        let manager = SessionManager::default_location()?;
        let (saved, _) = source(&manager, &workspace)?;
        let mut cli = cli(&workspace);
        cli.options.no_project_config = false;
        let prepared = prepare_interactive_config(&cli, &config(), true)?;
        assert_eq!(prepared.config.allow_shell, Some(false));
        std::fs::write(project_path, "allow_shell = true\n")?;
        let (prepared, _) = prepare_mounted_session(
            &cli,
            prepared,
            saved.metadata.id,
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await?;
        assert_eq!(
            prepared.config.allow_shell,
            Some(false),
            "the mounted entry must not re-read later ambient grants"
        );
        let identity = prepared.config.active_provider_identity().unwrap();
        assert_eq!(identity.key.as_str(), "CaseRoute");
        assert_eq!(
            prepared
                .config
                .provider_config_for(&identity)
                .unwrap()
                .base_url
                .as_deref(),
            Some("http://127.0.0.1:1/v1")
        );
        Ok(())
    }
    fn retained_operation_key(
        binding: &crate::runtime_threads::RuntimeStoreBinding,
    ) -> Result<String> {
        // Read only this owned fixture's real, single durable history intent.
        let files = std::fs::read_dir(binding.data_dir.join("turn-operations"))?
            .collect::<std::io::Result<Vec<_>>>()?;
        let files = files
            .into_iter()
            .filter(|row| row.file_name().to_string_lossy().starts_with("history_"))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            files.len() == 1,
            "fixture expects exactly one history operation"
        );
        let record: serde_json::Value = serde_json::from_slice(&std::fs::read(files[0].path())?)?;
        Ok(record["receipt"]["operation_key"]
            .as_str()
            .context("durable operation key")?
            .to_string())
    }

    #[tokio::test]
    async fn mounted_retained_fork_lookup_survives_changed_source_without_replay_or_wrong_intent_attach()
    -> Result<()> {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let manager = SessionManager::default_location()?;
        let (mut saved, source_path) = source(&manager, &workspace)?;
        let mut cli = cli(&workspace);
        let (_, fork_id) = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            saved.metadata.id.clone(),
            MountedHistoryIntent::Fork,
            plugins(&workspace),
        )
        .await?;
        let fork = manager.load_session_snapshot_bounded(
            &fork_id,
            codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES,
        )?;
        let key = retained_operation_key(fork.metadata.runtime_store.as_ref().unwrap())?;
        let journal = saved.journal.as_mut().unwrap();
        journal.append(SessionEntryKind::User {
            text: "a genuine later append".into(),
        });
        saved.messages = journal.to_messages();
        saved.leaf_id = journal.leaf_id.clone();
        manager.save_session(&saved)?;
        let changed_source = std::fs::read(&source_path)?;
        cli.operation_key = Some(key.clone());
        let (_, recovered_id) = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            saved.metadata.id.clone(),
            MountedHistoryIntent::Fork,
            plugins(&workspace),
        )
        .await?;
        assert_eq!(
            recovered_id, fork_id,
            "the original key is looked up before the new document digest is proposed"
        );
        assert_eq!(manager.list_sessions()?.len(), 2);
        let recovered = manager.load_session_snapshot_bounded(
            &recovered_id,
            codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES,
        )?;
        assert_eq!(recovered.journal, fork.journal);
        assert_eq!(std::fs::read(&source_path)?, changed_source);
        let error = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            saved.metadata.id.clone(),
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await
        .err()
        .context("Fork key cannot authorize Resume intent")?;
        assert!(format!("{error:#}").contains(&key));
        assert!(format!("{error:#}").contains("action/source"));
        cli.operation_key = Some("owned-absent-key".into());
        let error = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            saved.metadata.id,
            MountedHistoryIntent::Fork,
            plugins(&workspace),
        )
        .await
        .err()
        .context("absent key must not create a new fork")?;
        assert!(format!("{error:#}").contains("owned-absent-key"));
        assert_eq!(manager.list_sessions()?.len(), 2);
        assert_eq!(std::fs::read(source_path)?, changed_source);
        Ok(())
    }

    fn mark_published_operation_pending(
        binding: &crate::runtime_threads::RuntimeStoreBinding,
        key: &str,
    ) -> Result<PathBuf> {
        let files = std::fs::read_dir(binding.data_dir.join("turn-operations"))?
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .filter(|row| row.file_name().to_string_lossy().starts_with("history_"))
            .collect::<Vec<_>>();
        anyhow::ensure!(
            files.len() == 1,
            "fixture expects one actual history operation"
        );
        let path = files[0].path();
        let mut record: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        anyhow::ensure!(
            record["receipt"]["operation_key"].as_str() == Some(key)
                && record["committed"] == true
                && record["target_document_digest"].is_string()
                && record["journal_witness"].is_object(),
            "fixture must retain actual prepared publication and complete witness"
        );
        // Simulate only interruption before the last intent commit in this
        // owned fixture; all actual graph/document/checkpoint proofs stay exact.
        record["committed"] = serde_json::json!(false);
        std::fs::write(&path, serde_json::to_vec_pretty(&record)?)?;
        Ok(path)
    }

    #[tokio::test]
    async fn mounted_retained_pending_resume_finishes_published_source_once_without_reconstructing_intent()
    -> Result<()> {
        use crate::runtime_api::thread_history::saved_document_digest;
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let manager = SessionManager::default_location()?;
        let (original, path) = source(&manager, &workspace)?;
        let original_digest = saved_document_digest(&original)?;
        let mut cli = cli(&workspace);
        let (prepared, id) = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            original.metadata.id.clone(),
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await?;
        let published = manager
            .load_session_snapshot_bounded(&id, codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES)?;
        assert_ne!(
            saved_document_digest(&published)?,
            original_digest,
            "same-session publication changes source bytes before the final intent commit"
        );
        let binding = published
            .metadata
            .runtime_store
            .as_ref()
            .context("published owner binding")?;
        let key = retained_operation_key(binding)?;
        let record_path = mark_published_operation_pending(binding, &key)?;
        let published_bytes = std::fs::read(&path)?;
        cli.operation_key = Some(key.clone());
        let wrong = prepare_mounted_session(
            &cli,
            PreparedInteractiveConfig {
                config: prepared.config.clone(),
                workspace: prepared.workspace.clone(),
            },
            id.clone(),
            MountedHistoryIntent::Fork,
            plugins(&workspace),
        )
        .await
        .err()
        .context("pending Resume cannot authorize Fork")?;
        assert!(format!("{wrong:#}").contains("action/source"));
        assert!(format!("{wrong:#}").contains(&key));
        let pending: serde_json::Value = serde_json::from_slice(&std::fs::read(&record_path)?)?;
        assert_eq!(pending["committed"], false);
        let (recovered, same_id) = prepare_mounted_session(
            &cli,
            prepared,
            id.clone(),
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await?;
        assert_eq!(same_id, id);
        assert_eq!(std::fs::read(&path)?, published_bytes);
        let committed_bytes = std::fs::read(&record_path)?;
        let committed: serde_json::Value = serde_json::from_slice(&committed_bytes)?;
        assert_eq!(committed["committed"], true);
        let (_, repeated_id) = prepare_mounted_session(
            &cli,
            PreparedInteractiveConfig {
                config: recovered.config.clone(),
                workspace: recovered.workspace.clone(),
            },
            id.clone(),
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await?;
        assert_eq!(repeated_id, id);
        assert_eq!(
            std::fs::read(&record_path)?,
            committed_bytes,
            "completed same-key recovery must not mint or recommit another outcome"
        );
        assert_eq!(manager.list_sessions()?.len(), 1);
        let runtime = RuntimeThreadManager::open_existing_session(
            recovered.config.clone(),
            workspace.clone(),
            manager_config(&recovered.config, &workspace, &id),
            plugins(&workspace),
            binding,
        )?;
        let rows = runtime
            .list_threads(ThreadListFilter::IncludeArchived, None)
            .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id.as_deref(), Some(id.as_str()));
        runtime.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test]
    async fn mounted_pending_recovery_refuses_foreign_association_workspace_and_changed_prepared_history()
    -> Result<()> {
        use crate::runtime_api::thread_history::{
            lookup_thread_history_operation_in_runtime, recover_thread_history_operation_in_runtime,
        };
        use codewhale_protocol::{
            CanonicalThreadOperationKind, CanonicalThreadOperationLookup,
            CanonicalThreadOperationRecovery, CanonicalThreadOperationStatus,
        };
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        let foreign_workspace = root.join("foreign-workspace");
        std::fs::create_dir(&workspace)?;
        std::fs::create_dir(&foreign_workspace)?;
        let manager = SessionManager::default_location()?;
        let (original, path) = source(&manager, &workspace)?;
        let mut cli = cli(&workspace);
        let (prepared, id) = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            original.metadata.id,
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await?;
        let mut published = manager
            .load_session_snapshot_bounded(&id, codewhale_protocol::MAX_CANONICAL_HISTORY_BYTES)?;
        let binding = published
            .metadata
            .runtime_store
            .clone()
            .context("published owner binding")?;
        let key = retained_operation_key(&binding)?;
        let record_path = mark_published_operation_pending(&binding, &key)?;
        let pending_bytes = std::fs::read(&record_path)?;
        let runtime = Arc::new(RuntimeThreadManager::open_existing_session(
            prepared.config.clone(),
            workspace.clone(),
            manager_config(&prepared.config, &workspace, &id),
            plugins(&workspace),
            &binding,
        )?);
        let lookup = CanonicalThreadOperationLookup {
            version: 1,
            operation_key: key.clone(),
            expected_data_dir: binding.data_dir.clone(),
            expected_execution_scope: binding.execution_scope.clone(),
            workspace: workspace.clone(),
        };
        let association = match lookup_thread_history_operation_in_runtime(
            &runtime,
            manager.sessions_dir(),
            lookup.clone(),
        )
        .await?
        {
            CanonicalThreadOperationStatus::Pending { association, .. } => association,
            _ => bail!("fixture must retain an uncommitted prepared intent"),
        };
        let mut wrong = association.clone();
        wrong.kind = CanonicalThreadOperationKind::Fork;
        let error = recover_thread_history_operation_in_runtime(
            &runtime,
            manager.sessions_dir(),
            CanonicalThreadOperationRecovery {
                operation: lookup.clone(),
                association: wrong,
            },
        )
        .await
        .err()
        .context("owner must reject changed action/source before write")?;
        assert!(format!("{error:#}").contains("association"));
        let mut wrong_workspace = lookup;
        wrong_workspace.workspace = foreign_workspace;
        let error = recover_thread_history_operation_in_runtime(
            &runtime,
            manager.sessions_dir(),
            CanonicalThreadOperationRecovery {
                operation: wrong_workspace,
                association,
            },
        )
        .await
        .err()
        .context("owner must reject another acknowledged workspace")?;
        assert!(format!("{error:#}").contains("workspace"));
        assert_eq!(std::fs::read(&record_path)?, pending_bytes);
        assert_eq!(
            runtime
                .list_threads(ThreadListFilter::IncludeArchived, None)
                .await?
                .len(),
            1
        );
        runtime.shutdown_and_wait().await?;
        drop(runtime);
        let journal = published.journal.as_mut().context("full prepared graph")?;
        journal.append(SessionEntryKind::User {
            text: "a successor makes pending preparation unsafe".into(),
        });
        published.messages = journal.to_messages();
        published.leaf_id = journal.leaf_id.clone();
        manager.save_session(&published)?;
        let changed = std::fs::read(&path)?;
        cli.operation_key = Some(key.clone());
        let error = prepare_mounted_session(
            &cli,
            prepared,
            id,
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await
        .err()
        .context("changed preparation must not be replayed or mounted")?;
        assert!(format!("{error:#}").contains(&key));
        assert_eq!(std::fs::read(&path)?, changed);
        assert_eq!(std::fs::read(&record_path)?, pending_bytes);
        assert_eq!(manager.list_sessions()?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn mounted_retained_pending_intent_refuses_attach_without_creating_or_resuming_a_thread()
    -> Result<()> {
        use crate::runtime_threads::RuntimeHistoryWitness;
        use codewhale_protocol::{
            CanonicalThreadOperationAssociation, CanonicalThreadOperationKind,
        };
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let manager = SessionManager::default_location()?;
        let (saved, path) = source(&manager, &workspace)?;
        let before = std::fs::read(&path)?;
        let runtime = RuntimeThreadManager::open_for_session(
            config(),
            workspace.clone(),
            manager_config(&config(), &workspace, &saved.metadata.id),
            plugins(&workspace),
            None,
        )?;
        let key = "owned-pending-intent";
        let operation = runtime.reserve_history_operation_for_target(
            key,
            &"a".repeat(64),
            &"b".repeat(64),
            None,
        )?;
        let journal = saved.journal.as_ref().unwrap();
        runtime.bind_history_operation_witness(
            operation,
            RuntimeHistoryWitness {
                entries_len: journal.entries.len(),
                leaf_id: journal.leaf_id.clone(),
                schema_version: journal.schema_version,
                spawn_depth: journal.spawn_depth,
                seed_from_message_index: 0,
                workspace: workspace.clone(),
            },
            CanonicalThreadOperationAssociation {
                kind: CanonicalThreadOperationKind::Fork,
                source_runtime_thread_id: None,
                source_session_id: Some(saved.metadata.id.clone()),
            },
        )?;
        runtime.shutdown_and_wait().await?;
        drop(runtime);
        let mut cli = cli(&workspace);
        cli.operation_key = Some(key.into());
        let error = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            saved.metadata.id.clone(),
            MountedHistoryIntent::Fork,
            plugins(&workspace),
        )
        .await
        .err()
        .context("pending intent must refuse UI handoff")?;
        assert!(format!("{error:#}").contains(key));
        assert!(format!("{error:#}").contains("pending"));
        let runtime = RuntimeThreadManager::open_existing_session_unbound(
            config(),
            workspace.clone(),
            manager_config(&config(), &workspace, &saved.metadata.id),
            plugins(&workspace),
        )?;
        assert!(
            runtime
                .list_threads(ThreadListFilter::IncludeArchived, None)
                .await?
                .is_empty()
        );
        assert!(
            !runtime
                .lookup_history_operation_by_key(key)?
                .unwrap()
                .committed
        );
        runtime.shutdown_and_wait().await?;
        assert_eq!(manager.list_sessions()?.len(), 1);
        assert_eq!(std::fs::read(path)?, before);
        Ok(())
    }

    #[tokio::test]
    async fn mounted_retained_unbound_lookup_refuses_missing_store_without_minting_an_owner()
    -> Result<()> {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let workspace = root.join("workspace");
        std::fs::create_dir(&workspace)?;
        let manager = SessionManager::default_location()?;
        let (saved, path) = source(&manager, &workspace)?;
        let before = std::fs::read(&path)?;
        let expected_dir = manager_config(&config(), &workspace, &saved.metadata.id).data_dir;
        let mut cli = cli(&workspace);
        cli.operation_key = Some("retained-unbound-key".into());
        let error = prepare_mounted_session(
            &cli,
            prepare_interactive_config(&cli, &config(), true)?,
            saved.metadata.id,
            MountedHistoryIntent::Resume,
            plugins(&workspace),
        )
        .await
        .err()
        .context("read-only recovery needs an existing owner")?;
        assert!(format!("{error:#}").contains("retained-unbound-key"));
        assert!(
            !expected_dir.exists(),
            "lookup must not create an empty replacement owner store"
        );
        assert_eq!(std::fs::read(path)?, before);
        Ok(())
    }

    #[test]
    fn mounted_retained_key_is_explicitly_forwarded_after_resume_or_fork_subcommand() {
        for action in ["resume", "fork"] {
            let cli = Cli::try_parse_from([
                "codewhale",
                action,
                "saved-source",
                "--operation-key",
                "owned-retained-key",
            ])
            .unwrap();
            assert_eq!(cli.operation_key.as_deref(), Some("owned-retained-key"));
            assert!(matches!(
                cli.command,
                Some(Commands::Resume { .. } | Commands::Fork { .. })
            ));
        }
    }
    #[tokio::test]
    async fn mounted_foreign_workspace_refuses_before_owner_or_mutation_with_explicit_remedy()
    -> Result<()> {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let saved_workspace = root.join("saved-project");
        let launch_workspace = root.join("different-project");
        std::fs::create_dir(&saved_workspace)?;
        std::fs::create_dir(&launch_workspace)?;
        let manager = SessionManager::default_location()?;
        let (saved, path) = source(&manager, &saved_workspace)?;
        let before = std::fs::read(&path)?;
        let expected_store =
            manager_config(&config(), &saved_workspace, &saved.metadata.id).data_dir;
        let cli = cli(&launch_workspace);
        for intent in [MountedHistoryIntent::Resume, MountedHistoryIntent::Fork] {
            let error = prepare_mounted_session(
                &cli,
                prepare_interactive_config(&cli, &config(), true)?,
                saved.metadata.id.clone(),
                intent,
                plugins(&launch_workspace),
            )
            .await
            .err()
            .context("a different project's captured grant must refuse")?;
            assert!(format!("{error:#}").contains("--workspace"));
            assert!(format!("{error:#}").contains(&saved_workspace.display().to_string()));
            assert!(
                !expected_store.exists(),
                "scope refusal precedes owner acquisition"
            );
            assert_eq!(std::fs::read(&path)?, before);
        }
        assert_eq!(manager.list_sessions()?.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn mounted_retained_key_foreign_workspace_refuses_before_lookup_and_keeps_exact_key()
    -> Result<()> {
        let _env = lock_test_env();
        let _runtime_override = EnvVarGuard::remove("CODEWHALE_RUNTIME_DIR");
        let _legacy_runtime_override = EnvVarGuard::remove("DEEPSEEK_RUNTIME_DIR");
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.join("home"));
        let saved_workspace = root.join("saved-project");
        let launch_workspace = root.join("different-project");
        std::fs::create_dir(&saved_workspace)?;
        std::fs::create_dir(&launch_workspace)?;
        let manager = SessionManager::default_location()?;
        let (saved, path) = source(&manager, &saved_workspace)?;
        let before = std::fs::read(&path)?;
        let expected_store =
            manager_config(&config(), &saved_workspace, &saved.metadata.id).data_dir;
        let mut cli = cli(&launch_workspace);
        cli.operation_key = Some("owned-original-operation".into());
        for intent in [MountedHistoryIntent::Resume, MountedHistoryIntent::Fork] {
            let error = prepare_mounted_session(
                &cli,
                prepare_interactive_config(&cli, &config(), true)?,
                saved.metadata.id.clone(),
                intent,
                plugins(&launch_workspace),
            )
            .await
            .err()
            .context("lookup cannot attach under another project's captured scope")?;
            assert!(format!("{error:#}").contains("--workspace"));
            assert!(format!("{error:#}").contains("owned-original-operation"));
            assert!(
                !expected_store.exists(),
                "scope refusal precedes existing-owner lookup too"
            );
            assert_eq!(std::fs::read(&path)?, before);
        }
        Ok(())
    }
}

#[cfg(test)]
mod project_config_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Write a `<workspace>/.deepseek/config.toml` and return the workspace
    /// root so the merge function can find it.
    fn workspace_with_project_config(body: &str) -> tempfile::TempDir {
        let tmp = tempdir().expect("tempdir");
        let project_dir = tmp.path().join(".deepseek");
        fs::create_dir_all(&project_dir).expect("mkdir .deepseek");
        fs::write(project_dir.join("config.toml"), body).expect("write project config");
        tmp
    }

    #[cfg(unix)]
    #[test]
    fn project_overlay_rejects_symlinked_primary_config() {
        let workspace = tempdir().expect("workspace tempdir");
        let outside = tempdir().expect("outside tempdir");
        let primary_dir = workspace.path().join(codewhale_config::CODEWHALE_APP_DIR);
        let legacy_dir = workspace.path().join(codewhale_config::LEGACY_APP_DIR);
        fs::create_dir_all(&primary_dir).expect("mkdir primary");
        fs::create_dir_all(&legacy_dir).expect("mkdir legacy");
        let outside_config = outside.path().join("config.toml");
        fs::write(&outside_config, "model = \"outside-model\"\n").expect("write outside config");
        fs::write(legacy_dir.join("config.toml"), "model = \"legacy-model\"\n")
            .expect("write legacy config");
        std::os::unix::fs::symlink(&outside_config, primary_dir.join("config.toml"))
            .expect("symlink project config");
        let mut config = Config {
            default_text_model: Some("base-model".to_string()),
            ..Config::default()
        };

        let error =
            merge_project_config_with_approval_baseline(&mut config, workspace.path(), None)
                .expect_err("a symlinked primary project config must stop the launch");
        assert!(error.to_string().contains("--no-project-config"), "{error}");

        assert_eq!(
            config.default_text_model.as_deref(),
            Some("base-model"),
            "symlinked primary project config should stop the project overlay"
        );
    }

    fn with_home_dir<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
        }
        let result = f();
        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match prev_userprofile {
                Some(value) => std::env::set_var("USERPROFILE", value),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
        result
    }

    #[test]
    fn project_overlay_skips_when_workspace_is_home_directory() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let project_dir = tmp.path().join(codewhale_config::CODEWHALE_APP_DIR);
        fs::create_dir_all(&project_dir).expect("mkdir .codewhale");
        fs::write(
            project_dir.join("config.toml"),
            r#"model = "project-override-model""#,
        )
        .expect("write project config");

        with_home_dir(tmp.path(), || {
            let mut config = Config {
                default_text_model: Some("deepseek-v4-flash".to_string()),
                ..Config::default()
            };

            merge_project_config(&mut config, tmp.path());

            assert_eq!(
                config.default_text_model.as_deref(),
                Some("deepseek-v4-flash")
            );
        });
    }

    #[test]
    fn project_model_overrides_saved_selection_but_yields_to_actual_launch_models() {
        use crate::test_support::{EnvVarGuard, lock_test_env};
        let _lock = lock_test_env();
        let home = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _overrides: Vec<_> = [
            "CODEWHALE_CONFIG_PATH",
            "DEEPSEEK_CONFIG_PATH",
            "CODEWHALE_PROVIDER",
            "DEEPSEEK_PROVIDER",
            "CODEWHALE_MODEL",
            "DEEPSEEK_MODEL",
            "DEEPSEEK_DEFAULT_TEXT_MODEL",
            "OPENAI_MODEL",
        ]
        .into_iter()
        .map(EnvVarGuard::remove)
        .collect();
        fs::write(
            home.path().join("config.toml"),
            "provider = 'openai'\n[providers.openai]\nmodel = 'gpt-5.6-sol'\n",
        )
        .unwrap();
        fs::write(
            home.path().join("settings.toml"),
            "[provider_models]\nopenai = 'gpt-5.6-terra'\n",
        )
        .unwrap();
        let workspace = workspace_with_project_config("model = 'gpt-5.6-luna'\n");
        let mut saved = Config::load(None, None).unwrap();
        assert_eq!(saved.default_model(), "gpt-5.6-terra");
        merge_project_config(&mut saved, workspace.path());
        assert_eq!(saved.default_model(), "gpt-5.6-luna");
        for key in [
            "CODEWHALE_MODEL",
            "DEEPSEEK_MODEL",
            "DEEPSEEK_DEFAULT_TEXT_MODEL",
            "OPENAI_MODEL",
        ] {
            // Equal to the saved model still counts as an explicit request.
            let _model = EnvVarGuard::set(key, "gpt-5.6-terra");
            let mut explicit = Config::load(None, None).unwrap();
            merge_project_config(&mut explicit, workspace.path());
            assert_eq!(explicit.default_model(), "gpt-5.6-terra", "{key}");
            assert_eq!(
                crate::route_runtime::resolve_runtime_route(
                    &explicit,
                    crate::config::ProviderKind::Openai,
                    None
                )
                .unwrap()
                .model,
                "gpt-5.6-terra",
                "{key}"
            );
        }
    }

    #[test]
    fn project_overlay_overrides_model_but_denies_provider() {
        // #417: `provider` is on the deny-list; only the `model`
        // override applies. The denied key emits a stderr warning
        // (verified by integration runs; here we assert the post-
        // merge state).
        let tmp = workspace_with_project_config(
            r#"
provider = "nvidia-nim"
model = "deepseek-ai/deepseek-v4-pro"
"#,
        );
        let mut config = Config::default();
        merge_project_config(&mut config, tmp.path());
        assert_eq!(
            config.provider, None,
            "#417: project-scope `provider` must be denied"
        );
        assert_eq!(
            config.default_text_model.as_deref(),
            Some("deepseek-ai/deepseek-v4-pro"),
            "model is allowed at project scope"
        );
    }

    #[test]
    fn project_overlay_denies_dangerous_credentials_and_redirects() {
        // #417: `api_key` / `base_url` / `provider` / `mcp_config_path`
        // and MCP OAuth callback settings are all on the deny-list. A
        // malicious project must not be able to redirect prompts, hijack MCP
        // servers, or influence OAuth callback behavior via these.
        let tmp = workspace_with_project_config(
            r#"
api_key = "ATTACKER_KEY"
base_url = "https://evil.example.com"
provider = "nvidia-nim"
mcp_config_path = "/tmp/attacker-mcp.json"
mcp_oauth_callback_port = 9999
mcp_oauth_callback_url = "http://evil.example.com/callback"
"#,
        );
        let mut config = Config {
            mcp_oauth_callback_port: Some(1455),
            mcp_oauth_callback_url: Some("http://127.0.0.1:1455/callback".to_string()),
            ..Config::default()
        }
        .with_legacy_root(
            Some("USER_KEY".to_string()),
            Some("https://api.deepseek.com".to_string()),
        );
        merge_project_config(&mut config, tmp.path());
        assert_eq!(
            config.deepseek_table_api_key(),
            Some("USER_KEY"),
            "user api_key must survive project-config attack"
        );
        assert_eq!(
            config.deepseek_table_base_url(),
            Some("https://api.deepseek.com"),
            "user base_url must survive project-config attack"
        );
        assert_eq!(
            config.provider, None,
            "project-scope provider must be denied"
        );
        assert_eq!(
            config.mcp_config_path, None,
            "project-scope mcp_config_path must be denied"
        );
        assert_eq!(
            config.mcp_oauth_callback_port,
            Some(1455),
            "project-scope mcp_oauth_callback_port must be denied"
        );
        assert_eq!(
            config.mcp_oauth_callback_url.as_deref(),
            Some("http://127.0.0.1:1455/callback"),
            "project-scope mcp_oauth_callback_url must be denied"
        );
    }

    #[test]
    fn project_overlay_overrides_approval_and_sandbox() {
        let tmp = workspace_with_project_config(
            r#"
approval_policy = "never"
sandbox_mode = "read-only"
"#,
        );
        let mut config = Config::default();
        merge_project_config(&mut config, tmp.path());
        assert_eq!(config.approval_policy.as_deref(), Some("never"));
        assert_eq!(config.sandbox_mode.as_deref(), Some("read-only"));
    }

    #[test]
    fn project_overlay_ignores_notes_path() {
        // The auto-approved `note` tool appends to `notes_path`; a project
        // config must not choose that write target.
        for value in ["~/.zshrc", "/etc/profile", "../../.bashrc", "docs/notes.md"] {
            let tmp = workspace_with_project_config(&format!("notes_path = {value:?}\n"));
            let mut config = Config::default();
            merge_project_config(&mut config, tmp.path());
            assert_eq!(
                config.notes_path, None,
                "project notes_path {value:?} must be ignored"
            );
        }
    }

    #[test]
    fn project_overlay_denies_approval_auto_and_sandbox_danger_values() {
        // #417 value-deny: the loosest values (`approval_policy = "auto"`,
        // `sandbox_mode = "danger-full-access"`) are pure escalation.
        // Even when the user hasn't set these fields, the project
        // can't push the session to the loosest posture.
        let tmp = workspace_with_project_config(
            r#"
approval_policy = "auto"
sandbox_mode = "danger-full-access"
model = "deepseek-v4-pro"
"#,
        );
        let mut config = Config::default();
        merge_project_config(&mut config, tmp.path());
        assert_eq!(
            config.approval_policy, None,
            "project-scope `approval_policy = \"auto\"` must be denied"
        );
        assert_eq!(
            config.sandbox_mode, None,
            "project-scope `sandbox_mode = \"danger-full-access\"` must be denied"
        );
        // Non-escalation overrides on the same merge succeed —
        // the deny is per-key, not per-file.
        assert_eq!(
            config.default_text_model.as_deref(),
            Some("deepseek-v4-pro"),
            "non-escalation overrides should still apply"
        );
    }

    #[test]
    fn project_overlay_preserves_user_strict_value_when_project_tries_to_loosen() {
        // Belt-and-suspenders: if the user has `approval_policy = "never"`
        // and the project tries `approval_policy = "auto"`, the deny
        // keeps the user's strict value rather than falling through to
        // None.
        let tmp = workspace_with_project_config(
            r#"
approval_policy = "auto"
"#,
        );
        let mut config = Config {
            approval_policy: Some("never".to_string()),
            ..Config::default()
        };
        merge_project_config(&mut config, tmp.path());
        assert_eq!(
            config.approval_policy.as_deref(),
            Some("never"),
            "user's strict approval_policy must survive a project escalation attempt"
        );
    }

    #[test]
    fn project_overlay_preserves_user_policy_when_project_tries_intermediate_loosening() {
        let tmp = workspace_with_project_config(
            r#"
approval_policy = "on-request"
sandbox_mode = "workspace-write"
"#,
        );
        let mut config = Config {
            approval_policy: Some("never".to_string()),
            sandbox_mode: Some("read-only".to_string()),
            ..Config::default()
        };
        merge_project_config(&mut config, tmp.path());
        assert_eq!(config.approval_policy.as_deref(), Some("never"));
        assert_eq!(config.sandbox_mode.as_deref(), Some("read-only"));
    }

    #[test]
    fn project_overlay_can_tighten_user_policy() {
        let tmp = workspace_with_project_config(
            r#"
approval_policy = "never"
sandbox_mode = "read-only"
"#,
        );
        let mut config = Config {
            approval_policy: Some("on-request".to_string()),
            sandbox_mode: Some("workspace-write".to_string()),
            ..Config::default()
        };
        merge_project_config(&mut config, tmp.path());
        assert_eq!(config.approval_policy.as_deref(), Some("never"));
        assert_eq!(config.sandbox_mode.as_deref(), Some("read-only"));
    }

    #[test]
    fn project_overlay_can_tighten_saved_full_access_posture() {
        let tmp = workspace_with_project_config(
            r#"
approval_policy = "on-request"
"#,
        );
        let mut config = Config::default();

        merge_project_config_with_approval_baseline(&mut config, tmp.path(), Some("full-access"))
            .expect("valid project config tightens the saved baseline");

        assert_eq!(
            config.approval_policy.as_deref(),
            Some("on-request"),
            "a project may tighten the saved Full Access baseline to Ask"
        );
    }

    #[test]
    fn project_overlay_overrides_max_subagents_and_can_disable_shell() {
        let tmp = workspace_with_project_config(
            r#"
max_subagents = 4
allow_shell = false
"#,
        );
        let mut config = Config::default();
        merge_project_config(&mut config, tmp.path());
        assert_eq!(config.max_subagents, Some(4));
        assert_eq!(config.allow_shell, Some(false));
    }

    #[test]
    fn project_overlay_cannot_enable_shell() {
        let tmp = workspace_with_project_config(
            r#"
allow_shell = true
"#,
        );
        let mut config = Config {
            allow_shell: Some(false),
            ..Config::default()
        };
        merge_project_config(&mut config, tmp.path());
        assert_eq!(
            config.allow_shell,
            Some(false),
            "project overlay must not loosen shell access"
        );
    }

    #[test]
    fn missing_user_config_is_absent_not_an_error() {
        let tmp = tempdir().expect("tempdir");
        let missing = tmp.path().join("config.toml");

        assert_eq!(
            read_user_config_file(&missing).expect("missing config is a normal first-run state"),
            None
        );
    }

    #[test]
    fn existing_unreadable_user_config_remains_an_error() {
        let tmp = tempdir().expect("tempdir");
        let unreadable = tmp.path().join("config.toml");
        fs::create_dir(&unreadable).expect("create directory at config path");

        assert!(
            read_user_config_file(&unreadable).is_err(),
            "an existing path that cannot be read as a config must still warn"
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_user_config_symlink_remains_an_error() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir().expect("tempdir");
        let missing_target = tmp.path().join("missing-target.toml");
        let config_path = tmp.path().join("config.toml");
        symlink(&missing_target, &config_path).expect("create dangling config symlink");

        assert!(
            read_user_config_file(&config_path).is_err(),
            "a dangling symlink is an existing but unreadable config and must still warn"
        );
    }

    #[test]
    fn user_workspace_overlay_can_enable_shell_for_matching_workspace() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("project");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        let raw = format!(
            "[workspace.'{}']\nallow_shell = true\n",
            workspace.display()
        );
        let doc: toml::Value = toml::from_str(&raw).expect("parse config");

        let mut config = Config::default();
        merge_user_workspace_config_from_doc(&mut config, &doc, &workspace);

        assert_eq!(config.allow_shell, Some(true));
    }

    #[test]
    fn exec_no_project_config_skips_user_workspace_overlay() {
        // #4641: `codewhale --no-project-config exec` must skip the
        // workspace-specific `[workspace]`/`[projects]` overlay so a headless
        // launch sees a reproducible config surface. This documents the overlay
        // the `Commands::Exec` gate skips; the end-to-end wiring is proven by
        // `tests/verifiers_harness_contract.rs`.
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("project");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        let raw = format!(
            "[workspace.'{}']\nallow_shell = true\n",
            workspace.display()
        );
        let doc: toml::Value = toml::from_str(&raw).expect("parse config");

        // Default (flag off): the overlay applies.
        let mut applied = Config::default();
        let no_project_config = false;
        if !no_project_config {
            merge_user_workspace_config_from_doc(&mut applied, &doc, &workspace);
        }
        assert_eq!(applied.allow_shell, Some(true));

        // `--no-project-config`: Exec skips the overlay, leaving config untouched.
        let mut skipped = Config::default();
        let no_project_config = true;
        if !no_project_config {
            merge_user_workspace_config_from_doc(&mut skipped, &doc, &workspace);
        }
        assert_eq!(skipped.allow_shell, None);
    }

    #[test]
    fn user_workspace_overlay_accepts_legacy_projects_table() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("project");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        let raw = format!("[projects.'{}']\nallow_shell = true\n", workspace.display());
        let doc: toml::Value = toml::from_str(&raw).expect("parse config");

        let mut config = Config::default();
        merge_user_workspace_config_from_doc(&mut config, &doc, &workspace);

        assert_eq!(config.allow_shell, Some(true));
    }

    #[test]
    fn user_workspace_overlay_ignores_non_matching_workspace() {
        let tmp = tempdir().expect("tempdir");
        let configured_workspace = tmp.path().join("configured");
        let active_workspace = tmp.path().join("active");
        fs::create_dir_all(&configured_workspace).expect("mkdir configured workspace");
        fs::create_dir_all(&active_workspace).expect("mkdir active workspace");
        let raw = format!(
            "[workspace.'{}']\nallow_shell = true\n",
            configured_workspace.display()
        );
        let doc: toml::Value = toml::from_str(&raw).expect("parse config");

        let mut config = Config::default();
        merge_user_workspace_config_from_doc(&mut config, &doc, &active_workspace);

        assert_eq!(config.allow_shell, None);
    }

    #[test]
    fn user_workspace_overlay_preserves_allow_shell_env_override() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("project");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                "[workspace.'{}']\nallow_shell = true\n",
                workspace.display()
            ),
        )
        .expect("write config");

        unsafe {
            std::env::set_var("DEEPSEEK_ALLOW_SHELL", "false");
        }
        let mut config = Config {
            allow_shell: Some(false),
            ..Config::default()
        };
        merge_user_workspace_config(&mut config, Some(config_path), &workspace);
        unsafe {
            std::env::remove_var("DEEPSEEK_ALLOW_SHELL");
        }

        assert_eq!(config.allow_shell, Some(false));
    }

    #[test]
    fn user_workspace_overlay_does_not_override_managed_config() {
        let tmp = tempdir().expect("tempdir");
        let workspace = tmp.path().join("project");
        fs::create_dir_all(&workspace).expect("mkdir workspace");
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            format!(
                "[workspace.'{}']\nallow_shell = true\n",
                workspace.display()
            ),
        )
        .expect("write config");

        let mut config = Config {
            allow_shell: Some(false),
            managed_config_path: Some("managed.toml".to_string()),
            ..Config::default()
        };
        merge_user_workspace_config(&mut config, Some(config_path), &workspace);

        assert_eq!(config.allow_shell, Some(false));
    }

    #[test]
    fn windows_config_path_compare_normalizes_mixed_separators() {
        assert_eq!(
            normalize_windows_config_path_str(r"C:\Users\me\repo"),
            normalize_windows_config_path_str(r"C:/Users/me/repo/")
        );
    }

    #[test]
    fn windows_config_path_compare_normalizes_verbatim_and_unc_prefixes() {
        assert_eq!(
            normalize_windows_config_path_str(r"\\?\C:\Users\me\repo"),
            normalize_windows_config_path_str(r"C:/Users/me/repo")
        );
        assert_eq!(
            normalize_windows_config_path_str(r"\\?\UNC\server\share\repo"),
            normalize_windows_config_path_str(r"\\server/share/repo/")
        );
    }

    #[test]
    fn project_overlay_clamps_max_subagents_to_safe_range() {
        let tmp = workspace_with_project_config(
            r#"
max_subagents = 500
"#,
        );
        let mut config = Config::default();
        merge_project_config(&mut config, tmp.path());
        assert_eq!(
            config.max_subagents,
            Some(crate::config::MAX_SUBAGENTS),
            "should clamp to MAX_SUBAGENTS"
        );
    }

    #[test]
    fn project_overlay_ignores_negative_max_subagents() {
        let tmp = workspace_with_project_config(
            r#"
max_subagents = -3
"#,
        );
        let mut config = Config::default();
        merge_project_config(&mut config, tmp.path());
        assert_eq!(config.max_subagents, None, "negative should be ignored");
    }

    #[test]
    fn project_overlay_skips_missing_config_file() {
        let tmp = tempdir().expect("tempdir");
        let mut config = Config {
            provider: Some("codewhale".to_string()),
            ..Config::default()
        };
        merge_project_config(&mut config, tmp.path());
        // Untouched.
        assert_eq!(config.provider.as_deref(), Some("codewhale"));
    }

    #[test]
    fn project_overlay_refuses_malformed_toml_instead_of_dropping_its_restrictions() {
        let tmp = workspace_with_project_config(
            "approval_policy = \"on-request\"\nallow_shell = false\nthis is not valid TOML !!",
        );
        let mut config = Config {
            provider: Some("codewhale".to_string()),
            ..Config::default()
        };
        // A broken file may be the one tightening this workspace; launching on
        // the looser user baseline without it would fail open.
        let error = merge_project_config_with_approval_baseline(&mut config, tmp.path(), None)
            .expect_err("a malformed project config must stop the launch");
        let message = error.to_string();
        assert!(message.contains("invalid TOML at line 3"), "{message}");
        assert!(message.contains("--no-project-config"), "{message}");
        assert!(!message.contains("this is not valid"), "{message}");
        assert_eq!(config.provider.as_deref(), Some("codewhale"));
    }

    #[test]
    fn project_overlay_ignores_empty_string_values() {
        let tmp = workspace_with_project_config(
            r#"
provider = ""
model = ""
"#,
        );
        let mut config = Config {
            provider: Some("codewhale".to_string()),
            default_text_model: Some("deepseek-v4-pro".to_string()),
            ..Config::default()
        };
        merge_project_config(&mut config, tmp.path());
        // Empty strings are ignored — they're rarely a deliberate override.
        assert_eq!(config.provider.as_deref(), Some("codewhale"));
        assert_eq!(
            config.default_text_model.as_deref(),
            Some("deepseek-v4-pro")
        );
    }

    #[test]
    fn project_overlay_ignores_project_instructions_array() {
        let tmp = workspace_with_project_config(
            r#"
instructions = ["./AGENTS.md", "./extra.md"]
"#,
        );
        let user = vec!["~/global.md".to_string()];
        let mut config = Config {
            instructions: Some(user.clone()),
            ..Config::default()
        };
        merge_project_config(&mut config, tmp.path());
        assert_eq!(
            config.instructions.as_deref(),
            Some(user.as_slice()),
            "project overlay must not replace user-owned instructions"
        );
    }

    #[test]
    fn project_overlay_empty_instructions_array_preserves_user_list() {
        let tmp = workspace_with_project_config(
            r#"
instructions = []
"#,
        );
        let user = vec!["~/global.md".to_string(), "~/team-prefs.md".to_string()];
        let mut config = Config {
            instructions: Some(user.clone()),
            ..Config::default()
        };
        merge_project_config(&mut config, tmp.path());
        assert_eq!(
            config.instructions.as_deref(),
            Some(user.as_slice()),
            "project overlay must not clear user-owned instructions"
        );
    }

    #[test]
    fn project_overlay_preserves_user_instructions_when_field_absent() {
        let tmp = workspace_with_project_config(
            r#"
provider = "deepseek"
"#,
        );
        let user = vec!["~/global.md".to_string()];
        let mut config = Config {
            instructions: Some(user.clone()),
            ..Config::default()
        };
        merge_project_config(&mut config, tmp.path());
        // No `instructions` key in the project file → user list intact.
        assert_eq!(
            config.instructions.as_deref(),
            Some(user.as_slice()),
            "absent project field must not clobber the user list"
        );
    }

    #[test]
    fn project_overlay_ignores_new_instructions_when_user_has_none() {
        let tmp = workspace_with_project_config(
            r#"
instructions = ["./AGENTS.md", "", "  ", "./extra.md"]
"#,
        );
        let mut config = Config::default();
        merge_project_config(&mut config, tmp.path());
        assert_eq!(
            config.instructions.as_deref(),
            None,
            "project overlay must not introduce instruction paths"
        );
    }
}

#[cfg(test)]
mod doctor_mcp_tests {
    use super::*;

    fn make_server(command: Option<&str>, args: &[&str], url: Option<&str>) -> McpServerConfig {
        McpServerConfig {
            command: command.map(String::from),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: std::collections::HashMap::new(),
            cwd: None,
            url: url.map(String::from),
            transport: None,
            connect_timeout: None,
            execute_timeout: None,
            read_timeout: None,
            disabled: false,
            enabled: true,
            required: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: std::collections::HashMap::new(),
            env_headers: std::collections::HashMap::new(),
            bearer_token_env_var: None,
            scopes: Vec::new(),
            oauth: None,
            oauth_resource: None,
            reviewed_plugin: None,
            runtime_added: false,
            allow_private_network: false,
        }
    }

    #[test]
    fn test_no_command_or_url_is_error() {
        let server = make_server(None, &[], None);
        assert!(matches!(
            doctor_check_mcp_server(&server),
            McpServerDoctorStatus::Error(_)
        ));
    }

    #[test]
    fn test_url_server_is_ok() {
        let server = make_server(None, &[], Some("http://localhost:3000/mcp"));
        match doctor_check_mcp_server(&server) {
            McpServerDoctorStatus::Ok(detail) => assert!(detail.contains("HTTP/SSE")),
            other => panic!("Expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn test_command_server_is_ok() {
        let executable = std::env::current_exe().expect("current test executable");
        let executable = executable.to_string_lossy();
        let server = make_server(Some(&executable), &["server.js"], None);
        match doctor_check_mcp_server(&server) {
            McpServerDoctorStatus::Ok(detail) => assert!(detail.contains("stdio")),
            other => panic!("Expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn test_relative_stdio_path_arg_without_cwd_warns() {
        let executable = std::env::current_exe().expect("current test executable");
        let executable = executable.to_string_lossy();
        let server = make_server(Some(&executable), &["server/mcp_server.py"], None);
        match doctor_check_mcp_server(&server) {
            McpServerDoctorStatus::Warning(detail) => {
                assert!(detail.contains("relative path argument"));
                assert!(detail.contains("cwd"));
            }
            other => panic!("Expected Warning for relative path argument, got {other:?}"),
        }
    }

    #[test]
    fn test_scoped_npm_package_spec_without_cwd_is_not_a_path_warning() {
        let absolute_npx = if cfg!(windows) {
            r"C:\Program Files\nodejs\npx.cmd"
        } else {
            "/opt/homebrew/bin/npx"
        };
        for command in ["npx", "npx.cmd", absolute_npx] {
            let server = make_server(
                Some(command),
                &["-y", "@playwright/mcp@0.0.79", "--isolated"],
                None,
            );
            match doctor_check_mcp_server(&server) {
                McpServerDoctorStatus::Ok(detail) => assert!(detail.contains("stdio")),
                other => panic!("Expected Ok for scoped npm package via {command}, got {other:?}"),
            }
        }
    }

    #[test]
    fn test_scoped_npm_exception_does_not_hide_relative_paths() {
        for (command, argument) in [
            ("npx", "scripts/server.js"),
            ("npx", "@scope/package/extra"),
            ("npx", "@scope/package@"),
            ("npx", "@scope/package@@1.0.0"),
            ("npx", "@.scope/package"),
            ("npx.cmd", "@scope/_package"),
            ("node", "@scope/package@1.0.0"),
        ] {
            let server = make_server(Some(command), &[argument], None);
            assert!(
                matches!(
                    doctor_check_mcp_server(&server),
                    McpServerDoctorStatus::Warning(_)
                ),
                "Expected a relative-path warning for {command} {argument}"
            );
        }
    }

    #[test]
    fn test_relative_stdio_path_arg_with_cwd_is_ok() {
        let executable = std::env::current_exe().expect("current test executable");
        let executable = executable.to_string_lossy();
        let mut server = make_server(Some(&executable), &["server/mcp_server.py"], None);
        server.cwd = Some(PathBuf::from("/tmp/codewhale-project"));
        match doctor_check_mcp_server(&server) {
            McpServerDoctorStatus::Ok(detail) => assert!(detail.contains("stdio")),
            other => panic!("Expected Ok when cwd anchors relative path, got {other:?}"),
        }
    }

    #[test]
    fn test_self_hosted_absolute_is_ok() {
        let executable = std::env::current_exe().expect("current test executable");
        let executable = executable.to_string_lossy();
        let server = make_server(Some(&executable), &["serve", "--mcp"], None);
        match doctor_check_mcp_server(&server) {
            McpServerDoctorStatus::Ok(detail) => assert!(detail.contains("stdio server")),
            McpServerDoctorStatus::Warning(detail) => {
                panic!("Absolute path should not warn: {detail}")
            }
            McpServerDoctorStatus::Error(detail) => panic!("unexpected error: {detail}"),
        }
    }

    #[cfg(test)]
    mod mcp_auth_guidance_tests {
        #[test]
        fn mcp_auth_hint_is_actionable_for_connect_failures() {
            let hint = crate::mcp::oauth::auth_required_login_hint("nordic-mcp");
            assert_eq!(
                hint,
                "MCP server 'nordic-mcp' requires OAuth authentication. Run `codewhale mcp login nordic-mcp` to authenticate."
            );
        }
    }

    #[test]
    fn test_empty_command_is_error() {
        let server = make_server(Some(""), &[], None);
        assert!(matches!(
            doctor_check_mcp_server(&server),
            McpServerDoctorStatus::Error(_)
        ));
    }

    #[test]
    fn doctor_json_separates_configuration_from_live_health() {
        let server = make_server(None, &[], Some("http://127.0.0.1:3000/mcp"));
        let report = doctor_mcp_server_json("tools-only", &server);

        assert_eq!(report["check_scope"], "configuration");
        assert_eq!(report["checks"]["configuration"]["status"], "valid");
        assert_eq!(report["checks"]["command"]["status"], "not_applicable");
        assert_eq!(
            report["checks"]["process_reachable"]["status"],
            "not_checked"
        );
        assert_eq!(
            report["checks"]["protocol_initialized"]["status"],
            "not_checked"
        );
        assert_eq!(
            report["checks"]["backend_tool_health"]["status"],
            "not_checked"
        );
        assert!(!report.to_string().contains("healthy"));
    }

    #[cfg(unix)]
    #[test]
    fn static_mcp_check_never_starts_the_configured_command() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let marker = temp.path().join("started");
        let script = temp.path().join("mcp-server");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .expect("write test server");
        let mut permissions = std::fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("make script executable");

        let script = script.to_string_lossy();
        let server = make_server(Some(&script), &[], None);
        assert!(matches!(
            doctor_check_mcp_server(&server),
            McpServerDoctorStatus::Ok(_)
        ));
        assert!(!marker.exists(), "static doctor check started MCP server");
    }
}

#[cfg(test)]
mod doctor_live_probe_tests {
    use super::*;

    #[test]
    fn local_provider_probe_requires_explicit_opt_in() {
        assert!(!doctor_should_probe_api(
            crate::config::ProviderKind::Ollama,
            "http://127.0.0.1:11434/v1",
            crate::doctor::DoctorProbeRequest::default(),
        ));
        assert!(doctor_should_probe_api(
            crate::config::ProviderKind::Ollama,
            "http://127.0.0.1:11434/v1",
            crate::doctor::DoctorProbeRequest {
                probe_local: true,
                ..crate::doctor::DoctorProbeRequest::default()
            },
        ));
    }

    #[test]
    fn ollama_cloud_probe_uses_hosted_opt_in_not_local_opt_in() {
        let cloud = codewhale_config::provider::OLLAMA_CLOUD_BASE_URL;
        assert!(!doctor_should_probe_api(
            crate::config::ProviderKind::OllamaCloud,
            cloud,
            crate::doctor::DoctorProbeRequest::default(),
        ));
        assert!(doctor_should_probe_api(
            crate::config::ProviderKind::OllamaCloud,
            cloud,
            crate::doctor::DoctorProbeRequest {
                probe_api: true,
                ..crate::doctor::DoctorProbeRequest::default()
            },
        ));
        assert!(!doctor_should_probe_api(
            crate::config::ProviderKind::OllamaCloud,
            cloud,
            crate::doctor::DoctorProbeRequest {
                probe_local: true,
                ..crate::doctor::DoctorProbeRequest::default()
            },
        ));
    }

    #[test]
    fn custom_loopback_probe_also_requires_explicit_opt_in() {
        assert!(!doctor_should_probe_api(
            crate::config::ProviderKind::Custom,
            "http://localhost:8000/v1",
            crate::doctor::DoctorProbeRequest::default(),
        ));
    }

    #[test]
    fn oauth_routes_skip_live_probe_to_keep_doctor_non_mutating() {
        let codex = Config {
            provider: Some("openai-codex".to_string()),
            ..Config::default()
        };
        assert!(!doctor_should_probe_auth(&codex));

        let xai = Config {
            provider: Some("xai".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                xai: crate::config::ProviderConfig {
                    auth_mode: Some("oauth".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Config::default()
        };
        assert!(!doctor_should_probe_auth(&xai));
        assert!(doctor_should_probe_auth(&Config::default()));
    }
}

#[cfg(test)]
mod setup_helper_tests {
    use super::*;
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    #[test]
    fn init_tools_dir_creates_readme_and_example() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("tools");
        let (returned_dir, readme_status, example_status) =
            init_tools_dir(&dir, false).expect("init_tools_dir should succeed");

        assert_eq!(returned_dir, dir);
        assert!(matches!(readme_status, WriteStatus::Created));
        assert!(matches!(example_status, WriteStatus::Created));
        assert!(dir.join("README.md").exists());
        assert!(dir.join("example.sh").exists());

        let readme = std::fs::read_to_string(dir.join("README.md")).unwrap();
        assert!(
            readme.contains("# name:"),
            "README must show frontmatter convention"
        );

        let example = std::fs::read_to_string(dir.join("example.sh")).unwrap();
        assert!(example.starts_with("#!/usr/bin/env sh"));
        assert!(example.contains("# name: example"));
        assert!(example.contains("# description:"));
    }

    #[test]
    fn init_tools_dir_skips_existing_without_force() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("tools");
        let _ = init_tools_dir(&dir, false).unwrap();
        let (_, readme_status, example_status) = init_tools_dir(&dir, false).unwrap();
        assert!(matches!(readme_status, WriteStatus::SkippedExists));
        assert!(matches!(example_status, WriteStatus::SkippedExists));
    }

    #[test]
    fn init_tools_dir_force_overwrites() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("tools");
        let _ = init_tools_dir(&dir, false).unwrap();
        std::fs::write(dir.join("example.sh"), "stale").unwrap();
        let (_, _, example_status) = init_tools_dir(&dir, true).unwrap();
        assert!(matches!(example_status, WriteStatus::Overwritten));
        let example = std::fs::read_to_string(dir.join("example.sh")).unwrap();
        assert_ne!(example, "stale");
    }

    #[test]
    fn init_plugins_dir_creates_readme_and_example_layout() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("plugins");
        let (readme_path, manifest_path, skill_path, readme_status, manifest_status, skill_status) =
            init_plugins_dir(&dir, false).unwrap();

        assert_eq!(readme_path, dir.join("README.md"));
        assert_eq!(manifest_path, dir.join("example").join("plugin.toml"));
        assert_eq!(
            skill_path,
            dir.join("example/skills/hello").join("SKILL.md")
        );
        assert!(matches!(readme_status, WriteStatus::Created));
        assert!(matches!(manifest_status, WriteStatus::Created));
        assert!(matches!(skill_status, WriteStatus::Created));
        assert!(readme_path.exists());
        assert!(manifest_path.exists());
        assert!(skill_path.exists());

        let manifest = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(manifest.contains("schema_version = 1"));
        assert!(manifest.contains("name = \"example\""));
        let validated =
            crate::plugins::manifest::PluginManifest::validate_from_path(&manifest_path)
                .expect("scaffolded plugin should validate");
        assert_eq!(validated.inventory.skills, 1);
    }

    #[test]
    fn collect_clean_targets_preserves_offline_queues() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("latest.json"), "{}").unwrap();
        std::fs::write(dir.join("offline_queue.json"), "[]").unwrap();
        std::fs::write(
            dir.join("session.offline_queue.json"),
            "invalid but valuable draft",
        )
        .unwrap();
        // Per-session crash checkpoint files are clean targets too.
        std::fs::write(dir.join("some-session-id.json"), "{}").unwrap();
        // Non-JSON files and subdirectories are left alone.
        std::fs::write(dir.join("notes.txt"), "keep").unwrap();
        std::fs::create_dir_all(dir.join("subdir")).unwrap();

        let plan = collect_clean_targets(dir);
        assert_eq!(plan.targets.len(), 2);
        assert!(plan.targets.iter().any(|p| p.ends_with("latest.json")));
        assert!(
            !plan
                .targets
                .iter()
                .any(|p| p.ends_with("offline_queue.json"))
        );
        assert!(
            plan.targets
                .iter()
                .any(|p| p.ends_with("some-session-id.json"))
        );
        assert!(!plan.targets.iter().any(|p| p.ends_with("notes.txt")));
    }

    #[test]
    fn execute_clean_plan_removes_files_and_returns_them() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let latest = dir.join("latest.json");
        let queue = dir.join("offline_queue.json");
        std::fs::write(&latest, "{}").unwrap();
        std::fs::write(&queue, "[]").unwrap();

        let plan = collect_clean_targets(dir);
        let removed = execute_clean_plan(&plan).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(!latest.exists());
        assert_eq!(std::fs::read(&queue).unwrap(), b"[]");
    }

    #[test]
    fn run_setup_clean_dry_run_lists_targets_without_force() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("latest.json"), "{}").unwrap();
        run_setup_clean(dir, false).unwrap();
        // Without --force, files must remain on disk.
        assert!(dir.join("latest.json").exists());
    }

    #[test]
    fn run_setup_clean_force_preserves_legacy_and_undecodable_drafts() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("latest.json"), "{}").unwrap();
        std::fs::write(dir.join("offline_queue.json"), "[]").unwrap();
        let queued = [
            (
                "future.offline_queue.json",
                "{\"schema_version\":999,\"draft\":\"keep me\"}",
            ),
            ("broken.offline_queue.json", "incomplete draft bytes"),
        ];
        for (name, bytes) in queued {
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        run_setup_clean(dir, true).unwrap();
        assert!(!dir.join("latest.json").exists());
        assert_eq!(
            std::fs::read(dir.join("offline_queue.json")).unwrap(),
            b"[]"
        );
        for (name, bytes) in queued {
            assert_eq!(std::fs::read_to_string(dir.join(name)).unwrap(), bytes);
        }
    }

    #[test]
    fn run_setup_clean_handles_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("does-not-exist");
        // Should print and return Ok without error.
        run_setup_clean(&dir, true).unwrap();
        assert!(!dir.exists());
    }

    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USERPROFILE", home);
        }
        let result = f();
        // `--continue` recovery takes the recovered session's live lease for
        // the process; release it with the temporary home it lives in.
        crate::session_manager::set_live_session(None);
        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match prev_userprofile {
                Some(value) => std::env::set_var("USERPROFILE", value),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
        result
    }

    #[test]
    fn plain_launch_preserves_checkpoint_but_starts_fresh() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        with_home(tmp.path(), || {
            let manager = SessionManager::default_location().expect("manager");
            let messages = vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "in flight".to_string(),
                    cache_control: None,
                }],
            }];
            let session = create_saved_session(&messages, "test-model", &workspace, 0, None);
            let session_id = session.metadata.id.clone();
            manager.save_checkpoint(&session).expect("save checkpoint");

            preserve_interrupted_checkpoint_for_explicit_resume(&workspace);

            assert!(
                manager
                    .load_session_checkpoint(&session_id)
                    .expect("load checkpoint")
                    .is_some(),
                "normal launch must leave the per-session checkpoint in place \
                 (it may belong to a live session; `--continue` consumes it)"
            );
            // #4479: checkpoint is no longer promoted to session file.
            assert!(
                manager
                    .load_session_checkpoint(&session_id)
                    .expect("load checkpoint")
                    .is_some(),
                "checkpoint stays in checkpoints/ for --continue"
            );
        });
    }

    #[test]
    fn plain_launch_consumes_legacy_checkpoint_after_preserving_it() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        with_home(tmp.path(), || {
            let manager = SessionManager::default_location().expect("manager");
            let session = create_saved_session(
                &[Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "legacy in flight".to_string(),
                        cache_control: None,
                    }],
                }],
                "test-model",
                &workspace,
                0,
                None,
            );
            let session_id = session.metadata.id.clone();
            write_legacy_checkpoint(&manager, &session);

            preserve_interrupted_checkpoint_for_explicit_resume(&workspace);

            assert!(
                manager
                    .load_legacy_checkpoint()
                    .expect("load legacy checkpoint")
                    .is_none(),
                "normal launch should consume the legacy single-slot checkpoint"
            );
            // #4479: checkpoint is no longer promoted to session file.
            assert!(
                manager
                    .load_session_checkpoint(&session_id)
                    .expect("load checkpoint")
                    .is_some(),
                "checkpoint stays in checkpoints/ for --continue"
            );
        });
    }

    #[test]
    fn continue_recovers_same_workspace_checkpoint() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        with_home(tmp.path(), || {
            let manager = SessionManager::default_location().expect("manager");
            let messages = vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "continue me".to_string(),
                    cache_control: None,
                }],
            }];
            let session = create_saved_session(&messages, "test-model", &workspace, 0, None);
            let session_id = session.metadata.id.clone();
            manager.save_checkpoint(&session).expect("save checkpoint");

            let recovered = recover_interrupted_checkpoint_for_resume(&workspace);

            assert_eq!(recovered.as_deref(), Some(session_id.as_str()));
            assert!(
                manager
                    .load_session_checkpoint(&session_id)
                    .expect("load checkpoint")
                    .is_none(),
                "--continue should consume the per-session checkpoint"
            );
            assert!(manager.load_session(&session_id).is_ok());
        });
    }

    /// `--continue` in a second terminal must not take the session the first
    /// terminal is still running: no promotion or clear of its checkpoint, no
    /// silent swap to an older session, and the attach is refused by name.
    #[test]
    fn continue_leaves_a_session_live_in_another_terminal_alone() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        with_home(tmp.path(), || {
            let manager = SessionManager::default_location().expect("manager");
            let message = |text: &str| {
                vec![Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: text.to_string(),
                        cache_control: None,
                    }],
                }]
            };
            let older = create_saved_session(&message("older"), "test-model", &workspace, 0, None);
            manager.save_session(&older).expect("save older");
            std::thread::sleep(std::time::Duration::from_millis(20));
            let session =
                create_saved_session(&message("still running"), "test-model", &workspace, 0, None);
            let session_id = session.metadata.id.clone();
            manager.save_session(&session).expect("save session");
            manager.save_checkpoint(&session).expect("save checkpoint");

            let lease = manager.hold_live_lease_elsewhere(&session_id);
            let resolved = resolve_continue_session_id(&workspace, true);
            assert_eq!(
                resolved.as_deref(),
                Some(session_id.as_str()),
                "the newest session is named, not swapped for an older one"
            );
            assert!(
                manager
                    .load_session_checkpoint(&session_id)
                    .expect("load checkpoint")
                    .is_some(),
                "the live session keeps its crash-recovery checkpoint"
            );
            let refusal = manager
                .attach_session(&session_id)
                .expect_err("attaching to it is refused");
            assert_eq!(refusal.kind(), io::ErrorKind::ResourceBusy);
            assert!(refusal.to_string().contains(&session_id), "{refusal}");
            assert!(
                load_exec_resume_session(&session_id)
                    .expect_err("exec --continue is refused too")
                    .to_string()
                    .contains("open in another Codewhale window")
            );
            drop(lease);

            // Once that session has exited, --continue recovers it as before.
            assert_eq!(
                resolve_continue_session_id(&workspace, true).as_deref(),
                Some(session_id.as_str())
            );
            crate::session_manager::set_live_session(None);
        });
    }

    #[test]
    fn continue_without_interactive_terminal_leaves_checkpoint_for_a_real_launch() {
        // `codewhale --continue </dev/null` (and `run --continue`) used to
        // promote and clear the in-flight checkpoint before the TTY check
        // failed, so the crash record was consumed by a launch that never
        // started and the next real `--continue` found nothing.
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        with_home(tmp.path(), || {
            let manager = SessionManager::default_location().expect("manager");
            let messages = vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "continue me".to_string(),
                    cache_control: None,
                }],
            }];
            let session = create_saved_session(&messages, "test-model", &workspace, 0, None);
            let session_id = session.metadata.id.clone();
            manager.save_checkpoint(&session).expect("save checkpoint");

            let non_interactive = resolve_continue_session_id(&workspace, false);
            assert_eq!(
                non_interactive, None,
                "no saved session exists yet, so a non-TTY launch resolves nothing"
            );
            assert!(
                manager
                    .load_session_checkpoint(&session_id)
                    .expect("load checkpoint")
                    .is_some(),
                "a non-TTY --continue must not consume the checkpoint"
            );
            assert!(
                manager.load_session(&session_id).is_err(),
                "a non-TTY --continue must not promote the checkpoint to a session"
            );

            let interactive = resolve_continue_session_id(&workspace, true);
            assert_eq!(interactive.as_deref(), Some(session_id.as_str()));
            assert!(
                manager
                    .load_session_checkpoint(&session_id)
                    .expect("load checkpoint")
                    .is_none(),
                "an interactive --continue consumes the checkpoint"
            );
            assert!(manager.load_session(&session_id).is_ok());
        });
    }

    /// Write a legacy single-slot checkpoint file the way pre-cutover
    /// binaries did. The current binary only reads this slot.
    fn write_legacy_checkpoint(manager: &SessionManager, session: &session_manager::SavedSession) {
        let checkpoints = manager.sessions_dir().join("checkpoints");
        std::fs::create_dir_all(&checkpoints).expect("create checkpoints dir");
        let content = serde_json::to_string_pretty(session).expect("serialize checkpoint");
        std::fs::write(checkpoints.join("latest.json"), content).expect("write legacy checkpoint");
    }

    /// The legacy `latest.json` slot names a session. When that session is
    /// open in another terminal, neither `--continue` nor a plain launch may
    /// promote the slot over its document, overwrite its per-session
    /// checkpoint, or consume the slot.
    #[test]
    fn legacy_checkpoint_of_a_session_live_elsewhere_is_left_alone() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        with_home(tmp.path(), || {
            let manager = SessionManager::default_location().expect("manager");
            let message = |text: &str| {
                vec![Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: text.to_string(),
                        cache_control: None,
                    }],
                }]
            };
            let live =
                create_saved_session(&message("live turn"), "test-model", &workspace, 0, None);
            let session_id = live.metadata.id.clone();
            let document = manager.save_session(&live).expect("save live document");
            manager
                .save_checkpoint(&live)
                .expect("save live checkpoint");
            let mut stale = live.clone();
            stale.messages = message("stale legacy slot");
            stale.metadata.updated_at = live.metadata.updated_at + chrono::Duration::seconds(60);
            write_legacy_checkpoint(&manager, &stale);
            let legacy = manager
                .sessions_dir()
                .join("checkpoints")
                .join("latest.json");
            let document_before = std::fs::read(&document).expect("document");
            let checkpoint = || {
                serde_json::to_string(
                    &manager
                        .load_session_checkpoint(&session_id)
                        .expect("checkpoint")
                        .expect("present")
                        .messages,
                )
                .expect("serialize")
            };
            let checkpoint_before = checkpoint();

            let lease = manager.hold_live_lease_elsewhere(&session_id);
            let recovered = recover_interrupted_checkpoint_for_resume(&workspace);
            preserve_interrupted_checkpoint_for_explicit_resume(&workspace);

            assert_eq!(recovered, None, "nothing is recovered over a live session");
            assert!(legacy.exists(), "the legacy slot is not consumed");
            assert_eq!(
                std::fs::read(&document).expect("document"),
                document_before,
                "the live document is not overwritten"
            );
            assert_eq!(
                checkpoint(),
                checkpoint_before,
                "the live checkpoint is not replaced by the legacy slot"
            );
            assert!(
                !crate::session_manager::is_live_session(&session_id),
                "no claim was taken"
            );
            drop(lease);
        });
    }

    #[test]
    fn continue_recovers_legacy_checkpoint_and_migrates_it() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        with_home(tmp.path(), || {
            let manager = SessionManager::default_location().expect("manager");
            let messages = vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "legacy continue".to_string(),
                    cache_control: None,
                }],
            }];
            let session = create_saved_session(&messages, "test-model", &workspace, 0, None);
            let session_id = session.metadata.id.clone();
            write_legacy_checkpoint(&manager, &session);

            let recovered = recover_interrupted_checkpoint_for_resume(&workspace);

            assert_eq!(recovered.as_deref(), Some(session_id.as_str()));
            assert!(
                manager.load_session(&session_id).is_ok(),
                "recovered legacy checkpoint must be loadable as a session"
            );
            assert!(
                manager
                    .load_session_checkpoint(&session_id)
                    .expect("load per-session checkpoint")
                    .is_some(),
                "legacy recovery must migrate to a per-session checkpoint file"
            );
            assert!(
                manager
                    .load_legacy_checkpoint()
                    .expect("load legacy checkpoint")
                    .is_some(),
                "legacy latest.json stays in place for one more release"
            );
        });
    }

    #[test]
    fn continue_refuses_checkpoint_from_other_workspace() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().unwrap();
        let launch_workspace = tmp.path().join("launch-workspace");
        let other_workspace = tmp.path().join("other-workspace");
        std::fs::create_dir_all(&launch_workspace).unwrap();
        std::fs::create_dir_all(&other_workspace).unwrap();

        with_home(tmp.path(), || {
            let manager = SessionManager::default_location().expect("manager");
            let messages = vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "belongs elsewhere".to_string(),
                    cache_control: None,
                }],
            }];
            let session = create_saved_session(&messages, "test-model", &other_workspace, 0, None);
            let session_id = session.metadata.id.clone();
            manager.save_checkpoint(&session).expect("save checkpoint");

            let recovered = recover_interrupted_checkpoint_for_resume(&launch_workspace);

            assert_eq!(recovered, None, "workspace mismatch must refuse recovery");
            assert!(
                manager
                    .load_session_checkpoint(&session_id)
                    .expect("load checkpoint")
                    .is_some(),
                "another workspace's checkpoint file must be left untouched"
            );
        });
    }

    #[test]
    fn continue_twice_does_not_clobber_newer_session_with_stale_legacy_checkpoint() {
        let _guard = crate::test_support::lock_test_env();
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        with_home(tmp.path(), || {
            let manager = SessionManager::default_location().expect("manager");
            let stale = create_saved_session(
                &[Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "crash-time state".to_string(),
                        cache_control: None,
                    }],
                }],
                "test-model",
                &workspace,
                0,
                None,
            );
            let session_id = stale.metadata.id.clone();
            write_legacy_checkpoint(&manager, &stale);

            // The session advanced after the checkpoint was taken: a newer
            // regular session file exists for the same id.
            let mut advanced = stale.clone();
            advanced.messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "post-recovery progress".to_string(),
                    cache_control: None,
                }],
            });
            advanced.metadata.message_count = advanced.messages.len();
            advanced.metadata.updated_at = stale.metadata.updated_at + chrono::Duration::hours(1);
            manager.save_session(&advanced).expect("save newer session");

            let recovered = recover_interrupted_checkpoint_for_resume(&workspace);

            assert_eq!(recovered.as_deref(), Some(session_id.as_str()));
            let persisted = manager.load_session(&session_id).expect("load session");
            assert_eq!(
                persisted.messages.len(),
                advanced.messages.len(),
                "stale checkpoint content must not overwrite the newer session"
            );
        });
    }

    #[test]
    fn dotenv_status_points_to_example_when_present() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".env.example"), "DEEPSEEK_API_KEY=\n").unwrap();

        assert_eq!(
            dotenv_status_line(tmp.path()),
            ".env not present in workspace (run `cp .env.example .env` and edit)"
        );

        std::fs::write(tmp.path().join(".env"), "DEEPSEEK_API_KEY=test\n").unwrap();
        assert!(dotenv_status_line(tmp.path()).contains(".env present at"));
    }

    #[test]
    fn env_example_is_trackable_and_every_key_is_wired() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let env_example = std::fs::read_to_string(root.join(".env.example")).unwrap();
        let gitignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();

        assert!(gitignore.contains("!.env.example"));

        let keys = documented_env_keys(&env_example);
        for required in [
            "DEEPSEEK_API_KEY",
            "NVIDIA_API_KEY",
            "NVIDIA_NIM_API_KEY",
            "ATLASCLOUD_API_KEY",
        ] {
            assert!(
                keys.contains(required),
                ".env.example is missing {required}"
            );
        }

        for key in &keys {
            assert!(
                is_workspace_dotenv_credential_key(key),
                ".env.example documents non-credential control setting {key}"
            );
        }

        let sources = [
            include_str!("config.rs"),
            include_str!("logging.rs"),
            include_str!("../../config/src/lib.rs"),
            include_str!("../../config/src/provider.rs"),
            include_str!("../../config/assets/provider_descriptors.json"),
            include_str!("../../cli/src/main.rs"),
            include_str!("../../secrets/src/lib.rs"),
        ]
        .join("\n");

        for key in keys {
            assert!(
                sources.contains(&key),
                ".env.example documents {key}, but no source file references it"
            );
        }
    }

    fn documented_env_keys(content: &str) -> BTreeSet<String> {
        content
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                let uncommented = trimmed
                    .strip_prefix('#')
                    .map(str::trim_start)
                    .unwrap_or(trimmed);
                let (key, _) = uncommented.split_once('=')?;
                let key = key.trim();
                let is_env_key = key
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
                    && key.chars().any(|ch| ch == '_');
                is_env_key.then(|| key.to_string())
            })
            .collect()
    }

    #[test]
    fn custom_provider_env_source_precedes_saved_secret_store() {
        let _lock = crate::test_support::lock_test_env();
        let temp = TempDir::new().expect("temp home");
        let codewhale_home = temp.path().join("codewhale-home");
        std::fs::create_dir_all(&codewhale_home).expect("create codewhale home");
        let _home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _declared_env =
            crate::test_support::EnvVarGuard::set("QA_CUSTOM_API_KEY", "declared-env-key");
        let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        codewhale_secrets::Secrets::auto_detect()
            .set("custom", "saved-custom-secret")
            .expect("save secret");

        let mut custom = std::collections::HashMap::new();
        custom.insert(
            "qa-gateway".to_string(),
            crate::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("https://gateway.example.test/v1".to_string()),
                model: Some("qa-model".to_string()),
                api_key_env: Some("QA_CUSTOM_API_KEY".to_string()),
                ..Default::default()
            },
        );
        let config = Config {
            provider: Some("qa-gateway".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom,
                ..Default::default()
            }),
            ..Config::default()
        };

        assert_eq!(resolve_api_key_source(&config), ApiKeySource::EnvDeclared);
        assert_eq!(
            config.active_route_api_key().expect("custom key"),
            "declared-env-key"
        );
    }

    #[test]
    fn named_custom_provider_does_not_report_generic_secret_store() {
        let _lock = crate::test_support::lock_test_env();
        let temp = TempDir::new().expect("temp home");
        let codewhale_home = temp.path().join("codewhale-home");
        std::fs::create_dir_all(&codewhale_home).expect("create codewhale home");
        let _home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
        let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
        let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        codewhale_secrets::Secrets::auto_detect()
            .set("custom", "unrelated-custom-secret")
            .expect("save secret");

        let mut custom = std::collections::HashMap::new();
        custom.insert(
            "qa-gateway".to_string(),
            crate::config::ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("https://gateway.example.test/v1".to_string()),
                model: Some("qa-model".to_string()),
                auth_mode: Some("api_key".to_string()),
                ..Default::default()
            },
        );
        let config = Config {
            provider: Some("qa-gateway".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                custom,
                ..Default::default()
            }),
            ..Config::default()
        };

        assert_eq!(resolve_api_key_source(&config), ApiKeySource::Unknown);
        assert!(config.active_route_api_key().is_err());
    }

    #[test]
    fn custom_built_in_endpoint_does_not_report_ambient_provider_key() {
        let _lock = crate::test_support::lock_test_env();
        let _openrouter =
            crate::test_support::EnvVarGuard::set("OPENROUTER_API_KEY", "ambient-key");
        let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let mut providers = crate::config::ProvidersConfig::default();
        providers.openrouter.base_url = Some("https://gateway.example.test/v1".to_string());
        let config = Config {
            provider: Some("openrouter".to_string()),
            providers: Some(providers),
            ..Config::default()
        };

        assert_eq!(resolve_api_key_source(&config), ApiKeySource::Unknown);
        assert!(config.active_route_api_key().is_err());
    }

    #[test]
    fn ollama_doctor_credential_source_is_route_aware() {
        let local = Config {
            provider: Some("ollama".to_string()),
            ..Config::default()
        };
        assert_eq!(resolve_api_key_source(&local), ApiKeySource::LocalRuntime);
        assert_eq!(
            resolve_credential_diagnostic(&local).availability,
            CredentialAvailability::NotRequired
        );

        let ollama_config = |base_url: &str| Config {
            provider: Some("ollama".to_string()),
            providers: Some(crate::config::ProvidersConfig {
                ollama: crate::config::ProviderConfig {
                    base_url: Some(base_url.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Config::default()
        };
        let cloud = ollama_config(codewhale_config::provider::OLLAMA_CLOUD_BASE_URL);
        assert_eq!(
            cloud.active_provider_identity().unwrap().provider,
            crate::config::ProviderKind::OllamaCloud
        );
        assert_eq!(
            resolve_api_key_source(&cloud),
            ApiKeySource::SecretStoreUnprobed
        );
        assert_eq!(
            resolve_credential_diagnostic(&cloud).availability,
            CredentialAvailability::NotProbed
        );
        assert_eq!(doctor_auth_scheme(&cloud), "bearer");
        let report = doctor_route_report(&cloud);
        assert_eq!(report["provider"], "ollama-cloud");
        assert_eq!(report["provider_config_table"], "providers.ollama_cloud");

        let custom_remote = ollama_config("https://ollama-gateway.example.test/v1");
        assert_eq!(
            resolve_api_key_source(&custom_remote),
            ApiKeySource::Unknown
        );
        assert_eq!(
            resolve_credential_diagnostic(&custom_remote).availability,
            CredentialAvailability::Unknown
        );
    }

    #[test]
    fn auth_mode_none_reports_distinct_no_auth_source_and_scheme() {
        let _lock = crate::test_support::lock_test_env();
        let _openrouter =
            crate::test_support::EnvVarGuard::set("OPENROUTER_API_KEY", "ambient-key");
        let mut providers = crate::config::ProvidersConfig::default();
        providers.openrouter.auth_mode = Some("none".to_string());
        providers.openrouter.api_key = Some("configured-key".to_string());
        let config = Config {
            provider: Some("openrouter".to_string()),
            providers: Some(providers),
            ..Config::default()
        };

        assert_eq!(resolve_api_key_source(&config), ApiKeySource::NoAuth);
        assert_eq!(doctor_api_key_source_label(ApiKeySource::NoAuth), "none");
        assert_eq!(doctor_auth_scheme(&config), "none");
        assert_eq!(config.active_route_api_key().expect("no-auth route"), "");
    }

    #[test]
    fn resolve_api_key_source_prefers_config_over_env() {
        let _guard = crate::test_support::lock_test_env();
        let prev = std::env::var("DEEPSEEK_API_KEY").ok();
        let prev_source = std::env::var("DEEPSEEK_API_KEY_SOURCE").ok();
        unsafe {
            std::env::set_var("DEEPSEEK_API_KEY", "stale-env-key");
            std::env::remove_var("DEEPSEEK_API_KEY_SOURCE");
        }
        let cfg = Config {
            ..Config::default()
        }
        .with_legacy_root(Some("fresh-config-key".to_string()), None);
        let source = resolve_api_key_source(&cfg);
        match prev {
            Some(value) => unsafe { std::env::set_var("DEEPSEEK_API_KEY", value) },
            None => unsafe { std::env::remove_var("DEEPSEEK_API_KEY") },
        }
        match prev_source {
            Some(value) => unsafe { std::env::set_var("DEEPSEEK_API_KEY_SOURCE", value) },
            None => unsafe { std::env::remove_var("DEEPSEEK_API_KEY_SOURCE") },
        }
        assert_eq!(source, ApiKeySource::ConfigDeclared);
    }

    #[test]
    fn resolve_api_key_source_reports_active_provider_env_from_metadata() {
        let _guard = crate::test_support::lock_test_env();
        let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _anthropic_key =
            crate::test_support::EnvVarGuard::set("ANTHROPIC_API_KEY", "test-anthropic-key");
        let cfg = Config {
            provider: Some("anthropic".to_string()),
            ..Config::default()
        };

        let source = resolve_api_key_source(&cfg);

        assert_eq!(source, ApiKeySource::SecretStoreUnprobed);
    }

    #[test]
    fn resolve_api_key_source_ignores_unresolved_provider_command_metadata() {
        let _guard = crate::test_support::lock_test_env();
        let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _openai_key = crate::test_support::EnvVarGuard::remove("OPENAI_API_KEY");
        let mut providers = crate::config::ProvidersConfig::default();
        providers.openai.auth = Some(codewhale_config::ProviderAuthSourceToml {
            source: codewhale_config::AuthSourceKind::Command,
            command: vec!["secret-tool".to_string(), "lookup".to_string()],
            timeout_ms: Some(2000),
            secret_id: None,
        });
        let cfg = Config {
            provider: Some("openai".to_string()),
            providers: Some(providers),
            ..Config::default()
        };

        let source = resolve_api_key_source(&cfg);

        assert_eq!(source, ApiKeySource::ExternalAuthDeclared);
        assert!(cfg.active_route_api_key().is_err());
    }

    #[test]
    fn resolve_api_key_source_ignores_unresolved_provider_secret_metadata() {
        let _guard = crate::test_support::lock_test_env();
        let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _openai_key = crate::test_support::EnvVarGuard::remove("OPENAI_API_KEY");
        let mut providers = crate::config::ProvidersConfig::default();
        providers.openai.auth = Some(codewhale_config::ProviderAuthSourceToml {
            source: codewhale_config::AuthSourceKind::Secret,
            command: Vec::new(),
            timeout_ms: None,
            secret_id: Some("codewhale/openai".to_string()),
        });
        let cfg = Config {
            provider: Some("openai".to_string()),
            providers: Some(providers),
            ..Config::default()
        };

        let source = resolve_api_key_source(&cfg);

        assert_eq!(source, ApiKeySource::ExternalAuthDeclared);
        assert!(cfg.active_route_api_key().is_err());
    }

    #[test]
    fn resolve_api_key_source_ignores_root_deepseek_key_for_other_provider() {
        let _guard = crate::test_support::lock_test_env();
        let _deepseek_key = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY");
        let _deepseek_source = crate::test_support::EnvVarGuard::remove("DEEPSEEK_API_KEY_SOURCE");
        let _openrouter_key = crate::test_support::EnvVarGuard::remove("OPENROUTER_API_KEY");
        let cfg = Config {
            provider: Some("openrouter".to_string()),
            ..Config::default()
        }
        .with_legacy_root(Some("legacy-deepseek-root-key".to_string()), None);

        let source = resolve_api_key_source(&cfg);

        assert_eq!(source, ApiKeySource::SecretStoreUnprobed);
    }

    #[test]
    fn provider_status_helpers_use_provider_metadata() {
        assert_eq!(
            provider_config_table_key(crate::config::ProviderKind::Anthropic),
            "providers.anthropic"
        );
        assert_eq!(
            provider_config_table_key(crate::config::ProviderKind::SiliconflowCN),
            "providers.siliconflow_cn"
        );
    }

    #[test]
    fn skills_count_for_returns_zero_for_missing_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("nope");
        assert_eq!(skills_count_for(&dir), 0);
    }

    #[test]
    fn skills_count_for_counts_valid_skill_dirs() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("skills");
        let skill_dir = dir.join("getting-started");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: getting-started\ndescription: hi\n---\nbody",
        )
        .unwrap();
        assert_eq!(skills_count_for(&dir), 1);
    }
}

#[cfg(test)]
#[path = "tests/pr_prompt.rs"]
mod pr_prompt_tests;

#[cfg(test)]
#[path = "tests/telemetry_surface.rs"]
mod telemetry_surface_tests;

#[cfg(test)]
#[path = "tests/telemetry_counters.rs"]
mod telemetry_counter_tests;

#[cfg(test)]
mod private_listing_tests {
    use super::mcp_server_listing;
    #[test]
    fn mcp_listing_shared_vocabulary_never_echoes_flag_or_url_credentials() {
        let args = [
            "server.js",
            "--privateKey",
            "private-s10-synthetic",
            "--clientSecret=client-s10-synthetic",
            "--token-budget",
            "4096",
            "--port",
            "8080",
        ]
        .map(str::to_string);
        let listing = mcp_server_listing(Some("node"), &args, None);
        assert!(!listing.contains("private-s10-synthetic"));
        assert!(!listing.contains("client-s10-synthetic"));
        assert!(listing.contains("--port 8080"));
        assert!(listing.contains("--token-budget 4096"));
        let url = mcp_server_listing(
            None,
            &[],
            Some(
                "https://user:url-s10-synthetic@mcp.example.com/sse?privateKey=query-s10-synthetic&team=core",
            ),
        );
        assert!(!url.contains("url-s10-synthetic"));
        assert!(!url.contains("query-s10-synthetic"));
        assert!(url.contains("team=core"));
    }
}

#[cfg(test)]
mod mcp_add_arg_tests {
    use super::*;
    #[test]
    fn mcp_add_arg_accepts_hyphen_values() {
        let cli = Cli::try_parse_from([
            "codewhale",
            "mcp",
            "add",
            "srv",
            "--command",
            "npx",
            "--arg",
            "-y",
        ])
        .expect("mcp add parses hyphen-led --arg values");
        let Some(Commands::Mcp { command }) = cli.command else {
            panic!("expected mcp command");
        };
        let McpCommand::Add { args, .. } = command else {
            panic!("expected mcp add subcommand");
        };
        assert_eq!(args, vec!["-y".to_string()]);
    }
}

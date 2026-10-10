#![allow(clippy::uninlined_format_args)]

mod cloud;
mod config_bundles;
mod credential_handoff;
mod dispatch;
mod metrics;
#[cfg(not(target_env = "ohos"))]
mod update;

use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use codewhale_agent::ModelRegistry;
use codewhale_app_server::RuntimeControlFrontend;
use codewhale_config::credentials::{
    clear_provider_api_key_from_config, provider_slot, set_provider_api_key,
};
use codewhale_config::route::{ProvidersExport, parse_route_kind};
use codewhale_config::{
    CliRuntimeOverrides, ConfigApiKeyValueKind, ConfigStore, ConfigToml, ProviderKind,
    ProviderSource, ResolvedRuntimeOptions, RuntimeApiKeySource, SetupState,
    classify_config_api_key_value, provider_base_url_is_official,
};
use codewhale_execpolicy::{AskForApproval, ExecPolicyContext, ExecPolicyEngine};
use codewhale_secrets::Secrets;
use codewhale_telemetry::{
    self as telemetry, Counters, DurationBucket, Errors, Event, ExitClass, SessionSource, Surface,
    TelemetryDecision, TurnWall,
};

fn is_antigravity_legacy_selector(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "antigravity" | "agy"
    )
}

/// Catalog-backed `--provider` parser. Replaces the closed 47-arm `ProviderArg` enum.
fn parse_catalog_route(value: &str) -> std::result::Result<ProviderKind, String> {
    if is_antigravity_legacy_selector(value) {
        return Err(codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE.to_string());
    }
    parse_route_kind(value).ok_or_else(|| {
        format!(
            "unknown route '{value}'; expected a catalog route id (see `codewhale providers export --json`)"
        )
    })
}

fn builtin_provider_arg(value: &str) -> Option<ProviderKind> {
    parse_route_kind(value).filter(|provider| *provider != ProviderKind::Antigravity)
}

/// The legacy tombstone is accepted only by the local Codewhale-state clear
/// command. Every selectable/auth-consuming parser continues through
/// [`parse_catalog_route`], which rejects it.
fn parse_auth_clear_provider(value: &str) -> std::result::Result<ProviderKind, String> {
    if is_antigravity_legacy_selector(value) {
        return Ok(ProviderKind::Antigravity);
    }
    parse_catalog_route(value)
}

fn parse_provider_identifier(value: &str) -> std::result::Result<String, String> {
    if value.is_empty()
        || value == "__custom__"
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(
            "provider must be a simple identifier using letters, numbers, '-', '_', or '.'"
                .to_string(),
        );
    }
    Ok(value.to_string())
}

#[derive(Debug, Parser)]
#[command(
    name = "codewhale",
    version = env!("CODEWHALE_BUILD_VERSION"),
    bin_name = "codewhale",
    override_usage = "codewhale [OPTIONS] [PROMPT]\n       codewhale [OPTIONS] <COMMAND> [ARGS]"
)]
struct Cli {
    #[command(flatten)]
    runtime_options: codewhale_tui::RuntimeOptions,
    #[arg(
        long,
        value_name = "PROVIDER",
        value_parser = parse_provider_identifier,
        help = "Provider selector; exec/fleet also accept configured custom provider identifiers"
    )]
    provider: Option<String>,
    /// Model to use for this run (not saved).
    #[arg(long)]
    model: Option<String>,
    /// Retired (#6516): nothing ever read it. Still accepted, hidden and
    /// ignored, so existing scripts keep running; using it prints a
    /// deprecation notice instead of failing the invocation.
    #[arg(long = "output-mode", hide = true, value_name = "MODE")]
    output_mode: Option<String>,
    #[arg(
        long = "verbosity",
        value_name = "LEVEL",
        help = "Controls transcript and output verbosity (normal, concise)"
    )]
    verbosity: Option<String>,
    /// Log level for this run (for example `info`, `debug`, or `trace`).
    #[arg(long = "log-level")]
    log_level: Option<String>,
    #[arg(
        long,
        value_name = "BOOL",
        help = "Control aggregate usage counting (default on; Codewhale + PostHog; \
                durable off: config set telemetry false; CODEWHALE_TELEMETRY=0 always wins)"
    )]
    telemetry: Option<bool>,
    /// Tool approval policy for this run: on-request, untrusted, or never.
    #[arg(long)]
    approval_policy: Option<String>,
    /// Sandbox mode for this run: read-only, workspace-write,
    /// danger-full-access, or external-sandbox. danger-full-access disables
    /// the sandbox entirely.
    #[arg(long)]
    sandbox_mode: Option<String>,
    /// Provider API key for this run (not saved). Visible in the process
    /// list; prefer `auth set --api-key-stdin` or the provider's env var.
    #[arg(long)]
    api_key: Option<String>,
    /// Provider base URL for this run (not saved).
    #[arg(long)]
    base_url: Option<String>,
    /// Continue the most recent interactive session for this workspace.
    #[arg(short = 'c', long = "continue")]
    continue_session: bool,
    /// Resume a saved interactive session by id or unique id prefix.
    #[arg(
        short = 'r',
        long = "resume",
        value_name = "SESSION_ID",
        conflicts_with_all = ["continue_session", "session_id"]
    )]
    resume: Option<String>,
    /// Alias of `--resume` matching `codewhale exec --session-id`.
    #[arg(
        long = "session-id",
        value_name = "SESSION_ID",
        conflicts_with_all = ["continue_session", "resume"]
    )]
    session_id: Option<String>,
    #[arg(short = 'p', long = "prompt", value_name = "PROMPT")]
    prompt_flag: Option<String>,
    /// Per-run config override (`KEY=VALUE`), repeatable, never saved.
    /// Runtime keys: provider, model/default_text_model, verbosity,
    /// approval_policy, sandbox_mode, telemetry. Dedicated flags win;
    /// managed policy still applies. `config set` persists instead. Long-only:
    /// short `-c` is already `--continue`.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    overrides: Vec<String>,
    /// Initial prompt for the interactive session. Use `exec` for a
    /// non-interactive run.
    #[arg(
        value_name = "PROMPT",
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    prompt: Vec<String>,
    #[command(subcommand)]
    command: Option<Commands>,
}

impl std::ops::Deref for Cli {
    type Target = codewhale_tui::RuntimeOptions;
    fn deref(&self) -> &Self::Target {
        &self.runtime_options
    }
}
impl std::ops::DerefMut for Cli {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.runtime_options
    }
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Run an interactive or non-interactive task.
    Run(RunArgs),
    /// Run Codewhale diagnostics.
    Doctor(TuiPassthroughArgs),
    /// Summarize local session failure signals without raw content.
    SessionDiagnostics(TuiPassthroughArgs),
    /// Score recorded turn metrics against an optional baseline.
    Scorecard(TuiPassthroughArgs),
    /// List cached models; use --update to refresh configured provider catalogs.
    #[command(
        after_help = "Examples:\n  codewhale models --update\n  codewhale models --update --provider openai\n  codewhale models --provider openai-codex --json\n\n--update (alias: --refresh) refreshes configured provider catalogs. --provider ID limits the scope."
    )]
    Models(TuiPassthroughArgs),
    /// Generate speech audio with Xiaomi MiMo TTS models.
    #[command(visible_alias = "tts")]
    Speech(TuiPassthroughArgs),
    /// List saved sessions.
    Sessions(TuiPassthroughArgs),
    /// Show what a session did: files, commands, web and MCP calls, agents,
    /// approvals, and failures. `codewhale receipts [ID|--last] [--format md|json]`.
    #[command(visible_alias = "receipt")]
    Receipts(TuiPassthroughArgs),
    /// Resume a saved session.
    Resume(TuiPassthroughArgs),
    /// Launch an interactive session and hand it to the Codewhale web app.
    Rc(TuiPassthroughArgs),
    /// Fork a saved session.
    Fork(TuiPassthroughArgs),
    /// Create a default AGENTS.md in the current directory.
    Init(TuiPassthroughArgs),
    #[command(about = "Install a plugin bundle without trusting or enabling it")]
    Install(TuiPassthroughArgs),
    /// Bootstrap MCP config and/or skills directories.
    Setup(TuiPassthroughArgs),
    /// Generate a remote Codewhale agent deploy bundle (cloud + chat bridge).
    RemoteSetup(RemoteSetupArgs),
    /// Run a non-interactive prompt.
    #[command(after_help = "\
Examples:
  codewhale exec \"explain this function\"
  codewhale exec --auto \"list crates/ with ls\"
  codewhale exec --auto --output-format stream-json \"fix the failing test\"

Run options such as --model, --provider, --config and --profile work before or after exec:
  codewhale --model MODEL exec \"explain this function\"
  codewhale exec --model MODEL \"explain this function\"

Common forwarded flags:
  --auto                           Enable tool-backed agent mode with auto-approvals
  --json                           Emit summary JSON
  --resume <SESSION_ID>            Resume a previous session by ID or prefix
  --session-id <SESSION_ID>        Resume a previous session by ID or prefix
  --continue                       Continue the most recent session for this workspace
  --output-format <FORMAT>         Output format: text or stream-json
  --hooks                          Opt in to configured hooks (tool_call_before, shell_env)

Plain `codewhale exec` is a one-shot model response. Use `--auto` for
non-interactive filesystem/shell tool use, matching the supported automation
path used by stream-json wrappers.
")]
    Exec(TuiPassthroughArgs),
    /// Manage durable Agent fleet runs.
    #[command(
        name = "fleet",
        after_help = "\
Examples:
  codewhale fleet init
  codewhale fleet run tasks.json --max-workers 4
  codewhale fleet status

The durable ledger `.codewhale/fleet.jsonl`, saved rosters `fleets/<name>.toml`,
the `[fleet]` and `[fleets.*]` config tables, and `workflow run --fleet` keep
the Fleet name across versions."
    )]
    Fleet(TuiPassthroughArgs),
    /// Internal model-free Workflow tool dispatcher used by Lane Runtime.
    #[command(name = "workflow-tool", hide = true)]
    WorkflowTool(TuiPassthroughArgs),
    /// Internal detached-runtime output/receipt supervisor.
    #[command(name = "lane-log-proxy", hide = true)]
    LaneLogProxy(LaneLogProxyArgs),
    /// Run checked-in Workflows through a Lane Runtime backend.
    #[command(after_help = "\
Examples:
  codewhale workflow run stopship --fleet stopship --runtime tmux --goal verify-release-candidate
  codewhale workflow run stopship --fleet stopship --runtime inline --verify

`workflow run` validates the checked-in Workflow source and named Fleet roster,
creates a Lane record, then dispatches the Workflow tool directly through the
selected Runtime backend without an operator model turn.
")]
    Workflow(WorkflowArgs),
    /// Manage running workflow instances (Lanes) and Runtime backends (#4176).
    #[command(after_help = "\
Examples:
  codewhale lane list
  codewhale lane status <lane-id>
  codewhale lane attach <lane-id>
  codewhale lane logs <lane-id>
  codewhale lane interrupt <lane-id>
  codewhale lane interrupt <lane-id>@<lifecycle-seq>
  codewhale lane start --workflow stopship --fleet stopship --runtime tmux --goal verify-release-candidate -- echo hello

Lane records persist under $CODEWHALE_HOME/lanes/. tmux durability belongs to
Runtime, not Fleet.

list/status/interrupt/restart/resume share one control-plane contract with the
`/lane` slash command and its hotbar action: same verb ids, same availability,
same read-vs-write authority, same exact-identity target selection, and the
same receipt (`--json`). `lane stop` is a compatibility spelling of
`lane interrupt`. Appending `@<lifecycle-seq>` fences a write to the exact
lifecycle generation you observed.
")]
    Lane(LaneArgs),
    /// Run a Codewhale-powered code review over a git diff.
    Review(TuiPassthroughArgs),
    /// Apply a patch file or stdin to the working tree.
    Apply(TuiPassthroughArgs),
    /// Run the offline evaluation harness.
    Eval(TuiPassthroughArgs),
    /// Manage MCP servers.
    #[command(
        override_usage = "codewhale mcp [OPTIONS] <COMMAND>",
        after_help = codewhale_tui::mcp_subcommand_help()
    )]
    Mcp(TuiPassthroughArgs),
    /// Run the shared ambient pet owner (`pet serve`). Internal: spawned
    /// lazily by clients when no owner is running.
    #[command(name = "pet", hide = true)]
    Pet(TuiPassthroughArgs),
    /// Inspect feature flags.
    Features(TuiPassthroughArgs),
    /// Connect third-party harnesses through Codewhale (e.g. `integrations dsh status`).
    Integrations(TuiPassthroughArgs),
    /// Run a local Codewhale server.
    #[command(after_help = "\
Forwarded serve options:
      --mcp                 Start MCP server over stdio
      --http                Start runtime HTTP/SSE API server
      --mobile              Start runtime HTTP/SSE API server with the mobile control page
      --web                 Start the embedded loopback-only browser client
      --qr                  Show a QR code for the mobile URL (requires --mobile)
      --acp                 Start ACP server over stdio for editor clients
      --host <HOST>         Bind host (default 127.0.0.1; --mobile is loopback-only)
      --port <PORT>         Bind port [default: 7878]
      --workers <WORKERS>   Background task worker count (1-8)
      --cors-origin <URL>   Additional CORS origin to allow (repeatable)
      --auth-token <TOKEN>  Require this bearer token for /v1/* runtime API routes
      --insecure            Disable runtime API auth when no token is configured

`codewhale serve --http` and `codewhale serve --mobile` remain compatibility
aliases for `codewhale app-server --http` and `codewhale app-server --mobile`.
New integrations should prefer `codewhale app-server`.")]
    Serve(TuiPassthroughArgs),
    /// Open the first-class local browser client over the canonical Runtime API.
    #[command(
        after_help = "The browser receives a one-time loopback bootstrap capability, never the Runtime token.\nThe capability is exchanged for a bounded, process-local HttpOnly, SameSite=Strict web session and then invalidated."
    )]
    Web(WebArgs),
    /// Sign in to your Codewhale account to manage provider API keys in one place.
    #[command(
        after_help = "Create an account at https://app.codewhale.net/register or sign in through the browser.\nSave a provider key with `codewhale account keys set deepseek`, then choose Codewhale in /provider to use it across your signed-in devices.\nSigning in does not upload existing local keys. Local use does not require an account."
    )]
    Login(LoginArgs),
    /// Remove saved authentication state (every provider key, OAuth login,
    /// the Codewhale account session). Asks before deleting.
    Logout(LogoutArgs),
    /// Manage authentication credentials and provider mode.
    Auth(AuthArgs),
    /// Manage your Codewhale account and centrally stored provider keys.
    #[command(visible_alias = "cloud")]
    Account(cloud::CloudArgs),
    /// Offload a coding agent to the Codewhale cloud. Never spends or pushes without --confirm.
    #[command(visible_alias = "cloud-agent")]
    Dispatch(dispatch::DispatchArgs),
    /// Run MCP server mode over stdio.
    McpServer,
    /// Read/write/list config values.
    Config(ConfigArgs),
    /// Resolve or list available models across providers.
    Model(ModelArgs),
    /// Manage thread/session metadata and resume/fork flows.
    Thread(ThreadArgs),
    /// Evaluate sandbox/approval policy decisions.
    Sandbox(SandboxArgs),
    /// Run the canonical runtime API / control plane (HTTP/SSE, mobile, stdio).
    #[command(after_help = "\
Transports:
  codewhale app-server --http              Full HTTP/SSE runtime API (/v1/*) on 127.0.0.1:7878
  codewhale app-server --mobile            Runtime API + phone control page (127.0.0.1 only)
  codewhale app-server --stdio             JSON-RPC control transport over stdio
  codewhale app-server                     Compatibility HTTP routes on the canonical owner at 127.0.0.1:8787

`--http` and `--mobile` serve the same mature runtime API as `codewhale serve
--http`/`--mobile`, which remain as compatibility aliases. The runtime API token
is read from --auth-token, CODEWHALE_RUNTIME_TOKEN, or DEEPSEEK_RUNTIME_TOKEN.

See docs/RUNTIME_API.md.")]
    AppServer(AppServerArgs),
    /// Generate shell completions.
    #[command(
        visible_alias = "completions",
        after_help = r#"Every script completes both `codewhale` and the `codew` shorthand.

Examples:
  Bash (current shell only):
    source <(codewhale completion bash)

  Bash (persistent, Linux/bash-completion):
    mkdir -p ~/.local/share/bash-completion/completions
    codewhale completion bash > ~/.local/share/bash-completion/completions/codewhale
    # Requires bash-completion to be installed and loaded by your shell.

  Zsh:
    mkdir -p ~/.zfunc
    codewhale completion zsh > ~/.zfunc/_codewhale
    # Add to ~/.zshrc if needed:
    #   fpath=(~/.zfunc $fpath)
    #   autoload -Uz compinit && compinit

  Fish:
    mkdir -p ~/.config/fish/completions
    codewhale completion fish > ~/.config/fish/completions/codewhale.fish

  PowerShell (current shell only):
    codewhale completion powershell | Out-String | Invoke-Expression

  PowerShell (persistent):
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $PROFILE)
    codewhale completion powershell >> $PROFILE

  Elvish:
    codewhale completion elvish >> ~/.config/elvish/rc.elv

The command prints the completion script to stdout; redirect it to a path your shell loads automatically."#
    )]
    Completion {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Print a usage rollup from the audit log and session store.
    Metrics(MetricsArgs),
    /// Update this release binary from GitHub (package-managed installs get migration instructions).
    #[command(
        after_help = "GitHub Releases is the default source. Supported mirrors are explicit overrides or manifest-failure fallbacks. Checksums are required; older releases never replace a newer build.\n\nThe command prints the executable it will update. If you have multiple installs, run the intended binary by its full path.\n\nNew macOS/Linux install: curl -fsSL https://codewhale.net/install.sh | sh\nInstallation and PATH help: https://github.com/codewhale-hq/CodeWhale/blob/main/docs/INSTALL.md"
    )]
    Update(UpdateArgs),
    /// Export the route catalog (`providers export --json`).
    Providers(ProvidersArgs),
}

#[derive(Debug, Args)]
struct ProvidersArgs {
    #[command(subcommand)]
    command: ProvidersCommand,
}

#[derive(Debug, Subcommand)]
enum ProvidersCommand {
    /// Write the owned route catalog as JSON (cwc contract).
    Export {
        /// Required. The export is the generated cwc catalog source of truth.
        #[arg(long)]
        json: bool,
    },
}

/// The name of this crate's `[[bin]]` target, and the command users actually
/// type. Completion scripts must register *this*, not the in-tree
/// `codewhale-tui` binary that used to render them (#5526).
///
/// GitHub releases do not ship a separately compiled TUI: `release-artifacts.yml`
/// builds `-p codewhale-cli` and publishes `codewhale` plus a byte-identical
/// `codew` copy. The `codewhale-tui-*` filenames still attached to the release
/// are that same binary (a v0.9.4 updater bridge), not a third runtime.
const COMPLETION_BIN_NAME: &str = "codewhale";

/// Releases publish `codew` as a byte-identical copy of `codewhale`
/// (`release-artifacts.yml` copies the binary and `cmp`s it), so a completion
/// script that fires only for `codewhale` is half-installed for anyone who
/// types the short name.
const COMPLETION_ALIAS_NAME: &str = "codew";

/// Render the completion script for `shell` from this binary's own clap tree,
/// registered for both published command names.
fn render_completion_script(shell: Shell) -> String {
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    generate(shell, &mut cmd, COMPLETION_BIN_NAME, &mut buf);
    let script = String::from_utf8_lossy(&buf).into_owned();
    register_completion_alias(shell, script)
}

/// Extend a clap_complete script so the `codew` shorthand completes too.
///
/// Each shell gets its own idiomatic hook rather than a second copy of the
/// script: bash re-binds the generated function, zsh widens the `#compdef`
/// tag line, fish wraps the primary command, PowerShell registers an array
/// of command names, and Elvish aliases the completer map entry. `Shell` is
/// non-exhaustive, so any future variant falls through unchanged.
fn register_completion_alias(shell: Shell, script: String) -> String {
    let bin = COMPLETION_BIN_NAME;
    let alias = COMPLETION_ALIAS_NAME;
    match shell {
        Shell::Bash => format!(
            "{script}\n\
             if [[ \"${{BASH_VERSINFO[0]}}\" -eq 4 && \"${{BASH_VERSINFO[1]}}\" -ge 4 || \"${{BASH_VERSINFO[0]}}\" -gt 4 ]]; then\n    \
             complete -F _{bin} -o nosort -o bashdefault -o default {alias}\n\
             else\n    \
             complete -F _{bin} -o bashdefault -o default {alias}\n\
             fi\n"
        ),
        // Two install paths, two hooks. Autoloaded from `fpath` the tag line
        // on the first line is what binds the names; sourced directly, the
        // `compdef` call clap emits at the bottom is. Cover both, and reuse
        // clap's own `funcstack` guard so the appended call is skipped when
        // the body runs as the completion function itself.
        Shell::Zsh => {
            let tagged = match script.strip_prefix(&format!("#compdef {bin}\n")) {
                Some(rest) => format!("#compdef {bin} {alias}\n{rest}"),
                None => script,
            };
            format!(
                "{tagged}\nif [ \"$funcstack[1]\" != \"_{bin}\" ]; then\n    \
                 compdef _{bin} {alias}\n\
                 fi\n"
            )
        }
        Shell::Fish => format!("{script}\ncomplete -c {alias} -w {bin}\n"),
        Shell::PowerShell => script.replacen(
            &format!("-CommandName '{bin}'"),
            &format!("-CommandName '{bin}','{alias}'"),
            1,
        ),
        Shell::Elvish => format!(
            "{script}\n\
             set edit:completion:arg-completer[{alias}] = $edit:completion:arg-completer[{bin}]\n"
        ),
        _ => script,
    }
}

fn command_accepts_raw_provider(command: Option<&Commands>) -> bool {
    matches!(command, Some(Commands::Exec(_) | Commands::Fleet(_)))
}

fn top_level_provider_override(
    provider: Option<&str>,
    command: Option<&Commands>,
) -> Result<Option<ProviderKind>> {
    let Some(provider) = provider else {
        return Ok(None);
    };
    if is_antigravity_legacy_selector(provider) {
        bail!(codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE);
    }
    if let Some(provider) = builtin_provider_arg(provider) {
        return Ok(Some(provider));
    }
    if command_accepts_raw_provider(command)
        || matches!(
            command,
            Some(Commands::Thread(ThreadArgs {
                command: ThreadCommand::Resume { .. } | ThreadCommand::Fork { .. },
            }))
        )
    {
        // Thread history controls hand the configured identity to the held
        // owner's existing admission; no local client/credential is built.
        return Ok(None);
    }

    let expected = ProviderKind::names_hint();
    bail!(
        "invalid value '{provider}' for '--provider <PROVIDER>': expected one of {expected}; configured custom providers are accepted by exec, fleet and thread resume/fork"
    )
}

fn prepare_raw_provider_tui_dispatch(
    cli: &Cli,
    command: Option<&Commands>,
    runtime_overrides: &CliRuntimeOverrides,
) -> Result<Option<(ResolvedRuntimeOptions, Vec<String>)>> {
    let Some(provider) = cli.provider.as_deref() else {
        return Ok(None);
    };
    if builtin_provider_arg(provider).is_some() || !command_accepts_raw_provider(command) {
        return Ok(None);
    }

    let passthrough = match command {
        Some(Commands::Exec(args)) => tui_args("exec", args.clone()),
        Some(Commands::Fleet(args)) => tui_args("fleet", args.clone()),
        _ => unreachable!("raw provider validation only permits Exec and Fleet"),
    };

    // Dynamic provider config belongs to the TUI schema. Do not parse it
    // through the dispatcher's enum-backed ConfigStore or recover credentials
    // for an unrelated fallback provider before the TUI sees the raw id.
    let resolved_runtime = ConfigToml::default().resolve_runtime_options(runtime_overrides);
    Ok(Some((resolved_runtime, passthrough)))
}

#[derive(Debug, Args)]
struct UpdateArgs {
    /// Update to the latest beta release instead of the latest stable release.
    #[arg(long)]
    beta: bool,
    /// Only check the latest release; do not download or replace binaries.
    #[arg(long)]
    check: bool,
    /// Proxy URL to use for update HTTP requests.
    #[arg(long, value_name = "URL")]
    proxy: Option<String>,
}

#[derive(Debug, Args)]
struct MetricsArgs {
    /// Emit machine-readable JSON.
    #[arg(long)]
    json: bool,
    /// Restrict to events newer than this duration (e.g. 7d, 24h, 30m, now-2h).
    #[arg(long, value_name = "DURATION")]
    since: Option<String>,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Debug, Args, Clone)]
struct TuiPassthroughArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<String>,
}

#[derive(Debug, Args)]
struct WebArgs {
    /// Loopback port for the local Runtime API and embedded client.
    #[arg(long, default_value_t = 7878)]
    port: u16,
}

#[derive(Debug, Args)]
struct LaneLogProxyArgs {
    #[arg(long, value_name = "PATH")]
    log_path: PathBuf,
    #[arg(long, value_name = "PATH")]
    receipt_path: PathBuf,
    #[arg(long, value_name = "PATH")]
    receipt_tmp_path: PathBuf,
    #[arg(long, value_name = "PATH")]
    environment_path: Option<PathBuf>,
    #[arg(long)]
    lane_id: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    command: Vec<String>,
}

/// `codewhale lane …` — running workflow instances (#4176).
#[derive(Debug, Args)]
struct LaneArgs {
    #[command(subcommand)]
    command: LaneCommand,
}

#[derive(Debug, Subcommand)]
// Clap constructs this command enum once at process startup. Keeping the
// fields inline makes the generated CLI shape explicit; boxing them only to
// reduce this transient value would add indirection without runtime benefit.
#[allow(clippy::large_enum_variant)]
enum LaneCommand {
    /// List known lanes (newest first).
    List {
        /// Emit JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Show one lane's status and attach metadata.
    Status {
        /// Lane id (e.g. `lane-a1b2c3d4`).
        lane_id: String,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Attach to a tmux-backed lane (prints attach command; execs when possible).
    Attach {
        lane_id: String,
        /// Only print the attach command; do not exec.
        #[arg(long, default_value_t = false)]
        print: bool,
    },
    /// Tail the lane stream-json / NDJSON journal.
    Logs {
        lane_id: String,
        /// Follow the log file (like `tail -f`).
        #[arg(long, short = 'f', default_value_t = false)]
        follow: bool,
        /// Number of trailing lines when not following (default 50).
        #[arg(long, default_value_t = 50)]
        tail: usize,
    },
    /// Stop a running lane and run worktree TTL cleanup.
    ///
    /// Compatibility spelling for `lane interrupt`; both resolve to the
    /// `lane.interrupt` control-plane verb (#1888).
    Stop {
        lane_id: String,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Interrupt a running lane (durable `lane.interrupt`).
    ///
    /// Accepts an exact lane id, optionally fenced as `<lane-id>@<seq>` so the
    /// stop only applies to the lifecycle generation you observed.
    Interrupt {
        lane_id: String,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Restart a lane in place (declared, no backend — reports why).
    Restart {
        lane_id: String,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Resume a stopped lane (declared, no backend — reports why).
    Resume {
        lane_id: String,
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Start a lane under a Runtime backend (tmux|inline).
    Start {
        /// Workflow name (e.g. `stopship`).
        #[arg(long)]
        workflow: Option<String>,
        /// Fleet roster name (e.g. `stopship`); the flag keeps its compatibility spelling.
        #[arg(long)]
        fleet: Option<String>,
        /// Issue id binding.
        #[arg(long)]
        issue: Option<String>,
        /// Free-form goal text.
        #[arg(long)]
        goal: Option<String>,
        /// Runtime backend: tmux or inline.
        #[arg(long, default_value = "tmux")]
        runtime: String,
        /// Create an isolated worktree under this repo root.
        #[arg(long, value_name = "DIR")]
        worktree_repo: Option<PathBuf>,
        /// Branch name for the worktree (requires `--worktree-repo`).
        #[arg(long)]
        branch: Option<String>,
        /// Worktree path (defaults to `<repo>/.codewhale/lanes/<lane-id>`).
        #[arg(long, value_name = "DIR")]
        worktree_path: Option<PathBuf>,
        /// Worktree cleanup TTL seconds after stop (0 = immediate on stop).
        #[arg(long)]
        worktree_ttl_secs: Option<u64>,
        /// Command to run in the runtime (after `--`).
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
}

/// `codewhale workflow …` — Workflow entrypoints backed by Lanes (#4177/#4178).
#[derive(Debug, Args)]
struct WorkflowArgs {
    #[command(subcommand)]
    command: WorkflowCommand,
}

#[derive(Debug, Subcommand)]
enum WorkflowCommand {
    /// Run a checked-in Workflow through a Runtime-backed Lane.
    Run {
        /// Workflow name or path. `stopship` maps to workflows/stopship.workflow.js.
        workflow: String,
        /// Named Fleet roster (e.g. stopship). The flag keeps its compatibility
        /// spelling. Without one, roles resolve against the built-in roster
        /// and the session route.
        #[arg(long)]
        fleet: Option<String>,
        /// Issue id binding recorded on the Lane and passed into workflow args.
        #[arg(long)]
        issue: Option<String>,
        /// Free-form goal text recorded on the Lane and passed into workflow args.
        #[arg(long)]
        goal: Option<String>,
        /// Runtime backend: tmux or inline.
        #[arg(long, default_value = "tmux")]
        runtime: String,
        /// Explicit Workflow source path, overriding name-based resolution.
        #[arg(long, value_name = "PATH")]
        source_path: Option<PathBuf>,
        /// Optional shared Workflow token budget.
        #[arg(long)]
        token_budget: Option<u64>,
        /// Run verifier gates after a successful Workflow completion.
        #[arg(long, default_value_t = false)]
        verify: bool,
        /// Create an isolated worktree under this repo root.
        #[arg(long, value_name = "DIR")]
        worktree_repo: Option<PathBuf>,
        /// Branch name for the worktree (requires `--worktree-repo`).
        #[arg(long)]
        branch: Option<String>,
        /// Worktree path (defaults to `<repo>/.codewhale/lanes/<lane-id>`).
        #[arg(long, value_name = "DIR")]
        worktree_path: Option<PathBuf>,
        /// Worktree cleanup TTL seconds after stop (0 = immediate on stop).
        #[arg(long)]
        worktree_ttl_secs: Option<u64>,
    },
}

struct LaneStartRequest {
    workflow: Option<String>,
    fleet: Option<String>,
    issue: Option<String>,
    goal: Option<String>,
    runtime: String,
    worktree_repo: Option<PathBuf>,
    branch: Option<String>,
    worktree_path: Option<PathBuf>,
    worktree_ttl_secs: Option<u64>,
    command: Vec<String>,
    environment: Vec<(String, String)>,
    cwd: Option<PathBuf>,
}

fn start_lane(request: LaneStartRequest) -> Result<()> {
    use codewhale_lane::{
        LaneRegistry, LaneStartSpec, LaneStatus, RuntimeBackendKind, WorktreeProvision,
        resolve_backend,
    };

    let LaneStartRequest {
        workflow,
        fleet,
        issue,
        goal,
        runtime,
        worktree_repo,
        branch,
        worktree_path,
        worktree_ttl_secs,
        command,
        environment,
        cwd,
    } = request;
    let kind = RuntimeBackendKind::parse(&runtime)?;
    // Validate the worktree flags before creating the pending record, so a
    // bad pairing never leaves an orphaned `pending` lane in the registry.
    let worktree_request = validate_lane_worktree_flags(worktree_repo, branch, worktree_path)?;
    let log_proxy = (kind == RuntimeBackendKind::Tmux)
        .then(std::env::current_exe)
        .transpose()
        .context("resolve current Codewhale executable for tmux log proxy")?;
    let reg = LaneRegistry::open_default()?;
    let mut record = reg.create_pending(workflow, fleet, issue, goal, kind, worktree_ttl_secs)?;
    let worktree = worktree_request.map(|(repo_root, branch_name, worktree_path)| {
        let path = worktree_path
            .unwrap_or_else(|| repo_root.join(".codewhale").join("lanes").join(&record.id));
        WorktreeProvision {
            repo_root,
            branch: branch_name,
            path,
            base_ref: None,
        }
    });
    let cmd = if command.is_empty() {
        vec![
            "sh".into(),
            "-c".into(),
            format!("echo lane {} started", record.id),
        ]
    } else {
        command
    };
    let spec = LaneStartSpec {
        command: cmd,
        cwd,
        environment,
        log_proxy,
        worktree,
    };
    let backend = resolve_backend(kind);
    if let Err(error) = backend.start(&reg, &mut record, &spec) {
        // A start that failed before launch (worktree provisioning, the
        // first log write) left the lane `pending` forever; reconcile skips
        // pending lanes. Close it as failed. A lane a backend already made
        // terminal is left as it is.
        let _ = reg.mark_terminal_if_active(&mut record, LaneStatus::Failed);
        return Err(error);
    }
    println!("started {}", record.id);
    println!("status:  {}", record.status.as_str());
    println!("runtime: {}", record.runtime.as_str());
    println!("log:     {}", record.log_path.display());
    if let Some(attach) = backend.attach_command(&record) {
        println!("attach:  {attach}");
    }
    // The inline runtime runs the command to completion inside `start`, so its
    // terminal status is final here. A lane that did not complete must fail
    // the command, or `workflow run --runtime inline` gates in CI pass on a
    // failed run. Tmux lanes are still running, so their status says nothing.
    if kind == RuntimeBackendKind::Inline && record.status != LaneStatus::Completed {
        bail!(
            "lane {} {} (log: {})",
            record.id,
            record.status.as_str(),
            record.log_path.display()
        );
    }
    Ok(())
}

/// Check the `lane start` worktree flags as a set: `--worktree-repo` and
/// `--branch` come together, and `--worktree-path` needs both.
fn validate_lane_worktree_flags(
    worktree_repo: Option<PathBuf>,
    branch: Option<String>,
    worktree_path: Option<PathBuf>,
) -> Result<Option<(PathBuf, String, Option<PathBuf>)>> {
    match (worktree_repo, branch) {
        (Some(repo_root), Some(branch_name)) => Ok(Some((repo_root, branch_name, worktree_path))),
        (None, None) if worktree_path.is_some() => {
            bail!("--worktree-path requires --worktree-repo and --branch")
        }
        (None, None) => Ok(None),
        _ => bail!("--worktree-repo and --branch must be provided together"),
    }
}

/// Print one shared control receipt on the CLI surface.
///
/// The CLI does not format Lane control results itself: it renders the same
/// [`codewhale_lane::ControlReceipt`] the slash command and hotbar render, so
/// the three surfaces cannot drift in what they report (#1888).
fn emit_control_receipt(receipt: &codewhale_lane::ControlReceipt, json: bool) -> Result<()> {
    if json {
        // v0.9.2 compatibility: `lane list --json` has always emitted an array
        // of `LaneRecord`, and `lane status --json` a single one. Scripts
        // select `.[].id`, `.worktree_path`, `.log_path` off that shape, so the
        // receipt does not replace it. The receipt is what every other verb
        // emits, and what the human renderer shows for these two.
        match receipt.operation {
            codewhale_lane::ControlOperation::LaneList => {
                println!("{}", serde_json::to_string_pretty(&receipt.lane_records)?);
            }
            codewhale_lane::ControlOperation::LaneStatus => match receipt.lane_records.first() {
                Some(record) => println!("{}", serde_json::to_string_pretty(record)?),
                // Legacy behaviour for an unknown id: `reg.load()` failed, so
                // the command errored on stderr and printed *nothing* on
                // stdout. Emitting a receipt (or a bare `null`) here would make
                // `lane status --json <bad-id> | jq` succeed where it used to
                // fail. Stay silent and let the bail! below set the exit code.
                None if receipt.is_error() => {}
                None => println!("{}", serde_json::to_string_pretty(receipt)?),
            },
            _ => println!("{}", serde_json::to_string_pretty(receipt)?),
        }
    } else if receipt.is_error() {
        eprintln!("{}", receipt.render());
    } else {
        println!("{}", receipt.render());
    }
    if receipt.is_error() {
        let detail = receipt
            .failure
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| receipt.outcome.as_str().to_string());
        bail!("{}: {detail}", receipt.operation_id);
    }
    Ok(())
}

fn run_lane_control(
    operation: codewhale_lane::ControlOperation,
    lane_id: Option<&str>,
    json: bool,
) -> Result<()> {
    let receipt = codewhale_lane::control::execute_lane_control(
        codewhale_lane::ControlSurface::Cli,
        operation,
        lane_id,
    );
    emit_control_receipt(&receipt, json)
}

/// Read size for [`read_tail_lines`]; one chunk covers any ordinary tail.
const LANE_LOG_TAIL_CHUNK_BYTES: u64 = 64 * 1024;

/// The last `tail` non-empty lines of `file`, read backwards `chunk` bytes at
/// a time, so allocation and disk work scale with the requested tail rather
/// than with the whole (append-only, unbounded) lane log. Leaves the file
/// positioned at the end it measured, where `--follow` continues.
fn read_tail_lines(
    file: &mut std::fs::File,
    tail: usize,
    chunk: u64,
) -> std::io::Result<Vec<Vec<u8>>> {
    use std::io::{Seek, SeekFrom};

    let end = file.seek(SeekFrom::End(0))?;
    let mut pos = end;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        // Until the read reaches the start of the file, the bytes before the
        // first newline may be the tail of a longer line: never count them.
        let complete = if pos == 0 {
            &buf[..]
        } else {
            buf.iter()
                .position(|byte| *byte == b'\n')
                .map_or(&[][..], |index| &buf[index + 1..])
        };
        let lines: Vec<&[u8]> = complete
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .collect();
        if pos == 0 || lines.len() >= tail {
            let start = lines.len().saturating_sub(tail);
            let tail_lines = lines[start..].iter().map(|line| line.to_vec()).collect();
            file.seek(SeekFrom::Start(end))?;
            return Ok(tail_lines);
        }
        let read = chunk.max(1).min(pos);
        pos -= read;
        file.seek(SeekFrom::Start(pos))?;
        let mut block = vec![0; usize::try_from(read).unwrap_or(usize::MAX)];
        file.read_exact(&mut block)?;
        block.extend_from_slice(&buf);
        buf = block;
    }
}

fn run_lane_command(args: LaneArgs) -> Result<()> {
    use codewhale_lane::{ControlOperation, LaneRegistry, backend_for};
    use std::io::{BufRead, Write};
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    match args.command {
        LaneCommand::List { json } => run_lane_control(ControlOperation::LaneList, None, json),
        LaneCommand::Status { lane_id, json } => {
            run_lane_control(ControlOperation::LaneStatus, Some(&lane_id), json)
        }
        LaneCommand::Interrupt { lane_id, json } => {
            run_lane_control(ControlOperation::LaneInterrupt, Some(&lane_id), json)
        }
        LaneCommand::Restart { lane_id, json } => {
            run_lane_control(ControlOperation::LaneRestart, Some(&lane_id), json)
        }
        LaneCommand::Resume { lane_id, json } => {
            run_lane_control(ControlOperation::LaneResume, Some(&lane_id), json)
        }
        LaneCommand::Attach { lane_id, print } => {
            let reg = LaneRegistry::open_default()?;
            let mut lane = reg.load(&lane_id)?;
            let backend = backend_for(&lane);
            backend.reconcile(&reg, &mut lane)?;
            let Some(attach) = backend.attach_command(&lane) else {
                if !lane.status.is_active() {
                    bail!(
                        "lane `{lane_id}` is {} and has no active attach target",
                        lane.status.as_str()
                    );
                }
                bail!(
                    "lane `{lane_id}` runtime `{}` has no attach target",
                    lane.runtime.as_str()
                );
            };
            if print {
                println!("{attach}");
                return Ok(());
            }
            if let Some(session) = lane.tmux_session.as_deref() {
                let socket = lane
                    .tmux_socket
                    .as_deref()
                    .context("tmux lane is missing its pinned server socket")?;
                let status = Command::new("tmux")
                    .arg("-S")
                    .arg(socket)
                    .args(["attach", "-t", session])
                    .status();
                match status {
                    Ok(s) if s.success() => Ok(()),
                    Ok(s) => bail!("tmux attach failed ({s}); command was: {attach}"),
                    Err(err) => {
                        eprintln!("could not exec tmux: {err}");
                        println!("{attach}");
                        bail!("tmux attach unavailable");
                    }
                }
            } else {
                println!("{attach}");
                Ok(())
            }
        }
        LaneCommand::Logs {
            lane_id,
            follow,
            tail,
        } => {
            let reg = LaneRegistry::open_default()?;
            let lane = reg.load(&lane_id)?;
            let path = lane.log_path;
            if !path.exists() {
                bail!("log file missing: {}", path.display());
            }
            let mut file = std::fs::File::open(&path)?;
            let lines = read_tail_lines(&mut file, tail, LANE_LOG_TAIL_CHUNK_BYTES)?;
            let mut stdout = std::io::stdout().lock();
            for line in &lines {
                stdout.write_all(String::from_utf8_lossy(line).as_bytes())?;
                stdout.write_all(b"\n")?;
            }
            stdout.flush()?;
            if !follow {
                return Ok(());
            }
            // Same handle, already at the end the tail measured: a line
            // appended between the tail and the follow is printed, not lost.
            let mut reader = std::io::BufReader::new(file);
            loop {
                let mut line = Vec::new();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) => {
                        thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                    Ok(_) => {
                        let mut stdout = std::io::stdout().lock();
                        stdout.write_all(String::from_utf8_lossy(&line).as_bytes())?;
                        stdout.flush()?;
                    }
                    Err(err) => return Err(err.into()),
                }
            }
        }
        // `stop` is the historical spelling of `interrupt`. Both go through
        // the same verb so the durable transition, the lifecycle fence, and
        // the receipt are identical.
        LaneCommand::Stop { lane_id, json } => {
            run_lane_control(ControlOperation::LaneInterrupt, Some(&lane_id), json)
        }
        LaneCommand::Start {
            workflow,
            fleet,
            issue,
            goal,
            runtime,
            worktree_repo,
            branch,
            worktree_path,
            worktree_ttl_secs,
            command,
        } => start_lane(LaneStartRequest {
            workflow,
            fleet,
            issue,
            goal,
            runtime,
            worktree_repo,
            branch,
            worktree_path,
            worktree_ttl_secs,
            command,
            environment: Vec::new(),
            cwd: None,
        }),
    }
}

fn run_lane_log_proxy_command(args: LaneLogProxyArgs) -> Result<()> {
    let exit_code = codewhale_lane::run_lane_log_proxy(codewhale_lane::LaneLogProxySpec {
        command: args.command,
        log_path: args.log_path,
        receipt_path: args.receipt_path,
        receipt_tmp_path: args.receipt_tmp_path,
        environment_path: args.environment_path,
        lane_id: args.lane_id,
    })?;
    std::process::exit(exit_code);
}

fn run_workflow_command(
    cli: &Cli,
    resolved_runtime: &ResolvedRuntimeOptions,
    config_path: &Path,
    args: WorkflowArgs,
) -> Result<()> {
    match args.command {
        WorkflowCommand::Run {
            workflow,
            fleet,
            issue,
            goal,
            runtime,
            source_path,
            token_budget,
            verify,
            worktree_repo,
            branch,
            worktree_path,
            worktree_ttl_secs,
        } => {
            let workspace = workflow_workspace_root(cli.workspace.as_deref())?;
            let source_path =
                resolve_workflow_source_path(&workflow, source_path.as_ref(), &workspace)?;
            validate_workflow_source_file(&source_path)?;

            let source_root = if let Some(repo) = worktree_repo.as_deref() {
                repo.canonicalize()
                    .with_context(|| format!("resolve --worktree-repo {}", repo.display()))?
            } else {
                workspace.clone()
            };

            // A fleet is an optional pin layer, not a requirement: role-only
            // tasks resolve against the built-in roster and the session route
            // (matching the TUI tool path). When a fleet IS given, it is
            // loaded and validated before the run starts.
            if let Some(name) = fleet.as_deref() {
                let roots = named_fleet_search_roots(&workspace);
                let loaded =
                    codewhale_workflow::load_named_fleet(name, &roots).with_context(|| {
                        format!("load Fleet `{name}` from {}", display_roots(&roots))
                    })?;
                if workflow == "stopship" || name == "stopship" {
                    loaded
                        .validate_stopship_roles()
                        .with_context(|| format!("validate stopship roles in Fleet `{name}`"))?;
                }
            }

            let process = workflow_exec_command(WorkflowExecSpec {
                cli,
                resolved_runtime,
                config_path,
                source_root: &source_root,
                source_path: &source_path,
                workflow: &workflow,
                fleet: fleet.as_deref(),
                issue: issue.as_deref(),
                goal: goal.as_deref(),
                token_budget,
                verify,
            })?;
            start_lane(LaneStartRequest {
                workflow: Some(workflow),
                fleet,
                issue,
                goal,
                runtime,
                worktree_repo,
                branch,
                worktree_path,
                worktree_ttl_secs,
                command: process.command,
                environment: process.environment,
                cwd: Some(workspace),
            })
        }
    }
}

fn workflow_workspace_root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return path
            .canonicalize()
            .with_context(|| format!("resolve workflow workspace {}", path.display()));
    }
    let cwd = std::env::current_dir().context("resolve current directory")?;
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout);
        let root = text.trim();
        if !root.is_empty() {
            let root = PathBuf::from(root);
            return Ok(root.canonicalize().unwrap_or(root));
        }
    }
    Ok(cwd)
}

fn resolve_workflow_source_path(
    workflow: &str,
    source_path: Option<&PathBuf>,
    workspace: &Path,
) -> Result<PathBuf> {
    let candidates = workflow_source_candidates(workflow, source_path, workspace);
    for candidate in &candidates {
        if candidate.is_file() {
            return Ok(candidate.clone());
        }
    }
    bail!(
        "workflow source for `{workflow}` not found; tried {}",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn workflow_source_candidates(
    workflow: &str,
    source_path: Option<&PathBuf>,
    workspace: &Path,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = source_path {
        candidates.push(resolve_against_workspace(path, workspace));
        return candidates;
    }

    let raw = workflow.trim();
    let workflow_path = PathBuf::from(raw);
    if raw.contains('/') || raw.contains('\\') || raw.ends_with(".js") || raw.ends_with(".ts") {
        candidates.push(resolve_against_workspace(&workflow_path, workspace));
        return candidates;
    }

    let normalized = raw.replace('-', "_");
    for rel in [
        format!("workflows/{raw}.workflow.js"),
        format!("workflows/{normalized}.workflow.js"),
    ] {
        let path = workspace.join(rel);
        if !candidates.iter().any(|existing| existing == &path) {
            candidates.push(path);
        }
    }
    candidates
}

fn resolve_against_workspace(path: &Path, workspace: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    }
}

fn validate_workflow_source_file(path: &Path) -> Result<()> {
    let source =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if source.trim_start().starts_with("export default workflow(")
        || source.trim_start().starts_with("workflow(")
        || source.contains("\nworkflow(")
    {
        let identifier = path.display().to_string();
        if path.extension().and_then(|ext| ext.to_str()) == Some("ts") {
            codewhale_workflow::compile_typescript_workflow(&identifier, &source)
                .with_context(|| format!("parse declarative Workflow {}", path.display()))?;
        } else {
            codewhale_workflow::compile_javascript_workflow(&identifier, &source)
                .with_context(|| format!("parse declarative Workflow {}", path.display()))?;
        }
    }
    Ok(())
}

/// The same roots, in the same order, as the TUI's `fleet_search_roots`:
/// `$CODEWHALE_HOME`, then `<workspace>/.codewhale` (where the Fleet store
/// saves folder Fleets), then the workspace root for checked-in rosters.
fn named_fleet_search_roots(workspace: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(home) = codewhale_config::codewhale_home() {
        roots.push(home);
    }
    roots.push(workspace.join(".codewhale"));
    roots.push(workspace.to_path_buf());
    roots
}

fn display_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

struct WorkflowExecSpec<'a> {
    cli: &'a Cli,
    resolved_runtime: &'a ResolvedRuntimeOptions,
    config_path: &'a Path,
    source_root: &'a Path,
    source_path: &'a Path,
    workflow: &'a str,
    fleet: Option<&'a str>,
    issue: Option<&'a str>,
    goal: Option<&'a str>,
    token_budget: Option<u64>,
    verify: bool,
}

struct WorkflowProcessSpec {
    command: Vec<String>,
    environment: Vec<(String, String)>,
}

fn workflow_exec_command(spec: WorkflowExecSpec<'_>) -> Result<WorkflowProcessSpec> {
    let WorkflowExecSpec {
        cli,
        resolved_runtime,
        config_path,
        source_root,
        source_path,
        workflow,
        fleet,
        issue,
        goal,
        token_budget,
        verify,
    } = spec;
    let source_arg = source_path
        .strip_prefix(source_root)
        .with_context(|| {
            format!(
                "workflow source {} must be inside execution root {}",
                source_path.display(),
                source_root.display()
            )
        })?
        .display()
        .to_string();
    let mut payload = serde_json::json!({
        "action": "run",
        "source_path": source_arg,
        "fleet": fleet,
        "args": {
            "workflow": workflow,
            "fleet": fleet,
            "issue": issue,
            "goal": goal,
        },
        "verify": verify,
    });
    if let Some(token_budget) = token_budget {
        payload["token_budget"] = serde_json::json!(token_budget);
    }
    let input_json = serde_json::to_string(&payload)?;
    let passthrough = vec![
        "workflow-tool".to_string(),
        "--approval-source".to_string(),
        "explicit-workflow-command".to_string(),
        "--input-json".to_string(),
        input_json,
    ];
    let argv = {
        // Build argv with explicit config path like the previous dispatcher did.
        let mut args = Vec::new();
        let executable = std::env::current_exe()
            .context("resolve current Codewhale executable for workflow lane")?;
        let executable = executable.into_os_string().into_string().map_err(|path| {
            anyhow!(
                "current Codewhale executable path is not valid UTF-8: {}",
                PathBuf::from(path).display()
            )
        })?;
        args.push(executable);
        // config_path is the explicit workflow config path; prefer it over cli.config
        let cfg = Some(config_path);
        if let Some(cp) = cfg {
            args.push("--config".to_string());
            args.push(cp.display().to_string());
        } else if let Some(cp) = cli.config.as_deref() {
            args.push("--config".to_string());
            args.push(cp.display().to_string());
        }
        if let Some(profile) = cli.profile.as_ref() {
            args.push("--profile".to_string());
            args.push(profile.clone());
        }

        if cli.mouse_capture {
            args.push("--mouse-capture".to_string());
        }
        if cli.no_mouse_capture {
            args.push("--no-mouse-capture".to_string());
        }
        if cli.skip_onboarding {
            args.push("--skip-onboarding".to_string());
        }
        if cli.no_project_config {
            args.push("--no-project-config".to_string());
        }
        args.extend(passthrough.clone());
        args
    };
    apply_tui_env(cli, resolved_runtime, &passthrough);
    lane_process_spec_from_argv(&argv)
}

fn valid_lane_environment_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn shell_owned_lane_environment(key: &str) -> bool {
    matches!(
        key,
        "PWD" | "OLDPWD" | "SHLVL" | "_" | "TERM" | "TMUX" | "TMUX_PANE"
    )
}

fn lane_process_spec_from_argv(argv: &[String]) -> Result<WorkflowProcessSpec> {
    let mut environment = std::collections::BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        let (Some(key), Some(value)) = (key.to_str(), value.to_str()) else {
            continue;
        };
        if valid_lane_environment_key(key) && !shell_owned_lane_environment(key) {
            environment.insert(key.to_string(), value.to_string());
        }
    }
    Ok(WorkflowProcessSpec {
        command: argv.to_vec(),
        environment: environment.into_iter().collect(),
    })
}

/// Flags for `codewhale remote-setup`. Forwarded to the TUI binary, which owns
/// the interactive wizard and bundle generation.
#[derive(Debug, Args, Clone, Default)]
struct RemoteSetupArgs {
    /// Cloud target slug (lighthouse, azure, digitalocean). Skips the prompt.
    #[arg(long)]
    cloud: Option<String>,
    /// Chat bridge slug (feishu, telegram). Skips the prompt.
    #[arg(long)]
    bridge: Option<String>,
    /// Provider slug; validated against the provider registry. Skips the prompt.
    #[arg(long)]
    provider: Option<String>,
    /// Bundle output directory (default `./codewhale-deploy/<cloud>-<bridge>`).
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,
    /// Emit the bundle, do not provision (default).
    #[arg(long, default_value_t = false)]
    generate_only: bool,
    /// Reserved for cloud auto-provisioning, which is not implemented.
    /// Hidden from `--help`; passing it makes `remote-setup` fail.
    #[arg(
        long,
        default_value_t = false,
        conflicts_with = "generate_only",
        hide = true
    )]
    apply: bool,
    /// Skip the final confirmation gate (CI / non-interactive).
    #[arg(long, default_value_t = false)]
    yes: bool,
    /// Fail instead of prompting if any required value is missing.
    #[arg(long, default_value_t = false)]
    non_interactive: bool,
}

/// Build the forwarded argv for the TUI `remote-setup` subcommand from the
/// structured CLI flags. Mirrors the named flags exactly so the TUI clap parser
/// re-derives the same `RemoteSetupArgs`.
fn remote_setup_tui_args(args: RemoteSetupArgs) -> Vec<String> {
    let mut forwarded = vec!["remote-setup".to_string()];
    if let Some(cloud) = args.cloud {
        forwarded.push("--cloud".to_string());
        forwarded.push(cloud);
    }
    if let Some(bridge) = args.bridge {
        forwarded.push("--bridge".to_string());
        forwarded.push(bridge);
    }
    if let Some(provider) = args.provider {
        forwarded.push("--provider".to_string());
        forwarded.push(provider);
    }
    if let Some(out) = args.out {
        forwarded.push("--out".to_string());
        forwarded.push(out.to_string_lossy().into_owned());
    }
    if args.generate_only {
        forwarded.push("--generate-only".to_string());
    }
    if args.apply {
        forwarded.push("--apply".to_string());
    }
    if args.yes {
        forwarded.push("--yes".to_string());
    }
    if args.non_interactive {
        forwarded.push("--non-interactive".to_string());
    }
    forwarded
}

#[derive(Debug, Args)]
struct LogoutArgs {
    /// Delete without the confirmation prompt (required when stdin is not a terminal).
    #[arg(long, short = 'y', default_value_t = false)]
    yes: bool,
}

#[derive(Debug, Args)]
struct LoginArgs {
    /// Print the verification URL without trying to open a browser.
    #[arg(long, default_value_t = false)]
    no_open: bool,
    /// Maximum time to wait for browser authorization.
    #[arg(
        long = "timeout-seconds",
        default_value_t = cloud::DEFAULT_LOGIN_TIMEOUT_SECONDS,
        value_parser = clap::value_parser!(u64).range(1..=cloud::MAX_LOGIN_TIMEOUT_SECONDS)
    )]
    timeout_seconds: u64,
    /// Legacy provider-key flag: rejected with a redirect to `auth set`.
    #[arg(long, hide = true)]
    api_key: Option<String>,
    /// Legacy provider flag: rejected with a redirect to `auth set`.
    #[arg(long, value_parser = parse_catalog_route, hide = true)]
    provider: Option<ProviderKind>,
}

#[derive(Debug, Args)]
struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Sign in to a reviewed plugin-defined OAuth provider (PKCE loopback).
    #[command(name = "plugin-login")]
    PluginLogin {
        #[arg(long)]
        provider: String,
    },
    /// Remove host-owned credentials for a plugin-defined provider.
    #[command(name = "plugin-logout")]
    PluginLogout {
        #[arg(long)]
        provider: String,
    },

    /// Sign in to xAI/Grok with an SSH-friendly device code; run again to switch accounts.
    ///
    /// The account you approve on the xAI page replaces the Codewhale-owned
    /// xAI sign-in. `codewhale auth status --provider xai` shows which
    /// account is signed in.
    #[command(name = "xai-device")]
    XaiDevice,
    /// Sign in with ChatGPT; use CODEWHALE_CHATGPT_NEW_ACCOUNT=1 to register another account.
    ///
    /// Opens the ChatGPT sign-in page (PKCE loopback) and asks you to sign
    /// in, so you can choose a different account than the one the browser
    /// is using; if it does not, open the printed URL in a private window.
    /// The account you choose replaces the Codewhale-owned ChatGPT sign-in.
    /// `codewhale auth status --provider openai-codex` shows which account
    /// is signed in.
    #[command(name = "chatgpt")]
    Chatgpt,
    /// Revoke Codewhale-owned ChatGPT tokens. Codex CLI consent is unchanged.
    #[command(name = "chatgpt-revoke")]
    ChatgptRevoke,
    #[command(name = "claude", alias = "anthropic")]
    Claude,
    #[command(name = "claude-revoke")]
    ClaudeRevoke,
    /// Sign in to OrcaRouter with OAuth 2.0 + PKCE and store the issued key.
    ///
    /// Opens the OrcaRouter consent screen on a loopback callback and
    /// exchanges the authorization code for a durable `sk-orca-...` API key.
    /// The key is billed to your OrcaRouter account and revocable there.
    /// To paste an existing key instead, use
    /// `codewhale auth set --provider orcarouter`.
    #[command(name = "orcarouter")]
    Orcarouter,
    /// Explicitly allow read-only access to one credential file owned by
    /// another CLI. Managed mutation is currently unsupported and fails closed.
    #[command(name = "external-consent")]
    ExternalConsent {
        #[arg(long, value_parser = parse_catalog_route)]
        provider: ProviderKind,
        #[arg(long, value_enum)]
        mode: ExternalCredentialModeArg,
        /// Exact credential file path. Defaults to the selected CLI's resolved
        /// path without probing whether the file exists.
        #[arg(long, value_name = "PATH")]
        path: Option<PathBuf>,
        /// Confirm the disclosed exact read-only grant without an interactive
        /// prompt. Required when stdin is not a terminal.
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Revoke access to another CLI's credential file for one provider.
    #[command(name = "external-revoke")]
    ExternalRevoke {
        #[arg(long, value_parser = parse_catalog_route)]
        provider: ProviderKind,
    },
    /// Show current provider and runtime-effective credential route state.
    /// Without `--provider`, shows all known providers.
    /// With `--provider`, shows detailed status for that provider.
    Status {
        /// Show status for a specific provider only.
        #[arg(long, value_parser = parse_catalog_route)]
        provider: Option<ProviderKind>,
        /// Report resolved home/config/settings/backend paths and structural
        /// credential-source presence without printing credential values or
        /// probing provider credential stores.
        #[arg(long, default_value_t = false)]
        diagnostic: bool,
    },
    /// Save an API key to the credential store (config keeps metadata only).
    /// Reads from `--api-key`, `--api-key-stdin`, or prompts on stdin when
    /// neither is given. Does not echo the key.
    Set {
        #[arg(long, value_parser = parse_catalog_route)]
        provider: ProviderKind,
        /// Inline value (discouraged — visible in the process list and shell
        /// history; prefer `--api-key-stdin`).
        #[arg(long)]
        api_key: Option<String>,
        /// Read the key from stdin instead of prompting.
        #[arg(long = "api-key-stdin", default_value_t = false)]
        api_key_stdin: bool,
    },
    /// Report the effective credential route for a provider. Never prints a
    /// credential; reports the source layer or structural OAuth/repair state.
    Get {
        #[arg(long, value_parser = parse_catalog_route)]
        provider: ProviderKind,
    },
    /// Pipe the runtime-effective API key to a local client; refuses terminals.
    PrintApiKey {
        #[arg(long, value_parser = parse_catalog_route)]
        provider: ProviderKind,
    },
    /// Delete a provider's key from config and secret-store storage.
    Clear {
        #[arg(long, value_parser = parse_auth_clear_provider)]
        provider: ProviderKind,
    },
    /// List all known providers with their runtime-effective auth state,
    /// without revealing credentials.
    List,
    /// Advanced: migrate config-file keys into a platform credential store.
    #[command(hide = true)]
    Migrate {
        /// Don't actually write anything; print what would change.
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExternalCredentialModeArg {
    ReadOnly,
    Managed,
}

#[derive(Debug, Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Get {
        key: String,
    },
    Set {
        key: String,
        value: String,
    },
    Unset {
        key: String,
    },
    /// Review aggregate usage counting by Codewhale and PostHog (default on).
    Telemetry {
        /// Optional compatibility form: enable future sessions under this policy version.
        #[arg(long, value_name = "VERSION")]
        accept_notice: Option<u32>,
    },
    List,
    Path,
    /// Open the config file in `$VISUAL`/`$EDITOR` (else `vi`).
    Edit,
    /// Check the loaded config: unknown keys, empty secrets, malformed
    /// URLs. Read-only; prints warnings, fails on errors, never prints a
    /// credential.
    Doctor,
    /// Print the effective config (including `--set` overlays) as TOML with
    /// secrets redacted by key name.
    Dump,
    /// Import a portable config bundle from a file, HTTPS URL, or stdin (-).
    Import(config_bundles::ImportArgs),
    /// Export a portable, secret-free config bundle.
    Export(config_bundles::ExportArgs),
    /// Move legacy top-level `base_url` / `api_key` into their
    /// `[providers.<name>]` tables, keeping comments. Writes a one-time,
    /// credential-free backup first. Codewhale already reads the old shape;
    /// this only tidies the file.
    Migrate {
        /// Print what would move without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Resolve a top-level value that disagrees with its provider table
        /// by keeping one of them. Without it, a conflicting pair is left
        /// exactly as it is.
        #[arg(long, value_enum)]
        prefer: Option<LegacyRootPreferArg>,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum LegacyRootPreferArg {
    /// Keep the top-level value (the key Codewhale sends today).
    TopLevel,
    /// Keep the `[providers.<name>]` value.
    Table,
}

impl From<LegacyRootPreferArg> for codewhale_config::legacy_root::LegacyRootPrefer {
    fn from(value: LegacyRootPreferArg) -> Self {
        match value {
            LegacyRootPreferArg::TopLevel => Self::TopLevel,
            LegacyRootPreferArg::Table => Self::Table,
        }
    }
}

#[derive(Debug, Args)]
struct ModelArgs {
    #[command(subcommand)]
    command: ModelCommand,
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    List {
        #[arg(long, value_parser = parse_catalog_route)]
        provider: Option<ProviderKind>,
    },
    Resolve {
        model: Option<String>,
        #[arg(long, value_parser = parse_catalog_route)]
        provider: Option<ProviderKind>,
    },
    /// Set the default model (e.g. "deepseek-v4-pro"; "pro"/"flash" on routes that serve DeepSeek).
    Set { model: String },
}

#[derive(Debug, Args)]
struct ThreadArgs {
    #[command(subcommand)]
    command: ThreadCommand,
}

#[derive(Debug, Subcommand)]
enum ThreadCommand {
    List {
        #[arg(long, default_value_t = false)]
        all: bool,
        #[arg(long)]
        limit: Option<usize>,
    },
    Read {
        thread_id: String,
    },
    /// Resume through the acknowledged owner and print its durable receipt.
    Resume {
        thread_id: String,
        /// Retry an uncertain control with its original intent key.
        #[arg(long)]
        operation_key: Option<String>,
    },
    /// Fork complete history through the owner and print the new receipt.
    Fork {
        thread_id: String,
        /// Retry an uncertain control with its original intent key.
        #[arg(long)]
        operation_key: Option<String>,
    },
    Archive {
        thread_id: String,
    },
    Unarchive {
        thread_id: String,
    },
    SetName {
        thread_id: String,
        name: String,
    },
    /// Remove the custom name from a thread, restoring the default
    /// `(unnamed)` rendering in `thread list`.
    ClearName {
        thread_id: String,
    },
}

#[derive(Debug, Args)]
struct SandboxArgs {
    #[command(subcommand)]
    command: SandboxCommand,
}

#[derive(Debug, Subcommand)]
enum SandboxCommand {
    Check {
        command: String,
        #[arg(long, value_enum, default_value_t = ApprovalModeArg::OnRequest)]
        ask: ApprovalModeArg,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ApprovalModeArg {
    UnlessTrusted,
    OnFailure,
    OnRequest,
    Never,
}

impl From<ApprovalModeArg> for AskForApproval {
    fn from(value: ApprovalModeArg) -> Self {
        match value {
            ApprovalModeArg::UnlessTrusted => AskForApproval::UnlessTrusted,
            ApprovalModeArg::OnFailure => AskForApproval::OnFailure,
            ApprovalModeArg::OnRequest => AskForApproval::OnRequest,
            ApprovalModeArg::Never => AskForApproval::Never,
        }
    }
}

#[derive(Debug, Args)]
struct AppServerArgs {
    /// Serve the full HTTP/SSE runtime API (`/v1/*`: sessions, threads, turns,
    /// approvals, events, usage, fleet, tasks). This is the canonical runtime
    /// API surface; it delegates to the same server as `codewhale serve --http`.
    #[arg(long, conflicts_with_all = ["stdio", "mobile"])]
    http: bool,
    /// Serve the runtime API plus the phone-friendly mobile control page.
    /// Equivalent to the legacy `codewhale serve --mobile`.
    #[arg(long, conflicts_with = "stdio")]
    mobile: bool,
    /// Run the app-server JSON-RPC control transport over stdio.
    /// Used by local SDKs and JSON-RPC integrations.
    #[arg(long, default_value_t = false)]
    stdio: bool,
    /// Run as the desktop daemon: the same JSON-RPC control transport as
    /// `--stdio`, served on a user-private local endpoint under the Codewhale
    /// runtime directory (Unix socket or Windows named pipe). Clients must
    /// authenticate and `daemon/attach` first.
    #[arg(long, default_value_t = false, conflicts_with_all = ["stdio", "http", "mobile"])]
    socket: bool,
    /// Socket path override for --socket. Defaults to
    /// `$CODEWHALE_HOME/run/daemon.sock`, else `$XDG_RUNTIME_DIR/codewhale/daemon.sock`,
    /// else `~/Library/Application Support/codewhale/daemon.sock` (macOS) or
    /// `~/.codewhale/run/daemon.sock`.
    #[arg(long = "socket-path", requires = "socket")]
    socket_path: Option<PathBuf>,
    /// Show a QR code for the mobile URL in the terminal (requires --mobile).
    #[arg(long, requires = "mobile")]
    qr: bool,
    /// Bind host. Defaults to 127.0.0.1. --mobile is loopback-only: it does
    /// not widen the bind, and a non-loopback mobile bind is rejected.
    #[arg(long)]
    host: Option<String>,
    /// Bind port. Defaults to 7878 for --http/--mobile (the runtime API) and
    /// 8787 for the legacy in-process app-server HTTP transport.
    #[arg(long)]
    port: Option<u16>,
    /// Background task worker count (1-8). Only used with --http/--mobile.
    #[arg(long)]
    workers: Option<usize>,
    #[arg(long)]
    config: Option<PathBuf>,
    /// Bearer token required on runtime API routes. Visible in the process
    /// list; prefer CODEWHALE_RUNTIME_TOKEN.
    #[arg(long = "auth-token")]
    auth_token: Option<String>,
    #[arg(long, default_value_t = false)]
    insecure_no_auth: bool,
    #[arg(long = "cors-origin")]
    cors_origin: Vec<String>,
}

fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn run_cli() -> std::process::ExitCode {
    install_rustls_crypto_provider();

    let outcome = run();
    // A config write may have moved legacy top-level `base_url` / `api_key`
    // into their provider tables (#6394); say so once, off stdout.
    for notice in codewhale_config::legacy_root::take_notices() {
        eprintln!("note: {notice}");
    }
    match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            // Use the full anyhow chain so callers see the underlying
            // cause (e.g. the actual TOML parse error with line/column)
            // instead of just the top-level context message. The bare
            // `{err}` Display impl drops the chain — see #767, where
            // users hit "failed to parse config at <path>" with no
            // hint that the real error was a stray BOM or unbalanced
            // quote a few lines down.
            eprintln!("error: {err}");
            for cause in err.chain().skip(1) {
                eprintln!("  caused by: {cause}");
            }
            // A Codewhale account failure carries a class: CI logs must be
            // able to tell a bad credential from an unconfigured agent model
            // without parsing English, and the machine-readable code beside
            // it names the control-plane branch that was taken.
            if let Some(machine) = err.downcast_ref::<cloud::machine::MachineError>() {
                eprintln!(
                    "  codewhale: code={} status={}",
                    machine.code, machine.status
                );
                if let Ok(code) = u8::try_from(machine.exit_code) {
                    return std::process::ExitCode::from(code);
                }
            }
            std::process::ExitCode::FAILURE
        }
    }
}

fn split_lane_log_proxy_command(
    command: Option<Commands>,
) -> (Option<LaneLogProxyArgs>, Option<Commands>) {
    match command {
        Some(Commands::LaneLogProxy(args)) => (Some(args), None),
        command => (None, command),
    }
}

fn config_command_targets_project(matches: &clap::ArgMatches) -> bool {
    let Some(config_matches) = matches.subcommand_matches("config") else {
        return false;
    };
    let Some((command, command_matches)) = config_matches.subcommand() else {
        return false;
    };
    if !matches!(command, "import" | "export") {
        return false;
    }
    command_matches
        .try_get_one::<bool>("project")
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

fn config_store_path_for_dispatch(
    explicit_path: Option<PathBuf>,
    project_bundle_scope: bool,
    cwd: &Path,
) -> Option<PathBuf> {
    if explicit_path.is_none() && project_bundle_scope {
        // Mirror the project-config loader: the current app dir wins, but a
        // workspace that still keeps its document under the legacy app dir
        // must be read and updated in place rather than shadowed by a new
        // empty document.
        let current = cwd
            .join(codewhale_config::CODEWHALE_APP_DIR)
            .join(codewhale_config::CONFIG_FILE_NAME);
        let legacy = cwd
            .join(codewhale_config::LEGACY_APP_DIR)
            .join(codewhale_config::CONFIG_FILE_NAME);
        if !current.is_file() && legacy.is_file() {
            return Some(legacy);
        }
        return Some(current);
    }
    explicit_path
}

/// Runtime `--set` uses the dedicated flag handoff, so the existing loader
/// owns profile, provider, managed-policy and requirements precedence. Keep
/// config read/write commands on their separate, never-saved store overlay.
fn apply_runtime_set_overrides(cli: &mut Cli) -> Result<()> {
    let mut values = CliRuntimeOverrides::default();
    let mut provider = None;
    for spec in &cli.overrides {
        let (key, value) = spec
            .split_once('=')
            .context("invalid --set: expected KEY=VALUE (value omitted)")?;
        match key.trim() {
            "provider" => {
                provider = Some(
                    parse_provider_identifier(value)
                        .map_err(|_| anyhow!("invalid --set provider (value omitted)"))?,
                );
            }
            "model" | "default_text_model" => values.model = Some(value.to_string()),
            "verbosity" => values.verbosity = Some(value.to_string()),
            "approval_policy" => values.approval_policy = Some(value.to_string()),
            "sandbox_mode" => values.sandbox_mode = Some(value.to_string()),
            "telemetry" => {
                let mut config = ConfigToml::default();
                config
                    .set_value("telemetry", value)
                    .map_err(|_| anyhow!("invalid --set telemetry: expected a boolean"))?;
                values.telemetry = config.telemetry;
            }
            _ => bail!(
                "unsupported runtime --set key (value omitted): supported keys are provider, \
                 model, default_text_model, verbosity, approval_policy, sandbox_mode and \
                 telemetry; use the dedicated option or config set for other keys"
            ),
        }
        if value.trim().is_empty() {
            bail!("invalid runtime --set: value must not be empty");
        }
    }
    // A dedicated flag is more specific than a generic --set for the same
    // field. Repeated --set keys otherwise keep their last value.
    cli.provider = cli.provider.take().or(provider);
    cli.model = cli.model.take().or(values.model);
    cli.verbosity = cli.verbosity.take().or(values.verbosity);
    cli.approval_policy = cli.approval_policy.take().or(values.approval_policy);
    cli.sandbox_mode = cli.sandbox_mode.take().or(values.sandbox_mode);
    cli.telemetry = cli.telemetry.or(values.telemetry);
    Ok(())
}

/// `--output-mode` is retired (#6516): accepted so old scripts keep running,
/// but a caller who passes it is told it does nothing.
fn retired_output_mode_warning(cli: &Cli) -> Option<&'static str> {
    cli.output_mode.as_ref().map(|_| {
        "warning: --output-mode has no effect and is ignored; it will be removed in a future release"
    })
}

/// A secret passed as an argv value is readable by other local users through
/// the process list (and lands in shell history). Name the non-argv route.
/// `login`/`account` already reject the global `--api-key` with their own
/// guidance, so they get no second line. The pipe-only credential handoff
/// keeps its existing bounded diagnostics instead of adding interactive advice.
fn argv_secret_warning(cli: &Cli, command: Option<&Commands>) -> Option<&'static str> {
    const RUNTIME_TOKEN: &str =
        "warning: --auth-token is visible in the process list; use CODEWHALE_RUNTIME_TOKEN instead";
    match command {
        Some(Commands::AppServer(args)) if args.auth_token.is_some() => Some(RUNTIME_TOKEN),
        Some(Commands::Serve(args))
            if args
                .args
                .iter()
                .take_while(|arg| *arg != "--")
                .any(|arg| arg == "--auth-token" || arg.starts_with("--auth-token=")) =>
        {
            Some(RUNTIME_TOKEN)
        }
        Some(Commands::Auth(AuthArgs {
            command: AuthCommand::Set {
                api_key: Some(_), ..
            },
        })) => {
            Some("warning: --api-key is visible in the process list; use --api-key-stdin instead")
        }
        Some(Commands::Login(_) | Commands::Account(_))
        | Some(Commands::Auth(AuthArgs {
            command: AuthCommand::PrintApiKey { .. },
        })) => None,
        _ if cli.api_key.is_some() => Some(
            "warning: --api-key is visible in the process list; use `codewhale auth set --api-key-stdin` or the provider's API-key env var instead",
        ),
        _ => None,
    }
}

fn run() -> Result<()> {
    let argv: Vec<_> = std::env::args_os().collect();
    let matches = Cli::command().get_matches_from(&argv);
    let project_bundle_scope = config_command_targets_project(&matches);
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    preserve_exec_separator(&mut cli, &argv);
    capture_exec_startup_options(&mut cli)?;

    // The detached log proxy must not depend on user config parsing: its job
    // is to frame child output and publish a terminal receipt even when the
    // delegated command's own config is malformed.
    let (proxy, command) = split_lane_log_proxy_command(cli.command.take());
    if let Some(args) = proxy {
        return run_lane_log_proxy_command(args);
    }

    if !cli.overrides.is_empty() && matches!(command, Some(Commands::Auth(_))) {
        bail!("--set is not supported by auth commands; use a saved config");
    }
    if !cli.overrides.is_empty()
        && matches!(&command, Some(Commands::AppServer(args)) if !args.http && !args.mobile)
    {
        bail!(
            "--set is not supported by the legacy app-server transport; use app-server --http or a saved config"
        );
    }
    if !matches!(command, Some(Commands::Config(_))) {
        apply_runtime_set_overrides(&mut cli)?;
    }
    if let Some(warning) = retired_output_mode_warning(&cli) {
        eprintln!("{warning}");
    }
    if let Some(warning) = argv_secret_warning(&cli, command.as_ref()) {
        eprintln!("{warning}");
    }

    let pipe_api_key_handoff = matches!(
        &command,
        Some(Commands::Auth(AuthArgs {
            command: AuthCommand::PrintApiKey { .. }
        }))
    );
    if pipe_api_key_handoff {
        credential_handoff::prepare_stdout(io::stdout().is_terminal())?;
    }
    let runtime_provider = top_level_provider_override(cli.provider.as_deref(), command.as_ref())?;
    let uses_raw_tui_provider = cli.provider.is_some() && runtime_provider.is_none();
    let runtime_overrides = CliRuntimeOverrides {
        provider: runtime_provider,
        model: cli.model.clone(),
        api_key: cli.api_key.clone(),
        base_url: cli.base_url.clone(),
        auth_mode: None,
        log_level: cli.log_level.clone(),
        telemetry: cli.telemetry,
        approval_policy: cli.approval_policy.clone(),
        sandbox_mode: cli.sandbox_mode.clone(),
        yolo: Some(cli.yolo),
        verbosity: cli.verbosity.clone(),
    };
    if uses_raw_tui_provider
        && let Some((resolved_runtime, passthrough)) =
            prepare_raw_provider_tui_dispatch(&cli, command.as_ref(), &runtime_overrides)?
    {
        return run_tui_in_process(&cli, &resolved_runtime, passthrough);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config_path =
        config_store_path_for_dispatch(cli.config.clone(), project_bundle_scope, &cwd);
    let mut store = match ConfigStore::load(config_path) {
        Ok(store) => store,
        Err(error) => {
            if let Some(Commands::Doctor(args)) = command {
                // Only transport diagnostic options. The TUI reopens the
                // rejected source with its structural loader and owns the
                // redacted error formatter; no request can use this default.
                let diagnostic = ConfigToml::default().resolve_runtime_options(&runtime_overrides);
                return run_tui_in_process(&cli, &diagnostic, tui_args("doctor", args));
            }
            return Err(if pipe_api_key_handoff {
                anyhow!("unavailable credential")
            } else {
                error
            });
        }
    };
    // Root session flags only reach the TUI through the `None` branch below;
    // no subcommand handler reads them. Accepting them silently resumes
    // nothing -- `codewhale --resume abc exec "..."` would start a fresh
    // session while looking like it continued one.
    if command.is_some()
        && (cli.continue_session || cli.resume.is_some() || cli.session_id.is_some())
    {
        anyhow::bail!(
            "--continue/--resume/--session-id apply to the interactive session and \
             cannot be combined with a subcommand. Run them without a subcommand, or \
             use the subcommand's own flag (for example `codewhale exec --session-id <id>`)."
        );
    }
    // Only config inspection needs the store overlay. Runtime overrides use
    // the dedicated flags above and must never enter a store that another
    // command (or legacy credential migration) can save.
    if matches!(command, Some(Commands::Config(_))) {
        apply_per_run_overrides(&mut store, &cli.overrides)?;
    }

    match command {
        Some(Commands::Run(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, args.args)
        }
        Some(Commands::Doctor(args)) => {
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("doctor", args))
        }
        Some(Commands::Models(args)) => {
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("models", args))
        }
        Some(Commands::Speech(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("speech", args))
        }
        Some(Commands::Sessions(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("sessions", args))
        }
        Some(Commands::Receipts(args)) => {
            // Read-only: resolve the runtime without first-run setup side effects.
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("receipts", args))
        }
        Some(Commands::Resume(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_resume_command(&cli, &resolved_runtime, args)
        }
        Some(Commands::Rc(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            let mut passthrough = vec!["--remote-control".to_string()];
            passthrough.extend(args.args);
            run_tui_in_process(&cli, &resolved_runtime, passthrough)
        }
        Some(Commands::Fork(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("fork", args))
        }
        Some(Commands::Init(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("init", args))
        }
        Some(Commands::Install(args)) => {
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("install", args))
        }
        Some(Commands::Setup(args)) => {
            let resolved_runtime = if setup_is_status_report(&args) {
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides)
            } else {
                resolve_runtime_for_dispatch(&mut store, &runtime_overrides)
            };
            run_tui_in_process(&cli, &resolved_runtime, tui_args("setup", args))
        }
        Some(Commands::RemoteSetup(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, remote_setup_tui_args(args))
        }
        Some(Commands::Exec(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("exec", args))
        }
        Some(Commands::Fleet(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("fleet", args))
        }
        Some(Commands::WorkflowTool(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("workflow-tool", args))
        }
        Some(Commands::LaneLogProxy(_)) => unreachable!("lane log proxy dispatched above"),
        Some(Commands::Workflow(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            let config_path = store.path().to_path_buf();
            run_workflow_command(&cli, &resolved_runtime, &config_path, args)
        }
        Some(Commands::Lane(args)) => run_lane_command(args),
        Some(Commands::Review(args)) => {
            // CI path: a machine token authenticates as the account with no
            // local session and no browser. The account's own configured
            // provider then disambiguates a model that maps to several
            // configured routes, which review otherwise hard-errors on.
            let mut overrides = runtime_overrides.clone();
            if overrides.provider.is_none()
                && let Some(provider) = cloud::machine_review_provider()?
            {
                overrides.provider = Some(provider);
            }
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("review", args))
        }
        Some(Commands::Apply(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("apply", args))
        }
        Some(Commands::Eval(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("eval", args))
        }
        Some(Commands::SessionDiagnostics(args)) => {
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            run_tui_in_process(
                &cli,
                &resolved_runtime,
                tui_args("session-diagnostics", args),
            )
        }
        Some(Commands::Scorecard(args)) => {
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("scorecard", args))
        }
        Some(Commands::Mcp(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("mcp", args))
        }
        Some(Commands::Pet(args)) => {
            // The pet owner is a delegated Engine service, with no separate
            // process or argument owner. Keep its narrow command contract.
            let mut argv = vec!["pet".to_string()];
            argv.extend(args.args);
            let code = codewhale_tui::run(codewhale_tui::RuntimeOptions::default(), argv);
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            });
        }
        Some(Commands::Integrations(args)) => {
            // Integrations only need route *identity*. Do not recover or
            // export a stored credential just to plan/launch a third-party
            // harness: it resolves its own keys from its own environment.
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("integrations", args))
        }
        Some(Commands::Features(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_in_process(&cli, &resolved_runtime, tui_args("features", args))
        }
        Some(Commands::Serve(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            // `serve` starts a long-running runtime API listener; supervise the
            // delegated child so it is torn down with the dispatcher (#3259).
            run_tui_server_in_process(&cli, &resolved_runtime, tui_args("serve", args))
        }
        Some(Commands::Web(args)) => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_server_in_process(&cli, &resolved_runtime, web_serve_passthrough(&args))
        }
        Some(Commands::Login(args)) => {
            reject_legacy_login_provider_args(&args)?;
            cloud::reject_inline_api_key(cli.api_key.as_deref())?;
            cloud::run_account_login(
                args.no_open,
                args.timeout_seconds,
                cli.profile.as_deref(),
                &mut store,
            )
        }
        Some(Commands::Logout(args)) => {
            confirm_logout(args.yes)?;
            run_logout_command(&mut store, cli.profile.as_deref())
        }
        Some(Commands::Auth(args)) => match args.command {
            AuthCommand::PluginLogin { provider } => {
                let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
                run_tui_in_process(
                    &cli,
                    &resolved_runtime,
                    vec![
                        "auth".to_string(),
                        "plugin-login".to_string(),
                        "--provider".to_string(),
                        provider,
                    ],
                )
            }
            AuthCommand::PluginLogout { provider } => {
                let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
                run_tui_in_process(
                    &cli,
                    &resolved_runtime,
                    vec![
                        "auth".to_string(),
                        "plugin-logout".to_string(),
                        "--provider".to_string(),
                        provider,
                    ],
                )
            }
            AuthCommand::XaiDevice => {
                let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
                run_tui_in_process(
                    &cli,
                    &resolved_runtime,
                    vec!["auth".to_string(), "xai-device".to_string()],
                )
            }
            command @ (AuthCommand::Claude | AuthCommand::ClaudeRevoke) => {
                let route = if matches!(command, AuthCommand::Claude) {
                    "claude"
                } else {
                    "claude-revoke"
                };
                let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
                run_tui_in_process(
                    &cli,
                    &resolved_runtime,
                    vec!["auth".to_string(), route.to_string()],
                )
            }
            AuthCommand::Chatgpt => {
                let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
                run_tui_in_process(
                    &cli,
                    &resolved_runtime,
                    vec!["auth".to_string(), "chatgpt".to_string()],
                )
            }
            AuthCommand::ChatgptRevoke => {
                let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
                run_tui_in_process(
                    &cli,
                    &resolved_runtime,
                    vec!["auth".to_string(), "chatgpt-revoke".to_string()],
                )
            }
            AuthCommand::Orcarouter => {
                let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
                run_tui_in_process(
                    &cli,
                    &resolved_runtime,
                    vec!["auth".to_string(), "orcarouter".to_string()],
                )
            }
            command @ AuthCommand::Status {
                diagnostic: true, ..
            } => {
                // Like `doctor`, this is a read-only diagnostic. Starting a
                // telemetry session here would create
                // `$CODEWHALE_HOME/telemetry` before the report could truthfully
                // say the isolated home is missing.
                run_auth_command_with_runtime(&mut store, command, &runtime_overrides)
            }
            command => {
                let resolved_runtime =
                    resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
                let session = start_cli_telemetry(
                    &resolved_runtime,
                    Some(store.path().to_path_buf()),
                    Surface::Cli,
                );
                let outcome =
                    run_auth_command_with_runtime(&mut store, command, &runtime_overrides);
                finish_cli_telemetry(session, &outcome, cli.verbose);
                outcome
            }
        },
        Some(Commands::Account(args)) => {
            cloud::reject_inline_api_key(cli.api_key.as_deref())?;
            cloud::run(args, cli.profile.as_deref(), &mut store)
        }
        Some(Commands::Dispatch(args)) => dispatch::run(args),
        Some(Commands::McpServer) => {
            // Keep the CLI spelling, with the same tool and permission authority
            // as `serve --mcp`. The legacy child-server proxy is retired.
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_tui_server_in_process(
                &cli,
                &resolved_runtime,
                vec!["serve".to_string(), "--mcp".to_string()],
            )
        }
        Some(Commands::Config(args)) => {
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            let session = start_cli_telemetry(
                &resolved_runtime,
                Some(store.path().to_path_buf()),
                Surface::Cli,
            );
            let outcome = run_config_command(
                &mut store,
                args.command,
                project_bundle_scope,
                &cli.overrides,
            );
            finish_cli_telemetry(session, &outcome, cli.verbose);
            outcome
        }
        Some(Commands::Model(args)) => {
            // `model resolve` is a diagnostic: it must report the same route
            // the runtime would take, so it resolves through the same
            // read-only path `doctor` uses rather than looking only at flags.
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            run_model_command(
                &mut store,
                args.command,
                runtime_overrides.provider,
                &resolved_runtime,
            )
        }
        Some(Commands::Thread(args)) => {
            run_thread_command(&cli, &mut store, &runtime_overrides, args.command)
        }
        Some(Commands::Sandbox(args)) => run_sandbox_command(args.command),
        Some(Commands::AppServer(args)) => {
            // Every transport loads the same file: the subcommand's --config,
            // else the global one. The HTTP/mobile runtime API is delegated to
            // the `serve` path in the Engine library, which reads the captured
            // --config, and runtime options (provider/keyring) resolve from it
            // too, so bridge the choice there before resolving them.
            let config_path = app_server_config_path(&cli, &args);
            if config_path != cli.config {
                cli.config = config_path;
                store = ConfigStore::load(cli.config.clone())?;
            }
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            run_app_server_command(&cli, &resolved_runtime, args)
        }
        Some(Commands::Completion { shell }) => {
            let mut stdout = io::stdout();
            stdout.write_all(render_completion_script(shell).as_bytes())?;
            stdout.flush()?;
            Ok(())
        }
        Some(Commands::Metrics(args)) => run_metrics_command(args),
        Some(Commands::Update(args)) => {
            let resolved_runtime =
                resolve_runtime_for_diagnostic_dispatch(&store, &runtime_overrides);
            let session = start_cli_telemetry(
                &resolved_runtime,
                Some(store.path().to_path_buf()),
                Surface::Cli,
            );
            #[cfg(not(target_env = "ohos"))]
            let outcome = update::run_update(args.beta, args.check, args.proxy);
            #[cfg(target_env = "ohos")]
            let outcome = {
                let _ = args;
                Err(anyhow!(
                    "self-update is not supported on HarmonyOS/OpenHarmony yet"
                ))
            };
            finish_cli_telemetry(session, &outcome, cli.verbose);
            outcome
        }
        Some(Commands::Providers(args)) => run_providers_command(args),
        None => {
            let resolved_runtime = resolve_runtime_for_dispatch(&mut store, &runtime_overrides);
            let forwarded = root_tui_passthrough(&cli)?;
            run_tui_in_process(&cli, &resolved_runtime, forwarded)
        }
    }
}

fn root_tui_passthrough(cli: &Cli) -> Result<Vec<String>> {
    let mut forwarded = Vec::new();
    if cli.continue_session {
        forwarded.push("--continue".to_string());
    }
    let resume_session_id = cli
        .resume
        .as_deref()
        .or(cli.session_id.as_deref())
        .map(str::trim);
    if resume_session_id.is_some_and(str::is_empty) {
        // A shell expanding an unset variable -- `codewhale --resume
        // "$SESSION_ID"` -- must not quietly become a fresh session. The user
        // asked to resume; starting new loses the session they meant, and the
        // mistake is invisible until the history is gone.
        bail!(
            "--resume/--session-id needs a session id, but got an empty value \
             (an unset shell variable?). Use `codewhale --continue` to resume \
             the most recent session."
        );
    }
    if let Some(session_id) = resume_session_id {
        forwarded.push("--resume".to_string());
        forwarded.push(session_id.to_string());
    }

    let prompt =
        cli.prompt_flag
            .iter()
            .chain(cli.prompt.iter())
            .fold(String::new(), |mut acc, part| {
                if !acc.is_empty() {
                    acc.push(' ');
                }
                acc.push_str(part);
                acc
            });
    if !prompt.is_empty() {
        if cli.continue_session {
            bail!(
                "`codewhale --continue` resumes the interactive TUI. Use `codewhale exec --continue <PROMPT>` to continue a session non-interactively."
            );
        }
        if let Some(session_id) = resume_session_id {
            bail!(
                "`codewhale --resume {session_id}` resumes the interactive TUI. Use `codewhale exec --resume {session_id} <PROMPT>` to continue a session non-interactively."
            );
        }
        forwarded.push("--prompt".to_string());
        forwarded.push(prompt);
    }

    Ok(forwarded)
}

fn resolve_runtime_for_dispatch(
    store: &mut ConfigStore,
    runtime_overrides: &CliRuntimeOverrides,
) -> ResolvedRuntimeOptions {
    let runtime_secrets = Secrets::auto_detect();
    resolve_runtime_for_dispatch_with_secrets(store, runtime_overrides, &runtime_secrets)
}

/// Resolve enough routing state to delegate a static diagnostic without
/// reading or migrating the durable secret store.
///
/// The TUI's doctor/setup-status path performs its own read-only source check,
/// so this dispatcher must not recover and export a credential merely to start
/// that report. Regular runtime and authentication commands keep using
/// [`resolve_runtime_for_dispatch`].
fn resolve_runtime_for_diagnostic_dispatch(
    store: &ConfigStore,
    runtime_overrides: &CliRuntimeOverrides,
) -> ResolvedRuntimeOptions {
    store.config.resolve_runtime_options(runtime_overrides)
}

/// An armed telemetry session belonging to a subcommand that runs *in this
/// process*.
///
/// Existing at all is the permission: it is only ever constructed behind
/// [`TelemetryDecision::Enabled`] after persistent and run-scoped opt-outs are
/// applied.
struct CliTelemetrySession {
    started: std::time::Instant,
}

/// Arm telemetry for a subcommand the dispatcher executes itself.
///
/// Only the terminal branches take this path. Everything that delegates to the
/// TUI binary is armed over there, under its own surface, from the environment
/// this dispatcher forwards — naming a surface here for a delegated command
/// would report one run twice under two identities.
///
/// Persistent config and setup-state opt-outs are applied inside
/// [`telemetry::decide`].
fn start_cli_telemetry(
    resolved: &ResolvedRuntimeOptions,
    config_path: Option<PathBuf>,
    surface: Surface,
) -> Option<CliTelemetrySession> {
    let consent = resolve_cli_telemetry_consent(
        resolved,
        config_path,
        surface,
        telemetry::load_setup_state_for_decision(),
    )?;
    telemetry::init(consent);
    telemetry::record(Event::SessionStart {
        source: SessionSource::Unknown,
    });
    Some(CliTelemetrySession {
        started: std::time::Instant::now(),
    })
}

fn resolve_cli_telemetry_consent(
    resolved: &ResolvedRuntimeOptions,
    config_path: Option<PathBuf>,
    surface: Surface,
    setup: Option<SetupState>,
) -> Option<telemetry::TelemetryConsent> {
    let setup = setup?;
    let TelemetryDecision::Enabled(consent) = telemetry::decide(resolved, &setup, surface) else {
        return None;
    };
    Some(consent.with_config_path(config_path))
}

/// Close the short CLI session and seal its events to the local buffer, bounded.
///
/// The exit class comes from what actually happened, never from an exit code:
/// a cancelled run and a SIGINT both exit 130, so a code-derived class would
/// mislabel every cancel as a signal.
///
/// Local persistence re-resolves consent from disk, so a setting changed by
/// this command takes effect immediately. Configured endpoints send the sealed
/// events during a later interactive shutdown rather than this short command.
fn finish_cli_telemetry(session: Option<CliTelemetrySession>, outcome: &Result<()>, verbose: bool) {
    let Some(session) = session else {
        return;
    };
    telemetry::set_exit_class(if outcome.is_ok() {
        ExitClass::Clean
    } else {
        ExitClass::Error
    });
    telemetry::record(Event::SessionEnd {
        duration_bucket: DurationBucket::from_secs(session.started.elapsed().as_secs()),
        exit_class: telemetry::exit_class(),
        // Cold start is measured by the TUI's startup trace. This surface has
        // no equivalent, and inventing one from process start would be a
        // different measurement wearing the same name.
        cold_start_bucket: None,
        providers: Vec::new(),
        counters: Counters::default(),
        errors: Errors::default(),
        turn_wall: TurnWall::default(),
    });
    let persistence = telemetry::persist_local_blocking();
    if verbose {
        eprintln!("telemetry local persistence outcome={persistence:?}");
    }
}

fn resolve_runtime_for_dispatch_with_secrets(
    store: &mut ConfigStore,
    runtime_overrides: &CliRuntimeOverrides,
    secrets: &Secrets,
) -> ResolvedRuntimeOptions {
    store
        .config
        .resolve_runtime_options_with_secrets(runtime_overrides, secrets)
}

fn tui_args(command: &str, args: TuiPassthroughArgs) -> Vec<String> {
    let mut forwarded = Vec::with_capacity(args.args.len() + 1);
    forwarded.push(command.to_string());
    forwarded.extend(args.args);
    forwarded
}

fn setup_is_status_report(args: &TuiPassthroughArgs) -> bool {
    args.args.iter().any(|arg| arg == "--status")
}

/// Clap consumes the escape immediately after the subcommand. Restore it
/// from the exact argv suffix before interpreting forwarded startup options.
fn preserve_exec_separator(cli: &mut Cli, argv: &[impl AsRef<std::ffi::OsStr>]) {
    let Some(Commands::Exec(args)) = cli.command.as_mut() else {
        return;
    };
    let Some(start) = argv.len().checked_sub(args.args.len() + 2) else {
        return;
    };
    if argv[start].as_ref() == "exec"
        && argv[start + 1].as_ref() == "--"
        && argv[start + 2..]
            .iter()
            .zip(&args.args)
            .all(|(raw, parsed)| raw.as_ref() == std::ffi::OsStr::new(parsed))
    {
        args.args.insert(0, "--".to_string());
    }
}

/// Admit exec's recognized startup options through the same Clap definitions
/// before the one runtime override capture. Unknown forwarded options and all
/// tokens after `--` retain their order and their existing exec meaning.
fn capture_exec_startup_options(cli: &mut Cli) -> Result<()> {
    let Some(Commands::Exec(args)) = cli.command.as_ref() else {
        return Ok(());
    };
    let mut startup = vec!["codewhale".to_string()];
    let mut forwarded = Vec::with_capacity(args.args.len());
    let mut seen = std::collections::HashSet::new();
    let mut args = args.args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            forwarded.push(arg.clone());
            forwarded.extend(args.cloned());
            break;
        }
        let (flag, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(flag, value)| (flag, Some(value)));
        let already_captured = match flag {
            "--provider" => cli.provider.is_some(),
            "--model" => cli.model.is_some(),
            "--api-key" => cli.api_key.is_some(),
            "--base-url" => cli.base_url.is_some(),
            "--config" => cli.config.is_some(),
            "--profile" => cli.profile.is_some(),
            _ => {
                forwarded.push(arg.clone());
                continue;
            }
        };
        if already_captured || !seen.insert(flag) {
            bail!("{flag} may be supplied only once, before or after `exec`");
        }
        startup.push(arg.clone());
        if inline.is_none() {
            let Some(value) = args.next() else {
                bail!("{flag} requires a value");
            };
            startup.push(value.clone());
        }
    }
    // Reuse the canonical parsers, including native config paths and exact
    // configured provider identifiers. This does not resolve a second route.
    let captured = Cli::try_parse_from(startup)?;
    cli.provider = cli.provider.take().or(captured.provider);
    cli.model = cli.model.take().or(captured.model);
    cli.api_key = cli.api_key.take().or(captured.api_key);
    cli.base_url = cli.base_url.take().or(captured.base_url);
    cli.config = cli.config.take().or(captured.runtime_options.config);
    cli.profile = cli.profile.take().or(captured.runtime_options.profile);
    let Some(Commands::Exec(args)) = cli.command.as_mut() else {
        unreachable!("only exec startup options were captured");
    };
    args.args = forwarded;
    Ok(())
}

/// `codewhale login` used to configure provider API keys; that surface moved
/// to `auth set --provider`. The hidden legacy flags stay parseable so the
/// redirect below can name the replacement instead of an unknown-flag error.
fn reject_legacy_login_provider_args(args: &LoginArgs) -> Result<()> {
    if args.api_key.is_none() && args.provider.is_none() {
        return Ok(());
    }
    bail!(
        "`codewhale login` now signs in to your Codewhale account via the browser device flow. \
         To configure a provider key, run `codewhale auth set --provider <provider>` (hidden prompt) \
         or `codewhale auth set --provider <provider> --api-key-stdin`."
    )
}

const LOGOUT_CONFIRM_PROMPT: &str = "This deletes every saved provider API key and OAuth login, \
the Codewhale account session. Type 'yes' to log out: ";

/// `codewhale logout` wipes every provider credential at once, so it must not
/// run on a stray keystroke. Non-interactive callers opt in with `--yes`.
fn confirm_logout(yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        bail!(
            "logout would delete every saved provider key; nothing was deleted. \
             Re-run with --yes to confirm non-interactively."
        );
    }
    confirm_logout_answer(&mut io::stdin().lock(), &mut io::stderr().lock())
}

fn confirm_logout_answer(reader: &mut impl io::BufRead, writer: &mut impl io::Write) -> Result<()> {
    write!(writer, "{LOGOUT_CONFIRM_PROMPT}")?;
    writer.flush()?;
    let mut answer = String::new();
    reader
        .read_line(&mut answer)
        .context("reading logout confirmation")?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "yes" | "y") {
        bail!("logout cancelled; no credentials were deleted");
    }
    Ok(())
}

fn run_logout_command(store: &mut ConfigStore, profile: Option<&str>) -> Result<()> {
    run_logout_command_with_secrets(store, &Secrets::auto_detect(), profile)
}

fn run_logout_command_with_secrets(
    store: &mut ConfigStore,
    secrets: &Secrets,
    profile: Option<&str>,
) -> Result<()> {
    let failures = codewhale_config::with_xai_oauth_revocation_transaction(|| {
        run_logout_command_with_secrets_unlocked(store, secrets, profile)
    })?;
    if !failures.is_empty() {
        anyhow::bail!(
            "logout incomplete: failed to delete stored credentials for: {}",
            failures.join(", ")
        );
    }
    println!("logged out");
    Ok(())
}

fn run_logout_command_with_secrets_unlocked(
    store: &mut ConfigStore,
    secrets: &Secrets,
    profile: Option<&str>,
) -> Result<Vec<String>> {
    let original_config = store.config.clone();
    for provider in ProviderKind::ALL {
        clear_provider_api_key_from_config(store, provider);
        store
            .config
            .providers
            .for_provider_mut(provider)
            .external_credentials = None;
    }
    let xai = store.config.providers.for_provider_mut(ProviderKind::Xai);
    xai.oauth_credential_generation = None;
    xai.auth_mode = None;
    let anthropic = store
        .config
        .providers
        .for_provider_mut(ProviderKind::Anthropic);
    anthropic.oauth_credential_generation = None;
    if anthropic.auth_mode.as_deref() == Some("oauth") {
        anthropic.auth_mode = None;
    }
    let openai_codex = store
        .config
        .providers
        .for_provider_mut(ProviderKind::OpenaiCodex);
    if openai_codex
        .oauth_credential_generation
        .as_deref()
        .is_some_and(codewhale_config::is_valid_chatgpt_oauth_generation)
    {
        openai_codex.oauth_credential_generation = None;
        if openai_codex.auth_mode.as_deref() == Some("oauth") {
            openai_codex.auth_mode = None;
        }
    }
    store.config.auth_mode = None;
    if let Err(error) = store.save() {
        store.config = original_config;
        return Err(error);
    }
    let mut keyring_failures = clear_all_provider_api_keys_from_keyring(secrets);
    // Already inside with_xai_oauth_revocation_transaction: the locked
    // variant must not re-enter the non-reentrant lifecycle mutex.
    if let Err(error) = codewhale_config::clear_all_claude_oauth_credentials_locked() {
        keyring_failures.push(format!("Claude sign-in: {error}"));
    }
    if let Err(error) = codewhale_config::clear_all_chatgpt_oauth_credentials_locked() {
        keyring_failures.push(format!("chatgpt oauth: {error}"));
    }
    if let Err(error) = clear_daytona_slot(secrets) {
        keyring_failures.push(format!(
            "{}: {error}",
            codewhale_secrets::DAYTONA_TOKEN_SLOT
        ));
    }
    if let Err(error) = clear_account_session(profile) {
        keyring_failures.push(format!("account session: {error}"));
    }
    // The config save committed the authority change. Partial deletions must
    // not roll back xAI revocation; report them after its transaction commits.
    Ok(keyring_failures)
}

fn clear_daytona_slot(secrets: &Secrets) -> Result<(), codewhale_secrets::SecretsError> {
    if secrets
        .get(codewhale_secrets::DAYTONA_TOKEN_SLOT)?
        .is_some_and(|value| !value.trim().is_empty())
    {
        secrets.delete(codewhale_secrets::DAYTONA_TOKEN_SLOT)?;
    }
    Ok(())
}

fn clear_account_session(profile: Option<&str>) -> Result<(), String> {
    use codewhale_secrets::account::{
        ACCOUNT_API_BASE_ENV, AccountSessionStore, DEFAULT_ACCOUNT_API_BASE,
        secure_account_session_secrets,
    };
    let secrets = secure_account_session_secrets().map_err(|error| error.to_string())?;
    let api_base = std::env::var(ACCOUNT_API_BASE_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_ACCOUNT_API_BASE.to_string());
    AccountSessionStore::new(secrets, profile, &api_base)
        .clear()
        .map_err(|error| error.to_string())
}

#[cfg(test)]
fn no_keyring_secrets() -> Secrets {
    Secrets::new(std::sync::Arc::new(
        codewhale_secrets::InMemoryKeyringStore::new(),
    ))
}

/// Clear one provider's credential. `Ok(Some(message))` means the config leg
/// was saved but the secret store kept the key; the caller must exit non-zero.
fn clear_auth_provider(
    store: &mut ConfigStore,
    secrets: &Secrets,
    provider: ProviderKind,
) -> Result<Option<String>> {
    if provider == ProviderKind::Antigravity {
        return clear_legacy_antigravity_config(store, secrets).map(|()| None);
    }
    let outcome = codewhale_config::credentials::clear_provider_api_key(store, secrets, provider)?;
    let slot = outcome.slot;
    // The secret-store leg used to fail silently here, which meant `auth clear`
    // could print success while the key was still in the keyring. The config
    // no longer advertises a key the backend may hold, but the credential is
    // not revoked, so this is a failure rather than a note on stdout.
    if let Some(error) = &outcome.secret_store_error {
        return Ok(Some(format!(
            "cleared API key for {slot} from config, but the secret store refused the delete: {error}; the key may still be stored there"
        )));
    }
    if provider == ProviderKind::Xai {
        println!("cleared xAI credentials from config, secret store, and owned OAuth storage");
    } else {
        println!("cleared API key for {slot} from config and secret store");
    }
    Ok(None)
}

/// Remove only Codewhale-owned state for the retired Antigravity route.
///
/// This deliberately operates on the already-loaded Codewhale config and its
/// own secret slot. It never resolves an external credential path, reads an
/// environment credential, or invokes a Google/Antigravity logout or revoke
/// flow.
fn clear_legacy_antigravity_config(store: &mut ConfigStore, secrets: &Secrets) -> Result<()> {
    let provider = ProviderKind::Antigravity;
    let slot = provider_slot(provider);
    let original_config = store.config.clone();
    let prior_secret = secrets.get(slot).map_err(|error| {
        anyhow!(
            "could not snapshot the Codewhale-owned legacy {slot} secret slot before clearing it: {error}; config was not changed"
        )
    })?;

    store.config.providers.antigravity = Default::default();
    store
        .config
        .fallback_providers
        .retain(|fallback| *fallback != provider);
    if store.config.provider == provider {
        store.config.provider = ProviderKind::default();
        store.config.selected_provider_id = None;
    }

    if let Err(error) = secrets.delete(slot) {
        store.config = original_config;
        return Err(anyhow!(
            "could not clear the Codewhale-owned legacy {slot} secret slot: {error}; config was not changed"
        ));
    }

    if let Err(error) = store.save() {
        store.config = original_config;
        if let Some(previous) = prior_secret {
            let current = secrets.get(slot).map_err(|rollback| {
                anyhow!(
                    "{error}; additionally could not verify rollback of the Codewhale-owned legacy {slot} secret slot: {rollback}"
                )
            })?;
            match current {
                None => secrets.set(slot, &previous).map_err(|rollback| {
                    anyhow!(
                        "{error}; additionally failed to restore the Codewhale-owned legacy {slot} secret slot: {rollback}"
                    )
                })?,
                Some(current) if current == previous => {}
                Some(_) => {
                    return Err(anyhow!(
                        "{error}; additionally the Codewhale-owned legacy {slot} secret slot changed concurrently and was not overwritten during rollback"
                    ));
                }
            }
        }
        return Err(error);
    }

    codewhale_config::scrub_plaintext_api_keys_from_config_backup(store.path())?;
    codewhale_config::scrub_legacy_antigravity_from_config_backup(store.path())?;
    println!(
        "cleared Codewhale-owned legacy Antigravity config, consent, selection, fallback entries, and secret-store slot; Google and Antigravity sessions were not read, revoked, or changed. For Gemini, configure provider google and set GEMINI_API_KEY"
    );
    Ok(())
}

fn provider_env_set(provider: ProviderKind) -> bool {
    provider_env_value(provider).is_some()
}

fn provider_env_vars(provider: ProviderKind) -> &'static [&'static str] {
    provider.provider().env_vars()
}

fn provider_env_value(provider: ProviderKind) -> Option<(&'static str, String)> {
    provider_env_vars(provider).iter().find_map(|var| {
        std::env::var(var)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| (*var, value))
    })
}

fn openai_codex_auth_file_path() -> PathBuf {
    if let Ok(path) = std::env::var("OPENAI_CODEX_AUTH_FILE") {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            return codewhale_config::resolve_external_credential_path(&path).unwrap_or(path);
        }
    }

    let codex_home = std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".codex")
        });
    let path = codex_home.join("auth.json");
    codewhale_config::resolve_external_credential_path(&path).unwrap_or(path)
}

fn grok_auth_file_path() -> PathBuf {
    for key in ["GROK_AUTH_PATH", "XAI_AUTH_PATH"] {
        if let Ok(path) = std::env::var(key) {
            let path = PathBuf::from(path.trim());
            if !path.as_os_str().is_empty() {
                return codewhale_config::resolve_external_credential_path(&path).unwrap_or(path);
            }
        }
    }
    if let Ok(home) = std::env::var("GROK_HOME") {
        let home = PathBuf::from(home.trim());
        if !home.as_os_str().is_empty() {
            let path = home.join("auth.json");
            return codewhale_config::resolve_external_credential_path(&path).unwrap_or(path);
        }
    }
    let path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".grok")
        .join("auth.json");
    codewhale_config::resolve_external_credential_path(&path).unwrap_or(path)
}

fn external_credential_target(
    provider: ProviderKind,
    path_override: Option<PathBuf>,
) -> Result<(codewhale_config::ExternalCredentialSource, PathBuf)> {
    let (source, default_path) = match provider {
        ProviderKind::OpenaiCodex => (
            codewhale_config::ExternalCredentialSource::CodexCli,
            openai_codex_auth_file_path(),
        ),
        ProviderKind::Xai => (
            codewhale_config::ExternalCredentialSource::GrokCli,
            grok_auth_file_path(),
        ),
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic => (
            codewhale_config::ExternalCredentialSource::DshCli,
            codewhale_config::default_dsh_credentials_path(),
        ),
        ProviderKind::Moonshot => bail!(
            "Kimi is API-key-only in Codewhale. Create a key at https://platform.kimi.ai/console/api-keys; Kimi CLI OAuth import is unsupported."
        ),
        _ => bail!(
            "{} has no supported external CLI credential source",
            provider.as_str()
        ),
    };
    let path =
        codewhale_config::resolve_external_credential_path(path_override.unwrap_or(default_path))?;
    Ok((source, path))
}

fn provider_config_api_key(store: &ConfigStore, provider: ProviderKind) -> Option<&str> {
    let slot = store
        .config
        .providers
        .for_provider(provider)
        .api_key
        .as_deref();
    slot.filter(|value| classify_config_api_key_value(value) == ConfigApiKeyValueKind::Literal)
}

fn provider_config_set(store: &ConfigStore, provider: ProviderKind) -> bool {
    provider_config_api_key(store, provider).is_some()
}

fn provider_keyring_api_key(secrets: &Secrets, provider: ProviderKind) -> Option<String> {
    secrets
        .get(provider_slot(provider))
        .ok()
        .flatten()
        .filter(|v| !v.trim().is_empty())
}

fn provider_keyring_set(secrets: &Secrets, provider: ProviderKind) -> bool {
    provider_keyring_api_key(secrets, provider).is_some()
}

/// Delete the keyring credential of every provider that may have one stored.
///
/// Returns a human-readable entry per slot whose deletion failed, so the
/// caller can report the failure instead of claiming a clean logout while
/// credentials linger in the keyring. Slots shared by several providers
/// (e.g. the historical `siliconflow` slot) are deleted once. Only a slot the
/// store reports empty is skipped: an unreadable slot may still hold a key,
/// so its delete is attempted, and a refused delete is a failure unless the
/// store then reports the slot empty (the rule `auth clear` applies too).
fn clear_all_provider_api_keys_from_keyring(secrets: &Secrets) -> Vec<String> {
    let mut failures = Vec::new();
    let mut cleared_slots = std::collections::HashSet::new();
    for provider in ProviderKind::ALL {
        let slot = provider_slot(provider);
        if !cleared_slots.insert(slot) {
            continue;
        }
        if matches!(secrets.get(slot), Ok(None)) {
            continue;
        }
        if let Err(error) = secrets.delete(slot)
            && !matches!(secrets.get(slot), Ok(None))
        {
            failures.push(format!("{slot}: {error}"));
        }
    }
    failures
}

fn external_consent(
    store: &ConfigStore,
    provider: ProviderKind,
) -> Option<&codewhale_config::ExternalCredentialConsentToml> {
    store
        .config
        .providers
        .for_provider(provider)
        .external_credentials
        .as_ref()
}

fn external_read_consent(
    store: &ConfigStore,
    provider: ProviderKind,
) -> Option<&codewhale_config::ExternalCredentialConsentToml> {
    let (source, expected_path) = external_credential_target(provider, None).ok()?;
    external_consent(store, provider)
        .filter(|consent| consent.read_grant(provider, source, &expected_path).is_ok())
}

fn external_oauth_selected(store: &ConfigStore, provider: ProviderKind) -> bool {
    if external_read_consent(store, provider).is_none() {
        return false;
    }
    if provider == ProviderKind::OpenaiCodex {
        return true;
    }
    provider == ProviderKind::Xai
        && xai_oauth_mode_selected(store.config.providers.xai.auth_mode.as_deref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XaiOAuthGenerationPointer {
    Absent,
    Valid,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XaiAuthDiagnosticRoute {
    /// Normal API-key diagnostics apply. This includes custom endpoints, where
    /// xAI OAuth is intentionally inactive.
    ApiKey,
    /// A syntactically valid Codewhale-owned generation pointer selects the
    /// owned OAuth route. Diagnostics deliberately do not inspect the file.
    OwnedOAuth,
    /// A configured but unsafe/malformed generation pointer blocks external
    /// Grok CLI access. The runtime can still fall back to API-key sources.
    NeedsRepair,
    /// With no configured generation, an exact read-only Grok CLI consent can
    /// be selected structurally. The external file is never probed here.
    ExternalConsent,
}

#[derive(Debug, Clone)]
struct XaiAuthDiagnostics {
    base_url: String,
    official_endpoint: bool,
    auth_mode: Option<String>,
    oauth_selected: bool,
    generation: XaiOAuthGenerationPointer,
    route: XaiAuthDiagnosticRoute,
}

impl XaiAuthDiagnostics {
    /// API-key routes are reported from the same endpoint-bound resolver that
    /// dispatch uses. Owned OAuth and consent-only routes remain structural so
    /// diagnostics cannot turn into a credential-store probe.
    fn evaluates_runtime_api_key(&self) -> bool {
        matches!(
            self.route,
            XaiAuthDiagnosticRoute::ApiKey | XaiAuthDiagnosticRoute::NeedsRepair
        )
    }

    fn is_custom_endpoint(&self) -> bool {
        !self.official_endpoint
    }
}

/// Source and redacted tail from the shared runtime resolver. Keeping only a
/// redacted tail prevents the presentation layer from accidentally retaining a
/// plaintext credential after it has derived the effective route.
#[derive(Debug, Clone, Default)]
struct RuntimeAuthApiKey {
    source: Option<RuntimeApiKeySource>,
    last4: Option<String>,
}

impl RuntimeAuthApiKey {
    fn source_name(&self) -> Option<&'static str> {
        match self.source {
            Some(RuntimeApiKeySource::Cli) => Some("cli"),
            Some(RuntimeApiKeySource::ConfigFile) => Some("config"),
            Some(RuntimeApiKeySource::Keyring) => Some("secret store"),
            Some(RuntimeApiKeySource::Env) => Some("env"),
            None => None,
        }
    }

    fn source_with_last4(&self) -> Option<String> {
        self.source_name()
            .map(|source| match self.last4.as_deref() {
                Some(last4) => format!("{source} (last4: {last4})"),
                None => source.to_string(),
            })
    }

    fn uses(&self, source: RuntimeApiKeySource) -> bool {
        self.source == Some(source)
    }
}

fn runtime_overrides_for_provider(
    runtime_overrides: &CliRuntimeOverrides,
    provider: ProviderKind,
) -> CliRuntimeOverrides {
    let mut overrides = runtime_overrides.clone();
    overrides.provider = Some(provider);
    overrides
}

fn xai_oauth_mode_selected(auth_mode: Option<&str>) -> bool {
    auth_mode.is_some_and(|mode| {
        matches!(
            mode.trim()
                .to_ascii_lowercase()
                .replace(['-', ' '], "_")
                .as_str(),
            "oauth"
                | "xai_oauth"
                | "xai"
                | "grok"
                | "grok_oauth"
                | "grok_cli"
                | "device"
                | "device_code"
                | "device_auth"
        )
    })
}

fn xai_oauth_generation_pointer(store: &ConfigStore) -> XaiOAuthGenerationPointer {
    match store
        .config
        .providers
        .xai
        .oauth_credential_generation
        .as_deref()
    {
        None => XaiOAuthGenerationPointer::Absent,
        Some(generation) if codewhale_config::is_valid_xai_oauth_generation(generation) => {
            XaiOAuthGenerationPointer::Valid
        }
        Some(_) => XaiOAuthGenerationPointer::Invalid,
    }
}

/// Resolve the same xAI route facts the runtime uses, without asking the
/// durable credential store for a secret. `ConfigToml::resolve_runtime_options`
/// deliberately uses an in-memory store, so this is safe for diagnostic output
/// that must remain structural/non-probing.
fn xai_auth_diagnostics(
    store: &ConfigStore,
    runtime_overrides: &CliRuntimeOverrides,
) -> XaiAuthDiagnostics {
    // We only need the effective endpoint here. Suppressing API-key
    // resolution keeps valid-owned and consent-only diagnostics structural:
    // they must not read ambient credential state merely to describe a route.
    let mut route_overrides = runtime_overrides_for_provider(runtime_overrides, ProviderKind::Xai);
    route_overrides.api_key = None;
    route_overrides.auth_mode = Some("none".to_string());
    let resolved = store.config.resolve_runtime_options(&route_overrides);
    let official_endpoint =
        provider_base_url_is_official(ProviderKind::Xai, resolved.base_url.as_str());
    // The TUI activates xAI OAuth only from `[providers.xai] auth_mode`; a
    // root-level auth mode may influence generic API-key policy but must never
    // turn an inert xAI generation pointer into an OAuth route.
    let auth_mode = store.config.providers.xai.auth_mode.clone();
    let generation = xai_oauth_generation_pointer(store);
    let oauth_selected = xai_oauth_mode_selected(auth_mode.as_deref());
    let route = if !official_endpoint || !oauth_selected {
        XaiAuthDiagnosticRoute::ApiKey
    } else {
        match generation {
            XaiOAuthGenerationPointer::Valid => XaiAuthDiagnosticRoute::OwnedOAuth,
            XaiOAuthGenerationPointer::Invalid => XaiAuthDiagnosticRoute::NeedsRepair,
            XaiOAuthGenerationPointer::Absent
                if external_read_consent(store, ProviderKind::Xai).is_some() =>
            {
                XaiAuthDiagnosticRoute::ExternalConsent
            }
            XaiOAuthGenerationPointer::Absent => XaiAuthDiagnosticRoute::ApiKey,
        }
    };

    XaiAuthDiagnostics {
        base_url: resolved.base_url,
        official_endpoint,
        auth_mode,
        oauth_selected,
        generation,
        route,
    }
}

/// Return the API-key route exactly as the dispatcher would resolve it. This
/// is the critical distinction for a global `--base-url` or `XAI_BASE_URL`:
/// official-provider config, keyring, and ambient keys must not cross onto an
/// unrelated custom endpoint.
fn xai_runtime_api_key(
    store: &ConfigStore,
    secrets: &Secrets,
    runtime_overrides: &CliRuntimeOverrides,
) -> RuntimeAuthApiKey {
    let resolved = store.config.resolve_runtime_options_with_secrets(
        &runtime_overrides_for_provider(runtime_overrides, ProviderKind::Xai),
        secrets,
    );
    debug_assert_eq!(resolved.provider, ProviderKind::Xai);
    RuntimeAuthApiKey {
        source: resolved.api_key_source,
        last4: resolved.api_key.as_deref().map(last4_label),
    }
}

fn api_key_source_name(
    config_key: Option<&str>,
    keyring_key: Option<&str>,
    env_key: Option<&(&'static str, String)>,
) -> Option<&'static str> {
    if config_key.is_some() {
        Some("config")
    } else if keyring_key.is_some() {
        Some("secret store")
    } else if env_key.is_some() {
        Some("env")
    } else {
        None
    }
}

fn xai_status_summary_source(
    diagnostics: &XaiAuthDiagnostics,
    api_key: Option<&RuntimeAuthApiKey>,
) -> String {
    match diagnostics.route {
        XaiAuthDiagnosticRoute::OwnedOAuth => {
            "Codewhale-owned OAuth configured/unprobed (valid generation pointer)".to_string()
        }
        XaiAuthDiagnosticRoute::NeedsRepair => {
            let api_key = api_key
                .and_then(RuntimeAuthApiKey::source_name)
                .unwrap_or("no runtime-effective API key");
            format!("needs repair (invalid OAuth generation pointer; API-key fallback: {api_key})")
        }
        XaiAuthDiagnosticRoute::ExternalConsent => {
            "external consent configured/unprobed".to_string()
        }
        XaiAuthDiagnosticRoute::ApiKey => api_key
            .and_then(RuntimeAuthApiKey::source_name)
            .unwrap_or("unset")
            .to_string(),
    }
}

fn xai_credential_route_label(
    diagnostics: &XaiAuthDiagnostics,
    api_key: Option<&RuntimeAuthApiKey>,
) -> String {
    match diagnostics.route {
        XaiAuthDiagnosticRoute::OwnedOAuth => {
            "Codewhale-owned OAuth configured/unprobed (valid generation pointer; availability unprobed)"
                .to_string()
        }
        XaiAuthDiagnosticRoute::NeedsRepair => {
            let api_key = api_key
                .and_then(RuntimeAuthApiKey::source_with_last4)
                .unwrap_or_else(|| "no runtime-effective API key".to_string());
            format!(
                "xAI OAuth needs repair (invalid Codewhale-owned generation pointer; Grok CLI consent blocked; API-key fallback: {api_key})"
            )
        }
        XaiAuthDiagnosticRoute::ExternalConsent => {
            "external read-only consent configured/unprobed".to_string()
        }
        XaiAuthDiagnosticRoute::ApiKey => api_key
            .and_then(RuntimeAuthApiKey::source_with_last4)
            .unwrap_or_else(|| "missing".to_string()),
    }
}

fn xai_table_storage_status(
    api_key: Option<&RuntimeAuthApiKey>,
    source: RuntimeApiKeySource,
) -> &'static str {
    match api_key {
        Some(api_key) if api_key.uses(source) => "set",
        Some(_) => "-",
        // The selected structural OAuth/consent route intentionally does not
        // establish whether any API-key storage is populated.
        None => "unprobed",
    }
}

fn xai_list_storage_status(
    api_key: Option<&RuntimeAuthApiKey>,
    source: RuntimeApiKeySource,
) -> &'static str {
    match api_key {
        Some(api_key) if api_key.uses(source) => "yes",
        Some(_) => "no",
        None => "?",
    }
}

fn xai_list_route(
    diagnostics: &XaiAuthDiagnostics,
    api_key: Option<&RuntimeAuthApiKey>,
) -> &'static str {
    match diagnostics.route {
        XaiAuthDiagnosticRoute::OwnedOAuth => "owned-oauth-configured",
        XaiAuthDiagnosticRoute::NeedsRepair => "needs-repair",
        XaiAuthDiagnosticRoute::ExternalConsent => "external-consent-configured",
        XaiAuthDiagnosticRoute::ApiKey => match api_key.and_then(|api_key| api_key.source) {
            Some(RuntimeApiKeySource::Cli) => "cli",
            Some(RuntimeApiKeySource::ConfigFile) => "config",
            Some(RuntimeApiKeySource::Keyring) => "store",
            Some(RuntimeApiKeySource::Env) => "env",
            None => "missing",
        },
    }
}

fn xai_storage_detail(
    diagnostics: &XaiAuthDiagnostics,
    api_key: Option<&RuntimeAuthApiKey>,
    source: RuntimeApiKeySource,
) -> String {
    match api_key {
        Some(api_key) if api_key.uses(source) => api_key
            .last4
            .as_deref()
            .map(|last4| format!("runtime-effective, last4: {last4}"))
            .unwrap_or_else(|| "runtime-effective".to_string()),
        Some(_) if diagnostics.is_custom_endpoint() => {
            "not eligible for this custom xAI endpoint".to_string()
        }
        Some(_) => "not selected by the runtime resolver".to_string(),
        None if diagnostics.evaluates_runtime_api_key() && diagnostics.is_custom_endpoint() => {
            "not eligible for this custom xAI endpoint".to_string()
        }
        None if diagnostics.evaluates_runtime_api_key() => {
            "not set for this runtime route".to_string()
        }
        None => "unprobed (structural OAuth/consent route)".to_string(),
    }
}

fn xai_lookup_order(diagnostics: &XaiAuthDiagnostics) -> String {
    match diagnostics.route {
        XaiAuthDiagnosticRoute::OwnedOAuth => {
            "lookup order: configured Codewhale-owned OAuth generation (availability unprobed); Grok CLI consent blocked".to_string()
        }
        XaiAuthDiagnosticRoute::NeedsRepair => {
            "lookup order: invalid Codewhale-owned OAuth generation blocks Grok CLI consent; runtime-effective API-key fallback: CLI -> config -> secret store -> env".to_string()
        }
        XaiAuthDiagnosticRoute::ExternalConsent => {
            "lookup order: configured consent-gated exact Grok CLI file (availability unprobed)".to_string()
        }
        XaiAuthDiagnosticRoute::ApiKey if diagnostics.is_custom_endpoint() => {
            "lookup order: endpoint-bound API key only for this custom xAI endpoint (explicit CLI key or route-bound config key)".to_string()
        }
        XaiAuthDiagnosticRoute::ApiKey => {
            "lookup order: CLI -> config -> secret store -> env".to_string()
        }
    }
}

fn xai_get_line(diagnostics: &XaiAuthDiagnostics, api_key: Option<&RuntimeAuthApiKey>) -> String {
    match diagnostics.route {
        XaiAuthDiagnosticRoute::OwnedOAuth => {
            "xai: configured (source: Codewhale-owned OAuth generation; valid pointer; token availability unprobed)".to_string()
        }
        XaiAuthDiagnosticRoute::NeedsRepair => {
            let api_key = match api_key.and_then(RuntimeAuthApiKey::source_name) {
                Some("config") => "config-file".to_string(),
                Some("secret store") => "secret-store".to_string(),
                Some("env") => "env".to_string(),
                Some("cli") => "cli".to_string(),
                Some(other) => other.to_string(),
                None => "no runtime-effective API key".to_string(),
            };
            format!(
                "xai: needs repair (invalid Codewhale-owned OAuth generation pointer; Grok CLI consent blocked; API-key fallback: {api_key})"
            )
        }
        XaiAuthDiagnosticRoute::ExternalConsent => {
            "xai: configured (source: external read-only consent; availability unprobed)".to_string()
        }
        XaiAuthDiagnosticRoute::ApiKey => match api_key.and_then(RuntimeAuthApiKey::source_name) {
                Some("config") => "xai: set (source: config-file)".to_string(),
                Some("secret store") => "xai: set (source: secret-store)".to_string(),
                Some("env") => "xai: set (source: env)".to_string(),
                Some("cli") => "xai: set (source: cli)".to_string(),
                Some(other) => format!("xai: set (source: {other})"),
                None => "xai: not set".to_string(),
            },
    }
}

/// Describe the selected ChatGPT route without refreshing credentials or
/// consulting ambient tokens, API-key storage, or external CLI files for the
/// official plan endpoint. Custom routes use the dispatcher's bound-key resolver.
struct ChatgptAuthDiagnostics {
    official_endpoint: bool,
    source: String,
    api_key: Option<RuntimeAuthApiKey>,
}

fn chatgpt_auth_diagnostics(
    store: &ConfigStore,
    secrets: &Secrets,
    runtime_overrides: &CliRuntimeOverrides,
) -> ChatgptAuthDiagnostics {
    let overrides = runtime_overrides_for_provider(runtime_overrides, ProviderKind::OpenaiCodex);
    let mut route_overrides = overrides.clone();
    route_overrides.api_key = None;
    route_overrides.auth_mode = Some("none".to_string());
    let resolved = store.config.resolve_runtime_options(&route_overrides);
    let official_endpoint = codewhale_tui::is_official_chatgpt_api_base(&resolved.base_url);
    let api_key = (!official_endpoint).then(|| {
        let resolved = store
            .config
            .resolve_runtime_options_with_secrets(&overrides, secrets);
        RuntimeAuthApiKey {
            source: resolved.api_key_source,
            last4: resolved.api_key.as_deref().map(last4_label),
        }
    });
    let source = if official_endpoint {
        if store.config.providers.openai_codex.auth_mode.as_deref() != Some("oauth") {
            "missing (run `codewhale auth chatgpt` to register an official grant)".to_string()
        } else {
            match owned_oauth_account(store, ProviderKind::OpenaiCodex) {
                Ok(Some(account)) => format!(
                    "Codewhale-owned ChatGPT sign-in as {account} (verified grant; no refresh or network)"
                ),
                Ok(None) => {
                    "Codewhale-owned ChatGPT sign-in (verified grant; no refresh or network)"
                        .to_string()
                }
                Err(reason) => format!(
                    "missing (Codewhale-owned ChatGPT sign-in is unusable: {reason}; run `codewhale auth chatgpt`)"
                ),
            }
        }
    } else {
        api_key
            .as_ref()
            .and_then(RuntimeAuthApiKey::source_with_last4)
            .unwrap_or_else(|| {
                "missing (custom endpoint requires an explicit or route-bound API key)".to_string()
            })
    };
    ChatgptAuthDiagnostics {
        official_endpoint,
        source,
        api_key,
    }
}

fn chatgpt_auth_status_lines(
    store: &ConfigStore,
    secrets: &Secrets,
    runtime_overrides: &CliRuntimeOverrides,
) -> Vec<String> {
    let diagnostics = chatgpt_auth_diagnostics(store, secrets, runtime_overrides);
    let marker = if store.config.provider == ProviderKind::OpenaiCodex {
        " (active provider)"
    } else {
        ""
    };
    let storage_detail = |source| {
        if diagnostics.official_endpoint {
            "inactive for official ChatGPT sign-in".to_string()
        } else if let Some(key) = diagnostics.api_key.as_ref().filter(|key| key.uses(source)) {
            key.last4
                .as_deref()
                .map(|tail| format!("runtime-effective, last4: {tail}"))
                .unwrap_or_else(|| "runtime-effective".to_string())
        } else {
            "not eligible or not selected for this custom endpoint".to_string()
        }
    };
    let mut lines = vec![
        format!("provider: openai-codex{marker}"),
        format!(
            "route: {}",
            if diagnostics.official_endpoint {
                "official ChatGPT plan API"
            } else {
                "custom API-key endpoint"
            }
        ),
        format!(
            "model: {}",
            store
                .config
                .providers
                .openai_codex
                .model
                .as_deref()
                .unwrap_or("(default)")
        ),
        format!(
            "auth mode: {}",
            if diagnostics.official_endpoint {
                "oauth"
            } else {
                "api_key"
            }
        ),
        format!("active source: {}", diagnostics.source),
        if diagnostics.official_endpoint {
            "lookup order: verified Codewhale-owned ChatGPT sign-in only; ambient tokens and external CLI credentials are inactive".to_string()
        } else {
            "lookup order: endpoint-bound API key only (explicit CLI key or route-bound config key)"
                .to_string()
        },
        format!(
            "config file: {} ({})",
            codewhale_config::quote_os_path(store.path()),
            storage_detail(RuntimeApiKeySource::ConfigFile)
        ),
        format!(
            "secret store: {} ({})",
            secrets.backend_name(),
            storage_detail(RuntimeApiKeySource::Keyring)
        ),
        format!(
            "env var: {} ({})",
            provider_env_vars(ProviderKind::OpenaiCodex).join("/"),
            storage_detail(RuntimeApiKeySource::Env)
        ),
        "external credentials: inactive for this route (no file was probed)".to_string(),
    ];
    if diagnostics.official_endpoint {
        lines.push("switch account: `CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt` (choose another account, then restart open Codewhale sessions); `/auth chatgpt` reauthorizes the selected account".to_string());
    }
    lines
}

fn auth_get_line_with_runtime(
    store: &ConfigStore,
    secrets: &Secrets,
    provider: ProviderKind,
    runtime_overrides: &CliRuntimeOverrides,
) -> String {
    let slot = provider_slot(provider);
    if provider == ProviderKind::Xai {
        let diagnostics = xai_auth_diagnostics(store, runtime_overrides);
        let api_key = diagnostics
            .evaluates_runtime_api_key()
            .then(|| xai_runtime_api_key(store, secrets, runtime_overrides));
        return xai_get_line(&diagnostics, api_key.as_ref());
    }

    if provider == ProviderKind::OpenaiCodex {
        let diagnostics = chatgpt_auth_diagnostics(store, secrets, runtime_overrides);
        return format!(
            "{slot}: {} (source: {})",
            if diagnostics.source.starts_with("missing") {
                "not set"
            } else {
                "configured"
            },
            diagnostics.source
        );
    }

    let config_key = provider_config_api_key(store, provider);
    let keyring_key = config_key
        .is_none()
        .then(|| provider_keyring_api_key(secrets, provider))
        .flatten();
    let env_key = provider_env_value(provider);

    match api_key_source_name(config_key, keyring_key.as_deref(), env_key.as_ref()) {
        Some("config") => format!("{slot}: set (source: config-file)"),
        Some("secret store") => format!("{slot}: set (source: secret-store)"),
        Some("env") => format!("{slot}: set (source: env)"),
        Some(other) => format!("{slot}: set (source: {other})"),
        None => format!("{slot}: not set"),
    }
}

#[cfg(test)]
fn auth_status_all_providers(store: &ConfigStore, secrets: &Secrets) -> Vec<String> {
    auth_status_all_providers_with_runtime(store, secrets, &CliRuntimeOverrides::default())
}

fn auth_status_all_providers_with_runtime(
    store: &ConfigStore,
    secrets: &Secrets,
    runtime_overrides: &CliRuntimeOverrides,
) -> Vec<String> {
    let active_provider = store.config.provider;
    let mut lines = Vec::new();
    lines.push(account_status_line());
    lines.push(String::new());
    lines.push(format!(
        "active provider: {} (set via config or CODEWHALE_PROVIDER)",
        active_provider.as_str()
    ));
    lines.push(String::new());
    lines.push(format!(
        "{:<14} {:<8} {:<10} {:<8} {}",
        "provider", "config", "keyring", "env", "status"
    ));
    lines.push("-".repeat(70));

    for provider in ProviderKind::ALL {
        if provider == ProviderKind::Xai {
            let diagnostics = xai_auth_diagnostics(store, runtime_overrides);
            let api_key = diagnostics
                .evaluates_runtime_api_key()
                .then(|| xai_runtime_api_key(store, secrets, runtime_overrides));
            let active_marker = if provider == active_provider {
                " *"
            } else {
                ""
            };
            lines.push(format!(
                "{:<14} {:<8} {:<10} {:<8} {}{}",
                provider.as_str(),
                xai_table_storage_status(api_key.as_ref(), RuntimeApiKeySource::ConfigFile),
                xai_table_storage_status(api_key.as_ref(), RuntimeApiKeySource::Keyring),
                xai_table_storage_status(api_key.as_ref(), RuntimeApiKeySource::Env),
                xai_status_summary_source(&diagnostics, api_key.as_ref()),
                active_marker
            ));
            continue;
        }

        if provider == ProviderKind::OpenaiCodex {
            let diagnostics = chatgpt_auth_diagnostics(store, secrets, runtime_overrides);
            let status = |source| {
                if diagnostics
                    .api_key
                    .as_ref()
                    .is_some_and(|key| key.uses(source))
                {
                    "set"
                } else {
                    "-"
                }
            };
            lines.push(format!(
                "{:<14} {:<8} {:<10} {:<8} {}{}",
                provider.as_str(),
                status(RuntimeApiKeySource::ConfigFile),
                status(RuntimeApiKeySource::Keyring),
                status(RuntimeApiKeySource::Env),
                diagnostics.source,
                if provider == active_provider {
                    " *"
                } else {
                    ""
                }
            ));
            continue;
        }

        let config_key = provider_config_api_key(store, provider);
        let keyring_key = provider_keyring_api_key(secrets, provider);
        let env_key = provider_env_value(provider);
        let external_selected = external_oauth_selected(store, provider);

        let config_status = config_key.map(|_| "set").unwrap_or("-");
        let keyring_status = keyring_key.as_ref().map(|_| "set").unwrap_or("-");
        let env_status = env_key.as_ref().map(|_| "set").unwrap_or("-");

        let source = if external_selected {
            "external consent (not probed)".to_string()
        } else if config_key.is_some() {
            "config".to_string()
        } else if keyring_key.is_some() {
            "keyring".to_string()
        } else if env_key.is_some() {
            "env".to_string()
        } else {
            "unset".to_string()
        };

        let active_marker = if provider == active_provider {
            " *"
        } else {
            ""
        };

        lines.push(format!(
            "{:<14} {:<8} {:<10} {:<8} {}{}",
            provider.as_str(),
            config_status,
            keyring_status,
            env_status,
            source,
            active_marker
        ));
    }

    lines.push(String::new());
    lines.push("* = active provider (from config or CODEWHALE_PROVIDER)".to_string());
    lines.push("Run `codewhale auth status --provider <id>` for detailed info.".to_string());
    lines.push("Account sign-in is `codewhale login`.".to_string());
    lines
}

fn account_status_line() -> String {
    use codewhale_secrets::account::{
        ACCOUNT_API_BASE_ENV, AccountSessionState, AccountSessionStore, DEFAULT_ACCOUNT_API_BASE,
        secure_account_session_secrets,
    };
    let api_base = std::env::var(ACCOUNT_API_BASE_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_ACCOUNT_API_BASE.to_string());
    match secure_account_session_secrets() {
        Ok(secrets) => {
            match AccountSessionStore::new(secrets, None, &api_base)
                .runtime_info_at(chrono::Utc::now())
            {
                Ok(info) => {
                    let state = match info.state {
                        AccountSessionState::SignedOut => "not signed in",
                        AccountSessionState::Authenticated => "signed in",
                        AccountSessionState::OfflineCached => "offline (cached)",
                        AccountSessionState::Expired => "expired",
                        AccountSessionState::Revoked => "revoked",
                    };
                    format!("account: {state} (api {api_base})")
                }
                Err(error) => format!("account: unavailable ({error})"),
            }
        }
        Err(error) => format!("account: unavailable ({error})"),
    }
}

fn diagnostic_path_state(path: &Path, directory: bool) -> &'static str {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => "present (symlink; not followed)",
        Ok(metadata) if directory && metadata.is_dir() => "present",
        Ok(metadata) if !directory && metadata.is_file() => "present",
        Ok(_) => "present (unexpected type)",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
        Err(_) => "unknown",
    }
}

const fn secret_backend_kind_label(
    kind: codewhale_secrets::SecretBackendDiagnosticKind,
) -> &'static str {
    match kind {
        codewhale_secrets::SecretBackendDiagnosticKind::File => "file",
        codewhale_secrets::SecretBackendDiagnosticKind::System => "system",
        codewhale_secrets::SecretBackendDiagnosticKind::Unknown => "unknown",
    }
}

const fn secret_backend_inspection_label(
    inspection: codewhale_secrets::SecretBackendInspection,
) -> &'static str {
    match inspection {
        codewhale_secrets::SecretBackendInspection::MetadataOnly => "metadata_only",
        codewhale_secrets::SecretBackendInspection::NotProbed => "not_probed",
    }
}

const fn secret_backend_presence_label(
    presence: codewhale_secrets::SecretBackendPresence,
) -> &'static str {
    match presence {
        codewhale_secrets::SecretBackendPresence::Present => "present",
        codewhale_secrets::SecretBackendPresence::Absent => "missing",
        codewhale_secrets::SecretBackendPresence::Unknown => "unknown",
    }
}

/// Value-free home and credential-source report for `auth status --diagnostic`.
///
/// Unlike ordinary `auth status`, this path never constructs [`Secrets`] and
/// never asks a provider keyring for a value. File presence comes from metadata
/// only; provider environment variables are checked with the runtime's
/// non-empty-string semantics and their contents are never formatted.
fn auth_diagnostic_lines(store: &ConfigStore, provider: Option<ProviderKind>) -> Vec<String> {
    let explicit_home = codewhale_paths::codewhale_home_is_explicit();
    let resolved_home = codewhale_paths::codewhale_home();
    let mut lines = vec![
        "auth diagnostic (structural only; credential values are never printed and provider credential stores were not opened)".to_string(),
        String::new(),
    ];

    let home = match resolved_home {
        Ok(Some(path)) => {
            lines.push(format!(
                "codewhale home: {} (source: {}; state: {})",
                codewhale_config::quote_os_path(&path),
                if explicit_home {
                    "CODEWHALE_HOME (isolated)"
                } else {
                    "platform home"
                },
                diagnostic_path_state(&path, true),
            ));
            Some(path)
        }
        Ok(None) => {
            lines.push("codewhale home: unavailable (no user home resolved)".to_string());
            None
        }
        Err(error) => {
            lines.push(format!("codewhale home: unavailable ({error})"));
            None
        }
    };

    lines.push(format!(
        "config: {} ({})",
        codewhale_config::quote_os_path(store.path()),
        diagnostic_path_state(store.path(), false),
    ));
    if let Some(home) = home.as_ref() {
        let settings = home.join("settings.toml");
        lines.push(format!(
            "settings: {} ({})",
            codewhale_config::quote_os_path(&settings),
            diagnostic_path_state(&settings, false),
        ));
    } else {
        lines.push("settings: unavailable (Codewhale home unresolved)".to_string());
    }

    let backend = codewhale_secrets::diagnose_secret_backend();
    lines.push(format!(
        "secret backend: {} (inspection: {})",
        secret_backend_kind_label(backend.backend),
        secret_backend_inspection_label(backend.inspection),
    ));
    if let Some(path) = backend.path.as_ref() {
        lines.push(format!(
            "secret store: {} ({})",
            codewhale_config::quote_os_path(path),
            secret_backend_presence_label(backend.presence),
        ));
    } else {
        lines.push(format!(
            "secret store: unavailable ({})",
            secret_backend_presence_label(backend.presence),
        ));
    }
    if let Some(path) = backend.legacy_path.as_ref() {
        lines.push(format!(
            "legacy secret store: {} ({})",
            codewhale_config::quote_os_path(path),
            secret_backend_presence_label(backend.legacy_presence),
        ));
    } else if explicit_home {
        lines.push(
            "legacy secret store: suppressed by explicit CODEWHALE_HOME isolation".to_string(),
        );
    } else {
        lines.push("legacy secret store: unavailable (not probed)".to_string());
    }

    lines.push(String::new());
    // Diagnostic mode answers "which sources will this shell use?" for one
    // route. Ordinary `auth status` remains the all-provider inventory; a
    // different provider can be inspected explicitly with `--provider`.
    let providers = [provider.unwrap_or(store.config.provider)];
    for provider in providers {
        let config_present = provider_config_api_key(store, provider).is_some();
        let environment_present = provider_env_vars(provider)
            .iter()
            .any(|name| std::env::var(name).is_ok_and(|value| !value.trim().is_empty()));
        let environment_names = match provider_env_vars(provider) {
            [] => "none configured".to_string(),
            names => names.join("/"),
        };
        let external_configured = external_consent(store, provider).is_some();
        lines.push(format!(
            "provider {} sources: config_literal={}, secret_backend={} (provider entry unprobed), environment={} ({}), external_consent={}",
            provider.as_str(),
            if config_present { "present" } else { "missing" },
            secret_backend_presence_label(backend.presence),
            if environment_present { "present" } else { "missing" },
            environment_names,
            if external_configured {
                "configured"
            } else {
                "missing"
            },
        ));
    }
    lines
}

fn run_auth_diagnostic(store: &ConfigStore, provider: Option<ProviderKind>) -> Result<()> {
    for line in auth_diagnostic_lines(store, provider) {
        println!("{line}");
    }
    Ok(())
}

/// Account label (email, plan) of the Codewhale-owned subscription sign-in
/// the provider's config points at. The generation file is Codewhale's own;
/// reading its ID-token claims needs no consent, refresh or network, and the
/// label never carries token material. `Ok(None)`: usable sign-in whose ID
/// token has no email. `Err`: a fixed reason the sign-in is unusable.
fn owned_oauth_account(store: &ConfigStore, provider: ProviderKind) -> Result<Option<String>> {
    let generation = match provider {
        ProviderKind::OpenaiCodex => &store.config.providers.openai_codex,
        ProviderKind::Xai => &store.config.providers.xai,
        _ => bail!("provider has no subscription sign-in"),
    }
    .oauth_credential_generation
    .as_deref()
    .context("no sign-in generation configured")?;
    codewhale_tui::owned_oauth_account_label(provider, generation)
}

#[cfg(test)]
fn auth_list_lines(store: &ConfigStore, secrets: &Secrets) -> Vec<String> {
    auth_list_lines_with_runtime(store, secrets, &CliRuntimeOverrides::default())
}

fn auth_list_lines_with_runtime(
    store: &ConfigStore,
    secrets: &Secrets,
    runtime_overrides: &CliRuntimeOverrides,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push("provider     config store env  route".to_string());
    for provider in ProviderKind::ALL {
        // Label the row by the provider, not by its credential slot. This
        // table has one row per ProviderKind, but several kinds share a slot
        // (ProviderKind::secret_store_slot): SiliconflowCN shares
        // `siliconflow`, and the four Model Studio variants share
        // `modelstudio-token-plan`. Labelling by slot printed `siliconflow`
        // twice and `modelstudio-token-plan` four times, so the reader could
        // not tell which row was which provider. The status columns still
        // read the shared slot, which is what makes one saved key light up
        // the whole family.
        let label = provider.as_str();
        if provider == ProviderKind::Xai {
            let diagnostics = xai_auth_diagnostics(store, runtime_overrides);
            let api_key = diagnostics
                .evaluates_runtime_api_key()
                .then(|| xai_runtime_api_key(store, secrets, runtime_overrides));
            let account = (diagnostics.route == XaiAuthDiagnosticRoute::OwnedOAuth)
                .then(|| owned_oauth_account(store, provider).ok().flatten())
                .flatten()
                .map(|account| format!(" ({account})"))
                .unwrap_or_default();
            lines.push(format!(
                "{label:<12}  {}     {}      {}   {}{account}",
                xai_list_storage_status(api_key.as_ref(), RuntimeApiKeySource::ConfigFile),
                xai_list_storage_status(api_key.as_ref(), RuntimeApiKeySource::Keyring),
                xai_list_storage_status(api_key.as_ref(), RuntimeApiKeySource::Env),
                xai_list_route(&diagnostics, api_key.as_ref())
            ));
            continue;
        }

        if provider == ProviderKind::OpenaiCodex {
            let diagnostics = chatgpt_auth_diagnostics(store, secrets, runtime_overrides);
            let status = |source| {
                yes_no(
                    diagnostics
                        .api_key
                        .as_ref()
                        .is_some_and(|key| key.uses(source)),
                )
            };
            lines.push(format!(
                "{label:<12}  {}     {}      {}   {}",
                status(RuntimeApiKeySource::ConfigFile),
                status(RuntimeApiKeySource::Keyring),
                status(RuntimeApiKeySource::Env),
                diagnostics.source
            ));
            continue;
        }

        let file = provider_config_set(store, provider);
        let keyring = (!file).then(|| provider_keyring_set(secrets, provider));
        let env = provider_env_set(provider);
        let external_selected = external_oauth_selected(store, provider);
        let active = if external_selected {
            "external-consent".to_string()
        } else if file {
            "config".to_string()
        } else if keyring == Some(true) {
            "store".to_string()
        } else if env {
            "env".to_string()
        } else {
            "missing".to_string()
        };
        lines.push(format!(
            "{label:<12}  {}     {}      {}   {active}",
            yes_no(file),
            keyring_status_short(keyring),
            yes_no(env)
        ));
    }
    lines
}

#[cfg(test)]
fn auth_status_lines_for_provider(
    store: &ConfigStore,
    secrets: &Secrets,
    provider: ProviderKind,
) -> Vec<String> {
    auth_status_lines_for_provider_with_runtime(
        store,
        secrets,
        provider,
        &CliRuntimeOverrides::default(),
    )
}

fn auth_status_lines_for_provider_with_runtime(
    store: &ConfigStore,
    secrets: &Secrets,
    provider: ProviderKind,
    runtime_overrides: &CliRuntimeOverrides,
) -> Vec<String> {
    if provider == ProviderKind::Xai {
        return xai_auth_status_lines_for_provider(store, secrets, runtime_overrides);
    }

    if provider == ProviderKind::OpenaiCodex {
        return chatgpt_auth_status_lines(store, secrets, runtime_overrides);
    }

    let config_key = provider_config_api_key(store, provider);
    let keyring_key = provider_keyring_api_key(secrets, provider);
    let env_key = provider_env_value(provider);
    let external = external_consent(store, provider);
    let external_selected = external_oauth_selected(store, provider);

    let active_label = {
        let active_source = if external_selected {
            "external read-only consent (availability not probed)"
        } else if config_key.is_some() {
            "config"
        } else if keyring_key.is_some() {
            "secret store"
        } else if env_key.is_some() {
            "env"
        } else {
            "missing"
        };
        let active_last4 = config_key
            .map(last4_label)
            .or_else(|| keyring_key.as_deref().map(last4_label))
            .or_else(|| env_key.as_ref().map(|(_, value)| last4_label(value)));
        active_last4
            .map(|last4| format!("{active_source} (last4: {last4})"))
            .unwrap_or_else(|| active_source.to_string())
    };

    let env_var_label = env_key
        .as_ref()
        .map(|(name, _)| (*name).to_string())
        .unwrap_or_else(|| provider_env_vars(provider).join("/"));
    let env_status = env_key
        .as_ref()
        .map(|(_, value)| format!("set, last4: {}", last4_label(value)))
        .unwrap_or_else(|| "unset".to_string());

    let is_active = provider == store.config.provider;
    let active_marker = if is_active { " (active provider)" } else { "" };

    let provider_cfg = store.config.providers.for_provider(provider);
    let base_url = provider_cfg.base_url.as_deref().unwrap_or("(default)");
    let model = provider_cfg.model.as_deref().unwrap_or("(default)");

    let lookup_order = "lookup order: config -> secret store -> env".to_string();
    let auth_mode = provider_cfg
        .auth_mode
        .as_deref()
        .or(store.config.auth_mode.as_deref())
        .unwrap_or("api_key")
        .to_string();

    let mut lines = vec![
        format!("provider: {}{}", provider.as_str(), active_marker),
        format!("route: {}", base_url),
        format!("model: {}", model),
        format!("auth mode: {auth_mode}"),
        format!("active source: {active_label}"),
        lookup_order,
        format!(
            "config file: {} ({})",
            codewhale_config::quote_os_path(store.path()),
            source_status(config_key, "missing")
        ),
        format!(
            "secret store: {} ({})",
            secrets.backend_name(),
            source_status(keyring_key.as_deref(), "missing")
        ),
        format!("env var: {env_var_label} ({env_status})"),
    ];
    if let Ok((source, expected_path)) = external_credential_target(provider, None) {
        let status = codewhale_config::external_credential_consent_status(
            external,
            provider,
            source,
            &expected_path,
            store.config.provider,
        );
        lines.push(format!(
            "external credentials: {} (provider={}, source={}, owner={}, path={}, consent_version={}, state={}, scope_valid={}, ambient_path_changed={}; file not probed)",
            status.access.as_str(),
            status.provider,
            status.source.as_str(),
            status.owner,
            codewhale_config::quote_os_path(&status.path),
            status.consent_version,
            status.route_state,
            status.scope_valid,
            status.ambient_path_changed,
        ));
        lines.push(format!("semantics: {}", status.semantics));
        lines.push(format!("revoke: {}", status.revoke_command));
        if let Some(warning) = status.ambient_path_warning() {
            lines.push(warning);
        }
    } else {
        lines.push("external credentials: disabled (no file was probed)".to_string());
    }
    lines
}

fn xai_auth_status_lines_for_provider(
    store: &ConfigStore,
    secrets: &Secrets,
    runtime_overrides: &CliRuntimeOverrides,
) -> Vec<String> {
    let diagnostics = xai_auth_diagnostics(store, runtime_overrides);
    let api_key = diagnostics
        .evaluates_runtime_api_key()
        .then(|| xai_runtime_api_key(store, secrets, runtime_overrides));
    let external = external_consent(store, ProviderKind::Xai);
    let selected_marker = if store.config.provider == ProviderKind::Xai {
        " (selected provider)"
    } else {
        ""
    };
    let provider_cfg = &store.config.providers.xai;
    let model = provider_cfg.model.as_deref().unwrap_or("(default)");
    let auth_mode = diagnostics.auth_mode.as_deref().unwrap_or("api_key");

    let mut lines = vec![
        format!("provider: xai{selected_marker}"),
        format!("route: {}", diagnostics.base_url),
        format!("model: {model}"),
        format!("auth mode: {auth_mode}"),
        format!(
            "credential route: {}",
            xai_credential_route_label(&diagnostics, api_key.as_ref())
        ),
        xai_lookup_order(&diagnostics),
        format!(
            "config file: {} ({})",
            codewhale_config::quote_os_path(store.path()),
            xai_storage_detail(
                &diagnostics,
                api_key.as_ref(),
                RuntimeApiKeySource::ConfigFile
            )
        ),
        format!(
            "secret store: {} ({})",
            secrets.backend_name(),
            xai_storage_detail(&diagnostics, api_key.as_ref(), RuntimeApiKeySource::Keyring)
        ),
        format!(
            "env var: {} ({})",
            provider_env_vars(ProviderKind::Xai).join("/"),
            xai_storage_detail(&diagnostics, api_key.as_ref(), RuntimeApiKeySource::Env)
        ),
        format!(
            "endpoint policy: {}",
            if diagnostics.official_endpoint {
                "official xAI endpoint"
            } else {
                "custom xAI endpoint; API-key-only (owned and external OAuth are inactive)"
            }
        ),
    ];

    lines.push(match diagnostics.generation {
        XaiOAuthGenerationPointer::Absent => "xAI OAuth generation: absent".to_string(),
        XaiOAuthGenerationPointer::Valid
            if diagnostics.route == XaiAuthDiagnosticRoute::OwnedOAuth =>
        {
            "xAI OAuth generation: configured Codewhale-owned pointer (opened to read the account label only; token availability not probed)"
                .to_string()
        }
        XaiOAuthGenerationPointer::Valid => {
            "xAI OAuth generation: valid but inactive for this route".to_string()
        }
        XaiOAuthGenerationPointer::Invalid => {
            "xAI OAuth generation: invalid Codewhale-owned pointer".to_string()
        }
    });

    match diagnostics.route {
        XaiAuthDiagnosticRoute::OwnedOAuth => {
            lines.push(match owned_oauth_account(store, ProviderKind::Xai) {
                Ok(Some(account)) => format!("signed-in account: {account}"),
                Ok(None) => "signed-in account: unknown (the issuer sent no account email)"
                    .to_string(),
                Err(reason) => format!(
                    "signed-in account: none ({reason}; requests fall back to any runtime-effective xAI API key)"
                ),
            });
            lines.push(
                "switch account: `codewhale auth xai-device` (choose the other xAI account; replaces the Codewhale-owned sign-in)"
                    .to_string(),
            );
            lines.push(
                "external credentials: blocked by the configured Codewhale-owned xAI OAuth generation (file not probed)"
                    .to_string(),
            );
            return lines;
        }
        XaiAuthDiagnosticRoute::NeedsRepair => {
            lines.push(
                "external credentials: blocked by the invalid Codewhale-owned xAI OAuth generation pointer (file not probed)"
                    .to_string(),
            );
            lines.push(
                "repair: run `codewhale auth xai-device` to replace the owned generation, or switch [providers.xai] auth_mode to \"api_key\" and remove oauth_credential_generation. Grok CLI consent remains blocked until the pointer is absent."
                    .to_string(),
            );
            return lines;
        }
        XaiAuthDiagnosticRoute::ApiKey if diagnostics.is_custom_endpoint() => {
            lines.push(
                "external credentials: unavailable on a custom xAI endpoint (API-key-only; file not probed)"
                    .to_string(),
            );
            return lines;
        }
        XaiAuthDiagnosticRoute::ApiKey if !diagnostics.oauth_selected && external.is_some() => {
            lines.push(
                "external credentials: configured but inactive because xAI OAuth mode is not selected (file not probed)"
                    .to_string(),
            );
            return lines;
        }
        XaiAuthDiagnosticRoute::ApiKey | XaiAuthDiagnosticRoute::ExternalConsent => {}
    }

    if let Ok((source, expected_path)) = external_credential_target(ProviderKind::Xai, None) {
        let status = codewhale_config::external_credential_consent_status(
            external,
            ProviderKind::Xai,
            source,
            &expected_path,
            store.config.provider,
        );
        lines.push(format!(
            "external credentials: {} (provider={}, source={}, owner={}, path={}, consent_version={}, state={}, scope_valid={}, ambient_path_changed={}; file not probed)",
            status.access.as_str(),
            status.provider,
            status.source.as_str(),
            status.owner,
            codewhale_config::quote_os_path(&status.path),
            status.consent_version,
            status.route_state,
            status.scope_valid,
            status.ambient_path_changed,
        ));
        lines.push(format!("semantics: {}", status.semantics));
        lines.push(format!("revoke: {}", status.revoke_command));
        if let Some(warning) = status.ambient_path_warning() {
            lines.push(warning);
        }
    } else {
        lines.push("external credentials: disabled (no file was probed)".to_string());
    }
    lines
}

fn source_status(value: Option<&str>, missing_label: &str) -> String {
    value
        .map(|v| format!("set, last4: {}", last4_label(v)))
        .unwrap_or_else(|| missing_label.to_string())
}

fn last4_label(value: &str) -> String {
    let trimmed = value.trim();
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 4 {
        return "<redacted>".to_string();
    }
    let last4: String = chars[chars.len() - 4..].iter().collect();
    format!("...{last4}")
}

fn run_auth_command_with_runtime(
    store: &mut ConfigStore,
    command: AuthCommand,
    runtime_overrides: &CliRuntimeOverrides,
) -> Result<()> {
    let command = match command {
        AuthCommand::Status {
            provider,
            diagnostic: true,
        } => {
            // Keep the structural diagnostic structurally read-only: ordinary
            // status constructs the configured credential facade so it can report
            // runtime-effective sources, but diagnostic mode must not even create
            // a system-keyring handle or inspect a file-backed store.
            return run_auth_diagnostic(store, provider);
        }
        command => command,
    };
    run_auth_command_with_secrets_and_runtime(
        store,
        command,
        &Secrets::auto_detect(),
        runtime_overrides,
    )
}

#[cfg(test)]
fn run_auth_command_with_secrets(
    store: &mut ConfigStore,
    command: AuthCommand,
    secrets: &Secrets,
) -> Result<()> {
    run_auth_command_with_secrets_and_runtime(
        store,
        command,
        secrets,
        &CliRuntimeOverrides::default(),
    )
}

fn run_auth_command_with_secrets_and_runtime(
    store: &mut ConfigStore,
    command: AuthCommand,
    secrets: &Secrets,
    runtime_overrides: &CliRuntimeOverrides,
) -> Result<()> {
    match command {
        AuthCommand::PluginLogin { .. } | AuthCommand::PluginLogout { .. } => {
            bail!("plugin OAuth commands must run through the runtime dispatch")
        }
        AuthCommand::XaiDevice => {
            let argv = vec!["auth".to_string(), "xai-device".to_string()];
            let code = codewhale_tui::run(codewhale_tui::RuntimeOptions::default(), argv);
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            })
        }
        command @ (AuthCommand::Claude | AuthCommand::ClaudeRevoke) => {
            let route = if matches!(command, AuthCommand::Claude) {
                "claude"
            } else {
                "claude-revoke"
            };
            let code = codewhale_tui::run(
                codewhale_tui::RuntimeOptions::default(),
                vec!["auth".to_string(), route.to_string()],
            );
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            })
        }
        AuthCommand::Chatgpt => {
            let argv = vec!["auth".to_string(), "chatgpt".to_string()];
            let code = codewhale_tui::run(codewhale_tui::RuntimeOptions::default(), argv);
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            })
        }
        AuthCommand::ChatgptRevoke => {
            let argv = vec!["auth".to_string(), "chatgpt-revoke".to_string()];
            let code = codewhale_tui::run(codewhale_tui::RuntimeOptions::default(), argv);
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            })
        }
        AuthCommand::Orcarouter => {
            let argv = vec!["auth".to_string(), "orcarouter".to_string()];
            let code = codewhale_tui::run(codewhale_tui::RuntimeOptions::default(), argv);
            std::process::exit(if code == std::process::ExitCode::SUCCESS {
                0
            } else {
                1
            })
        }
        AuthCommand::ExternalConsent {
            provider,
            mode,
            path,
            yes,
        } => {
            let (source, path) = external_credential_target(provider, path)?;
            let preview = external_consent_preview_lines(provider, source, &path);
            for line in &preview {
                println!("{line}");
            }
            if mode == ExternalCredentialModeArg::Managed {
                bail!(
                    "managed external credential access is unsupported in v0.9.1: no provider has a reviewed schema-safe preservation adapter. Use --mode read-only, or use Codewhale-owned login/API-key storage."
                );
            }
            confirm_external_consent(yes)?;
            let path_value = path.to_str().context(
                "external credential path cannot be persisted losslessly because it is not valid UTF-8",
            )?;
            let provider_key = provider.provider().provider_config_key();
            codewhale_config::mutate_config_document(store.path(), |document| {
                if matches!(provider, ProviderKind::OpenaiCodex | ProviderKind::Xai) {
                    codewhale_config::set_config_document_value(
                        document,
                        &["providers", provider_key, "auth_mode"],
                        "oauth",
                    )?;
                }
                let prefix = &["providers", provider_key, "external_credentials"];
                codewhale_config::set_config_document_value(
                    document,
                    &[prefix[0], prefix[1], prefix[2], "access"],
                    "read_only",
                )?;
                codewhale_config::set_config_document_value(
                    document,
                    &[prefix[0], prefix[1], prefix[2], "provider"],
                    provider.as_str(),
                )?;
                codewhale_config::set_config_document_value(
                    document,
                    &[prefix[0], prefix[1], prefix[2], "source"],
                    source.as_str(),
                )?;
                codewhale_config::set_config_document_value(
                    document,
                    &[prefix[0], prefix[1], prefix[2], "path"],
                    path_value,
                )?;
                codewhale_config::set_config_document_value(
                    document,
                    &[prefix[0], prefix[1], prefix[2], "consent_version"],
                    i64::from(codewhale_config::EXTERNAL_CREDENTIAL_CONSENT_VERSION),
                )
            })?;
            store
                .reload()
                .context("external consent was saved, but config reload failed")?;
            println!(
                "saved read-only external credential consent: provider={}, owner={}, path={}, consent_version={} ({})",
                provider.as_str(),
                source.as_str(),
                codewhale_config::quote_os_path(&path),
                codewhale_config::EXTERNAL_CREDENTIAL_CONSENT_VERSION,
                codewhale_config::EXTERNAL_CREDENTIAL_READ_ONLY_SEMANTICS,
            );
            println!(
                "revoke with: codewhale auth external-revoke --provider {}",
                provider.as_str()
            );
            Ok(())
        }
        AuthCommand::ExternalRevoke { provider } => {
            let provider_key = provider.provider().provider_config_key();
            codewhale_config::mutate_config_document(store.path(), |document| {
                codewhale_config::unset_config_document_value(
                    document,
                    &["providers", provider_key, "external_credentials"],
                )?;
                Ok(())
            })?;
            store
                .reload()
                .context("external consent was revoked, but config reload failed")?;
            println!(
                "external credential access disabled for {}",
                provider.as_str()
            );
            Ok(())
        }
        AuthCommand::Status {
            provider,
            diagnostic,
        } => {
            if diagnostic {
                return run_auth_diagnostic(store, provider);
            }
            match provider {
                Some(provider) => {
                    for line in auth_status_lines_for_provider_with_runtime(
                        store,
                        secrets,
                        provider,
                        runtime_overrides,
                    ) {
                        println!("{line}");
                    }
                }
                None => {
                    for line in
                        auth_status_all_providers_with_runtime(store, secrets, runtime_overrides)
                    {
                        println!("{line}");
                    }
                }
            }
            Ok(())
        }
        AuthCommand::Set {
            provider,
            api_key,
            api_key_stdin,
        } => {
            let slot = provider_slot(provider);
            if provider == ProviderKind::Ollama && api_key.is_none() && !api_key_stdin {
                let provider_cfg = store.config.providers.for_provider_mut(provider);
                if provider_cfg.base_url.is_none() {
                    provider_cfg.base_url = Some("http://localhost:11434/v1".to_string());
                }
                store.save()?;
                println!(
                    "configured {slot} provider in {} (API key optional)",
                    store.path().display()
                );
                return Ok(());
            }
            let api_key = match (api_key, api_key_stdin) {
                (Some(v), _) => v,
                (None, true) => read_api_key_from_stdin()?,
                (None, false) => prompt_api_key(provider)?,
            };
            let mut credential_store =
                codewhale_config::credentials::credential_metadata_store(store)?;
            if let Some(redirected) = credential_store.as_ref() {
                eprintln!(
                    "ambient config {} is workspace-scoped; writing credential metadata to the user-global {} instead",
                    codewhale_config::quote_os_path(store.path()),
                    codewhale_config::quote_os_path(redirected.path()),
                );
            }
            let store = credential_store.as_mut().unwrap_or(store);
            let secret_store_saved = set_provider_api_key(store, secrets, provider, &api_key)?;
            // Don't print the key. Don't echo length.
            if secret_store_saved {
                println!(
                    "saved API key for {slot} to {} (config contains metadata only)",
                    secret_store_location(secrets),
                );
            } else {
                println!("saved API key for {slot} to {}", store.path().display());
            }
            println!("model unchanged; run `codewhale model resolve` to see the active model");
            Ok(())
        }
        AuthCommand::Get { provider } => {
            println!(
                "{}",
                auth_get_line_with_runtime(store, secrets, provider, runtime_overrides)
            );
            Ok(())
        }
        AuthCommand::PrintApiKey { provider } => {
            let mut stdout = io::stdout().lock();
            credential_handoff::handoff_secret_line(&mut stdout, io::stdout().is_terminal(), || {
                credential_handoff::resolve_api_key(store, secrets, provider, runtime_overrides)
            })
        }
        AuthCommand::Clear { provider } => {
            let incomplete = if provider == ProviderKind::Xai {
                codewhale_config::with_xai_oauth_revocation_transaction(|| {
                    clear_auth_provider(store, secrets, provider)
                })?
            } else {
                clear_auth_provider(store, secrets, provider)?
            };
            // Reported after the xAI transaction commits: the config leg is
            // saved, so failing inside it would roll back a revocation that
            // already happened.
            if let Some(message) = incomplete {
                bail!(message);
            }
            Ok(())
        }
        AuthCommand::List => {
            for line in auth_list_lines_with_runtime(store, secrets, runtime_overrides) {
                println!("{line}");
            }
            Ok(())
        }
        AuthCommand::Migrate { dry_run } => run_auth_migrate(store, secrets, dry_run),
    }
}

/// Where `auth set` just wrote a key. The file backend's static label names
/// `~/.codewhale/secrets/`, which is wrong under `CODEWHALE_HOME`; report the
/// resolved file instead. Other backends keep their label.
fn secret_store_location(secrets: &Secrets) -> String {
    let label = secrets.backend_name();
    if label.starts_with("file-based")
        && let Ok((path, _)) = codewhale_secrets::FileKeyringStore::default_paths_read_only()
    {
        return format!("file-based ({})", codewhale_config::quote_os_path(&path));
    }
    label.to_string()
}

fn external_consent_preview_lines(
    provider: ProviderKind,
    source: codewhale_config::ExternalCredentialSource,
    path: &Path,
) -> Vec<String> {
    vec![
        "External credential consent preview (nothing has been saved):".to_string(),
        format!("  provider: {}", provider.as_str()),
        format!(
            "  owning CLI: {} ({})",
            source.owner_label(),
            source.as_str()
        ),
        format!(
            "  exact resolved path: {}",
            codewhale_config::quote_os_path(path)
        ),
        format!(
            "  access: read_only ({})",
            codewhale_config::EXTERNAL_CREDENTIAL_READ_ONLY_SEMANTICS
        ),
        "  managed: unavailable (no reviewed schema-safe preservation adapter)".to_string(),
        format!(
            "  revoke: codewhale auth external-revoke --provider {}",
            provider.as_str()
        ),
    ]
}

fn confirm_external_consent(yes: bool) -> Result<()> {
    use std::io::IsTerminal;

    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "external credential consent was not saved: non-interactive use requires explicit --yes after reviewing the preview"
        );
    }
    confirm_external_consent_answer(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
}

fn confirm_external_consent_answer(
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<()> {
    write!(writer, "Type 'yes' to grant this exact read-only access: ")?;
    writer.flush()?;
    let mut answer = String::new();
    reader
        .read_line(&mut answer)
        .context("reading external credential consent confirmation")?;
    if answer.trim() != "yes" {
        bail!("external credential consent cancelled; no configuration was changed");
    }
    Ok(())
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no " }
}

fn keyring_status_short(state: Option<bool>) -> &'static str {
    match state {
        Some(true) => "yes",
        Some(false) => "no ",
        None => "n/a",
    }
}

fn prompt_api_key(provider: ProviderKind) -> Result<String> {
    use std::io::IsTerminal;
    read_prompted_api_key(
        provider.as_str(),
        io::stdin().is_terminal(),
        |prompt| {
            // The help promises the key is not echoed: a plain `read_line`
            // would leave it on screen, in scrollback, and in recordings.
            // `read_secure_line` returns "" on a stream that is not a
            // terminal, so prompt on whichever of stderr/stdout is one.
            let term = match hidden_prompt_stream(
                io::stderr().is_terminal(),
                io::stdout().is_terminal(),
            ) {
                Some(PromptStream::Stderr) => console::Term::stderr(),
                Some(PromptStream::Stdout) => console::Term::stdout(),
                None => {
                    return Err(io::Error::other(
                        "both stdout and stderr are redirected, so the key cannot be read \
                         without echo; pipe it on stdin instead",
                    ));
                }
            };
            term.write_str(prompt)?;
            // Ends the line itself once the key is read.
            term.read_secure_line()
        },
        read_api_key_from_stdin,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PromptStream {
    Stderr,
    Stdout,
}

fn hidden_prompt_stream(
    stderr_is_terminal: bool,
    stdout_is_terminal: bool,
) -> Option<PromptStream> {
    if stderr_is_terminal {
        Some(PromptStream::Stderr)
    } else if stdout_is_terminal {
        Some(PromptStream::Stdout)
    } else {
        None
    }
}

fn read_prompted_api_key(
    provider_id: &str,
    stdin_is_terminal: bool,
    read_hidden_line: impl FnOnce(&str) -> io::Result<String>,
    read_piped: impl FnOnce() -> Result<String>,
) -> Result<String> {
    use std::io::Write;
    let prompt = format!("Enter API key for {provider_id}: ");
    if !stdin_is_terminal {
        // Non-interactive: read directly without prompting twice.
        eprint!("{prompt}");
        io::stderr().flush().ok();
        return read_piped();
    }
    let buf = read_hidden_line(&prompt).context("failed to read API key from the terminal")?;
    let key = buf.trim().to_string();
    if key.is_empty() {
        bail!("empty API key provided");
    }
    Ok(key)
}

/// Move plaintext keys from config.toml into the configured secret store.
/// Hidden in v0.8.8 because the normal setup path is config/env only.
fn run_auth_migrate(store: &mut ConfigStore, secrets: &Secrets, dry_run: bool) -> Result<()> {
    let mut migrated: Vec<(ProviderKind, &'static str)> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let literal =
        |value: &String| classify_config_api_key_value(value) == ConfigApiKeyValueKind::Literal;

    for provider in ProviderKind::ALL {
        let slot = provider_slot(provider);
        let from_provider_block = store
            .config
            .providers
            .for_provider(provider)
            .api_key
            .clone()
            .filter(literal);
        let Some(value) = from_provider_block else {
            continue;
        };

        if let Ok(Some(existing)) = secrets.get(slot)
            && existing == value
        {
            // Already migrated; safe to strip the file slot.
        } else if dry_run {
            migrated.push((provider, slot));
            continue;
        } else if let Err(err) = secrets.set(slot, &value) {
            warnings.push(format!(
                "skipped {slot}: failed to write to secret store: {err}"
            ));
            continue;
        }
        if !dry_run {
            store.config.providers.for_provider_mut(provider).api_key = None;
        }
        migrated.push((provider, slot));
    }

    if !dry_run && !migrated.is_empty() {
        store
            .save()
            .context("failed to write updated config.toml")?;
    }
    if !dry_run {
        codewhale_config::scrub_plaintext_api_keys_from_config_backup(store.path())
            .context("failed to remove plaintext API keys from config backup")?;
    }

    println!("secret store backend: {}", secrets.backend_name());
    if migrated.is_empty() {
        println!("nothing to migrate (config.toml has no plaintext api_key entries)");
    } else {
        println!(
            "{} {} provider key(s):",
            if dry_run { "would migrate" } else { "migrated" },
            migrated.len()
        );
        for (_, slot) in &migrated {
            println!("  - {slot}");
        }
        if !dry_run {
            println!(
                "config.toml at {} no longer contains api_key entries for migrated providers.",
                store.path().display()
            );
        }
    }
    for w in warnings {
        eprintln!("warning: {w}");
    }
    Ok(())
}

fn run_config_command(
    store: &mut ConfigStore,
    command: ConfigCommand,
    project_bundle_scope: bool,
    per_run_overrides: &[String],
) -> Result<()> {
    if project_bundle_scope && !codewhale_config::config_path_is_workspace_scoped(store.path()) {
        bail!(
            "--project requires a workspace config ({} is the user-global document)",
            store.path().display()
        );
    }
    // A per-run overlay must never leak into the file: commands that write
    // the store refuse it outright instead of saving a merged document.
    if !per_run_overrides.is_empty()
        && matches!(
            command,
            ConfigCommand::Set { .. }
                | ConfigCommand::Unset { .. }
                | ConfigCommand::Import(_)
                | ConfigCommand::Migrate { dry_run: false, .. }
                | ConfigCommand::Telemetry {
                    accept_notice: Some(_)
                }
        )
    {
        bail!(
            "--set is per-run and never saved; it cannot be combined with `config set`, \
             `config unset`, or `config import`. Drop --set, or use a read command."
        );
    }
    match command {
        ConfigCommand::Get { key } => {
            if per_run_overrides.is_empty() && codewhale_tui::route_preferences::is_route_key(&key)
            {
                if let Some(value) = codewhale_tui::route_preferences::get(store.path(), &key)? {
                    println!("{value}");
                    return Ok(());
                }
                bail!("key not found: {key}");
            }
            if codewhale_config::notifications::in_namespace(&key) {
                let config = codewhale_config::notifications::from_extras(&store.config.extras)?;
                let keys = if key.eq_ignore_ascii_case("notifications") {
                    codewhale_config::notifications::NotificationSetting::ALL.to_vec()
                } else {
                    vec![codewhale_config::notifications::NotificationSetting::required(&key)?]
                };
                for setting in keys {
                    if key.eq_ignore_ascii_case("notifications") {
                        println!(
                            "notifications.{} = {}",
                            setting.key(),
                            config.display(setting)
                        );
                    } else {
                        println!("{}", config.display(setting));
                    }
                }
                return Ok(());
            }
            // A settings.toml key is answered from settings.toml, even when a
            // stale config.toml copy that nothing reads is still present.
            if codewhale_tui::config_keys::config_key_home(&key)
                == codewhale_tui::config_keys::ConfigKeyHome::SettingsToml
            {
                let value = settings_key_value(store, &key)?;
                note_unread_config_copy(store, &key);
                println!("{value}");
                return Ok(());
            }
            if key == "stream" || key.starts_with("stream.") {
                let stream = codewhale_tui::config_keys::resolved_stream_config(&store.config)?;
                let value = if key == "stream" {
                    &stream
                } else {
                    stream
                        .get(&key["stream.".len()..])
                        .with_context(|| format!("key not found: {key}"))?
                };
                println!("{value}");
                return Ok(());
            }
            if let Some(value) = store.config.get_display_value(&key) {
                if key == "telemetry" {
                    println!(
                        "Usage reporting: {}",
                        telemetry_preference_status(store.config.telemetry)
                    );
                    println!("Details: codewhale config telemetry");
                } else {
                    println!("{value}");
                }
                return Ok(());
            }
            bail!("key not found: {key}");
        }
        ConfigCommand::Set { key, value } => {
            if codewhale_tui::route_preferences::is_route_key(&key) {
                codewhale_tui::route_preferences::set(store.path(), &key, &value)?;
                store.reload()?;
                println!("set {key}");
                return Ok(());
            }
            if codewhale_config::notifications::in_namespace(&key) {
                let setting = codewhale_config::notifications::NotificationSetting::required(&key)?;
                codewhale_config::notifications::NotificationConfigUpdate::parse(setting, &value)?
                    .persist(store.path())?;
                store.reload()?;
                println!("set notifications.{}", setting.key());
                return Ok(());
            }
            // Refuse a key nothing reads, and send settings.toml keys to
            // settings.toml, before config.toml is touched (#6563).
            match codewhale_tui::config_keys::config_key_home(&key) {
                codewhale_tui::config_keys::ConfigKeyHome::ConfigToml => {
                    // A value typed or validated by its config.toml reader.
                    if let Some(typed) =
                        codewhale_tui::config_keys::config_toml_value(&key, &value)?
                    {
                        store.config.extras.insert(key.trim().to_string(), typed);
                        store.save()?;
                        println!("set {key}");
                        return Ok(());
                    }
                }
                codewhale_tui::config_keys::ConfigKeyHome::SettingsToml => {
                    refuse_workspace_scoped_settings_key(store, &key)?;
                    let path = codewhale_tui::config_keys::set_settings_value(&key, &value)?;
                    println!("set {key} in {}", path.display());
                    note_unread_config_copy(store, &key);
                    return Ok(());
                }
                codewhale_tui::config_keys::ConfigKeyHome::Unknown => {
                    bail!(codewhale_tui::config_keys::unknown_config_key_message(&key));
                }
            }
            store.config.set_value(&key, &value)?;
            if key == "telemetry" {
                let enabled = store
                    .config
                    .telemetry
                    .context("telemetry must be true or false")?;
                let receipt = codewhale_tui::set_telemetry_preference(
                    Some(store.path().to_path_buf()),
                    enabled,
                )?;
                println!("{receipt}");
                if enabled {
                    println!("{}", telemetry::notice::STARTUP_DISCLOSURE);
                }
            } else {
                store.save()?;
                println!("set {}", config_key_label(store, &key));
            }
            Ok(())
        }
        ConfigCommand::Telemetry { accept_notice } => {
            println!("{}\n", telemetry::notice::NOTICE_BODY);
            if let Some(version) = accept_notice {
                let receipt = codewhale_tui::accept_telemetry_notice(
                    Some(store.path().to_path_buf()),
                    version,
                )?;
                println!("{receipt}");
            } else {
                println!(
                    "Usage reporting: {}",
                    telemetry_preference_status(store.config.telemetry)
                );
                println!("To enable: codewhale config set telemetry true");
                println!("To opt out: codewhale config set telemetry false");
            }
            Ok(())
        }
        ConfigCommand::Unset { key } => {
            if codewhale_tui::route_preferences::is_route_key(&key) {
                codewhale_tui::route_preferences::unset(store.path(), &key)?;
                store.reload()?;
                println!("unset {key}");
                return Ok(());
            }
            if codewhale_config::notifications::in_namespace(&key) {
                let setting = codewhale_config::notifications::NotificationSetting::required(&key)?;
                setting.unset(store.path())?;
                store.reload()?;
                println!("unset notifications.{}", setting.key());
                return Ok(());
            }
            let label = config_key_label(store, &key);
            store.config.unset_value(&key)?;
            store.save()?;
            println!("unset {label}");
            Ok(())
        }
        ConfigCommand::List => {
            // Configured truth, not live-session truth (DGF-01): a running
            // session keeps the route it resolved at launch, so these values
            // must not be read as "what the current session is serving".
            // `#` keeps the header safe for `key = value` line parsers.
            println!("# configured values ({})", store.path().display());
            println!(
                "# a running session keeps the route it resolved at launch; `codewhale model resolve` reports the route a new session would take"
            );
            for (key, value) in store.config.list_values() {
                println!("{key} = {value}");
            }
            Ok(())
        }
        ConfigCommand::Path => {
            println!("{}", store.path().display());
            Ok(())
        }
        ConfigCommand::Edit => {
            let path = store.path().to_path_buf();
            println!("{}", path.display());
            let editor = std::env::var("VISUAL")
                .or_else(|_| std::env::var("EDITOR"))
                .unwrap_or_else(|_| "vi".to_string());
            let status = Command::new(&editor)
                .arg(&path)
                .status()
                .with_context(|| format!("failed to launch editor {editor:?}"))?;
            if !status.success() {
                bail!("editor {editor:?} exited with {status}");
            }
            Ok(())
        }
        ConfigCommand::Doctor => run_config_doctor(store),
        ConfigCommand::Dump => {
            if !per_run_overrides.is_empty() {
                println!(
                    "# {} per-run --set override(s), not saved",
                    per_run_overrides.len()
                );
            }
            println!("# {}", store.path().display());
            let mut document = store.config.redacted_toml_value();
            document
                .as_table_mut()
                .context("config must be a TOML table")?
                .insert(
                    "stream".to_string(),
                    codewhale_tui::config_keys::resolved_stream_config(&store.config)?,
                );
            print!("{}", toml::to_string_pretty(&document)?);
            Ok(())
        }
        ConfigCommand::Import(args) => {
            let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            config_bundles::run_import(&args, store, &workspace)
        }
        ConfigCommand::Export(args) => config_bundles::run_export(&args, store),
        ConfigCommand::Migrate { dry_run, prefer } => {
            run_config_migrate(store, dry_run, prefer.map(Into::into))
        }
    }
}

/// `key`, or `key (providers.<name>.<field>)` when `key` is a legacy
/// top-level spelling that addresses the active provider's table (#6394).
fn config_key_label(store: &ConfigStore, key: &str) -> String {
    match store.config.root_alias_key(key) {
        Some(real) => format!("{real} (`{key}` now names the active provider's table)"),
        None => key.to_string(),
    }
}

fn run_config_migrate(
    store: &mut ConfigStore,
    dry_run: bool,
    prefer: Option<codewhale_config::legacy_root::LegacyRootPrefer>,
) -> Result<()> {
    let path = store.path().to_path_buf();
    let receipt = if dry_run {
        codewhale_config::preview_legacy_root_config(&path, prefer)?
    } else {
        let (receipt, backup) = codewhale_config::migrate_legacy_root_config(&path, prefer)?;
        if let Some(backup) = backup {
            println!("backup: {}", backup.display());
        }
        store.reload()?;
        receipt
    };
    if receipt.is_empty() {
        println!("nothing to migrate in {}", path.display());
        return Ok(());
    }
    for line in receipt.lines() {
        println!("{line}");
    }
    if dry_run {
        println!("dry run: {} was not changed", path.display());
    }
    Ok(())
}

/// Doctor lines for legacy top-level keys: sources only, never values.
fn legacy_root_doctor_lines(
    receipt: &codewhale_config::legacy_root::LegacyRootMigration,
) -> Vec<String> {
    use codewhale_config::legacy_root::LegacyRootNote;
    let mut lines = Vec::new();
    for note in &receipt.notes {
        match note {
            LegacyRootNote::Conflict { .. } => lines.push(format!("warning: {note}")),
            _ => lines.push(format!(
                "note: {note} (in memory; the next save or `codewhale config migrate` updates the file)"
            )),
        }
    }
    lines
}

/// settings.toml is user-global. A `config` command aimed at a workspace
/// document (`--project`, or a workspace `--config`) must not write it, or
/// report its value as the project's.
fn refuse_workspace_scoped_settings_key(store: &ConfigStore, key: &str) -> Result<()> {
    if codewhale_config::config_path_is_workspace_scoped(store.path()) {
        bail!(
            "`{key}` is a user setting stored in settings.toml and has no project scope; \
             {} is a workspace config. Run the command without --project (or use /settings). \
             No value was changed.",
            store.path().display()
        );
    }
    Ok(())
}

/// `config get` for a settings.toml key: the saved settings.toml value,
/// never a config.toml copy that nothing reads.
fn settings_key_value(store: &ConfigStore, key: &str) -> Result<String> {
    refuse_workspace_scoped_settings_key(store, key)?;
    codewhale_tui::config_keys::settings_value(key)?.ok_or_else(|| anyhow!("key not found: {key}"))
}

/// Point at a config.toml copy of a settings.toml key: nothing reads it.
fn note_unread_config_copy(store: &ConfigStore, key: &str) {
    if store.config.extras.contains_key(key.trim()) {
        eprintln!(
            "note: {} also has `{key}`, which nothing reads; remove it with \
             `codewhale config unset {key}`",
            store.path().display()
        );
    }
}

/// Apply per-run `--set KEY=VALUE` overlays to the loaded store in memory.
/// Nothing is saved; callers that persist must refuse overrides first
/// (see `run_config_command`).
fn apply_per_run_overrides(store: &mut ConfigStore, specs: &[String]) -> Result<()> {
    for spec in specs {
        let (key, value) = spec
            .split_once('=')
            .context("invalid --set: expected KEY=VALUE")?;
        store.config.set_value(key.trim(), value).map_err(|error| {
            anyhow!(
                "invalid --set: {}",
                codewhale_config::persistence::redact_secrets(&format!("{error:#}"))
            )
        })?;
    }
    Ok(())
}

/// Read-only credential and endpoint check, plus a report of config.toml
/// keys nothing reads (#6563). Unread keys are warnings: they are preserved
/// on save and never fail the check. Never prints a credential — presence
/// and shape only.
fn run_config_doctor(store: &ConfigStore) -> Result<()> {
    println!("# {}", store.path().display());
    let mut errors: Vec<String> = Vec::new();
    let unread = codewhale_tui::config_keys::unread_config_keys(
        store.config.extras.keys().map(String::as_str),
    );
    for finding in &unread {
        println!("warning: {finding}");
    }

    // Legacy top-level `base_url` / `api_key` (#6394): what loading moved in
    // memory, which pair still disagrees, and which value is in use. Sources
    // only, never values.
    for line in legacy_root_doctor_lines(store.legacy_root_migration()) {
        println!("{line}");
    }

    let mut secrets: Vec<(String, Option<String>)> = Vec::new();
    let mut endpoints: Vec<(String, Option<String>)> = Vec::new();
    for provider in ProviderKind::ALL {
        let table = store.config.providers.for_provider(provider);
        secrets.push((format!("{provider:?}.api_key"), table.api_key.clone()));
        endpoints.push((format!("{provider:?}.base_url"), table.base_url.clone()));
    }
    for (name, secret) in secrets {
        if secret
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            errors.push(format!("`{name}` is set but empty"));
        }
    }
    for (name, endpoint) in endpoints {
        if let Some(url) = endpoint.as_deref() {
            let url_for_check = url.to_ascii_lowercase();
            if !url_for_check.starts_with("http://") && !url_for_check.starts_with("https://") {
                errors.push(format!("`{name}` is not an http(s) URL: {url}"));
            }
        }
    }

    if !errors.is_empty() {
        for error in &errors {
            println!("error: {error}");
        }
        bail!("doctor: {} error(s): {}", errors.len(), errors.join("; "));
    }
    if unread.is_empty() {
        println!("doctor: credentials and endpoints clean");
    } else {
        println!(
            "doctor: credentials and endpoints clean; {} config.toml key(s) nothing reads",
            unread.len()
        );
    }
    Ok(())
}

fn telemetry_preference_status(preference: Option<bool>) -> &'static str {
    let (enabled, source) = codewhale_config::resolved_telemetry_consent(preference);
    if !enabled {
        return match source {
            codewhale_config::TelemetrySource::Env => "Off (environment or run kill switch)",
            _ => "Off (saved preference)",
        };
    }
    match telemetry::load_setup_state_for_decision() {
        Some(state) if state.telemetry_opted_out() => "Off (saved opt-out)",
        None => "Off (privacy state unreadable)",
        Some(_) => match source {
            codewhale_config::TelemetrySource::Default => "On (default)",
            codewhale_config::TelemetrySource::Env | codewhale_config::TelemetrySource::Cli => {
                "On (environment or run preference)"
            }
            codewhale_config::TelemetrySource::Config => "On (saved preference)",
        },
    }
}

fn model_command_provider_hint(
    command_provider: Option<ProviderKind>,
    top_level_provider: Option<ProviderKind>,
) -> Option<ProviderKind> {
    command_provider.or(top_level_provider)
}

fn provider_source_label(source: ProviderSource) -> String {
    match source {
        ProviderSource::Cli => "--provider".to_string(),
        ProviderSource::Env(name) => format!("environment ({name})"),
        ProviderSource::Config => "config".to_string(),
    }
}

fn canonical_model_for_set(model: &str) -> &str {
    match model.to_ascii_lowercase().as_str() {
        "pro" | "deepseek-v4pro" => "deepseek-v4-pro",
        "flash" | "deepseek-v4flash" => "deepseek-v4-flash",
        "flash-vision" | "deepseek-v4flashvisionexp" => "deepseek-v4-flash-vision-exp",
        "auto" => "auto",
        _ => model,
    }
}

/// The provider (and its endpoint) whose model `model set` writes.
///
/// A home config gets `[providers.<saved route>] model`, so the saved route in
/// that file decides. A workspace-scoped config gets a root `model` that
/// applies to whatever route is in effect, which that file alone may not name.
fn model_set_route(
    store: &ConfigStore,
    resolved_runtime: &ResolvedRuntimeOptions,
) -> Result<Option<(ProviderKind, String)>> {
    if codewhale_config::config_path_is_workspace_scoped(store.path()) {
        return Ok(Some((
            resolved_runtime.provider,
            resolved_runtime.base_url.clone(),
        )));
    }
    let (route, _, _) = codewhale_tui::route_preferences::selected_route(store.path())?;
    Ok(ProviderKind::parse_config_identity(&route).map(|provider| {
        let base_url = store
            .config
            .providers
            .for_provider(provider)
            .base_url
            .clone()
            .filter(|base| !base.trim().is_empty())
            .unwrap_or_else(|| {
                codewhale_config::provider::provider_for_kind(provider)
                    .default_base_url()
                    .to_string()
            });
        (provider, base_url)
    }))
}

fn run_model_command(
    store: &mut ConfigStore,
    command: ModelCommand,
    top_level_provider: Option<ProviderKind>,
    resolved_runtime: &ResolvedRuntimeOptions,
) -> Result<()> {
    match command {
        ModelCommand::List { provider } => {
            let filter = model_command_provider_hint(provider, top_level_provider);
            codewhale_tui::maybe_load_persisted_cache();
            let providers: &[ProviderKind] = match &filter {
                Some(provider) => std::slice::from_ref(provider),
                None => ProviderKind::all(),
            };
            for provider in providers {
                for model in codewhale_tui::all_catalog_models_for_provider(*provider) {
                    println!("{model} ({})", provider.as_str());
                }
            }
            Ok(())
        }
        ModelCommand::Resolve { model, provider } => {
            let registry = ModelRegistry::default();
            // Only `model resolve --provider X` is a hypothetical. The
            // top-level `--provider` is the route this process is actually on,
            // and it is already folded into `resolved_runtime` — treating it as
            // a hypothetical made `codewhale --provider moonshot --model
            // kimi-k3 model resolve` re-derive a registry default and report
            // `kimi-k2.7-code` while the runtime used `kimi-k3` (v0.9.1 kimi-k3 dogfood report). The
            // top-level `--model` was not consulted at all on that path.
            let subcommand_provider = provider;
            let queried = model.as_deref().map(str::trim).filter(|m| !m.is_empty());

            // With no explicit query, this reports the route the runtime would
            // actually take — the same answer `doctor` gives — rather than
            // re-deriving one from an empty flag set. Re-deriving is what made
            // a Z.ai config report `provider: deepseek` (#4832).
            if queried.is_none() && subcommand_provider.is_none() {
                let saved = if matches!(resolved_runtime.provider_source, ProviderSource::Config)
                    && !matches!(
                        resolved_runtime.model_source,
                        codewhale_config::ModelSource::Cli | codewhale_config::ModelSource::Env
                    ) {
                    Some(codewhale_tui::route_preferences::selected_route(
                        store.path(),
                    )?)
                } else {
                    None
                };
                let provider = saved
                    .as_ref()
                    .map_or(resolved_runtime.provider.as_str(), |(provider, _, _)| {
                        provider.as_str()
                    });
                let model = saved
                    .as_ref()
                    .map_or(resolved_runtime.model.as_str(), |(_, model, _)| {
                        model.as_str()
                    });
                let source = saved
                    .as_ref()
                    .map_or(resolved_runtime.model_source, |(_, _, source)| *source);
                println!(
                    "requested: {}",
                    if source.is_explicit() { model } else { "" }
                );
                println!("resolved: {model}");
                println!("provider: {provider}");
                println!("used_fallback: {}", !source.is_explicit());
                println!(
                    "provider_source: {}",
                    provider_source_label(resolved_runtime.provider_source)
                );
                println!("model_source: {}", source.as_str());
                // The runtime refuses a route its resolver rejected; saying
                // `resolved:` without the rejection would report it as usable.
                if saved.is_none()
                    && let Err(error) = &resolved_runtime.route
                {
                    println!("route_error: {error}");
                }
                return Ok(());
            }

            // An explicit model or provider makes this a hypothetical query
            // inside a named route. The subcommand provider wins; otherwise
            // the configured runtime provider remains authoritative. Model
            // text never authorizes switching providers or credential slots.
            let provider_hint = subcommand_provider.or(Some(resolved_runtime.provider));
            let resolved = registry.resolve(queried, provider_hint)?;
            println!("requested: {}", resolved.requested.unwrap_or_default());
            println!("resolved: {}", resolved.resolved.id);
            println!("provider: {}", resolved.resolved.provider.as_str());
            println!("used_fallback: {}", resolved.used_fallback);
            println!(
                "provider_source: {}",
                if subcommand_provider.is_some() {
                    "--provider".to_string()
                } else {
                    provider_source_label(resolved_runtime.provider_source)
                }
            );
            println!(
                "model_source: {}",
                if queried.is_some() {
                    "argument"
                } else {
                    // This branch is reachable only for an explicit
                    // subcommand provider with no requested model. The model
                    // therefore came from that provider's default, not from
                    // the configured runtime route we deliberately overrode.
                    "provider default"
                }
            );
            Ok(())
        }
        ModelCommand::Set { model } => {
            let trimmed = model.trim();
            if trimmed.is_empty() {
                bail!("Model name cannot be empty");
            }
            // The short names are DeepSeek's. They expand wherever a DeepSeek
            // id is servable (DeepSeek itself and the hosts that serve its
            // models, such as OpenRouter or Together). On a vendor that only
            // serves its own family (OpenAI, Anthropic, ...) `pro` is that
            // vendor's own name, so it is stored as typed.
            let expanded = canonical_model_for_set(trimmed);
            let canonical = if expanded != trimmed
                && model_set_route(store, resolved_runtime)?.is_some_and(|(provider, base_url)| {
                    codewhale_config::known_foreign_model_owner(provider, expanded, &base_url)
                        .is_some()
                }) {
                trimmed
            } else {
                expanded
            };
            codewhale_tui::route_preferences::set(store.path(), "model", canonical)?;
            store.reload()?;
            println!("Default model set to '{canonical}'");
            Ok(())
        }
    }
}

/// These controls attach to the actual canonical owner. The IO reactor
/// forwards requests only; it constructs no Engine or history writer.
fn run_thread_command(
    cli: &Cli,
    _store: &mut ConfigStore,
    _runtime_overrides: &CliRuntimeOverrides,
    command: ThreadCommand,
) -> Result<()> {
    let mutation_options = thread_control_mutation_options(cli, &command)?;
    // Resolve only explicit startup paths before any attachment await. An
    // absent workspace is supplied by the acknowledged owner, never cwd.
    let selection = if cli.workspace.is_some() || cli.profile.is_some() || cli.config.is_some() {
        let startup = if cli
            .workspace
            .as_ref()
            .is_some_and(|path| path.is_relative())
            || cli.config.as_ref().is_some_and(|path| path.is_relative())
        {
            std::env::current_dir().context("capture thread-control startup directory")?
        } else {
            PathBuf::new()
        };
        thread_control_selection(cli, &startup)
    } else {
        None
    };
    let config_path = selection
        .as_ref()
        .and_then(|selection| selection.config_source.clone());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialize canonical control IO")?;
    run_thread_control_command_with(command, |request| {
        let request = apply_thread_control_mutation_options(request, &mutation_options)?;
        runtime.block_on(codewhale_app_server::request_thread_control(
            config_path,
            None,
            selection,
            request,
        ))
    })
}

/// Only explicit normalized history proposals cross this boundary. The held
/// owner's existing typed decoder and route/posture checks admit them.
fn thread_control_mutation_options(
    cli: &Cli,
    command: &ThreadCommand,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let retained_key = match command {
        ThreadCommand::Resume { operation_key, .. } | ThreadCommand::Fork { operation_key, .. } => {
            operation_key
        }
        _ => return Ok(serde_json::Map::new()),
    };
    anyhow::ensure!(
        cli.api_key.is_none() && cli.base_url.is_none(),
        "thread history controls cannot import --api-key or --base-url; configure/authenticate the owning Runtime, then select its config/profile (values omitted)"
    );
    anyhow::ensure!(
        !cli.yolo && cli.verbosity.is_none() && cli.telemetry.is_none(),
        "unsupported per-run thread setting; configure the owning Runtime instead (values omitted)"
    );
    for spec in &cli.overrides {
        let (key, _) = spec
            .split_once('=')
            .context("invalid --set: expected KEY=VALUE (value omitted)")?;
        anyhow::ensure!(
            matches!(
                key.trim(),
                "provider" | "model" | "default_text_model" | "approval_policy" | "sandbox_mode"
            ),
            "unsupported per-run thread --set key; use the owning Runtime config/profile (key and value omitted)"
        );
    }
    let mut fields = serde_json::Map::new();
    if let Some(model) = cli.model.as_ref() {
        fields.insert("model".into(), serde_json::json!(model));
    }
    if let Some(provider) = cli.provider.as_ref() {
        let identity = builtin_provider_arg(provider)
            .map_or_else(|| provider.clone(), |provider| provider.as_str().to_owned());
        fields.insert("model_provider".into(), serde_json::json!(identity));
    }
    if let Some(policy) = cli.approval_policy.as_ref() {
        fields.insert("approval_policy".into(), serde_json::json!(policy));
    }
    if let Some(sandbox) = cli.sandbox_mode.as_ref() {
        fields.insert("sandbox".into(), serde_json::json!(sandbox));
    }
    anyhow::ensure!(
        retained_key.is_none() || fields.is_empty(),
        "--operation-key recovers the original admitted intent; omit newly supplied model/provider/policy/sandbox options, inspect its receipt, or start a fresh control without the retained key"
    );
    Ok(fields)
}

fn apply_thread_control_mutation_options(
    request: codewhale_app_server::ThreadRequest,
    options: &serde_json::Map<String, serde_json::Value>,
) -> Result<codewhale_app_server::ThreadRequest> {
    if options.is_empty() {
        return Ok(request);
    }
    anyhow::ensure!(
        matches!(
            &request,
            codewhale_app_server::ThreadRequest::Resume(_)
                | codewhale_app_server::ThreadRequest::Fork(_)
        ),
        "history proposal requires Resume/Fork"
    );
    let mut value = serde_json::to_value(request)?;
    value
        .as_object_mut()
        .context("typed history control must be an object")?
        .extend(options.clone());
    serde_json::from_value(value)
        .context("invalid explicit typed history proposal (values omitted)")
}

fn thread_control_selection(
    cli: &Cli,
    startup: &Path,
) -> Option<codewhale_app_server::ThreadControlSelection> {
    (cli.workspace.is_some() || cli.profile.is_some() || cli.config.is_some()).then(|| {
        codewhale_app_server::ThreadControlSelection {
            workspace: cli
                .workspace
                .as_ref()
                .map(|path| resolve_against_workspace(path, startup)),
            config_profile: cli.profile.clone(),
            config_source: cli
                .config
                .as_ref()
                .map(|path| resolve_against_workspace(path, startup)),
        }
    })
}

fn thread_control_request(command: &ThreadCommand) -> Result<codewhale_app_server::ThreadRequest> {
    use codewhale_app_server::{
        ThreadListParams, ThreadReadParams, ThreadRequest, ThreadSetNameParams,
    };
    Ok(match command {
        ThreadCommand::List { all, limit } => ThreadRequest::List(ThreadListParams {
            include_archived: *all,
            limit: *limit,
        }),
        ThreadCommand::Read { thread_id } => ThreadRequest::Read(ThreadReadParams {
            thread_id: thread_id.clone(),
        }),
        ThreadCommand::Archive { thread_id } => ThreadRequest::Archive {
            thread_id: thread_id.clone(),
        },
        ThreadCommand::Unarchive { thread_id } => ThreadRequest::Unarchive {
            thread_id: thread_id.clone(),
        },
        ThreadCommand::SetName { thread_id, name } => ThreadRequest::SetName(ThreadSetNameParams {
            thread_id: thread_id.clone(),
            name: name.clone(),
        }),
        ThreadCommand::ClearName { thread_id } => ThreadRequest::SetName(ThreadSetNameParams {
            thread_id: thread_id.clone(),
            name: String::new(),
        }),
        ThreadCommand::Resume {
            thread_id,
            operation_key,
        } => serde_json::from_value(
            serde_json::json!({"kind":"resume","thread_id":thread_id,"operation_key":operation_key}),
        )?,
        ThreadCommand::Fork {
            thread_id,
            operation_key,
        } => serde_json::from_value(
            serde_json::json!({"kind":"fork","thread_id":thread_id,"operation_key":operation_key}),
        )?,
    })
}

fn run_thread_control_command_with<F>(mut command: ThreadCommand, control: F) -> Result<()>
where
    F: FnOnce(codewhale_app_server::ThreadRequest) -> Result<codewhale_app_server::ThreadResponse>,
{
    let operation = match &mut command {
        ThreadCommand::Resume { operation_key, .. } | ThreadCommand::Fork { operation_key, .. } => {
            Some(
                operation_key
                    .get_or_insert_with(codewhale_app_server::capture_thread_operation_key)
                    .clone(),
            )
        }
        _ => None,
    };
    let request = thread_control_request(&command)?;
    let response = control(request).with_context(|| {
        operation.as_ref().map_or_else(|| "canonical thread control failed".to_owned(), |key|
            format!("canonical control outcome may have committed; inspect or retry with --operation-key {key}, no automatic replay"))
    })?;
    if response.status == "missing" {
        bail!("thread not found: {}", response.thread_id);
    }
    let expected = match &command {
        ThreadCommand::List { .. } => "list",
        ThreadCommand::Read { thread_id }
        | ThreadCommand::Resume { thread_id, .. }
        | ThreadCommand::Archive { thread_id }
        | ThreadCommand::Unarchive { thread_id }
        | ThreadCommand::SetName { thread_id, .. }
        | ThreadCommand::ClearName { thread_id } => thread_id,
        ThreadCommand::Fork { .. } => response
            .data
            .get("receipt")
            .and_then(|value| value.get("runtime_thread_id"))
            .and_then(serde_json::Value::as_str)
            .context("canonical fork result has no committed target receipt")?,
    };
    anyhow::ensure!(
        response.thread_id == expected,
        "canonical control returned another thread identity"
    );
    if let Some(operation) = operation.as_ref() {
        let receipt = response
            .data
            .get("receipt")
            .context("canonical control has no durable receipt")?;
        anyhow::ensure!(
            receipt
                .get("operation_key")
                .and_then(serde_json::Value::as_str)
                == Some(operation.as_str()),
            "canonical control returned another intent receipt; retain --operation-key {operation} for inspection, no replay"
        );
        anyhow::ensure!(
            receipt
                .get("runtime_thread_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| !id.is_empty())
                && receipt
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| !id.is_empty()),
            "canonical committed target/session identity missing; retain --operation-key {operation}"
        );
    }
    match command {
        ThreadCommand::List { .. } => {
            for thread in response.threads {
                println!(
                    "{} | {} | {} | {}",
                    thread.id,
                    thread.name.as_deref().unwrap_or("(unnamed)"),
                    thread.model_provider,
                    thread.cwd.display()
                );
            }
        }
        ThreadCommand::Read { .. } => println!("{}", serde_json::to_string_pretty(&response)?),
        ThreadCommand::Archive { thread_id } => println!("archived {thread_id}"),
        ThreadCommand::Unarchive { thread_id } => println!("unarchived {thread_id}"),
        ThreadCommand::SetName { thread_id, .. } => println!("renamed {thread_id}"),
        ThreadCommand::ClearName { thread_id } => println!("cleared name for {thread_id}"),
        ThreadCommand::Resume { .. } | ThreadCommand::Fork { .. } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&response.data["receipt"])?
            );
        }
    }
    Ok(())
}

fn run_sandbox_command(command: SandboxCommand) -> Result<()> {
    match command {
        SandboxCommand::Check { command, ask } => {
            let engine = ExecPolicyEngine::new(Vec::new(), vec!["rm -rf".to_string()]);
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let decision = engine.check(ExecPolicyContext {
                command: &command,
                cwd: &cwd.display().to_string(),
                tool: Some("exec_shell"),
                path: None,
                ask_for_approval: ask.into(),
                sandbox_mode: Some("workspace-write"),
            })?;
            println!("{}", serde_json::to_string_pretty(&decision)?);
            Ok(())
        }
    }
}

fn run_app_server_command(
    cli: &Cli,
    resolved_runtime: &ResolvedRuntimeOptions,
    args: AppServerArgs,
) -> Result<()> {
    let mut startup = cli.runtime_options.clone();
    startup.config = app_server_config_path(cli, &args);
    startup.control_frontend = if args.stdio {
        Some(RuntimeControlFrontend::Stdio)
    } else if args.socket {
        Some(RuntimeControlFrontend::Socket {
            path: args.socket_path.clone(),
        })
    } else if !args.http && !args.mobile {
        Some(RuntimeControlFrontend::LegacyHttp)
    } else {
        None
    };
    let mut launch = args;
    if launch.port.is_none() {
        launch.port = Some(if launch.stdio || launch.socket {
            0
        } else if launch.http || launch.mobile {
            7878
        } else {
            8787
        });
    }
    if !launch.http && !launch.mobile {
        launch.auth_token = launch.auth_token.or_else(app_server_token_from_env);
    }
    let argv = app_server_serve_passthrough(&launch);
    apply_tui_env(cli, resolved_runtime, &argv);
    let code = codewhale_tui::run(startup, argv);
    std::process::exit(if code == std::process::ExitCode::SUCCESS {
        0
    } else {
        1
    })
}

/// The config file an in-process app-server loads: the subcommand's own
/// `--config`, else the global one.
fn app_server_config_path(cli: &Cli, args: &AppServerArgs) -> Option<PathBuf> {
    args.config.clone().or_else(|| cli.config.clone())
}

/// Build the `serve` argv forwarded to the TUI binary for
/// `codewhale app-server --http`/`--mobile`. Maps app-server flags onto the
/// matching `serve` flags (note `--insecure-no-auth` → `--insecure`). The
/// subcommand-level `--config` is bridged through the global `--config` in the
/// dispatcher, so it is intentionally not part of this passthrough. An auth
/// token from the compatibility environment is retained in this same-process
/// argument vector; no child process or owner discovery receipt receives it.
/// Canonical Runtime environment resolution remains in the Runtime API.
fn app_server_serve_passthrough(args: &AppServerArgs) -> Vec<String> {
    let mut forwarded = vec!["serve".to_string()];
    forwarded.push(if args.mobile { "--mobile" } else { "--http" }.to_string());
    if let Some(host) = args.host.as_ref() {
        forwarded.push("--host".to_string());
        forwarded.push(host.clone());
    }
    if let Some(port) = args.port {
        forwarded.push("--port".to_string());
        forwarded.push(port.to_string());
    }
    if let Some(workers) = args.workers {
        forwarded.push("--workers".to_string());
        forwarded.push(workers.to_string());
    }
    for origin in &args.cors_origin {
        forwarded.push("--cors-origin".to_string());
        forwarded.push(origin.clone());
    }
    if let Some(token) = args.auth_token.as_ref() {
        forwarded.push("--auth-token".to_string());
        forwarded.push(token.clone());
    }
    if args.insecure_no_auth {
        forwarded.push("--insecure".to_string());
    }
    if args.qr {
        forwarded.push("--qr".to_string());
    }
    forwarded
}

fn web_serve_passthrough(args: &WebArgs) -> Vec<String> {
    vec![
        "serve".to_string(),
        "--web".to_string(),
        "--port".to_string(),
        args.port.to_string(),
    ]
}

fn app_server_token_from_env() -> Option<String> {
    std::env::var("CODEWHALE_APP_SERVER_TOKEN")
        .ok()
        .or_else(|| std::env::var("DEEPSEEK_APP_SERVER_TOKEN").ok())
}

fn run_resume_command(
    cli: &Cli,
    resolved_runtime: &ResolvedRuntimeOptions,
    args: TuiPassthroughArgs,
) -> Result<()> {
    let passthrough = tui_args("resume", args);
    if should_pick_resume_in_dispatcher(&passthrough, cfg!(windows)) {
        return run_dispatcher_resume_picker(cli, resolved_runtime);
    }
    run_tui_in_process(cli, resolved_runtime, passthrough)
}

fn run_dispatcher_resume_picker(
    cli: &Cli,
    resolved_runtime: &ResolvedRuntimeOptions,
) -> Result<()> {
    let argv = vec!["sessions".to_string()];
    apply_tui_env(cli, resolved_runtime, &argv);
    let code = codewhale_tui::run(cli.runtime_options.clone(), argv);
    if code != std::process::ExitCode::SUCCESS {
        std::process::exit(if code == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        })
    }

    println!();
    println!("Windows note: enter a session id or prefix from the list above.");
    println!("You can also run `codewhale resume --last` to skip this prompt.");
    print!("Session id/prefix (Enter to cancel): ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read session selection")?;
    let session_id = input.trim();
    if session_id.is_empty() {
        bail!("No session selected.");
    }

    run_tui_in_process(
        cli,
        resolved_runtime,
        vec!["resume".to_string(), session_id.to_string()],
    )
}

fn should_pick_resume_in_dispatcher(passthrough: &[String], is_windows: bool) -> bool {
    is_windows && passthrough == ["resume"]
}

fn run_tui_in_process(
    cli: &Cli,
    resolved_runtime: &ResolvedRuntimeOptions,
    passthrough: Vec<String>,
) -> Result<()> {
    let argv = passthrough.clone();
    apply_tui_env(cli, resolved_runtime, &passthrough);
    let code = codewhale_tui::run(cli.runtime_options.clone(), argv);
    std::process::exit(if code == std::process::ExitCode::SUCCESS {
        0
    } else {
        1
    })
}

fn run_tui_server_in_process(
    cli: &Cli,
    resolved_runtime: &ResolvedRuntimeOptions,
    passthrough: Vec<String>,
) -> Result<()> {
    let argv = passthrough.clone();
    apply_tui_env(cli, resolved_runtime, &passthrough);
    let code = codewhale_tui::run(cli.runtime_options.clone(), argv);
    std::process::exit(if code == std::process::ExitCode::SUCCESS {
        0
    } else {
        1
    })
}

/// Set one process environment variable for the CLI-to-TUI bridge.
///
/// Callers must guarantee no concurrent environment access: production
/// callers run pre-runtime on the main thread, and tests serialize on the
/// shared env lock. All current callers are inside [`apply_tui_env`].
fn set_tui_env(key: impl AsRef<std::ffi::OsStr>, value: impl AsRef<std::ffi::OsStr>) {
    // SAFETY: no concurrent environment access. Production setters run on
    // the main thread before the TUI runtime starts, and the only other
    // thread that may be alive is the detached telemetry writer, which
    // never reads or writes the process environment. Tests serialize on
    // the shared env lock instead.
    unsafe {
        std::env::set_var(key, value);
    }
}

fn apply_tui_env(cli: &Cli, resolved_runtime: &ResolvedRuntimeOptions, passthrough: &[String]) {
    let mut verbosity = if cli.profile.is_some() {
        cli.verbosity.clone()
    } else {
        resolved_runtime.verbosity.clone()
    };
    if verbosity.is_none()
        && passthrough
            .iter()
            .any(|arg| matches!(arg.as_str(), "exec" | "eval"))
    {
        verbosity = Some("concise".to_string());
    }
    let uses_raw_tui_provider = cli
        .provider
        .as_deref()
        .is_some_and(|provider| builtin_provider_arg(provider).is_none());
    let keyring_bridge_provider = resolved_runtime.provider;
    let keyring_bridge_api_key = resolved_runtime.api_key.as_ref();
    let keyring_bridge_source = resolved_runtime.api_key_source;
    if let Some(provider) = cli.provider.as_deref() {
        let provider = builtin_provider_arg(provider).map_or_else(
            || provider.to_string(),
            |provider| provider.as_str().to_string(),
        );
        set_tui_env("CODEWHALE_PROVIDER", provider);
    }
    if !(uses_raw_tui_provider
        || (cli.profile.is_some()
            && matches!(resolved_runtime.provider_source, ProviderSource::Config)))
        && matches!(keyring_bridge_source, Some(RuntimeApiKeySource::Keyring))
        && let Some(api_key) = keyring_bridge_api_key
    {
        for var in provider_env_vars(keyring_bridge_provider) {
            set_tui_env(var, api_key);
        }
        set_tui_env(
            codewhale_config::CLI_API_KEY_SOURCE_ENV,
            RuntimeApiKeySource::Keyring.as_env_value(),
        );
    }
    if let Some(model) = cli.model.as_ref() {
        set_tui_env("CODEWHALE_MODEL", model);
    }
    if let Some(v) = verbosity.as_ref() {
        set_tui_env("CODEWHALE_VERBOSITY", v);
    }
    if let Some(log_level) = cli.log_level.as_ref() {
        set_tui_env("CODEWHALE_LOG_LEVEL", log_level);
    }
    let telemetry = resolved_runtime.telemetry.to_string();
    set_tui_env("CODEWHALE_TELEMETRY", telemetry);
    let floor = cli.telemetry == Some(false) || codewhale_config::telemetry_floor_in_force();
    set_tui_env(
        codewhale_config::TELEMETRY_FLOOR_ENV,
        if floor { "1" } else { "0" },
    );
    if let Some(endpoint) = resolved_runtime.telemetry_endpoint.as_ref() {
        set_tui_env("CODEWHALE_TELEMETRY_ENDPOINT", endpoint);
    }
    if let Some(policy) = cli.approval_policy.as_ref() {
        set_tui_env("CODEWHALE_APPROVAL_POLICY", policy);
    }
    if let Some(mode) = cli.sandbox_mode.as_ref() {
        set_tui_env("CODEWHALE_SANDBOX_MODE", mode);
    }
    if cli.yolo {
        set_tui_env("CODEWHALE_YOLO", "true");
    }
    if let Some(api_key) = cli.api_key.as_ref() {
        set_tui_env(codewhale_config::CLI_API_KEY_ENV, api_key);
        if !uses_raw_tui_provider && (cli.profile.is_none() || cli.provider.is_some()) {
            for var in provider_env_vars(resolved_runtime.provider) {
                set_tui_env(var, api_key);
            }
        }
        set_tui_env(codewhale_config::CLI_API_KEY_SOURCE_ENV, "cli");
    }
    if let Some(base_url) = cli.base_url.as_ref() {
        set_tui_env("CODEWHALE_BASE_URL", base_url);
    }
}

// There is deliberately no "just run the TUI with these args" helper here. One
// existed, `thread resume`/`thread fork` used it, and it forwarded neither
// `--config` nor the resolved telemetry value — so the kill switch the
// dispatcher had already applied never reached the process that emits. Every
// delegation is now in-process, and
// `only_one_function_may_locate_and_spawn_the_tui` pins that.

fn run_providers_command(args: ProvidersArgs) -> Result<()> {
    match args.command {
        ProvidersCommand::Export { json } => {
            if !json {
                bail!("`codewhale providers export` requires `--json`");
            }
            let export = ProvidersExport::from_registry(env!("CODEWHALE_BUILD_VERSION"));
            serde_json::to_writer_pretty(io::stdout(), &export)
                .context("failed to write providers export")?;
            println!();
            Ok(())
        }
    }
}

fn run_metrics_command(args: MetricsArgs) -> Result<()> {
    let since = match args.since.as_deref() {
        Some(s) => {
            Some(metrics::parse_since(s).with_context(|| format!("invalid --since value: {s:?}"))?)
        }
        None => None,
    };
    metrics::run(metrics::MetricsArgs {
        json: args.json,
        since,
    })
}

/// Maximum bytes read for an API key on stdin. Keys are short; anything
/// larger is a piped file, not a key.
const MAX_STDIN_API_KEY_BYTES: u64 = 8 * 1024;

fn read_api_key_from_stdin() -> Result<String> {
    let mut input = String::new();
    io::stdin()
        .take(MAX_STDIN_API_KEY_BYTES + 1)
        .read_to_string(&mut input)
        .context("failed to read api key from stdin")?;
    if input.len() as u64 > MAX_STDIN_API_KEY_BYTES {
        bail!("API key on stdin exceeds the 8 KiB limit");
    }
    let key = input.trim().to_string();
    if key.is_empty() {
        bail!("empty API key provided");
    }
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use codewhale_config::{ModelSource, ProviderSource};
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};

    fn parse_ok(argv: &[&str]) -> Cli {
        let mut cli = Cli::try_parse_from(argv)
            .unwrap_or_else(|err| panic!("parse failed for {argv:?}: {err}"));
        preserve_exec_separator(&mut cli, argv);
        cli
    }

    /// `lane logs --tail` reads backwards; a line split across a chunk
    /// boundary must come back whole, and the handle must end at EOF.
    #[test]
    fn lane_log_tail_reads_whole_lines_across_chunk_boundaries() {
        use std::io::Seek;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lane.log");
        let body: String = (0..50).map(|i| format!("line-{i:03}\n\n")).collect();
        std::fs::write(&path, &body).unwrap();
        for chunk in [1, 3, 7, 64, 4096] {
            for tail in [0, 1, 2, 49, 50, 80] {
                let mut file = std::fs::File::open(&path).unwrap();
                let got = read_tail_lines(&mut file, tail, chunk).unwrap();
                let want: Vec<Vec<u8>> = (50usize.saturating_sub(tail)..50)
                    .map(|i| format!("line-{i:03}").into_bytes())
                    .collect();
                assert_eq!(got, want, "chunk {chunk}, tail {tail}");
                assert_eq!(file.stream_position().unwrap(), body.len() as u64);
            }
        }
    }

    fn help_for(argv: &[&str]) -> String {
        let err = Cli::try_parse_from(argv).expect_err("expected --help to short-circuit parsing");
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        err.to_string()
    }

    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) struct ScopedEnvVar {
        name: &'static str,
        previous: Option<OsString>,
    }

    impl ScopedEnvVar {
        pub(crate) fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(name);
            // Safety: tests using this helper serialize with env_lock() and
            // restore the original value in Drop.
            unsafe { std::env::set_var(name, value) };
            Self { name, previous }
        }

        pub(crate) fn remove(name: &'static str) -> Self {
            let previous = std::env::var_os(name);
            // Safety: tests using this helper serialize with env_lock() and
            // restore the original value in Drop.
            unsafe { std::env::remove_var(name) };
            Self { name, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            // Safety: tests using this helper serialize with env_lock().
            unsafe {
                if let Some(previous) = self.previous.take() {
                    std::env::set_var(self.name, previous.clone());
                } else {
                    std::env::remove_var(self.name);
                }
            }
        }
    }

    #[derive(Default)]
    struct RecordingKeyringStore {
        gets: Mutex<Vec<String>>,
        values: Mutex<std::collections::BTreeMap<String, String>>,
        fail_delete_slot: Option<&'static str>,
        /// A slot whose reads fail (a locked or access-denied keyring entry).
        fail_get_slot: Option<&'static str>,
    }

    impl RecordingKeyringStore {
        fn set_value(&self, key: &str, value: &str) {
            self.values
                .lock()
                .expect("recording values lock")
                .insert(key.to_string(), value.to_string());
        }

        fn queried(&self) -> Vec<String> {
            self.gets.lock().expect("recording gets lock").clone()
        }
    }

    impl codewhale_secrets::KeyringStore for RecordingKeyringStore {
        fn get(
            &self,
            key: &str,
        ) -> std::result::Result<Option<String>, codewhale_secrets::SecretsError> {
            self.gets
                .lock()
                .expect("recording gets lock")
                .push(key.to_string());
            if self.fail_get_slot == Some(key) {
                return Err(codewhale_secrets::SecretsError::Keyring(
                    "test read failure".into(),
                ));
            }
            Ok(self
                .values
                .lock()
                .expect("recording values lock")
                .get(key)
                .cloned())
        }

        fn set(
            &self,
            key: &str,
            value: &str,
        ) -> std::result::Result<(), codewhale_secrets::SecretsError> {
            self.set_value(key, value);
            Ok(())
        }

        fn delete(&self, key: &str) -> std::result::Result<(), codewhale_secrets::SecretsError> {
            if self.fail_delete_slot == Some(key) {
                return Err(codewhale_secrets::SecretsError::Keyring(
                    "test delete failure".into(),
                ));
            }
            self.values
                .lock()
                .expect("recording values lock")
                .remove(key);
            Ok(())
        }

        fn backend_name(&self) -> &'static str {
            "recording"
        }
    }

    fn install_fake_tui_binary() -> (tempfile::TempDir, ScopedEnvVar) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let custom = dir
            .path()
            .join(format!("custom-tui{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&custom, b"").unwrap();
        let custom_str = custom.to_string_lossy();
        let bin = ScopedEnvVar::set("DEEPSEEK_TUI_BIN", &custom_str);
        (dir, bin)
    }

    fn resolved_runtime_for_test(
        provider: ProviderKind,
        provider_source: ProviderSource,
    ) -> ResolvedRuntimeOptions {
        ResolvedRuntimeOptions {
            provider,
            provider_source,
            model: "test-model".to_string(),
            model_source: ModelSource::ProviderDefault,
            api_key: None,
            api_key_source: None,
            base_url: "http://localhost:8000/v1".to_string(),
            auth_mode: None,
            insecure_skip_tls_verify: false,
            log_level: None,
            telemetry: false,
            telemetry_source: codewhale_config::TelemetrySource::Default,
            telemetry_explicit_off: false,
            telemetry_endpoint: None,
            approval_policy: None,
            sandbox_mode: None,
            yolo: None,
            verbosity: None,
            http_headers: std::collections::BTreeMap::new(),
            route: Err(codewhale_config::route::RouteError::EmptyModel),
        }
    }

    #[test]
    fn tui_credential_handoff_stays_with_the_selected_provider() {
        let _lock = env_lock();
        let mut names = ProviderKind::ALL
            .into_iter()
            .flat_map(provider_env_vars)
            .copied()
            .collect::<Vec<_>>();
        names.extend([
            codewhale_config::CLI_API_KEY_ENV,
            codewhale_config::CLI_API_KEY_SOURCE_ENV,
            codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV,
            "CODEWHALE_PROVIDER",
            "DEEPSEEK_PROVIDER",
            "CODEWHALE_TELEMETRY",
            "DEEPSEEK_TELEMETRY",
            codewhale_config::TELEMETRY_FLOOR_ENV,
        ]);
        names.sort_unstable();
        names.dedup();
        let _clean_env = names
            .into_iter()
            .map(ScopedEnvVar::remove)
            .collect::<Vec<_>>();
        let _deepseek_key = ScopedEnvVar::set("DEEPSEEK_API_KEY", "existing-deepseek-key");

        let clear_bridge = || {
            // Safety: this test holds env_lock() and the guards above restore
            // every touched variable.
            unsafe {
                for var in ProviderKind::ALL
                    .into_iter()
                    .flat_map(provider_env_vars)
                    .filter(|var| **var != "DEEPSEEK_API_KEY")
                {
                    std::env::remove_var(var);
                }
                std::env::remove_var(codewhale_config::CLI_API_KEY_ENV);
                std::env::remove_var(codewhale_config::CLI_API_KEY_SOURCE_ENV);
                std::env::remove_var(codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV);
            }
        };

        for (provider_arg, provider) in [
            ("nvidia-nim", ProviderKind::NvidiaNim),
            ("openrouter", ProviderKind::Openrouter),
            ("anthropic", ProviderKind::Anthropic),
        ] {
            clear_bridge();
            let keyring_key = format!("{provider_arg}-keyring-key");
            let mut keyring_runtime = resolved_runtime_for_test(provider, ProviderSource::Cli);
            keyring_runtime.api_key = Some(keyring_key.clone());
            keyring_runtime.api_key_source = Some(RuntimeApiKeySource::Keyring);
            let keyring_cli = parse_ok(&["codewhale", "--provider", provider_arg]);

            apply_tui_env(&keyring_cli, &keyring_runtime, &[]);

            assert_eq!(
                std::env::var("DEEPSEEK_API_KEY").as_deref(),
                Ok("existing-deepseek-key"),
                "{provider_arg} keyring handoff replaced DeepSeek's credential"
            );
            for var in provider_env_vars(provider) {
                assert_eq!(
                    std::env::var(var).as_deref(),
                    Ok(keyring_key.as_str()),
                    "{provider_arg} keyring handoff missed {var}"
                );
            }
            assert_eq!(
                std::env::var(codewhale_config::CLI_API_KEY_SOURCE_ENV).as_deref(),
                Ok("keyring")
            );
            assert!(std::env::var(codewhale_config::CLI_API_KEY_ENV).is_err());
            assert!(
                std::env::var(codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV).is_err(),
                "new dispatchers must not write the retired vendor-named marker"
            );

            clear_bridge();
            let explicit_key = format!("{provider_arg}-explicit-key");
            let explicit_cli = parse_ok(&[
                "codewhale",
                "--provider",
                provider_arg,
                "--api-key",
                explicit_key.as_str(),
            ]);
            let explicit_runtime = resolved_runtime_for_test(provider, ProviderSource::Cli);

            apply_tui_env(&explicit_cli, &explicit_runtime, &[]);

            assert_eq!(
                std::env::var("DEEPSEEK_API_KEY").as_deref(),
                Ok("existing-deepseek-key"),
                "{provider_arg} explicit CLI credential handoff replaced DeepSeek's credential"
            );
            for var in provider_env_vars(provider) {
                assert_eq!(
                    std::env::var(var).as_deref(),
                    Ok(explicit_key.as_str()),
                    "{provider_arg} explicit CLI credential handoff missed {var}"
                );
            }
            assert_eq!(
                std::env::var(codewhale_config::CLI_API_KEY_ENV).as_deref(),
                Ok(explicit_key.as_str())
            );
            assert_eq!(
                std::env::var(codewhale_config::CLI_API_KEY_SOURCE_ENV).as_deref(),
                Ok("cli")
            );
            assert!(std::env::var(codewhale_config::LEGACY_CLI_API_KEY_SOURCE_ENV).is_err());
        }
    }

    #[test]
    fn yolo_flag_writes_only_the_codewhale_env_var() {
        let _lock = env_lock();
        let _guards = [
            ScopedEnvVar::remove("CODEWHALE_TELEMETRY"),
            ScopedEnvVar::remove("DEEPSEEK_TELEMETRY"),
            ScopedEnvVar::remove(codewhale_config::TELEMETRY_FLOOR_ENV),
            ScopedEnvVar::remove("CODEWHALE_YOLO"),
            ScopedEnvVar::remove("DEEPSEEK_YOLO"),
        ];

        let cli = parse_ok(&["codewhale", "--yolo"]);
        let runtime = resolved_runtime_for_test(ProviderKind::NvidiaNim, ProviderSource::Cli);
        apply_tui_env(&cli, &runtime, &[]);

        assert_eq!(
            std::env::var("CODEWHALE_YOLO").as_deref(),
            Ok("true"),
            "--yolo must still enable the posture via CODEWHALE_YOLO"
        );
        assert!(
            std::env::var("DEEPSEEK_YOLO").is_err(),
            "--yolo must not write the retired DEEPSEEK_YOLO alias (#5443)"
        );
    }

    #[test]
    fn clap_command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    // Regression for #767: `run_cli` prints the full anyhow chain so users
    // see the underlying TOML parser error (line/column, expected token)
    // instead of just the top-level "failed to parse config at <path>"
    // wrapper. anyhow's bare `Display` impl drops the chain — pin both
    // pieces here so a future refactor of the printing path doesn't
    // silently regress.
    #[test]
    fn anyhow_chain_surfaces_toml_parse_cause() {
        use anyhow::Context;
        let inner = anyhow::anyhow!("TOML parse error at line 1, column 20");
        let err = Err::<(), _>(inner)
            .context("failed to parse config at C:\\Users\\test\\.deepseek\\config.toml")
            .unwrap_err();

        // What `eprintln!("error: {err}")` prints (top context only).
        assert_eq!(
            err.to_string(),
            "failed to parse config at C:\\Users\\test\\.deepseek\\config.toml",
        );

        // What the `for cause in err.chain().skip(1)` loop iterates over.
        let causes: Vec<String> = err.chain().skip(1).map(ToString::to_string).collect();
        assert_eq!(causes, vec!["TOML parse error at line 1, column 20"]);
    }

    #[test]
    fn parses_config_command_matrix() {
        let cli = parse_ok(&["deepseek", "config", "get", "provider"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Config(ConfigArgs {
                command: ConfigCommand::Get { ref key }
            })) if key == "provider"
        ));

        let cli = parse_ok(&["deepseek", "config", "set", "model", "deepseek-v4-flash"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Config(ConfigArgs {
                command: ConfigCommand::Set { ref key, ref value }
            })) if key == "model" && value == "deepseek-v4-flash"
        ));

        let cli = parse_ok(&["deepseek", "config", "unset", "model"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Config(ConfigArgs {
                command: ConfigCommand::Unset { ref key }
            })) if key == "model"
        ));

        assert!(matches!(
            parse_ok(&["deepseek", "config", "list"]).command,
            Some(Commands::Config(ConfigArgs {
                command: ConfigCommand::List
            }))
        ));
        assert!(matches!(
            parse_ok(&["deepseek", "config", "path"]).command,
            Some(Commands::Config(ConfigArgs {
                command: ConfigCommand::Path
            }))
        ));
        assert!(matches!(
            parse_ok(&["codewhale", "config", "edit"]).command,
            Some(Commands::Config(ConfigArgs {
                command: ConfigCommand::Edit
            }))
        ));
        assert!(matches!(
            parse_ok(&["codewhale", "config", "doctor"]).command,
            Some(Commands::Config(ConfigArgs {
                command: ConfigCommand::Doctor
            }))
        ));
        assert!(matches!(
            parse_ok(&["codewhale", "config", "dump"]).command,
            Some(Commands::Config(ConfigArgs {
                command: ConfigCommand::Dump
            }))
        ));
    }

    #[test]
    fn parses_repeatable_global_set_overrides() {
        let cli = parse_ok(&[
            "codewhale",
            "--set",
            "verbosity=concise",
            "--set",
            "model=deepseek-v4-flash",
            "config",
            "get",
            "verbosity",
        ]);
        assert_eq!(
            cli.overrides,
            vec![
                "verbosity=concise".to_string(),
                "model=deepseek-v4-flash".to_string()
            ]
        );
    }

    #[test]
    fn config_doctor_is_clean_on_minimal_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        write_config_fixture(&path, "verbosity = \"concise\"\n");
        let store = ConfigStore::load(Some(path)).expect("load fixture");
        run_config_doctor(&store).expect("clean doctor");
    }

    #[test]
    fn stream_config_commands_preserve_saved_values_and_apply_per_run_overrides() {
        let _env = env_lock();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        write_config_fixture(&path, "[stream]\nopen_timeout_secs=70\nmax_resumes=6\n");
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        run_config_command(
            &mut store,
            ConfigCommand::Set {
                key: "stream.tcp_keepalive_secs".into(),
                value: "17".into(),
            },
            false,
            &[],
        )
        .unwrap();
        store.reload().unwrap();
        assert_eq!(
            store.config.extras["stream"]["tcp_keepalive_secs"].as_integer(),
            Some(17)
        );
        run_config_command(
            &mut store,
            ConfigCommand::Unset {
                key: "stream.tcp_keepalive_secs".into(),
            },
            false,
            &[],
        )
        .unwrap();
        store.reload().unwrap();
        assert!(
            store.config.extras["stream"]
                .get("tcp_keepalive_secs")
                .is_none()
        );
        let original = std::fs::read(&path).unwrap();
        let overlays = ["stream.open_timeout_secs=120".into()];
        apply_per_run_overrides(&mut store, &overlays).unwrap();
        run_config_command(&mut store, ConfigCommand::Dump, false, &overlays).unwrap();
        run_config_command(
            &mut store,
            ConfigCommand::Get {
                key: "stream.open_timeout_secs".into(),
            },
            false,
            &overlays,
        )
        .unwrap();
        let stream = codewhale_tui::config_keys::resolved_stream_config(&store.config).unwrap();
        assert_eq!(stream["open_timeout_secs"].as_integer(), Some(120));
        assert_eq!(stream["max_resumes"].as_integer(), Some(6));
        assert_eq!(
            std::fs::read(path).unwrap(),
            original,
            "per-run overlay does not persist"
        );
        assert!(
            apply_per_run_overrides(&mut store, &["stream.open_timout_secs=90".into()]).is_err()
        );
    }

    #[test]
    fn config_doctor_reports_unread_keys_without_failing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        write_config_fixture(
            &path,
            "zzz_unknown = 1\ncalm_mode = \"false\"\nmax_subagents = 4\n",
        );
        let store = ConfigStore::load(Some(path)).expect("load fixture");
        let unread = codewhale_tui::config_keys::unread_config_keys(
            store.config.extras.keys().map(String::as_str),
        );
        assert_eq!(unread.len(), 2, "{unread:#?}");
        assert!(
            unread
                .iter()
                .any(|line| line.contains("`calm_mode` belongs in settings.toml")),
            "{unread:#?}"
        );
        assert!(
            unread
                .iter()
                .any(|line| line.contains("`zzz_unknown` is not read by anything")),
            "{unread:#?}"
        );
        // Warnings, not errors: the keys are preserved and the check passes.
        run_config_doctor(&store).expect("unread keys warn, they do not fail");
    }

    #[test]
    fn config_set_refuses_unknown_keys_and_routes_settings_to_settings_toml() {
        let _env = env_lock();
        let home = tempfile::tempdir().expect("isolated home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.path().to_string_lossy());
        let _config_path = ScopedEnvVar::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_config_path = ScopedEnvVar::remove("DEEPSEEK_CONFIG_PATH");
        let path = home.path().join("config.toml");
        // A stale settings key left in config.toml by 0.10.0 (#6563).
        let original = "verbosity = \"normal\"\ncalm_mode = \"flase\"\n";
        write_config_fixture(&path, original);
        let settings_path = home.path().join("settings.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("load fixture");
        let set = |store: &mut ConfigStore, key: &str, value: &str| {
            run_config_command(
                store,
                ConfigCommand::Set {
                    key: key.into(),
                    value: value.into(),
                },
                false,
                &[],
            )
        };

        let error = set(&mut store, "totally_bogus_key", "42").expect_err("unknown key");
        assert!(
            format!("{error:#}").contains("unknown config key `totally_bogus_key`"),
            "{error:#}"
        );
        let error = set(&mut store, "calm_mod", "on").expect_err("typo");
        assert!(
            format!("{error:#}").contains("Did you mean `calm_mode`?"),
            "{error:#}"
        );
        // A settings.toml key with a bad value is refused by its validator.
        set(&mut store, "calm_mode", "flase").expect_err("invalid boolean");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!settings_path.exists(), "refusals write nothing");

        set(&mut store, "calm_mode", "off").expect("settings key routes");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let settings = std::fs::read_to_string(&settings_path).expect("settings.toml written");
        assert!(settings.contains("calm_mode = false"), "{settings}");
        // `config get` answers from settings.toml, not the stale copy.
        assert_eq!(settings_key_value(&store, "calm_mode").unwrap(), "false");
        run_config_command(
            &mut store,
            ConfigCommand::Get {
                key: "calm_mode".into(),
            },
            false,
            &[],
        )
        .expect("get settings key");

        // config.toml keys still land in config.toml.
        set(&mut store, "skills_dir", "/tmp/skills").expect("config key");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("skills_dir"),
        );

        // Typed TUI fields keep their type, so the TUI's strict parse of the
        // whole file still succeeds; values their reader refuses are refused.
        set(&mut store, "yolo", "true").expect("typed bool");
        set(&mut store, "max_subagents", "4").expect("typed integer");
        set(&mut store, "reasoning_effort", "none").expect("reader alias");
        let before_refusals = std::fs::read_to_string(&path).unwrap();
        set(&mut store, "max_subagents", "lots").expect_err("not an integer");
        set(&mut store, "reasoning_effort", "sideways").expect_err("unknown effort");
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written, before_refusals, "refusals write nothing");
        let document: toml::Table = toml::from_str(&written).expect("config.toml parses");
        assert_eq!(document["yolo"], toml::Value::Boolean(true), "{written}");
        assert_eq!(
            document["max_subagents"],
            toml::Value::Integer(4),
            "{written}"
        );
        assert_eq!(
            document["reasoning_effort"],
            toml::Value::String("off".into()),
            "{written}"
        );
    }

    #[test]
    fn project_scoped_config_refuses_user_global_settings_keys() {
        let _env = env_lock();
        let home = tempfile::tempdir().expect("isolated home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.path().to_string_lossy());
        let _config_path = ScopedEnvVar::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_config_path = ScopedEnvVar::remove("DEEPSEEK_CONFIG_PATH");
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::create_dir_all(workspace.path().join(".git")).expect("checkout marker");
        let project_path = workspace.path().join(".codewhale/config.toml");
        write_config_fixture(&project_path, "verbosity = \"normal\"\n");
        let mut store = ConfigStore::load(Some(project_path.clone())).expect("load project");

        for command in [
            ConfigCommand::Set {
                key: "calm_mode".into(),
                value: "on".into(),
            },
            ConfigCommand::Get {
                key: "calm_mode".into(),
            },
        ] {
            let error = run_config_command(&mut store, command, true, &[])
                .expect_err("settings keys have no project scope");
            assert!(
                format!("{error:#}").contains("has no project scope"),
                "{error:#}"
            );
        }
        assert!(
            !home.path().join("settings.toml").exists(),
            "the user-global settings.toml is untouched"
        );
        assert_eq!(
            std::fs::read_to_string(&project_path).unwrap(),
            "verbosity = \"normal\"\n"
        );
    }

    #[test]
    fn config_doctor_fails_on_empty_secret_and_bad_url() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        // The top-level endpoint is checked where it now lives (#6394); an
        // empty top-level key would simply be dropped, so the empty key is a
        // table value here.
        write_config_fixture(
            &path,
            "base_url = \"gopher://x\"\n\n[providers.deepseek]\napi_key = \"\"\n",
        );
        let store = ConfigStore::load(Some(path)).expect("load fixture");
        let error = run_config_doctor(&store).expect_err("doctor must fail");
        let message = format!("{error:#}");
        assert!(
            message.contains("api_key") && message.contains("empty"),
            "{message}"
        );
        assert!(
            message.contains("base_url") && message.contains("http"),
            "{message}"
        );
    }

    #[test]
    fn per_run_overrides_apply_in_memory_and_never_save() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        write_config_fixture(&path, "verbosity = \"normal\"\n");
        let mut store = ConfigStore::load(Some(path.clone())).expect("load fixture");
        apply_per_run_overrides(&mut store, &["verbosity=concise".to_string()])
            .expect("overlay applies");
        assert_eq!(store.config.verbosity.as_deref(), Some("concise"));
        let before = toml::to_string(&store.config).unwrap();
        let bytes_before = std::fs::read(&path).unwrap();
        let token = ["sk-live-", "Z7qX4mNb2Vc9Lk3PwR8t"].concat();
        for (key, value) in [
            ("approval_policy", "ask"),
            ("sandbox_mode", "full"),
            ("verbosity", "quiet"),
            ("approval_policy", token.as_str()),
            ("sandbox_mode", token.as_str()),
            ("verbosity", token.as_str()),
        ] {
            let error = apply_per_run_overrides(&mut store, &[format!("{key}={value}")])
                .expect_err("invalid overlay");
            let rendered = format!("{error:#}");
            assert!(!rendered.contains(&token), "{rendered}");
            assert!(rendered.contains("fix: codewhale config set"), "{rendered}");
            assert_eq!(toml::to_string(&store.config).unwrap(), before);
            assert_eq!(std::fs::read(&path).unwrap(), bytes_before);
        }
        // Malformed input and an unsupported dotted key must not reintroduce
        // credential-shaped text through the outer context or nested-key help.
        for spec in [token.clone(), format!("{token}.unknown=normal")] {
            let error = apply_per_run_overrides(&mut store, &[spec]).expect_err("invalid overlay");
            let rendered = format!("{error:#}");
            assert!(!rendered.contains(&token), "{rendered}");
            assert!(rendered.contains("invalid --set"), "{rendered}");
            assert_eq!(toml::to_string(&store.config).unwrap(), before);
            assert_eq!(std::fs::read(&path).unwrap(), bytes_before);
        }
        let error = apply_per_run_overrides(&mut store, &["no-equals-here".to_string()])
            .expect_err("missing = must fail");
        assert!(format!("{error:#}").contains("KEY=VALUE"));
        // Nothing was saved: a reload sees the file, not the overlay.
        let reloaded = ConfigStore::load(Some(path)).expect("reload");
        assert_eq!(reloaded.config.verbosity.as_deref(), Some("normal"));
    }

    #[test]
    fn unsupported_nested_config_set_preserves_original_file_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let original =
            "# Keep this comment and spacing\n[tools]\nuser_input_timeout_seconds = 7 # fixture\n";
        write_config_fixture(&path, original);
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        let err = run_config_command(
            &mut store,
            ConfigCommand::Set {
                key: "tools.user_input_timeout_seconds".into(),
                value: "0".into(),
            },
            false,
            &[],
        )
        .unwrap_err();
        assert!(err.to_string().contains("[tools]"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn mutating_config_commands_refuse_per_run_overrides() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("config.toml");
        write_config_fixture(&path, "verbosity = \"normal\"\n");
        let mut store = ConfigStore::load(Some(path)).expect("load fixture");
        let overrides = vec!["verbosity=concise".to_string()];
        let error = run_config_command(
            &mut store,
            ConfigCommand::Set {
                key: "verbosity".to_string(),
                value: "concise".to_string(),
            },
            false,
            &overrides,
        )
        .expect_err("set with --set must refuse");
        assert!(format!("{error:#}").contains("--set"), "{error:#}");
        // Reads still work under an overlay.
        run_config_command(&mut store, ConfigCommand::List, false, &overrides)
            .expect("list with --set");
    }

    fn config_dispatch_from(
        argv: &[OsString],
        cwd: &Path,
    ) -> (Option<PathBuf>, ConfigCommand, bool) {
        let matches = Cli::command()
            .try_get_matches_from(argv.iter().cloned())
            .unwrap_or_else(|error| panic!("config command should parse: {error}"));
        let project_bundle_scope = config_command_targets_project(&matches);
        let cli = Cli::from_arg_matches(&matches)
            .unwrap_or_else(|error| panic!("config command should decode: {error}"));
        let selected_path =
            config_store_path_for_dispatch(cli.config.clone(), project_bundle_scope, cwd);
        let Some(Commands::Config(ConfigArgs { command })) = cli.command else {
            panic!("expected config command");
        };
        (selected_path, command, project_bundle_scope)
    }

    fn write_config_fixture(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().expect("config should have a parent"))
            .expect("create config parent");
        std::fs::write(path, body).expect("write config fixture");
    }

    #[test]
    fn project_config_dispatch_prefers_current_app_dir_and_falls_back_to_legacy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let current = workspace.join(".codewhale/config.toml");
        let legacy = workspace.join(".deepseek/config.toml");

        // Fresh workspace: create under the current app dir.
        std::fs::create_dir_all(&workspace).expect("workspace");
        assert_eq!(
            config_store_path_for_dispatch(None, true, &workspace),
            Some(current.clone())
        );

        // Legacy-only workspace: operate on the legacy document in place.
        write_config_fixture(&legacy, "verbosity = \"legacy\"\n");
        assert_eq!(
            config_store_path_for_dispatch(None, true, &workspace),
            Some(legacy.clone())
        );

        // Both present: the current app dir wins, matching the loader.
        write_config_fixture(&current, "verbosity = \"current\"\n");
        assert_eq!(
            config_store_path_for_dispatch(None, true, &workspace),
            Some(current.clone())
        );

        // An explicit --config path always wins; without --project nothing is selected.
        let explicit = temp.path().join("explicit.toml");
        assert_eq!(
            config_store_path_for_dispatch(Some(explicit.clone()), true, &workspace),
            Some(explicit)
        );
        assert_eq!(
            config_store_path_for_dispatch(None, false, &workspace),
            None
        );
    }

    #[test]
    fn project_config_import_dispatches_to_the_cwd_document() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".git")).expect("create checkout marker");
        let project_path = workspace.join(".codewhale/config.toml");
        let global_path = temp.path().join("global-config.toml");
        write_config_fixture(&project_path, "verbosity = \"project-before\"\n");
        write_config_fixture(&global_path, "verbosity = \"global-only\"\n");

        let bundle_path = temp.path().join("project-bundle.toml");
        std::fs::write(
            &bundle_path,
            r#"schema_version = 1
kind = "codewhale.portable-config"

[project]
verbosity = "concise"
"#,
        )
        .expect("write project bundle");
        let argv = [
            OsString::from("codewhale"),
            OsString::from("config"),
            OsString::from("import"),
            bundle_path.as_os_str().to_owned(),
            OsString::from("--yes"),
            OsString::from("--project"),
        ];
        let (selected_path, command, project_bundle_scope) =
            config_dispatch_from(&argv, &workspace);
        assert_eq!(selected_path.as_deref(), Some(project_path.as_path()));

        let mut store = ConfigStore::load(selected_path).expect("load selected project config");
        run_config_command(&mut store, command, project_bundle_scope, &[])
            .expect("import project bundle");
        let project = ConfigStore::load(Some(project_path.clone())).expect("reload project");
        let global = ConfigStore::load(Some(global_path.clone())).expect("reload global");
        assert_eq!(project.config.verbosity.as_deref(), Some("concise"));
        assert_eq!(global.config.verbosity.as_deref(), Some("global-only"));

        let explicit_argv = [
            OsString::from("codewhale"),
            OsString::from("--config"),
            global_path.as_os_str().to_owned(),
            OsString::from("config"),
            OsString::from("import"),
            bundle_path.as_os_str().to_owned(),
            OsString::from("--yes"),
            OsString::from("--project"),
        ];
        let global_before = std::fs::read(&global_path).expect("read global before refusal");
        let (selected_path, command, project_bundle_scope) =
            config_dispatch_from(&explicit_argv, &workspace);
        assert_eq!(selected_path.as_deref(), Some(global_path.as_path()));
        let mut explicit_store =
            ConfigStore::load(selected_path).expect("load explicit global config");
        let error = run_config_command(&mut explicit_store, command, project_bundle_scope, &[])
            .expect_err("project import must reject an explicit non-workspace config");
        assert!(
            error
                .to_string()
                .contains("--project requires a workspace config"),
            "{error:#}"
        );
        assert_eq!(
            std::fs::read(&global_path).expect("read global after refusal"),
            global_before
        );
    }

    #[test]
    fn project_config_export_reads_the_cwd_document() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".git")).expect("create checkout marker");
        let project_path = workspace.join(".codewhale/config.toml");
        let global_path = temp.path().join("global-config.toml");
        let output_path = temp.path().join("portable.toml");
        write_config_fixture(&project_path, "verbosity = \"project-only\"\n");
        write_config_fixture(&global_path, "verbosity = \"global-only\"\n");

        let argv = [
            OsString::from("codewhale"),
            OsString::from("config"),
            OsString::from("export"),
            OsString::from("--portable"),
            OsString::from("--project"),
            OsString::from("--out"),
            output_path.as_os_str().to_owned(),
        ];
        let (selected_path, command, project_bundle_scope) =
            config_dispatch_from(&argv, &workspace);
        assert_eq!(selected_path.as_deref(), Some(project_path.as_path()));

        let mut store = ConfigStore::load(selected_path).expect("load selected project config");
        run_config_command(&mut store, command, project_bundle_scope, &[])
            .expect("export project bundle");
        let body = std::fs::read_to_string(&output_path).expect("read portable export");
        let bundle = config_bundles::parse_bundle_str(&body, "portable.toml")
            .expect("parse portable export");
        assert_eq!(
            bundle
                .project
                .entries
                .get("verbosity")
                .and_then(toml::Value::as_str),
            Some("project-only")
        );
        assert!(bundle.global.entries.is_empty());

        let explicit_output_path = temp.path().join("explicit-portable.toml");
        let explicit_argv = [
            OsString::from("codewhale"),
            OsString::from("--config"),
            global_path.as_os_str().to_owned(),
            OsString::from("config"),
            OsString::from("export"),
            OsString::from("--portable"),
            OsString::from("--project"),
            OsString::from("--out"),
            explicit_output_path.as_os_str().to_owned(),
        ];
        let global_before = std::fs::read(&global_path).expect("read global before refusal");
        let (selected_path, command, project_bundle_scope) =
            config_dispatch_from(&explicit_argv, &workspace);
        assert_eq!(selected_path.as_deref(), Some(global_path.as_path()));
        let mut explicit_store =
            ConfigStore::load(selected_path).expect("load explicit global config");
        let error = run_config_command(&mut explicit_store, command, project_bundle_scope, &[])
            .expect_err("project export must reject an explicit non-workspace config");
        assert!(
            error
                .to_string()
                .contains("--project requires a workspace config"),
            "{error:#}"
        );
        assert!(!explicit_output_path.exists());
        assert_eq!(
            std::fs::read(&global_path).expect("read global after refusal"),
            global_before
        );
    }

    #[test]
    fn parses_update_beta_flag() {
        let cli = parse_ok(&["codewhale", "update"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Update(UpdateArgs {
                beta: false,
                check: false,
                proxy: None
            }))
        ));

        let cli = parse_ok(&["codewhale", "update", "--beta"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Update(UpdateArgs {
                beta: true,
                check: false,
                proxy: None
            }))
        ));

        let cli = parse_ok(&["codewhale", "update", "--check"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Update(UpdateArgs {
                beta: false,
                check: true,
                proxy: None
            }))
        ));

        let cli = parse_ok(&["codewhale", "update", "--proxy", "socks5://127.0.0.1:1080"]);
        let Some(Commands::Update(args)) = cli.command else {
            panic!("expected update command");
        };
        assert!(!args.beta);
        assert!(!args.check);
        assert_eq!(args.proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
    }

    #[test]
    fn parses_model_command_matrix() {
        let cli = parse_ok(&["deepseek", "model", "list"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Model(ModelArgs {
                command: ModelCommand::List { provider: None }
            }))
        ));

        let cli = parse_ok(&["deepseek", "model", "list", "--provider", "openai"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Model(ModelArgs {
                command: ModelCommand::List {
                    provider: Some(ProviderKind::Openai)
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "model", "resolve", "deepseek-v4-flash"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Model(ModelArgs {
                command: ModelCommand::Resolve {
                    model: Some(ref model),
                    provider: None
                }
            })) if model == "deepseek-v4-flash"
        ));

        let cli = parse_ok(&[
            "deepseek",
            "model",
            "resolve",
            "--provider",
            "deepseek",
            "deepseek-v4-pro",
        ]);
        assert!(matches!(
            cli.command,
            Some(Commands::Model(ModelArgs {
                command: ModelCommand::Resolve {
                    model: Some(ref model),
                    provider: Some(ProviderKind::Deepseek)
                }
            })) if model == "deepseek-v4-pro"
        ));

        let cli = parse_ok(&["deepseek", "model", "set", "pro"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Model(ModelArgs {
                command: ModelCommand::Set { ref model }
            })) if model == "pro"
        ));
    }

    #[test]
    fn model_command_provider_hint_uses_subcommand_then_top_level_provider() {
        assert_eq!(
            model_command_provider_hint(None, Some(ProviderKind::Zai)),
            Some(ProviderKind::Zai)
        );
        assert_eq!(
            model_command_provider_hint(Some(ProviderKind::Minimax), Some(ProviderKind::Zai)),
            Some(ProviderKind::Minimax)
        );
        assert_eq!(model_command_provider_hint(None, None), None);

        let cli = parse_ok(&["codewhale", "--provider", "zai", "model", "list"]);
        assert_eq!(cli.provider.as_deref(), Some("zai"));
        assert!(matches!(
            cli.command,
            Some(Commands::Model(ModelArgs {
                command: ModelCommand::List { provider: None }
            }))
        ));
    }

    #[test]
    fn durable_cli_route_edits_use_canonical_config_and_keep_temporary_overrides_unsaved() {
        let _env = env_lock();
        let home = tempfile::tempdir().expect("isolated home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.path().to_string_lossy());
        let _config_path = ScopedEnvVar::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_config_path = ScopedEnvVar::remove("DEEPSEEK_CONFIG_PATH");
        let path = home.path().join("config.toml");
        std::fs::write(&path, "provider = \"deepseek\"\ndefault_text_model = \"deepseek-v4-pro\"\n[providers.zai]\nmodel = \"GLM-5.2\"\n").unwrap();
        let settings_path = home.path().join("settings.toml");
        let settings = "default_provider = \"zai\"\n[provider_models]\nzai = \"GLM-5.3\"\n";
        std::fs::write(&settings_path, settings).unwrap();
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        let runtime = resolved_runtime_for_test(ProviderKind::Deepseek, ProviderSource::Config);
        run_model_command(
            &mut store,
            ModelCommand::Set {
                model: "GLM-5.2".into(),
            },
            None,
            &runtime,
        )
        .unwrap();
        assert_eq!(store.config.provider, ProviderKind::Zai);
        assert_eq!(store.config.providers.zai.model.as_deref(), Some("GLM-5.2"));
        assert_eq!(
            store.config.extras["route_preferences_version"].as_integer(),
            Some(1)
        );

        run_config_command(
            &mut store,
            ConfigCommand::Set {
                key: "default_text_model".into(),
                value: "GLM-5.1".into(),
            },
            false,
            &[],
        )
        .unwrap();
        assert_eq!(store.config.providers.zai.model.as_deref(), Some("GLM-5.1"));
        assert_eq!(
            codewhale_tui::route_preferences::get(&path, "model")
                .unwrap()
                .as_deref(),
            Some("GLM-5.1")
        );
        run_config_command(
            &mut store,
            ConfigCommand::Unset {
                key: "providers.zai.model".into(),
            },
            false,
            &[],
        )
        .unwrap();
        assert!(store.config.providers.zai.model.is_none());
        assert_eq!(std::fs::read_to_string(settings_path).unwrap(), settings);

        let before = std::fs::read(&path).unwrap();
        let overrides = vec!["model=temporary-model".to_string()];
        assert!(
            run_config_command(
                &mut store,
                ConfigCommand::Set {
                    key: "model".into(),
                    value: "GLM-5.2".into(),
                },
                false,
                &overrides
            )
            .is_err()
        );
        apply_per_run_overrides(&mut store, &overrides).unwrap();
        assert_eq!(store.config.model.as_deref(), Some("temporary-model"));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn model_set_canonicalizes_deepseek_vision_aliases() {
        for alias in ["flash-vision", "deepseek-v4flashvisionexp"] {
            assert_eq!(
                canonical_model_for_set(alias),
                "deepseek-v4-flash-vision-exp"
            );
        }
        assert_eq!(
            canonical_model_for_set("deepseek-v4-flash-vision-exp"),
            "deepseek-v4-flash-vision-exp"
        );
    }

    #[test]
    fn model_set_keeps_short_names_literal_off_deepseek_routes() {
        let _env = env_lock();
        let home = tempfile::tempdir().expect("isolated home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.path().to_string_lossy());
        let _config_path = ScopedEnvVar::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_config_path = ScopedEnvVar::remove("DEEPSEEK_CONFIG_PATH");
        let path = home.path().join("config.toml");
        for (provider, expected) in [
            (ProviderKind::Openai, "pro"),
            (ProviderKind::Anthropic, "pro"),
            (ProviderKind::Deepseek, "deepseek-v4-pro"),
            // Hosts that serve DeepSeek but do not resolve `pro` themselves.
            (ProviderKind::Openrouter, "deepseek-v4-pro"),
            (ProviderKind::Together, "deepseek-v4-pro"),
        ] {
            std::fs::write(&path, format!("provider = \"{}\"\n", provider.as_str())).unwrap();
            let mut store = ConfigStore::load(Some(path.clone())).unwrap();
            let runtime = resolved_runtime_for_test(provider, ProviderSource::Config);
            run_model_command(
                &mut store,
                ModelCommand::Set {
                    model: "pro".into(),
                },
                None,
                &runtime,
            )
            .unwrap();
            assert_eq!(store.config.provider, provider);
            assert_eq!(
                store
                    .config
                    .providers
                    .for_provider(provider)
                    .model
                    .as_deref(),
                Some(expected),
                "{provider:?}"
            );
        }
    }

    #[test]
    fn model_set_in_a_workspace_config_follows_the_effective_route() {
        let _env = env_lock();
        let home = tempfile::tempdir().expect("isolated home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.path().to_string_lossy());
        let _config_path = ScopedEnvVar::remove("CODEWHALE_CONFIG_PATH");
        let _legacy_config_path = ScopedEnvVar::remove("DEEPSEEK_CONFIG_PATH");
        // A project config that names no provider: its root `model` applies to
        // the route in effect (OpenAI here, from the global config), not to
        // the DeepSeek default this file alone would suggest.
        let repo = home.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join(".codewhale")).unwrap();
        let path = repo.join(".codewhale").join("config.toml");
        assert!(codewhale_config::config_path_is_workspace_scoped(&path));
        for (provider, expected) in [
            (ProviderKind::Openai, "pro"),
            (ProviderKind::Deepseek, "deepseek-v4-pro"),
        ] {
            std::fs::write(&path, "").unwrap();
            let mut store = ConfigStore::load(Some(path.clone())).unwrap();
            let mut runtime = resolved_runtime_for_test(provider, ProviderSource::Config);
            runtime.base_url = codewhale_config::provider::provider_for_kind(provider)
                .default_base_url()
                .to_string();
            run_model_command(
                &mut store,
                ModelCommand::Set {
                    model: "pro".into(),
                },
                None,
                &runtime,
            )
            .unwrap();
            let saved = std::fs::read_to_string(&path).unwrap();
            assert!(
                saved.contains(&format!("model = \"{expected}\"")),
                "{provider:?}: {saved}"
            );
        }
    }

    #[test]
    fn thread_commands_refuse_unknown_ids_instead_of_reporting_success() {
        for command in [
            ThreadCommand::Archive {
                thread_id: "missing".into(),
            },
            ThreadCommand::Unarchive {
                thread_id: "missing".into(),
            },
            ThreadCommand::Read {
                thread_id: "missing".into(),
            },
        ] {
            let error = run_thread_control_command_with(command, |_| {
                serde_json::from_value(serde_json::json!({
                    "thread_id":"missing","status":"missing","threads":[],"events":[],"data":{}
                }))
                .map_err(Into::into)
            })
            .expect_err("unknown canonical thread must fail");
            assert!(format!("{error:#}").contains("thread not found: missing"));
        }
    }

    #[test]
    fn thread_fork_validates_new_owner_receipt_instead_of_parent_identity() {
        let cli = parse_ok(&[
            "codewhale",
            "thread",
            "fork",
            "parent",
            "--operation-key",
            "same-intent",
        ]);
        let Some(Commands::Thread(ThreadArgs { command })) = cli.command else {
            panic!("thread fork")
        };
        let calls = std::cell::Cell::new(0usize);
        run_thread_control_command_with(command, |request| {
            calls.set(calls.get()+1);
            let codewhale_app_server::ThreadRequest::Fork(params) = request else { panic!("fork request") };
            assert_eq!(params.thread_id, "parent");
            assert_eq!(params.operation_key.as_deref(), Some("same-intent"));
            serde_json::from_value(serde_json::json!({"thread_id":"child","status":"forked",
                "data":{"receipt":{"runtime_thread_id":"child","session_id":"child-session","operation_key":"same-intent"}}}))
                .map_err(Into::into)
        }).unwrap();
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn thread_resume_uncertainty_retains_one_client_key_and_never_replays() {
        let calls = std::cell::Cell::new(0usize);
        let captured = std::cell::RefCell::new(String::new());
        let error = run_thread_control_command_with(
            ThreadCommand::Resume {
                thread_id: "parent".into(),
                operation_key: None,
            },
            |request| {
                calls.set(calls.get() + 1);
                let codewhale_app_server::ThreadRequest::Resume(params) = request else {
                    panic!("resume request")
                };
                *captured.borrow_mut() = params.operation_key.unwrap();
                bail!("selected owner closed after admission")
            },
        )
        .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert!(!captured.borrow().is_empty());
        assert!(format!("{error:#}").contains(&format!("--operation-key {}", captured.borrow())));
        assert!(format!("{error:#}").contains("no automatic replay"));
    }

    #[test]
    fn thread_control_startup_selection_resolves_only_explicit_paths() {
        let cli = parse_ok(&[
            "codewhale",
            "--workspace",
            "selected",
            "--profile",
            "reviewed",
            "--config",
            "chosen.toml",
            "thread",
            "list",
        ]);
        let startup = Path::new("/captured-startup");
        let selected = thread_control_selection(&cli, startup).unwrap();
        assert_eq!(selected.workspace, Some(startup.join("selected")));
        assert_eq!(selected.config_profile.as_deref(), Some("reviewed"));
        assert_eq!(selected.config_source, Some(startup.join("chosen.toml")));
        let cli = parse_ok(&["codewhale", "--profile", "reviewed", "thread", "list"]);
        let selected = thread_control_selection(&cli, startup).unwrap();
        assert!(selected.workspace.is_none() && selected.config_source.is_none());
    }

    #[test]
    fn thread_history_cli_normalized_route_and_policy_reach_typed_owner_request() {
        let mut cli = parse_ok(&[
            "codewhale",
            "--provider",
            "owned-route",
            "--model",
            "explicit-model",
            "--set",
            "default_text_model=set-model",
            "--set",
            "approval_policy=on-request",
            "--set",
            "sandbox_mode=workspace-write",
            "thread",
            "resume",
            "source",
        ]);
        apply_runtime_set_overrides(&mut cli).unwrap();
        assert!(
            top_level_provider_override(cli.provider.as_deref(), cli.command.as_ref())
                .unwrap()
                .is_none()
        );
        assert!(
            prepare_raw_provider_tui_dispatch(
                &cli,
                cli.command.as_ref(),
                &CliRuntimeOverrides::default()
            )
            .unwrap()
            .is_none()
        );
        let Some(Commands::Thread(args)) = cli.command.as_ref() else {
            panic!("thread command");
        };
        let options = thread_control_mutation_options(&cli, &args.command).unwrap();
        let request = apply_thread_control_mutation_options(
            thread_control_request(&args.command).unwrap(),
            &options,
        )
        .unwrap();
        let codewhale_app_server::ThreadRequest::Resume(params) = request else {
            panic!("typed Resume");
        };
        assert_eq!(params.model.as_deref(), Some("explicit-model"));
        assert_eq!(params.model_provider.as_deref(), Some("owned-route"));
        assert_eq!(params.approval_policy.as_deref(), Some("on-request"));
        assert_eq!(params.sandbox.as_deref(), Some("workspace-write"));
        assert!(
            params.config.is_none() && params.path.is_none(),
            "no resolved Config or ambient path is copied"
        );
        let cli = parse_ok(&[
            "codewhale",
            "--provider",
            "openai-codex",
            "thread",
            "fork",
            "source",
        ]);
        let Some(Commands::Thread(args)) = cli.command.as_ref() else {
            panic!("thread command");
        };
        let options = thread_control_mutation_options(&cli, &args.command).unwrap();
        assert_eq!(
            options["model_provider"],
            builtin_provider_arg("openai-codex").unwrap().as_str()
        );
    }

    #[test]
    fn thread_history_cli_refuses_credentials_and_unsupported_settings_without_values() {
        for argv in [
            vec![
                "codewhale",
                "--api-key",
                "private-control-sentinel",
                "thread",
                "resume",
                "source",
            ],
            vec![
                "codewhale",
                "--base-url",
                "https://private-control-sentinel.invalid",
                "thread",
                "fork",
                "source",
            ],
            vec![
                "codewhale",
                "--set",
                "api_key=private-control-sentinel",
                "thread",
                "resume",
                "source",
            ],
            vec![
                "codewhale",
                "--set",
                "telemetry=true",
                "thread",
                "fork",
                "source",
            ],
        ] {
            let cli = parse_ok(&argv);
            let Some(Commands::Thread(args)) = cli.command.as_ref() else {
                panic!("thread command");
            };
            let error = thread_control_mutation_options(&cli, &args.command).unwrap_err();
            let text = format!("{error:#}");
            assert!(text.contains("owning Runtime") && !text.contains("private-control-sentinel"));
        }
    }

    #[test]
    fn thread_history_cli_retained_key_refuses_new_policy_or_route_proposal() {
        for flag in [
            "--model",
            "--provider",
            "--approval-policy",
            "--sandbox-mode",
        ] {
            let value = match flag {
                "--provider" => "owned-route",
                "--model" => "another-model",
                "--approval-policy" => "on-request",
                _ => "workspace-write",
            };
            let cli = parse_ok(&[
                "codewhale",
                flag,
                value,
                "thread",
                "resume",
                "source",
                "--operation-key",
                "retained-key",
            ]);
            let Some(Commands::Thread(args)) = cli.command.as_ref() else {
                panic!("thread command");
            };
            assert!(
                format!(
                    "{:#}",
                    thread_control_mutation_options(&cli, &args.command).unwrap_err()
                )
                .contains("original admitted intent")
            );
        }
        let cli = parse_ok(&[
            "codewhale",
            "thread",
            "fork",
            "source",
            "--operation-key",
            "retained-key",
        ]);
        let Some(Commands::Thread(args)) = cli.command.as_ref() else {
            panic!("thread command");
        };
        assert!(
            thread_control_mutation_options(&cli, &args.command)
                .unwrap()
                .is_empty()
        );
        let request = thread_control_request(&args.command).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap()["operation_key"],
            "retained-key"
        );
    }

    #[test]
    fn thread_control_without_selection_never_mints_ambient_workspace() {
        let cli = parse_ok(&["codewhale", "thread", "list"]);
        assert!(thread_control_selection(&cli, Path::new("/unrelated-startup")).is_none());
    }

    #[test]
    fn thread_control_refuses_a_receipt_from_another_intent() {
        let error = run_thread_control_command_with(ThreadCommand::Fork {
            thread_id:"parent".into(), operation_key:Some("expected-intent".into())
        }, |_| serde_json::from_value(serde_json::json!({"thread_id":"child","status":"forked",
            "data":{"receipt":{"runtime_thread_id":"child","session_id":"child-session","operation_key":"foreign-intent"}}})).map_err(Into::into))
            .unwrap_err();
        assert!(format!("{error:#}").contains("another intent receipt"));
        assert!(format!("{error:#}").contains("expected-intent"));
    }

    #[test]
    fn app_server_in_process_transports_honor_the_global_config() {
        let cli = parse_ok(&[
            "codewhale",
            "--config",
            "/tmp/global-config.toml",
            "app-server",
            "--stdio",
        ]);
        let Some(Commands::AppServer(args)) = &cli.command else {
            panic!("expected app-server");
        };
        assert_eq!(
            app_server_config_path(&cli, args),
            Some(PathBuf::from("/tmp/global-config.toml"))
        );

        let cli = parse_ok(&[
            "codewhale",
            "--config",
            "/tmp/global-config.toml",
            "app-server",
            "--config",
            "/tmp/app-server-config.toml",
            "--socket",
        ]);
        let Some(Commands::AppServer(args)) = &cli.command else {
            panic!("expected app-server");
        };
        assert_eq!(
            app_server_config_path(&cli, args),
            Some(PathBuf::from("/tmp/app-server-config.toml"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn inline_lane_start_exits_nonzero_when_the_lane_fails() {
        let _env = env_lock();
        let home = tempfile::tempdir().expect("isolated home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.path().to_string_lossy());
        let request = |script: &str| LaneStartRequest {
            workflow: None,
            fleet: None,
            issue: None,
            goal: None,
            runtime: "inline".to_string(),
            worktree_repo: None,
            branch: None,
            worktree_path: None,
            worktree_ttl_secs: None,
            command: vec!["sh".into(), "-c".into(), script.to_string()],
            environment: Vec::new(),
            cwd: None,
        };
        let error = start_lane(request("exit 3")).expect_err("a failed inline lane must fail");
        assert!(format!("{error:#}").contains(" failed"), "{error:#}");
        start_lane(request("exit 0")).expect("a completed inline lane succeeds");
    }

    #[test]
    fn interactive_api_key_prompt_reads_through_the_hidden_reader() {
        let key = read_prompted_api_key(
            "deepseek",
            true,
            |prompt| {
                assert_eq!(prompt, "Enter API key for deepseek: ");
                Ok("  sk-hidden-fixture \n".to_string())
            },
            || panic!("terminal input must not use the plain reader"),
        )
        .unwrap();
        assert_eq!(key, "sk-hidden-fixture");
        let key = read_prompted_api_key(
            "deepseek",
            false,
            |_| panic!("piped input has no terminal to hide"),
            || Ok("sk-piped-fixture".to_string()),
        )
        .unwrap();
        assert_eq!(key, "sk-piped-fixture");
        assert!(
            read_prompted_api_key("deepseek", true, |_| Ok("  \n".into()), || unreachable!())
                .is_err()
        );
    }

    #[test]
    fn hidden_key_prompt_uses_a_terminal_stream_when_stderr_is_redirected() {
        // `read_secure_line` on a non-terminal stream returns "" without
        // reading, so `auth set 2>err.log` must prompt on stdout instead.
        assert_eq!(hidden_prompt_stream(true, true), Some(PromptStream::Stderr));
        assert_eq!(
            hidden_prompt_stream(true, false),
            Some(PromptStream::Stderr)
        );
        assert_eq!(
            hidden_prompt_stream(false, true),
            Some(PromptStream::Stdout)
        );
        assert_eq!(hidden_prompt_stream(false, false), None);
    }

    #[test]
    fn auth_clear_fails_when_the_secret_store_keeps_the_key() {
        use codewhale_secrets::{KeyringStore, SecretsError};
        use std::sync::Arc;

        struct UndeletableStore(Option<&'static str>);

        impl KeyringStore for UndeletableStore {
            fn get(&self, _key: &str) -> Result<Option<String>, SecretsError> {
                Ok(self.0.map(str::to_string))
            }

            fn set(&self, _key: &str, _value: &str) -> Result<(), SecretsError> {
                Err(SecretsError::ReadOnly)
            }

            fn delete(&self, _key: &str) -> Result<(), SecretsError> {
                Err(SecretsError::Keyring("test delete failure".to_string()))
            }

            fn backend_name(&self) -> &'static str {
                "undeletable test store"
            }
        }

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("config.toml");
        let clear = |held: Option<&'static str>| {
            let mut store = ConfigStore::load(Some(path.clone())).expect("load config");
            store.config.providers.deepseek.api_key = Some("sk-config-fixture".to_string());
            store.save().unwrap();
            let secrets = Secrets::new(Arc::new(UndeletableStore(held)));
            let outcome = run_auth_command_with_secrets(
                &mut store,
                AuthCommand::Clear {
                    provider: ProviderKind::Deepseek,
                },
                &secrets,
            );
            assert!(store.config.providers.deepseek.api_key.is_none());
            outcome
        };

        let error = clear(Some("sk-keyring-fixture")).expect_err("a kept key is not cleared");
        let message = format!("{error:#}");
        assert!(message.contains("refused the delete"), "{message}");
        assert!(!message.contains("sk-keyring-fixture"), "{message}");
        clear(None).expect("nothing stored means nothing left to revoke");
    }

    #[test]
    fn xai_auth_clear_reports_a_kept_key_after_the_revocation_commits() {
        use codewhale_secrets::{KeyringStore, SecretsError};
        use std::sync::Arc;

        struct UndeletableStore;

        impl KeyringStore for UndeletableStore {
            fn get(&self, _key: &str) -> Result<Option<String>, SecretsError> {
                Ok(Some("xai-keyring-fixture".to_string()))
            }

            fn set(&self, _key: &str, _value: &str) -> Result<(), SecretsError> {
                Err(SecretsError::ReadOnly)
            }

            fn delete(&self, _key: &str) -> Result<(), SecretsError> {
                Err(SecretsError::Keyring("test delete failure".to_string()))
            }

            fn backend_name(&self) -> &'static str {
                "undeletable test store"
            }
        }

        let _env = env_lock();
        let home = tempfile::tempdir().expect("isolated home");
        // The owned credentials directory is opened without following links,
        // so the home must not sit behind one (macOS `/var` -> `/private/var`).
        let home_path = home.path().canonicalize().expect("canonical home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home_path.to_string_lossy());
        let path = home_path.join("config.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("load config");
        store.config.providers.xai.api_key = Some("xai-config-fixture".to_string());
        store.config.providers.xai.auth_mode = Some("api_key".to_string());
        store.save().unwrap();
        let secrets = Secrets::new(Arc::new(UndeletableStore));
        let error = run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Clear {
                provider: ProviderKind::Xai,
            },
            &secrets,
        )
        .expect_err("a kept xAI key is not cleared");
        let message = format!("{error:#}");
        assert!(message.contains("refused the delete"), "{message}");
        assert!(!message.contains("xai-keyring-fixture"), "{message}");
        // The error is raised after the transaction commits, so the saved
        // config leg is not rolled back.
        let saved = ConfigStore::load(Some(path)).expect("reload config");
        assert!(saved.config.providers.xai.api_key.is_none());
        assert!(saved.config.providers.xai.auth_mode.is_none());
    }

    #[test]
    fn parses_thread_command_matrix() {
        let cli = parse_ok(&["deepseek", "thread", "list", "--all", "--limit", "50"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Thread(ThreadArgs {
                command: ThreadCommand::List {
                    all: true,
                    limit: Some(50)
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "thread", "read", "thread-1"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Thread(ThreadArgs {
                command: ThreadCommand::Read { ref thread_id }
            })) if thread_id == "thread-1"
        ));

        let cli = parse_ok(&["deepseek", "thread", "resume", "thread-2"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Thread(ThreadArgs {
                command: ThreadCommand::Resume { ref thread_id, operation_key: None }
            })) if thread_id == "thread-2"
        ));

        let cli = parse_ok(&["deepseek", "thread", "fork", "thread-3"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Thread(ThreadArgs {
                command: ThreadCommand::Fork { ref thread_id, operation_key: None }
            })) if thread_id == "thread-3"
        ));

        let cli = parse_ok(&["deepseek", "thread", "archive", "thread-4"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Thread(ThreadArgs {
                command: ThreadCommand::Archive { ref thread_id }
            })) if thread_id == "thread-4"
        ));

        let cli = parse_ok(&["deepseek", "thread", "unarchive", "thread-5"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Thread(ThreadArgs {
                command: ThreadCommand::Unarchive { ref thread_id }
            })) if thread_id == "thread-5"
        ));

        let cli = parse_ok(&["deepseek", "thread", "set-name", "thread-6", "My Thread"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Thread(ThreadArgs {
                command: ThreadCommand::SetName {
                    ref thread_id,
                    ref name
                }
            })) if thread_id == "thread-6" && name == "My Thread"
        ));

        let cli = parse_ok(&["deepseek", "thread", "clear-name", "thread-7"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Thread(ThreadArgs {
                command: ThreadCommand::ClearName { ref thread_id }
            })) if thread_id == "thread-7"
        ));
    }

    #[test]
    fn parses_sandbox_app_server_and_completion_matrix() {
        let cli = parse_ok(&[
            "deepseek",
            "sandbox",
            "check",
            "echo hello",
            "--ask",
            "on-failure",
        ]);
        assert!(matches!(
            cli.command,
            Some(Commands::Sandbox(SandboxArgs {
                command: SandboxCommand::Check {
                    ref command,
                    ask: ApprovalModeArg::OnFailure
                }
            })) if command == "echo hello"
        ));

        let cli = parse_ok(&[
            "deepseek",
            "app-server",
            "--host",
            "0.0.0.0",
            "--port",
            "9999",
        ]);
        assert!(matches!(
            cli.command,
            Some(Commands::AppServer(AppServerArgs {
                host: Some(ref host),
                port: Some(9999),
                stdio: false,
                http: false,
                mobile: false,
                ..
            })) if host == "0.0.0.0"
        ));

        let cli = parse_ok(&["deepseek", "app-server", "--stdio"]);
        assert!(matches!(
            cli.command,
            Some(Commands::AppServer(AppServerArgs { stdio: true, .. }))
        ));

        let cli = parse_ok(&["deepseek", "completion", "bash"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Completion { shell: Shell::Bash })
        ));
    }

    /// The `[[bin]] name` declared in this crate's manifest is the only thing a
    /// user ever types. Read it from disk rather than restating it, so renaming
    /// the binary without re-pointing the completion generator fails here
    /// instead of silently shipping a script nobody's shell loads (#5526).
    fn declared_bin_name() -> String {
        let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
            .expect("read crates/cli/Cargo.toml");
        let bin_section = manifest
            .split("[[bin]]")
            .nth(1)
            .expect("crates/cli/Cargo.toml declares a [[bin]] target");
        for line in bin_section.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("name") {
                let value = rest.trim_start().trim_start_matches('=').trim();
                return value.trim_matches('"').to_string();
            }
        }
        panic!("[[bin]] section has no name key");
    }

    #[test]
    fn completion_bin_name_matches_the_declared_bin_target() {
        assert_eq!(
            COMPLETION_BIN_NAME,
            declared_bin_name(),
            "completion scripts must register the binary this crate actually builds"
        );
    }

    /// Issue #5526: `codewhale completions <shell>` used to forward to the
    /// in-tree `codewhale-tui` binary, so every generated script registered
    /// `codewhale-tui` — not a GitHub-release command — and exposed the TUI's
    /// smaller subcommand tree. Pin the registered names per shell.
    #[test]
    fn generated_completion_scripts_register_the_published_command_names() {
        let bin = declared_bin_name();
        let alias = COMPLETION_ALIAS_NAME;

        // Match whole lines throughout: `codew` is a prefix of `codewhale`,
        // so a substring check for the alias is satisfied by the primary
        // binding and would pass on an unfixed build.
        let has_line =
            |script: &str, wanted: &str| script.lines().any(|line| line.trim() == wanted);

        let bash = render_completion_script(Shell::Bash);
        assert!(
            has_line(
                &bash,
                &format!("complete -F _{bin} -o bashdefault -o default {bin}")
            ),
            "bash script must bind the real binary name:\n{bash}"
        );
        assert!(
            has_line(
                &bash,
                &format!("complete -F _{bin} -o bashdefault -o default {alias}")
            ),
            "bash script must bind the {alias} shorthand too"
        );

        let zsh = render_completion_script(Shell::Zsh);
        assert_eq!(
            zsh.lines().next(),
            Some(format!("#compdef {bin} {alias}").as_str()),
            "zsh compdef tag line must list both published command names"
        );
        assert!(
            has_line(&zsh, &format!("compdef _{bin} {bin}")),
            "zsh script must bind {bin} on the sourced path"
        );
        assert!(
            has_line(&zsh, &format!("compdef _{bin} {alias}")),
            "zsh script must bind {alias} on the sourced path too"
        );

        let fish = render_completion_script(Shell::Fish);
        assert!(
            fish.contains(&format!("complete -c {bin} ")),
            "fish script must complete the real binary name"
        );
        assert!(
            has_line(&fish, &format!("complete -c {alias} -w {bin}")),
            "fish script must wrap the {alias} shorthand onto {bin}"
        );

        let powershell = render_completion_script(Shell::PowerShell);
        assert!(
            powershell.contains(&format!(
                "Register-ArgumentCompleter -Native -CommandName '{bin}','{alias}'"
            )),
            "PowerShell script must register both published command names"
        );

        let elvish = render_completion_script(Shell::Elvish);
        assert!(
            has_line(
                &elvish,
                &format!("set edit:completion:arg-completer[{bin}] = {{|@words|")
            ),
            "elvish script must bind the real binary name:\n{elvish}"
        );
        assert!(
            has_line(
                &elvish,
                &format!(
                    "set edit:completion:arg-completer[{alias}] = $edit:completion:arg-completer[{bin}]"
                )
            ),
            "elvish script must alias the {alias} shorthand onto {bin}"
        );

        for (shell, script) in [
            ("bash", &bash),
            ("zsh", &zsh),
            ("fish", &fish),
            ("powershell", &powershell),
            ("elvish", &elvish),
        ] {
            assert!(
                !script.contains("codewhale-tui"),
                "{shell} completions leaked the in-tree codewhale-tui name (#5526)"
            );
        }
    }

    /// The other half of #5526: the script has to describe *this* CLI's
    /// commands. Rendering from a different clap tree would drop or invent
    /// subcommands, which is exactly how the forwarded script went stale.
    #[test]
    fn generated_completion_scripts_cover_the_real_subcommand_surface() {
        let bash = render_completion_script(Shell::Bash);
        for sub in Cli::command().get_subcommands() {
            if sub.is_hide_set() {
                continue;
            }
            let name = sub.get_name();
            assert!(
                bash.contains(name),
                "bash completions omit the `{name}` subcommand"
            );
        }
    }

    /// `completions` is what the issue reporter typed and what the TUI called
    /// it; keep it working, now as an alias that renders in-process.
    #[test]
    fn completions_is_an_alias_for_completion() {
        assert!(matches!(
            parse_ok(&["codewhale", "completions", "powershell"]).command,
            Some(Commands::Completion {
                shell: Shell::PowerShell
            })
        ));
    }

    #[test]
    fn app_server_transports_are_mutually_exclusive() {
        assert!(matches!(
            parse_ok(&["deepseek", "app-server", "--http"]).command,
            Some(Commands::AppServer(AppServerArgs {
                http: true,
                mobile: false,
                stdio: false,
                ..
            }))
        ));
        assert!(matches!(
            parse_ok(&["deepseek", "app-server", "--mobile"]).command,
            Some(Commands::AppServer(AppServerArgs {
                mobile: true,
                http: false,
                stdio: false,
                ..
            }))
        ));

        assert!(matches!(
            parse_ok(&["deepseek", "app-server", "--socket"]).command,
            Some(Commands::AppServer(AppServerArgs {
                socket: true,
                socket_path: None,
                http: false,
                mobile: false,
                stdio: false,
                ..
            }))
        ));

        for argv in [
            ["deepseek", "app-server", "--http", "--mobile"].as_slice(),
            ["deepseek", "app-server", "--http", "--stdio"].as_slice(),
            ["deepseek", "app-server", "--mobile", "--stdio"].as_slice(),
            ["deepseek", "app-server", "--socket", "--stdio"].as_slice(),
            ["deepseek", "app-server", "--socket", "--http"].as_slice(),
            ["deepseek", "app-server", "--socket", "--mobile"].as_slice(),
        ] {
            let err = Cli::try_parse_from(argv).expect_err("conflicting transports must fail");
            assert_eq!(err.kind(), ErrorKind::ArgumentConflict, "argv={argv:?}");
        }
    }

    #[test]
    fn app_server_socket_path_requires_socket() {
        let err = Cli::try_parse_from(["deepseek", "app-server", "--socket-path", "/tmp/d.sock"])
            .expect_err("--socket-path without --socket must fail");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        match parse_ok(&[
            "deepseek",
            "app-server",
            "--socket",
            "--socket-path",
            "/tmp/d.sock",
        ])
        .command
        {
            Some(Commands::AppServer(AppServerArgs {
                socket: true,
                socket_path: Some(path),
                ..
            })) => assert_eq!(path, PathBuf::from("/tmp/d.sock")),
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn app_server_qr_requires_mobile() {
        let err = Cli::try_parse_from(["deepseek", "app-server", "--qr"])
            .expect_err("--qr without --mobile must fail");
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert!(matches!(
            parse_ok(&["deepseek", "app-server", "--mobile", "--qr"]).command,
            Some(Commands::AppServer(AppServerArgs {
                mobile: true,
                qr: true,
                ..
            }))
        ));
    }

    #[test]
    fn app_server_serve_passthrough_maps_flags_to_serve() {
        let args = AppServerArgs {
            http: true,
            mobile: false,
            stdio: false,
            socket: false,
            socket_path: None,
            qr: false,
            host: Some("127.0.0.1".to_string()),
            port: Some(9000),
            workers: Some(4),
            config: None,
            auth_token: Some("tok".to_string()),
            insecure_no_auth: true,
            cors_origin: vec!["http://localhost:5173".to_string()],
        };
        let argv = app_server_serve_passthrough(&args);
        let as_str: Vec<&str> = argv.iter().map(String::as_str).collect();
        // app-server's --insecure-no-auth maps onto serve's --insecure.
        assert_eq!(
            as_str,
            vec![
                "serve",
                "--http",
                "--host",
                "127.0.0.1",
                "--port",
                "9000",
                "--workers",
                "4",
                "--cors-origin",
                "http://localhost:5173",
                "--auth-token",
                "tok",
                "--insecure",
            ]
        );
    }

    #[test]
    fn app_server_serve_passthrough_mobile_defaults_are_minimal() {
        let args = AppServerArgs {
            http: false,
            mobile: true,
            stdio: false,
            socket: false,
            socket_path: None,
            qr: true,
            host: None,
            port: None,
            workers: None,
            config: None,
            auth_token: None,
            insecure_no_auth: false,
            cors_origin: vec![],
        };
        let argv = app_server_serve_passthrough(&args);
        let as_str: Vec<&str> = argv.iter().map(String::as_str).collect();
        // No host/port forwarded → serve applies its own loopback default.
        // No auth token is injected from the environment into child argv.
        assert_eq!(as_str, vec!["serve", "--mobile", "--qr"]);
    }

    #[test]
    fn web_command_is_typed_and_delegates_without_auth_material() {
        let cli = parse_ok(&["codewhale", "web", "--port", "9091"]);
        let args = match cli.command {
            Some(Commands::Web(args)) => args,
            other => panic!("expected web command, got {other:?}"),
        };
        assert_eq!(args.port, 9091);
        let forwarded = web_serve_passthrough(&args);
        assert_eq!(forwarded, ["serve", "--web", "--port", "9091"]);
        assert!(!forwarded.iter().any(|arg| arg.contains("token")));
    }

    #[test]
    fn web_command_defaults_to_runtime_port_and_documents_bootstrap_boundary() {
        let cli = parse_ok(&["codewhale", "web"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Web(WebArgs { port: 7878 }))
        ));
        let help = help_for(&["codewhale", "web", "--help"]);
        assert!(help.contains("--port"));
        assert!(help.contains("one-time loopback bootstrap"));
        assert!(!help.contains("--auth-token"));
    }

    #[test]
    fn serve_help_documents_forwarded_runtime_modes() {
        let help = help_for(&["codewhale", "serve", "--help"]);
        for flag in ["--http", "--mobile", "--web", "--mcp", "--acp"] {
            assert!(
                help.contains(flag),
                "serve help should document forwarded flag {flag}; help was:\n{help}"
            );
        }
        assert!(help.contains("compatibility"));
    }

    #[test]
    fn parses_direct_tui_command_aliases() {
        let cli = parse_ok(&["deepseek", "doctor"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Doctor(TuiPassthroughArgs { ref args })) if args.is_empty()
        ));

        let cli = parse_ok(&["deepseek", "models", "--json"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Models(TuiPassthroughArgs { ref args })) if args == &["--json"]
        ));

        let cli = parse_ok(&["deepseek", "resume", "abc123"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Resume(TuiPassthroughArgs { ref args })) if args == &["abc123"]
        ));

        let cli = parse_ok(&["deepseek", "setup", "--skills", "--local"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Setup(TuiPassthroughArgs { ref args }))
                if args == &["--skills", "--local"]
        ));

        let cli = parse_ok(&["codewhale", "fleet", "init"]);
        assert!(cli.prompt.is_empty());
        assert!(matches!(
            cli.command,
            Some(Commands::Fleet(TuiPassthroughArgs { ref args })) if args == &["init"]
        ));

        let cli = parse_ok(&[
            "codewhale",
            "fleet",
            "run",
            "tasks.json",
            "--max-workers",
            "2",
        ]);
        assert!(cli.prompt.is_empty());
        assert!(matches!(
            cli.command,
            Some(Commands::Fleet(TuiPassthroughArgs { ref args }))
                if args == &["run", "tasks.json", "--max-workers", "2"]
        ));

        let cli = parse_ok(&[
            "codewhale",
            "workflow",
            "run",
            "stopship",
            "--fleet",
            "stopship",
            "--runtime",
            "tmux",
            "--issue",
            "4375",
        ]);
        assert!(matches!(
            cli.command,
            Some(Commands::Workflow(WorkflowArgs {
                command: WorkflowCommand::Run {
                    ref workflow,
                    ref fleet,
                    ref runtime,
                    ref issue,
                    ..
                }
            })) if workflow == "stopship"
                && fleet.as_deref() == Some("stopship")
                && runtime == "tmux"
                && issue.as_deref() == Some("4375")
        ));
    }

    /// Fleet is the only top-level spelling for durable runs. The retired
    /// `pod` spelling must fail to parse instead of dispatching.
    #[test]
    fn fleet_is_the_only_top_level_command_and_pod_is_rejected() {
        for tail in [
            vec!["init"],
            vec!["status"],
            vec!["run", "tasks.json", "--max-workers", "2"],
        ] {
            let fleet = parse_ok(
                &std::iter::once("codewhale")
                    .chain(["fleet"])
                    .chain(tail.iter().copied())
                    .collect::<Vec<_>>(),
            );
            let Some(Commands::Fleet(fleet_args)) = &fleet.command else {
                panic!("fleet must parse into the fleet command: {tail:?}");
            };
            assert_eq!(fleet_args.args, tail, "{tail:?}");
            assert!(fleet.prompt.is_empty(), "{tail:?}");

            let retired = parse_ok(
                &std::iter::once("codewhale")
                    .chain(["pod"])
                    .chain(tail.iter().copied())
                    .collect::<Vec<_>>(),
            );
            assert!(
                retired.command.is_none(),
                "retired pod must not dispatch to any command: {tail:?}"
            );
            assert_eq!(
                retired.prompt.first().map(String::as_str),
                Some("pod"),
                "retired pod words fall through to prompt text: {tail:?}"
            );
        }

        // Help advertises fleet only.
        let help = help_for(&["codewhale", "--help"]);
        let commands = help
            .lines()
            .map(str::trim_start)
            .filter(|line| line.starts_with("fleet"))
            .collect::<Vec<_>>();
        assert_eq!(
            commands.len(),
            1,
            "expected exactly one entry: {commands:?}"
        );
        assert!(commands[0].starts_with("fleet"), "{commands:?}");
        assert!(
            commands[0].contains("fleet"),
            "help summary should name fleet: {commands:?}"
        );
        assert!(
            !help.contains("Manage durable Agent Fleet runs"),
            "the retired Fleet-led summary must be gone from top-level help"
        );

        // `fleet --help` forwards to the delegated binary; the wrapper's own
        // help (with these examples) stays reachable as `help fleet`.
        let fleet_help = help_for(&["codewhale", "help", "fleet"]);
        assert!(fleet_help.contains("Manage durable Agent fleet runs"));
        assert!(fleet_help.contains("codewhale fleet run tasks.json --max-workers 4"));

        // The inner command token matches the canonical name so receipts
        // and any echoed invocation never regress to the retired name.
        let args = TuiPassthroughArgs {
            args: vec!["status".into()],
        };
        assert_eq!(
            tui_args("fleet", args.clone()),
            vec!["fleet".to_string(), "status".to_string()]
        );
        assert!(command_accepts_raw_provider(Some(&Commands::Fleet(args))));
    }

    #[test]
    fn exec_and_fleet_accept_builtin_and_raw_provider_identifiers() {
        let builtin = parse_ok(&["codewhale", "--provider", "openrouter", "exec", "Reply OK"]);
        assert_eq!(builtin.provider.as_deref(), Some("openrouter"));
        assert_eq!(
            top_level_provider_override(builtin.provider.as_deref(), builtin.command.as_ref())
                .expect("built-in Exec provider"),
            Some(ProviderKind::Openrouter)
        );

        assert_eq!(
            top_level_provider_override(
                Some("qianfan"),
                Some(&Commands::Exec(TuiPassthroughArgs {
                    args: vec!["Reply OK".into()]
                }))
            )
            .expect("qianfan is a catalog route"),
            Some(ProviderKind::Qianfan)
        );

        for (provider, command) in [
            ("lm-studio", vec!["exec", "Reply OK"]),
            ("lm-studio", vec!["fleet", "status"]),
        ] {
            let argv = std::iter::once("codewhale")
                .chain(["--provider", provider])
                .chain(command.iter().copied())
                .collect::<Vec<_>>();
            let cli = parse_ok(&argv);
            assert_eq!(cli.provider.as_deref(), Some(provider));
            assert_eq!(
                top_level_provider_override(cli.provider.as_deref(), cli.command.as_ref())
                    .expect("raw TUI provider"),
                None,
                "{argv:?} should defer the raw provider id to the TUI"
            );
        }
    }

    #[test]
    fn opencode_go_provider_aliases_parse_as_builtin() {
        for alias in ["opencode-go", "opencode_go", "opencodego"] {
            assert_eq!(builtin_provider_arg(alias), Some(ProviderKind::OpencodeGo));
        }
    }

    #[test]
    fn ollama_cloud_provider_aliases_parse_as_builtin() {
        for alias in ["ollama-cloud", "ollama_cloud"] {
            assert_eq!(builtin_provider_arg(alias), Some(ProviderKind::OllamaCloud));
        }
    }

    #[test]
    fn antigravity_provider_aliases_are_clear_only_and_never_raw_custom() {
        for alias in ["antigravity", "agy"] {
            assert_eq!(builtin_provider_arg(alias), None, "{alias}");
            assert_eq!(
                parse_auth_clear_provider(alias),
                Ok(ProviderKind::Antigravity),
                "{alias}"
            );
            let error = parse_catalog_route(alias).expect_err("legacy route is not selectable");
            assert!(error.contains("non-runnable legacy provider"), "{error}");
            assert!(error.contains("--provider antigravity"), "{error}");
            assert!(error.contains("google"), "{error}");
            assert!(error.contains("GEMINI_API_KEY"), "{error}");

            let clear = parse_ok(&["codewhale", "auth", "clear", "--provider", alias]);
            assert!(matches!(
                clear.command,
                Some(Commands::Auth(AuthArgs {
                    command: AuthCommand::Clear {
                        provider: ProviderKind::Antigravity,
                    }
                }))
            ));

            for argv in [
                vec!["codewhale", "auth", "set", "--provider", alias],
                vec!["codewhale", "auth", "get", "--provider", alias],
                vec!["codewhale", "auth", "print-api-key", "--provider", alias],
                vec!["codewhale", "auth", "status", "--provider", alias],
                vec!["codewhale", "auth", "external-revoke", "--provider", alias],
                vec![
                    "codewhale",
                    "auth",
                    "external-consent",
                    "--provider",
                    alias,
                    "--mode",
                    "read-only",
                    "--yes",
                ],
                vec!["codewhale", "model", "list", "--provider", alias],
                vec!["codewhale", "model", "resolve", "--provider", alias],
            ] {
                let error = Cli::try_parse_from(argv)
                    .expect_err("legacy Antigravity route must be rejected outside auth clear");
                assert_eq!(error.kind(), ErrorKind::ValueValidation);
                assert!(
                    error.to_string().contains("non-runnable legacy provider"),
                    "{error}"
                );
            }

            for command in [
                Commands::Exec(TuiPassthroughArgs {
                    args: vec!["Reply OK".into()],
                }),
                Commands::Fleet(TuiPassthroughArgs {
                    args: vec!["status".into()],
                }),
            ] {
                let error = top_level_provider_override(Some(alias), Some(&command))
                    .expect_err("legacy alias must not fall through as a raw custom provider");
                assert!(
                    error.to_string().contains("non-runnable legacy provider"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn legacy_dual_wire_provider_flag_keeps_named_table_kind() {
        // The CLI flag must resolve legacy spellings to the table-owning
        // dialect kind (mirroring TOML serde), never to the collapsed catalog
        // primary, or the user's own [providers.*] table is orphaned.
        for alias in [
            "minimax-anthropic",
            "minimax_anthropic",
            "mini-max-anthropic",
            "mini_max_anthropic",
        ] {
            assert_eq!(
                builtin_provider_arg(alias),
                Some(ProviderKind::MinimaxAnthropic),
                "{alias}"
            );
        }
        let cli = parse_ok(&[
            "codewhale",
            "--provider",
            "minimax-anthropic",
            "exec",
            "Reply OK",
        ]);
        assert_eq!(
            top_level_provider_override(cli.provider.as_deref(), cli.command.as_ref())
                .expect("legacy dual-wire provider"),
            Some(ProviderKind::MinimaxAnthropic)
        );
    }

    #[test]
    fn opencode_zen_provider_aliases_parse_as_builtin() {
        for alias in [
            "opencode-zen",
            "opencode_zen",
            "opencodezen",
            "zen",
            "opencode",
        ] {
            assert_eq!(builtin_provider_arg(alias), Some(ProviderKind::OpencodeZen));
        }
    }

    #[test]
    fn raw_provider_ids_remain_restricted_to_exec_and_fleet() {
        let cli = parse_ok(&["codewhale", "--provider", "lm-studio", "model", "list"]);
        let err = top_level_provider_override(cli.provider.as_deref(), cli.command.as_ref())
            .expect_err("model registry commands still require a built-in provider");
        assert!(err.to_string().contains(
            "configured custom providers are accepted by exec, fleet and thread resume/fork"
        ));

        let err = Cli::try_parse_from(["codewhale", "auth", "set", "--provider", "lm-studio"])
            .expect_err("auth keeps enum-only provider validation");
        assert_eq!(err.kind(), ErrorKind::ValueValidation);

        let err = Cli::try_parse_from([
            "codewhale",
            "--provider",
            "../../lm-studio",
            "exec",
            "Reply OK",
        ])
        .expect_err("provider ids must stay simple tokens");
        assert!(
            err.to_string()
                .contains("provider must be a simple identifier")
        );
    }

    #[test]
    fn hidden_lane_log_proxy_parses_child_argv_and_preserves_other_commands() {
        let cli = parse_ok(&[
            "codewhale",
            "lane-log-proxy",
            "--log-path",
            "/tmp/lane.ndjson",
            "--receipt-path",
            "/tmp/lane.exit.json",
            "--receipt-tmp-path",
            "/tmp/lane.exit.json.tmp",
            "--environment-path",
            "/tmp/lane.env.json",
            "--lane-id",
            "lane-proof",
            "--",
            "/bin/echo",
            "--child-flag",
            "hello",
        ]);
        let (proxy, command) = split_lane_log_proxy_command(cli.command);
        assert!(command.is_none());
        let proxy = proxy.expect("proxy args");
        assert_eq!(proxy.lane_id, "lane-proof");
        assert_eq!(
            proxy.command,
            ["/bin/echo", "--child-flag", "hello"].map(str::to_string)
        );

        let cli = parse_ok(&["codewhale", "lane", "list", "--json"]);
        let (proxy, command) = split_lane_log_proxy_command(cli.command);
        assert!(proxy.is_none());
        assert!(matches!(
            command,
            Some(Commands::Lane(LaneArgs {
                command: LaneCommand::List { json: true }
            }))
        ));
    }

    /// #1888: the CLI must expose exactly the Lane verbs the shared contract
    /// declares, under the same ids — no CLI-only verb, no missing verb.
    #[test]
    fn cli_lane_subcommands_cover_the_shared_control_contract() {
        use codewhale_lane::{ControlDomain, ControlOperation, ControlSurface};

        for descriptor in codewhale_lane::control::operations_for_domain(ControlDomain::Lane) {
            let argv = [
                "codewhale".to_string(),
                "lane".to_string(),
                descriptor.verb.to_string(),
            ];
            let mut argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            if descriptor.target.requires_identity() {
                argv.push("lane-a1b2c3d4");
            }
            let cli = parse_ok(&argv);
            let Some(Commands::Lane(args)) = cli.command else {
                panic!("`{}` must parse as a lane subcommand", descriptor.verb);
            };
            let parsed = match args.command {
                LaneCommand::List { .. } => ControlOperation::LaneList,
                LaneCommand::Status { .. } => ControlOperation::LaneStatus,
                LaneCommand::Interrupt { .. } | LaneCommand::Stop { .. } => {
                    ControlOperation::LaneInterrupt
                }
                LaneCommand::Restart { .. } => ControlOperation::LaneRestart,
                LaneCommand::Resume { .. } => ControlOperation::LaneResume,
                other => panic!(
                    "unexpected lane subcommand for {}: {other:?}",
                    descriptor.verb
                ),
            };
            assert_eq!(
                parsed, descriptor.operation,
                "`codewhale lane {}` must map to {}",
                descriptor.verb, descriptor.id
            );
            assert!(
                descriptor.offers(ControlSurface::Cli),
                "{} must be declared on the CLI surface",
                descriptor.id
            );
        }
    }

    /// `lane stop` is a compatibility spelling, not a second verb.
    #[test]
    fn lane_stop_and_interrupt_resolve_to_one_verb() {
        use codewhale_lane::{ControlDomain, ControlOperation};

        for spelling in ["stop", "interrupt", "cancel", "kill"] {
            assert_eq!(
                ControlOperation::parse_verb(ControlDomain::Lane, spelling),
                Some(ControlOperation::LaneInterrupt),
                "{spelling}"
            );
        }
        let stop = parse_ok(&["codewhale", "lane", "stop", "lane-a1b2c3d4"]);
        assert!(matches!(
            stop.command,
            Some(Commands::Lane(LaneArgs {
                command: LaneCommand::Stop { .. }
            }))
        ));
    }

    #[test]
    fn named_fleet_search_roots_include_the_saved_workspace_dir() {
        let workspace = Path::new("/ws");
        let roots = named_fleet_search_roots(workspace);
        let tail: Vec<&Path> = roots
            .iter()
            .rev()
            .take(2)
            .rev()
            .map(PathBuf::as_path)
            .collect();
        assert_eq!(tail, [Path::new("/ws/.codewhale"), Path::new("/ws")]);
    }

    #[test]
    fn lane_stop_accepts_json_like_interrupt() {
        let stop = parse_ok(&["codewhale", "lane", "stop", "lane-a1b2c3d4", "--json"]);
        assert!(matches!(
            stop.command,
            Some(Commands::Lane(LaneArgs {
                command: LaneCommand::Stop { ref lane_id, json: true }
            })) if lane_id == "lane-a1b2c3d4"
        ));
        let plain = parse_ok(&["codewhale", "lane", "stop", "lane-a1b2c3d4"]);
        assert!(matches!(
            plain.command,
            Some(Commands::Lane(LaneArgs {
                command: LaneCommand::Stop { json: false, .. }
            }))
        ));
    }

    #[test]
    fn lane_worktree_flags_are_validated_as_a_set() {
        let repo = PathBuf::from("/repo");
        let custom = PathBuf::from("/elsewhere/wt");

        assert!(
            validate_lane_worktree_flags(None, None, None)
                .unwrap()
                .is_none()
        );
        let (root, branch, path) = validate_lane_worktree_flags(
            Some(repo.clone()),
            Some("feat".to_string()),
            Some(custom.clone()),
        )
        .unwrap()
        .expect("paired flags provision a worktree");
        assert_eq!(root, repo);
        assert_eq!(branch, "feat");
        assert_eq!(path, Some(custom.clone()));

        let err = validate_lane_worktree_flags(None, None, Some(custom))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--worktree-path requires"), "{err}");
        assert!(validate_lane_worktree_flags(Some(repo), None, None).is_err());
        assert!(validate_lane_worktree_flags(None, Some("feat".into()), None).is_err());
    }

    #[test]
    fn short_workflow_names_do_not_resolve_version_pinned_files() {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        // A bare short name must never expand to a version-pinned script.
        // The v0868_* lane scripts are gone, but the guard stays so a future
        // vXXXX_ naming habit cannot silently become resolvable.
        let candidates = workflow_source_candidates("issue-sweep", None, &workspace);
        assert!(candidates.iter().all(|path| {
            !path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("v0868_"))
        }));
        assert!(resolve_workflow_source_path("issue-sweep", None, &workspace).is_err());

        // An explicit repo-relative path still resolves — checked against a
        // workflow that actually ships.
        let explicit =
            resolve_workflow_source_path("workflows/stopship.workflow.js", None, &workspace)
                .expect("explicit workflow path");
        assert!(explicit.ends_with("workflows/stopship.workflow.js"));
    }

    #[test]
    fn workflow_run_resolves_stopship_alias_and_payload() {
        let _lock = env_lock();
        let (_dir, _tui) = install_fake_tui_binary();
        let _provider = ScopedEnvVar::remove("DEEPSEEK_PROVIDER");
        let _model = ScopedEnvVar::remove("DEEPSEEK_MODEL");
        let _codewhale_model = ScopedEnvVar::remove("CODEWHALE_MODEL");
        let _base_url = ScopedEnvVar::remove("DEEPSEEK_BASE_URL");
        let _api_key = ScopedEnvVar::remove("DEEPSEEK_API_KEY");
        let _cli_api_key = ScopedEnvVar::remove("CODEWHALE_CLI_API_KEY");
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let cli = parse_ok(&[
            "codewhale",
            "--profile",
            "workflow-profile",
            "--model",
            "explicit-workflow-model",
            "--api-key",
            "explicit-profile-key",
            "--workspace",
            workspace.to_str().expect("workspace UTF-8"),
        ]);
        let resolved = resolved_runtime_for_test(ProviderKind::Deepseek, ProviderSource::Config);
        let source = resolve_workflow_source_path("stopship", None, &workspace)
            .expect("stopship workflow source");
        assert!(source.ends_with("workflows/stopship.workflow.js"));

        let process = workflow_exec_command(WorkflowExecSpec {
            cli: &cli,
            resolved_runtime: &resolved,
            config_path: &workspace.join("config.toml"),
            source_root: &workspace,
            source_path: &source,
            workflow: "stopship",
            fleet: Some("stopship"),
            issue: Some("4375"),
            goal: Some("fix stopship"),
            token_budget: Some(25_000),
            verify: true,
        })
        .expect("command");
        let current_executable = std::env::current_exe().expect("current executable");
        assert_eq!(
            process.command.first().map(String::as_str),
            current_executable.to_str(),
            "workflow lanes must launch the exact runtime that built their process spec"
        );
        let joined = process.command.join("\n");
        assert!(joined.contains("workflow-tool"));
        assert!(joined.contains("explicit-workflow-command"));
        assert!(joined.contains("--input-json"));
        assert!(!process.command.iter().any(|arg| arg == "exec"));
        assert!(!process.command.iter().any(|arg| arg == "--workspace"));
        assert!(
            process
                .command
                .windows(2)
                .any(|pair| pair == ["--profile", "workflow-profile"])
        );
        assert!(!joined.contains("Run the CodeWhale"));
        assert!(joined.contains("\"source_path\":\"workflows/stopship.workflow.js\""));
        assert!(joined.contains("\"fleet\":\"stopship\""));
        assert!(joined.contains("\"issue\":\"4375\""));
        assert!(joined.contains("\"token_budget\":25000"));
        assert!(joined.contains("\"verify\":true"));
        assert!(process.environment.iter().any(|(key, value)| {
            key == "CODEWHALE_MODEL" && value == "explicit-workflow-model"
        }));
        assert!(
            !process
                .environment
                .iter()
                .any(|(key, _)| key == "DEEPSEEK_MODEL"),
            "the dispatcher must not write the retired DEEPSEEK_* twins (#6516)"
        );
        assert!(
            !process
                .environment
                .iter()
                .any(|(key, _)| key == "DEEPSEEK_PROVIDER")
        );
        assert!(
            !process
                .environment
                .iter()
                .any(|(key, _)| key == "DEEPSEEK_BASE_URL")
        );
        assert!(
            !process
                .environment
                .iter()
                .any(|(key, _)| key == "DEEPSEEK_API_KEY")
        );
        assert!(process.environment.iter().any(|(key, value)| {
            key == "CODEWHALE_CLI_API_KEY" && value == "explicit-profile-key"
        }));
        assert!(
            !process
                .command
                .iter()
                .any(|argument| argument.contains("explicit-profile-key"))
        );
        assert!(
            process
                .environment
                .iter()
                .all(|(_, value)| value != "test-model")
        );
    }

    #[test]
    fn exec_keeps_global_looking_flags_as_passthrough_args() {
        let cli = parse_ok(&[
            "codewhale",
            "exec",
            "--provider",
            "definitely-not-a-provider",
            "Reply OK",
        ]);

        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec command");
        };

        assert_eq!(
            args.args,
            vec![
                "--provider".to_string(),
                "definitely-not-a-provider".to_string(),
                "Reply OK".to_string(),
            ]
        );
    }

    #[test]
    fn exec_routes_provider_after_subcommand_once() {
        let before = parse_ok(&[
            "codewhale",
            "--provider",
            "openai",
            "--model",
            "gpt-5.6",
            "exec",
            "Reply OK",
        ]);
        let mut after = parse_ok(&[
            "codewhale",
            "exec",
            "--provider",
            "openai",
            "--model",
            "gpt-5.6",
            "Reply OK",
        ]);
        capture_exec_startup_options(&mut after).expect("canonical startup capture");
        assert_eq!(after.provider, before.provider);
        assert_eq!(after.model, before.model);
        assert_eq!(
            top_level_provider_override(after.provider.as_deref(), after.command.as_ref()).unwrap(),
            Some(ProviderKind::Openai)
        );
        let Some(Commands::Exec(args)) = after.command else {
            panic!("expected exec");
        };
        assert_eq!(args.args, ["Reply OK"]);
    }

    #[test]
    fn exec_rejects_duplicate_startup_options_across_positions() {
        for tail in ["openai", "openrouter"] {
            let mut cli = parse_ok(&[
                "codewhale",
                "--provider",
                "openai",
                "exec",
                &format!("--provider={tail}"),
                "Reply OK",
            ]);
            let err = capture_exec_startup_options(&mut cli).expect_err("duplicate route pin");
            assert!(
                err.to_string()
                    .contains("--provider may be supplied only once")
            );
            assert_eq!(cli.provider.as_deref(), Some("openai"));
        }
        let mut cli = parse_ok(&[
            "codewhale",
            "exec",
            "--provider=openai",
            "--provider=openai",
            "Reply OK",
        ]);
        assert!(capture_exec_startup_options(&mut cli).is_err());
        assert_eq!(cli.provider, None);
    }

    #[test]
    fn exec_allows_documented_forwarded_flags() {
        let mut cli = parse_ok(&[
            "codewhale",
            "exec",
            "--auto",
            "--model=gpt-5.6",
            "--output-format",
            "stream-json",
            "fix tests",
        ]);
        capture_exec_startup_options(&mut cli).expect("documented exec flags should pass");
        assert_eq!(cli.model.as_deref(), Some("gpt-5.6"));
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec");
        };
        assert_eq!(
            args.args,
            ["--auto", "--output-format", "stream-json", "fix tests"]
        );
    }

    #[test]
    fn exec_allows_literal_prompt_flags_after_separator() {
        let mut cli = parse_ok(&[
            "codewhale",
            "exec",
            "--",
            "--provider",
            "is literal prompt text",
        ]);
        capture_exec_startup_options(&mut cli).expect("separator should stop startup capture");
        assert_eq!(cli.provider, None);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec");
        };
        assert_eq!(args.args, ["--", "--provider", "is literal prompt text"]);

        let mut cli = parse_ok(&[
            "codewhale",
            "--provider=openai",
            "exec",
            "--",
            "--provider=literal",
            "--model=literal",
            "--",
        ]);
        capture_exec_startup_options(&mut cli).expect("escaped route flags stay literal");
        assert_eq!(cli.provider.as_deref(), Some("openai"));
        assert_eq!(cli.model, None);
        let Some(Commands::Exec(args)) = cli.command else {
            panic!("expected exec");
        };
        assert_eq!(
            args.args,
            ["--", "--provider=literal", "--model=literal", "--"]
        );
    }

    #[test]
    fn exec_captures_config_and_profile_before_runtime_precedence() {
        let mut cli = parse_ok(&[
            "codewhale",
            "exec",
            "--config=chosen.toml",
            "--profile",
            "chosen",
            "--provider=openai",
            "--api-key=fixture-key",
            "--base-url=http://127.0.0.1:9/v1",
            "Reply OK",
        ]);
        capture_exec_startup_options(&mut cli).expect("canonical typed options");
        assert_eq!(cli.config.as_deref(), Some(Path::new("chosen.toml")));
        assert_eq!(cli.profile.as_deref(), Some("chosen"));
        assert_eq!(cli.api_key.as_deref(), Some("fixture-key"));
        assert_eq!(cli.base_url.as_deref(), Some("http://127.0.0.1:9/v1"));
        for flag in [
            "--config",
            "--profile",
            "--api-key",
            "--base-url",
            "--model",
        ] {
            let mut cli = parse_ok(&[
                "codewhale",
                flag,
                "first",
                "exec",
                flag,
                "second",
                "Reply OK",
            ]);
            assert!(capture_exec_startup_options(&mut cli).is_err(), "{flag}");
        }
    }

    #[test]
    fn dispatcher_resume_picker_only_handles_bare_windows_resume() {
        assert!(should_pick_resume_in_dispatcher(
            &["resume".to_string()],
            true
        ));
        assert!(!should_pick_resume_in_dispatcher(
            &["resume".to_string(), "--last".to_string()],
            true
        ));
        assert!(!should_pick_resume_in_dispatcher(
            &["resume".to_string(), "abc123".to_string()],
            true
        ));
        assert!(!should_pick_resume_in_dispatcher(
            &["resume".to_string()],
            false
        ));
    }

    #[test]
    fn auth_set_uses_isolated_file_store_and_preserves_tui_defaults() {
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let codewhale_home = dir.path().join("codewhale-home");
        let codewhale_home_value = codewhale_home.to_string_lossy().into_owned();
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &codewhale_home_value);
        let _backend = ScopedEnvVar::set("CODEWHALE_SECRET_BACKEND", "file");
        let path = codewhale_home.join("config.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        let secrets = Secrets::auto_detect();

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Set {
                provider: ProviderKind::Deepseek,
                api_key: Some("sk-test".to_string()),
                api_key_stdin: false,
            },
            &secrets,
        )
        .expect("auth set should persist credential");
        assert!(store.config.providers.deepseek.api_key.is_none());
        // Intentional change: auth set used to pin `deepseek-v4-pro` here,
        // silently moving a fresh install off the cheaper `deepseek-flash`
        // provider default. Saving a key must not choose a model.
        assert!(store.config.default_text_model.is_none());
        assert!(store.config.providers.deepseek.model.is_none());
        let saved = std::fs::read_to_string(&path).expect("config should be written");
        assert!(!saved.contains("sk-test"), "{saved}");
        assert!(
            !saved
                .lines()
                .any(|line| line.trim_start().starts_with("api_key="))
        );
        assert!(!saved.contains("default_text_model"), "{saved}");
        assert_eq!(
            secrets.get("deepseek").expect("read secret").as_deref(),
            Some("sk-test")
        );
    }

    /// `codewhale login` now means the Codewhale account device flow: the
    /// account-login flags parse through and reach the cloud path.
    #[test]
    fn login_parses_account_device_flow_flags() {
        let cli = parse_ok(&["codewhale", "login", "--no-open", "--timeout-seconds", "5"]);
        let Some(Commands::Login(args)) = cli.command else {
            panic!("expected Login");
        };
        assert!(args.no_open);
        assert_eq!(args.timeout_seconds, 5);
        assert!(args.api_key.is_none());
        assert!(args.provider.is_none());

        let cli = parse_ok(&["codewhale", "login"]);
        let Some(Commands::Login(args)) = cli.command else {
            panic!("expected Login");
        };
        assert!(!args.no_open);
        assert_eq!(args.timeout_seconds, 600);
    }

    /// The provider-key surface moved to `auth set --provider`; the hidden
    /// legacy flags must redirect loudly instead of silently configuring a key.
    #[test]
    fn login_rejects_legacy_provider_flags_with_redirect() {
        let err = reject_legacy_login_provider_args(&LoginArgs {
            no_open: false,
            timeout_seconds: 600,
            api_key: Some("sk-x".to_string()),
            provider: None,
        })
        .expect_err("legacy --api-key must be rejected");
        let rendered = err.to_string();
        assert!(
            rendered.contains("auth set --provider"),
            "redirect must name `auth set --provider`: {rendered}"
        );

        let err = reject_legacy_login_provider_args(&LoginArgs {
            no_open: false,
            timeout_seconds: 600,
            api_key: None,
            provider: Some(ProviderKind::Deepseek),
        })
        .expect_err("legacy --provider must be rejected");
        assert!(
            err.to_string().contains("auth set --provider"),
            "redirect must name `auth set --provider`"
        );

        reject_legacy_login_provider_args(&LoginArgs {
            no_open: false,
            timeout_seconds: 600,
            api_key: None,
            provider: None,
        })
        .expect("plain account login carries no legacy flags");
    }

    /// Root help keeps the `login` token, but its meaning is now the account
    /// sign-in; the subcommand help must say so.
    #[test]
    fn login_help_describes_account_signin() {
        let help = help_for(&["codewhale", "login", "--help"]);
        assert!(
            help.contains("Codewhale account"),
            "login help must describe account sign-in: {help}"
        );
        assert!(
            help.contains("manage provider API keys"),
            "login help must explain the account's immediate benefit: {help}"
        );
        assert!(
            help.contains("Signing in does not upload existing local keys"),
            "login help must explain the local-key upload boundary: {help}"
        );
    }

    #[test]
    fn auth_parses_daytona_slot_commands_as_unknown() {
        // The internal cloud-agent slot must not be a user command: parsing
        // rejects it and `auth --help` never teaches it.
        for argv in [
            vec![
                "codewhale",
                "auth",
                "set-slot",
                "daytona",
                "--api-key-stdin",
            ],
            vec!["codewhale", "auth", "clear-slot", "daytona"],
        ] {
            let error = Cli::try_parse_from(argv).expect_err("slot commands must not parse");
            assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "{error}");
        }
        let help = help_for(&["codewhale", "auth", "--help"]);
        assert!(!help.contains("set-slot"), "{help}");
        assert!(!help.contains("clear-slot"), "{help}");
        assert!(!help.to_lowercase().contains("daytona"), "{help}");
    }

    /// #5198: `auth set` shares the login resolver — provider auth markers go
    /// user-global even when the ambient config is workspace-scoped.
    #[test]
    fn auth_set_with_repo_scoped_ambient_config_writes_user_global_metadata() {
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git marker");
        let repo_config_dir = repo.join(".codewhale");
        std::fs::create_dir_all(&repo_config_dir).expect("repo config dir");
        let repo_config = repo_config_dir.join("config.toml");
        std::fs::write(&repo_config, "approval_policy = \"never\"\n").expect("repo config");

        let codewhale_home = dir.path().join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &codewhale_home.to_string_lossy());
        let _config = ScopedEnvVar::set("CODEWHALE_CONFIG_PATH", &repo_config.to_string_lossy());
        let _legacy_config = ScopedEnvVar::remove("DEEPSEEK_CONFIG_PATH");
        let _backend = ScopedEnvVar::set("CODEWHALE_SECRET_BACKEND", "file");
        let mut store = ConfigStore::load(None).expect("ambient store should load");
        let secrets = Secrets::auto_detect();

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Set {
                provider: ProviderKind::Openrouter,
                api_key: Some("sk-or-repo-scoped".to_string()),
                api_key_stdin: false,
            },
            &secrets,
        )
        .expect("auth set should persist credential");

        assert_eq!(
            secrets.get("openrouter").expect("read secret").as_deref(),
            Some("sk-or-repo-scoped")
        );
        let global = std::fs::read_to_string(codewhale_home.join("config.toml"))
            .expect("user-global config");
        assert!(
            global.contains("auth_mode = \"api_key\""),
            "user-global config must carry the auth markers: {global}"
        );
        assert!(
            global.contains("openrouter"),
            "user-global config must name the provider table: {global}"
        );
        assert!(!global.contains("sk-or-repo-scoped"), "{global}");
        let repo_after = std::fs::read_to_string(&repo_config).expect("repo config");
        assert_eq!(
            repo_after, "approval_policy = \"never\"\n",
            "workspace config must stay untouched by credential metadata: {repo_after}"
        );
    }

    #[test]
    fn parses_auth_subcommand_matrix() {
        let cli = parse_ok(&["deepseek", "auth", "xai-device"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::XaiDevice
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "chatgpt"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Chatgpt
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "chatgpt-revoke"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::ChatgptRevoke
            }))
        ));

        let cli = parse_ok(&[
            "deepseek",
            "auth",
            "external-consent",
            "--provider",
            "openai-codex",
            "--mode",
            "read-only",
            "--path",
            "/tmp/codex-auth.json",
            "--yes",
        ]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::ExternalConsent {
                    provider: ProviderKind::OpenaiCodex,
                    mode: ExternalCredentialModeArg::ReadOnly,
                    path: Some(_),
                    yes: true,
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "external-revoke", "--provider", "xai"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::ExternalRevoke {
                    provider: ProviderKind::Xai,
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "set", "--provider", "deepseek"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderKind::Deepseek,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&[
            "deepseek",
            "auth",
            "set",
            "--provider",
            "openrouter",
            "--api-key-stdin",
        ]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderKind::Openrouter,
                    api_key: None,
                    api_key_stdin: true,
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "get", "--provider", "novita"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Get {
                    provider: ProviderKind::Novita
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "clear", "--provider", "nvidia-nim"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Clear {
                    provider: ProviderKind::NvidiaNim
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "set", "--provider", "fireworks"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderKind::Fireworks,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "set", "--provider", "siliconflow"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderKind::Siliconflow,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "set", "--provider", "arcee"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderKind::Arcee,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "set", "--provider", "moonshot"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderKind::Moonshot,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "set", "--provider", "wanjie-ark"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderKind::WanjieArk,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "get", "--provider", "sglang"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Get {
                    provider: ProviderKind::Sglang
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "get", "--provider", "vllm"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Get {
                    provider: ProviderKind::Vllm
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "set", "--provider", "ollama"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Set {
                    provider: ProviderKind::Ollama,
                    api_key: None,
                    api_key_stdin: false,
                }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "status", "--provider", "openai-codex"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Status {
                    provider: Some(ProviderKind::OpenaiCodex),
                    diagnostic: false,
                }
            }))
        ));

        let cli = parse_ok(&[
            "deepseek",
            "auth",
            "status",
            "--diagnostic",
            "--provider",
            "deepseek",
        ]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Status {
                    provider: Some(ProviderKind::Deepseek),
                    diagnostic: true,
                }
            }))
        ));

        for (provider, expected) in [
            ("anthropic", ProviderKind::Anthropic),
            ("openmodel", ProviderKind::Openmodel),
            ("open-model", ProviderKind::Openmodel),
            ("zai", ProviderKind::Zai),
            ("stepfun", ProviderKind::Stepfun),
            ("minimax", ProviderKind::Minimax),
            ("minimax-anthropic", ProviderKind::MinimaxAnthropic),
            ("minimax_anthropic", ProviderKind::MinimaxAnthropic),
            ("deepinfra", ProviderKind::Deepinfra),
            ("deep-infra", ProviderKind::Deepinfra),
            ("siliconflow-cn", ProviderKind::SiliconflowCN),
            ("siliconflow-CN", ProviderKind::SiliconflowCN),
            ("siliconflow_china", ProviderKind::SiliconflowCN),
        ] {
            let cli = parse_ok(&[
                "deepseek",
                "auth",
                "set",
                "--provider",
                provider,
                "--api-key-stdin",
            ]);
            assert!(matches!(
                cli.command,
                Some(Commands::Auth(AuthArgs {
                    command: AuthCommand::Set {
                        provider,
                        api_key: None,
                        api_key_stdin: true,
                    }
                })) if provider == expected
            ));
        }

        let cli = parse_ok(&["deepseek", "auth", "list"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::List
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "migrate"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Migrate { dry_run: false }
            }))
        ));

        let cli = parse_ok(&["deepseek", "auth", "migrate", "--dry-run"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Auth(AuthArgs {
                command: AuthCommand::Migrate { dry_run: true }
            }))
        ));
    }

    #[test]
    fn auth_help_describes_runtime_effective_diagnostics() {
        let get = help_for(&["codewhale", "auth", "get", "--help"]);
        assert!(get.contains("effective credential route"), "{get}");
        assert!(get.contains("structural OAuth/repair state"), "{get}");

        let status = help_for(&["codewhale", "auth", "status", "--help"]);
        assert!(
            status.contains("runtime-effective credential route state"),
            "{status}"
        );

        let list = help_for(&["codewhale", "auth", "list", "--help"]);
        assert!(list.contains("runtime-effective auth state"), "{list}");
    }

    #[test]
    fn auth_set_writes_secret_store_and_keeps_config_credential_free() {
        use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-set-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        let inner = Arc::new(InMemoryKeyringStore::new());
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Set {
                provider: ProviderKind::Deepseek,
                api_key: Some("sk-keyring".to_string()),
                api_key_stdin: false,
            },
            &secrets,
        )
        .expect("set should succeed");
        assert!(store.config.providers.deepseek.api_key.is_none());
        let saved = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(!saved.contains("sk-keyring"), "{saved}");
        assert!(
            !saved
                .lines()
                .any(|line| line.trim_start().starts_with("api_key ="))
        );
        assert_eq!(
            inner.get("deepseek").unwrap().as_deref(),
            Some("sk-keyring")
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_set_refuses_plaintext_config_when_secret_store_write_fails() {
        use codewhale_secrets::{KeyringStore, SecretsError};
        use std::sync::Arc;

        struct FailingStore;

        impl KeyringStore for FailingStore {
            fn get(&self, _key: &str) -> Result<Option<String>, SecretsError> {
                Ok(None)
            }

            fn set(&self, _key: &str, _value: &str) -> Result<(), SecretsError> {
                Err(SecretsError::Keyring("test write failure".to_string()))
            }

            fn delete(&self, _key: &str) -> Result<(), SecretsError> {
                Ok(())
            }

            fn backend_name(&self) -> &'static str {
                "failing test store"
            }
        }

        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("config.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("load config");
        let secrets = Secrets::new(Arc::new(FailingStore));

        let error = run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Set {
                provider: ProviderKind::Openrouter,
                api_key: Some("fallback-test-credential".to_string()),
                api_key_stdin: false,
            },
            &secrets,
        )
        .expect_err("secret-store failure must not downgrade to plaintext");

        let message = format!("{error:#}");
        assert!(message.contains("Secret storage write failed"), "{message}");
        assert!(message.contains("Refusing"), "{message}");
        assert!(
            message.contains(&codewhale_config::quote_os_path(store.path())),
            "{message}"
        );
        assert!(store.config.providers.openrouter.api_key.is_none());
        assert!(!path.exists(), "plaintext config must stay untouched");
    }

    #[test]
    fn auth_set_provider_key_does_not_switch_active_provider() {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-set-preserve-provider-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.provider = ProviderKind::Deepseek;
        let secrets = no_keyring_secrets();

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Set {
                provider: ProviderKind::Arcee,
                api_key: Some("arcee-key".to_string()),
                api_key_stdin: false,
            },
            &secrets,
        )
        .expect("set should succeed");

        assert_eq!(store.config.provider, ProviderKind::Deepseek);
        assert!(store.config.providers.arcee.api_key.is_none());
        assert_eq!(
            store.config.providers.arcee.auth_mode.as_deref(),
            Some("api_key")
        );

        let reloaded = ConfigStore::load(Some(path.clone())).expect("store should reload");
        assert_eq!(reloaded.config.provider, ProviderKind::Deepseek);
        assert!(reloaded.config.providers.arcee.api_key.is_none());
        assert_eq!(
            reloaded.config.providers.arcee.auth_mode.as_deref(),
            Some("api_key")
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_set_ollama_accepts_empty_key_and_records_base_url() {
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-ollama-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.provider = ProviderKind::Deepseek;
        let secrets = no_keyring_secrets();

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Set {
                provider: ProviderKind::Ollama,
                api_key: None,
                api_key_stdin: false,
            },
            &secrets,
        )
        .expect("ollama auth set should not require a key");

        assert_eq!(store.config.provider, ProviderKind::Deepseek);
        assert_eq!(
            store.config.providers.ollama.base_url.as_deref(),
            Some("http://localhost:11434/v1")
        );
        assert_eq!(store.config.providers.ollama.api_key, None);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_clear_removes_from_config() {
        use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-clear-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.providers.deepseek.api_key = Some("sk-stale".to_string());
        store.save().unwrap();

        let inner = Arc::new(InMemoryKeyringStore::new());
        inner.set("deepseek", "sk-stale").unwrap();
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Clear {
                provider: ProviderKind::Deepseek,
            },
            &secrets,
        )
        .expect("clear should succeed");
        assert!(store.config.providers.deepseek.api_key.is_none());
        assert_eq!(inner.get("deepseek").unwrap(), None);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn antigravity_clear_removes_only_codewhale_owned_legacy_state() {
        use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let dir = tempfile::TempDir::new().expect("isolated legacy fixture");
        let config_path = dir.path().join("config.toml");
        let external_session_path = dir.path().join("external-antigravity-session.db");
        let external_session = b"external session bytes must remain unchanged";
        std::fs::write(&external_session_path, external_session)
            .expect("write external session trap");

        let mut store = ConfigStore::load(Some(config_path.clone())).expect("load empty config");
        store.config.provider = ProviderKind::Antigravity;
        store.config.fallback_providers = vec![ProviderKind::Antigravity, ProviderKind::Google];
        {
            let legacy = &mut store.config.providers.antigravity;
            legacy.api_key = Some("legacy-codewhale-fixture-key".to_string());
            legacy.base_url = Some("https://legacy.invalid/v1".to_string());
            legacy.model = Some("legacy-fixture-model".to_string());
            legacy.context_window = Some(1234);
            legacy.mode = Some("legacy-fixture-mode".to_string());
            legacy.wire = Some("legacy-fixture-wire".to_string());
            legacy.auth_mode = Some("oauth".to_string());
            legacy.insecure_skip_tls_verify = Some(true);
            legacy
                .http_headers
                .insert("X-Legacy-Fixture".to_string(), "fixture".to_string());
            legacy.path_suffix = Some("legacy-fixture-path".to_string());
            legacy.external_credentials =
                Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                    ProviderKind::Antigravity,
                    codewhale_config::ExternalCredentialSource::AgyCli,
                    external_session_path.clone(),
                ));
            legacy.extras.insert(
                "legacy_fixture_extra".to_string(),
                toml::Value::String("remove-me".to_string()),
            );
        }
        store.config.providers.google.api_key = Some("google-fixture-key".to_string());
        store.config.providers.google.base_url = Some("https://google.example/v1".to_string());
        store.config.providers.google.model = Some("google-fixture-model".to_string());
        store.save().expect("save legacy fixture");

        // Released configs accepted the short `[providers.agy]` table alias.
        // Exercise that on-disk spelling as well as the clear command's alias.
        let canonical = std::fs::read_to_string(&config_path).expect("read canonical fixture");
        let alias = canonical.replace("[providers.antigravity", "[providers.agy");
        std::fs::write(&config_path, alias).expect("write legacy alias fixture");
        let mut store = ConfigStore::load(Some(config_path.clone())).expect("reload alias fixture");

        let inner = Arc::new(InMemoryKeyringStore::new());
        inner
            .set("antigravity", "legacy-codewhale-secret-slot")
            .expect("seed Codewhale-owned legacy secret slot");
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Clear {
                provider: ProviderKind::Antigravity,
            },
            &secrets,
        )
        .expect("legacy clear should succeed");

        assert_eq!(store.config.provider, ProviderKind::default());
        assert_eq!(store.config.fallback_providers, vec![ProviderKind::Google]);
        assert!(store.config.providers.antigravity.is_empty());
        assert_eq!(inner.get("antigravity").unwrap(), None);
        assert_eq!(
            store.config.providers.google.api_key.as_deref(),
            Some("google-fixture-key")
        );
        assert_eq!(
            store.config.providers.google.base_url.as_deref(),
            Some("https://google.example/v1")
        );
        assert_eq!(
            store.config.providers.google.model.as_deref(),
            Some("google-fixture-model")
        );
        assert_eq!(
            std::fs::read(&external_session_path).expect("external session trap still exists"),
            external_session
        );

        let raw = std::fs::read_to_string(&config_path).expect("read cleared config");
        assert!(!raw.contains("[providers.antigravity"), "{raw}");
        assert!(!raw.contains("[providers.agy"), "{raw}");
        assert!(!raw.contains("legacy_fixture_extra"), "{raw}");
        assert!(raw.contains("[providers.google]"), "{raw}");

        let backup_path = config_path.with_file_name(format!(
            "{}.bak",
            config_path
                .file_name()
                .expect("config fixture has a file name")
                .to_string_lossy()
        ));
        let backup = std::fs::read_to_string(backup_path).expect("read cleared config backup");
        assert!(!backup.contains("[providers.antigravity"), "{backup}");
        assert!(!backup.contains("[providers.agy"), "{backup}");
        assert!(!backup.contains("legacy_fixture_extra"), "{backup}");
        assert!(
            !backup.contains(&external_session_path.to_string_lossy().to_string()),
            "{backup}"
        );
        assert!(
            backup.contains("base_url = \"https://google.example/v1\""),
            "{backup}"
        );
        assert!(
            backup.contains("model = \"google-fixture-model\""),
            "{backup}"
        );

        let reloaded = ConfigStore::load(Some(config_path)).expect("reload cleared config");
        assert_eq!(reloaded.config.provider, ProviderKind::default());
        assert!(reloaded.config.providers.antigravity.is_empty());
        assert_eq!(
            reloaded.config.providers.google.api_key.as_deref(),
            Some("google-fixture-key")
        );
    }

    #[test]
    fn antigravity_clear_restores_codewhale_secret_when_config_write_fails() {
        use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let dir = tempfile::TempDir::new().expect("isolated rollback fixture");
        let config_path = dir.path().join("config.toml");
        let external_session_path = dir.path().join("external-session.db");
        let external_session = b"external session rollback trap";
        std::fs::write(&external_session_path, external_session)
            .expect("write external session trap");
        let mut store = ConfigStore::load(Some(config_path.clone())).expect("load absent config");
        store.config.provider = ProviderKind::Antigravity;
        store.config.fallback_providers = vec![ProviderKind::Antigravity];
        store.config.providers.antigravity.api_key = Some("legacy-config-fixture".to_string());
        store.config.providers.antigravity.external_credentials =
            Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                ProviderKind::Antigravity,
                codewhale_config::ExternalCredentialSource::AgyCli,
                external_session_path.clone(),
            ));
        std::fs::create_dir(&config_path).expect("make config target unwritable as a file");

        let inner = Arc::new(InMemoryKeyringStore::new());
        inner
            .set("antigravity", "legacy-secret-fixture")
            .expect("seed Codewhale-owned legacy slot");
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Clear {
                provider: ProviderKind::Antigravity,
            },
            &secrets,
        )
        .expect_err("config failure must fail the clear transaction");

        assert_eq!(store.config.provider, ProviderKind::Antigravity);
        assert_eq!(
            store.config.fallback_providers,
            vec![ProviderKind::Antigravity]
        );
        assert_eq!(
            store.config.providers.antigravity.api_key.as_deref(),
            Some("legacy-config-fixture")
        );
        assert!(
            store
                .config
                .providers
                .antigravity
                .external_credentials
                .is_some()
        );
        assert_eq!(
            inner
                .get("antigravity")
                .expect("read restored slot")
                .as_deref(),
            Some("legacy-secret-fixture")
        );
        assert_eq!(
            std::fs::read(external_session_path).expect("external session trap still exists"),
            external_session
        );
    }

    #[test]
    fn auth_status_scoped_probe_and_list_all_provider_keyrings() {
        use codewhale_secrets::{KeyringStore, SecretsError};
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct RecordingStore {
            gets: Mutex<Vec<String>>,
        }

        impl KeyringStore for RecordingStore {
            fn get(&self, key: &str) -> Result<Option<String>, SecretsError> {
                self.gets.lock().unwrap().push(key.to_string());
                Ok(None)
            }

            fn set(&self, _key: &str, _value: &str) -> Result<(), SecretsError> {
                Ok(())
            }

            fn delete(&self, _key: &str) -> Result<(), SecretsError> {
                Ok(())
            }

            fn backend_name(&self) -> &'static str {
                "recording"
            }
        }

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-active-keyring-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.provider = ProviderKind::Deepseek;
        let inner = Arc::new(RecordingStore::default());
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Status {
                provider: Some(ProviderKind::Deepseek),
                diagnostic: false,
            },
            &secrets,
        )
        .expect("status should succeed");
        run_auth_command_with_secrets(&mut store, AuthCommand::List, &secrets)
            .expect("list should succeed");

        let probed = inner.gets.lock().unwrap();
        // Scoped status probes only the requested provider.
        assert_eq!(probed[0], "deepseek");
        // List now probes all providers (not just active) to fix the
        // stale keyring-only-for-active-provider bug.
        assert!(probed.len() > 1, "list should probe all providers");
        assert!(
            ProviderKind::ALL
                .iter()
                .filter(|p| **p != ProviderKind::OpenaiCodex)
                .all(|p| probed.contains(&provider_slot(*p).to_string())),
            "API-key providers should be probed by auth list: {:?}",
            *probed
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_diagnostic_reports_paths_and_presence_without_values() {
        let _lock = env_lock();
        let fixture = tempfile::TempDir::new().expect("fixture root");
        // macOS spells /var through a /private symlink. Canonicalize the
        // fixture root so the metadata-only backend diagnostic can prove every
        // ancestor is a real directory instead of truthfully returning
        // `unknown` for the symlinked spelling.
        let home = fixture
            .path()
            .canonicalize()
            .expect("canonical fixture root")
            .join("isolated-codewhale-home");
        let config_path = home.join("config.toml");
        let settings_path = home.join("settings.toml");
        let secret_path = home.join("secrets").join("secrets.json");
        std::fs::create_dir_all(secret_path.parent().expect("secret parent"))
            .expect("create diagnostic fixture");
        std::fs::write(
            &config_path,
            "api_key = \"diagnostic-config-secret-1234\"\n",
        )
        .expect("write config fixture");
        std::fs::write(&settings_path, "default_mode = \"plan\"\n")
            .expect("write settings fixture");
        std::fs::write(
            &secret_path,
            r#"{"deepseek":"diagnostic-store-secret-5678"}"#,
        )
        .expect("write secret fixture");

        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let _backend = ScopedEnvVar::set("CODEWHALE_SECRET_BACKEND", "file");
        let _env = ScopedEnvVar::set("DEEPSEEK_API_KEY", "diagnostic-env-secret-9012");
        let store = ConfigStore::load(Some(config_path.clone())).expect("load config fixture");

        let output = auth_diagnostic_lines(&store, Some(ProviderKind::Deepseek)).join("\n");
        assert!(
            output.contains(&format!(
                "codewhale home: {} (source: CODEWHALE_HOME (isolated); state: present)",
                codewhale_config::quote_os_path(&home)
            )),
            "{output}"
        );
        assert!(
            output.contains(&format!(
                "config: {} (present)",
                codewhale_config::quote_os_path(&config_path)
            )),
            "{output}"
        );
        assert!(
            output.contains(&format!(
                "settings: {} (present)",
                codewhale_config::quote_os_path(&settings_path)
            )),
            "{output}"
        );
        assert!(
            output.contains("secret backend: file (inspection: metadata_only)"),
            "{output}"
        );
        assert!(
            output.contains(&format!(
                "secret store: {} (present)",
                codewhale_config::quote_os_path(&secret_path)
            )),
            "{output}"
        );
        assert!(
            output.contains("provider deepseek sources: config_literal=present, secret_backend=present (provider entry unprobed), environment=present (DEEPSEEK_API_KEY)"),
            "{output}"
        );
        assert!(
            output.contains("legacy secret store: suppressed by explicit CODEWHALE_HOME isolation"),
            "{output}"
        );
        for secret_fragment in [
            "diagnostic-config-secret",
            "diagnostic-store-secret",
            "diagnostic-env-secret",
            "1234",
            "5678",
            "9012",
            "last4",
        ] {
            assert!(
                !output.contains(secret_fragment),
                "diagnostic leaked {secret_fragment:?}: {output}"
            );
        }
    }

    #[test]
    fn auth_status_reports_all_active_provider_sources_with_last4() {
        use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let _lock = env_lock();
        let _env = ScopedEnvVar::set("DEEPSEEK_API_KEY", "sk-env-1111");

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-status-table-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.provider = ProviderKind::Deepseek;
        store.config.providers.deepseek.api_key = Some("sk-config-3333".to_string());

        let inner = Arc::new(InMemoryKeyringStore::new());
        inner.set("deepseek", "sk-keyring-2222").unwrap();
        let secrets = Secrets::new(inner);

        let output =
            auth_status_lines_for_provider(&store, &secrets, ProviderKind::Deepseek).join("\n");

        assert!(output.contains("provider: deepseek"));
        assert!(output.contains("active source: config (last4: ...3333)"));
        assert!(output.contains("lookup order: config -> secret store -> env"));
        assert!(output.contains("config file: "));
        assert!(output.contains("set, last4: ...3333"));
        assert!(output.contains("secret store: in-memory (test) (set, last4: ...2222)"));
        assert!(output.contains("env var: DEEPSEEK_API_KEY (set, last4: ...1111)"));
        assert!(!output.contains("sk-config-3333"));
        assert!(!output.contains("sk-keyring-2222"));
        assert!(!output.contains("sk-env-1111"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_status_all_providers_lists_every_known_provider() {
        use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-all-status-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.provider = ProviderKind::Deepseek;
        store.config.providers.arcee.api_key = Some("sk-arcee-test1234".to_string());

        let inner = Arc::new(InMemoryKeyringStore::new());
        inner.set("openrouter", "sk-or-test5678").unwrap();
        let secrets = Secrets::new(inner);

        let output = auth_status_all_providers(&store, &secrets).join("\n");

        assert!(output.contains("account:"), "{output}");
        assert!(output.contains("codewhale login"), "{output}");
        // No-brand invariant: the internal cloud-agent slot is not user
        // surface, so status never names it or teaches a set-slot command.
        assert!(!output.to_lowercase().contains("daytona"), "{output}");
        assert!(!output.contains("set-slot"), "{output}");

        // Should list all known providers
        assert!(output.contains("deepseek"));
        assert!(output.contains("arcee"));
        assert!(output.contains("openrouter"));
        assert!(output.contains("huggingface"));
        assert!(output.contains("ollama"));

        // Active provider should be marked
        assert!(output.contains("deepseek") && output.contains("*"));

        // Arcee should show config source
        assert!(output.contains("config"));

        // Should NOT leak raw keys
        assert!(!output.contains("sk-arcee-test1234"));
        assert!(!output.contains("sk-or-test5678"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn chatgpt_auth_diagnostics_ignore_ambient_tokens_and_external_consent() {
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let _access_token = ScopedEnvVar::set("OPENAI_CODEX_ACCESS_TOKEN", "ambient-secret-9911");
        let _codex_token = ScopedEnvVar::set("CODEX_ACCESS_TOKEN", "alias-secret-9922");
        let auth_path = dir.path().join("auth.json");
        let external_raw = r#"{"tokens":{"access_token":"external-secret-9933"}}"#;
        std::fs::write(&auth_path, external_raw).expect("external trap");
        let _auth_file = ScopedEnvVar::set("OPENAI_CODEX_AUTH_FILE", &auth_path.to_string_lossy());
        let mut store = ConfigStore::load(Some(home.join("config.toml"))).expect("store");
        store.config.provider = ProviderKind::OpenaiCodex;
        store.config.providers.openai_codex.external_credentials =
            Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                ProviderKind::OpenaiCodex,
                codewhale_config::ExternalCredentialSource::CodexCli,
                auth_path.clone(),
            ));
        let keyring = std::sync::Arc::new(RecordingKeyringStore::default());
        let secrets = Secrets::new(keyring.clone());
        let runtime = CliRuntimeOverrides::default();
        let status =
            auth_status_lines_for_provider(&store, &secrets, ProviderKind::OpenaiCodex).join("\n");
        let get = auth_get_line_with_runtime(&store, &secrets, ProviderKind::OpenaiCodex, &runtime);
        let list = auth_list_lines(&store, &secrets).join("\n");
        let summary = auth_status_all_providers(&store, &secrets).join("\n");
        assert!(status.contains("auth mode: oauth"), "{status}");
        assert!(status.contains("active source: missing"), "{status}");
        assert!(
            status.contains("verified Codewhale-owned ChatGPT sign-in only"),
            "{status}"
        );
        assert!(
            status.contains("CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt"),
            "{status}"
        );
        assert!(
            get.starts_with("openai-codex: not set"),
            "official ChatGPT must remain unset without an owned grant"
        );
        for output in [&status, &get, &list, &summary] {
            assert!(
                !output.contains("secret-99"),
                "authentication diagnostic leaked token material"
            );
            assert!(
                !output.contains("active source: env"),
                "official ChatGPT must not select ambient credentials"
            );
            assert!(
                !output.contains("external-consent"),
                "authentication diagnostic leaked external consent"
            );
        }
        assert!(
            !keyring.queried().iter().any(|slot| slot == "openai-codex"),
            "official ChatGPT must not inspect API-key storage"
        );
        assert_eq!(
            std::fs::read_to_string(auth_path).expect("external trap unchanged"),
            external_raw
        );
    }

    #[test]
    fn chatgpt_custom_endpoint_diagnostics_use_only_route_bound_api_keys() {
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let _token = ScopedEnvVar::set("OPENAI_CODEX_ACCESS_TOKEN", "ambient-secret-9944");
        let _alias = ScopedEnvVar::set("CODEX_ACCESS_TOKEN", "alias-secret-9955");
        let mut store = ConfigStore::load(Some(home.join("config.toml"))).expect("store");
        store.config.provider = ProviderKind::OpenaiCodex;
        let provider = &mut store.config.providers.openai_codex;
        provider.base_url = Some("https://custom.example/v1".to_string());
        provider.api_key = Some("route-bound-secret-9966".to_string());
        provider.auth_mode = Some("oauth".to_string());
        provider.oauth_credential_generation = Some("../must-not-read.json".to_string());
        let secrets = no_keyring_secrets();
        let runtime = CliRuntimeOverrides::default();
        let status =
            auth_status_lines_for_provider(&store, &secrets, ProviderKind::OpenaiCodex).join("\n");
        let get = auth_get_line_with_runtime(&store, &secrets, ProviderKind::OpenaiCodex, &runtime);
        let list = auth_list_lines(&store, &secrets).join("\n");
        let summary = auth_status_all_providers(&store, &secrets).join("\n");
        assert!(status.contains("auth mode: api_key"), "{status}");
        assert!(
            status.contains("active source: config (last4: ...9966)"),
            "{status}"
        );
        for output in [&status, &get, &list, &summary] {
            assert!(
                output.contains("config (last4: ...9966)"),
                "custom endpoint diagnostic must report its bound key source"
            );
            assert!(
                !output.contains("secret-99"),
                "custom endpoint diagnostic leaked token material"
            );
            assert!(
                !output.contains("verified grant"),
                "custom endpoint must not report an official OAuth grant"
            );
        }
        assert!(!status.contains("switch account:"), "{status}");

        let another_endpoint = CliRuntimeOverrides {
            base_url: Some("https://other.example/v1".to_string()),
            ..CliRuntimeOverrides::default()
        };
        let get = auth_get_line_with_runtime(
            &store,
            &secrets,
            ProviderKind::OpenaiCodex,
            &another_endpoint,
        );
        assert!(
            get.starts_with("openai-codex: not set"),
            "a different endpoint must not reuse the configured key"
        );
        assert!(
            !get.contains("9966"),
            "key must remain bound to its configured endpoint"
        );
        let explicit = CliRuntimeOverrides {
            api_key: Some("explicit-secret-9977".to_string()),
            ..another_endpoint
        };
        let get =
            auth_get_line_with_runtime(&store, &secrets, ProviderKind::OpenaiCodex, &explicit);
        assert!(
            get.contains("cli (last4: ...9977)"),
            "an explicit key must report its CLI source"
        );
        assert!(
            !get.contains("explicit-secret"),
            "authentication diagnostic leaked the explicit key"
        );
    }

    #[test]
    fn chatgpt_auth_diagnostics_never_display_custom_url_credentials() {
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let mut store = ConfigStore::load(Some(home.join("config.toml"))).expect("store");
        store.config.provider = ProviderKind::OpenaiCodex;
        let raw_url = "https://private-user:private-password@custom.example/v1?token=private-query-token#private-fragment";
        store.config.providers.openai_codex.base_url = Some(raw_url.to_string());
        store.config.providers.openai_codex.api_key = Some("route-bound-secret-9988".to_string());
        let secrets = no_keyring_secrets();
        let runtime = CliRuntimeOverrides::default();
        let status =
            auth_status_lines_for_provider(&store, &secrets, ProviderKind::OpenaiCodex).join("\n");
        assert!(
            status.contains("route: custom API-key endpoint"),
            "{status}"
        );
        let get = auth_get_line_with_runtime(&store, &secrets, ProviderKind::OpenaiCodex, &runtime);
        let list = auth_list_lines(&store, &secrets).join("\n");
        let summary = auth_status_all_providers(&store, &secrets).join("\n");
        for output in [&status, &get, &list, &summary] {
            for secret in [
                raw_url,
                "private-user",
                "private-password",
                "private-query-token",
                "private-fragment",
                "route-bound-secret",
            ] {
                assert!(
                    !output.contains(secret),
                    "URL or credential leaked in diagnostic output"
                );
            }
        }
    }

    #[test]
    fn owned_subscription_sign_ins_show_account_label_without_token_material() {
        let _lock = env_lock();
        let _codex_token = ScopedEnvVar::remove("OPENAI_CODEX_ACCESS_TOKEN");
        let _codex_alias = ScopedEnvVar::remove("CODEX_ACCESS_TOKEN");
        let _xai_key = ScopedEnvVar::remove("XAI_API_KEY");
        let _xai_base = ScopedEnvVar::remove("XAI_BASE_URL");
        let _auth_mode = ScopedEnvVar::remove("DEEPSEEK_AUTH_MODE");
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let mut store = ConfigStore::load(Some(home.join("config.toml"))).expect("store");
        let chatgpt_generation = "chatgpt-auth-0123456789abcdef0123456789abcdef.json";
        let xai_generation = "xai-auth-0123456789abcdef0123456789abcdef.json";
        store
            .config
            .providers
            .openai_codex
            .oauth_credential_generation = Some(chatgpt_generation.to_string());
        store.config.providers.openai_codex.auth_mode = Some("oauth".to_string());
        store.config.providers.xai.auth_mode = Some("oauth".to_string());
        store.config.providers.xai.oauth_credential_generation = Some(xai_generation.to_string());

        // {"email":"b@example.com","https://api.openai.com/auth":{"chatgpt_plan_type":"pro"}}
        let chatgpt_claims = "eyJlbWFpbCI6ImJAZXhhbXBsZS5jb20iLCJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9wbGFuX3R5cGUiOiJwcm8ifX0";
        // {"email":"grok@example.com"}
        let xai_claims = "eyJlbWFpbCI6Imdyb2tAZXhhbXBsZS5jb20ifQ";
        let chatgpt_file = serde_json::json!({
            "https://auth.openai.com::app_EMoamEEZ73f0CkXaXp7hrann": {
                "access_token": "chatgpt-access-secret-9911",
                "refresh_token": "chatgpt-refresh-secret-9912",
                "id_token": format!("hdr.{chatgpt_claims}.sig-secret-9913"),
            }
        });
        let xai_file = serde_json::json!({
            "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {
                "key": "xai-access-secret-9921",
                "refresh_token": "xai-refresh-secret-9922",
                "id_token": format!("hdr.{xai_claims}.sig-secret-9923"),
            }
        });
        codewhale_config::with_xai_oauth_lifecycle_lock(|owned| {
            owned.write(
                chatgpt_generation,
                chatgpt_file.to_string().as_bytes(),
                false,
            )?;
            owned.write(xai_generation, xai_file.to_string().as_bytes(), false)?;
            Ok(())
        })
        .expect("seed Codewhale-owned sign-ins");
        let secrets = no_keyring_secrets();

        let legacy =
            auth_status_lines_for_provider(&store, &secrets, ProviderKind::OpenaiCodex).join("\n");
        assert!(legacy.contains("active source: missing"), "{legacy}");
        assert!(
            !legacy.contains("b@example.com"),
            "legacy claims must not name an official account: {legacy}"
        );

        // Stored-proof fixture; signed JWT verification is exercised in the
        // OAuth tests. Only the protected, verified grant may supply a label.
        let official_file = serde_json::json!({
            "https://auth.openai.com::oaiapp_codewhale_test": {
                "access_token": "chatgpt-access-secret-9911",
                "refresh_token": "chatgpt-refresh-secret-9912",
                "id_token": format!("hdr.{chatgpt_claims}.sig-secret-9913"),
                "account_id": "test-sub",
                "oidc_issuer": "https://auth.openai.com",
                "oidc_client_id": "oaiapp_codewhale_test",
                "siwc_registration": {
                    "issuer": "https://auth.openai.com",
                    "client_id": "oaiapp_codewhale_test",
                    "subject": "test-sub",
                    "email": "b@example.com",
                    "host_id": "urn:uuid:01234567-89ab-cdef-0123-456789abcdef"
                },
                "siwc_scope": "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
                "siwc_token_sha256": "oPH6E3xSo6jmzfjPkkNVow-MDP5cTEzxdTLxk_woXeY"
            }
        });
        codewhale_config::with_xai_oauth_lifecycle_lock(|owned| {
            owned.write(
                chatgpt_generation,
                official_file.to_string().as_bytes(),
                true,
            )
        })
        .expect("install protected verified-grant fixture");

        let codex =
            auth_status_lines_for_provider(&store, &secrets, ProviderKind::OpenaiCodex).join("\n");
        assert!(
            codex.contains("active source: Codewhale-owned ChatGPT sign-in as b@example.com (pro)"),
            "{codex}"
        );
        assert!(
            codex.contains(
                "switch account: `CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt`"
            ),
            "{codex}"
        );
        let xai = auth_status_lines_for_provider(&store, &secrets, ProviderKind::Xai).join("\n");
        assert!(xai.contains("signed-in account: grok@example.com"), "{xai}");
        assert!(
            xai.contains("switch account: `codewhale auth xai-device`"),
            "{xai}"
        );
        let list = auth_list_lines(&store, &secrets).join("\n");
        assert!(
            list.contains("Codewhale-owned ChatGPT sign-in as b@example.com (pro)"),
            "{list}"
        );
        assert!(
            list.contains("owned-oauth-configured (grok@example.com)"),
            "{list}"
        );
        for output in [&codex, &xai, &list] {
            for secret in ["secret-99", chatgpt_claims, xai_claims] {
                assert!(
                    !output.contains(secret),
                    "owned sign-in diagnostic leaked token material"
                );
            }
        }

        // Ambient tokens never replace the verified own grant or its label.
        {
            let _token = ScopedEnvVar::set("OPENAI_CODEX_ACCESS_TOKEN", "env-token-secret-9931");
            let codex = auth_status_lines_for_provider(&store, &secrets, ProviderKind::OpenaiCodex)
                .join("\n");
            assert!(
                codex.contains("Codewhale-owned ChatGPT sign-in as b@example.com (pro)"),
                "{codex}"
            );
            assert!(
                codex.contains("CODEWHALE_CHATGPT_NEW_ACCOUNT=1 codewhale auth chatgpt"),
                "{codex}"
            );
            assert!(
                !codex.contains("unset OPENAI_CODEX_ACCESS_TOKEN first"),
                "{codex}"
            );
            assert!(!codex.contains("env-token-secret-9931"), "{codex}");
            let get = auth_get_line_with_runtime(
                &store,
                &secrets,
                ProviderKind::OpenaiCodex,
                &CliRuntimeOverrides::default(),
            );
            assert!(
                get.contains("b@example.com (pro)"),
                "owned sign-in diagnostic must retain the selected account label"
            );
            let summary = auth_status_all_providers(&store, &secrets).join("\n");
            assert!(
                summary.contains("Codewhale-owned ChatGPT sign-in as b@example.com (pro)"),
                "{summary}"
            );
        }

        // A missing generation file is reported, not silently dropped.
        std::fs::remove_file(
            codewhale_config::xai_oauth_credentials_dir()
                .expect("credentials dir")
                .join(xai_generation),
        )
        .expect("remove xai generation");
        let xai = auth_status_lines_for_provider(&store, &secrets, ProviderKind::Xai).join("\n");
        assert!(
            xai.contains("signed-in account: none (sign-in file is missing"),
            "{xai}"
        );
        assert!(!xai.contains("storage unprobed"), "{xai}");
    }

    #[test]
    fn xai_valid_owned_generation_blocks_external_consent_without_storage_probes() {
        use std::sync::Arc;

        let _lock = env_lock();
        let _xai_key = ScopedEnvVar::remove("XAI_API_KEY");
        let _xai_base = ScopedEnvVar::remove("XAI_BASE_URL");
        let _auth_mode = ScopedEnvVar::remove("DEEPSEEK_AUTH_MODE");
        let dir = tempfile::TempDir::new().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let external_path = dir.path().join("grok-auth.json");
        let external_raw = "external owner bytes must not be read";
        std::fs::write(&external_path, external_raw).expect("external auth trap");
        let _grok_auth_path = ScopedEnvVar::set("GROK_AUTH_PATH", &external_path.to_string_lossy());

        let mut store = ConfigStore::load(Some(config_path)).expect("store should load");
        store.config.provider = ProviderKind::Xai;
        store.config.providers.xai.auth_mode = Some("oauth".to_string());
        store.config.providers.xai.oauth_credential_generation =
            Some("xai-auth-0123456789abcdef0123456789abcdef.json".to_string());
        store.config.providers.xai.external_credentials =
            Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                ProviderKind::Xai,
                codewhale_config::ExternalCredentialSource::GrokCli,
                external_path.clone(),
            ));
        let keyring = Arc::new(RecordingKeyringStore::default());
        let secrets = Secrets::new(keyring.clone());

        let scoped = auth_status_lines_for_provider(&store, &secrets, ProviderKind::Xai).join("\n");
        assert!(
            scoped.contains(
                "credential route: Codewhale-owned OAuth configured/unprobed (valid generation pointer; availability unprobed)"
            ),
            "{scoped}"
        );
        assert!(scoped.contains("external credentials: blocked by the configured Codewhale-owned xAI OAuth generation"), "{scoped}");
        assert!(
            scoped.contains(
                "xAI OAuth generation: configured Codewhale-owned pointer (opened to read the account label only; token availability not probed)"
            ),
            "{scoped}"
        );
        assert!(
            !scoped.contains("active source: Codewhale-owned OAuth"),
            "a valid pointer is configured/unprobed, not an active credential: {scoped}"
        );
        assert!(
            !scoped.contains("fallback"),
            "an owned generation must never advertise Grok CLI fallback: {scoped}"
        );

        let all = auth_status_all_providers(&store, &secrets).join("\n");
        let xai_row = all
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI status row");
        assert!(
            xai_row.contains("Codewhale-owned OAuth configured/unprobed"),
            "{xai_row}"
        );

        let list = auth_list_lines(&store, &secrets).join("\n");
        let xai_list_row = list
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI list row");
        assert!(
            xai_list_row.ends_with("owned-oauth-configured"),
            "{xai_list_row}"
        );

        let get = auth_get_line_with_runtime(
            &store,
            &secrets,
            ProviderKind::Xai,
            &CliRuntimeOverrides::default(),
        );
        assert!(
            get.starts_with("xai: configured (source: Codewhale-owned OAuth generation"),
            "owned OAuth must be reported as a configured generation"
        );
        assert!(
            !get.starts_with("xai: set"),
            "owned OAuth must not be reported as an API key"
        );
        assert!(
            !get.contains("fallback"),
            "owned OAuth must not report a fallback credential"
        );
        // #6715 review: no surface says "storage unprobed" for a route whose
        // generation `auth status` opens for the account label; only the
        // token's availability is left unverified.
        // The assertion messages deliberately do not interpolate `get`: it is
        // built from fixed source labels only, but it flows from the runtime
        // API-key resolver, so CodeQL's cleartext-logging query treats a
        // formatted copy as a credential sink.
        assert!(
            get.contains("token availability unprobed"),
            "the xAI get line must say only token availability is unprobed"
        );
        assert!(
            !get.contains("storage unprobed"),
            "the xAI get line must not say storage is unprobed"
        );
        assert!(!scoped.contains("storage unprobed"), "{scoped}");
        assert!(
            !keyring.queried().iter().any(|slot| slot == "xai"),
            "owned OAuth diagnostics must not query the xAI API-key store: {:?}",
            keyring.queried()
        );
        assert_eq!(
            std::fs::read_to_string(external_path).expect("external trap unchanged"),
            external_raw
        );

        store.config.providers.xai.auth_mode = None;
        store.config.auth_mode = Some("oauth".to_string());
        assert_eq!(
            xai_auth_diagnostics(&store, &CliRuntimeOverrides::default()).route,
            XaiAuthDiagnosticRoute::ApiKey,
            "a root auth mode must not select the xAI OAuth runtime route"
        );
    }

    #[test]
    fn xai_invalid_generation_requires_repair_blocks_external_and_keeps_api_key_diagnostics() {
        use std::sync::Arc;

        let _lock = env_lock();
        let _xai_key = ScopedEnvVar::remove("XAI_API_KEY");
        let _xai_base = ScopedEnvVar::remove("XAI_BASE_URL");
        let _auth_mode = ScopedEnvVar::remove("DEEPSEEK_AUTH_MODE");
        let dir = tempfile::TempDir::new().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let external_path = dir.path().join("grok-auth.json");
        let external_raw = "external owner bytes must remain unread";
        std::fs::write(&external_path, external_raw).expect("external auth trap");
        let _grok_auth_path = ScopedEnvVar::set("GROK_AUTH_PATH", &external_path.to_string_lossy());

        let mut store = ConfigStore::load(Some(config_path)).expect("store should load");
        store.config.provider = ProviderKind::Xai;
        store.config.providers.xai.auth_mode = Some("oauth".to_string());
        store.config.providers.xai.api_key = Some("fake-cfg-key-1234".to_string());
        store.config.providers.xai.oauth_credential_generation = Some("../unsafe.json".to_string());
        store.config.providers.xai.external_credentials =
            Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                ProviderKind::Xai,
                codewhale_config::ExternalCredentialSource::GrokCli,
                external_path.clone(),
            ));
        let keyring = Arc::new(RecordingKeyringStore::default());
        let secrets = Secrets::new(keyring.clone());

        let scoped = auth_status_lines_for_provider(&store, &secrets, ProviderKind::Xai).join("\n");
        assert!(
            scoped.contains("credential route: xAI OAuth needs repair"),
            "{scoped}"
        );
        assert!(
            scoped.contains("API-key fallback: config (last4: ...1234)"),
            "{scoped}"
        );
        assert!(scoped.contains("external credentials: blocked by the invalid Codewhale-owned xAI OAuth generation pointer"), "{scoped}");
        assert!(
            scoped.contains("repair: run `codewhale auth xai-device`"),
            "{scoped}"
        );
        assert!(
            !scoped.contains("external read-only consent (availability not probed)"),
            "invalid owned pointers must not activate Grok CLI consent: {scoped}"
        );

        let all = auth_status_all_providers(&store, &secrets).join("\n");
        let xai_row = all
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI status row");
        assert!(xai_row.contains("needs repair"), "{xai_row}");
        assert!(xai_row.contains("API-key fallback: config"), "{xai_row}");

        let list = auth_list_lines(&store, &secrets).join("\n");
        let xai_list_row = list
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI list row");
        assert!(xai_list_row.ends_with("needs-repair"), "{xai_list_row}");

        let get = auth_get_line_with_runtime(
            &store,
            &secrets,
            ProviderKind::Xai,
            &CliRuntimeOverrides::default(),
        );
        assert!(get.contains("xai: needs repair"), "{get}");
        assert!(get.contains("API-key fallback: config-file"), "{get}");
        assert!(
            !keyring.queried().iter().any(|slot| slot == "xai"),
            "an invalid owned pointer must not query the xAI API-key store: {:?}",
            keyring.queried()
        );
        assert_eq!(
            std::fs::read_to_string(external_path).expect("external trap unchanged"),
            external_raw
        );
    }

    #[test]
    fn xai_cli_custom_endpoint_rejects_inherited_api_key_sources() {
        use std::sync::Arc;

        let _lock = env_lock();
        let _xai_key = ScopedEnvVar::set("XAI_API_KEY", "fake-ambient-key-3333");
        let _xai_base = ScopedEnvVar::remove("XAI_BASE_URL");
        let _auth_mode = ScopedEnvVar::remove("DEEPSEEK_AUTH_MODE");
        let dir = tempfile::TempDir::new().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let external_path = dir.path().join("grok-auth.json");
        let external_raw = "external owner bytes must remain unprobed";
        std::fs::write(&external_path, external_raw).expect("external auth trap");
        let _grok_auth_path = ScopedEnvVar::set("GROK_AUTH_PATH", &external_path.to_string_lossy());

        let mut store = ConfigStore::load(Some(config_path)).expect("store should load");
        store.config.provider = ProviderKind::Xai;
        store.config.providers.xai.api_key = Some("fake-cfg-key-1111".to_string());
        store.config.providers.xai.auth_mode = Some("oauth".to_string());
        store.config.providers.xai.oauth_credential_generation =
            Some("xai-auth-0123456789abcdef0123456789abcdef.json".to_string());
        store.config.providers.xai.external_credentials =
            Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                ProviderKind::Xai,
                codewhale_config::ExternalCredentialSource::GrokCli,
                external_path.clone(),
            ));
        let keyring = Arc::new(RecordingKeyringStore::default());
        keyring.set_value("xai", "fake-store-key-2222");
        let secrets = Secrets::new(keyring.clone());
        let runtime_overrides = CliRuntimeOverrides {
            base_url: Some("https://gateway.example.test/v1".to_string()),
            ..CliRuntimeOverrides::default()
        };

        let scoped = auth_status_lines_for_provider_with_runtime(
            &store,
            &secrets,
            ProviderKind::Xai,
            &runtime_overrides,
        )
        .join("\n");
        assert!(
            scoped.contains("route: https://gateway.example.test/v1"),
            "{scoped}"
        );
        assert!(scoped.contains("credential route: missing"), "{scoped}");
        assert!(
            scoped.contains("custom xAI endpoint; API-key-only"),
            "{scoped}"
        );
        assert!(
            scoped.contains("not eligible for this custom xAI endpoint"),
            "{scoped}"
        );
        assert!(
            scoped.contains("external credentials: unavailable on a custom xAI endpoint"),
            "{scoped}"
        );
        for redacted_tail in ["...1111", "...2222", "...3333"] {
            assert!(
                !scoped.contains(redacted_tail),
                "custom CLI route must not advertise an inherited credential: {scoped}"
            );
        }

        let all =
            auth_status_all_providers_with_runtime(&store, &secrets, &runtime_overrides).join("\n");
        let xai_row = all
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI status row");
        assert!(xai_row.contains("unset"), "{xai_row}");
        assert!(
            !xai_row.contains("config") && !xai_row.contains("keyring") && !xai_row.contains("env"),
            "xAI summary must show runtime-effective sources only: {xai_row}"
        );

        let list = auth_list_lines_with_runtime(&store, &secrets, &runtime_overrides).join("\n");
        let xai_list_row = list
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI list row");
        assert!(xai_list_row.ends_with("missing"), "{xai_list_row}");

        let get =
            auth_get_line_with_runtime(&store, &secrets, ProviderKind::Xai, &runtime_overrides);
        assert_eq!(get, "xai: not set");
        assert!(
            !keyring.queried().iter().any(|slot| slot == "xai"),
            "a global custom endpoint must not query xAI keyring state: {:?}",
            keyring.queried()
        );
        assert_eq!(
            std::fs::read_to_string(external_path).expect("external trap unchanged"),
            external_raw
        );
    }

    #[test]
    fn xai_env_custom_endpoint_rejects_inherited_api_key_sources() {
        use std::sync::Arc;

        let _lock = env_lock();
        let _xai_key = ScopedEnvVar::set("XAI_API_KEY", "fake-ambient-key-6666");
        let _xai_base = ScopedEnvVar::set("XAI_BASE_URL", "https://env-gateway.example.test/v1");
        let _auth_mode = ScopedEnvVar::remove("DEEPSEEK_AUTH_MODE");
        let dir = tempfile::TempDir::new().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let external_path = dir.path().join("grok-auth.json");
        let external_raw = "external owner bytes must remain unprobed";
        std::fs::write(&external_path, external_raw).expect("external auth trap");
        let _grok_auth_path = ScopedEnvVar::set("GROK_AUTH_PATH", &external_path.to_string_lossy());

        let mut store = ConfigStore::load(Some(config_path)).expect("store should load");
        store.config.provider = ProviderKind::Xai;
        store.config.providers.xai.api_key = Some("fake-cfg-key-4444".to_string());
        store.config.providers.xai.auth_mode = Some("oauth".to_string());
        store.config.providers.xai.oauth_credential_generation =
            Some("xai-auth-0123456789abcdef0123456789abcdef.json".to_string());
        store.config.providers.xai.external_credentials =
            Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                ProviderKind::Xai,
                codewhale_config::ExternalCredentialSource::GrokCli,
                external_path.clone(),
            ));
        let keyring = Arc::new(RecordingKeyringStore::default());
        keyring.set_value("xai", "fake-store-key-5555");
        let secrets = Secrets::new(keyring.clone());

        let scoped = auth_status_lines_for_provider(&store, &secrets, ProviderKind::Xai).join("\n");
        assert!(
            scoped.contains("route: https://env-gateway.example.test/v1"),
            "{scoped}"
        );
        assert!(scoped.contains("credential route: missing"), "{scoped}");
        assert!(
            scoped.contains("custom xAI endpoint; API-key-only"),
            "{scoped}"
        );
        for redacted_tail in ["...4444", "...5555", "...6666"] {
            assert!(
                !scoped.contains(redacted_tail),
                "custom env route must not advertise an inherited credential: {scoped}"
            );
        }

        let all = auth_status_all_providers(&store, &secrets).join("\n");
        let xai_row = all
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI status row");
        assert!(xai_row.contains("unset"), "{xai_row}");

        let list = auth_list_lines(&store, &secrets).join("\n");
        let xai_list_row = list
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI list row");
        assert!(xai_list_row.ends_with("missing"), "{xai_list_row}");

        assert_eq!(
            auth_get_line_with_runtime(
                &store,
                &secrets,
                ProviderKind::Xai,
                &CliRuntimeOverrides::default(),
            ),
            "xai: not set"
        );
        assert!(
            !keyring.queried().iter().any(|slot| slot == "xai"),
            "an XAI_BASE_URL custom route must not query xAI keyring state: {:?}",
            keyring.queried()
        );
        assert_eq!(
            std::fs::read_to_string(external_path).expect("external trap unchanged"),
            external_raw
        );
    }

    #[test]
    fn xai_config_bound_custom_endpoint_uses_its_route_key() {
        use std::sync::Arc;

        let _lock = env_lock();
        let _xai_key = ScopedEnvVar::remove("XAI_API_KEY");
        let _xai_base = ScopedEnvVar::remove("XAI_BASE_URL");
        let dir = tempfile::TempDir::new().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let mut store = ConfigStore::load(Some(config_path)).expect("store should load");
        store.config.provider = ProviderKind::Xai;
        store.config.providers.xai.base_url =
            Some("https://bound-gateway.example.test/v1".to_string());
        store.config.providers.xai.api_key = Some("fake-bound-key-7777".to_string());
        let keyring = Arc::new(RecordingKeyringStore::default());
        keyring.set_value("xai", "fake-store-key-8888");
        let secrets = Secrets::new(keyring.clone());

        let scoped = auth_status_lines_for_provider(&store, &secrets, ProviderKind::Xai).join("\n");
        assert!(
            scoped.contains("credential route: config (last4: ...7777)"),
            "{scoped}"
        );
        assert!(
            scoped.contains("config file:") && scoped.contains("runtime-effective, last4: ...7777"),
            "{scoped}"
        );
        assert_eq!(
            auth_get_line_with_runtime(
                &store,
                &secrets,
                ProviderKind::Xai,
                &CliRuntimeOverrides::default(),
            ),
            "xai: set (source: config-file)"
        );
        assert!(
            !keyring.queried().iter().any(|slot| slot == "xai"),
            "an endpoint-bound config key should resolve before the xAI keyring: {:?}",
            keyring.queried()
        );
    }

    #[test]
    fn xai_absent_generation_with_consent_is_external_configured_and_unprobed() {
        use std::sync::Arc;

        let _lock = env_lock();
        let _xai_key = ScopedEnvVar::remove("XAI_API_KEY");
        let _xai_base = ScopedEnvVar::remove("XAI_BASE_URL");
        let _auth_mode = ScopedEnvVar::remove("DEEPSEEK_AUTH_MODE");
        let dir = tempfile::TempDir::new().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let external_path = dir.path().join("grok-auth.json");
        let external_raw = "external owner bytes remain unprobed";
        std::fs::write(&external_path, external_raw).expect("external auth trap");
        let _grok_auth_path = ScopedEnvVar::set("GROK_AUTH_PATH", &external_path.to_string_lossy());

        let mut store = ConfigStore::load(Some(config_path)).expect("store should load");
        store.config.provider = ProviderKind::Xai;
        store.config.providers.xai.auth_mode = Some("oauth".to_string());
        store.config.providers.xai.external_credentials =
            Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                ProviderKind::Xai,
                codewhale_config::ExternalCredentialSource::GrokCli,
                external_path.clone(),
            ));
        let keyring = Arc::new(RecordingKeyringStore::default());
        let secrets = Secrets::new(keyring.clone());

        let scoped = auth_status_lines_for_provider(&store, &secrets, ProviderKind::Xai).join("\n");
        assert!(
            scoped.contains("credential route: external read-only consent configured/unprobed"),
            "{scoped}"
        );
        assert!(
            scoped.contains("external credentials: read_only"),
            "{scoped}"
        );
        assert!(
            scoped.contains(
                "lookup order: configured consent-gated exact Grok CLI file (availability unprobed)"
            ),
            "{scoped}"
        );

        let all = auth_status_all_providers(&store, &secrets).join("\n");
        let xai_row = all
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI status row");
        assert!(
            xai_row.contains("external consent configured/unprobed"),
            "{xai_row}"
        );

        let list = auth_list_lines(&store, &secrets).join("\n");
        let xai_list_row = list
            .lines()
            .find(|line| line.starts_with("xai"))
            .expect("xAI list row");
        assert!(
            xai_list_row.ends_with("external-consent-configured"),
            "{xai_list_row}"
        );

        let get = auth_get_line_with_runtime(
            &store,
            &secrets,
            ProviderKind::Xai,
            &CliRuntimeOverrides::default(),
        );
        assert!(
            get.contains("source: external read-only consent; availability unprobed"),
            "{get}"
        );
        assert!(
            !keyring.queried().iter().any(|slot| slot == "xai"),
            "external-consent diagnostics must not query the xAI API-key store: {:?}",
            keyring.queried()
        );
        assert_eq!(
            std::fs::read_to_string(external_path).expect("external trap unchanged"),
            external_raw
        );
    }

    #[test]
    fn auth_list_keeps_legacy_codex_consent_inactive_without_probing_file() {
        use codewhale_secrets::InMemoryKeyringStore;
        use std::sync::Arc;

        let _lock = env_lock();
        let _access_token = ScopedEnvVar::set("OPENAI_CODEX_ACCESS_TOKEN", "");
        let _codex_token = ScopedEnvVar::set("CODEX_ACCESS_TOKEN", "");

        let dir = tempfile::TempDir::new().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let auth_path = dir.path().join("auth.json");
        std::fs::write(&auth_path, r#"{"tokens":{"access_token":"secret-token"}}"#)
            .expect("write auth file");
        let auth_path_str = auth_path.to_string_lossy().into_owned();
        let _auth_file = ScopedEnvVar::set("OPENAI_CODEX_AUTH_FILE", &auth_path_str);

        let mut store = ConfigStore::load(Some(config_path)).expect("store should load");
        store.config.provider = ProviderKind::OpenaiCodex;
        store.config.providers.openai_codex.external_credentials =
            Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                ProviderKind::OpenaiCodex,
                codewhale_config::ExternalCredentialSource::CodexCli,
                auth_path,
            ));
        let secrets = Secrets::new(Arc::new(InMemoryKeyringStore::new()));

        let output = auth_list_lines(&store, &secrets).join("\n");
        let row = output
            .lines()
            .find(|line| line.starts_with("openai-codex"))
            .unwrap_or_else(|| panic!("missing openai-codex row:\n{output}"));
        assert!(
            row.contains("missing (run `codewhale auth chatgpt`"),
            "{row}"
        );
        assert!(!row.contains("external-consent"), "{row}");
        assert!(!output.contains("secret-token"));
    }

    #[test]
    fn auth_list_labels_each_row_by_its_own_provider() {
        // ProviderKind::secret_store_slot collapses families onto one durable
        // slot -- SiliconflowCN onto `siliconflow`, the four Model Studio
        // variants onto `modelstudio-token-plan` -- but this table has one row
        // per kind. Labelling rows by slot printed `siliconflow` twice and
        // `modelstudio-token-plan` four times, so a reader could not tell which
        // row belonged to which provider.
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let store =
            ConfigStore::load(Some(dir.path().join("config.toml"))).expect("store should load");
        let secrets = Secrets::new(std::sync::Arc::new(
            codewhale_secrets::InMemoryKeyringStore::new(),
        ));

        let lines = auth_list_lines(&store, &secrets);
        let labels: Vec<&str> = lines
            .iter()
            .skip(1)
            .filter_map(|line| line.split_whitespace().next())
            .collect();

        assert_eq!(
            labels.len(),
            ProviderKind::ALL.len(),
            "one row per provider kind: {labels:?}"
        );
        let unique: std::collections::BTreeSet<&&str> = labels.iter().collect();
        assert_eq!(
            unique.len(),
            labels.len(),
            "every row must name its own provider, not a shared slot: {labels:?}"
        );
    }

    #[test]
    fn external_consent_persists_exact_scope_and_api_key_or_revoke_disables_it() {
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let config_path = dir.path().join("config.toml");
        let external_path = dir.path().join("grok-auth.json");
        let external_raw = r#"{"secret":"must-never-be-read-or-written"}"#;
        std::fs::write(&external_path, external_raw).expect("external auth trap");
        let mut store = ConfigStore::load(Some(config_path.clone())).expect("store should load");
        let secrets = no_keyring_secrets();

        let preview = external_consent_preview_lines(
            ProviderKind::Xai,
            codewhale_config::ExternalCredentialSource::GrokCli,
            &external_path,
        )
        .join("\n");
        assert!(preview.contains("owning CLI: Grok CLI"), "{preview}");
        assert!(
            preview.contains(&format!(
                "exact resolved path: {}",
                codewhale_config::quote_os_path(&external_path)
            )),
            "{preview}"
        );
        assert!(preview.contains("no refresh, identity-provider or discovery requests"));
        assert!(preview.contains("normal requests to the explicitly selected provider"));
        assert!(preview.contains("managed: unavailable"));

        let mut prompt = Vec::new();
        confirm_external_consent_answer(&mut "yes\n".as_bytes(), &mut prompt)
            .expect("exact yes confirms");
        assert!(
            String::from_utf8(prompt)
                .unwrap()
                .contains("exact read-only")
        );
        let cancelled = confirm_external_consent_answer(&mut "YES\n".as_bytes(), &mut Vec::new())
            .expect_err("confirmation is deliberate and case-sensitive");
        assert!(cancelled.to_string().contains("cancelled"));

        let unconfirmed = run_auth_command_with_secrets(
            &mut store,
            AuthCommand::ExternalConsent {
                provider: ProviderKind::Xai,
                mode: ExternalCredentialModeArg::ReadOnly,
                path: Some(external_path.clone()),
                yes: false,
            },
            &secrets,
        )
        .expect_err("non-interactive consent requires --yes");
        assert!(unconfirmed.to_string().contains("requires explicit --yes"));
        assert!(store.config.providers.xai.external_credentials.is_none());
        assert!(
            !config_path.exists(),
            "unconfirmed consent must not persist"
        );

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::ExternalConsent {
                provider: ProviderKind::Xai,
                mode: ExternalCredentialModeArg::ReadOnly,
                path: Some(external_path.clone()),
                yes: true,
            },
            &secrets,
        )
        .expect("read-only consent should persist");

        let consent = store
            .config
            .providers
            .xai
            .external_credentials
            .as_ref()
            .expect("persisted consent");
        assert_eq!(
            consent.access,
            codewhale_config::ExternalCredentialAccess::ReadOnly
        );
        assert_eq!(consent.provider, ProviderKind::Xai.as_str());
        assert_eq!(
            consent.source,
            codewhale_config::ExternalCredentialSource::GrokCli
        );
        assert_eq!(consent.path, external_path);
        assert_eq!(
            consent.consent_version,
            codewhale_config::EXTERNAL_CREDENTIAL_CONSENT_VERSION
        );
        assert_eq!(
            store.config.providers.xai.auth_mode.as_deref(),
            Some("oauth")
        );
        assert_eq!(
            std::fs::read_to_string(&consent.path).expect("external file unchanged"),
            external_raw
        );

        let reloaded = ConfigStore::load(Some(config_path.clone())).expect("reload consent");
        let reloaded_consent = reloaded
            .config
            .providers
            .xai
            .external_credentials
            .as_ref()
            .expect("reloaded exact consent");
        assert_eq!(reloaded_consent.provider, ProviderKind::Xai.as_str());
        assert_eq!(
            reloaded_consent.source,
            codewhale_config::ExternalCredentialSource::GrokCli
        );
        assert_eq!(reloaded_consent.path, external_path);
        assert_eq!(
            reloaded_consent.consent_version,
            codewhale_config::EXTERNAL_CREDENTIAL_CONSENT_VERSION
        );

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Set {
                provider: ProviderKind::Xai,
                api_key: Some("xai-codewhale-owned-key".to_string()),
                api_key_stdin: false,
            },
            &secrets,
        )
        .expect("Codewhale-owned API key should supersede external consent");
        assert!(store.config.providers.xai.external_credentials.is_none());
        assert_eq!(
            std::fs::read_to_string(&external_path).expect("external file still unchanged"),
            external_raw
        );

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::ExternalConsent {
                provider: ProviderKind::Xai,
                mode: ExternalCredentialModeArg::ReadOnly,
                path: Some(external_path.clone()),
                yes: true,
            },
            &secrets,
        )
        .expect("consent can be granted again");
        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::ExternalRevoke {
                provider: ProviderKind::Xai,
            },
            &secrets,
        )
        .expect("revoke should persist");
        assert!(store.config.providers.xai.external_credentials.is_none());
        assert_eq!(
            std::fs::read_to_string(&external_path).expect("revoke never touches external file"),
            external_raw
        );
    }

    #[test]
    fn unsupported_managed_and_kimi_external_consent_fail_closed() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let config_path = dir.path().join("config.toml");
        let external_path = dir.path().join("external-auth.json");
        std::fs::write(&external_path, "must remain unchanged").expect("external fixture");
        let mut store = ConfigStore::load(Some(config_path.clone())).expect("store should load");
        let secrets = no_keyring_secrets();

        let managed = run_auth_command_with_secrets(
            &mut store,
            AuthCommand::ExternalConsent {
                provider: ProviderKind::OpenaiCodex,
                mode: ExternalCredentialModeArg::Managed,
                path: Some(external_path.clone()),
                yes: true,
            },
            &secrets,
        )
        .expect_err("managed access must fail without a preservation adapter");
        assert!(
            managed
                .to_string()
                .contains("schema-safe preservation adapter")
        );

        let kimi = run_auth_command_with_secrets(
            &mut store,
            AuthCommand::ExternalConsent {
                provider: ProviderKind::Moonshot,
                mode: ExternalCredentialModeArg::ReadOnly,
                path: Some(external_path.clone()),
                yes: true,
            },
            &secrets,
        )
        .expect_err("Kimi must remain API-key-only");
        assert!(kimi.to_string().contains("API-key-only"));
        assert!(
            kimi.to_string()
                .contains("https://platform.kimi.ai/console/api-keys")
        );
        assert!(
            store
                .config
                .providers
                .openai_codex
                .external_credentials
                .is_none()
        );
        assert!(
            store
                .config
                .providers
                .moonshot
                .external_credentials
                .is_none()
        );
        assert_eq!(
            std::fs::read_to_string(external_path).expect("external fixture unchanged"),
            "must remain unchanged"
        );
        assert!(
            !config_path.exists(),
            "rejected consent must not write config"
        );
    }

    #[test]
    fn api_key_config_failure_restores_absent_and_existing_secret_state() {
        let _lock = env_lock();
        for prior in [None, Some("prior-xai-key")] {
            let dir = tempfile::TempDir::new().expect("tempdir");
            let home = dir
                .path()
                .canonicalize()
                .expect("canonical temp root")
                .join("codewhale-home");
            let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
            let config_path = dir.path().join("config.toml");
            let mut store = ConfigStore::load(Some(config_path.clone())).expect("load store");
            store.config.providers.xai.auth_mode = Some("oauth".to_string());
            store.config.providers.xai.external_credentials =
                Some(codewhale_config::ExternalCredentialConsentToml::read_only(
                    ProviderKind::Xai,
                    codewhale_config::ExternalCredentialSource::GrokCli,
                    dir.path().join("external.json"),
                ));
            std::fs::create_dir(&config_path).expect("turn config target into a directory");
            let secrets = no_keyring_secrets();
            if let Some(prior) = prior {
                secrets.set("xai", prior).expect("seed prior secret");
            }

            let error = run_auth_command_with_secrets(
                &mut store,
                AuthCommand::Set {
                    provider: ProviderKind::Xai,
                    api_key: Some("new-xai-key".to_string()),
                    api_key_stdin: false,
                },
                &secrets,
            )
            .expect_err("config write must fail");
            assert!(error.to_string().contains("config"), "{error:#}");
            assert_eq!(
                secrets.get("xai").expect("restored secret"),
                prior.map(str::to_string)
            );
            assert_eq!(
                store.config.providers.xai.auth_mode.as_deref(),
                Some("oauth")
            );
            assert!(store.config.providers.xai.external_credentials.is_some());
            assert!(store.config.providers.xai.api_key.is_none());
            assert!(config_path.is_dir());
        }
    }

    #[test]
    fn auth_status_scoped_provider_shows_detailed_info() {
        use codewhale_secrets::InMemoryKeyringStore;
        use std::sync::Arc;

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-scoped-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.provider = ProviderKind::Deepseek;
        store.config.providers.arcee.api_key = Some("sk-arcee-9999".to_string());

        let secrets = Secrets::new(Arc::new(InMemoryKeyringStore::new()));

        let output =
            auth_status_lines_for_provider(&store, &secrets, ProviderKind::Arcee).join("\n");

        assert!(output.contains("provider: arcee"));
        assert!(output.contains("active source: config (last4: ...9999)"));
        assert!(output.contains("route:"));
        assert!(output.contains("model:"));
        assert!(!output.contains("sk-arcee-9999"));

        for sentinel in [codewhale_config::API_KEYRING_SENTINEL, "  __KEYRING__  "] {
            store.config.providers.arcee.api_key = Some(sentinel.to_string());
            assert_eq!(provider_config_api_key(&store, ProviderKind::Arcee), None);
        }

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn dispatch_uses_secret_store_without_rehydrating_plaintext_config() {
        use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        // Runtime resolution reads process-global provider environment overrides.
        // Serialize with the tests that temporarily set those overrides so this
        // in-memory DeepSeek credential is not resolved against another provider.
        let _lock = env_lock();
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-dispatch-keyring-heal-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        let inner = Arc::new(InMemoryKeyringStore::new());
        inner.set("deepseek", "ring-key").unwrap();
        let secrets = Secrets::new(inner);

        let resolved = resolve_runtime_for_dispatch_with_secrets(
            &mut store,
            &CliRuntimeOverrides::default(),
            &secrets,
        );

        assert_eq!(resolved.api_key.as_deref(), Some("ring-key"));
        assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Keyring));
        assert!(store.config.providers.deepseek.api_key.is_none());
        assert!(
            !path.exists(),
            "dispatch must not create config from a stored key"
        );

        let resolved_again = resolve_runtime_for_dispatch_with_secrets(
            &mut store,
            &CliRuntimeOverrides::default(),
            &secrets,
        );
        assert_eq!(resolved_again.api_key.as_deref(), Some("ring-key"));
        assert_eq!(
            resolved_again.api_key_source,
            Some(RuntimeApiKeySource::Keyring)
        );
        assert!(
            !path.exists(),
            "repeat dispatch must remain credential-file free"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn logout_confirmation_accepts_only_an_explicit_yes() {
        let mut out = Vec::new();
        confirm_logout_answer(&mut std::io::Cursor::new("yes\n"), &mut out).expect("yes confirms");
        assert!(String::from_utf8_lossy(&out).contains("Type 'yes' to log out"));
        confirm_logout_answer(&mut std::io::Cursor::new("Y\n"), &mut Vec::new())
            .expect("y confirms");
        for answer in ["\n", "no\n", "", "yess\n"] {
            let err = confirm_logout_answer(&mut std::io::Cursor::new(answer), &mut Vec::new())
                .expect_err("anything else cancels");
            assert!(
                err.to_string().contains("no credentials were deleted"),
                "{err}"
            );
        }
        assert!(confirm_logout(true).is_ok(), "--yes skips the prompt");
    }

    #[test]
    fn logout_parses_yes_flag() {
        let cli = parse_ok(&["codewhale", "logout", "--yes"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Logout(LogoutArgs { yes: true }))
        ));
        let cli = parse_ok(&["codewhale", "logout"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Logout(LogoutArgs { yes: false }))
        ));
    }

    #[test]
    fn logout_removes_plaintext_provider_keys() {
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let path = home.join("config.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.providers.deepseek.api_key = Some("sk-stale".to_string());
        store.config.providers.fireworks.api_key = Some("fw-stale".to_string());
        store.config.providers.xai.auth_mode = Some("oauth".to_string());
        let generation = "xai-auth-0123456789abcdef0123456789abcdef.json";
        store.config.providers.xai.oauth_credential_generation = Some(generation.to_string());
        store.save().unwrap();
        let credentials = home.join("credentials");
        codewhale_config::with_xai_oauth_lifecycle_lock(|owned| {
            owned.write(generation, b"xai-generation", false)?;
            owned.write(
                codewhale_config::LEGACY_XAI_OAUTH_FILE_NAME,
                b"legacy-xai",
                false,
            )?;
            Ok(())
        })
        .expect("seed Codewhale-owned xAI credentials");
        std::fs::write(credentials.join("other-provider.json"), "preserve").unwrap();

        let secrets = no_keyring_secrets();

        run_logout_command_with_secrets(&mut store, &secrets, None).expect("logout should succeed");
        assert!(store.config.providers.deepseek.api_key.is_none());
        assert!(store.config.providers.fireworks.api_key.is_none());
        assert!(store.config.providers.xai.auth_mode.is_none());
        assert!(
            store
                .config
                .providers
                .xai
                .oauth_credential_generation
                .is_none()
        );
        assert!(!credentials.join(generation).exists());
        assert!(!credentials.join("xai-auth.json").exists());
        assert!(credentials.join("other-provider.json").exists());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn logout_finishes_revocation_and_other_deletions_before_reporting_failures() {
        use codewhale_secrets::account::{
            ACCOUNT_API_BASE_ENV, AccountAuthBundle, AccountSessionStore, DEFAULT_ACCOUNT_API_BASE,
            secure_account_session_secrets,
        };

        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let _api_base = ScopedEnvVar::remove(ACCOUNT_API_BASE_ENV);
        let mut store = ConfigStore::load(Some(home.join("config.toml"))).expect("load config");
        let generation = "xai-auth-0123456789abcdef0123456789abcdef.json";
        store.config.providers.xai.auth_mode = Some("oauth".into());
        store.config.providers.xai.oauth_credential_generation = Some(generation.into());
        store.save().expect("save xAI authority");
        let owned_files = [
            generation,
            codewhale_config::LEGACY_XAI_OAUTH_FILE_NAME,
            codewhale_config::LEGACY_CHATGPT_OAUTH_FILE_NAME,
        ];
        codewhale_config::with_xai_oauth_lifecycle_lock(|owned| {
            for name in owned_files {
                owned.write(name, b"test-oauth-credential", false)?;
            }
            Ok(())
        })
        .expect("seed OAuth credentials");
        let account = AccountSessionStore::new(
            secure_account_session_secrets().expect("account store"),
            None,
            DEFAULT_ACCOUNT_API_BASE,
        );
        account
            .save(AccountAuthBundle {
                token_type: "Bearer".into(),
                access_token: "test-access".into(),
                refresh_token: "test-refresh".into(),
                session: None,
                user: None,
            })
            .expect("seed account session");
        let failing_slot = provider_slot(ProviderKind::Deepseek);
        let keyring = RecordingKeyringStore {
            fail_delete_slot: Some(failing_slot),
            ..RecordingKeyringStore::default()
        };
        for provider in [ProviderKind::Deepseek, ProviderKind::Fireworks] {
            keyring.set_value(provider_slot(provider), "test-credential");
        }
        keyring.set_value(codewhale_secrets::DAYTONA_TOKEN_SLOT, "test-daytona");
        let secrets = Secrets::new(std::sync::Arc::new(keyring));
        let error = run_logout_command_with_secrets(&mut store, &secrets, None)
            .expect_err("partial logout must fail");
        assert!(error.to_string().contains("logout incomplete"));
        for name in owned_files {
            assert!(
                !home.join("credentials").join(name).exists(),
                "{name} survived"
            );
        }
        assert!(error.to_string().contains(failing_slot));
        assert!(secrets.get(failing_slot).unwrap().is_some());
        assert!(
            secrets
                .get(provider_slot(ProviderKind::Fireworks))
                .unwrap()
                .is_none()
        );
        assert!(
            secrets
                .get(codewhale_secrets::DAYTONA_TOKEN_SLOT)
                .unwrap()
                .is_none()
        );
        assert!(account.load().expect("load cleared account").is_none());
        let saved = ConfigStore::load(Some(home.join("config.toml"))).expect("reload config");
        assert!(saved.config.providers.xai.auth_mode.is_none());
        assert!(
            saved
                .config
                .providers
                .xai
                .oauth_credential_generation
                .is_none()
        );
    }

    /// R02-08: a slot the keyring cannot read may still hold a key. Logout
    /// must attempt its delete and report the failure, never print success.
    #[test]
    fn logout_reports_a_key_it_could_neither_read_nor_delete() {
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let _api_base = ScopedEnvVar::remove(codewhale_secrets::account::ACCOUNT_API_BASE_ENV);
        let mut store = ConfigStore::load(Some(home.join("config.toml"))).expect("load config");
        let locked_slot = provider_slot(ProviderKind::Deepseek);
        let keyring = RecordingKeyringStore {
            fail_get_slot: Some(locked_slot),
            fail_delete_slot: Some(locked_slot),
            ..RecordingKeyringStore::default()
        };
        keyring.set_value(locked_slot, "test-credential");
        let secrets = Secrets::new(std::sync::Arc::new(keyring));
        let error = run_logout_command_with_secrets(&mut store, &secrets, None)
            .expect_err("an unconfirmed deletion must fail logout");
        assert!(error.to_string().contains("logout incomplete"), "{error}");
        assert!(error.to_string().contains(locked_slot), "{error}");
    }

    #[test]
    fn logout_clears_keyring_credentials_for_all_providers() {
        // Logout used to delete the keyring secret only for the *active*
        // provider, leaving credentials stored under other providers
        // behind while printing "logged out".
        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let path = home.join("config.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.provider = ProviderKind::Deepseek;

        let secrets = no_keyring_secrets();
        secrets
            .set(provider_slot(ProviderKind::Deepseek), "sk-deepseek")
            .expect("seed deepseek key");
        secrets
            .set(provider_slot(ProviderKind::Fireworks), "fw-stale")
            .expect("seed fireworks key");

        run_logout_command_with_secrets(&mut store, &secrets, None).expect("logout should succeed");

        for provider in [ProviderKind::Deepseek, ProviderKind::Fireworks] {
            assert!(
                provider_keyring_api_key(&secrets, provider).is_none(),
                "keyring credential for {provider:?} survived logout"
            );
        }

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn logout_clears_account_session_and_daytona_slot() {
        use codewhale_secrets::account::{
            AccountAuthBundle, AccountSession, AccountSessionStore, AccountUser,
            DEFAULT_ACCOUNT_API_BASE, secure_account_session_secrets,
        };

        let _lock = env_lock();
        let dir = tempfile::TempDir::new().expect("tempdir");
        let home = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("codewhale-home");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", &home.to_string_lossy());
        let path = home.join("config.toml");
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");

        let secrets = no_keyring_secrets();
        secrets
            .set(codewhale_secrets::DAYTONA_TOKEN_SLOT, "dtn_logout")
            .expect("seed daytona token");

        let account = secure_account_session_secrets().expect("account store");
        AccountSessionStore::new(account, None, DEFAULT_ACCOUNT_API_BASE)
            .save(AccountAuthBundle {
                token_type: "Bearer".to_string(),
                access_token: "access-logout".to_string(),
                refresh_token: "refresh-logout".to_string(),
                session: Some(AccountSession {
                    id: "session-logout".to_string(),
                    ..AccountSession::default()
                }),
                user: Some(AccountUser {
                    id: "acct-logout".to_string(),
                    ..AccountUser::default()
                }),
            })
            .expect("seed account session");

        run_logout_command_with_secrets(&mut store, &secrets, None).expect("logout should succeed");

        assert!(
            secrets
                .get(codewhale_secrets::DAYTONA_TOKEN_SLOT)
                .expect("read daytona")
                .is_none(),
            "daytona slot survived logout"
        );
        let account = secure_account_session_secrets().expect("account store after logout");
        assert!(
            AccountSessionStore::new(account, None, DEFAULT_ACCOUNT_API_BASE)
                .load()
                .expect("load account")
                .is_none(),
            "account session survived logout"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_set_slot_daytona_has_no_user_surface() {
        // The internal cloud-agent credential is managed by Codewhale, not
        // users: no CLI command may write or clear it, and no help text may
        // teach it. Membership (`codewhale login`) is the only door.
        use codewhale_secrets::InMemoryKeyringStore;
        use std::sync::Arc;

        for argv in [
            vec![
                "codewhale",
                "auth",
                "set-slot",
                "daytona",
                "--api-key",
                "dtn_saved",
            ],
            vec!["codewhale", "auth", "clear-slot", "daytona"],
        ] {
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "slot commands must not parse"
            );
        }

        let inner = Arc::new(InMemoryKeyringStore::new());
        let secrets = Secrets::new(inner);
        assert!(
            secrets
                .get(codewhale_secrets::DAYTONA_TOKEN_SLOT)
                .expect("read slot")
                .is_none()
        );
    }

    #[test]
    fn auth_migrate_moves_plaintext_keys_into_keyring_and_strips_file() {
        use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-migrate-test-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.providers.deepseek.api_key = Some("sk-deep".to_string());
        store.config.providers.openrouter.api_key = Some("or-key".to_string());
        store.config.providers.novita.api_key = Some("nv-key".to_string());
        store.save().unwrap();

        let inner = Arc::new(InMemoryKeyringStore::new());
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(
            &mut store,
            AuthCommand::Migrate { dry_run: false },
            &secrets,
        )
        .expect("migrate should succeed");

        assert_eq!(inner.get("deepseek").unwrap(), Some("sk-deep".to_string()));
        assert_eq!(inner.get("openrouter").unwrap(), Some("or-key".to_string()));
        assert_eq!(inner.get("novita").unwrap(), Some("nv-key".to_string()));

        // Config file must no longer contain the api keys.
        assert!(store.config.providers.deepseek.api_key.is_none());
        assert!(store.config.providers.openrouter.api_key.is_none());
        assert!(store.config.providers.novita.api_key.is_none());

        let saved = std::fs::read_to_string(&path).expect("config exists post-migrate");
        assert!(!saved.contains("sk-deep"), "plaintext leaked: {saved}");
        assert!(!saved.contains("or-key"), "plaintext leaked: {saved}");
        assert!(!saved.contains("nv-key"), "plaintext leaked: {saved}");

        let backup_path = path.with_file_name(format!(
            "{}.bak",
            path.file_name().unwrap_or_default().to_string_lossy()
        ));
        let backup = std::fs::read_to_string(&backup_path).expect("credential-free backup");
        assert!(
            !backup.contains("sk-deep"),
            "plaintext leaked in backup: {backup}"
        );
        assert!(
            !backup.contains("or-key"),
            "plaintext leaked in backup: {backup}"
        );
        assert!(
            !backup.contains("nv-key"),
            "plaintext leaked in backup: {backup}"
        );

        let resolved = resolve_runtime_for_dispatch_with_secrets(
            &mut store,
            &CliRuntimeOverrides::default(),
            &secrets,
        );
        assert_eq!(resolved.api_key_source, Some(RuntimeApiKeySource::Keyring));
        let after_dispatch = std::fs::read_to_string(&path).expect("config after dispatch");
        assert!(!after_dispatch.contains("sk-deep"), "{after_dispatch}");
        assert!(
            !after_dispatch
                .lines()
                .any(|line| line.trim_start().starts_with("api_key ="))
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn auth_migrate_dry_run_does_not_modify_anything() {
        use codewhale_secrets::{InMemoryKeyringStore, KeyringStore};
        use std::sync::Arc;

        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
        let path = std::env::temp_dir().join(format!(
            "deepseek-cli-auth-migrate-dry-{}-{nanos}.toml",
            std::process::id()
        ));
        let mut store = ConfigStore::load(Some(path.clone())).expect("store should load");
        store.config.providers.openrouter.api_key = Some("or-stay".to_string());
        store.save().unwrap();

        let inner = Arc::new(InMemoryKeyringStore::new());
        let secrets = Secrets::new(inner.clone());

        run_auth_command_with_secrets(&mut store, AuthCommand::Migrate { dry_run: true }, &secrets)
            .expect("dry-run should succeed");

        assert_eq!(inner.get("openrouter").unwrap(), None);
        assert_eq!(
            store.config.providers.openrouter.api_key.as_deref(),
            Some("or-stay")
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn parses_global_override_flags() {
        let cli = parse_ok(&[
            "deepseek",
            "--provider",
            "openai",
            "--config",
            "/tmp/deepseek.toml",
            "--profile",
            "work",
            "--model",
            "deepseek-v4-pro",
            "--output-mode",
            "json",
            "--verbosity",
            "concise",
            "--log-level",
            "debug",
            "--telemetry",
            "true",
            "--approval-policy",
            "on-request",
            "--sandbox-mode",
            "workspace-write",
            "--base-url",
            "https://openai-compatible.example/v1",
            "--api-key",
            "sk-test",
            "--workspace",
            "/tmp/workspace",
            "--no-mouse-capture",
            "--skip-onboarding",
            "model",
            "resolve",
            "deepseek-v4-pro",
        ]);

        assert_eq!(cli.provider.as_deref(), Some("openai"));
        assert_eq!(cli.config, Some(PathBuf::from("/tmp/deepseek.toml")));
        assert_eq!(cli.profile.as_deref(), Some("work"));
        assert_eq!(cli.model.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(cli.output_mode.as_deref(), Some("json"));
        assert_eq!(cli.verbosity.as_deref(), Some("concise"));
        assert_eq!(cli.log_level.as_deref(), Some("debug"));
        assert_eq!(cli.telemetry, Some(true));
        assert_eq!(cli.approval_policy.as_deref(), Some("on-request"));
        assert_eq!(cli.sandbox_mode.as_deref(), Some("workspace-write"));
        assert_eq!(
            cli.base_url.as_deref(),
            Some("https://openai-compatible.example/v1")
        );
        assert_eq!(cli.api_key.as_deref(), Some("sk-test"));
        assert_eq!(cli.workspace, Some(PathBuf::from("/tmp/workspace")));
        assert!(cli.no_mouse_capture);
        assert!(!cli.mouse_capture);
        assert!(cli.skip_onboarding);
    }

    #[test]
    fn cli_provider_helpers_follow_config_metadata() {
        let registry_kinds: Vec<ProviderKind> = codewhale_config::provider::all_providers()
            .iter()
            .map(|provider| provider.kind())
            .collect();
        // Full registry keeps legacy dialect/plan kinds; ALL is the catalog surface.
        assert_eq!(registry_kinds.len(), 52);
        // The tombstone stays in the registry (old config must still parse
        // and clear) and left the catalog surface when it stopped being
        // selectable.
        assert_eq!(ProviderKind::ALL.len(), 46);
        for kind in ProviderKind::ALL {
            assert!(
                registry_kinds.contains(&kind),
                "catalog kind {kind:?} must remain in the full registry"
            );
        }

        for provider in registry_kinds {
            assert_eq!(provider_env_vars(provider), provider.provider().env_vars());
            // Shared-account families collapse onto one durable slot (see
            // ProviderKind::secret_store_slot); everything else uses its own id.
            assert_eq!(
                provider_slot(provider),
                provider.secret_store_slot(),
                "{provider:?} slot must match ProviderKind::secret_store_slot"
            );
            if provider == ProviderKind::SiliconflowCN {
                assert_eq!(
                    provider_slot(provider),
                    provider_slot(ProviderKind::Siliconflow)
                );
            } else if matches!(
                provider,
                ProviderKind::ModelstudioTokenPlan
                    | ProviderKind::ModelstudioTokenPlanAnthropic
                    | ProviderKind::ModelstudioCodingPlan
                    | ProviderKind::ModelstudioCodingPlanAnthropic
            ) {
                assert_eq!(
                    provider_slot(provider),
                    "modelstudio-token-plan",
                    "{provider:?} must share the Model Studio family slot"
                );
            } else {
                assert_eq!(provider_slot(provider), provider.provider().id());
            }
        }
    }

    #[test]
    fn the_telemetry_flag_documents_itself_in_help() {
        // A consent control nobody can find is a consent control nobody has.
        let help = Cli::command().render_long_help().to_string();
        let telemetry_line = help
            .lines()
            .position(|line| line.contains("--telemetry"))
            .map(|index| help.lines().skip(index).take(3).collect::<String>())
            .expect("--telemetry must appear in --help");
        assert!(
            telemetry_line.contains("telemetry"),
            "expected a help string beside --telemetry, got: {telemetry_line}"
        );
        assert!(
            telemetry_line.contains("default on"),
            "the help string must disclose the default: {telemetry_line}"
        );
        assert!(
            telemetry_line.contains("CODEWHALE_TELEMETRY=0 always")
                && telemetry_line.contains("wins"),
            "the help string must document the always-winning opt-out: {telemetry_line}"
        );
        let help = help_for(&["codewhale", "config", "telemetry", "--help"]);
        assert!(help.contains("PostHog"));
        assert!(help.contains("--accept-notice"));
    }

    #[test]
    fn cli_telemetry_acceptance_is_versioned_and_reuses_settings_persistence() {
        let _lock = env_lock();
        let _telemetry_env = [
            ScopedEnvVar::remove("CODEWHALE_TELEMETRY"),
            ScopedEnvVar::remove("DEEPSEEK_TELEMETRY"),
            ScopedEnvVar::remove(codewhale_config::TELEMETRY_FLOOR_ENV),
        ];
        let temp = tempfile::tempdir().expect("tempdir");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", temp.path().to_str().unwrap());
        let path = temp.path().join("config.toml");
        write_config_fixture(&path, "telemetry = false\nverbosity = \"concise\"\n");
        let mut state = SetupState::default();
        state.record_telemetry_notice("3", false);
        state.save().expect("seed decline");
        let mut store = ConfigStore::load(Some(path.clone())).expect("load config");

        // An explicit enable command clears a historical decline through Settings.
        run_config_command(
            &mut store,
            ConfigCommand::Set {
                key: "telemetry".into(),
                value: "true".into(),
            },
            false,
            &[],
        )
        .expect("save configuration preference");
        assert!(!SetupState::load().unwrap().unwrap().telemetry_opted_out());
        assert_eq!(
            telemetry_preference_status(Some(true)),
            "On (saved preference)"
        );
        let before = std::fs::read(SetupState::path().unwrap()).unwrap();
        run_config_command(
            &mut store,
            ConfigCommand::Telemetry {
                accept_notice: None,
            },
            false,
            &[],
        )
        .expect("read notice");
        assert!(
            run_config_command(
                &mut store,
                ConfigCommand::Telemetry {
                    accept_notice: Some(3)
                },
                false,
                &[]
            )
            .is_err()
        );
        assert_eq!(std::fs::read(SetupState::path().unwrap()).unwrap(), before);

        run_config_command(
            &mut store,
            ConfigCommand::Telemetry {
                accept_notice: Some(telemetry::NOTICE_VERSION),
            },
            false,
            &[],
        )
        .expect("accept current processor notice");
        assert_eq!(
            telemetry_preference_status(Some(true)),
            "On (saved preference)"
        );
        let saved = ConfigStore::load(Some(path)).unwrap();
        assert_eq!(saved.config.telemetry, Some(true));
        assert_eq!(saved.config.verbosity.as_deref(), Some("concise"));
        assert!(
            !temp.path().join("telemetry").exists(),
            "acceptance never arms this process"
        );
    }

    #[test]
    fn cli_telemetry_acceptance_refuses_overlays_and_corrupt_privacy_records() {
        let _lock = env_lock();
        let temp = tempfile::tempdir().expect("tempdir");
        let _home = ScopedEnvVar::set("CODEWHALE_HOME", temp.path().to_str().unwrap());
        let path = temp.path().join("config.toml");
        write_config_fixture(&path, "telemetry = false\n");
        std::fs::write(SetupState::path().unwrap(), "not-json").unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut store = ConfigStore::load(Some(path.clone())).unwrap();
        for overrides in [vec![], vec!["telemetry=true".to_string()]] {
            assert!(
                run_config_command(
                    &mut store,
                    ConfigCommand::Telemetry {
                        accept_notice: Some(telemetry::NOTICE_VERSION),
                    },
                    false,
                    &overrides
                )
                .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert_eq!(
                std::fs::read_to_string(SetupState::path().unwrap()).unwrap(),
                "not-json"
            );
        }
    }

    #[test]
    fn root_help_describes_product_actions_not_internal_tui_layers() {
        let help = Cli::command().render_long_help().to_string();
        assert!(
            !help.contains("TUI"),
            "root help must describe what Codewhale does, not its internal UI/runtime layers:\n{help}"
        );
    }

    #[test]
    fn only_one_function_may_locate_and_spawn_the_tui() {
        // Single-binary invariant: no sibling TUI discovery exists. The
        // two-process glue has been deleted; the only TUI entry is codewhale_tui::run.
        let source = include_str!("lib.rs");
        let a = format!("{}{}", "locate_sibling", "_tui_binary");
        let b = format!("{}{}", "tui_spawn", "_error");
        let c = format!("{}{}", "build_tui", "_command");
        let d = format!("{}{}", "Command::new", "(&tui)");
        assert!(
            !source.contains(&a),
            "single binary must not contain sibling TUI discovery"
        );
        assert!(
            !source.contains(&b),
            "single binary must not contain tui spawn error"
        );
        assert!(
            !source.contains(&c),
            "single binary must not contain build_tui dispatch"
        );
        assert!(
            !source.contains(&d),
            "single binary must not contain Command new tui"
        );
    }

    #[test]
    fn parses_no_project_config_before_subcommand() {
        let cli = parse_ok(&["codewhale", "--no-project-config", "exec", "list the files"]);
        assert!(cli.no_project_config);
        match cli.command {
            Some(Commands::Exec(args)) => {
                assert_eq!(args.args, vec!["list the files".to_string()]);
            }
            other => panic!("expected exec subcommand, got {other:?}"),
        }
    }

    #[test]
    fn no_project_config_after_passthrough_subcommand_is_not_the_dispatcher_flag() {
        // `exec` captures trailing args (`trailing_var_arg`), so a misplaced
        // `--no-project-config` is NOT honored as the dispatcher flag — it must
        // appear before the subcommand, exactly like `--skip-onboarding`.
        let cli = parse_ok(&["codewhale", "exec", "--no-project-config", "hi"]);
        assert!(!cli.no_project_config);
        match cli.command {
            Some(Commands::Exec(args)) => {
                assert!(args.args.iter().any(|a| a == "--no-project-config"));
            }
            other => panic!("expected exec subcommand, got {other:?}"),
        }
    }

    #[test]
    fn parses_top_level_prompt_flag_for_interactive_startup_prompt() {
        let cli = parse_ok(&["deepseek", "-p", "Reply with exactly OK."]);

        assert_eq!(cli.prompt_flag.as_deref(), Some("Reply with exactly OK."));
        assert!(cli.prompt.is_empty());
        assert_eq!(
            root_tui_passthrough(&cli).unwrap(),
            vec!["--prompt".to_string(), "Reply with exactly OK.".to_string()]
        );
    }

    #[test]
    fn root_launch_facts_are_typed_and_prompt_whitespace_is_preserved() {
        let cli = parse_ok(&[
            "codewhale",
            "--workspace",
            "workspace with spaces",
            "--fresh",
            "--mouse-capture",
            "--no-project-config",
            "--enable",
            "extension_host",
            "--disable",
            "web_search",
            "--prompt",
            "Keep  two spaces\nand a tab\there",
            "then",
            "explain them",
        ]);
        let options = &cli.runtime_options;
        assert_eq!(
            options.workspace,
            Some(PathBuf::from("workspace with spaces"))
        );
        assert!(options.fresh && options.mouse_capture && options.no_project_config);
        assert_eq!(options.enable, ["extension_host"]);
        assert_eq!(options.disable, ["web_search"]);
        assert_eq!(
            root_tui_passthrough(&cli).unwrap(),
            [
                "--prompt",
                "Keep  two spaces\nand a tab\there then explain them",
            ]
        );
    }

    #[test]
    fn root_prompt_tail_does_not_reinterpret_literal_launch_flags() {
        let cli = parse_ok(&[
            "codewhale",
            "Explain",
            "--fresh",
            "--mouse-capture",
            "as literal flags",
        ]);
        assert_eq!(
            cli.runtime_options,
            codewhale_tui::RuntimeOptions::default()
        );
        assert_eq!(
            root_tui_passthrough(&cli).unwrap(),
            [
                "--prompt",
                "Explain --fresh --mouse-capture as literal flags",
            ]
        );
    }

    #[test]
    fn canonical_cli_accepts_legacy_tui_workspace_and_completion_aliases() {
        let cli = parse_ok(&[
            "codewhale-tui",
            "-w",
            "legacy workspace",
            "--verbose",
            "--max-subagents",
            "4",
            "doctor",
        ]);
        assert_eq!(
            cli.workspace.as_deref(),
            Some(std::path::Path::new("legacy workspace"))
        );
        assert!(cli.verbose);
        assert_eq!(cli.max_subagents, Some(4));
        assert!(matches!(cli.command, Some(Commands::Doctor(_))));
        let cli = parse_ok(&["codewhale-tui", "completions", "bash"]);
        assert!(matches!(
            cli.command,
            Some(Commands::Completion { shell: Shell::Bash })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn typed_runtime_paths_keep_non_utf8_workspace_bytes() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(
            b"/tmp/workspace-\xff".to_vec(),
        ));
        let cli = Cli::try_parse_from([
            std::ffi::OsString::from("codewhale"),
            std::ffi::OsString::from("--workspace"),
            path.clone().into_os_string(),
        ])
        .expect("native workspace path parses");
        assert_eq!(cli.runtime_options.workspace, Some(path));
        assert!(root_tui_passthrough(&cli).unwrap().is_empty());
    }

    #[test]
    fn parses_top_level_continue_for_interactive_resume() {
        let cli = parse_ok(&["codewhale", "--continue"]);

        assert!(cli.continue_session);
        assert!(cli.prompt_flag.is_none());
        assert!(cli.prompt.is_empty());
        assert_eq!(root_tui_passthrough(&cli).unwrap(), vec!["--continue"]);
    }

    #[test]
    fn parses_top_level_resume_flags_for_interactive_resume() {
        // The operations runbook advertises `codewhale --resume <id>`. Before
        // the root flag existed, the trailing prompt positional swallowed it
        // and forwarded `--prompt "--resume <id>"` to the TUI (exit 2).
        for argv in [
            &["codewhale", "--resume", "800596e6"][..],
            &["codewhale", "--resume=800596e6"][..],
            &["codewhale", "-r", "800596e6"][..],
            &["codewhale", "--session-id", "800596e6"][..],
            &["codewhale", "--session-id=800596e6"][..],
        ] {
            let cli = parse_ok(argv);
            assert!(
                cli.prompt.is_empty(),
                "{argv:?} must not be swallowed as a prompt: {:?}",
                cli.prompt
            );
            assert!(cli.prompt_flag.is_none(), "{argv:?}");
            assert!(cli.command.is_none(), "{argv:?}");
            assert_eq!(
                root_tui_passthrough(&cli).unwrap(),
                vec!["--resume".to_string(), "800596e6".to_string()],
                "{argv:?}"
            );
        }
    }

    #[test]
    fn empty_resume_identifier_is_rejected_rather_than_starting_fresh() {
        // `codewhale --resume "$SESSION_ID"` with the variable unset used to
        // trim to empty, filter to None, and start a brand-new session while
        // looking like it resumed one. Losing the session the user asked for
        // must be loud.
        for argv in [
            &["codewhale", "--resume", ""][..],
            &["codewhale", "--session-id", "   "][..],
        ] {
            let cli = parse_ok(argv);
            let err = root_tui_passthrough(&cli)
                .expect_err("an empty resume id must not silently start a fresh session");
            assert!(
                err.to_string().contains("needs a session id"),
                "{argv:?}: {err}"
            );
        }
    }

    #[test]
    fn top_level_resume_rejects_startup_prompt_and_conflicting_flags() {
        let cli = parse_ok(&["codewhale", "--resume", "800596e6", "-p", "follow up"]);
        let err = root_tui_passthrough(&cli).expect_err("prompted resume should be rejected");
        assert!(
            err.to_string()
                .contains("codewhale exec --resume 800596e6 <PROMPT>"),
            "{err}"
        );

        assert!(Cli::try_parse_from(["codewhale", "--resume", "800596e6", "--continue"]).is_err());
        assert!(Cli::try_parse_from(["codewhale", "--resume", "a", "--session-id", "b"]).is_err());
        assert!(Cli::try_parse_from(["codewhale", "--session-id", "b", "-c"]).is_err());
    }

    #[test]
    fn parses_rc_as_the_account_owned_interactive_handoff() {
        let cli = parse_ok(&["codewhale", "rc"]);

        let Some(Commands::Rc(args)) = cli.command else {
            panic!("rc should parse as the remote-control TUI handoff");
        };
        assert!(args.args.is_empty());
    }

    #[test]
    fn top_level_continue_rejects_startup_prompt() {
        let cli = parse_ok(&["codewhale", "--continue", "-p", "follow up"]);

        let err = root_tui_passthrough(&cli).expect_err("prompted continue should be rejected");
        assert!(
            err.to_string()
                .contains("codewhale exec --continue <PROMPT>")
        );
    }

    #[test]
    fn parses_split_top_level_prompt_words_for_windows_cmd_shims() {
        let cli = parse_ok(&["deepseek", "hello", "world"]);

        assert_eq!(cli.prompt, vec!["hello", "world"]);
        assert!(cli.command.is_none());
        assert_eq!(
            root_tui_passthrough(&cli).unwrap(),
            vec!["--prompt".to_string(), "hello world".to_string()]
        );
    }

    #[test]
    fn prompt_flag_keeps_split_tail_words_for_windows_cmd_shims() {
        let cli = parse_ok(&["deepseek", "-p", "hello", "world"]);

        assert_eq!(cli.prompt_flag.as_deref(), Some("hello"));
        assert_eq!(cli.prompt, vec!["world"]);
        assert_eq!(
            root_tui_passthrough(&cli).unwrap(),
            vec!["--prompt".to_string(), "hello world".to_string()]
        );
    }

    #[test]
    fn known_subcommands_still_parse_before_prompt_tail() {
        let cli = parse_ok(&["deepseek", "doctor"]);

        assert!(cli.prompt.is_empty());
        assert!(matches!(cli.command, Some(Commands::Doctor(_))));
    }

    #[test]
    fn root_help_surface_contains_expected_subcommands_and_globals() {
        let rendered = help_for(&["deepseek", "--help"]);

        for token in [
            "run",
            "doctor",
            "models",
            "sessions",
            "resume",
            "setup",
            "login",
            "logout",
            "auth",
            "mcp-server",
            "config",
            "model",
            "providers",
            "thread",
            "sandbox",
            "app-server",
            "completion",
            "metrics",
            "--provider",
            "--model",
            "--config",
            "--profile",
            "--log-level",
            "--telemetry",
            "--base-url",
            "--api-key",
            "--approval-policy",
            "--sandbox-mode",
            "--mouse-capture",
            "--no-mouse-capture",
            "--skip-onboarding",
            "--fresh",
            "--continue",
            "--prompt",
        ] {
            assert!(
                rendered.contains(token),
                "expected help to contain token: {token}"
            );
        }
    }

    /// Help must exit in the wrapper's parser, before config, credentials or
    /// provider setup can run. Its schema also preserves wrapper-only rules.
    #[test]
    fn passthrough_subcommand_help_exits_in_the_wrapper() {
        for subcommand in [
            "doctor",
            "setup",
            "init",
            "models",
            "exec",
            "review",
            "sessions",
            "resume",
            "fleet",
            "apply",
            "eval",
            "mcp",
            "features",
            "integrations",
            "receipts",
            "rc",
            "fork",
            "speech",
        ] {
            for flag in ["--help", "-h"] {
                let error = Cli::try_parse_from(["codewhale", subcommand, flag])
                    .expect_err("help must exit before dispatch");
                assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
                assert!(
                    error
                        .to_string()
                        .contains(&format!("Usage: codewhale {subcommand}")),
                    "{subcommand} must show its own help: {error}"
                );
            }
        }
        let exec = help_for(&["codewhale", "exec", "--help"]);
        assert!(exec.contains("work before or after exec"), "{exec}");
        assert!(exec.contains("codewhale --model MODEL exec"), "{exec}");
        assert!(exec.contains("codewhale exec --model MODEL"), "{exec}");
        let rc = help_for(&["codewhale", "rc", "--help"]);
        assert!(rc.contains("hand it to the Codewhale web app"), "{rc}");
    }

    /// `codewhale mcp --help` printed `Usage: codewhale mcp [OPTIONS]
    /// [ARGS]...` and nothing else, so `add`, `list`, `tools`, `connect` and
    /// `remove` could only be found by already knowing them.
    #[test]
    fn mcp_help_lists_its_subcommands() {
        for flag in ["--help", "-h"] {
            let help = help_for(&["codewhale", "mcp", flag]);
            assert!(
                help.contains("Usage: codewhale mcp [OPTIONS] <COMMAND>"),
                "{help}"
            );
            for subcommand in [
                "list", "init", "connect", "tools", "add", "login", "logout", "remove", "enable",
                "disable", "validate", "add-self",
            ] {
                assert!(
                    help.lines().any(|line| line
                        .strip_prefix("  ")
                        .and_then(|line| line.strip_prefix(subcommand))
                        .is_some_and(|rest| rest.starts_with("  "))),
                    "mcp help must list `{subcommand}` with its description:\n{help}"
                );
            }
            assert!(help.contains("codewhale mcp <COMMAND> --help"), "{help}");
        }
    }

    #[test]
    fn root_help_describes_every_global_option() {
        let help = help_for(&["codewhale", "--help"]);
        for needle in [
            "Path to the config file",
            "Config profile to apply",
            "Model to use for this run",
            "Log level for this run",
            "Tool approval policy",
            "danger-full-access disables",
            "Provider API key for this run",
            "Provider base URL for this run",
            "Enable terminal mouse capture",
            "Initial prompt for the interactive session",
        ] {
            assert!(
                help.contains(needle),
                "root help missing `{needle}`:\n{help}"
            );
        }
        let app_server = help_for(&["codewhale", "app-server", "--help"]);
        assert!(
            app_server.contains("CODEWHALE_RUNTIME_TOKEN"),
            "{app_server}"
        );
        let auth_set = help_for(&["codewhale", "auth", "set", "--help"]);
        assert!(
            auth_set.contains("Save an API key to the credential store"),
            "{auth_set}"
        );
    }

    #[test]
    fn argv_secrets_print_a_process_list_hint() {
        let warn = |argv: &[&str]| {
            let cli = parse_ok(argv);
            argv_secret_warning(&cli, cli.command.as_ref())
        };
        assert!(
            warn(&["codewhale", "--api-key", "sk-x", "doctor"])
                .expect("global --api-key warns")
                .contains("process list")
        );
        assert!(
            warn(&["codewhale", "app-server", "--http", "--auth-token", "t"])
                .expect("app-server --auth-token warns")
                .contains("CODEWHALE_RUNTIME_TOKEN")
        );
        assert!(
            warn(&["codewhale", "serve", "--http", "--auth-token=t"])
                .expect("serve --auth-token warns")
                .contains("CODEWHALE_RUNTIME_TOKEN")
        );
        assert!(
            warn(&[
                "codewhale",
                "auth",
                "set",
                "--provider",
                "deepseek",
                "--api-key",
                "k"
            ])
            .expect("auth set --api-key warns")
            .contains("--api-key-stdin")
        );
        assert_eq!(warn(&["codewhale", "doctor"]), None);
        assert_eq!(
            warn(&[
                "codewhale",
                "auth",
                "set",
                "--provider",
                "deepseek",
                "--api-key-stdin"
            ]),
            None
        );
        assert_eq!(warn(&["codewhale", "app-server", "--http"]), None);
        // login/account reject the global flag with their own guidance.
        assert_eq!(warn(&["codewhale", "--api-key", "sk-x", "login"]), None);
        assert_eq!(
            warn(&[
                "codewhale",
                "--api-key",
                "sk-x",
                "auth",
                "print-api-key",
                "--provider",
                "deepseek",
            ]),
            None
        );
    }

    /// #6516: `--output-mode` never had a reader. It stays accepted so old
    /// scripts keep running, but it is no longer advertised.
    #[test]
    fn retired_output_mode_flag_is_accepted_but_hidden() {
        let cli = parse_ok(&["deepseek", "--output-mode", "json", "doctor"]);
        assert_eq!(cli.output_mode.as_deref(), Some("json"));
        let warning = retired_output_mode_warning(&cli).expect("using the flag warns");
        assert!(warning.contains("--output-mode has no effect"), "{warning}");
        assert_eq!(
            retired_output_mode_warning(&parse_ok(&["deepseek", "doctor"])),
            None
        );
        let rendered = help_for(&["deepseek", "--help"]);
        assert!(!rendered.contains("--output-mode"), "{rendered}");
    }

    #[test]
    fn subcommand_help_surfaces_are_stable() {
        let cases = [
            ("config", vec!["get", "set", "unset", "list", "path"]),
            ("model", vec!["list", "resolve"]),
            (
                "thread",
                vec![
                    "list",
                    "read",
                    "resume",
                    "fork",
                    "archive",
                    "unarchive",
                    "set-name",
                    "clear-name",
                ],
            ),
            ("sandbox", vec!["check"]),
            (
                "exec",
                vec![
                    "--auto",
                    "--json",
                    "--resume",
                    "--session-id",
                    "--continue",
                    "--output-format",
                    "stream-json",
                ],
            ),
            (
                "app-server",
                vec!["--host", "--port", "--config", "--stdio"],
            ),
            (
                "completion",
                vec![
                    "<SHELL>",
                    "bash",
                    "Every script completes both `codewhale` and the `codew` shorthand.",
                    "source <(codewhale completion bash)",
                    "~/.local/share/bash-completion/completions/codewhale",
                    "fpath=(~/.zfunc $fpath)",
                    "codewhale completion fish > ~/.config/fish/completions/codewhale.fish",
                    "codewhale completion powershell | Out-String | Invoke-Expression",
                    "codewhale completion elvish >> ~/.config/elvish/rc.elv",
                ],
            ),
            ("metrics", vec!["--json", "--since"]),
        ];

        for (subcommand, expected_tokens) in cases {
            // `help <sub>`: passthrough subcommands such as `exec` forward
            // `--help` to the delegated binary instead of rendering here.
            let argv = ["deepseek", "help", subcommand];
            let rendered = help_for(&argv);
            for token in expected_tokens {
                assert!(
                    rendered.contains(token),
                    "expected help for `{subcommand}` to include `{token}`"
                );
            }
        }
    }

    #[test]
    fn cli_telemetry_start_fails_closed_on_a_corrupt_setup_state() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let setup_path = dir.path().join("setup_state.json");
        std::fs::write(&setup_path, b"{not-json").expect("write corrupt setup state");
        let setup = telemetry::load_setup_state_for_decision_at(&setup_path);
        assert!(setup.is_none(), "corrupt privacy state must not default on");

        let resolved =
            ConfigToml::default().resolve_runtime_options(&CliRuntimeOverrides::default());
        assert!(
            resolve_cli_telemetry_consent(&resolved, None, Surface::Cli, setup).is_none(),
            "CLI startup must not obtain permission from an unreadable privacy record"
        );
    }
}

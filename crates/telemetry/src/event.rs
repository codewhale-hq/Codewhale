//! The wire schema. This module is the whole of what may leave the machine.
//!
//! Every field below is an integer, a boolean, or a **closed enum string**,
//! except exactly four bounded strings: `app_version`, `git_sha`,
//! `panic_site`, and `model_usage.model`, which is closed against the bundled
//! model catalog. Each of those four has a written rule. There is no free-form string type here and no open-keyed map, which is
//! what makes "aggregates and events, never content" a property the compiler
//! and the test suite can enforce rather than a promise.
//!
//! Bounded is not the same as *built bounded*. These types are also a
//! **deserialization target**: `flush` reads `buffer.jsonl` back off disk and
//! hands the lines to `serde`, which will fill `site`, `previous_version`, and
//! `providers` with any string the file contains. Anything running as the user
//! can append to that file, `$CODEWHALE_HOME` is a predictable path, and this
//! product executes model-authored shell commands. [`Event::is_bounded`] is
//! therefore checked on the drain path, and it is the reason the guarantee
//! above survives contact with the filesystem.
//!
//! Three standing rules for anyone extending this file:
//!
//! 1. **Never `#[derive(Serialize)]` over an existing state type.**
//!    `codewhale_state::Thread` carries `git_sha`, `git_branch`,
//!    `git_origin_url`, `cwd`, and `path`. A payload builder that accepts one
//!    and derives breaches the red lines in a single line. Every struct here is
//!    built from scratch with explicit fields.
//! 2. **Bump [`SCHEMA_VERSION`] on any field add, remove, or retype**, and
//!    never reuse a number. The golden snapshot test fails until you do.
//! 3. **A new string field needs a clause in [`Event::is_bounded`]**, not just
//!    a doc comment naming its rule. A rule only the constructor honours is a
//!    rule the drain path does not have.

use serde::{Deserialize, Serialize};

/// Wire schema version. Bumped on any field add, remove, or retype; never
/// reused. `crates/telemetry/tests/golden/v1.json` pins what v1 was.
pub const SCHEMA_VERSION: u32 = 4;

/// Default-on policy disclosure version. This is not human consent.
pub const NOTICE_VERSION: u32 = 6;

/// Which product surface produced a batch.
///
/// Deliberately **not** derived from the executable: `codewhale-tui` serves at
/// least five surfaces, and app-server runs in-process inside `codewhale`, so
/// `current_exe()` would report every app-server session as CLI. Each
/// subcommand dispatch names its own surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Surface {
    /// The interactive terminal UI.
    Tui,
    /// `codewhale exec` — one non-interactive run.
    Exec,
    /// Terminal `config` / `auth` / `update` subcommands.
    Cli,
    /// The app-server protocol surface (in-process inside `codewhale`).
    AppServer,
    /// The MCP server surface.
    McpServer,
    /// `codewhale serve`.
    Serve,
    /// Public website product interactions.
    Website,
    /// Browser application interactions.
    WebApp,
    /// Native desktop application interactions.
    Desktop,
    /// Control-plane application interactions.
    ControlPlane,
    /// A VS Code-family extension client of this schema.
    ///
    /// Deliberately **generic**, the way `web-app` and `desktop` are: the
    /// client is not one publisher's extension, and a surface naming a vendor
    /// would narrow the whitelist for every later extension built on the same
    /// client. The editor's own name and version are **not** collected — this
    /// value names the kind of client that emitted the batch, never the host
    /// application it happened to run inside.
    VscodeExtension,
}

impl Surface {
    /// Every surface, for exhaustive iteration in tests and schema checks.
    pub const ALL: &'static [Self] = &[
        Self::Tui,
        Self::Exec,
        Self::Cli,
        Self::AppServer,
        Self::McpServer,
        Self::Serve,
        Self::Website,
        Self::WebApp,
        Self::Desktop,
        Self::ControlPlane,
        Self::VscodeExtension,
    ];

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Exec => "exec",
            Self::Cli => "cli",
            Self::AppServer => "app-server",
            Self::McpServer => "mcp-server",
            Self::Serve => "serve",
            Self::Website => "website",
            Self::WebApp => "web-app",
            Self::Desktop => "desktop",
            Self::ControlPlane => "control-plane",
            Self::VscodeExtension => "vscode-extension",
        }
    }

    /// The inverse of [`Self::as_str`], or `None` for anything else.
    ///
    /// The round trip is pinned by a test, so a value that can be spelled can
    /// also be parsed and vice versa. This is the only place a surface name is
    /// turned back into a variant, which is what lets an *embedder* declare one
    /// without a second copy of the closed list.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|surface| surface.as_str() == value)
    }
}

/// Operating system family. A closed whitelist, so an unrecognised
/// `std::env::consts::OS` reports `other` rather than shipping a novel string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    /// Linux.
    Linux,
    /// macOS.
    Macos,
    /// Windows.
    Windows,
    /// FreeBSD.
    Freebsd,
    /// Android.
    Android,
    /// Anything else.
    Other,
}

impl Os {
    /// Every value, for exhaustive iteration.
    pub const ALL: &'static [Self] = &[
        Self::Linux,
        Self::Macos,
        Self::Windows,
        Self::Freebsd,
        Self::Android,
        Self::Other,
    ];

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::Macos => "macos",
            Self::Windows => "windows",
            Self::Freebsd => "freebsd",
            Self::Android => "android",
            Self::Other => "other",
        }
    }
}

/// CPU family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    /// 64-bit x86.
    X86_64,
    /// 64-bit ARM.
    Aarch64,
    /// Anything else.
    Other,
}

impl Arch {
    /// Every value, for exhaustive iteration.
    pub const ALL: &'static [Self] = &[Self::X86_64, Self::Aarch64, Self::Other];

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Other => "other",
        }
    }
}

/// C runtime the binary was **compiled** against.
///
/// Compile-time (`cfg!(target_env)`), never runtime-detected: the only way to
/// read this at runtime is `/etc/os-release` or shelling to `ldd`, both of which
/// surface corporate golden-image vendor strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Libc {
    /// glibc.
    Gnu,
    /// musl.
    Musl,
    /// Not a libc target (macOS, Windows, and anything else).
    None,
}

impl Libc {
    /// Every value, for exhaustive iteration.
    pub const ALL: &'static [Self] = &[Self::Gnu, Self::Musl, Self::None];

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gnu => "gnu",
            Self::Musl => "musl",
            Self::None => "none",
        }
    }
}

/// Whether this binary is newly installed, upgraded, or downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallKind {
    /// No prior version record on this machine.
    Install,
    /// The recorded version is older than this one.
    Upgrade,
    /// The recorded version is newer than this one.
    Downgrade,
}

impl InstallKind {
    /// Every value, for exhaustive iteration.
    pub const ALL: &'static [Self] = &[Self::Install, Self::Upgrade, Self::Downgrade];

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Upgrade => "upgrade",
            Self::Downgrade => "downgrade",
        }
    }
}

/// How a session was started.
///
/// Mirrors `codewhale_state::SessionSource` by value, deliberately re-declared
/// here rather than imported: this crate must not depend on the thread store,
/// which is the one crate whose types carry paths and git identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    /// A user opened a session directly.
    Interactive,
    /// Resumed from a persisted session.
    Resume,
    /// Forked from an existing conversation.
    Fork,
    /// Started programmatically.
    Api,
    /// Not stated.
    Unknown,
    /// Started under CI (`CI` or `GITHUB_ACTIONS` set), including the GitHub
    /// Action, so automation is never counted as a person.
    Ci,
}

impl SessionSource {
    /// Every value, for exhaustive iteration.
    pub const ALL: &'static [Self] = &[
        Self::Interactive,
        Self::Resume,
        Self::Fork,
        Self::Api,
        Self::Unknown,
        Self::Ci,
    ];

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Resume => "resume",
            Self::Fork => "fork",
            Self::Api => "api",
            Self::Unknown => "unknown",
            Self::Ci => "ci",
        }
    }
}

/// How long a session lasted, bucketed. Half-open intervals, in seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurationBucket {
    /// `d < 60`
    #[serde(rename = "lt_1m")]
    Lt1m,
    /// `60 <= d < 600`
    #[serde(rename = "1m_10m")]
    OneToTen,
    /// `600 <= d < 3600`
    #[serde(rename = "10m_60m")]
    TenToSixty,
    /// `d >= 3600`
    #[serde(rename = "gt_60m")]
    Gt60m,
}

impl DurationBucket {
    /// Every value, for exhaustive iteration.
    pub const ALL: &'static [Self] = &[Self::Lt1m, Self::OneToTen, Self::TenToSixty, Self::Gt60m];

    /// Bucket a session duration in whole seconds.
    #[must_use]
    pub fn from_secs(secs: u64) -> Self {
        match secs {
            0..60 => Self::Lt1m,
            60..600 => Self::OneToTen,
            600..3600 => Self::TenToSixty,
            _ => Self::Gt60m,
        }
    }

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lt1m => "lt_1m",
            Self::OneToTen => "1m_10m",
            Self::TenToSixty => "10m_60m",
            Self::Gt60m => "gt_60m",
        }
    }
}

/// How the process ended.
///
/// Derived from an explicit atomic set by the panic hook, the signal task, and
/// the clean path — **never from an exit code**. `RunTerminationReason::Canceled`
/// maps to exit 130, the same value the SIGINT path uses, so a code-based
/// derivation would report every Esc-cancelled turn as a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitClass {
    /// Ordinary successful exit.
    Clean,
    /// Terminated by a signal.
    Signal,
    /// Terminated by a panic.
    Panic,
    /// Exited non-successfully without a signal or a panic.
    Error,
}

impl ExitClass {
    /// Every value, for exhaustive iteration.
    pub const ALL: &'static [Self] = &[Self::Clean, Self::Signal, Self::Panic, Self::Error];

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Signal => "signal",
            Self::Panic => "panic",
            Self::Error => "error",
        }
    }

    /// Stable numeric encoding for the process-wide `AtomicU8`.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Clean => 0,
            Self::Signal => 1,
            Self::Panic => 2,
            Self::Error => 3,
        }
    }

    /// Inverse of [`Self::as_u8`]; anything unrecognised reads as `Clean`.
    #[must_use]
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Signal,
            2 => Self::Panic,
            3 => Self::Error,
            _ => Self::Clean,
        }
    }
}

/// Cold-start time, bucketed, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColdStartBucket {
    /// `ms < 250`
    #[serde(rename = "lt_250")]
    Lt250,
    /// `250 <= ms < 1000`
    #[serde(rename = "250_1000")]
    Mid,
    /// `1000 <= ms < 3000`
    #[serde(rename = "1000_3000")]
    Slow,
    /// `ms >= 3000`
    #[serde(rename = "gte_3000")]
    Gte3000,
}

impl ColdStartBucket {
    /// Every value, for exhaustive iteration.
    pub const ALL: &'static [Self] = &[Self::Lt250, Self::Mid, Self::Slow, Self::Gte3000];

    /// Bucket a cold-start measurement in milliseconds.
    #[must_use]
    pub fn from_millis(ms: u64) -> Self {
        match ms {
            0..250 => Self::Lt250,
            250..1000 => Self::Mid,
            1000..3000 => Self::Slow,
            _ => Self::Gte3000,
        }
    }

    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lt250 => "lt_250",
            Self::Mid => "250_1000",
            Self::Slow => "1000_3000",
            Self::Gte3000 => "gte_3000",
        }
    }
}

/// Feature-use counts for one session.
///
/// A struct of named `u32`s rather than a map, deliberately: a
/// `BTreeMap<&'static str, u32>` is an open key set the compiler cannot police,
/// so the doc-match test would be asserting that the doc matches a fixture
/// rather than the binary. Adding a counter now requires editing this file,
/// which is where that test lives. Every field serializes, including zeros.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    /// Model turns completed.
    pub turns: u32,
    /// Tool calls executed, across every surface.
    pub tool_calls: u32,
    /// Fleet dispatches started.
    pub fleet_dispatch: u32,
    /// `workflow_run` invocations, keyed off the parsed action discriminant.
    pub workflow_run: u32,
    /// Sub-agents spawned.
    pub subagent_spawn: u32,
    /// MCP servers that reached `connected`.
    pub mcp_server_connected: u32,
    /// Native-memory searches.
    pub memory_search: u32,
    /// Approval modals shown.
    pub approval_modal_shown: u32,
    /// Approvals granted by an auto-allow rule.
    pub approval_auto_allowed: u32,
    /// Command-palette opens.
    pub command_palette_open: u32,
}

impl Counters {
    /// Field names in declaration order, for the doc-match test.
    pub const FIELDS: &'static [&'static str] = &[
        "turns",
        "tool_calls",
        "fleet_dispatch",
        "workflow_run",
        "subagent_spawn",
        "mcp_server_connected",
        "memory_search",
        "approval_modal_shown",
        "approval_auto_allowed",
        "command_palette_open",
    ];
}

/// Closed aggregate product counters shared with browser and app clients.
/// Runtime sessions keep their existing counters and add no second collector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProductCounters {
    /// Aggregate page view count.
    pub page_view: u32,
    /// Aggregate docs view count.
    pub docs_view: u32,
    /// Aggregate install copy count.
    pub install_copy: u32,
    /// Aggregate download count.
    pub download: u32,
    /// Aggregate signup count.
    pub signup: u32,
    /// Aggregate login count.
    pub login: u32,
    /// Aggregate session create count.
    pub session_create: u32,
    /// Aggregate session resume count.
    pub session_resume: u32,
    /// Aggregate turn submit count.
    pub turn_submit: u32,
    /// Aggregate turn complete count.
    pub turn_complete: u32,
    /// Aggregate settings open count.
    pub settings_open: u32,
    /// Aggregate integration connect count.
    pub integration_connect: u32,
    /// Aggregate error shown count.
    pub error_shown: u32,
}

impl ProductCounters {
    /// Closed wire field names.
    pub const FIELDS: &'static [&'static str] = &[
        "page_view",
        "docs_view",
        "install_copy",
        "download",
        "signup",
        "login",
        "session_create",
        "session_resume",
        "turn_submit",
        "turn_complete",
        "settings_open",
        "integration_connect",
        "error_shown",
    ];
}

/// Error counts for one session.
///
/// Every value is a count of a **variant discriminant**, never of an
/// `err.to_string()`. `ToolError::PathEscape`'s `Display` *is* an absolute path;
/// the secret store's *is* the store's absolute path; every `LlmError` variant
/// carries the raw provider HTTP body verbatim, and a 400 from a content filter
/// routinely echoes the prompt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Errors {
    /// Credential preflight rejected the route.
    pub auth_preflight_failed: u32,
    /// Provider responded 4xx.
    pub provider_http_4xx: u32,
    /// Provider responded 5xx.
    pub provider_http_5xx: u32,
    /// A tool call was denied by policy.
    pub tool_denied_by_policy: u32,
    /// A tool call timed out.
    pub tool_timeout: u32,
    /// A request failed below HTTP — DNS, connect, TLS, or timeout.
    pub network_error: u32,
}

impl Errors {
    /// Field names in declaration order, for the doc-match test.
    pub const FIELDS: &'static [&'static str] = &[
        "auth_preflight_failed",
        "provider_http_4xx",
        "provider_http_5xx",
        "tool_denied_by_policy",
        "tool_timeout",
        "network_error",
    ];
}

/// Per-session histogram of turn wall-clock time.
///
/// A histogram, never a per-turn series: a timestamped stream of turn durations
/// reconstructs a session's working rhythm, which is the same objection that
/// rules out per-tool-call phone-home.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnWall {
    /// Turns under 5 seconds.
    pub lt_5s: u32,
    /// Turns in `[5s, 30s)`.
    #[serde(rename = "5_30s")]
    pub five_to_thirty: u32,
    /// Turns in `[30s, 120s)`.
    #[serde(rename = "30_120s")]
    pub thirty_to_onetwenty: u32,
    /// Turns at or over 120 seconds.
    pub gte_120s: u32,
}

impl TurnWall {
    /// Field names in wire spelling, in declaration order.
    pub const FIELDS: &'static [&'static str] = &["lt_5s", "5_30s", "30_120s", "gte_120s"];

    /// Record one turn of `secs` wall-clock seconds.
    pub fn observe_secs(&mut self, secs: u64) {
        match secs {
            0..5 => self.lt_5s = self.lt_5s.saturating_add(1),
            5..30 => self.five_to_thirty = self.five_to_thirty.saturating_add(1),
            30..120 => self.thirty_to_onetwenty = self.thirty_to_onetwenty.saturating_add(1),
            _ => self.gte_120s = self.gte_120s.saturating_add(1),
        }
    }
}

/// One telemetry event.
///
/// The tag is the `event` key, so the wire form is flat and the variant set is
/// closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// This binary's version differs from the one last recorded on this machine.
    InstallOrUpgrade {
        /// Install, upgrade, or downgrade.
        kind: InstallKind,
        /// The previously recorded version, read from the telemetry state file
        /// **only** — never from session history or config mtimes, which have a
        /// different privacy contract.
        previous_version: Option<String>,
    },
    /// A session began.
    SessionStart {
        /// How it was started.
        source: SessionSource,
    },
    /// A session ended. Everything the session accumulated ships here, once.
    SessionEnd {
        /// Bucketed wall-clock session length.
        duration_bucket: DurationBucket,
        /// How the process ended.
        exit_class: ExitClass,
        /// Bucketed cold start. `null` on surfaces that do not measure it.
        cold_start_bucket: Option<ColdStartBucket>,
        /// Sorted, deduplicated `ProviderKind` names. A custom provider yields
        /// the literal `"custom"`, never the customer's `[providers.<name>]`
        /// table key, and no model id is sent for any provider.
        providers: Vec<String>,
        /// Feature-use counts.
        counters: Counters,
        /// Error counts.
        errors: Errors,
        /// Turn wall-clock histogram.
        turn_wall: TurnWall,
    },
    /// The process panicked.
    Panic {
        /// Source location, reduced by the `crates/` allowlist. Never the panic
        /// *message*: a slicing panic embeds the entire string being sliced, and
        /// this tree slices user and model text in dozens of places.
        site: String,
    },
    /// Anonymous service health aggregates under explicit operator consent.
    OperationsSummary {
        /// Aggregate requests.
        requests: u32,
        /// Aggregate errors.
        errors: u32,
        /// Aggregate duration ms total.
        duration_ms_total: u32,
        /// Aggregate duration ms max.
        duration_ms_max: u32,
        /// Aggregate probes.
        probes: u32,
        /// Aggregate probes failed.
        probes_failed: u32,
    },
    /// Aggregate browser or application interactions without a timeline.
    ProductUsage {
        /// Closed counts; never a URL, user identity, or work content.
        counters: ProductCounters,
    },
    /// One session's model turns and token counts for one provider and model.
    ModelUsage {
        /// A known provider id ([`is_known_provider_id`]).
        provider: String,
        /// A bundled-catalog model id, or `other` ([`is_reported_model_id`]).
        model: String,
        /// Turn outcomes and token totals.
        counters: ModelCounters,
        /// Turn wall-clock buckets for these turns.
        turn_wall: TurnWall,
    },
    /// One session's calls of one tool.
    ToolUsage {
        /// A first-party tool name, `mcp` or `other` ([`is_reported_tool_name`]).
        tool: String,
        /// Calls made.
        calls: u32,
        /// Calls that returned an error.
        errors: u32,
    },
}

/// Turn outcomes and token totals for one provider and model in a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCounters {
    /// Turns that completed.
    pub turns_ok: u32,
    /// Turns that failed.
    pub turns_failed: u32,
    /// Turns the user or the runtime interrupted.
    pub turns_interrupted: u32,
    /// Input tokens, including cache reads.
    pub input_tokens: u64,
    /// Output tokens, including reasoning.
    pub output_tokens: u64,
    /// Input tokens served from the provider's prompt cache.
    pub cache_read_tokens: u64,
    /// Input tokens written to the provider's prompt cache.
    pub cache_write_tokens: u64,
}

impl ModelCounters {
    /// Wire keys, in order.
    pub const FIELDS: &'static [&'static str] = &[
        "turns_ok",
        "turns_failed",
        "turns_interrupted",
        "input_tokens",
        "output_tokens",
        "cache_read_tokens",
        "cache_write_tokens",
    ];
}

/// First-party tool names reported by `tool_usage`. Any other name is
/// reported as `mcp` or `other`, so a plugin's or server's tool name never
/// leaves the machine.
pub const TOOL_NAMES: &[&str] = &[
    "agent",
    "apply_patch",
    "authenticate",
    "bash",
    "code_execution",
    "context_archive_get",
    "context_archive_search",
    "create_goal",
    "diagnostics",
    "edit",
    "edit_file",
    "exec_shell",
    "execute_tools",
    "fetch_url",
    "file_search",
    "fim_edit",
    "finance",
    "get_goal",
    "git_blame",
    "git_commit_plan",
    "git_diff",
    "git_fetch",
    "git_log",
    "git_merge_tree",
    "git_show",
    "git_status",
    "grep_files",
    "handle_read",
    "image_ocr",
    "js_execution",
    "list_dir",
    "load_skill",
    "lsp",
    "memory_get",
    "memory_search",
    "note",
    "notify",
    "pandoc_convert",
    "project_map",
    "read",
    "read_file",
    "read_media",
    "remember",
    "request_plugin_install",
    "request_user_input",
    "retrieve_tool_result",
    "revert_turn",
    "review",
    "rlm",
    "run_tests",
    "run_verifiers",
    "session_get",
    "session_search",
    "start_mcp_server",
    "task_create",
    "task_shell_wait",
    "todo_write",
    "tool_search",
    "update_goal",
    "update_plan",
    "validate_data",
    "verify",
    "wait_for_dev_server",
    "web_search",
    "workflow",
    "write",
    "write_file",
];

/// The `tool` value for a tool name: itself when first-party, `mcp` for an
/// MCP pool tool, otherwise `other`.
#[must_use]
pub fn reported_tool_name(name: &str, is_mcp: bool) -> &'static str {
    match TOOL_NAMES.iter().find(|known| **known == name) {
        Some(known) => known,
        None if is_mcp => "mcp",
        None => "other",
    }
}

/// Whether `value` may appear as `tool_usage.tool`.
#[must_use]
pub fn is_reported_tool_name(value: &str) -> bool {
    value == "mcp" || value == "other" || TOOL_NAMES.contains(&value)
}

/// The `model` value for a model id: the lowercased id when the bundled model
/// catalog lists it, otherwise `other`. Custom, local and fine-tuned model
/// names therefore never leave the machine.
#[must_use]
pub fn reported_model_id(model: &str) -> String {
    let id = model.trim().to_ascii_lowercase();
    if is_catalogued_model(&id) {
        id
    } else {
        "other".to_string()
    }
}

/// Whether `value` may appear as `model_usage.model`.
#[must_use]
pub fn is_reported_model_id(value: &str) -> bool {
    value == "other" || (value == value.to_ascii_lowercase() && is_catalogued_model(value))
}

fn is_catalogued_model(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    let catalog = codewhale_config::catalog::bundled_models_dev_catalog();
    catalog.reviewed.intrinsic.contains_key(id)
        || catalog
            .models
            .keys()
            .any(|key| key.eq_ignore_ascii_case(id))
        || catalog.providers.values().any(|provider| {
            provider
                .models
                .keys()
                .any(|key| key.eq_ignore_ascii_case(id))
        })
}

impl Event {
    /// The `event` discriminant, for the doc-match test.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::InstallOrUpgrade { .. } => "install_or_upgrade",
            Self::SessionStart { .. } => "session_start",
            Self::SessionEnd { .. } => "session_end",
            Self::Panic { .. } => "panic",
            Self::ProductUsage { .. } => "product_usage",
            Self::ModelUsage { .. } => "model_usage",
            Self::ToolUsage { .. } => "tool_usage",
            Self::OperationsSummary { .. } => "operations_summary",
        }
    }

    /// Whether every string this event carries is inside its declared bound.
    ///
    /// The bounds above are enforced by the *constructors* — `Counters` is a
    /// struct of `u32`s, `providers` comes from `ProviderKind::as_str()`, and
    /// `site` comes from [`crate::reduce_panic_site`]. That holds only for an
    /// event this process built. Events are also **read back from
    /// `buffer.jsonl` and deserialized** before a batch is assembled, and
    /// `serde` will happily fill `site`, `previous_version`, and `providers`
    /// with any string the file contains. Anything running as this user can
    /// append a line to that file — including a `Bash` tool call this session
    /// made on the model's behalf — so the drain path must re-establish the
    /// bound rather than inherit it.
    ///
    /// Failing this check drops the event. It is never *sanitized*: a payload
    /// that the schema cannot account for is not made safe by editing it.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        match self {
            Self::SessionStart { .. }
            | Self::ProductUsage { .. }
            | Self::OperationsSummary { .. } => true,
            Self::InstallOrUpgrade {
                previous_version, ..
            } => previous_version
                .as_deref()
                .is_none_or(is_release_version_string),
            Self::SessionEnd { providers, .. } => {
                providers.iter().all(|name| is_known_provider_id(name))
            }
            Self::Panic { site } => is_reduced_panic_site(site),
            Self::ModelUsage {
                provider, model, ..
            } => is_known_provider_id(provider) && is_reported_model_id(model),
            Self::ToolUsage { tool, .. } => is_reported_tool_name(tool),
        }
    }
}

/// Whether `value` is a release version this schema may carry.
///
/// `^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$` — the rule already written on
/// [`Batch::app_version`], applied to `previous_version` as well because that
/// field is read back from `state.json` rather than built in this process.
#[must_use]
pub fn is_release_version_string(value: &str) -> bool {
    let (core, pre) = match value.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (value, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3
        || !parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    match pre {
        None => true,
        Some(pre) => !pre.is_empty() && pre.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.'),
    }
}

/// Whether `value` is in the output space of [`crate::reduce_panic_site`]:
/// the literal `<dep>`, or `crates/…​.rs:<line>:<column>` over the allowlist
/// charset.
#[must_use]
pub fn is_reduced_panic_site(value: &str) -> bool {
    if value == "<dep>" {
        return true;
    }
    let Some((file, rest)) = value.split_once(".rs:") else {
        return false;
    };
    let Some((line, column)) = rest.split_once(':') else {
        return false;
    };
    file.starts_with("crates/")
        && file
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'/' | b'.' | b'-'))
        && !line.is_empty()
        && line.bytes().all(|b| b.is_ascii_digit())
        && !column.is_empty()
        && column.bytes().all(|b| b.is_ascii_digit())
}

/// Whether `value` is a provider id this build knows.
///
/// Checked against the **full** provider registry, not
/// `ProviderKind::all()`: that list is the selectable catalog subset, while
/// captured intrinsic kinds legitimately include dialect kinds
/// (`deepseek-anthropic`, the Model Studio plan variants) that are absent from
/// it. Narrowing to the catalog would silently drop a real user's route.
#[must_use]
pub fn is_known_provider_id(value: &str) -> bool {
    codewhale_config::provider::all_providers()
        .iter()
        .any(|provider| provider.id() == value)
}

/// The POST body. One per flush.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Batch {
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// [`NOTICE_VERSION`], policy metadata rather than proof of acceptance.
    pub notice_version: u32,
    /// RFC3339 UTC, second precision. Per-**batch** only — events carry no
    /// timestamps at all.
    pub sent_at: String,
    /// Random v4 UUID stored on this machine, rotated every 90 days.
    pub install_id: String,
    /// `CARGO_PKG_VERSION`. Must match `^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$`.
    pub app_version: String,
    /// First 12 hex chars of the release-CI build sha, or `null` for every
    /// locally built binary.
    pub git_sha: Option<String>,
    /// Which surface produced this batch.
    pub surface: Surface,
    /// OS family.
    pub os: Os,
    /// CPU family.
    pub arch: Arch,
    /// Compile-time libc.
    pub libc: Libc,
    /// Whether both stdin and stdout were terminals.
    pub tty: bool,
    /// The events.
    pub events: Vec<Event>,
}

impl Batch {
    /// Envelope field names in declaration order, for the doc-match test.
    pub const FIELDS: &'static [&'static str] = &[
        "schema_version",
        "notice_version",
        "sent_at",
        "install_id",
        "app_version",
        "git_sha",
        "surface",
        "os",
        "arch",
        "libc",
        "tty",
        "events",
    ];
}

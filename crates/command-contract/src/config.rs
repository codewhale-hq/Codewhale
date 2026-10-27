//! Config-group command facets (FEAT-028 D3/D4).
//!
//! These traits let `/config`, `/settings`, `/mode`, `/theme`, `/verbose`,
//! `/login`, `/logout`, `/pet` and `/trust` express their intent without naming
//! `App`, a service locator, a callback, or any concrete host type. Hosts own
//! persistence, services and I/O; the values here grant no terminal, TUI or
//! runtime authority, and a missing facet must fail safely rather than panic.

use crate::outcome::ConfigAction;
use std::path::{Path, PathBuf};

/// A message the host raises for the command to render.
///
/// The host never renders user-facing text for the config group: it returns a
/// stable key plus named replacements (each replacement is itself a message,
/// so a host never pre-renders a localized fragment), and the command resolves
/// it through the `presentation` shape. Non-localized text (protocol/config/
/// command names, computed values) stays a `Literal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigMessage {
    /// Resolved by the presentation shape via its stable snake_case key.
    Keyed {
        key: String,
        params: Vec<(String, ConfigMessage)>,
    },
    /// Non-localized text.
    Literal(String),
    /// A sequence rendered in order, for literal framing around keyed parts.
    Concat(Vec<ConfigMessage>),
}

impl ConfigMessage {
    pub fn keyed(key: impl Into<String>) -> Self {
        Self::Keyed {
            key: key.into(),
            params: Vec::new(),
        }
    }
    pub fn keyed_with(key: impl Into<String>, params: Vec<(String, ConfigMessage)>) -> Self {
        Self::Keyed {
            key: key.into(),
            params,
        }
    }
    pub fn literal(text: impl Into<String>) -> Self {
        Self::Literal(text.into())
    }
    pub fn concat(parts: Vec<ConfigMessage>) -> Self {
        Self::Concat(parts)
    }
}

impl From<String> for ConfigMessage {
    fn from(value: String) -> Self {
        Self::Literal(value)
    }
}

impl From<&str> for ConfigMessage {
    fn from(value: &str) -> Self {
        Self::Literal(value.to_string())
    }
}

impl From<std::borrow::Cow<'_, str>> for ConfigMessage {
    fn from(value: std::borrow::Cow<'_, str>) -> Self {
        Self::Literal(value.into_owned())
    }
}

/// Portable result of a config-group host operation.
///
/// The host raises a [`ConfigMessage`] and the single action the request
/// implies; the command resolves the message through the presentation shape and
/// maps the reply to `CommandResult<ConfigAction>` without touching `App`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigReply {
    pub message: Option<ConfigMessage>,
    pub action: Option<ConfigAction>,
    pub is_error: bool,
}

impl ConfigReply {
    pub fn ok() -> Self {
        Self {
            message: None,
            action: None,
            is_error: false,
        }
    }
    pub fn message(msg: impl Into<ConfigMessage>) -> Self {
        Self {
            message: Some(msg.into()),
            action: None,
            is_error: false,
        }
    }
    pub fn error(msg: impl Into<ConfigMessage>) -> Self {
        let msg = match msg.into() {
            ConfigMessage::Literal(text) => ConfigMessage::Literal(format!("Error: {text}")),
            other => other,
        };
        Self {
            message: Some(msg),
            action: None,
            is_error: true,
        }
    }
    pub fn action(action: ConfigAction) -> Self {
        Self {
            message: None,
            action: Some(action),
            is_error: false,
        }
    }
    pub fn with_message_and_action(msg: impl Into<ConfigMessage>, action: ConfigAction) -> Self {
        Self {
            message: Some(msg.into()),
            action: Some(action),
            is_error: false,
        }
    }
}

/// One structured `/settings` row: the label, its column width and its value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSettingsRow {
    pub label: String,
    pub value: String,
    pub label_width: usize,
}

/// Typed `/settings text` report.
///
/// The host supplies display rows and the config-file path; the command owns
/// the title, separator, footer and joining.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSettingsReport {
    pub rows: Vec<CommandSettingsRow>,
    pub config_file: String,
    pub from_settings: Vec<String>,
    pub session_overrides: Vec<String>,
}

/// Typed `/config subagents [status]` report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSubagentsReport {
    pub disabled_reason: Option<String>,
    pub config_path: String,
    pub provider_key: String,
    pub provider_label: String,
    pub explicit_enabled: Option<bool>,
    pub limits: Vec<CommandSubagentsLimit>,
    /// Per-provider override rows for the active provider, when configured.
    pub provider_rows: Option<Vec<CommandSubagentsProviderRow>>,
}

/// One of the five sub-agent limits: raw, resolved global and active-provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSubagentsLimit {
    pub key: &'static str,
    pub raw: String,
    pub global: String,
    pub provider: String,
}

/// One per-provider override row (already display-formatted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSubagentsProviderRow {
    pub key: &'static str,
    pub value: String,
}

/// One row of the `/config status` editability audit.
///
/// The host supplies typed facts; the command owns the table layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandConfigAuditRow {
    pub key: String,
    pub current: String,
    pub editability: String,
    pub command: String,
    /// Keyed when the note is localized, literal otherwise.
    pub note: ConfigMessage,
}

/// Typed `/config status` audit: the resolved config path and its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandConfigAuditReport {
    pub config_path: String,
    pub rows: Vec<CommandConfigAuditRow>,
}

/// The complete reply contract for mutating config operations.
///
/// A mutating [`CommandConfigContext`] operation MUST return a [`ConfigReply`]
/// whose keyed messages use only these keys, each with exactly these parameter
/// names. The command validates every entry (and prepares the substituted
/// template) *before* the effect, so rendering a mutating reply is pure
/// substitution and never translates after the effect. A key or parameter set
/// outside this contract is a host bug.
pub const EFFECT_REPLY_TEMPLATES: &[(&str, &[&str])] = &[
    ("config_command_invalid_value", &["key", "value", "choices"]),
    ("config_notification_updated", &["key", "value", "scope"]),
    ("config_notifications_set_hint", &[]),
    ("config_prompt_suggestion_updated", &["value", "scope"]),
    ("config_scope_saved", &[]),
    ("config_scope_session", &[]),
    ("config_search_updated", &["value", "scope"]),
    ("contextual_tips_not_saved", &["error"]),
    ("help_usage_label", &[]),
    ("startup_default_not_saved", &["setting", "error"]),
];

/// Read/write configuration, presets, the editability audit, login state and
/// the config-owned view/display commands (`/mode`, `/theme`, `/verbose`,
/// `/fullscreen`, `/inline`, `/statusbar`, `/workbar`, `/import-claude`).
///
/// The host owns per-key validation, normalization, persistence and the exact
/// message; the portable handler owns argument parsing and subcommand routing.
/// A missing facet must fail safely rather than panic.
pub trait CommandConfigContext {
    /// Path of the active user configuration file, when one is resolved.
    fn config_path(&self) -> Option<PathBuf>;
    fn translation_enabled(&self) -> bool;
    fn set_translation_enabled(&mut self, enabled: bool);

    /// `/config status|audit|editability|editable` — typed audit rows.
    fn audit(&mut self) -> Result<CommandConfigAuditReport, String>;
    /// `/config workflow` / `/config goal` — typed effective facts.
    fn workflow_summary(&mut self) -> CommandWorkflowSummary;
    /// `/config subagents status` — typed subagent facts.
    fn subagents_status(&mut self) -> Result<CommandSubagentsReport, String>;
    /// `/config subagents <key>` — show one subagent setting.
    fn subagent(&mut self, key: &str) -> ConfigReply;
    /// `/config subagents <key> <value> [--save]`.
    fn set_subagent(&mut self, key: &str, value: &str, persist: bool) -> ConfigReply;
    /// `/config search.provider` — the current provider and source.
    fn search_provider_status(&mut self) -> ConfigReply;
    /// Accepted `search.provider` values, for the portable usage message.
    fn search_provider_names(&self) -> &'static str;
    /// `/config search.provider <value> [--save]`.
    fn set_search_provider(&mut self, value: &str, persist: bool) -> ConfigReply;
    /// `/config notifications` — the notifications report.
    fn notifications_status(&mut self) -> ConfigReply;
    /// `/config notifications <key>` — show one notification setting.
    fn notification(&mut self, key: &str) -> ConfigReply;
    /// `/config notifications <key> <value> [--save]`.
    fn set_notification(&mut self, key: &str, value: &str, persist: bool) -> ConfigReply;
    /// `/config preset <name> [--save]`.
    fn apply_preset(&mut self, name: &str, persist: bool) -> ConfigReply;
    /// `/config <key>` — show the current value.
    fn get_value(&mut self, key: &str) -> ConfigReply;
    /// `/config <key> <value> [--save]`.
    fn set_value(&mut self, key: &str, value: &str, persist: bool) -> ConfigReply;
    /// `/settings text` — typed settings facts.
    fn show_settings(&mut self) -> Result<CommandSettingsReport, String>;
    /// `/theme schema` — the user theme JSON schema.
    fn theme_schema(&self) -> String;
    /// `/theme path` — the user themes directory, rendered for display.
    fn themes_dir(&self) -> Result<String, String>;
    /// Current verbose-transcript flag.
    fn verbose_enabled(&self) -> bool;
    /// Set the verbose-transcript flag (the host also marks history updated).
    fn set_verbose_enabled(&mut self, enabled: bool);
    /// Current terminal screen mode, for `/fullscreen` and `/inline`.
    fn screen_mode(&self) -> crate::types::CommandScreenMode;
    /// `/import-claude` — build the plan from the host's home/workspace.
    fn build_plan(&mut self) -> Option<CommandClaudePlan>;
    /// `/import-claude` — persist the rendered report and optional bundle.
    fn write_artifacts(&mut self, report: &str, bundle: Option<&str>) -> CommandClaudeArtifacts;
    /// `/import-claude --apply` — carry over the standing instructions.
    fn apply_import(
        &mut self,
        plan: &CommandClaudePlan,
        report_path: &Path,
        wrote_report: bool,
    ) -> CommandClaudeApplyOutcome;
    /// Host display form of a path.
    fn display_path(&self, path: &Path) -> String;
}

/// Pet presence, mode, sound and habitat actions for `/pet`.
///
/// The command owns the verb parsing and message composition; the facet only
/// observes and mutates the pet state and resolves the localized labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPetControl {
    Sound,
    Browser,
    Window,
    Select,
    FocusAgents,
    /// Body style and/or text language for `/pet style|language`.
    Appearance {
        body: Option<CommandPetBodyStyle>,
        language: Option<CommandPetTextLanguage>,
    },
}

/// Portable pet body style for `/pet style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPetBodyStyle {
    Smooth,
    Pixel,
}

impl CommandPetBodyStyle {
    /// Canonical `pet_watch` token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smooth => "smooth",
            Self::Pixel => "pixel",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "smooth" => Some(Self::Smooth),
            "pixel" => Some(Self::Pixel),
            _ => None,
        }
    }
}

/// Portable pet text language for `/pet language`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPetTextLanguage {
    Auto,
    En,
    ZhHans,
    ZhHant,
    Ja,
}

impl CommandPetTextLanguage {
    /// Canonical `pet_watch` token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::En => "en",
            Self::ZhHans => "zh-hans",
            Self::ZhHant => "zh-hant",
            Self::Ja => "ja",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "en" => Some(Self::En),
            "zh-hans" => Some(Self::ZhHans),
            "zh-hant" => Some(Self::ZhHant),
            "ja" => Some(Self::Ja),
            _ => None,
        }
    }
}

/// Localized pet labels the command composes into its messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPetLabel {
    ModeOn,
    ModeOff,
    ModeOnLabel,
    ModeOffLabel,
    ViewOpen,
    ViewClosed,
    HabitatQueued,
    ExportQueued,
    ExportUnavailable,
    SoundOn,
    /// The habitat control could not be queued.
    WatchUnavailable,
}

pub trait CommandPetContext {
    fn enabled(&self) -> bool;
    fn is_open(&self) -> bool;
    fn status(&self) -> String;
    fn avatar_choices(&self) -> String;
    fn select_avatar(&mut self, key: &str) -> bool;
    fn preview_avatar(&mut self, name: &str, view: bool) -> bool;
    fn set_enabled(&mut self, enabled: bool, persist: bool);
    fn set_sound(&mut self, enabled: bool);
    fn sound_label(&self) -> String;
    fn export(&mut self) -> bool;
    fn open_habitat(&mut self);
    /// Queue a habitat control; `false` when it could not be delivered.
    fn queue(&mut self, control: CommandPetControl) -> bool;
    fn label(&self, which: CommandPetLabel) -> String;
}

/// Account sign-in state for `/login`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandAccountState {
    SignedOut,
    Authenticated,
    OfflineCached,
    Expired,
    Revoked,
    Unavailable(String),
}

/// `/login` status projection (the command composes the report text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLoginStatus {
    pub account: CommandAccountState,
    pub api_base: String,
    pub provider: String,
}

/// `/logout` result; each entry is `Ok(true)` cleared, `Ok(false)` absent,
/// `Err` the host error text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandLogoutOutcome {
    pub provider: String,
    pub account: Result<bool, String>,
    pub legacy_sandbox: Result<bool, String>,
}

/// Failure to clear the active provider key during `/logout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogoutFailure {
    pub provider: String,
    pub error: String,
}

/// Account/provider session state for `/login` and `/logout`.
pub trait CommandLoginContext {
    fn login_status(&self) -> CommandLoginStatus;
    fn logout(&mut self) -> Result<CommandLogoutOutcome, LogoutFailure>;
    /// `/connect [<provider>]` — plugin provider sign-in.
    fn connect(&mut self, provider: Option<&str>) -> ConfigReply;
    /// `/disconnect <provider>` — plugin provider sign-out.
    fn disconnect(&mut self, provider: &str) -> ConfigReply;
}

/// Workbar placement/panel observation and mutation for `/workbar`.
pub trait CommandWorkSurfaceContext {
    fn placement(&self) -> crate::types::CommandWorkSurfacePlacement;
    fn panel(&self) -> crate::types::CommandRailPanel;
    fn effective_placement(&self) -> crate::types::CommandWorkSurfacePlacement;
    fn area_present(&self) -> bool;
    fn panel_title(&self, panel: crate::types::CommandRailPanel) -> String;
    /// Apply a placement; when `persist`, also writes the setting. The reply is
    /// the key-engine result (ignored on success, returned on error).
    fn set_placement(
        &mut self,
        placement: crate::types::CommandWorkSurfacePlacement,
        persist: bool,
    ) -> ConfigReply;
    fn set_panel(&mut self, panel: crate::types::CommandRailPanel, persist: bool) -> ConfigReply;
    fn mark_redraw(&mut self);
}

/// A path problem surfaced by `/trust add|remove`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustPathError {
    /// The supplied path does not exist.
    NotFound(PathBuf),
    /// The allow-list could not be updated; carries the host error text.
    UpdateFailed(String),
}

/// Workspace trust and the per-path allow-list for `/trust`.
pub trait CommandTrustContext {
    fn trust_mode(&self) -> bool;
    fn trusted_paths(&self) -> Vec<PathBuf>;
    /// Add a path (the host expands `~` and validates existence).
    fn add_trusted_path(&mut self, raw: &str) -> Result<PathBuf, TrustPathError>;
    /// Returns the resolved path and whether it was removed.
    fn remove_trusted_path(&mut self, raw: &str) -> Result<(PathBuf, bool), TrustPathError>;
    /// The localized `Usage:` label used by the `/trust on|off` usage error.
    fn usage_label(&self) -> String;
}

// ---------------------------------------------------------------------------
// Portable payloads for host effects that cross the config boundary (D5).
//
// The TUI conversion layer has no access to `App`, so engine-affecting actions
// cannot be re-derived at conversion time. These mirrors carry the exact data
// the host needs, and the host converts each direction faithfully.
// ---------------------------------------------------------------------------

/// Route capability fact, mirroring the config layer's `CapabilityState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCapabilityState {
    Supported,
    Unsupported,
    Unknown,
}

/// Compaction configuration carried by `ConfigAction::CompactionChanged`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCompactionConfig {
    pub enabled: bool,
    pub token_threshold: usize,
    pub model: String,
    pub image_input: CommandCapabilityState,
    pub effective_context_window: Option<u32>,
    pub cache_summary: bool,
    pub focus: Option<String>,
    pub runtime_cost_owner: Option<String>,
    pub workspace: Option<PathBuf>,
    pub summary_instructions: Option<String>,
    pub retained_user_message_tokens: usize,
}

/// Subagent runtime configuration carried by `ConfigAction::SubagentRuntimeChanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSubagentRuntimeConfig {
    pub enabled: bool,
    pub max_subagents: usize,
    pub launch_concurrency: usize,
    pub max_spawn_depth: u32,
    pub api_timeout_secs: u64,
    pub heartbeat_timeout_secs: u64,
}

/// Search provider carried by `ConfigAction::SearchProviderChanged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSearchProvider {
    Bing,
    DuckDuckGo,
    Firecrawl,
    Tavily,
    Bocha,
    Metaso,
    Searxng,
    Baidu,
    Volcengine,
    Sofya,
    Serply,
    DeepSeek,
}

/// Operator-notification delivery method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandNotificationMethod {
    Auto,
    Osc9,
    Bel,
    Kitty,
    Ghostty,
    Off,
}

/// Turn-completion sound choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCompletionSound {
    Off,
    Beep,
    Bell,
    Whale,
    File,
}

/// When operator notifications are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandNotificationCondition {
    Always,
    Unfocused,
    Never,
}

/// Subagent-completion notification policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandSubagentCompletion {
    Always,
    FinalOnly,
    Off,
}

/// One notification event kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandNotificationEvent {
    TurnComplete,
    SubagentTerminal,
    ApprovalNeeded,
    InputNeeded,
    ElevationNeeded,
    ModelNotify,
}

/// One validated notification edit, mirroring `NotificationConfigUpdate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandNotificationUpdate {
    Method(CommandNotificationMethod),
    ThresholdSecs(u64),
    IncludeSummary(bool),
    Quiet(bool),
    Sound(Option<CommandCompletionSound>),
    Condition(CommandNotificationCondition),
    CompletionSound(CommandCompletionSound),
    SubagentCompletion(CommandSubagentCompletion),
    SoundFile(PathBuf),
    Event(CommandNotificationEvent, bool),
    EventSoundEnabled(bool),
    EventSoundEvents(Vec<String>),
    EventSoundMinIntervalMs(u64),
    EventSoundQuiet(bool),
}

// ---------------------------------------------------------------------------
// `/import-claude` plan model (portable). The host builds the plan from the
// Claude sources and performs the writes/apply; the command renders it.
// ---------------------------------------------------------------------------

/// Effective `[workflow]`/`[goal]` facts for `/config workflow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandWorkflowSummary {
    pub automatic: bool,
    pub auto_start_read_only: bool,
    pub require_approval_for_writes: bool,
    pub max_children: u32,
    pub max_concurrent: u32,
    pub max_depth: u32,
    pub default_token_budget: u64,
    pub goal_max_continuations: u32,
    pub goal_enforce_token_budget: bool,
}

/// Outcome of reading one Claude source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSourceStatus {
    Found,
    NotFound,
    Oversize(u64),
    InvalidJson,
    Unreadable(String),
}

impl CommandSourceStatus {
    /// The one-line provenance entry used by the rendered plan and report.
    ///
    /// `display_path` is the host's display projection (for example `~/...`),
    /// so the portable layer never leaks an absolute home path.
    #[must_use]
    pub fn describe(&self, display_path: &str) -> String {
        match self {
            Self::Found => format!("found: {display_path}"),
            Self::NotFound => format!("not found: {display_path}"),
            Self::Oversize(bytes) => {
                format!("skipped, too large ({bytes} bytes): {display_path}")
            }
            Self::InvalidJson => format!("invalid JSON: {display_path}"),
            Self::Unreadable(error) => format!("unreadable: {display_path} ({error})"),
        }
    }
}

/// One MCP server discovered in the Claude sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMcpCandidate {
    pub name: String,
    pub summary: String,
    pub hard_blocked: bool,
}

/// The full `/import-claude` plan. Pure data: building it mutates nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandClaudePlan {
    pub sources: Vec<(PathBuf, CommandSourceStatus)>,
    pub mcp_candidates: Vec<CommandMcpCandidate>,
    pub per_project_mcp: Vec<String>,
    /// Safe env entries proposed for the portable bundle, key-sorted.
    pub env_safe: Vec<(String, String)>,
    pub env_skipped: Vec<String>,
    pub permissions_default_mode: Option<String>,
    pub permissions_allow: Vec<String>,
    pub permissions_ask: Vec<String>,
    pub permissions_deny: Vec<String>,
    pub hook_events: Vec<String>,
    pub has_claude_md: bool,
    pub claude_projects: Vec<String>,
}

impl CommandClaudePlan {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mcp_candidates.is_empty()
            && self.per_project_mcp.is_empty()
            && self.env_safe.is_empty()
            && self.env_skipped.is_empty()
            && self.permissions_default_mode.is_none()
            && self.permissions_allow.is_empty()
            && self.permissions_ask.is_empty()
            && self.permissions_deny.is_empty()
            && self.hook_events.is_empty()
            && !self.has_claude_md
            && self.claude_projects.is_empty()
    }
}

/// One typed `/import-claude --apply` follow-up.
///
/// The host supplies the facts; the command composes the sentences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandImportFollowUp {
    InstructionsCopied { destination: String },
    InstructionsKeptExisting { destination: String, source: String },
    InstructionsCopyFailed { source: String, destination: String },
    HomeUnresolved,
    McpBlocked { name: String },
    McpNeedsReview { name: String },
    HookEvents { count: usize },
    PermissionRules,
}

/// Paths and write outcomes from persisting the report/bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandClaudeArtifacts {
    pub report_path: PathBuf,
    pub bundle_path: PathBuf,
    pub wrote_report: bool,
    pub wrote_bundle: bool,
}

/// Typed `--apply` outcome; the command composes the final message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandClaudeApplyOutcome {
    pub done: Vec<CommandImportFollowUp>,
    pub manual: Vec<CommandImportFollowUp>,
    pub report_path: PathBuf,
    pub wrote_report: bool,
}

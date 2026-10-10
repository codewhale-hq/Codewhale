//! Plain data types shared across the TUI: modes, effort/collapse/display
//! enums, the public `TuiOptions` construction bag, queued-message records,
//! and the action enums drained by the event loop.
//!
//! Everything here is pure data (plus parsing/labeling helpers that need no
//! `App` state). The TUI-owned items are re-exported from `app.rs` so existing
//! `crate::tui::app::X` paths are unchanged; types owned by another crate
//! (such as [`AppMode`]) are named at their own crate path instead.

use super::*;

use codewhale_config::AppMode;

/// What an interactive setting selection actually did.
///
/// The three cases are genuinely different to the user, and the boolean this
/// replaced conflated the last two: a refused selection and an accepted one
/// that only wrote the startup default both returned `false`, so every caller
/// reported "already in that mode" and showed no receipt for the write.
///
/// Only [`Self::Changed`] means live session state moved — that is the case
/// that must still emit an `AppAction` so the engine is resynchronized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingSelection {
    /// Live state moved, and the startup default was persisted.
    Changed,
    /// Live state already matched, and the startup default was persisted. This
    /// is the normal shape after a session restore, where the live value and
    /// the startup default legitimately disagree.
    PersistedSame,
    /// Refused by the turn lock (#2982). Nothing was written anywhere.
    Refused,
}

impl SettingSelection {
    /// Whether live state moved — i.e. whether the engine needs resyncing.
    #[must_use]
    pub fn changed_live_state(self) -> bool {
        matches!(self, Self::Changed)
    }

    /// Whether the selection was accepted at all (either case that persisted).
    #[must_use]
    #[cfg(test)]
    pub fn accepted(self) -> bool {
        !matches!(self, Self::Refused)
    }
}

/// Localized, TUI-only presentation of [`AppMode`]. Kept out of
/// codewhale-config so the mode type does not depend on the locale packs.
pub trait AppModeUi {
    /// Localized short name for the mode picker (user-facing surface only).
    fn display_name_localized(self, locale: Locale) -> Cow<'static, str>;
    /// Localized one-line hint for the mode picker (user-facing surface only).
    fn picker_hint_localized(self, locale: Locale) -> Cow<'static, str>;
}

impl AppModeUi for AppMode {
    /// Localized short name for the mode picker (user-facing surface only).
    fn display_name_localized(self, locale: Locale) -> Cow<'static, str> {
        tr(
            locale,
            match self {
                AppMode::Agent => MessageId::AppModeAgent,
                AppMode::Plan => MessageId::AppModePlan,
                AppMode::Operate => MessageId::AppModeOperate,
            },
        )
    }

    /// Localized one-line hint for the mode picker (user-facing surface only).
    fn picker_hint_localized(self, locale: Locale) -> Cow<'static, str> {
        tr(
            locale,
            match self {
                AppMode::Agent => MessageId::AppModeAgentHint,
                AppMode::Plan => MessageId::AppModePlanHint,
                AppMode::Operate => MessageId::AppModeOperateHint,
            },
        )
    }
}

/// Exact provider/model route whose prompt can be inspected or replayed.
///
/// Auto-model sessions keep `model == "auto"` as the user's selection, so
/// cache operations must carry the last concrete route separately. The base
/// URL is absent after restoring an older session because saved Auto receipts
/// intentionally do not persist raw endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheReplayTarget {
    pub(crate) provider: ProviderKind,
    pub(crate) provider_identity: String,
    /// Additive exact provider id used by persisted-route resolution.
    /// `None` is meaningful for the legacy root-level `custom` route.
    pub(crate) provider_id: Option<String>,
    pub(crate) model: String,
    pub(crate) base_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerDensity {
    Compact,
    Comfortable,
    Spacious,
}

impl ComposerDensity {
    #[must_use]
    pub fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" | "tight" => Self::Compact,
            "spacious" | "loose" => Self::Spacious,
            _ => Self::Comfortable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptSpacing {
    Compact,
    Comfortable,
    Spacious,
}

impl TranscriptSpacing {
    #[must_use]
    pub fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" | "tight" => Self::Compact,
            "spacious" | "loose" => Self::Spacious,
            _ => Self::Comfortable,
        }
    }
}

/// Controls how dense tool-call runs are collapsed in the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCollapseMode {
    /// Collapse qualifying tool runs by default.
    ///
    /// Collapsed success cells keep the tool-name + arg/command summary as the
    /// single intent line (#3256 decision): that is already the model-visible
    /// call summary, so a second "intent" source is not required.
    Compact,
    /// Never collapse tool runs automatically.
    Expanded,
    /// Collapse only when calm mode is active.
    Calm,
}

impl ToolCollapseMode {
    #[must_use]
    pub fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "expanded" | "off" | "none" => Self::Expanded,
            "calm" | "calm-mode" | "calm_only" | "calm-only" => Self::Calm,
            // `collapsed`/`collapse` are issue #3256's preferred names for the
            // default; treat them like the canonical `compact`.
            _ => Self::Compact,
        }
    }

    #[must_use]
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Expanded => "expanded",
            Self::Calm => "calm",
        }
    }

    #[must_use]
    pub fn is_active(self, calm_mode: bool) -> bool {
        match self {
            Self::Compact => true,
            Self::Expanded => false,
            Self::Calm => calm_mode,
        }
    }
}

/// Configuration required to bootstrap the TUI.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct TuiOptions {
    pub model: String,
    pub workspace: PathBuf,
    pub config_path: Option<PathBuf>,
    pub config_profile: Option<String>,
    pub allow_shell: bool,
    /// Screen the TUI starts on (alternate screen, or a full-height inline
    /// viewport that leaves the host scrollback intact).
    pub screen_mode: ScreenMode,
    /// Capture mouse input for internal scrolling/selection, on the screen
    /// the session starts on.
    pub use_mouse_capture: bool,
    /// The user's mouse-capture answer with the screen factored out (CLI
    /// flag, `tui.mouse_capture`, or the host default). `/fullscreen` and
    /// `/inline` re-derive `use_mouse_capture` from it, so the documented
    /// default keeps applying after a runtime switch.
    pub mouse_capture_preference: bool,
    /// Enable terminal bracketed-paste mode (OSC `?2004h` / `?2004l`). Defaults
    /// on; settable via `bracketed_paste = false` in `settings.toml` for the
    /// rare terminal that mishandles it.
    pub use_bracketed_paste: bool,
    /// Maximum number of concurrent sub-agents.
    pub max_subagents: usize,
    pub skills_dir: PathBuf,
    pub memory_path: PathBuf,
    #[expect(dead_code)]
    pub notes_path: PathBuf,
    pub mcp_config_path: PathBuf,
    pub use_memory: bool,
    /// Start in agent mode (defaults to agent; --yolo starts in YOLO)
    pub start_in_agent_mode: bool,
    /// Skip onboarding screens
    pub skip_onboarding: bool,
    /// Auto-approve tool executions (yolo mode)
    pub yolo: bool,
    /// Resume a previous session by ID
    pub resume_session_id: Option<String>,
    /// Pre-populate the composer with this text when the TUI starts.
    /// Used by `deepseek pr <N>` (#451) to drop the model into a
    /// session with the PR context already typed — the user can edit
    /// before sending or hit Enter to fire as-is.
    pub initial_input: Option<InitialInput>,
    /// One-line receipt to show once at startup.
    ///
    /// Auto-resume uses this to say what it did — reattached, or fell back to
    /// a fresh transcript because the candidate was missing, unreadable, or
    /// recorded against a different workspace (#2934). Silence is the correct
    /// value when nothing happened worth reporting.
    pub startup_notice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InitialInput {
    /// Pre-populate the composer and wait for the user to press Enter.
    ///
    /// Used by `codewhale pr <N>` (#451) to drop the model into a session
    /// with the PR context already typed so the user can edit before sending.
    Prefill(String),
    /// Pre-populate the composer, submit it once startup is ready, then keep
    /// the interactive session open for follow-up messages (#2370).
    Submit(String),
    /// Begin account-owned web remote control after the TUI is initialized.
    RemoteControl,
}

// === Sub-state structs for App field organization (#377) ===

/// Vim modal editing mode for the composer input area.
///
/// Enabled via `[composer] mode = "vim"` in `settings.toml`.  When the
/// composer vim mode is active the user starts in `Normal` mode and presses
/// `i`, `a`, or `o` to enter `Insert` mode.  `Esc` from `Insert` returns to
/// `Normal`.  Standard vim motions (`h`/`j`/`k`/`l`, `w`/`b`, `0`/`$`, `x`,
/// `dd`) work in `Normal` mode.  `Visual` is reserved for future selection
/// support and currently behaves like `Normal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VimMode {
    /// Normal / command mode — motions and operators, no text insertion.
    #[default]
    Normal,
    /// Insert mode — characters are appended at the cursor as typed.
    Insert,
    /// Visual mode — reserved for future selection support.
    Visual,
}

impl VimMode {}

/// Message queued while the engine is busy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedMessage {
    pub display: String,
    pub skill_instruction: Option<String>,
    pub skill_provenance: Option<crate::skills::SkillProvenance>,
    /// True once this turn has been painted into `history` as `HistoryCell::User`.
    /// Queue/offline submit echoes before the model runs; Immediate prepare skips
    /// a second paint when this is set so drained queued turns do not double.
    pub history_echoed: bool,
}

/// The message the current turn was dispatched with, and the transcript cell
/// that shows it. When the engine reports the turn was never sent (a key
/// rejected before any model output), this is what goes back in the composer,
/// skill included, and the bubble that comes out of the transcript (#6566).
#[derive(Debug, Clone)]
pub struct UnansweredSubmission {
    pub message: QueuedMessage,
    pub history_cell: usize,
}

/// A steer handed to the engine that the engine has not yet recorded.
///
/// Live-only, and deliberately not in `api_messages`: `EngineHandle::steer`
/// succeeding means the channel took the text, not that a turn accepted it.
/// The engine commits a steer at a step boundary and drops one whose turn has
/// already moved on, so painting a settled transcript cell at send time
/// produced a cell that could sit above the work it followed, or survive
/// forever for a steer the model never saw (#6190). It becomes a real cell
/// when the engine's own record shows it, and a "could not send" receipt when
/// the turn ends without it.
#[derive(Debug, Clone)]
pub struct InflightSteer {
    /// The composed message, carried so acceptance can paint the same cell
    /// (including the queue-time echo it may already own).
    pub message: QueuedMessage,
    /// Exactly what was handed to `EngineHandle::steer`. The engine records
    /// this as the first text block of the accepted user message, which is
    /// what acceptance matches on.
    pub content: String,
    /// `api_messages.len()` when the steer was sent — the lower bound for the
    /// acceptance search, so an identical earlier message cannot claim it.
    pub sent_after_index: usize,
    /// Held until acceptance knows the message index to anchor them to.
    pub references: Vec<codewhale_core::ContextReference>,
}

/// Prefix for the bounded, tool-less model turn produced by `/workflow`.
///
/// The marker travels with the queued message so a draft that waits behind an
/// active turn keeps the same no-tools policy when it is eventually sent.
pub(crate) const WORKFLOW_DRAFT_INSTRUCTION_PREFIX: &str = "[codewhale.workflow-draft.v1]";

/// How a freshly-typed user input should be sent.
///
/// Picked by [`App::decide_composer_submit`] when the user submits a
/// non-empty composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitDisposition {
    /// Engine idle and online: send immediately.
    Immediate,
    /// Park on `queued_messages` (offline, or engine busy — #382).
    Queue,
    /// Amend the active turn immediately (#382).
    Steer,
    /// Park on `queued_messages` for dispatch after TurnComplete.
    /// Legacy path; #382 unified busy states under `Queue`.
    #[expect(dead_code)]
    QueueFollowUp,
}

/// Enter-shaped gestures understood by the composer state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerSubmitChord {
    Enter,
    CtrlEnter,
}

/// The complete result of resolving a submit gesture against composer state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerSubmitAction {
    Submit(SubmitDisposition),
    /// Promote the oldest already-queued message into the active turn.
    SendQueuedNow,
    Noop,
}

/// Detailed tool payload attached to a history cell.
#[derive(Debug, Clone)]
pub struct ToolDetailRecord {
    pub tool_id: String,
    pub tool_name: String,
    pub input: Value,
    pub output: Option<String>,
}

/// Lightweight task view for sidebar rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPanelEntry {
    pub id: String,
    pub status: String,
    pub prompt_summary: String,
    pub duration_ms: Option<u64>,
    pub kind: TaskPanelEntryKind,
    pub stale: bool,
    pub elapsed_since_output_ms: Option<u64>,
    pub owner_agent_id: Option<String>,
    pub owner_agent_name: Option<String>,
    /// #2889: structured current activity for the Work panel.
    pub current_tool: Option<String>,
    pub role: Option<String>,
    pub files_touched: u32,
    /// A finished shell's exit code. `None` while running, and for durable
    /// tasks.
    pub exit_code: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPanelEntryKind {
    Background,
    Shell,
}

impl QueuedMessage {
    pub fn new(display: String, skill_instruction: Option<String>) -> Self {
        Self {
            display,
            skill_instruction,
            skill_provenance: None,
            history_echoed: false,
        }
    }

    #[must_use]
    pub fn with_skill_provenance(
        mut self,
        provenance: Option<crate::skills::SkillProvenance>,
    ) -> Self {
        self.skill_provenance = provenance;
        self
    }

    #[must_use]
    pub(crate) fn is_workflow_draft(&self) -> bool {
        self.skill_instruction
            .as_deref()
            .is_some_and(|instruction| instruction.starts_with(WORKFLOW_DRAFT_INSTRUCTION_PREFIX))
    }

    #[allow(dead_code)] // Tests and queue helpers use the display-only form; send path resolves @mentions.
    pub fn content(&self) -> String {
        if let Some(skill_instruction) = self.skill_instruction.as_ref() {
            format!(
                "{skill_instruction}\n\n---\n\nUser request: {}",
                self.display
            )
        } else {
            self.display.clone()
        }
    }
}

// === Actions ===

/// A typed goal-control request accepted by the TUI and delivered to the
/// engine mailbox. Keeping this separate from transcript text lets the host
/// persist, retry, and reconcile controls without impersonating the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GoalControlIntent {
    SetStatus {
        status: crate::tools::goal::GoalStatus,
        clear: bool,
    },
    SetObjective {
        objective: String,
        token_budget: Option<u32>,
    },
}

/// One accepted goal control waiting for its authoritative GoalUpdated
/// receipt. `dispatched` distinguishes mailbox backpressure from an operation
/// already ordered in the engine channel; both remain pending until receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingGoalControl {
    pub goal_id: Option<String>,
    pub intent: GoalControlIntent,
    pub dispatched: bool,
}

/// Which screen the TUI paints on.
///
/// This is the single source of truth for the alternate screen: `App` stores
/// the mode and derives `use_alt_screen()` from it, so a switch cannot leave
/// the flag and the live terminal disagreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenMode {
    /// Alternate screen buffer. The TUI owns the whole terminal; the host
    /// scrollback is preserved but unreachable until the session exits.
    #[default]
    Fullscreen,
    /// A ratatui inline viewport the full height of the terminal, with no
    /// alternate screen. The shell's scrollback stays intact and scrollable
    /// after exit, at the cost of the TUI no longer owning a private buffer.
    Inline,
}

impl ScreenMode {
    /// Whether this mode runs on the alternate screen buffer.
    #[must_use]
    pub const fn uses_alt_screen(self) -> bool {
        matches!(self, Self::Fullscreen)
    }

    /// Whether mouse capture is on for this screen, given the user's
    /// preference. This is the one rule: startup and the `/fullscreen` ·
    /// `/inline` switch both ask it. Capture needs the alternate screen —
    /// inline mode exists so the terminal owns selection and scrollback.
    #[must_use]
    pub const fn mouse_capture(self, preferred: bool) -> bool {
        self.uses_alt_screen() && preferred
    }

    /// Canonical name, as `/screen` prints it and `parse` accepts it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fullscreen => "fullscreen",
            Self::Inline => "inline",
        }
    }

    /// Parse a user-supplied mode word, including the legacy
    /// `tui.alternate_screen` vocabulary (`auto`/`always` → fullscreen,
    /// `never` → inline) so the existing config key keeps selecting a real
    /// behaviour instead of being parsed and ignored.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fullscreen" | "full" | "alt" | "alt-screen" | "auto" | "always" => {
                Some(Self::Fullscreen)
            }
            "inline" | "scrollback" | "never" | "off" => Some(Self::Inline),
            _ => None,
        }
    }
}

/// Actions emitted by the UI event loop.
#[derive(Debug, Clone, PartialEq)]
pub enum AppAction {
    SetWorkspaceTrust {
        trusted: bool,
        save: bool,
    },
    Quit,
    #[allow(dead_code)] // For explicit /load command
    LoadSession(PathBuf),
    RemoteControl(crate::remote_control::RemoteControlAction),
    SyncSession {
        session_id: Option<String>,
        messages: Vec<Message>,
        system_prompt: Option<SystemPrompt>,
        model: String,
        workspace: PathBuf,
        mode: AppMode,
    },
    OpenConfigView,
    /// Open this workspace's `.codewhale/hooks.toml` in `$EDITOR`, creating
    /// it from a commented template first when it does not exist yet.
    EditProjectHooks,
    /// Open the native git worktree manager.
    OpenWorktreeManager,
    /// Open the `/model` two-pane picker (Pro/Flash + Off/High/Max).
    OpenModelPicker,
    /// Open the `/provider` picker modal — DeepSeek / NVIDIA NIM / OpenRouter
    /// / Novita with inline API-key prompt for un-configured providers (#52).
    OpenProviderPicker,
    /// Open the `/provider` picker in setup/catalog mode, optionally focused on
    /// a built-in provider that needs credentials before first use.
    OpenProviderSetup {
        provider: Option<codewhale_config::ProviderId>,
    },
    /// Open the named, keyless DS4 local-runtime preset for review and save.
    OpenDs4Setup,
    /// Run the xAI/Grok device-code flow with the TUI temporarily suspended.
    StartXaiDeviceLogin,
    /// Run native ChatGPT PKCE sign-in with the TUI temporarily suspended.
    StartChatgptPkceLogin,
    StartChatgptRevoke,
    StartClaudeLogin,
    StartClaudeRevoke,
    /// Run OrcaRouter OAuth 2.0 + PKCE sign-in (loopback redirect) with the TUI
    /// temporarily suspended. Produces a durable `sk-orca-...` key in the
    /// ordinary `orcarouter` credential slot — the same slot the API-key path
    /// writes — so nothing downstream knows which adapter was used.
    StartPluginLogin {
        provider: String,
    },
    StartPluginLogout {
        provider: String,
    },
    StartOrcarouterPkceLogin,
    /// Clear the saved OrcaRouter credential.
    StartOrcarouterRevoke,
    /// Open the `/mode` picker modal for Act / Plan / Operate.
    OpenModePicker,
    /// Switch the live terminal between `/fullscreen` and `/inline`. Handled
    /// where the ratatui `Terminal` lives, because stock ratatui cannot change
    /// an existing terminal's viewport — the switch rebuilds it behind a probe.
    SetScreenMode(ScreenMode),
    /// Refresh the engine prompt after the UI operating mode changes.
    ModeChanged(AppMode),
    /// Synchronize a saved top-level approval policy into the live Config,
    /// then refresh the engine prompt from the App's updated permission mode.
    ApprovalPolicyPersisted {
        policy: Option<String>,
    },
    /// Reload the active user permission rules after `/permissions` safely
    /// removes one from the sibling `permissions.toml`.
    PermissionRulesChanged,
    /// Rebuild the engine's Skill/MCP catalogue from the App's newly replaced
    /// immutable plugin snapshot after trust, enable, revoke, or reload.
    PluginRegistryChanged,
    /// Open the `/statusline` multi-select picker for footer items.
    OpenStatusPicker,
    /// Open the `/feedback` picker for GitHub issue/security destinations.
    OpenFeedbackPicker,
    /// Read/review a scoped immutable issue draft without parking the UI.
    ReviewIssueReport {
        id: String,
        change: Option<String>,
    },
    /// Open the `/theme` picker modal with live preview of every preset.
    OpenThemePicker,
    /// Open the `/skills manage` manager — audit inventory + owned mutations.
    OpenSkillsManager,
    /// Open the `/workflows` run dashboard — live and retained workflow runs.
    OpenWorkflowsManager,
    /// Open the unified, read-only extensions inventory on a specific tab.
    OpenExtensions {
        tab: crate::tui::views::extensions::ExtensionsTab,
    },
    /// Open `/fleet` — the saved named-Fleet list (the primary Fleet surface).
    OpenFleetList,
    /// Open the `/fleet` roster — the saved-party view of the agent team.
    OpenFleetRoster,
    /// Open the selected v2 Fleet editor, or legacy profile setup when no
    /// named Fleet is selected.
    OpenFleetSetup,
    /// `/fleet add`: validate the provider against the live config, write
    /// the member rows, and mark the engine roster stale.
    FleetAddModel {
        provider: String,
        model: String,
        roles: Vec<String>,
    },
    /// `/fleet remove`: drop every member row pinning the route and mark the
    /// engine roster stale.
    FleetRemoveModel {
        provider: String,
        model: String,
    },
    /// Open the `/hotbar` setup wizard.
    OpenHotbarSetup,
    /// Open the constitution-first `/setup` wizard shell.
    OpenSetupWizard,
    /// Open the constitution-first `/setup` wizard at a specific step.
    OpenSetupWizardAt {
        step: codewhale_config::SetupStep,
    },
    /// Record that the bundled/default constitution should be used.
    UseBundledConstitution,
    /// Open the exact effective base-prompt preview for the next turn (#3928).
    ///
    /// Handled where the session config lives, so the preview is built by the
    /// same function the dispatch path uses. Human-only: it issues no provider
    /// request and expands no tool catalog.
    PreviewEffectiveBasePrompt,
    /// Disable the Hotbar: persist `hotbar = []` and clear the live slots.
    DisableHotbar,
    /// Restore the default recommended Hotbar slots: remove the `hotbar` key so
    /// the resolver falls back to the built-in defaults.
    RestoreHotbarDefaults,
    /// Open an external URL in the system browser.
    OpenExternalUrl {
        url: String,
        label: String,
    },
    /// Run an extension command in the extension host (`/name input`). The UI
    /// event loop awaits it, then shows its text and/or submits its prompt
    /// as the user's next message.
    RunExtensionCommand {
        command: crate::extension_host::command::ExtensionCommandRef,
        name: String,
        input: String,
    },
    /// Send a message to the AI (normal chat mode).
    SendMessage(String),
    /// Same-session rollback. A retry is admitted only after the Engine
    /// acknowledges this history and its persisted snapshot is durable.
    ConversationUndo {
        sync: codewhale_command_contract::facets::SessionSyncPayload,
        retry_input: Option<String>,
        /// `retry_input` is the text of a pending `/edit`, already taken from
        /// the composer. If the rollback is refused it must go back there, with
        /// edit mode re-armed, or the user's revision is lost.
        edit_replacement: bool,
    },
    /// Send a built-in Workflow planning turn with separate user-visible text
    /// and bounded runtime guidance. Draft instructions carry a typed marker
    /// that makes the dispatch path expose no tools for that turn.
    WorkflowInstruction {
        display: String,
        instruction: String,
    },
    /// Cancel a running sub-agent through the engine manager.
    CancelSubAgent {
        agent_id: String,
    },
    /// Update the runtime goal status (`/goal pause|resume|clear|…`) without
    /// dispatching a model turn. The UI layer translates this into
    /// `Op::SetGoalStatus`.
    SetGoalStatus {
        status: crate::tools::goal::GoalStatus,
        clear: bool,
    },
    /// Set or replace the goal objective (`/goal <objective>`). The engine
    /// owns the goal and starts the first goal turn itself as runtime
    /// steering; the objective is never sent as a raw user message.
    SetGoalObjective {
        objective: String,
        token_budget: Option<u32>,
    },
    ListSubAgents,
    /// Ask the engine to describe the exact next outbound request
    /// (`/preview-request`, #1004). The engine is the authority: only it can
    /// rebuild the current tool catalog, MCP state, gates, and resolved route.
    PreviewOutboundRequest {
        /// Render the manifest as JSON instead of the human-readable table.
        json: bool,
        /// Render the exact base prompt only. Never includes runtime/system layers.
        base_prompt_only: bool,
        /// Optional text used only to resolve `auto` reasoning/routing. Never
        /// added to the conversation and never sent to a provider.
        hypothetical_prompt: Option<String>,
    },
    /// Show bounded read-only text without copying it into transcript history.
    OpenTextPager {
        title: String,
        content: String,
    },
    /// Show a unified diff in a pager with the transcript's diff rendering,
    /// without copying it into transcript history (`/diff`).
    OpenDiffPager {
        title: String,
        diff: String,
    },
    /// Review a host-generated command; the pager carries its exact token
    /// through explicit confirmation and the normal command dispatcher.
    OpenCommandReview {
        title: String,
        content: String,
        command: String,
    },
    /// Router setup (`/router`, `/model router`, #6525): open the view, test a
    /// preset with one routing call, or save one to `[auto.router]`.
    RouterSetup {
        request: crate::tui::views::router_setup::RouterRequest,
    },
    /// Live remaining-credit lookup for prepaid providers (`/balance`).
    FetchBalance,
    FetchModels,
    /// Force a Models.dev live-catalog refresh into ProviderLake (#4187).
    RefreshModelsDevCatalog,
    CacheWarmup,
    /// Switch the active LLM backend (DeepSeek vs NVIDIA NIM) without
    /// restarting the process. The runtime rebuilds its API client from
    /// the updated config. `model` overrides the post-switch model
    /// (already normalized but not yet provider-prefixed).
    SwitchProvider {
        provider: codewhale_config::ProviderId,
        model: Option<String>,
    },
    /// Switch provider+model through the same apply path as a `/model` route
    /// row. Used by Hotbar route slots so dispatch does not hand-mutate config.
    SwitchModelRoute {
        identity: crate::config::ProviderIdentity,
        model: String,
    },
    UpdateCompaction(CompactionConfig),
    UpdateStreamChunkTimeout(u64),
    UpdateSubagentRuntimeConfig {
        enabled: bool,
        max_subagents: usize,
        launch_concurrency: usize,
        max_spawn_depth: u32,
        api_timeout_secs: u64,
        heartbeat_timeout_secs: u64,
    },
    /// Apply `/config search.provider` to the live Config and engine.
    UpdateSearchProvider {
        provider: crate::config::SearchProvider,
    },
    /// Apply `/config prompt_suggestion` to the live Config.
    UpdatePromptSuggestion {
        enabled: bool,
    },
    /// Apply one `/config notifications` scalar to the live Config.
    UpdateNotification {
        update: crate::config::NotificationConfigUpdate,
    },
    /// Enable or disable the background advisor watcher for this session (#3982).
    SetAdvisorEnabled {
        enabled: bool,
    },
    /// Open the live transcript overlay through a terminal-safe command path.
    OpenLiveTranscript,
    /// Open the whole-turn inspector (Ctrl+Alt+O, /turn inspect).
    OpenTurnInspector,
    OpenContextInspector,
    CompactContext {
        /// Optional user focus from `/compact <focus>`, forwarded into the
        /// successor-brief summary prompt.
        focus: Option<String>,
    },
    PurgeContext,
    TaskAdd {
        prompt: String,
    },
    TaskList,
    TaskShow {
        id: String,
    },
    TaskCancel {
        id: String,
    },
    Automation(AutomationAction),
    ShellJob(ShellJobAction),
    Mcp(McpUiAction),
    /// Switch to a different config profile without restarting.
    SwitchProfile {
        /// Profile name to load.
        profile: String,
    },
    /// Switch the workspace used by tools, hooks, tasks, and session metadata.
    SwitchWorkspace {
        workspace: PathBuf,
    },
    /// Record from the microphone and route the transcription into the
    /// composer (or auto-send it). Emitted by `/voice` and the voice hotbar
    /// action; handled in the UI event loop where the live `Config` supplies
    /// provider credentials.
    VoiceCapture,
    /// Upload an already-rendered, redacted session page as a secret gist.
    /// Emitted only by `/share confirm`.
    ShareSession {
        html: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationAction {
    /// Open the automations room, optionally focused on one id.
    Open {
        focus: Option<String>,
    },
    List,
    Show(String),
    Pause(String),
    Resume(String),
    Delete {
        id: String,
        confirmation: Option<String>,
    },
    Run(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellJobAction {
    List,
    Show {
        id: String,
    },
    Poll {
        id: String,
        wait: bool,
    },
    SendStdin {
        id: String,
        input: String,
        close: bool,
    },
    Cancel {
        id: String,
    },
    CancelAll,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpUiAction {
    Show,
    Init {
        force: bool,
    },
    AddStdio {
        name: String,
        command: String,
        args: Vec<String>,
    },
    AddHttp {
        name: String,
        url: String,
        transport: Option<String>,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    Remove {
        name: String,
    },
    Login {
        name: String,
        scopes: Vec<String>,
    },
    Logout {
        name: String,
    },
    /// Retry one failed/timed-out server through the engine-owned live pool.
    Retry {
        name: String,
    },
    /// List consent-gated external MCP import candidates with provenance.
    ImportList,
    /// Approve importing one discovered external server into user mcp.json.
    ImportApprove {
        name: String,
    },
    /// Decline an external candidate (durable until source content changes).
    ImportDecline {
        name: String,
    },
    Validate,
    /// Report this server's last observed state without starting a new pool.
    Diagnose {
        name: String,
    },
    Reload,
}

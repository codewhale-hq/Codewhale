//! Coherent shell grammar for the underwater TUI.
//!
//! This module owns phase, responsive density, the empty-state composition,
//! and the compact header/footer fact budget. Product data still belongs to
//! [`App`]; this is only its terminal projection. Keeping these decisions in
//! one place prevents the default UI from drifting back into a header +
//! sidebar + dashboard + footer composition with four owners for one fact.

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use crate::tui::ui_text::{semantic_truncate, text_display_width};
use crate::tui::{
    app::{App, OnboardingState},
    ocean::COMPLETION_BREATH_MS,
    views::ModalKind,
};
use codewhale_config::AppMode;
use codewhale_execpolicy::ApprovalMode;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette::ChromeInk;

/// Responsive density tier. It changes how much truth is shown, never the
/// underlying state grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellTier {
    Compact,
    Normal,
    Wide,
}

/// What one launch key produces. The composer holds focus and takes every
/// ordinary key, so the only launch-owned input is F1 help; the card's
/// rows are driven by Up/Down + Enter (and the mouse) through
/// [`run_launch_card_row`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchAction {
    None,
    /// The prominent new-session entry: begin a fresh session in the
    /// current workspace.
    NewSession,
    ReturnToSession,
    /// Resume one recent-work row by session id.
    ResumeSession(String),
    /// The see-all overflow: open the full session picker.
    BrowseSessions,
    /// Inspect and manage the servers counted by the MCP summary.
    McpManager,
    /// The MCP problems row: type the remedy it prints into the composer
    /// (`/mcp login <name>` or `/mcp`). Typing beats copying — it works over
    /// SSH where a clipboard may not exist, and the user sees the command
    /// before Enter sends it (#6085).
    McpRemedy,
    Help,
}

/// Translate a launch key into one product action. Reached only through
/// [`LaunchComposerKey::MenuChord`]; every other key belongs to the
/// composer authority.
pub fn handle_launch_key(
    _launch: &mut crate::tui::app::LaunchState,
    key: KeyEvent,
    _locale: Locale,
) -> LaunchAction {
    match key.code {
        KeyCode::F(1) => LaunchAction::Help,
        _ => LaunchAction::None,
    }
}

/// One interactive row on the startup card: the prominent new-session
/// entry, one recent-work row, or the see-all overflow. Labels are
/// localized; `detail` is right-aligned metadata (a recent row's age).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCardRow {
    pub id: crate::tui::app::LaunchRowId,
    pub label: String,
    pub detail: String,
    /// The new-session entry paints prominent (bold accent) when it is
    /// neither keyboard-selected nor hovered.
    pub prominent: bool,
}

/// A recent session projected for the card: the display title plus its
/// right-aligned detail line. Preformatted by the caller so the renderer
/// stays deterministic for golden buffers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRecentEntry {
    pub id: String,
    pub title: String,
    pub detail: String,
}

/// The card's rows in paint/click/keyboard order: the prominent
/// new-session entry first, then recent work, then the see-all overflow
/// when more sessions sit behind the inline list. The single ordering
/// keyboard, mouse, and paint share.
#[must_use]
pub fn launch_card_rows(
    locale: Locale,
    recent: &[LaunchRecentEntry],
    has_more: bool,
) -> Vec<LaunchCardRow> {
    let mut rows = Vec::with_capacity(recent.len() + 2);
    rows.push(LaunchCardRow {
        id: crate::tui::app::LaunchRowId::NewSession,
        label: tr(locale, MessageId::LaunchNewSession).into_owned(),
        detail: String::new(),
        prominent: true,
    });
    rows.extend(recent.iter().map(|entry| LaunchCardRow {
        id: crate::tui::app::LaunchRowId::Recent(entry.id.clone()),
        label: entry.title.clone(),
        detail: entry.detail.clone(),
        prominent: false,
    }));
    if has_more {
        rows.push(LaunchCardRow {
            id: crate::tui::app::LaunchRowId::SeeAll,
            label: tr(locale, MessageId::LaunchSeeAllSessions).into_owned(),
            detail: String::new(),
            prominent: false,
        });
    }
    rows
}

/// Project the launch state's loaded recent-work list into card entries:
/// display titles with right-aligned relative ages, like the resume
/// picker. Pure projection of loaded state — no disk reads.
fn launch_recent_entries(app: &App) -> (Vec<LaunchRecentEntry>, bool) {
    let recent = app
        .launch
        .recent
        .iter()
        .map(|session| {
            let raw = crate::session_manager::extract_title(&session.title);
            let title = if raw == "Session" || raw.trim().is_empty() {
                crate::session_manager::truncate_id(&session.id).to_string()
            } else {
                raw.to_string()
            };
            let age = crate::tui::session_picker::format_relative_time(
                &session.updated_at,
                app.ui_locale,
            );
            LaunchRecentEntry {
                id: session.id.clone(),
                title,
                detail: if session.interrupted {
                    format!("interrupted · {age}")
                } else {
                    age
                },
            }
        })
        .collect::<Vec<_>>();
    // More sessions than the inline cap, or sessions the card's filter
    // dropped that `/resume` still lists (empty auto-created shells):
    // either way the see-all row is how the truth stays reachable.
    let has_more = app.launch.total_workspace_sessions > recent.len()
        || (recent.is_empty() && app.launch.has_scoped_sessions);
    (recent, has_more)
}

/// Both painting and input use the same primary action on revisited home.
fn home_card_rows(app: &App, recent: &[LaunchRecentEntry], has_more: bool) -> Vec<LaunchCardRow> {
    let mut rows = launch_card_rows(app.ui_locale, recent, has_more);
    if app.launch.return_to_session {
        rows[0].id = crate::tui::app::LaunchRowId::ReturnToSession;
        rows[0].label = format!(
            "{}  Esc",
            tr(app.ui_locale, MessageId::HomeBackToConversation)
        );
    }
    rows
}

/// The card's rows for live `App` state, for keyboard navigation and Enter.
///
/// The painted rows are the authority. Paint sheds the tail of the recent
/// list to fit a short pane and turns the overflow row on when it does, so a
/// list built here from scratch would let Up/Down land on — and Enter resume
/// — a session the screen is not showing. `row_hitboxes` is what the last
/// frame actually drew, in paint order, and `mouse_ui` indexes that same
/// list: one ordering for paint, mouse, and keyboard, with no second state.
#[must_use]
pub fn launch_rows_for_app(app: &App) -> Vec<LaunchCardRow> {
    let (recent, _) = launch_recent_entries(app);
    // An empty pane has no navigable rows, including before its first paint.
    // A preserved multiline draft can legitimately leave no room for home.
    let mut superset = home_card_rows(app, &recent, true);
    // MCP rows join the same ordering only when the boot block painted them.
    for id in [
        crate::tui::app::LaunchRowId::McpManager,
        crate::tui::app::LaunchRowId::McpRemedy,
    ] {
        superset.push(LaunchCardRow {
            id,
            label: String::new(),
            detail: String::new(),
            prominent: false,
        });
    }
    app.launch
        .row_hitboxes
        .iter()
        .filter_map(|(id, _)| superset.iter().find(|row| &row.id == id).cloned())
        .collect()
}

/// Re-anchor the card's clickable rows on what `area` just painted.
///
/// The frame renderer calls this instead of rebuilding hitboxes inline, so
/// the row list keyboard and mouse read back cannot describe a row the
/// transcript did not draw.
pub fn refresh_launch_row_hitboxes(app: &mut App, area: Rect) {
    let state = launch_empty_state(app, area);
    app.launch.row_hitboxes = state
        .rows
        .into_iter()
        .filter_map(|(id, row)| {
            let y = area.y.checked_add(u16::try_from(row).ok()?)?;
            (y < area.y.saturating_add(area.height)).then_some((
                id,
                Rect::new(area.x + state.text_column.x, y, state.text_column.width, 1),
            ))
        })
        .collect();
    // A pane that shrank can leave the highlight past the last painted row.
    // Clear it rather than clamping: clamping would silently move the
    // selection onto a different session.
    let painted = app.launch.row_hitboxes.len();
    if app
        .launch
        .menu_selected
        .is_some_and(|index| index >= painted)
    {
        app.launch.menu_selected = None;
    }
    if app.launch.hovered_row.is_some_and(|index| index >= painted) {
        app.launch.hovered_row = None;
    }
}

/// The click twin of [`run_launch_card_row`]: one card row id runs the
/// same action the keyboard's Enter runs, so mouse and keyboard share one
/// contract.
#[must_use]
pub fn launch_row_click_action(id: &crate::tui::app::LaunchRowId) -> LaunchAction {
    match id {
        crate::tui::app::LaunchRowId::NewSession => LaunchAction::NewSession,
        crate::tui::app::LaunchRowId::ReturnToSession => LaunchAction::ReturnToSession,
        crate::tui::app::LaunchRowId::Recent(session_id) => {
            LaunchAction::ResumeSession(session_id.clone())
        }
        crate::tui::app::LaunchRowId::SeeAll => LaunchAction::BrowseSessions,
        crate::tui::app::LaunchRowId::McpManager => LaunchAction::McpManager,
        crate::tui::app::LaunchRowId::McpRemedy => LaunchAction::McpRemedy,
    }
}

/// Ask before resuming: open the confirmation popup for `session_id`.
///
/// Both the card's Enter and a click on a recent row route here. Resuming
/// replaces the whole session context, and the popup is where that is said —
/// an arming line over the composer read as chrome rather than as a question.
pub fn open_launch_resume_confirm(app: &mut App, session_id: &str) {
    if app.view_stack.top_kind() == Some(crate::tui::views::ModalKind::LaunchResumeConfirm) {
        return;
    }
    let entry = app
        .launch
        .recent
        .iter()
        .find(|entry| entry.id == session_id);
    let title = entry
        .map(|entry| entry.title.clone())
        .unwrap_or_else(|| session_id.to_string());
    let detail = entry
        .map(|entry| {
            let when =
                crate::tui::session_picker::format_relative_time(&entry.updated_at, app.ui_locale);
            format!(
                "{when} · {}",
                crate::tui::session_picker::format_message_count(
                    entry.message_count,
                    app.ui_locale
                )
            )
        })
        .unwrap_or_default();
    app.view_stack.push(
        crate::tui::launch_resume_confirm::LaunchResumeConfirmView::new(
            session_id.to_string(),
            title,
            detail,
            app.ui_locale,
        ),
    );
    app.needs_redraw = true;
}
/// Run the card's highlighted row. Enter on the card is the list's runner;
/// an untouched list runs nothing.
pub fn run_launch_card_row(rows: &[LaunchCardRow], menu_selected: Option<usize>) -> LaunchAction {
    let Some(selected) = menu_selected else {
        return LaunchAction::None;
    };
    match rows.get(selected) {
        None => LaunchAction::None,
        Some(row) => launch_row_click_action(&row.id),
    }
}

/// What the pre-session composer layer decided about one key.
///
/// This is only an admission guard, never an input implementation: the
/// startup composer is the session's own [`crate::tui::app::ComposerState`],
/// and every editing key is answered by the conversation composer match in
/// the event loop — the single composer input authority — exactly as it
/// would be in a live session. Word motion, selection, completion menus,
/// attachments, history, paste bursts, and vim behaviour therefore cannot
/// drift from the shell. Only three things are launch-specific here: an
/// empty Enter, F1 help, and submitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchComposerKey {
    /// The key is fully consumed and does nothing more (Enter on an empty
    /// composer with no menu entry highlighted: there is no row to run and
    /// nothing to send; Esc clearing the menu highlight or bringing the
    /// card back).
    Consumed,
    /// Submit the composed message through the normal dispatch path.
    Submit,
    /// A completion-menu selection was applied (a slash or mention popup was
    /// open and Enter picked the highlighted entry); the key is consumed
    /// without submitting — the completed text stays in the composer.
    MenuSelect,
    /// The launch chord (F1 help): the same key is then handed to
    /// [`handle_launch_key`]. It deliberately wins over its composer
    /// meaning while the launch screen is up.
    MenuChord,
    /// Not launch-specific: the conversation composer match below owns the
    /// key. The event loop must not run [`handle_launch_key`] for it.
    ComposerAuthority,
    /// Move the launch card's row selection (Up/Down while the card is up).
    MenuNavigate(i32),
    /// Run the card's highlighted row. Revisited home retains its draft;
    /// on startup, only an empty composer yields Enter to the card.
    MenuRun,
}

/// Admit one key for the pre-session composer.
///
/// Editing keys are never handled here — they fall through to the
/// conversation composer match so there is exactly one composer input
/// system. Only F1 help stays launch-owned via
/// [`LaunchComposerKey::MenuChord`].
pub fn handle_launch_composer_key(app: &mut App, key: KeyEvent) -> LaunchComposerKey {
    if app.launch.return_to_session && key.code == KeyCode::Esc {
        app.launch.dismiss();
        return LaunchComposerKey::Consumed;
    }
    let multiline = app.composer_multiline_mode;
    let card_up = app.launch.dissolve_started_ms.is_none();
    match key.code {
        KeyCode::Enter
            if crate::tui::composer_ui::composer_submit_chord(key, multiline).is_some() =>
        {
            if key.modifiers != KeyModifiers::NONE
                && let Some(pending) = app.flush_paste_burst_before_modified_input_if_enabled()
            {
                app.insert_str(&pending);
            }
            // Explicit home navigation takes precedence over a preserved draft.
            // Only painted rows may own Enter, just as with mouse activation.
            if app.launch.return_to_session
                && card_up
                && app
                    .launch
                    .menu_selected
                    .is_some_and(|index| index < app.launch.row_hitboxes.len())
            {
                return LaunchComposerKey::MenuRun;
            }
            // #573 parity with the session composer's Enter arm: when a
            // completion popup is matching (e.g. `/mo` → `/model`), Enter
            // applies the highlighted entry instead of sending the literal
            // prefix. A mention completion amends the composed text and is
            // consumed; a slash completion completes the command and falls
            // through to Submit so the launch dispatch path executes it.
            let mention_entries = crate::tui::file_mention::visible_mention_menu_entries(app, 1);
            if !mention_entries.is_empty()
                && crate::tui::file_mention::apply_mention_menu_selection(app, &mention_entries)
            {
                return LaunchComposerKey::MenuSelect;
            }
            let slash_entries = crate::tui::slash_menu::visible_slash_menu_entries(app, 1);
            if !slash_entries.is_empty() {
                crate::tui::slash_menu::apply_slash_menu_selection(app, &slash_entries, false);
                app.close_slash_menu();
            }
            if app.input.trim().is_empty() {
                if card_up && app.launch.menu_selected.is_some() {
                    // The card owns Enter only once the user has arrowed
                    // onto a row; an untouched list runs nothing.
                    return LaunchComposerKey::MenuRun;
                }
                LaunchComposerKey::Consumed
            } else {
                app.launch.dissolve_card(app.ambient_clock_ms);
                LaunchComposerKey::Submit
            }
        }
        KeyCode::Up if card_up => LaunchComposerKey::MenuNavigate(-1),
        KeyCode::Down if card_up => LaunchComposerKey::MenuNavigate(1),
        // Esc walks back one step: a highlighted row is unhighlighted;
        // an empty composer with the card gone brings the card back. A draft
        // in the composer keeps Esc's composer meaning.
        KeyCode::Esc if card_up && app.launch.menu_selected.is_some() => {
            app.launch.menu_selected = None;
            LaunchComposerKey::Consumed
        }
        KeyCode::Esc if !card_up && app.input.is_empty() => {
            app.launch.restore_card();
            LaunchComposerKey::Consumed
        }
        KeyCode::F(1) => LaunchComposerKey::MenuChord,
        // Every other key — text, caret motion, word motion, selection,
        // newline chords, Home/End, kill/chord editing, vim motions, Esc,
        // Tab, history — is answered by the conversation composer authority.
        _ => {
            // Typing goes straight to the composer, and the first keystroke
            // dissolves the card (founder decision, 2026-09-02).
            if card_up
                && matches!(key.code, KeyCode::Char(_))
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
            {
                if app.launch.return_to_session {
                    app.launch.dismiss();
                } else {
                    app.launch.dissolve_card(app.ambient_clock_ms);
                }
            }
            LaunchComposerKey::ComposerAuthority
        }
    }
}

impl ShellTier {
    // `for_area` (the two-dimensional variant) went with the empty state's
    // tier branch: the idle caption sheds detail continuously now, so nothing
    // was left that wanted a coarse three-way answer about a whole Rect. The
    // row and column floors it encoded still exist, spelled out as
    // `AMBIENT_MIN_CHAT_HEIGHT` / `AMBIENT_MIN_CHAT_WIDTH` where the layout
    // can honour them.
    #[must_use]
    pub fn for_chrome_width(width: u16) -> Self {
        if width < 60 {
            Self::Compact
        } else if width < 110 {
            Self::Normal
        } else {
            Self::Wide
        }
    }
}

/// Perceptual session phase. Every treatment reads from this same enum so a
/// footer cannot say `idle` while the transcript is asking for approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPhase {
    Idle,
    Typing,
    Working,
    /// A live verification pass (tests/checks/lints). Same clock family as
    /// `Working` but rendered as the metered braille tick — checking, not
    /// searching (ocean state model).
    Verifying,
    Waiting,
    Approval,
    Done,
    Failed,
}

/// The one truthful verb shown while a turn is live. This deliberately stays
/// smaller than the tool taxonomy: the phase strip only needs to distinguish
/// hidden reasoning, read-shaped exploration, other tool use, verification,
/// and generic model work. It never exposes reasoning text or tool arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveActivityKind {
    Working,
    Compacting,
    AutoCompacting,
    Reasoning,
    Reading,
    UsingTool,
    UsingSubagents,
    Verifying,
}

/// Bounded projection of live turn activity. Completed entries are ignored,
/// so an `ActiveCell` retained until `TurnComplete` cannot keep the shell in a
/// false working state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveActivity {
    kind: LiveActivityKind,
    running_tools: usize,
}

impl LiveActivity {
    #[must_use]
    pub(crate) fn from_app(app: &App) -> Self {
        let tools = running_tool_facts(app);
        let kind = if app
            .active_compaction
            .as_ref()
            .is_some_and(|compaction| compaction.auto)
        {
            LiveActivityKind::AutoCompacting
        } else if app.active_compaction.is_some() {
            LiveActivityKind::Compacting
        } else if tools.verifying {
            LiveActivityKind::Verifying
        } else if app_has_unfinished_subagents(app) {
            LiveActivityKind::UsingSubagents
        } else if tools.count > 0 && tools.all_reading {
            LiveActivityKind::Reading
        } else if tools.count > 0 {
            LiveActivityKind::UsingTool
        } else if app.streaming_thinking_active_entry.is_some() {
            LiveActivityKind::Reasoning
        } else {
            LiveActivityKind::Working
        };
        Self {
            kind,
            running_tools: tools.count,
        }
    }

    #[must_use]
    pub(crate) fn kind(self) -> LiveActivityKind {
        self.kind
    }

    #[must_use]
    fn is_explicit(self) -> bool {
        !matches!(self.kind, LiveActivityKind::Working)
    }

    #[must_use]
    fn label(self, locale: Locale) -> Cow<'static, str> {
        match self.kind {
            LiveActivityKind::Working => tr(locale, MessageId::PhaseWorking),
            LiveActivityKind::Compacting => tr(locale, MessageId::ContextManualCompacting),
            LiveActivityKind::AutoCompacting => tr(locale, MessageId::ContextAutoCompacting),
            LiveActivityKind::Reasoning => tr(locale, MessageId::PhaseReasoning),
            LiveActivityKind::Reading => tr(locale, MessageId::PhaseReading),
            LiveActivityKind::UsingTool => tr(locale, MessageId::PhaseUsingTool),
            LiveActivityKind::UsingSubagents => tr(locale, MessageId::PhaseSubagents),
            LiveActivityKind::Verifying => tr(locale, MessageId::PhaseVerifying),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RunningToolFacts {
    count: usize,
    all_reading: bool,
    verifying: bool,
}

/// True when any sub-agent spawned by this session is still running: live
/// progress rows win over the cache, whose Running entries are the persisted
/// view of the same actors.
fn app_has_unfinished_subagents(app: &App) -> bool {
    !app.agent_progress.is_empty()
        || app.subagent_cache.iter().any(|agent| {
            matches!(
                agent.status,
                crate::tools::subagent::SubAgentStatus::Running
            )
        })
}

impl Default for RunningToolFacts {
    fn default() -> Self {
        Self {
            count: 0,
            all_reading: true,
            verifying: false,
        }
    }
}

impl RunningToolFacts {
    fn observe(&mut self, reading: bool, verifying: bool) {
        self.count = self.count.saturating_add(1);
        self.all_reading &= reading;
        self.verifying |= verifying;
    }
}

const WORKING_BUBBLE_FRAMES: [&str; 8] = ["⠀", "⢀", "⣀", "⣄", "⣤", "⣦", "⣶", "⣿"];
const COMPLETION_RELEASE_MS: u128 = 560;
// The idle whale portrait rows (IDLE_WHALE_ROWS / UWU_IDLE_WHALE_ROWS) and
// their caustic shimmer were deleted per the 2026-08-29 founder directive:
// hand-drawn whale art is out; the only sanctioned terminal mark is the one
// generated from the brand master path. The ambient empty-state surface
// (wordmark, context caption, prompt) below is not whale art and stays.

impl ShellPhase {
    #[must_use]
    pub fn from_app(app: &App) -> Self {
        Self::from_app_with_activity(app, LiveActivity::from_app(app))
    }

    #[must_use]
    pub(crate) fn from_app_with_activity(app: &App, activity: LiveActivity) -> Self {
        if matches!(
            app.view_stack.top_kind(),
            Some(ModalKind::Approval | ModalKind::Elevation | ModalKind::UserInput)
        ) {
            return Self::Approval;
        }
        if matches!(
            activity.kind(),
            LiveActivityKind::Compacting | LiveActivityKind::AutoCompacting
        ) {
            // A typed CompactionStarted event is newer and more specific than
            // a prior turn's failed projection. Keep the recovery operation
            // visible until its matching terminal event arrives.
            return Self::Working;
        }
        if app.turn_error_posted
            || matches!(app.runtime_turn_status.as_deref(), Some("failed" | "error"))
        {
            return Self::Failed;
        }
        // A child agent's unanswered approval or question is the person's
        // move, even while other agents keep working: the footer says
        // "waiting on you", not "agents underway" (#6565).
        if app.pending_user_input_prompt.is_some()
            || !app.pending_child_requests.is_empty()
            || app
                .task_panel
                .iter()
                .any(|task| matches!(task.status.as_str(), "waiting" | "needs_user"))
        {
            return Self::Waiting;
        }
        if app.is_loading
            || matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
            || activity.is_explicit()
        {
            if activity.kind() == LiveActivityKind::Verifying {
                return Self::Verifying;
            }
            return Self::Working;
        }
        if !app.input.is_empty() {
            return Self::Typing;
        }
        if matches!(app.runtime_turn_status.as_deref(), Some("completed")) {
            return Self::Done;
        }
        Self::Idle
    }

    #[must_use]
    pub fn label(self, locale: Locale) -> Cow<'static, str> {
        match self {
            Self::Idle => tr(locale, MessageId::PhaseIdle),
            Self::Typing => tr(locale, MessageId::PhaseDraft),
            Self::Working => tr(locale, MessageId::PhaseWorking),
            Self::Verifying => tr(locale, MessageId::PhaseVerifying),
            Self::Waiting | Self::Approval => tr(locale, MessageId::PhaseWaitingOnYou),
            Self::Done => tr(locale, MessageId::PhaseDone),
            Self::Failed => tr(locale, MessageId::PhaseFailed),
        }
    }
}

/// Exhaustive on purpose: a new [`AppMode`] must be handed a Policy ink
/// deliberately rather than inheriting act's by falling through a wildcard.
pub(crate) fn header_mode_ink(mode: AppMode) -> ChromeInk {
    match mode {
        AppMode::Plan => ChromeInk::PolicyPlan,
        AppMode::Operate => ChromeInk::PolicyOperate,
        AppMode::Agent => ChromeInk::PolicyAct,
    }
}

pub(crate) fn header_permission_ink(mode: ApprovalMode) -> ChromeInk {
    match mode {
        ApprovalMode::Suggest | ApprovalMode::Never => ChromeInk::PermissionAsk,
        ApprovalMode::Auto => ChromeInk::PermissionAutoReview,
        ApprovalMode::Bypass => ChromeInk::PermissionFullAccess,
    }
}

/// One posture word with its ink — the unit the classic header's lockup was
/// made of, now carried as merged-footer chips.
pub(crate) type PostureChip = (Cow<'static, str>, ChromeInk);

/// The posture lockup as two standalone chips for the Tideline merged
/// footer (spec §3: the old header's mode/permission chips move into the
/// footer activity segment). Same words, same inks, and the same mapping
/// the classic header used — [`header_mode_ink`] for the mode word,
/// [`header_permission_ink`] for the permission phrase. The filesystem
/// scope notice, when it deviates, folds into the permission chip's text
/// (the header already painted it in the permission ink).
pub(crate) fn posture_chips(app: &App) -> (Option<PostureChip>, Option<PostureChip>) {
    let mode = (
        mode_label(app.ui_locale, app.mode),
        header_mode_ink(app.mode),
    );
    let mut permission = (
        permission_label(app),
        header_permission_ink(app.approval_mode),
    );
    if let Some(scope) = filesystem_scope_notice(app) {
        permission.0 = format!("{} · {scope}", permission.0).into();
    }
    (Some(mode), Some(permission))
}

/// Summarize only tools whose lifecycle is actually `Running`. A read label
/// is earned only when every running entry is read/exploration-shaped; mixed
/// work stays the neutral `using tool`. Verification wins because it is the
/// existing stronger promise made by the phase strip.
fn running_tool_facts(app: &App) -> RunningToolFacts {
    use crate::tui::history::{HistoryCell, ToolCell, ToolStatus};
    use crate::tui::widgets::tool_card::{ToolFamily, tool_family_for_name};

    let mut facts = RunningToolFacts::default();
    let Some(active) = app.active_cell.as_ref() else {
        return facts;
    };
    for cell in active.entries() {
        let HistoryCell::Tool(tool) = cell else {
            continue;
        };
        match tool {
            ToolCell::Exec(exec) if exec.status == ToolStatus::Running => {
                facts.observe(false, exec_is_verification(&exec.command));
            }
            ToolCell::Generic(generic) if generic.status == ToolStatus::Running => {
                let family = tool_family_for_name(&generic.name);
                facts.observe(
                    matches!(family, ToolFamily::Read | ToolFamily::Find),
                    family == ToolFamily::Verify || generic.name == "read_lints",
                );
            }
            ToolCell::Exploring(exploring) => {
                for entry in &exploring.entries {
                    if entry.status == ToolStatus::Running {
                        facts.observe(true, false);
                    }
                }
            }
            ToolCell::WebSearch(search) if search.status == ToolStatus::Running => {
                facts.observe(true, false);
            }
            other if other.status() == Some(ToolStatus::Running) => {
                facts.observe(false, false);
            }
            _ => {}
        }
    }
    facts
}

fn exec_is_verification(command: &str) -> bool {
    let trimmed = command.trim_start();
    let mut tokens = trimmed.split_whitespace();
    let first = tokens.next().unwrap_or("");
    let second = tokens.next().unwrap_or("");
    match first {
        "cargo" => matches!(second, "test" | "check" | "clippy" | "nextest"),
        "go" => matches!(second, "test" | "vet"),
        "npm" | "pnpm" | "yarn" | "bun" => matches!(second, "test" | "lint" | "check"),
        "make" => matches!(second, "test" | "check" | "lint"),
        "python" | "python3" => trimmed.contains("-m pytest") || trimmed.contains("-m unittest"),
        "pytest" | "jest" | "vitest" | "tsc" | "eslint" | "ruff" | "mypy" | "clippy-driver"
        | "golangci-lint" | "shellcheck" => true,
        _ => false,
    }
}

fn completion_elapsed_ms(app: &App) -> Option<u128> {
    if !app.motion_policy().allows_decorative() {
        return None;
    }
    app.ocean_completion_started_at
        .map(|started| started.elapsed().as_millis())
        .filter(|elapsed| *elapsed < COMPLETION_BREATH_MS)
}

/// Truthful window-title activity verb for the OSC-0 whale animation.
///
/// Uses short English fragments (with fixed-width ellipsis) so alt-tabbed
/// sessions stay legible without depending on the full localized phase strip.
#[must_use]
pub(crate) fn title_activity_verb(app: &App) -> &'static str {
    let activity = LiveActivity::from_app(app);
    let phase = ShellPhase::from_app_with_activity(app, activity);
    match phase {
        ShellPhase::Waiting | ShellPhase::Approval => "waiting on you…",
        ShellPhase::Verifying => "verifying…",
        ShellPhase::Done => "done",
        ShellPhase::Failed => "failed",
        ShellPhase::Typing => "drafting…",
        ShellPhase::Idle => "idle",
        ShellPhase::Working => match activity.kind() {
            LiveActivityKind::Compacting | LiveActivityKind::AutoCompacting => {
                "compacting context…"
            }
            LiveActivityKind::Reasoning => "reasoning…",
            LiveActivityKind::Reading => "reading…",
            LiveActivityKind::UsingTool => "using tool…",
            LiveActivityKind::UsingSubagents => "fleet underway…",
            LiveActivityKind::Verifying => "verifying…",
            LiveActivityKind::Working => "working…",
        },
    }
}

/// Push the current shell phase into the terminal title whale animation.
pub(crate) fn sync_title_activity(app: &App) {
    crate::tui::notifications::set_title_motion_enabled(
        app.motion_policy().allows_decorative() && app.status_indicator != "off",
    );
    // Keep the `[title] …` window-title prefix in step with the session and
    // config defaults; change detection inside makes this free when nothing
    // moved.
    crate::tui::notifications::set_title_prefix(app.window_title_prefix());
    if app.is_loading
        || matches!(
            ShellPhase::from_app(app),
            ShellPhase::Working
                | ShellPhase::Verifying
                | ShellPhase::Waiting
                | ShellPhase::Approval
                | ShellPhase::Typing
        )
    {
        crate::tui::notifications::set_title_activity_verb(title_activity_verb(app));
    }
}

pub(crate) fn phase_marker_with_activity(
    app: &App,
    phase: ShellPhase,
    activity: LiveActivity,
) -> (&'static str, Cow<'static, str>) {
    let locale = app.ui_locale;
    match phase {
        ShellPhase::Idle => ("·", phase.label(locale)),
        ShellPhase::Typing => ("›", phase.label(locale)),
        ShellPhase::Working => {
            // The footer and the live tool card share one wall-clock cadence,
            // so the two primary liveness marks never look like unrelated
            // spinners. The shared helper also preserves the 400ms
            // "motion is earned" delay and reduced/still fallback.
            let policy = app.motion_policy();
            let animated = crate::tui::spinner::braille_spinner_frame(app.turn_started_at, false);
            let earned = app.turn_started_at.is_none_or(|started| {
                started.elapsed().as_millis()
                    >= u128::from(crate::tui::spinner::LIVE_MARKER_DELAY_MS)
            });
            let frame = policy.spinner_glyph(animated, earned);
            (frame, activity.label(locale))
        }
        ShellPhase::Verifying => {
            // Metered braille tick on the shared live clock — checking, not
            // searching. Reduced motion holds the legible mid frame.
            let policy = app.motion_policy();
            let animated = crate::tui::spinner::verification_tick_frame(app.turn_started_at, false);
            let earned = app.turn_started_at.is_none_or(|started| {
                started.elapsed().as_millis()
                    >= u128::from(crate::tui::spinner::LIVE_MARKER_DELAY_MS)
            });
            let frame = policy.spinner_glyph(animated, earned);
            (frame, phase.label(locale))
        }
        ShellPhase::Waiting | ShellPhase::Approval => ("◆", phase.label(locale)),
        ShellPhase::Done => match completion_elapsed_ms(app) {
            Some(elapsed) if elapsed < COMPLETION_RELEASE_MS => {
                let index = ((elapsed / 140) as usize + 4).min(WORKING_BUBBLE_FRAMES.len() - 1);
                (WORKING_BUBBLE_FRAMES[index], phase.label(locale))
            }
            _ => (crate::tui::glyphs::DONE, phase.label(locale)),
        },
        ShellPhase::Failed => (crate::tui::glyphs::FAILED, phase.label(locale)),
    }
}

pub(crate) fn mode_label(locale: Locale, mode: AppMode) -> Cow<'static, str> {
    match mode {
        AppMode::Agent => tr(locale, MessageId::ChipModeAct),
        AppMode::Plan => tr(locale, MessageId::ChipModePlan),
        AppMode::Operate => tr(locale, MessageId::ChipModeOperate),
    }
}

/// Permission chip words. This maps from the typed [`ApprovalMode`] state —
/// never from the English `permission_chip_label()` strings — so localizing
/// (or rewording) the upstream chip labels can never silently break the chip.
///
/// Tool-approval posture only. Filesystem scope is a separate fact and only
/// earns header columns when it is worth reading — see
/// [`filesystem_scope_notice`].
pub(crate) fn permission_label(app: &App) -> Cow<'static, str> {
    crate::core::authority::permission_label_for(app.ui_locale, app.mode, app.approval_mode)
}

/// The effective filesystem scope — but only when it says something the
/// permission word beside it does not already say.
///
/// This chip exists because "Full Access" (tool approval) was being read as
/// unrestricted disk writes (user report, 2026-07-23), and because a policy
/// with no enforcement backend used to name a boundary nobody applied
/// (2026-08-04 audit). Both of those are deviations. The default — an
/// enforced workspace-write boundary — is what every ordinary session already
/// has, and printing `files: workspace` on every frame of every session spent
/// seventeen columns of the primary chrome saying so. A notice that is always
/// on cannot signal anything; folding the expected case away is what lets
/// `files: workspace (unenforced)` and the Full-Access-but-confined case land
/// as warnings when they do appear.
///
/// `read-only` under Plan is dropped for the same reason from the other side:
/// the permission word there is already the literal phrase "read only".
#[must_use]
fn filesystem_scope_notice(app: &App) -> Option<Cow<'static, str>> {
    // Spelled out because the old `fs:` prefix read as an unexplained
    // acronym (user report, 2026-07-23): this chip states which files the
    // session may write.
    let policy = crate::core::authority::sandbox_policy_for_turn(
        app.mode,
        app.approval_mode,
        app.configured_sandbox_mode.as_deref(),
        &app.workspace,
        crate::core::authority::SandboxNetworkAccess::from_config(app.configured_sandbox_network),
    );
    // A policy is an intent; enforcement needs a backend. On default Linux
    // (bubblewrap is opt-in) and on all Windows there is none. Say
    // "unenforced" rather than name a boundary that is not applied.
    // `DangerFullAccess` is already honest, and `ExternalSandbox` is enforced
    // by the external runner, not by us.
    let unenforced = app.sandbox_backend.is_none()
        && !matches!(
            policy,
            codewhale_sandbox::SandboxPolicy::DangerFullAccess
                | codewhale_sandbox::SandboxPolicy::ExternalSandbox { .. }
        );
    match policy {
        codewhale_sandbox::SandboxPolicy::ReadOnly { .. } if unenforced => {
            Some(Cow::Borrowed("files: read-only (unenforced)"))
        }
        codewhale_sandbox::SandboxPolicy::ReadOnly { .. } => {
            (app.mode != AppMode::Plan).then_some(Cow::Borrowed("files: read-only"))
        }
        // `DangerFullAccess` only ever arises from the Bypass posture
        // (`sandbox_policy_for_turn`), whose permission chip already reads
        // "Full Access" two words to the left. The name is the disclosure;
        // restating it as `files: full disk` spent columns saying it twice.
        // The scope chip speaks in this posture only when the scope is
        // *narrower* than the name implies (the WorkspaceWrite arm below).
        codewhale_sandbox::SandboxPolicy::DangerFullAccess => None,
        codewhale_sandbox::SandboxPolicy::ExternalSandbox { .. } => {
            Some(Cow::Borrowed("files: external sandbox"))
        }
        codewhale_sandbox::SandboxPolicy::WorkspaceWrite { .. } if unenforced => {
            Some(Cow::Borrowed("files: workspace (unenforced)"))
        }
        // The unremarkable case: writes are confined to the workspace and the
        // OS is actually enforcing it. Saying so on every frame of every
        // session spends the header on a fact nobody is asking about — with
        // one exception. When the permission chip reads "Full Access", the
        // scope chip is the only thing on screen that says the writes are
        // still confined. Suppressing it there recreates precisely the
        // misreading the chip was added for (tool-approval "Full Access" taken
        // to mean unrestricted disk writes), and that pairing is reachable:
        // Bypass with a configured `workspace-write` is clamped to this policy
        // by `sandbox_policy_for_turn`.
        codewhale_sandbox::SandboxPolicy::WorkspaceWrite { .. } => {
            (app.approval_mode == ApprovalMode::Bypass).then_some(Cow::Borrowed("files: workspace"))
        }
    }
}

fn truncate_to_width(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let mut result = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width + 1 > width {
            break;
        }
        result.push(ch);
        used += ch_width;
    }
    result.push('…');
    result
}

/// The transcript rows the idle brand mark needs before it will draw at all.
///
/// Named so the *layout* can honour it before the frame is split. Anything that reserves rows above
/// the transcript must subtract against this constant rather than guess, or
/// the reservation and the render gate drift and the mark is evicted by
/// chrome that was sized without knowing the mark existed.
pub(crate) const AMBIENT_MIN_CHAT_HEIGHT: u16 = 16;
/// Companion column floor, same reasoning as [`AMBIENT_MIN_CHAT_HEIGHT`].
pub(crate) const AMBIENT_MIN_CHAT_WIDTH: u16 = 60;

/// Build the post-launch idle composition: brand, workspace context, and one
/// direct invitation. Commands stay in the command surface instead of reading
/// like onboarding homework.
///
/// Expressed in terms of the ambient floor constants so the layout rule that
/// reserves the rows and the gate that spends them cannot disagree. (The old
/// spelling also tested `height >= 14 && width >= 28`, which was dead: the
/// tier check already demands 16 rows and 60 columns.)
#[must_use]
pub(crate) fn empty_state_mark_visible(area: Rect) -> bool {
    area.height >= AMBIENT_MIN_CHAT_HEIGHT && area.width >= AMBIENT_MIN_CHAT_WIDTH
}

#[must_use]
pub(crate) fn decorative_shell_motion_enabled(app: &App) -> bool {
    app.motion_policy().allows_decorative()
        && !app.attention_hold_active()
        && app.onboarding == OnboardingState::None
        && !app.launch.visible
        && app.view_stack.is_empty()
}

/// Shorten a workspace path to its trailing components, marked with a leading
/// ellipsis so it reads as "somewhere above here" rather than as a real path.
fn shorten_workspace(workspace: &str, keep: usize) -> String {
    let sep = if workspace.contains('/') { '/' } else { '\\' };
    let parts: Vec<&str> = workspace.split(sep).filter(|p| !p.is_empty()).collect();
    if parts.len() <= keep {
        return workspace.to_string();
    }
    let tail = parts[parts.len() - keep..].join(&sep.to_string());
    let shortened = format!("…{sep}{tail}");
    // Only elide when it actually buys width. `~/code/app` -> `…/code/app` is
    // the same length and throws away the `~`, which carries more meaning than
    // the ellipsis does.
    if shortened.width() >= workspace.width() {
        return workspace.to_string();
    }
    shortened
}

/// Compose the empty-state caption so the caller's centering can survive.
///
/// This line sits between the wordmark and "What do you want to accomplish?",
/// and every other element of that block is centered. It used to be built at
/// full length and then handed to `truncate_to_width(.., width)`, which made it
/// exactly `width` wide — so the caller's `(width - context.width()) / 2` inset
/// evaluated to zero and the caption rendered flush-left, full-bleed, cutting
/// the composition in half. The clipping also destroyed the information: an
/// absolute path truncated mid-directory ("…/34267917-11f4-4d15-911a-…") tells
/// the reader nothing about where they are.
///
/// So the caption sheds detail rather than getting cut. In order of what goes
/// first: the MCP count, then the branch, then the leading path components. The
/// folder you are in is the last thing to go, because it is the only part a
/// person actually reads here.
///
/// One rule was added after watching it at 120 columns: the margin is
/// proportional, not a flat four. A flat four let a 114-column path "fit" a
/// 119-column lane, which put the centring inset back at two and reproduced
/// the full-bleed banner this function exists to prevent — the same failure,
/// arrived at from the other direction. A sixth of the lane, split either
/// side, means the caption is always visibly a caption.
fn empty_state_caption(
    workspace: &str,
    branch: &str,
    mcp_label: &str,
    mcp_count: usize,
    width: usize,
) -> String {
    // Leave a margin so the line is visibly inset rather than merely fitting,
    // and scale it, because "four columns" is only a margin at 60 columns.
    let budget = width.saturating_sub((width / 6).max(4)).max(8);
    let candidates = [
        format!("{workspace} · {branch} · {mcp_label} {mcp_count}"),
        format!("{workspace} · {branch}"),
        workspace.to_string(),
        format!("{} · {branch}", shorten_workspace(workspace, 2)),
        shorten_workspace(workspace, 2),
        shorten_workspace(workspace, 1),
    ];
    for candidate in &candidates {
        if candidate.width() <= budget {
            return candidate.clone();
        }
    }
    // Nothing fit: the last resort is the folder name alone, and the caller
    // still clamps. Better a bare name than a path clipped mid-component.
    shorten_workspace(workspace, 1)
}

/// The launch card as the idle transcript's own content, plus where its
/// clickable rows landed.
///
/// The opening screen used to be a second surface: its own layout, its own
/// composer widget, its own input authority. Founder ruling: "we don't have
/// to have a different look for the opening screen ... we can make it an
/// asset that exists there instead". So it is drawn as the empty state of the
/// ordinary transcript — the ocean, the water and the chrome underneath it are
/// the ones every other screen already uses, and the composer below it is the
/// real one.
pub struct LaunchEmptyState {
    pub lines: Vec<Line<'static>>,
    /// Text lane relative to the paint area. Outer whitespace is not a
    /// control; selection and pointer targets share this lane.
    text_column: Rect,
    /// Clickable rows as `(id, row index within `lines`)`. The caller turns
    /// these into rects against the painted area, so hitboxes and glyphs
    /// cannot drift apart.
    pub rows: Vec<(crate::tui::app::LaunchRowId, usize)>,
}

/// Minimum left indent. Wider terminals balance the bounded reading lane
/// inside the transcript rather than leaving it stranded against one edge.
const LAUNCH_BLOCK_INDENT: usize = 2;
/// The card's reading measure: a row is a title with its detail set against
/// it, and without a ceiling the detail right-aligns against the terminal's
/// far edge. The title is primary; the relative age is secondary.
const LAUNCH_CARD_MEASURE: usize = 72;
/// Gap between a row's title and its right-aligned detail.
const LAUNCH_ROW_GAP: usize = 3;
/// Below this the row spends its whole lane on the title and sheds the detail.
const LAUNCH_ROW_MIN_TITLE: usize = 28;
/// Labels align with their heading; the action cue has its own gutter.
/// Blank rows the card spends on rhythm when the pane is tall enough.
const LAUNCH_SEPARATORS: usize = 3;
/// Blank rows per separator when the pane can afford them.
const LAUNCH_GAP_ROOMY: usize = 2;
/// Below this width the block gives up its left indent.
const LAUNCH_INDENT_MIN_WIDTH: usize = 12;

pub fn empty_state_lines(app: &App, area: Rect) -> Vec<Line<'static>> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    // The opening screen is this screen: the launch card is the idle
    // transcript's own content, not a second surface painted over it.
    if app.launch.visible {
        // The first keystroke starts the dissolve clock; the card sinks by
        // ink, not by position — every span eases toward the water behind it
        // over `LAUNCH_CARD_DISSOLVE_MS`, and at the end the ambient surface
        // (wordmark, caption, prompt) is what remains. Reduced motion takes
        // the endpoint at once.
        let motion_allowed = app.motion_policy().allows_decorative() && !app.low_motion;
        let dissolve = app
            .launch
            .card_dissolve_progress(app.ambient_clock_ms, motion_allowed);
        if dissolve < 1.0 {
            let mut state = launch_empty_state(app, area);
            if dissolve > 0.0 {
                fade_lines(&mut state.lines, dissolve, app.ui_theme.surface_bg);
            }
            return state.lines;
        }
    }
    let width = usize::from(area.width);
    let mut lines = vec![Line::from(""); usize::from(area.height / 4)];
    // The idle whale portrait that used to open this block was deleted per
    // the 2026-08-29 founder directive; the ambient empty-state surface
    // (wordmark, context caption, prompt) is not whale art and stays.

    let identity = crate::tui::workspace_context::identity_from_context(
        &app.workspace,
        app.workspace_context.as_deref(),
    );
    let workspace = codewhale_util::utils::display_path(&app.workspace);
    let branch = identity.branch.as_deref().map_or_else(
        || tr(app.ui_locale, MessageId::EmptyStateNoGit),
        |branch| Cow::Owned(branch.to_string()),
    );
    // Compact used to bypass the caption entirely and print the bare branch,
    // which in a plain folder rendered as the single centred word "no git" —
    // a whole row of the hero spent naming something that is not there. The
    // shedding ladder already degrades gracefully at any width, so every tier
    // now goes through it.
    let context = empty_state_caption(
        &workspace,
        &branch,
        tr(app.ui_locale, MessageId::EmptyStateMcpLabel).as_ref(),
        app.mcp_configured_count,
        width,
    );
    let brand = "codewhale";
    let brand_inset = " ".repeat(width.saturating_sub(brand.width()) / 2);
    lines.push(Line::from(Span::styled(
        format!("{brand_inset}{brand}"),
        Style::default()
            .fg(app.ui_theme.text_body)
            .add_modifier(Modifier::BOLD),
    )));
    let context = truncate_to_width(&context, width);
    let inset = " ".repeat(width.saturating_sub(context.width()) / 2);
    lines.push(Line::from(Span::styled(
        format!("{inset}{context}"),
        Style::default().fg(app.ui_theme.text_soft),
    )));
    if area.height >= 4 {
        lines.push(Line::from(""));
        let prompt = tr(app.ui_locale, MessageId::EmptyStatePrompt);
        let prompt = truncate_to_width(prompt.as_ref(), width);
        let inset = " ".repeat(width.saturating_sub(prompt.width()) / 2);
        lines.push(Line::from(Span::styled(
            format!("{inset}{prompt}"),
            Style::default().fg(app.ui_theme.text_body),
        )));
    }
    lines
}

/// The remedy the problems row prints, as the command Enter/click types into
/// the composer (#6085): `/mcp login <name>` when a server wants a login,
/// else `/mcp` for failures. `None` when nothing is wrong. One helper serves
/// the row's tail and its action, so what is painted is what runs.
pub(crate) fn mcp_remedy_command(app: &App) -> Option<String> {
    use crate::tui::session_boot::{McpServerBootState, PluginBootSummary, SessionBootSurface};
    let boot = SessionBootSurface::from_parts(
        app.mcp_snapshot.as_ref(),
        app.mcp_boot_initializing(),
        &app.mcp_connecting,
        app.mcp_configured_count,
        PluginBootSummary::default(),
    );
    let first_in = |state: McpServerBootState| -> Option<&str> {
        boot.servers
            .iter()
            .find(|row| row.state == state)
            .map(|row| row.name.as_str())
    };
    if let Some(name) = first_in(McpServerBootState::NeedsLogin) {
        return Some(format!("/mcp login {name}"));
    }
    first_in(McpServerBootState::Failed).map(|_| "/mcp".to_string())
}

/// The launch screen's MCP block: what actually became of the configured
/// servers, painted under the recent-work list.
///
/// The Tideline footer has one clause for this whole fact, so a 23-server
/// workspace rendered as `MCP · 1 connecting · alibaba-cloud-ops` — one
/// arbitrary name, every failure hidden (founder, 2026-09-09). The launch
/// screen has the rows the footer does not, so the two states that carry a
/// remedy get a row each and *name* their servers; the healthy majority stays
/// a count, because a list of things that worked is not information. A server
/// that needs a login and a server that could not connect are different
/// problems with different fixes, so they never share a row.
///
/// State comes from [`crate::tui::session_boot::SessionBootSurface`], the one
/// MCP status owner; this is only its launch projection, and it computes
/// nothing about a server itself.
///
/// The problems row is selectable (#6085): `problems_row` is its index within
/// `lines`, which `launch_empty_state` turns into a hitbox so the row joins
/// the card's shared paint/click/keyboard ordering. Enter or click types the
/// printed remedy into the composer — the user sees the command before a
/// second Enter sends it.
struct McpLaunchBlock {
    lines: Vec<Line<'static>>,
    problems_row: Option<usize>,
}

fn mcp_launch_lines(app: &App, text_width: usize) -> McpLaunchBlock {
    use crate::tui::session_boot::{
        ITEM_SEPARATOR, McpServerBootState, PluginBootSummary, SessionBootPhase, SessionBootSurface,
    };

    // MCP only: the plugin half of the boot surface has its own footer chip
    // and its own screen, and walking the plugin registry every frame to
    // discard it would be waste.
    let boot = SessionBootSurface::from_parts(
        app.mcp_snapshot.as_ref(),
        app.mcp_boot_initializing(),
        &app.mcp_connecting,
        app.mcp_configured_count,
        PluginBootSummary::default(),
    );
    if boot.phase == SessionBootPhase::Hidden || text_width == 0 {
        return McpLaunchBlock {
            lines: Vec::new(),
            problems_row: None,
        };
    }
    let theme = &app.ui_theme;
    let locale = app.ui_locale;
    let names_in = |state: McpServerBootState| -> Vec<&str> {
        boot.servers
            .iter()
            .filter(|row| row.state == state)
            .map(|row| row.name.as_str())
            .collect()
    };
    let failed = names_in(McpServerBootState::Failed);
    let needs_login = names_in(McpServerBootState::NeedsLogin);
    let connected = names_in(McpServerBootState::Connected).len();
    // Before the first boot event the names have not arrived, but the count
    // has; without it a 23-server workspace would paint nothing at all.
    let connecting = names_in(McpServerBootState::Connecting)
        .len()
        .max(boot.connecting_without_names());

    let indent = 0;
    let lane = text_width.saturating_sub(indent);
    let mut lines: Vec<Line<'static>> = Vec::new();

    // The summary carries every non-zero state, because it is also the floor:
    // when the pane can afford one row of this block, that row still has to
    // say two servers failed. Order is by what the reader must act on, so the
    // tail shed below gives up `connected` first — and while anything is in
    // flight that clause leads, since a boot that has connected nothing yet
    // must never read as a boot that finished with nothing connected.
    // `label (n)` rather than `n label`: number agreement is a grammar this
    // renderer cannot get right in fifteen languages.
    let mut parts: Vec<String> = Vec::new();
    let mut count_part = |id: MessageId, count: usize| {
        if count > 0 {
            parts.push(format!("{} ({count})", tr(locale, id)));
        }
    };
    count_part(MessageId::McpStateConnecting, connecting);
    count_part(MessageId::McpStateFailed, failed.len());
    count_part(MessageId::McpStateAuthorizationRequired, needs_login.len());
    count_part(MessageId::ExtensionsStateConnected, connected);
    if parts.is_empty() {
        parts.push(format!(
            "{} (0)",
            tr(locale, MessageId::ExtensionsStateConnected)
        ));
    }
    let mut summary = format!("MCP{ITEM_SEPARATOR}{}", parts.join(ITEM_SEPARATOR));
    while parts.len() > 1 && text_display_width(&summary) > text_width {
        parts.pop();
        summary = format!("MCP{ITEM_SEPARATOR}{}", parts.join(ITEM_SEPARATOR));
    }
    lines.push(Line::from(Span::styled(
        semantic_truncate(&summary, text_width),
        Style::default().fg(if !failed.is_empty() {
            theme.error_fg
        } else if !needs_login.is_empty() {
            theme.warning
        } else {
            theme.text_muted
        }),
    )));

    // One problems row answers *which* and *what to type*: `✕` groups the
    // failed names, `⚠` switches the group to names that want a login, and
    // the remedy rides at the tail. Narrow panes shed the hint, then names
    // from the tail into `+N`, and finally the row itself — never the
    // summary. Glyph *and* state grouping carry the difference, so it
    // survives a monochrome terminal and a colour-blind reader.
    let mut problems_row = None;
    if !failed.is_empty() || !needs_login.is_empty() {
        let hint = match (needs_login.first(), failed.is_empty()) {
            (Some(name), true) => format!("/mcp login {name}"),
            (Some(name), false) => format!("/mcp{ITEM_SEPARATOR}/mcp login {name}"),
            (None, false) => "/mcp".to_string(),
            (None, true) => String::new(),
        };
        let problems = mcp_problems_row(&failed, &needs_login, &hint, lane);
        if let Some(text) = problems {
            let mut spans = Vec::with_capacity(2);
            if indent > 0 {
                spans.push(Span::raw(" ".repeat(indent)));
            }
            spans.push(Span::styled(
                text,
                Style::default().fg(if failed.is_empty() {
                    theme.warning
                } else {
                    theme.error_fg
                }),
            ));
            problems_row = Some(lines.len());
            lines.push(Line::from(spans));
        }
    }
    McpLaunchBlock {
        lines,
        problems_row,
    }
}

/// One problems row: `✕ alibaba-cloud-ops · aws-mcp · ⚠ slack +4 · /mcp`.
///
/// `✕` opens the failed group, `⚠` opens the needs-login group, and the
/// remedy sits at the tail. Shedding folds names into `+N` from the tail
/// while the remedy holds — at each width the named command is tried first,
/// then bare `/mcp`, then none — and when even `✕ +2 · ⚠ +5` cannot fit
/// the row is dropped whole: the summary above still says the count.
fn mcp_problems_row(
    failed: &[&str],
    needs_login: &[&str],
    hint: &str,
    lane: usize,
) -> Option<String> {
    use crate::tui::glyphs::{ATTENTION, FAILED};
    use crate::tui::session_boot::ITEM_SEPARATOR;

    let group = |glyph: &str, names: &[&str], shown: usize, row: &mut String| {
        if names.is_empty() {
            return;
        }
        if !row.is_empty() {
            row.push_str(ITEM_SEPARATOR);
        }
        row.push_str(glyph);
        row.push(' ');
        if shown > 0 {
            row.push_str(&names[..shown].join(ITEM_SEPARATOR));
            let extra = names.len() - shown;
            if extra > 0 {
                row.push_str(ITEM_SEPARATOR);
                row.push('+');
                row.push_str(&extra.to_string());
            }
        } else {
            row.push('+');
            row.push_str(&names.len().to_string());
        }
    };
    let total = failed.len() + needs_login.len();
    for shown in (0..=total).rev() {
        let failed_shown = shown.min(failed.len());
        let login_shown = shown.saturating_sub(failed_shown);
        let mut body = String::new();
        group(FAILED, failed, failed_shown, &mut body);
        group(ATTENTION, needs_login, login_shown, &mut body);
        for tail in [hint, "/mcp", ""] {
            if tail.is_empty() && !hint.is_empty() && shown > 0 {
                continue;
            }
            let mut row = body.clone();
            if !tail.is_empty() {
                row.push_str(ITEM_SEPARATOR);
                row.push_str(tail);
            }
            if text_display_width(&row) <= lane {
                return Some(row);
            }
        }
    }
    None
}

/// Ease every painted glyph toward the water behind it. `dissolve` is
/// `card_dissolve_progress` — 0 is full ink, 1 is gone. Spans without a
/// foreground (padding, blanks) carry nothing to fade.
fn fade_lines(lines: &mut [Line<'static>], dissolve: f32, water: Color) {
    for line in lines.iter_mut() {
        for span in line.spans.iter_mut() {
            if let Some(fg) = span.style.fg {
                span.style.fg = Some(crate::tui::mark::lerp_color(fg, water, dissolve));
            }
        }
    }
}

/// How much of the card fits the pane. `New session` is never shed: it is the
/// screen's one actionable choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LaunchFit {
    brand: bool,
    context: bool,
    help: bool,
    notice: bool,
    whats_new: usize,
    /// The "no model connected · run /provider" line (UX-3).
    setup: bool,
    heading: bool,
    blanks: usize,
    shown: usize,
    see_all: bool,
    /// Rows of the MCP block, tail-first as the pane shrinks. The block
    /// always costs one separator row on top of these, so it never reads as
    /// another session in the list above it.
    mcp: usize,
    /// Blank rows per separator. A pane with height to spare spends it on
    /// breathing room before it spends it on more content: one blank line
    /// between blocks packs the card into the top-left corner of a tall
    /// terminal and reads as clutter even when every row is earning its place.
    /// This is the first thing shed, so a short pane is unaffected.
    gap: usize,
}

impl LaunchFit {
    const fn rows(self) -> usize {
        (self.brand as usize)
            + (self.context as usize)
            + (self.help as usize)
            + (self.notice as usize)
            + self.whats_new
            + (self.setup as usize)
            + self.blanks * self.gap
            + 1
            + (self.heading as usize)
            + self.shown
            + (self.see_all as usize)
            + self.mcp
            + (self.mcp > 0) as usize * self.gap
    }
}

/// Shed the card down to `height`, in a fixed order: rhythm, the migration
/// notice, the MCP block's detail, identity/help chrome, then the tail of
/// the recent list. The overflow row keeps any hidden sessions reachable.
/// The "no model connected" line goes last of all: on a keyless first run
/// it is the only thing on the card that explains why nothing will answer.
///
/// The MCP block gives up its rows before the recent list does (recent work
/// is what the screen is *for*) but keeps its summary line until almost
/// everything else has gone, because "2 failed" in one row still tells the
/// truth that the footer chip could not.
fn launch_fit(
    height: usize,
    recent: usize,
    has_more: bool,
    notice: bool,
    whats_new: usize,
    mcp: usize,
    setup: bool,
) -> LaunchFit {
    let mut fit = LaunchFit {
        brand: true,
        context: true,
        help: true,
        notice,
        whats_new,
        setup,
        heading: recent > 0 || has_more,
        blanks: LAUNCH_SEPARATORS,
        shown: recent,
        see_all: has_more,
        mcp,
        gap: LAUNCH_GAP_ROOMY,
    };
    let mut step = 0u8;
    while fit.rows() > height {
        match step {
            // Breathing room is the first luxury to go, before any content.
            0 => fit.gap = 1,
            1 => fit.blanks = 1,
            2 => fit.blanks = 0,
            3 => fit.notice = false,
            4 => {
                while fit.whats_new > 0 && fit.rows() > height {
                    fit.whats_new -= 1;
                }
            }
            5 => {
                while fit.mcp > 1 && fit.rows() > height {
                    fit.mcp -= 1;
                }
            }
            6 => fit.help = false,
            7 => fit.context = false,
            8 => fit.heading = false,
            9 => fit.brand = false,
            10 => {
                while fit.shown > 0 && fit.rows() > height {
                    fit.shown -= 1;
                    fit.see_all = true;
                }
            }
            11 => fit.mcp = 0,
            12 => fit.see_all = false,
            13 => fit.setup = false,
            _ => break,
        }
        step += 1;
    }
    fit
}

fn whats_new_launch_lines(app: &App, text_width: usize) -> Vec<(String, bool)> {
    let Some(notice) = app.launch.whats_new.as_ref() else {
        return Vec::new();
    };
    if app.launch.return_to_session {
        return Vec::new();
    }
    let headline = notice.headline();
    let mut lines: Vec<(String, bool)> = if text_display_width(&headline) <= text_width {
        vec![(headline, false)]
    } else {
        let mut rows = crate::tui::onboarding::wrap_words(&notice.updated, text_width);
        if let Some(counts) = &notice.counts {
            rows.extend(crate::tui::onboarding::wrap_words(counts, text_width));
        }
        rows.into_iter().map(|row| (row, false)).collect()
    };
    let notes = [notice.notes_with_command(), notice.notes.clone()]
        .into_iter()
        .find(|line| text_display_width(line) <= text_width)
        .unwrap_or_else(|| notice.url.clone());
    lines.push((notes, true));
    lines
}

pub fn launch_empty_state(app: &App, area: Rect) -> LaunchEmptyState {
    if area.width == 0 || area.height == 0 {
        return LaunchEmptyState {
            lines: Vec::new(),
            rows: Vec::new(),
            text_column: Rect::default(),
        };
    }
    let width = usize::from(area.width);
    let height = usize::from(area.height);
    let theme = &app.ui_theme;
    let locale = app.ui_locale;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rows: Vec<(crate::tui::app::LaunchRowId, usize)> = Vec::new();

    // One reading lane: identity never steals width from session titles.
    // Reserve a two-cell action gutter where the terminal can afford it.
    let block_indent = if width >= LAUNCH_INDENT_MIN_WIDTH {
        LAUNCH_BLOCK_INDENT.max(width.saturating_sub(LAUNCH_CARD_MEASURE + 2) / 2)
    } else {
        0
    };
    let action_gutter = if width >= LAUNCH_INDENT_MIN_WIDTH {
        2
    } else {
        0
    };
    let text_indent = block_indent + action_gutter;
    let text_width = width
        .saturating_sub(text_indent + block_indent)
        .min(LAUNCH_CARD_MEASURE);
    if text_width == 0 {
        return LaunchEmptyState {
            lines: Vec::new(),
            rows: Vec::new(),
            text_column: Rect::default(),
        };
    }

    let (entries, has_more) = launch_recent_entries(app);
    // Nothing will answer a message until a model is connected; the card
    // says so instead of letting the first Enter fail silently (UX-3).
    let no_model_connected = app.onboarding_needs_api_key;
    // Built before the fit ladder runs: how many rows the block wants is a
    // fact about this workspace's servers, not about the pane.
    let mcp_block = mcp_launch_lines(app, text_width);
    let whats_new_lines = whats_new_launch_lines(app, text_width);
    // Brand occupies the header only; recent titles retain the whole reading lane.
    // Scale the canonical raster derivative before sacrificing any controls.
    use crate::tui::mark::MarkSize;
    let mut mark = if crate::tui::color_compat::ascii_safe_enabled() {
        None
    } else if height >= 14 && text_width >= 40 {
        Some(MarkSize::Large)
    } else if height >= 8 && text_width >= 26 {
        Some(MarkSize::Small)
    } else if height >= 4 && text_width >= 19 {
        Some(MarkSize::Tiny)
    } else {
        None
    };
    let mark_extra = mark.map_or(0, |size| usize::from(size.cells().1).saturating_sub(2));
    let mut fit = launch_fit(
        height.saturating_sub(mark_extra),
        entries.len(),
        has_more,
        app.launch.claude_code_detected,
        whats_new_lines.len(),
        mcp_block.lines.len(),
        no_model_connected,
    );
    if mark.is_some() && !(fit.brand && fit.context) {
        // At the absolute height floor the wordmark yields to the actions too.
        mark = None;
        fit = launch_fit(
            height,
            entries.len(),
            has_more,
            app.launch.claude_code_detected,
            whats_new_lines.len(),
            mcp_block.lines.len(),
            no_model_connected,
        );
    }
    let header_width =
        text_width.saturating_sub(mark.map_or(0, |size| usize::from(size.cells().0) + 2));
    let spacious = fit.blanks == LAUNCH_SEPARATORS;
    let visible: Vec<LaunchRecentEntry> = entries.into_iter().take(fit.shown).collect();
    let card_rows = home_card_rows(app, &visible, fit.see_all);

    // The text column, in order. `None` is a blank row.
    let mut text: Vec<Option<Line<'static>>> = Vec::new();
    if fit.brand {
        let brand = "codewhale";
        let version = format!("v{}", env!("CODEWHALE_BUILD_VERSION"));
        let mut spans = vec![Span::styled(
            semantic_truncate(brand, header_width),
            Style::default().fg(theme.accent_primary).bold(),
        )];
        if header_width >= text_display_width(brand) + 1 + text_display_width(&version) {
            spans.push(Span::styled(
                format!(
                    "{}{version}",
                    " ".repeat(
                        header_width - text_display_width(brand) - text_display_width(&version)
                    )
                ),
                Style::default().fg(theme.text_muted),
            ));
        }
        text.push(Some(Line::from(spans)));
    }
    if fit.context {
        let workspace = shorten_workspace(&codewhale_util::utils::display_path(&app.workspace), 2);
        let identity = crate::tui::workspace_context::identity_from_context(
            &app.workspace,
            app.workspace_context.as_deref(),
        );
        let mut spans = vec![Span::styled(
            semantic_truncate(&workspace, header_width),
            Style::default().fg(theme.text_soft),
        )];
        if let Some(branch) = identity.branch {
            let detail = format!(" · {branch}");
            if text_display_width(&workspace) + text_display_width(&detail) <= header_width {
                spans.push(Span::styled(detail, Style::default().fg(theme.text_muted)));
            }
        }
        text.push(Some(Line::from(spans)));
    }
    if let Some(mark) = mark {
        text.resize_with(usize::from(mark.cells().1), || None);
        for (row, dots) in mark.rows().iter().enumerate() {
            let mut spans = vec![
                Span::styled(
                    crate::tui::mark::reveal_row(
                        dots,
                        app.launch
                            .mark_reveal_started_at
                            .map_or(crate::tui::mark::REVEAL_MS, |started| {
                                started.elapsed().as_millis()
                            }),
                        app.motion_policy().allows_decorative() && !app.launch.return_to_session,
                    ),
                    Style::default().fg(theme.accent_primary),
                ),
                Span::raw("  "),
            ];
            if let Some(line) = text[row].take() {
                spans.extend(line.spans);
            }
            text[row] = Some(Line::from(spans));
        }
    }
    if fit.setup {
        let line = format!(
            "{}{}{}",
            tr(locale, MessageId::LaunchNoModelConnected),
            crate::tui::session_boot::ITEM_SEPARATOR,
            tr(locale, MessageId::LaunchRunCommand).replace("{command}", "/provider"),
        );
        text.push(Some(Line::from(Span::styled(
            semantic_truncate(&line, text_width),
            Style::default().fg(theme.warning),
        ))));
    }
    for (line, muted) in whats_new_lines.iter().take(fit.whats_new) {
        let ink = if *muted {
            theme.text_muted
        } else {
            theme.text_body
        };
        text.push(Some(Line::from(Span::styled(
            semantic_truncate(line, text_width),
            Style::default().fg(ink),
        ))));
    }
    // The migration notice, while there is still a question to answer. It
    // retires for good once `/import-claude` has been run.
    if fit.notice {
        text.push(Some(Line::from(Span::styled(
            semantic_truncate(&tr(locale, MessageId::LaunchNoticeClaude), text_width),
            Style::default().fg(theme.text_muted),
        ))));
    }
    if fit.blanks >= 1 {
        for _ in 0..fit.gap {
            text.push(None);
        }
    }

    for row in &card_rows {
        let style = if row.prominent {
            Style::default().fg(theme.text_body).bold()
        } else {
            Style::default().fg(theme.text_body)
        };
        if matches!(row.id, crate::tui::app::LaunchRowId::SeeAll) && spacious {
            for _ in 0..fit.gap {
                text.push(None);
            }
        }
        let lane = text_width;
        let detail_width = text_display_width(&row.detail);
        // Age is context: when the lane cannot hold a readable title
        // beside it, drop the age whole
        // rather than ellipsing the title to a stub. This is also the
        // fallback for a locale that spends more cells on the same fact.
        let detail = if row.detail.is_empty()
            || lane < LAUNCH_ROW_MIN_TITLE + LAUNCH_ROW_GAP + detail_width
        {
            ""
        } else {
            row.detail.as_str()
        };
        let label_budget = if detail.is_empty() {
            lane
        } else {
            lane.saturating_sub(LAUNCH_ROW_GAP + detail_width)
        };
        let label = semantic_truncate(&row.label, label_budget);
        let label_width = text_display_width(&label);
        let mut spans = Vec::with_capacity(4);
        spans.push(Span::styled(label, style));
        if !detail.is_empty() {
            let pad = lane
                .saturating_sub(label_width)
                .saturating_sub(detail_width);
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(
                detail.to_string(),
                Style::default().fg(theme.text_muted),
            ));
        }
        if row.prominent {
            spans.push(Span::styled(
                " ".repeat(lane.saturating_sub(label_width)),
                style,
            ));
        }
        rows.push((row.id.clone(), text.len()));
        text.push(Some(Line::from(spans)));
        if row.prominent && fit.heading {
            // An empty workspace needs only the invitation and composer.
            // Real history, including filtered sessions reachable via See all,
            // still gets a heading; zero counts are not content.
            if spacious {
                for _ in 0..fit.gap {
                    text.push(None);
                }
            }
            let label = semantic_truncate(&tr(locale, MessageId::LaunchRecentHeading), text_width);
            let remaining = text_width.saturating_sub(text_display_width(&label) + 2);
            let mut spans = vec![Span::styled(
                label,
                Style::default().fg(theme.text_soft).bold(),
            )];
            if remaining >= 4 {
                let rule = if crate::tui::color_compat::ascii_safe_enabled() {
                    "-"
                } else {
                    "─"
                };
                spans.push(Span::styled(
                    format!("  {}", rule.repeat(remaining)),
                    Style::default().fg(theme.border),
                ));
            }
            text.push(Some(Line::from(spans)));
        }
    }

    // MCP status, under the recent-work list, where the founder asked for it
    // (2026-09-09): the footer chip could name one server out of 23 and hid
    // every failure behind a count.
    if fit.mcp > 0 {
        for _ in 0..fit.gap {
            text.push(None);
        }
        for (offset, line) in mcp_block.lines.into_iter().enumerate() {
            if offset >= fit.mcp {
                break;
            }
            if offset == 0 {
                rows.push((crate::tui::app::LaunchRowId::McpManager, text.len()));
            } else if mcp_block.problems_row == Some(offset) {
                rows.push((crate::tui::app::LaunchRowId::McpRemedy, text.len()));
            }
            text.push(Some(line));
        }
    }

    // Keep the invitation and recent work ahead of command instructions.
    if fit.help {
        if text.len() + 1 < height {
            text.push(None);
        }
        text.push(Some(Line::from(Span::styled(
            semantic_truncate(
                &tr(locale, MessageId::LaunchHelpLine).replace(
                    "{dock}",
                    crate::tui::shell_key_routing::binding(
                        crate::tui::shell_key_routing::ShellBindingId::ViewCycle,
                    )
                    .footer_chord,
                ),
                text_width,
            ),
            Style::default().fg(theme.text_hint),
        ))));
    }

    // Paint the state mouse/keyboard navigation already records. Restrict the
    // band to the text lane so selecting a session never colors the margins.
    for (index, (_, row)) in rows.iter().enumerate() {
        let style = if app.launch.menu_selected == Some(index) {
            Some(crate::tui::menu_style::selected_row_bg_style().bold())
        } else if app.launch.hovered_row == Some(index) {
            Some(crate::tui::menu_style::hovered_row_style())
        } else {
            None
        };
        if let Some(style) = style
            && let Some(Some(line)) = text.get_mut(*row)
        {
            let padding = text_width.saturating_sub(line.width());
            line.spans.push(Span::raw(" ".repeat(padding)));
            for span in &mut line.spans {
                span.style = span.style.patch(style);
            }
        }
    }

    // Stable focus gutter: the cursor identifies the current Enter target.
    // The band includes the gutter and only the bounded reading lane.
    // A little top breathing room only comes from unused space. Compact
    // terminals never sacrifice a control for this composition.
    if height >= 16 {
        // Use spare height to balance the launcher above the composer. Leave
        // the bottom half as breathing room; controls never lose a row.
        let top = height.saturating_sub(text.len()) / 2;
        lines.resize_with(top, || Line::from(""));
    }
    let block_rows = text.len().min(height.saturating_sub(lines.len()));
    let mut row_offsets = Vec::with_capacity(block_rows);
    for (row, line) in text.iter().take(block_rows).enumerate() {
        let action = rows.iter().position(|(_, y)| *y == row);
        let selected = action.is_some_and(|i| app.launch.menu_selected == Some(i));
        let hovered = action.is_some_and(|i| app.launch.hovered_row == Some(i));
        let style = if selected {
            crate::tui::menu_style::selected_row_style()
        } else if hovered {
            crate::tui::menu_style::hovered_row_style()
        } else {
            Style::default().fg(theme.text_muted)
        };
        let mut spans = vec![Span::raw(" ".repeat(block_indent))];
        if action_gutter > 0 {
            let marker = if selected || hovered {
                if crate::tui::color_compat::ascii_safe_enabled() {
                    "> "
                } else {
                    "› "
                }
            } else {
                "  "
            };
            spans.push(Span::styled(marker, style));
        }
        if let Some(line) = line {
            spans.extend(line.spans.iter().cloned());
        }
        row_offsets.push(lines.len());
        lines.push(Line::from(spans));
    }

    // Re-point the hitboxes at the composed rows. A row the block could not
    // fit has no offset, so it has no hitbox either.
    let rows = rows
        .into_iter()
        .filter_map(|(id, text_row)| row_offsets.get(text_row).map(|y| (id, *y)))
        .collect();

    LaunchEmptyState {
        lines,
        rows,
        text_column: Rect::new(
            block_indent as u16,
            0,
            (text_width + action_gutter) as u16,
            area.height,
        ),
    }
}

#[cfg(test)]
mod launch_card_tests {
    use super::{
        LAUNCH_CARD_MEASURE, LaunchAction, launch_empty_state, launch_fit, launch_recent_entries,
        launch_row_click_action, launch_rows_for_app, refresh_launch_row_hitboxes,
        run_launch_card_row, text_display_width,
    };
    use crate::tui::app::{App, LaunchRecentSession, LaunchRowId};
    use ratatui::layout::Rect;
    use ratatui::text::Line;
    use unicode_segmentation::UnicodeSegmentation;

    fn app_with_recent(titles: &[&str], total: usize) -> App {
        let mut app = crate::tui::test_support::test_app_with_options(
            crate::tui::test_support::test_tui_options(std::env::temp_dir()),
        );
        app.launch.visible = true;
        app.launch.claude_code_detected = false;
        app.launch.recent = titles
            .iter()
            .enumerate()
            .map(|(index, title)| LaunchRecentSession {
                id: format!("{index}0abcdef-session"),
                title: (*title).to_string(),
                updated_at: chrono::Utc::now()
                    - chrono::Duration::hours(i64::try_from(index).unwrap_or(0) + 1),
                message_count: 40 + index,
                interrupted: false,
            })
            .collect();
        app.launch.total_workspace_sessions = total;
        app
    }

    #[test]
    fn launch_primary_action_has_readable_ink_in_every_theme() {
        for theme in codewhale_palette::SELECTABLE_THEMES {
            let mut app = app_with_recent(&["Recent proof"], 1);
            app.ui_theme = theme.ui_theme();
            app.theme_id = *theme;
            let card = launch_empty_state(&app, Rect::new(0, 0, 100, 24));
            let span = card
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .find(|span| span.content.contains("New session"))
                .unwrap();
            assert_eq!(
                span.style.fg,
                Some(app.ui_theme.text_body),
                "{}",
                theme.name()
            );
            if let Some(ratio) =
                codewhale_palette::contrast_ratio(span.style.fg.unwrap(), app.ui_theme.panel_bg)
            {
                assert!(
                    ratio >= 4.5,
                    "{} New session contrast {ratio}",
                    theme.name()
                );
            }
        }
    }

    #[test]
    fn completion_settle_keeps_done_label_stable() {
        let mut app = app_with_recent(&[], 0);
        app.low_motion = false;
        app.fancy_animations = true;
        let activity = super::LiveActivity::from_app(&app);
        for elapsed in [0, 280, 700] {
            app.ocean_completion_started_at =
                Some(std::time::Instant::now() - std::time::Duration::from_millis(elapsed));
            let (_, label) =
                super::phase_marker_with_activity(&app, super::ShellPhase::Done, activity);
            assert_eq!(label, super::ShellPhase::Done.label(app.ui_locale));
        }
    }

    #[test]
    fn launch_reveal_stops_scheduling_after_its_endpoint() {
        let mut app = app_with_recent(&[], 0);
        app.onboarding = crate::tui::app::OnboardingState::None;
        app.theme_id = codewhale_palette::ThemeId::Shoreline;
        app.low_motion = false;
        app.fancy_animations = true;
        app.launch.mark_reveal_started_at = Some(std::time::Instant::now());
        assert!(super::launch_motion_active(&app, false, true));
        assert!(!super::launch_motion_active(&app, true, true));
        app.low_motion = true;
        assert!(!super::launch_motion_active(&app, false, true));
        app.low_motion = false;
        app.launch.mark_reveal_started_at =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(361));
        assert!(!super::launch_motion_active(&app, false, true));
    }

    /// The founder's own shape: many servers, a couple genuinely broken, a
    /// pile sitting unauthenticated, the rest fine.
    fn with_mcp(mut app: App) -> App {
        use crate::mcp::{McpManagerSnapshot, McpServerCapabilityMetadata, McpServerSnapshot};
        let mut servers = Vec::new();
        let mut push = |name: &str, connected: bool, error: Option<&str>, auth: bool| {
            servers.push(McpServerSnapshot {
                name: name.to_string(),
                enabled: true,
                required: false,
                transport: "stdio".to_string(),
                command_or_url: format!("cmd-{name}"),
                connect_timeout: 5,
                execute_timeout: 5,
                read_timeout: 5,
                connected,
                error: error.map(str::to_string),
                auth_required: auth,
                capability_metadata: McpServerCapabilityMetadata::NotObserved,
                tools: Vec::new(),
                resources: Vec::new(),
                prompts: Vec::new(),
            });
        };
        for name in ["github", "linear", "supabase", "posthog", "vercel"] {
            push(name, true, None, false);
        }
        push(
            "alibaba-cloud-ops",
            false,
            Some("Invalid request parameters"),
            false,
        );
        push("aws-mcp", false, Some("Stdio transport closed"), false);
        for name in ["slack", "notion", "stripe", "figma", "excalidraw"] {
            push(name, false, Some("401 Unauthorized"), true);
        }
        app.mcp_configured_count = servers.len();
        app.mcp_snapshot = Some(McpManagerSnapshot {
            config_path: std::path::PathBuf::from("mcp.json"),
            config_exists: true,
            reload_required: false,
            servers,
        });
        app.set_mcp_boot_initializing(false);
        app.mcp_connecting = Vec::new();
        app
    }

    fn flatten(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.to_string())
            .collect::<String>()
    }

    fn painted(app: &App, width: u16, height: u16) -> Vec<String> {
        launch_empty_state(app, Rect::new(0, 0, width, height))
            .lines
            .iter()
            .map(|line| flatten(line).trim_end().to_string())
            .collect()
    }

    fn row_ids(app: &App) -> Vec<LaunchRowId> {
        app.launch
            .row_hitboxes
            .iter()
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The recent row's own text, with the block indent stripped. Only used at
    /// widths narrow enough that the detail has shed, so what remains is the
    /// title alone.
    fn recent_row_title(app: &App, width: u16, height: u16) -> String {
        let state = launch_empty_state(app, Rect::new(0, 0, width, height));
        let (_, row) = state
            .rows
            .iter()
            .find(|(id, _)| matches!(id, LaunchRowId::Recent(_)))
            .expect("a recent row painted");
        flatten(&state.lines[*row])
            .trim()
            .trim_start_matches("› ")
            .trim_start_matches("> ")
            .to_string()
    }

    fn is_grapheme_prefix(candidate: &str, full: &str) -> bool {
        let mut source = full.graphemes(true);
        candidate.graphemes(true).all(|g| source.next() == Some(g))
    }

    // --- one ordering for paint, mouse, and keyboard -------------------

    #[test]
    fn keyboard_rows_are_exactly_the_rows_the_pane_painted() {
        let mut app = app_with_recent(&["one", "two", "three", "four", "five"], 9);

        refresh_launch_row_hitboxes(&mut app, Rect::new(0, 0, 120, 30));
        let tall = launch_rows_for_app(&app);
        assert_eq!(
            tall.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
            row_ids(&app),
            "keyboard list must be the painted list",
        );
        assert!(tall.len() >= 7, "{:?}", row_ids(&app));

        // Highlight the last row, then shrink the pane under it.
        app.launch.menu_selected = Some(tall.len() - 1);
        refresh_launch_row_hitboxes(&mut app, Rect::new(0, 0, 120, 4));
        let short = launch_rows_for_app(&app);
        assert_eq!(
            short.iter().map(|row| row.id.clone()).collect::<Vec<_>>(),
            row_ids(&app),
        );
        assert!(short.len() < tall.len(), "the short pane shed nothing");

        // The stale highlight is gone, so Enter cannot resume a row that is
        // no longer on screen.
        assert_eq!(app.launch.menu_selected, None);
        assert_eq!(
            run_launch_card_row(&short, app.launch.menu_selected),
            LaunchAction::None,
        );
    }

    #[test]
    fn no_keyboard_row_names_a_session_the_pane_is_not_showing() {
        let mut app = app_with_recent(&["one", "two", "three", "four", "five"], 5);
        for height in 0u16..=14 {
            refresh_launch_row_hitboxes(&mut app, Rect::new(0, 0, 120, height));
            let painted: Vec<LaunchRowId> = row_ids(&app);
            for row in launch_rows_for_app(&app) {
                assert!(
                    painted.contains(&row.id),
                    "height {height}: {:?} is runnable but was not painted",
                    row.id,
                );
            }
            // Every arrow position runs a painted row or nothing at all.
            for index in 0..painted.len() + 3 {
                let rows = launch_rows_for_app(&app);
                match run_launch_card_row(&rows, Some(index)) {
                    LaunchAction::None => {}
                    LaunchAction::ResumeSession(id) => assert!(
                        painted.contains(&LaunchRowId::Recent(id.clone())),
                        "height {height}: Enter at {index} would resume unpainted {id}",
                    ),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn shed_sessions_stay_reachable_through_the_overflow_row() {
        let mut app = app_with_recent(&["one", "two", "three", "four", "five"], 5);
        // Five sessions, none behind the inline list: a tall pane needs no
        // overflow row, a short one sheds and therefore must offer it.
        refresh_launch_row_hitboxes(&mut app, Rect::new(0, 0, 120, 30));
        assert!(!row_ids(&app).contains(&LaunchRowId::SeeAll));
        refresh_launch_row_hitboxes(&mut app, Rect::new(0, 0, 120, 4));
        let ids = row_ids(&app);
        assert!(ids.contains(&LaunchRowId::SeeAll), "{ids:?}");
        assert!(
            launch_rows_for_app(&app)
                .iter()
                .any(|row| row.id == LaunchRowId::SeeAll)
        );
    }

    // --- the card fits the pane it is drawn into -----------------------

    #[test]
    fn the_card_fits_every_pane_it_is_drawn_into() {
        // Both shapes: no MCP servers at all, and the twelve-server workspace
        // whose status block is the widest thing the card paints.
        for app in [
            app_with_recent(&["one", "two", "three", "four", "five"], 9),
            with_mcp(app_with_recent(&["one", "two", "three", "four", "five"], 9)),
        ] {
            for width in [0u16, 1, 2, 3, 8, 12, 20, 31, 32, 40, 44, 64, 80, 120, 200] {
                for height in 0u16..=30 {
                    let state = launch_empty_state(&app, Rect::new(0, 0, width, height));
                    assert!(
                        state.lines.len() <= usize::from(height),
                        "{width}x{height}: {} lines",
                        state.lines.len(),
                    );
                    for line in &state.lines {
                        let painted = text_display_width(&flatten(line));
                        assert!(
                            painted <= usize::from(width),
                            "{width}x{height}: row of {painted} cells",
                        );
                    }
                    for (id, row) in &state.rows {
                        assert!(
                            *row < state.lines.len(),
                            "{width}x{height}: hitbox {id:?} has no row",
                        );
                    }
                    if width > 0 && height > 0 {
                        assert!(
                            state
                                .rows
                                .iter()
                                .any(|(id, _)| matches!(id, LaunchRowId::NewSession)),
                            "{width}x{height}: nothing actionable painted",
                        );
                    }
                }
            }
        }
    }

    // --- MCP status, under the recent list ------------------------------

    /// The defect this block was built for: the footer chip named one server
    /// out of twenty-three and reported every other state as a bare count, so
    /// ten servers sitting unauthenticated were invisible. Failed and
    /// needs-login are different problems with different fixes and must never
    /// collapse into one clause — and the block is two lines at most now:
    /// the summary owns the counts, one problems row names what broke.
    #[test]
    fn the_mcp_block_names_what_broke_and_separates_it_from_what_needs_a_login() {
        let app = with_mcp(app_with_recent(&["one", "two"], 2));
        let lines = painted(&app, 120, 30);
        // The summary owns the totals, and every non-zero state is on it.
        let summary = lines
            .iter()
            .find(|line| line.contains("MCP"))
            .expect("the MCP summary");
        assert!(summary.contains("connection failed (2)"), "{summary:?}");
        assert!(
            summary.contains("authorization required (5)"),
            "ten servers at not-logged-in were invisible before this: {summary:?}",
        );
        // One problems row answers *which*: ✕ groups the failures, ⚠ groups
        // the logins, and the remedy rides at the tail.
        let problems: Vec<_> = lines
            .iter()
            .filter(|line| {
                line.contains(crate::tui::glyphs::FAILED)
                    || line.contains(crate::tui::glyphs::ATTENTION)
            })
            .collect();
        assert_eq!(problems.len(), 1, "one problems row: {lines:#?}");
        let row = problems[0];
        assert!(row.contains("alibaba-cloud-ops"), "{row:?}");
        assert!(row.contains("aws-mcp"), "{row:?}");
        assert!(row.contains("slack"), "{row:?}");
        // The remedy is the command to type, not advice about typing one —
        // the named form sheds to bare `/mcp` before any name does.
        assert!(row.contains("/mcp"), "{row:?}");
    }

    /// While the boot is in flight the block must not read as a finished one.
    #[test]
    fn a_boot_in_flight_says_connecting_rather_than_connected() {
        let mut app = app_with_recent(&["one"], 1);
        app.set_mcp_boot_initializing(true);
        app.mcp_configured_count = 23;
        app.mcp_connecting = Vec::new();
        let lines = painted(&app, 120, 30);
        let summary = lines
            .iter()
            .find(|line| line.contains("MCP"))
            .expect("the MCP summary");
        assert!(summary.contains("connecting (23)"), "{summary:?}");
        assert!(
            !summary.contains("connected"),
            "a boot with nothing connected yet claimed a count: {summary:?}",
        );
    }

    #[test]
    fn the_fit_ladder_never_sheds_the_one_actionable_row() {
        for height in 1usize..=16 {
            for recent in 0usize..=5 {
                for has_more in [false, true] {
                    for (notice, whats_new) in [(false, 0), (true, 0), (false, 3), (true, 2)] {
                        for mcp in 0usize..=4 {
                            for setup in [false, true] {
                                let fit = launch_fit(
                                    height, recent, has_more, notice, whats_new, mcp, setup,
                                );
                                assert!(fit.rows() <= height.max(1), "{height} {recent}: {fit:?}");
                                assert!(fit.shown <= recent);
                                assert!(fit.mcp <= mcp);
                                if fit.shown < recent {
                                    assert!(
                                        fit.see_all || fit.rows() >= height,
                                        "shed rows became unreachable: {fit:?}",
                                    );
                                }
                                if setup && !fit.setup {
                                    assert_eq!(
                                        (fit.shown, fit.mcp, fit.see_all),
                                        (0, 0, false),
                                        "the no-model line outlived other rows: {fit:?}",
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_keyless_launch_says_no_model_is_connected_and_how_to_fix_it() {
        let mut app = app_with_recent(&["Fix the parser"], 1);
        app.onboarding_needs_api_key = true;
        let lines = painted(&app, 100, 30).join("\n");
        assert!(lines.contains("no model connected"), "{lines}");
        assert!(lines.contains("/provider"), "{lines}");
        // Even a pane too short for the recent list keeps the recovery line.
        let short = painted(&app, 100, 3).join("\n");
        assert!(short.contains("no model connected"), "{short}");

        app.onboarding_needs_api_key = false;
        let lines = painted(&app, 100, 30).join("\n");
        assert!(!lines.contains("no model connected"), "{lines}");
    }

    #[test]
    fn empty_workspace_omits_recent_section_but_hidden_history_stays_reachable() {
        let app = app_with_recent(&[], 0);
        let text = painted(&app, 100, 24).join("\n");
        assert!(text.contains("New session"));
        assert!(!text.contains("Recent"));
        assert!(!text.contains("No recent sessions"));
        assert!(!launch_fit(24, 0, false, false, 0, 0, false).heading);
        assert!(launch_fit(24, 0, true, false, 0, 0, false).heading);
        assert!(launch_fit(24, 0, true, false, 0, 0, false).see_all);
    }

    // --- the row reads as one object -----------------------------------

    #[test]
    fn a_row_keeps_its_detail_beside_its_title() {
        let app = app_with_recent(&["Ship the launch card"], 1);
        let lines = painted(&app, 200, 24);
        let row = lines
            .iter()
            .find(|line| line.contains("Ship the launch card"))
            .expect("recent row painted");
        assert!(
            text_display_width(row.trim_start()) <= LAUNCH_CARD_MEASURE + 2,
            "row runs to the terminal edge: {row:?}",
        );
        let (entries, _) = launch_recent_entries(&app);
        assert!(
            row.contains(&entries[0].detail),
            "row lost its age: {row:?}"
        );
        assert!(
            !row.contains("msgs"),
            "message counts belong in session details: {row:?}"
        );
    }

    #[test]
    fn a_narrow_pane_spends_its_lane_on_the_title() {
        let app = app_with_recent(&["Ship the launch card"], 1);
        let row = recent_row_title(&app, 40, 24);
        let (entries, _) = launch_recent_entries(&app);
        assert!(
            !row.contains(&entries[0].detail),
            "age should have shed: {row:?}"
        );
        assert!(row.starts_with("Ship the launch"), "{row:?}");
    }

    // --- scripts --------------------------------------------------------

    #[test]
    fn titles_truncate_on_grapheme_boundaries_in_several_scripts() {
        // Not a claim about every script: these are the four shapes that break
        // char-indexed truncation — wide cells, RTL runs, ZWJ/skin-tone emoji,
        // and combining marks.
        let samples = [
            (
                "latin",
                "Ship the launch card and verify truncation behaviour",
            ),
            ("cjk", "部署新的会话启动卡片并验证宽字符截断行为"),
            ("arabic", "تهيئة بطاقة إطلاق الجلسة والتحقق من سلوك الاقتطاع"),
            ("hebrew", "הגדרת כרטיס פתיחת הפעלה ואימות התנהגות הקיצוץ"),
            ("emoji", "👩🏽‍🚀 crew 👨‍👩‍👧‍👦 families and flags 🇯🇵 shipping today"),
            (
                "combining",
                "cafe\u{301} de\u{301}ja\u{300} vu\u{308} with combining marks throughout",
            ),
        ];
        for (name, title) in samples {
            let app = app_with_recent(&[title], 1);
            for width in [8u16, 12, 20, 32, 40, 60, 80, 120, 200] {
                for line in painted(&app, width, 24) {
                    assert!(
                        text_display_width(&line) <= usize::from(width),
                        "{name} at {width}: {line:?} overruns the pane",
                    );
                }
            }
            // At these widths the detail has shed, so the row is the title
            // alone: what is painted must be whole graphemes off its front.
            for width in [12u16, 20, 32, 40] {
                let row = recent_row_title(&app, width, 24);
                let body = row.strip_suffix('…').unwrap_or(&row);
                assert!(
                    is_grapheme_prefix(body, title),
                    "{name} at {width}: {body:?} splits a grapheme of {title:?}",
                );
            }
        }
    }

    // --- rhythm ---------------------------------------------------------

    #[test]
    fn a_tall_pane_breathes_and_a_short_one_gives_the_rhythm_up_first() {
        // Counting blank *painted* rows would be wrong: the mark column sits
        // behind the first six of them. The rhythm is the gap between the
        // new-session entry and the heading below it.
        let app = app_with_recent(&["one", "two", "three", "four", "five"], 9);

        let tall = painted(&app, 120, 30);
        let entry = tall
            .iter()
            .position(|line| line.contains("New session"))
            .expect("new-session row painted");
        assert!(
            !tall[entry + 1].contains("Recent"),
            "a tall pane should breathe: {tall:#?}",
        );

        let tight = painted(&app, 120, 12);
        let entry = tight
            .iter()
            .position(|line| line.contains("New session"))
            .expect("new-session row painted");
        assert!(
            tight[entry + 1].contains("Recent"),
            "a tight pane kept rhythm it cannot afford: {tight:#?}",
        );
    }

    /// Print the card as the renderer actually paints it, for the evidence
    /// fixture. Ignored by default; run with
    /// `cargo test -p codewhale-tui --lib render_launch_card_fixture -- --ignored --nocapture`.
    #[test]
    #[ignore = "fixture generator, not an assertion"]
    #[allow(clippy::print_stdout)] // the fixture's whole job is its stdout
    fn render_launch_card_fixture() {
        let app = with_mcp(app_with_recent(
            &[
                "Fix the diagnostic display",
                "Hunter has explicitly authorized setting up Codewhale",
                "部署新的会话启动卡片并验证宽字符截断行为",
                "👩🏽\u{200d}🚀 crew 👨\u{200d}👩\u{200d}👧\u{200d}👦 families and flags 🇯🇵",
                "תהיה כרטיס פתיחת הפעלה",
            ],
            9,
        ));
        for (width, height) in [(170u16, 24u16), (120, 24), (80, 24), (40, 12), (20, 8)] {
            println!("\n{width}x{height}");
            println!("+{}+", "-".repeat(usize::from(width)));
            for line in painted(&app, width, height) {
                let pad = usize::from(width).saturating_sub(text_display_width(&line));
                println!("|{line}{}|", " ".repeat(pad));
            }
            println!("+{}+", "-".repeat(usize::from(width)));
        }
    }

    // --- the problems row runs the remedy it prints (#6085) -------------

    #[test]
    fn mcp_problems_row_joins_the_shared_row_ordering() {
        let mut app = with_mcp(app_with_recent(&["one", "two"], 9));
        refresh_launch_row_hitboxes(&mut app, Rect::new(0, 0, 120, 30));

        let ids = row_ids(&app);
        assert_eq!(
            ids.last(),
            Some(&LaunchRowId::McpRemedy),
            "the problems row is the last row in the painted ordering"
        );

        // Keyboard: arrowing onto the last row and pressing Enter runs the
        // remedy action, through the same arm a click reaches.
        let rows = launch_rows_for_app(&app);
        assert_eq!(
            rows.last().map(|row| row.id.clone()),
            Some(LaunchRowId::McpRemedy),
            "Up/Down must be able to land on the painted problems row"
        );
        assert_eq!(
            run_launch_card_row(&rows, Some(rows.len() - 1)),
            LaunchAction::McpRemedy
        );
        assert_eq!(
            launch_row_click_action(&LaunchRowId::McpRemedy),
            LaunchAction::McpRemedy,
            "click and Enter share one contract"
        );
    }

    #[test]
    fn launch_healthy_mcp_and_recent_rows_have_visible_focus_in_their_click_lane() {
        let mut app = with_mcp(app_with_recent(&["Recent proof"], 1));
        app.mcp_snapshot
            .as_mut()
            .unwrap()
            .servers
            .retain(|s| s.connected);
        app.mcp_configured_count = 5;
        for (width, height) in [(40, 12), (60, 16), (80, 24), (100, 32), (140, 40)] {
            let area = Rect::new(3, 2, width, height);
            refresh_launch_row_hitboxes(&mut app, area);
            let rows = launch_rows_for_app(&app);
            let mcp = rows
                .iter()
                .position(|r| r.id == LaunchRowId::McpManager)
                .unwrap();
            assert_eq!(
                run_launch_card_row(&rows, Some(mcp)),
                LaunchAction::McpManager
            );
            assert!(!row_ids(&app).contains(&LaunchRowId::McpRemedy));

            for index in [1, mcp] {
                app.launch.menu_selected = None;
                app.launch.hovered_row = Some(index);
                let hovered = launch_empty_state(&app, area);
                let (_, y) = hovered.rows[index];
                let hit = app.launch.row_hitboxes[index].1;
                assert_eq!(hit.x, area.x + hovered.text_column.x);
                assert_eq!(hit.y, area.y + y as u16);
                assert!(hit.right() <= area.right());
                let text = hovered.lines[y].spans.last().unwrap();
                assert_eq!(
                    text.style.bg,
                    crate::tui::menu_style::hovered_row_style().bg
                );

                app.launch.menu_selected = Some(index);
                let selected = launch_empty_state(&app, area);
                assert_eq!(
                    selected.lines[y].spans.last().unwrap().style,
                    crate::tui::menu_style::selected_row_bg_style().bold()
                );
                // Neither the whale nor the leading whitespace changes color.
                assert_eq!(selected.lines[y].spans[0].style.bg, None);
            }
        }
    }

    #[test]
    fn mcp_warning_ink_survives_selection_and_compact_layout() {
        let mut app = with_mcp(app_with_recent(&["Recent proof"], 1));
        for (width, height) in [(40, 12), (80, 24), (140, 40)] {
            let area = Rect::new(0, 0, width, height);
            let layout = launch_empty_state(&app, area);
            let index = layout
                .rows
                .iter()
                .position(|(id, _)| *id == LaunchRowId::McpManager)
                .unwrap();
            app.launch.menu_selected = Some(index);
            let selected = launch_empty_state(&app, area);
            let (_, y) = selected.rows[index];
            let summary = selected.lines[y]
                .spans
                .iter()
                .find(|span| span.content.starts_with("MCP"))
                .unwrap();
            assert_eq!(summary.style.fg, Some(app.ui_theme.error_fg));
            assert_eq!(
                summary.style.bg,
                crate::tui::menu_style::selected_row_bg_style().bg
            );
        }
    }

    #[test]
    fn mcp_remedy_action_types_the_command_into_the_composer() {
        let mut app = with_mcp(app_with_recent(&["one"], 9));
        app.launch.menu_selected = Some(0);

        crate::tui::ui::type_launch_mcp_remedy(&mut app);

        // `slack` is the fixture's first needs-login server; typing the
        // printed remedy beats copying it — no clipboard to depend on.
        assert_eq!(app.input, "/mcp login slack");
        assert_eq!(app.cursor_position, app.input.chars().count());
        assert_eq!(app.launch.menu_selected, None);
    }

    #[test]
    fn mcp_remedy_preserves_a_draft_and_opens_the_manager() {
        let mut app = with_mcp(app_with_recent(&["one"], 9));
        app.launch.return_to_session = true;
        app.input = "unsent draft".into();
        app.cursor_position = 4;
        crate::tui::ui::type_launch_mcp_remedy(&mut app);
        assert_eq!(app.input, "unsent draft");
        assert_eq!(app.cursor_position, 4);
        assert_eq!(
            app.view_stack.top_kind(),
            Some(crate::tui::views::ModalKind::Extensions),
        );
    }

    #[test]
    fn mcp_remedy_action_is_a_noop_when_nothing_is_wrong() {
        let mut app = app_with_recent(&["one"], 9);
        crate::tui::ui::type_launch_mcp_remedy(&mut app);
        assert!(app.input.is_empty());
    }

    /// The migration notice ends with the reassurance that nothing applies
    /// unapproved; a truncated notice drops exactly that half. The English
    /// line must fit the card measure whole at a wide terminal.
    #[test]
    fn claude_notice_is_never_truncated_on_a_wide_terminal() {
        let notice = codewhale_localization::tr(
            codewhale_localization::Locale::En,
            codewhale_localization::MessageId::LaunchNoticeClaude,
        );
        assert!(
            text_display_width(&notice) <= LAUNCH_CARD_MEASURE,
            "notice is {} cells; the card lane is {LAUNCH_CARD_MEASURE}: {notice:?}",
            text_display_width(&notice),
        );
        let mut app = app_with_recent(&["one"], 1);
        app.launch.claude_code_detected = true;
        let state = launch_empty_state(&app, Rect::new(0, 0, 160, 40));
        assert!(
            state
                .lines
                .iter()
                .any(|line| flatten(line).contains(notice.as_ref())),
            "the full notice must paint at 160 columns",
        );
    }

    #[test]
    fn every_hitbox_points_at_the_row_that_painted() {
        let app = app_with_recent(&["one", "two", "three"], 9);
        for height in 1u16..=24 {
            let state = launch_empty_state(&app, Rect::new(0, 0, 120, height));
            for (id, row) in &state.rows {
                let text = flatten(&state.lines[*row]);
                assert!(
                    !text.trim().is_empty(),
                    "height {height}: hitbox for {id:?} points at a blank row",
                );
            }
        }
    }
}

#[cfg(test)]
mod empty_state_caption_tests {
    use super::{empty_state_caption, shorten_workspace};
    use unicode_width::UnicodeWidthStr;

    const DEEP: &str = "/private/tmp/claude-501/-Volumes-VIXinSSD-CW-codewhale/34267917-11f4-4d15-911a-2a8acd5c49e1/scratchpad/surface/ws2";

    #[test]
    fn caption_stays_narrow_enough_to_actually_centre() {
        // The caller centres this line with `(width - caption.width()) / 2`.
        // Building it at full length and truncating to `width` made that inset
        // zero, so the caption rendered flush-left and full-bleed straight
        // through the centred whale/wordmark/prompt composition.
        for width in [60usize, 80, 100, 120] {
            let caption = empty_state_caption(DEEP, "no git", "MCP", 0, width);
            assert!(
                caption.width() <= width,
                "width {width}: caption {caption:?} overflows the lane",
            );
            assert!(
                width.saturating_sub(caption.width()) / 2 > 0,
                "width {width}: caption {caption:?} would render flush-left",
            );
        }
    }

    #[test]
    fn caption_keeps_the_folder_you_are_standing_in() {
        let long = "/a/very/deeply/nested/checkout/somewhere/far/away/myproject";
        for width in [40usize, 60, 80, 120] {
            let caption = empty_state_caption(long, "main", "MCP", 2, width);
            assert!(
                caption.contains("myproject"),
                "width {width}: {caption:?} dropped the current folder",
            );
        }
    }

    #[test]
    fn caption_sheds_the_least_important_detail_first() {
        let ws = "~/code/app";
        let wide = empty_state_caption(ws, "main", "MCP", 3, 120);
        assert!(wide.contains("MCP 3") && wide.contains("main") && wide.contains(ws));

        let mid = empty_state_caption(ws, "main", "MCP", 3, 24);
        assert!(
            !mid.contains("MCP"),
            "{mid:?} should shed the MCP count first"
        );
        assert!(mid.contains("main"), "{mid:?} should still name the branch");

        let tight = empty_state_caption(ws, "main", "MCP", 3, 16);
        assert!(
            tight.contains("app"),
            "{tight:?} should still name the folder"
        );
    }

    #[test]
    fn elision_lands_on_a_separator_not_mid_component() {
        // The old line ended in an ellipsis mid-directory
        // ("…/34267917-11f4-4d15-911a-"), which told the reader nothing.
        let caption = empty_state_caption(DEEP, "no git", "MCP", 0, 60);
        assert!(
            !caption.contains("2a8acd5c49e1"),
            "{caption:?} clipped mid-component"
        );
        if caption.starts_with('…') {
            assert!(
                caption.starts_with("…/"),
                "elision must land on a separator: {caption:?}",
            );
        }
    }

    #[test]
    fn caption_margin_scales_so_it_is_always_visibly_a_caption() {
        // The flat four-column margin only looked like a margin at 60 columns.
        // At 119 it let a 114-column path through with an inset of two — a
        // full-bleed banner cutting the centred composition in half, which is
        // the exact failure the shedding ladder exists to prevent.
        for width in [40usize, 60, 80, 100, 119, 120, 200] {
            for workspace in [DEEP, "/a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/project"] {
                let caption = empty_state_caption(workspace, "main", "MCP", 2, width);
                let inset = width.saturating_sub(caption.width()) / 2;
                assert!(
                    inset * 12 >= width,
                    "width {width}: caption {caption:?} insets by only {inset}",
                );
            }
        }
    }

    #[test]
    fn shorten_workspace_is_a_no_op_when_it_already_fits() {
        assert_eq!(shorten_workspace("~/code/app", 2), "~/code/app".to_string());
        assert_eq!(shorten_workspace("app", 2), "app".to_string());
    }
}

// ---------------------------------------------------------------------------
// Launch motion scheduling: the bounded mark reveal, a real dissolve, or
// an active water field requests frames through the existing scheduler.
// ---------------------------------------------------------------------------

/// Whether the launch screen has a visible transition or ambient scene.
#[must_use]
pub fn launch_motion_active(app: &App, obscured: bool, ambient_settled: bool) -> bool {
    if !app.launch.visible
        || obscured
        || app.onboarding != OnboardingState::None
        || !app.view_stack.is_empty()
        || !app.motion_policy().allows_decorative()
    {
        return false;
    }
    let now = app.ambient_clock_ms;
    let dissolve = app.launch.card_dissolve_progress(now, true);
    let dissolving = dissolve > 0.0 && dissolve < 1.0;
    let water_alive = app.theme_id == codewhale_palette::ThemeId::Underwater && !ambient_settled;
    let revealing = !app.launch.return_to_session
        && !crate::tui::color_compat::ascii_safe_enabled()
        && app
            .launch
            .mark_reveal_started_at
            .is_some_and(|started| started.elapsed().as_millis() < crate::tui::mark::REVEAL_MS);
    revealing || dissolving || water_alive
}

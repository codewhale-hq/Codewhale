use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tui::app::{App, ComposerSubmitChord};

const COMPOSER_ARROW_SCROLL_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EscapeAction {
    CloseSlashMenu,
    CancelRequest,
    PauseCommand,
    DiscardQueuedDraft,
    DismissPluginCta,
    ClearInput,
    Noop,
}

pub(crate) fn next_escape_action(app: &App, slash_menu_open: bool) -> EscapeAction {
    if slash_menu_open {
        EscapeAction::CloseSlashMenu
    } else if app.queued_draft.is_some() {
        EscapeAction::DiscardQueuedDraft
    } else if app.paused || app.paused_goal_objective.is_some() {
        EscapeAction::CancelRequest
    } else if app.pausable
        && !app.paused
        && !app.is_compacting
        && !app.manual_compaction_queued
        && (app.is_loading || matches!(app.runtime_turn_status.as_deref(), Some("in_progress")))
    {
        EscapeAction::PauseCommand
    } else if app.is_loading
        || app.is_compacting
        || app.manual_compaction_queued
        || app.goal_continuation_waiting
        || matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
    {
        EscapeAction::CancelRequest
    } else if !app.input.is_empty() {
        // A draft is the person's work: Esc clears it (recoverably) before it
        // dismisses a plugin offer (0.10.1 plugin offering policy, rule 9).
        EscapeAction::ClearInput
    } else if app.plugin_cta.phase.is_visible() {
        EscapeAction::DismissPluginCta
    } else {
        EscapeAction::Noop
    }
}

/// Rows one PageUp/PageDown travels in the slash menu. Pages clamp at the
/// ends per the shared vocabulary instead of wrapping (#6290).
const SLASH_MENU_PAGE: usize = 10;

/// Move the slash-menu selection by one shared-vocabulary motion (#6290).
/// Steps wrap; pages travel [`SLASH_MENU_PAGE`] rows and clamp. The menu is
/// single-column, so the region axis is a no-op.
pub(crate) fn move_slash_menu_selection(
    app: &mut App,
    entry_count: usize,
    motion: codewhale_tui_kit::list_nav::Motion,
) {
    if entry_count == 0 {
        return;
    }
    let selected = app.slash_menu_selected.min(entry_count.saturating_sub(1));
    if let Some(next) =
        codewhale_tui_kit::list_nav::apply(selected, entry_count, SLASH_MENU_PAGE, motion)
    {
        app.slash_menu_selected = next;
    }
}

pub(crate) fn select_previous_slash_menu_entry(app: &mut App, entry_count: usize) {
    move_slash_menu_selection(app, entry_count, codewhale_tui_kit::list_nav::Motion::Prev);
}

pub(crate) fn select_next_slash_menu_entry(app: &mut App, entry_count: usize) {
    move_slash_menu_selection(app, entry_count, codewhale_tui_kit::list_nav::Motion::Next);
}

pub(crate) fn handle_composer_history_arrow(
    app: &mut App,
    key: KeyEvent,
    slash_menu_open: bool,
    mention_menu_open: bool,
) -> bool {
    if slash_menu_open || mention_menu_open {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::SUPER) {
        return false;
    }

    // When `composer_arrows_scroll` is enabled, plain Up/Down scroll the
    // transcript for single-line drafts. Multiline drafts keep editor-like
    // line navigation. If the user holds Up/Down at the first/last line, do
    // not replace their current draft with prompt history unless they are
    // already navigating history — scroll the transcript instead. Terminals
    // that convert the wheel into arrow keys (iTerm2's alternate-screen
    // setting) reach the composer through this path, so a draft boundary that
    // merely redraws would strand the user with no way to scroll back (#5223).
    // A single logical line that soft-wraps across several visual rows is
    // treated the same way: Up/Down step between visual rows and only reach
    // history (or the transcript) from the first/last visual row.
    let scroll_transcript = app.composer_arrows_scroll && !app.input.contains('\n');
    let protect_multiline_draft = app.input.contains('\n') && app.history_index.is_none();

    match key.code {
        KeyCode::Up => {
            if move_cursor_visual_row(app, true) {
                // The cursor stepped to the visual row above, so the draft is
                // untouched and history is not recalled.
            } else if scroll_transcript
                || (protect_multiline_draft && !cursor_has_previous_logical_line(app))
            {
                app.scroll_up(COMPOSER_ARROW_SCROLL_LINES);
            } else {
                app.vim_move_up();
            }
            true
        }
        KeyCode::Down => {
            if move_cursor_visual_row(app, false) {
                // The cursor stepped to the visual row below, so the draft is
                // untouched and history is not recalled.
            } else if scroll_transcript
                || (protect_multiline_draft && !cursor_has_next_logical_line(app))
            {
                app.scroll_down(COMPOSER_ARROW_SCROLL_LINES);
            } else {
                app.vim_move_down();
            }
            true
        }
        _ => false,
    }
}

fn cursor_has_previous_logical_line(app: &App) -> bool {
    let cursor_byte = byte_index_at_char(&app.input, app.cursor_position);
    app.input[..cursor_byte].contains('\n')
}

fn cursor_has_next_logical_line(app: &App) -> bool {
    let cursor_byte = byte_index_at_char(&app.input, app.cursor_position);
    app.input[cursor_byte..].contains('\n')
}

fn byte_index_at_char(text: &str, char_index: usize) -> usize {
    if char_index == 0 {
        return 0;
    }
    text.char_indices()
        .nth(char_index)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

/// Step the cursor one visual row within a soft-wrapped single logical line,
/// returning whether it moved.
///
/// A long prompt with no newline still spans several screen rows; without this
/// the first Up recalls history and the draft visibly "disappears", which reads
/// as deletion. The wrapping reused here is the renderer's own
/// (`wrap_input_lines_for_mouse`) and the width is the last rendered composer
/// geometry, so key handling cannot disagree with what the user sees. History
/// navigation keeps its claim while an entry is on screen (`history_index` is
/// set). Callers fall through to the legacy scroll/history behavior when this
/// returns false: first/last visual row, a single visual row, or no rendered
/// geometry yet.
fn move_cursor_visual_row(app: &mut App, up: bool) -> bool {
    if app.history_index.is_some() || app.input.contains('\n') {
        return false;
    }
    let Some(plane) = app.viewport.last_composer_content else {
        return false;
    };
    let width =
        crate::tui::widgets::composer_content_geometry(plane, app.is_history_search_active())
            .text_width();
    let Some(cursor) = codewhale_ratatui::native_composer_step_row(
        &app.input,
        app.cursor_position,
        width,
        if up { -1 } else { 1 },
        codewhale_ratatui::NativeComposerRowColumn::DisplayCells,
    ) else {
        return false;
    };
    app.cursor_position = cursor;
    app.needs_redraw = true;
    true
}

pub(crate) fn is_word_cursor_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::ALT)
}

/// On macOS, map `SUPER` (Cmd ⌘) to `CONTROL` when `CONTROL` is not already
/// set, so that terminal emulators that don't pass Ctrl faithfully still work.
/// On all other platforms this is a no-op.
#[cfg(target_os = "macos")]
pub(crate) fn normalize_macos_modifiers(modifiers: KeyModifiers) -> KeyModifiers {
    // Strip SUPER and add CONTROL so that exact modifier equality checks
    // (e.g. `modifiers == KeyModifiers::CONTROL` in Ctrl+G/Ctrl+S stashing) work
    // correctly after normalization.
    if modifiers.contains(KeyModifiers::SUPER) {
        (modifiers - KeyModifiers::SUPER) | KeyModifiers::CONTROL
    } else {
        modifiers
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn normalize_macos_modifiers(modifiers: KeyModifiers) -> KeyModifiers {
    modifiers
}

pub(crate) fn handle_composer_alt_word_motion_key(app: &mut App, key: KeyEvent) -> bool {
    if !key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }

    match key.code {
        KeyCode::Char('f') | KeyCode::Char('F') => {
            app.clear_selection();
            app.move_cursor_word_forward();
            true
        }
        KeyCode::Char('b') | KeyCode::Char('B') => {
            app.clear_selection();
            app.move_cursor_word_backward();
            true
        }
        _ => false,
    }
}

/// Whether this terminal can deliver `Shift+Enter` as distinct from `Enter`.
///
/// Only the kitty keyboard protocol disambiguates them; a legacy terminal
/// sends the same byte for both, so the app never sees the modifier no matter
/// what the code does with it. macOS Terminal.app is the common case here.
/// Probed once — the answer cannot change for the life of the process.
pub(crate) fn terminal_can_report_shift_enter() -> bool {
    KEYBOARD_ENHANCEMENT_SUPPORTED
        .get()
        .copied()
        .unwrap_or(false)
}

static KEYBOARD_ENHANCEMENT_SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub(crate) fn probe_terminal_keyboard_enhancement() {
    let _ = KEYBOARD_ENHANCEMENT_SUPPORTED.get_or_init(|| {
        !cfg!(test) && crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false)
    });
}

pub(crate) fn is_composer_newline_key(key: KeyEvent, multiline_mode: bool) -> bool {
    match key.code {
        KeyCode::Char('j') => key.modifiers.contains(KeyModifiers::CONTROL),
        KeyCode::Enter => {
            (key.modifiers.contains(KeyModifiers::ALT)
                && (!multiline_mode || key.modifiers != KeyModifiers::ALT))
                || (key.modifiers.contains(KeyModifiers::SHIFT)
                    && !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !multiline_mode)
                || (key.modifiers == KeyModifiers::NONE && multiline_mode)
        }
        _ => false,
    }
}

pub(crate) fn is_forced_submit_key(key: KeyEvent) -> bool {
    matches!(
        composer_submit_chord(key, false),
        Some(ComposerSubmitChord::CtrlEnter)
    )
}

pub(crate) fn composer_submit_chord(
    key: KeyEvent,
    multiline_mode: bool,
) -> Option<ComposerSubmitChord> {
    if !matches!(key.code, KeyCode::Enter) {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return (multiline_mode && key.modifiers == KeyModifiers::ALT)
            .then_some(ComposerSubmitChord::Enter);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        Some(ComposerSubmitChord::CtrlEnter)
    } else if (key.modifiers == KeyModifiers::NONE && !multiline_mode)
        || (key.modifiers == KeyModifiers::SHIFT && multiline_mode)
    {
        Some(ComposerSubmitChord::Enter)
    } else {
        None
    }
}

pub(crate) fn handle_history_search_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter => {
            let _ = app.accept_history_search();
        }
        KeyCode::Esc => {
            app.cancel_history_search();
        }
        KeyCode::Char('c') | KeyCode::Char('C')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.cancel_history_search();
        }
        KeyCode::Backspace => {
            app.history_search_backspace();
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            while app
                .history_search_query()
                .is_some_and(|query| !query.is_empty())
            {
                app.history_search_backspace();
            }
        }
        KeyCode::Up => {
            app.history_search_select_previous();
        }
        KeyCode::Down => {
            app.history_search_select_next();
        }
        KeyCode::Char(ch)
            if key.modifiers.is_empty()
                || key.modifiers == KeyModifiers::SHIFT
                || key.modifiers == KeyModifiers::NONE =>
        {
            app.history_search_insert_char(ch);
        }
        _ => {}
    }
}

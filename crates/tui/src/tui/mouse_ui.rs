use std::time::{Duration, Instant};

/// Timing trace of the last left-click in the composer. crossterm never
/// decodes click counts, so double/triple-click detection keeps the prior
/// click's time and position; a fast click within the slop window increments
/// the count, anything else resets it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ComposerClickTrace {
    at: Instant,
    column: u16,
    row: u16,
    count: u8,
}

/// Two clicks closer than this are one double-click (composer word select,
/// and the context menu's guard against confirming a destructive row).
pub(crate) const DOUBLE_CLICK_MS: u64 = 400;
const COMPOSER_CLICK_SLOP_CELLS: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposerClickGesture {
    Caret,
    Word,
    Line,
}

fn classify_composer_click(
    trace: &mut Option<ComposerClickTrace>,
    column: u16,
    row: u16,
) -> ComposerClickGesture {
    let at = Instant::now();
    let next = match trace.as_ref() {
        Some(prev)
            if at.duration_since(prev.at).as_millis() <= u128::from(DOUBLE_CLICK_MS)
                && prev.row.abs_diff(row) <= COMPOSER_CLICK_SLOP_CELLS
                && prev.column.abs_diff(column) <= COMPOSER_CLICK_SLOP_CELLS =>
        {
            ComposerClickTrace {
                at,
                column,
                row,
                count: prev.count.saturating_add(1),
            }
        }
        _ => ComposerClickTrace {
            at,
            column,
            row,
            count: 1,
        },
    };
    let count = next.count;
    *trace = Some(next);
    match count {
        2 => ComposerClickGesture::Word,
        n if n >= 3 => ComposerClickGesture::Line,
        _ => ComposerClickGesture::Caret,
    }
}

/// Char-index bounds of the word (or CJK run) containing char `pos`.
///
/// Takes and returns char indices, the unit of `App::cursor_position` and
/// `App::selection_anchor`, so a multi-byte composer never mixes the two.
fn composer_word_bounds(text: &str, pos: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let is_word = |ch: char| ch.is_alphanumeric() || (ch as u32) >= 0x80;
    let idx = pos.min(chars.len() - 1);
    if !is_word(chars[idx]) {
        return (idx, idx + 1);
    }
    let mut start = idx;
    while start > 0 && is_word(chars[start - 1]) {
        start -= 1;
    }
    let mut end = idx + 1;
    while end < chars.len() && is_word(chars[end]) {
        end += 1;
    }
    (start, end)
}

/// Char-index bounds of the logical line containing char `pos` (excluding
/// the newline).
fn composer_line_bounds(text: &str, pos: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let pos = pos.min(chars.len());
    let start = chars[..pos]
        .iter()
        .rposition(|&ch| ch == '\n')
        .map_or(0, |i| i + 1);
    let end = chars[pos..]
        .iter()
        .position(|&ch| ch == '\n')
        .map_or(chars.len(), |offset| pos + offset);
    (start, end)
}

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::tui::app::{App, SidebarRowAction, StatusToastLevel};
use crate::tui::command_palette::{
    CommandPaletteView, build_entries_with_plugins as build_command_palette_entries,
};
use crate::tui::context_menu::{ContextMenuEntry, ContextMenuView};
use crate::tui::scrolling::{ScrollDirection, TranscriptScroll};
use crate::tui::selection::{SelectionAutoscroll, TranscriptSelectionPoint};
use crate::tui::tideline::InteractionAction;
use crate::tui::ui_text::{
    history_cell_to_clipboard_text, history_cell_to_text, line_to_plain, slice_visible_columns,
    text_display_width, text_visible_width, truncate_line_to_width,
};
use crate::tui::views::{ContextMenuAction, HelpView, ModalKind, ViewEvent};
use codewhale_localization::MessageId;
use codewhale_models::{ContentBlock, Message};

// These functions will need to be imported from ui.rs or we can just import crate::tui::ui::*.
use crate::tui::ui::{
    copy_cell_to_clipboard, detail_target_label, open_context_inspector,
    open_details_pager_for_cell, open_pager_for_selection,
};

const COMPOSER_MOUSE_SCROLL_LINES: usize = 3;

pub(crate) fn should_drop_loading_mouse_motion(app: &App, mouse: MouseEvent) -> bool {
    if !app.is_loading {
        return false;
    }

    match mouse.kind {
        // v0.9.1: keep a cheap hover hit-test alive while streaming. Motion
        // events are no longer dropped wholesale — the frame limiter bounds
        // redraw cost. Only expensive transcript reflow stays deferred.
        MouseEventKind::Moved => false,
        MouseEventKind::Drag(_) => {
            // Divider drags must stay live during active turns — dropping
            // these events wedges the resize state mid-drag (#3063).
            !app.viewport.transcript_selection.dragging
                && !app.viewport.transcript_scrollbar_dragging
                && !app.work_surface.is_resizing()
        }
        _ => false,
    }
}

fn toggle_tool_run_expand(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.tool_collapse_active() {
        return false;
    }
    let Some(rendered_idx) = transcript_cell_index_from_mouse(app, mouse) else {
        return false;
    };
    let original_idx = app.original_cell_index_for_rendered(rendered_idx);
    if app.tool_run_start_for_history_index(original_idx) != Some(original_idx) {
        return false;
    }
    app.toggle_tool_run_expansion_at(original_idx)
}

/// Map a mouse (column, row) within the composer area to a char index
/// in the composer input string. Uses the canonical prompt-adjusted text rect
/// for coordinate mapping, and accounts for vertical padding and scroll offset.
fn mouse_pos_to_char_index(app: &App, col: u16, row: u16, text_area: Rect) -> Option<usize> {
    Some(codewhale_ratatui::native_composer_source_at(
        &app.input,
        usize::from(text_area.width.max(1)),
        usize::from(col.saturating_sub(text_area.x)),
        usize::from(row.saturating_sub(text_area.y)),
        app.viewport.last_composer_scroll_offset,
        app.viewport.last_composer_top_padding,
    ))
}

/// Wheel dispatch stays with the existing editor; row projection is shared
/// with painting and caret geometry, preserving its scalar-column policy.
fn move_composer_cursor_by_wrapped_rows(app: &mut App, text_area: Rect, rows: isize) -> bool {
    let Some(cursor) = codewhale_ratatui::native_composer_step_row(
        &app.input,
        app.cursor_position,
        usize::from(text_area.width.max(1)),
        rows,
        codewhale_ratatui::NativeComposerRowColumn::SourceScalars,
    ) else {
        return false;
    };
    app.clear_selection();
    app.cursor_position = cursor;
    app.needs_redraw = true;
    true
}

/// A click on the workbar opens `/workflows`, the view that lists every run
/// with its agents and cancels one.
fn handle_workbar_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        || !mouse_hits_rect(mouse, app.viewport.last_workbar_area)
        || app.workflow_runs.is_empty()
    {
        return false;
    }
    crate::tui::views::workflows_manager::open(app);
    true
}

fn handle_plugin_cta_mouse(app: &mut App, mouse: MouseEvent) -> Option<Vec<ViewEvent>> {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return None;
    }
    if !mouse_hits_rect(mouse, app.viewport.last_plugin_cta_area) {
        return None;
    }
    if mouse_hits_rect(mouse, app.viewport.last_plugin_cta_dismiss_area) {
        // "Don't suggest again": the explicit, persisted dismissal.
        let _ = app.dismiss_plugin_cta();
        return Some(Vec::new());
    }
    // Only the labelled Review button opens the existing inventory. A click
    // elsewhere is consumed; no suggested command or mutation is dispatched.
    if mouse_hits_rect(mouse, app.viewport.last_plugin_cta_review_area)
        && let Some(tab) = app.accept_plugin_cta_review()
        && app.view_stack.top_kind() != Some(crate::tui::views::ModalKind::Extensions)
    {
        app.view_stack
            .push(crate::tui::views::extensions::ExtensionsView::new(app, tab));
    }
    Some(Vec::new())
}

/// Slash-autocomplete rows painted inside the composer. Click selects
/// (second click on the same row applies, matching the command palette);
/// wheel moves the highlight. Returns true when the event was consumed so
/// the composer caret / draft-scroll path does not also handle it.
fn handle_slash_autocomplete_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let hitboxes = app.viewport.last_slash_menu_hitboxes.borrow();
    if hitboxes.is_empty() {
        return false;
    }
    let over_row = hitboxes
        .iter()
        .find_map(|(idx, rect)| mouse_hits_rect(mouse, Some(*rect)).then_some(*idx));
    let over_menu = over_row.is_some()
        || hitboxes.iter().any(|(_, rect)| {
            mouse.row >= rect.y
                && mouse.row < rect.y.saturating_add(rect.height)
                && mouse.column >= rect.x
                && mouse.column < rect.x.saturating_add(rect.width)
        });
    // Wheel over any painted slash row moves selection (mouse == keys).
    // Clicks only fire when the pointer is on a row rect.
    match mouse.kind {
        MouseEventKind::ScrollUp if over_menu => {
            drop(hitboxes);
            let entries = crate::tui::slash_menu::visible_slash_menu_entries(app, 128);
            if entries.is_empty() {
                return false;
            }
            crate::tui::composer_ui::select_previous_slash_menu_entry(app, entries.len());
            app.needs_redraw = true;
            true
        }
        MouseEventKind::ScrollDown if over_menu => {
            drop(hitboxes);
            let entries = crate::tui::slash_menu::visible_slash_menu_entries(app, 128);
            if entries.is_empty() {
                return false;
            }
            crate::tui::composer_ui::select_next_slash_menu_entry(app, entries.len());
            app.needs_redraw = true;
            true
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(idx) = over_row else {
                return false;
            };
            drop(hitboxes);
            let entries = crate::tui::slash_menu::visible_slash_menu_entries(app, 128);
            if entries.is_empty() || idx >= entries.len() {
                return false;
            }
            // Same as command palette: click the highlighted row to apply;
            // click another row to move the highlight (mouse == keys).
            if app.slash_menu_selected == idx {
                let _ = crate::tui::slash_menu::apply_slash_menu_selection(app, &entries, true);
            } else {
                app.slash_menu_selected = idx;
                app.slash_menu_hidden = false;
            }
            app.needs_redraw = true;
            true
        }
        _ => false,
    }
}

/// Handle mouse events within the composer area.
/// Returns true if the event was consumed.
pub(crate) fn handle_composer_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.view_stack.is_empty() {
        return false;
    }
    // A transcript selection or scrollbar drag that ends over the composer
    // belongs to the surface that started it: the transcript handler must
    // still see the release to clear its drag state and publish the text.
    if matches!(
        mouse.kind,
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
    ) && (app.viewport.transcript_selection.dragging
        || app.viewport.transcript_scrollbar_dragging)
    {
        return false;
    }
    // Use outer area for hit-testing (includes border).
    let Some(area) = app.viewport.last_composer_area else {
        return false;
    };
    if mouse.column < area.x
        || mouse.column >= area.x + area.width
        || mouse.row < area.y
        || mouse.row >= area.y + area.height
    {
        return false;
    }
    // Slash autocomplete owns its painted rows before caret placement or
    // draft scroll — otherwise a click on `/model` would only move the caret.
    if handle_slash_autocomplete_mouse(app, mouse) {
        return true;
    }
    // Resolve the border- and submit-aware input plane through the same
    // persistent prompt geometry used by rendering, cursor placement, and
    // viewport bookkeeping. The frame records it after reserving `[↵]`.
    let input_plane = app.viewport.last_composer_content.unwrap_or(area);
    let text_area =
        crate::tui::widgets::composer_content_geometry(input_plane, app.is_history_search_active())
            .text_area;

    match mouse.kind {
        // Only claim the wheel while the caret still has somewhere to go. At
        // the top or bottom of the draft — or with no wrapped draft at all —
        // fall through so the transcript scrolls instead of the event being
        // silently swallowed by the composer rect (#5223).
        MouseEventKind::ScrollUp => move_composer_cursor_by_wrapped_rows(
            app,
            text_area,
            -(COMPOSER_MOUSE_SCROLL_LINES as isize),
        ),
        MouseEventKind::ScrollDown => move_composer_cursor_by_wrapped_rows(
            app,
            text_area,
            COMPOSER_MOUSE_SCROLL_LINES as isize,
        ),
        MouseEventKind::Down(MouseButton::Left) => {
            clear_transcript_selection(app);
            crate::tui::work_surface::release_focus(app);
            if let Some(submit) = crate::tui::widgets::active_composer_submit_rect(app, area)
                && mouse_hits_rect(mouse, Some(submit))
            {
                // Same chord the keyboard Enter path uses. Empty / paste-burst
                // clicks are consumed so they cannot also move the caret.
                let action =
                    app.decide_composer_submit(crate::tui::app::ComposerSubmitChord::Enter);
                if !matches!(action, crate::tui::app::ComposerSubmitAction::Noop)
                    && (app.composer_enter_would_submit(true)
                        || matches!(action, crate::tui::app::ComposerSubmitAction::SendQueuedNow))
                {
                    app.pending_composer_submit = Some(crate::tui::app::ComposerSubmitChord::Enter);
                }
                app.needs_redraw = true;
                return true;
            }
            if let Some(pos) = mouse_pos_to_char_index(app, mouse.column, mouse.row, text_area) {
                match classify_composer_click(
                    &mut app.viewport.composer_click_trace,
                    mouse.column,
                    mouse.row,
                ) {
                    ComposerClickGesture::Word => {
                        let (start, end) = composer_word_bounds(&app.input, pos);
                        app.selection_anchor = Some(start);
                        app.cursor_position = end;
                    }
                    ComposerClickGesture::Line => {
                        let (start, end) = composer_line_bounds(&app.input, pos);
                        app.selection_anchor = Some(start);
                        app.cursor_position = end;
                    }
                    ComposerClickGesture::Caret => {
                        app.cursor_position = pos;
                        app.selection_anchor = None;
                    }
                }
                app.needs_redraw = true;
            }
            true
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(pos) = mouse_pos_to_char_index(app, mouse.column, mouse.row, text_area) {
                if app.selection_anchor.is_none() {
                    app.selection_anchor = Some(app.cursor_position);
                }
                app.cursor_position = pos;
                app.needs_redraw = true;
            }
            true
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if app.selection_anchor == Some(app.cursor_position) {
                app.selection_anchor = None;
            }
            true
        }
        MouseEventKind::Down(MouseButton::Middle) if app.clipboard.uses_primary_selection() => {
            if let Some(text) = app.clipboard.read_primary_text() {
                // Flush already-typed bytes at their original caret first.
                app.insert_paste_text("");
                let Some(position) =
                    mouse_pos_to_char_index(app, mouse.column, mouse.row, text_area)
                else {
                    return true;
                };
                // PRIMARY often contains this very selection. Insert at the
                // pointer, preserving the selected original rather than cutting it.
                app.selection_anchor = None;
                app.cursor_position = position;
                crate::tui::work_surface::release_focus(app);
                app.insert_paste_text(&text);
            }
            true
        }
        _ => false,
    }
}

pub(crate) fn handle_mouse_event(app: &mut App, mouse: MouseEvent) -> Vec<ViewEvent> {
    if app.view_stack.top_kind() == Some(ModalKind::ContextMenu) {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Right)) {
            app.view_stack.pop();
            // A menu opened over a modal (the Extensions row menu) hands the
            // new right-click back to that modal, not to the transcript.
            if !app.view_stack.is_empty() {
                app.needs_redraw = true;
                return app.view_stack.handle_mouse(mouse);
            }
            open_context_menu(app, mouse);
            return Vec::new();
        }
        return app.view_stack.handle_mouse(mouse);
    }

    // Decision prompts leave transcript evidence visible above them. A question
    // sheet owns the wheel over its content; approval cards retain their existing
    // transcript-scroll behavior. Visible side surfaces keep their ownership.
    // Other modals still own wheel input exclusively (#4371, #6045).
    if matches!(
        app.view_stack.top_kind(),
        Some(ModalKind::Approval | ModalKind::UserInput)
    ) {
        let over_prompt = mouse_hits_rect(mouse, app.viewport.last_prompt_area);
        let over_side_surface = mouse_hits_rect(mouse, app.work_surface.last_area);
        let direction = match mouse.kind {
            MouseEventKind::ScrollUp => Some(ScrollDirection::Up),
            MouseEventKind::ScrollDown => Some(ScrollDirection::Down),
            _ => None,
        };
        if let Some(direction) = direction {
            if over_prompt && app.view_stack.top_kind() == Some(ModalKind::UserInput) {
                app.needs_redraw = true;
                return app.view_stack.handle_mouse(mouse);
            }
            if over_prompt || !over_side_surface {
                scroll_transcript_with_mouse(app, direction);
            }
            return Vec::new();
        }
    }

    if !app.view_stack.is_empty() {
        app.needs_redraw = true;
        return app.view_stack.handle_mouse(mouse);
    }

    if crate::tui::pet_watch::main_view(app) {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && mouse_hits_rect(mouse, app.pet_watch.inspect_area)
        {
            crate::tui::pet_watch::open_habitat(app);
            return Vec::new();
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && mouse_hits_rect(mouse, app.pet_watch.view_area)
        {
            crate::tui::pet_watch::cycle_view(app, true);
            return Vec::new();
        }
        if mouse_hits_rect(mouse, app.pet_watch.area) {
            crate::tui::pet_watch::command(app, crate::tui::pet_watch::Control::Mouse(mouse));
            return Vec::new();
        }
    }

    // A drag can finish outside the composer/transcript that started it.
    // Publish once before other visible surfaces consume the release event.
    if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left))
        && app.clipboard.uses_primary_selection()
    {
        let text = if app.viewport.transcript_selection.dragging {
            selection_to_text(app).unwrap_or_default()
        } else {
            app.selected_text()
        };
        let _ = app.clipboard.write_primary_text(&text);
    }

    // Topbar facts are typed controls, not decorative text. Route this before
    // either launch or session content so a segment painted in the one shared
    // header has identical mouse behavior in both shell states.
    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        let action = app
            .viewport
            .interaction_targets
            .target_at(mouse.column, mouse.row)
            .and_then(|target| target.mouse_action);
        if let Some(action) = action
            && !matches!(
                action,
                InteractionAction::ShowDockPanel(_) | InteractionAction::DismissDock
            )
        {
            app.needs_redraw = true;
            return match action {
                InteractionAction::InspectContext => {
                    open_context_inspector(app);
                    Vec::new()
                }
                InteractionAction::OpenProviderPicker => {
                    vec![ViewEvent::TopbarRoutePickerRequested]
                }
                InteractionAction::OpenAutomations => apply_sidebar_row_action(
                    app,
                    SidebarRowAction::Command("/automation".to_string()),
                ),
                InteractionAction::OpenModelPicker => {
                    vec![ViewEvent::TopbarModelPickerRequested]
                }
                InteractionAction::ShowDockPanel(_) | InteractionAction::DismissDock => {
                    unreachable!("dock targets defer to the strip")
                }
            };
        }
    }

    // The launch card is content on the ordinary screen, not a surface that
    // owns the frame. It used to consume every mouse event and return, which
    // was right when it *was* a separate surface and became a bug the moment
    // it stopped being one: scrolling, the real composer, the work surface
    // and every other target were unreachable while it was up, and the
    // send-glyph branch pointed at a `send_area` the deleted launch composer
    // used to set. So the card takes its own rows and lets everything else
    // fall through to the handlers that own it.
    if app.launch.visible && !app.launch.row_hitboxes.is_empty() {
        let hit = app
            .launch
            .row_hitboxes
            .iter()
            .position(|(_, area)| mouse_hits_rect(mouse, Some(*area)));
        match mouse.kind {
            MouseEventKind::Moved => {
                if hit != app.launch.hovered_row {
                    app.launch.hovered_row = hit;
                    app.needs_redraw = true;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = hit {
                    let id = app.launch.row_hitboxes[index].0.clone();
                    match &id {
                        // Resuming replaces the whole session context —
                        // founder live-test: "you just click it and boom
                        // you're there ... you don't realize it's happening".
                        // It asks first. New session and See all stay one
                        // click, because neither discards anything.
                        crate::tui::app::LaunchRowId::Recent(session_id) => {
                            app.launch.menu_selected = Some(index);
                            crate::tui::underwater::open_launch_resume_confirm(app, session_id);
                        }
                        _ => {
                            app.launch.status = None;
                            app.pending_launch_action =
                                Some(crate::tui::underwater::launch_row_click_action(&id));
                        }
                    }
                    app.needs_redraw = true;
                    return Vec::new();
                }
            }
            _ => {}
        }
    }

    // Ocean work surface owns its rect, scrolling, focus, and row actions.
    // Route it before workflow/composer/transcript so wheel events never leak
    // into an unrelated viewport.
    let work_surface = crate::tui::work_surface::handle_mouse(app, mouse);
    if let Some(action) = work_surface.action {
        return apply_sidebar_row_action(app, action);
    }
    if work_surface.consumed {
        return Vec::new();
    }
    // The posture bar's live counts open the dock view they count. The
    // strip's own tabs were consumed above; anything left carrying a dock
    // action is a footer chip.
    if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
        && let Some(InteractionAction::ShowDockPanel(panel)) = app
            .viewport
            .interaction_targets
            .target_at(mouse.column, mouse.row)
            .and_then(|target| target.mouse_action)
    {
        crate::tui::work_surface::select_dock_panel(app, panel);
        // Clicking the affordance teaches it just as well as the chord does.
        app.note_footer_hint_used(crate::tui::footer_hints::DOCK_OPEN);
        return Vec::new();
    }

    if handle_workbar_mouse(app, mouse) {
        return Vec::new();
    }

    if let Some(events) = handle_plugin_cta_mouse(app, mouse) {
        return events;
    }

    // Composer mouse events take priority over transcript.
    if handle_composer_mouse(app, mouse) {
        return Vec::new();
    }

    match mouse.kind {
        MouseEventKind::Moved => {
            // Update last mouse position for tooltip rendering + hover layer.
            app.last_mouse_pos = Some((mouse.column, mouse.row));
            let previous_hover = crate::tui::hover_layer::current_hover();
            crate::tui::hover_layer::set_pointer(mouse.column, mouse.row);
            crate::tui::hover_layer::resolve_hover();
            if crate::tui::hover_layer::current_hover() != previous_hover {
                app.needs_redraw = true;
            }

            // Check sidebar sections for hover popovers. Only surface a
            // popover when the hovered row lost information in the compact
            // sidebar view.
            let mut found = false;
            for section in &app.sidebar_hover.sections {
                if mouse.column >= section.content_area.x
                    && mouse.column
                        < section
                            .content_area
                            .x
                            .saturating_add(section.content_area.width)
                    && mouse.row >= section.content_area.y
                    && mouse.row
                        < section
                            .content_area
                            .y
                            .saturating_add(section.content_area.height)
                {
                    if let Some(row) = section.rows.iter().find(|row| row.row_y == mouse.row) {
                        let desired = row.is_truncated.then(|| {
                            if let Some(detail) = row.detail.as_deref()
                                && !detail.trim().is_empty()
                            {
                                format!("{}\n{detail}", row.full_text)
                            } else {
                                row.full_text.clone()
                            }
                        });
                        if app.sidebar_hover_tooltip != desired {
                            app.sidebar_hover_tooltip = desired;
                            app.needs_redraw = true;
                        }
                        found = true;
                        break;
                    } else if section.rows.is_empty() {
                        let line_idx = (mouse.row.saturating_sub(section.content_area.y)) as usize;
                        if let Some(full) = section.lines.get(line_idx) {
                            let truncated =
                                text_display_width(full) > section.content_area.width as usize;
                            let desired = truncated.then(|| full.clone());
                            if app.sidebar_hover_tooltip != desired {
                                app.sidebar_hover_tooltip = desired;
                                app.needs_redraw = true;
                            }
                            found = true;
                            break;
                        }
                    }
                }
            }
            if !found && app.sidebar_hover_tooltip.is_some() {
                app.sidebar_hover_tooltip = None;
                app.needs_redraw = true;
            }
        }
        MouseEventKind::ScrollUp => {
            scroll_transcript_with_mouse(app, ScrollDirection::Up);
        }
        MouseEventKind::ScrollDown => {
            scroll_transcript_with_mouse(app, ScrollDirection::Down);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            app.viewport.transcript_scrollbar_dragging = false;
            app.viewport.selection_autoscroll = None;

            // #3028/#4009: Check sidebar hover state for clickable rows before
            // falling through to transcript selection. Command rows still use
            // the command-palette pipeline; agent rows are direct UI actions.
            if let Some(action) = sidebar_click_action(app, mouse) {
                return apply_sidebar_row_action(app, action);
            }

            // Click on the transcript scrollbar gutter starts a scrollbar
            // drag so the visible thumb remains interactive for users who
            // prefer mouse-based navigation.
            if mouse_hits_transcript_scrollbar(app, mouse) {
                app.viewport.transcript_scrollbar_dragging = true;
                return Vec::new();
            }

            if mouse_hits_rect(mouse, app.viewport.jump_to_latest_button_area) {
                app.scroll_to_bottom();
                return Vec::new();
            }

            // The pinned prompt header names the user message scrolled just
            // above the viewport; a click returns to it. Resolve the message
            // against the current layout so a rewrite between paint and click
            // cannot jump to a stale line offset.
            if mouse_hits_rect(mouse, app.viewport.pinned_prompt_area) {
                if let Some(line) = app.pinned_prompt_target_line() {
                    app.scroll_to_transcript_line(line);
                }
                return Vec::new();
            }

            if toggle_tool_run_expand(app, mouse) {
                return Vec::new();
            }

            if let Some(point) = selection_point_from_mouse(app, mouse) {
                app.viewport.transcript_selection.anchor = Some(point);
                app.viewport.transcript_selection.head = Some(point);
                app.viewport.transcript_selection.dragging = true;
                app.viewport.transcript_selection.owner =
                    app.transcript_owner_at_line(point.line_index);
                app.needs_redraw = true;

                if app.is_loading
                    && app.viewport.transcript_scroll.is_at_tail()
                    && let Some(anchor) = TranscriptScroll::anchor_for(
                        app.viewport.transcript_cache.line_meta(),
                        app.viewport.last_transcript_top,
                    )
                {
                    app.viewport.transcript_scroll = anchor;
                }
            } else {
                clear_transcript_selection(app);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.viewport.transcript_scrollbar_dragging {
                scroll_transcript_to_mouse_row(app, mouse.row);
                return Vec::new();
            }

            if app.viewport.transcript_selection.dragging {
                update_selection_drag(app, mouse);
            }
        }
        MouseEventKind::Up(MouseButton::Left) if app.viewport.transcript_scrollbar_dragging => {
            app.viewport.transcript_scrollbar_dragging = false;
            app.viewport.selection_autoscroll = None;
            app.needs_redraw = true;
        }
        MouseEventKind::Up(MouseButton::Left) if app.viewport.transcript_selection.dragging => {
            app.viewport.transcript_selection.dragging = false;
            app.viewport.selection_autoscroll = None;
            if selection_has_content(app) && !app.clipboard.uses_primary_selection() {
                copy_active_selection(app);
            }
        }
        MouseEventKind::Down(MouseButton::Right) => {
            open_context_menu(app, mouse);
        }
        _ => {}
    }

    Vec::new()
}

fn scroll_transcript_with_mouse(app: &mut App, direction: ScrollDirection) {
    let update = app.viewport.mouse_scroll.on_scroll(direction);
    app.viewport.pending_scroll_delta = app
        .viewport
        .pending_scroll_delta
        .saturating_add(update.delta_lines);
    if update.delta_lines != 0 {
        app.user_scrolled_during_stream = true;
        app.needs_redraw = true;
    }
}

/// Resolve a right-click in the sidebar to the hovered row's full copyable
/// text: the row's untruncated text plus its hover detail when present.
fn sidebar_row_copy_text(app: &App, mouse: MouseEvent) -> Option<String> {
    for section in &app.sidebar_hover.sections {
        if !mouse_hits_rect(mouse, Some(section.content_area)) {
            continue;
        }
        if let Some(row) = section.rows.iter().find(|row| row.row_y == mouse.row) {
            let mut text = row.full_text.clone();
            if let Some(detail) = row.detail.as_deref()
                && !detail.trim().is_empty()
            {
                text.push('\n');
                text.push_str(detail);
            }
            return Some(text).filter(|text| !text.trim().is_empty());
        }
        let line_idx = (mouse.row.saturating_sub(section.content_area.y)) as usize;
        if let Some(full) = section.lines.get(line_idx) {
            return Some(full.clone()).filter(|text| !text.trim().is_empty());
        }
    }
    None
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

/// Resolve a left-click in the sidebar to a typed row action, if the clicked
/// row has a click action assigned (#3028, #4009).
fn sidebar_click_action(app: &App, mouse: MouseEvent) -> Option<SidebarRowAction> {
    let row = sidebar_row_at(app, mouse)?;
    if let (Some(action), Some(start), Some(end)) = (
        row.stop_action.as_ref(),
        row.stop_zone_start_col,
        row.stop_zone_end_col,
    ) && mouse.column >= start
        && mouse.column < end
    {
        return Some(action.clone());
    }
    row.click_action.clone()
}

/// The work-surface row under the pointer, if any.
fn sidebar_row_at(app: &App, mouse: MouseEvent) -> Option<&crate::tui::app::SidebarHoverRow> {
    for section in &app.sidebar_hover.sections {
        if mouse.column >= section.content_area.x
            && mouse.column
                < section
                    .content_area
                    .x
                    .saturating_add(section.content_area.width)
            && mouse.row >= section.content_area.y
            && mouse.row
                < section
                    .content_area
                    .y
                    .saturating_add(section.content_area.height)
            && let Some(row) = section.rows.iter().find(|row| row.row_y == mouse.row)
        {
            return Some(row);
        }
    }
    None
}

pub(crate) fn apply_sidebar_row_action(app: &mut App, action: SidebarRowAction) -> Vec<ViewEvent> {
    match action {
        SidebarRowAction::Command(command) => {
            use crate::tui::views::CommandPaletteAction;
            vec![ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }]
        }
        SidebarRowAction::PrefillCommand(command) => {
            app.input = command;
            app.cursor_position = app.input.len();
            app.status_message = Some(app.tr(MessageId::SidebarDestructiveArmed).into_owned());
            app.needs_redraw = true;
            Vec::new()
        }
        SidebarRowAction::ShowSubagentsPanel => {
            use crate::tui::work_surface::RailPanel;
            // The register header is a two-way door: opening the Agents panel
            // from anywhere, and returning to Tasks when it is already open,
            // so the to-do list is never one click away with no way back.
            let target = match app.work_surface.panel {
                RailPanel::Agents => RailPanel::Tasks,
                _ => RailPanel::Agents,
            };
            crate::tui::work_surface::select_dock_panel(app, target);
            app.status_message = Some(
                match target {
                    RailPanel::Agents => "Showing subagents",
                    _ => "Showing tasks",
                }
                .to_string(),
            );
            app.needs_redraw = true;
            Vec::new()
        }
        SidebarRowAction::OpenAgentDetail { agent_id } => {
            if !crate::tui::agent_details::open_agent_details(app, &agent_id) {
                crate::tui::work_surface::agent_details_closed(app, &agent_id);
                app.status_message = Some("Agent details are unavailable".to_string());
            }
            app.needs_redraw = true;
            Vec::new()
        }
        SidebarRowAction::OpenAgentTranscript { agent_id } => {
            // The primary agent destination: focus the worker in place. The
            // focused view explains a missing capture instead of dead-ending.
            crate::tui::agent_focus::focus_agent(app, &agent_id);
            app.needs_redraw = true;
            Vec::new()
        }
        SidebarRowAction::CancelAgent { agent_id } => {
            vec![ViewEvent::SidebarAgentCancel { agent_id }]
        }
        SidebarRowAction::InspectWork {
            title,
            body,
            stop_action,
        } => {
            let width = app
                .viewport
                .last_transcript_area
                .map(|area| area.width)
                .unwrap_or(80);
            let mut pager =
                crate::tui::pager::PagerView::from_text(title, &body, width.saturating_sub(2))
                    .with_copy_text(body);
            let stop_event = stop_action.and_then(|action| match *action {
                SidebarRowAction::Command(command) => {
                    use crate::tui::views::CommandPaletteAction;
                    Some(ViewEvent::CommandPaletteSelected {
                        action: CommandPaletteAction::ExecuteCommand { command },
                    })
                }
                SidebarRowAction::CancelAgent { agent_id } => {
                    Some(ViewEvent::SidebarAgentCancel { agent_id })
                }
                _ => None,
            });
            if let Some(event) = stop_event {
                pager = pager.with_destructive_action(
                    's',
                    app.tr(MessageId::SidebarStopControl),
                    app.tr(MessageId::WorkSurfaceStopConfirmHint),
                    event,
                );
            }
            app.view_stack.push(pager);
            app.needs_redraw = true;
            Vec::new()
        }
    }
}

pub(crate) fn resolve_agent_transcript_text(app: &App, agent_id: &str) -> Option<String> {
    use crate::tools::handle::{HandleValue, VarHandle};

    let lookup = VarHandle {
        kind: "var_handle".to_string(),
        session_id: format!("agent:{agent_id}"),
        name: "full_transcript".to_string(),
        type_name: String::new(),
        length: 0,
        repr_preview: String::new(),
        sha256: String::new(),
    };
    let payload = match app.runtime_services.handle_store.try_lock() {
        Ok(store) => match store.get(&lookup) {
            Some(record) => match &record.value {
                HandleValue::Json(value) => Some(value.clone()),
                HandleValue::Text(_) => None,
            },
            None => None,
        },
        Err(_) => return None,
    };

    // The handle is a deliberately bounded live projection. Prefer the private
    // on-disk message stream so Open means the entire chat, including early
    // turns that no longer fit in the 1 MiB resident tail. While the worker is
    // live, require its artifact count to match the latest handle count; a
    // failed/stale append must fall back to the explicit omission banner. With
    // no process-local handle (for example after restart), the validated
    // artifact remains the durable source of truth.
    if let Ok(display) =
        crate::tools::subagent::load_subagent_transcript_display_artifact(&app.workspace, agent_id)
    {
        let matches_resident_count = payload.as_ref().is_none_or(|resident| {
            resident
                .get("message_count")
                .and_then(serde_json::Value::as_u64)
                .and_then(|count| usize::try_from(count).ok())
                == Some(display.conversation_count)
                && resident
                    .get("complete_transcript_artifact")
                    .and_then(|artifact| artifact.get("complete"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true)
        });
        if matches_resident_count {
            let text = agent_messages_text(&display.messages);
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }

    let payload = payload?;
    let text = agent_transcript_text(&payload);
    if text.trim().is_empty() {
        return None;
    }
    Some(text)
}

pub(crate) fn agent_transcript_evidence_available(app: &App, agent_id: &str) -> bool {
    resolve_agent_transcript_text(app, agent_id).is_some()
}

/// Turn the agent transcript handle into a readable conversation. The worker
/// may retain tool calls and results, but private model thinking never appears
/// here; the parent transcript has the same default privacy behavior.
fn agent_transcript_text(payload: &serde_json::Value) -> String {
    let Some(messages) = payload
        .get("messages")
        .and_then(serde_json::Value::as_array)
    else {
        return String::new();
    };

    let omitted = payload
        .get("omitted_messages")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let total = payload
        .get("message_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(messages.len() as u64);
    let mut text = String::new();
    if omitted > 0 {
        text.push_str(&format!(
            "Showing the latest {} of {total} worker messages. Earlier messages were omitted from the in-memory transcript.\n\n",
            messages.len()
        ));
    }

    let parsed: Vec<Message> = messages
        .iter()
        .filter_map(|raw| serde_json::from_value::<Message>(raw.clone()).ok())
        .collect();
    text.push_str(&agent_messages_text(&parsed));
    text
}

fn agent_messages_text(messages: &[Message]) -> String {
    let mut text = String::new();
    for message in messages {
        let body = agent_message_text(message);
        if body.trim().is_empty() {
            continue;
        }
        text.push_str(&format!("── {} ──\n{body}\n\n", message.role));
    }
    text
}

fn agent_message_text(message: &Message) -> String {
    let mut text = String::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: body, .. } => {
                if !body.trim().is_empty() {
                    text.push_str(body);
                    text.push('\n');
                }
            }
            ContentBlock::ToolUse { name, input, .. }
            | ContentBlock::ServerToolUse { name, input, .. } => {
                text.push_str(&format!(
                    "→ {name}\n{}\n",
                    serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string())
                ));
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } => {
                let label = if is_error.unwrap_or(false) {
                    "← tool error"
                } else {
                    "← tool result"
                };
                text.push_str(&format!("{label} ({tool_use_id})\n{content}\n"));
            }
            ContentBlock::ImageUrl { image_url } => {
                text.push_str(&format!("[image: {}]\n", image_url.url));
            }
            // Thinking blocks are deliberately not surfaced in the main TUI
            // and should not leak through a worker detail view either.
            ContentBlock::Thinking { .. } => {}
            other => {
                text.push_str(&format!(
                    "{}\n",
                    serde_json::to_string_pretty(other).unwrap_or_else(|_| "[worker event]".into())
                ));
            }
        }
    }
    text.trim_end().to_string()
}

pub(crate) fn mouse_hits_transcript_scrollbar(app: &App, mouse: MouseEvent) -> bool {
    let Some(area) = app.viewport.last_transcript_area else {
        return false;
    };
    if area.width <= 1 || app.viewport.last_transcript_total <= app.viewport.last_transcript_visible
    {
        return false;
    }

    let scrollbar_col = area.x.saturating_add(area.width.saturating_sub(1));
    mouse.column == scrollbar_col
        && mouse.row >= area.y
        && mouse.row < area.y.saturating_add(area.height)
}

pub(crate) fn scroll_transcript_to_mouse_row(app: &mut App, row: u16) -> bool {
    let Some(area) = app.viewport.last_transcript_area else {
        return false;
    };
    let total = app.viewport.last_transcript_total;
    let visible = app.viewport.last_transcript_visible;
    if area.height == 0 || total <= visible {
        return false;
    }

    let max_start = total.saturating_sub(visible);
    if max_start == 0 {
        app.scroll_to_bottom();
        return true;
    }

    let max_row = usize::from(area.height.saturating_sub(1));
    let relative_row = usize::from(row.saturating_sub(area.y)).min(max_row);
    let numerator = relative_row
        .saturating_mul(max_start)
        .saturating_add(max_row / 2);
    // Round to the nearest transcript offset so short thumbs still feel
    // responsive on compact terminals.
    let top = numerator.checked_div(max_row).unwrap_or(0);

    app.viewport.transcript_scroll = if top >= max_start {
        TranscriptScroll::to_bottom()
    } else {
        TranscriptScroll::at_line(top)
    };
    app.viewport.pending_scroll_delta = 0;
    app.user_scrolled_during_stream = !app.viewport.transcript_scroll.is_at_tail();
    app.needs_redraw = true;
    true
}

/// Cadence between auto-scroll ticks while drag-selecting past the
/// transcript edge (#1163). 30 ms ≈ 33 lines/sec, comparable to the feel
/// of a steady scroll-wheel drag.
const SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(30);

/// Update the transcript selection while the left button is dragging.
/// When the mouse leaves the transcript rect vertically, arm
/// `selection_autoscroll` so the main loop can advance the viewport on a
/// fixed cadence; when the mouse returns inside, disarm it.
pub(crate) fn update_selection_drag(app: &mut App, mouse: MouseEvent) {
    if let Some(point) = selection_point_from_mouse(app, mouse) {
        app.viewport.transcript_selection.head = Some(point);
        app.viewport.selection_autoscroll = None;
        app.needs_redraw = true;
        return;
    }

    let Some(area) = app.viewport.last_transcript_area else {
        return;
    };
    if area.height == 0 || area.width == 0 {
        return;
    }

    let direction = if mouse.row < area.y {
        -1
    } else if mouse.row >= area.y.saturating_add(area.height) {
        1
    } else {
        // Outside horizontally only — leave selection head where it is.
        return;
    };

    let max_col = area.x.saturating_add(area.width.saturating_sub(1));
    let column = mouse.column.clamp(area.x, max_col);

    // Fire on the next tick immediately by setting `next_tick` to now.
    app.viewport.selection_autoscroll = Some(SelectionAutoscroll {
        direction,
        column,
        next_tick: Instant::now(),
    });
    app.needs_redraw = true;
}

/// Advance the drag-edge auto-scroll one step if its cadence has elapsed.
/// Called once per main-loop iteration.
pub(crate) fn tick_selection_autoscroll(app: &mut App) {
    let Some(state) = app.viewport.selection_autoscroll else {
        return;
    };

    if !app.viewport.transcript_selection.dragging {
        app.viewport.selection_autoscroll = None;
        return;
    }

    let Some(area) = app.viewport.last_transcript_area else {
        return;
    };
    if area.height == 0 {
        return;
    }

    let now = Instant::now();
    if now < state.next_tick {
        return;
    }

    app.viewport.pending_scroll_delta = app
        .viewport
        .pending_scroll_delta
        .saturating_add(state.direction);
    app.user_scrolled_during_stream = true;

    let edge_row = if state.direction < 0 {
        area.y
    } else {
        area.y.saturating_add(area.height.saturating_sub(1))
    };
    if let Some(point) = selection_point_from_position(
        area,
        state.column,
        edge_row,
        app.viewport.last_transcript_top,
        app.viewport.last_transcript_total,
        app.viewport.last_transcript_padding_top,
    ) {
        app.viewport.transcript_selection.head = Some(point);
    }

    app.viewport.selection_autoscroll = Some(SelectionAutoscroll {
        next_tick: now + SELECTION_AUTOSCROLL_INTERVAL,
        ..state
    });
    app.needs_redraw = true;
}

pub(crate) fn mouse_hits_rect(mouse: MouseEvent, area: Option<Rect>) -> bool {
    point_hits_rect(mouse.column, mouse.row, area)
}

fn point_hits_rect(column: u16, row: u16, area: Option<Rect>) -> bool {
    let Some(area) = area else {
        return false;
    };

    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

pub(crate) fn open_context_menu(app: &mut App, mouse: MouseEvent) {
    let entries = build_context_menu_entries(app, mouse);
    if entries.is_empty() {
        return;
    }
    let title = app.tr(MessageId::CtxMenuTitle).to_string();
    push_context_menu(app, entries, mouse.column, mouse.row, title);
}

/// Open a context menu at a screen position. Shared by the transcript/work
/// surface menu and by modal views that ask for one over themselves.
pub(crate) fn push_context_menu(
    app: &mut App,
    entries: Vec<ContextMenuEntry>,
    column: u16,
    row: u16,
    title: String,
) {
    let reduced = app.motion_policy().as_low_motion();
    app.view_stack.push(ContextMenuView::new_with_motion(
        entries, column, row, title, reduced,
    ));
    app.needs_redraw = true;
}

pub(crate) fn build_context_menu_entries(app: &App, mouse: MouseEvent) -> Vec<ContextMenuEntry> {
    let mut entries = Vec::new();
    let on_work_surface = mouse_hits_rect(mouse, app.work_surface.last_area);

    if on_work_surface {
        push_work_row_entries(app, mouse, &mut entries);
    } else {
        // Paste first — the most common action when right-clicking in the
        // composer or transcript after copying text from the output area.
        entries.push(
            ContextMenuEntry::new(
                app.tr(MessageId::CtxMenuPaste),
                app.tr(MessageId::CtxMenuPasteDesc),
                ContextMenuAction::Paste,
            )
            .with_glyph("📋")
            .with_hint("p")
            .primary(),
        );
    }
    let mut targeted = !entries.is_empty() && on_work_surface;

    if selection_has_content(app) {
        targeted = true;
        entries.push(
            ContextMenuEntry::new(
                app.tr(MessageId::CtxMenuCopySelection),
                app.tr(MessageId::CtxMenuCopySelectionDesc),
                ContextMenuAction::CopySelection,
            )
            .with_glyph("⎘")
            .with_hint("y")
            .section_start(),
        );
        entries.push(
            ContextMenuEntry::new(
                app.tr(MessageId::CtxMenuOpenSelection),
                app.tr(MessageId::CtxMenuOpenSelectionDesc),
                ContextMenuAction::OpenSelection,
            )
            .with_glyph("↗"),
        );
        entries.push(
            ContextMenuEntry::new(
                app.tr(MessageId::CtxMenuClearSelection),
                "",
                ContextMenuAction::ClearSelection,
            )
            .with_glyph("×"),
        );
    }

    if !on_work_surface
        && let Some(filtered_cell_index) = transcript_cell_index_from_mouse(app, mouse)
    {
        targeted = true;
        let cell_index = app.original_cell_index_for_rendered(filtered_cell_index);
        let target = detail_target_label(app, cell_index)
            .map(|label| truncate_line_to_width(label.as_str(), 28))
            .unwrap_or_else(|| "message".to_string());
        entries.push(
            ContextMenuEntry::new(
                app.tr(MessageId::CtxMenuOpenDetails),
                target,
                ContextMenuAction::OpenDetails { cell_index },
            )
            .with_glyph("▣")
            .section_start(),
        );
        entries.push(
            ContextMenuEntry::new(
                app.tr(MessageId::CtxMenuCopyMessage),
                app.tr(MessageId::CtxMenuCopyMessageDesc),
                ContextMenuAction::CopyCell { cell_index },
            )
            .with_glyph("⎘"),
        );
        // Offered only when a `path:line` or bare file path on the clicked
        // line (or, failing that, in the cell) resolves to a file inside the
        // workspace. Model output is not trusted to name files outside it.
        if let Some((path, line)) = context_menu_file_reference(app, mouse, cell_index) {
            let shown = path
                .strip_prefix(&app.workspace)
                .unwrap_or(&path)
                .display()
                .to_string();
            entries.push(
                ContextMenuEntry::new(
                    app.tr(MessageId::CtxMenuOpenInEditor),
                    format!("{shown}:{line}"),
                    ContextMenuAction::OpenFileAtLine { path, line },
                )
                .with_glyph("↗")
                .with_hint("e"),
            );
        }
        // Hide/show cell toggle.
        if app.collapsed_cells.contains(&cell_index) {
            entries.push(
                ContextMenuEntry::new(
                    app.tr(MessageId::CtxMenuShowCell),
                    app.tr(MessageId::CtxMenuShowCellDesc),
                    ContextMenuAction::ShowCell { cell_index },
                )
                .with_glyph("◇"),
            );
        } else {
            entries.push(
                ContextMenuEntry::new(
                    app.tr(MessageId::CtxMenuHideCell),
                    app.tr(MessageId::CtxMenuHideCellDesc),
                    ContextMenuAction::HideCell { cell_index },
                )
                .with_glyph("○"),
            );
        }
    }

    // When cells are hidden, offer a way to show them all.
    if !app.collapsed_cells.is_empty() {
        let count = app.collapsed_cells.len();
        let label = app.tr(MessageId::CtxMenuShowHidden).to_string();
        entries.push(
            ContextMenuEntry::new(
                format!("{label} ({count})"),
                app.tr(MessageId::CtxMenuShowHiddenDesc),
                ContextMenuAction::ShowAllHidden,
            )
            .with_glyph("◇")
            .section_start(),
        );
    }

    // App chrome belongs to empty space. On a message, a row or a selection
    // it only pushed the target's own actions off the bottom of the menu.
    if !targeted {
        push_chrome_entries(app, &mut entries);
    }
    entries
}

fn push_chrome_entries(app: &App, entries: &mut Vec<ContextMenuEntry>) {
    entries.push(
        ContextMenuEntry::new(
            app.tr(MessageId::CtxMenuCmdPalette),
            app.tr(MessageId::CtxMenuCmdPaletteDesc),
            ContextMenuAction::OpenCommandPalette,
        )
        .with_glyph("⌘")
        .section_start(),
    );
    entries.push(
        ContextMenuEntry::new(
            app.tr(MessageId::CtxMenuContextInspector),
            app.tr(MessageId::CtxMenuContextInspectorDesc),
            ContextMenuAction::OpenContextInspector,
        )
        .with_glyph("ⓘ"),
    );
    entries.push(
        ContextMenuEntry::new(
            app.tr(MessageId::CtxMenuHelp),
            app.tr(MessageId::CtxMenuHelpDesc),
            ContextMenuAction::OpenHelp,
        )
        .with_glyph("?"),
    );

    // Host window control (Windows only): pin/unpin the terminal window into
    // an always-on-top mini window. The label flips while pinned ("还原窗口"
    // instead of "弹出置顶小窗") so the entry always describes what the click
    // will do.
    if crate::tui::window_control::available() {
        let pinned = crate::tui::window_control::pinned();
        entries.push(
            ContextMenuEntry::new(
                app.tr(if pinned {
                    MessageId::CtxMenuWindowUnpin
                } else {
                    MessageId::CtxMenuWindowPin
                }),
                app.tr(MessageId::CtxMenuWindowPinDesc),
                ContextMenuAction::ToggleWindowPin,
            )
            .with_glyph(if pinned { "↩" } else { "📌" }),
        );
    }
}

/// A work-surface row's menu: the row's own action first (what a left click
/// does), then the agent or work item's other doors, then Copy, and any stop
/// last behind an in-menu confirm. Each entry carries a typed row action, so
/// the menu runs exactly what the row runs; nothing is a free-form command.
///
/// Focus is the one agent destination: focusing shows the agent's chat and
/// addresses the composer to it, so "Message agent" and "Open transcript"
/// would be the same action under three names.
fn push_work_row_entries(app: &App, mouse: MouseEvent, entries: &mut Vec<ContextMenuEntry>) {
    let confirm = || app.tr(MessageId::CtxMenuConfirmArmed).into_owned();
    let row = |label: String, action: SidebarRowAction| {
        ContextMenuEntry::new(label, "", ContextMenuAction::Row(action))
    };
    // The menu is built from the row's own action, never from its inline
    // stop zone: a right-click there must not make an unconfirmed stop the
    // primary entry. The row's stop, if any, goes last behind the confirm.
    let hovered = sidebar_row_at(app, mouse);
    let mut stop = hovered
        .and_then(|hovered| hovered.stop_action.clone())
        .map(|action| {
            let label = match action {
                SidebarRowAction::CancelAgent { .. } => MessageId::CtxMenuStopAgent,
                _ => MessageId::CtxMenuStopWork,
            };
            row(app.tr(label).into_owned(), action).confirm(confirm())
        });
    match hovered.and_then(|hovered| hovered.click_action.clone()) {
        Some(SidebarRowAction::OpenAgentTranscript { agent_id }) => {
            entries.push(
                ContextMenuEntry::new(
                    app.tr(MessageId::CtxMenuFocusAgent),
                    app.tr(MessageId::CtxMenuFocusAgentDesc),
                    ContextMenuAction::Row(SidebarRowAction::OpenAgentTranscript {
                        agent_id: agent_id.clone(),
                    }),
                )
                .with_glyph("▶")
                .primary(),
            );
            entries.push(row(
                app.tr(MessageId::CtxMenuOpenDetails).into_owned(),
                SidebarRowAction::OpenAgentDetail {
                    agent_id: agent_id.clone(),
                },
            ));
            entries.push(ContextMenuEntry::new(
                app.tr(MessageId::CtxMenuCopyId),
                agent_id.clone(),
                ContextMenuAction::CopyText {
                    text: agent_id.clone(),
                },
            ));
            let running = app.subagent_cache.iter().any(|agent| {
                agent.agent_id == agent_id
                    && matches!(
                        agent.status,
                        crate::tools::subagent::SubAgentStatus::Running
                    )
            });
            if running {
                stop = Some(
                    row(
                        app.tr(MessageId::CtxMenuStopAgent).into_owned(),
                        SidebarRowAction::CancelAgent { agent_id },
                    )
                    .confirm(confirm()),
                );
            }
        }
        Some(SidebarRowAction::CancelAgent { agent_id }) => {
            stop = Some(
                row(
                    app.tr(MessageId::CtxMenuStopAgent).into_owned(),
                    SidebarRowAction::CancelAgent { agent_id },
                )
                .confirm(confirm()),
            );
        }
        Some(action @ SidebarRowAction::InspectWork { .. }) => {
            if let SidebarRowAction::InspectWork {
                stop_action: Some(stop_action),
                ..
            } = &action
            {
                stop = Some(
                    row(
                        app.tr(MessageId::CtxMenuStopWork).into_owned(),
                        (**stop_action).clone(),
                    )
                    .confirm(confirm()),
                );
            }
            entries.push(row(app.tr(MessageId::CtxMenuOpenDetails).into_owned(), action).primary());
        }
        Some(action @ SidebarRowAction::OpenAgentDetail { .. }) => {
            entries.push(row(app.tr(MessageId::CtxMenuOpenDetails).into_owned(), action).primary());
        }
        Some(action @ SidebarRowAction::ShowSubagentsPanel) => {
            entries.push(row(app.tr(MessageId::CtxMenuOpen).into_owned(), action).primary());
        }
        Some(SidebarRowAction::Command(command)) => {
            if let Some(id) = command.strip_prefix("/jobs show ")
                && app.task_panel.iter().any(|entry| {
                    entry.id == id && crate::tui::background_indicator::is_live_shell_entry(entry)
                })
            {
                stop = Some(
                    row(
                        app.tr(MessageId::CtxMenuStopWork).into_owned(),
                        SidebarRowAction::Command(format!("/jobs cancel {id}")),
                    )
                    .confirm(confirm()),
                );
            }
            let label = app
                .tr(MessageId::CtxMenuRunCommand)
                .replace("{command}", &command);
            entries.push(row(label, SidebarRowAction::Command(command)).primary());
        }
        // Stages a destructive command in the composer; Enter there runs it.
        Some(SidebarRowAction::PrefillCommand(command)) => {
            let label = app
                .tr(MessageId::CtxMenuRunCommand)
                .replace("{command}", &command);
            entries.push(
                row(
                    format!("{label}…"),
                    SidebarRowAction::PrefillCommand(command),
                )
                .primary(),
            );
        }
        None => {}
    }
    // Copy the row's full text (rows can't be mouse-selected, so the menu is
    // the only copy path).
    if let Some(text) = sidebar_row_copy_text(app, mouse) {
        entries.push(
            ContextMenuEntry::new(
                app.tr(MessageId::CtxMenuCopyRow),
                truncate_line_to_width(first_line(&text), 28),
                ContextMenuAction::CopyText { text },
            )
            .with_glyph("⎘")
            .with_hint("y"),
        );
    }
    if let Some(stop) = stop {
        entries.push(stop.with_glyph("■").section_start());
    }
}

/// The workspace file a right-click on a transcript cell points at: the first
/// `path:line` on the clicked line, else a bare path there at line 1, else
/// the same search over the cell. All must be
/// regular files inside the workspace, reached without links
/// (`history::workspace_file`); `open_file_in_editor` checks again.
fn context_menu_file_reference(
    app: &App,
    mouse: MouseEvent,
    cell_index: usize,
) -> Option<(std::path::PathBuf, u32)> {
    let clicked_line = selection_point_from_mouse(app, mouse).and_then(|point| {
        app.viewport
            .transcript_cache
            .lines()
            .get(point.line_index)
            .map(line_to_plain)
    });
    if let Some(found) = clicked_line.as_deref().and_then(|line| {
        crate::tui::history::file_line_reference(line, &app.workspace)
            .or_else(|| crate::tui::history::bare_file_reference(line, &app.workspace))
    }) {
        return Some(found);
    }
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    let text = history_cell_to_text(app.cell_at_virtual_index(cell_index)?, width);
    crate::tui::history::first_file_line_reference(&text, &app.workspace)
}

pub(crate) fn transcript_cell_index_from_mouse(app: &App, mouse: MouseEvent) -> Option<usize> {
    let point = selection_point_from_mouse(app, mouse)?;
    app.viewport
        .transcript_cache
        .line_meta()
        .get(point.line_index)
        .and_then(|meta| meta.cell_line())
        .map(|(cell_index, _)| cell_index)
}

/// What a context-menu action needs from the host after `App` state changed.
#[derive(Debug)]
pub(crate) enum ContextMenuOutcome {
    Done,
    /// Row actions produce the same view events a left click on the row does;
    /// the host runs them through its view-event dispatcher.
    Events(Vec<ViewEvent>),
    /// Open a workspace file in `$EDITOR`. That needs the terminal (the TUI
    /// suspends while the editor owns it), so the host does it.
    OpenInEditor {
        path: std::path::PathBuf,
        line: u32,
    },
}

/// Apply a context-menu action to `App`. Terminal work comes back as an
/// outcome for the caller, so every action path is testable without one.
pub(crate) fn apply_context_menu_action(
    app: &mut App,
    action: ContextMenuAction,
) -> ContextMenuOutcome {
    let mut outcome = ContextMenuOutcome::Done;
    match action {
        ContextMenuAction::CopySelection => {
            copy_active_selection(app);
        }
        ContextMenuAction::OpenSelection => {
            if !open_active_selection(app) {
                app.status_message = Some("No selection to open".to_string());
            }
        }
        ContextMenuAction::ClearSelection => {
            app.status_message = Some(
                if clear_active_selection(app) {
                    "Selection cleared"
                } else {
                    "No selection to clear"
                }
                .to_string(),
            );
        }
        ContextMenuAction::CopyCell { cell_index } => {
            copy_cell_to_clipboard(app, cell_index);
        }
        ContextMenuAction::OpenDetails { cell_index } => {
            if !open_details_pager_for_cell(app, cell_index) {
                app.status_message = Some("No details available for that line".to_string());
            }
        }
        ContextMenuAction::Paste => {
            app.paste_from_clipboard();
        }
        ContextMenuAction::Row(action) => {
            outcome = ContextMenuOutcome::Events(apply_sidebar_row_action(app, action));
        }
        ContextMenuAction::Extension { item_id, verb } => {
            match app.view_stack.extensions_menu_action(&item_id, verb) {
                Some(events) => outcome = ContextMenuOutcome::Events(events),
                None => {
                    app.status_message = Some("That extension is no longer listed".to_string());
                }
            }
        }
        ContextMenuAction::CopyText { text } => match app.clipboard.write_text_status(&text) {
            Ok(transport) => {
                let receipt = copy_receipt(app, transport, app.tr(MessageId::ClipboardCopied));
                app.push_status_toast(receipt, StatusToastLevel::Info, None);
            }
            Err(error) => {
                let receipt = copy_failed_receipt(app, error);
                app.push_status_toast(receipt, StatusToastLevel::Error, None);
            }
        },
        ContextMenuAction::ToggleWindowPin => {
            crate::tui::window_control::toggle_pin(app);
        }
        ContextMenuAction::OpenCommandPalette => {
            codewhale_telemetry::session_counters()
                .bump(codewhale_telemetry::Counter::CommandPaletteOpen);
            app.view_stack.push(CommandPaletteView::new_for_locale(
                app.ui_locale,
                build_command_palette_entries(
                    app.ui_locale,
                    &app.skills_dir,
                    app.skills_discovery_mode,
                    &app.workspace,
                    &app.mcp_config_path,
                    app.mcp_snapshot.as_ref(),
                    app.extension_plugin_view().as_ref(),
                ),
            ));
        }
        ContextMenuAction::OpenContextInspector => {
            open_context_inspector(app);
        }
        ContextMenuAction::OpenHelp => {
            let help =
                HelpView::new_for_app(app, false).with_groups_expanded(app.help_expand_groups);
            app.view_stack.push(help);
        }
        ContextMenuAction::OpenFileAtLine { path, line } => {
            outcome = ContextMenuOutcome::OpenInEditor { path, line };
        }
        ContextMenuAction::HideCell { cell_index } => {
            app.collapsed_cells.insert(cell_index);
            app.status_message = Some("Cell hidden".to_string());
        }
        ContextMenuAction::ShowCell { cell_index } => {
            app.collapsed_cells.remove(&cell_index);
            app.status_message = Some("Cell shown".to_string());
        }
        ContextMenuAction::ShowAllHidden => {
            let count = app.collapsed_cells.len();
            app.collapsed_cells.clear();
            app.status_message = Some(format!("{count} hidden cell(s) restored"));
        }
    }
    app.needs_redraw = true;
    outcome
}

/// Open a workspace file at a line in `$EDITOR`. The editor gets the terminal
/// through the same suspend path the composer and `/hooks edit` use, one at a
/// time, and we wait for it. It used to be spawned detached while the TUI
/// still held raw mode, the alt screen and mouse capture (#6235).
/// The path `open_file_in_editor` may hand to the editor: `path` only while it
/// is still a regular file inside `workspace` reached without links
/// (`history::workspace_file`), else `None`.
fn editor_target(
    workspace: &std::path::Path,
    path: &std::path::Path,
) -> Option<std::path::PathBuf> {
    path.to_str()
        .and_then(|raw| crate::tui::history::workspace_file(workspace, raw))
}

pub(crate) fn open_file_in_editor(
    terminal: &mut ratatui::Terminal<crate::tui::color_compat::ColorCompatBackend<std::io::Stdout>>,
    app: &mut App,
    path: &std::path::Path,
    line: u32,
) {
    // The menu checked this path when it was built; a link can be swapped in
    // before the click, so check again right before the editor gets it.
    let Some(path) = editor_target(&app.workspace, path) else {
        app.status_message = Some(
            app.tr(MessageId::CtxMenuEditorRefused)
                .replace("{path}", &path.display().to_string()),
        );
        return;
    };
    let path = path.as_path();
    let outcome = crate::tui::external_editor::spawn_editor_for_path(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
        path,
        Some(line),
    );
    app.needs_redraw = true;
    app.status_message = Some(match outcome {
        Ok(crate::tui::external_editor::EditorOutcome::Cancelled) => {
            format!("Editor exited without opening {}", path.display())
        }
        Ok(_) => format!("Closed editor for {}:{line}", path.display()),
        Err(error) => format!("Could not open the editor: {error}"),
    });
}

pub(crate) fn selection_point_from_mouse(
    app: &App,
    mouse: MouseEvent,
) -> Option<TranscriptSelectionPoint> {
    selection_point_from_position(
        app.viewport.last_transcript_area?,
        mouse.column,
        mouse.row,
        app.viewport.last_transcript_top,
        app.viewport.last_transcript_total,
        app.viewport.last_transcript_padding_top,
    )
}

pub(crate) fn selection_point_from_position(
    area: Rect,
    column: u16,
    row: u16,
    transcript_top: usize,
    transcript_total: usize,
    padding_top: usize,
) -> Option<TranscriptSelectionPoint> {
    if column < area.x
        || column >= area.x + area.width
        || row < area.y
        || row >= area.y + area.height
    {
        return None;
    }

    if transcript_total == 0 {
        return None;
    }

    let row = row.saturating_sub(area.y) as usize;
    if row < padding_top {
        return None;
    }
    let row = row.saturating_sub(padding_top);

    let col = column.saturating_sub(area.x) as usize;
    let line_index = transcript_top
        .saturating_add(row)
        .min(transcript_total.saturating_sub(1));

    Some(TranscriptSelectionPoint {
        line_index,
        column: col,
    })
}

/// The one selection Copy, Open and Clear act on: the composer's when it has
/// one, else the transcript's. The menu entries and Ctrl+C all read it here,
/// so Copy cannot take the composer's text while Clear clears the
/// transcript's and reports "Selection cleared".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActiveSelection {
    Composer(String),
    Transcript,
}

pub(crate) fn active_selection(app: &App) -> Option<ActiveSelection> {
    let composer = app.selected_text();
    if !composer.is_empty() {
        return Some(ActiveSelection::Composer(composer));
    }
    selection_to_text(app)
        .is_some_and(|text| !text.is_empty())
        .then_some(ActiveSelection::Transcript)
}

pub(crate) fn selection_has_content(app: &App) -> bool {
    active_selection(app).is_some()
}

/// The receipt for a clipboard write. `native` is said only when a native
/// clipboard took the text; a terminal (OSC 52 / tmux) write is never
/// acknowledged, so its receipt says where the text went, not that it
/// arrived. An asynchronous failure still replaces either receipt through
/// the event loop's `poll_write_completion` drain.
pub(crate) fn copy_receipt(
    app: &App,
    transport: crate::tui::clipboard::CopyTransport,
    native: impl Into<String>,
) -> String {
    match transport {
        crate::tui::clipboard::CopyTransport::Native => native.into(),
        crate::tui::clipboard::CopyTransport::Terminal => {
            app.tr(MessageId::ClipboardSentToTerminal).into_owned()
        }
    }
}

pub(crate) fn copy_failed_receipt(app: &App, error: impl std::fmt::Display) -> String {
    app.tr(MessageId::ClipboardCopyFailed)
        .replace("{error}", &error.to_string())
}

/// Ctrl+X on a composer selection. The text is deleted only after a native
/// clipboard confirmed the copy; an OSC 52 / tmux copy is never confirmed,
/// so the text stays and the receipt says why.
pub(crate) fn cut_selection(app: &mut App) {
    let sel = app.selected_text();
    if sel.is_empty() {
        return;
    }
    match app.clipboard.write_text_status(&sel) {
        Ok(crate::tui::clipboard::CopyTransport::Native) => {
            app.push_status_toast(
                app.tr(MessageId::ClipboardCopied),
                StatusToastLevel::Info,
                None,
            );
            app.delete_selection();
        }
        Ok(crate::tui::clipboard::CopyTransport::Terminal) => {
            let receipt = app.tr(MessageId::ClipboardCutKeptText).into_owned();
            app.push_status_toast(receipt, StatusToastLevel::Info, None);
        }
        Err(error) => {
            let receipt = copy_failed_receipt(app, error);
            app.push_status_toast(receipt, StatusToastLevel::Error, None);
        }
    }
}

fn open_active_selection(app: &mut App) -> bool {
    match active_selection(app) {
        Some(ActiveSelection::Composer(text)) => {
            let width = app
                .viewport
                .last_transcript_area
                .map(|area| area.width)
                .unwrap_or(80);
            app.view_stack.push(crate::tui::pager::PagerView::from_text(
                "Selection",
                &text,
                width.saturating_sub(2),
            ));
            true
        }
        Some(ActiveSelection::Transcript) => open_pager_for_selection(app),
        None => false,
    }
}

fn clear_active_selection(app: &mut App) -> bool {
    match active_selection(app) {
        Some(ActiveSelection::Composer(_)) => {
            app.clear_selection();
            app.needs_redraw = true;
            true
        }
        Some(ActiveSelection::Transcript) => {
            clear_transcript_selection(app);
            true
        }
        None => false,
    }
}

/// Branches taken by the Ctrl+C key handler. The order encodes priority and is
/// the unit-tested contract for #1337 / #1367: a transcript selection always
/// wins (so users learn that Ctrl+C copies when there's something to copy);
/// otherwise an active turn is interrupted; otherwise a non-empty composer is
/// cleared recoverably; otherwise the quit-arm flow runs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CtrlCDisposition {
    CopySelection,
    CancelTurn,
    ClearInput,
    ConfirmExit,
    ArmExit,
}

pub(crate) fn ctrl_c_disposition(app: &App) -> CtrlCDisposition {
    if selection_has_content(app) {
        CtrlCDisposition::CopySelection
    } else if app.is_loading
        || app.is_compacting
        || app.manual_compaction_queued
        || app.goal_continuation_waiting
    {
        CtrlCDisposition::CancelTurn
    } else if !app.input.is_empty() {
        CtrlCDisposition::ClearInput
    } else if app.quit_is_armed() {
        CtrlCDisposition::ConfirmExit
    } else {
        CtrlCDisposition::ArmExit
    }
}

/// Normalize the raw Ctrl+C control byte to canonical `Ctrl+C`.
///
/// In PTY/raw-mode the terminal driver delivers Ctrl+C as the literal byte
/// `0x03` (the ETX control character). crossterm usually decodes that to
/// `Char('c') + CONTROL`, but some terminal / kitty-keyboard-protocol
/// combinations surface it as `Char('\u{3}')` instead, where it slips past the
/// `Char('c') + CONTROL` arm of the key handler and never reaches the
/// quit-arm flow (#4090). Rewriting every encoding of Ctrl+C to the canonical
/// form here keeps the double-press-to-exit behavior consistent across PTY,
/// raw-mode, and kitty-enhanced terminals.
pub(crate) fn normalize_raw_ctrl_c(key: &mut KeyEvent) {
    if matches!(key.code, KeyCode::Char('\u{3}')) {
        key.code = KeyCode::Char('c');
        key.modifiers.insert(KeyModifiers::CONTROL);
    }
}

pub(crate) fn copy_active_selection(app: &mut App) {
    // Composer selection takes priority.
    let sel = app.selected_text();
    if !sel.is_empty() {
        match app.clipboard.write_text_status(&sel) {
            Ok(transport) => {
                let receipt = copy_receipt(app, transport, app.tr(MessageId::ClipboardCopied));
                app.push_status_toast(receipt, StatusToastLevel::Info, None);
                app.clear_selection();
            }
            Err(error) => {
                let receipt = copy_failed_receipt(app, error);
                app.push_status_toast(receipt, StatusToastLevel::Error, None);
            }
        }
        return;
    }
    if !app.viewport.transcript_selection.is_active() {
        return;
    }
    // Markdown source first (#6156): project every intersected cell through
    // the canonical clean-copy path. Falls back to rendered text when the
    // `[tui] selection_copy_markdown` key is off or no cell metadata
    // intersects the range.
    let payload = if app.viewport.selection_copy_markdown {
        selection_to_markdown(app).map(|(text, cells)| (text, Some(cells)))
    } else {
        None
    };
    let payload = payload.or_else(|| {
        selection_to_text(app)
            .filter(|text| !text.is_empty())
            .map(|text| (text, None))
    });
    if let Some((text, markdown_cells)) = payload {
        match app.clipboard.write_text_status(&text) {
            Ok(transport) => {
                let receipt = match (transport, markdown_cells) {
                    (crate::tui::clipboard::CopyTransport::Native, Some(cells)) => app
                        .tr(MessageId::SelectionCopiedAsMarkdown)
                        .replace("{count}", &cells.to_string()),
                    (transport, _) => {
                        copy_receipt(app, transport, app.tr(MessageId::ClipboardCopied))
                    }
                };
                app.push_status_toast(receipt, StatusToastLevel::Info, None);
            }
            Err(error) => {
                let receipt = copy_failed_receipt(app, error);
                app.push_status_toast(receipt, StatusToastLevel::Error, None);
            }
        }
    } else {
        clear_transcript_selection(app);
        app.status_message = Some("No selection to copy".to_string());
    }
}

/// Whether a drag selection covers every cell it touches end to end (#6228).
///
/// Two checks: the edge columns must reach the content edges on the boundary
/// lines, and the line range must not cut a cell in half at either end.
/// Middle lines are fully covered by construction, and cells render as
/// contiguous spans, so the two edge cells decide for the whole range.
fn selection_covers_cells_fully(
    app: &App,
    start: &TranscriptSelectionPoint,
    end: &TranscriptSelectionPoint,
    start_index: usize,
    end_index: usize,
) -> bool {
    let (first_head, _) = match content_column_span(app, start_index) {
        Some(span) => span,
        None => return false,
    };
    if start.column > first_head {
        return false;
    }
    let (_, last_tail) = match content_column_span(app, end_index) {
        Some(span) => span,
        None => return false,
    };
    if end.column < last_tail {
        return false;
    }
    let line_meta = app.viewport.transcript_cache.line_meta();
    let mut edge_cells = (start_index..=end_index).filter_map(|line_index| {
        line_meta
            .get(line_index)
            .and_then(|meta| meta.cell_line())
            .map(|(cell_index, _)| cell_index)
    });
    let Some(first_cell) = edge_cells.next() else {
        return false;
    };
    let last_cell = edge_cells.next_back().unwrap_or(first_cell);
    [first_cell, last_cell].into_iter().all(|cell| {
        let mut span = line_meta
            .iter()
            .enumerate()
            .filter_map(|(line_index, meta)| {
                meta.cell_line()
                    .filter(|(cell_index, _)| *cell_index == cell)
                    .map(|_| line_index)
            });
        match (span.next(), span.next_back()) {
            (Some(cell_first), Some(cell_last)) => {
                cell_first >= start_index && cell_last <= end_index
            }
            (Some(only), None) => start_index <= only && only <= end_index,
            (None, _) => false,
        }
    })
}

/// Rendered-column span of selectable content on one transcript cache line.
///
/// Mirrors the prefix math in [`selection_to_text`]: rail decorations plus
/// copy-only prefixes are visual, so content runs from their combined width
/// to that width plus the content's display width.
fn content_column_span(app: &App, line_index: usize) -> Option<(usize, usize)> {
    let cache = &app.viewport.transcript_cache;
    let full_width = text_visible_width(&line_to_plain(cache.lines().get(line_index)?));
    let rail_width = cache.rail_prefix_width(line_index).min(full_width);
    let copy_prefix = cache
        .line_meta()
        .get(line_index)
        .map(|meta| meta.copy_prefix_width())
        .unwrap_or(0)
        .min(full_width.saturating_sub(rail_width));
    let head = rail_width.saturating_add(copy_prefix);
    let tail = head.saturating_add(
        full_width
            .saturating_sub(rail_width)
            .saturating_sub(copy_prefix),
    );
    Some((head, tail))
}

/// Project a transcript drag selection to Markdown source (#6156).
///
/// Collects every history cell intersecting the selection's rendered line
/// range, in order, and serializes each through
/// `history_cell_to_clipboard_text` — the same canonical projection Ctrl-Y
/// and `/copy` use — joined with a blank line. Returns the payload plus the
/// projected cell count for the toast.
///
/// Markdown source is only truthful for whole cells, so a selection that
/// cuts a cell in half is not projected here at all — it keeps its exact
/// rendered text through the caller's [`selection_to_text`] fallback (#6228).
///
/// Returns `None` when the selection is a fragment, when no cell metadata
/// intersects the range, or when every projection is blank.
pub(crate) fn selection_to_markdown(app: &App) -> Option<(String, usize)> {
    let (start, end) = app.viewport.transcript_selection.ordered_endpoints()?;
    let lines = app.viewport.transcript_cache.lines();
    if lines.is_empty() {
        return None;
    }
    let end_index = end.line_index.min(lines.len().saturating_sub(1));
    let start_index = start.line_index.min(end_index);
    if !selection_covers_cells_fully(app, &start, &end, start_index, end_index) {
        return None;
    }
    let line_meta = app.viewport.transcript_cache.line_meta();
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(80);
    let mut rendered = Vec::new();
    for line_index in start_index..=end_index {
        if let Some((cell_index, _)) = line_meta.get(line_index).and_then(|meta| meta.cell_line())
            && !rendered.contains(&cell_index)
        {
            rendered.push(cell_index);
        }
    }
    let mut seen_original = Vec::new();
    let mut parts = Vec::new();
    for rendered_index in rendered {
        let original = app.original_cell_index_for_rendered(rendered_index);
        if seen_original.contains(&original) {
            continue;
        }
        seen_original.push(original);
        let Some(cell) = app.cell_at_virtual_index(original) else {
            continue;
        };
        let text = history_cell_to_clipboard_text(cell, width);
        if !text.trim().is_empty() {
            parts.push(text);
        }
    }
    if parts.is_empty() {
        return None;
    }
    let count = parts.len();
    Some((parts.join("\n\n"), count))
}
pub(crate) fn clear_transcript_selection(app: &mut App) {
    app.needs_redraw |= app.viewport.transcript_selection.is_active();
    app.viewport.transcript_selection.clear();
}
pub(crate) fn selection_to_text(app: &App) -> Option<String> {
    let (start, end) = app.viewport.transcript_selection.ordered_endpoints()?;
    let lines = app.viewport.transcript_cache.lines();
    if lines.is_empty() {
        return None;
    }
    let end_index = end.line_index.min(lines.len().saturating_sub(1));
    let start_index = start.line_index.min(end_index);

    let line_meta = app.viewport.transcript_cache.line_meta();
    let mut selected = String::new();
    let mut separator_before = None;
    #[allow(clippy::needless_range_loop)]
    for line_index in start_index..=end_index {
        if let Some(separator) = separator_before {
            selected.push_str(separator);
        }
        // Rail-prefix decorations are stored as cache metadata rather than
        // detected from glyphs, so new decoration types are covered without
        // changes to the copy path (#1163).
        let rail_width = app.viewport.transcript_cache.rail_prefix_width(line_index);
        // Convert the rendered line to plain text (strips OSC-8), then
        // slice off the rail prefix so subsequent column offsets operate
        // on content-only text.
        let full_text = line_to_plain(&lines[line_index]);
        // Selection columns are painted terminal cells, where control
        // characters are invisible (ratatui strips them). Measure and slice
        // in that space so columns after a tab stay aligned with what the
        // user dragged over; the fixed-width fallback would shift every
        // downstream column.
        let line_after_rail = if rail_width > 0 {
            slice_visible_columns(&full_text, rail_width, text_visible_width(&full_text))
        } else {
            full_text
        };
        let line_after_rail_width = text_visible_width(&line_after_rail);
        let copy_prefix_width = line_meta
            .get(line_index)
            .map(|meta| meta.copy_prefix_width())
            .unwrap_or(0)
            .min(line_after_rail_width);
        let line_text = if copy_prefix_width > 0 {
            slice_visible_columns(&line_after_rail, copy_prefix_width, line_after_rail_width)
        } else {
            line_after_rail
        };
        let visual_prefix_width = rail_width.saturating_add(copy_prefix_width);
        let line_width = text_visible_width(&line_text);
        // Selection coordinates are recorded in rendered-column space, which
        // includes visual prefixes. Add them back so the column window maps
        // correctly into copy-only text.
        let (raw_col_start, raw_col_end) = if start_index == end_index {
            (start.column, end.column)
        } else if line_index == start_index {
            (start.column, line_width.saturating_add(visual_prefix_width))
        } else if line_index == end_index {
            (0, end.column)
        } else {
            (0, line_width.saturating_add(visual_prefix_width))
        };

        let col_start = raw_col_start
            .saturating_sub(visual_prefix_width)
            .min(line_width);
        let col_end = raw_col_end
            .saturating_sub(visual_prefix_width)
            .min(line_width);

        let slice = slice_visible_columns(&line_text, col_start, col_end);
        selected.push_str(&slice);
        separator_before = line_meta
            .get(line_index)
            .map(|meta| meta.copy_separator_after().as_str())
            .or(Some("\n"));
    }
    Some(selected)
}

#[cfg(test)]
mod tests {
    use super::{
        agent_transcript_text, build_context_menu_entries, handle_composer_mouse,
        handle_mouse_event, sidebar_click_action,
    };
    use crate::tui::app::{
        App, SidebarHoverRow, SidebarHoverSection, SidebarRowAction, TuiOptions,
    };
    use crate::tui::tideline::{
        ContextBudgetSnapshot, InspectDetail, InteractionAction, InteractionFocus,
        InteractionTarget, InteractionTargetId,
    };
    use crate::tui::views::{ContextMenuAction, ModalKind, ViewEvent};
    use codewhale_models::Role;
    use codewhale_models::{ContentBlock, Message};
    use codewhale_runtime::config::Config;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;
    use serde_json::json;
    use std::path::PathBuf;
    use tempfile::tempdir;

    pub(super) fn create_test_app() -> App {
        let options = TuiOptions {
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        };
        let mut app = App::new(options, &Config::default());
        // Legacy strip geometry (see ui.rs); Bottom default has its own tests.
        app.work_surface.placement = crate::tui::work_surface::WorkSurfacePlacement::Top;
        app
    }

    #[test]
    fn plugin_review_click_opens_inventory_and_emits_no_install_or_trust_command() {
        let _lock = crate::test_support::lock_test_env();
        let root = tempdir().unwrap();
        let _home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(root.path()),
            &Config::default(),
        );
        app.surface_plugin_review_request("catalog-only", "/plugin install arbitrary-source");
        assert!(app.plugin_cta.phase.is_visible());
        let before = app.plugin_registry.list().len();
        let area = Rect::new(0, 0, 140, 1);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        crate::tui::plugin_suggestions::draw_plugin_cta(&mut app, area, &mut buffer);
        assert!(
            super::handle_plugin_cta_mouse(&mut app, left_click(0, 0))
                .unwrap()
                .is_empty()
        );
        assert!(app.view_stack.is_empty());
        assert!(app.plugin_cta.phase.is_visible());
        let button = app.viewport.last_plugin_cta_review_area.unwrap();
        let events =
            super::handle_plugin_cta_mouse(&mut app, left_click(button.x, button.y)).unwrap();
        assert!(
            events.is_empty(),
            "review navigation must not dispatch commands"
        );
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::Extensions));
        assert!(!app.plugin_cta.phase.is_visible());
        assert_eq!(app.plugin_registry.list().len(), before);
        assert!(app.plugin_registry.get("catalog-only").is_none());
    }

    #[test]
    fn workbar_click_opens_the_workflows_view() {
        let mut app = create_test_app();
        app.workflow_runs
            .push(crate::tui::widgets::workflow_panel::WorkflowPanel::new(
                "workflow_1",
                "audit",
                1,
            ));
        app.viewport.last_workbar_area = Some(Rect::new(0, 20, 80, 1));
        assert!(!super::handle_workbar_mouse(&mut app, left_click(10, 5)));
        assert!(app.view_stack.is_empty());
        assert!(super::handle_workbar_mouse(&mut app, left_click(10, 20)));
        assert_eq!(
            app.view_stack.top_kind(),
            Some(crate::tui::views::ModalKind::WorkflowsManager)
        );
    }

    #[test]
    fn composer_click_maps_tabs_as_painted() {
        // A tab paints no cells, so clicking the visible char after one
        // must resolve past it instead of stopping on the tab itself.
        let mut app = create_test_app();
        let area = Rect::new(0, 0, 80, 10);
        app.input = "a\tb".to_string();
        assert_eq!(super::mouse_pos_to_char_index(&app, 1, 0, area), Some(2));
        app.input = "\ta".to_string();
        assert_eq!(super::mouse_pos_to_char_index(&app, 0, 0, area), Some(1));
    }

    fn hover_row(row_y: u16, action: Option<&str>) -> SidebarHoverRow {
        SidebarHoverRow {
            row_y,
            display_text: "row".to_string(),
            full_text: "row".to_string(),
            detail: None,
            is_truncated: false,
            click_action: action.map(|action| SidebarRowAction::Command(action.to_string())),
            stop_action: None,
            stop_zone_start_col: None,
            stop_zone_end_col: None,
        }
    }

    fn hover_row_with_stop(row_y: u16, action: &str, stop_action: &str) -> SidebarHoverRow {
        SidebarHoverRow {
            row_y,
            display_text: "job row [x]".to_string(),
            full_text: "job row [x]".to_string(),
            detail: None,
            is_truncated: false,
            click_action: Some(SidebarRowAction::Command(action.to_string())),
            stop_action: Some(SidebarRowAction::Command(stop_action.to_string())),
            stop_zone_start_col: Some(68),
            stop_zone_end_col: Some(71),
        }
    }

    fn action_command(action: Option<SidebarRowAction>) -> Option<String> {
        match action? {
            SidebarRowAction::Command(command) => Some(command),
            _ => None,
        }
    }

    fn left_click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn right_click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn mouse_move(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Moved,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn the_launch_card_does_not_swallow_the_rest_of_the_screen() {
        // Founder live-test: "the clickability and the mouse pointing thing
        // isn't working". While the opening screen was a separate surface it
        // was right for it to consume every mouse event and return; the
        // moment it became content on the ordinary screen that gate made
        // scrolling, the composer and the work surface unreachable. A click
        // that misses the card's rows must fall through.
        let mut app = create_test_app();
        app.launch.visible = true;
        app.launch.row_hitboxes = vec![(
            crate::tui::app::LaunchRowId::NewSession,
            Rect::new(2, 5, 30, 1),
        )];
        app.viewport.last_transcript_area = Some(Rect::new(0, 0, 80, 20));
        app.viewport.pending_scroll_delta = 0;

        // A wheel tick on the launch screen still scrolls.
        handle_mouse_event(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 10,
                row: 10,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert_ne!(
            app.viewport.pending_scroll_delta, 0,
            "the wheel must reach the transcript on the opening screen"
        );

        // A click away from the card's rows starts no launch action.
        app.pending_launch_action = None;
        handle_mouse_event(&mut app, left_click(60, 15));
        assert_eq!(
            app.pending_launch_action, None,
            "a click off the card must not be read as a card action"
        );

        // A click on a row still runs it.
        handle_mouse_event(&mut app, left_click(4, 5));
        assert_eq!(
            app.pending_launch_action,
            Some(crate::tui::underwater::LaunchAction::NewSession),
            "the card's own rows still work"
        );
    }

    #[test]
    fn clicking_a_recent_row_opens_the_resume_confirmation_popup() {
        // Founder live-test: "you just click it and boom you're there ... you
        // don't realize it's happening", then, on the first fix: "the
        // resuming confirmation needs to be a popup not something in the
        // composer that's even more confusing". Resuming replaces the whole
        // session context, so the click opens a popup that names the session
        // and asks; nothing resumes until that is confirmed.
        let mut app = create_test_app();
        app.launch.visible = true;
        app.launch.recent = vec![crate::tui::app::LaunchRecentSession {
            id: "sess-1".to_string(),
            title: "refactor the parser".to_string(),
            updated_at: chrono::Utc::now(),
            message_count: 12,
            interrupted: false,
        }];
        app.launch.row_hitboxes = vec![(
            crate::tui::app::LaunchRowId::Recent("sess-1".to_string()),
            Rect::new(2, 5, 30, 1),
        )];
        app.pending_launch_action = None;

        handle_mouse_event(&mut app, left_click(4, 5));
        assert_eq!(
            app.pending_launch_action, None,
            "the click must not resume anything on its own"
        );
        assert_eq!(
            app.view_stack.top_kind(),
            Some(crate::tui::views::ModalKind::LaunchResumeConfirm),
            "it opens the confirmation popup instead"
        );
        assert!(
            app.launch.status.is_none(),
            "and nothing is written over the composer dock"
        );
    }

    #[test]
    fn a_new_session_row_still_takes_one_click() {
        // Only resuming discards context, so New session keeps its single
        // click; adding a confirm step there would be friction for nothing.
        let mut app = create_test_app();
        app.launch.visible = true;
        app.launch.row_hitboxes = vec![(
            crate::tui::app::LaunchRowId::NewSession,
            Rect::new(2, 5, 30, 1),
        )];
        app.pending_launch_action = None;

        handle_mouse_event(&mut app, left_click(4, 5));
        assert_eq!(
            app.pending_launch_action,
            Some(crate::tui::underwater::LaunchAction::NewSession),
            "New session runs on the first click"
        );
    }

    #[test]
    fn idle_pointer_enter_and_leave_request_hover_redraws() {
        let _guard = crate::tui::hover_layer::HOVER_TEST_LOCK.lock().unwrap();
        crate::tui::hover_layer::clear_pointer();
        crate::tui::hover_layer::begin_frame();
        crate::tui::hover_layer::register_rect(
            crate::tui::hover_hit::HoverTargetKind::TruncatedText,
            Rect::new(10, 5, 20, 1),
            "full clipped row",
            false,
        );

        let mut app = create_test_app();
        app.launch.visible = false;
        app.needs_redraw = false;
        handle_mouse_event(&mut app, mouse_move(12, 5));
        assert!(
            app.needs_redraw,
            "entering a target must repaint while idle"
        );
        assert_eq!(
            crate::tui::hover_layer::current_hover().map(|hit| hit.kind),
            Some(crate::tui::hover_hit::HoverTargetKind::TruncatedText)
        );

        app.needs_redraw = false;
        handle_mouse_event(&mut app, mouse_move(40, 5));
        assert!(app.needs_redraw, "leaving a target must clear its popover");
        assert!(crate::tui::hover_layer::current_hover().is_none());
        crate::tui::hover_layer::clear_pointer();
    }

    #[test]
    fn slash_autocomplete_click_selects_and_second_click_applies() {
        let mut app = create_test_app();
        app.launch.visible = false;
        app.work_surface.last_area = None;
        app.input = "/he".to_string();
        app.cursor_position = app.input.chars().count();
        app.slash_menu_hidden = false;
        app.slash_menu_selected = 0;
        // Simulate two painted rows from ComposerWidget.
        app.viewport.last_composer_area = Some(Rect::new(0, 18, 80, 6));
        *app.viewport.last_slash_menu_hitboxes.borrow_mut() =
            vec![(0, Rect::new(1, 20, 78, 1)), (1, Rect::new(1, 21, 78, 1))];

        assert!(
            handle_composer_mouse(&mut app, left_click(5, 21)),
            "slash row click must be consumed by the composer"
        );
        assert_eq!(
            app.slash_menu_selected, 1,
            "click on another row highlights it"
        );
        let before = app.input.clone();
        assert_eq!(
            before, "/he",
            "select-only click must not rewrite the composer"
        );

        assert!(handle_composer_mouse(&mut app, left_click(5, 21)));
        assert_ne!(app.input, before, "click on the highlighted row applies it");
        assert!(
            app.input.starts_with('/'),
            "applied slash entry must replace the composer: {:?}",
            app.input
        );
    }

    #[test]
    fn slash_autocomplete_wheel_moves_selection() {
        let mut app = create_test_app();
        app.launch.visible = false;
        app.work_surface.last_area = None;
        app.input = "/he".to_string();
        app.cursor_position = app.input.chars().count();
        app.slash_menu_hidden = false;
        app.slash_menu_selected = 0;
        app.viewport.last_composer_area = Some(Rect::new(0, 18, 80, 6));
        *app.viewport.last_slash_menu_hitboxes.borrow_mut() =
            vec![(0, Rect::new(1, 20, 78, 1)), (1, Rect::new(1, 21, 78, 1))];
        let entries = crate::tui::slash_menu::visible_slash_menu_entries(&app, 128);
        assert!(entries.len() >= 2, "prefix must offer multiple entries");

        assert!(handle_composer_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 5,
                row: 20,
                modifiers: KeyModifiers::NONE,
            },
        ));
        assert_eq!(app.slash_menu_selected, 1);

        assert!(handle_composer_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 5,
                row: 20,
                modifiers: KeyModifiers::NONE,
            },
        ));
        assert_eq!(app.slash_menu_selected, 0);
    }

    #[test]
    fn active_composer_send_click_queues_the_keyboard_submit_chord() {
        let mut app = create_test_app();
        app.launch.visible = false;
        app.composer_border = true;
        app.input = "ship it".to_string();
        app.cursor_position = app.input.chars().count();
        let area = Rect::new(0, 20, 80, 4);
        app.viewport.last_composer_area = Some(area);
        // Match the frame's submit-aware input plane: x=74 stays blank,
        // then the shared `[↵]` target begins at x=75.
        app.viewport.last_composer_content = Some(Rect::new(1, 21, 73, 2));
        let submit = crate::tui::widgets::active_composer_submit_rect(&app, area)
            .expect("enclosed composer submit");

        handle_mouse_event(&mut app, left_click(submit.x, submit.y));
        assert_eq!(
            app.pending_composer_submit,
            Some(crate::tui::app::ComposerSubmitChord::Enter)
        );
        assert_eq!(app.input, "ship it");
        assert_eq!(app.cursor_position, app.input.chars().count());

        app.pending_composer_submit = None;
        handle_mouse_event(&mut app, left_click(area.x + 4, area.y + 1));
        assert_eq!(app.pending_composer_submit, None);

        app.input.clear();
        app.cursor_position = 0;
        handle_mouse_event(&mut app, left_click(submit.x, submit.y));
        assert_eq!(app.pending_composer_submit, None);
        assert!(app.input.is_empty());
    }

    #[test]
    fn context_meter_click_uses_the_same_inspector_as_the_keyboard_shortcut() {
        let mut app = create_test_app();
        app.launch.visible = false;
        app.viewport
            .interaction_targets
            .register(InteractionTarget {
                id: InteractionTargetId::HEADER_CONTEXT,
                area: Rect::new(52, 0, 20, 1),
                focus: InteractionFocus::Direct,
                keyboard_action: Some(InteractionAction::InspectContext),
                mouse_action: Some(InteractionAction::InspectContext),
                inspect_detail: InspectDetail::ContextBudget(ContextBudgetSnapshot {
                    used_tokens: 3_000,
                    max_tokens: 10_000,
                    percent_basis_points: 3_000,
                }),
            });

        handle_mouse_event(&mut app, left_click(60, 0));

        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::ContextInspector));
        assert!(
            crate::tui::shell_key_routing::is_context_inspector_shortcut(&KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::ALT
            ))
        );
    }

    #[test]
    fn topbar_route_click_emits_provider_picker_request() {
        let mut app = create_test_app();
        // The launch screen shares the same header, so this specifically
        // protects against its old catch-all mouse route swallowing the
        // topbar affordance before it reached the event handler.
        app.launch.visible = true;
        app.viewport
            .interaction_targets
            .register(InteractionTarget {
                id: InteractionTargetId::HEADER_ROUTE,
                area: Rect::new(20, 0, 24, 1),
                focus: InteractionFocus::Direct,
                keyboard_action: Some(InteractionAction::OpenProviderPicker),
                mouse_action: Some(InteractionAction::OpenProviderPicker),
                inspect_detail: InspectDetail::Route,
            });

        let events = handle_mouse_event(&mut app, left_click(24, 0));

        assert!(matches!(
            events.as_slice(),
            [ViewEvent::TopbarRoutePickerRequested]
        ));
        assert!(app.view_stack.is_empty());
    }

    #[test]
    fn context_menu_keeps_paste_first_outside_sidebar() {
        let mut app = create_test_app();
        app.work_surface.last_area = Some(Rect::new(60, 4, 20, 6));

        let entries = build_context_menu_entries(&app, right_click(10, 4));

        assert!(matches!(
            entries.first().map(|entry| &entry.action),
            Some(ContextMenuAction::Paste)
        ));
    }

    #[test]
    fn sidebar_context_menu_omits_paste_without_row_action() {
        let mut app = create_test_app();
        app.work_surface.last_area = Some(Rect::new(60, 4, 20, 6));
        app.sidebar_hover.sections.push(SidebarHoverSection {
            content_area: Rect::new(60, 4, 20, 6),
            lines: vec!["header".to_string()],
            rows: vec![hover_row(4, None)],
        });

        let entries = build_context_menu_entries(&app, right_click(65, 4));

        assert!(
            !entries
                .iter()
                .any(|entry| matches!(entry.action, ContextMenuAction::Paste)),
            "sidebar menu should not offer paste: {entries:?}"
        );
    }

    #[test]
    fn sidebar_context_menu_runs_clickable_row_action() {
        let mut app = create_test_app();
        app.work_surface.last_area = Some(Rect::new(60, 4, 20, 6));
        app.sidebar_hover.sections.push(SidebarHoverSection {
            content_area: Rect::new(60, 4, 20, 6),
            lines: vec!["job row".to_string()],
            rows: vec![hover_row(4, Some("/jobs show shell_x"))],
        });

        let entries = build_context_menu_entries(&app, right_click(65, 4));

        // The row's own command, named, and run as the same typed row action
        // a left click runs — not a free-form command string.
        let first = entries.first().expect("sidebar row should have menu");
        assert_eq!(first.label, "Run /jobs show shell_x");
        assert_eq!(
            first.action,
            ContextMenuAction::Row(SidebarRowAction::Command("/jobs show shell_x".to_string()))
        );
        assert!(
            !entries
                .iter()
                .any(|entry| matches!(entry.action, ContextMenuAction::Paste)),
            "clickable sidebar menu should not offer paste: {entries:?}"
        );
    }

    #[test]
    fn sidebar_click_resolves_row_actions_inside_section() {
        let mut app = create_test_app();
        app.sidebar_hover.sections.push(SidebarHoverSection {
            content_area: Rect::new(60, 4, 20, 6),
            lines: vec![
                "header".to_string(),
                "job row".to_string(),
                "job detail".to_string(),
                "agent row".to_string(),
            ],
            rows: vec![
                hover_row(4, None),
                hover_row(5, Some("/jobs show shell_x")),
                hover_row(6, Some("/jobs cancel shell_x")),
                SidebarHoverRow {
                    row_y: 7,
                    display_text: "agent row".to_string(),
                    full_text: "agent row".to_string(),
                    detail: None,
                    is_truncated: false,
                    click_action: Some(SidebarRowAction::OpenAgentDetail {
                        agent_id: "agent_123".to_string(),
                    }),
                    stop_action: None,
                    stop_zone_start_col: None,
                    stop_zone_end_col: None,
                },
            ],
        });

        assert_eq!(
            action_command(sidebar_click_action(&app, left_click(65, 5))).as_deref(),
            Some("/jobs show shell_x"),
            "job label row resolves to its show action"
        );
        assert_eq!(
            action_command(sidebar_click_action(&app, left_click(79, 6))).as_deref(),
            Some("/jobs cancel shell_x"),
            "job detail row resolves to its cancel action"
        );
        assert!(matches!(
            sidebar_click_action(&app, left_click(60, 7)),
            Some(SidebarRowAction::OpenAgentDetail { agent_id })
                if agent_id == "agent_123"
        ));
        assert_eq!(
            sidebar_click_action(&app, left_click(65, 4)),
            None,
            "header row has no action"
        );
    }

    #[test]
    fn sidebar_click_routes_inline_stop_zone_before_row_action() {
        let mut app = create_test_app();
        app.work_surface.last_area = Some(Rect::new(60, 4, 20, 4));
        app.sidebar_hover.sections.push(SidebarHoverSection {
            content_area: Rect::new(60, 4, 20, 4),
            lines: vec!["job row [x]".to_string()],
            rows: vec![hover_row_with_stop(
                4,
                "/jobs show shell_x",
                "/jobs cancel shell_x",
            )],
        });

        assert_eq!(
            action_command(sidebar_click_action(&app, left_click(62, 4))).as_deref(),
            Some("/jobs show shell_x"),
            "clicking the label opens the job"
        );
        assert_eq!(
            action_command(sidebar_click_action(&app, left_click(69, 4))).as_deref(),
            Some("/jobs cancel shell_x"),
            "clicking [x] cancels the job"
        );
    }

    #[test]
    fn sidebar_click_routes_agent_inline_stop_zone_before_peek_action() {
        let mut app = create_test_app();
        app.work_surface.last_area = Some(Rect::new(60, 4, 24, 4));
        app.sidebar_hover.sections.push(SidebarHoverSection {
            content_area: Rect::new(60, 4, 24, 4),
            lines: vec!["[~] worker Agent 1 [x]".to_string()],
            rows: vec![SidebarHoverRow {
                row_y: 4,
                display_text: "[~] Agent 1 is working [x]".to_string(),
                full_text: "[~] Agent 1 is working [x]".to_string(),
                detail: None,
                is_truncated: false,
                click_action: Some(SidebarRowAction::OpenAgentDetail {
                    agent_id: "agent_123".to_string(),
                }),
                stop_action: Some(SidebarRowAction::CancelAgent {
                    agent_id: "agent_123".to_string(),
                }),
                stop_zone_start_col: Some(68),
                stop_zone_end_col: Some(71),
            }],
        });

        assert!(matches!(
            sidebar_click_action(&app, left_click(62, 4)),
            Some(SidebarRowAction::OpenAgentDetail { agent_id })
                if agent_id == "agent_123"
        ));
        assert!(matches!(
            sidebar_click_action(&app, left_click(69, 4)),
            Some(SidebarRowAction::CancelAgent { agent_id }) if agent_id == "agent_123"
        ));
    }

    #[test]
    fn sidebar_context_menu_offers_copy_of_hovered_row() {
        let mut app = create_test_app();
        app.work_surface.last_area = Some(Rect::new(60, 4, 20, 6));
        app.sidebar_hover.sections.push(SidebarHoverSection {
            content_area: Rect::new(60, 4, 20, 6),
            lines: vec!["agent row".to_string()],
            rows: vec![SidebarHoverRow {
                row_y: 4,
                display_text: "[~] worker doc-che…".to_string(),
                full_text: "[~] worker doc-checker".to_string(),
                detail: Some("id: agent_123 · 2 step(s)".to_string()),
                is_truncated: true,
                click_action: None,
                stop_action: None,
                stop_zone_start_col: None,
                stop_zone_end_col: None,
            }],
        });

        let entries = build_context_menu_entries(&app, right_click(65, 4));

        let copy = entries
            .iter()
            .find(|entry| matches!(entry.action, ContextMenuAction::CopyText { .. }))
            .expect("sidebar row should offer Copy");
        assert_eq!(copy.label, "Copy row");
        assert!(matches!(
            &copy.action,
            ContextMenuAction::CopyText { text }
                if text == "[~] worker doc-checker\nid: agent_123 · 2 step(s)"
        ));
    }

    #[test]
    fn sidebar_click_outside_section_resolves_to_none() {
        let mut app = create_test_app();
        app.sidebar_hover.sections.push(SidebarHoverSection {
            content_area: Rect::new(60, 4, 20, 6),
            lines: vec!["job row".to_string()],
            rows: vec![hover_row(4, Some("/jobs show shell_x"))],
        });

        // Left of the sidebar (transcript area).
        assert_eq!(sidebar_click_action(&app, left_click(10, 4)), None);
        // Below the section's content area.
        assert_eq!(sidebar_click_action(&app, left_click(65, 30)), None);
        // Inside the section but on an empty row without metadata.
        assert_eq!(sidebar_click_action(&app, left_click(65, 8)), None);
    }

    fn work_row_section(app: &mut App, action: SidebarRowAction) {
        app.work_surface.last_area = Some(Rect::new(60, 4, 20, 6));
        app.sidebar_hover.sections.push(SidebarHoverSection {
            content_area: Rect::new(60, 4, 20, 6),
            lines: vec!["row".to_string()],
            rows: vec![SidebarHoverRow {
                row_y: 4,
                display_text: "row".to_string(),
                full_text: "row text".to_string(),
                detail: None,
                is_truncated: false,
                click_action: Some(action),
                stop_action: None,
                stop_zone_start_col: None,
                stop_zone_end_col: None,
            }],
        });
    }

    /// N14: focusing an agent shows its chat and addresses the composer to
    /// it, so Focus, Message and Open transcript are one action. The menu
    /// offers it once, and every other entry does something different.
    #[test]
    fn agent_row_menu_offers_focus_once() {
        let mut app = create_test_app();
        work_row_section(
            &mut app,
            SidebarRowAction::OpenAgentTranscript {
                agent_id: "agent_1".to_string(),
            },
        );

        let entries = build_context_menu_entries(&app, right_click(65, 4));
        let labels: Vec<&str> = entries.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Focus agent", "Open details", "Copy id", "Copy row"]
        );
        assert!(entries[0].primary);
        assert_eq!(
            entries[0].action,
            ContextMenuAction::Row(SidebarRowAction::OpenAgentTranscript {
                agent_id: "agent_1".to_string()
            })
        );
        for (index, entry) in entries.iter().enumerate() {
            assert!(
                entries[index + 1..]
                    .iter()
                    .all(|e| e.action != entry.action),
                "two rows do the same thing: {labels:?}"
            );
        }
    }

    /// A work item with a stop carries it last, behind the menu's confirm,
    /// as the same typed action the inspector's stop runs.
    #[test]
    fn work_item_menu_puts_a_confirmed_stop_last() {
        let mut app = create_test_app();
        work_row_section(
            &mut app,
            SidebarRowAction::InspectWork {
                title: "job".to_string(),
                body: "body".to_string(),
                stop_action: Some(Box::new(SidebarRowAction::Command(
                    "/jobs cancel shell_x".to_string(),
                ))),
            },
        );

        let entries = build_context_menu_entries(&app, right_click(65, 4));
        assert_eq!(entries[0].label, "Open details");
        let stop = entries.last().expect("stop entry");
        assert_eq!(stop.label, "Stop…");
        assert!(stop.confirm_label.is_some());
        assert_eq!(
            stop.action,
            ContextMenuAction::Row(SidebarRowAction::Command(
                "/jobs cancel shell_x".to_string()
            ))
        );
    }

    /// T8: "Run" used to run through a dedicated arm while a dead arm's
    /// status claimed the command was "staged in composer". A row action now
    /// returns exactly the events a left click on the row produces.
    #[test]
    fn row_action_returns_what_a_left_click_runs() {
        let mut app = create_test_app();
        let outcome = super::apply_context_menu_action(
            &mut app,
            ContextMenuAction::Row(SidebarRowAction::Command("/cost".to_string())),
        );
        let super::ContextMenuOutcome::Events(events) = outcome else {
            panic!("row actions hand their events to the host: {outcome:?}");
        };
        assert!(matches!(
            events.as_slice(),
            [ViewEvent::CommandPaletteSelected {
                action: crate::tui::views::CommandPaletteAction::ExecuteCommand { command },
            }] if command == "/cost"
        ));
        assert!(app.input.is_empty(), "nothing is staged in the composer");

        let outcome = super::apply_context_menu_action(
            &mut app,
            ContextMenuAction::Row(SidebarRowAction::CancelAgent {
                agent_id: "agent_1".to_string(),
            }),
        );
        assert!(matches!(
            outcome,
            super::ContextMenuOutcome::Events(ref events)
                if matches!(events.as_slice(), [ViewEvent::SidebarAgentCancel { agent_id }] if agent_id == "agent_1")
        ));
    }

    #[test]
    fn open_file_hands_the_editor_to_the_host() {
        let mut app = create_test_app();
        let path = PathBuf::from("/ws/src/a.rs");
        let outcome = super::apply_context_menu_action(
            &mut app,
            ContextMenuAction::OpenFileAtLine {
                path: path.clone(),
                line: 12,
            },
        );
        assert!(matches!(
            outcome,
            super::ContextMenuOutcome::OpenInEditor { path: ref p, line: 12 } if *p == path
        ));
    }

    #[test]
    fn hide_and_restore_cells_report_what_changed() {
        let mut app = create_test_app();
        super::apply_context_menu_action(&mut app, ContextMenuAction::HideCell { cell_index: 3 });
        assert!(app.collapsed_cells.contains(&3));
        assert_eq!(app.status_message.as_deref(), Some("Cell hidden"));
        super::apply_context_menu_action(&mut app, ContextMenuAction::ShowAllHidden);
        assert!(app.collapsed_cells.is_empty());
        assert_eq!(
            app.status_message.as_deref(),
            Some("1 hidden cell(s) restored")
        );
    }

    #[test]
    fn extension_action_without_the_panel_says_so() {
        let mut app = create_test_app();
        let outcome = super::apply_context_menu_action(
            &mut app,
            ContextMenuAction::Extension {
                item_id: "gone".to_string(),
                verb: crate::tui::views::ExtensionMenuVerb::Remove,
            },
        );
        assert!(matches!(outcome, super::ContextMenuOutcome::Done));
        assert_eq!(
            app.status_message.as_deref(),
            Some("That extension is no longer listed")
        );
    }

    fn app_with_answer(workspace: &std::path::Path, content: &str) -> App {
        let mut app = create_test_app();
        app.workspace = workspace.to_path_buf();
        app.history = vec![crate::tui::history::HistoryCell::Assistant {
            content: content.to_string(),
            streaming: false,
        }];
        app.resync_history_revisions();
        app.viewport.transcript_cache.ensure(
            &app.history,
            &app.history_revisions,
            80,
            app.transcript_render_options(),
        );
        app.viewport.last_transcript_area = Some(Rect::new(0, 0, 80, 8));
        app.viewport.last_transcript_top = 0;
        app.viewport.last_transcript_total = app.viewport.transcript_cache.total_lines();
        app
    }

    fn line_of(app: &App, needle: &str) -> u16 {
        let index = app
            .viewport
            .transcript_cache
            .lines()
            .iter()
            .position(|line| crate::tui::ui_text::line_to_plain(line).contains(needle))
            .expect("rendered line");
        u16::try_from(index).unwrap()
    }

    /// T4: Open in editor used to follow any absolute path in model output,
    /// and `..` escaped through `workspace.join`. It is offered only for a
    /// file inside the workspace, and the clicked line wins over the cell.
    #[test]
    fn open_in_editor_is_offered_only_inside_the_workspace() {
        let root = tempdir().expect("tempdir");
        let workspace = root.path().join("ws");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(workspace.join("src/b.rs"), "fn b() {}\n").unwrap();
        std::fs::write(root.path().join("secret.rs"), "outside\n").unwrap();
        let outside = root.path().join("secret.rs");

        let app = app_with_answer(
            &workspace,
            &format!(
                "first `src/a.rs:3`\n\nthen --> src/b.rs:7:2\n\nand {}:1",
                outside.display()
            ),
        );
        let open = |app: &App, row: u16| {
            build_context_menu_entries(app, right_click(4, row))
                .into_iter()
                .find_map(|entry| match entry.action {
                    ContextMenuAction::OpenFileAtLine { path, line } => Some((path, line)),
                    _ => None,
                })
        };
        assert_eq!(
            open(&app, line_of(&app, "src/b.rs")),
            Some((workspace.join("src/b.rs"), 7)),
            "the clicked line wins"
        );
        assert_eq!(
            open(&app, line_of(&app, "secret.rs")),
            Some((workspace.join("src/a.rs"), 3)),
            "an outside path on the clicked line falls back to the cell's own"
        );

        let app = app_with_answer(
            &workspace,
            &format!("{}:1 and ../secret.rs:2", outside.display()),
        );
        assert_eq!(open(&app, line_of(&app, "secret.rs")), None);
    }

    /// App chrome (palette, inspector, help) belongs to empty space; on a
    /// message it only pushed the message's own actions down.
    #[test]
    fn chrome_entries_only_on_empty_space() {
        let root = tempdir().expect("tempdir");
        let app = app_with_answer(root.path(), "alpha beta");
        let has_palette = |entries: &[crate::tui::context_menu::ContextMenuEntry]| {
            entries
                .iter()
                .any(|e| e.action == ContextMenuAction::OpenCommandPalette)
        };
        assert!(!has_palette(&build_context_menu_entries(
            &app,
            right_click(4, line_of(&app, "alpha"))
        )));
        let empty = create_test_app();
        let entries = build_context_menu_entries(&empty, right_click(4, 4));
        assert!(has_palette(&entries));
        assert_eq!(entries[0].action, ContextMenuAction::Paste);
    }

    /// T6: Copy took the composer selection while Clear cleared only the
    /// transcript's and still said "Selection cleared", and Open ignored the
    /// composer. All three now act on the same selection.
    #[test]
    fn copy_open_and_clear_act_on_the_composer_selection() {
        let root = tempdir().expect("tempdir");
        let mut app = app_with_answer(root.path(), "alpha beta");
        // A transcript selection too: the composer's must still win.
        app.viewport.transcript_selection.anchor =
            Some(crate::tui::selection::TranscriptSelectionPoint {
                line_index: 0,
                column: 0,
            });
        app.viewport.transcript_selection.head =
            Some(crate::tui::selection::TranscriptSelectionPoint {
                line_index: 0,
                column: 6,
            });
        let select = |app: &mut App| {
            app.input = "hello world".to_string();
            app.selection_anchor = Some(0);
            app.cursor_position = 5;
        };

        select(&mut app);
        super::apply_context_menu_action(&mut app, ContextMenuAction::CopySelection);
        assert_eq!(app.clipboard.last_written_text(), Some("hello"));
        assert_eq!(
            app.status_toasts.back().map(|toast| toast.text.as_str()),
            Some("Copied to the clipboard")
        );

        select(&mut app);
        super::apply_context_menu_action(&mut app, ContextMenuAction::OpenSelection);
        assert_eq!(app.view_stack.top_kind(), Some(ModalKind::Pager));
        app.view_stack.pop();

        super::apply_context_menu_action(&mut app, ContextMenuAction::ClearSelection);
        assert!(
            app.selected_text().is_empty(),
            "the composer selection cleared"
        );
        assert_eq!(app.status_message.as_deref(), Some("Selection cleared"));
        assert!(
            app.viewport.transcript_selection.is_active(),
            "Clear acts on the selection Copy would take, not both"
        );

        super::clear_transcript_selection(&mut app);
        super::apply_context_menu_action(&mut app, ContextMenuAction::ClearSelection);
        assert_eq!(
            app.status_message.as_deref(),
            Some("No selection to clear"),
            "nothing cleared is not reported as cleared"
        );
    }

    /// The menu resolved the file when it opened; a link swapped in before
    /// the click (to a real key file outside the workspace, or to `/`) must
    /// be refused at launch. The key is one the test creates, so the check
    /// does not depend on what the host has in `~/.ssh`.
    #[cfg(unix)]
    #[test]
    fn editor_target_rechecks_links_swapped_in_after_the_menu_opened() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = &dir.path().join("ws");
        let key = dir.path().join("home/.ssh/id_ed25519");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "PRIVATE KEY\n").unwrap();
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        let file = workspace.join("src/a.rs");
        std::fs::write(&file, "fn a() {}\n").unwrap();
        assert!(super::editor_target(workspace, &file).is_some());

        // The file becomes a link to the key.
        std::fs::remove_file(&file).unwrap();
        std::os::unix::fs::symlink(&key, &file).unwrap();
        assert!(file.is_file(), "the link resolves to a real file");
        assert_eq!(super::editor_target(workspace, &file), None);

        // The directory becomes a link to /.
        std::fs::remove_file(&file).unwrap();
        std::fs::remove_dir(workspace.join("src")).unwrap();
        std::os::unix::fs::symlink("/", workspace.join("src")).unwrap();
        assert_eq!(
            super::editor_target(workspace, &workspace.join("src/etc/hosts")),
            None
        );
        assert_eq!(
            super::editor_target(workspace, std::path::Path::new("/etc/hosts")),
            None
        );

        // The refusal the user sees names the file and why.
        let refusal = codewhale_localization::tr(
            codewhale_localization::Locale::En,
            codewhale_localization::MessageId::CtxMenuEditorRefused,
        )
        .replace("{path}", "src/a.rs");
        assert_eq!(
            refusal,
            "Did not open src/a.rs: it is no longer a file inside the workspace"
        );
    }

    /// Ctrl+X deletes the selection only when a native clipboard confirmed
    /// the copy. A terminal (OSC 52 / tmux) copy keeps the text and says why;
    /// a failed copy keeps it and says the copy failed.
    #[test]
    fn cut_deletes_only_after_a_native_copy() {
        fn select_all(app: &mut App, text: &str) {
            app.input = text.to_string();
            app.selection_anchor = Some(0);
            app.cursor_position = text.chars().count();
        }
        fn last_toast(app: &App) -> Option<&str> {
            app.status_toasts.back().map(|toast| toast.text.as_str())
        }

        let mut app = create_test_app();
        select_all(&mut app, "hello");
        super::cut_selection(&mut app);
        assert_eq!(app.clipboard.last_written_text(), Some("hello"));
        assert_eq!(app.input, "", "a native copy cuts");
        assert_eq!(last_toast(&app), Some("Copied to the clipboard"));

        app.clipboard = crate::tui::clipboard::ClipboardHandler::terminal_only_for_test();
        select_all(&mut app, "kept");
        super::cut_selection(&mut app);
        assert_eq!(
            app.input, "kept",
            "an unconfirmed terminal copy keeps the text"
        );
        assert_eq!(
            last_toast(&app),
            Some(
                "Sent to the terminal clipboard; the text stays because terminals do not confirm copies"
            )
        );

        app.clipboard = crate::tui::clipboard::ClipboardHandler::unavailable_for_test(false);
        select_all(&mut app, "also kept");
        super::cut_selection(&mut app);
        assert_eq!(app.input, "also kept", "a failed copy keeps the text");
        assert!(
            last_toast(&app).is_some_and(|toast| toast.starts_with("Clipboard copy failed: ")),
            "{:?}",
            last_toast(&app)
        );
    }

    /// A right-click on a row's inline stop zone opens the row's own menu;
    /// the stop is offered last behind the confirm, never as the primary
    /// entry that one click would run.
    #[test]
    fn right_click_on_a_stop_zone_keeps_the_stop_confirmed() {
        let mut app = create_test_app();
        work_row_section(
            &mut app,
            SidebarRowAction::Command("/jobs show shell_x".to_string()),
        );
        let row = &mut app.sidebar_hover.sections[0].rows[0];
        row.stop_action = Some(SidebarRowAction::Command(
            "/jobs cancel shell_x".to_string(),
        ));
        row.stop_zone_start_col = Some(76);
        row.stop_zone_end_col = Some(79);

        let entries = build_context_menu_entries(&app, right_click(77, 4));
        let primary = entries.iter().find(|entry| entry.primary).expect("primary");
        assert_eq!(
            primary.action,
            ContextMenuAction::Row(SidebarRowAction::Command("/jobs show shell_x".to_string()))
        );
        assert!(primary.confirm_label.is_none());
        let stop = entries.last().expect("stop entry");
        assert_eq!(stop.label, "Stop…");
        assert_eq!(
            stop.action,
            ContextMenuAction::Row(SidebarRowAction::Command(
                "/jobs cancel shell_x".to_string()
            ))
        );
        assert!(stop.confirm_label.is_some(), "the stop needs the confirm");
    }

    /// N9: an OSC 52 / tmux write is never acknowledged, so its receipt says
    /// the text was sent to the terminal, not that it was copied.
    #[test]
    fn copy_receipts_name_the_transport() {
        fn last_toast(app: &App) -> Option<&str> {
            app.status_toasts.back().map(|toast| toast.text.as_str())
        }

        let mut app = create_test_app();
        super::apply_context_menu_action(
            &mut app,
            ContextMenuAction::CopyText {
                text: "agent_1".to_string(),
            },
        );
        assert_eq!(app.clipboard.last_written_text(), Some("agent_1"));
        assert_eq!(last_toast(&app), Some("Copied to the clipboard"));

        app.clipboard = crate::tui::clipboard::ClipboardHandler::terminal_only_for_test();
        super::apply_context_menu_action(
            &mut app,
            ContextMenuAction::CopyText {
                text: "agent_1".to_string(),
            },
        );
        assert_eq!(
            last_toast(&app),
            Some("Sent to the terminal clipboard (terminals do not confirm it)")
        );

        // The terminal lane holds one write at a time; let the first land.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.clipboard.poll_write_completion().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "terminal write landed"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        app.input = "hello".to_string();
        app.selection_anchor = Some(0);
        app.cursor_position = 5;
        super::apply_context_menu_action(&mut app, ContextMenuAction::CopySelection);
        assert_eq!(
            last_toast(&app),
            Some("Sent to the terminal clipboard (terminals do not confirm it)")
        );

        app.clipboard = crate::tui::clipboard::ClipboardHandler::unavailable_for_test(false);
        super::apply_context_menu_action(
            &mut app,
            ContextMenuAction::CopyText {
                text: "agent_1".to_string(),
            },
        );
        assert!(
            last_toast(&app).is_some_and(|toast| toast.starts_with("Clipboard copy failed: ")),
            "{:?}",
            last_toast(&app)
        );
    }

    /// T11: Paste with nothing to paste used to do nothing and say nothing.
    #[test]
    fn paste_with_nothing_to_paste_says_so() {
        let mut app = create_test_app();
        app.clipboard = crate::tui::clipboard::ClipboardHandler::unavailable_for_test(false);
        app.input = "draft".to_string();
        super::apply_context_menu_action(&mut app, ContextMenuAction::Paste);
        assert_eq!(app.input, "draft");
        assert_eq!(
            app.status_message.as_deref(),
            Some("Nothing to paste: the clipboard is empty or could not be read")
        );
    }

    #[test]
    fn worker_transcript_formats_visible_activity_without_thinking() {
        let transcript = agent_transcript_text(&json!({
            "message_count": 2,
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "Survey Harnesses", "cache_control": null}]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "private chain of thought", "signature": null},
                    {"type": "tool_use", "id": "call_1", "name": "list_dir", "input": {"path": "/tmp"}, "caller": null},
                    {"type": "text", "text": "I found the workspace.", "cache_control": null}
                ]}
            ]
        }));

        assert!(transcript.contains("── user ──\nSurvey Harnesses"));
        assert!(transcript.contains("→ list_dir"));
        assert!(transcript.contains("I found the workspace."));
        assert!(!transcript.contains("private chain of thought"));
    }

    #[test]
    fn worker_open_reads_first_and_last_turns_from_complete_artifact() {
        let tmp = tempdir().expect("tempdir");
        let agent_id = "agent_large_chat";
        let early = format!("EARLY-OPEN-MARKER\n{}", "a".repeat(1_100_000));
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: early,
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "LAST-OPEN-MARKER".to_string(),
                    cache_control: None,
                }],
            },
        ];
        let artifact = crate::tools::subagent::write_subagent_transcript_artifact_for_test(
            tmp.path(),
            agent_id,
            &messages,
        )
        .expect("write complete worker transcript");
        assert!(
            std::fs::metadata(artifact)
                .expect("artifact metadata")
                .len()
                > 1024 * 1024,
            "regression requires a transcript larger than the resident handle budget"
        );

        let mut app = create_test_app();
        app.workspace = tmp.path().to_path_buf();
        {
            let mut store = app
                .runtime_services
                .handle_store
                .try_lock()
                .expect("handle store");
            let _ = store.insert_json(
                format!("agent:{agent_id}"),
                "full_transcript",
                json!({
                    "kind": "subagent_full_transcript",
                    "message_count": 2,
                    "omitted_messages": 1,
                    "messages_complete": false,
                    "messages": [messages[1].clone()],
                }),
            );
        }

        crate::tui::agent_focus::focus_agent(&mut app, agent_id);
        let focus = app.agent_focus.as_ref().expect("Open focuses the worker");
        let body = focus
            .cells
            .iter()
            .flat_map(|cell| cell.transcript_lines(120))
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("EARLY-OPEN-MARKER"), "{body}");
        assert!(body.contains("LAST-OPEN-MARKER"), "{body}");
        assert_eq!(
            focus.omitted_messages, 0,
            "Open must use the complete artifact, not the compacted resident tail"
        );
    }

    #[cfg(test)]
    mod composer_selection_tests {
        use super::super::*;

        #[test]
        fn word_bounds_select_words_and_respect_cjk() {
            // Chars: fix(0-2) sp(3) the(4-6) sp(7) 深(8) 海(9) sp(10) test.rs(11-17)
            let text = "fix the 深海 test.rs";
            assert_eq!(composer_word_bounds(text, 2), (0, 3)); // 'fix'
            assert_eq!(composer_word_bounds(text, 5), (4, 7)); // 'the'
            assert_eq!(composer_word_bounds(text, 9), (8, 10)); // '深海' (space at 10)
            assert_eq!(composer_word_bounds(text, 11), (11, 15)); // 'test' (stops at '.')
            assert_eq!(composer_word_bounds(text, 15), (15, 16)); // '.'
            assert_eq!(composer_word_bounds(text, 16), (16, 18)); // 'rs'
        }

        #[test]
        fn click_bounds_are_char_indices_on_multibyte_text() {
            // The click maps to a char index and the result becomes the
            // char-indexed cursor/anchor. Byte math here panicked on a
            // triple-click inside CJK text and skewed double-click spans.
            let text = "你好\n世界 ok";
            assert_eq!(composer_line_bounds(text, 1), (0, 2));
            assert_eq!(composer_line_bounds(text, 4), (3, 8));
            assert_eq!(composer_word_bounds(text, 3), (3, 5)); // '世界'
            assert_eq!(composer_word_bounds(text, 6), (6, 8)); // 'ok'
            let mixed = "fix 深海 test";
            assert_eq!(composer_word_bounds(mixed, 8), (7, 11)); // 'test'
        }

        #[test]
        fn word_bounds_at_punctuation_returns_the_single_char() {
            let text = "a, b";
            assert_eq!(composer_word_bounds(text, 1), (1, 2)); // ','
        }

        #[test]
        fn line_bounds_exclude_the_newline() {
            let text = "first\nsecond third\nfourth";
            assert_eq!(composer_line_bounds(text, 2), (0, 5));
            assert_eq!(composer_line_bounds(text, 9), (6, 18));
            assert_eq!(composer_line_bounds(text, 20), (19, 25));
            assert_eq!(composer_line_bounds(text, 0), (0, 5));
        }

        #[test]
        fn click_classification_resets_outside_the_window_or_slop() {
            let mut trace = None;
            assert_eq!(
                classify_composer_click(&mut trace, 10, 4),
                ComposerClickGesture::Caret
            );
            assert_eq!(
                classify_composer_click(&mut trace, 10, 4),
                ComposerClickGesture::Word
            );
            assert_eq!(
                classify_composer_click(&mut trace, 10, 4),
                ComposerClickGesture::Line
            );
            // A click far away resets the chain back to a caret.
            assert_eq!(
                classify_composer_click(&mut trace, 10, 40),
                ComposerClickGesture::Caret
            );
            assert_eq!(
                classify_composer_click(&mut trace, 10, 40),
                ComposerClickGesture::Word
            );
        }
    }
}

#[cfg(test)]
mod primary_tests;

//! `/statusline` multi-select picker.
//!
//! Mirrors codex-rs's `bottom_pane::status_line_setup` ergonomically: a
//! checklist of bottom-chrome items the user can toggle on/off with Space (or
//! Enter), moved through with ↑/↓, applied immediately so the live chrome
//! reflects every change. Enter saves to `~/.deepseek/config.toml` under
//! `tui.status_items`; Esc reverts to the snapshot taken on open.
//!
//! Every row here changes what is on screen (#5950): the metrics line's
//! segments ([`crate::tui::ui::frame::info_segments`]) and the posture bar's
//! mode chip. Between 0.9.12 and that fix the list was persisted and never
//! read, so the checklist was decoration — a new variant belongs in
//! `crates/runtime/src/config.rs` only once something paints it.

use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Widget},
};

use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, action_footer_lines,
    centered_modal_area, render_modal_footer, render_modal_surface, render_panel_scroll_rail,
};
use codewhale_config::ProviderKind;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::config::StatusItem;
use codewhale_tui_kit::menu_style;
use unicode_width::UnicodeWidthStr;

/// Picker state. We hold both the user's working selection AND the original
/// snapshot so Esc can perfectly revert the live preview.
pub struct StatusPickerView {
    /// Every available item, in the order shown to the user. We keep this
    /// list ordered so toggles produce a stable on-screen layout that
    /// doesn't shuffle as items flip.
    rows: Vec<StatusItem>,
    /// Indices in `rows` currently checked on (the user's working set).
    selected: Vec<bool>,
    /// Highlighted row.
    cursor: usize,
    /// Snapshot of `app.status_items` at open time so Esc reverts cleanly.
    original: Vec<StatusItem>,
    locale: Locale,
    row_hitboxes: RefCell<Vec<(usize, Rect)>>,
}

impl StatusPickerView {
    #[must_use]
    pub fn new(active: &[StatusItem], provider: ProviderKind, locale: Locale) -> Self {
        let rows: Vec<StatusItem> = StatusItem::all()
            .iter()
            .filter(|item| item.is_available_for(provider))
            .copied()
            .collect();
        let selected: Vec<bool> = rows
            .iter()
            .map(|item| {
                active.contains(item)
                    || (active.contains(&StatusItem::SessionMetrics)
                        && matches!(item, StatusItem::Ttft | StatusItem::OutputRate))
            })
            .collect();
        Self {
            rows,
            selected,
            cursor: 0,
            original: active.to_vec(),
            locale,
            row_hitboxes: RefCell::new(Vec::new()),
        }
    }

    /// Build the current selection in the same order the user sees it.
    /// Preserves `StatusItem::all()` order so toggling produces deterministic
    /// `tui.status_items` output (no churn-induced diffs in config.toml).
    fn current_selection(&self) -> Vec<StatusItem> {
        self.rows
            .iter()
            .zip(self.selected.iter())
            .filter_map(|(item, on)| if *on { Some(*item) } else { None })
            .collect()
    }

    /// Apply one [`list_nav`](codewhale_tui_kit::list_nav) motion (#6290), returning
    /// whether it was consumed. Vertical motions wrap at the ends for
    /// Prev/Next and clamp for paging and Home/End; the horizontal axis does
    /// not exist on this single-column checklist. The checklist fits on one
    /// screen, so a page is the whole list.
    fn apply_motion(&mut self, motion: codewhale_tui_kit::list_nav::Motion) -> bool {
        let Some(next) = codewhale_tui_kit::list_nav::apply(
            self.cursor,
            self.rows.len(),
            self.rows.len().max(1),
            motion,
        ) else {
            return false;
        };
        self.cursor = next;
        true
    }

    fn toggle_current(&mut self) {
        if let Some(slot) = self.selected.get_mut(self.cursor) {
            *slot = !*slot;
        }
    }

    fn live_preview_event(&self) -> ViewEvent {
        ViewEvent::StatusItemsUpdated {
            items: self.current_selection(),
            final_save: false,
        }
    }

    fn final_event(&self) -> ViewEvent {
        ViewEvent::StatusItemsUpdated {
            items: self.current_selection(),
            final_save: true,
        }
    }

    fn revert_event(&self) -> ViewEvent {
        ViewEvent::StatusItemsUpdated {
            items: self.original.clone(),
            final_save: false,
        }
    }
}

impl ModalView for StatusPickerView {
    fn kind(&self) -> ModalKind {
        ModalKind::StatusPicker
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        // Movement keys come from the shared vocabulary (#6290): `j`/`k`,
        // Home/End and the page keys mean here what they mean on every other
        // list. This match owns only the checklist's own verbs.
        if let Some(motion) = codewhale_tui_kit::list_nav::motion(&key)
            && self.apply_motion(motion)
        {
            return ViewAction::None;
        }
        match key.code {
            KeyCode::Esc => {
                // Roll the live preview back to the snapshot so Esc means
                // "take me back to where I was."
                ViewAction::EmitAndClose(self.revert_event())
            }
            KeyCode::Enter => ViewAction::EmitAndClose(self.final_event()),
            KeyCode::Char(' ') | KeyCode::Char('x') | KeyCode::Char('X') => {
                self.toggle_current();
                ViewAction::Emit(self.live_preview_event())
            }
            KeyCode::Char('a') | KeyCode::Char('A')
                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                // Quality-of-life: 'a' selects all so the user can quickly
                // see every chip available before paring back.
                self.selected.fill(true);
                ViewAction::Emit(self.live_preview_event())
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                // 'n' clears all so the user can build up from scratch.
                self.selected.fill(false);
                ViewAction::Emit(self.live_preview_event())
            }
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.apply_motion(codewhale_tui_kit::list_nav::Motion::Prev);
            }
            MouseEventKind::ScrollDown => {
                self.apply_motion(codewhale_tui_kit::list_nav::Motion::Next);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let clicked = self.row_hitboxes.borrow().iter().find_map(|(index, rect)| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                        .then_some(*index)
                });
                if let Some(index) = clicked {
                    self.cursor = index;
                    self.toggle_current();
                    return ViewAction::Emit(self.live_preview_event());
                }
            }
            _ => {}
        }
        ViewAction::None
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let hints = [
            ActionHint::new(
                "Space",
                tr(self.locale, MessageId::StatusPickerActionToggle),
            ),
            ActionHint::new("a", tr(self.locale, MessageId::StatusPickerActionAll)),
            ActionHint::new("n", tr(self.locale, MessageId::StatusPickerActionNone)),
            ActionHint::new("Enter", tr(self.locale, MessageId::StatusPickerActionSave)),
            ActionHint::new("Esc", tr(self.locale, MessageId::StatusPickerActionCancel)),
        ];
        // Two header lines + one row per StatusItem + the action footer rows
        // as they wrap at the popup's inner width, plus the border rows, and
        // the padding rows above and below when the whole list fits.
        // centered_modal_area clamps this to the frame and lets the scroll
        // offset absorb any remaining overflow.
        let popup_width = centered_modal_area(area, 64, 8, 40, 8).width;
        let footer_rows = action_footer_lines(&hints, popup_width.saturating_sub(4))
            .len()
            .max(1) as u16;
        let unpadded_height = (self.rows.len() as u16)
            .saturating_add(4)
            .saturating_add(footer_rows);
        let pad = u16::from(unpadded_height.saturating_add(2) <= area.height.saturating_sub(2));
        let popup_area =
            centered_modal_area(area, 64, unpadded_height.saturating_add(2 * pad), 40, 8);

        render_modal_surface(area, popup_area, buf);

        let block = Block::default()
            .title(Line::from(Span::styled(
                tr(self.locale, MessageId::StatusPickerTitle),
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG))
            .padding(Padding::new(1, 1, pad, pad));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let content = render_modal_footer(inner, buf, &hints);

        self.row_hitboxes.borrow_mut().clear();
        let visible_rows = content.height.saturating_sub(2) as usize;
        let row_start = visible_row_start(self.rows.len(), self.cursor, visible_rows);
        let row_width = content
            .width
            .saturating_sub(u16::from(visible_rows < self.rows.len()));

        let mut lines: Vec<Line> = Vec::with_capacity(visible_rows + 2);
        lines.push(Line::from(Span::styled(
            tr(self.locale, MessageId::StatusPickerInstruction),
            Style::default().fg(palette::TEXT_MUTED),
        )));
        lines.push(Line::from(""));

        for (idx, item) in self
            .rows
            .iter()
            .enumerate()
            .skip(row_start)
            .take(visible_rows)
        {
            self.row_hitboxes.borrow_mut().push((
                idx,
                Rect::new(
                    content.x,
                    content.y + 2 + (idx - row_start) as u16,
                    row_width,
                    1,
                ),
            ));
            let checked = *self.selected.get(idx).unwrap_or(&false);
            let is_cursor = idx == self.cursor;
            let mark = if checked { "[✓]" } else { "[ ]" };

            let row_style = if is_cursor {
                menu_style::selected_row_style()
            } else if checked {
                Style::default().fg(palette::TEXT_PRIMARY)
            } else {
                Style::default().fg(palette::TEXT_MUTED)
            };
            let hint_style = if is_cursor {
                menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
            } else {
                Style::default().fg(palette::TEXT_DIM)
            };
            let pointer = codewhale_tui_kit::glyphs::selection_marker(is_cursor);

            if is_cursor {
                let selected_style = menu_style::selected_row_style();
                let line =
                    status_row_text(pointer, mark, item, self.locale, usize::from(row_width));
                lines.push(Line::from(Span::styled(line, selected_style)));
            } else {
                let label = item.label();
                let hint = item.hint(self.locale);
                let prefix = format!(" {pointer} {mark} {label}  (");
                let truncated_hint = codewhale_tui_kit::ui_text::semantic_truncate_between_affixes(
                    &prefix,
                    hint.as_ref(),
                    ")",
                    usize::from(row_width),
                );
                lines.push(Line::from(vec![
                    Span::styled(format!(" {pointer} "), row_style),
                    Span::styled(mark.to_string(), row_style),
                    Span::styled(" ", row_style),
                    Span::styled(label.to_string(), row_style),
                    Span::styled("  ", row_style),
                    Span::styled(format!("({})", truncated_hint), hint_style),
                ]));
            }
        }

        Paragraph::new(lines).render(content, buf);
        render_panel_scroll_rail(
            Rect {
                y: content.y.saturating_add(2),
                height: content.height.saturating_sub(2),
                ..content
            },
            buf,
            self.rows.len(),
            row_start,
            visible_rows,
            true,
        );
    }
}

fn visible_row_start(total_rows: usize, cursor: usize, visible_rows: usize) -> usize {
    if total_rows == 0 || visible_rows == 0 || total_rows <= visible_rows {
        return 0;
    }
    let max_start = total_rows - visible_rows;
    cursor
        .saturating_add(1)
        .saturating_sub(visible_rows)
        .min(max_start)
}

fn status_row_text(
    pointer: &str,
    mark: &str,
    item: &StatusItem,
    locale: Locale,
    width: usize,
) -> String {
    let prefix = format!(" {pointer} {mark} {}  (", item.label());
    let mut text = codewhale_tui_kit::ui_text::semantic_truncate_with_affixes(
        &prefix,
        item.hint(locale).as_ref(),
        ")",
        width,
    );
    let current_width = text.width();
    if current_width < width {
        text.push_str(&" ".repeat(width - current_width));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_localization::Locale;

    #[test]
    fn opens_with_active_items_pre_selected() {
        let active = StatusItem::default_footer();
        let view = StatusPickerView::new(&active, ProviderKind::Deepseek, Locale::En);
        assert_eq!(view.current_selection(), active);
    }

    #[test]
    fn legacy_metrics_can_be_split_and_cancel_restores_the_saved_pair() {
        let original = vec![StatusItem::SessionMetrics];
        let mut view = StatusPickerView::new(&original, ProviderKind::Stepfun, Locale::En);
        assert_eq!(
            view.current_selection(),
            vec![StatusItem::Ttft, StatusItem::OutputRate]
        );
        view.cursor = view
            .rows
            .iter()
            .position(|item| *item == StatusItem::OutputRate)
            .unwrap();
        view.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(view.current_selection(), vec![StatusItem::Ttft]);
        match view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)) {
            ViewAction::EmitAndClose(ViewEvent::StatusItemsUpdated { items, final_save }) => {
                assert_eq!(items, original);
                assert!(!final_save);
            }
            action => panic!("unexpected cancel: {action:?}"),
        }
    }

    #[test]
    fn mouse_toggles_the_painted_row_after_scrolling_a_short_picker() {
        let mut view = StatusPickerView::new(
            &StatusItem::default_footer(),
            ProviderKind::Stepfun,
            Locale::En,
        );
        view.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let area = Rect::new(0, 0, 40, 12);
        view.render(area, &mut Buffer::empty(area));
        let (index, rect) = *view.row_hitboxes.borrow().last().expect("visible row");
        let was_selected = view.selected[index];
        let action = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x + 2,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            action,
            ViewAction::Emit(ViewEvent::StatusItemsUpdated {
                final_save: false,
                ..
            })
        ));
        assert_eq!(view.cursor, index);
        assert_eq!(view.selected[index], !was_selected);
    }

    #[test]
    fn space_toggles_current_row_and_emits_live_preview() {
        let active = StatusItem::default_footer();
        let mut view = StatusPickerView::new(&active, ProviderKind::Deepseek, Locale::En);
        let action = view.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        match action {
            ViewAction::Emit(ViewEvent::StatusItemsUpdated { items, final_save }) => {
                assert!(!final_save);
                assert!(!items.contains(&StatusItem::Mode));
            }
            other => panic!("expected live preview emit, got {other:?}"),
        }
    }

    #[test]
    fn enter_emits_final_save() {
        let active = StatusItem::default_footer();
        let mut view = StatusPickerView::new(&active, ProviderKind::Deepseek, Locale::En);
        let action = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            ViewAction::EmitAndClose(ViewEvent::StatusItemsUpdated { final_save, .. }) => {
                assert!(final_save);
            }
            other => panic!("expected final save EmitAndClose, got {other:?}"),
        }
    }

    #[test]
    fn esc_reverts_to_snapshot() {
        let active = StatusItem::default_footer();
        let mut view = StatusPickerView::new(&active, ProviderKind::Deepseek, Locale::En);
        view.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        // Move through the shared vocabulary, the same path a key takes.
        view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        view.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        let action = view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        match action {
            ViewAction::EmitAndClose(ViewEvent::StatusItemsUpdated { items, final_save }) => {
                assert!(!final_save);
                assert_eq!(items, active);
            }
            other => panic!("expected revert EmitAndClose, got {other:?}"),
        }
    }

    #[test]
    fn select_all_and_select_none_keys_work() {
        let active: Vec<StatusItem> = Vec::new();
        let mut view = StatusPickerView::new(&active, ProviderKind::Deepseek, Locale::En);
        let action = view.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        match action {
            ViewAction::Emit(ViewEvent::StatusItemsUpdated { items, .. }) => {
                assert_eq!(items.len(), StatusItem::all().len());
            }
            other => panic!("expected select-all emit, got {other:?}"),
        }
        let action = view.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        match action {
            ViewAction::Emit(ViewEvent::StatusItemsUpdated { items, .. }) => {
                assert!(items.is_empty());
            }
            other => panic!("expected select-none emit, got {other:?}"),
        }
    }

    #[test]
    fn arrow_keys_wrap_cursor_at_edges() {
        let active = StatusItem::default_footer();
        let mut view = StatusPickerView::new(&active, ProviderKind::Deepseek, Locale::En);
        assert_eq!(view.cursor, 0);
        view.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(view.cursor, StatusItem::all().len() - 1);
        view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(view.cursor, 0);
        view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(view.cursor, 1);
        view.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(view.cursor, 0);
    }

    #[test]
    fn visible_row_start_keeps_cursor_in_view() {
        assert_eq!(visible_row_start(14, 0, 8), 0);
        assert_eq!(visible_row_start(14, 7, 8), 0);
        assert_eq!(visible_row_start(14, 8, 8), 1);
        assert_eq!(visible_row_start(14, 13, 8), 6);
    }

    #[test]
    fn selected_row_text_fills_available_width() {
        let text = status_row_text("▸", "[ ]", &StatusItem::Cache, Locale::En, 40);
        assert_eq!(text.width(), 40);
        assert!(text.starts_with(" ▸ [ ] Prompt cache hit rate"));
    }

    #[test]
    fn selected_row_text_semantically_truncates_hint_at_narrow_width() {
        let text = status_row_text("▸", "[ ]", &StatusItem::Cache, Locale::En, 44);
        assert_eq!(text.width(), 44);
        // Cut at a word boundary, never mid-word.
        assert!(text.contains("% of…"), "{text:?}");
        assert!(!text.contains("% of p"), "{text:?}");
    }

    #[test]
    fn balance_offered_for_prepaid_providers_and_hidden_for_local() {
        let active = StatusItem::default_footer();
        let openrouter = StatusPickerView::new(&active, ProviderKind::Openrouter, Locale::En);
        assert!(openrouter.rows.contains(&StatusItem::Balance));
        let ollama = StatusPickerView::new(&active, ProviderKind::Ollama, Locale::En);
        assert!(!ollama.rows.contains(&StatusItem::Balance));
        assert!(ollama.rows.contains(&StatusItem::Mode));
    }

    #[test]
    fn status_picker_displays_localized_title_for_zh_hans() {
        assert_eq!(tr(Locale::ZhHans, MessageId::StatusPickerTitle), " 状态行 ");
    }

    /// The four terminal sizes the v0.8.66 modal blocker (#3732) requires
    /// every overlay to remain readable and fully operable at.
    const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

    #[test]
    fn status_picker_is_usable_and_opaque_at_blocker_sizes() {
        use crate::tui::views::ViewStack;
        let active = StatusItem::default_footer();
        for (w, h) in BLOCKER_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("X");
                }
            }
            let mut stack = ViewStack::new();
            stack.push(StatusPickerView::new(
                &active,
                ProviderKind::Deepseek,
                Locale::En,
            ));
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect();
            let text = rows.join("\n");

            for label in ["toggle", "all", "none", "save", "cancel"] {
                assert!(text.contains(label), "{w}x{h}: missing footer '{label}'");
            }
            assert!(
                !text.contains('X'),
                "{w}x{h}: background bleed-through into modal surface"
            );
            assert_eq!(
                buf[(w / 2, h / 2)].bg,
                palette::WHALE_BG,
                "{w}x{h}: modal interior must be opaque"
            );
            for (y, row) in rows.iter().enumerate() {
                assert!(
                    UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                    "{w}x{h}: row {y} overflows width: {row:?}"
                );
            }
        }
    }

    #[test]
    fn status_picker_no_english_leak_in_non_en_locales() {
        for locale in [
            Locale::Ja,
            Locale::ZhHans,
            Locale::ZhHant,
            Locale::PtBr,
            Locale::Es419,
            Locale::Vi,
            Locale::Ca,
            Locale::De,
            Locale::Fr,
            Locale::Id,
            Locale::Hi,
            Locale::Ru,
            Locale::Uk,
        ] {
            let title = tr(locale, MessageId::StatusPickerTitle);
            if locale == Locale::De {
                // German "Statuszeile" is the correct native term — "Status"
                // is a German word, not an English leak.
                assert_eq!(title, " Statuszeile ");
            } else {
                assert!(
                    !title.contains("Status"),
                    "{} leaks English in title: {title}",
                    locale.tag()
                );
            }
            let instruction = tr(locale, MessageId::StatusPickerInstruction);
            assert!(
                !instruction.contains("footer"),
                "{} leaks English in instruction: {instruction}",
                locale.tag()
            );
        }
    }
}

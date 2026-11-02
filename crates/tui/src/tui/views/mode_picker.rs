//! `/mode` picker for the currently supported interactive modes.

use std::cell::RefCell;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Padding, Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::AppModeUi;
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, centered_modal_area,
    render_modal_footer, render_modal_surface,
};
use codewhale_config::AppMode;
use codewhale_localization::Locale;
use codewhale_palette as palette;
use codewhale_tui_kit::menu_style;

// Operate is visible because the engine now enforces a coordinator/worker
// boundary while allowing ordinary conversation and asynchronous dispatch.
const VISIBLE_MODES: [AppMode; 3] = [AppMode::Agent, AppMode::Plan, AppMode::Operate];

pub struct ModePickerView {
    cursor: usize,
    locale: Locale,
    row_hitboxes: RefCell<Vec<Rect>>,
}

impl ModePickerView {
    #[must_use]
    pub fn new(current: AppMode, locale: Locale) -> Self {
        let cursor = VISIBLE_MODES
            .iter()
            .position(|mode| *mode == current)
            .unwrap_or(0);
        Self {
            cursor,
            locale,
            row_hitboxes: RefCell::new(Vec::new()),
        }
    }

    fn selected_mode(&self) -> AppMode {
        VISIBLE_MODES
            .get(self.cursor)
            .copied()
            .unwrap_or(AppMode::Agent)
    }

    /// Apply one [`list_nav`](codewhale_tui_kit::list_nav) motion (#6290), returning
    /// whether it was consumed. Vertical motions cover the whole list —
    /// Home/End and the page keys included. The horizontal axis has nowhere
    /// to go on a single-column surface, so those motions are not consumed.
    fn apply_motion(&mut self, motion: codewhale_tui_kit::list_nav::Motion) -> bool {
        let Some(next) = codewhale_tui_kit::list_nav::apply(
            self.cursor,
            VISIBLE_MODES.len(),
            VISIBLE_MODES.len(),
            motion,
        ) else {
            return false;
        };
        self.cursor = next;
        true
    }

    fn select_by_number(&mut self, number: char) -> Option<ViewAction> {
        let idx = VISIBLE_MODES
            .iter()
            .position(|mode| mode.number() == number)?;
        self.cursor = idx;
        Some(ViewAction::EmitAndClose(ViewEvent::ModeSelected {
            mode: self.selected_mode(),
        }))
    }
}

impl ModalView for ModePickerView {
    fn kind(&self) -> ModalKind {
        ModalKind::ModePicker
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        // Movement keys come from the shared vocabulary (#6290), so
        // `j`/`k`, Home/End and the page keys mean here exactly what they
        // mean on every other list. This match owns only the keys the
        // vocabulary does not claim.
        if let Some(motion) = codewhale_tui_kit::list_nav::motion(&key)
            && self.apply_motion(motion)
        {
            return ViewAction::None;
        }
        match key.code {
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Enter => ViewAction::EmitAndClose(ViewEvent::ModeSelected {
                mode: self.selected_mode(),
            }),
            KeyCode::Char(number) => self.select_by_number(number).unwrap_or(ViewAction::None),
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.apply_motion(codewhale_tui_kit::list_nav::Motion::Prev);
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.apply_motion(codewhale_tui_kit::list_nav::Motion::Next);
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let clicked = self.row_hitboxes.borrow().iter().position(|rect| {
                    rect.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
                });
                if let Some(index) = clicked {
                    self.cursor = index;
                    return self.handle_key(KeyEvent::new(KeyCode::Enter, mouse.modifiers));
                }
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let popup_height = u16::try_from(VISIBLE_MODES.len()).unwrap_or(2) + 7;
        let popup_area = centered_modal_area(area, 68, popup_height, 44, 8);

        render_modal_surface(area, popup_area, buf);

        let block = Block::default()
            .title(Line::from(Span::styled(
                " Mode ",
                Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG))
            .padding(Padding::uniform(1));

        let inner = block.inner(popup_area);
        block.render(popup_area, buf);

        let content = render_modal_footer(
            inner,
            buf,
            &[
                ActionHint::new("↑/↓", "move"),
                ActionHint::new("Enter", "select"),
                ActionHint::new("Esc", "cancel"),
            ],
        );

        self.row_hitboxes.borrow_mut().clear();

        let mut lines = Vec::with_capacity(VISIBLE_MODES.len());

        for (idx, mode) in VISIBLE_MODES.iter().copied().enumerate() {
            let is_cursor = idx == self.cursor;
            let row_style = if is_cursor {
                menu_style::selected_row_style()
            } else {
                Style::default().fg(palette::TEXT_PRIMARY)
            };
            let hint_style = if is_cursor {
                menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
            } else {
                Style::default().fg(palette::TEXT_MUTED)
            };
            let pointer = codewhale_tui_kit::glyphs::selection_marker(is_cursor);
            let name = mode.display_name_localized(self.locale);
            let hint = mode.picker_hint_localized(self.locale);
            // Pad by terminal columns, not scalar count, so wide (CJK) mode
            // names keep the hint column aligned.
            let pad = " ".repeat(8usize.saturating_sub(UnicodeWidthStr::width(&*name)));
            let prefix = format!("{pointer} {}. {name}{pad}", mode.number());
            // A hint is prose: the pane edge used to cut it mid-word
            // (`ask f`, `before ac`). Truncate at a word joint instead —
            // a clipped clause reads as a sentence, a clipped word reads
            // as a bug.
            let hint_width =
                usize::from(content.width).saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
            let hint = codewhale_tui_kit::ui_text::semantic_truncate(hint.as_ref(), hint_width);

            lines.push(Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(hint, hint_style),
            ]));
            self.row_hitboxes.borrow_mut().push(Rect::new(
                content.x,
                content
                    .y
                    .saturating_add(u16::try_from(idx).unwrap_or(u16::MAX)),
                content.width,
                1,
            ));
        }

        Paragraph::new(lines).render(content, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn opens_on_current_mode() {
        let view = ModePickerView::new(AppMode::Plan, Locale::En);
        assert_eq!(view.selected_mode(), AppMode::Plan);
    }

    #[test]
    fn enter_emits_selected_mode() {
        let mut view = ModePickerView::new(AppMode::Agent, Locale::En);
        view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let action = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            ViewAction::EmitAndClose(ViewEvent::ModeSelected { mode }) => {
                assert_eq!(mode, AppMode::Plan);
            }
            other => panic!("expected ModeSelected, got {other:?}"),
        }
    }

    /// The four terminal sizes the v0.8.66 modal blocker (#3732) requires
    /// every overlay to remain readable and fully operable at.
    const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

    fn render_at(width: u16, height: u16) -> (Buffer, Rect) {
        use crate::tui::views::ViewStack;
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        // Pre-fill with a sentinel so any cell the composited modal fails to
        // paint (bleed-through) is detectable as a surviving 'X'.
        for y in 0..height {
            for x in 0..width {
                buf[(x, y)].set_symbol("X");
            }
        }
        // Render through the ViewStack so the shared opaque backdrop is painted
        // exactly as it is in production.
        let mut stack = ViewStack::new();
        stack.push(ModePickerView::new(AppMode::Agent, Locale::En));
        stack.render(area, &mut buf);
        (buf, area)
    }

    fn rows(buf: &Buffer, area: Rect) -> Vec<String> {
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn mode_picker_is_usable_and_opaque_at_blocker_sizes() {
        for (w, h) in BLOCKER_SIZES {
            let (buf, area) = render_at(w, h);
            let text = rows(&buf, area).join("\n");

            // Action labels are present (footer never drops an action).
            assert!(text.contains("move"), "{w}x{h}: missing 'move' hint");
            assert!(text.contains("select"), "{w}x{h}: missing 'select' hint");
            assert!(text.contains("cancel"), "{w}x{h}: missing 'cancel' hint");

            // The cursor row carries the charter selection pointer.
            assert!(
                text.contains(codewhale_tui_kit::glyphs::SELECTION),
                "{w}x{h}: missing charter selection pointer"
            );

            // Composited frame is fully opaque: no sentinel survives and every
            // cell carries the modal/backdrop ink background.
            assert!(
                !text.contains('X'),
                "{w}x{h}: background bleed-through into modal surface"
            );
            let center = &buf[(w / 2, h / 2)];
            assert_eq!(
                center.bg,
                palette::WHALE_BG,
                "{w}x{h}: modal interior must be opaque"
            );

            // No row exceeds the frame width (no horizontal overflow).
            for (y, row) in rows(&buf, area).iter().enumerate() {
                assert!(
                    UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                    "{w}x{h}: row {y} overflows width: {row:?}"
                );
            }
        }
    }

    #[test]
    fn operate_is_advertised_as_a_visible_mode() {
        let (buf, area) = render_at(80, 24);
        let text = rows(&buf, area).join("\n");
        assert!(text.contains("Operate"), "{text}");
    }

    #[test]
    fn number_keys_select_modes() {
        // Visible roster: 1 Act, 2 Plan, 3 Operate.
        let mut view = ModePickerView::new(AppMode::Agent, Locale::En);
        let action = view.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE));
        assert!(matches!(
            action,
            ViewAction::EmitAndClose(ViewEvent::ModeSelected {
                mode: AppMode::Operate
            })
        ));

        // Legacy YOLO shorthand (4) is not offered by the picker.
        let mut view = ModePickerView::new(AppMode::Agent, Locale::En);
        let action = view.handle_key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE));
        assert!(matches!(action, ViewAction::None));

        // Old Operate number (5) is gone — no numeric gaps.
        let mut view = ModePickerView::new(AppMode::Agent, Locale::En);
        let action = view.handle_key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::NONE));
        assert!(matches!(action, ViewAction::None));
    }

    #[test]
    fn mouse_click_renders_and_selects_mode_row() {
        let mut view = ModePickerView::new(AppMode::Agent, Locale::En);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("test terminal");
        terminal
            .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
            .expect("render mode picker");
        let rect = view.row_hitboxes.borrow()[1];
        let action = view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            action,
            ViewAction::EmitAndClose(ViewEvent::ModeSelected {
                mode: AppMode::Plan
            })
        ));
    }
}

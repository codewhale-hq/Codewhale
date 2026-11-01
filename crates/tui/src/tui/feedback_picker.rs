//! `/feedback` picker for GitHub feedback destinations.

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
    ActionHint, CommandPaletteAction, ModalKind, ModalView, ViewAction, ViewEvent,
    centered_modal_area, render_modal_footer, render_modal_surface,
};
use codewhale_palette as palette;
use codewhale_tui_kit::menu_style;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy)]
struct FeedbackOption {
    number: char,
    label: &'static str,
    description: &'static str,
    command: &'static str,
}

const OPTIONS: &[FeedbackOption] = &[
    FeedbackOption {
        number: '1',
        label: "Bug report",
        description: "Report a problem or regression",
        command: "/feedback bug",
    },
    FeedbackOption {
        number: '2',
        label: "Feature request",
        description: "Suggest an idea or improvement",
        command: "/feedback feature",
    },
    FeedbackOption {
        number: '3',
        label: "Security vulnerability",
        description: "Review the security policy before reporting",
        command: "/feedback security",
    },
];

pub struct FeedbackPickerView {
    selected: usize,
    /// Screen row of each option row, recorded as it is painted. Keyboard and
    /// mouse must reach the same rows; without this the view inherits the
    /// no-op `handle_mouse` and silently swallows every click.
    row_hitboxes: RefCell<Vec<Rect>>,
}

impl FeedbackPickerView {
    #[must_use]
    pub fn new() -> Self {
        Self {
            selected: 0,
            row_hitboxes: RefCell::new(Vec::new()),
        }
    }

    fn move_up(&mut self) {
        self.selected = codewhale_tui_kit::list_nav::wrap_index(self.selected, OPTIONS.len(), -1);
    }

    fn move_down(&mut self) {
        self.selected = codewhale_tui_kit::list_nav::wrap_index(self.selected, OPTIONS.len(), 1);
    }

    fn select_number(&mut self, number: char) -> Option<ViewAction> {
        let idx = OPTIONS.iter().position(|option| option.number == number)?;
        self.selected = idx;
        Some(self.selected_action())
    }

    fn selected_action(&self) -> ViewAction {
        let command = OPTIONS
            .get(self.selected)
            .map(|option| option.command)
            .unwrap_or(OPTIONS[0].command)
            .to_string();
        ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
            action: CommandPaletteAction::ExecuteCommand { command },
        })
    }
}

impl Default for FeedbackPickerView {
    fn default() -> Self {
        Self::new()
    }
}

impl ModalView for FeedbackPickerView {
    fn kind(&self) -> ModalKind {
        ModalKind::FeedbackPicker
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Enter => self.selected_action(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_up();
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_down();
                ViewAction::None
            }
            KeyCode::Char(number)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && OPTIONS.iter().any(|option| option.number == number) =>
            {
                self.select_number(number).unwrap_or(ViewAction::None)
            }
            _ => ViewAction::None,
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let popup_area = centered_modal_area(area, 78, (OPTIONS.len() as u16) + 7, 44, 8);

        render_modal_surface(area, popup_area, buf);

        let block = Block::default()
            .title(Line::from(Span::styled(
                " Feedback ",
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
                ActionHint::new("Enter", "open"),
                ActionHint::new("Esc", "cancel"),
            ],
        );

        let mut lines = Vec::with_capacity(OPTIONS.len() + 2);
        lines.push(Line::from(""));
        // `lines` opens with a blank row, so option `idx` paints one row lower.
        let mut hitboxes = self.row_hitboxes.borrow_mut();
        hitboxes.clear();

        for (idx, option) in OPTIONS.iter().enumerate() {
            let row = content
                .y
                .saturating_add(u16::try_from(idx).unwrap_or(u16::MAX))
                + 1;
            if row < content.bottom() {
                hitboxes.push(Rect::new(content.x, row, content.width, 1));
            }
            let is_selected = idx == self.selected;
            let row_style = if is_selected {
                menu_style::selected_row_style()
            } else {
                Style::default().fg(palette::TEXT_PRIMARY)
            };
            let desc_style = if is_selected {
                menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
            } else {
                Style::default().fg(palette::TEXT_MUTED)
            };
            let pointer = codewhale_tui_kit::glyphs::selection_marker(is_selected);
            let lead = format!("{pointer} {}. ", option.number);
            let description_width = usize::from(content.width).saturating_sub(
                UnicodeWidthStr::width(lead.as_str()) + UnicodeWidthStr::width(option.label) + 4,
            );
            let description = codewhale_ratatui::text::truncate_words(
                option.description,
                description_width,
                crate::tui::color_compat::ascii_safe_enabled(),
            );

            lines.push(Line::from(vec![
                Span::styled(lead, row_style),
                Span::styled(option.label, row_style),
                Span::raw("    "),
                Span::styled(description, desc_style),
            ]));
        }

        drop(hitboxes);
        Paragraph::new(lines).render(content, buf);
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.move_up();
                ViewAction::None
            }
            MouseEventKind::ScrollDown => {
                self.move_down();
                ViewAction::None
            }
            MouseEventKind::Moved => {
                let hovered = self
                    .row_hitboxes
                    .borrow()
                    .iter()
                    .position(|rect| rect.y == mouse.row);
                if let Some(index) = hovered {
                    self.selected = index;
                }
                ViewAction::None
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let clicked = self
                    .row_hitboxes
                    .borrow()
                    .iter()
                    .position(|rect| rect.y == mouse.row);
                match clicked {
                    // Click to focus, click again to open — the same two-step
                    // the session picker uses, so a stray click never opens a
                    // browser tab the user did not choose.
                    Some(index) if index == self.selected => self.selected_action(),
                    Some(index) => {
                        self.selected = index;
                        ViewAction::None
                    }
                    None => ViewAction::None,
                }
            }
            _ => ViewAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(kind: MouseEventKind, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: 2,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Selectable rows need keyboard and mouse parity. This view overrode no
    /// `handle_mouse`, so it inherited the no-op default and the view stack
    /// swallowed every click on it — the only list in the picker family that
    /// could not be used with a pointer.
    #[test]
    fn feedback_rows_are_clickable_and_land_on_the_row_under_the_pointer() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut view = FeedbackPickerView::new();
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("terminal");
        terminal
            .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
            .expect("draw");

        let rows: Vec<u16> = view.row_hitboxes.borrow().iter().map(|r| r.y).collect();
        assert_eq!(rows.len(), OPTIONS.len(), "every option needs a hitbox");
        assert!(
            rows.windows(2).all(|w| w[1] == w[0] + 1),
            "option hitboxes must be consecutive rows, got {rows:?}"
        );

        // A click on the last option must select that option — an off-by-one
        // against the leading blank row would put it on its neighbour or drop it.
        let last = *rows.last().expect("a row");
        assert!(matches!(
            view.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), last)),
            ViewAction::None
        ));
        assert_eq!(view.selected, OPTIONS.len() - 1);

        // Clicking the already-selected row opens it; a single stray click
        // never does, so a misplaced pointer cannot open a browser tab.
        assert_eq!(
            emitted_command(
                view.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), last))
            ),
            OPTIONS[OPTIONS.len() - 1].command
        );

        assert!(matches!(
            view.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0)),
            ViewAction::None
        ));
    }

    fn emitted_command(action: ViewAction) -> String {
        match action {
            ViewAction::EmitAndClose(ViewEvent::CommandPaletteSelected {
                action: CommandPaletteAction::ExecuteCommand { command },
            }) => command,
            other => panic!("expected feedback command emit, got {other:?}"),
        }
    }

    #[test]
    fn enter_emits_selected_feedback_command() {
        let mut view = FeedbackPickerView::new();
        let command =
            emitted_command(view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(command, "/feedback bug");
    }

    #[test]
    fn arrow_down_selects_feature_command() {
        let mut view = FeedbackPickerView::new();
        view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let command =
            emitted_command(view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(command, "/feedback feature");
    }

    #[test]
    fn digit_selects_security_command() {
        let mut view = FeedbackPickerView::new();
        let command =
            emitted_command(view.handle_key(KeyEvent::new(KeyCode::Char('3'), KeyModifiers::NONE)));
        assert_eq!(command, "/feedback security");
    }

    #[test]
    fn esc_closes_picker() {
        let mut view = FeedbackPickerView::new();
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ViewAction::Close
        ));
    }

    /// The four terminal sizes the v0.8.66 modal blocker (#3732) requires
    /// every overlay to remain readable and fully operable at.
    const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

    #[test]
    fn feedback_is_usable_and_opaque_at_blocker_sizes() {
        use crate::tui::views::ViewStack;
        use ratatui::{buffer::Buffer, layout::Rect};
        use unicode_width::UnicodeWidthStr;

        for (w, h) in BLOCKER_SIZES {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            for y in 0..h {
                for x in 0..w {
                    buf[(x, y)].set_symbol("X");
                }
            }
            let mut stack = ViewStack::new();
            stack.push(FeedbackPickerView::new());
            stack.render(area, &mut buf);

            let rows: Vec<String> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect();
            let text = rows.join("\n");

            for label in ["move", "open", "cancel"] {
                assert!(text.contains(label), "{w}x{h}: missing footer '{label}'");
            }
            assert!(
                text.contains(codewhale_tui_kit::glyphs::SELECTION),
                "{w}x{h}: missing charter selection pointer"
            );
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
}

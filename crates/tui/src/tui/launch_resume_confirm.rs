//! Confirmation popup for resuming a session from the launch card.
//!
//! Resuming replaces the whole session context, and a single click used to do
//! it instantly — founder live-test: "you just click it and boom you're there
//! ... you don't realize it's happening". The first attempt at a fix put an
//! arming line over the composer dock, which read as one more piece of chrome
//! rather than as a question ("that's even more confusing tbh"). This is the
//! question, as a popup, with the session it is about named in it.

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Widget, Wrap};

use crate::tui::views::{
    ModalKind, ModalView, ViewAction, ViewEvent, centered_modal_area, render_modal_surface,
};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;

pub struct LaunchResumeConfirmView {
    session_id: String,
    title: String,
    detail: String,
    locale: Locale,
    /// The painted popup, so a click outside it can dismiss.
    last_area: std::cell::Cell<Option<Rect>>,
    buttons: std::cell::Cell<[Rect; 2]>,
    selected: usize,
    hovered: Option<usize>,
}

impl LaunchResumeConfirmView {
    #[must_use]
    pub fn new(session_id: String, title: String, detail: String, locale: Locale) -> Self {
        Self {
            session_id,
            title,
            detail,
            locale,
            last_area: std::cell::Cell::new(None),
            buttons: std::cell::Cell::new([Rect::default(); 2]),
            selected: 0,
            hovered: None,
        }
    }

    fn confirm(&self) -> ViewAction {
        ViewAction::EmitAndClose(ViewEvent::LaunchResumeConfirmed {
            session_id: self.session_id.clone(),
        })
    }
}

impl ModalView for LaunchResumeConfirmView {
    fn kind(&self) -> ModalKind {
        ModalKind::LaunchResumeConfirm
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        // Resuming replaces the whole session, so only a plain press answers:
        // a release/repeat, or a Ctrl/Alt/Super chord that happens to end in
        // Enter or `y`, is not a confirmation (U01-05). Shift stays allowed
        // for `Y`.
        if key.kind != KeyEventKind::Press {
            return ViewAction::None;
        }
        let chord = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match key.code {
            KeyCode::Enter if !chord => {
                if self.selected == 0 {
                    self.confirm()
                } else {
                    ViewAction::Close
                }
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.selected = 1 - self.selected;
                ViewAction::None
            }
            // `y` is the habit every terminal confirmation teaches; Esc and
            // `n` both back out. Nothing else acts, so a stray keystroke
            // cannot resume a session by accident.
            KeyCode::Char('y') | KeyCode::Char('Y') if !chord => self.confirm(),
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => ViewAction::Close,
            _ => ViewAction::None,
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        let target = self
            .buttons
            .get()
            .iter()
            .position(|rect| crate::tui::mouse_ui::mouse_hits_rect(mouse, Some(*rect)));
        if matches!(mouse.kind, MouseEventKind::Moved) {
            self.hovered = target;
            return ViewAction::None;
        }
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return ViewAction::None;
        }
        if let Some(index) = target {
            return if index == 0 {
                self.confirm()
            } else {
                ViewAction::Close
            };
        }
        // A click outside the popup is a dismissal, never a confirmation:
        // the whole point is that resuming needs a deliberate act.
        match self.last_area.get() {
            Some(area) if crate::tui::mouse_ui::mouse_hits_rect(mouse, Some(area)) => {
                ViewAction::None
            }
            Some(_) => ViewAction::Close,
            None => ViewAction::None,
        }
    }

    fn occupied_region(&self, area: Rect) -> Rect {
        centered_modal_area(area, 64, 11, 32, 7)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let popup = self.occupied_region(area);
        self.last_area.set(Some(popup));
        render_modal_surface(area, popup, buf);

        let block = Block::default()
            .title(Line::from(Span::styled(
                tr(self.locale, MessageId::LaunchResumeConfirmTitle).to_string(),
                Style::default().fg(palette::TEXT_PRIMARY).bold(),
            )))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette::BORDER_COLOR))
            .style(Style::default().bg(palette::WHALE_BG))
            .padding(Padding::uniform(1));
        let inner = block.inner(popup);
        block.render(popup, buf);

        // Reserve the consequence and controls first. A long session title
        // is one clipped line, never a paragraph that can bury the warning.
        let body_height = inner.height.saturating_sub(1);
        let warning = Paragraph::new(tr(self.locale, MessageId::LaunchResumeConfirmBody))
            .style(Style::default().fg(palette::TEXT_SOFT))
            .wrap(Wrap { trim: true });
        let warning_height = warning
            .line_count(inner.width)
            .min(usize::from(body_height)) as u16;
        let spare = body_height.saturating_sub(warning_height);
        let mut y = inner.y;
        if spare > 0 {
            Paragraph::new(codewhale_tui_kit::ui_text::truncate_line_to_width(
                &self.title,
                usize::from(inner.width),
            ))
            .style(Style::default().fg(palette::TEXT_PRIMARY).bold())
            .render(Rect::new(inner.x, y, inner.width, 1), buf);
            y += 1;
        }
        if spare > 1 {
            Paragraph::new(codewhale_tui_kit::ui_text::truncate_line_to_width(
                &self.detail,
                usize::from(inner.width),
            ))
            .style(Style::default().fg(palette::TEXT_MUTED))
            .render(Rect::new(inner.x, y, inner.width, 1), buf);
            y += 1;
        }
        if spare > 2 {
            y += 1;
        }
        warning.render(Rect::new(inner.x, y, inner.width, warning_height), buf);

        // These are real controls, sharing the same measured rectangles for
        // paint and pointer input. Keep both reachable in compact terminals.
        let button_width = inner.width.saturating_sub(1) / 2;
        let y = inner.bottom().saturating_sub(1);
        let buttons = [
            Rect::new(inner.x, y, button_width, u16::from(inner.height > 0)),
            Rect::new(
                inner.x + button_width + 1,
                y,
                button_width,
                u16::from(inner.height > 0),
            ),
        ];
        self.buttons.set(buttons);
        for (index, id) in [
            MessageId::LaunchResumeConfirmResume,
            MessageId::LaunchResumeConfirmCancel,
        ]
        .into_iter()
        .enumerate()
        {
            let label = tr(self.locale, id);
            let key = if self.selected == index {
                "Enter"
            } else if index == 1 {
                "Esc"
            } else {
                ""
            };
            let text = if !key.is_empty()
                && unicode_width::UnicodeWidthStr::width(label.as_ref()) + key.len() + 3
                    <= usize::from(button_width)
            {
                format!("{label}  {key}")
            } else {
                label.into_owned()
            };
            let style = if self.selected == index {
                codewhale_tui_kit::menu_style::selected_row_style()
            } else if self.hovered == Some(index) {
                codewhale_tui_kit::menu_style::hovered_row_style().fg(palette::TEXT_PRIMARY)
            } else {
                Style::default()
                    .bg(palette::SURFACE_ELEVATED)
                    .fg(palette::TEXT_PRIMARY)
            };
            Paragraph::new(text)
                .centered()
                .style(style)
                .render(buttons[index], buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn view() -> LaunchResumeConfirmView {
        LaunchResumeConfirmView::new(
            "sess-1".to_string(),
            "refactor the parser".to_string(),
            "3h ago · 12 msgs".to_string(),
            Locale::En,
        )
    }

    #[test]
    fn enter_confirms_and_esc_walks_away() {
        let mut confirm = view();
        match confirm.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)) {
            ViewAction::EmitAndClose(ViewEvent::LaunchResumeConfirmed { session_id }) => {
                assert_eq!(session_id, "sess-1");
            }
            other => panic!("Enter must confirm, got {other:?}"),
        }

        let mut confirm = view();
        assert!(matches!(
            confirm.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ViewAction::Close
        ));

        // A stray keystroke resumes nothing: the whole point is that this
        // takes a deliberate act.
        let mut confirm = view();
        assert!(matches!(
            confirm.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            ViewAction::None
        ));

        // U01-05: a chord that ends in Enter or `y`, and a key release, are
        // not a confirmation.
        for key in [
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::SUPER),
            KeyEvent::new_with_kind(
                KeyCode::Enter,
                KeyModifiers::NONE,
                crossterm::event::KeyEventKind::Release,
            ),
        ] {
            let mut confirm = view();
            assert!(
                matches!(confirm.handle_key(key), ViewAction::None),
                "{key:?} must not resume"
            );
        }
    }

    #[test]
    fn the_popup_names_the_session_it_is_asking_about() {
        let confirm = view();
        let area = Rect::new(0, 0, 100, 30);
        let mut buf = Buffer::empty(area);
        confirm.render(area, &mut buf);
        let painted: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            painted.contains("refactor the parser"),
            "the session title is in the popup:\n{painted}"
        );
        assert!(
            painted.contains("12 msgs"),
            "so is enough detail to recognise it:\n{painted}"
        );
    }
}

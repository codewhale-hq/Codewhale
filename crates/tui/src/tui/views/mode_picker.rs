//! `/mode` picker: the conversation's mode first, then permissions.

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

use crate::tui::app::{App, AppModeUi};
use crate::tui::conversation_mode as modes;
use crate::tui::views::{
    ActionHint, ModalKind, ModalView, ViewAction, ViewEvent, centered_modal_area,
    render_modal_footer, render_modal_surface,
};
use codewhale_config::AppMode;
use codewhale_engine::conversation_modes::ConversationModeTable;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_runtime::config::Config;
use codewhale_tui_kit::menu_style;

const VISIBLE_MODES: [AppMode; 2] = [AppMode::Agent, AppMode::Plan];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConversationRow {
    id: String,
    name: String,
    description: String,
    unavailable: Option<String>,
    current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    Conversation(usize),
    Permission(AppMode),
}

pub struct ModePickerView {
    cursor: usize,
    locale: Locale,
    rows: Vec<ConversationRow>,
    locked: bool,
    other_command: String,
    row_hitboxes: RefCell<Vec<Rect>>,
}

impl ModePickerView {
    pub async fn for_app(app: &App, config: &Config) -> Self {
        let table = modes::current_table(config).await;
        Self::from_table(
            app.mode,
            app.ui_locale,
            &table,
            app.conversation_mode.as_deref(),
            app.conversation_mode_locked(),
        )
    }

    #[must_use]
    pub fn from_table(
        current: AppMode,
        locale: Locale,
        table: &ConversationModeTable,
        conversation: Option<&str>,
        locked: bool,
    ) -> Self {
        let current_id = codewhale_engine::conversation_modes::effective_id(conversation);
        let rows = table
            .modes
            .iter()
            .map(|mode| ConversationRow {
                id: mode.id.clone(),
                name: modes::display_name(locale, mode),
                description: modes::display_description(locale, mode),
                unavailable: (!mode.available).then(|| {
                    modes::display_reason(
                        locale,
                        mode.reason
                            .as_deref()
                            .unwrap_or("This mode is unavailable."),
                    )
                }),
                current: mode.id == current_id,
            })
            .collect();
        let other_command = modes::new_command(
            table
                .modes
                .iter()
                .find(|mode| mode.id != current_id)
                .map_or(codewhale_engine::conversation_modes::STANDARD, |mode| {
                    mode.id.as_str()
                }),
        );
        let mut view = Self {
            cursor: 0,
            locale,
            rows,
            locked,
            other_command,
            row_hitboxes: RefCell::new(Vec::new()),
        };
        view.cursor = view
            .entries()
            .iter()
            .position(|entry| *entry == Entry::Permission(current))
            .unwrap_or(0);
        view
    }

    fn entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        if !self.locked {
            entries.extend(
                self.rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row.unavailable.is_none())
                    .map(|(index, _)| Entry::Conversation(index)),
            );
        }
        entries.extend(VISIBLE_MODES.iter().copied().map(Entry::Permission));
        entries
    }

    fn selected(&self) -> Option<Entry> {
        self.entries().get(self.cursor).copied()
    }

    fn event_for(&self, entry: Entry) -> ViewEvent {
        match entry {
            Entry::Conversation(index) => ViewEvent::ConversationModeSelected {
                id: self.rows[index].id.clone(),
            },
            Entry::Permission(mode) => ViewEvent::ModeSelected { mode },
        }
    }

    fn apply_motion(&mut self, motion: codewhale_tui_kit::list_nav::Motion) -> bool {
        let len = self.entries().len();
        let Some(next) = codewhale_tui_kit::list_nav::apply(self.cursor, len, len, motion) else {
            return false;
        };
        self.cursor = next;
        true
    }

    fn select_by_number(&mut self, number: char) -> Option<ViewAction> {
        let entries = self.entries();
        let idx = entries.iter().position(
            |entry| matches!(entry, Entry::Permission(mode) if mode.number() == number),
        )?;
        self.cursor = idx;
        Some(ViewAction::EmitAndClose(self.event_for(entries[idx])))
    }

    fn line_count(&self) -> u16 {
        let rows = u16::try_from(self.rows.len()).unwrap_or(u16::MAX);
        let conversation = if self.rows.is_empty() {
            0
        } else {
            2 + rows + u16::from(self.locked)
        };
        conversation + 1 + u16::try_from(VISIBLE_MODES.len()).unwrap_or(2)
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
        if let Some(motion) = codewhale_tui_kit::list_nav::motion(&key)
            && self.apply_motion(motion)
        {
            return ViewAction::None;
        }
        match key.code {
            KeyCode::Esc => ViewAction::Close,
            KeyCode::Enter => match self.selected() {
                Some(entry) => ViewAction::EmitAndClose(self.event_for(entry)),
                None => ViewAction::None,
            },
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
        let popup_height = self.line_count() + 7;
        let popup_area = centered_modal_area(area, 68, popup_height, 44, 8);

        render_modal_surface(area, popup_area, buf);

        let title = format!(
            " {} ",
            tr(self.locale, MessageId::ConversationModePickerTitle)
        );
        let block = Block::default()
            .title(Line::from(Span::styled(
                title,
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

        let entries = self.entries();
        let heading_style = Style::default()
            .fg(palette::TEXT_MUTED)
            .add_modifier(Modifier::BOLD);
        let muted = Style::default().fg(palette::TEXT_MUTED);
        let width = usize::from(content.width);
        let mut lines: Vec<Line<'static>> = Vec::new();

        let push_row = |lines: &mut Vec<Line<'static>>,
                        entry: Option<Entry>,
                        prefix: String,
                        text: String,
                        disabled: bool| {
            let cursor_here = entry.is_some_and(|entry| entries.get(self.cursor) == Some(&entry));
            let row_style = if cursor_here {
                menu_style::selected_row_style()
            } else if disabled {
                muted
            } else {
                Style::default().fg(palette::TEXT_PRIMARY)
            };
            let hint_style = if cursor_here {
                menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT)
            } else {
                muted
            };
            let room = width.saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
            let text = codewhale_tui_kit::ui_text::semantic_truncate(&text, room);
            if entry.is_some() {
                let y = content
                    .y
                    .saturating_add(u16::try_from(lines.len()).unwrap_or(u16::MAX));
                self.row_hitboxes
                    .borrow_mut()
                    .push(Rect::new(content.x, y, content.width, 1));
            }
            lines.push(Line::from(vec![
                Span::styled(prefix, row_style),
                Span::styled(text, hint_style),
            ]));
        };

        if !self.rows.is_empty() {
            lines.push(Line::from(Span::styled(
                tr(self.locale, MessageId::ConversationModeGroupConversation).into_owned(),
                heading_style,
            )));
            if self.locked {
                lines.push(Line::from(Span::styled(
                    codewhale_tui_kit::ui_text::semantic_truncate(
                        tr(self.locale, MessageId::ConversationModeLockedNote)
                            .replace("{command}", &format!("`{}`", self.other_command))
                            .as_str(),
                        width,
                    ),
                    muted,
                )));
            }
            for (index, row) in self.rows.iter().enumerate() {
                let selectable = !self.locked && row.unavailable.is_none();
                let entry = selectable.then_some(Entry::Conversation(index));
                let cursor_here =
                    entry.is_some_and(|entry| entries.get(self.cursor) == Some(&entry));
                let pointer = if cursor_here {
                    codewhale_tui_kit::glyphs::selection_marker(true).to_string()
                } else {
                    " ".to_string()
                };
                let check = if row.current { "✓" } else { " " };
                let pad =
                    " ".repeat(16usize.saturating_sub(UnicodeWidthStr::width(row.name.as_str())));
                let prefix = format!("{pointer} {check} {}{pad}", row.name);
                let text = row
                    .unavailable
                    .clone()
                    .unwrap_or_else(|| row.description.clone());
                push_row(
                    &mut lines,
                    entry,
                    prefix,
                    text,
                    row.unavailable.is_some() || self.locked,
                );
            }
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(
            tr(self.locale, MessageId::ConversationModeGroupPermissions).into_owned(),
            heading_style,
        )));
        for mode in VISIBLE_MODES {
            let entry = Entry::Permission(mode);
            let cursor_here = entries.get(self.cursor) == Some(&entry);
            let pointer = codewhale_tui_kit::glyphs::selection_marker(cursor_here);
            let name = mode.display_name_localized(self.locale);
            let hint = mode.picker_hint_localized(self.locale);
            let pad = " ".repeat(8usize.saturating_sub(UnicodeWidthStr::width(&*name)));
            let prefix = format!("{pointer} {}. {name}{pad}", mode.number());
            push_row(&mut lines, Some(entry), prefix, hint.into_owned(), false);
        }

        Paragraph::new(lines).render(content, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_engine::conversation_modes::{
        self, CREATOR_NOT_READY_REASON, CreatorAvailabilityGuard,
    };
    use crossterm::event::KeyModifiers;
    use ratatui::{Terminal, backend::TestBackend};

    fn picker(current: AppMode, conversation: Option<&str>, locked: bool) -> ModePickerView {
        ModePickerView::from_table(
            current,
            Locale::En,
            &conversation_modes::table(Some(CREATOR_NOT_READY_REASON.to_string())),
            conversation,
            locked,
        )
    }

    #[test]
    fn opens_on_current_mode() {
        let view = picker(AppMode::Plan, None, false);
        assert_eq!(view.selected(), Some(Entry::Permission(AppMode::Plan)));
    }

    #[test]
    fn enter_emits_selected_mode() {
        let mut view = picker(AppMode::Agent, None, false);
        view.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let action = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        match action {
            ViewAction::EmitAndClose(ViewEvent::ModeSelected { mode }) => {
                assert_eq!(mode, AppMode::Plan);
            }
            other => panic!("expected ModeSelected, got {other:?}"),
        }
    }

    const BLOCKER_SIZES: [(u16, u16); 4] = [(80, 24), (100, 30), (120, 32), (160, 40)];

    fn render_view(view: ModePickerView, width: u16, height: u16) -> (Buffer, Rect) {
        use crate::tui::views::ViewStack;
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        for y in 0..height {
            for x in 0..width {
                buf[(x, y)].set_symbol("X");
            }
        }
        let mut stack = ViewStack::new();
        stack.push(view);
        stack.render(area, &mut buf);
        (buf, area)
    }

    fn render_at(width: u16, height: u16) -> (Buffer, Rect) {
        render_view(picker(AppMode::Agent, None, false), width, height)
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

    fn frame_text(buf: &Buffer, area: Rect) -> String {
        rows(buf, area)
            .iter()
            .map(|row| row.trim_end().to_string())
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn mode_picker_is_usable_and_opaque_at_blocker_sizes() {
        for (w, h) in BLOCKER_SIZES {
            for view in [
                picker(AppMode::Agent, None, false),
                picker(AppMode::Plan, Some("creator"), true),
            ] {
                let (buf, area) = render_view(view, w, h);
                let text = rows(&buf, area).join("\n");

                assert!(text.contains("move"), "{w}x{h}: missing 'move' hint");
                assert!(text.contains("select"), "{w}x{h}: missing 'select' hint");
                assert!(text.contains("cancel"), "{w}x{h}: missing 'cancel' hint");
                assert!(
                    text.contains("Conversation mode") && text.contains("Permissions"),
                    "{w}x{h}: both groups are labelled"
                );
                assert!(
                    text.contains(codewhale_tui_kit::glyphs::SELECTION),
                    "{w}x{h}: missing charter selection pointer"
                );
                assert!(
                    !text.contains('X'),
                    "{w}x{h}: background bleed-through into modal surface"
                );
                let lines = rows(&buf, area);
                let heading_row = lines
                    .iter()
                    .position(|row| row.contains("Conversation mode"))
                    .expect("heading row");
                let heading_byte = lines[heading_row]
                    .find("Conversation mode")
                    .expect("heading column");
                let heading_x = lines[heading_row][..heading_byte].chars().count();
                let padding = &buf[(
                    u16::try_from(heading_x + "Conversation mode".len() + 3).unwrap(),
                    u16::try_from(heading_row).unwrap(),
                )];
                assert_eq!(
                    padding.bg,
                    palette::WHALE_BG,
                    "{w}x{h}: modal interior must be opaque"
                );
                for (y, row) in rows(&buf, area).iter().enumerate() {
                    assert!(
                        UnicodeWidthStr::width(row.trim_end()) <= w as usize,
                        "{w}x{h}: row {y} overflows width: {row:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn operate_is_not_advertised_as_a_visible_mode() {
        let (buf, area) = render_at(80, 24);
        let text = rows(&buf, area).join("\n");
        assert!(!text.contains("Operate"), "{text}");
    }

    #[test]
    fn number_keys_select_modes() {
        let mut view = picker(AppMode::Agent, None, false);
        let action = view.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        assert!(matches!(
            action,
            ViewAction::EmitAndClose(ViewEvent::ModeSelected {
                mode: AppMode::Plan
            })
        ));

        for key in ['3', '4', '5'] {
            let mut view = picker(AppMode::Agent, None, false);
            let action = view.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
            assert!(matches!(action, ViewAction::None), "{key}");
        }
    }

    #[test]
    fn mouse_click_renders_and_selects_mode_row() {
        let mut view = picker(AppMode::Agent, None, false);
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("test terminal");
        terminal
            .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
            .expect("render mode picker");
        let rect = view.row_hitboxes.borrow()[2];
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

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn mode_picker_lists_conversation_modes_first_and_locks_after_first_message() {
        let _available = CreatorAvailabilityGuard::set(None);
        let table = conversation_modes::current_table(Default::default()).await;
        for (w, h) in [(80, 24), (120, 32)] {
            let open = ModePickerView::from_table(AppMode::Agent, Locale::En, &table, None, false);
            let (buf, area) = render_view(open, w, h);
            let text = frame_text(&buf, area);
            println!("--- /mode picker, blank conversation, creator available, {w}x{h}\n{text}");
            let order = |needle: &str| {
                text.find(needle)
                    .unwrap_or_else(|| panic!("{needle}: {text}"))
            };
            assert!(order("Conversation mode") < order("Standard mode"));
            assert!(order("Standard mode") < order("Creator mode"));
            assert!(order("Creator mode") < order("Permissions"));
            assert!(order("Permissions") < order("1. Work"));
            assert!(!text.contains("Fixed after the first message"));

            let mut open =
                ModePickerView::from_table(AppMode::Agent, Locale::En, &table, None, false);
            open.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
            open.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
            assert!(matches!(
                open.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                ViewAction::EmitAndClose(ViewEvent::ConversationModeSelected { ref id }) if id == "standard"
            ));

            let locked = ModePickerView::from_table(
                AppMode::Agent,
                Locale::En,
                &table,
                Some("creator"),
                true,
            );
            let (buf, area) = render_view(locked, w, h);
            let text = frame_text(&buf, area);
            println!(
                "--- /mode picker, after the first message, creator conversation, {w}x{h}\n{text}"
            );
            assert!(
                text.contains("Fixed after the first message. Start another with /new creator.")
            );
            assert!(text.contains("Creator mode"), "{text}");
        }
        let mut locked =
            ModePickerView::from_table(AppMode::Agent, Locale::En, &table, Some("creator"), true);
        assert_eq!(
            locked.entries().len(),
            2,
            "only the permission rows stay selectable"
        );
        for key in [KeyCode::Up, KeyCode::Up, KeyCode::Home] {
            locked.handle_key(KeyEvent::new(key, KeyModifiers::NONE));
            assert!(matches!(locked.selected(), Some(Entry::Permission(_))));
        }
        for number in ['1', '2'] {
            let mut locked =
                ModePickerView::from_table(AppMode::Agent, Locale::En, &table, None, true);
            assert!(matches!(
                locked.handle_key(KeyEvent::new(KeyCode::Char(number), KeyModifiers::NONE)),
                ViewAction::EmitAndClose(ViewEvent::ModeSelected { .. })
            ));
        }
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn creator_row_is_listed_disabled_with_its_reason_while_unavailable() {
        let runtime_missing =
            "Creator mode needs a plugin runtime (Node.js or Bun), and none was found.";
        for reason in [CREATOR_NOT_READY_REASON, runtime_missing] {
            let _guard = CreatorAvailabilityGuard::set(Some(reason));
            let table = conversation_modes::current_table(Default::default()).await;
            let view = ModePickerView::from_table(AppMode::Agent, Locale::En, &table, None, false);
            let (buf, area) = render_view(view, 100, 30);
            let text = frame_text(&buf, area);
            println!("--- /mode picker, creator unavailable ({reason})\n{text}");
            assert!(
                text.contains("Creator mode"),
                "listed, never hidden: {text}"
            );
            let shown = if reason == CREATOR_NOT_READY_REASON {
                "Not ready yet in this version."
            } else {
                "Creator mode needs a plugin runtime"
            };
            assert!(text.contains(shown), "{text}");
            assert!(!text.contains(CREATOR_NOT_READY_REASON));
            let mut view =
                ModePickerView::from_table(AppMode::Agent, Locale::En, &table, None, false);
            assert_eq!(view.entries().len(), 3);
            for key in [
                KeyCode::Up,
                KeyCode::Up,
                KeyCode::Up,
                KeyCode::Home,
                KeyCode::End,
            ] {
                let action = view.handle_key(KeyEvent::new(key, KeyModifiers::NONE));
                assert!(matches!(action, ViewAction::None));
            }
            let mut view =
                ModePickerView::from_table(AppMode::Agent, Locale::En, &table, None, false);
            view.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
            let action = view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert!(
                !matches!(action, ViewAction::EmitAndClose(ViewEvent::ConversationModeSelected { ref id }) if id == "creator"),
                "an unavailable mode is never selectable"
            );
        }
        let _guard = CreatorAvailabilityGuard::set(None);
        let table = conversation_modes::current_table(Default::default()).await;
        let mut view = ModePickerView::from_table(AppMode::Agent, Locale::En, &table, None, false);
        view.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(matches!(
            view.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ViewAction::EmitAndClose(ViewEvent::ConversationModeSelected { ref id }) if id == "creator"
        ));
    }
}

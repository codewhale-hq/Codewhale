//! Modal for request_user_input tool prompts.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Widget, Wrap};

use crate::tools::user_input::{
    UserInputAnswer, UserInputQuestion, UserInputRequest, UserInputResponse,
};
use crate::tui::menu_style;
use crate::tui::views::{ModalKind, ModalView, ViewAction, ViewEvent, render_modal_surface};
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;

fn modal_block(title: &str) -> Block<'static> {
    Block::default()
        .title(Line::from(vec![Span::styled(
            title.to_string(),
            Style::default().fg(palette::WHALE_HUMAN).bold(),
        )]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette::BORDER_COLOR))
        .style(Style::default().bg(palette::WHALE_BG))
        .padding(Padding::uniform(1))
}

fn render_modal_chrome(area: Rect, popup_area: Rect, buf: &mut Buffer) {
    render_modal_surface(area, popup_area, buf);
}

fn hint_label(locale: Locale, id: MessageId) -> Span<'static> {
    Span::styled(
        format!(" {}", tr(locale, id)),
        Style::default().fg(palette::TEXT_MUTED),
    )
}

fn push_option_lines(
    lines: &mut Vec<Line<'static>>,
    selected: bool,
    number: usize,
    label: String,
    description: String,
    ticked: bool,
) {
    let row_style = if selected {
        menu_style::selected_row_style()
    } else {
        Style::default().fg(palette::TEXT_PRIMARY)
    };
    let detail_style = if selected {
        row_style
    } else {
        Style::default().fg(palette::TEXT_MUTED)
    };
    let prefix = crate::tui::glyphs::selection_marker(selected);
    // Multi-select rows get a check-mark gutter when toggled into the pending
    // set, mirroring the affordance used in other multi-option pickers.
    let mark = if ticked { "✔ " } else { "  " };

    lines.push(Line::from(Span::styled(
        format!("{prefix}{mark}{number}) {label}"),
        row_style,
    )));
    lines.push(Line::from(Span::styled(
        format!("      {description}"),
        detail_style,
    )));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Selecting,
    OtherInput,
}

#[derive(Debug, Clone)]
pub struct UserInputView {
    tool_id: String,
    request: UserInputRequest,
    locale: Locale,
    question_index: usize,
    selected: usize,
    mode: InputMode,
    other_input: String,
    /// Answers committed for previous questions. Going back pops the last
    /// batch so an accidental Enter is reversible.
    answered: Vec<Vec<UserInputAnswer>>,
    /// Indices toggled into the pending multi-select set for the current
    /// question. Only used when `question.multi_select` is true.
    multi_pending: Vec<usize>,
    /// Wheel browsing uses wrapped rows from the last paint. Option navigation
    /// or typing returns to following focus so the next edit stays visible.
    manual_scroll: bool,
    scroll_offset: Cell<u16>,
    max_scroll: Cell<u16>,
}

impl UserInputView {
    pub fn new(tool_id: impl Into<String>, request: UserInputRequest, locale: Locale) -> Self {
        Self {
            tool_id: tool_id.into(),
            request,
            locale,
            question_index: 0,
            selected: 0,
            mode: InputMode::Selecting,
            other_input: String::new(),
            answered: Vec::new(),
            multi_pending: Vec::new(),
            manual_scroll: false,
            scroll_offset: Cell::new(0),
            max_scroll: Cell::new(0),
        }
    }

    fn current_question(&self) -> &UserInputQuestion {
        &self.request.questions[self.question_index]
    }

    /// Whether the "Other" free-text row is offered for the current question.
    /// Free text is ALWAYS available so the user can answer with their own
    /// words even when the model did not offer it. `allow_free_text` remains
    /// part of the wire request (backward-compatible) but no longer gates the
    /// row: a custom response must always be reachable alongside the options.
    fn offers_other(&self) -> bool {
        true
    }

    fn option_count(&self) -> usize {
        // Options + conditional "Other" row + conditional "Confirm" row.
        let mut count = self.current_question().options.len();
        count += usize::from(self.offers_other());
        count += usize::from(self.is_multi_select());
        count
    }

    fn is_other_selected(&self) -> bool {
        // "Other" sits immediately before the Confirm row when both exist, and
        // is last otherwise.
        let other_last = !self.is_multi_select();
        if other_last {
            self.offers_other() && self.selected + 1 == self.option_count()
        } else {
            self.offers_other() && self.selected + 2 == self.option_count()
        }
    }

    /// True when the multi-select "Confirm selection" row is highlighted.
    fn is_confirm_selected(&self) -> bool {
        self.confirm_index() == Some(self.selected)
    }

    fn confirm_index(&self) -> Option<usize> {
        self.is_multi_select()
            .then(|| self.option_count().saturating_sub(1))
    }

    fn is_multi_select(&self) -> bool {
        self.current_question().multi_select
    }

    /// Number of content lines the render path emits for the current state.
    /// Drives the content-sized popup height so the dialog hugs what it
    /// shows instead of claiming a fixed share of the screen.
    fn content_line_count(&self) -> usize {
        let question = self.current_question();
        // "Action required" banner, header line, blank, question, blank.
        let mut count = 5;
        count += question.options.len() * 2;
        if self.offers_other() {
            count += 2;
        }
        if self.is_multi_select() {
            count += 2;
        }
        if self.mode == InputMode::OtherInput {
            count += 2;
        }
        // Trailing blank line + controls hint.
        count += 2;
        count
    }

    fn toggle_pending(&mut self, index: usize) {
        if let Some(pos) = self.multi_pending.iter().position(|i| *i == index) {
            self.multi_pending.remove(pos);
        } else {
            self.multi_pending.push(index);
        }
    }

    /// Build the answer(s) for the current question from a single selected
    /// option index (single-select and the confirm step of multi-select).
    fn answers_for_selection(&self, index: usize) -> Vec<UserInputAnswer> {
        let question = self.current_question();
        let option = &question.options[index];
        vec![UserInputAnswer {
            id: question.id.clone(),
            label: option.label.clone(),
            value: option.label.clone(),
        }]
    }

    fn committed_answers(&self) -> Vec<UserInputAnswer> {
        self.answered.iter().flatten().cloned().collect()
    }

    fn advance_question(&mut self, new_answers: Vec<UserInputAnswer>) -> ViewAction {
        self.answered.push(new_answers);
        if self.question_index + 1 >= self.request.questions.len() {
            let response = UserInputResponse {
                answers: self.committed_answers(),
            };
            return ViewAction::EmitAndClose(ViewEvent::UserInputSubmitted {
                tool_id: self.tool_id.clone(),
                response,
            });
        }
        self.question_index += 1;
        self.selected = 0;
        self.mode = InputMode::Selecting;
        self.other_input.clear();
        self.multi_pending.clear();
        ViewAction::None
    }

    fn go_back(&mut self) -> ViewAction {
        if self.answered.is_empty() {
            return ViewAction::None;
        }
        self.answered.pop();
        self.question_index = self.question_index.saturating_sub(1);
        self.selected = 0;
        self.mode = InputMode::Selecting;
        self.other_input.clear();
        self.multi_pending.clear();
        ViewAction::None
    }

    /// Content-line range that must stay on screen: the highlighted option,
    /// or the typed custom-response row while editing it.
    fn focused_line_range(&self) -> (usize, usize) {
        if self.mode == InputMode::OtherInput {
            let start = self.content_line_count().saturating_sub(4);
            return (start, start.saturating_add(1));
        }
        let start = 5 + self.selected.saturating_mul(2);
        (start, start.saturating_add(1))
    }

    fn handle_selecting_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                ViewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.option_count().saturating_sub(1));
                ViewAction::None
            }
            KeyCode::Left | KeyCode::Char('h') => self.go_back(),
            KeyCode::Char(ch) if ch.is_ascii_digit() => {
                let Some(number) = ch.to_digit(10) else {
                    return ViewAction::None;
                };
                if number == 0 {
                    return ViewAction::None;
                }
                let index = usize::try_from(number - 1).unwrap_or(usize::MAX);
                if index >= self.option_count() {
                    return ViewAction::None;
                }
                self.selected = index;
                self.activate_or_confirm_selection()
            }
            KeyCode::Char(' ') if self.is_multi_select() => {
                // Space toggles the highlighted option in the pending set
                // without leaving the picker (standard multi-select affordance).
                // The Other row and the Confirm row are not options: toggling
                // them would corrupt the pending set.
                let is_confirm = self.confirm_index() == Some(self.selected);
                if !self.is_other_selected() && !is_confirm {
                    self.toggle_pending(self.selected);
                }
                ViewAction::None
            }
            KeyCode::Enter => self.activate_or_confirm_selection(),
            KeyCode::Esc => ViewAction::EmitAndClose(ViewEvent::UserInputCancelled {
                tool_id: self.tool_id.clone(),
            }),
            _ => ViewAction::None,
        }
    }

    /// Resolve a digit/Enter activation for the currently highlighted row.
    ///
    /// - "Other" row → enter free-text input mode.
    /// - multi-select option → add to the pending set (never remove — that is
    ///   Space's job) and move focus to the Confirm row, so the single-select
    ///   muscle memory of Enter-then-Enter submits the highlighted option
    ///   instead of toggling it back out and submitting an empty set.
    /// - multi-select Confirm row → submit the pending set.
    /// - single-select option → submit immediately (legacy behavior).
    fn activate_or_confirm_selection(&mut self) -> ViewAction {
        if self.is_other_selected() {
            self.mode = InputMode::OtherInput;
            self.other_input.clear();
            return ViewAction::None;
        }
        if self.is_multi_select() {
            if self.is_confirm_selected() {
                // Flush the pending set as this question's answers. An empty
                // set is allowed (skip-like) — the model is expected to offer a
                // sensible default, but we don't deadlock.
                let question = self.current_question();
                let answers: Vec<UserInputAnswer> = self
                    .multi_pending
                    .iter()
                    .filter_map(|i| question.options.get(*i))
                    .map(|opt| UserInputAnswer {
                        id: question.id.clone(),
                        label: opt.label.clone(),
                        value: opt.label.clone(),
                    })
                    .collect();
                return self.advance_question(answers);
            }
            // Enter on a real option selects it and moves to Confirm. It
            // never toggles out: double-Enter must submit the highlighted
            // option, matching single-select on the same view.
            if !self.multi_pending.contains(&self.selected) {
                self.multi_pending.push(self.selected);
            }
            if let Some(confirm) = self.confirm_index() {
                self.selected = confirm;
            }
            return ViewAction::None;
        }
        // Single-select: submit immediately.
        let answers = self.answers_for_selection(self.selected);
        self.advance_question(answers)
    }

    fn handle_other_input_key(&mut self, key: KeyEvent) -> ViewAction {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Selecting;
                self.other_input.clear();
                ViewAction::None
            }
            KeyCode::Enter => {
                let question = self.current_question();
                let answer = UserInputAnswer {
                    id: question.id.clone(),
                    label: "Other".to_string(),
                    value: self.other_input.trim().to_string(),
                };
                // In multi-select mode a free-text "Other" is still a single
                // answer appended to whatever options were toggled.
                let mut answers: Vec<UserInputAnswer> = self
                    .multi_pending
                    .iter()
                    .filter_map(|i| question.options.get(*i))
                    .map(|opt| UserInputAnswer {
                        id: question.id.clone(),
                        label: opt.label.clone(),
                        value: opt.label.clone(),
                    })
                    .collect();
                answers.push(answer);
                self.advance_question(answers)
            }
            KeyCode::Backspace => {
                self.other_input.pop();
                ViewAction::None
            }
            KeyCode::Char('h')
                if key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                self.other_input.pop();
                ViewAction::None
            }
            KeyCode::Char(ch) => {
                if !ch.is_control() {
                    self.other_input.push(ch);
                }
                ViewAction::None
            }
            _ => ViewAction::None,
        }
    }
}

impl ModalView for UserInputView {
    fn kind(&self) -> ModalKind {
        ModalKind::UserInput
    }

    fn input_context_id(&self) -> usize {
        self.question_index
    }

    fn user_input_request_id(&self) -> Option<&str> {
        Some(&self.tool_id)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn handle_key(&mut self, key: KeyEvent) -> ViewAction {
        self.manual_scroll = false;
        match self.mode {
            InputMode::Selecting => self.handle_selecting_key(key),
            InputMode::OtherInput => self.handle_other_input_key(key),
        }
    }

    /// A pasted custom answer lands in the free-text field (U08-m1); while
    /// choosing among options the paste is not consumed. Line breaks become
    /// spaces because Enter submits this single-line answer.
    fn handle_paste(&mut self, text: &str) -> bool {
        if self.mode != InputMode::OtherInput {
            return false;
        }
        self.other_input.extend(
            text.chars()
                .map(|ch| {
                    if matches!(ch, '\n' | '\r' | '\t') {
                        ' '
                    } else {
                        ch
                    }
                })
                .filter(|ch| !ch.is_control()),
        );
        true
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> ViewAction {
        let scroll = match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_offset.get().saturating_sub(3),
            MouseEventKind::ScrollDown => self.scroll_offset.get().saturating_add(3),
            _ => return ViewAction::None,
        };
        self.manual_scroll = true;
        self.scroll_offset.set(scroll.min(self.max_scroll.get()));
        ViewAction::None
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let question = self.current_question();
        let locale = self.locale;
        let total = self.request.questions.len();
        let header = format!(
            " {} ({}/{}) ",
            question.header,
            self.question_index + 1,
            total
        );

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(vec![Span::styled(
            tr(locale, MessageId::UserInputActionRequired),
            Style::default().fg(palette::WHALE_ACTION).bold(),
        )]));
        lines.push(Line::from(vec![
            Span::styled(
                question.header.clone(),
                Style::default().fg(palette::TEXT_PRIMARY).bold(),
            ),
            Span::styled(
                format!(
                    "  {}",
                    tr(locale, MessageId::UserInputQuestionProgress)
                        .replace("{current}", &(self.question_index + 1).to_string())
                        .replace("{total}", &total.to_string())
                ),
                Style::default().fg(palette::TEXT_MUTED),
            ),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            question.question.clone(),
            Style::default().fg(palette::TEXT_PRIMARY).bold(),
        )]));
        lines.push(Line::from(""));

        for (idx, option) in question.options.iter().enumerate() {
            let number = idx + 1;
            let ticked = self.is_multi_select() && self.multi_pending.contains(&idx);
            push_option_lines(
                &mut lines,
                self.selected == idx,
                number,
                option.label.clone(),
                option.description.clone(),
                ticked,
            );
        }

        // A custom response stays available alongside the suggested options.
        if self.offers_other() {
            let other_index = question.options.len();
            let other_number = other_index + 1;
            push_option_lines(
                &mut lines,
                self.selected == other_index,
                other_number,
                tr(locale, MessageId::UserInputOtherLabel).into_owned(),
                tr(locale, MessageId::UserInputOtherDescription).into_owned(),
                false,
            );
        }

        // Multi-select gets a dedicated "Confirm selection" row after the
        // options (and after "Other" when present). Selecting and pressing
        // Enter on it flushes the pending set as the question's answers.
        if let Some(confirm_index) = self.confirm_index() {
            let confirm_number = confirm_index + 1;
            push_option_lines(
                &mut lines,
                self.selected == confirm_index,
                confirm_number,
                tr(locale, MessageId::UserInputConfirmLabel).into_owned(),
                tr(locale, MessageId::UserInputConfirmDescription)
                    .replace("{count}", &self.multi_pending.len().to_string()),
                false,
            );
        }

        if self.mode == InputMode::OtherInput {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("> {}", tr(locale, MessageId::UserInputCustomPrompt)),
                    Style::default().fg(palette::TEXT_PRIMARY).bold(),
                ),
                Span::raw(" "),
                Span::styled(
                    if self.other_input.is_empty() {
                        tr(locale, MessageId::UserInputCustomPlaceholder).into_owned()
                    } else {
                        self.other_input.clone()
                    },
                    Style::default().fg(palette::WHALE_HUMAN),
                ),
            ]));
        }

        lines.push(Line::from(""));
        if self.mode == InputMode::OtherInput {
            lines.push(Line::from(vec![
                Span::styled("Enter", Style::default().fg(palette::WHALE_ACTION).bold()),
                hint_label(locale, MessageId::UserInputHintSubmit),
                Span::raw("  "),
                Span::styled("Esc", Style::default().fg(palette::WHALE_ACTION).bold()),
                hint_label(locale, MessageId::UserInputHintBack),
            ]));
        } else {
            let opt_count = self.option_count();
            let quick_pick_label = if opt_count <= 9 {
                format!("1-{opt_count}")
            } else {
                tr(locale, MessageId::UserInputHintDigit).into_owned()
            };
            if self.is_multi_select() {
                lines.push(Line::from(vec![
                    Span::styled(
                        quick_pick_label,
                        Style::default().fg(palette::WHALE_ACTION).bold(),
                    ),
                    hint_label(locale, MessageId::UserInputHintMove),
                    Span::raw("  "),
                    Span::styled("Space", Style::default().fg(palette::WHALE_ACTION).bold()),
                    hint_label(locale, MessageId::UserInputHintToggle),
                    Span::raw("  "),
                    Span::styled("Enter", Style::default().fg(palette::WHALE_ACTION).bold()),
                    hint_label(locale, MessageId::UserInputHintSelectConfirm),
                    Span::raw("  "),
                    Span::styled("←/h", Style::default().fg(palette::WHALE_ACTION).bold()),
                    hint_label(locale, MessageId::UserInputHintBack),
                    Span::raw("  "),
                    Span::styled("Esc", Style::default().fg(palette::WHALE_ACTION).bold()),
                    hint_label(locale, MessageId::UserInputHintCancel),
                ]));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(
                        quick_pick_label,
                        Style::default().fg(palette::WHALE_ACTION).bold(),
                    ),
                    hint_label(locale, MessageId::UserInputHintQuickPick),
                    Span::raw("  "),
                    Span::styled("↑/↓", Style::default().fg(palette::WHALE_ACTION).bold()),
                    hint_label(locale, MessageId::UserInputHintMove),
                    Span::raw("  "),
                    Span::styled("Enter", Style::default().fg(palette::WHALE_ACTION).bold()),
                    hint_label(locale, MessageId::UserInputHintConfirm),
                    Span::raw("  "),
                    Span::styled("←/h", Style::default().fg(palette::WHALE_ACTION).bold()),
                    hint_label(locale, MessageId::UserInputHintBack),
                    Span::raw("  "),
                    Span::styled("Esc", Style::default().fg(palette::WHALE_ACTION).bold()),
                    hint_label(locale, MessageId::UserInputHintCancel),
                ]));
            }
        }

        let popup_area = sheet_rect(area, self.content_line_count());
        let inner_h = popup_area.height.saturating_sub(4) as usize;
        // Paragraph scroll offsets count wrapped rows, not the source Lines.
        // Use the same wrapper for focus measurement and painting so long
        // questions/descriptions cannot hide the highlighted choice (#6045).
        let width = modal_block(&header).inner(popup_area).width.max(1);
        let heights: Vec<usize> = lines
            .iter()
            .map(|line| {
                Paragraph::new(line.clone())
                    .wrap(Wrap { trim: true })
                    .line_count(width)
            })
            .collect();
        let (start, end) = self.focused_line_range();
        let focus_end = heights[..=end].iter().sum::<usize>().saturating_sub(1);
        let focus_start = if self.mode == InputMode::OtherInput {
            // Keep the typing end visible when a custom answer wraps.
            focus_end
        } else {
            heights[..start].iter().sum()
        };
        let total_rows = heights.iter().sum::<usize>();
        let max_scroll = u16::try_from(total_rows.saturating_sub(inner_h)).unwrap_or(u16::MAX);
        let scroll = if self.manual_scroll {
            self.scroll_offset.get().min(max_scroll)
        } else {
            scroll_to_keep_range_visible((focus_start, focus_end), total_rows, inner_h)
        };
        self.scroll_offset.set(scroll);
        self.max_scroll.set(max_scroll);
        let paragraph = Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true })
            .scroll((scroll, 0))
            .block(modal_block(&header));

        render_modal_chrome(area, popup_area, buf);
        paragraph.render(popup_area, buf);
    }

    fn occupied_region(&self, area: Rect) -> Rect {
        // Bottom sheet plus the one-cell drop shadow `render_modal_surface`
        // draws at +1/+1. Transcript above the sheet stays undimmed.
        let popup = sheet_rect(area, self.content_line_count());
        Rect {
            x: popup.x,
            y: popup.y,
            width: (popup.width.saturating_add(1)).min(area.right().saturating_sub(popup.x)),
            height: (popup.height.saturating_add(1)).min(area.bottom().saturating_sub(popup.y)),
        }
    }
}

/// Bottom-anchored sheet: grows with content, leaves a transcript strip
/// above when the frame is tall enough, and never uses a fixed 22-row cap
/// that clips options on every terminal ≥ 37 rows (#6045).
fn sheet_rect(r: Rect, content_lines: usize) -> Rect {
    if r.width == 0 || r.height == 0 {
        return Rect {
            x: r.x,
            y: r.y.saturating_add(r.height),
            width: 0,
            height: 0,
        };
    }
    let desired = u16::try_from(content_lines)
        .unwrap_or(u16::MAX)
        .saturating_add(4);
    let transcript_reserve = if r.height >= 16 {
        4u16.min(r.height.saturating_sub(8))
    } else {
        0
    };
    let max_height = r
        .height
        .saturating_sub(transcript_reserve)
        .max(6.min(r.height));
    let height = desired.min(max_height).max(6.min(r.height)).min(r.height);
    Rect {
        x: r.x,
        y: r.y.saturating_add(r.height.saturating_sub(height)),
        width: r.width,
        height,
    }
}

fn scroll_to_keep_range_visible(
    (focus_start, focus_end): (usize, usize),
    total_lines: usize,
    inner_h: usize,
) -> u16 {
    if inner_h == 0 || total_lines <= inner_h {
        return 0;
    }
    let max_scroll = total_lines.saturating_sub(inner_h);
    let mut scroll = 0usize;
    if focus_end >= inner_h {
        scroll = focus_end.saturating_add(1).saturating_sub(inner_h);
    }
    if focus_start < scroll {
        scroll = focus_start;
    }
    u16::try_from(scroll.min(max_scroll)).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::user_input::{UserInputOption, UserInputQuestion, UserInputRequest};

    fn render_view(view: &UserInputView, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        view.render(area, &mut buf);

        (0..height)
            .map(|y| (0..width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn sample_view() -> UserInputView {
        UserInputView::new(
            "tool-1",
            UserInputRequest {
                questions: vec![UserInputQuestion {
                    header: "Confirm".to_string(),
                    id: "confirm".to_string(),
                    question: "What should happen next?".to_string(),
                    options: vec![
                        UserInputOption {
                            label: "Ship it".to_string(),
                            description: "Proceed with the current change set".to_string(),
                        },
                        UserInputOption {
                            label: "Revise it".to_string(),
                            description: "Return to editing before continuing".to_string(),
                        },
                    ],
                    allow_free_text: true,
                    multi_select: false,
                }],
            },
            Locale::En,
        )
    }

    #[test]
    fn user_input_modal_calls_out_required_action_and_controls() {
        let rendered = render_view(&sample_view(), 110, 36);

        assert!(rendered.contains("Action required"));
        assert!(rendered.contains("Question 1 of 1"));
        assert!(rendered.contains("quick pick"));
        // allow_free_text=true surfaces the Other row.
        assert!(rendered.contains("Other"));
    }

    #[test]
    fn user_input_modal_renders_custom_response_state() {
        let mut view = sample_view();
        view.selected = 2;
        view.mode = InputMode::OtherInput;
        view.other_input = "Need one more pass".to_string();

        let rendered = render_view(&view, 110, 36);

        assert!(rendered.contains("Custom response"));
        assert!(rendered.contains("Need one more pass"));
        assert!(rendered.contains("Enter"));
        assert!(rendered.contains("submit"));
    }

    #[test]
    fn user_input_modal_keeps_other_row_when_free_text_disabled() {
        // v0.9.4: a custom free-text response is ALWAYS available alongside
        // the options, even when the model did not offer it. The wire field
        // `allow_free_text` stays for backward compatibility but no longer
        // gates the row (#3102 originally hid it).
        let mut view = sample_view();
        view.request.questions[0].allow_free_text = false;
        view.selected = 0;

        let rendered = render_view(&view, 110, 36);
        assert!(
            rendered.contains("Type a custom response"),
            "Other row must stay reachable even when allow_free_text is false"
        );
        assert!(rendered.contains("Other"));

        // Entering the row switches to free-text input mode regardless.
        view.selected = view.option_count() - 1;
        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.mode, InputMode::OtherInput);
    }

    #[test]
    fn user_input_modal_renders_multi_select_ticks_and_confirm() {
        // Issue #3102: multi_select=true renders a check-mark gutter on
        // toggled options plus a trailing "Confirm selection" row, and the
        // controls hint advertises Space/Enter toggle semantics. With the
        // v0.9.4 always-available Other row, confirm sits at index 3.
        let mut view = sample_view();
        view.request.questions[0].multi_select = true;
        view.request.questions[0].allow_free_text = false;
        // Toggle the first option into the pending set.
        view.multi_pending.push(0);
        // Highlight the confirm row (last selectable row).
        view.selected = view.option_count() - 1;

        let rendered = render_view(&view, 120, 40);
        assert!(rendered.contains("✔"), "toggled option shows a check mark");
        assert!(
            rendered.contains("Confirm selection"),
            "multi-select renders a confirm row"
        );
        assert!(rendered.contains("Submit 1 selected"));
        assert!(rendered.contains("toggle"));
        assert!(
            rendered.contains("▸  4) Confirm selection"),
            "confirm row should display selected focus at its real quick-pick index"
        );
        assert!(
            !rendered.contains("5) Confirm selection"),
            "confirm row must not advertise an unreachable quick-pick number"
        );
    }

    #[test]
    fn user_input_modal_space_toggles_and_enter_confirms_multi_select() {
        // Keyboard-first multi-select: Space toggles the highlighted option
        // into the pending set without leaving the picker; Enter on the
        // confirm row flushes the set.
        let mut view = sample_view();
        view.request.questions[0].multi_select = true;
        view.selected = 0;

        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.multi_pending, vec![0], "Space toggles option 0 in");

        let _action = view.handle_selecting_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(view.multi_pending.is_empty(), "Space toggles option 0 out");

        // Space on the confirm row must not toggle it (it is not an option).
        view.selected = view.confirm_index().expect("confirm row present");
        let before = view.multi_pending.clone();
        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.multi_pending, before, "Space on confirm is a no-op");

        // Enter on the confirm row flushes the pending set as answers.
        view.multi_pending.push(0);
        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(action, ViewAction::EmitAndClose(ViewEvent::UserInputSubmitted { tool_id, response })
                if tool_id == "tool-1" && response.answers.first().is_some_and(|a| a.value == "Ship it")),
            "Enter on confirm submits the toggled options"
        );
    }

    #[test]
    fn user_input_modal_double_enter_never_submits_empty_multi_select() {
        // Enter on a multi-select option used to toggle it into the pending
        // set, so a second Enter (single-select muscle memory) toggled it
        // back out — and Confirm then submitted an empty answer set.
        let mut view = sample_view();
        view.request.questions[0].multi_select = true;
        view.selected = 0;

        // First Enter: option 0 joins the pending set, focus moves to Confirm.
        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(
            view.multi_pending,
            vec![0],
            "Enter selects the highlighted option"
        );
        assert!(
            view.is_confirm_selected(),
            "focus moves to the Confirm row after Enter"
        );

        // Second Enter submits exactly that option — never an empty set.
        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(action, ViewAction::EmitAndClose(ViewEvent::UserInputSubmitted { tool_id, response })
                if tool_id == "tool-1"
                    && response.answers.len() == 1
                    && response.answers[0].value == "Ship it"),
            "double-Enter must submit the highlighted option, not an empty set"
        );
    }

    #[test]
    fn user_input_modal_enter_never_deselects_multi_select_option() {
        // Deselecting remains Space's job: Enter on an already-toggled option
        // keeps it in the pending set.
        let mut view = sample_view();
        view.request.questions[0].multi_select = true;
        view.selected = 0;

        let _ = view.handle_selecting_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(view.multi_pending, vec![0], "Space toggles option 0 in");

        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(
            view.multi_pending,
            vec![0],
            "Enter must not toggle the option back out"
        );

        // Space still toggles both ways.
        view.selected = 0;
        let _ = view.handle_selecting_key(KeyEvent::from(KeyCode::Char(' ')));
        assert!(view.multi_pending.is_empty(), "Space toggles option 0 out");
    }

    fn many_option_view() -> UserInputView {
        UserInputView::new(
            "tool-1",
            UserInputRequest {
                questions: vec![
                    UserInputQuestion {
                        header: "Choose".to_string(),
                        id: "q1".to_string(),
                        question: "Which path?".to_string(),
                        options: (1..=8)
                            .map(|n| UserInputOption {
                                label: format!("Option {n}"),
                                description: format!(
                                    "A longer description for option {n} that wraps on a narrow terminal"
                                ),
                            })
                            .collect(),
                        allow_free_text: true,
                        multi_select: false,
                    },
                    UserInputQuestion {
                        header: "Confirm".to_string(),
                        id: "q2".to_string(),
                        question: "Second question after the first.".to_string(),
                        options: vec![UserInputOption {
                            label: "Yes".to_string(),
                            description: "Proceed".to_string(),
                        }],
                        allow_free_text: true,
                        multi_select: false,
                    },
                ],
            },
            Locale::En,
        )
    }

    #[test]
    fn user_input_sheet_is_bottom_anchored_and_not_capped_at_22() {
        let area = Rect::new(0, 0, 141, 38);
        let view = many_option_view();
        let content = view.content_line_count();
        let popup = sheet_rect(area, content);

        assert_eq!(
            popup.bottom(),
            area.bottom(),
            "sheet must sit on the bottom"
        );
        assert!(popup.y > 0, "transcript strip remains above the sheet");
        assert_eq!(popup.width, area.width);
        assert!(
            popup.height > 22,
            "141×38 must not be stuck at the old 22-row cap, got {}",
            popup.height
        );

        let capped = sheet_rect(area, 100);
        assert!(
            capped.height > 22,
            "long content may grow past 22 rows; got {}",
            capped.height
        );
        assert_eq!(capped.bottom(), area.bottom());
    }

    fn wheel(view: &mut UserInputView, kind: MouseEventKind) {
        assert!(matches!(
            view.handle_mouse(MouseEvent {
                kind,
                column: 1,
                row: 1,
                modifiers: crossterm::event::KeyModifiers::NONE,
            }),
            ViewAction::None
        ));
    }

    #[test]
    fn user_input_wheel_browses_wrapped_rows_without_changing_answers() {
        for (width, height) in [(40, 12), (80, 24), (100, 32), (141, 38)] {
            let mut view = many_option_view();
            view.request.questions[0].multi_select = true;
            view.multi_pending.push(0);
            let before = render_view(&view, width, height);
            let initial = view.scroll_offset.get();
            wheel(&mut view, MouseEventKind::ScrollDown);
            let after = render_view(&view, width, height);
            assert_eq!(
                view.scroll_offset.get(),
                (initial + 3).min(view.max_scroll.get())
            );
            if view.max_scroll.get() == 0 {
                assert_eq!(before, after, "fully visible content must stay still");
            } else {
                assert_ne!(
                    before, after,
                    "overflowing question content must scroll at {width}x{height}"
                );
            }
            assert_eq!(view.selected, 0);
            assert_eq!(view.multi_pending, [0]);
            assert!(view.answered.is_empty());

            for _ in 0..100 {
                wheel(&mut view, MouseEventKind::ScrollDown);
            }
            assert_eq!(view.scroll_offset.get(), view.max_scroll.get());
            view.handle_key(KeyEvent::from(KeyCode::Down));
            let focused = render_view(&view, width, height);
            assert!(focused.contains("▸  2) Option 2"), "{focused}");
            assert_eq!(view.multi_pending, [0]);
            for _ in 0..100 {
                wheel(&mut view, MouseEventKind::ScrollUp);
            }
            assert_eq!(view.scroll_offset.get(), 0);
        }
    }

    #[test]
    fn user_input_wheel_resize_and_typing_restore_custom_answer_visibility() {
        let mut view = many_option_view();
        view.selected = view.option_count() - 1;
        view.handle_key(KeyEvent::from(KeyCode::Enter));
        for ch in "retained custom answer".chars() {
            view.handle_key(KeyEvent::from(KeyCode::Char(ch)));
        }
        render_view(&view, 40, 12);
        wheel(&mut view, MouseEventKind::ScrollDown);
        let narrow_offset = view.scroll_offset.get();
        render_view(&view, 141, 38);
        assert!(view.max_scroll.get() < narrow_offset);
        assert_eq!(view.scroll_offset.get(), view.max_scroll.get());

        render_view(&view, 40, 12);
        for _ in 0..100 {
            wheel(&mut view, MouseEventKind::ScrollUp);
        }
        let browsing = render_view(&view, 40, 12);
        assert!(!browsing.contains("retained custom answer"));
        view.handle_key(KeyEvent::from(KeyCode::Char('!')));
        let editing = render_view(&view, 40, 12);
        assert!(editing.contains("answer!"), "{editing}");
        assert_eq!(view.other_input, "retained custom answer!");
        view.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(view.answered[0][0].value, "retained custom answer!");
        assert_eq!(view.question_index, 1);
        view.handle_key(KeyEvent::from(KeyCode::Left));
        assert_eq!(view.question_index, 0);
        assert!(
            view.answered.is_empty(),
            "back navigation permits correction"
        );
    }

    #[test]
    fn user_input_sheet_fits_80x24_and_keeps_selection_visible() {
        let mut view = many_option_view();
        view.selected = 7;
        let rendered = render_view(&view, 80, 24);
        assert!(
            rendered.contains("Option 8") || rendered.contains("8) Option"),
            "selected last option must be scrolled into view on 80×24:\n{rendered}"
        );
        let popup = sheet_rect(Rect::new(0, 0, 80, 24), view.content_line_count());
        assert_eq!(popup.bottom(), 24);
        assert!(popup.height <= 24);
    }

    #[test]
    fn wrapped_questions_keep_choices_and_custom_typing_visible() {
        let mut view = sample_view();
        let question = &mut view.request.questions[0];
        question.question = "Choose a synthetic option to verify the question sheet, its scrolling, and visible selection when both the question and option descriptions wrap across several terminal rows.".into();
        question.options = (1..=4)
            .map(|n| UserInputOption {
                label: format!("Option {n}"),
                description: "This synthetic option has a long description that wraps across several rows; choosing it writes no external state and triggers no provider charge.".into(),
            })
            .collect();
        question.multi_select = true;

        for (width, height) in [(40, 12), (60, 16), (80, 24), (100, 32), (140, 40)] {
            view.mode = InputMode::Selecting;
            for selected in 0..view.option_count() {
                view.selected = selected;
                let label = match selected {
                    4 => "Other".to_string(),
                    5 => "Confirm selection".to_string(),
                    _ => format!("Option {}", selected + 1),
                };
                let focused = format!(
                    "{}  {}) {label}",
                    crate::tui::glyphs::selection_marker(true),
                    selected + 1
                );
                let rendered = render_view(&view, width, height);
                assert!(
                    rendered.contains(&focused),
                    "highlighted choice must remain visible at {width}x{height}:\n{rendered}"
                );
            }
            view.mode = InputMode::OtherInput;
            view.other_input = format!("{}TAIL_SENTINEL", "輸入 text ".repeat(40));
            let rendered = render_view(&view, width, height);
            assert!(
                rendered.contains("TAIL_SENTINEL"),
                "custom-response typing end must remain visible at {width}x{height}:\n{rendered}"
            );
        }
    }

    #[test]
    fn user_input_custom_response_stays_visible_while_typing() {
        let mut view = many_option_view();
        view.selected = view.option_count() - 1;
        view.mode = InputMode::OtherInput;
        view.other_input = "Need one more pass on the last option".to_string();
        for (width, height) in [(141, 38), (80, 24)] {
            let rendered = render_view(&view, width, height);
            assert!(
                rendered.contains("Need one more pass"),
                "typed custom response must stay visible at {width}×{height}:\n{rendered}"
            );
        }
    }

    #[test]
    fn user_input_left_goes_back_to_previous_question() {
        let mut view = many_option_view();
        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.question_index, 1);
        assert_eq!(view.answered.len(), 1);

        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Left));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(view.question_index, 0);
        assert!(view.answered.is_empty());

        let action = view.handle_selecting_key(KeyEvent::from(KeyCode::Char('h')));
        assert!(matches!(action, ViewAction::None));
        assert_eq!(
            view.question_index, 0,
            "back on the first question is a no-op"
        );
    }

    #[test]
    fn user_input_modal_occupied_region_matches_painted_card_plus_shadow() {
        let area = Rect::new(0, 0, 120, 40);
        let view = sample_view();
        let popup = sheet_rect(area, view.content_line_count());
        let occupied = view.occupied_region(area);

        assert_eq!(occupied.x, popup.x);
        assert_eq!(occupied.y, popup.y);
        assert_eq!(
            occupied.width,
            (popup.width.saturating_add(1)).min(area.right().saturating_sub(popup.x))
        );
        assert_eq!(
            occupied.height,
            (popup.height.saturating_add(1)).min(area.bottom().saturating_sub(popup.y))
        );
        assert!(area.right() >= occupied.right());
        assert!(area.bottom() >= occupied.bottom());
    }

    #[test]
    fn user_input_modal_leaves_surrounding_frame_visible() {
        use crate::tui::views::ViewStack;

        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        // Pre-fill the frame as if the live transcript had painted it.
        for y in 0..area.height {
            for x in 0..area.width {
                buf[(x, y)].set_symbol("·");
            }
        }

        let mut stack = ViewStack::default();
        stack.push(sample_view());
        stack.render(area, &mut buf);

        // Transcript above the bottom sheet survives untouched.
        assert_eq!(buf[(0, 0)].symbol(), "·");
        assert_eq!(buf[(119, 0)].symbol(), "·");
        assert_eq!(buf[(60, 0)].symbol(), "·");
        assert_eq!(buf[(60, 4)].symbol(), "·");
        // The sheet itself is blanked + repainted by the modal surface.
        assert_ne!(buf[(60, 39)].symbol(), "·");
        assert_ne!(buf[(0, 39)].symbol(), "·");
        assert_ne!(buf[(119, 39)].symbol(), "·");
    }

    #[test]
    fn user_input_modal_numbers_confirm_after_other_row() {
        let mut view = sample_view();
        view.request.questions[0].multi_select = true;
        view.request.questions[0].allow_free_text = true;
        view.selected = view.option_count() - 1;

        let rendered = render_view(&view, 120, 40);
        assert!(rendered.contains("3) Other"));
        assert!(
            rendered.contains("▸  4) Confirm selection"),
            "confirm should follow the optional Other row with selected focus"
        );
        assert!(!rendered.contains("5) Confirm selection"));
    }
}

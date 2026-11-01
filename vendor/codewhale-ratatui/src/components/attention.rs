//! A quiet inbox for work that needs a person's answer.
//!
//! Hosts supply every request, identity, state, priority, timestamp and action.
//! Priority changes display order only. Selection refers to the original input
//! index, so sorting never changes which request a host means to open. This
//! component neither grants permission nor dispatches an action.

use std::{borrow::Cow, cmp::Reverse};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Clear, Widget},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{EmptyState, KeyHints, ListState, Paint, Role, State, StatusMark, Theme, glyphs, text};

use super::workbench::row;

/// Localized inbox copy. States use the item's [`StatusMark`] word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttentionWords {
    pub title: Cow<'static, str>,
    /// Shown after the number of caller-supplied items.
    pub requests: Cow<'static, str>,
    /// Shown after the count, including partially visible requests.
    pub hidden: Cow<'static, str>,
    pub clipped: Cow<'static, str>,
    pub empty_title: Cow<'static, str>,
    pub empty_detail: Cow<'static, str>,
}

const ENGLISH: AttentionWords = AttentionWords {
    title: Cow::Borrowed("Needs you"),
    requests: Cow::Borrowed("waiting"),
    hidden: Cow::Borrowed("not fully shown"),
    clipped: Cow::Borrowed("Open the full request before answering"),
    empty_title: Cow::Borrowed("No requests waiting"),
    empty_detail: Cow::Borrowed("Questions and decisions appear here."),
};

impl Default for AttentionWords {
    fn default() -> Self {
        ENGLISH.clone()
    }
}

/// A request with project and agent context. Its action is display copy;
/// the host owns the underlying decision and all keyboard/pointer handlers.
#[derive(Clone, Debug)]
pub struct AttentionItem<'a> {
    pub project: Cow<'a, str>,
    pub agent: Cow<'a, str>,
    pub request: Cow<'a, str>,
    pub status: StatusMark,
    /// Greater values appear first; equal priorities preserve caller order.
    pub priority: u8,
    pub timestamp: Option<Cow<'a, str>>,
    pub action: Option<Cow<'a, str>>,
    /// Shortcuts appear only on the selected request, alongside its action.
    pub hints: Option<&'a KeyHints>,
    pub selected: bool,
    pub focused: bool,
    /// Keep the inbox concise. Further request rows get an explicit cue,
    /// and the action rail is withheld until the full request is available.
    pub request_rows: u16,
    pub words: Option<&'a AttentionWords>,
}

impl<'a> AttentionItem<'a> {
    #[must_use]
    pub fn new(
        project: impl Into<Cow<'a, str>>,
        agent: impl Into<Cow<'a, str>>,
        request: impl Into<Cow<'a, str>>,
        state: State,
    ) -> Self {
        Self {
            project: project.into(),
            agent: agent.into(),
            request: request.into(),
            status: StatusMark::new(state),
            priority: 0,
            timestamp: None,
            action: None,
            hints: None,
            selected: false,
            focused: false,
            request_rows: 3,
            words: None,
        }
    }

    #[must_use]
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    #[must_use]
    pub fn timestamp(mut self, timestamp: impl Into<Cow<'a, str>>) -> Self {
        self.timestamp = Some(timestamp.into());
        self
    }

    #[must_use]
    pub fn action(mut self, action: impl Into<Cow<'a, str>>) -> Self {
        self.action = Some(action.into());
        self
    }

    #[must_use]
    pub fn hints(mut self, hints: &'a KeyHints) -> Self {
        self.hints = Some(hints);
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn request_rows(mut self, rows: u16) -> Self {
        self.request_rows = rows.max(1);
        self
    }

    #[must_use]
    pub fn status_word(mut self, word: impl Into<Cow<'static, str>>) -> Self {
        self.status = self.status.word(word);
        self
    }

    #[must_use]
    pub fn with_words(mut self, words: &'a AttentionWords) -> Self {
        self.words = Some(words);
        self
    }

    fn words(&self) -> &AttentionWords {
        self.words.unwrap_or(&ENGLISH)
    }

    fn plan(&self, width: u16, theme: &Theme, selected: bool) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let content_width = width.saturating_sub(gutter(width));
        let project = safe(&self.project);
        let agent = safe(&self.agent);
        let identity = if agent.is_empty() {
            project
        } else {
            format!("{project} / {agent}")
        };
        let timestamp = self
            .timestamp
            .as_deref()
            .map(safe)
            .filter(|value| !value.is_empty());
        let mut state = self.status.spans(theme);
        let status_width: usize = state.iter().map(Span::width).sum();
        if let Some(timestamp) = timestamp {
            state.push(Span::raw("  "));
            state.push(Span::styled(timestamp, theme.fg(Role::Muted)));
        }
        let state_width: usize = state.iter().map(Span::width).sum();
        let mut complete = status_width <= usize::from(content_width);
        let mut lines = Vec::new();
        let strong = theme.fg(Role::Foreground).add_modifier(Modifier::BOLD);
        if content_width >= 52 && state_width + 4 < usize::from(content_width) {
            let identity_width = usize::from(content_width).saturating_sub(state_width + 2);
            let identity_fit = text::truncate(&identity, identity_width, theme.ascii());
            complete &= identity_fit == identity;
            let used = text::width(&identity_fit);
            let mut header = vec![
                Span::styled(identity_fit.into_owned(), strong),
                Span::raw(
                    " ".repeat(usize::from(content_width).saturating_sub(used + state_width)),
                ),
            ];
            header.extend(state);
            lines.push(Line::from(header));
        } else {
            let status = Line::from(state);
            // Metadata can recede at narrow widths, but an identity that is
            // clipped must never sit above an available decision shortcut.
            lines.push(status);
            let identity_fit = text::truncate(&identity, usize::from(content_width), theme.ascii());
            complete &= identity_fit == identity;
            lines.push(Line::from(Span::styled(identity_fit.into_owned(), strong)));
        }
        let request = wrap(&self.request, content_width, theme.ascii());
        let keep = usize::from(self.request_rows.max(1));
        complete &= request.complete && request.lines.len() <= keep;
        lines.extend(
            request
                .lines
                .into_iter()
                .take(keep)
                .map(|line| Line::from(Span::styled(line, theme.fg(Role::Foreground)))),
        );
        if complete {
            let mut actions = Vec::new();
            if let Some(action) = self
                .action
                .as_deref()
                .map(safe)
                .filter(|value| !value.is_empty())
            {
                actions.extend(
                    wrap(&action, content_width, theme.ascii())
                        .lines
                        .into_iter()
                        .map(|line| {
                            Line::from(Span::styled(
                                line,
                                theme.fg(if selected { Role::Accent } else { Role::Muted }),
                            ))
                        }),
                );
            }
            if selected && let Some(hints) = self.hints {
                let hint_lines = hints.lines(content_width, theme);
                if actions.len() == 1
                    && hint_lines.len() == 1
                    && actions[0].width() + hint_lines[0].width() + 3 <= usize::from(content_width)
                {
                    let gap = usize::from(content_width)
                        .saturating_sub(actions[0].width() + hint_lines[0].width());
                    actions[0].spans.push(Span::raw(" ".repeat(gap)));
                    actions[0].spans.extend(hint_lines[0].spans.clone());
                } else {
                    actions.extend(hint_lines);
                }
            }
            lines.extend(actions);
        } else {
            lines.push(clipped_line(self.words(), content_width, theme));
        }
        lines
    }

    fn draw(&self, area: Rect, buf: &mut Buffer, theme: &Theme, selected: bool, focused: bool) {
        if area.is_empty() {
            return;
        }
        Clear.render(area, buf);
        buf.set_style(
            area,
            theme.bg(if selected {
                Role::Selected
            } else {
                Role::Background
            }),
        );
        let gutter = gutter(area.width);
        let content = Rect {
            x: area.x.saturating_add(gutter),
            width: area.width.saturating_sub(gutter),
            ..area
        };
        let mut lines = self.plan(area.width, theme, selected);
        if lines.len() > usize::from(area.height) {
            if area.height == 1 {
                lines.truncate(1);
            } else {
                lines.truncate(usize::from(area.height.saturating_sub(1)));
                lines.push(clipped_line(self.words(), content.width, theme));
            }
        }
        for (index, line) in lines.iter().take(usize::from(area.height)).enumerate() {
            let y = area.y.saturating_add(rows(index));
            if gutter > 0 {
                let mark = if index == 0 && selected {
                    glyphs::SELECTION
                } else {
                    "▏"
                };
                let rail = if selected && focused {
                    Role::Accent
                } else {
                    Role::BorderStrong
                };
                buf.set_stringn(
                    area.x,
                    y,
                    glyphs::pick(mark, theme.ascii()),
                    1,
                    theme.fg(rail),
                );
            }
            row(
                Rect {
                    y,
                    height: 1,
                    ..content
                },
                buf,
                line,
            );
        }
    }
}

impl Paint for AttentionItem<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.draw(
            area.intersection(buf.area),
            buf,
            theme,
            self.selected,
            self.focused,
        );
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        rows(self.plan(width, theme, self.selected).len())
    }
}

/// An inbox with stable caller priority, original-index selection and an
/// honest overflow rail. Hosts own navigation, ordering inputs and opening.
#[derive(Clone, Debug)]
pub struct AttentionQueue<'a> {
    pub items: &'a [AttentionItem<'a>],
    /// Original index in `items`, never an index in the sorted presentation.
    pub selected: Option<usize>,
    pub focused: bool,
    /// Preferred offset in priority order; selection remains in view.
    pub offset: usize,
    pub words: Option<&'a AttentionWords>,
}

impl<'a> AttentionQueue<'a> {
    #[must_use]
    pub fn new(items: &'a [AttentionItem<'a>]) -> Self {
        Self {
            items,
            selected: None,
            focused: false,
            offset: 0,
            words: None,
        }
    }

    #[must_use]
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = Some(index);
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    #[must_use]
    pub fn with_words(mut self, words: &'a AttentionWords) -> Self {
        self.words = Some(words);
        self
    }

    /// Original item indices in display order. Priority is a presentation
    /// input, with no urgency or lifecycle inferred from state or age.
    #[must_use]
    pub fn order(&self) -> Vec<usize> {
        let mut order: Vec<_> = (0..self.items.len()).collect();
        order.sort_by_key(|index| (Reverse(self.items[*index].priority), *index));
        order
    }

    fn words(&self) -> &AttentionWords {
        self.words.unwrap_or(&ENGLISH)
    }
}

impl Paint for AttentionQueue<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        Clear.render(area, buf);
        buf.set_style(area, theme.bg(Role::Background));
        let words = self.words();
        paint_heading(
            words,
            self.items.len(),
            Rect { height: 1, ..area },
            buf,
            theme,
        );
        if area.height < 3 {
            return;
        }
        for x in area.x..area.right() {
            buf[(x, area.y.saturating_add(1))]
                .set_symbol(glyphs::pick("─", theme.ascii()))
                .set_style(theme.fg(Role::Border));
        }
        let body = Rect {
            y: area.y.saturating_add(2),
            height: area.height.saturating_sub(2),
            ..area
        };
        if self.items.is_empty() {
            paint_empty(words, body, buf, theme);
            return;
        }
        let order = self.order();
        let selected = self.selected.filter(|index| *index < self.items.len());
        let heights: Vec<_> = order
            .iter()
            .map(|index| {
                rows(
                    self.items[*index]
                        .plan(body.width, theme, selected == Some(*index))
                        .len(),
                )
            })
            .collect();
        let total = heights
            .iter()
            .map(|height| usize::from(*height))
            .sum::<usize>()
            .saturating_add(order.len().saturating_sub(1));
        let clipped = total > usize::from(body.height) || self.offset > 0;
        let available = body.height.saturating_sub(u16::from(clipped));
        let rank = selected.and_then(|selected| order.iter().position(|index| *index == selected));
        let offset = if let Some(rank) = rank {
            ListState {
                selected: rank,
                offset: self.offset,
            }
            .visible_offset(order.len(), available, |index| {
                heights[index].saturating_add(u16::from(index + 1 < order.len()))
            })
        } else {
            self.offset.min(order.len().saturating_sub(1))
        };
        let mut used = 0u16;
        let mut complete = 0usize;
        for rank in offset..order.len() {
            if used >= available {
                break;
            }
            let index = order[rank];
            let height = heights[rank].min(available.saturating_sub(used));
            self.items[index].draw(
                Rect {
                    y: body.y.saturating_add(used),
                    height,
                    ..body
                },
                buf,
                theme,
                selected == Some(index),
                self.focused,
            );
            complete += usize::from(height == heights[rank]);
            used = used.saturating_add(height).saturating_add(1);
        }
        if clipped && complete < self.items.len() {
            let hidden = self.items.len().saturating_sub(complete);
            row(
                Rect {
                    y: body.bottom().saturating_sub(1),
                    height: 1,
                    ..body
                },
                buf,
                &Line::from(Span::styled(
                    format!("{hidden} {}", safe(&words.hidden)),
                    theme.fg(Role::Muted),
                )),
            );
        }
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        if width == 0 {
            return 0;
        }
        if self.items.is_empty() {
            return 5;
        }
        let content = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| item.plan(width, theme, self.selected == Some(index)).len())
            .sum::<usize>();
        rows(
            content
                .saturating_add(self.items.len().saturating_sub(1))
                .saturating_add(2),
        )
    }
}

// Heading's fit is private; keep its title priority and quiet meta, using
// the shared buffer-row writer even at the last representable coordinate.
fn paint_heading(
    words: &AttentionWords,
    count: usize,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    let width = usize::from(area.width);
    let title = safe(&words.title);
    let meta = format!("{count} {}", safe(&words.requests));
    let full = text::width(&title)
        .saturating_add(text::width(&meta))
        .saturating_add(2)
        <= width;
    let budget = if full {
        text::width(&meta)
    } else if width >= 12 {
        text::width(&meta).min(width / 3)
    } else {
        0
    };
    let meta = if full || budget >= 4 {
        text::truncate(&meta, budget, theme.ascii()).into_owned()
    } else {
        String::new()
    };
    let room = width.saturating_sub(if meta.is_empty() {
        0
    } else {
        text::width(&meta) + 2
    });
    let title = text::truncate(&title, room, theme.ascii()).into_owned();
    let gap = width.saturating_sub(text::width(&title) + text::width(&meta));
    row(
        area,
        buf,
        &Line::from(vec![
            Span::styled(
                title,
                theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" ".repeat(if meta.is_empty() { 0 } else { gap })),
            Span::styled(meta, theme.fg(Role::Muted)),
        ]),
    );
}

fn paint_empty(words: &AttentionWords, area: Rect, buf: &mut Buffer, theme: &Theme) {
    if area.right() != u16::MAX && area.bottom() != u16::MAX {
        EmptyState::new(words.empty_title.clone())
            .body(words.empty_detail.clone())
            .paint(area, buf, theme);
        return;
    }
    // The general empty component uses centered terminal widgets. At the
    // saturated edge, retain its title/body hierarchy with safe row writes.
    let title = text::truncate(
        &safe(&words.empty_title),
        usize::from(area.width),
        theme.ascii(),
    )
    .into_owned();
    let body = wrap(&words.empty_detail, area.width, theme.ascii());
    let mut lines = vec![Line::styled(
        title,
        theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
    )];
    lines.extend(
        body.lines
            .into_iter()
            .take(3)
            .map(|line| Line::styled(line, theme.fg(Role::Muted))),
    );
    lines.truncate(usize::from(area.height));
    let top = area
        .y
        .saturating_add(area.height.saturating_sub(rows(lines.len())) / 2);
    for (index, mut line) in lines.into_iter().enumerate() {
        let inset = usize::from(area.width).saturating_sub(line.width()) / 2;
        line.spans.insert(0, Span::raw(" ".repeat(inset)));
        row(
            Rect {
                y: top.saturating_add(rows(index)),
                height: 1,
                ..area
            },
            buf,
            &line,
        );
    }
}

fn safe(value: &str) -> String {
    text::display_safe(value).into_owned()
}
fn rows(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}
fn gutter(width: u16) -> u16 {
    if width >= 6 { 2 } else { 0 }
}

fn clipped_line(words: &AttentionWords, width: u16, theme: &Theme) -> Line<'static> {
    let words = safe(&words.clipped);
    Line::from(Span::styled(
        text::truncate_words(&words, usize::from(width), theme.ascii()).into_owned(),
        theme.fg(Role::Attention),
    ))
}

// Keep authored line breaks and word spacing; long words break only between
// whole Unicode graphemes. A grapheme wider than the viewport gets a cut mark.
struct Wrapped {
    lines: Vec<String>,
    complete: bool,
}

fn wrap(value: &str, width: u16, ascii: bool) -> Wrapped {
    let mut result = Wrapped {
        lines: Vec::new(),
        complete: true,
    };
    if value.is_empty() || width == 0 {
        return result;
    }
    let width = usize::from(width);
    for logical in value.split('\n') {
        let logical = safe(logical);
        let graphemes: Vec<_> = logical
            .graphemes(true)
            .map(|value| {
                if text::width(value) > width {
                    result.complete = false;
                    Cow::Owned(text::truncate(value, width, ascii).into_owned())
                } else {
                    Cow::Borrowed(value)
                }
            })
            .collect();
        if graphemes.is_empty() {
            result.lines.push(String::new());
        }
        let mut start = 0;
        while start < graphemes.len() {
            let mut end = start;
            let mut cells = 0;
            let mut boundary = None;
            while end < graphemes.len() && cells + text::width(&graphemes[end]) <= width {
                cells += text::width(&graphemes[end]);
                if graphemes[end].chars().all(char::is_whitespace) {
                    boundary = Some(end + 1);
                }
                end += 1;
            }
            if end < graphemes.len()
                && let Some(boundary) = boundary
            {
                end = boundary;
            }
            result.lines.push(
                graphemes[start..end]
                    .iter()
                    .map(|value| value.as_ref())
                    .collect(),
            );
            start = end;
        }
    }
    result
}

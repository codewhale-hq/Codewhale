//! Settings say what the value is, where it came from and when it applies.
//! Hosts own persistence, schema validation and reset actions.

use crate::{Paint, Role, Theme, glyphs, text};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

// Prefer word boundaries without discarding whitespace or changing styled runs.
// Words wider than the row still break at whole graphemes.
fn wrap_spans(spans: &[Span<'static>], width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    if width == 0 {
        return Vec::new();
    }
    let graphemes: Vec<_> = spans
        .iter()
        .flat_map(|span| {
            span.content.graphemes(true).map(move |grapheme| {
                let cells = text::width(grapheme);
                if cells > width {
                    ("?", 1, span.style)
                } else {
                    (grapheme, cells, span.style)
                }
            })
        })
        .collect();
    let whitespace = |grapheme: &str| grapheme.chars().next().is_some_and(char::is_whitespace);
    let mut lines = Vec::new();
    let mut start = 0;
    while start < graphemes.len() {
        let mut end = start;
        let mut cells = 0;
        let mut last_boundary = None;
        while end < graphemes.len() && cells + graphemes[end].1 <= width {
            cells += graphemes[end].1;
            if whitespace(graphemes[end].0) {
                last_boundary = Some(end + 1);
            }
            end += 1;
        }
        if end < graphemes.len()
            && !whitespace(graphemes[end].0)
            && let Some(boundary) = last_boundary
        {
            end = boundary;
        }
        let mut current: Vec<Span<'static>> = Vec::new();
        for &(grapheme, _, style) in &graphemes[start..end] {
            if let Some(last) = current.last_mut()
                && last.style == style
            {
                last.content.to_mut().push_str(grapheme);
            } else {
                current.push(Span::styled(grapheme.to_string(), style));
            }
        }
        lines.push(Line::from(current));
        start = end;
    }
    lines
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingWords {
    pub applies_now: Cow<'static, str>,
    pub changed: Cow<'static, str>,
    pub reset: Cow<'static, str>,
    pub default: Cow<'static, str>,
}
impl Default for SettingWords {
    fn default() -> Self {
        Self {
            applies_now: "Applies now".into(),
            changed: "changed".into(),
            reset: "reset to default".into(),
            default: "Default".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SettingRow<'a> {
    pub label: Cow<'a, str>,
    pub value: Cow<'a, str>,
    pub source: Option<Cow<'a, str>>,
    pub apply: Option<Cow<'a, str>>,
    pub locked: Option<Cow<'a, str>>,
    pub changed: bool,
    pub modified: bool,
    pub selected: bool,
    pub words: SettingWords,
}
impl<'a> SettingRow<'a> {
    #[must_use]
    pub fn new(label: impl Into<Cow<'a, str>>, value: impl Into<Cow<'a, str>>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            source: None,
            apply: None,
            locked: None,
            changed: false,
            modified: false,
            selected: false,
            words: SettingWords::default(),
        }
    }
    #[must_use]
    pub fn source(mut self, source: impl Into<Cow<'a, str>>) -> Self {
        self.source = Some(source.into());
        self
    }
    #[must_use]
    pub fn apply(mut self, apply: impl Into<Cow<'a, str>>) -> Self {
        self.apply = Some(apply.into());
        self
    }
    #[must_use]
    pub fn locked(mut self, reason: impl Into<Cow<'a, str>>) -> Self {
        self.locked = Some(reason.into());
        self
    }
    #[must_use]
    pub fn changed(mut self, changed: bool) -> Self {
        self.changed = changed;
        self
    }
    #[must_use]
    pub fn modified(mut self, modified: bool) -> Self {
        self.modified = modified;
        self
    }
    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    #[must_use]
    pub fn with_words(mut self, words: &SettingWords) -> Self {
        self.words = words.clone();
        self
    }
    #[must_use]
    pub fn lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let label_style = if self.selected {
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
        } else {
            theme.fg(Role::Foreground)
        };
        let marker = glyphs::pick(glyphs::selection_marker(self.selected), theme.ascii());
        let mut spans = vec![
            Span::styled(format!("{marker} "), theme.fg(Role::Accent)),
            Span::styled(text::display_safe(&self.label).into_owned(), label_style),
            Span::raw("  "),
            Span::styled(
                text::display_safe(&self.value).into_owned(),
                theme.fg(if self.locked.is_some() {
                    Role::Muted
                } else {
                    Role::Foreground
                }),
            ),
        ];
        let separator = if theme.ascii() { "  " } else { " · " };
        for fact in [
            self.source.as_deref(),
            Some(self.apply.as_deref().unwrap_or(&self.words.applies_now)),
            self.locked.as_deref(),
            self.changed.then_some(self.words.changed.as_ref()),
        ]
        .into_iter()
        .flatten()
        {
            if !fact.is_empty() {
                spans.push(Span::styled(
                    format!("{separator}{}", text::display_safe(fact)),
                    theme.fg(Role::Muted),
                ));
            }
        }
        // A locked setting cannot promise a reset action.
        if self.modified && self.locked.is_none() {
            spans.push(Span::styled(
                format!("{separator}r {}", text::display_safe(&self.words.reset)),
                theme.fg(Role::Muted),
            ));
        }
        wrap_spans(&spans, width)
    }
}
impl Paint for SettingRow<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let lines = self.lines(area.width, theme);
        if self.selected {
            let shown = u16::try_from(lines.len())
                .unwrap_or(u16::MAX)
                .min(area.height);
            buf.set_style(
                Rect {
                    height: shown,
                    ..area
                },
                theme.bg(Role::Selected),
            );
        }
        Paragraph::new(lines).render(area, buf);
    }
    fn height(&self, width: u16, theme: &Theme) -> u16 {
        u16::try_from(self.lines(width, theme).len()).unwrap_or(u16::MAX)
    }
}

#[derive(Clone, Debug)]
pub struct SettingDetail<'a> {
    pub label: Cow<'a, str>,
    pub description: Cow<'a, str>,
    pub apply: Option<Cow<'a, str>>,
    pub source: Option<Cow<'a, str>>,
    pub default_value: Option<Cow<'a, str>>,
    pub locked: Option<Cow<'a, str>>,
    pub modified: bool,
    pub words: SettingWords,
}
impl<'a> SettingDetail<'a> {
    #[must_use]
    pub fn new(label: impl Into<Cow<'a, str>>, description: impl Into<Cow<'a, str>>) -> Self {
        Self {
            label: label.into(),
            description: description.into(),
            apply: None,
            source: None,
            default_value: None,
            locked: None,
            modified: false,
            words: SettingWords::default(),
        }
    }
    #[must_use]
    pub fn apply(mut self, apply: impl Into<Cow<'a, str>>) -> Self {
        self.apply = Some(apply.into());
        self
    }
    #[must_use]
    pub fn source(mut self, source: impl Into<Cow<'a, str>>) -> Self {
        self.source = Some(source.into());
        self
    }
    #[must_use]
    pub fn default_value(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.default_value = Some(value.into());
        self
    }
    #[must_use]
    pub fn locked(mut self, reason: impl Into<Cow<'a, str>>) -> Self {
        self.locked = Some(reason.into());
        self
    }
    #[must_use]
    pub fn modified(mut self, modified: bool) -> Self {
        self.modified = modified;
        self
    }
    #[must_use]
    pub fn with_words(mut self, words: &SettingWords) -> Self {
        self.words = words.clone();
        self
    }
    #[must_use]
    pub fn lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let mut lines = wrap_spans(
            &[Span::styled(
                text::display_safe(&self.label).into_owned(),
                theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
            )],
            width,
        );
        for paragraph in self.description.lines() {
            lines.extend(wrap_spans(
                &[Span::styled(
                    text::display_safe(paragraph).into_owned(),
                    theme.fg(Role::Foreground),
                )],
                width,
            ));
        }
        let mut facts = vec![
            self.apply
                .as_deref()
                .unwrap_or(&self.words.applies_now)
                .to_string(),
        ];
        if let Some(source) = &self.source {
            facts.push(source.to_string());
        }
        if let Some(value) = &self.default_value {
            facts.push(format!("{}: {value}", self.words.default));
        }
        if let Some(reason) = &self.locked {
            facts.push(reason.to_string());
        }
        if self.modified && self.locked.is_none() {
            facts.push(format!("r {}", self.words.reset));
        }
        for fact in facts {
            lines.extend(wrap_spans(
                &[Span::styled(
                    text::display_safe(&fact).into_owned(),
                    theme.fg(Role::Muted),
                )],
                width,
            ));
        }
        lines
    }
}
impl Paint for SettingDetail<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        Paragraph::new(self.lines(area.width, theme)).render(area, buf);
    }
    fn height(&self, width: u16, theme: &Theme) -> u16 {
        u16::try_from(self.lines(width, theme).len()).unwrap_or(u16::MAX)
    }
}

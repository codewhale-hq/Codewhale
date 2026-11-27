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
    /// Whether to show the default `applies_now` fact on unmodified, unchanged
    /// rows when no explicit `apply` text is set (default `true`).
    pub show_default_apply: bool,
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
            show_default_apply: true,
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
    /// Control whether unmodified, unchanged rows display the default
    /// `applies_now` fact (default `true`). Setting `false` suppresses it
    /// unless an explicit `apply` fact is provided or the row is modified or
    /// changed.
    #[must_use]
    pub fn show_default_apply(mut self, show: bool) -> Self {
        self.show_default_apply = show;
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
        let apply = self.apply.as_deref().or_else(|| {
            (self.show_default_apply || self.modified || self.changed)
                .then_some(self.words.applies_now.as_ref())
        });
        for fact in [
            self.source.as_deref(),
            apply,
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

/// One section entry in a [`SettingsRail`], pairing a display label with its
/// current item or search-match count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsSection<'a> {
    /// Section title shown in the rail.
    pub label: Cow<'a, str>,
    /// Number of settings in this section (or matching settings while searching).
    pub count: usize,
}

impl<'a> SettingsSection<'a> {
    /// Create a section entry with a `label` and `count`.
    #[must_use]
    pub fn new(label: impl Into<Cow<'a, str>>, count: usize) -> Self {
        Self {
            label: label.into(),
            count,
        }
    }
}

/// Count how many matching items fall into each section index in
/// `0..section_count`. Out-of-range indices in `item_sections` are ignored.
#[must_use]
pub fn section_match_counts(section_count: usize, item_sections: &[usize]) -> Vec<usize> {
    let mut counts = vec![0; section_count];
    for &section in item_sections {
        if let Some(slot) = counts.get_mut(section) {
            *slot += 1;
        }
    }
    counts
}

/// Vertical section navigation rail for Settings, displaying per-section match
/// counts, dimming zero-match sections while searching, and showing an
/// `Esc clear` cue when a search filter is active.
#[derive(Clone, Debug)]
pub struct SettingsRail<'a> {
    sections: &'a [SettingsSection<'a>],
    active: usize,
    searching: bool,
    clear_hint: Cow<'a, str>,
}

impl<'a> SettingsRail<'a> {
    /// Create a settings rail for `sections` with `active` selected.
    #[must_use]
    pub fn new(sections: &'a [SettingsSection<'a>], active: usize) -> Self {
        Self {
            sections,
            active,
            searching: false,
            clear_hint: Cow::Borrowed("Esc clear"),
        }
    }

    /// Mark whether a search query is active. While searching, inactive
    /// zero-match sections recede and the clear cue is shown when height
    /// permits.
    #[must_use]
    pub const fn searching(mut self, searching: bool) -> Self {
        self.searching = searching;
        self
    }

    /// Override the search-clear cue (default `"Esc clear"`). Pass an empty
    /// string to hide it.
    #[must_use]
    pub fn clear_hint(mut self, hint: impl Into<Cow<'a, str>>) -> Self {
        self.clear_hint = hint.into();
        self
    }
}

impl Paint for SettingsRail<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let ascii = theme.ascii();
        let width = usize::from(area.width);
        let clear_text = self
            .searching
            .then(|| text::display_safe(&self.clear_hint).into_owned())
            .filter(|s| !s.is_empty());
        let show_clear = clear_text.is_some() && (area.height >= 2 || self.sections.is_empty());
        let list_rows = usize::from(area.height.saturating_sub(u16::from(show_clear)));

        if !self.sections.is_empty() && list_rows > 0 {
            let active = self.active.min(self.sections.len() - 1);
            let start = if active >= list_rows {
                active + 1 - list_rows
            } else {
                0
            };
            let end = (start + list_rows).min(self.sections.len());
            for (row_idx, section_idx) in (start..end).enumerate() {
                let section = &self.sections[section_idx];
                let is_active = section_idx == active;
                let dimmed = !is_active && self.searching && section.count == 0;
                let y = area.y + row_idx as u16;
                let row_rect = Rect {
                    y,
                    height: 1,
                    ..area
                };
                if is_active {
                    buf.set_style(row_rect, theme.bg(Role::Selected));
                }

                let marker = glyphs::pick(glyphs::selection_marker(is_active), ascii);
                let marker_w = text::width(marker) + 1;
                let count_str = section.count.to_string();
                let count_w = text::width(&count_str);
                let show_count = width >= marker_w + count_w + 2;
                let label_budget = if show_count {
                    width.saturating_sub(marker_w + count_w + 1)
                } else {
                    width.saturating_sub(marker_w)
                };

                let label_style = if is_active {
                    theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
                } else if dimmed {
                    theme.fg(Role::Muted).add_modifier(Modifier::DIM)
                } else {
                    theme.fg(Role::Foreground)
                };
                let count_style = if is_active {
                    theme.fg(Role::Foreground)
                } else if dimmed {
                    theme.fg(Role::Muted).add_modifier(Modifier::DIM)
                } else {
                    theme.fg(Role::Muted)
                };
                let marker_style = if is_active {
                    theme.fg(Role::Accent)
                } else {
                    theme.fg(Role::Muted)
                };

                let safe_label = text::display_safe(&section.label);
                let mut spans = Vec::new();
                if width >= marker_w {
                    spans.push(Span::styled(format!("{marker} "), marker_style));
                    if label_budget > 0 {
                        spans.push(Span::styled(
                            text::truncate(&safe_label, label_budget, ascii).into_owned(),
                            label_style,
                        ));
                    }
                } else {
                    spans.push(Span::styled(
                        text::truncate(&safe_label, width, ascii).into_owned(),
                        label_style,
                    ));
                }
                Line::from(spans).render(row_rect, buf);

                if show_count {
                    let cx = area
                        .right()
                        .saturating_sub(u16::try_from(count_w).unwrap_or(0));
                    buf.set_stringn(cx, y, &count_str, count_w, count_style);
                }
            }
        }

        if show_clear && let Some(clear) = clear_text {
            let y = area.bottom() - 1;
            let text = text::truncate(&clear, width, ascii);
            buf.set_stringn(area.x, y, text.as_ref(), width, theme.fg(Role::Hint));
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        let cue = u16::from(self.searching && !text::display_safe(&self.clear_hint).is_empty());
        u16::try_from(self.sections.len())
            .unwrap_or(u16::MAX)
            .saturating_add(cue)
    }
}

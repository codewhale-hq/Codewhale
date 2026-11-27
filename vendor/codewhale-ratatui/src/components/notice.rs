//! Calm semantic notices: a mark, a title, optional body, and optional actions.

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Widget,
};

use crate::{KeyHint, Paint, Role, Theme, glyphs, text};

/// Semantic tone of a [`Notice`]. Each tone pairs a distinct 1-cell mark with
/// a semantic [`Role`] so meaning never relies on color alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoticeTone {
    /// Neutral informational banner.
    Info,
    /// Caution or attention required.
    Warning,
    /// Confirmed positive outcome.
    Success,
    /// Failure or blocked state.
    Error,
}

impl NoticeTone {
    /// Every notice tone in canonical order.
    pub const ALL: [NoticeTone; 4] = [
        NoticeTone::Info,
        NoticeTone::Warning,
        NoticeTone::Success,
        NoticeTone::Error,
    ];

    /// Semantic theme role for this tone.
    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            NoticeTone::Info => Role::Accent,
            NoticeTone::Warning => Role::Attention,
            NoticeTone::Success => Role::Live,
            NoticeTone::Error => Role::Danger,
        }
    }

    /// 1-cell Unicode glyph for this tone.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            NoticeTone::Info => glyphs::CURRENT,
            NoticeTone::Warning => glyphs::ATTENTION,
            NoticeTone::Success => glyphs::DONE,
            NoticeTone::Error => glyphs::FAILED,
        }
    }

    /// Short English label for this tone.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            NoticeTone::Info => "Info",
            NoticeTone::Warning => "Warning",
            NoticeTone::Success => "Success",
            NoticeTone::Error => "Error",
        }
    }
}

/// Inline semantic notice banner matching Desktop `packages/ui` `<Notice>`.
///
/// Paints a calm tonal [`Role::Surface`] ground where terminal grounds differ
/// and a semantic left rail on every profile, ensuring clear separation on
/// Ansi16, `NO_COLOR`, and ASCII-safe terminals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice<'a> {
    /// Semantic tone determining icon and accent role.
    pub tone: NoticeTone,
    /// Primary single-line title.
    pub title: Cow<'a, str>,
    /// Optional supporting prose body.
    pub body: Option<Cow<'a, str>>,
    /// Optional primary action hint.
    pub action: Option<KeyHint>,
    /// Optional dismiss cue (e.g. `Esc dismiss`).
    pub dismiss: Option<KeyHint>,
}

impl<'a> Notice<'a> {
    /// Create a notice with the given `tone` and `title`.
    #[must_use]
    pub fn new(tone: NoticeTone, title: impl Into<Cow<'a, str>>) -> Self {
        Self {
            tone,
            title: title.into(),
            body: None,
            action: None,
            dismiss: None,
        }
    }

    /// Create an informational notice.
    #[must_use]
    pub fn info(title: impl Into<Cow<'a, str>>) -> Self {
        Self::new(NoticeTone::Info, title)
    }

    /// Create a warning notice.
    #[must_use]
    pub fn warning(title: impl Into<Cow<'a, str>>) -> Self {
        Self::new(NoticeTone::Warning, title)
    }

    /// Create a success notice.
    #[must_use]
    pub fn success(title: impl Into<Cow<'a, str>>) -> Self {
        Self::new(NoticeTone::Success, title)
    }

    /// Create an error notice.
    #[must_use]
    pub fn error(title: impl Into<Cow<'a, str>>) -> Self {
        Self::new(NoticeTone::Error, title)
    }

    /// Attach optional supporting body copy.
    #[must_use]
    pub fn body(mut self, body: impl Into<Cow<'a, str>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Attach an optional action key hint.
    #[must_use]
    pub fn action(mut self, action: KeyHint) -> Self {
        self.action = Some(action);
        self
    }

    /// Toggle a default `Esc dismiss` cue.
    #[must_use]
    pub fn dismissable(mut self, dismissable: bool) -> Self {
        self.dismiss = dismissable.then(|| KeyHint::new("Esc", "dismiss"));
        self
    }

    /// Attach a custom or localized dismiss cue.
    #[must_use]
    pub fn dismiss_hint(mut self, dismiss: KeyHint) -> Self {
        self.dismiss = Some(dismiss);
        self
    }

    fn has_footer(&self) -> bool {
        self.action.is_some() || self.dismiss.is_some()
    }

    fn body_lines(&self, content_width: usize) -> Vec<String> {
        let Some(body) = self.body.as_deref() else {
            return Vec::new();
        };
        if content_width == 0 {
            return Vec::new();
        }
        let safe = text::display_safe(body);
        wrap_words(&safe, content_width)
    }

    fn content_width(width: u16) -> usize {
        usize::from(width).saturating_sub(2)
    }
}

impl Paint for Notice<'_> {
    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        if width == 0 {
            return 0;
        }
        let content_width = Self::content_width(width);
        let body_rows = self.body_lines(content_width).len();
        let footer_rows = usize::from(self.has_footer());
        u16::try_from(1 + body_rows + footer_rows).unwrap_or(u16::MAX)
    }

    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }

        let ascii = theme.ascii();
        let tonal = theme.grounds_differ(Role::Surface, Role::Background);
        let surface_bg = if tonal {
            theme.bg(Role::Surface)
        } else {
            ratatui::style::Style::default()
        };

        if tonal {
            for y in area.y..area.y.saturating_add(area.height) {
                for x in area.x..area.x.saturating_add(area.width) {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_style(surface_bg);
                    }
                }
            }
        }

        let rail = glyphs::pick("▏", ascii);
        let rail_style = theme.fg(self.tone.role()).patch(surface_bg);
        let mark = glyphs::pick(self.tone.glyph(), ascii);
        let mark_style = theme.fg(self.tone.role()).patch(surface_bg);
        let title_style = theme
            .fg(Role::Foreground)
            .add_modifier(Modifier::BOLD)
            .patch(surface_bg);
        let body_style = theme.fg(Role::Muted).patch(surface_bg);

        let content_width = Self::content_width(area.width);
        let body_lines = self.body_lines(content_width);
        let total_rows = area.height;

        let mut row = 0u16;
        if row < total_rows {
            let y = area.y + row;
            if area.width <= 2 {
                let spans = if area.width == 1 {
                    vec![Span::styled(mark, mark_style)]
                } else {
                    vec![
                        Span::styled(rail, rail_style),
                        Span::styled(mark, mark_style),
                    ]
                };
                Line::from(spans).render(
                    Rect {
                        x: area.x,
                        y,
                        width: area.width,
                        height: 1,
                    },
                    buf,
                );
            } else {
                let mut spans = vec![
                    Span::styled(rail, rail_style),
                    Span::styled(" ", surface_bg),
                ];
                if content_width > 0 {
                    spans.push(Span::styled(mark, mark_style));
                    let title_budget = content_width.saturating_sub(text::width(mark) + 1);
                    if title_budget > 0 {
                        let safe_title = text::display_safe(&self.title);
                        let clipped = text::truncate_words(&safe_title, title_budget, ascii);
                        spans.push(Span::styled(" ", surface_bg));
                        spans.push(Span::styled(clipped.into_owned(), title_style));
                    }
                }
                Line::from(spans).render(
                    Rect {
                        x: area.x,
                        y,
                        width: area.width,
                        height: 1,
                    },
                    buf,
                );
            }
            row += 1;
        }

        let reserved_footer = u16::from(self.has_footer() && total_rows > 1);
        let body_limit = total_rows.saturating_sub(row + reserved_footer);
        for line in body_lines.into_iter().take(usize::from(body_limit)) {
            let y = area.y + row;
            let clipped = text::truncate_words(&line, content_width, ascii);
            let spans = vec![
                Span::styled(rail, rail_style),
                Span::styled(" ", surface_bg),
                Span::styled(clipped.into_owned(), body_style),
            ];
            Line::from(spans).render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            row += 1;
        }

        if self.has_footer() && row < total_rows {
            let y = area.y + row;
            let sep = if ascii { "  " } else { " · " };
            let mut parts = Vec::new();
            if let Some(action) = &self.action {
                parts.push(format_hint(action));
            }
            if let Some(dismiss) = &self.dismiss {
                parts.push(format_hint(dismiss));
            }
            let joined = parts.join(sep);
            let clipped = text::truncate(&joined, content_width, ascii);
            let spans = vec![
                Span::styled(rail, rail_style),
                Span::styled(" ", surface_bg),
                Span::styled(clipped.into_owned(), body_style),
            ];
            Line::from(spans).render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        }
    }
}

fn format_hint(hint: &KeyHint) -> String {
    let keys = text::display_safe(&hint.keys);
    let verb = text::display_safe(&hint.verb);
    format!("{keys} {verb}")
}

fn wrap_words(input: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for raw_line in input.lines() {
        let mut current = String::new();
        let mut current_w = 0usize;
        for word in raw_line.split_whitespace() {
            let w = text::width(word);
            if current.is_empty() {
                current.push_str(word);
                current_w = w;
            } else if current_w + 1 + w <= max_width {
                current.push(' ');
                current.push_str(word);
                current_w += 1 + w;
            } else {
                lines.push(current);
                current = word.to_owned();
                current_w = w;
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
    }
    lines
}

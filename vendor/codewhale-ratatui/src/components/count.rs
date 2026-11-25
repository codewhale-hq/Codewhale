//! Compact numeric count badges (`0`, `12`, `99+`).

use std::fmt;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::Widget,
};

use crate::{Paint, Role, Theme, text};

/// Compact numeric count badge matching Desktop `packages/ui` `<Count>`.
///
/// Formats values directly (`"0"`, `"12"`) or capped with a trailing `+`
/// (`Count::new(120).max(99)` → `"99+"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Count {
    /// Numeric value to display.
    pub value: usize,
    /// Optional inclusive maximum; larger values format as `"{cap}+"`.
    pub cap: Option<usize>,
    /// Theme role used to paint the badge.
    pub role: Role,
}

impl Count {
    /// Create a count badge for `value`.
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self {
            value,
            cap: None,
            role: Role::Muted,
        }
    }

    /// Cap display at `cap`, rendering larger counts as `"{cap}+"`.
    #[must_use]
    pub const fn max(mut self, cap: usize) -> Self {
        self.cap = Some(cap);
        self
    }

    /// Override the painted theme [`Role`] (defaults to [`Role::Muted`]).
    #[must_use]
    pub const fn role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }

    /// Formatted badge text.
    #[must_use]
    pub fn label(&self) -> String {
        self.to_string()
    }

    /// Display width in terminal cells.
    #[must_use]
    pub fn width(&self) -> usize {
        text::width(&self.label())
    }

    /// Build a styled [`Span`] for inline composition.
    #[must_use]
    pub fn span(&self, theme: &Theme) -> Span<'static> {
        Span::styled(self.label(), theme.fg(self.role))
    }
}

impl fmt::Display for Count {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.cap {
            Some(cap) if self.value > cap => write!(f, "{cap}+"),
            _ => write!(f, "{}", self.value),
        }
    }
}

impl Paint for Count {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let label = self.label();
        let clipped = text::truncate(&label, usize::from(area.width), theme.ascii());
        Line::from(Span::styled(clipped.into_owned(), theme.fg(self.role))).render(
            Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: 1,
            },
            buf,
        );
    }
}

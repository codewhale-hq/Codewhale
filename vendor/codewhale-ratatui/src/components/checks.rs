//! Semantic CI/check summary badges (`✓ Passing`, `✕ Failing (2)`).

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::{Line, Span},
    widgets::Widget,
};

use crate::{Paint, Role, Theme, glyphs, text};

/// Outcome of a set of automated checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChecksState {
    /// All checks succeeded.
    Passing,
    /// One or more checks failed.
    Failing,
    /// Checks are still running or queued.
    Pending,
    /// Checks were skipped or not configured.
    Skipped,
}

impl ChecksState {
    /// Every check state in canonical order.
    pub const ALL: [ChecksState; 4] = [
        ChecksState::Passing,
        ChecksState::Failing,
        ChecksState::Pending,
        ChecksState::Skipped,
    ];

    /// Semantic theme [`Role`] for this check state.
    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            ChecksState::Passing => Role::Live,
            ChecksState::Failing => Role::Danger,
            ChecksState::Pending => Role::Attention,
            ChecksState::Skipped => Role::Muted,
        }
    }

    /// Distinct 1-cell Unicode glyph for this check state.
    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            ChecksState::Passing => glyphs::DONE,
            ChecksState::Failing => glyphs::FAILED,
            ChecksState::Pending => glyphs::ATTENTION,
            ChecksState::Skipped => glyphs::READY,
        }
    }

    /// Default English word for this check state.
    #[must_use]
    pub const fn word(self) -> &'static str {
        ChecksWords::english(self)
    }
}

/// Localized words for [`ChecksMark`]. `Default` is English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecksWords {
    /// Word for [`ChecksState::Passing`].
    pub passing: Cow<'static, str>,
    /// Word for [`ChecksState::Failing`].
    pub failing: Cow<'static, str>,
    /// Word for [`ChecksState::Pending`].
    pub pending: Cow<'static, str>,
    /// Word for [`ChecksState::Skipped`].
    pub skipped: Cow<'static, str>,
}

impl ChecksWords {
    /// English default word for `state`.
    #[must_use]
    pub const fn english(state: ChecksState) -> &'static str {
        match state {
            ChecksState::Passing => "Passing",
            ChecksState::Failing => "Failing",
            ChecksState::Pending => "Pending",
            ChecksState::Skipped => "Skipped",
        }
    }

    /// Look up the word for `state`.
    #[must_use]
    pub fn get(&self, state: ChecksState) -> &str {
        match state {
            ChecksState::Passing => &self.passing,
            ChecksState::Failing => &self.failing,
            ChecksState::Pending => &self.pending,
            ChecksState::Skipped => &self.skipped,
        }
    }
}

impl Default for ChecksWords {
    fn default() -> Self {
        Self {
            passing: Cow::Borrowed(Self::english(ChecksState::Passing)),
            failing: Cow::Borrowed(Self::english(ChecksState::Failing)),
            pending: Cow::Borrowed(Self::english(ChecksState::Pending)),
            skipped: Cow::Borrowed(Self::english(ChecksState::Skipped)),
        }
    }
}

/// Semantic CI/check badge matching Desktop `packages/ui` `<ChecksMark>`.
///
/// Always pairs a distinct 1-cell glyph with a word (and optional count), so
/// check status never relies on color alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChecksMark {
    /// Semantic check outcome.
    pub state: ChecksState,
    /// Displayed label word.
    pub word: Cow<'static, str>,
    /// Optional check count shown beside the word (`(12)`).
    pub count: Option<usize>,
    /// Paint the word in the state's semantic role instead of [`Role::Foreground`].
    pub tinted: bool,
}

impl ChecksMark {
    /// Create a check badge with the default English word for `state`.
    #[must_use]
    pub fn new(state: ChecksState) -> Self {
        Self {
            state,
            word: Cow::Borrowed(state.word()),
            count: None,
            tinted: false,
        }
    }

    /// Create a check badge using localized [`ChecksWords`].
    #[must_use]
    pub fn with_words(state: ChecksState, words: &ChecksWords) -> Self {
        Self {
            state,
            word: Cow::Owned(words.get(state).to_owned()),
            count: None,
            tinted: false,
        }
    }

    /// Override the displayed word.
    #[must_use]
    pub fn word(mut self, word: impl Into<Cow<'static, str>>) -> Self {
        self.word = word.into();
        self
    }

    /// Attach an optional check count.
    #[must_use]
    pub const fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// Tint the word with the state's semantic role.
    #[must_use]
    pub const fn tinted(mut self, tinted: bool) -> Self {
        self.tinted = tinted;
        self
    }

    /// Total display width in cells when rendered without truncation.
    #[must_use]
    pub fn width(&self, ascii: bool) -> usize {
        let mark_w = text::width(glyphs::pick(self.state.glyph(), ascii));
        let word_w = text::width(&text::display_safe(&self.word));
        let count_w = self.count.map_or(0, |n| text::width(&format!(" ({n})")));
        mark_w + 1 + word_w + count_w
    }

    /// Styled spans for inline composition.
    #[must_use]
    pub fn spans(&self, theme: &Theme) -> Vec<Span<'static>> {
        let mark = glyphs::pick(self.state.glyph(), theme.ascii());
        let word = text::display_safe(&self.word).into_owned();
        let word_role = if self.tinted {
            self.state.role()
        } else {
            Role::Foreground
        };
        let mut spans = vec![
            Span::styled(mark, theme.fg(self.state.role())),
            Span::raw(" "),
            Span::styled(word, theme.fg(word_role)),
        ];
        if let Some(count) = self.count {
            spans.push(Span::styled(format!(" ({count})"), theme.fg(Role::Muted)));
        }
        spans
    }
}

impl Paint for ChecksMark {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let ascii = theme.ascii();
        let mark = glyphs::pick(self.state.glyph(), ascii);
        let mark_w = text::width(mark);
        let max_w = usize::from(area.width);
        let row_rect = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };

        if max_w <= mark_w + 1 {
            Line::from(Span::styled(mark, theme.fg(self.state.role()))).render(row_rect, buf);
            return;
        }

        let word_role = if self.tinted {
            self.state.role()
        } else {
            Role::Foreground
        };
        let rest_budget = max_w.saturating_sub(mark_w + 1);
        let safe_word = text::display_safe(&self.word);
        let suffix = self.count.map(|n| format!(" ({n})"));
        let full_rest = match &suffix {
            Some(s) => format!("{safe_word}{s}"),
            None => safe_word.into_owned(),
        };

        let spans = if text::width(&full_rest) <= rest_budget {
            self.spans(theme)
        } else {
            let clipped = text::truncate(&full_rest, rest_budget, ascii);
            vec![
                Span::styled(mark, theme.fg(self.state.role())),
                Span::raw(" "),
                Span::styled(clipped.into_owned(), theme.fg(word_role)),
            ]
        };

        Line::from(spans).render(row_rect, buf);
    }
}

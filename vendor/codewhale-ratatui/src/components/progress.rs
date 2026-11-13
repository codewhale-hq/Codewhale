//! Progress from reported counts, never from an animation or elapsed time.
//!
//! [`CountBar`] says how many of how many in words (`3 of 5 phases`), because
//! the count is the fact. A bar beside it is optional ink and is drawn only
//! when the total is known:
//!
//! - no percentage, no time-remaining estimate, no smoothing: the bar fills
//!   in eighth cells (whole cells in ASCII), never reaches full before
//!   `done` reaches `total`, and never shows work that has not happened;
//! - when `done` passes `total` the words keep the real numbers (`7 of 5`);
//!   only the painted fill is clamped;
//! - when the total is unknown there is no bar at all, and the words say so
//!   (`12 files so far, total unknown`) instead of guessing a fraction.
//!
//! The state (and so the mark and word) comes from the host; the bar never
//! infers "done" from the counts.

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    symbols::block,
    text::{Line, Span},
    widgets::Widget,
};

use crate::{Paint, Role, State, StateWords, StatusMark, Theme, glyphs, text};

/// Words a count bar prints. `Default` is English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountBarWords {
    /// The word beside the mark.
    pub states: StateWords,
    /// `12 files so far, total unknown`: what follows the count.
    pub unknown_total: Cow<'static, str>,
    /// `3 of 5 phases`.
    pub of: Cow<'static, str>,
    /// `12 files so far`.
    pub so_far: Cow<'static, str>,
}

impl Default for CountBarWords {
    fn default() -> Self {
        Self {
            states: StateWords::default(),
            unknown_total: Cow::Borrowed("total unknown"),
            of: Cow::Borrowed("of"),
            so_far: Cow::Borrowed("so far"),
        }
    }
}

/// `● Working  3 of 5 phases  [██████░░░░░░]`.
///
/// The host reports the state and the exact counts. Only the painted
/// fraction is clamped: `12 of 10` stays `12 of 10`. A zero total paints no
/// completed ink; an unknown total has no bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountBar {
    pub done: u64,
    /// `None` when nobody knows how many there will be.
    pub total: Option<u64>,
    pub state: State,
    /// What is being counted: `phases`, `files`.
    pub label: Cow<'static, str>,
    pub words: CountBarWords,
}

impl CountBar {
    /// `done` of `total`, working.
    #[must_use]
    pub fn new(done: u64, total: u64) -> Self {
        Self {
            done,
            total: Some(total),
            state: State::Working,
            label: Cow::Borrowed(""),
            words: CountBarWords::default(),
        }
    }

    /// `done` so far, with no known total: the indeterminate form.
    #[must_use]
    pub fn unknown(done: u64) -> Self {
        Self {
            total: None,
            state: State::Unknown,
            ..Self::new(done, 0)
        }
    }

    #[must_use]
    pub fn state(mut self, state: State) -> Self {
        self.state = state;
        self
    }

    #[must_use]
    pub fn label(mut self, label: impl Into<Cow<'static, str>>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn with_words(mut self, words: &CountBarWords) -> Self {
        self.words = words.clone();
        self
    }

    /// How many bar cells are filled out of `cells`: whole cells, rounded
    /// down, and never all of them before the work is complete.
    #[must_use]
    pub fn filled(&self, cells: u16) -> u16 {
        let Some(total) = self.total else { return 0 };
        if total == 0 || cells == 0 {
            return 0;
        }
        if self.done >= total {
            return cells;
        }
        let part = u128::from(self.done) * u128::from(cells) / u128::from(total);
        u16::try_from(part).unwrap_or(cells).min(cells - 1)
    }

    /// The words: `3 of 5 phases`, or `12 files so far, total unknown`.
    fn counting(&self) -> String {
        let label = text::display_safe(&self.label);
        let unit = if label.is_empty() {
            String::new()
        } else {
            format!(" {label}")
        };
        match self.total {
            Some(total) => format!(
                "{} {} {total}{unit}",
                self.done,
                text::display_safe(&self.words.of)
            ),
            None => {
                let sep = if unit.is_empty() { "" } else { " " };
                format!(
                    "{}{unit}{sep}{}, {}",
                    self.done,
                    text::display_safe(&self.words.so_far),
                    text::display_safe(&self.words.unknown_total)
                )
            }
        }
    }
}

/// The bar never grows past this many cells; a longer one says nothing more.
const MAX_BAR: usize = 24;
/// Narrower than this the bar is left out: the words carry the count.
const MIN_BAR: usize = 6;

impl Paint for CountBar {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let width = usize::from(area.width);
        let ascii = theme.ascii();
        let mark = StatusMark::with_words(self.state, &self.words.states);
        let glyph = mark.glyph(theme);
        let word = text::display_safe(&mark.word).into_owned();
        let counting = self.counting();
        // When the row is too narrow the state word goes first (the mark
        // stays), then the end of the count's words; never the numbers.
        let with_word = text::width(glyph) + 1 + text::width(&word) + 2;
        let without_word = text::width(glyph) + 1;
        let counting_w = text::width(&counting);
        let show_word = with_word + counting_w <= width;
        let lead_w = if show_word { with_word } else { without_word };

        let mut spans = vec![Span::styled(glyph, theme.fg(self.state.role()))];
        if show_word {
            spans.push(Span::styled(
                format!(" {word}  "),
                theme.fg(Role::Foreground),
            ));
        } else {
            spans.push(Span::raw(" "));
        }
        let room = width.saturating_sub(lead_w);
        let fits = counting_w <= room;
        let shown = if fits {
            counting
        } else {
            text::truncate(&counting, room, ascii).into_owned()
        };
        spans.push(Span::styled(shown, theme.fg(Role::Foreground)));

        if self.total.is_some() && fits {
            let cells = (room - counting_w).saturating_sub(4).min(MAX_BAR);
            if cells >= MIN_BAR {
                let filled = usize::from(self.filled(cells as u16));
                let fraction = match self.total {
                    Some(total) if !ascii && total > 0 && self.done < total => {
                        ((u128::from(self.done) * cells as u128 * 8 / u128::from(total)) % 8)
                            as usize
                    }
                    _ => 0,
                };
                spans.push(Span::raw("  "));
                spans.push(Span::styled("[", theme.fg(Role::Muted)));
                spans.push(Span::styled(
                    glyphs::pick("\u{2588}", ascii).repeat(filled),
                    theme.fg(self.state.role()),
                ));
                if fraction > 0 {
                    let edge = [
                        "",
                        block::ONE_EIGHTH,
                        block::ONE_QUARTER,
                        block::THREE_EIGHTHS,
                        block::HALF,
                        block::FIVE_EIGHTHS,
                        block::THREE_QUARTERS,
                        block::SEVEN_EIGHTHS,
                    ][fraction];
                    spans.push(Span::styled(edge, theme.fg(self.state.role())));
                }
                spans.push(Span::styled(
                    glyphs::pick("\u{2591}", ascii)
                        .repeat(cells - filled - usize::from(fraction > 0)),
                    theme.fg(Role::Dim),
                ));
                spans.push(Span::styled("]", theme.fg(Role::Muted)));
            }
        }
        Line::from(spans).render(Rect::new(area.x, area.y, area.width, 1), buf);
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        1
    }
}

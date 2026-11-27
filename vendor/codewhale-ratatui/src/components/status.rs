//! Status marks: a glyph and a word, never color alone.
//!
//! Replaces the engine's `menu_style::StatusMark` (`crates/tui/src/tui/
//! menu_style.rs`, `Hmbown/CodeWhale` `58b1dd3dd`). The constructor takes a
//! [`State`], and the word comes with it, so there is no way to build a
//! colored dot that means nothing without its color.

use std::borrow::Cow;

use ratatui::{buffer::Buffer, layout::Rect, text::Span, widgets::Widget};

use crate::{Paint, Role, Theme, glyphs, text};

/// Product state. One hue, one mark and one word each, on every surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Working,
    Done,
    NeedsYou,
    Failed,
    Stopped,
    Ready,
    /// Nobody reported a state. Shown as unknown, never as a failure.
    Unknown,
    /// Blocked on a hard precondition or external gate.
    Blocked,
    /// Resting calmly between active runs.
    Resting,
    /// Asleep or suspended.
    Asleep,
}

impl State {
    pub const ALL: [State; 10] = [
        State::Working,
        State::Done,
        State::NeedsYou,
        State::Failed,
        State::Stopped,
        State::Ready,
        State::Unknown,
        State::Blocked,
        State::Resting,
        State::Asleep,
    ];

    #[must_use]
    pub const fn role(self) -> Role {
        match self {
            State::Working | State::Done => Role::Live,
            State::NeedsYou => Role::Attention,
            State::Failed | State::Blocked => Role::Danger,
            State::Stopped | State::Ready | State::Unknown | State::Resting | State::Asleep => {
                Role::Muted
            }
        }
    }

    #[must_use]
    pub const fn glyph(self) -> &'static str {
        match self {
            State::Working => glyphs::CURRENT,
            State::Done => glyphs::DONE,
            State::NeedsYou => glyphs::ATTENTION,
            State::Failed => glyphs::FAILED,
            State::Stopped => glyphs::STOPPED,
            State::Ready => glyphs::READY,
            State::Unknown => glyphs::UNKNOWN,
            State::Blocked => glyphs::BLOCKED,
            State::Resting => glyphs::RESTING,
            State::Asleep => glyphs::ASLEEP,
        }
    }

    /// The English default word, from [`StateWords::english`]. Hosts pass
    /// their own words with [`StatusMark::with_words`] or
    /// [`StatusMark::word`].
    #[must_use]
    pub const fn word(self) -> &'static str {
        StateWords::english(self)
    }
}

/// The word shown beside each [`State`]'s mark. The kit owns no copy beyond
/// this default: a host fills one per locale and passes it in. `Default` is
/// English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateWords {
    pub working: Cow<'static, str>,
    pub done: Cow<'static, str>,
    pub needs_you: Cow<'static, str>,
    pub failed: Cow<'static, str>,
    pub stopped: Cow<'static, str>,
    pub ready: Cow<'static, str>,
    pub unknown: Cow<'static, str>,
}

impl StateWords {
    /// The English word for `state`: the only copy in the kit's status
    /// marks, and what [`StateWords::default`] holds.
    #[must_use]
    pub const fn english(state: State) -> &'static str {
        match state {
            State::Working => "Working",
            State::Done => "Done",
            State::NeedsYou => "Needs you",
            State::Failed => "Failed",
            State::Stopped => "Stopped",
            State::Ready => "Ready",
            State::Unknown => "Unknown",
            State::Blocked => "Blocked",
            State::Resting => "Resting",
            State::Asleep => "Asleep",
        }
    }

    /// The word for `state`.
    #[must_use]
    pub fn get(&self, state: State) -> &str {
        match state {
            State::Working => &self.working,
            State::Done => &self.done,
            State::NeedsYou => &self.needs_you,
            State::Failed => &self.failed,
            State::Stopped => &self.stopped,
            State::Ready => &self.ready,
            State::Unknown => &self.unknown,
            State::Blocked | State::Resting | State::Asleep => Self::english(state),
        }
    }
}

impl Default for StateWords {
    fn default() -> Self {
        let english = |state| Cow::Borrowed(Self::english(state));
        Self {
            working: english(State::Working),
            done: english(State::Done),
            needs_you: english(State::NeedsYou),
            failed: english(State::Failed),
            stopped: english(State::Stopped),
            ready: english(State::Ready),
            unknown: english(State::Unknown),
        }
    }
}

/// `● Working`, `✓ Done`, `◆ Needs you`: the mark takes the state's hue; the
/// word stays `Foreground` so it reads at full contrast.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusMark {
    pub state: State,
    pub word: Cow<'static, str>,
    /// Optional quiet detail clause rendered after `word` as `word · detail`.
    pub detail: Option<Cow<'static, str>>,
    /// Paint the word in the state's hue too, for a mark that stands alone.
    pub tinted: bool,
}

impl StatusMark {
    #[must_use]
    pub fn new(state: State) -> Self {
        Self {
            state,
            word: Cow::Borrowed(state.word()),
            detail: None,
            tinted: false,
        }
    }

    /// A mark whose word comes from the host's [`StateWords`].
    #[must_use]
    pub fn with_words(state: State, words: &StateWords) -> Self {
        Self {
            state,
            word: Cow::Owned(words.get(state).to_owned()),
            detail: None,
            tinted: false,
        }
    }

    #[must_use]
    pub fn word(mut self, word: impl Into<Cow<'static, str>>) -> Self {
        self.word = word.into();
        self
    }

    /// Attach a quiet detail clause rendered as `word · detail`.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<Cow<'static, str>>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn tinted(mut self) -> Self {
        self.tinted = true;
        self
    }

    /// The mark's glyph for this terminal.
    #[must_use]
    pub fn glyph(&self, theme: &Theme) -> &'static str {
        glyphs::pick(self.state.glyph(), theme.ascii())
    }

    #[must_use]
    pub fn spans(&self, theme: &Theme) -> Vec<Span<'static>> {
        self.spans_for_width(theme, None)
    }

    fn spans_for_width(&self, theme: &Theme, max_width: Option<usize>) -> Vec<Span<'static>> {
        let hue = theme.fg(self.state.role());
        let word_style = if self.tinted {
            hue
        } else {
            theme.fg(Role::Foreground)
        };
        let glyph = self.glyph(theme);
        let safe_word = text::display_safe(&self.word).into_owned();
        let mut spans = vec![
            Span::styled(glyph, hue),
            Span::raw(" "),
            Span::styled(safe_word.clone(), word_style),
        ];
        if let Some(raw_detail) = self.detail.as_deref() {
            let safe_detail = text::display_safe(raw_detail);
            let trimmed = safe_detail.trim();
            if !trimmed.is_empty() {
                let sep = format!(" {} ", glyphs::pick(glyphs::NEUTRAL, theme.ascii()));
                if let Some(limit) = max_width {
                    let prefix_width = text::width(glyph) + 1 + text::width(&safe_word);
                    let sep_width = text::width(&sep);
                    if let Some(avail) = limit.checked_sub(prefix_width + sep_width)
                        && avail > 0
                    {
                        let fitted = text::truncate_words(trimmed, avail, theme.ascii());
                        if !fitted.is_empty() {
                            spans.push(Span::styled(sep, theme.fg(Role::Muted)));
                            spans.push(Span::styled(fitted.into_owned(), theme.fg(Role::Muted)));
                        }
                    }
                } else {
                    spans.push(Span::styled(sep, theme.fg(Role::Muted)));
                    spans.push(Span::styled(trimmed.to_owned(), theme.fg(Role::Muted)));
                }
            }
        }
        spans
    }
}

impl Paint for StatusMark {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let max_width = self.detail.as_ref().map(|_| usize::from(area.width));
        ratatui::text::Line::from(self.spans_for_width(theme, max_width)).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Profile;

    #[test]
    fn every_state_has_a_distinct_mark_and_a_word() {
        for profile in [Profile::DarkTrue, Profile::Ascii] {
            let theme = profile.theme();
            let marks: Vec<_> = State::ALL
                .iter()
                .map(|s| StatusMark::new(*s).glyph(&theme))
                .collect();
            for (i, a) in marks.iter().enumerate() {
                assert!(!a.trim().is_empty());
                for b in &marks[i + 1..] {
                    assert_ne!(a, b, "{profile:?}: two states share {a}");
                }
            }
        }
        for state in State::ALL {
            assert!(!state.word().is_empty());
        }
    }

    /// The default words are the English ones, and a host's words reach the
    /// painted mark.
    #[test]
    fn words_are_parameters_with_english_defaults() {
        let english = StateWords::default();
        for state in State::ALL {
            assert_eq!(english.get(state), state.word());
            assert_eq!(
                StatusMark::new(state),
                StatusMark::with_words(state, &english)
            );
        }
        let host = StateWords {
            needs_you: "Braucht dich".into(),
            ..StateWords::default()
        };
        let theme = Profile::DarkTrue.theme();
        let buf = crate::testing::render(20, 1, |area, buf| {
            StatusMark::with_words(State::NeedsYou, &host).paint(area, buf, &theme);
        });
        assert_eq!(crate::testing::text(&buf), "\u{25c6} Braucht dich");
        assert_eq!(host.get(State::Done), "Done");
    }

    #[test]
    fn detail_renders_and_truncates_safely_in_unicode_and_ascii() {
        let dark = Profile::DarkTrue.theme();
        let ascii = Profile::Ascii.theme();
        let mark = StatusMark::new(State::Blocked).detail("waiting on CI approval");

        let wide = crate::testing::render(40, 1, |area, buf| mark.paint(area, buf, &dark));
        assert_eq!(
            crate::testing::text(&wide),
            "\u{2298} Blocked \u{00b7} waiting on CI approval"
        );

        let narrow = crate::testing::render(22, 1, |area, buf| mark.paint(area, buf, &dark));
        let narrow_text = crate::testing::text(&narrow);
        assert!(
            narrow_text.starts_with("\u{2298} Blocked \u{00b7} "),
            "{narrow_text}"
        );
        assert!(narrow_text.contains('\u{2026}'), "{narrow_text}");

        let ascii_buf = crate::testing::render(22, 1, |area, buf| mark.paint(area, buf, &ascii));
        let ascii_text = crate::testing::text(&ascii_buf);
        assert!(ascii_text.is_ascii(), "{ascii_text}");
        assert!(ascii_text.starts_with("! Blocked . "), "{ascii_text}");

        let tiny = crate::testing::render(9, 1, |area, buf| mark.paint(area, buf, &dark));
        assert_eq!(crate::testing::text(&tiny), "\u{2298} Blocked");
    }
}

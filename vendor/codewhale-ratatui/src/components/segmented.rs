//! Segmented: two to five mutually exclusive options in one row.
//!
//! ```text
//!  Full  [Reduced]  Still
//! ```
//!
//! The selected option is bracketed (`[Reduced]`), bold and underlined; the
//! others are `Muted`. The brackets are the cue that survives 16 colors,
//! `NO_COLOR` and ASCII; the focused control lights them in `Primary`. When
//! the options do not fit the row, it degrades to the current one and its
//! position: `Reduced (2 of 3)`. Past five options use a [`crate::Picker`].

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::{Paint, Role, Theme, text};

/// The most options a [`Segmented`] shows; any beyond are ignored.
pub const SEGMENTED_MAX: usize = 5;

/// The word in the compact form `Reduced (2 of 3)`. The kit owns no copy
/// beyond this English default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentedWords {
    pub of: Cow<'static, str>,
}

impl Default for SegmentedWords {
    fn default() -> Self {
        Self {
            of: Cow::Borrowed("of"),
        }
    }
}

/// Which option is selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentedState {
    pub selected: usize,
}

/// What a key did to a [`SegmentedState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentedOutcome {
    /// The key means nothing here, or changed nothing (an arrow at the end,
    /// a number already selected); the host may use it.
    Ignored,
    /// Option `n` is now selected; repaint, and preview it if it previews.
    Selected(usize),
}

impl SegmentedState {
    #[must_use]
    pub const fn new(selected: usize) -> Self {
        Self { selected }
    }

    /// Apply a key press to a control of `len` options. Keys: `←`/`→` (they
    /// stop at the ends, so a host can use the arrow there), `Home`, `End`
    /// and `1`..`5`. Releases, chords with Ctrl, Alt or Super, other keys
    /// and every key while `enabled` is false are
    /// [`SegmentedOutcome::Ignored`].
    pub fn handle_key(&mut self, key: KeyEvent, len: usize, enabled: bool) -> SegmentedOutcome {
        let len = len.min(SEGMENTED_MAX);
        let held = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER;
        if !enabled
            || len == 0
            || key.kind == KeyEventKind::Release
            || key.modifiers.intersects(held)
        {
            return SegmentedOutcome::Ignored;
        }
        let current = self.selected.min(len - 1);
        let next = match key.code {
            KeyCode::Left => current.saturating_sub(1),
            KeyCode::Right => (current + 1).min(len - 1),
            KeyCode::Home => 0,
            KeyCode::End => len - 1,
            KeyCode::Char(c @ '1'..='5') => usize::from(c as u8 - b'1'),
            _ => return SegmentedOutcome::Ignored,
        };
        if next >= len || next == current {
            return SegmentedOutcome::Ignored;
        }
        self.selected = next;
        SegmentedOutcome::Selected(next)
    }
}

/// A segmented control.
#[derive(Clone, Debug)]
pub struct Segmented<'a> {
    options: Vec<Cow<'a, str>>,
    pub state: SegmentedState,
    pub words: SegmentedWords,
    /// This control has the keyboard.
    pub focused: bool,
    /// `Some(reason)` dims the control and shows why it cannot change, when
    /// the row has room.
    pub disabled: Option<Cow<'a, str>>,
}

impl<'a> Segmented<'a> {
    /// A control over the first [`SEGMENTED_MAX`] of `options`.
    #[must_use]
    pub fn new<I, S>(options: I, state: SegmentedState) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Cow<'a, str>>,
    {
        Self {
            options: options
                .into_iter()
                .take(SEGMENTED_MAX)
                .map(Into::into)
                .collect(),
            state,
            words: SegmentedWords::default(),
            focused: false,
            disabled: None,
        }
    }

    #[must_use]
    pub fn with_words(mut self, words: &SegmentedWords) -> Self {
        self.words = words.clone();
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn disabled(mut self, reason: impl Into<Cow<'a, str>>) -> Self {
        self.disabled = Some(reason.into());
        self
    }

    #[must_use]
    pub fn options(&self) -> &[Cow<'a, str>] {
        &self.options
    }

    /// Apply a key to this control's state; a disabled control ignores
    /// every key.
    pub fn handle_key(&mut self, key: KeyEvent) -> SegmentedOutcome {
        self.state
            .handle_key(key, self.options.len(), self.disabled.is_none())
    }

    /// The cells the full form takes: each option with a cell of bracket or
    /// padding either side, and one between options.
    #[must_use]
    pub fn full_width(&self) -> usize {
        let labels: usize = self
            .options
            .iter()
            .map(|o| text::width(&text::display_safe(o)) + 2)
            .sum();
        labels + self.options.len().saturating_sub(1)
    }

    fn selected(&self) -> usize {
        self.state
            .selected
            .min(self.options.len().saturating_sub(1))
    }

    fn paint_full(&self, row: Rect, buf: &mut Buffer, theme: &Theme) {
        let disabled = self.disabled.is_some();
        let quiet = if disabled {
            theme.fg(Role::Muted).add_modifier(Modifier::DIM)
        } else {
            theme.fg(Role::Muted)
        };
        let bracket = if self.focused && !disabled {
            theme.fg(Role::Accent)
        } else {
            theme.fg(Role::Foreground)
        };
        let chosen = Style::default()
            .patch(theme.fg(Role::Foreground))
            .add_modifier(
                Modifier::BOLD
                    | if disabled {
                        Modifier::DIM
                    } else {
                        Modifier::UNDERLINED
                    },
            );
        let selected = self.selected();
        let mut x = row.x;
        for (i, option) in self.options.iter().enumerate() {
            let label = text::display_safe(option);
            let w = u16::try_from(text::width(&label)).unwrap_or(0);
            if i == selected {
                buf.set_stringn(x, row.y, "[", 1, bracket);
                buf.set_stringn(x + 1, row.y, &label, usize::from(w), chosen);
                buf.set_stringn(x + 1 + w, row.y, "]", 1, bracket);
            } else {
                buf.set_stringn(x + 1, row.y, &label, usize::from(w), quiet);
            }
            x += w + 3;
        }
        if let Some(reason) = &self.disabled {
            let room = usize::from(row.right().saturating_sub(x)).saturating_sub(1);
            if room >= 4 {
                let reason = text::display_safe(reason);
                let reason = text::truncate_words(&reason, room, theme.ascii());
                buf.set_stringn(x + 1, row.y, &reason, room, theme.fg(Role::Muted));
            }
        }
    }

    fn paint_compact(&self, row: Rect, buf: &mut Buffer, theme: &Theme) {
        let disabled = self.disabled.is_some();
        let current = text::display_safe(&self.options[self.selected()]);
        let suffix = format!(
            " ({} {} {})",
            self.selected() + 1,
            self.words.of,
            self.options.len()
        );
        let width = usize::from(row.width);
        let budget = width.saturating_sub(text::width(&suffix));
        let (label, suffix_x) = if budget >= 3 {
            let label = text::truncate(&current, budget, theme.ascii());
            let at = text::width(&label);
            (label, at)
        } else {
            (text::truncate(&current, width, theme.ascii()), width)
        };
        let label_style = theme.fg(Role::Foreground).add_modifier(if disabled {
            Modifier::BOLD | Modifier::DIM
        } else {
            Modifier::BOLD
        });
        buf.set_stringn(row.x, row.y, &label, width, label_style);
        if suffix_x < width {
            let x = row.x + u16::try_from(suffix_x).unwrap_or(0);
            buf.set_stringn(x, row.y, &suffix, width - suffix_x, theme.fg(Role::Muted));
        }
    }
}

impl Paint for Segmented<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() || self.options.is_empty() {
            return;
        }
        let row = Rect { height: 1, ..area };
        if self.full_width() <= usize::from(row.width) {
            self.paint_full(row, buf, theme);
        } else {
            self.paint_compact(row, buf, theme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Profile, render, text as rendered};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn line(control: &Segmented<'_>, profile: Profile, width: u16) -> String {
        let theme = profile.theme();
        rendered(&render(width, 1, |area, buf| {
            control.paint(area, buf, &theme)
        }))
    }

    #[test]
    fn arrows_stop_at_the_ends_and_numbers_jump() {
        let mut state = SegmentedState::new(0);
        assert_eq!(
            state.handle_key(key(KeyCode::Left), 3, true),
            SegmentedOutcome::Ignored
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Right), 3, true),
            SegmentedOutcome::Selected(1)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Right), 3, true),
            SegmentedOutcome::Selected(2)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Right), 3, true),
            SegmentedOutcome::Ignored
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Char('1')), 3, true),
            SegmentedOutcome::Selected(0)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Char('1')), 3, true),
            SegmentedOutcome::Ignored
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Char('4')), 3, true),
            SegmentedOutcome::Ignored
        );
        assert_eq!(
            state.handle_key(key(KeyCode::End), 3, true),
            SegmentedOutcome::Selected(2)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Home), 3, true),
            SegmentedOutcome::Selected(0)
        );
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn disabled_released_chorded_and_empty_controls_ignore_keys() {
        let mut state = SegmentedState::new(1);
        assert_eq!(
            state.handle_key(key(KeyCode::Right), 3, false),
            SegmentedOutcome::Ignored
        );
        let mut release = key(KeyCode::Right);
        release.kind = KeyEventKind::Release;
        assert_eq!(
            state.handle_key(release, 3, true),
            SegmentedOutcome::Ignored
        );
        let chord = KeyEvent::new(KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(state.handle_key(chord, 3, true), SegmentedOutcome::Ignored);
        assert_eq!(
            state.handle_key(key(KeyCode::Right), 0, true),
            SegmentedOutcome::Ignored
        );
        assert_eq!(state.selected, 1);
        let mut control = Segmented::new(["A", "B"], SegmentedState::new(0)).disabled("locked");
        assert_eq!(
            control.handle_key(key(KeyCode::Right)),
            SegmentedOutcome::Ignored
        );
    }

    #[test]
    fn more_than_five_options_keep_the_first_five() {
        let control = Segmented::new(["a", "b", "c", "d", "e", "f"], SegmentedState::new(0));
        assert_eq!(control.options().len(), SEGMENTED_MAX);
        let mut state = SegmentedState::new(0);
        assert_eq!(
            state.handle_key(key(KeyCode::End), 9, true),
            SegmentedOutcome::Selected(4)
        );
    }

    #[test]
    fn the_selection_is_bracketed_in_every_profile() {
        let control = Segmented::new(["Full", "Reduced", "Still"], SegmentedState::new(1));
        for profile in Profile::ALL {
            assert_eq!(
                line(&control, profile, 40),
                " Full  [Reduced]  Still",
                "{profile:?}"
            );
        }
    }

    #[test]
    fn it_degrades_to_the_current_option_and_its_position() {
        let control = Segmented::new(["Full", "Reduced", "Still"], SegmentedState::new(1));
        assert_eq!(control.full_width(), 6 + 9 + 7 + 2);
        assert_eq!(
            line(&control, Profile::DarkTrue, 24),
            " Full  [Reduced]  Still"
        );
        assert_eq!(line(&control, Profile::DarkTrue, 23), "Reduced (2 of 3)");
        // Too narrow even for the position: the option's name, cut last.
        assert_eq!(line(&control, Profile::DarkTrue, 10), "Reduced");
        assert_eq!(line(&control, Profile::DarkTrue, 6), "Reduc…");
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let theme = Profile::DarkTrue.theme();
        let control = Segmented::new(["Full", "Reduced"], SegmentedState::new(1)).focused(true);
        for (w, h) in [(0, 0), (1, 1), (3, 1), (5, 1), (9, 0)] {
            let _ = render(w, h, |area, buf| control.paint(area, buf, &theme));
        }
    }
}

//! Toggle: an on/off row with a label, a mark and a word for the state.
//!
//! On is `●` and "On", off is `○` and "Off" (`[x]` and `[ ]` in ASCII), so
//! the state is a shape and a word, never a color. Neither uses `Primary`,
//! which is reserved for actions, focus and links: on is `Foreground`, off
//! is `Muted`. The focused row carries the `▸` marker, bold and the
//! `Selected` ground, three cues that survive where grounds do not paint. A
//! disabled row is dimmed and says why.

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};

use crate::{Paint, Role, Theme, glyphs, text};

/// The words beside a toggle's mark. The kit owns no copy beyond this
/// English default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToggleWords {
    pub on: Cow<'static, str>,
    pub off: Cow<'static, str>,
}

impl Default for ToggleWords {
    fn default() -> Self {
        Self {
            on: Cow::Borrowed("On"),
            off: Cow::Borrowed("Off"),
        }
    }
}

/// Whether the toggle is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToggleState {
    pub on: bool,
}

impl From<bool> for ToggleState {
    fn from(on: bool) -> Self {
        Self { on }
    }
}

/// What a key did to a [`ToggleState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleOutcome {
    /// The key means nothing to a toggle; the host may use it.
    Ignored,
    /// Space or Enter flipped it; the field is the new state.
    Toggled(bool),
}

impl ToggleState {
    /// Apply a key press: Space or Enter flips the state. Releases, other
    /// keys and chords with Ctrl, Alt or Super are [`ToggleOutcome::Ignored`],
    /// and so is everything while `enabled` is false.
    pub fn handle_key(&mut self, key: KeyEvent, enabled: bool) -> ToggleOutcome {
        let held = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER;
        if !enabled || key.kind == KeyEventKind::Release || key.modifiers.intersects(held) {
            return ToggleOutcome::Ignored;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.on = !self.on;
                ToggleOutcome::Toggled(self.on)
            }
            _ => ToggleOutcome::Ignored,
        }
    }
}

/// One on/off row.
#[derive(Clone, Debug)]
pub struct Toggle<'a> {
    pub label: Cow<'a, str>,
    pub state: ToggleState,
    pub words: ToggleWords,
    /// This row has the keyboard.
    pub focused: bool,
    /// `Some(reason)` dims the row and shows why it cannot change.
    pub disabled: Option<Cow<'a, str>>,
}

impl<'a> Toggle<'a> {
    #[must_use]
    pub fn new(label: impl Into<Cow<'a, str>>, state: impl Into<ToggleState>) -> Self {
        Self {
            label: label.into(),
            state: state.into(),
            words: ToggleWords::default(),
            focused: false,
            disabled: None,
        }
    }

    #[must_use]
    pub fn with_words(mut self, words: &ToggleWords) -> Self {
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

    /// Apply a key to this row's state; a disabled row ignores every key.
    pub fn handle_key(&mut self, key: KeyEvent) -> ToggleOutcome {
        self.state.handle_key(key, self.disabled.is_none())
    }

    /// The mark: `●` on, `○` off; `[x]` and `[ ]` in ASCII.
    fn mark(&self, ascii: bool) -> &'static str {
        match (self.state.on, ascii) {
            (true, false) => glyphs::CURRENT,
            (false, false) => glyphs::AVAILABLE,
            (true, true) => "[x]",
            (false, true) => "[ ]",
        }
    }
}

impl Paint for Toggle<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let row = Rect { height: 1, ..area };
        let width = usize::from(row.width);
        let ascii = theme.ascii();
        let disabled = self.disabled.is_some();
        if self.focused {
            buf.set_style(row, theme.bg(Role::Selected));
        }

        let word = text::display_safe(if self.state.on {
            &self.words.on
        } else {
            &self.words.off
        });
        let state = format!("{} {word}", self.mark(ascii));
        let state_w = text::width(&state).min(width);
        let state_x = row.right() - u16::try_from(state_w).unwrap_or(row.width);
        let state_style = match (disabled, self.state.on) {
            (true, _) => theme.fg(Role::Muted).add_modifier(Modifier::DIM),
            (false, true) => theme.fg(Role::Foreground),
            (false, false) => theme.fg(Role::Muted),
        };
        buf.set_stringn(state_x, row.y, &state, state_w, state_style);

        // The label and its marker take what the state leaves, with two
        // cells between them.
        let left_room = width.saturating_sub(state_w + 2);
        let marker = glyphs::pick(glyphs::selection_marker(self.focused), ascii);
        let marker_w = text::width(marker) + 1;
        if left_room <= marker_w {
            return;
        }
        buf.set_stringn(row.x, row.y, marker, marker_w, theme.fg(Role::Accent));
        let label_style = if disabled {
            theme.fg(Role::Muted).add_modifier(Modifier::DIM)
        } else if self.focused {
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
        } else {
            theme.fg(Role::Foreground)
        };
        let label = text::display_safe(&self.label);
        let label = text::truncate(&label, left_room - marker_w, ascii);
        let label_x = row.x + u16::try_from(marker_w).unwrap_or(0);
        buf.set_stringn(label_x, row.y, &label, left_room - marker_w, label_style);

        if let Some(reason) = &self.disabled {
            let used = marker_w + text::width(&label) + 2;
            let room = left_room.saturating_sub(used);
            if room >= 4 {
                let reason = text::display_safe(reason);
                let reason = text::truncate_words(&reason, room, ascii);
                let x = row.x + u16::try_from(used).unwrap_or(0);
                buf.set_stringn(x, row.y, &reason, room, theme.fg(Role::Muted));
            }
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

    #[test]
    fn space_and_enter_flip_and_report_the_new_state() {
        let mut state = ToggleState::default();
        assert_eq!(
            state.handle_key(key(KeyCode::Char(' ')), true),
            ToggleOutcome::Toggled(true)
        );
        assert!(state.on);
        assert_eq!(
            state.handle_key(key(KeyCode::Enter), true),
            ToggleOutcome::Toggled(false)
        );
        assert!(!state.on);
    }

    #[test]
    fn other_keys_releases_chords_and_disabled_rows_do_nothing() {
        let mut state = ToggleState { on: true };
        assert_eq!(
            state.handle_key(key(KeyCode::Up), true),
            ToggleOutcome::Ignored
        );
        assert_eq!(
            state.handle_key(
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL),
                true
            ),
            ToggleOutcome::Ignored
        );
        let mut release = key(KeyCode::Char(' '));
        release.kind = KeyEventKind::Release;
        assert_eq!(state.handle_key(release, true), ToggleOutcome::Ignored);
        assert_eq!(
            state.handle_key(key(KeyCode::Char(' ')), false),
            ToggleOutcome::Ignored
        );
        assert!(state.on, "nothing flipped it");
        let mut row = Toggle::new("Show tool details", true).disabled("set by your admin");
        assert_eq!(row.handle_key(key(KeyCode::Enter)), ToggleOutcome::Ignored);
    }

    #[test]
    fn the_state_is_a_mark_and_a_word_in_every_profile() {
        for profile in [Profile::DarkTrue, Profile::NoColor, Profile::Ascii] {
            let theme = profile.theme();
            for (on, mark, word) in [(true, "●", "On"), (false, "○", "Off")] {
                let buf = render(40, 1, |area, buf| {
                    Toggle::new("Show tool details", on).paint(area, buf, &theme)
                });
                let line = rendered(&buf);
                let mark = if profile == Profile::Ascii {
                    if on { "[x]" } else { "[ ]" }
                } else {
                    mark
                };
                assert!(line.ends_with(&format!("{mark} {word}")), "{line:?}");
                assert!(line.contains("Show tool details"), "{line:?}");
            }
        }
    }

    #[test]
    fn a_narrow_row_keeps_the_state_and_cuts_the_label() {
        let theme = Profile::DarkTrue.theme();
        let buf = render(16, 1, |area, buf| {
            Toggle::new("Show tool details", true).paint(area, buf, &theme)
        });
        let line = rendered(&buf);
        assert!(line.ends_with("● On"), "{line:?}");
        assert!(line.contains('…'), "{line:?}");
        for w in 0..4 {
            let _ = render(w, 1, |area, buf| {
                Toggle::new("Show", false)
                    .focused(true)
                    .paint(area, buf, &theme)
            });
        }
    }

    #[test]
    fn a_disabled_row_says_why() {
        let theme = Profile::NoColor.theme();
        let buf = render(60, 1, |area, buf| {
            Toggle::new("Telemetry", false)
                .disabled("set by your admin")
                .paint(area, buf, &theme)
        });
        let line = rendered(&buf);
        assert!(line.contains("set by your admin"), "{line:?}");
        assert!(line.ends_with("○ Off"), "{line:?}");
    }
}

//! Tabs: a strip of labels with one selected.
//!
//! ```text
//!  General  Appearance  Agents 2  Privacy  2 more ›
//!           ──────────
//! ```
//!
//! The selected tab is bold `Foreground` with an underline as wide as its
//! label, never the full row; the others are `Muted`. Given one row only,
//! the selected tab is bracketed instead (`[Appearance]`), so it still shows
//! at 16 colors, under `NO_COLOR` and in ASCII. A tab may carry a count
//! badge (`Agents 2`). When the tabs do not fit, the strip scrolls to keep
//! the selection visible and marks what is hidden on each side:
//! `‹ 2 more` and `3 more ›`.
//!
//! Replaces the engine's `render_settings_category_strip`
//! (`crates/tui/src/tui/views/mod.rs`, `Hmbown/CodeWhale` `58b1dd3dd`),
//! whose windowing this follows.

use std::borrow::Cow;
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{buffer::Buffer, layout::Rect, style::Modifier};

use crate::{Paint, Role, Theme, glyphs, text};

/// One tab: its label and an optional count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tab {
    pub label: Cow<'static, str>,
    /// A number shown after the label (`Agents 2`); above 99 it reads `99+`.
    pub badge: Option<u32>,
}

impl Tab {
    #[must_use]
    pub fn new(label: impl Into<Cow<'static, str>>) -> Self {
        Self {
            label: label.into(),
            badge: None,
        }
    }

    #[must_use]
    pub fn badge(mut self, count: u32) -> Self {
        self.badge = Some(count);
        self
    }

    fn badge_text(&self) -> Option<String> {
        self.badge
            .map(|n| if n > 99 { "99+".into() } else { n.to_string() })
    }
}

/// The word in the overflow marks, `2 more ›`. The kit owns no copy beyond
/// this English default; an empty word leaves just the count: `2 ›`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabsWords {
    pub more: Cow<'static, str>,
}

impl Default for TabsWords {
    fn default() -> Self {
        Self {
            more: Cow::Borrowed("more"),
        }
    }
}

/// Which tab is selected and how far the strip has scrolled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabsState {
    pub selected: usize,
    /// The first tab shown, remembered between frames so the strip scrolls
    /// by as little as it must. [`Tabs::handle_key`] and
    /// [`Tabs::scroll_into_view`] keep it current.
    pub offset: usize,
}

/// What a key did to a [`TabsState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabsOutcome {
    /// The key means nothing to tabs, or changed nothing (an arrow at the
    /// end); the host may use it.
    Ignored,
    /// Tab `n` is now selected.
    Selected(usize),
}

impl TabsState {
    #[must_use]
    pub const fn new(selected: usize) -> Self {
        Self {
            selected,
            offset: 0,
        }
    }

    /// Apply a key press to a strip of `len` tabs. `←`/`→` stop at the ends;
    /// `Tab` and `Shift+Tab` wrap around; `Home` and `End` jump. Releases,
    /// chords with Ctrl, Alt or Super, other keys and an empty strip are
    /// [`TabsOutcome::Ignored`]. [`Tabs::handle_key`] also scrolls the new
    /// selection into view.
    pub fn handle_key(&mut self, key: KeyEvent, len: usize) -> TabsOutcome {
        let held = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER;
        if len == 0 || key.kind == KeyEventKind::Release || key.modifiers.intersects(held) {
            return TabsOutcome::Ignored;
        }
        let current = self.selected.min(len - 1);
        let backwards = key.modifiers.contains(KeyModifiers::SHIFT);
        let next = match key.code {
            KeyCode::Left => current.saturating_sub(1),
            KeyCode::Right => (current + 1).min(len - 1),
            KeyCode::Home => 0,
            KeyCode::End => len - 1,
            KeyCode::BackTab => (current + len - 1) % len,
            KeyCode::Tab if backwards => (current + len - 1) % len,
            KeyCode::Tab => (current + 1) % len,
            _ => return TabsOutcome::Ignored,
        };
        if next == current && self.selected < len {
            // One tab wrapping onto itself, or an arrow at an end.
            return TabsOutcome::Ignored;
        }
        self.selected = next;
        TabsOutcome::Selected(next)
    }
}

/// A strip of tabs.
#[derive(Clone, Debug)]
pub struct Tabs<'a> {
    pub items: &'a [Tab],
    pub state: TabsState,
    pub words: TabsWords,
    /// This strip has the keyboard: the selection is lit in `Primary`.
    pub focused: bool,
}

impl<'a> Tabs<'a> {
    #[must_use]
    pub fn new(items: &'a [Tab], state: TabsState) -> Self {
        Self {
            items,
            state,
            words: TabsWords::default(),
            focused: false,
        }
    }

    #[must_use]
    pub fn with_words(mut self, words: &TabsWords) -> Self {
        self.words = words.clone();
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    fn selected(&self) -> usize {
        self.state.selected.min(self.items.len().saturating_sub(1))
    }

    /// The label and badge of tab `index`, as painted.
    fn content(&self, index: usize) -> String {
        let tab = &self.items[index];
        let label = text::display_safe(&tab.label);
        match tab.badge_text() {
            Some(badge) => format!("{label} {badge}"),
            None => label.into_owned(),
        }
    }

    /// A tab's cells: its content and a cell of padding either side.
    fn slot_width(&self, index: usize) -> usize {
        text::width(&self.content(index)) + 2
    }

    /// `‹ 2 more `: what is hidden to the left, with a cell of air before the
    /// first tab. The compact form, `‹2`, is for a strip too narrow to spare
    /// room for the word.
    fn left_mark(&self, hidden: usize, ascii: bool, compact: bool) -> String {
        let arrow = glyphs::pick("‹", ascii);
        if compact {
            format!("{arrow}{hidden}")
        } else if self.words.more.is_empty() {
            format!("{arrow} {hidden} ")
        } else {
            format!("{arrow} {hidden} {} ", text::display_safe(&self.words.more))
        }
    }

    /// ` 3 more ›`: what is hidden to the right, with a cell of air after the
    /// last tab (`3›` when compact).
    fn right_mark(&self, hidden: usize, ascii: bool, compact: bool) -> String {
        let arrow = glyphs::pick("›", ascii);
        if compact {
            format!("{hidden}{arrow}")
        } else if self.words.more.is_empty() {
            format!(" {hidden} {arrow}")
        } else {
            format!(" {hidden} {} {arrow}", text::display_safe(&self.words.more))
        }
    }

    /// The window of tabs for a strip `width` cells wide, scrolled as little
    /// as it must from [`TabsState::offset`], with room kept for the marks.
    fn window(&self, width: u16, compact: bool) -> Range<usize> {
        let (n, width) = (self.items.len(), usize::from(width));
        if n == 0 {
            return 0..0;
        }
        let selected = self.selected();
        let slots: Vec<usize> = (0..n).map(|i| self.slot_width(i)).collect();
        let left = |hidden: usize| text::width(&self.left_mark(hidden, false, compact));
        let right = |hidden: usize| text::width(&self.right_mark(hidden, false, compact));
        let mut start = self.state.offset.min(selected);
        loop {
            let mut used = if start > 0 { left(start) } else { 0 };
            let mut end = start;
            while end < n && used + slots[end] <= width {
                used += slots[end];
                end += 1;
            }
            while end < n && end > start + 1 && used + right(n - end) > width {
                end -= 1;
                used -= slots[end];
            }
            end = end.max(start + 1);
            if selected < end {
                return start..end;
            }
            start += 1;
        }
    }

    /// The tabs shown in a strip `width` cells wide, and whether the marks
    /// had to shrink to `‹2` and `3›` to leave the selected tab its room.
    fn layout(&self, width: u16) -> (Range<usize>, bool) {
        let n = self.items.len();
        let total = |range: &Range<usize>, compact: bool| {
            let slots: usize = range.clone().map(|i| self.slot_width(i)).sum();
            let left = if range.start > 0 {
                text::width(&self.left_mark(range.start, false, compact))
            } else {
                0
            };
            let right = if range.end < n {
                text::width(&self.right_mark(n - range.end, false, compact))
            } else {
                0
            };
            left + slots + right
        };
        let full = self.window(width, false);
        if total(&full, false) <= usize::from(width) {
            return (full, false);
        }
        (self.window(width, true), true)
    }

    /// The tabs shown in a strip `width` cells wide: scrolled as little as
    /// it must from [`TabsState::offset`], with room kept for the overflow
    /// marks, and always including the selected tab (cut to fit if it alone
    /// is wider than the strip).
    #[must_use]
    pub fn visible(&self, width: u16) -> Range<usize> {
        self.layout(width).0
    }

    /// Store the offset this strip paints with at `width` into `state`, so
    /// scrolling is stable from one frame to the next.
    pub fn scroll_into_view(&self, state: &mut TabsState, width: u16) {
        let strip = Tabs {
            state: *state,
            ..self.clone()
        };
        state.offset = strip.visible(width).start;
    }

    /// Apply a key to this strip's state and keep the selection on screen at
    /// `width`.
    pub fn handle_key(&mut self, key: KeyEvent, width: u16) -> TabsOutcome {
        let outcome = self.state.handle_key(key, self.items.len());
        self.state.offset = self.visible(width).start;
        outcome
    }

    /// The tab under a click at `(column, row)` when the strip was painted
    /// in `area`, for hosts that take mouse input. The overflow marks are
    /// not tabs.
    #[must_use]
    pub fn tab_at(&self, area: Rect, column: u16, row: u16) -> Option<usize> {
        if row < area.y || row >= area.bottom() || column < area.x || column >= area.right() {
            return None;
        }
        let (range, compact) = self.layout(area.width);
        let mut x = area.x
            + if range.start > 0 {
                u16::try_from(text::width(&self.left_mark(range.start, false, compact)))
                    .unwrap_or(0)
            } else {
                0
            };
        for index in range {
            let w = u16::try_from(self.slot_width(index)).unwrap_or(u16::MAX);
            if column >= x && column < x.saturating_add(w) {
                return Some(index);
            }
            x = x.saturating_add(w);
        }
        None
    }
}

impl Paint for Tabs<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() || self.items.is_empty() {
            return;
        }
        let ascii = theme.ascii();
        let underline_row = area.height >= 2;
        let (range, compact) = self.layout(area.width);
        let (selected, y) = (self.selected(), area.y);
        let room = |x: u16| usize::from(area.right().saturating_sub(x));
        let cells = |s: &str| u16::try_from(text::width(s)).unwrap_or(u16::MAX);
        let mut x = area.x;

        let quiet = theme.fg(Role::Muted);
        if range.start > 0 {
            let mark = self.left_mark(range.start, ascii, compact);
            buf.set_stringn(x, y, &mark, room(x), quiet);
            x = x.saturating_add(cells(&mark));
        }
        let lit = if self.focused {
            theme.fg(Role::Accent)
        } else {
            theme.fg(Role::Foreground)
        };
        for index in range.clone() {
            if x >= area.right() {
                break;
            }
            let is_selected = index == selected;
            let tab = &self.items[index];
            let label = text::display_safe(&tab.label);
            let badge = tab.badge_text();
            // The last visible tab leaves room for the mark that follows it,
            // unless it is the one tab wider than the whole strip.
            let tail = if index + 1 == range.end && range.end < self.items.len() {
                usize::from(cells(&self.right_mark(
                    self.items.len() - range.end,
                    false,
                    compact,
                )))
            } else {
                0
            };
            let avail = room(x).saturating_sub(tail);
            let badge_w = badge.as_deref().map_or(0, |b| text::width(b) + 1);
            let label_room = avail.saturating_sub(2 + badge_w).max(1);
            let label = text::truncate(&label, label_room, ascii);
            let label_w = cells(&label);

            let (left_pad, right_pad) = if is_selected && !underline_row {
                ("[", "]")
            } else {
                (" ", " ")
            };
            let label_style = if is_selected {
                theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
            } else {
                quiet
            };
            let pad_style = if is_selected { lit } else { quiet };
            buf.set_stringn(x, y, left_pad, room(x), pad_style);
            buf.set_stringn(x + 1, y, &label, room(x + 1), label_style);
            let mut end = x + 1 + label_w;
            if let Some(badge) = &badge
                && usize::from(label_w) + 2 + badge_w <= avail
            {
                buf.set_stringn(end + 1, y, badge, room(end + 1), quiet);
                end += 1 + cells(badge);
            }
            buf.set_stringn(end, y, right_pad, room(end), pad_style);
            if is_selected && underline_row {
                let rule = glyphs::pick("─", ascii);
                for dx in x + 1..end {
                    if dx < area.right() {
                        buf[(dx, y + 1)].set_symbol(rule).set_style(lit);
                    }
                }
            }
            x = end.saturating_add(1);
        }
        if range.end < self.items.len() && x < area.right() {
            let mark = self.right_mark(self.items.len() - range.end, ascii, compact);
            buf.set_stringn(x, y, &mark, room(x), quiet);
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Profile, render, text as rendered};

    const LABELS: [&str; 6] = [
        "General",
        "Appearance",
        "Agents",
        "Privacy",
        "Shortcuts",
        "Advanced",
    ];

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn items() -> Vec<Tab> {
        LABELS.into_iter().map(Tab::new).collect()
    }

    #[test]
    fn arrows_stop_and_tab_wraps() {
        let mut state = TabsState::new(0);
        assert_eq!(
            state.handle_key(key(KeyCode::Left), 3),
            TabsOutcome::Ignored
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Right), 3),
            TabsOutcome::Selected(1)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Tab), 3),
            TabsOutcome::Selected(2)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Right), 3),
            TabsOutcome::Ignored
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Tab), 3),
            TabsOutcome::Selected(0)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::BackTab), 3),
            TabsOutcome::Selected(2)
        );
        let shift_tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT);
        assert_eq!(state.handle_key(shift_tab, 3), TabsOutcome::Selected(1));
        assert_eq!(
            state.handle_key(key(KeyCode::End), 3),
            TabsOutcome::Selected(2)
        );
        assert_eq!(
            state.handle_key(key(KeyCode::Home), 3),
            TabsOutcome::Selected(0)
        );
    }

    #[test]
    fn releases_chords_empty_strips_and_lone_tabs_do_nothing() {
        let mut state = TabsState::new(0);
        let mut release = key(KeyCode::Right);
        release.kind = KeyEventKind::Release;
        assert_eq!(state.handle_key(release, 3), TabsOutcome::Ignored);
        let chord = KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL);
        assert_eq!(state.handle_key(chord, 3), TabsOutcome::Ignored);
        assert_eq!(
            state.handle_key(key(KeyCode::Right), 0),
            TabsOutcome::Ignored
        );
        assert_eq!(state.handle_key(key(KeyCode::Tab), 1), TabsOutcome::Ignored);
        assert_eq!(
            state.handle_key(key(KeyCode::Char('x')), 3),
            TabsOutcome::Ignored
        );
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn a_selection_past_the_end_is_pulled_back_in() {
        let mut state = TabsState::new(99);
        assert_eq!(
            state.handle_key(key(KeyCode::Tab), 3),
            TabsOutcome::Selected(0)
        );
        let items = items();
        assert!(
            Tabs::new(&items, TabsState::new(99))
                .visible(30)
                .contains(&5)
        );
    }

    #[test]
    fn everything_shows_when_it_fits() {
        let items = items();
        assert_eq!(Tabs::new(&items, TabsState::new(0)).visible(120), 0..6);
    }

    #[test]
    fn the_selection_stays_visible_at_every_width_and_position() {
        let items = items();
        for width in 1..=120 {
            for selected in 0..items.len() {
                let range = Tabs::new(&items, TabsState::new(selected)).visible(width);
                assert!(
                    range.contains(&selected),
                    "width {width} selected {selected}: {range:?}"
                );
            }
        }
    }

    #[test]
    fn a_scrolled_strip_marks_what_it_hides_on_both_sides() {
        let theme = Profile::DarkTrue.theme();
        let items = items();
        let tabs = Tabs::new(&items, TabsState::new(3));
        let buf = render(36, 2, |area, buf| tabs.paint(area, buf, &theme));
        let text = rendered(&buf);
        let first = text.lines().next().unwrap();
        let range = tabs.visible(36);
        assert!(range.start > 0 && range.end < LABELS.len(), "{range:?}");
        assert!(first.starts_with("‹ "), "{first:?}");
        assert!(first.contains("Privacy"), "{first:?}");
        assert!(first.contains("more"), "{first:?}");
        assert!(first.trim_end().ends_with('›'), "{first:?}");
    }

    #[test]
    fn the_strip_scrolls_by_as_little_as_it_must() {
        // Walk right across a strip that cannot show everything, as a host
        // does: the first visible tab never moves back, and by at most two.
        let items = items();
        let mut state = TabsState::new(0);
        let mut last = 0;
        for _ in 0..items.len() - 1 {
            let mut strip = Tabs::new(&items, state);
            strip.handle_key(key(KeyCode::Right), 30);
            state = strip.state;
            assert!(
                state.offset >= last && state.offset <= last + 2,
                "{state:?}"
            );
            last = state.offset;
        }
        // Walking back does not snap the strip to the left.
        let before = state.offset;
        let mut strip = Tabs::new(&items, state);
        strip.handle_key(key(KeyCode::Left), 30);
        assert_eq!(strip.state.offset, before.min(strip.state.selected));
        assert!(strip.visible(30).contains(&strip.state.selected));
    }

    #[test]
    fn scroll_into_view_stores_the_painted_offset() {
        let items = items();
        let mut state = TabsState::new(5);
        Tabs::new(&items, state).scroll_into_view(&mut state, 30);
        assert!(state.offset > 0);
        assert!(Tabs::new(&items, state).visible(30).contains(&5));
    }

    #[test]
    fn the_selected_tab_is_underlined_or_bracketed_in_every_profile() {
        let items = [Tab::new("General"), Tab::new("Agents").badge(2)];
        for profile in Profile::ALL {
            let theme = profile.theme();
            let tabs = Tabs::new(&items, TabsState::new(1));
            let two = rendered(&render(40, 2, |area, buf| tabs.paint(area, buf, &theme)));
            let rows: Vec<_> = two.lines().collect();
            assert_eq!(rows[0].trim_end(), " General  Agents 2", "{profile:?}");
            let rule = if profile == Profile::Ascii {
                "-"
            } else {
                "─"
            };
            assert_eq!(
                rows[1].trim_end(),
                format!("          {}", rule.repeat(8)),
                "{profile:?}"
            );
            let one = rendered(&render(40, 1, |area, buf| tabs.paint(area, buf, &theme)));
            assert_eq!(one, " General [Agents 2]", "{profile:?}");
        }
    }

    #[test]
    fn badges_cap_at_ninety_nine() {
        let items = [Tab::new("Inbox").badge(250)];
        let tabs = Tabs::new(&items, TabsState::new(0));
        let theme = Profile::NoColor.theme();
        let line = rendered(&render(20, 1, |area, buf| tabs.paint(area, buf, &theme)));
        assert!(line.contains("Inbox 99+"), "{line:?}");
    }

    #[test]
    fn a_click_finds_the_tab_under_it() {
        let items = items();
        let tabs = Tabs::new(&items, TabsState::new(0));
        let area = Rect::new(0, 0, 120, 2);
        assert_eq!(tabs.tab_at(area, 3, 0), Some(0));
        assert_eq!(tabs.tab_at(area, 12, 0), Some(1));
        assert_eq!(tabs.tab_at(area, 3, 5), None);
    }

    #[test]
    fn tiny_and_empty_strips_do_not_panic() {
        let theme = Profile::DarkTrue.theme();
        let mut items = items();
        items[2] = Tab::new("Agents").badge(4);
        for selected in [0, 3, 9] {
            for (w, h) in [(0, 0), (1, 1), (2, 2), (5, 1), (7, 2), (12, 1)] {
                let tabs = Tabs::new(&items, TabsState::new(selected)).focused(true);
                let _ = render(w, h, |area, buf| tabs.paint(area, buf, &theme));
            }
        }
        let none = Tabs::new(&[], TabsState::new(0));
        let _ = render(10, 2, |area, buf| none.paint(area, buf, &theme));
        assert_eq!(none.visible(10), 0..0);
    }
}

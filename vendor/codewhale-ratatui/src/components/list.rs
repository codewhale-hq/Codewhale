//! List: a scrolling list of rows the caller paints, with a selection that
//! always stays on screen.
//!
//! ```text
//! ▸ Theme                     Shoreline · follows terminal
//!   Motion                    Full
//!   Density                   Comfortable                  █
//! ```
//!
//! The list owns what every selectable list repeats: the `▸` marker, the
//! selection ground, the scrollbar, wrapping navigation that skips disabled
//! rows, paging, and the viewport arithmetic that keeps the selected row
//! fully visible. The caller owns what a row says: a row is anything that
//! implements [`ListRow`], one to several lines tall.
//!
//! A selected row carries three cues, because a fill alone measures only
//! about 1.3:1 against its neighbours: the `▸` marker (`>` in ASCII), the
//! label in bold (a row asks [`ListRowState::ink`]), and the `Selected`
//! ground where grounds paint. The marker and the bold survive 16 colors and
//! `NO_COLOR`; the ground is the extra.
//!
//! An empty list paints its [`EmptyState`] if it has one, so the "nothing
//! here" message lives where the rows would have been.
//!
//! Replaces the scattered selection markers (`▸`, `❯`, a literal `"▸ "`),
//! `menu_style::selected_row_style` and `list_nav::apply` in the engine's
//! `crates/tui/src/tui/` (`Hmbown/CodeWhale` `58b1dd3dd`), and
//! `render_panel_scroll_rail`. [`crate::Picker`] is built on the same
//! navigation and scrollbar.

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    symbols::scrollbar,
    text::{Line, Span},
    widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget},
};

use crate::{EmptyState, Paint, Role, Theme, glyphs, text};

/// Cells the marker takes before a row's content: `▸ `.
const GUTTER: u16 = 2;
/// Cells the scrollbar takes: the bar and one of air.
const RAIL: u16 = 2;

/// What a row is asked to know when it paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListRowState {
    pub index: usize,
    /// This is the selected row. The list has already drawn the marker and
    /// the ground; the row makes its label bold ([`ListRowState::ink`]).
    pub selected: bool,
    /// The row cannot be chosen; it should recede and say why.
    pub disabled: bool,
}

impl ListRowState {
    /// The style for a row's main text: bold `Foreground` when selected,
    /// receded when disabled, `Foreground` otherwise.
    #[must_use]
    pub fn ink(&self, theme: &Theme) -> Style {
        if self.disabled {
            theme.fg(Role::Muted).add_modifier(Modifier::DIM)
        } else if self.selected {
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
        } else {
            theme.fg(Role::Foreground)
        }
    }
}

/// One row of a [`List`]: its height and how it paints.
pub trait ListRow {
    /// Lines this row takes at `width` cells of content. At least one;
    /// zero is read as one.
    fn height(&self, _width: u16) -> u16 {
        1
    }

    /// Whether navigation skips this row and Enter cannot choose it.
    fn is_disabled(&self) -> bool {
        false
    }

    /// Paint the row's content into `area`: the list has already drawn the
    /// marker, the selection ground and the gutter, and `area` is as tall as
    /// [`ListRow::height`] (less if the viewport clips it). Style every cell
    /// through `theme`; use [`ListRowState::ink`] for the main text.
    fn paint_row(&self, area: Rect, buf: &mut Buffer, theme: &Theme, state: ListRowState);
}

fn paint_label(label: &str, area: Rect, buf: &mut Buffer, theme: &Theme, state: ListRowState) {
    let label = text::display_safe(label);
    let shown = text::truncate(&label, usize::from(area.width), theme.ascii());
    Line::from(Span::styled(shown.into_owned(), state.ink(theme))).render(area, buf);
}

impl ListRow for &str {
    fn paint_row(&self, area: Rect, buf: &mut Buffer, theme: &Theme, state: ListRowState) {
        paint_label(self, area, buf, theme, state);
    }
}

impl ListRow for String {
    fn paint_row(&self, area: Rect, buf: &mut Buffer, theme: &Theme, state: ListRowState) {
        paint_label(self, area, buf, theme, state);
    }
}

impl ListRow for Cow<'_, str> {
    fn paint_row(&self, area: Rect, buf: &mut Buffer, theme: &Theme, state: ListRowState) {
        paint_label(self, area, buf, theme, state);
    }
}

/// The next enabled row after `selected`, wrapping at the ends. `selected`
/// stays put when no other row is enabled.
pub(crate) fn step_to(
    selected: usize,
    len: usize,
    forward: bool,
    enabled: impl Fn(usize) -> bool,
) -> usize {
    if len == 0 {
        return 0;
    }
    let selected = selected.min(len - 1);
    for d in 1..=len {
        let at = if forward {
            (selected + d) % len
        } else {
            (selected + len - d % len) % len
        };
        if enabled(at) {
            return at;
        }
    }
    selected
}

/// The enabled row nearest `target` in the direction of travel, else the
/// nearest the other way, else `target`. No wrapping: for Home, End and
/// paging, which clamp.
pub(crate) fn settle_at(
    target: usize,
    len: usize,
    forward: bool,
    enabled: impl Fn(usize) -> bool,
) -> usize {
    if len == 0 {
        return 0;
    }
    let target = target.min(len - 1);
    let after = (target..len).find(|&i| enabled(i));
    let before = (0..=target).rev().find(|&i| enabled(i));
    let (first, second) = if forward {
        (after, before)
    } else {
        (before, after)
    };
    first.or(second).unwrap_or(target)
}

/// The offset that keeps `selected` fully visible in `height` lines, keeps
/// `offset` where it can, and leaves no blank lines under the last row while
/// rows are hidden above. `row_h` gives each row's height.
pub(crate) fn offset_for(
    selected: usize,
    offset: usize,
    len: usize,
    height: u16,
    row_h: impl Fn(usize) -> u16,
) -> usize {
    if len == 0 {
        return 0;
    }
    let height = usize::from(height);
    let h = |i: usize| usize::from(row_h(i).max(1));
    let selected = selected.min(len - 1);

    // The lowest offset that still puts the last row on the bottom line.
    let mut tail = 0;
    let mut bottom = len;
    while bottom > 0 && tail + h(bottom - 1) <= height {
        tail += h(bottom - 1);
        bottom -= 1;
    }
    let mut offset = offset.min(bottom.min(len - 1));

    if selected < offset {
        offset = selected;
    } else {
        // The first row from which the selected row still ends in view.
        let mut used = 0;
        let mut first = selected + 1;
        while first > 0 && used + h(first - 1) <= height {
            used += h(first - 1);
            first -= 1;
        }
        offset = offset.max(first.min(selected));
    }
    offset
}

/// Which row is selected and how far the list has scrolled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListState {
    pub selected: usize,
    /// The first row painted. [`List`] recomputes it from the selection when
    /// it paints; keep it here so scrolling is stable between frames.
    pub offset: usize,
}

impl ListState {
    #[must_use]
    pub const fn new(selected: usize) -> Self {
        Self {
            selected,
            offset: 0,
        }
    }

    /// Put the selection on an enabled row inside `0..len`, after the rows
    /// changed (a refresh, a filter). Keeps the row if it still can be
    /// selected; otherwise takes the next enabled row, else the previous one.
    pub fn settle(&mut self, len: usize, enabled: impl Fn(usize) -> bool) {
        if len == 0 {
            *self = Self::default();
            return;
        }
        let at = self.selected.min(len - 1);
        self.selected = settle_at(at, len, true, enabled);
    }

    /// The next enabled row, wrapping from the last to the first.
    pub fn select_next(&mut self, len: usize, enabled: impl Fn(usize) -> bool) {
        self.selected = step_to(self.selected, len, true, enabled);
    }

    /// The previous enabled row, wrapping from the first to the last.
    pub fn select_prev(&mut self, len: usize, enabled: impl Fn(usize) -> bool) {
        self.selected = step_to(self.selected, len, false, enabled);
    }

    /// The first enabled row.
    pub fn home(&mut self, len: usize, enabled: impl Fn(usize) -> bool) {
        self.selected = settle_at(0, len, true, enabled);
    }

    /// The last enabled row.
    pub fn end(&mut self, len: usize, enabled: impl Fn(usize) -> bool) {
        self.selected = settle_at(len.saturating_sub(1), len, false, enabled);
    }

    /// Move `step` rows, clamped at the ends (a page is a request to travel,
    /// not to wrap), landing on an enabled row.
    pub fn page(&mut self, len: usize, step: usize, down: bool, enabled: impl Fn(usize) -> bool) {
        let step = step.max(1);
        let target = if down {
            (self.selected + step).min(len.saturating_sub(1))
        } else {
            self.selected.saturating_sub(step)
        };
        self.selected = settle_at(target, len, down, enabled);
    }

    /// The offset a list of `len` rows paints with in `height` lines, where
    /// `row_h` gives each row's height: the selected row is fully visible.
    #[must_use]
    pub fn visible_offset(&self, len: usize, height: u16, row_h: impl Fn(usize) -> u16) -> usize {
        offset_for(self.selected, self.offset, len, height, row_h)
    }

    /// Store the offset [`List`] will paint with.
    pub fn scroll_into_view(&mut self, len: usize, height: u16, row_h: impl Fn(usize) -> u16) {
        self.offset = self.visible_offset(len, height, row_h);
    }

    /// Apply a key press to the state of `rows` painted in `viewport`, and
    /// say what happened. Keys: `↑`/`↓` (wrapping, skipping disabled rows),
    /// `Home`, `End`, `PgUp`, `PgDn`, `Enter`, `Space`, `Esc`. Releases and
    /// other keys are [`ListOutcome::Ignored`]; so are Enter and Space on a
    /// disabled row. Esc cancels even an empty list. Modified keys belong
    /// to the host; held keys repeat navigation but never choose or toggle.
    pub fn handle_key<R: ListRow>(
        &mut self,
        key: KeyEvent,
        rows: &[R],
        viewport: Rect,
    ) -> ListOutcome {
        if key.kind == KeyEventKind::Release
            || !key.modifiers.is_empty()
            || (key.kind == KeyEventKind::Repeat
                && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ')))
        {
            return ListOutcome::Ignored;
        }
        let len = rows.len();
        if key.code == KeyCode::Esc {
            return ListOutcome::Cancelled;
        }
        if len == 0 {
            return ListOutcome::Ignored;
        }
        let enabled = |i: usize| !rows[i].is_disabled();
        let width = viewport.width.saturating_sub(GUTTER);
        let row_h = |i: usize| rows[i].height(width);
        match key.code {
            KeyCode::Up => self.select_prev(len, enabled),
            KeyCode::Down => self.select_next(len, enabled),
            KeyCode::Home => self.home(len, enabled),
            KeyCode::End => self.end(len, enabled),
            KeyCode::PageUp | KeyCode::PageDown => {
                let page = self.rows_per_page(len, viewport.height, row_h);
                self.page(len, page, key.code == KeyCode::PageDown, enabled);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if self.selected >= len || rows[self.selected].is_disabled() {
                    return ListOutcome::Ignored;
                }
                return if key.code == KeyCode::Enter {
                    ListOutcome::Chose(self.selected)
                } else {
                    ListOutcome::Toggled(self.selected)
                };
            }
            _ => return ListOutcome::Ignored,
        }
        self.scroll_into_view(len, viewport.height, row_h);
        ListOutcome::Moved
    }

    /// Rows that fit in one screenful from the current offset.
    fn rows_per_page(&self, len: usize, height: u16, row_h: impl Fn(usize) -> u16) -> usize {
        let mut used = 0usize;
        let mut count = 0;
        for i in self.offset.min(len)..len {
            used += usize::from(row_h(i).max(1));
            if used > usize::from(height) {
                break;
            }
            count += 1;
        }
        count.max(1)
    }
}

/// What a key did to a [`ListState`]: the message a host reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListOutcome {
    /// The key means nothing to a list; the host may use it.
    Ignored,
    /// The selection moved (or tried to); repaint.
    Moved,
    /// Enter: row `n` was chosen.
    Chose(usize),
    /// Space: row `n` was toggled (a checklist row).
    Toggled(usize),
    /// Esc: the list was dismissed.
    Cancelled,
}

/// Draw ratatui's scrollbar down the right edge of `area`: `█` on `│`, or
/// `#` on `|` in ASCII-safe terminals, in `Foreground` over `Border` so it
/// reads without any ground. `visible` rows of `len` are showing, from
/// `offset`.
pub(crate) fn paint_scrollbar(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    len: usize,
    offset: usize,
    visible: usize,
) {
    let (thumb, track) = if theme.ascii() {
        ("#", "|")
    } else {
        ("█", "│")
    };
    let mut state = ScrollbarState::new(len.saturating_sub(visible))
        .position(offset)
        .viewport_content_length(visible);
    Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .symbols(scrollbar::Set {
            track,
            thumb,
            begin: track,
            end: track,
        })
        .begin_symbol(None)
        .end_symbol(None)
        .thumb_style(theme.fg(Role::Foreground))
        .track_style(theme.fg(Role::Border))
        .render(area, buf, &mut state);
}

/// A row placed in the viewport: its index, its line from the top, and its
/// height (clipped at the bottom edge).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    index: usize,
    y: u16,
    height: u16,
}

/// Where a painted [`List`] put its rows.
struct Placed {
    offset: usize,
    slots: Vec<Slot>,
    /// Whether the scrollbar is drawn (and rows are narrower for it).
    rail: bool,
}

/// A scrolling list of caller-painted rows.
///
/// Use [`Paint::paint`] with a state snapshot, or render the themed widget
/// with application-owned [`ListState`]. Stateful rendering stores the
/// actual viewport offset, including variable-height rows and clipping:
///
/// ```no_run
/// use codewhale_ratatui::{List, ListState, Paint, Theme};
/// # fn draw(frame: &mut ratatui::Frame<'_>, state: &mut ListState) {
/// let theme = Theme::detect().tui();
/// let rows = ["First session", "Second session"];
/// let list = List::new(&rows, ListState::default());
/// frame.render_stateful_widget(list.themed(&theme), frame.area(), state);
/// # }
/// ```
pub struct List<'a, R: ListRow> {
    rows: &'a [R],
    state: ListState,
    empty: Option<EmptyState<'a>>,
}

impl<R: ListRow> Clone for List<'_, R> {
    fn clone(&self) -> Self {
        Self {
            rows: self.rows,
            state: self.state,
            empty: self.empty.clone(),
        }
    }
}

impl<'a, R: ListRow> List<'a, R> {
    #[must_use]
    pub const fn new(rows: &'a [R], state: ListState) -> Self {
        Self {
            rows,
            state,
            empty: None,
        }
    }

    /// What to show when there are no rows.
    #[must_use]
    pub fn empty(mut self, empty: EmptyState<'a>) -> Self {
        self.empty = Some(empty);
        self
    }

    fn place(&self, area: Rect) -> Placed {
        let len = self.rows.len();
        let place_at = |width: u16| {
            let h = |i: usize| self.rows[i].height(width).max(1);
            let offset = self.state.visible_offset(len, area.height, h);
            let mut slots = Vec::new();
            let mut y = 0u16;
            for index in offset..len {
                if y >= area.height {
                    break;
                }
                let height = h(index).min(area.height - y);
                slots.push(Slot { index, y, height });
                // By the clipped height: a row may report up to `u16::MAX`
                // lines, and `y` plus that would overflow.
                y += height;
            }
            let overflow = offset > 0
                || slots
                    .last()
                    .is_some_and(|s| s.index + 1 < len || s.height < h(s.index));
            (offset, slots, overflow)
        };
        let content = area.width.saturating_sub(GUTTER);
        let (offset, slots, overflow) = place_at(content);
        if overflow && area.width > GUTTER + RAIL {
            let (offset, slots, _) = place_at(content - RAIL);
            return Placed {
                offset,
                slots,
                rail: true,
            };
        }
        Placed {
            offset,
            slots,
            rail: false,
        }
    }

    /// The row under a click at `(column, row)` when the list was painted in
    /// `area`, for hosts that take mouse input.
    #[must_use]
    pub fn row_at(&self, area: Rect, column: u16, row: u16) -> Option<usize> {
        let inside =
            column >= area.x && column < area.right() && row >= area.y && row < area.bottom();
        if !inside || self.rows.is_empty() {
            return None;
        }
        let y = row - area.y;
        self.place(area)
            .slots
            .iter()
            .find(|s| y >= s.y && y < s.y + s.height)
            .map(|s| s.index)
    }
}

impl<R: ListRow> Paint for List<'_, R> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        if self.rows.is_empty() {
            if let Some(empty) = &self.empty {
                empty.paint(area, buf, theme);
            }
            return;
        }
        let placed = self.place(area);
        let list_w = area.width - if placed.rail { RAIL } else { 0 };
        let gutter = GUTTER.min(list_w);
        let marker = glyphs::pick(glyphs::SELECTION, theme.ascii());
        for slot in &placed.slots {
            let row = &self.rows[slot.index];
            let state = ListRowState {
                index: slot.index,
                selected: slot.index == self.state.selected,
                disabled: row.is_disabled(),
            };
            let rect = Rect {
                y: area.y + slot.y,
                height: slot.height,
                width: list_w,
                ..area
            };
            if state.selected {
                buf.set_style(rect, theme.bg(Role::Selected));
                if gutter > 0 {
                    buf.set_string(rect.x, rect.y, marker, theme.fg(Role::Accent));
                }
            }
            row.paint_row(
                Rect {
                    x: rect.x + gutter,
                    width: list_w - gutter,
                    ..rect
                },
                buf,
                theme,
                state,
            );
        }
        if placed.rail {
            paint_scrollbar(
                area,
                buf,
                theme,
                self.rows.len(),
                placed.offset,
                placed.slots.len(),
            );
        }
    }

    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        let content = width.saturating_sub(GUTTER);
        let total: usize = self
            .rows
            .iter()
            .map(|r| usize::from(r.height(content).max(1)))
            .sum();
        u16::try_from(total).unwrap_or(u16::MAX)
    }
}

/// Render with application-owned selection and scroll state. This explicit
/// state overrides the constructor's snapshot; the resolved viewport offset
/// is stored after clipping and scrollbar layout.
impl<R: ListRow> StatefulWidget for crate::Themed<'_, List<'_, R>> {
    type State = ListState;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        StatefulWidget::render(&self, area, buf, state);
    }
}

impl<R: ListRow> StatefulWidget for &crate::Themed<'_, List<'_, R>> {
    type State = ListState;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        if self.component.rows.is_empty() {
            *state = ListState::default();
            self.component.paint(area, buf, self.theme);
            return;
        }
        state.selected = state.selected.min(self.component.rows.len() - 1);
        let view = List {
            rows: self.component.rows,
            state: *state,
            empty: None,
        };
        state.offset = view.place(area).offset;
        view.paint(area, buf, self.theme);
    }
}

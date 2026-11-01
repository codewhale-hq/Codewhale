//! Picker: the list behind the mode picker, the status-line picker, and
//! every other "choose one" or "choose some" list.
//!
//! A selected row carries three cues, because a fill alone measures only
//! about 1.3:1 against its neighbours: the `▸` marker lit in `Primary`, the
//! label in bold, and the `Selected` ground where grounds paint. Checked rows
//! show `●` and unchecked `○` (`[x]` and `[ ]` in ASCII), so on/off is a
//! shape, not a color. Disabled rows say why.
//!
//! With a query line the picker is a fuzzy finder. The caller owns the text
//! and its editing; the picker paints the query it is given
//! ([`Picker::query`]), ranks with [`PickerMatches::rank`] (the matcher in
//! [`crate::fuzzy_score`]), underlines the matched characters as well as
//! coloring them, and says "No matches" with an [`crate::EmptyState`] rather
//! than a count of zero. Tabs ([`PickerTabs`]) filter by category, and a
//! preview pane ([`PickerPreview`]) sits beside the list at
//! [`PICKER_PREVIEW_MIN_WIDTH`] columns and up and is left out below it.
//! [`PickerState::handle_picker_key`] separates the keys that move from the
//! keys that belong to the query. Without any of these, a picker is the plain
//! list it always was.
//!
//! Replaces the row rendering in the engine's `views/mode_picker.rs` and
//! `views/status_picker.rs`, and `menu_style::selected_row_style`
//! (`Hmbown/CodeWhale` `58b1dd3dd`).

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
use unicode_segmentation::UnicodeSegmentation;

use super::list::{paint_scrollbar, settle_at, step_to};
use crate::{EmptyState, Paint, Role, Theme, fuzzy_score, glyphs, text};

/// One row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickerItem {
    pub label: Cow<'static, str>,
    /// Muted words after the label: what choosing this does.
    pub detail: Option<Cow<'static, str>>,
    /// A shortcut shown before the label (`1`, `2`, `3`).
    pub key: Option<char>,
    /// `Some` makes this a checklist row.
    pub checked: Option<bool>,
    /// `Some(reason)` greys the row and shows the reason as its detail.
    pub disabled: Option<Cow<'static, str>>,
}

impl PickerItem {
    #[must_use]
    pub fn new(label: impl Into<Cow<'static, str>>) -> Self {
        Self {
            label: label.into(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<Cow<'static, str>>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn key(mut self, key: char) -> Self {
        self.key = Some(key);
        self
    }

    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    #[must_use]
    pub fn disabled(mut self, reason: impl Into<Cow<'static, str>>) -> Self {
        self.disabled = Some(reason.into());
        self
    }
}

/// Which row is selected and how far the list has scrolled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PickerState {
    pub selected: usize,
    pub offset: usize,
}

impl PickerState {
    #[must_use]
    pub const fn new(selected: usize) -> Self {
        Self {
            selected,
            offset: 0,
        }
    }

    pub fn next(&mut self, len: usize) {
        if len > 0 {
            self.selected = self.selected.min(len - 1).saturating_add(1) % len;
        }
    }

    pub fn prev(&mut self, len: usize) {
        if len > 0 {
            self.selected = if self.selected.min(len - 1) == 0 {
                len - 1
            } else {
                self.selected.min(len - 1) - 1
            };
        }
    }

    pub fn home(&mut self) {
        self.selected = 0;
    }

    pub fn end(&mut self, len: usize) {
        self.selected = len.saturating_sub(1);
    }

    /// Move a page, clamped at the ends.
    pub fn page(&mut self, len: usize, rows: u16, down: bool) {
        let step = usize::from(rows.max(1));
        self.selected = if down {
            self.selected
                .saturating_add(step)
                .min(len.saturating_sub(1))
        } else {
            self.selected
                .min(len.saturating_sub(1))
                .saturating_sub(step)
        };
    }

    /// The offset that keeps the selection on screen in `rows` rows.
    #[must_use]
    pub fn visible_offset(&self, len: usize, rows: usize) -> usize {
        let rows = rows.max(1);
        let selected = self.selected.min(len.saturating_sub(1));
        let mut offset = self.offset.min(len.saturating_sub(rows));
        if selected < offset {
            offset = selected;
        } else if selected >= offset.saturating_add(rows) {
            offset = selected + 1 - rows;
        }
        offset
    }

    /// Store the offset [`Picker`] painted with, so scrolling is stable.
    pub fn scroll_into_view(&mut self, len: usize, rows: u16) {
        self.selected = self.selected.min(len.saturating_sub(1));
        self.offset = self.visible_offset(len, usize::from(rows));
    }
}

/// What a key did to a [`PickerState`]: the message a host reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerOutcome {
    /// The key means nothing to a picker; the host may use it.
    Ignored,
    /// The selection moved (or tried to); repaint.
    Moved,
    /// Enter: row `n` was chosen.
    Chose(usize),
    /// Space: row `n` was toggled (a checklist row).
    Toggled(usize),
    /// Esc: the picker was dismissed.
    Cancelled,
    /// A picker with a query line: the key belongs to the query. Feed it to
    /// the text editor, then rebuild the [`PickerMatches`] and
    /// [`PickerState::reset`]. Only [`PickerState::handle_picker_key`]
    /// returns it.
    Query,
    /// Tabs: the active tab is now `n`. Store it, rebuild the matches and
    /// repaint; the state has already been reset.
    Tab(usize),
}

impl PickerState {
    /// Apply a key press to the state for a list of `len` rows shown in
    /// `rows` rows, and say what happened. Keys: `↑`/`↓` (wrapping), `Home`,
    /// `End`, `PgUp`, `PgDn`, `Enter`, `Space`, `Esc`. Releases and other
    /// keys are [`PickerOutcome::Ignored`]; so is every key on an empty list.
    /// Modified keys belong to the host. Held keys repeat navigation only.
    pub fn handle_key(&mut self, key: KeyEvent, len: usize, rows: u16) -> PickerOutcome {
        if key.kind == KeyEventKind::Release
            || len == 0
            || !key.modifiers.is_empty()
            || (key.kind == KeyEventKind::Repeat
                && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ')))
        {
            return PickerOutcome::Ignored;
        }
        self.scroll_into_view(len, rows);
        match key.code {
            KeyCode::Up => self.prev(len),
            KeyCode::Down => self.next(len),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(len),
            KeyCode::PageUp => self.page(len, rows, false),
            KeyCode::PageDown => self.page(len, rows, true),
            KeyCode::Enter => return PickerOutcome::Chose(self.selected),
            KeyCode::Char(' ') => return PickerOutcome::Toggled(self.selected),
            KeyCode::Esc => return PickerOutcome::Cancelled,
            _ => return PickerOutcome::Ignored,
        }
        self.scroll_into_view(len, rows);
        PickerOutcome::Moved
    }
}

impl PickerState {
    /// Back to the first row, as after the query or the tab changed.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// [`PickerState::handle_key`] for a picker with a query line, tabs,
    /// filtered rows or disabled rows: it asks `picker` how many rows are
    /// shown, which are disabled, and whether a query is being typed.
    ///
    /// Rows are the *shown* rows: with a filter, `Chose(n)` is the `n`th
    /// match, and [`Picker::item_index`] gives the item it stands for.
    ///
    /// Keys, with a query line (typing): `↑`/`↓`, `PgUp`/`PgDn`, `Enter`,
    /// `Esc`, `Tab`/`Shift+Tab` (tabs) and `Ctrl+Space` (toggle) are the
    /// picker's; every other key press is [`PickerOutcome::Query`], for the
    /// host's text editor, so letters, `Space`, `Backspace`, `←`/`→`,
    /// `Home` and `End` are never eaten. Without a query line, `Home`/`End`
    /// and `←`/`→` (tabs) also move, and `Space` toggles. Navigation skips
    /// disabled rows, and `Enter`/`Space` on one are ignored. `Esc` cancels
    /// even with nothing shown. Held keys repeat editing/navigation only;
    /// modified activation/navigation and releases are ignored.
    ///
    /// When the query or the tab changes, rebuild the [`PickerMatches`] and
    /// [`PickerState::reset`] (a tab change resets for you).
    pub fn handle_picker_key(
        &mut self,
        key: KeyEvent,
        picker: &Picker<'_>,
        rows: u16,
    ) -> PickerOutcome {
        if key.kind == KeyEventKind::Release {
            return PickerOutcome::Ignored;
        }
        let allowed_modifiers = match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT
            }
            KeyCode::Char(' ') => {
                key.modifiers.is_empty() || key.modifiers == KeyModifiers::CONTROL
            }
            KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown => key.modifiers.is_empty(),
            _ => true,
        };
        if !allowed_modifiers {
            return PickerOutcome::Ignored;
        }
        let len = picker.shown_len();
        let typing = picker.query.is_some();
        if key.kind == KeyEventKind::Repeat
            && (matches!(key.code, KeyCode::Enter | KeyCode::Esc)
                || (key.code == KeyCode::Char(' ')
                    && (!typing || key.modifiers == KeyModifiers::CONTROL)))
        {
            return PickerOutcome::Ignored;
        }
        let enabled = |i: usize| {
            picker
                .shown_item(i)
                .is_none_or(|(item, _)| item.disabled.is_none())
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let tabs = picker.tabs.map_or(0, |t| t.labels.len());
        let tab = |forward: bool, state: &mut Self| {
            let active = picker
                .tabs
                .map_or(0, |t| t.active.min(tabs.saturating_sub(1)));
            let next = if forward {
                (active + 1) % tabs
            } else {
                (active + tabs - 1) % tabs
            };
            state.reset();
            PickerOutcome::Tab(next)
        };
        if len > 0 {
            self.selected = self.selected.min(len - 1);
        }
        let page = usize::from(rows.max(1));
        match key.code {
            KeyCode::Esc => return PickerOutcome::Cancelled,
            KeyCode::Tab | KeyCode::BackTab if tabs > 1 => {
                let back =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                return tab(!back, self);
            }
            // No tabs to change: Tab is the host's (focus), never the query's.
            KeyCode::Tab | KeyCode::BackTab => return PickerOutcome::Ignored,
            KeyCode::Left | KeyCode::Right if !typing && tabs > 1 => {
                if !key.modifiers.is_empty() {
                    return PickerOutcome::Ignored;
                }
                return tab(key.code == KeyCode::Right, self);
            }
            KeyCode::Enter => {
                return if len > 0 && enabled(self.selected) {
                    PickerOutcome::Chose(self.selected)
                } else {
                    PickerOutcome::Ignored
                };
            }
            KeyCode::Char(' ') if ctrl || !typing => {
                return if len > 0 && enabled(self.selected) {
                    PickerOutcome::Toggled(self.selected)
                } else {
                    PickerOutcome::Ignored
                };
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown if len > 0 => {
                self.selected = match key.code {
                    KeyCode::Up => step_to(self.selected, len, false, enabled),
                    KeyCode::Down => step_to(self.selected, len, true, enabled),
                    KeyCode::PageUp => {
                        settle_at(self.selected.saturating_sub(page), len, false, enabled)
                    }
                    _ => settle_at((self.selected + page).min(len - 1), len, true, enabled),
                };
            }
            KeyCode::Home if !typing && len > 0 && key.modifiers.is_empty() => {
                self.selected = settle_at(0, len, true, enabled)
            }
            KeyCode::End if !typing && len > 0 && key.modifiers.is_empty() => {
                self.selected = settle_at(len - 1, len, false, enabled);
            }
            _ => {
                return if typing && !matches!(key.code, KeyCode::Up | KeyCode::Down) {
                    PickerOutcome::Query
                } else {
                    PickerOutcome::Ignored
                };
            }
        }
        self.scroll_into_view(len, rows);
        PickerOutcome::Moved
    }
}

/// The words a [`Picker`] shows itself. The kit owns no copy beyond this
/// English default: a host fills one per locale and passes it with
/// [`Picker::words`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerWords {
    /// The query line while it is empty.
    pub placeholder: Cow<'static, str>,
    /// Before the quoted query when nothing matches: `No matches for "x"`.
    pub no_matches: Cow<'static, str>,
    /// The one next step when nothing matches.
    pub no_matches_action: Cow<'static, str>,
    /// When there is nothing to choose from at all.
    pub empty: Cow<'static, str>,
}

static ENGLISH: PickerWords = PickerWords {
    placeholder: Cow::Borrowed("Type to filter"),
    no_matches: Cow::Borrowed("No matches for"),
    no_matches_action: Cow::Borrowed("Edit the search to see more"),
    empty: Cow::Borrowed("Nothing to choose from"),
};

impl Default for PickerWords {
    fn default() -> Self {
        ENGLISH.clone()
    }
}

/// The query line's text and where the cursor is. The caller owns editing:
/// it passes what the line holds, the picker paints it and filters nothing
/// by itself (see [`PickerMatches::rank`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PickerQuery<'a> {
    pub text: &'a str,
    /// The cursor's column in display cells from the start of `text`.
    pub cursor: usize,
}

impl<'a> PickerQuery<'a> {
    #[must_use]
    pub const fn new(text: &'a str, cursor: usize) -> Self {
        Self { text, cursor }
    }
}

/// Categories across the top. Each item belongs to one tab (or to every
/// tab); `Tab` and `Shift+Tab` (`←`/`→` without a query line) move between
/// them through [`PickerOutcome::Tab`].
#[derive(Clone, Copy, Debug)]
pub struct PickerTabs<'a> {
    pub labels: &'a [Cow<'static, str>],
    pub active: usize,
    /// The tab each item belongs to, by item index; `None`, or an index past
    /// the end, puts the item in every tab.
    pub of: &'a [Option<usize>],
    /// A tab that shows every item (an "All" tab), if there is one.
    pub all: Option<usize>,
}

impl<'a> PickerTabs<'a> {
    #[must_use]
    pub const fn new(labels: &'a [Cow<'static, str>], active: usize) -> Self {
        Self {
            labels,
            active,
            of: &[],
            all: None,
        }
    }

    /// Which tab each item belongs to.
    #[must_use]
    pub const fn assign(mut self, of: &'a [Option<usize>]) -> Self {
        self.of = of;
        self
    }

    /// The tab that shows every item.
    #[must_use]
    pub const fn all(mut self, index: usize) -> Self {
        self.all = Some(index);
        self
    }

    fn shows(&self, item: usize) -> bool {
        self.all == Some(self.active)
            || self
                .of
                .get(item)
                .copied()
                .flatten()
                .is_none_or(|tab| tab == self.active)
    }
}

/// A caller-painted pane beside the list: what the highlighted item is.
/// Paint into `area` through `theme`; `item` is the highlighted item and
/// `index` its place in the picker's `items`. Any
/// `Fn(Rect, &mut Buffer, &Theme, &PickerItem, usize)` is one.
pub trait PickerPreview {
    fn paint_preview(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        item: &PickerItem,
        index: usize,
    );
}

impl<F> PickerPreview for F
where
    F: Fn(Rect, &mut Buffer, &Theme, &PickerItem, usize),
{
    fn paint_preview(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        item: &PickerItem,
        index: usize,
    ) {
        self(area, buf, theme, item, index);
    }
}

/// One item that passed the filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerMatch {
    /// The item's index in the picker's `items`.
    pub index: usize,
    pub score: i32,
    /// `char` indices of the query in the item's display-safe label, to
    /// highlight; empty when the query matched the detail instead, or there
    /// is no query.
    pub positions: Vec<usize>,
}

/// A detail match ranks below every label match.
const DETAIL_PENALTY: i32 = 10_000;

/// The items a picker shows: the ones in the active tab that match the
/// query, best first. Build it when the query or the tab changes, keep it
/// across frames, and hand it to [`Picker::matches`] and to
/// [`PickerState::handle_picker_key`] (through the picker).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PickerMatches {
    rows: Vec<PickerMatch>,
}

impl PickerMatches {
    /// Filter and rank `items` by `query` (a fuzzy subsequence match on the
    /// label, then on the detail), keeping the active tab's items. An empty
    /// query keeps every item of the tab in its original order.
    #[must_use]
    pub fn rank(items: &[PickerItem], query: &str, tabs: Option<&PickerTabs<'_>>) -> Self {
        let mut rows: Vec<PickerMatch> = items
            .iter()
            .enumerate()
            .filter(|(index, _)| tabs.is_none_or(|t| t.shows(*index)))
            .filter_map(|(index, item)| {
                let label = text::display_safe(&item.label);
                if let Some(m) = fuzzy_score(query, &label) {
                    return Some(PickerMatch {
                        index,
                        score: m.score,
                        positions: m.positions,
                    });
                }
                let detail = text::display_safe(item.detail.as_deref()?);
                fuzzy_score(query, &detail).map(|m| PickerMatch {
                    index,
                    score: m.score - DETAIL_PENALTY,
                    positions: Vec::new(),
                })
            })
            .collect();
        rows.sort_by_key(|m| (std::cmp::Reverse(m.score), m.index));
        Self { rows }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The `shown`th match.
    #[must_use]
    pub fn get(&self, shown: usize) -> Option<&PickerMatch> {
        self.rows.get(shown)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PickerMatch> {
        self.rows.iter()
    }
}

/// The narrowest picker that shows its preview pane; below this the pane is
/// left out and the list takes the width.
pub const PICKER_PREVIEW_MIN_WIDTH: u16 = 56;
/// The narrowest the preview pane is drawn, and the narrowest list beside it.
const PREVIEW_MIN: u16 = 20;

/// Where a [`Picker`] puts its parts in an area.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PickerLayout {
    pub tabs: Option<Rect>,
    pub query: Option<Rect>,
    pub list: Rect,
    /// The rule between the list and the preview, one column wide.
    pub separator: Option<Rect>,
    pub preview: Option<Rect>,
}

/// Spans plus the cells they cover.
#[derive(Default)]
struct Row {
    spans: Vec<Span<'static>>,
    used: usize,
}

impl Row {
    fn push(&mut self, s: String, style: Style) {
        self.used += text::width(&s);
        self.spans.push(Span::styled(s, style));
    }
}

/// `label` cut to `width` cells and padded, as runs that are or are not on a
/// matched position. Cuts and pads exactly as [`text::pad`] does.
fn label_runs(
    label: &str,
    positions: &[usize],
    width: usize,
    ascii: bool,
    base: Style,
    hit: Style,
) -> Vec<(String, Style)> {
    let fitted = text::truncate(label, width, ascii);
    // The ellipsis a cut adds is not part of the label.
    let common = fitted
        .chars()
        .zip(label.chars())
        .take_while(|(a, b)| a == b)
        .count();
    let mut runs: Vec<(String, Style)> = Vec::new();
    let mut at = 0;
    for g in fitted.graphemes(true) {
        let n = g.chars().count();
        let hot = at < common && (at..at + n).any(|c| positions.binary_search(&c).is_ok());
        at += n;
        let style = if hot { hit } else { base };
        match runs.last_mut() {
            Some((s, st)) if *st == style => s.push_str(g),
            _ => runs.push((g.to_owned(), style)),
        }
    }
    let gap = width.saturating_sub(text::width(&fitted));
    if gap > 0 {
        match runs.last_mut() {
            Some((s, st)) if *st == base => s.push_str(&" ".repeat(gap)),
            _ => runs.push((" ".repeat(gap), base)),
        }
    }
    runs
}

/// A list of [`PickerItem`]s, optionally with a query line, tabs, a filter
/// and a preview pane.
///
/// ```text
///   ● All   Models   Themes
///   › shore▏                                         2/17
/// ▸ Shoreline          Graphite ground     │ Shoreline
///   Shoreline light    Paper ground        │ ● Working
/// ```
///
/// Alone it is the plain list it always was. Add pieces as the picker needs
/// them: [`Picker::query`] (the caller edits the text, the picker paints it
/// and highlights the match), [`Picker::matches`] (what [`PickerMatches`]
/// kept), [`Picker::tabs`], [`Picker::preview`] (beside the list at
/// [`PICKER_PREVIEW_MIN_WIDTH`] columns and up, dropped below) and
/// [`Picker::words`].
///
/// Ratatui's stateful adapter persists selection and the viewport offset in
/// your own [`PickerState`], overriding the constructor's state snapshot:
///
/// ```no_run
/// use codewhale_ratatui::{Paint, Picker, PickerItem, PickerState, Theme};
/// # fn draw(frame: &mut ratatui::Frame<'_>, state: &mut PickerState) {
/// let theme = Theme::detect().tui();
/// let items = [PickerItem::new("Underwater"), PickerItem::new("Dracula")];
/// let picker = Picker::new(&items, PickerState::default()).query("", 0);
/// frame.render_stateful_widget(picker.themed(&theme), frame.area(), state);
/// # }
/// ```
#[derive(Clone, Copy)]
pub struct Picker<'a> {
    pub items: &'a [PickerItem],
    pub state: PickerState,
    query: Option<PickerQuery<'a>>,
    tabs: Option<PickerTabs<'a>>,
    matches: Option<&'a PickerMatches>,
    preview: Option<&'a dyn PickerPreview>,
    preview_min: u16,
    words: Option<&'a PickerWords>,
}

impl std::fmt::Debug for Picker<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picker")
            .field("items", &self.items)
            .field("state", &self.state)
            .field("query", &self.query)
            .field("tabs", &self.tabs)
            .field("matches", &self.matches)
            .field("preview", &self.preview.is_some())
            .finish_non_exhaustive()
    }
}

impl<'a> Picker<'a> {
    #[must_use]
    pub const fn new(items: &'a [PickerItem], state: PickerState) -> Self {
        Self {
            items,
            state,
            query: None,
            tabs: None,
            matches: None,
            preview: None,
            preview_min: PICKER_PREVIEW_MIN_WIDTH,
            words: None,
        }
    }

    /// Show a query line holding `text` with the cursor at `cursor` (display
    /// cells from the start). The caller edits; filtering is
    /// [`PickerMatches::rank`].
    #[must_use]
    pub const fn query(mut self, text: &'a str, cursor: usize) -> Self {
        self.query = Some(PickerQuery::new(text, cursor));
        self
    }

    /// Show tabs across the top.
    #[must_use]
    pub const fn tabs(mut self, tabs: PickerTabs<'a>) -> Self {
        self.tabs = Some(tabs);
        self
    }

    /// Show only these items, in this order, with their matches highlighted.
    /// Selection and scrolling then index the matches.
    #[must_use]
    pub const fn matches(mut self, matches: &'a PickerMatches) -> Self {
        self.matches = Some(matches);
        self
    }

    /// Show a pane beside the list for the highlighted item.
    #[must_use]
    pub const fn preview(mut self, preview: &'a dyn PickerPreview) -> Self {
        self.preview = Some(preview);
        self
    }

    /// The width from which the preview pane shows (default
    /// [`PICKER_PREVIEW_MIN_WIDTH`]).
    #[must_use]
    pub const fn preview_min_width(mut self, width: u16) -> Self {
        self.preview_min = width;
        self
    }

    /// Words for the placeholder and the empty states (default English).
    #[must_use]
    pub const fn words(mut self, words: &'a PickerWords) -> Self {
        self.words = Some(words);
        self
    }

    fn words_or_default(&self) -> &PickerWords {
        self.words.unwrap_or(&ENGLISH)
    }

    /// How many rows are shown: the matches, or every item.
    #[must_use]
    pub fn shown_len(&self) -> usize {
        self.matches.map_or(self.items.len(), PickerMatches::len)
    }

    /// The index in `items` of the `shown`th row.
    #[must_use]
    pub fn item_index(&self, shown: usize) -> Option<usize> {
        match self.matches {
            Some(m) => m.get(shown).map(|m| m.index),
            None => (shown < self.items.len()).then_some(shown),
        }
    }

    /// The `shown`th row's item and the label positions to highlight.
    fn shown_item(&self, shown: usize) -> Option<(&'a PickerItem, &'a [usize])> {
        match self.matches {
            Some(m) => {
                let m = m.get(shown)?;
                Some((self.items.get(m.index)?, m.positions.as_slice()))
            }
            None => Some((self.items.get(shown)?, &[])),
        }
    }

    /// Where the parts go in `area`.
    #[must_use]
    pub fn layout(&self, area: Rect) -> PickerLayout {
        let tabs = self.tabs.is_some_and(|t| !t.labels.is_empty());
        let query = self.query.is_some();
        // A row for the list outranks the tabs, and the tabs the query.
        let (tabs, query) = if area.height > u16::from(tabs) + u16::from(query) {
            (tabs, query)
        } else if area.height > u16::from(query) {
            (false, query)
        } else {
            (false, false)
        };
        let mut y = area.y;
        let mut row = |wanted: bool| {
            wanted.then(|| {
                let r = Rect {
                    y,
                    height: 1,
                    ..area
                };
                y += 1;
                r
            })
        };
        let tabs = row(tabs);
        let query = row(query);
        let body = Rect {
            y,
            height: area.bottom().saturating_sub(y),
            ..area
        };
        let pane =
            (self.preview.is_some() && self.shown_len() > 0 && body.width >= self.preview_min)
                .then(|| {
                    let pane = (body.width * 2 / 5).max(PREVIEW_MIN);
                    (body.width.saturating_sub(pane + 3), pane)
                })
                .filter(|&(list, _)| list >= PREVIEW_MIN);
        match pane {
            Some((list_w, pane_w)) => PickerLayout {
                tabs,
                query,
                list: Rect {
                    width: list_w,
                    ..body
                },
                separator: Some(Rect {
                    x: body.x + list_w + 1,
                    width: 1,
                    ..body
                }),
                preview: Some(Rect {
                    x: body.x + list_w + 3,
                    width: pane_w,
                    ..body
                }),
            },
            None => PickerLayout {
                tabs,
                query,
                list: body,
                separator: None,
                preview: None,
            },
        }
    }

    /// How many rows the list shows in `area`: what to pass
    /// [`PickerState::handle_picker_key`].
    #[must_use]
    pub fn list_rows(&self, area: Rect) -> u16 {
        self.layout(area).list.height
    }

    /// Where the terminal cursor belongs when the query line has focus, for
    /// `Frame::set_cursor_position`. It follows the text when a long query
    /// is cut to its end.
    #[must_use]
    pub fn cursor(&self, area: Rect) -> Option<ratatui::layout::Position> {
        let rect = self.layout(area).query?;
        let view = self.query_view(self.query?, rect.width, false)?;
        let col = u16::try_from(view.cursor).unwrap_or(u16::MAX);
        Some(ratatui::layout::Position::new(
            (rect.x + QUERY_INDENT)
                .saturating_add(col)
                .min(rect.right().saturating_sub(1)),
            rect.y,
        ))
    }

    /// The shown row under a click at `(column, row)` when the picker was
    /// painted in `area`, for hosts that take mouse input. Without a filter
    /// that is the item's index; otherwise pass it to
    /// [`Picker::item_index`].
    #[must_use]
    pub fn row_at(&self, area: Rect, column: u16, row: u16) -> Option<usize> {
        let list = self.layout(area).list;
        let shown = self.shown_len();
        let inside =
            column >= list.x && column < list.right() && row >= list.y && row < list.bottom();
        let reserved = if shown > usize::from(list.height) && list.width > 2 {
            2
        } else {
            0
        };
        if !inside || column >= list.right().saturating_sub(reserved) {
            return None;
        }
        let offset = self.state.visible_offset(shown, usize::from(list.height));
        let index = offset + usize::from(row - list.y);
        (index < shown).then_some(index)
    }

    fn label_column(&self) -> usize {
        (0..self.shown_len())
            .filter_map(|at| self.shown_item(at))
            .map(|(item, _)| text::width(&text::display_safe(&item.label)))
            .max()
            .unwrap_or(0)
    }

    fn row(
        &self,
        selected: bool,
        item: &PickerItem,
        positions: &[usize],
        width: usize,
        label_col: usize,
        theme: &Theme,
    ) -> Line<'static> {
        let ascii = theme.ascii();
        let disabled = item.disabled.is_some();

        let mut row = Row::default();

        let marker = glyphs::pick(glyphs::selection_marker(selected), ascii);
        row.push(format!("{marker} "), theme.fg(Role::Accent));
        if let Some(key) = item.key {
            row.push(format!("{key} "), theme.fg(Role::Muted));
        }
        if let Some(checked) = item.checked {
            let (mark, role) = match (checked, ascii) {
                (true, false) => (glyphs::CURRENT, Role::Foreground),
                (false, false) => (glyphs::AVAILABLE, Role::Muted),
                (true, true) => ("[x]", Role::Foreground),
                (false, true) => ("[ ]", Role::Muted),
            };
            row.push(format!("{mark} "), theme.fg(role));
        }

        let label_style = if disabled {
            theme.fg(Role::Muted).add_modifier(Modifier::DIM)
        } else if selected {
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
        } else {
            theme.fg(Role::Foreground)
        };
        let label = text::display_safe(&item.label);
        let label_w = label_col.min(width.saturating_sub(row.used));
        if positions.is_empty() {
            row.push(text::pad(&label, label_w, ascii), label_style);
        } else {
            // Matched characters are underlined as well as colored, so the
            // match shows without color.
            let hit = if disabled {
                label_style.add_modifier(Modifier::UNDERLINED)
            } else {
                label_style
                    .patch(theme.fg(Role::Accent))
                    .add_modifier(Modifier::UNDERLINED)
            };
            for (run, style) in label_runs(&label, positions, label_w, ascii, label_style, hit) {
                row.push(run, style);
            }
        }

        let detail = item.disabled.as_ref().or(item.detail.as_ref());
        if let Some(detail) = detail {
            let room = width.saturating_sub(row.used + 2);
            if room >= 4 {
                let detail = text::display_safe(detail);
                row.push("  ".into(), theme.fg(Role::Muted));
                row.push(
                    text::truncate_words(&detail, room, ascii).into_owned(),
                    theme.fg(Role::Muted),
                );
            }
        }

        let spans = row.spans;
        let mut line = Line::from(spans);
        if selected {
            line = line.style(theme.bg(Role::Selected));
        }
        line
    }

    fn paint_rows(&self, list: Rect, buf: &mut Buffer, theme: &Theme) {
        let shown = self.shown_len();
        let rows = usize::from(list.height);
        let overflow = shown > rows;
        // The scrollbar takes the last column and keeps one cell of air.
        let body = Rect {
            width: list
                .width
                .saturating_sub(2 * u16::from(overflow && list.width > 2)),
            ..list
        };
        let offset = self.state.visible_offset(shown, rows);
        let selected = self.state.selected.min(shown.saturating_sub(1));
        let label_col = self.label_column();
        for (row, at) in (offset..shown).take(rows).enumerate() {
            let Some((item, positions)) = self.shown_item(at) else {
                continue;
            };
            let rect = Rect {
                y: body.y + row as u16,
                height: 1,
                ..body
            };
            if at == selected {
                buf.set_style(rect, theme.bg(Role::Selected));
            }
            self.row(
                at == selected,
                item,
                positions,
                usize::from(body.width),
                label_col,
                theme,
            )
            .render(rect, buf);
        }
        if overflow && list.width > 2 {
            paint_scrollbar(list, buf, theme, shown, offset, rows);
        }
    }

    fn paint_empty(&self, list: Rect, buf: &mut Buffer, theme: &Theme) {
        let words = self.words_or_default();
        let query = self
            .query
            .map(|q| text::display_safe(q.text).trim().to_owned())
            .unwrap_or_default();
        let empty = if query.is_empty() {
            EmptyState::new(words.empty.as_ref())
        } else {
            let (open, close) = if theme.ascii() {
                ("\"", "\"")
            } else {
                ("\u{201c}", "\u{201d}")
            };
            let query = text::truncate(&query, 24, theme.ascii());
            EmptyState::new(format!("{} {open}{query}{close}", words.no_matches))
                .action(words.no_matches_action.as_ref())
        };
        empty.paint(list, buf, theme);
    }

    fn paint_tabs(&self, tabs: PickerTabs<'_>, rect: Rect, buf: &mut Buffer, theme: &Theme) {
        const GAP: usize = 2;
        let ascii = theme.ascii();
        let n = tabs.labels.len();
        if n == 0 {
            return;
        }
        let active = tabs.active.min(n - 1);
        let mark = glyphs::pick(glyphs::CURRENT, ascii);
        let labels: Vec<String> = tabs
            .labels
            .iter()
            .map(|l| text::display_safe(l).into_owned())
            .collect();
        let cost = |i: usize| {
            text::width(&labels[i])
                + if i == active {
                    text::width(mark) + 1
                } else {
                    0
                }
        };
        let room = usize::from(rect.width);
        // Cells the `…` on a hidden side takes, with its space.
        let more = |lo: usize, hi: usize| 2 * usize::from(lo > 0) + 2 * usize::from(hi + 1 < n);
        let (mut lo, mut hi) = (active, active);
        let mut used = cost(active);
        loop {
            if hi + 1 < n && used + GAP + cost(hi + 1) + more(lo, hi + 1) <= room {
                used += GAP + cost(hi + 1);
                hi += 1;
            } else if lo > 0 && used + GAP + cost(lo - 1) + more(lo - 1, hi) <= room {
                used += GAP + cost(lo - 1);
                lo -= 1;
            } else {
                break;
            }
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        let ellipsis = glyphs::pick(glyphs::ELLIPSIS, ascii);
        if lo > 0 {
            spans.push(Span::styled(format!("{ellipsis} "), theme.fg(Role::Hint)));
        }
        for (i, label) in labels.iter().enumerate().take(hi + 1).skip(lo) {
            if i > lo {
                spans.push(Span::raw(" ".repeat(GAP)));
            }
            if i == active {
                spans.push(Span::styled(format!("{mark} "), theme.fg(Role::Accent)));
                let room = room.saturating_sub(text::width(mark) + 1);
                spans.push(Span::styled(
                    text::truncate(label, room, ascii).into_owned(),
                    theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(label.clone(), theme.fg(Role::Muted)));
            }
        }
        if hi + 1 < n {
            spans.push(Span::styled(format!(" {ellipsis}"), theme.fg(Role::Hint)));
        }
        Line::from(spans).render(rect, buf);
    }

    /// What the query line shows in `width` cells: the text (or the
    /// placeholder), the cursor's column in it, and the count at the right.
    fn query_view(&self, query: PickerQuery<'_>, width: u16, ascii: bool) -> Option<QueryView> {
        let room = usize::from(width.checked_sub(QUERY_INDENT)?);
        let typed = text::display_safe(query.text);

        // `3/17`, right-aligned, while there is something typed.
        let count = (!typed.is_empty() && self.shown_len() > 0)
            .then(|| format!("{}/{}", self.shown_len(), self.items.len()))
            .filter(|c| text::width(c) + 2 < room);
        let room = room.saturating_sub(count.as_ref().map_or(0, |c| text::width(c) + 2));

        let mut cursor = query.cursor.min(text::width(&typed));
        let (shown, hint) = if typed.is_empty() {
            let words = self.words_or_default();
            (
                text::truncate(&words.placeholder, room, ascii).into_owned(),
                true,
            )
        } else if text::width(&typed) <= room {
            (typed.into_owned(), false)
        } else {
            // Keep the end, where the cursor usually is.
            let ellipsis = glyphs::pick(glyphs::ELLIPSIS, ascii);
            let budget = room.saturating_sub(text::width(ellipsis));
            let mut tail: Vec<&str> = Vec::new();
            let mut used = 0;
            for g in typed.graphemes(true).rev() {
                let w = text::width(g);
                if used + w > budget {
                    break;
                }
                used += w;
                tail.push(g);
            }
            tail.reverse();
            let skipped = text::width(&typed) - used;
            cursor = cursor.saturating_sub(skipped) + text::width(ellipsis);
            (format!("{ellipsis}{}", tail.concat()), false)
        };
        Some(QueryView {
            shown,
            hint,
            cursor,
            room,
            count,
        })
    }

    fn paint_query(&self, query: PickerQuery<'_>, rect: Rect, buf: &mut Buffer, theme: &Theme) {
        let ascii = theme.ascii();
        // `/` where ASCII-safe: `>` would read as the selection marker below.
        let prompt = if ascii { "/" } else { "\u{203a}" };
        buf.set_string(rect.x, rect.y, prompt, theme.fg(Role::Accent));
        let Some(view) = self.query_view(query, rect.width, ascii) else {
            return;
        };
        let x0 = rect.x + QUERY_INDENT;
        let style = if view.hint {
            theme.fg(Role::Hint)
        } else {
            theme.fg(Role::Foreground)
        };
        buf.set_stringn(x0, rect.y, &view.shown, view.room, style);
        if let Some(count) = &view.count {
            let x = rect
                .right()
                .saturating_sub(u16::try_from(text::width(count)).unwrap_or(0));
            buf.set_string(x, rect.y, count, theme.fg(Role::Hint));
        }
        // A reversed cell at the cursor shows it in every profile.
        if let Ok(col) = u16::try_from(view.cursor)
            && view.cursor <= view.room
        {
            let at = x0 + col;
            if at < rect.right() {
                buf[(at, rect.y)].modifier.insert(Modifier::REVERSED);
            }
        }
    }
}

/// The query line as it is painted.
struct QueryView {
    shown: String,
    /// `shown` is the placeholder.
    hint: bool,
    /// The cursor's column in `shown`.
    cursor: usize,
    /// Cells left for `shown`, before the count.
    room: usize,
    count: Option<String>,
}

/// The query line's text starts after `› `.
const QUERY_INDENT: u16 = 2;

impl Paint for Picker<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let layout = self.layout(area);
        if let (Some(rect), Some(tabs)) = (layout.tabs, self.tabs) {
            self.paint_tabs(tabs, rect, buf, theme);
        }
        if let (Some(rect), Some(query)) = (layout.query, self.query) {
            self.paint_query(query, rect, buf, theme);
        }
        if layout.list.is_empty() {
            return;
        }
        let shown = self.shown_len();
        if shown == 0 {
            self.paint_empty(layout.list, buf, theme);
            return;
        }
        self.paint_rows(layout.list, buf, theme);
        if let (Some(rule), Some(pane), Some(preview)) =
            (layout.separator, layout.preview, self.preview)
        {
            let bar = glyphs::pick("\u{2502}", theme.ascii());
            for y in rule.y..rule.bottom() {
                buf.set_string(rule.x, y, bar, theme.fg(Role::Border));
            }
            let at = self.state.selected.min(shown - 1);
            if let (Some((item, _)), Some(index)) = (self.shown_item(at), self.item_index(at)) {
                preview.paint_preview(pane, buf, theme, item, index);
            }
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        let head = u16::from(self.tabs.is_some_and(|t| !t.labels.is_empty()))
            + u16::from(self.query.is_some());
        let body = match self.shown_len() {
            0 => 3,
            n => u16::try_from(n).unwrap_or(u16::MAX),
        };
        head.saturating_add(body)
    }
}

/// Render with persistent host state instead of the constructor's snapshot.
/// Query rows, tabs and clipping are included in the stored scroll offset.
impl ratatui::widgets::StatefulWidget for crate::Themed<'_, Picker<'_>> {
    type State = PickerState;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        ratatui::widgets::StatefulWidget::render(&self, area, buf, state);
    }
}

impl ratatui::widgets::StatefulWidget for &crate::Themed<'_, Picker<'_>> {
    type State = PickerState;
    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let mut view = Picker {
            items: self.component.items,
            state: *state,
            query: self.component.query,
            tabs: self.component.tabs,
            matches: self.component.matches,
            preview: self.component.preview,
            preview_min: self.component.preview_min,
            words: self.component.words,
        };
        let viewport = view.layout(area).list;
        if !viewport.is_empty() {
            state.scroll_into_view(view.shown_len(), viewport.height);
            view.state = *state;
        }
        view.paint(area, buf, self.theme);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_map_to_the_scrolled_row() {
        let items: Vec<PickerItem> = (0..10)
            .map(|i| PickerItem::new(format!("Item {i}")))
            .collect();
        let picker = Picker::new(&items, PickerState::new(9));
        let area = Rect::new(2, 3, 20, 4);
        assert_eq!(picker.row_at(area, 5, 3), Some(6));
        assert_eq!(picker.row_at(area, 5, 6), Some(9));
        assert_eq!(picker.row_at(area, 1, 3), None);
        assert_eq!(picker.row_at(area, 5, 7), None);
    }

    /// A disabled row says why, in place of its detail, and recedes.
    #[test]
    fn disabled_rows_say_why() {
        use crate::testing::{Profile, render, text};
        let items = [
            PickerItem::new("Workspace").detail("This folder"),
            PickerItem::new("Git branch")
                .detail("Current branch")
                .disabled("Not a git repository"),
        ];
        let picker = Picker::new(&items, PickerState::new(0));
        for profile in Profile::ALL {
            let theme = profile.theme();
            let buf = render(48, 2, |area, buf| picker.paint(area, buf, &theme));
            let shown = text(&buf);
            let row = shown.lines().nth(1).unwrap_or_default();
            assert!(
                row.contains("Not a git repository"),
                "{}: {row}",
                profile.name()
            );
            assert!(!row.contains("Current branch"), "{}: {row}", profile.name());
            let label = row.find("Git").expect("label drawn");
            let cell = &buf[(u16::try_from(label).unwrap_or(0), 1)];
            assert!(
                cell.modifier.contains(Modifier::DIM),
                "{}: disabled label recedes",
                profile.name()
            );
        }
    }

    #[test]
    fn keys_move_choose_toggle_and_cancel() {
        let key = |code| KeyEvent::new(code, crossterm::event::KeyModifiers::NONE);
        let mut s = PickerState::new(0);
        assert_eq!(s.handle_key(key(KeyCode::Up), 3, 2), PickerOutcome::Moved);
        assert_eq!(s.selected, 2, "wraps");
        assert_eq!(s.offset, 1, "keeps the selection in view");
        assert_eq!(s.handle_key(key(KeyCode::Home), 3, 2), PickerOutcome::Moved);
        assert_eq!(s.handle_key(key(KeyCode::Down), 3, 2), PickerOutcome::Moved);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter), 3, 2),
            PickerOutcome::Chose(1)
        );
        assert_eq!(
            s.handle_key(key(KeyCode::Char(' ')), 3, 2),
            PickerOutcome::Toggled(1)
        );
        assert_eq!(
            s.handle_key(key(KeyCode::Esc), 3, 2),
            PickerOutcome::Cancelled
        );
        assert_eq!(
            s.handle_key(key(KeyCode::Char('x')), 3, 2),
            PickerOutcome::Ignored
        );
        let mut release = key(KeyCode::Down);
        release.kind = KeyEventKind::Release;
        assert_eq!(s.handle_key(release, 3, 2), PickerOutcome::Ignored);
        assert_eq!(s.selected, 1, "a release moves nothing");
        assert_eq!(
            PickerState::default().handle_key(key(KeyCode::Enter), 0, 2),
            PickerOutcome::Ignored,
            "an empty list chooses nothing"
        );
    }

    #[test]
    fn state_wraps_pages_and_keeps_selection_visible() {
        let mut s = PickerState::new(0);
        s.prev(3);
        assert_eq!(s.selected, 2);
        s.next(3);
        assert_eq!(s.selected, 0);
        s.page(20, 5, true);
        assert_eq!(s.selected, 5);
        assert_eq!(s.visible_offset(20, 4), 2);
        s.end(20);
        s.scroll_into_view(20, 4);
        assert_eq!(s.offset, 16);
        s.home();
        assert_eq!(s.visible_offset(20, 4), 0);
    }
}

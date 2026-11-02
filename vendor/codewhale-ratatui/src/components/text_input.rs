//! Package Input: a single-line text input.
//!
//! Three parts, each usable alone:
//!
//! - [`LineBuffer`]: the text model. One line, grapheme-aware (a combining
//!   sequence, a CJK character or an emoji ZWJ sequence is one step for the
//!   cursor and one delete), with display widths from `unicode-width`, the
//!   editing operations a terminal field needs, and the horizontal scroll
//!   window that keeps the cursor visible in a narrow field.
//! - [`TextInputState`]: a buffer plus whether it is a secret, and
//!   [`TextInputState::handle_key`], which turns a key into a
//!   [`TextInputOutcome`] the host reacts to.
//! - [`TextInput`]: paints a state: label, placeholder, a cursor cell, clip
//!   marks where the text runs past the field, and a note underneath (an
//!   error, a reason it is disabled, or help).
//!
//! Editing keys follow the Codewhale engine's composer and its automation
//! editor field (`crates/tui/src/tui/app/composer.rs`, `views/automations/
//! editor.rs`, `Hmbown/CodeWhale`): a *word* is a run of non-whitespace, so
//! `Ctrl+W` takes `/usr/local/bin` whole, as a shell's `unix-word-rubout`
//! does.
//!
//! | Key | Does |
//! |---|---|
//! | characters | insert (on Windows an AltGr chord inserts; `Alt+<non-ASCII>` inserts, as macOS Option does) |
//! | `Backspace`, `Ctrl+H` / `Delete` | delete the grapheme before / after the cursor |
//! | `Ctrl+W`, `Alt+Backspace`, `Ctrl+Backspace` | delete the word before the cursor |
//! | `Alt+D`, `Alt+Delete`, `Ctrl+Delete` | delete the word after the cursor |
//! | `Ctrl+U` / `Ctrl+K` | delete to the start / end of the line |
//! | `←` `→`, `Ctrl+B` `Ctrl+F` | move by grapheme |
//! | `Alt+←` `Alt+→`, `Ctrl+←` `Ctrl+→`, `Alt+B` `Alt+F` | move by word |
//! | `Home` `End`, `Ctrl+A` `Ctrl+E` | start / end of the line |
//! | `Enter` / `Esc` | [`TextInputOutcome::Submitted`] / [`TextInputOutcome::Cancelled`] |
//!
//! A secret field ([`TextInputState::secret`]) paints one mask cell per
//! grapheme, never the last character typed, and its word keys act on the
//! whole line, so the shape of the secret (where its spaces are) does not
//! leak through the cursor either.
//!
//! The kit paints the cursor itself, as a reversed cell, so a field works
//! with no hardware cursor. A host that wants the terminal's own cursor
//! calls [`TextInput::cursor_position`].

use std::{
    borrow::Cow,
    fmt,
    sync::atomic::{AtomicUsize, Ordering},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Modifier, Style},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{Paint, Role, Theme, glyphs, keys, text};

/// The prompt mark of the focused field. Its ASCII form is `>`.
const PROMPT: &str = "›";
/// Cells the prompt and its space take before the text.
const PROMPT_CELLS: u16 = 2;
/// Shown in an empty, unfocused field that has no placeholder.
const EMPTY: &str = "—";
/// A secret's mask, one cell per grapheme. ASCII-safe terminals get `*`: the
/// charter's fallback for `•` is a lone `.`, which reads as punctuation.
const MASK: &str = "•";
const MASK_ASCII: &str = "*";
/// A field narrower than this has no room for clip marks.
const MIN_CELLS_FOR_MARKS: usize = 4;
/// Longest note (an error, a reason) under a field, in rows.
const MAX_NOTE_ROWS: usize = 3;

// ---------------------------------------------------------------------------
// LineBuffer
// ---------------------------------------------------------------------------

/// One line of text and a cursor, edited by grapheme.
///
/// The cursor is always between graphemes (a combining sequence or an emoji
/// ZWJ sequence is never split). Text that goes in is made safe first
/// ([`LineBuffer::sanitize`]), so the buffer never holds a newline, a control
/// character, a bidi override or an invisible zero-width cluster.
///
/// Every editing method returns whether it changed anything, so a host (or
/// [`TextInputState::handle_key`]) can tell an edit from a no-op at a
/// boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineBuffer {
    text: String,
    /// Byte offset, always on a grapheme boundary.
    cursor: usize,
    /// Most graphemes the buffer will hold.
    limit: usize,
}

impl Default for LineBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl LineBuffer {
    /// The default cap on a line, in graphemes: generous for a paste, and a
    /// guard against one that is not.
    pub const DEFAULT_LIMIT: usize = 65_536;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            limit: Self::DEFAULT_LIMIT,
        }
    }

    /// A buffer holding `text` (made safe, one line), cursor at the end.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        let mut buffer = Self::new();
        buffer.insert_str(text);
        buffer
    }

    /// Cap the line at `graphemes`; text past the cap is cut now and refused
    /// later.
    #[must_use]
    pub fn with_limit(mut self, graphemes: usize) -> Self {
        self.limit = graphemes;
        if self.len() > graphemes {
            let cut = self
                .text
                .grapheme_indices(true)
                .nth(graphemes)
                .map_or(self.text.len(), |(at, _)| at);
            self.text.truncate(cut);
            self.cursor = self.cursor.min(cut);
        }
        self
    }

    /// `text` as a line: no control characters (so no newline, tab or escape),
    /// no bidi overrides ([`text::display_safe`]), no line or paragraph
    /// separators, and no cluster that draws nothing (a lone zero-width space
    /// or joiner would be invisible text in a field). Combining marks and
    /// ZWJ sequences that belong to a visible character stay.
    #[must_use]
    pub fn sanitize(text: &str) -> String {
        text::display_safe(text)
            .graphemes(true)
            .filter(|g| !g.contains(['\u{2028}', '\u{2029}']) && text::width(g) > 0)
            .collect()
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Length in graphemes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.text.graphemes(true).count()
    }

    /// Display width of the whole line, in cells.
    #[must_use]
    pub fn width(&self) -> usize {
        text::width(&self.text)
    }

    /// The cursor, as a grapheme index (`0..=len`).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.text[..self.cursor].graphemes(true).count()
    }

    /// The cursor, as a byte offset into [`LineBuffer::text`].
    #[must_use]
    pub const fn cursor_byte(&self) -> usize {
        self.cursor
    }

    /// Move the cursor to grapheme `index` (clamped to the end).
    pub fn set_cursor(&mut self, index: usize) {
        self.cursor = self
            .text
            .grapheme_indices(true)
            .nth(index)
            .map_or(self.text.len(), |(at, _)| at);
    }

    #[must_use]
    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }

    #[must_use]
    pub fn after_cursor(&self) -> &str {
        &self.text[self.cursor..]
    }

    pub fn graphemes(&self) -> impl Iterator<Item = &str> {
        self.text.graphemes(true)
    }

    /// Display width of each grapheme, or 1 each when `masked`.
    #[must_use]
    pub fn widths(&self, masked: bool) -> Vec<usize> {
        self.graphemes()
            .map(|g| if masked { 1 } else { text::width(g) })
            .collect()
    }

    /// Replace the line (made safe), cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        self.text.clear();
        self.cursor = 0;
        self.insert_str(text);
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    // -- inserting ----------------------------------------------------------

    pub fn insert_char(&mut self, c: char) -> bool {
        let mut utf8 = [0; 4];
        self.insert_str(c.encode_utf8(&mut utf8))
    }

    /// Insert `text` at the cursor: a typed character, or a paste. The text
    /// is made safe in context ([`LineBuffer::sanitize`]): a typed combining
    /// mark or joiner may extend a visible grapheme already in the line.
    /// A pasted newline is dropped, not turned into a space, and a paste
    /// that would pass the limit is cut without removing existing text.
    pub fn insert_str(&mut self, text: &str) -> bool {
        let safe = text::display_safe(text);
        if safe.is_empty() {
            return false;
        }
        let prefix = &self.text[..self.cursor];
        let clean_prefix = Self::sanitize(&format!("{prefix}{safe}"));
        // The stored prefix is already safe. Sanitize input beside it so
        // extensions survive while invisible-only clusters do not consume
        // the paste budget or hide later visible text.
        let clean = &clean_prefix[prefix.len()..];
        // At each insertion boundary a cluster can join its neighbor. Keep
        // two extra incoming clusters so extending a full line is possible;
        // then enforce the cap on the combined, sanitized result.
        let room = self.limit.saturating_sub(self.len()).saturating_add(2);
        let mut end = clean
            .grapheme_indices(true)
            .nth(room)
            .map_or(clean.len(), |(at, _)| at);
        while end > 0 {
            let insertion = &clean[..end];
            let mut candidate = self.text.clone();
            candidate.insert_str(self.cursor, insertion);
            let cursor = Self::sanitize(&candidate[..self.cursor + insertion.len()]).len();
            let candidate = Self::sanitize(&candidate);
            if candidate.graphemes(true).count() <= self.limit {
                if candidate == self.text {
                    return false;
                }
                self.text = candidate;
                self.cursor = cursor;
                self.snap_cursor();
                return true;
            }
            end = insertion
                .grapheme_indices(true)
                .next_back()
                .map_or(0, |(at, _)| at);
        }
        false
    }

    /// Typed text can join the grapheme after the cursor (`e` before a
    /// combining mark): keep the cursor between graphemes.
    fn snap_cursor(&mut self) {
        let cursor = self.cursor.min(self.text.len());
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(at, _)| at)
            .find(|at| *at >= cursor)
            .unwrap_or(self.text.len());
    }

    // -- boundaries ---------------------------------------------------------

    fn prev_boundary(&self, from: usize) -> usize {
        self.text[..from]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(at, _)| at)
    }

    fn next_boundary(&self, from: usize) -> usize {
        from + self.text[from..].graphemes(true).next().map_or(0, str::len)
    }

    /// Start of the word before `from`: skip whitespace, then the word.
    fn word_start_before(&self, from: usize) -> usize {
        let mut at = from;
        let mut back = self.text[..from].grapheme_indices(true).rev().peekable();
        while let Some((start, _)) = back.next_if(|(_, g)| is_space(g)) {
            at = start;
        }
        while let Some((start, _)) = back.next_if(|(_, g)| !is_space(g)) {
            at = start;
        }
        at
    }

    /// Start of the next word after `from`: skip the rest of this word, then
    /// the whitespace. At the last word this is the end of the line.
    fn next_word_start(&self, from: usize) -> usize {
        let mut at = from;
        let mut ahead = self.text[from..].grapheme_indices(true).peekable();
        while let Some((start, g)) = ahead.next_if(|(_, g)| !is_space(g)) {
            at = from + start + g.len();
        }
        while let Some((start, g)) = ahead.next_if(|(_, g)| is_space(g)) {
            at = from + start + g.len();
        }
        at
    }

    /// End of the word at or after `from`: skip whitespace, then the word.
    fn word_end_after(&self, from: usize) -> usize {
        let mut at = from;
        let mut ahead = self.text[from..].grapheme_indices(true).peekable();
        while let Some((start, g)) = ahead.next_if(|(_, g)| is_space(g)) {
            at = from + start + g.len();
        }
        while let Some((start, g)) = ahead.next_if(|(_, g)| !is_space(g)) {
            at = from + start + g.len();
        }
        at
    }

    fn remove(&mut self, from: usize, to: usize) -> bool {
        if from >= to {
            return false;
        }
        self.text.replace_range(from..to, "");
        self.cursor = from;
        // Removing a separator can join neighboring regional indicators
        // into one flag. The old byte boundary may now be inside a grapheme.
        self.snap_cursor();
        true
    }

    // -- deleting -----------------------------------------------------------

    /// Delete the grapheme before the cursor (all of a combining or ZWJ
    /// sequence).
    pub fn backspace(&mut self) -> bool {
        let from = self.prev_boundary(self.cursor);
        self.remove(from, self.cursor)
    }

    /// Delete the grapheme after the cursor.
    pub fn delete(&mut self) -> bool {
        let to = self.next_boundary(self.cursor);
        self.remove(self.cursor, to)
    }

    /// `Ctrl+W`, `Alt+Backspace`: delete back to the start of the word.
    pub fn delete_word_back(&mut self) -> bool {
        let from = self.word_start_before(self.cursor);
        self.remove(from, self.cursor)
    }

    /// `Alt+D`: delete forward to the end of the next word.
    pub fn delete_word_forward(&mut self) -> bool {
        let to = self.word_end_after(self.cursor);
        self.remove(self.cursor, to)
    }

    /// `Ctrl+U`: delete everything before the cursor.
    pub fn kill_to_start(&mut self) -> bool {
        self.remove(0, self.cursor)
    }

    /// `Ctrl+K`: delete everything after the cursor.
    pub fn kill_to_end(&mut self) -> bool {
        let end = self.text.len();
        self.remove(self.cursor, end)
    }

    // -- moving -------------------------------------------------------------

    fn move_to(&mut self, to: usize) -> bool {
        let moved = to != self.cursor;
        self.cursor = to;
        moved
    }

    pub fn move_left(&mut self) -> bool {
        let to = self.prev_boundary(self.cursor);
        self.move_to(to)
    }

    pub fn move_right(&mut self) -> bool {
        let to = self.next_boundary(self.cursor);
        self.move_to(to)
    }

    /// To the start of the word before the cursor.
    pub fn word_left(&mut self) -> bool {
        let to = self.word_start_before(self.cursor);
        self.move_to(to)
    }

    /// To the start of the next word (the end of the line after the last).
    pub fn word_right(&mut self) -> bool {
        let to = self.next_word_start(self.cursor);
        self.move_to(to)
    }

    pub fn home(&mut self) -> bool {
        self.move_to(0)
    }

    pub fn end(&mut self) -> bool {
        let end = self.text.len();
        self.move_to(end)
    }

    // -- scrolling ----------------------------------------------------------

    /// The slice of the line to show in `avail` cells with the cursor in
    /// view, scrolling as little as possible from `prev_start` (the first
    /// grapheme shown last time). `masked` counts one cell per grapheme.
    #[must_use]
    pub fn window(&self, avail: usize, prev_start: usize, masked: bool) -> LineWindow {
        line_window_of(&self.widths(masked), Some(self.cursor()), prev_start, avail)
    }

    /// The start of the line in `avail` cells, with no cursor: how an
    /// unfocused field shows its text.
    #[must_use]
    pub fn head_window(&self, avail: usize, masked: bool) -> LineWindow {
        line_window_of(&self.widths(masked), None, 0, avail)
    }
}

fn is_space(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}

/// Which graphemes of a line a field shows, and where the cursor lands.
///
/// All indices are graphemes. `start..end` is what is drawn; when text is
/// cut off on a side a clip mark takes one cell there (none in a field too
/// narrow to spare it: `left_mark` and `right_mark` say which marks are
/// drawn). Whenever the field is wide enough for its cursor, `cursor_col`
/// is `Some`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineWindow {
    pub start: usize,
    pub end: usize,
    /// Text before `start` is hidden.
    pub left_clipped: bool,
    /// Text from `end` on is hidden.
    pub right_clipped: bool,
    /// A clip mark is drawn in the first cell.
    pub left_mark: bool,
    /// A clip mark is drawn in the last cell.
    pub right_mark: bool,
    /// The cell, counted from the field's left edge, the cursor sits in; one
    /// past the last grapheme when it is at the end of the line.
    pub cursor_col: Option<usize>,
}

/// The window over graphemes of the given display `widths`: the pure core of
/// [`LineBuffer::window`]. With a `cursor` it scrolls as little as possible
/// from `prev_start` to keep it visible, and fills spare room on the left
/// rather than leave a gap after the text; without one it starts at 0.
#[must_use]
pub fn line_window_of(
    widths: &[usize],
    cursor: Option<usize>,
    prev_start: usize,
    avail: usize,
) -> LineWindow {
    let n = widths.len();
    let mark = usize::from(avail >= MIN_CELLS_FOR_MARKS);
    let span = |a: usize, b: usize| widths[a..b].iter().sum::<usize>();
    let cursor = cursor.map(|c| c.min(n));
    // At the end of the line the cursor is a cell past the last grapheme.
    let cursor_cell = usize::from(cursor == Some(n));

    let mut start = 0;
    if let Some(c) = cursor {
        // The leftmost start that still shows the cursor: walk back from it
        // while it, what follows it up to a right mark, and the left mark fit.
        let cursor_w = if c < n { widths[c] } else { 1 };
        let right = if c + 1 < n { mark } else { 0 };
        let mut lowest = c;
        while lowest > 0 {
            let left = usize::from(lowest - 1 > 0) * mark;
            if span(lowest - 1, c) + cursor_w + right + left > avail {
                break;
            }
            lowest -= 1;
        }
        start = prev_start.clamp(lowest, c);
        // Spare room: if the whole tail fits, show more of the head. A cell
        // stays reserved for a cursor at the end of the line even while the
        // cursor is elsewhere, so the text does not shift by one every time
        // the cursor crosses the end.
        let tail_fits = |s: usize| span(s, n) + 1 + usize::from(s > 0) * mark <= avail;
        if tail_fits(start) {
            while start > 0 && tail_fits(start - 1) {
                start -= 1;
            }
        }
    }

    let left = usize::from(start > 0) * mark;
    let budget = avail.saturating_sub(left);
    let (end, right_clipped) = if span(start, n) + cursor_cell <= budget {
        (n, false)
    } else {
        let budget = budget.saturating_sub(mark);
        let (mut end, mut used) = (start, 0);
        while end < n && used + widths[end] <= budget {
            used += widths[end];
            end += 1;
        }
        (end, end < n)
    };
    let cursor_col = cursor.and_then(|c| {
        let col = left + span(start, c.min(end).max(start));
        let on_a_drawn_grapheme = c >= start && c < end;
        let at_the_end = c == n && end == n && col < avail;
        (on_a_drawn_grapheme || at_the_end).then_some(col)
    });
    LineWindow {
        start,
        end,
        left_clipped: start > 0,
        right_clipped,
        left_mark: start > 0 && mark == 1,
        right_mark: right_clipped && mark == 1,
        cursor_col,
    }
}

// ---------------------------------------------------------------------------
// TextInputState
// ---------------------------------------------------------------------------

/// What a key did to a [`TextInputState`]: the message a host reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextInputOutcome {
    /// The key means nothing to a field (or did nothing at a boundary: `←`
    /// at the start, `Backspace` on an empty line); the host may use it.
    Ignored,
    /// The text or the cursor changed; repaint.
    Changed,
    /// Enter.
    Submitted,
    /// Esc.
    Cancelled,
}

/// What [`TextInputState::apply`] did, a step finer than the outcome: the
/// form needs to tell an edit from a cursor move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyEffect {
    Nothing,
    Moved,
    Edited,
    Submit,
    Cancel,
}

/// Where the field was last scrolled to, so scrolling is sticky: moving the
/// cursor left inside the window does not shift the text.
///
/// [`TextInput`] paints from `&self` and records here what it painted. The
/// two cells are atomics so the state stays `Send + Sync`.
#[derive(Debug, Default)]
struct InputView {
    start: AtomicUsize,
    /// Cells the text area had last time; 0 until the first paint.
    avail: AtomicUsize,
}

impl Clone for InputView {
    fn clone(&self) -> Self {
        Self {
            start: AtomicUsize::new(self.start.load(Ordering::Relaxed)),
            avail: AtomicUsize::new(self.avail.load(Ordering::Relaxed)),
        }
    }
}

/// The state of one text input: its [`LineBuffer`], whether it is a secret,
/// and where it is scrolled to. Keys go in through
/// [`TextInputState::handle_key`] and [`TextInputState::paste`].
///
/// `Debug` never prints a secret's text.
#[derive(Clone, Default)]
pub struct TextInputState {
    buffer: LineBuffer,
    masked: bool,
    view: InputView,
}

impl fmt::Debug for TextInputState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("TextInputState");
        if self.masked {
            s.field("text", &"<hidden>");
        } else {
            s.field("text", &self.buffer.text())
                .field("cursor", &self.buffer.cursor());
        }
        s.field("masked", &self.masked).finish()
    }
}

impl TextInputState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A field holding `text`, cursor at the end.
    #[must_use]
    pub fn with_text(text: &str) -> Self {
        Self {
            buffer: LineBuffer::from_text(text),
            ..Self::default()
        }
    }

    /// A secret: painted as a mask, edited as one block.
    #[must_use]
    pub fn secret() -> Self {
        Self {
            masked: true,
            ..Self::default()
        }
    }

    /// Cap the line at `graphemes` ([`LineBuffer::with_limit`]).
    #[must_use]
    pub fn with_limit(mut self, graphemes: usize) -> Self {
        self.buffer = self.buffer.with_limit(graphemes);
        self
    }

    #[must_use]
    pub const fn is_masked(&self) -> bool {
        self.masked
    }

    #[must_use]
    pub fn text(&self) -> &str {
        self.buffer.text()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// The cursor, as a grapheme index.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.buffer.cursor()
    }

    #[must_use]
    pub const fn buffer(&self) -> &LineBuffer {
        &self.buffer
    }

    /// Edit the buffer directly (a host's own shortcut, a completion). The
    /// scroll catches up on the next key or paint.
    pub const fn buffer_mut(&mut self) -> &mut LineBuffer {
        &mut self.buffer
    }

    pub fn set_text(&mut self, text: &str) {
        self.buffer.set_text(text);
        self.follow();
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.view.start.store(0, Ordering::Relaxed);
    }

    /// Insert pasted text ([`LineBuffer::insert_str`]): one line, safe.
    pub fn paste(&mut self, text: &str) -> TextInputOutcome {
        if self.buffer.insert_str(text) {
            self.follow();
            TextInputOutcome::Changed
        } else {
            TextInputOutcome::Ignored
        }
    }

    /// The window a field `avail` cells wide shows now.
    #[must_use]
    pub fn window(&self, avail: usize) -> LineWindow {
        self.buffer
            .window(avail, self.view.start.load(Ordering::Relaxed), self.masked)
    }

    /// Tell the state how many cells the text area has, and scroll the
    /// cursor into it. [`TextInput`] does this each time it paints; a host
    /// calls it only to scroll before the first paint, or after a resize.
    pub fn scroll_into_view(&self, avail: usize) {
        self.view.avail.store(avail, Ordering::Relaxed);
        self.follow();
    }

    fn follow(&self) {
        let avail = self.view.avail.load(Ordering::Relaxed);
        if avail > 0 {
            self.view
                .start
                .store(self.window(avail).start, Ordering::Relaxed);
        }
    }

    /// Record what a paint showed, so the next key scrolls from there.
    fn remember(&self, start: usize, avail: usize) {
        self.view.start.store(start, Ordering::Relaxed);
        self.view.avail.store(avail, Ordering::Relaxed);
    }

    /// Apply a key press and say what happened (see the module's key table).
    /// Editing and movement accept held-key repeats. Enter/Esc require an
    /// unmodified initial press. Releases and every key a field has no use
    /// for (Tab, the arrows up and down, `Ctrl+C`, `Cmd` chords) are
    /// [`TextInputOutcome::Ignored`].
    pub fn handle_key(&mut self, key: KeyEvent) -> TextInputOutcome {
        match self.apply(key) {
            KeyEffect::Nothing => TextInputOutcome::Ignored,
            KeyEffect::Moved | KeyEffect::Edited => TextInputOutcome::Changed,
            KeyEffect::Submit => TextInputOutcome::Submitted,
            KeyEffect::Cancel => TextInputOutcome::Cancelled,
        }
    }

    pub(super) fn apply(&mut self, key: KeyEvent) -> KeyEffect {
        if key.kind == KeyEventKind::Release {
            return KeyEffect::Nothing;
        }
        let mods = key.modifiers;
        // `Cmd` chords belong to the host.
        if mods.contains(KeyModifiers::SUPER) {
            return KeyEffect::Nothing;
        }
        let (ctrl, alt) = (
            mods.contains(KeyModifiers::CONTROL),
            mods.contains(KeyModifiers::ALT),
        );
        let word = ctrl || alt;
        let effect = match key.code {
            KeyCode::Enter | KeyCode::Esc => {
                return if key.kind == KeyEventKind::Press && mods.is_empty() {
                    if key.code == KeyCode::Enter {
                        KeyEffect::Submit
                    } else {
                        KeyEffect::Cancel
                    }
                } else {
                    KeyEffect::Nothing
                };
            }
            KeyCode::Char(c) => {
                if keys::is_ctrl_h_backspace(&key) {
                    self.edit(LineBuffer::backspace)
                } else if ctrl && !alt {
                    match c.to_ascii_lowercase() {
                        'w' => self.edit_word_back(),
                        'u' => self.edit(LineBuffer::kill_to_start),
                        'k' => self.edit(LineBuffer::kill_to_end),
                        'a' => self.go(LineBuffer::home),
                        'e' => self.go(LineBuffer::end),
                        'b' => self.go(LineBuffer::move_left),
                        'f' => self.go(LineBuffer::move_right),
                        _ => KeyEffect::Nothing,
                    }
                } else if alt && !ctrl {
                    match c {
                        _ if !c.is_ascii() => self.edit_char(c),
                        'b' | 'B' => self.go_word_left(),
                        'f' | 'F' => self.go_word_right(),
                        'd' | 'D' => self.edit_word_forward(),
                        _ => KeyEffect::Nothing,
                    }
                } else if ctrl && alt {
                    // Windows reports AltGr as Ctrl+Alt.
                    if cfg!(windows) {
                        self.edit_char(c)
                    } else {
                        KeyEffect::Nothing
                    }
                } else {
                    self.edit_char(c)
                }
            }
            KeyCode::Backspace if word => self.edit_word_back(),
            KeyCode::Backspace => self.edit(LineBuffer::backspace),
            KeyCode::Delete if word => self.edit_word_forward(),
            KeyCode::Delete => self.edit(LineBuffer::delete),
            KeyCode::Left if word => self.go_word_left(),
            KeyCode::Left => self.go(LineBuffer::move_left),
            KeyCode::Right if word => self.go_word_right(),
            KeyCode::Right => self.go(LineBuffer::move_right),
            KeyCode::Home => self.go(LineBuffer::home),
            KeyCode::End => self.go(LineBuffer::end),
            _ => KeyEffect::Nothing,
        };
        if effect != KeyEffect::Nothing {
            self.follow();
        }
        effect
    }

    fn edit(&mut self, op: fn(&mut LineBuffer) -> bool) -> KeyEffect {
        if op(&mut self.buffer) {
            KeyEffect::Edited
        } else {
            KeyEffect::Nothing
        }
    }

    fn go(&mut self, op: fn(&mut LineBuffer) -> bool) -> KeyEffect {
        if op(&mut self.buffer) {
            KeyEffect::Moved
        } else {
            KeyEffect::Nothing
        }
    }

    fn edit_char(&mut self, c: char) -> KeyEffect {
        if self.buffer.insert_char(c) {
            KeyEffect::Edited
        } else {
            KeyEffect::Nothing
        }
    }

    // A secret has no visible words: its word keys act on the whole line.
    fn edit_word_back(&mut self) -> KeyEffect {
        self.edit(if self.masked {
            LineBuffer::kill_to_start
        } else {
            LineBuffer::delete_word_back
        })
    }

    fn edit_word_forward(&mut self) -> KeyEffect {
        self.edit(if self.masked {
            LineBuffer::kill_to_end
        } else {
            LineBuffer::delete_word_forward
        })
    }

    fn go_word_left(&mut self) -> KeyEffect {
        self.go(if self.masked {
            LineBuffer::home
        } else {
            LineBuffer::word_left
        })
    }

    fn go_word_right(&mut self) -> KeyEffect {
        self.go(if self.masked {
            LineBuffer::end
        } else {
            LineBuffer::word_right
        })
    }
}

// ---------------------------------------------------------------------------
// Painting: shared by TextInput and Form
// ---------------------------------------------------------------------------

/// The words a [`TextInput`] shows itself. English by default; the host
/// passes its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextInputWords {
    /// Shown under a disabled field that gave no reason of its own.
    pub disabled: Cow<'static, str>,
}

impl Default for TextInputWords {
    fn default() -> Self {
        Self {
            disabled: Cow::Borrowed("Disabled"),
        }
    }
}

/// A line or more of words under a field, with the mark that gives them
/// their meaning.
pub(super) struct FieldNote {
    pub mark: Option<(&'static str, Role)>,
    pub text: String,
    pub ink: Role,
}

impl FieldNote {
    /// `✕ message`: the mark carries the state, the words stay body ink.
    pub fn error(message: &str) -> Self {
        Self {
            mark: Some((glyphs::FAILED, Role::Danger)),
            text: message.to_owned(),
            ink: Role::Foreground,
        }
    }

    /// `· reason`: a field that cannot be edited says why.
    pub fn locked(reason: &str) -> Self {
        Self {
            mark: Some((glyphs::NEUTRAL, Role::Muted)),
            text: reason.to_owned(),
            ink: Role::Muted,
        }
    }

    /// Plain muted guidance, aligned with the text.
    pub fn help(text: &str) -> Self {
        Self {
            mark: None,
            text: text.to_owned(),
            ink: Role::Muted,
        }
    }
}

/// Break `text` into rows of at most `width` cells at spaces (a longer word
/// is cut between graphemes). Past `max_rows` the rest is joined onto the
/// last row, which the painter then cuts with `…`.
pub(super) fn wrap_field_note(text: &str, width: usize, max_rows: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows: Vec<String> = Vec::new();
    let (mut row, mut used) = (String::new(), 0);
    for word in text.split_whitespace() {
        let w = text::width(word);
        if w > width {
            if !row.is_empty() {
                rows.push(std::mem::take(&mut row));
            }
            used = 0;
            for g in word.graphemes(true) {
                let gw = text::width(g);
                if used + gw > width && !row.is_empty() {
                    rows.push(std::mem::take(&mut row));
                    used = 0;
                }
                row.push_str(g);
                used += gw;
            }
        } else if row.is_empty() {
            row.push_str(word);
            used = w;
        } else if used + 1 + w <= width {
            row.push(' ');
            row.push_str(word);
            used += 1 + w;
        } else {
            rows.push(std::mem::replace(&mut row, word.to_owned()));
            used = w;
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    if rows.len() > max_rows && max_rows > 0 {
        let tail = rows.split_off(max_rows - 1).join(" ");
        rows.push(tail);
    }
    rows
}

/// Everything around a field's text: label above, prompt and well, note
/// below. The body is painted by the caller into the text area.
pub(super) struct FieldChrome<'a> {
    pub label: Option<&'a str>,
    pub focused: bool,
    /// Disabled or read-only: no well, quiet ink.
    pub dimmed: bool,
    pub note: Option<FieldNote>,
}

/// Where each part of a [`FieldChrome`] landed.
pub(super) struct FieldPlaced {
    pub label: Option<Rect>,
    pub input: Rect,
    /// The text area of the input row, after the prompt.
    pub value: Rect,
    pub notes: Vec<(Rect, String)>,
}

impl FieldChrome<'_> {
    fn note_rows(&self, width: u16) -> Vec<String> {
        self.note.as_ref().map_or_else(Vec::new, |note| {
            let width = usize::from(width.saturating_sub(PROMPT_CELLS));
            wrap_field_note(&text::display_safe(&note.text), width, MAX_NOTE_ROWS)
        })
    }

    /// Rows this field wants at `width`: label, input, note.
    pub fn height(&self, width: u16) -> u16 {
        let rows = usize::from(self.label.is_some()) + 1 + self.note_rows(width).len();
        u16::try_from(rows).unwrap_or(u16::MAX)
    }

    /// Lay the parts out in `area`. Too short: the note shrinks to one row
    /// (the first row of an error is the one that matters), then the label
    /// goes, then the note.
    pub fn place(&self, area: Rect) -> Option<FieldPlaced> {
        if area.is_empty() {
            return None;
        }
        let notes = self.note_rows(area.width);
        let rows = usize::from(area.height);
        let mut show_label = self.label.is_some();
        let mut shown_notes = notes.len();
        let need = |label: bool, notes: usize| usize::from(label) + 1 + notes;
        while need(show_label, shown_notes) > rows && shown_notes > 1 {
            shown_notes -= 1;
        }
        if need(show_label, shown_notes) > rows {
            show_label = false;
        }
        shown_notes = shown_notes.min(rows - 1);

        let mut y = area.y;
        let row = |y: u16| Rect {
            y,
            height: 1,
            ..area
        };
        let label = show_label.then(|| {
            y += 1;
            row(y - 1)
        });
        let input = row(y);
        y += 1;
        let mut placed_notes = Vec::new();
        for text in notes.into_iter().take(shown_notes) {
            placed_notes.push((row(y), text));
            y += 1;
        }
        let value = Rect {
            x: input.x.saturating_add(PROMPT_CELLS.min(input.width)),
            width: input.width.saturating_sub(PROMPT_CELLS),
            ..input
        };
        Some(FieldPlaced {
            label,
            input,
            value,
            notes: placed_notes,
        })
    }

    /// Paint everything but the text, then hand the text area to `body`.
    pub fn paint(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        body: impl FnOnce(Rect, &mut Buffer),
    ) {
        let area = area.intersection(buf.area);
        let Some(placed) = self.place(area) else {
            return;
        };
        let ascii = theme.ascii();

        if let (Some(rect), Some(label)) = (placed.label, self.label) {
            let label = text::display_safe(label);
            let shown = text::truncate(&label, usize::from(rect.width), ascii);
            let style = if self.dimmed {
                theme.fg(Role::Muted).add_modifier(Modifier::DIM)
            } else if self.focused {
                theme.fg(Role::Foreground).add_modifier(Modifier::BOLD)
            } else {
                theme.fg(Role::Foreground)
            };
            buf.set_stringn(rect.x, rect.y, shown, usize::from(rect.width), style);
        }

        // The well: where grounds paint it lifts the field; where they do
        // not, the prompt and the cursor carry focus.
        if !self.dimmed {
            let ground = if self.focused {
                Role::Selected
            } else {
                Role::Surface
            };
            buf.set_style(placed.input, theme.bg(ground));
        }
        if self.focused {
            buf.set_stringn(
                placed.input.x,
                placed.input.y,
                glyphs::pick(PROMPT, ascii),
                usize::from(placed.input.width),
                theme.fg(Role::Accent),
            );
        }
        if placed.value.width > 0 {
            body(placed.value, buf);
        }

        if let Some(note) = &self.note {
            let width = usize::from(area.width.saturating_sub(PROMPT_CELLS));
            for (i, (rect, row)) in placed.notes.iter().enumerate() {
                if i == 0
                    && let Some((mark, role)) = note.mark
                {
                    buf.set_stringn(
                        rect.x,
                        rect.y,
                        glyphs::pick(mark, ascii),
                        usize::from(rect.width),
                        theme.fg(role),
                    );
                }
                let shown = text::truncate(row, width, ascii);
                buf.set_stringn(
                    rect.x.saturating_add(PROMPT_CELLS),
                    rect.y,
                    shown,
                    width,
                    theme.fg(note.ink),
                );
            }
        }
    }
}

/// The clip mark at the edge of a field's text.
fn clip_mark(left: bool, ascii: bool) -> &'static str {
    match (ascii, left) {
        (false, _) => glyphs::ELLIPSIS,
        // A lone `.` at the edge of editable text reads as typed text.
        (true, true) => "<",
        (true, false) => ">",
    }
}

/// Paint the text of a field into `area` (one row): the text or the mask, or
/// the placeholder when empty; clip marks; the cursor cell when focused.
pub(super) fn paint_input_value(
    state: &TextInputState,
    placeholder: Option<&str>,
    focused: bool,
    dimmed: bool,
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
) {
    if area.is_empty() {
        return;
    }
    let ascii = theme.ascii();
    let avail = usize::from(area.width);
    let ink = if dimmed {
        theme.fg(Role::Muted).add_modifier(Modifier::DIM)
    } else {
        theme.fg(Role::Foreground)
    };
    let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
    let at = |col: usize| {
        area.x
            .saturating_add(u16::try_from(col).unwrap_or(u16::MAX))
    };

    if state.is_empty() {
        let hint = theme.fg(if dimmed { Role::Dim } else { Role::Hint });
        let placeholder = text::display_safe(placeholder.unwrap_or_default());
        // An empty field with nothing to say still shows where it is: a dash
        // (the design's `—`), so it does not read as a gap in the form.
        let placeholder = if placeholder.is_empty() && !focused {
            Cow::Borrowed(glyphs::pick(EMPTY, ascii))
        } else {
            placeholder
        };
        let shown = text::truncate(&placeholder, avail, ascii);
        buf.set_stringn(area.x, area.y, &shown, avail, hint);
        if focused {
            // The cursor sits on the placeholder's first cell.
            let first = shown
                .graphemes(true)
                .next()
                .map_or(1, |g| text::width(g).max(1));
            let cell = Rect {
                width: u16::try_from(first.min(avail)).unwrap_or(1),
                height: 1,
                ..area
            };
            buf.set_style(cell, cursor_style);
        }
        return;
    }

    let masked = state.is_masked();
    let window = if focused {
        let window = state.window(avail);
        state.remember(window.start, avail);
        window
    } else {
        state.buffer().head_window(avail, masked)
    };
    let mask = if ascii { MASK_ASCII } else { MASK };
    let mut col = usize::from(window.left_mark);
    for g in state
        .buffer()
        .graphemes()
        .skip(window.start)
        .take(window.end - window.start)
    {
        let (shown, w) = if masked {
            (mask, 1)
        } else {
            (g, text::width(g))
        };
        buf.set_stringn(at(col), area.y, shown, usize::from(area.width), ink);
        col += w;
    }
    let mark_style = theme.fg(Role::Muted);
    if window.left_mark {
        buf.set_stringn(area.x, area.y, clip_mark(true, ascii), 1, mark_style);
    }
    if window.right_mark {
        buf.set_stringn(
            at(avail - 1),
            area.y,
            clip_mark(false, ascii),
            1,
            mark_style,
        );
    }
    if focused && let Some(cursor_col) = window.cursor_col {
        let on = state.buffer().cursor();
        let w = if masked || on >= state.buffer().len() {
            1
        } else {
            text::width(state.buffer().graphemes().nth(on).unwrap_or(" ")).max(1)
        };
        let cell = Rect {
            x: at(cursor_col),
            width: u16::try_from(w.min(avail - cursor_col)).unwrap_or(1),
            height: 1,
            ..area
        };
        buf.set_style(cell, cursor_style);
    }
}

// ---------------------------------------------------------------------------
// TextInput
// ---------------------------------------------------------------------------

/// Paints a [`TextInputState`]:
///
/// ```text
///   Port
///   › 8080▏
///   ✕ Enter a port from 1 to 65535.
/// ```
///
/// Every state has a mark and a word: focus is the `›` prompt and the cursor
/// cell; an invalid field is `✕` and the reason; a disabled one is `·` and
/// its reason (or [`TextInputWords::disabled`]); a secret is its mask.
/// Where grounds paint, the field sits in a well (`Surface`, `Selected` when
/// focused).
#[derive(Clone, Debug)]
pub struct TextInput<'a> {
    state: &'a TextInputState,
    label: Option<Cow<'static, str>>,
    placeholder: Option<Cow<'static, str>>,
    focused: bool,
    disabled: Option<Cow<'static, str>>,
    error: Option<Cow<'static, str>>,
    help: Option<Cow<'static, str>>,
    words: TextInputWords,
}

impl<'a> TextInput<'a> {
    #[must_use]
    pub fn new(state: &'a TextInputState) -> Self {
        Self {
            state,
            label: None,
            placeholder: None,
            focused: false,
            disabled: None,
            error: None,
            help: None,
            words: TextInputWords::default(),
        }
    }

    /// A line above the field naming it.
    #[must_use]
    pub fn label(mut self, label: impl Into<Cow<'static, str>>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Hint ink shown while the field is empty.
    #[must_use]
    pub fn placeholder(mut self, placeholder: impl Into<Cow<'static, str>>) -> Self {
        self.placeholder = Some(placeholder.into());
        self
    }

    #[must_use]
    pub const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Disable the field and say why. An empty reason shows
    /// [`TextInputWords::disabled`].
    #[must_use]
    pub fn disabled(mut self, reason: impl Into<Cow<'static, str>>) -> Self {
        self.disabled = Some(reason.into());
        self
    }

    /// Mark the field invalid: `✕` and the reason under it.
    #[must_use]
    pub fn error(mut self, message: impl Into<Cow<'static, str>>) -> Self {
        self.error = Some(message.into());
        self
    }

    /// Muted guidance under the field, shown while there is no error.
    #[must_use]
    pub fn help(mut self, help: impl Into<Cow<'static, str>>) -> Self {
        self.help = Some(help.into());
        self
    }

    #[must_use]
    pub fn words(mut self, words: &TextInputWords) -> Self {
        self.words = words.clone();
        self
    }

    fn chrome(&self) -> FieldChrome<'_> {
        let note = if let Some(error) = &self.error {
            Some(FieldNote::error(error))
        } else if let Some(reason) = &self.disabled {
            let reason = if reason.trim().is_empty() {
                &self.words.disabled
            } else {
                reason
            };
            Some(FieldNote::locked(reason))
        } else {
            self.help.as_deref().map(FieldNote::help)
        };
        FieldChrome {
            label: self.label.as_deref(),
            focused: self.focused && self.disabled.is_none(),
            dimmed: self.disabled.is_some(),
            note,
        }
    }

    /// Where the terminal's own cursor goes when this field is painted in
    /// `area`: the cell the painted cursor is in. `None` unless focused and
    /// the cursor is visible.
    #[must_use]
    pub fn cursor_position(&self, area: Rect) -> Option<Position> {
        let chrome = self.chrome();
        if !chrome.focused {
            return None;
        }
        let placed = chrome.place(area)?;
        let col = if self.state.is_empty() {
            0
        } else {
            self.state
                .window(usize::from(placed.value.width))
                .cursor_col?
        };
        let x = placed
            .value
            .x
            .saturating_add(u16::try_from(col).unwrap_or(u16::MAX));
        (placed.value.width > 0 && x < placed.value.right())
            .then_some(Position::new(x, placed.value.y))
    }
}

impl Paint for TextInput<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let chrome = self.chrome();
        let (focused, dimmed) = (chrome.focused, chrome.dimmed);
        chrome.paint(area, buf, theme, |value, buf| {
            paint_input_value(
                self.state,
                self.placeholder.as_deref(),
                focused,
                dimmed,
                value,
                buf,
                theme,
            );
        });
    }

    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        self.chrome().height(width)
    }
}

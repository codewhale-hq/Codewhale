//! Diff: a diff the caller has already parsed, painted so its meaning never
//! depends on color.
//!
//! The kit does not diff and does not highlight. The caller hands in
//! [`DiffLine`]s (or [`parse_unified`] reads unified-diff text into them),
//! optionally with intraline [`DiffLine::emphasis`] ranges its own word-diff
//! produced and a [`DiffHighlight`] hook from its own highlighter. What the
//! kit owns is the paint:
//!
//! - every changed line carries `+` or `−` (`-` in ASCII) in its own
//!   column, so nothing depends on a ground;
//! - where grounds paint (truecolor, 256 colors) the two tint roles sit
//!   behind the whole row, under `Foreground`, `Muted` and the line's own hue
//!   only (the inks the tints are audited for); at 16 colors and `NO_COLOR`
//!   the tint is dropped and emphasis falls back to bold and underline;
//! - long lines are cut with a continuation mark or wrapped (a setting);
//! - tabs expand, and control and bidi characters are removed by
//!   [`text::display_safe`] before anything is measured.
//!
//! Modelled on the engine's `crates/tui/src/tui/diff_render.rs`
//! (`Hmbown/CodeWhale` `58b1dd3dd`).

use std::{borrow::Cow, ops::Range};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{Paint, Role, Theme, glyphs, text};

/// What a [`DiffLine`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffKind {
    Context,
    Added,
    Removed,
    /// `@@ -12,7 +12,8 @@ fn main()`.
    HunkHeader,
    /// `diff --git`, `index`, `---`, `+++`, `rename from`, `Binary files`.
    FileHeader,
    /// `\ No newline at end of file`: about the line above it.
    Note,
}

/// One line of a parsed diff. `text` excludes the leading `+`, `-` or space;
/// `emphasis` holds byte ranges into `text` the caller's word-diff marked as
/// the part that changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffLine<'a> {
    pub kind: DiffKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: &'a str,
    pub emphasis: &'a [Range<usize>],
}

impl<'a> DiffLine<'a> {
    #[must_use]
    pub const fn new(kind: DiffKind, text: &'a str) -> Self {
        Self {
            kind,
            old_no: None,
            new_no: None,
            text,
            emphasis: &[],
        }
    }

    #[must_use]
    pub const fn context(old_no: u32, new_no: u32, text: &'a str) -> Self {
        Self {
            old_no: Some(old_no),
            new_no: Some(new_no),
            ..Self::new(DiffKind::Context, text)
        }
    }

    #[must_use]
    pub const fn added(new_no: u32, text: &'a str) -> Self {
        Self {
            new_no: Some(new_no),
            ..Self::new(DiffKind::Added, text)
        }
    }

    #[must_use]
    pub const fn removed(old_no: u32, text: &'a str) -> Self {
        Self {
            old_no: Some(old_no),
            ..Self::new(DiffKind::Removed, text)
        }
    }

    #[must_use]
    pub const fn hunk(text: &'a str) -> Self {
        Self::new(DiffKind::HunkHeader, text)
    }

    #[must_use]
    pub const fn file(text: &'a str) -> Self {
        Self::new(DiffKind::FileHeader, text)
    }

    /// The byte ranges of `text` to emphasise.
    #[must_use]
    pub const fn emphasis(mut self, emphasis: &'a [Range<usize>]) -> Self {
        self.emphasis = emphasis;
        self
    }
}

/// `(added, removed)` line counts, for a summary beside the diff.
#[must_use]
pub fn diff_counts(lines: &[DiffLine<'_>]) -> (usize, usize) {
    let count = |kind| lines.iter().filter(|l| l.kind == kind).count();
    (count(DiffKind::Added), count(DiffKind::Removed))
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// The numbers of a hunk header: `(old_start, old_len, new_start, new_len)`.
fn hunk_numbers(header: &str) -> Option<(u32, u32, u32, u32)> {
    let mut parts = header.strip_prefix("@@")?.split_whitespace();
    let span = |part: &str, sign: char| -> Option<(u32, u32)> {
        let body = part.strip_prefix(sign)?;
        let (start, len) = match body.split_once(',') {
            Some((start, len)) => (start, len),
            None => (body, "1"),
        };
        Some((start.parse().ok()?, len.parse().ok()?))
    };
    let (old_start, old_len) = span(parts.next()?, '-')?;
    let (new_start, new_len) = span(parts.next()?, '+')?;
    Some((old_start, old_len, new_start, new_len))
}

/// Read unified-diff text (`git diff`, `diff -u`, a patch file) into lines.
///
/// Inside a hunk the header's counts decide what a line is, so a removed
/// line that reads `-- note` (`--- note` in the diff) is a removed line, not
/// a file header. Renames, copies, mode changes and binary notices are
/// [`DiffKind::FileHeader`]; `\ No newline at end of file` is
/// [`DiffKind::Note`]. CRLF line endings are dropped, blank lines between
/// files are skipped, and the result borrows from `diff`.
#[must_use]
pub fn parse_unified(diff: &str) -> Vec<DiffLine<'_>> {
    let mut out = Vec::new();
    let (mut old_left, mut new_left) = (0u32, 0u32);
    let (mut old_no, mut new_no): (Option<u32>, Option<u32>) = (None, None);
    for raw in diff.lines() {
        if raw.starts_with('\\') {
            out.push(DiffLine::new(DiffKind::Note, raw));
            continue;
        }
        let in_hunk = old_left > 0 || new_left > 0;
        let header = !in_hunk && (raw.starts_with("--- ") || raw.starts_with("+++ "));
        if !header && !raw.starts_with("@@") && !raw.starts_with("diff ") {
            let body = match raw.as_bytes().first() {
                Some(b'+') => Some((DiffKind::Added, &raw[1..])),
                Some(b'-') => Some((DiffKind::Removed, &raw[1..])),
                Some(b' ') => Some((DiffKind::Context, &raw[1..])),
                // Editors strip the trailing space of an empty context line.
                None if in_hunk => Some((DiffKind::Context, "")),
                _ => None,
            };
            if let Some((kind, text)) = body {
                let (old, new) = match kind {
                    DiffKind::Added => (None, new_no),
                    DiffKind::Removed => (old_no, None),
                    _ => (old_no, new_no),
                };
                if kind != DiffKind::Added {
                    old_left = old_left.saturating_sub(1);
                    old_no = old_no.map(|n| n.saturating_add(1));
                }
                if kind != DiffKind::Removed {
                    new_left = new_left.saturating_sub(1);
                    new_no = new_no.map(|n| n.saturating_add(1));
                }
                out.push(DiffLine {
                    kind,
                    old_no: old,
                    new_no: new,
                    text,
                    emphasis: &[],
                });
                continue;
            }
        }
        if raw.starts_with("@@") {
            match hunk_numbers(raw) {
                Some((old_start, old_len, new_start, new_len)) => {
                    (old_left, new_left) = (old_len, new_len);
                    (old_no, new_no) = (Some(old_start), Some(new_start));
                }
                None => {
                    (old_left, new_left) = (0, 0);
                    (old_no, new_no) = (None, None);
                }
            }
            out.push(DiffLine::hunk(raw));
        } else if raw.is_empty() {
            // Spacing between files in a patch series.
        } else {
            if raw.starts_with("diff ") {
                (old_left, new_left) = (0, 0);
                (old_no, new_no) = (None, None);
            }
            out.push(DiffLine::file(raw));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// What a line longer than the room does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DiffWrap {
    /// Cut it and end the row with a continuation mark (`…`).
    #[default]
    Truncate,
    /// Continue it on the next row, under the same gutter.
    Wrap,
}

/// Which line-number columns to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiffGutter {
    /// Old and new numbers side by side.
    Both,
    /// One number: the old for a removed line, the new for the rest.
    Single,
    /// No numbers; the sign column stays.
    Off,
}

/// A caller's syntax highlighter for one line: byte ranges of the line's
/// text and the [`Role`] to ink each with. On changed lines under a tint only
/// `Foreground` and `Muted` are honoured, the inks the tints are audited
/// for; everything else falls back to `Foreground`.
pub type DiffHighlight = fn(&str) -> Vec<(Range<usize>, Role)>;

/// Words a diff prints. `Default` is English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffWords {
    /// `+3 more`: lines that did not fit, counted exactly.
    pub more: Cow<'static, str>,
}

impl Default for DiffWords {
    fn default() -> Self {
        Self {
            more: Cow::Borrowed("more"),
        }
    }
}

/// A parsed diff, ready to paint.
#[derive(Clone, Debug)]
pub struct Diff<'a> {
    pub lines: Vec<DiffLine<'a>>,
    pub wrap: DiffWrap,
    /// Spaces per tab stop.
    pub tab_width: u8,
    /// `None` picks by width: both numbers from 56 columns, one below. A
    /// gutter that does not fit gives way (both numbers, one, none) so the
    /// `+`/`-` sign always shows, down to a single column.
    pub gutter: Option<DiffGutter>,
    pub highlight: Option<DiffHighlight>,
    /// The first line to show: the host's scroll position.
    pub scroll: usize,
    pub words: DiffWords,
}

impl<'a> Diff<'a> {
    #[must_use]
    pub fn new(lines: Vec<DiffLine<'a>>) -> Self {
        Self {
            lines,
            wrap: DiffWrap::default(),
            tab_width: 4,
            gutter: None,
            highlight: None,
            scroll: 0,
            words: DiffWords::default(),
        }
    }

    #[must_use]
    pub fn wrap(mut self, wrap: DiffWrap) -> Self {
        self.wrap = wrap;
        self
    }

    #[must_use]
    pub fn tab_width(mut self, tab_width: u8) -> Self {
        self.tab_width = tab_width.max(1);
        self
    }

    #[must_use]
    pub fn gutter(mut self, gutter: DiffGutter) -> Self {
        self.gutter = Some(gutter);
        self
    }

    #[must_use]
    pub fn highlight(mut self, highlight: DiffHighlight) -> Self {
        self.highlight = Some(highlight);
        self
    }

    #[must_use]
    pub fn scroll(mut self, scroll: usize) -> Self {
        self.scroll = scroll;
        self
    }

    #[must_use]
    pub fn words(mut self, words: DiffWords) -> Self {
        self.words = words;
        self
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// One displayed grapheme: tabs already expanded, controls already gone.
struct Piece {
    text: String,
    width: usize,
    /// Byte offset of the source grapheme in the line's `text`.
    at: usize,
}

/// `text` as pieces. Offsets stay those of the original string, so a
/// caller's emphasis and highlight ranges still land on the right cells
/// after control characters are removed and tabs are widened.
fn pieces(text: &str, tab_width: usize) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut col = 0;
    for (at, g) in text.grapheme_indices(true) {
        if g == "\t" {
            let n = tab_width - col % tab_width;
            out.extend((0..n).map(|_| Piece {
                text: " ".to_string(),
                width: 1,
                at,
            }));
            col += n;
            continue;
        }
        let safe = text::display_safe(g);
        if safe.is_empty() {
            continue;
        }
        let width = text::width(&safe);
        col += width;
        out.push(Piece {
            text: safe.into_owned(),
            width,
            at,
        });
    }
    out
}

/// The cut points of `pieces` for rows `avail` wide: `(start, end, cut)`,
/// where `cut` means the row ends in the continuation mark.
fn split_rows(
    pieces: &[Piece],
    avail: usize,
    wrap: DiffWrap,
    mark_w: usize,
) -> Vec<(usize, usize, bool)> {
    let avail = avail.max(1);
    let total: usize = pieces.iter().map(|p| p.width).sum();
    if total <= avail {
        return vec![(0, pieces.len(), false)];
    }
    let take = |from: usize, budget: usize| -> usize {
        let (mut used, mut end) = (0, from);
        while end < pieces.len() && (used + pieces[end].width <= budget || end == from) {
            used += pieces[end].width;
            end += 1;
        }
        end
    };
    match wrap {
        DiffWrap::Truncate => {
            let end = take(0, avail.saturating_sub(mark_w));
            vec![(0, end, true)]
        }
        DiffWrap::Wrap => {
            let mut rows = Vec::new();
            let mut start = 0;
            while start < pieces.len() {
                let end = take(start, avail);
                rows.push((start, end, false));
                start = end;
            }
            rows
        }
    }
}

/// A row ready to paint.
struct Row {
    /// Index into the diff's lines.
    line: usize,
    spans: Vec<Span<'static>>,
    tint: Style,
}

fn digits(n: u32) -> usize {
    n.max(1).ilog10() as usize + 1
}

/// Cells the number columns take, with the space after each.
fn numbers_width(gutter: DiffGutter, nw: usize) -> usize {
    match gutter {
        DiffGutter::Both => 2 * nw + 2,
        DiffGutter::Single => nw + 1,
        DiffGutter::Off => 0,
    }
}

/// The richest gutter that leaves the sign, the space after it and a cell of
/// text in `width` columns, and whether the space fits. Numbers are shed
/// first (`Both`, then `Single`, then none); the sign is the last thing to go,
/// so a changed line says what it is at any width.
fn fit_gutter(wanted: DiffGutter, nw: usize, width: usize) -> (DiffGutter, bool) {
    let ladder = match wanted {
        DiffGutter::Both => &[DiffGutter::Both, DiffGutter::Single, DiffGutter::Off][..],
        DiffGutter::Single => &[DiffGutter::Single, DiffGutter::Off][..],
        DiffGutter::Off => &[DiffGutter::Off][..],
    };
    match ladder.iter().find(|g| numbers_width(**g, nw) + 3 <= width) {
        Some(g) => (*g, true),
        None => (DiffGutter::Off, false),
    }
}

impl Diff<'_> {
    fn number_width(&self) -> usize {
        let top = self
            .lines
            .iter()
            .flat_map(|l| [l.old_no, l.new_no])
            .flatten()
            .max()
            .unwrap_or(0);
        digits(top).max(2)
    }

    fn rows(&self, width: u16, theme: &Theme) -> Vec<Row> {
        let ascii = theme.ascii();
        let width = usize::from(width);
        let gutter = self.gutter.unwrap_or(if width >= 56 {
            DiffGutter::Both
        } else {
            DiffGutter::Single
        });
        let nw = self.number_width();
        let (gutter, gap) = fit_gutter(gutter, nw, width);
        // Number columns, the sign, and the space after it when there is room.
        let gutter_w = numbers_width(gutter, nw) + 1 + usize::from(gap);
        let mut rows = Vec::new();
        for (index, line) in self.lines.iter().enumerate().skip(self.scroll) {
            let changed = matches!(line.kind, DiffKind::Added | DiffKind::Removed);
            let tint = match line.kind {
                DiffKind::Added => theme.bg(Role::DiffAddedTint),
                DiffKind::Removed => theme.bg(Role::DiffRemovedTint),
                _ => Style::default(),
            };
            let tinted = changed && tint.bg.is_some();
            let hue = match line.kind {
                DiffKind::Added => Role::Live,
                DiffKind::Removed => Role::Danger,
                _ => Role::Foreground,
            };
            let sign = match line.kind {
                DiffKind::Added => "+",
                DiffKind::Removed if ascii => "-",
                DiffKind::Removed => "\u{2212}",
                _ => " ",
            };
            let own_gutter = matches!(
                line.kind,
                DiffKind::Context | DiffKind::Added | DiffKind::Removed
            );
            let prefix_w = if own_gutter || line.kind == DiffKind::Note {
                gutter_w
            } else {
                0
            };
            // Zero when even the sign takes the whole row: no body is shown.
            let avail = width.saturating_sub(prefix_w);
            let all = pieces(line.text, usize::from(self.tab_width.max(1)));
            // The one ellipsis; ASCII spells it out where there is room.
            let mark = match (ascii, avail) {
                (false, _) => glyphs::ELLIPSIS,
                (true, 4..) => "...",
                (true, _) => ".",
            };
            let mark_w = text::width(mark);
            let cuts = if avail == 0 {
                vec![(0, 0, false)]
            } else {
                split_rows(&all, avail, self.wrap, mark_w)
            };
            let highlights = match (self.highlight, own_gutter) {
                (Some(highlight), true) => highlight(line.text),
                _ => Vec::new(),
            };
            for (n, (start, end, cut)) in cuts.iter().enumerate() {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let number = |v: Option<u32>| match (n, v) {
                    (0, Some(v)) => format!("{v:>nw$}"),
                    _ => " ".repeat(nw),
                };
                if own_gutter {
                    let (old, new) = (number(line.old_no), number(line.new_no));
                    let shown = match gutter {
                        DiffGutter::Both => format!("{old} {new} "),
                        DiffGutter::Single => {
                            let one = if line.kind == DiffKind::Removed {
                                old
                            } else {
                                number(line.new_no.or(line.old_no))
                            };
                            format!("{one} ")
                        }
                        DiffGutter::Off => String::new(),
                    };
                    spans.push(Span::styled(shown, theme.fg(Role::Muted)));
                    let sign_style = if changed {
                        theme.fg(hue).add_modifier(Modifier::BOLD)
                    } else {
                        theme.fg(Role::Foreground)
                    };
                    spans.push(Span::styled(sign.to_string(), sign_style));
                    if gap {
                        spans.push(Span::raw(" "));
                    }
                } else if line.kind == DiffKind::Note {
                    spans.push(Span::raw(" ".repeat(gutter_w)));
                }
                let (base_role, base_mod) = match line.kind {
                    DiffKind::FileHeader
                        if line.text.starts_with("diff ") || line.text.starts_with("+++ ") =>
                    {
                        (Role::Foreground, Modifier::BOLD)
                    }
                    DiffKind::FileHeader => (Role::Foreground, Modifier::empty()),
                    DiffKind::HunkHeader => (Role::Muted, Modifier::empty()),
                    DiffKind::Note => (Role::Hint, Modifier::empty()),
                    _ => (Role::Foreground, Modifier::empty()),
                };
                let mut run: Option<(Style, String)> = None;
                for piece in &all[*start..*end] {
                    let emphasised = line.emphasis.iter().any(|r| r.contains(&piece.at));
                    let mut role = highlights
                        .iter()
                        .find(|(r, _)| r.contains(&piece.at))
                        .map(|(_, role)| *role)
                        .filter(|role| !tinted || matches!(role, Role::Foreground | Role::Muted))
                        .unwrap_or(base_role);
                    let mut modifier = base_mod;
                    if emphasised {
                        modifier |= Modifier::BOLD;
                        if tinted || (changed && theme.color(hue).is_some()) {
                            role = hue;
                        }
                        if !tinted {
                            modifier |= Modifier::UNDERLINED;
                        }
                    }
                    let style = theme.fg(role).add_modifier(modifier);
                    match &mut run {
                        Some((s, buf)) if *s == style => buf.push_str(&piece.text),
                        _ => {
                            if let Some((s, buf)) = run.take() {
                                spans.push(Span::styled(buf, s));
                            }
                            run = Some((style, piece.text.clone()));
                        }
                    }
                }
                if let Some((s, buf)) = run {
                    spans.push(Span::styled(buf, s));
                }
                if *cut {
                    // `Hint` is not audited behind a tint; `Muted` is.
                    let ink = if tinted { Role::Muted } else { Role::Hint };
                    spans.push(Span::styled(mark.to_string(), theme.fg(ink)));
                }
                rows.push(Row {
                    line: index,
                    spans,
                    tint: if changed { tint } else { Style::default() },
                });
            }
        }
        rows
    }
}

impl Paint for Diff<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let rows = self.rows(area.width, theme);
        let height = usize::from(area.height);
        // One row cannot hold both a line and the count of the rest: the
        // first line wins.
        let clipped = rows.len() > height && height > 1;
        let shown = if clipped {
            height - 1
        } else {
            rows.len().min(height)
        };
        for (y, row) in rows.iter().take(shown).enumerate() {
            let rect = Rect::new(area.x, area.y + y as u16, area.width, 1);
            buf.set_style(rect, row.tint);
            Line::from(row.spans.clone()).render(rect, buf);
        }
        if clipped {
            // Lines with a row that is not on screen, counted once each: a
            // wrapped line cut off part-way is not fully shown either.
            let mut hidden_lines: Vec<usize> = rows.iter().skip(shown).map(|r| r.line).collect();
            hidden_lines.dedup();
            let more = format!(
                "+{} {}",
                hidden_lines.len(),
                text::display_safe(&self.words.more)
            );
            let more = text::truncate(&more, usize::from(area.width), theme.ascii());
            let rect = Rect::new(area.x, area.y + shown as u16, area.width, 1);
            Line::from(Span::styled(more.into_owned(), theme.fg(Role::Muted))).render(rect, buf);
        }
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        u16::try_from(self.rows(width, theme).len()).unwrap_or(u16::MAX)
    }
}

/// Which git or session slice a diff review is inspecting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DiffScope {
    /// Unstaged working-tree changes.
    #[default]
    Unstaged,
    /// Staged index changes.
    Staged,
    /// Changes compared against the base branch.
    Branch,
    /// Changes produced by a single turn.
    Turn,
}

impl DiffScope {
    /// Short display label for this diff scope.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unstaged => "Unstaged",
            Self::Staged => "Staged",
            Self::Branch => "Branch",
            Self::Turn => "Turn",
        }
    }
}

/// One file tracked in a diff review checklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffReviewFile<'a> {
    /// Repository-relative file path.
    pub path: Cow<'a, str>,
    /// Lines added in this file.
    pub added: usize,
    /// Lines removed in this file.
    pub removed: usize,
    /// Whether this file has been marked viewed.
    pub viewed: bool,
}

impl<'a> DiffReviewFile<'a> {
    /// Construct an unviewed review file entry.
    #[must_use]
    pub fn new(path: impl Into<Cow<'a, str>>, added: usize, removed: usize) -> Self {
        Self {
            path: path.into(),
            added,
            removed,
            viewed: false,
        }
    }

    /// Mark whether this file has been viewed.
    #[must_use]
    pub const fn viewed(mut self, viewed: bool) -> Self {
        self.viewed = viewed;
        self
    }
}

/// Return the next unviewed file index after `current` in list order, wrapping around.
///
/// Never returns `current` when `current` is the only remaining unviewed file.
/// When `current` is `None` or out of bounds, returns the first unviewed file.
#[must_use]
pub fn next_unviewed(files: &[DiffReviewFile<'_>], current: Option<usize>) -> Option<usize> {
    if files.is_empty() {
        return None;
    }
    match current {
        Some(idx) if idx < files.len() => {
            for offset in 1..files.len() {
                let candidate = (idx + offset) % files.len();
                if !files[candidate].viewed {
                    return Some(candidate);
                }
            }
            None
        }
        _ => files.iter().position(|file| !file.viewed),
    }
}

/// Compact diff review summary header (`Scope · viewed/total · +N −M`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffReviewHeader {
    /// Active diff scope.
    pub scope: DiffScope,
    /// Number of viewed files.
    pub viewed: usize,
    /// Total number of files in the review.
    pub total: usize,
    /// Total lines added across the review.
    pub added: usize,
    /// Total lines removed across the review.
    pub removed: usize,
}

impl DiffReviewHeader {
    /// Construct a review header from explicit counts.
    #[must_use]
    pub const fn new(
        scope: DiffScope,
        viewed: usize,
        total: usize,
        added: usize,
        removed: usize,
    ) -> Self {
        Self {
            scope,
            viewed,
            total,
            added,
            removed,
        }
    }

    /// Summarize a slice of [`DiffReviewFile`] entries under `scope`.
    #[must_use]
    pub fn from_files(scope: DiffScope, files: &[DiffReviewFile<'_>]) -> Self {
        let mut viewed = 0usize;
        let mut added = 0usize;
        let mut removed = 0usize;
        for file in files {
            if file.viewed {
                viewed += 1;
            }
            added = added.saturating_add(file.added);
            removed = removed.saturating_add(file.removed);
        }
        Self {
            scope,
            viewed,
            total: files.len(),
            added,
            removed,
        }
    }
}

impl Paint for DiffReviewHeader {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let ascii = theme.ascii();
        let sep = if ascii { " - " } else { " · " };
        let minus = if ascii { "-" } else { "\u{2212}" };
        let viewed = self.viewed.min(self.total);
        let width = usize::from(area.width);
        let scope_text = text::truncate(self.scope.label(), width, ascii).into_owned();
        let mut used = text::width(&scope_text);
        let mut spans = vec![Span::styled(
            scope_text,
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
        )];
        let progress = format!("{sep}{viewed}/{}", self.total);
        let progress_w = text::width(&progress);
        if used + progress_w <= width {
            spans.push(Span::styled(progress, theme.fg(Role::Muted)));
            used += progress_w;
        }
        let add_text = format!("+{}", self.added);
        let rem_text = format!("{minus}{}", self.removed);
        let counts_w = text::width(sep) + text::width(&add_text) + 1 + text::width(&rem_text);
        if used + counts_w <= width {
            spans.push(Span::styled(sep, theme.fg(Role::Muted)));
            spans.push(Span::styled(add_text, theme.fg(Role::Live)));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(rem_text, theme.fg(Role::Danger)));
        }
        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
    }
}

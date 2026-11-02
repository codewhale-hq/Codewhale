//! Package Approval: the approval card and the review verdict.
//!
//! This is what a person reads before letting an agent run a command or change
//! files, so it follows a few rules that nothing else in the kit needs:
//!
//! - **The subject is verbatim.** The command or target is never elided in the
//!   middle. It wraps; if the card cannot show all of it, the card says how many
//!   characters and lines are not shown and offers the binding that reveals
//!   them, and [`ApprovalPaint`] tells the caller so it can refuse a blind
//!   approval.
//! - **What is shown is what will run.** Every control character, escape
//!   sequence, bidi override, zero-width or otherwise invisible character is
//!   drawn as a named token (`‹ESC›`, `‹RLO›`, `‹ZWJ›`, `‹LF›`, `‹HT›`), styled
//!   reversed and bold so it cannot be mistaken for text. Leading and trailing
//!   spaces are drawn too (`‹SP×3›`). A literal `‹` that could be mistaken for
//!   the start of a token is itself drawn as a token (`‹U+2039›`), so the
//!   display decodes to exactly one subject. [`visible_text`] is that encoding.
//! - **Nothing is decided by color, and nothing is preselected.** "Outside this
//!   project" and elevation carry a mark and words. The choices come from the
//!   caller with their key chords; the card never invents one, and the default
//!   focus is the first choice that does not grant anything.
//! - **Displayed chords are handled chords.** [`ApprovalState`] owns the
//!   choices, so the keys the card advertises are the keys `handle_key`
//!   accepts.
//!
//! The host owns policy and input guards. A key that arrives before the card
//! was drawn should not count; [`ApprovalState::unarmed`] and
//! [`ApprovalState::arm`] are the hooks for that.

use std::borrow::Cow;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

use crate::{
    Depth, KeyHint, KeyHints, Paint, Panel, Role, State, StateWords, Theme, glyphs,
    keys::{self, Platform},
    text,
};

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

/// A phrase with a singular and a plural form. `{n}` is replaced by the count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalCountWords {
    pub one: Cow<'static, str>,
    pub many: Cow<'static, str>,
}

impl ApprovalCountWords {
    #[must_use]
    pub fn new(one: impl Into<Cow<'static, str>>, many: impl Into<Cow<'static, str>>) -> Self {
        Self {
            one: one.into(),
            many: many.into(),
        }
    }

    /// The phrase for `n`.
    #[must_use]
    pub fn render(&self, n: usize) -> String {
        let phrase = if n == 1 { &self.one } else { &self.many };
        phrase.replace("{n}", &n.to_string())
    }
}

/// The words of the "not shown" notice, shared by the card and the verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalClipWords {
    pub hidden_chars: ApprovalCountWords,
    pub hidden_lines: ApprovalCountWords,
    pub hidden_preview: ApprovalCountWords,
    pub hidden_details: ApprovalCountWords,
    pub hidden_choices: ApprovalCountWords,
    /// Verb beside the reveal binding: `o show all`.
    pub reveal: Cow<'static, str>,
    /// Shown for a subject with no characters.
    pub empty: Cow<'static, str>,
}

impl Default for ApprovalClipWords {
    fn default() -> Self {
        Self {
            hidden_chars: ApprovalCountWords::new(
                "{n} character not shown",
                "{n} characters not shown",
            ),
            hidden_lines: ApprovalCountWords::new("{n} line", "{n} lines"),
            hidden_preview: ApprovalCountWords::new(
                "{n} preview line not shown",
                "{n} preview lines not shown",
            ),
            hidden_details: ApprovalCountWords::new(
                "{n} detail line not shown",
                "{n} detail lines not shown",
            ),
            hidden_choices: ApprovalCountWords::new(
                "{n} choice not shown",
                "{n} choices not shown",
            ),
            reveal: "show all".into(),
            empty: "(empty)".into(),
        }
    }
}

/// Every word the approval card shows. The kit owns no copy beyond this
/// English default: a host fills one per locale and passes it in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalWords {
    pub needs_you: Cow<'static, str>,
    pub command: Cow<'static, str>,
    pub file_change: Cow<'static, str>,
    pub network: Cow<'static, str>,
    pub tool_call: Cow<'static, str>,
    pub elevation: Cow<'static, str>,
    /// `In /path/to/dir`
    pub in_dir: Cow<'static, str>,
    pub inside: Cow<'static, str>,
    pub outside: Cow<'static, str>,
    pub unchecked: Cow<'static, str>,
    pub elevated: Cow<'static, str>,
    pub requested_by: Cow<'static, str>,
    pub sub_agent: Cow<'static, str>,
    pub risk: Cow<'static, str>,
    pub preview: Cow<'static, str>,
    pub non_ascii: ApprovalCountWords,
    /// Key-hint verbs for the rail under the card.
    pub move_focus: Cow<'static, str>,
    pub choose_focused: Cow<'static, str>,
    pub cancel: Cow<'static, str>,
    pub clip: ApprovalClipWords,
}

impl Default for ApprovalWords {
    fn default() -> Self {
        Self {
            needs_you: Cow::Borrowed(StateWords::english(State::NeedsYou)),
            command: "Run a command".into(),
            file_change: "Change files".into(),
            network: "Use the network".into(),
            tool_call: "Call a tool".into(),
            elevation: "Raise access".into(),
            in_dir: "In".into(),
            inside: "Inside this project".into(),
            outside: "Outside this project".into(),
            unchecked: "Not checked against the project".into(),
            elevated: "Runs with elevated access".into(),
            requested_by: "Requested by".into(),
            sub_agent: "sub-agent".into(),
            risk: "Risk".into(),
            preview: "Preview".into(),
            non_ascii: ApprovalCountWords::new(
                "Contains {n} non-ASCII character: check it",
                "Contains {n} non-ASCII characters: check them",
            ),
            move_focus: "move".into(),
            choose_focused: "choose".into(),
            cancel: "cancel".into(),
            clip: ApprovalClipWords::default(),
        }
    }
}

impl ApprovalWords {
    fn kind(&self, kind: ApprovalKind) -> &str {
        match kind {
            ApprovalKind::Command => &self.command,
            ApprovalKind::FileChange => &self.file_change,
            ApprovalKind::Network => &self.network,
            ApprovalKind::ToolCall => &self.tool_call,
            ApprovalKind::Elevation => &self.elevation,
        }
    }
}

// ---------------------------------------------------------------------------
// The subject
// ---------------------------------------------------------------------------

/// What the agent wants to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApprovalKind {
    /// Run a shell command.
    Command,
    /// Write or patch files.
    FileChange,
    /// Reach the network.
    Network,
    /// Call a tool or connected app.
    ToolCall,
    /// Run with more access than the sandbox gives.
    Elevation,
}

/// Where the action lands relative to the project. Never assumed: the default
/// is [`ApprovalScope::Unchecked`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ApprovalScope {
    Inside,
    Outside,
    #[default]
    Unchecked,
}

/// The agent that is asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalAgent<'a> {
    pub name: Cow<'a, str>,
    pub sub_agent: bool,
}

/// Plain data the caller fills. Every string may come from a model or a file:
/// the card draws all of it through [`visible_text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalSubject<'a> {
    pub kind: ApprovalKind,
    /// The exact command, path, URL or tool call.
    pub target: Cow<'a, str>,
    pub cwd: Option<Cow<'a, str>>,
    pub scope: ApprovalScope,
    pub agent: Option<ApprovalAgent<'a>>,
    /// Risk notes the caller supplies, one per row.
    pub risk_notes: Vec<Cow<'a, str>>,
    /// A patch or file preview, one line per entry.
    pub preview: Vec<Cow<'a, str>>,
}

impl<'a> ApprovalSubject<'a> {
    #[must_use]
    pub fn new(kind: ApprovalKind, target: impl Into<Cow<'a, str>>) -> Self {
        Self {
            kind,
            target: target.into(),
            cwd: None,
            scope: ApprovalScope::Unchecked,
            agent: None,
            risk_notes: Vec::new(),
            preview: Vec::new(),
        }
    }

    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<Cow<'a, str>>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn scope(mut self, scope: ApprovalScope) -> Self {
        self.scope = scope;
        self
    }

    #[must_use]
    pub fn agent(mut self, name: impl Into<Cow<'a, str>>, sub_agent: bool) -> Self {
        self.agent = Some(ApprovalAgent {
            name: name.into(),
            sub_agent,
        });
        self
    }

    #[must_use]
    pub fn risk_note(mut self, note: impl Into<Cow<'a, str>>) -> Self {
        self.risk_notes.push(note.into());
        self
    }

    #[must_use]
    pub fn preview_line(mut self, line: impl Into<Cow<'a, str>>) -> Self {
        self.preview.push(line.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Visible text: the one encoding of "what will run"
// ---------------------------------------------------------------------------

/// How tokens are spelled. Unicode terminals get `‹ESC›`; ASCII-safe ones
/// get `<ESC>` (the glyph charter's own fallback for the guillemets).
#[derive(Clone, Copy)]
struct Marks {
    open: &'static str,
    close: &'static str,
    times: &'static str,
}

impl Marks {
    fn new(ascii: bool) -> Self {
        Self {
            open: glyphs::pick("‹", ascii),
            close: glyphs::pick("›", ascii),
            times: if ascii { "x" } else { "×" },
        }
    }

    fn token(self, name: &str) -> String {
        format!("{}{name}{}", self.open, self.close)
    }

    fn spaces(self, n: usize) -> String {
        if n == 1 {
            self.token("SP")
        } else {
            self.token(&format!("SP{}{n}", self.times))
        }
    }

    /// Whether `rest` (the source after an opener) would read as the rest of
    /// a token: name characters, then the closer. Such an opener is itself
    /// drawn as a token, which keeps the encoding one-to-one.
    fn looks_like_token(self, rest: &str) -> bool {
        let mut names = 0;
        for c in rest.chars() {
            if c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, '+' | 'x' | '×') {
                names += 1;
            } else {
                return names > 0 && self.close.starts_with(c);
            }
        }
        false
    }
}

/// A character that is drawn as a token rather than as itself: control
/// characters, bidi controls, zero-width and invisible characters, and every
/// space except the ordinary one.
fn is_flagged(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00A0}' | '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}'
            | '\u{1680}' | '\u{17B4}' | '\u{17B5}' | '\u{180B}'..='\u{180F}'
            | '\u{2000}'..='\u{200F}' | '\u{2028}'..='\u{202F}' | '\u{205F}'..='\u{206F}'
            | '\u{2800}' | '\u{3000}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
            | '\u{FFA0}' | '\u{FFF9}'..='\u{FFFC}' | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E01EF}')
}

fn char_name(c: char) -> String {
    let name = match c {
        '\0' => "NUL",
        '\u{7}' => "BEL",
        '\u{8}' => "BS",
        '\t' => "HT",
        '\n' => "LF",
        '\u{B}' => "VT",
        '\u{C}' => "FF",
        '\r' => "CR",
        '\u{1B}' => "ESC",
        '\u{7F}' => "DEL",
        '\u{A0}' => "NBSP",
        '\u{AD}' => "SHY",
        '\u{61C}' => "ALM",
        '\u{200B}' => "ZWSP",
        '\u{200C}' => "ZWNJ",
        '\u{200D}' => "ZWJ",
        '\u{200E}' => "LRM",
        '\u{200F}' => "RLM",
        '\u{202A}' => "LRE",
        '\u{202B}' => "RLE",
        '\u{202C}' => "PDF",
        '\u{202D}' => "LRO",
        '\u{202E}' => "RLO",
        '\u{2060}' => "WJ",
        '\u{2066}' => "LRI",
        '\u{2067}' => "RLI",
        '\u{2068}' => "FSI",
        '\u{2069}' => "PDI",
        '\u{FEFF}' => "BOM",
        _ => return format!("U+{:04X}", c as u32),
    };
    name.to_owned()
}

/// One drawn piece of text: a grapheme, or a token standing for characters.
#[derive(Clone)]
struct Unit<'a> {
    text: Cow<'a, str>,
    width: usize,
    /// Source characters this unit stands for (0 for the card's own words).
    chars: usize,
    style: Style,
    /// An ordinary space: a place where a row may break.
    space: bool,
}

struct Looks {
    plain: Style,
    token: Style,
}

/// Visit `src` as the units it is drawn with. Returns how many ordinary
/// (non-flagged) non-ASCII characters it holds.
fn escape<'a>(
    src: &'a str,
    ascii: bool,
    edges: bool,
    looks: &Looks,
    mut visit: impl FnMut(Unit<'a>),
) -> usize {
    let marks = Marks::new(ascii);
    let token_unit = |name: String, chars: usize| Unit {
        width: text::width(&name),
        text: Cow::Owned(name),
        chars,
        style: looks.token,
        space: false,
    };
    // Leading and trailing spaces are drawn for anything that is run or
    // named; prose and previews (indented code, diff context) keep theirs.
    let lead = if edges {
        src.len() - src.trim_start_matches(' ').len()
    } else {
        0
    };
    let trail = if !edges || lead == src.len() {
        0
    } else {
        src.len() - src.trim_end_matches(' ').len()
    };
    let mid = &src[lead..src.len() - trail];
    let mut non_ascii = 0;
    if lead > 0 {
        visit(token_unit(marks.spaces(lead), lead));
    }
    for (at, g) in mid.grapheme_indices(true) {
        if !g.chars().any(is_flagged) && text::width(g) > 0 {
            if g == marks.open && marks.looks_like_token(&mid[at + g.len()..]) {
                let opener = marks.open.chars().next().unwrap_or('<');
                visit(token_unit(marks.token(&char_name(opener)), 1));
                continue;
            }
            non_ascii += g.chars().filter(|c| !c.is_ascii()).count();
            visit(Unit {
                text: Cow::Borrowed(g),
                width: text::width(g),
                chars: g.chars().count(),
                style: looks.plain,
                space: g == " ",
            });
            continue;
        }
        // A cluster with something invisible in it: draw each invisible
        // character as a token and keep the visible runs between them.
        let mut run: Option<usize> = None;
        let mut flush = |end: usize, run: &mut Option<usize>, visit: &mut dyn FnMut(Unit<'a>)| {
            if let Some(start) = run.take() {
                let piece = &g[start..end];
                if text::width(piece) > 0 {
                    non_ascii += piece.chars().filter(|c| !c.is_ascii()).count();
                    visit(Unit {
                        text: Cow::Borrowed(piece),
                        width: text::width(piece),
                        chars: piece.chars().count(),
                        style: looks.plain,
                        space: false,
                    });
                } else {
                    for c in piece.chars() {
                        visit(token_unit(marks.token(&char_name(c)), 1));
                    }
                }
            }
        };
        for (off, c) in g.char_indices() {
            if is_flagged(c) {
                flush(off, &mut run, &mut visit);
                visit(token_unit(marks.token(&char_name(c)), 1));
            } else if run.is_none() {
                run = Some(off);
            }
        }
        flush(g.len(), &mut run, &mut visit);
    }
    if trail > 0 {
        visit(token_unit(marks.spaces(trail), trail));
    }
    non_ascii
}

/// `src` as the card draws it: every invisible, control or direction-changing
/// character as a named token, leading and trailing spaces as `SP` tokens.
/// This is the text a test or a host can compare with what was painted.
#[must_use]
pub fn visible_text(src: &str, ascii: bool) -> String {
    let looks = Looks {
        plain: Style::default(),
        token: Style::default(),
    };
    let mut out = String::new();
    escape(src, ascii, true, &looks, |u| out.push_str(&u.text));
    out
}

// ---------------------------------------------------------------------------
// Rows and wrapping
// ---------------------------------------------------------------------------

/// One painted row, with the source characters (or choices) it carries.
#[derive(Clone)]
struct Row {
    spans: Vec<Span<'static>>,
    units: usize,
}

fn spans_of(units: &[Unit<'_>]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut style: Option<Style> = None;
    for u in units {
        if style != Some(u.style) {
            if let Some(s) = style {
                spans.push(Span::styled(std::mem::take(&mut buf), s));
            }
            style = Some(u.style);
        }
        buf.push_str(&u.text);
    }
    if let Some(s) = style {
        spans.push(Span::styled(buf, s));
    }
    spans
}

#[derive(Default)]
struct Wrapped {
    rows: Vec<Row>,
    total_rows: usize,
    total_chars: usize,
}

/// Greedy wrapping that never drops or rewrites a character: a row breaks
/// after an ordinary space when that leaves the row at least half full, and
/// otherwise at the cell where it runs out. The rows, read in order, are the
/// text.
struct Wrapper<'a> {
    width: usize,
    cap: usize,
    cur: Vec<Unit<'a>>,
    cur_w: usize,
    last_break: Option<usize>,
    any: bool,
    out: Wrapped,
}

impl<'a> Wrapper<'a> {
    fn new(width: usize, cap: usize) -> Self {
        Self {
            width,
            cap,
            cur: Vec::new(),
            cur_w: 0,
            last_break: None,
            any: false,
            out: Wrapped::default(),
        }
    }

    fn emit(&mut self, spans: Vec<Span<'static>>, units: usize) {
        self.out.total_rows += 1;
        if self.out.rows.len() < self.cap {
            self.out.rows.push(Row { spans, units });
        }
    }

    fn flush(&mut self) {
        if self.cur.is_empty() {
            return;
        }
        let units = self.cur.iter().map(|u| u.chars).sum();
        let spans = spans_of(&self.cur);
        self.emit(spans, units);
        self.cur.clear();
        self.cur_w = 0;
        self.last_break = None;
    }

    fn push(&mut self, u: Unit<'a>) {
        self.any = true;
        self.out.total_chars += u.chars;
        if self.width == 0 {
            return;
        }
        if u.width > self.width {
            self.flush();
            self.overwide(&u);
            return;
        }
        if self.cur_w + u.width > self.width {
            let split = match self.last_break {
                Some(b)
                    if b < self.cur.len()
                        && self.cur[..b].iter().map(|u| u.width).sum::<usize>() * 2
                            >= self.width =>
                {
                    b
                }
                _ => self.cur.len(),
            };
            let carry = self.cur.split_off(split);
            self.flush();
            self.cur_w = carry.iter().map(|u| u.width).sum();
            self.cur = carry;
            if self.cur_w + u.width > self.width {
                self.flush();
            }
        }
        self.cur_w += u.width;
        let space = u.space;
        self.cur.push(u);
        if space {
            self.last_break = Some(self.cur.len());
        }
    }

    /// A unit wider than a whole row (a token at a tiny width): split it
    /// across rows by character. A single character wider than the row has
    /// nowhere to go and counts as not shown.
    fn overwide(&mut self, u: &Unit<'_>) {
        let mut piece = String::new();
        let mut used = 0;
        for c in u.text.chars() {
            let w = c.width().unwrap_or(0);
            if w > self.width {
                if !piece.is_empty() {
                    self.emit(vec![Span::styled(std::mem::take(&mut piece), u.style)], 0);
                    used = 0;
                }
                self.emit(Vec::new(), 0);
                continue;
            }
            if used + w > self.width {
                self.emit(vec![Span::styled(std::mem::take(&mut piece), u.style)], 0);
                used = 0;
            }
            piece.push(c);
            used += w;
        }
        if !piece.is_empty() {
            // The characters count as shown only once the last piece is.
            self.emit(vec![Span::styled(piece, u.style)], u.chars);
        }
    }

    fn finish(mut self) -> Wrapped {
        self.flush();
        if self.out.total_rows == 0 {
            // A blank line still takes a row.
            self.emit(Vec::new(), 0);
        }
        self.out
    }

    /// Push the card's own words (no source characters).
    fn words(&mut self, words: &str, style: Style) {
        for g in text::display_safe(words).graphemes(true) {
            self.push(Unit {
                text: Cow::Owned(g.to_owned()),
                width: text::width(g),
                chars: 0,
                style,
                space: g == " ",
            });
        }
    }

    /// Push caller text through [`escape`]; returns its non-ASCII count.
    fn subject(&mut self, src: &'a str, ascii: bool, looks: &Looks) -> usize {
        escape(src, ascii, true, looks, |u| self.push(u))
    }

    /// Like [`Wrapper::subject`] for prose and previews: control characters
    /// and invisible characters are drawn, edge spaces are left alone.
    fn prose(&mut self, src: &'a str, ascii: bool, looks: &Looks) -> usize {
        escape(src, ascii, false, looks, |u| self.push(u))
    }
}

/// Rows of one part of the card, with what did not fit.
struct Section {
    rows: Vec<Row>,
    total_rows: usize,
    total_chars: usize,
    cap: usize,
}

impl Section {
    fn new(cap: usize) -> Self {
        Self {
            rows: Vec::new(),
            total_rows: 0,
            total_chars: 0,
            cap,
        }
    }

    fn absorb(&mut self, wrapped: Wrapped) {
        self.total_rows += wrapped.total_rows;
        self.total_chars += wrapped.total_chars;
        for row in wrapped.rows {
            if self.rows.len() < self.cap {
                self.rows.push(row);
            }
        }
    }

    /// One logical line, wrapped into `width`, with a hanging gutter.
    fn line<'a>(
        &mut self,
        width: usize,
        gutter: Option<(&str, Style, &str, Style)>,
        build: impl FnOnce(&mut Wrapper<'a>),
    ) {
        let gutter_w = gutter.map_or(0, |g| text::width(g.0));
        let mut wrapper = Wrapper::new(width.saturating_sub(gutter_w), self.cap);
        build(&mut wrapper);
        let mut wrapped = wrapper.finish();
        if let Some((first, first_style, rest, rest_style)) = gutter {
            for (i, row) in wrapped.rows.iter_mut().enumerate() {
                let (g, s) = if i == 0 {
                    (first, first_style)
                } else {
                    (rest, rest_style)
                };
                row.spans.insert(0, Span::styled(g.to_owned(), s));
            }
        }
        self.absorb(wrapped);
    }

    fn hidden(&self, shown: usize) -> ApprovalHidden {
        let shown = shown.min(self.rows.len());
        let shown_chars: usize = self.rows[..shown].iter().map(|r| r.units).sum();
        ApprovalHidden {
            chars: self.total_chars.saturating_sub(shown_chars),
            lines: self.total_rows.saturating_sub(shown),
        }
    }
}

/// Pack unbreakable items into rows, `sep` between them on a row; an item
/// wider than a row wraps. Row units count the items that end in the row: an
/// item counts as shown only once its last row is, so a choice cut off after
/// its key is reported as cut.
fn pack(
    items: Vec<Vec<Unit<'static>>>,
    sep: &[Unit<'static>],
    width: usize,
    cap: usize,
) -> Section {
    let sep_w: usize = sep.iter().map(|u| u.width).sum();
    let mut sec = Section::new(cap);
    let mut cur: Vec<Unit<'static>> = Vec::new();
    let mut cur_items = 0;
    let mut used = 0;
    let flush = |sec: &mut Section, cur: &mut Vec<Unit<'static>>, n: &mut usize| {
        if !cur.is_empty() {
            sec.total_rows += 1;
            if sec.rows.len() < sec.cap {
                sec.rows.push(Row {
                    spans: spans_of(cur),
                    units: *n,
                });
            }
            cur.clear();
            *n = 0;
        }
    };
    for item in items {
        sec.total_chars += 1;
        let w: usize = item.iter().map(|u| u.width).sum();
        if w > width {
            flush(&mut sec, &mut cur, &mut cur_items);
            used = 0;
            let mut wrapper = Wrapper::new(width, cap);
            for u in item {
                wrapper.push(u);
            }
            let mut wrapped = wrapper.finish();
            // The last row only counts if it was kept: rows past the cap are
            // dropped, and an item whose tail is dropped is not shown.
            if wrapped.rows.len() == wrapped.total_rows
                && let Some(last) = wrapped.rows.last_mut()
            {
                last.units = 1;
            }
            wrapped.total_chars = 0;
            sec.absorb(wrapped);
            continue;
        }
        if !cur.is_empty() && used + sep_w + w > width {
            flush(&mut sec, &mut cur, &mut cur_items);
            used = 0;
        }
        if !cur.is_empty() {
            cur.extend(sep.iter().cloned());
            used += sep_w;
        }
        used += w;
        cur.extend(item);
        cur_items += 1;
    }
    flush(&mut sec, &mut cur, &mut cur_items);
    sec
}

fn trusted(text: &str, style: Style) -> Vec<Unit<'static>> {
    text::display_safe(text)
        .graphemes(true)
        .map(|g| Unit {
            text: Cow::Owned(g.to_owned()),
            width: text::width(g),
            chars: 0,
            style,
            space: g == " ",
        })
        .collect()
}

// ---------------------------------------------------------------------------
// What a paint reports
// ---------------------------------------------------------------------------

/// What one part of a card did not show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApprovalHidden {
    /// Source characters not shown.
    pub chars: usize,
    /// Rows not shown.
    pub lines: usize,
}

impl ApprovalHidden {
    #[must_use]
    pub const fn any(self) -> bool {
        self.chars > 0 || self.lines > 0
    }

    const fn plus(self, other: Self) -> Self {
        Self {
            chars: self.chars + other.chars,
            lines: self.lines + other.lines,
        }
    }
}

/// What a paint showed and did not show. The caller reads [`clipped`] to
/// refuse a blind approval.
///
/// [`clipped`]: ApprovalPaint::clipped
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApprovalPaint {
    /// The exact command or target.
    pub subject: ApprovalHidden,
    /// The preview.
    pub preview: ApprovalHidden,
    /// Agent, location, elevation, risk notes and the non-ASCII note.
    pub details: ApprovalHidden,
    /// Choices that did not fit and so cannot be seen.
    pub choices: usize,
    /// Non-ASCII characters in the subject, location and agent name (the
    /// card says so when there are any; this is how many).
    pub non_ascii: usize,
}

impl ApprovalPaint {
    /// Whether anything the person should read before deciding was not shown.
    #[must_use]
    pub const fn clipped(&self) -> bool {
        self.subject.any() || self.preview.any() || self.details.any() || self.choices > 0
    }
}

// ---------------------------------------------------------------------------
// Choices and keys
// ---------------------------------------------------------------------------

/// A caller-defined name for a choice. The kit never interprets it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChoiceId(pub u32);

/// What choosing does to the action. The card uses it for one thing: the
/// default focus is never a choice that grants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ApprovalEffect {
    /// The action may proceed.
    Grants,
    /// The action does not proceed.
    Refuses,
    /// Neither: "deny and say why" (it refuses, then asks), "open details".
    Other,
}

/// A key and its modifiers, matched exactly: `y` is not `Y`, and `Ctrl+Y` is
/// neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ApprovalKey {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl ApprovalKey {
    #[must_use]
    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// A character with no modifier.
    #[must_use]
    pub const fn char(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// `Ctrl` plus a character.
    #[must_use]
    pub const fn ctrl(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// The label the card shows, spelled as every other key in the kit.
    #[must_use]
    pub fn label(&self, platform: Platform) -> String {
        keys::chord_label(&KeyEvent::new(self.code, self.modifiers), platform)
    }

    fn matches(&self, key: &KeyEvent) -> bool {
        key.code == self.code && key.modifiers == self.modifiers
    }

    /// Keys that move focus, confirm or cancel are the card's own.
    const fn reserved(&self) -> bool {
        matches!(
            self.code,
            KeyCode::Enter
                | KeyCode::Esc
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
        )
    }
}

/// One thing the person can choose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalChoice {
    pub id: ChoiceId,
    pub label: Cow<'static, str>,
    pub effect: ApprovalEffect,
    /// Chords that choose directly. The first is the one shown. A chord an
    /// earlier choice already has, or one the card owns, is dropped by
    /// [`ApprovalState::new`].
    pub keys: Vec<ApprovalKey>,
}

impl ApprovalChoice {
    #[must_use]
    pub fn new(id: ChoiceId, label: impl Into<Cow<'static, str>>, effect: ApprovalEffect) -> Self {
        Self {
            id,
            label: label.into(),
            effect,
            keys: Vec::new(),
        }
    }

    #[must_use]
    pub fn key(mut self, key: ApprovalKey) -> Self {
        self.keys.push(key);
        self
    }

    /// A plain character chord.
    #[must_use]
    pub fn char_key(self, c: char) -> Self {
        self.key(ApprovalKey::char(c))
    }
}

/// What a key did to an [`ApprovalState`]: the message a host reacts to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// The key means nothing to the card; the host may use it.
    Ignored,
    /// Focus moved; repaint.
    Moved,
    /// A choice was made: Enter on the focused choice, or its chord.
    Chose(ChoiceId),
    /// The reveal chord: show the whole subject.
    Reveal,
    /// Esc. The caller decides whether that denies.
    Cancelled,
}

/// The choices, which one has focus, and the keys. The card paints from this,
/// so the chords it shows are the chords [`ApprovalState::handle_key`] takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalState {
    choices: Vec<ApprovalChoice>,
    focus: Option<usize>,
    reveal: Option<ApprovalKey>,
    armed: bool,
}

impl ApprovalState {
    /// Focus starts on the first choice that refuses, else the first that is
    /// neither, else nowhere: a card of only grants has no default, and Enter
    /// then chooses nothing.
    #[must_use]
    pub fn new(choices: Vec<ApprovalChoice>) -> Self {
        let mut state = Self {
            choices,
            focus: None,
            reveal: None,
            armed: true,
        };
        state.sanitize();
        state.focus = [ApprovalEffect::Refuses, ApprovalEffect::Other]
            .into_iter()
            .find_map(|effect| state.choices.iter().position(|c| c.effect == effect));
        state
    }

    /// Bind the key that reveals the whole subject.
    #[must_use]
    pub fn reveal(mut self, key: ApprovalKey) -> Self {
        self.reveal = (!key.reserved()).then_some(key);
        self.sanitize();
        self
    }

    /// Ignore every key until [`ApprovalState::arm`]: the host arms the state
    /// after the card was drawn, so keys typed ahead cannot answer it.
    #[must_use]
    pub fn unarmed(mut self) -> Self {
        self.armed = false;
        self
    }

    pub fn arm(&mut self) {
        self.armed = true;
    }

    #[must_use]
    pub fn choices(&self) -> &[ApprovalChoice] {
        &self.choices
    }

    #[must_use]
    pub fn reveal_key(&self) -> Option<ApprovalKey> {
        self.reveal
    }

    /// The index of the focused choice.
    #[must_use]
    pub fn focus(&self) -> Option<usize> {
        self.focus
    }

    #[must_use]
    pub fn focused(&self) -> Option<ChoiceId> {
        self.focus.map(|i| self.choices[i].id)
    }

    /// Whether choosing `id` lets the action proceed. A host that refuses a
    /// blind approval asks this of a [`ApprovalOutcome::Chose`] while the last
    /// paint [`ApprovalPaint::clipped`].
    #[must_use]
    pub fn grants(&self, id: ChoiceId) -> bool {
        self.choices
            .iter()
            .any(|c| c.id == id && c.effect == ApprovalEffect::Grants)
    }

    /// Drop chords the card owns (navigation, Enter, Esc, the reveal key) and
    /// chords an earlier choice already has, so the card never advertises a
    /// key that does something else.
    fn sanitize(&mut self) {
        let reveal = self.reveal;
        let mut taken: Vec<ApprovalKey> = Vec::new();
        for choice in &mut self.choices {
            choice
                .keys
                .retain(|k| !k.reserved() && Some(*k) != reveal && !taken.contains(k));
            taken.extend(choice.keys.iter().copied());
        }
    }

    fn step(&mut self, forward: bool) -> ApprovalOutcome {
        let len = self.choices.len();
        if len == 0 {
            return ApprovalOutcome::Ignored;
        }
        self.focus = Some(match (self.focus, forward) {
            (None, true) => 0,
            (None, false) => len - 1,
            (Some(i), true) => (i + 1) % len,
            (Some(i), false) => (i + len - 1) % len,
        });
        ApprovalOutcome::Moved
    }

    /// Apply a key press. `Enter` chooses the focused choice and nothing
    /// else; a choice's chord chooses it directly; arrows and `Tab` move
    /// focus; `Esc` is [`ApprovalOutcome::Cancelled`]. Releases, held-key
    /// repeats (for everything but moving), modified Enter/Esc/arrows and
    /// every other key are [`ApprovalOutcome::Ignored`].
    pub fn handle_key(&mut self, key: KeyEvent) -> ApprovalOutcome {
        if key.kind == KeyEventKind::Release || !self.armed {
            return ApprovalOutcome::Ignored;
        }
        let repeat = key.kind == KeyEventKind::Repeat;
        let plain = key.modifiers.is_empty();
        match key.code {
            KeyCode::Right | KeyCode::Down | KeyCode::Tab if plain => return self.step(true),
            KeyCode::Left | KeyCode::Up if plain => return self.step(false),
            KeyCode::BackTab
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                return self.step(false);
            }
            _ => {}
        }
        // A held key must never answer the card.
        if repeat {
            return ApprovalOutcome::Ignored;
        }
        match key.code {
            KeyCode::Esc if plain => return ApprovalOutcome::Cancelled,
            KeyCode::Enter if plain => {
                return self
                    .focused()
                    .map_or(ApprovalOutcome::Ignored, ApprovalOutcome::Chose);
            }
            _ => {}
        }
        if self.reveal.is_some_and(|k| k.matches(&key)) {
            return ApprovalOutcome::Reveal;
        }
        self.choices
            .iter()
            .find(|c| c.keys.iter().any(|k| k.matches(&key)))
            .map_or(ApprovalOutcome::Ignored, |c| ApprovalOutcome::Chose(c.id))
    }
}

// ---------------------------------------------------------------------------
// The card
// ---------------------------------------------------------------------------

struct Styles {
    plain: Style,
    strong: Style,
    muted: Style,
    hint: Style,
    border: Style,
    attention: Style,
    danger: Style,
    primary: Style,
    token: Style,
}

impl Styles {
    fn new(theme: &Theme) -> Self {
        let bold = Modifier::BOLD;
        Self {
            plain: theme.fg(Role::Foreground),
            strong: theme.fg(Role::Foreground).add_modifier(bold),
            muted: theme.fg(Role::Muted),
            hint: theme.fg(Role::Hint),
            border: theme.fg(Role::Border),
            attention: theme.fg(Role::Attention).add_modifier(bold),
            danger: theme.fg(Role::Danger).add_modifier(bold),
            primary: theme.fg(Role::Accent).add_modifier(bold),
            // Reversed and bold: a token is told from text by shape and
            // video, not hue, so it reads at 16 colors and under NO_COLOR.
            token: theme
                .fg(Role::Attention)
                .add_modifier(bold | Modifier::REVERSED),
        }
    }

    fn looks(&self, plain: Style) -> Looks {
        Looks {
            plain,
            token: self.token,
        }
    }
}

const MAX_ROWS: usize = 2048;

/// Between the parts of a line: ` · `, or ` - ` where marks are ASCII-safe
/// (the ASCII form of `·` is `.`, which reads as punctuation).
const fn dot(ascii: bool) -> &'static str {
    if ascii { " - " } else { " · " }
}

/// The "not shown" notice for `report`, wrapped into `width`, and whether it
/// is the whole notice. Given only `max_rows` rows it says less rather than run
/// off the end: first the later counts go, then the line count, then the
/// reveal key; the first count never does. `reveal` is the label of the key
/// that shows everything, when the host has one.
fn clip_notice(
    clip: &ApprovalClipWords,
    report: &ApprovalPaint,
    reveal: Option<String>,
    width: usize,
    max_rows: usize,
    theme: &Theme,
    st: &Styles,
) -> (Section, bool) {
    let mut first = None;
    let mut parts = Vec::new();
    if report.subject.any() {
        // `760 characters not shown (11 lines)`
        let chars = clip.hidden_chars.render(report.subject.chars);
        let mut part = chars.clone();
        if report.subject.lines > 0 {
            part.push_str(&format!(
                " ({})",
                clip.hidden_lines.render(report.subject.lines)
            ));
        }
        first = Some(chars);
        parts.push(part);
    }
    if report.preview.lines > 0 {
        parts.push(clip.hidden_preview.render(report.preview.lines));
    }
    if report.details.lines > 0 {
        parts.push(clip.hidden_details.render(report.details.lines));
    }
    if report.choices > 0 {
        parts.push(clip.hidden_choices.render(report.choices));
    }
    if parts.is_empty() {
        return (Section::new(0), true);
    }
    let reveal =
        reveal.filter(|_| report.subject.any() || report.preview.any() || report.details.any());
    let ascii = theme.ascii();
    let build = |parts: &[String], with_reveal: bool| {
        let mut sec = Section::new(MAX_ROWS);
        sec.line(width, None, |w| {
            w.words(&format!("{} ", glyphs::pick("⚠", ascii)), st.attention);
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    w.words(dot(ascii), st.border);
                }
                w.words(part, st.strong);
                // The binding sits beside the first count, where a one-row
                // notice still shows it.
                if let (0, Some(key), true) = (i, &reveal, with_reveal) {
                    w.words(dot(ascii), st.border);
                    w.words(key, st.strong);
                    w.words(&format!(" {}", clip.reveal), st.muted);
                }
            }
        });
        sec
    };
    let want_reveal = reveal.is_some();
    let mut variants: Vec<(Vec<String>, bool)> = (1..=parts.len())
        .rev()
        .map(|n| (parts[..n].to_vec(), want_reveal))
        .collect();
    if let Some(chars) = first {
        variants.push((vec![chars.clone()], want_reveal));
        variants.push((vec![chars], false));
    }
    let last = variants.len() - 1;
    for (i, (p, r)) in variants.iter().enumerate() {
        let sec = build(p, *r);
        if sec.total_rows <= max_rows || i == last {
            return (sec, i == 0);
        }
    }
    unreachable!("the last variant always returns")
}

struct Composed {
    rows: Vec<Row>,
    report: ApprovalPaint,
}

/// A decision: what the agent wants to do, where, and the choices. Paints a
/// bordered overlay panel; hand it the area the card may use.
#[derive(Clone, Debug)]
pub struct ApprovalCard<'a> {
    pub subject: &'a ApprovalSubject<'a>,
    pub state: &'a ApprovalState,
    words: Cow<'a, ApprovalWords>,
    flag_non_ascii: bool,
}

impl<'a> ApprovalCard<'a> {
    #[must_use]
    pub fn new(subject: &'a ApprovalSubject<'a>, state: &'a ApprovalState) -> Self {
        Self {
            subject,
            state,
            words: Cow::Owned(ApprovalWords::default()),
            flag_non_ascii: true,
        }
    }

    /// Words from the host, by reference.
    #[must_use]
    pub fn words(mut self, words: &'a ApprovalWords) -> Self {
        self.words = Cow::Borrowed(words);
        self
    }

    /// Say when the subject holds non-ASCII text (a homoglyph can pass for a
    /// letter). On by default; a host whose people write in other scripts
    /// turns it off.
    #[must_use]
    pub fn flag_non_ascii(mut self, on: bool) -> Self {
        self.flag_non_ascii = on;
        self
    }

    fn header(&self, theme: &Theme) -> String {
        let mark = glyphs::pick(State::NeedsYou.glyph(), theme.ascii());
        format!(
            "{mark} {}{}{}",
            self.words.needs_you,
            dot(theme.ascii()),
            self.words.kind(self.subject.kind)
        )
    }

    fn rail(&self, theme: &Theme) -> KeyHints {
        let platform = Platform::current(theme.ascii());
        KeyHints::new(vec![
            KeyHint::new(
                keys::pair_label(KeyCode::Left, KeyCode::Right, platform),
                self.words.move_focus.clone(),
            ),
            KeyHint::new("Enter", self.words.choose_focused.clone()),
            KeyHint::new("Esc", self.words.cancel.clone()),
        ])
    }

    /// The panel is only the frame: its own rail of hints would cost three
    /// rows even where the card is short of them, so the card draws the
    /// hints itself, last, and only where there is room.
    fn panel(&self, theme: &Theme) -> Panel<'static> {
        Panel::new(Depth::Overlay)
            .title(self.header(theme))
            .focused(true)
    }

    fn choices_section(&self, width: usize, cap: usize, theme: &Theme, st: &Styles) -> Section {
        let platform = Platform::current(theme.ascii());
        let selected = theme.bg(Role::Selected);
        let items = self
            .state
            .choices()
            .iter()
            .enumerate()
            .map(|(i, choice)| {
                let focused = self.state.focus() == Some(i);
                let patch = |s: Style| if focused { s.patch(selected) } else { s };
                let mut units = trusted(
                    glyphs::pick(glyphs::selection_marker(focused), theme.ascii()),
                    patch(st.primary),
                );
                units.extend(trusted(" ", patch(st.plain)));
                if let Some(key) = choice.keys.first() {
                    units.extend(trusted(&key.label(platform), patch(st.strong)));
                    units.extend(trusted(" ", patch(st.plain)));
                }
                let label = if focused { st.strong } else { st.plain };
                units.extend(trusted(&choice.label, patch(label)));
                units
            })
            .collect();
        let sep = if theme.ascii() {
            trusted("  ", st.plain)
        } else {
            trusted(dot(false), st.border)
        };
        pack(items, &sep, width, cap)
    }

    /// The "not shown" notice for `report`; see [`clip_notice`].
    fn notice(
        &self,
        report: &ApprovalPaint,
        width: usize,
        max_rows: usize,
        theme: &Theme,
        st: &Styles,
    ) -> (Section, bool) {
        let reveal = self
            .state
            .reveal_key()
            .map(|key| key.label(Platform::current(theme.ascii())));
        clip_notice(&self.words.clip, report, reveal, width, max_rows, theme, st)
    }

    /// Lay the card's body out in `width` x `h`. Every part wraps; what does
    /// not fit is counted, and the choices come first.
    fn compose(&self, width: usize, h: usize, theme: &Theme) -> Composed {
        let st = Styles::new(theme);
        let ascii = theme.ascii();
        let words = &*self.words;
        let subject = self.subject;
        let cap = h.min(MAX_ROWS);
        let mut non_ascii = 0;

        // Agent.
        let mut agent = Section::new(cap);
        if let Some(a) = &subject.agent {
            agent.line(width, None, |w| {
                w.words(&format!("{} ", words.requested_by), st.muted);
                non_ascii += w.subject(&a.name, ascii, &st.looks(st.strong));
                if a.sub_agent {
                    w.words(&format!(" ({})", words.sub_agent), st.muted);
                }
            });
        }

        // The subject, verbatim.
        let mut main = Section::new(cap);
        let first = if subject.kind == ApprovalKind::Command {
            "$ ".to_owned()
        } else {
            format!("{} ", glyphs::pick("›", ascii))
        };
        let gutter = Some((first.as_str(), st.primary, "  ", st.plain));
        if subject.target.is_empty() {
            main.line(width, gutter, |w| w.words(&words.clip.empty, st.muted));
        } else {
            main.line(width, gutter, |w| {
                non_ascii += w.subject(&subject.target, ascii, &st.looks(st.strong));
            });
        }

        // Elevation and scope: a mark and words, never color alone.
        let mut facts = Section::new(cap);
        if subject.kind == ApprovalKind::Elevation {
            facts.line(width, None, |w| {
                w.words(&format!("{} ", glyphs::pick("⚠", ascii)), st.danger);
                w.words(&words.elevated, st.danger);
            });
        }
        match subject.scope {
            ApprovalScope::Outside => facts.line(width, None, |w| {
                w.words(&format!("{} ", glyphs::pick("⚠", ascii)), st.attention);
                w.words(&words.outside, st.attention);
            }),
            ApprovalScope::Inside => facts.line(width, None, |w| w.words(&words.inside, st.muted)),
            ApprovalScope::Unchecked => facts.line(width, None, |w| {
                w.words(
                    &format!(
                        "{} {}",
                        glyphs::pick(glyphs::UNKNOWN, ascii),
                        words.unchecked
                    ),
                    st.muted,
                );
            }),
        }

        let mut cwd = Section::new(cap);
        if let Some(dir) = &subject.cwd {
            cwd.line(width, None, |w| {
                w.words(&format!("{} ", words.in_dir), st.muted);
                non_ascii += w.subject(dir, ascii, &st.looks(st.plain));
            });
        }

        let mut notes = Section::new(cap);
        for note in &subject.risk_notes {
            notes.line(width, None, |w| {
                w.words(&format!("{}: ", words.risk), st.attention);
                w.prose(note, ascii, &st.looks(st.plain));
            });
        }

        let mut preview = Section::new(cap);
        if !subject.preview.is_empty() {
            preview.line(width, None, |w| w.words(&words.preview, st.muted));
            let rail = format!("{} ", glyphs::pick("│", ascii));
            for line in &subject.preview {
                let gutter = Some((rail.as_str(), st.border, rail.as_str(), st.border));
                preview.line(width, gutter, |w| {
                    w.prose(line, ascii, &st.looks(st.plain));
                });
            }
        }

        let mut flag = Section::new(cap);
        if self.flag_non_ascii && non_ascii > 0 {
            flag.line(width, None, |w| {
                w.words(&format!("{} ", glyphs::pick("⚠", ascii)), st.attention);
                w.words(&words.non_ascii.render(non_ascii), st.attention);
            });
        }

        let choices = self.choices_section(width, cap, theme, &st);

        // Display order. Priority: the first row of the subject, then where
        // and how (the facts), the rest of the subject, the agent, risk
        // notes, the non-ASCII note, the directory and the preview.
        let sections = [&agent, &main, &facts, &cwd, &notes, &preview, &flag];
        let steps: [(usize, usize); 8] = [
            (1, 1),
            (2, usize::MAX),
            (1, usize::MAX),
            (0, usize::MAX),
            (4, usize::MAX),
            (6, usize::MAX),
            (3, usize::MAX),
            (5, usize::MAX),
        ];
        let body_total: usize = sections.iter().map(|s| s.total_rows).sum();
        let mut choice_rows = choices.rows.len().min(h);
        if choice_rows == h && h > 0 {
            // No row would be left for the notice that must say the body was
            // cut: it takes the last choice row. A card that shows its
            // choices and hides what they are for would be a blind approval
            // with a button.
            choice_rows -= 1;
        }
        let after_choices = h - choice_rows;

        let choices_hidden = choices.total_chars - choices_shown(&choices, choice_rows);
        // Hand out `after_choices - reserve` rows in priority order.
        let allocate = |reserve: usize| {
            let mut left = after_choices - reserve;
            let mut taken = [0usize; 7];
            for (i, upto) in steps {
                let more = (sections[i].total_rows.min(upto) - taken[i].min(upto)).min(left);
                taken[i] += more;
                left -= more;
            }
            let sum = |ids: &[usize]| {
                ids.iter().fold(ApprovalHidden::default(), |acc, &i| {
                    acc.plus(sections[i].hidden(taken[i]))
                })
            };
            let report = ApprovalPaint {
                subject: sections[1].hidden(taken[1]),
                preview: sections[5].hidden(taken[5]),
                details: sum(&[0, 2, 3, 4, 6]),
                choices: choices_hidden,
                non_ascii,
            };
            (taken, left, report)
        };

        // If the body cannot all fit, reserve the fewest rows that hold the
        // notice saying so: every row spent on the notice is one fewer for
        // the card's facts.
        let mut reserve = 0;
        if body_total > after_choices {
            reserve = 1.min(after_choices);
            while reserve < after_choices {
                let (_, _, report) = allocate(reserve);
                if self.notice(&report, width, reserve, theme, &st).1 {
                    break;
                }
                reserve += 1;
            }
        }
        let (taken, mut left, report) = allocate(reserve);

        let mut rows = Vec::new();
        for (i, sec) in sections.iter().enumerate() {
            rows.extend(sec.rows[..taken[i].min(sec.rows.len())].iter().cloned());
        }
        if report.clipped() {
            let (notice, _) = self.notice(&report, width, reserve, theme, &st);
            rows.extend(notice.rows.into_iter().take(reserve));
        } else if left > 0 {
            rows.push(Row {
                spans: Vec::new(),
                units: 0,
            });
            left -= 1;
        }
        rows.extend(choices.rows.iter().take(choice_rows).cloned());
        // The key hints come last, and only where there is room for them.
        let rail = self
            .rail(theme)
            .lines(u16::try_from(width).unwrap_or(u16::MAX), theme);
        if !report.clipped() && !rail.is_empty() && left > rail.len() {
            rows.push(Row {
                spans: Vec::new(),
                units: 0,
            });
            rows.extend(rail.into_iter().map(|line| Row {
                spans: line.spans,
                units: 0,
            }));
        }
        Composed { rows, report }
    }

    /// Paint the card and say what it did not show.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) -> ApprovalPaint {
        let area = area.intersection(buf.area);
        let inner = self.panel(theme).draw(area, buf, theme);
        let inner = inner.intersection(area);
        let composed = self.compose(usize::from(inner.width), usize::from(inner.height), theme);
        paint_rows(&composed.rows, inner, buf);
        composed.report
    }
}

fn choices_shown(choices: &Section, shown_rows: usize) -> usize {
    choices.rows[..shown_rows.min(choices.rows.len())]
        .iter()
        .map(|r| r.units)
        .sum()
}

fn paint_rows(rows: &[Row], area: Rect, buf: &mut Buffer) {
    for (i, row) in rows.iter().enumerate().take(usize::from(area.height)) {
        let y = area.y + u16::try_from(i).unwrap_or(u16::MAX);
        buf.set_line(area.x, y, &Line::from(row.spans.clone()), area.width);
    }
}

impl Paint for ApprovalCard<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.render(area, buf, theme);
    }

    /// Rows that show everything at `width`.
    fn height(&self, width: u16, theme: &Theme) -> u16 {
        let panel = self.panel(theme);
        let probe = |h: u16| {
            let area = Rect::new(0, 0, width, h);
            panel.draw(area, &mut Buffer::empty(area), theme)
        };
        let rows = self
            .compose(usize::from(probe(64).width), MAX_ROWS, theme)
            .rows
            .len();
        let wanted = u16::try_from(rows).unwrap_or(u16::MAX / 2);
        (wanted..=wanted.saturating_add(16))
            .find(|h| usize::from(probe(*h).height) >= rows)
            .unwrap_or_else(|| wanted.saturating_add(16))
    }
}

// ---------------------------------------------------------------------------
// Review verdicts
// ---------------------------------------------------------------------------

/// What an automated reviewer concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReviewKind {
    /// The reviewer let the action proceed.
    Allowed,
    /// The reviewer stopped the action.
    Denied,
    /// The reviewer set the action aside for the person.
    Held,
    /// The review failed or timed out. This is not a verdict about the
    /// action, and it never looks like one.
    Undecided,
}

impl ReviewKind {
    pub const ALL: [ReviewKind; 4] = [
        ReviewKind::Allowed,
        ReviewKind::Denied,
        ReviewKind::Held,
        ReviewKind::Undecided,
    ];

    /// The product state whose mark and hue this verdict wears. Four marks:
    /// `✓` `✕` `◆` `?`, which stay four different shapes in ASCII.
    #[must_use]
    pub const fn state(self) -> State {
        match self {
            ReviewKind::Allowed => State::Done,
            ReviewKind::Denied => State::Failed,
            ReviewKind::Held => State::NeedsYou,
            ReviewKind::Undecided => State::Unknown,
        }
    }

    /// Whether the person has to do something about this verdict.
    #[must_use]
    pub const fn needs_a_next_step(self) -> bool {
        !matches!(self, ReviewKind::Allowed)
    }
}

/// The words of a verdict. English by default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewWords {
    pub allowed: Cow<'static, str>,
    pub denied: Cow<'static, str>,
    pub held: Cow<'static, str>,
    pub undecided: Cow<'static, str>,
    pub why: Cow<'static, str>,
    pub waiting_for: Cow<'static, str>,
    /// Under a held verdict.
    pub held_note: Cow<'static, str>,
    /// Under an undecided one: it is not a verdict.
    pub undecided_note: Cow<'static, str>,
    pub clip: ApprovalClipWords,
}

impl Default for ReviewWords {
    fn default() -> Self {
        Self {
            allowed: "Allowed".into(),
            denied: "Denied".into(),
            held: "Held for you".into(),
            undecided: "Could not decide".into(),
            why: "Why".into(),
            waiting_for: "Waiting for".into(),
            held_note: "Your call.".into(),
            undecided_note: "No verdict. This is not a safety decision: your call.".into(),
            clip: ApprovalClipWords::default(),
        }
    }
}

impl ReviewWords {
    fn word(&self, kind: ReviewKind) -> &str {
        match kind {
            ReviewKind::Allowed => &self.allowed,
            ReviewKind::Denied => &self.denied,
            ReviewKind::Held => &self.held,
            ReviewKind::Undecided => &self.undecided,
        }
    }
}

/// A held, denied or undecided verdict that offers no next step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewError;

impl std::fmt::Display for ReviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a denied, held or undecided review needs an enabled key or a waiting-for line"
        )
    }
}

impl std::error::Error for ReviewError {}

/// An automated reviewer's verdict on one action, in the transcript.
#[derive(Clone, Debug)]
pub struct ReviewVerdict<'a> {
    pub kind: ReviewKind,
    pub reviewer: Cow<'a, str>,
    /// One line: why. Wrapped, never cut.
    pub reason: Cow<'a, str>,
    /// The action, verbatim, when the verdict should show it.
    pub subject: Option<Cow<'a, str>>,
    /// A plain word after the reviewer: "Deletes unseen files". No hue.
    pub category: Option<Cow<'a, str>>,
    /// What must happen first, as text with no key.
    pub waiting_for: Option<Cow<'a, str>>,
    /// Right-aligned in the quietest ink when it fits.
    pub id: Option<Cow<'a, str>>,
    /// What the person can do about it.
    pub hints: &'a KeyHints,
    words: Cow<'a, ReviewWords>,
}

impl<'a> ReviewVerdict<'a> {
    /// A denied, held or undecided verdict must say what changes its outcome:
    /// an enabled key in `hints`, or a `waiting_for` line. Otherwise the
    /// person is told something stopped and given nothing to do about it.
    pub fn new(
        kind: ReviewKind,
        reviewer: impl Into<Cow<'a, str>>,
        reason: impl Into<Cow<'a, str>>,
        hints: &'a KeyHints,
        waiting_for: Option<Cow<'a, str>>,
    ) -> Result<Self, ReviewError> {
        let has_key = hints.items.iter().any(|h| h.enabled);
        let has_wait = waiting_for.as_deref().is_some_and(|w| !w.trim().is_empty());
        if kind.needs_a_next_step() && !has_key && !has_wait {
            return Err(ReviewError);
        }
        Ok(Self {
            kind,
            reviewer: reviewer.into(),
            reason: reason.into(),
            subject: None,
            category: None,
            waiting_for,
            id: None,
            hints,
            words: Cow::Owned(ReviewWords::default()),
        })
    }

    #[must_use]
    pub fn subject(mut self, subject: impl Into<Cow<'a, str>>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    #[must_use]
    pub fn category(mut self, category: impl Into<Cow<'a, str>>) -> Self {
        self.category = Some(category.into());
        self
    }

    #[must_use]
    pub fn id(mut self, id: impl Into<Cow<'a, str>>) -> Self {
        self.id = Some(id.into());
        self
    }

    #[must_use]
    pub fn words(mut self, words: &'a ReviewWords) -> Self {
        self.words = Cow::Borrowed(words);
        self
    }

    fn compose(&self, width: usize, h: usize, theme: &Theme) -> Composed {
        let st = Styles::new(theme);
        let ascii = theme.ascii();
        let words = &*self.words;
        let cap = h.min(MAX_ROWS);
        let state = self.kind.state();
        let hue = theme.fg(state.role());

        let mut head = Section::new(cap);
        head.line(width, None, |w| {
            w.words(&format!("{} ", glyphs::pick(state.glyph(), ascii)), hue);
            w.words(words.word(self.kind), st.strong);
            w.words(dot(ascii), st.border);
            w.subject(&self.reviewer, ascii, &st.looks(st.muted));
            if let Some(category) = &self.category {
                w.words(dot(ascii), st.border);
                w.words(category, st.muted);
            }
        });
        if let (Some(id), 1, Some(row)) = (&self.id, head.total_rows, head.rows.first_mut()) {
            let used: usize = row.spans.iter().map(|s| text::width(&s.content)).sum();
            let id = text::display_safe(id);
            let id_w = text::width(&id);
            if used + 2 + id_w <= width {
                row.spans.push(Span::raw(" ".repeat(width - used - id_w)));
                row.spans.push(Span::styled(id.into_owned(), st.hint));
            }
        }

        let mut subject = Section::new(cap);
        if let Some(src) = &self.subject {
            let gutter = Some(("$ ", st.primary, "  ", st.plain));
            subject.line(width, gutter, |w| {
                w.subject(src, ascii, &st.looks(st.strong));
            });
        }

        let mut reason = Section::new(cap);
        reason.line(width, None, |w| {
            w.words(&format!("{}: ", words.why), st.muted);
            w.prose(&self.reason, ascii, &st.looks(st.plain));
        });

        let mut note = Section::new(cap);
        let note_text = match self.kind {
            ReviewKind::Held => Some(&words.held_note),
            ReviewKind::Undecided => Some(&words.undecided_note),
            ReviewKind::Allowed | ReviewKind::Denied => None,
        };
        if let Some(n) = note_text {
            note.line(width, None, |w| w.words(n, st.strong));
        }
        let mut waiting = Section::new(cap);
        if let Some(wait) = &self.waiting_for {
            waiting.line(width, None, |w| {
                w.words(&format!("{}: ", words.waiting_for), st.muted);
                w.prose(wait, ascii, &st.looks(st.plain));
            });
        }

        // A key that would do nothing is not shown.
        let enabled = KeyHints::new(
            self.hints
                .items
                .iter()
                .filter(|h| h.enabled)
                .cloned()
                .collect(),
        );
        let hint_lines = enabled.lines(u16::try_from(width).unwrap_or(u16::MAX), theme);
        let mut hints = Section::new(cap);
        for line in hint_lines {
            hints.total_rows += 1;
            if hints.rows.len() < cap {
                hints.rows.push(Row {
                    spans: line.spans,
                    units: 0,
                });
            }
        }

        // Display order: head, subject, reason, note, waiting, notice, hints.
        // Priority: the head, the hints, the action, then the reasons.
        let sections = [&head, &subject, &reason, &note, &waiting, &hints];
        let priority = [0, 5, 1, 2, 3, 4];
        let body_total: usize = sections.iter().map(|s| s.total_rows).sum();
        let allocate = |reserve: usize| {
            let mut left = h - reserve;
            let mut taken = [0usize; 6];
            for i in priority {
                taken[i] = sections[i].total_rows.min(left);
                left -= taken[i];
            }
            let report = ApprovalPaint {
                subject: subject.hidden(taken[1]),
                preview: ApprovalHidden::default(),
                details: head
                    .hidden(taken[0])
                    .plus(reason.hidden(taken[2]))
                    .plus(note.hidden(taken[3]))
                    .plus(waiting.hidden(taken[4])),
                choices: hints.total_rows.saturating_sub(taken[5]),
                non_ascii: 0,
            };
            (taken, report)
        };
        let mut reserve = 0;
        if body_total > h {
            reserve = 1.min(h);
            while reserve < h {
                let (_, report) = allocate(reserve);
                if clip_notice(&words.clip, &report, None, width, reserve, theme, &st).1 {
                    break;
                }
                reserve += 1;
            }
        }
        let (taken, report) = allocate(reserve);
        let mut rows = Vec::new();
        for i in [0, 1, 2, 3, 4] {
            rows.extend(
                sections[i].rows[..taken[i].min(sections[i].rows.len())]
                    .iter()
                    .cloned(),
            );
        }
        if report.clipped() {
            let (notice, _) = clip_notice(&words.clip, &report, None, width, reserve, theme, &st);
            rows.extend(notice.rows.into_iter().take(reserve));
        }
        rows.extend(hints.rows[..taken[5].min(hints.rows.len())].iter().cloned());
        Composed { rows, report }
    }

    /// Paint the verdict and say what it did not show.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) -> ApprovalPaint {
        let area = area.intersection(buf.area);
        let composed = self.compose(usize::from(area.width), usize::from(area.height), theme);
        paint_rows(&composed.rows, area, buf);
        composed.report
    }
}

impl Paint for ReviewVerdict<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.render(area, buf, theme);
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        let rows = self.compose(usize::from(width), MAX_ROWS, theme).rows.len();
        u16::try_from(rows).unwrap_or(u16::MAX)
    }
}

// ---------------------------------------------------------------------------
// The aggregate
// ---------------------------------------------------------------------------

/// The words of the aggregate row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewAggregateWords {
    pub title: Cow<'static, str>,
    pub allowed: Cow<'static, str>,
    pub denied: Cow<'static, str>,
    pub held: Cow<'static, str>,
    pub undecided: Cow<'static, str>,
    pub reviewing: Cow<'static, str>,
    pub none_yet: Cow<'static, str>,
}

impl Default for ReviewAggregateWords {
    fn default() -> Self {
        Self {
            title: "Auto-Review".into(),
            allowed: "allowed".into(),
            denied: "denied".into(),
            held: "held for you".into(),
            undecided: "could not decide".into(),
            reviewing: "reviewing".into(),
            none_yet: "nothing checked yet".into(),
        }
    }
}

/// Several verdicts as one row with real counts:
/// `Auto-Review · ✓ 42 allowed · ✕ 3 denied · ◆ 1 held for you · ? 1 could
/// not decide`. A count of zero is left out; a row of zeros says nothing has
/// been checked. Static: no spinner and no highlight.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewAggregate {
    pub allowed: u32,
    pub denied: u32,
    pub held: u32,
    pub undecided: u32,
    /// Reviews running now, shown as `● reviewing 2`.
    pub reviewing: u32,
    pub words: ReviewAggregateWords,
}

impl ReviewAggregate {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Count verdicts.
    #[must_use]
    pub fn from_kinds(kinds: impl IntoIterator<Item = ReviewKind>) -> Self {
        let mut agg = Self::default();
        for kind in kinds {
            match kind {
                ReviewKind::Allowed => agg.allowed += 1,
                ReviewKind::Denied => agg.denied += 1,
                ReviewKind::Held => agg.held += 1,
                ReviewKind::Undecided => agg.undecided += 1,
            }
        }
        agg
    }

    #[must_use]
    pub fn reviewing(mut self, n: u32) -> Self {
        self.reviewing = n;
        self
    }

    fn section(&self, width: usize, cap: usize, theme: &Theme) -> Section {
        let st = Styles::new(theme);
        let ascii = theme.ascii();
        let words = &self.words;
        let mut items = vec![trusted(&words.title, st.strong)];
        let counted = [
            (ReviewKind::Allowed, self.allowed, &words.allowed),
            (ReviewKind::Denied, self.denied, &words.denied),
            (ReviewKind::Held, self.held, &words.held),
            (ReviewKind::Undecided, self.undecided, &words.undecided),
        ];
        let mut any = false;
        for (kind, n, word) in counted {
            if n == 0 {
                continue;
            }
            any = true;
            let state = kind.state();
            let mut unit = trusted(
                &format!("{} ", glyphs::pick(state.glyph(), ascii)),
                theme.fg(state.role()),
            );
            unit.extend(trusted(&format!("{n} "), st.strong));
            unit.extend(trusted(word, st.plain));
            items.push(unit);
        }
        if self.reviewing > 0 {
            any = true;
            let mut unit = trusted(
                &format!("{} ", glyphs::pick(State::Working.glyph(), ascii)),
                theme.fg(State::Working.role()),
            );
            unit.extend(trusted(&format!("{} ", words.reviewing), st.plain));
            unit.extend(trusted(&self.reviewing.to_string(), st.strong));
            items.push(unit);
        }
        if !any {
            items.push(trusted(&words.none_yet, st.muted));
        }
        let sep = if ascii {
            trusted("  ", st.plain)
        } else {
            trusted(dot(ascii), st.border)
        };
        pack(items, &sep, width, cap)
    }

    /// Paint the row; returns how many items did not fit.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) -> usize {
        let area = area.intersection(buf.area);
        let sec = self.section(usize::from(area.width), usize::from(area.height), theme);
        let hidden = sec.total_chars - choices_shown(&sec, sec.rows.len());
        paint_rows(&sec.rows, area, buf);
        hidden
    }
}

impl Paint for ReviewAggregate {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.render(area, buf, theme);
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        let sec = self.section(usize::from(width), MAX_ROWS, theme);
        u16::try_from(sec.total_rows).unwrap_or(u16::MAX)
    }
}

// ---------------------------------------------------------------------------
// Native bottom decision band over host-projected display facts
// ---------------------------------------------------------------------------

/// A caller-owned option in canonical decision order. The band never handles
/// its key or decides its effect. A withheld persistent action retains an empty
/// rectangle at this index so pointer dispatch cannot change its meaning.
#[derive(Clone, Debug)]
pub struct DecisionBandAction {
    pub line: Line<'static>,
    pub persistent: bool,
}

/// Validated rule coverage from the host. The band owns full/compact display
/// fitting; it never reparses a command or constructs a permission rule.
#[derive(Clone, Debug)]
pub struct DecisionBandSave {
    pub summary: String,
    pub entries: Vec<String>,
    pub omitted: usize,
    pub label: String,
    pub separator: String,
    /// `{count}` is replaced with the number of additional rules.
    pub compact_more: String,
    pub full_more: String,
    pub label_style: Style,
    pub summary_style: Style,
    pub entries_style: Style,
    pub more_style: Style,
}

impl DecisionBandSave {
    fn lines(&self, width: u16, compact: bool) -> Vec<Line<'static>> {
        let entries = self.entries.join("; ");
        if compact {
            let more = if self.omitted > 0 {
                self.compact_more
                    .replace("{count}", &self.omitted.to_string())
            } else {
                String::new()
            };
            let budget = usize::from(width)
                .saturating_sub(
                    2 + self.label.chars().count()
                        + self.summary.chars().count()
                        + self.separator.chars().count()
                        + more.chars().count(),
                )
                .max(12);
            return vec![Line::from(vec![
                Span::raw("  "),
                Span::styled(self.label.clone(), self.label_style),
                Span::styled(self.summary.clone(), self.summary_style),
                Span::styled(
                    format!(
                        "{}{}{more}",
                        self.separator,
                        band_byte_clip(&entries, budget)
                    ),
                    self.entries_style,
                ),
            ])];
        }
        let mut lines = vec![
            Line::from(vec![
                Span::raw("  "),
                Span::styled(self.label.clone(), self.label_style),
                Span::styled(self.summary.clone(), self.summary_style),
            ]),
            Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    band_byte_clip(&entries, usize::from(width.saturating_sub(10)).max(20)),
                    self.entries_style,
                ),
            ]),
        ];
        if self.omitted > 0 {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(
                    self.full_more.replace("{count}", &self.omitted.to_string()),
                    self.more_style,
                ),
            ]));
        }
        lines
    }
}

fn band_byte_clip(value: &str, budget: usize) -> String {
    // Retain the native host's existing UTF-8-boundary preview contract.
    // This coverage is informational; full validated rules stay with the host.
    if value.len() <= budget {
        return value.to_owned();
    }
    let budget = budget.saturating_sub(3);
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= budget)
        .last()
        .unwrap_or(0);
    format!("{}...", &value[..end])
}

fn band_safe_span(span: &Span<'static>) -> Span<'static> {
    let mut safe = span.clone();
    if let Cow::Owned(value) = text::display_safe(&span.content) {
        safe.content = Cow::Owned(value);
    }
    safe
}

fn band_safe_line(line: &Line<'static>) -> Line<'static> {
    let mut safe = line.clone();
    safe.spans = line.spans.iter().map(band_safe_span).collect();
    safe
}

/// Exact Ratatui word-wrap measurement, shared by band layout and painting.
#[must_use]
pub fn decision_wrapped_rows(lines: &[Line<'_>], width: u16) -> u16 {
    use ratatui::widgets::{Paragraph, Wrap};
    let safe: Vec<_> = lines
        .iter()
        .map(|line| {
            let mut safe = line.clone();
            for span in &mut safe.spans {
                if let Cow::Owned(value) = text::display_safe(&span.content) {
                    span.content = Cow::Owned(value);
                }
            }
            safe
        })
        .collect();
    let rows = if width == 0 {
        lines.len()
    } else {
        Paragraph::new(safe)
            .wrap(Wrap { trim: false })
            .line_count(width)
    };
    u16::try_from(rows).unwrap_or(u16::MAX)
}

/// A native approval band over already-projected host display facts. Labels,
/// styles, badges, risk/effect/owner data and decisions remain host authority.
/// Every span and coverage string is run through [`text::display_safe`] before
/// measurement and painting, preserving styles while removing bidi/controls.
/// These are logical lines; final word-wrap, fit, save visibility, paint and
/// pointer rectangles belong to the single plan below. No input handler or
/// ApprovalState is constructed for this presentation.
#[derive(Clone, Debug)]
pub struct DecisionBand {
    pub body: Vec<Line<'static>>,
    pub saves: Vec<DecisionBandSave>,
    pub question: Line<'static>,
    pub actions: Vec<DecisionBandAction>,
    pub footer: Line<'static>,
    pub save_hint: Option<Span<'static>>,
    pub background: Style,
    pub rule: Span<'static>,
    pub truncation_hint: Span<'static>,
    pub collapsed: Option<Line<'static>>,
}

/// The complete painted contract. Empty action boxes remain at their original
/// indices. A host can enable persistent keys only while `save_shown` is true.
#[derive(Clone, Debug)]
pub struct DecisionBandPlan {
    pub region: Rect,
    pub body_rect: Rect,
    pub control_rect: Rect,
    pub save_rect: Rect,
    pub action_rects: Vec<Rect>,
    pub save_shown: bool,
    background: Style,
    rule: Span<'static>,
    hint: Span<'static>,
    head: Vec<Line<'static>>,
    save: Vec<Line<'static>>,
    controls: Vec<Line<'static>>,
    head_truncated: bool,
    collapsed: Option<Line<'static>>,
}

impl DecisionBand {
    fn controls(&self, offer_save: bool) -> Vec<Line<'static>> {
        let mut controls = vec![self.question.clone()];
        controls.extend(
            self.actions
                .iter()
                .filter(|action| offer_save || !action.persistent)
                .map(|action| action.line.clone()),
        );
        let mut footer = self.footer.clone();
        if offer_save && let Some(hint) = &self.save_hint {
            footer.spans.push(hint.clone());
        }
        controls.push(footer);
        controls
    }

    #[must_use]
    pub fn plan(&self, area: Rect) -> DecisionBandPlan {
        self.sanitized().plan_safe(area)
    }

    fn sanitized(&self) -> Self {
        let mut safe = self.clone();
        safe.body = self.body.iter().map(band_safe_line).collect();
        safe.question = band_safe_line(&self.question);
        for action in &mut safe.actions {
            action.line = band_safe_line(&action.line);
        }
        safe.footer = band_safe_line(&self.footer);
        safe.save_hint = self.save_hint.as_ref().map(band_safe_span);
        safe.rule = band_safe_span(&self.rule);
        safe.truncation_hint = band_safe_span(&self.truncation_hint);
        safe.collapsed = self.collapsed.as_ref().map(band_safe_line);
        for save in &mut safe.saves {
            save.summary = text::display_safe(&save.summary).into_owned();
            for entry in &mut save.entries {
                *entry = text::display_safe(entry).into_owned();
            }
            save.label = text::display_safe(&save.label).into_owned();
            save.separator = text::display_safe(&save.separator).into_owned();
            save.compact_more = text::display_safe(&save.compact_more).into_owned();
            save.full_more = text::display_safe(&save.full_more).into_owned();
        }
        safe
    }

    fn plan_safe(&self, area: Rect) -> DecisionBandPlan {
        let mut result = DecisionBandPlan {
            region: Rect::new(area.x, area.bottom(), 0, 0),
            body_rect: Rect::default(),
            control_rect: Rect::default(),
            save_rect: Rect::default(),
            action_rects: vec![Rect::default(); self.actions.len()],
            save_shown: false,
            background: self.background,
            rule: self.rule.clone(),
            hint: self.truncation_hint.clone(),
            head: Vec::new(),
            save: Vec::new(),
            controls: Vec::new(),
            head_truncated: false,
            collapsed: None,
        };
        if area.is_empty() {
            return result;
        }
        if let Some(line) = &self.collapsed {
            result.region = Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1);
            result.collapsed = Some(line.clone());
            return result;
        }
        let compact_save: Vec<_> = self
            .saves
            .iter()
            .flat_map(|save| save.lines(area.width, true))
            .collect();
        let save_reserve = decision_wrapped_rows(&compact_save, area.width);
        let controls_with_save = self.controls(true);
        let mut compact_body = self.body.clone();
        compact_body.extend(compact_save.iter().cloned());
        let compact_region = band_region(area, &compact_body, save_reserve, &controls_with_save);
        let compact_inner = compact_region.height.saturating_sub(1);
        let controls_rows =
            decision_wrapped_rows(&controls_with_save, area.width).min(compact_inner);
        let save_shown =
            !compact_save.is_empty() && save_reserve <= compact_inner.saturating_sub(controls_rows);
        let (body, save, controls, reserve) = if save_shown {
            let full_save: Vec<_> = self
                .saves
                .iter()
                .flat_map(|save| save.lines(area.width, false))
                .collect();
            let mut full_body = self.body.clone();
            full_body.extend(full_save.iter().cloned());
            let full_region = band_region(area, &full_body, save_reserve, &controls_with_save);
            let full_inner = full_region.height.saturating_sub(1);
            let full_controls =
                decision_wrapped_rows(&controls_with_save, area.width).min(full_inner);
            if decision_wrapped_rows(&full_body, area.width)
                <= full_inner.saturating_sub(full_controls)
            {
                (full_body, full_save, controls_with_save, save_reserve)
            } else {
                (compact_body, compact_save, controls_with_save, save_reserve)
            }
        } else {
            (self.body.clone(), Vec::new(), self.controls(false), 0)
        };
        result.region = band_region(area, &body, reserve, &controls);
        result.save_shown = save_shown;
        let inner = result.region.height.saturating_sub(1);
        let control_rows = decision_wrapped_rows(&controls, area.width).min(inner);
        let body_height = inner.saturating_sub(control_rows);
        result.body_rect = Rect::new(
            area.x,
            result.region.y.saturating_add(1),
            area.width,
            body_height,
        );
        result.control_rect =
            Rect::new(area.x, result.body_rect.bottom(), area.width, control_rows);
        let body_truncated = decision_wrapped_rows(&body, area.width) > body_height;
        let save_rows = if body_truncated {
            decision_wrapped_rows(&save, area.width).min(body_height)
        } else {
            decision_wrapped_rows(&save, area.width)
        };
        result.save_rect = Rect::new(
            area.x,
            if body_truncated {
                result.body_rect.bottom().saturating_sub(save_rows)
            } else {
                result
                    .body_rect
                    .y
                    .saturating_add(decision_wrapped_rows(&self.body, area.width))
            },
            area.width,
            save_rows,
        );
        result.head_truncated = body_truncated;
        result.head = if body_truncated {
            self.body.clone()
        } else {
            body
        };
        result.save = if body_truncated { save } else { Vec::new() };
        let mut shown_index = 0;
        for (index, action) in self.actions.iter().enumerate() {
            if action.persistent && !save_shown {
                continue;
            }
            let first = 1 + shown_index;
            shown_index += 1;
            let top = decision_wrapped_rows(&controls[..first], area.width);
            let bottom = decision_wrapped_rows(&controls[..first + 1], area.width);
            let y = result.control_rect.y.saturating_add(top);
            let height = bottom
                .saturating_sub(top)
                .min(result.control_rect.bottom().saturating_sub(y));
            if height > 0 {
                result.action_rects[index] = Rect::new(area.x, y, area.width, height);
            }
        }
        result.controls = controls;
        result
    }

    /// Paint and return the exact interactive contract for the visible buffer.
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> DecisionBandPlan {
        let plan = self.plan(area.intersection(buf.area));
        plan.paint(buf);
        plan
    }
}

impl DecisionBandPlan {
    fn paint(&self, buf: &mut Buffer) {
        use ratatui::widgets::{Block, Clear, Paragraph, Widget, Wrap};
        if self.region.is_empty() {
            return;
        }
        Clear.render(self.region, buf);
        if let Some(line) = &self.collapsed {
            Paragraph::new(line.clone()).render(self.region, buf);
            return;
        }
        Block::default()
            .style(self.background)
            .render(self.region, buf);
        let rule = self.rule.content.repeat(usize::from(self.region.width));
        buf.set_stringn(
            self.region.x,
            self.region.y,
            rule,
            usize::from(self.region.width),
            self.rule.style,
        );
        if self.head_truncated {
            let head_height = self.body_rect.height.saturating_sub(self.save_rect.height);
            if head_height > 0 {
                let shown = head_height.saturating_sub(1);
                if shown > 0 {
                    Paragraph::new(self.head.clone())
                        .wrap(Wrap { trim: false })
                        .render(
                            Rect {
                                height: shown,
                                ..self.body_rect
                            },
                            buf,
                        );
                }
                buf.set_span(
                    self.region.x,
                    self.body_rect.y.saturating_add(shown),
                    &self.hint,
                    self.region.width,
                );
            }
            if self.save_rect.height > 0 {
                Paragraph::new(self.save.clone())
                    .wrap(Wrap { trim: false })
                    .render(self.save_rect, buf);
            }
        } else {
            Paragraph::new(self.head.clone())
                .wrap(Wrap { trim: false })
                .render(self.body_rect, buf);
        }
        Paragraph::new(self.controls.clone())
            .wrap(Wrap { trim: false })
            .render(self.control_rect, buf);
    }
}

fn band_region(
    area: Rect,
    body: &[Line<'static>],
    save_rows: u16,
    controls: &[Line<'static>],
) -> Rect {
    if area.is_empty() {
        return Rect::new(area.x, area.bottom(), 0, 0);
    }
    let body_rows = decision_wrapped_rows(body, area.width);
    let control_rows = decision_wrapped_rows(controls, area.width);
    let desired = 1u16.saturating_add(body_rows).saturating_add(control_rows);
    let controls_floor = 1u16.saturating_add(control_rows).min(area.height);
    let head_rows = body_rows.saturating_sub(save_rows);
    let preview_rows = if area.height >= 16 {
        head_rows.min(4).saturating_add(save_rows)
    } else {
        save_rows
    };
    let preview_floor = controls_floor.saturating_add(preview_rows).min(area.height);
    let preferred_cap = area.height.div_ceil(2);
    let short_cap = area.height.saturating_mul(4).div_ceil(5);
    let save_floor = controls_floor.saturating_add(save_rows).min(area.height);
    let max_height = preferred_cap
        .max(preview_floor.min(short_cap.saturating_add(save_rows)))
        .max(save_floor)
        .min(area.height);
    let height = desired.clamp(controls_floor, max_height);
    Rect::new(
        area.x,
        area.y.saturating_add(area.height.saturating_sub(height)),
        area.width,
        height,
    )
}

impl Paint for DecisionBand {
    fn paint(&self, area: Rect, buf: &mut Buffer, _theme: &Theme) {
        self.render(area, buf);
    }
    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        self.plan(Rect::new(0, 0, width, u16::MAX)).region.height
    }
}

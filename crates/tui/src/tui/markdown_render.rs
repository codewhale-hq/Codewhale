//! Markdown rendering for TUI transcript lines.
//!
//! ## Width-independent parse vs width-dependent render (CX#6)
//!
//! The previous renderer was a single function `render_markdown(content, width)`
//! that scanned the source, classified each line (heading / list / code-fence /
//! paragraph / link), and word-wrapped to `Line<'static>` in one pass. That meant
//! every terminal resize forced a full re-parse of the source for every visible
//! cell — wasted work on the streaming cell whose content is changing anyway.
//!
//! The codex tui solves this by splitting parse from render. We mirror that:
//!
//! * [`parse`] turns the markdown source into a [`ParsedMarkdown`] AST: a vector
//!   of width-independent [`Block`]s. The block kind already records all the
//!   classification decisions (heading level, list bullet, code block membership)
//!   that don't depend on width.
//! * [`render_parsed`] takes a `ParsedMarkdown` plus a width and a base style and
//!   produces `Vec<Line<'static>>`. It only does word-wrap and span styling.
//!
//! [`render_markdown`] is kept as a thin convenience that does both — useful for
//! callers (Thinking body, message body) that don't want to manage the cache.
//!
//! The transcript cache layer (see `tui/transcript.rs`) caches the parsed AST per
//! cell and re-runs only the render step on width changes. That makes resize a
//! re-flow operation rather than a re-parse + re-flow operation.

#[cfg(test)]
use std::cell::Cell;
use std::cell::RefCell;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, HighlightState, Theme, ThemeSet};
use syntect::parsing::{ParseState as SyntectParseState, SyntaxSet};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::osc8;
use crate::tui::ui_text::CopyLineSeparator;
use codewhale_palette as palette;

// Thread-local counter incremented every time `parse` runs. Used by tests to
// prove that width-only changes hit the cached-AST path and skip parsing.
// Thread-local (not global atomic) so concurrent tests calling `parse()` can't
// pollute each other's counters.
#[cfg(test)]
thread_local! {
    static PARSE_INVOCATIONS: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
#[must_use]
pub fn parse_invocation_count() -> u64 {
    PARSE_INVOCATIONS.with(|c| c.get())
}

#[cfg(test)]
pub fn reset_parse_invocation_count() {
    PARSE_INVOCATIONS.with(|c| c.set(0));
}

/// One classified line of markdown source, width-independent.
///
/// All decisions that depend only on the source text (heading level, bullet
/// kind, whether we're inside a fenced code block, paragraph text) are made at
/// parse time. Width-dependent layout (word-wrap, prefix indent) is deferred to
/// the render step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// `# heading text`. Includes the heading level (1..6).
    Heading { level: usize, text: String },
    /// A horizontal rule emitted under a level-1 heading.
    HeadingRule,
    /// A standalone `---` / `***` / `___` horizontal rule.
    HorizontalRule,
    /// A bullet (`-`/`*`) or ordered (`1.`) list item with its prefix and body.
    ListItem { bullet: String, text: String },
    /// A `>` quote line with its nesting depth (1 = single `>`). The text has
    /// the quote markers stripped; depth is capped at [`MAX_QUOTE_DEPTH`].
    Quote { depth: usize, text: String },
    /// A line inside a fenced code block. Fences themselves are dropped, but
    /// their language token and block identity stay available to syntect.
    Code {
        line: String,
        language: Option<String>,
        block_id: usize,
    },
    /// A table row: cells split on `|`.
    TableRow(Vec<String>),
    /// A table separator row (`|---|---|`). Kept so the renderer can draw
    /// horizontal rules at the correct positions.
    TableSeparator,
    /// A non-empty paragraph line that may contain inline links.
    Paragraph { text: String },
    /// An empty source line, preserved so paragraph spacing survives.
    Blank,
}

/// Width-independent parsed-markdown AST for one cell's source.
///
/// Wrapped in `Arc` at the cache layer so the cache can hand the same AST to
/// many render calls without copying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMarkdown {
    blocks: Vec<Block>,
}

/// Width-dependent rendered line plus the source block kind that produced it.
///
/// Most callers only need styled terminal lines, but transcript rendering also
/// needs to avoid adding its conversational continuation rail in front of code
/// blocks. Keeping this metadata here avoids guessing from styled spans.
#[derive(Debug, Clone)]
pub struct RenderedMarkdownLine {
    pub line: Line<'static>,
    /// Hyperlinks aligned to display columns in `line`. Targets stay
    /// out-of-band; `Span::content` always contains visible text only.
    pub links: Vec<osc8::LineLink>,
    pub is_code: bool,
    pub copy_prefix_width: usize,
    pub copy_separator_after: CopyLineSeparator,
}

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();
static COLOR_DEPTH: OnceLock<palette::ColorDepth> = OnceLock::new();
static PALETTE_MODE: OnceLock<palette::PaletteMode> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme_set() -> &'static ThemeSet {
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

/// Load the syntect syntax and theme sets ahead of the first fenced code
/// block, so that render does not pay the one-time deserialization cost.
/// Idempotent; intended to run once on a background thread at TUI boot.
pub(crate) fn prewarm_syntax_highlighting() {
    let _ = syntax_set();
    let _ = theme_set();
}

fn syntax_color_depth() -> palette::ColorDepth {
    *COLOR_DEPTH.get_or_init(palette::ColorDepth::detect)
}

pub(crate) fn detected_palette_mode() -> palette::PaletteMode {
    *PALETTE_MODE.get_or_init(palette::PaletteMode::detect)
}

/// Parse markdown source into a width-independent block AST.
///
/// This is a small line-oriented parser tuned for the patterns we render:
/// fenced code blocks, ATX headings, dash/star/numbered list items, and plain
/// paragraphs with optional links. It does not attempt to handle every CommonMark
/// edge case — that's intentional. The renderer will treat anything we don't
/// classify as `Block::Paragraph`.
#[must_use]
pub fn parse(content: &str) -> ParsedMarkdown {
    #[cfg(test)]
    PARSE_INVOCATIONS.with(|c| c.set(c.get() + 1));

    STREAM_PARSE_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        // Reuse the committed prefix when this call continues the same source
        // (the streaming case). Anything else — a different cell, a shrunk
        // buffer, an edit to earlier bytes — fails the check and starts clean.
        let state = memo.get_or_insert_with(ParseState::default);
        if !state.can_resume_from(content) {
            *state = ParseState::default();
        }
        state.commit_complete_lines(content);
        let parsed = state.snapshot(content);
        // Don't hold a whole large message alive between unrelated renders.
        if state.consumed > MAX_MEMOIZED_PREFIX_BYTES {
            *state = ParseState::default();
        }
        parsed
    })
}

/// Upper bound on the source we keep memoized between `parse` calls. Streaming
/// messages are the reason this exists; past this size the memory cost of
/// holding the prefix outweighs the re-parse it saves.
const MAX_MEMOIZED_PREFIX_BYTES: usize = 1024 * 1024;

thread_local! {
    /// Single-entry resume memo for the streaming re-parse (#3897).
    ///
    /// Deliberately one entry and thread-local: the hot path is one cell
    /// growing chunk by chunk on the render thread. A miss costs exactly what
    /// the old code always paid, so this can only make things faster or
    /// identical — never wrong, because [`ParseState::can_resume_from`]
    /// verifies the prefix byte-for-byte before reusing anything.
    static STREAM_PARSE_MEMO: RefCell<Option<ParseState>> = const { RefCell::new(None) };
}

/// Resumable parser state.
///
/// The parser is strictly line-oriented: each source line maps to blocks using
/// only a three-field carry (`open_fence_len`, `code_language`,
/// `code_block_id`). That is what makes resuming *exact* rather than
/// approximate — appending text can never change how an earlier complete line
/// parsed, so committed blocks never need revisiting.
///
/// Streaming is the case that matters (#3897): the renderer re-parses the whole
/// growing message on every chunk, which is quadratic over message length.
#[derive(Debug, Clone, Default)]
pub struct ParseState {
    blocks: Vec<Block>,
    /// The exact source bytes already folded into `blocks`. Kept verbatim so
    /// resumption is *verified* against the new content rather than assumed —
    /// a caller that hands over unrelated text gets a full re-parse, not
    /// silently wrong output.
    prefix: String,
    /// FNV-1a digest of every byte committed since the last reset. The live
    /// incremental cache drops `prefix` to bound memory, so the
    /// verified-append resume path compares this digest against the incoming
    /// content instead of retaining a second copy of the source.
    committed_digest: u64,
    /// Length of `prefix`. Always ends just past a newline, so only whole
    /// lines are ever committed.
    consumed: usize,
    /// Length of the opening code fence in backticks while inside a fenced
    /// code block (`None` outside). A closing fence must be at least this
    /// long per CommonMark; shorter backtick lines are code content.
    open_fence_len: Option<usize>,
    code_language: Option<String>,
    code_block_id: usize,
}

impl ParseState {
    /// Fold every *complete* line after `consumed` into `blocks`.
    ///
    /// The trailing partial line is deliberately left uncommitted: streaming
    /// can still extend it, and committing it early would be the one way this
    /// could diverge from a full re-parse.
    fn commit_complete_lines(&mut self, content: &str) {
        let Some(rest) = content.get(self.consumed..) else {
            return;
        };
        let Some(last_newline) = rest.rfind('\n') else {
            return;
        };
        let complete = &rest[..=last_newline];
        for raw_line in complete.lines() {
            push_parsed_line(
                raw_line,
                &mut self.blocks,
                &mut self.open_fence_len,
                &mut self.code_language,
                &mut self.code_block_id,
            );
        }
        let mut digest = if self.consumed == 0 {
            committed_prefix_digest(&[])
        } else {
            self.committed_digest
        };
        for &byte in complete.as_bytes() {
            digest ^= u64::from(byte);
            digest = digest.wrapping_mul(FNV_1A_PRIME);
        }
        self.committed_digest = digest;
        self.prefix.push_str(complete);
        self.consumed += complete.len();
    }

    /// The full AST: committed blocks plus the trailing partial line, parsed
    /// against a throwaway copy of the carry so `self` stays resumable.
    fn snapshot(&self, content: &str) -> ParsedMarkdown {
        let tail = content.get(self.consumed..).unwrap_or_default();
        if tail.is_empty() {
            return ParsedMarkdown {
                blocks: self.blocks.clone(),
            };
        }
        let mut blocks = self.blocks.clone();
        let mut open_fence_len = self.open_fence_len;
        let mut code_language = self.code_language.clone();
        let mut code_block_id = self.code_block_id;
        for raw_line in tail.lines() {
            push_parsed_line(
                raw_line,
                &mut blocks,
                &mut open_fence_len,
                &mut code_language,
                &mut code_block_id,
            );
        }
        ParsedMarkdown { blocks }
    }

    /// True when `content` still starts with everything already committed.
    ///
    /// Streaming only ever appends, so this is the common case. An edit that
    /// rewrites earlier bytes (a re-render of a different cell, a retry) fails
    /// here and the caller falls back to a full parse — correctness never
    /// depends on the caller guessing right.
    fn can_resume_from(&self, content: &str) -> bool {
        content.len() >= self.consumed
            && content.is_char_boundary(self.consumed)
            && self.committed_prefix_matches(content)
    }

    /// Resume after the caller has proved that the raw source mutation was an
    /// append. The live transcript obtains that proof at the `push_str` seam.
    ///
    /// The receipt covers the *raw* stream, but this cache consumes the
    /// latex-rendered projection of it, and a math block that closes late
    /// rewrites already-committed bytes: an open `\[` is committed as literal
    /// text and only becomes its rendered form once the closing `\]` arrives
    /// (#6196). A length check cannot see that rewrite, so the committed
    /// prefix is verified by digest. FNV-1a over hot cache lines costs orders
    /// of magnitude less per beat than the render it guards, while retaining
    /// the prefix itself would pin the whole message in memory.
    fn can_resume_verified_append(&self, content: &str) -> bool {
        content.len() >= self.consumed
            && content.is_char_boundary(self.consumed)
            && self.committed_digest
                == committed_prefix_digest(&content.as_bytes()[..self.consumed])
    }

    fn committed_prefix_matches(&self, content: &str) -> bool {
        self.prefix == content[..self.consumed]
    }
}

/// FNV-1a constants. Chosen for speed and zero dependencies: the
/// verified-append resume check hashes the whole committed prefix on every
/// streaming beat, so the guard must stay far below the render cost it
/// protects.
const FNV_1A_PRIME: u64 = 0x0000_0100_0000_01b3;

fn committed_prefix_digest(bytes: &[u8]) -> u64 {
    const FNV_1A_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    let mut digest = FNV_1A_OFFSET_BASIS;
    for &byte in bytes {
        digest ^= u64::from(byte);
        digest = digest.wrapping_mul(FNV_1A_PRIME);
    }
    digest
}

/// Deterministic work receipts for the live incremental renderer.
///
/// These count source lines classified and stable/tail blocks rendered. They
/// deliberately do not use wall-clock time, allocator counters, or sampling,
/// so regression tests are stable on every machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MarkdownRenderWork {
    pub classified_lines: u64,
    pub stable_blocks_rendered: u64,
    pub tail_blocks_rendered: u64,
    pub invalidations: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IncrementalRenderKey {
    width: u16,
    base_style: Style,
    palette_mode: palette::PaletteMode,
}

#[derive(Debug, Clone)]
struct IncrementalCodeHighlighter {
    block_id: usize,
    language: Option<String>,
    state: Option<(HighlightState, SyntectParseState)>,
}

/// Persistent state for the one changing Markdown cell in the transcript.
///
/// Stable rendered lines live in the owning `CachedCell`; this object retains
/// only parser/highlighter carry plus the line index at which its replaceable
/// tail begins. That lets each append truncate and replace the old tail without
/// cloning or re-rendering the committed prefix.
#[derive(Debug, Default)]
pub(crate) struct IncrementalMarkdownRenderCache {
    parser: ParseState,
    key: Option<IncrementalRenderKey>,
    source_len: usize,
    stable_rendered_line_count: usize,
    code_highlighter: Option<IncrementalCodeHighlighter>,
    work: MarkdownRenderWork,
}

pub(crate) struct IncrementalMarkdownRenderDelta {
    pub replace_from: usize,
    pub lines: Vec<RenderedMarkdownLine>,
}

impl IncrementalMarkdownRenderCache {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn work(&self) -> MarkdownRenderWork {
        self.work
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn retained_source_bytes(&self) -> usize {
        self.parser.prefix.len()
    }

    /// Update from a source mutation whose append-only provenance was recorded
    /// by the live event loop. `verified_append` must be false for edits,
    /// replacement text, cell reuse, or any unknown mutation.
    pub(crate) fn update(
        &mut self,
        content: &str,
        width: u16,
        base_style: Style,
        palette_mode: palette::PaletteMode,
        verified_append: bool,
    ) -> IncrementalMarkdownRenderDelta {
        let key = IncrementalRenderKey {
            width: width.max(1),
            base_style,
            palette_mode,
        };

        let can_resume = self.key == Some(key)
            && verified_append
            && content.len() >= self.source_len
            && self.parser.can_resume_verified_append(content);
        let replace_from = if can_resume {
            self.stable_rendered_line_count
        } else {
            self.reset_for_invalidation();
            self.work.invalidations = self.work.invalidations.saturating_add(1);
            0
        };
        self.key = Some(key);

        let before_consumed = self.parser.consumed;
        self.parser.commit_complete_lines(content);
        self.work.classified_lines = self.work.classified_lines.saturating_add(
            content[before_consumed..self.parser.consumed]
                .lines()
                .count() as u64,
        );
        self.source_len = content.len();
        // Append provenance is carried by the event-loop receipt, so the live
        // cache does not need a second copy of already committed source. The
        // absolute byte offset and parser carry are enough to resume.
        self.parser.prefix.clear();

        let stable_end = stable_block_prefix_len(&self.parser.blocks);
        let mut lines = self.render_stable_prefix(stable_end, key);
        self.stable_rendered_line_count =
            self.stable_rendered_line_count.saturating_add(lines.len());

        let mut tail_blocks = self.parser.blocks.clone();
        tail_blocks.extend(self.parser.snapshot_tail(content));
        if !tail_blocks.is_empty() {
            self.work.tail_blocks_rendered = self
                .work
                .tail_blocks_rendered
                .saturating_add(tail_blocks.len() as u64);
            let mut tail_highlighter = self.code_highlighter.clone();
            lines.extend(render_incremental_blocks(
                &tail_blocks,
                key,
                &mut tail_highlighter,
            ));
        }

        if lines.is_empty() && self.stable_rendered_line_count == 0 {
            lines.push(empty_rendered_markdown_line());
        }

        IncrementalMarkdownRenderDelta {
            replace_from,
            lines,
        }
    }

    fn render_stable_prefix(
        &mut self,
        end: usize,
        key: IncrementalRenderKey,
    ) -> Vec<RenderedMarkdownLine> {
        if end == 0 {
            return Vec::new();
        }
        self.work.stable_blocks_rendered =
            self.work.stable_blocks_rendered.saturating_add(end as u64);
        let lines =
            render_incremental_blocks(&self.parser.blocks[..end], key, &mut self.code_highlighter);
        self.parser.blocks.drain(..end);
        lines
    }

    fn reset_for_invalidation(&mut self) {
        self.parser = ParseState::default();
        self.key = None;
        self.source_len = 0;
        self.stable_rendered_line_count = 0;
        self.code_highlighter = None;
    }
}

impl ParseState {
    fn snapshot_tail(&self, content: &str) -> Vec<Block> {
        let tail = content.get(self.consumed..).unwrap_or_default();
        let mut blocks = Vec::new();
        let mut open_fence_len = self.open_fence_len;
        let mut code_language = self.code_language.clone();
        let mut code_block_id = self.code_block_id;
        for raw_line in tail.lines() {
            push_parsed_line(
                raw_line,
                &mut blocks,
                &mut open_fence_len,
                &mut code_language,
                &mut code_block_id,
            );
        }
        blocks
    }
}

fn stable_block_prefix_len(blocks: &[Block]) -> usize {
    let Some(last_non_table) = blocks
        .iter()
        .rposition(|block| !matches!(block, Block::TableRow(_) | Block::TableSeparator))
    else {
        return 0;
    };
    if last_non_table + 1 == blocks.len() {
        blocks.len()
    } else {
        last_non_table + 1
    }
}

/// Classify one source line into blocks, advancing the fenced-code carry.
///
/// Extracted from the original loop body unchanged so the batch and streaming
/// paths cannot drift: both call exactly this.
fn push_parsed_line(
    raw_line: &str,
    blocks: &mut Vec<Block>,
    open_fence_len: &mut Option<usize>,
    code_language: &mut Option<String>,
    code_block_id: &mut usize,
) {
    let trimmed = raw_line.trim_start();
    let fence_len = trimmed.chars().take_while(|c| *c == '`').count();
    if fence_len >= 3 {
        match *open_fence_len {
            // Inside a code block: a fence at least as long as the opener
            // closes it; a shorter backtick line is code content per
            // CommonMark and must not flip the state or escape the block.
            Some(open) if fence_len >= open && trimmed[fence_len..].trim().is_empty() => {
                *open_fence_len = None;
                *code_language = None;
            }
            Some(_) => {
                blocks.push(Block::Code {
                    line: raw_line.to_string(),
                    language: code_language.clone(),
                    block_id: *code_block_id,
                });
            }
            None => {
                *open_fence_len = Some(fence_len);
                *code_block_id = code_block_id.saturating_add(1);
                *code_language = normalized_fence_language(&trimmed[fence_len..]);
            }
        }
        return;
    }

    if open_fence_len.is_some() {
        blocks.push(Block::Code {
            line: raw_line.to_string(),
            language: code_language.clone(),
            block_id: *code_block_id,
        });
        return;
    }

    if let Some((depth, text)) = parse_blockquote(trimmed) {
        blocks.push(Block::Quote {
            depth,
            text: text.to_string(),
        });
        return;
    }

    if let Some((level, text)) = parse_heading(trimmed) {
        blocks.push(Block::Heading {
            level,
            text: text.to_string(),
        });
        if level == 1 {
            blocks.push(Block::HeadingRule);
        }
        return;
    }

    if let Some((bullet, text)) = parse_list_item(trimmed) {
        blocks.push(Block::ListItem {
            bullet,
            text: text.to_string(),
        });
        return;
    }

    if is_horizontal_rule(trimmed) {
        blocks.push(Block::HorizontalRule);
        return;
    }

    match parse_table_row(trimmed) {
        Some(cells) => {
            blocks.push(Block::TableRow(cells));
            return;
        }
        None if trimmed.starts_with('|') => {
            blocks.push(Block::TableSeparator);
            return;
        }
        None => {}
    }

    if trimmed.is_empty() {
        // Whitespace-only lines are blank paragraphs.
        blocks.push(Block::Blank);
        return;
    }

    blocks.push(Block::Paragraph {
        text: raw_line.to_string(),
    });
}

/// Render a parsed-markdown AST at the given terminal width.
///
/// This is the width-dependent half: word-wrapping, link styling, code-block
/// formatting. The AST is owned by the caller (typically the transcript cache),
/// so width-only changes can call `render_parsed` again with the same AST and
/// skip the parse step entirely.
#[must_use]
pub fn render_parsed(parsed: &ParsedMarkdown, width: u16, base_style: Style) -> Vec<Line<'static>> {
    render_parsed_tagged_with_palette(parsed, width, base_style, detected_palette_mode())
        .into_iter()
        .map(|line| line.line)
        .collect()
}

/// Render a parsed-markdown AST and preserve per-line source metadata.
#[cfg(test)]
#[must_use]
pub fn render_parsed_tagged(
    parsed: &ParsedMarkdown,
    width: u16,
    base_style: Style,
) -> Vec<RenderedMarkdownLine> {
    render_parsed_tagged_with_palette(parsed, width, base_style, detected_palette_mode())
}

/// Render parsed markdown using the caller's resolved UI palette mode.
///
/// The live transcript uses this entry point so an explicit theme selection
/// wins over terminal/OS auto-detection and participates in cache invalidation.
#[must_use]
pub(crate) fn render_parsed_tagged_with_palette(
    parsed: &ParsedMarkdown,
    width: u16,
    base_style: Style,
    palette_mode: palette::PaletteMode,
) -> Vec<RenderedMarkdownLine> {
    let width = width.max(1) as usize;
    let mut out: Vec<RenderedMarkdownLine> = Vec::with_capacity(parsed.blocks.len());

    let mut i = 0;
    while i < parsed.blocks.len() {
        if matches!(
            &parsed.blocks[i],
            Block::TableRow(_) | Block::TableSeparator
        ) {
            let start = i;
            while i < parsed.blocks.len()
                && matches!(
                    &parsed.blocks[i],
                    Block::TableRow(_) | Block::TableSeparator
                )
            {
                i += 1;
            }
            out.extend(
                render_table_group(&parsed.blocks[start..i], width, base_style)
                    .into_iter()
                    .map(|line| RenderedMarkdownLine {
                        line,
                        links: Vec::new(),
                        is_code: false,
                        copy_prefix_width: 0,
                        copy_separator_after: CopyLineSeparator::Newline,
                    }),
            );
            continue;
        }

        if let Block::Code {
            language, block_id, ..
        } = &parsed.blocks[i]
        {
            let start = i;
            while i < parsed.blocks.len()
                && matches!(
                    &parsed.blocks[i],
                    Block::Code {
                        block_id: candidate,
                        ..
                    } if candidate == block_id
                )
            {
                i += 1;
            }
            let source_lines = parsed.blocks[start..i]
                .iter()
                .filter_map(|block| match block {
                    Block::Code { line, .. } => Some(line.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let highlighted =
                highlight_code_block(language.as_deref(), &source_lines, base_style, palette_mode);
            for spans in highlighted {
                out.extend(render_wrapped_code_spans_tagged(spans, width));
            }
            continue;
        }

        match &parsed.blocks[i] {
            Block::Heading { text, .. } => {
                let style = Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::BOLD);
                out.extend(render_line_with_links_tagged(
                    text,
                    width,
                    style,
                    link_style(),
                ));
            }
            Block::HeadingRule => {
                out.push(RenderedMarkdownLine {
                    line: Line::from(Span::styled(
                        "─".repeat(width.min(40)),
                        Style::default().fg(palette::TEXT_DIM),
                    )),
                    links: Vec::new(),
                    is_code: false,
                    copy_prefix_width: 0,
                    copy_separator_after: CopyLineSeparator::Newline,
                });
            }
            Block::HorizontalRule => {
                out.push(RenderedMarkdownLine {
                    line: Line::from(Span::styled(
                        "─".repeat(width.min(60)),
                        Style::default().fg(palette::TEXT_DIM),
                    )),
                    links: Vec::new(),
                    is_code: false,
                    copy_prefix_width: 0,
                    copy_separator_after: CopyLineSeparator::Newline,
                });
            }
            Block::ListItem { bullet, text } => {
                let bullet_style = Style::default().fg(palette::WHALE_ACTION);
                out.extend(render_list_line_tagged(
                    bullet,
                    text,
                    width,
                    bullet_style,
                    base_style,
                ));
            }
            Block::Code { .. } => unreachable!(),
            Block::Quote { depth, text } => {
                let rail_style = Style::default().fg(palette::WHALE_ACTION);
                let text_style = Style::default().fg(palette::TEXT_DIM);
                out.extend(render_quote_line_tagged(
                    text, *depth, width, rail_style, text_style,
                ));
            }
            Block::Paragraph { text } => {
                let link_style = Style::default()
                    .fg(palette::WHALE_ACTION)
                    .add_modifier(Modifier::UNDERLINED);
                out.extend(render_line_with_links_tagged(
                    text, width, base_style, link_style,
                ));
            }
            Block::Blank => {
                out.push(RenderedMarkdownLine {
                    line: Line::from(""),
                    links: Vec::new(),
                    is_code: false,
                    copy_prefix_width: 0,
                    copy_separator_after: CopyLineSeparator::Newline,
                });
            }
            Block::TableRow(_) | Block::TableSeparator => unreachable!(),
        }
        i += 1;
    }

    if out.is_empty() {
        out.push(RenderedMarkdownLine {
            line: Line::from(""),
            links: Vec::new(),
            is_code: false,
            copy_prefix_width: 0,
            copy_separator_after: CopyLineSeparator::Newline,
        });
    }

    out
}

fn empty_rendered_markdown_line() -> RenderedMarkdownLine {
    RenderedMarkdownLine {
        line: Line::from(""),
        links: Vec::new(),
        is_code: false,
        copy_prefix_width: 0,
        copy_separator_after: CopyLineSeparator::Newline,
    }
}

/// Render a block suffix while carrying syntax state across calls.
///
/// Non-code blocks use the canonical batch renderer unchanged. Code lines are
/// the only group whose styling depends on preceding blocks, so their syntect
/// parse/highlight state is retained explicitly and cloned for the replaceable
/// tail. This keeps an open fence incremental without sacrificing exact final
/// highlighting.
fn render_incremental_blocks(
    blocks: &[Block],
    key: IncrementalRenderKey,
    code_highlighter: &mut Option<IncrementalCodeHighlighter>,
) -> Vec<RenderedMarkdownLine> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < blocks.len() {
        if let Block::Code {
            line,
            language,
            block_id,
        } = &blocks[index]
        {
            let spans = highlight_incremental_code_line(
                *block_id,
                language.as_deref(),
                line,
                key.base_style,
                key.palette_mode,
                code_highlighter,
            );
            out.extend(render_wrapped_code_spans_tagged(
                spans,
                usize::from(key.width),
            ));
            index += 1;
            continue;
        }

        let start = index;
        while index < blocks.len() && !matches!(blocks[index], Block::Code { .. }) {
            index += 1;
        }
        out.extend(render_parsed_tagged_with_palette(
            &ParsedMarkdown {
                blocks: blocks[start..index].to_vec(),
            },
            key.width,
            key.base_style,
            key.palette_mode,
        ));
    }
    out
}

fn highlight_incremental_code_line(
    block_id: usize,
    language: Option<&str>,
    line: &str,
    base_style: Style,
    palette_mode: palette::PaletteMode,
    cache: &mut Option<IncrementalCodeHighlighter>,
) -> Vec<Span<'static>> {
    let language_owned = language.map(str::to_owned);
    let needs_reset = cache
        .as_ref()
        .is_none_or(|current| current.block_id != block_id || current.language != language_owned);
    if needs_reset {
        let state = language
            .and_then(find_code_syntax)
            .map(|syntax| HighlightLines::new(syntax, selected_syntax_theme(palette_mode)).state());
        *cache = Some(IncrementalCodeHighlighter {
            block_id,
            language: language_owned,
            state,
        });
    }

    let plain_style = base_style.fg(palette::TEXT_TOOL_OUTPUT);
    let Some(current) = cache.as_mut() else {
        return vec![Span::styled(line.to_string(), plain_style)];
    };
    let Some((highlight_state, parse_state)) = current.state.take() else {
        return vec![Span::styled(line.to_string(), plain_style)];
    };
    let mut highlighter = HighlightLines::from_state(
        selected_syntax_theme(palette_mode),
        highlight_state,
        parse_state,
    );
    let highlighted = match highlighter.highlight_line(line, syntax_set()) {
        Ok(ranges) if !ranges.is_empty() => ranges
            .into_iter()
            .map(|(style, text)| {
                Span::styled(
                    text.to_string(),
                    syntax_style_to_ratatui(style, base_style, palette_mode),
                )
            })
            .collect(),
        _ => vec![Span::styled(line.to_string(), plain_style)],
    };
    current.state = Some(highlighter.state());
    highlighted
}

/// Convenience wrapper: parse + render in one call.
///
/// Equivalent to `render_parsed(&parse(content), width, base_style)`. Callers
/// that don't manage their own cache (the Thinking body, the immediate message
/// body) use this.
#[must_use]
pub fn render_markdown(content: &str, width: u16, base_style: Style) -> Vec<Line<'static>> {
    let parsed = parse(content);
    render_parsed(&parsed, width, base_style)
}

/// Convenience wrapper: parse + render while keeping per-line source metadata.
#[cfg(test)]
#[must_use]
pub fn render_markdown_tagged(
    content: &str,
    width: u16,
    base_style: Style,
) -> Vec<RenderedMarkdownLine> {
    let parsed = parse(content);
    render_parsed_tagged(&parsed, width, base_style)
}

/// Parse and render markdown using an already-resolved UI palette mode.
#[must_use]
pub(crate) fn render_markdown_tagged_with_palette(
    content: &str,
    width: u16,
    base_style: Style,
    palette_mode: palette::PaletteMode,
) -> Vec<RenderedMarkdownLine> {
    let parsed = parse(content);
    render_parsed_tagged_with_palette(&parsed, width, base_style, palette_mode)
}

/// Render plain text: split on newlines, word-wrap each line independently,
/// preserve leading whitespace and blank lines. No markdown interpretation.
#[must_use]
pub fn render_plain_text(content: &str, width: u16, base_style: Style) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let mut lines = Vec::new();
    for raw_line in content.split('\n') {
        if raw_line.is_empty() {
            lines.push(Line::from(""));
        } else {
            lines.extend(wrap_plain_line(raw_line, width, base_style));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

/// Word-wrap a single line at `width`, preserving leading whitespace.
/// Handles over-long words by char-breaking (same strategy as the markdown
/// line renderer).
fn wrap_plain_line(line: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    if width == 0 || line.is_empty() {
        return vec![Line::from("")];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut last_break_pos = None;

    for grapheme in line.graphemes(true) {
        loop {
            let grapheme_width = markdown_grapheme_width(grapheme, current_width);
            if current_width + grapheme_width <= width || current.is_empty() {
                break;
            }

            if let Some(pos) = last_break_pos {
                if pos == current.len() {
                    chunks.push(std::mem::take(&mut current));
                    current_width = 0;
                    last_break_pos = None;
                    break;
                }

                if current[..pos].chars().any(|c| !c.is_whitespace()) {
                    let tail = current.split_off(pos);
                    chunks.push(std::mem::take(&mut current));
                    current = tail;
                    current_width = plain_display_width(&current);
                    last_break_pos = last_plain_break_pos(&current);
                    continue;
                }
            }

            chunks.push(std::mem::take(&mut current));
            current_width = 0;
            last_break_pos = None;
            break;
        }

        let grapheme_width = markdown_grapheme_width(grapheme, current_width);
        current.push_str(grapheme);
        current_width += grapheme_width;
        if grapheme.chars().all(char::is_whitespace) {
            last_break_pos = Some(current.len());
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        return vec![Line::from("")];
    }

    chunks
        .into_iter()
        .map(|chunk| Line::from(vec![Span::styled(chunk, style)]))
        .collect()
}

fn plain_display_width(text: &str) -> usize {
    let mut width = 0usize;
    for grapheme in text.graphemes(true) {
        width += markdown_grapheme_width(grapheme, width);
    }
    width
}

fn last_plain_break_pos(text: &str) -> Option<usize> {
    text.char_indices()
        .rev()
        .find_map(|(idx, ch)| ch.is_whitespace().then_some(idx + ch.len_utf8()))
}

fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    let rest = &trimmed[hashes..];
    if !(1..=6).contains(&hashes) || !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    let text = rest.trim();
    if text.is_empty() {
        None
    } else {
        Some((hashes, text))
    }
}

fn parse_list_item(line: &str) -> Option<(String, &str)> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
        return Some(("-".to_string(), trimmed[2..].trim()));
    }
    let bytes = trimmed.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == 0 || idx >= bytes.len() || bytes[idx] != b'.' {
        return None;
    }
    let rest = &trimmed[idx + 1..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some((format!("{}.", &trimmed[..idx]), rest.trim_start()))
}

/// Upper bound on the nesting depth rendered for a `>` quote. Deeper quotes
/// are clamped and the extra markers dropped from the rendered text; capping
/// stops a pathological input like `>>>>>>>>>>>> text` from consuming the
/// whole line width in rails.
const MAX_QUOTE_DEPTH: usize = 4;

/// Parse a `>` blockquote line, returning `(depth, text)`.
///
/// CommonMark nests with `>>` or `> >`; we count every leading `>` regardless
/// of interleaved spaces, then trim the remaining content. A lone `>` yields
/// an empty quote line. Deliberately lenient about missing space after `>` so
/// model output like `>note` still renders as a quote.
fn parse_blockquote(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('>') {
        return None;
    }
    let mut rest = trimmed;
    let mut depth = 0usize;
    while rest.starts_with('>') {
        depth = depth.saturating_add(1);
        rest = rest[1..].trim_start_matches([' ', '\t']);
    }
    Some((depth.clamp(1, MAX_QUOTE_DEPTH), rest.trim()))
}

fn normalized_fence_language(info: &str) -> Option<String> {
    let token = info
        .trim()
        .split(|ch: char| ch.is_whitespace() || ch == ',')
        .next()
        .unwrap_or("")
        .trim_matches(['{', '}', '.'])
        .to_ascii_lowercase();
    if token.is_empty() || matches!(token.as_str(), "text" | "txt" | "plain" | "plaintext") {
        return None;
    }
    let normalized = match token.as_str() {
        "rs" => "rust",
        "js" | "jsx" | "node" => "javascript",
        "ts" | "tsx" => "typescript",
        "py" => "python",
        "rb" => "ruby",
        "sh" | "shell" | "zsh" => "bash",
        "yml" => "yaml",
        "md" => "markdown",
        other => other,
    };
    Some(normalized.to_string())
}

fn selected_syntax_theme(mode: palette::PaletteMode) -> &'static Theme {
    let themes = theme_set();
    let preferred = match mode {
        palette::PaletteMode::Dark | palette::PaletteMode::Grayscale => "base16-ocean.dark",
        palette::PaletteMode::Light => "InspiredGitHub",
        palette::PaletteMode::SolarizedLight => "Solarized (light)",
    };
    themes
        .themes
        .get(preferred)
        .or_else(|| themes.themes.values().next())
        .expect("syntect ships at least one default theme")
}

fn syntax_style_to_ratatui(
    style: syntect::highlighting::Style,
    base_style: Style,
    palette_mode: palette::PaletteMode,
) -> Style {
    let fg = syntax_rgb_to_terminal_color(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
        palette_mode,
        syntax_color_depth(),
    );
    let mut modifiers = Modifier::empty();
    if style.font_style.contains(FontStyle::BOLD) {
        modifiers |= Modifier::BOLD;
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        modifiers |= Modifier::ITALIC;
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        modifiers |= Modifier::UNDERLINED;
    }
    base_style.fg(fg).add_modifier(modifiers)
}

fn syntax_rgb_to_terminal_color(
    r: u8,
    g: u8,
    b: u8,
    mode: palette::PaletteMode,
    depth: palette::ColorDepth,
) -> Color {
    let (r, g, b) = if mode == palette::PaletteMode::Grayscale {
        let luma =
            ((u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 + 500) / 1000) as u8;
        let readable = luma.clamp(96, 232);
        (readable, readable, readable)
    } else {
        (r, g, b)
    };
    let mut color = Color::Rgb(r, g, b);
    if matches!(
        color,
        reserved if reserved == palette::WHALE_HUMAN
            || reserved == palette::WHALE_LIVE
            || reserved == palette::WHALE_ACTION
            || reserved == palette::WHALE_ERROR
    ) {
        // Syntax colors are content, not brand/attention/work/danger state.
        // Shift exact collisions before terminal-depth reduction so the cell
        // cannot acquire a reserved semantic role in the color backend.
        color = Color::Rgb(r, g, b.saturating_add(1));
    }
    let color = palette::adapt_color(color, depth);
    let reserved = [
        palette::WHALE_HUMAN,
        palette::WHALE_LIVE,
        palette::WHALE_ACTION,
        palette::WHALE_ERROR,
    ]
    .map(|semantic| palette::adapt_color(semantic, depth));
    if !reserved.contains(&color) {
        return color;
    }

    // Quantization can make distinct RGB values collide again. Walk a small,
    // deterministic neutral ramp until the terminal-level color no longer
    // impersonates one of the four reserved semantic lanes.
    for delta in [17_u8, 34, 51, 68, 85, 102, 119, 136] {
        let candidate = palette::adapt_color(
            Color::Rgb(
                r.wrapping_add(delta),
                g.wrapping_add(delta / 2),
                b.wrapping_add(delta / 3),
            ),
            depth,
        );
        if !reserved.contains(&candidate) {
            return candidate;
        }
    }
    // All supported depths have more than four colors, so this is only a
    // defensive fallback for a future adapter with a narrower gamut.
    Color::Reset
}

fn highlight_code_block(
    language: Option<&str>,
    lines: &[&str],
    base_style: Style,
    palette_mode: palette::PaletteMode,
) -> Vec<Vec<Span<'static>>> {
    let plain_style = base_style.fg(palette::TEXT_TOOL_OUTPUT);
    let Some(language) = language else {
        return lines
            .iter()
            .map(|line| vec![Span::styled((*line).to_string(), plain_style)])
            .collect();
    };
    let syntaxes = syntax_set();
    let Some(syntax) = find_code_syntax(language) else {
        return lines
            .iter()
            .map(|line| vec![Span::styled((*line).to_string(), plain_style)])
            .collect();
    };

    let mut highlighter = HighlightLines::new(syntax, selected_syntax_theme(palette_mode));
    lines
        .iter()
        .map(|line| match highlighter.highlight_line(line, syntaxes) {
            Ok(ranges) if !ranges.is_empty() => ranges
                .into_iter()
                .map(|(style, text)| {
                    Span::styled(
                        text.to_string(),
                        syntax_style_to_ratatui(style, base_style, palette_mode),
                    )
                })
                .collect(),
            _ => vec![Span::styled((*line).to_string(), plain_style)],
        })
        .collect()
}

fn find_code_syntax(language: &str) -> Option<&'static syntect::parsing::SyntaxReference> {
    let syntaxes = syntax_set();
    syntaxes
        .find_syntax_by_token(language)
        .or_else(|| syntaxes.find_syntax_by_extension(language))
        .or_else(|| {
            syntaxes
                .syntaxes()
                .iter()
                .find(|syntax| syntax.name.eq_ignore_ascii_case(language))
        })
}

fn render_wrapped_code_spans_tagged(
    spans: Vec<Span<'static>>,
    width: usize,
) -> Vec<RenderedMarkdownLine> {
    let prefix = "  ";
    let prefix_width = prefix.width();
    let available = width.saturating_sub(prefix_width).max(1);
    let mut rows: Vec<Vec<(String, Style)>> = vec![Vec::new()];
    let mut current_width = 0usize;

    for span in spans {
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = markdown_grapheme_width(grapheme, current_width);
            if current_width + grapheme_width > available && current_width > 0 {
                rows.push(Vec::new());
                current_width = 0;
            }
            let row = rows.last_mut().expect("code rows are never empty");
            if let Some((text, style)) = row.last_mut()
                && *style == span.style
            {
                text.push_str(grapheme);
            } else {
                row.push((grapheme.to_string(), span.style));
            }
            current_width += markdown_grapheme_width(grapheme, current_width);
        }
    }

    let last_index = rows.len().saturating_sub(1);
    rows.into_iter()
        .enumerate()
        .map(|(idx, row)| {
            let mut rendered = vec![Span::raw(prefix)];
            rendered.extend(
                row.into_iter()
                    .map(|(text, style)| Span::styled(text, style)),
            );
            RenderedMarkdownLine {
                line: Line::from(rendered),
                links: Vec::new(),
                is_code: true,
                copy_prefix_width: prefix_width,
                copy_separator_after: if idx == last_index {
                    CopyLineSeparator::Newline
                } else {
                    CopyLineSeparator::None
                },
            }
        })
        .collect()
}

fn render_list_line_tagged(
    bullet: &str,
    text: &str,
    width: usize,
    bullet_style: Style,
    text_style: Style,
) -> Vec<RenderedMarkdownLine> {
    let bullet_prefix = format!("{bullet} ");
    let bullet_width = bullet_prefix.width();
    let available = width.saturating_sub(bullet_width).max(1);
    let wrapped = render_line_with_links_tagged(text, available, text_style, link_style());

    let mut out = Vec::new();
    for (idx, rendered) in wrapped.into_iter().enumerate() {
        let links = rendered
            .links
            .iter()
            .map(|link| link.shifted(bullet_width))
            .collect();
        if idx == 0 {
            let mut spans = vec![Span::styled(bullet_prefix.clone(), bullet_style)];
            spans.extend(rendered.line.spans);
            out.push(RenderedMarkdownLine {
                line: Line::from(spans),
                links,
                is_code: false,
                copy_prefix_width: 0,
                copy_separator_after: rendered.copy_separator_after,
            });
        } else {
            let mut spans = vec![Span::raw(" ".repeat(bullet_width))];
            spans.extend(rendered.line.spans);
            out.push(RenderedMarkdownLine {
                line: Line::from(spans),
                links,
                is_code: false,
                copy_prefix_width: bullet_width,
                copy_separator_after: rendered.copy_separator_after,
            });
        }
    }
    out
}

/// Render a `>` quote line: a vertical-rule rail per nesting depth plus the
/// quote text with inline formatting (bold, code, links).
///
/// The rail is display chrome, not source markup. On the first row the
/// transcript rail-scan (`compute_rail_prefix_width`) already strips it along
/// with the assistant glyph, so `copy_prefix_width` must be `0` there — like
/// list items — or selection copy would strip the rail twice and lose quote
/// text. Wrapped continuation rows have no rail scan (plain spaces), so they
/// report the rail width so selection copy skips their alignment.
fn render_quote_line_tagged(
    text: &str,
    depth: usize,
    width: usize,
    rail_style: Style,
    text_style: Style,
) -> Vec<RenderedMarkdownLine> {
    let depth = depth.clamp(1, MAX_QUOTE_DEPTH);
    let rail = "│ ".repeat(depth);
    let rail_width = rail.width();
    let available = width.saturating_sub(rail_width).max(1);
    let wrapped = render_line_with_links_tagged(text, available, text_style, link_style());

    let mut out = Vec::new();
    for (idx, rendered) in wrapped.into_iter().enumerate() {
        let links = rendered
            .links
            .iter()
            .map(|link| link.shifted(rail_width))
            .collect();
        let mut spans = if idx == 0 {
            (0..depth).map(|_| Span::styled("│ ", rail_style)).collect()
        } else {
            vec![Span::raw(" ".repeat(rail_width))]
        };
        spans.extend(rendered.line.spans);
        out.push(RenderedMarkdownLine {
            line: Line::from(spans),
            links,
            is_code: false,
            // First row: the transcript rail-scan strips the visible rail
            // (mirror `render_list_line_tagged`); continuation rows: report
            // the alignment width so selection copy skips it.
            copy_prefix_width: if idx == 0 { 0 } else { rail_width },
            copy_separator_after: rendered.copy_separator_after,
        });
    }
    out
}

#[cfg(test)]
fn render_line_with_links(
    line: &str,
    width: usize,
    base_style: Style,
    link_style: Style,
) -> Vec<Line<'static>> {
    render_line_with_links_tagged(line, width, base_style, link_style)
        .into_iter()
        .map(|rendered| rendered.line)
        .collect()
}

fn render_line_with_links_tagged(
    line: &str,
    width: usize,
    base_style: Style,
    link_style: Style,
) -> Vec<RenderedMarkdownLine> {
    if line.trim().is_empty() {
        return vec![RenderedMarkdownLine {
            line: Line::from(""),
            links: Vec::new(),
            is_code: false,
            copy_prefix_width: 0,
            copy_separator_after: CopyLineSeparator::Newline,
        }];
    }

    // Flatten inline tokens into (word, style) pairs preserving inter-token spaces.
    let tokens = parse_inline_spans(line, base_style, link_style);
    let mut words: Vec<InlineToken> = Vec::new();
    for token in tokens {
        let mut first = true;
        for part in token.text.split(' ') {
            if !first {
                // The space consumed by split remains part of a markdown-link
                // label when the surrounding token is linked. It is still a
                // wrap opportunity and is dropped at a row boundary.
                words.push(InlineToken::new(
                    " ".to_string(),
                    token.style,
                    token.link_url.clone(),
                ));
            }
            if !part.is_empty() {
                words.push(InlineToken::new(
                    part.to_string(),
                    token.style,
                    token.link_url.clone(),
                ));
            }
            first = false;
        }
    }

    let mut lines: Vec<RenderedMarkdownLine> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut current_links: Vec<osc8::LineLink> = Vec::new();
    let mut current_width = 0usize;

    for word in words {
        let ww = word.text.width();
        if word.text == " " {
            // Space: emit only if we're mid-line and it fits; otherwise drop
            // (it's a potential wrap point, not content).
            if !current_spans.is_empty() && current_width < width {
                current_spans.push(word.span_for(" ".to_string()));
                record_inline_link(&mut current_links, &word, current_width, 1);
                current_width += 1;
            }
            continue;
        }
        // If the word itself is wider than an entire line, hard-break it at
        // grapheme boundaries so wrapping always makes progress (#1344,
        // #1351). Without this, long URLs / paths / hashes were placed on
        // their own line whole and silently overflowed the right edge of
        // the transcript.
        if ww > width && width > 0 {
            // Flush the in-progress line first.
            if !current_spans.is_empty() {
                push_inline_line(
                    &mut lines,
                    &mut current_spans,
                    &mut current_links,
                    CopyLineSeparator::Space,
                );
                current_width = 0;
            }
            // Char-break the word into width-sized chunks. Each full chunk
            // becomes its own line; the final partial chunk continues the
            // current line so the next word can pack onto it.
            let mut chunk = String::new();
            let mut chunk_w = 0usize;
            for grapheme in word.text.graphemes(true) {
                let grapheme_width = grapheme.width();
                if chunk_w + grapheme_width > width && chunk_w > 0 {
                    let chunk = std::mem::take(&mut chunk);
                    let mut links = Vec::new();
                    record_inline_link(&mut links, &word, 0, chunk_w);
                    lines.push(RenderedMarkdownLine {
                        line: Line::from(vec![word.span_for(chunk)]),
                        links,
                        is_code: false,
                        copy_prefix_width: 0,
                        copy_separator_after: CopyLineSeparator::None,
                    });
                    chunk_w = 0;
                }
                chunk.push_str(grapheme);
                chunk_w += grapheme_width;
            }
            if !chunk.is_empty() {
                record_inline_link(&mut current_links, &word, 0, chunk_w);
                current_spans.push(word.span_for(chunk));
                current_width = chunk_w;
            }
            continue;
        }
        // Wrap before this word if it doesn't fit.
        if current_width > 0 && current_width + ww > width {
            // Trim trailing space span before breaking.
            push_inline_line(
                &mut lines,
                &mut current_spans,
                &mut current_links,
                CopyLineSeparator::Space,
            );
            current_width = 0;
        }
        record_inline_link(&mut current_links, &word, current_width, ww);
        current_spans.push(word.into_span());
        current_width += ww;
    }

    if !current_spans.is_empty() {
        push_inline_line(
            &mut lines,
            &mut current_spans,
            &mut current_links,
            CopyLineSeparator::Newline,
        );
    } else if let Some(last) = lines.last_mut() {
        last.copy_separator_after = CopyLineSeparator::Newline;
    }
    if lines.is_empty() {
        lines.push(RenderedMarkdownLine {
            line: Line::from(""),
            links: Vec::new(),
            is_code: false,
            copy_prefix_width: 0,
            copy_separator_after: CopyLineSeparator::Newline,
        });
    }
    lines
}

fn push_inline_line(
    lines: &mut Vec<RenderedMarkdownLine>,
    spans: &mut Vec<Span<'static>>,
    links: &mut Vec<osc8::LineLink>,
    copy_separator_after: CopyLineSeparator,
) {
    if let Some(last) = spans.last()
        && last.content.as_ref() == " "
    {
        spans.pop();
    }
    let visible_width = spans
        .iter()
        .map(|span| span.content.as_ref().width())
        .sum::<usize>();
    links.retain(|link| link.col_start < visible_width);
    for link in links.iter_mut() {
        link.col_end = link.col_end.min(visible_width.saturating_sub(1));
    }
    lines.push(RenderedMarkdownLine {
        line: Line::from(std::mem::take(spans)),
        links: std::mem::take(links),
        is_code: false,
        copy_prefix_width: 0,
        copy_separator_after,
    });
}

fn record_inline_link(
    links: &mut Vec<osc8::LineLink>,
    token: &InlineToken,
    col_start: usize,
    width: usize,
) {
    let Some(target) = token.link_url.as_ref() else {
        return;
    };
    if width == 0 {
        return;
    }
    let col_end = col_start.saturating_add(width).saturating_sub(1);
    if let Some(last) = links.last_mut()
        && last.target == *target
        && last.col_end.saturating_add(1) == col_start
    {
        last.col_end = col_end;
        return;
    }
    links.push(osc8::LineLink {
        col_start,
        col_end,
        target: target.clone(),
    });
}

#[derive(Clone)]
struct InlineToken {
    text: String,
    style: Style,
    link_url: Option<String>,
}

impl InlineToken {
    fn new(text: String, style: Style, link_url: Option<String>) -> Self {
        Self {
            text,
            style,
            link_url,
        }
    }

    fn span_for(&self, text: String) -> Span<'static> {
        Span::styled(text, self.style)
    }

    fn into_span(self) -> Span<'static> {
        Span::styled(self.text, self.style)
    }
}

/// Parse an entire line into (text, style) segments, handling **bold**,
/// *italic*, `code`, ~~strikethrough~~, `[text](url)` links, and bare URLs.
fn parse_inline_spans(line: &str, base_style: Style, link_style: Style) -> Vec<InlineToken> {
    let bold_style = base_style.add_modifier(Modifier::BOLD);
    let italic_style = base_style.add_modifier(Modifier::ITALIC);
    let code_style = base_style
        .add_modifier(Modifier::ITALIC)
        .bg(palette::SURFACE_ELEVATED);
    let strike_style = base_style.add_modifier(Modifier::CROSSED_OUT);
    let mut out = Vec::new();
    let mut rest = line;

    while !rest.is_empty() {
        // Backslash escape (CommonMark §2.4): `\` before ASCII punctuation
        // yields the literal character. Producers escape untrusted text
        // (plugin names, paths) this way so it cannot open markup; without
        // this arm the backslash itself reached the terminal
        // (`computer\-use`, `0\.1\.0` — 0.9.12 defect #21).
        if let Some(escaped) = rest.strip_prefix('\\')
            && let Some(ch) = escaped.chars().next()
            && ch.is_ascii_punctuation()
        {
            out.push(InlineToken::new(ch.to_string(), base_style, None));
            rest = &escaped[ch.len_utf8()..];
            continue;
        }
        // **bold**
        if let Some(end) = rest.strip_prefix("**").and_then(|s| s.find("**")) {
            let inner = &rest[2..2 + end];
            out.push(InlineToken::new(inner.to_string(), bold_style, None));
            rest = &rest[2 + end + 2..];
            continue;
        }
        // __bold__
        if let Some(end) = rest.strip_prefix("__").and_then(|s| s.find("__")) {
            let inner = &rest[2..2 + end];
            out.push(InlineToken::new(inner.to_string(), bold_style, None));
            rest = &rest[2 + end + 2..];
            continue;
        }
        // *italic*
        if rest.starts_with('*')
            && !rest.starts_with("**")
            && let Some(end) = rest[1..].find('*')
        {
            let inner = &rest[1..1 + end];
            let after = &rest[1 + end + 1..];
            // Closing delimiter must not be immediately followed by a
            // letter, digit, or underscore (otherwise it's part of an
            // identifier like `codewhale_tui`, not italic markup).
            if !after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                out.push(InlineToken::new(inner.to_string(), italic_style, None));
                rest = after;
                continue;
            }
        }
        // _italic_
        if rest.starts_with('_')
            && !rest.starts_with("__")
            && let Some(end) = rest[1..].find('_')
        {
            let inner = &rest[1..1 + end];
            let after = &rest[1 + end + 1..];
            // CommonMark forbids an intraword `_` from opening emphasis and
            // requires the opener to be left-flanking (not followed by
            // whitespace). Guarding only the closer let `b_p … t_?` italicize
            // the prose between two math subscripts (#6042).
            let preceded_by_word = line[..line.len() - rest.len()]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
            let openable =
                !preceded_by_word && inner.chars().next().is_some_and(|c| !c.is_whitespace());
            // Closing delimiter must not be immediately followed by a
            // letter, digit, or underscore.
            if openable && !after.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                out.push(InlineToken::new(inner.to_string(), italic_style, None));
                rest = after;
                continue;
            }
        }
        // `inline code`
        if let Some(end) = rest.strip_prefix('`').and_then(|s| s.find('`')) {
            let inner = &rest[1..1 + end];
            out.push(InlineToken::new(inner.to_string(), code_style, None));
            rest = &rest[1 + end + 1..];
            continue;
        }
        // ~~strikethrough~~
        if let Some(end) = rest.strip_prefix("~~").and_then(|s| s.find("~~")) {
            let inner = &rest[2..2 + end];
            out.push(InlineToken::new(inner.to_string(), strike_style, None));
            rest = &rest[2 + end + 2..];
            continue;
        }
        // [text](url)
        if rest.starts_with('[')
            && let Some(bracket_end) = rest.find(']')
        {
            let text = &rest[1..bracket_end];
            let after_bracket = &rest[bracket_end + 1..];
            if after_bracket.starts_with('(')
                && let Some(paren_end) = after_bracket.find(')')
            {
                let url = &after_bracket[1..paren_end];
                // The runtime toggle gates backend emission, not layout.
                // Keeping the same visible label and metadata in both modes
                // prevents toggling OSC 8 from reflowing the transcript.
                out.push(InlineToken::new(
                    text.to_string(),
                    link_style,
                    normalized_link_target(url),
                ));
                rest = &after_bracket[paren_end + 1..];
                continue;
            }
        }
        // URL: consume until whitespace, then keep trailing punctuation
        // visible but outside the hyperlink target.
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            let token = &rest[..token_end];
            let url_end = trailing_url_end(token);
            let url = &token[..url_end];
            out.push(InlineToken::new(
                url.to_string(),
                link_style,
                normalized_http_link_target(url),
            ));
            if url_end < token_end {
                out.push(InlineToken::new(
                    token[url_end..].to_string(),
                    base_style,
                    None,
                ));
            }
            rest = &rest[token_end..];
            continue;
        }
        // Plain text: consume until next marker or URL; always advance at least 1 char.
        let next = find_next_marker(rest).max(rest.chars().next().map_or(1, |c| c.len_utf8()));
        out.push(InlineToken::new(rest[..next].to_string(), base_style, None));
        rest = &rest[next..];
    }
    out
}

/// Normalize an explicit markdown link destination, the only construct the
/// renderer treats as a structured reference. Prose is never scanned for
/// path-shaped text: that linkifies identifiers and version strings, and points
/// at files that do not exist.
fn normalized_link_target(target: &str) -> Option<String> {
    normalized_http_link_target(target).or_else(|| normalized_file_link_target(target))
}

/// A destination that is already an absolute path, with or without the `file:`
/// scheme. Relative destinations stay inert because this layer has no workspace
/// root to resolve them against, and a `file://host/...` form is rejected rather
/// than reinterpreted as local. Windows paths must therefore arrive pre-formed
/// as `file:///C:/…`.
fn normalized_file_link_target(target: &str) -> Option<String> {
    let path = match target.get(..7) {
        Some(prefix) if prefix.eq_ignore_ascii_case("file://") => &target[7..],
        _ => target,
    };
    if !path.starts_with('/') || path.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return None;
    }
    Some(format!("file://{path}"))
}

/// OSC 8 targets produced by markdown are deliberately limited to ordinary
/// web URLs. The browser-opening gesture is user-initiated, but accepting
/// arbitrary schemes here would still turn untrusted model output into a
/// `javascript:` or application-protocol link. Normalize the scheme and reject
/// whitespace/control characters before metadata reaches a frame.
fn normalized_http_link_target(target: &str) -> Option<String> {
    let (scheme, rest) = if target
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
    {
        ("https://", &target[8..])
    } else if target
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
    {
        ("http://", &target[7..])
    } else {
        return None;
    };
    if rest.is_empty()
        || rest.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        || rest.split(['/', '?', '#']).next().is_none_or(str::is_empty)
    {
        return None;
    }
    Some(format!("{scheme}{rest}"))
}

fn trailing_url_end(candidate: &str) -> usize {
    let mut end = candidate.len();
    while end > 0 {
        let remaining = &candidate[..end];
        let Some(ch) = remaining.chars().next_back() else {
            break;
        };
        let trim = matches!(ch, ',' | '.' | ';' | '!' | '\'' | '"')
            || matches!(ch, ')' | ']' | '}' | '>')
                && has_unmatched_closing_delimiter(remaining, ch);
        if !trim {
            break;
        }
        end -= ch.len_utf8();
    }
    end
}

fn has_unmatched_closing_delimiter(candidate: &str, closing: char) -> bool {
    let opening = match closing {
        ')' => '(',
        ']' => '[',
        '}' => '{',
        '>' => '<',
        _ => return false,
    };
    candidate.chars().filter(|ch| *ch == closing).count()
        > candidate.chars().filter(|ch| *ch == opening).count()
}

/// Find the index of the next inline marker (`**`, `__`, `*`, `_`, `http`)
/// in `s`, or `s.len()` if none found.
fn find_next_marker(s: &str) -> usize {
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        let ch_len = s[i..].chars().next().map_or(1, |c| c.len_utf8());
        let slice = &s[i..];
        if slice.starts_with('\\')
            || slice.starts_with("**")
            || slice.starts_with("__")
            || slice.starts_with("~~")
            || slice.starts_with('`')
            || slice.starts_with('[')
            || (slice.starts_with('*') && !slice.starts_with("**"))
            || (slice.starts_with('_') && !slice.starts_with("__"))
            || slice.starts_with("http://")
            || slice.starts_with("https://")
        {
            return i;
        }
        i += ch_len;
    }
    s.len()
}

fn is_horizontal_rule(line: &str) -> bool {
    let stripped: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    (stripped.chars().all(|c| c == '-')
        || stripped.chars().all(|c| c == '*')
        || stripped.chars().all(|c| c == '_'))
        && stripped.len() >= 3
}

/// Parse a markdown table row like `| foo | bar |` into trimmed cell strings.
/// Returns `None` for separator rows (`|---|---|`).
fn parse_table_row(line: &str) -> Option<Vec<String>> {
    if !line.starts_with('|') {
        return None;
    }
    let inner = line.trim_matches('|');
    let cells = split_table_cells(inner);
    // Separator row: every non-empty cell is only dashes/colons/spaces
    if cells
        .iter()
        .all(|c| c.is_empty() || c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' '))
    {
        return None;
    }
    Some(cells)
}

fn split_table_cells(inner: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut in_code = false;
    let mut chars = inner.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if matches!(chars.peek(), Some('|')) {
                    current.push('|');
                    let _ = chars.next();
                } else {
                    current.push(ch);
                }
            }
            '`' => {
                in_code = !in_code;
                current.push(ch);
            }
            '|' if !in_code => {
                cells.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    cells.push(current.trim().to_string());
    cells
}

/// Word-wrap a single cell's text into one or more visual lines, each
/// constrained to `col_width` display columns. Whitespace is the preferred
/// break point; words wider than `col_width` are hard-broken at grapheme
/// boundaries so wrapping always makes progress (no infinite loop on URLs
/// or paths). Returns at least one segment.
fn wrap_cell_text(cell: &str, col_width: usize) -> Vec<String> {
    if cell.is_empty() || cell.width() <= col_width {
        return vec![cell.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;

    for word in cell.split_whitespace() {
        let word_w = word.width();
        if current_w == 0 {
            if word_w > col_width {
                push_word_breaking_graphemes(
                    word,
                    col_width,
                    &mut current,
                    &mut current_w,
                    &mut lines,
                );
            } else {
                current.push_str(word);
                current_w = word_w;
            }
        } else if current_w + 1 + word_w <= col_width {
            current.push(' ');
            current.push_str(word);
            current_w += 1 + word_w;
        } else {
            lines.push(std::mem::take(&mut current));
            current_w = 0;
            if word_w > col_width {
                push_word_breaking_graphemes(
                    word,
                    col_width,
                    &mut current,
                    &mut current_w,
                    &mut lines,
                );
            } else {
                current.push_str(word);
                current_w = word_w;
            }
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn render_table_row(
    cells: &[String],
    num_cols: usize,
    width: usize,
    base_style: Style,
) -> Vec<Line<'static>> {
    let col_width = table_col_width(num_cols, width);
    let sep_style = Style::default().fg(palette::TEXT_DIM);

    // Wrap each cell into one or more visual segments. The row's visual
    // height equals the tallest column. Cells that wrap to fewer segments
    // get blank-padded continuation lines so column separators stay aligned.
    let wrapped: Vec<Vec<String>> = (0..num_cols)
        .map(|i| wrap_cell_text(cells.get(i).map_or("", String::as_str), col_width))
        .collect();
    let row_height = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(row_height);
    for row in 0..row_height {
        let mut spans: Vec<Span> = vec![Span::styled("│ ".to_string(), sep_style)];
        for (i, cell_segments) in wrapped.iter().enumerate() {
            let segment = cell_segments.get(row).map(String::as_str).unwrap_or("");
            let cell_spans = parse_inline_spans(segment, base_style, link_style());
            let cell_width: usize = cell_spans.iter().map(|token| token.text.width()).sum();
            let pad = col_width.saturating_sub(cell_width);
            for token in cell_spans {
                spans.push(token.into_span());
            }
            spans.push(Span::raw(" ".repeat(pad)));
            if i + 1 < num_cols {
                spans.push(Span::styled(" │ ".to_string(), sep_style));
            } else {
                spans.push(Span::styled(" │".to_string(), sep_style));
            }
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn table_col_width(num_cols: usize, term_width: usize) -> usize {
    let col_width = (term_width.saturating_sub(3 * num_cols + 1)) / num_cols;
    col_width.max(4)
}

fn render_table_border(
    num_cols: usize,
    col_width: usize,
    sep_style: Style,
    left: &str,
    mid: &str,
    right: &str,
) -> Line<'static> {
    let fill = "\u{2500}".repeat(col_width);
    let mut s = String::new();
    s.push_str(left);
    for i in 0..num_cols {
        s.push_str(&fill);
        if i + 1 < num_cols {
            s.push_str(mid);
        } else {
            s.push_str(right);
        }
    }
    Line::from(Span::styled(s, sep_style))
}

fn render_table_group(blocks: &[Block], width: usize, base_style: Style) -> Vec<Line<'static>> {
    let sep_style = Style::default().fg(palette::TEXT_DIM);

    let num_cols = blocks
        .iter()
        .filter_map(|b| match b {
            Block::TableRow(cells) => Some(cells.len()),
            _ => None,
        })
        .max()
        .unwrap_or(1);

    let col_width = table_col_width(num_cols, width);

    let mut lines = Vec::new();

    // Top border
    lines.push(render_table_border(
        num_cols,
        col_width,
        sep_style,
        "\u{250C}\u{2500}",
        "\u{2500}\u{252C}\u{2500}",
        "\u{2500}\u{2510}",
    ));

    let mid_border = || {
        render_table_border(
            num_cols,
            col_width,
            sep_style,
            "\u{251C}\u{2500}",
            "\u{2500}\u{253C}\u{2500}",
            "\u{2500}\u{2524}",
        )
    };

    let header_end = blocks
        .iter()
        .position(|b| matches!(b, Block::TableSeparator))
        .unwrap_or(0);

    for (i, block) in blocks.iter().enumerate() {
        match block {
            Block::TableRow(cells) => {
                let style = if i < header_end {
                    base_style.add_modifier(Modifier::BOLD)
                } else {
                    base_style
                };
                lines.extend(render_table_row(cells, num_cols, width, style));
            }
            Block::TableSeparator => {
                lines.push(mid_border());
            }
            _ => {}
        }
    }

    // Bottom border
    lines.push(render_table_border(
        num_cols,
        col_width,
        sep_style,
        "\u{2514}\u{2500}",
        "\u{2500}\u{2534}\u{2500}",
        "\u{2500}\u{2518}",
    ));

    lines
}

fn link_style() -> Style {
    Style::default()
        .fg(palette::WHALE_ACTION)
        .add_modifier(Modifier::UNDERLINED)
}

/// Display-column width of one extended grapheme for terminal line-wrap
/// calculations.
///
/// A tab advances to the next 8-column tab stop. Single-codepoint characters
/// retain the previous one-column fallback (with an override for enclosed
/// alphanumerics that render as 2 columns in CJK terminals). Multi-codepoint
/// emoji, keycaps, and combining sequences use the same string-level width
/// contract as Ratatui, with an override for keycap sequences containing
/// U+20E3 that unicode-width undercounts. (#4479)
fn markdown_grapheme_width(grapheme: &str, col: usize) -> usize {
    if grapheme == "\t" {
        return 8 - (col % 8); // advance to next 8-column tab stop
    }
    if let Some(ch) = grapheme.chars().next()
        && ch.len_utf8() == grapheme.len()
    {
        return match ch {
            // Enclosed alphanumerics, dingbat circled digits, and circled
            // numbers on black square render as 2 columns in CJK terminals
            // even though unicode-width reports 1. (#4479)
            '\u{2460}'..='\u{24FF}' | '\u{2776}'..='\u{2793}' | '\u{3248}'..='\u{324F}' => 2,
            _ => ch.width().unwrap_or(1),
        };
    }
    // Keycap sequences (with or without FE0F) render as 2 columns.
    if grapheme.contains('\u{20e3}') {
        return 2;
    }
    grapheme.width()
}

/// Push graphemes from `word` into `current`, flushing to `lines` when the
/// running display width would exceed `width`. String-level Unicode width
/// matches Ratatui for emoji and combining sequences.
/// Used by `wrap_cell_text` so a word longer than the allotted width never
/// silently overflows the right edge.
fn push_word_breaking_graphemes(
    word: &str,
    width: usize,
    current: &mut String,
    current_width: &mut usize,
    lines: &mut Vec<String>,
) {
    for grapheme in word.graphemes(true) {
        let grapheme_width = grapheme.width();
        if *current_width + grapheme_width > width && *current_width > 0 {
            lines.push(std::mem::take(current));
            *current_width = 0;
        }
        current.push_str(grapheme);
        *current_width += grapheme_width;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Style;

    fn visible_lines(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn backslash_escapes_yield_the_literal_punctuation() {
        let lines = render_markdown(
            "ID: computer\\-use 0\\.1\\.0 \\*not italic\\* trailing\\",
            80,
            Style::default(),
        );
        assert_eq!(
            visible_lines(&lines),
            vec!["ID: computer-use 0.1.0 *not italic* trailing\\"]
        );
    }

    fn rendered_fingerprint(lines: &[RenderedMarkdownLine]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                format!(
                    "{:?}|{:?}|{}|{}|{:?}",
                    line.line,
                    line.links,
                    line.is_code,
                    line.copy_prefix_width,
                    line.copy_separator_after
                )
            })
            .collect()
    }

    fn update_incremental_render(
        cache: &mut IncrementalMarkdownRenderCache,
        rendered: &mut Vec<RenderedMarkdownLine>,
        source: &str,
        width: u16,
        palette_mode: palette::PaletteMode,
        verified_append: bool,
    ) {
        update_incremental_render_with_style(
            cache,
            rendered,
            source,
            width,
            Style::default(),
            palette_mode,
            verified_append,
        );
    }

    fn update_incremental_render_with_style(
        cache: &mut IncrementalMarkdownRenderCache,
        rendered: &mut Vec<RenderedMarkdownLine>,
        source: &str,
        width: u16,
        base_style: Style,
        palette_mode: palette::PaletteMode,
        verified_append: bool,
    ) {
        let delta = cache.update(source, width, base_style, palette_mode, verified_append);
        rendered.truncate(delta.replace_from);
        rendered.extend(delta.lines);
    }

    #[test]
    fn incremental_render_is_exact_and_linear_for_unicode_fences_and_tables() {
        let mut cache = IncrementalMarkdownRenderCache::default();
        let mut rendered = Vec::new();
        let mut source = String::new();
        let chunks = 80usize;

        for index in 0..chunks {
            source.push_str(&format!(
                "## 段落 {index}\nUnicode e\u{301} 世界 🚀\n```rust\nlet 値_{index}: usize = {index}; // 注釈\n```\n| key | value |\n|---|---|\n| {index} | 世界 |\n\n"
            ));
            update_incremental_render(
                &mut cache,
                &mut rendered,
                &source,
                96,
                palette::PaletteMode::Dark,
                index > 0,
            );
            let cold = render_markdown_tagged_with_palette(
                &source,
                96,
                Style::default(),
                palette::PaletteMode::Dark,
            );
            assert_eq!(
                rendered_fingerprint(&rendered),
                rendered_fingerprint(&cold),
                "incremental output diverged after chunk {index}"
            );
        }

        let work = cache.work();
        let parsed = reference_parse(&source);
        assert_eq!(work.classified_lines as usize, source.lines().count());
        assert_eq!(work.stable_blocks_rendered as usize, parsed.blocks.len());
        assert_eq!(work.tail_blocks_rendered, 0);
        assert_eq!(work.invalidations, 1);
    }

    #[test]
    fn incremental_render_invalidates_on_mutation_width_theme_and_style() {
        let mut cache = IncrementalMarkdownRenderCache::default();
        let mut rendered = Vec::new();
        let mut source = "alpha\n```rust\nlet value = 1;\n```\n".to_string();
        update_incremental_render(
            &mut cache,
            &mut rendered,
            &source,
            80,
            palette::PaletteMode::Dark,
            false,
        );

        source.push_str("tail 世界\n");
        update_incremental_render(
            &mut cache,
            &mut rendered,
            &source,
            80,
            palette::PaletteMode::Dark,
            true,
        );

        source.replace_range(..5, "ALPHA");
        update_incremental_render(
            &mut cache,
            &mut rendered,
            &source,
            80,
            palette::PaletteMode::Dark,
            false,
        );
        let mutated = render_markdown_tagged_with_palette(
            &source,
            80,
            Style::default(),
            palette::PaletteMode::Dark,
        );
        assert_eq!(
            rendered_fingerprint(&rendered),
            rendered_fingerprint(&mutated)
        );

        update_incremental_render(
            &mut cache,
            &mut rendered,
            &source,
            37,
            palette::PaletteMode::Dark,
            true,
        );
        update_incremental_render(
            &mut cache,
            &mut rendered,
            &source,
            37,
            palette::PaletteMode::Light,
            true,
        );

        let changed_style = Style::default().add_modifier(Modifier::ITALIC);
        update_incremental_render_with_style(
            &mut cache,
            &mut rendered,
            &source,
            37,
            changed_style,
            palette::PaletteMode::Light,
            true,
        );
        let rethemed = render_markdown_tagged_with_palette(
            &source,
            37,
            changed_style,
            palette::PaletteMode::Light,
        );
        assert_eq!(
            rendered_fingerprint(&rendered),
            rendered_fingerprint(&rethemed)
        );
        assert_eq!(cache.work().invalidations, 5);
    }

    #[test]
    fn incremental_render_drops_committed_source_without_a_large_answer_cliff() {
        let line = format!("{}\n", "x".repeat(16 * 1024));
        let mut source = String::new();
        let mut cache = IncrementalMarkdownRenderCache::default();
        let mut rendered = Vec::new();

        for index in 0..80 {
            source.push_str(&line);
            update_incremental_render(
                &mut cache,
                &mut rendered,
                &source,
                u16::MAX,
                palette::PaletteMode::Dark,
                index > 0,
            );
            assert_eq!(cache.retained_source_bytes(), 0);
        }

        assert!(source.len() > 1024 * 1024);
        assert_eq!(cache.retained_source_bytes(), 0);
        assert_eq!(cache.work().classified_lines, 80);
        assert_eq!(cache.work().stable_blocks_rendered, 80);
        assert_eq!(cache.work().invalidations, 1);
    }

    #[test]
    fn verified_append_resume_rejects_a_rewritten_committed_prefix() {
        // #6196: the streaming cache consumes the latex-rendered projection
        // of the raw stream. While a `\[` display block is open,
        // `render_latex_in_text` passes it through as literal text and the
        // incremental cache commits those lines. When the closing `\]`
        // finally arrives the rendered form rewrites the *committed* bytes
        // even though the raw stream only appended, so the append receipt
        // stays valid and only a content-aware committed-prefix check can
        // see the rewrite. A length-only check resumed from the stale lines
        // and the un-rendered opener stuck in the cell forever. The strings
        // below are the projections the latex layer produces for that
        // sequence.
        let mut cache = IncrementalMarkdownRenderCache::default();
        let mut rendered = Vec::new();
        // Beat 1: raw stream `para\n\[ \alpha\n` — the open block passes
        // through literally and its complete line is committed.
        update_incremental_render(
            &mut cache,
            &mut rendered,
            "para\n\\[ \\alpha\n",
            80,
            palette::PaletteMode::Dark,
            false,
        );
        assert_eq!(cache.work().invalidations, 1);

        // Beat 2: the raw stream appended `\]` plus more prose; the
        // projection rewrites the committed line and grows past its old
        // length, so the resume guard cannot rely on length alone.
        let closed = "para\nα\nthe identity holds for every pair of terms\n";
        update_incremental_render(
            &mut cache,
            &mut rendered,
            closed,
            80,
            palette::PaletteMode::Dark,
            true,
        );
        assert_eq!(
            cache.work().invalidations,
            2,
            "rewriting committed bytes must invalidate the resume"
        );
        let cold = render_markdown_tagged_with_palette(
            closed,
            80,
            Style::default(),
            palette::PaletteMode::Dark,
        );
        assert_eq!(
            rendered_fingerprint(&rendered),
            rendered_fingerprint(&cold),
            "the re-render must match a cold render of the closed form"
        );
        assert!(
            !rendered.iter().any(|line| {
                line.line
                    .spans
                    .iter()
                    .any(|span| span.content.contains("\\["))
            }),
            "the stale literal latex opener must not survive the close"
        );
    }

    #[test]
    fn underscores_inside_identifiers_render_as_literal_text() {
        // Regression for PR #1455 / @tiger-dog: previously the inline
        // markdown parser ate the underscore in `codewhale_tui` because
        // it matched the `_italic_` pattern without a CommonMark-style
        // boundary check. The closing `_` followed by `t` (a letter)
        // must now be treated as part of the identifier, not as
        // markup. The same rule applies to `*` so identifiers like
        // `crate*foo` round-trip cleanly.
        let cases = [
            "crate codewhale_tui handles approvals",
            "see foo_bar_baz for details",
            "look at *not_emphasised*tail",
        ];
        for source in cases {
            let parsed = parse(source);
            let rendered: String = render_parsed(&parsed, 80, Style::default())
                .iter()
                .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
                .collect();
            // The original identifier (with underscores intact) must
            // appear in the rendered output. We don't assert on style
            // here — that's an implementation detail; we assert on
            // the user-visible character sequence.
            for token in source.split_whitespace().filter(|t| t.contains('_')) {
                assert!(
                    rendered.contains(token),
                    "identifier {token:?} must survive markdown rendering of {source:?}; got {rendered:?}"
                );
            }
        }
    }

    #[test]
    fn underscore_emphasis_does_not_open_mid_word_across_math_subscripts() {
        // #6042: the closing-side guard alone let `b_p` open emphasis and
        // `t_?` close it, italicizing 60 characters of prose. CommonMark
        // forbids an intraword `_` from opening emphasis.
        let source = "[t_, b_p]. Actually — hold on, do we even tile all the way from t_?";
        let lines = render_parsed(&parse(source), 200, Style::default());
        let italic: Vec<&str> = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.style.add_modifier.contains(Modifier::ITALIC))
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            italic.is_empty(),
            "math subscripts must not open emphasis; italic spans: {italic:?}"
        );

        // A properly flanked `_italic_` run still renders italic…
        let flanked = render_parsed(&parse("an _emphasised_ word"), 80, Style::default());
        assert!(
            flanked
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.style.add_modifier.contains(Modifier::ITALIC)
                    && span.content.as_ref() == "emphasised"),
            "a flanked _italic_ run must still render italic"
        );
        // …and after punctuation it still opens.
        let after_punct = render_parsed(&parse("word (_also_)"), 80, Style::default());
        assert!(
            after_punct
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.style.add_modifier.contains(Modifier::ITALIC)
                    && span.content.as_ref() == "also"),
            "a _ run after punctuation must still open"
        );
    }

    #[test]
    fn render_markdown_matches_parse_then_render() {
        let source = "# Title\n\nA paragraph with a https://example.com link.\n\n- one\n- two\n```\ncode\n```";
        let direct = render_markdown(source, 80, Style::default())
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>();
        let parsed = parse(source);
        let two_step = render_parsed(&parsed, 80, Style::default())
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>();
        assert_eq!(direct, two_step);
    }

    #[test]
    fn render_plain_text_preserves_literal_markdown_and_spacing() {
        let source = "  # heading\n- item\n   \nhello    world\n";
        let lines = render_plain_text(source, 80, Style::default());

        assert_eq!(
            visible_lines(&lines),
            vec!["  # heading", "- item", "   ", "hello    world", ""]
        );
    }

    #[test]
    fn render_plain_text_wraps_without_collapsing_spaces() {
        let source = "alpha    beta gamma";
        let lines = render_plain_text(source, 12, Style::default());
        for width in rendered_widths(&lines) {
            assert!(width <= 12, "rendered width {width} exceeds budget");
        }

        let combined = visible_lines(&lines).join("");
        assert_eq!(combined, source);
    }

    #[test]
    fn render_plain_text_breaks_overlong_words() {
        let source = "x".repeat(40);
        let lines = render_plain_text(&source, 9, Style::default());
        for width in rendered_widths(&lines) {
            assert!(width <= 9, "rendered width {width} exceeds budget");
        }

        let combined = visible_lines(&lines).join("");
        assert_eq!(combined, source);
    }

    #[test]
    fn parse_is_width_independent() {
        // Same source, two parses, must produce identical AST. (Sanity:
        // parse must not depend on hidden global state like terminal width.)
        let source = "Hello\n\n## Heading\n- list\n";
        let a = parse(source);
        let b = parse(source);
        assert_eq!(a, b);
    }

    #[test]
    fn render_parsed_word_wrap_changes_with_width() {
        // The same AST must produce different layouts at different widths;
        // otherwise the split is decorative, not functional.
        let parsed = parse("alpha beta gamma delta epsilon zeta");
        let wide = render_parsed(&parsed, 80, Style::default());
        let narrow = render_parsed(&parsed, 10, Style::default());
        assert!(
            narrow.len() > wide.len(),
            "narrow should produce more lines"
        );
    }

    #[test]
    fn parse_invocations_increment() {
        // Counter is thread-local, so concurrent tests calling `parse()`
        // can't pollute each other.
        reset_parse_invocation_count();
        let _ = parse("hello\n");
        let _ = parse("world\n");
        assert_eq!(parse_invocation_count(), 2);
    }

    #[test]
    fn render_parsed_does_not_call_parse() {
        // Width-only changes must hit only the render path. This is the
        // perf invariant CX#6 was filed for.
        let parsed = parse("multiline\nsource\nwith several\nlines\n");
        reset_parse_invocation_count();
        let _ = render_parsed(&parsed, 80, Style::default());
        let _ = render_parsed(&parsed, 40, Style::default());
        let _ = render_parsed(&parsed, 20, Style::default());
        assert_eq!(
            parse_invocation_count(),
            0,
            "render_parsed must not call parse"
        );
    }

    // -----------------------------------------------------------------
    //  #3897 — streaming re-parse is incremental, not quadratic
    // -----------------------------------------------------------------

    /// Markdown corpus chosen to hit every carry-state transition the parser
    /// has: fences opened and closed across chunk boundaries, an unterminated
    /// fence, headings, nested lists, tables, rules, blanks, CRLF, and CJK.
    fn streaming_corpus() -> Vec<&'static str> {
        vec![
            "# Title\n\nSome prose that wraps.\n\n- alpha\n- beta\n",
            "text\n```rust\nlet x = 1;\nlet y = 2;\n```\nafter\n",
            "```\nunterminated fence never closes\nstill inside\n",
            "| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\ndone\n",
            "1. one\n2. two\n   * nested\n\n## Sub\n\n***\n",
            "混合 CJK 内容\n\n```python\nprint(\"中文\")\n```\n尾部\n",
            "crlf lines\r\nsecond\r\n\r\n```go\nfmt.Println()\r\n```\r\n",
            "no trailing newline at all",
            "",
        ]
    }

    /// The acceptance guarantee: streaming a message chunk by chunk produces,
    /// at every intermediate prefix, exactly what a full re-parse of that
    /// prefix produces. Byte-for-byte on the AST, so a divergence in any field
    /// of any block fails here rather than showing up as a render artifact.
    #[test]
    fn incremental_parse_matches_a_full_reparse_at_every_prefix() {
        for source in streaming_corpus() {
            // Grow one byte at a time (respecting char boundaries) — the
            // worst case for a parser that commits too eagerly.
            for end in 0..=source.len() {
                if !source.is_char_boundary(end) {
                    continue;
                }
                let prefix = &source[..end];
                let streamed = parse(prefix);

                // A cold parser is the reference: no memo, no resumption.
                let mut cold = ParseState::default();
                cold.commit_complete_lines(prefix);
                let reference = cold.snapshot(prefix);

                assert_eq!(
                    streamed, reference,
                    "prefix {end} of {source:?} diverged from a full re-parse"
                );
            }
        }
    }

    /// The performance guarantee: work per chunk must not grow with the
    /// message. Counted in lines actually classified, which is the quantity
    /// that was quadratic — the old code re-classified every line on every
    /// chunk.
    #[test]
    fn streaming_does_not_reclassify_committed_lines() {
        let chunk = "a line of prose\n";
        let chunks = 400;

        let mut content = String::new();
        let mut state = ParseState::default();
        let mut total_committed = 0usize;

        for _ in 0..chunks {
            content.push_str(chunk);
            assert!(
                state.can_resume_from(&content),
                "an append-only stream must always be resumable"
            );
            let before = state.blocks.len();
            state.commit_complete_lines(&content);
            total_committed += state.blocks.len() - before;
        }

        // Quadratic would be chunks * (chunks + 1) / 2 = 80,200 classifications.
        assert_eq!(
            total_committed, chunks,
            "each line must be classified exactly once across the whole stream"
        );
        assert_eq!(state.blocks.len(), chunks);
    }

    /// Resumption is verified, never assumed. Content that does not extend the
    /// committed prefix must fall back to a full parse rather than splice
    /// unrelated blocks together.
    #[test]
    fn a_changed_prefix_is_not_resumable() {
        let mut state = ParseState::default();
        state.commit_complete_lines("first line\nsecond line\n");

        assert!(state.can_resume_from("first line\nsecond line\nthird\n"));
        // Earlier bytes rewritten.
        assert!(!state.can_resume_from("FIRST line\nsecond line\nthird\n"));
        // Buffer shrank (a different, shorter cell).
        assert!(!state.can_resume_from("first line\n"));
        // Entirely unrelated content.
        assert!(!state.can_resume_from("something else\n"));
    }

    /// Interleaving two different sources through the shared memo must not
    /// contaminate either — this is the multi-cell render-loop case.
    #[test]
    fn interleaved_sources_do_not_contaminate_each_other() {
        let a = "# Alpha\n\nalpha body\n";
        let b = "```rust\nlet b = 1;\n```\n";
        for _ in 0..5 {
            assert_eq!(parse(a), reference_parse(a));
            assert_eq!(parse(b), reference_parse(b));
        }
    }

    fn reference_parse(content: &str) -> ParsedMarkdown {
        let mut cold = ParseState::default();
        cold.commit_complete_lines(content);
        cold.snapshot(content)
    }

    #[test]
    fn fenced_code_block_collected_in_parse() {
        let parsed = parse("text\n```rust\ncode line one\ncode line two\n```\nmore\n");
        let blocks = &parsed.blocks;
        // text paragraph, two code lines, more paragraph (fences are dropped)
        let code_lines: Vec<_> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Code {
                    line,
                    language,
                    block_id,
                } => Some((line.as_str(), language.as_deref(), *block_id)),
                _ => None,
            })
            .collect();
        assert_eq!(
            code_lines,
            vec![
                ("code line one", Some("rust"), 1),
                ("code line two", Some("rust"), 1),
            ]
        );
    }

    #[test]
    fn adjacent_code_fences_keep_distinct_highlighter_state() {
        let parsed = parse("```rust\n/* open\n```\n```rust\nlet x = 1;\n```\n");
        let ids = parsed
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Code { block_id, .. } => Some(*block_id),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn rust_fence_respects_terminal_color_depth_without_reserved_rgb() {
        let rendered = render_markdown_tagged(
            "```rust\nfn main() {\n    let answer: u32 = 42; // comment\n}\n```",
            100,
            Style::default(),
        );
        let colors = rendered
            .iter()
            .flat_map(|line| line.line.spans.iter())
            .filter_map(|span| span.style.fg)
            .collect::<std::collections::HashSet<_>>();
        if syntax_color_depth() == palette::ColorDepth::Monochrome {
            assert_eq!(colors, [Color::Reset].into(), "NO_COLOR: {colors:?}");
        } else {
            assert!(colors.len() > 1, "expected syntax colors, got: {colors:?}");
        }
        for color in colors {
            assert_ne!(color, palette::WHALE_HUMAN);
            assert_ne!(color, palette::WHALE_LIVE);
            assert_ne!(color, palette::WHALE_ACTION);
            assert_ne!(color, palette::WHALE_ERROR);
        }
    }

    #[test]
    fn syntax_colors_use_existing_depth_quantizer_and_grayscale_path() {
        assert!(matches!(
            syntax_rgb_to_terminal_color(
                120,
                80,
                200,
                palette::PaletteMode::Dark,
                palette::ColorDepth::Ansi256,
            ),
            Color::Indexed(_)
        ));
        assert!(matches!(
            syntax_rgb_to_terminal_color(
                120,
                80,
                200,
                palette::PaletteMode::Dark,
                palette::ColorDepth::Ansi16,
            ),
            Color::Black
                | Color::Red
                | Color::Green
                | Color::Yellow
                | Color::Blue
                | Color::Magenta
                | Color::Cyan
                | Color::Gray
                | Color::DarkGray
                | Color::LightRed
                | Color::LightGreen
                | Color::LightYellow
                | Color::LightBlue
                | Color::LightMagenta
                | Color::LightCyan
                | Color::White
        ));
        let gray = syntax_rgb_to_terminal_color(
            120,
            80,
            200,
            palette::PaletteMode::Grayscale,
            palette::ColorDepth::TrueColor,
        );
        assert!(matches!(gray, Color::Rgb(r, g, b) if r == g && g == b));
    }

    #[test]
    fn syntax_assets_are_lazy_singletons_and_explicit_modes_select_themes() {
        assert!(std::ptr::eq(syntax_set(), syntax_set()));
        assert!(std::ptr::eq(theme_set(), theme_set()));
        assert!(!std::ptr::eq(
            selected_syntax_theme(palette::PaletteMode::Dark),
            selected_syntax_theme(palette::PaletteMode::Light),
        ));
    }

    #[test]
    fn depth_quantization_cannot_reintroduce_reserved_semantic_colors() {
        let reserved = [
            palette::WHALE_HUMAN,
            palette::WHALE_LIVE,
            palette::WHALE_ACTION,
            palette::WHALE_ERROR,
        ];
        for depth in [
            palette::ColorDepth::TrueColor,
            palette::ColorDepth::Ansi256,
            palette::ColorDepth::Ansi16,
        ] {
            let reserved_at_depth = reserved.map(|color| palette::adapt_color(color, depth));
            for semantic in reserved {
                let Color::Rgb(r, g, b) = semantic else {
                    panic!("reserved syntax guard expects RGB semantic colors");
                };
                let syntax =
                    syntax_rgb_to_terminal_color(r, g, b, palette::PaletteMode::Dark, depth);
                assert!(
                    !reserved_at_depth.contains(&syntax),
                    "{syntax:?} reintroduced a reserved color at {depth:?}"
                );
            }
        }
    }

    #[test]
    fn code_block_indentation_is_preserved_in_render() {
        // Leading whitespace in code blocks is semantic — indented lines must
        // not be stripped to column zero when rendered.
        let md = "```\nfn main() {\n    println!(\"hi\");\n}\n```\n";
        let lines = render_markdown(md, 80, Style::default());
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        // The indented line must start with spaces (the 2-space code prefix
        // plus the 4-space source indentation).
        let indented = text
            .iter()
            .find(|t| t.contains("println"))
            .expect("should find println line");
        assert!(
            indented.starts_with("      "),
            "expected 6+ leading spaces (2 block prefix + 4 indent), got: {indented:?}"
        );
    }

    #[test]
    fn markdown_grapheme_width_uses_tab_stop_and_string_width() {
        // At column 0 a tab fills to column 8.
        assert_eq!(markdown_grapheme_width("\t", 0), 8);
        // At column 4 a tab fills to column 8 (4 remaining).
        assert_eq!(markdown_grapheme_width("\t", 4), 4);
        // At column 8 a tab fills to the next stop at 16 (8 columns).
        assert_eq!(markdown_grapheme_width("\t", 8), 8);
        // Regular ASCII is 1.
        assert_eq!(markdown_grapheme_width("a", 0), 1);
        // A fully-qualified keycap is one two-column grapheme.
        assert_eq!(markdown_grapheme_width("1\u{fe0f}\u{20e3}", 0), 2);
    }

    #[test]
    fn ordered_and_unordered_list_items_parse() {
        let parsed = parse("- alpha\n* beta\n1. gamma\n");
        let items: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::ListItem { bullet, text } => Some((bullet.as_str(), text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(items, vec![("-", "alpha"), ("-", "beta"), ("1.", "gamma")]);
    }

    #[test]
    fn blockquote_lines_parse_with_depth() {
        let parsed =
            parse("> hello\n>\n>> nested\n> > spaced\n>no-space\n>\t tabbed\nlone > arrow\n");
        let quotes: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Quote { depth, text } => Some((*depth, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            quotes,
            vec![
                (1, "hello"),
                (1, ""),
                (2, "nested"),
                (2, "spaced"),
                (1, "no-space"),
                (1, "tabbed"),
            ]
        );
        // `lone > arrow` starts with prose, so it stays a paragraph.
        assert_eq!(parsed.blocks.len(), 7);
    }

    #[test]
    fn code_fence_contains_quote_lines_untouched() {
        // Fenced-code lines are collected before blockquote classification, so
        // `>` inside a fence must stay literal code content.
        let parsed = parse("```\n> not a quote\n\n> but this is\n```\n");
        let code: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Code { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(code, vec!["> not a quote", "", "> but this is"]);
        assert!(
            parsed
                .blocks
                .iter()
                .all(|b| !matches!(b, Block::Quote { .. })),
            "lines inside a fence must stay code, never quotes"
        );
    }

    #[test]
    fn four_backtick_fence_keeps_shorter_fence_and_quotes_as_code() {
        // A ```` opener must not be closed by a shorter ``` line: the shorter
        // fence and any `>` lines after it stay code content (CommonMark
        // fence-length rule), and the block only closes on a fence >= opener.
        let parsed = parse("````\n```\n> still code\n`````\n> now a quote\n");
        let code: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Code { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(code, vec!["```", "> still code"]);
        let quotes: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Quote { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(quotes, vec!["now a quote"]);
    }

    #[test]
    fn longer_fence_closes_shorter_opener() {
        // CommonMark: a closing fence may be longer than the opener; only the
        // opener's length is the minimum.
        let parsed = parse("```\ncode\n````\nplain\n");
        let code: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Code { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(code, vec!["code"]);
        let paragraphs: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(paragraphs, vec!["plain"]);
    }

    #[test]
    fn backticks_with_info_do_not_close_an_open_fence() {
        let parsed = parse("```\n```rust\n> still code\n```\n");
        let code: Vec<_> = parsed
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::Code { line, .. } => Some(line.as_str()),
                _ => None,
            })
            .collect();

        assert_eq!(code, vec!["```rust", "> still code"]);
        assert!(
            parsed
                .blocks
                .iter()
                .all(|block| !matches!(block, Block::Quote { .. }))
        );
    }

    #[test]
    fn blockquote_renders_rail_and_inline_formatting() {
        let rendered = render_markdown_tagged(
            "> **bold** `code` and see https://example.com",
            80,
            Style::default(),
        );
        assert_eq!(
            tagged_visible(&rendered),
            vec!["│ bold code and see https://example.com"]
        );
        let spans = &rendered[0].line.spans;
        assert_eq!(spans[0].content, "│ ");
        assert!(
            spans
                .iter()
                .any(|span| span.content == "bold"
                    && span.style.add_modifier.contains(Modifier::BOLD)),
            "inline bold must survive inside a quote"
        );
        assert!(
            spans
                .iter()
                .any(|span| span.content == "code"
                    && span.style.bg == Some(palette::SURFACE_ELEVATED)),
            "inline code is styled distinctly from plain text"
        );
        // First-row rail is stripped by the transcript rail-scan along with
        // the assistant glyph, so copy must report 0 here (mirror list items)
        // — reporting the rail width would strip it twice and lose text.
        assert_eq!(rendered[0].copy_prefix_width, 0);
        assert_eq!(
            rendered[0].links,
            vec![osc8::LineLink {
                col_start: 20,
                col_end: 38,
                target: "https://example.com".to_string(),
            }]
        );
    }

    #[test]
    fn nested_blockquote_renders_multiple_rails_capped() {
        let rendered =
            render_markdown_tagged(">>> deep\n>>>>>>>>>> too deep\n", 80, Style::default());
        assert_eq!(
            tagged_visible(&rendered),
            vec!["│ │ │ deep", "│ │ │ │ too deep"]
        );
        assert_eq!(
            rendered[0]
                .line
                .spans
                .iter()
                .take(3)
                .map(|span| span.content.as_ref())
                .collect::<Vec<_>>(),
            vec!["│ ", "│ ", "│ "],
            "each rail must remain independently discoverable by selection copy"
        );
    }

    #[test]
    fn blockquote_wraps_with_continuation_rail_indent() {
        let source = "> alpha beta gamma delta epsilon zeta";
        let rendered = render_markdown_tagged(source, 12, Style::default());
        let visible = tagged_visible(&rendered);
        assert!(visible.len() > 1, "fixture must wrap: {visible:?}");
        assert!(visible[0].starts_with("│ "), "first row starts with rail");
        // Copy-prefix accounting: first row reports 0 (rail-scan strips it),
        // continuation rows report the rail width (plain spaces are not
        // stripped by the rail-scan).
        assert_eq!(rendered[0].copy_prefix_width, 0);
        for row in rendered.iter().skip(1) {
            assert_eq!(row.copy_prefix_width, 2);
        }
        for row in &visible[1..] {
            assert!(
                row.starts_with("  "),
                "continuation rows keep the rail width indent: {row:?}"
            );
            assert!(
                !row.starts_with('│'),
                "rail appears only on the first row: {row:?}"
            );
        }
        for width in rendered.iter().map(|row| {
            row.line
                .spans
                .iter()
                .map(|span| span.content.as_ref().width())
                .sum::<usize>()
        }) {
            assert!(width <= 12, "rendered width {width} exceeds budget");
        }
        // Rows re-join into the quote content (rail and alignment stripped).
        let combined = visible
            .iter()
            .map(|row| row.trim_start_matches('│').trim_start())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(combined, &source[2..]);
    }

    fn tagged_visible(lines: &[RenderedMarkdownLine]) -> Vec<String> {
        lines
            .iter()
            .map(|rendered| {
                rendered
                    .line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn http_links_keep_visible_text_and_out_of_band_metadata() {
        let source = "see https://example.com for details";
        let rendered = render_markdown_tagged(source, 80, Style::default());
        assert_eq!(tagged_visible(&rendered), vec![source]);
        assert!(
            rendered
                .iter()
                .flat_map(|line| &line.line.spans)
                .all(|span| { !span.content.contains('\x1b') && !span.content.contains("]8;;") }),
            "escape payloads must never enter visible spans"
        );
        assert_eq!(
            rendered[0].links,
            vec![osc8::LineLink {
                col_start: 4,
                col_end: 22,
                target: "https://example.com".to_string(),
            }]
        );
    }

    #[test]
    fn bare_http_links_exclude_surrounding_punctuation_from_target() {
        let source = "see (https://example.com/path).";
        let rendered = render_markdown_tagged(source, 80, Style::default());
        assert_eq!(tagged_visible(&rendered), vec![source]);
        assert_eq!(rendered[0].links.len(), 1);
        let link = &rendered[0].links[0];
        assert_eq!(link.target, "https://example.com/path");
        assert_eq!(link.col_start, 5);
        assert_eq!(link.col_end, 28);
    }

    #[test]
    fn bare_http_links_preserve_balanced_parentheses_in_target() {
        let url = "https://en.wikipedia.org/wiki/Function_(mathematics)";
        let source = format!("see {url}.");
        let rendered = render_markdown_tagged(&source, 100, Style::default());
        assert_eq!(tagged_visible(&rendered), vec![source]);
        assert_eq!(rendered[0].links.len(), 1);
        assert_eq!(rendered[0].links[0].target, url);
    }

    #[test]
    fn wrapped_url_chunks_keep_visible_label_and_full_target() {
        let url = "https://raw.githubusercontent.com/Hmbown/deepseek-skills/main/index.json";
        let rendered = render_markdown_tagged(url, 34, Style::default());
        let visible = tagged_visible(&rendered);
        assert!(visible.len() > 1, "fixture must wrap: {visible:?}");
        assert_eq!(visible.concat(), url);
        for (line, text) in rendered.iter().zip(&visible) {
            assert_eq!(line.links.len(), 1, "each wrapped chunk is linked");
            assert_eq!(line.links[0].target, url);
            assert_eq!(line.links[0].col_start, 0);
            assert_eq!(line.links[0].col_end, text.width().saturating_sub(1));
            assert!(!text.contains('\x1b') && !text.contains("]8;;"));
        }
    }

    #[test]
    fn named_link_shows_only_label_and_keeps_target_in_metadata() {
        let rendered = render_markdown_tagged(
            "read [the docs](https://example.com/guide) now",
            80,
            Style::default(),
        );
        assert_eq!(tagged_visible(&rendered), vec!["read the docs now"]);
        assert_eq!(
            rendered[0].links,
            vec![osc8::LineLink {
                col_start: 5,
                col_end: 12,
                target: "https://example.com/guide".to_string(),
            }]
        );
    }

    #[test]
    fn named_links_reject_non_web_schemes_and_normalize_http_scheme() {
        let unsafe_link = render_markdown_tagged("[run](javascript:alert)", 80, Style::default());
        assert_eq!(tagged_visible(&unsafe_link), vec!["run"]);
        assert!(unsafe_link.iter().all(|line| line.links.is_empty()));

        let web_link =
            render_markdown_tagged("[docs](HTTPS://example.com/guide)", 80, Style::default());
        assert_eq!(tagged_visible(&web_link), vec!["docs"]);
        assert_eq!(web_link[0].links[0].target, "https://example.com/guide");
    }

    #[test]
    fn named_links_target_absolute_paths_with_the_file_scheme() {
        let rendered = render_markdown_tagged(
            "edit [main.rs](/repo/src/main.rs) now",
            80,
            Style::default(),
        );
        assert_eq!(tagged_visible(&rendered), vec!["edit main.rs now"]);
        assert_eq!(
            rendered[0].links,
            vec![osc8::LineLink {
                col_start: 5,
                col_end: 11,
                target: "file:///repo/src/main.rs".to_string(),
            }]
        );

        let explicit =
            render_markdown_tagged("[main.rs](FILE:///repo/src/main.rs)", 80, Style::default());
        assert_eq!(explicit[0].links[0].target, "file:///repo/src/main.rs");
    }

    #[test]
    fn named_links_reject_relative_paths_and_smuggled_control_bytes() {
        // Nothing here resolves a relative path against a workspace root, so a
        // relative destination would link to whatever the terminal's cwd is.
        let relative = render_markdown_tagged("[main.rs](src/main.rs)", 80, Style::default());
        assert_eq!(tagged_visible(&relative), vec!["main.rs"]);
        assert!(relative.iter().all(|line| line.links.is_empty()));

        let hostile = render_markdown_tagged(
            "[log](/tmp/a\x07b\x1b]8;;https://evil.test\x1b\\c)",
            80,
            Style::default(),
        );
        assert!(
            hostile.iter().all(|line| line.links.is_empty()),
            "control bytes must not reach a link target: {hostile:?}"
        );

        // A `file://host/share` destination is a remote reference, not a local
        // path, and must not be rewritten into one.
        let host = render_markdown_tagged("[share](file://evil.test/etc)", 80, Style::default());
        assert!(host.iter().all(|line| line.links.is_empty()));
    }

    #[test]
    fn bare_paths_in_prose_are_never_linkified() {
        let rendered = render_markdown_tagged(
            "the fix landed in /repo/src/main.rs today",
            80,
            Style::default(),
        );
        assert!(rendered.iter().all(|line| line.links.is_empty()));
    }

    #[test]
    fn table_separator_row_is_kept() {
        // Separator rows are now kept as TableSeparator blocks so the
        // renderer can draw horizontal rules at the correct positions.
        let src = "| 项目属性 | 详情 |\n|----------|------|\n| **语言** | Rust 1.88+ |\n";
        let parsed = parse(src);
        let blocks: Vec<_> = parsed.blocks.iter().collect();
        // Should have 2 TableRow blocks (header + data) + 1 TableSeparator
        let table_rows: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b, Block::TableRow(_)))
            .collect();
        assert_eq!(table_rows.len(), 2, "expected 2 table rows: {blocks:?}");
        let separators: Vec<_> = blocks
            .iter()
            .filter(|b| matches!(b, Block::TableSeparator))
            .collect();
        assert_eq!(
            separators.len(),
            1,
            "expected 1 table separator: {blocks:?}"
        );
    }

    #[test]
    fn bold_markers_stripped_in_render() {
        let src = "这是一个 **Rust 工作区项目**，包含多个 crate。\n";
        let lines = render_markdown(src, 80, Style::default());
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        assert!(
            !text.contains("**"),
            "bold markers leaked into output: {text:?}"
        );
        assert!(text.contains("Rust"), "bold content missing: {text:?}");
    }

    #[test]
    fn table_renders_with_box_drawing_borders() {
        let src = "| 文件 | 改动 |\n|---|---|\n| foo.rs | 重写 |\n";
        let lines = render_markdown(src, 60, Style::default());
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        // Column pipes still present
        assert!(text.contains('│'), "table pipe separator missing: {text:?}");
        // Separator row rendered as middle border, not raw markdown
        assert!(
            !text.contains("|---|"),
            "raw separator row leaked: {text:?}"
        );
        // Top and bottom borders present
        assert!(
            text.contains('\u{250C}'),
            "top-left corner missing: {text:?}"
        );
        assert!(
            text.contains('\u{2510}'),
            "top-right corner missing: {text:?}"
        );
        assert!(
            text.contains('\u{2514}'),
            "bottom-left corner missing: {text:?}"
        );
        assert!(
            text.contains('\u{2518}'),
            "bottom-right corner missing: {text:?}"
        );
        // Middle separator present (at the |---|---| position)
        assert!(
            text.contains('\u{251C}'),
            "middle-left junction missing: {text:?}"
        );
        assert!(
            text.contains('\u{2524}'),
            "middle-right junction missing: {text:?}"
        );
    }

    #[test]
    fn table_pipes_inside_inline_code_stay_in_the_cell() {
        let src = "| Check | Result |\n\
                   |---|---|\n\
                   | `strings ~/.cargo/bin/codewhale-tui | grep -c \"legacy marker\"` | 0 matches |\n";
        let parsed = parse(src);

        let rows: Vec<&Vec<String>> = parsed
            .blocks
            .iter()
            .filter_map(|block| match block {
                Block::TableRow(cells) => Some(cells),
                _ => None,
            })
            .collect();

        assert_eq!(rows.len(), 2, "expected header + data row: {rows:?}");
        assert_eq!(
            rows[1],
            &vec![
                "`strings ~/.cargo/bin/codewhale-tui | grep -c \"legacy marker\"`".to_string(),
                "0 matches".to_string(),
            ]
        );

        let rendered_lines = visible_lines(&render_markdown(src, 200, Style::default()));
        let rendered = rendered_lines.join("\n");
        assert!(
            rendered.contains("grep -c"),
            "inline-code command was lost: {rendered}"
        );
        let data_line = rendered_lines
            .iter()
            .find(|line| line.contains("strings ~/.cargo/bin/codewhale-tui"))
            .expect("data row should render");
        assert_eq!(
            data_line.matches('│').count(),
            3,
            "two-column table row should have left, middle, and right separators: {data_line:?}"
        );
    }

    /// Cells longer than the per-column width must word-wrap to multiple
    /// lines instead of getting truncated with `…`. Truncation silently
    /// drops content the user can never see — particularly bad in narrow
    /// Windows terminals or with verbose English/Chinese instructional
    /// tables (the common LLM-output case).
    #[test]
    fn table_cell_wider_than_column_wraps_instead_of_truncating() {
        let src = "| Feature | How to verify |\n\
                   |---|---|\n\
                   | Workspace-local commands | Drop a .deepseek/commands/foo.md in any project, run deepseek from there, type /foo — should dispatch |\n";
        let lines = render_markdown(src, 80, Style::default());
        let combined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        assert!(
            !combined.contains('…'),
            "table cell was truncated with `…` instead of wrapping; got: {combined:?}"
        );
        assert!(
            combined.contains("type /foo"),
            "tail of long cell was lost; got: {combined:?}"
        );
        assert!(
            combined.contains("Workspace-local commands"),
            "short cell content lost; got: {combined:?}"
        );
    }

    /// Wrapped table rows must keep column separators on every visual
    /// line so the columns remain visually aligned across all wrapped
    /// segments. A wrapped row's continuation lines should still show
    /// the `│` separator pipes at the same column positions.
    #[test]
    fn wrapped_table_row_preserves_column_separators() {
        let src = "| A | B |\n\
                   |---|---|\n\
                   | short | this is a very very long second cell that absolutely must wrap to a new visual line because it cannot fit in the column allocated to it at this terminal width |\n";
        let lines = render_markdown(src, 60, Style::default());
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect();

        // Every line in the rendered table — including wrapped continuation
        // lines — must show the pipe column separator. We identify table
        // body lines as ones that start with the row separator `│`.
        let body_lines: Vec<&String> = rendered.iter().filter(|s| s.starts_with('│')).collect();

        assert!(
            body_lines.len() >= 3,
            "expected at least header + multi-line data row (3+ body lines), got {}: {:?}",
            body_lines.len(),
            body_lines
        );

        for line in &body_lines {
            assert!(
                line.matches('│').count() >= 3,
                "every wrapped table line should have N+1 column separators \
                 for N columns; got fewer in: {line:?}"
            );
        }

        // All of the long cell's content must appear across the wrapped lines.
        let combined: String = rendered.join("\n");
        for fragment in ["this is a very very long", "must wrap", "terminal width"] {
            assert!(
                combined.contains(fragment),
                "fragment {fragment:?} missing from wrapped output:\n{combined}"
            );
        }
    }

    // ─── Paragraph wrap regression suite (#1344, #1351) ────────────────────
    //
    // The bug: paragraph wrap (render_line_with_links) is word-based. A
    // single word wider than the available width was placed alone on a line
    // and silently overflowed the right edge of the transcript. Long URLs /
    // paths / hashes / no-whitespace CJK runs all hit this. The fix
    // hard-breaks overlong words at grapheme boundaries; these tests pin
    // that across widths 40/60/80/120.

    fn rendered_widths(rendered: &[Line<'static>]) -> Vec<usize> {
        rendered
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref().width())
                    .sum::<usize>()
            })
            .collect()
    }

    fn render_paragraph_for_test(text: &str, width: usize) -> Vec<Line<'static>> {
        render_line_with_links(text, width, Style::default(), Style::default())
    }

    #[test]
    fn paragraph_wrap_breaks_overlong_word_at_width_40() {
        // 200-char no-whitespace token must not exceed the 40-col window.
        let long = "a".repeat(200);
        let rendered = render_paragraph_for_test(&long, 40);
        for w in rendered_widths(&rendered) {
            assert!(w <= 40, "rendered width {w} exceeds 40-col window");
        }
        // And the full content must still be present across the wrapped lines.
        let combined: String = rendered
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert_eq!(combined.matches('a').count(), 200);
    }

    #[test]
    fn paragraph_wrap_breaks_no_whitespace_cjk_at_width_40() {
        // #963: long CJK runs without whitespace must wrap by display width
        // instead of overflowing or truncating. Each Han character is 2 cols.
        let long = "界".repeat(300);
        let rendered = render_paragraph_for_test(&long, 40);
        for w in rendered_widths(&rendered) {
            assert!(w <= 40, "rendered width {w} exceeds 40-col window");
        }
        let combined: String = rendered
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert_eq!(combined.chars().filter(|&ch| ch == '界').count(), 300);
        assert!(
            rendered.len() >= 15,
            "300 double-width chars should wrap into many rows, got {}",
            rendered.len()
        );
    }

    #[test]
    fn paragraph_wrap_breaks_overlong_word_at_widths_60_80_120() {
        let long = format!("https://example.com/{}", "p".repeat(180));
        for &width in &[60usize, 80, 120] {
            let rendered = render_paragraph_for_test(&long, width);
            for w in rendered_widths(&rendered) {
                assert!(
                    w <= width,
                    "width={width}: rendered line width {w} exceeds budget"
                );
            }
            assert!(rendered.len() >= 2, "width={width}: expected wrap");
        }
    }

    #[test]
    fn paragraph_wrap_keeps_short_words_unbroken() {
        // Regression guard: short words must still be broken at whitespace,
        // not mid-word. Width 40, only short words, expect zero mid-word
        // breaks (each line reads as natural English).
        let text = "the quick brown fox jumps over the lazy dog and then it stops moving";
        let rendered = render_paragraph_for_test(text, 40);
        for line in &rendered {
            let s: String = line.spans.iter().map(|s| s.content.to_string()).collect();
            // Heuristic: trimmed line should not start with a partial word
            // (i.e. should start with a real English start) — every line in
            // this fixture starts with a word in our short list.
            let first = s.split_whitespace().next().unwrap_or("");
            assert!(
                [
                    "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "and", "then",
                    "it", "stops", "moving"
                ]
                .contains(&first),
                "line {s:?} appears to start with a partial word"
            );
        }
    }

    #[test]
    fn paragraph_wrap_mixed_short_and_overlong_word() {
        // The overlong word must wrap; the trailing short words must pack
        // onto subsequent lines. The combined content is preserved.
        let long = "x".repeat(150);
        let text = format!("intro {long} tail words go here");
        let rendered = render_paragraph_for_test(&text, 80);
        for w in rendered_widths(&rendered) {
            assert!(w <= 80, "rendered width {w} exceeds 80-col window");
        }
        let combined: String = rendered
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        for fragment in ["intro", "tail", "words", "go", "here"] {
            assert!(
                combined.contains(fragment),
                "fragment {fragment:?} missing from wrapped output:\n{combined}"
            );
        }
        assert_eq!(combined.matches('x').count(), 150);
    }

    #[test]
    fn wrap_cell_text_already_handled_long_words_remains_correct() {
        // Regression guard for the v0.8.25 table-cell fix. After consolidating
        // the char-break helper, wrap_cell_text must continue to handle
        // overlong cells. Pin the property: every wrapped segment fits
        // within the column width, and content is preserved.
        let long = "y".repeat(120);
        let segments = wrap_cell_text(&long, 30);
        for seg in &segments {
            assert!(seg.width() <= 30, "segment {seg:?} exceeds col 30");
        }
        let combined: String = segments.join("");
        assert_eq!(combined.matches('y').count(), 120);
    }

    #[test]
    fn paragraph_wrap_handles_zero_width_gracefully() {
        // Width 0 should not panic or hang; it returns the input as-is or
        // empty, but never produces a line wider than 0 (when 0 means "no
        // budget at all"). This pins the early-return path against future
        // regressions.
        let rendered = render_paragraph_for_test("hello world", 0);
        // Any output is acceptable (the path is degenerate); assert no panic.
        let _ = rendered;
    }

    fn rendered_text(rendered: &[Line<'static>]) -> String {
        rendered
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect()
    }

    fn assert_rendered_widths_fit(rendered: &[Line<'static>], width: usize, label: &str) {
        for line_width in rendered_widths(rendered) {
            assert!(
                line_width <= width,
                "{label} width={width}: rendered line width {line_width} exceeds budget"
            );
        }
    }

    // ── Unicode / CJK / emoji / combining-char width QA (#3488) ────────────

    #[test]
    fn paragraph_wrap_keeps_unicode_runs_within_qa_widths() {
        let cases = [
            ("cjk", "界".repeat(300)),
            ("emoji", "😀".repeat(200)),
            ("mixed-cjk-emoji", "界😀世🚀".repeat(90)),
        ];

        for (label, text) in cases {
            for &width in &[80usize, 100, 120] {
                let rendered = render_paragraph_for_test(&text, width);
                assert_rendered_widths_fit(&rendered, width, label);
                assert_eq!(
                    rendered_text(&rendered),
                    text,
                    "{label} width={width}: content changed while wrapping"
                );

                let min_lines = text.width().div_ceil(width);
                assert!(
                    rendered.len() >= min_lines,
                    "{label} width={width}: expected at least {min_lines} lines, got {}",
                    rendered.len()
                );
            }
        }
    }

    #[test]
    fn paragraph_wrap_preserves_mixed_unicode_and_ascii_fragments() {
        let cjk = "这是一个测试字符串".repeat(10); // 80 Han chars = 160 cols
        let emoji = "🚀".repeat(12);
        let text = format!("Note: {cjk} done {emoji}");

        for &width in &[80usize, 100, 120] {
            let rendered = render_paragraph_for_test(&text, width);
            assert_rendered_widths_fit(&rendered, width, "mixed unicode/ascii");

            let visible = visible_lines(&rendered).join("\n");
            for fragment in ["Note:", "测试", "done"] {
                assert!(
                    visible.contains(fragment),
                    "width={width}: fragment {fragment:?} missing from output:\n{visible}"
                );
            }
            assert_eq!(
                visible.matches('🚀').count(),
                12,
                "width={width}: emoji content lost"
            );
        }
    }

    #[test]
    fn table_render_keeps_cjk_cells_within_qa_widths() {
        let cjk = "界".repeat(80);
        let src = format!("| Name | Value |\n|---|---|\n| CJK | {cjk} |\n");

        for &width in &[80usize, 100, 120] {
            let rendered = render_markdown(&src, width as u16, Style::default());
            assert_rendered_widths_fit(&rendered, width, "table cjk");
            assert_eq!(
                rendered_text(&rendered).matches('界').count(),
                80,
                "width={width}: CJK table cell content lost"
            );
        }
    }

    #[test]
    fn paragraph_wrap_keeps_cjk_transcript_within_narrow_widths() {
        // The seed cases cover 80/100/120; narrow terminals (resize / small
        // panes) are the other half of #3488's terminal-width lane. A CJK
        // transcript paragraph must still wrap inside tiny windows without
        // overflowing the border or dropping content.
        let text = "实时输出结果显示正常".repeat(6); // 60 Han glyphs, 120 cols
        for &width in &[20usize, 40] {
            let rendered = render_paragraph_for_test(&text, width);
            assert_rendered_widths_fit(&rendered, width, "narrow cjk transcript");
            assert_eq!(
                rendered_text(&rendered),
                text,
                "width={width}: CJK transcript content changed while wrapping"
            );
        }
    }

    // A deliberate measurement probe, not an assertion: run it with
    // `--nocapture` to read the per-append cost. Printing is the whole point,
    // so the module-wide stdout ban is lifted here the same way
    // `core/engine/tests.rs` lifts it for its probes.
    #[allow(clippy::print_stdout)]
    #[test]
    fn probe_incremental_stream_cost() {
        // Faithful to one streaming message: the document grows a word at a
        // time and the render path re-parses and re-renders the whole visible
        // text on every tick. Reports the cost per 250 appends so growth with
        // message size is visible.
        use std::time::Instant;
        let words: Vec<&str> =
            "the quick brown fox jumps over a lazy dog and then keeps going for a while"
                .split(' ')
                .collect();
        let mut doc = String::new();
        let width = 100u16;
        let mut window_start = Instant::now();
        for i in 0..2000usize {
            doc.push_str(words[i % words.len()]);
            doc.push(' ');
            let parsed = parse(&doc);
            let lines = render_parsed(&parsed, width, Style::default());
            std::hint::black_box(&lines);
            if (i + 1) % 250 == 0 {
                let elapsed = window_start.elapsed();
                println!(
                    "PROBE bytes={} per_append={:?} total={:?}",
                    doc.len(),
                    elapsed / 250,
                    elapsed
                );
                window_start = Instant::now();
            }
        }
    }
}

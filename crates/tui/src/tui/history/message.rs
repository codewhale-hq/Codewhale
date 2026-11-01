//! User, assistant, and system message transcript rendering.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use codewhale_palette as palette;
use codewhale_tui_kit::markdown_render;
use codewhale_tui_kit::ui_text::CopyLineSeparator;

use super::{ASSISTANT_GLYPH, USER_GLYPH};

pub(crate) struct RenderedTranscriptLine {
    pub line: Line<'static>,
    pub links: Vec<codewhale_tui_kit::osc8::LineLink>,
    pub copy_prefix_width: usize,
    pub copy_separator_after: CopyLineSeparator,
}

pub(super) fn render_message(
    prefix: &str,
    label_style: Style,
    body_style: Style,
    content: &str,
    width: u16,
) -> Vec<Line<'static>> {
    render_message_with_copy_metadata(prefix, label_style, body_style, content, width)
        .into_iter()
        .map(|rendered| rendered.line)
        .collect()
}

pub(super) fn render_message_with_copy_metadata(
    prefix: &str,
    label_style: Style,
    body_style: Style,
    content: &str,
    width: u16,
) -> Vec<RenderedTranscriptLine> {
    render_message_with_copy_metadata_for_palette(
        prefix,
        label_style,
        body_style,
        content,
        width,
        markdown_render::detected_palette_mode(),
    )
}

pub(super) fn render_message_with_copy_metadata_for_palette(
    prefix: &str,
    label_style: Style,
    body_style: Style,
    content: &str,
    width: u16,
    palette_mode: palette::PaletteMode,
) -> Vec<RenderedTranscriptLine> {
    // An assistant cell whose content is entirely whitespace (e.g. a stray
    // newline streamed between reasoning and a tool call) would otherwise
    // render as a bare, orphaned role glyph floating on its own line — the
    // "blue dots with nothing after them" artifact. Render nothing so the
    // transcript doesn't accumulate empty markers. Real prose, including
    // messages that merely start with blank lines, still renders normally.
    if prefix == ASSISTANT_GLYPH && content.trim().is_empty() {
        return Vec::new();
    }
    let prefix_width = UnicodeWidthStr::width(prefix);
    let prefix_width_u16 = u16::try_from(prefix_width.saturating_add(2)).unwrap_or(u16::MAX);
    let content_width = usize::from(width.saturating_sub(prefix_width_u16).max(1));
    let mut lines = Vec::new();
    // Pre-process $...$ and $$...$$ LaTeX math expressions into Unicode
    let content_rendered = super::latex_render::render_latex_in_text(content);
    let rendered = markdown_render::render_markdown_tagged_with_palette(
        &content_rendered,
        content_width as u16,
        body_style,
        palette_mode,
    );
    for (idx, rendered_line) in rendered.into_iter().enumerate() {
        lines.push(decorate_markdown_line(
            prefix,
            prefix_width,
            label_style,
            idx,
            rendered_line,
        ));
    }
    if lines.is_empty() {
        lines.push(RenderedTranscriptLine {
            line: Line::from(""),
            links: Vec::new(),
            copy_prefix_width: 0,
            copy_separator_after: CopyLineSeparator::Newline,
        });
    }
    lines
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn update_streaming_message_render(
    cache: &mut markdown_render::IncrementalMarkdownRenderCache,
    content: &str,
    width: u16,
    label_style: Style,
    body_style: Style,
    palette_mode: palette::PaletteMode,
    verified_append: bool,
    lines: &mut Vec<Line<'static>>,
    links: &mut Vec<Vec<codewhale_tui_kit::osc8::LineLink>>,
    copy_separators: &mut Vec<CopyLineSeparator>,
    copy_prefix_widths: &mut Vec<usize>,
) -> usize {
    let prefix = ASSISTANT_GLYPH;
    let prefix_width = UnicodeWidthStr::width(prefix);
    let prefix_width_u16 = u16::try_from(prefix_width.saturating_add(2)).unwrap_or(u16::MAX);
    let content_width = width.saturating_sub(prefix_width_u16).max(1);
    // Pre-process $...$ and $$...$$ LaTeX math expressions into Unicode
    let content = super::latex_render::render_latex_in_text(content);
    let delta = cache.update(
        &content,
        content_width,
        body_style,
        palette_mode,
        verified_append,
    );

    let replace_from = delta.replace_from;
    lines.truncate(replace_from);
    links.truncate(replace_from);
    copy_separators.truncate(replace_from);
    copy_prefix_widths.truncate(replace_from);

    for rendered_line in delta.lines {
        let index = lines.len();
        let rendered =
            decorate_markdown_line(prefix, prefix_width, label_style, index, rendered_line);
        lines.push(rendered.line);
        links.push(rendered.links);
        copy_separators.push(rendered.copy_separator_after);
        copy_prefix_widths.push(rendered.copy_prefix_width);
    }
    replace_from
}

fn decorate_markdown_line(
    prefix: &str,
    prefix_width: usize,
    label_style: Style,
    index: usize,
    rendered_line: markdown_render::RenderedMarkdownLine,
) -> RenderedTranscriptLine {
    let display_prefix_width = if prefix.is_empty() {
        0
    } else {
        prefix_width + 1
    };
    let links = rendered_line
        .links
        .iter()
        .map(|link| link.shifted(display_prefix_width))
        .collect();
    let line = if index == 0 {
        let mut spans = Vec::new();
        if !prefix.is_empty() {
            spans.push(Span::styled(
                prefix.to_string(),
                label_style.add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" "));
        }
        spans.extend(rendered_line.line.spans);
        Line::from(spans)
    } else {
        let indent = if prefix.is_empty() {
            String::new()
        } else if rendered_line.is_code {
            " ".repeat(prefix_width + 1)
        } else {
            let mut s = String::with_capacity(prefix_width + 1);
            s.push('\u{258F}');
            s.extend(std::iter::repeat_n(' ', prefix_width));
            s
        };
        let rail_style = Style::default().fg(palette::TEXT_DIM);
        let mut spans = vec![Span::styled(indent, rail_style)];
        spans.extend(rendered_line.line.spans);
        Line::from(spans)
    };
    RenderedTranscriptLine {
        line,
        links,
        copy_prefix_width: rendered_line.copy_prefix_width
            + history_copy_prefix_width(prefix, prefix_width, rendered_line.is_code, index),
        copy_separator_after: rendered_line.copy_separator_after,
    }
}

fn history_copy_prefix_width(
    prefix: &str,
    prefix_width: usize,
    is_code: bool,
    line_index: usize,
) -> usize {
    if line_index > 0 && is_code && !prefix.is_empty() {
        prefix_width + 1
    } else {
        0
    }
}

pub(super) fn hard_break_copy_lines(lines: Vec<Line<'static>>) -> Vec<RenderedTranscriptLine> {
    lines
        .into_iter()
        .map(|line| RenderedTranscriptLine {
            line,
            links: Vec::new(),
            copy_prefix_width: 0,
            copy_separator_after: CopyLineSeparator::Newline,
        })
        .collect()
}

/// Render a plain-text user message: split on newlines, word-wrap each line,
/// preserve leading whitespace. No markdown interpretation (headings, lists,
/// code blocks, etc. are rendered as literal text).
pub(super) fn render_plain_message(
    prefix: &str,
    label_style: Style,
    body_style: Style,
    content: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let prefix_width = UnicodeWidthStr::width(prefix);
    let prefix_width_u16 = u16::try_from(prefix_width.saturating_add(2)).unwrap_or(u16::MAX);
    let content_width = width.saturating_sub(prefix_width_u16).max(1);
    // Pre-process $...$ and $$...$$ LaTeX math expressions into Unicode
    let content_rendered = super::latex_render::render_latex_in_text(content);
    let rendered = markdown_render::render_plain_text(&content_rendered, content_width, body_style);
    let mut lines = Vec::with_capacity(rendered.len());

    for (idx, line) in rendered.into_iter().enumerate() {
        if idx == 0 {
            let mut spans = Vec::new();
            if !prefix.is_empty() {
                spans.push(Span::styled(
                    prefix.to_string(),
                    label_style.add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::raw(" "));
            }
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        } else {
            let indent = if prefix.is_empty() {
                String::new()
            } else {
                let mut s = String::with_capacity(prefix_width + 1);
                s.push('\u{258F}');
                s.extend(std::iter::repeat_n(' ', prefix_width));
                s
            };
            let rail_style = Style::default().fg(palette::TEXT_DIM);
            let mut spans = vec![Span::styled(indent, rail_style)];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

/// A user turn is marked by its `▎` rail and the adaptive `USER_BODY` ink.
/// Only the newest one also carries the elevated-surface background, so the
/// eye lands on the turn in play; every older prompt renders on the bare
/// ground. (A fill behind *every* user row read as striping on every theme,
/// which is why the block is newest-only rather than per-role.)
pub(super) fn render_user_message(
    content: &str,
    width: u16,
    highlight: bool,
) -> Vec<Line<'static>> {
    let lines = render_plain_message(
        USER_GLYPH,
        user_label_style(),
        user_body_style(),
        content,
        width,
    );
    if !highlight {
        return lines;
    }
    let bg = palette::SURFACE_ELEVATED;
    lines
        .into_iter()
        .map(|mut line| {
            line.style = line.style.bg(bg);
            let target_width = usize::from(width);
            let line_width = line.width();
            if line_width < target_width {
                line.spans.push(Span::styled(
                    " ".repeat(target_width - line_width),
                    Style::default().bg(bg),
                ));
            }
            line
        })
        .collect()
}

pub(super) fn user_label_style() -> Style {
    Style::default().fg(palette::USER_BODY)
}

pub(super) fn user_body_style() -> Style {
    Style::default().fg(palette::USER_BODY)
}

/// Style for the assistant glyph (`●`). When the cell is streaming and
/// motion is allowed, the foreground pulses on a 2s cycle between 30% and
/// 100% brightness — the only deliberately animated element in a calm
/// transcript. When idle (or low_motion is on) it sits at the full DeepSeek
/// sky color so finished turns read as solid rather than dim.
pub(super) fn assistant_label_style_for(streaming: bool, low_motion: bool) -> Style {
    let color = if streaming && !low_motion {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        palette::pulse_brightness(palette::WHALE_ACTION, now_ms)
    } else {
        palette::WHALE_ACTION
    };
    Style::default().fg(color)
}

pub(super) fn system_label_style() -> Style {
    Style::default().fg(palette::TEXT_DIM)
}

pub(super) fn message_body_style() -> Style {
    Style::default().fg(palette::TEXT_PRIMARY)
}

pub(super) fn system_body_style() -> Style {
    Style::default().fg(palette::TEXT_MUTED).italic()
}

/// Double the bare backslashes in a machine-generated note.
///
/// A note names paths, and a Windows path is littered with `\`. The markdown
/// renderer honours CommonMark §2.4, where a backslash before ASCII punctuation
/// is an escape: `\.` yields `.`, so `C:\Users\asus\.codewhale` rendered as
/// `C:\Users\asus.codewhale` and the note pointed at a directory that does not
/// exist. The renderer is right — a producer that wants a literal backslash
/// escapes it — so plain note prose is escaped here, at the one place every note
/// passes through.
///
/// Doubling is the whole fix: `\\` renders as `\`.
///
/// Three regions are left exactly as written, because the renderer already
/// copies them verbatim and doubling inside one would *print* the extra
/// backslash:
///
/// - a fenced block, closed only by a run at least as long as its opener;
/// - a line carrying inline code (`` ` ``), a shape a note's third-party text
///   (MCP guidance, extension prompts) brings with it;
/// - a table row.
///
/// A line that is both prose and delimited keeps today's behavior rather than
/// trading one wrong backslash for another.
pub(super) fn escape_note_backslashes(content: &str) -> std::borrow::Cow<'_, str> {
    if !content.contains('\\') {
        return std::borrow::Cow::Borrowed(content);
    }
    let mut out = String::with_capacity(content.len() + 8);
    let mut fence_len: Option<usize> = None;
    for (index, line) in content.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let trimmed = line.trim_start();
        let run = trimmed.chars().take_while(|ch| *ch == '`').count();
        if let Some(open) = fence_len {
            // The renderer closes a fence only on a closing run at least as
            // long as the opener, so a shorter run inside one is code content.
            if run >= open && trimmed[run..].trim().is_empty() {
                fence_len = None;
            }
            out.push_str(line);
            continue;
        }
        if run >= 3 {
            fence_len = Some(run);
            out.push_str(line);
            continue;
        }
        if carries_verbatim_inline_markdown(line) {
            out.push_str(line);
            continue;
        }
        for ch in line.chars() {
            if ch == '\\' {
                out.push('\\');
            }
            out.push(ch);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Whether the renderer will copy this line's inline markup verbatim, so a
/// doubled backslash would reach the screen doubled.
fn carries_verbatim_inline_markdown(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 1 {
        return true;
    }
    line.contains('`')
}

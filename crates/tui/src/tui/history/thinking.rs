//! Rendering for reasoning/thinking transcript cells.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_tui_kit::markdown_render;

/// Reasoning header opener. Replaces the spinner glyph on thinking cells —
/// reasoning is a slow exhale, not a tool spin.
pub(super) const REASONING_OPENER: &str = "\u{2026}"; // …
/// Reasoning body left rail. Dashed (`╎`) instead of the solid `▏` block to
/// visually separate reasoning from message body and tool output.
pub(super) const REASONING_RAIL: &str = "\u{254E} "; // ╎ + space
/// Trailing-line cursor on streaming reasoning. Anchored to the live colour
/// so the user sees where new tokens land.
pub(super) const REASONING_CURSOR: &str = "\u{258E}"; // ▎

const THINKING_SUMMARY_LINE_LIMIT: usize = 4;
/// Non-calm completed preview; calm mode uses a header-only projection.
const THINKING_COMPLETED_PREVIEW_LINE_LIMIT: usize = 2;
const THINKING_STREAMING_PREVIEW_LINE_LIMIT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkingVisualState {
    Live,
    Done,
    Idle,
}

#[cfg(test)]
#[must_use]
pub fn extract_reasoning_summary(text: &str) -> Option<String> {
    extract_explicit_reasoning_summary(text).or_else(|| {
        let fallback = text.trim();
        if fallback.is_empty() {
            None
        } else {
            Some(fallback.to_string())
        }
    })
}

fn extract_explicit_reasoning_summary(text: &str) -> Option<String> {
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.to_lowercase().starts_with("summary") {
            let mut summary = String::new();
            if let Some((_, rest)) = trimmed.split_once(':')
                && !rest.trim().is_empty()
            {
                summary.push_str(rest.trim());
                summary.push('\n');
            }
            while let Some(next) = lines.peek() {
                let next_trimmed = next.trim();
                if next_trimmed.is_empty() {
                    break;
                }
                if next_trimmed.starts_with('#') || next_trimmed.starts_with("**") {
                    break;
                }
                summary.push_str(next_trimmed);
                summary.push('\n');
                lines.next();
            }
            let summary = summary.trim().to_string();
            return if summary.is_empty() {
                None
            } else {
                Some(summary)
            };
        }
    }
    None
}

pub(super) fn render_thinking(
    content: &str,
    width: u16,
    streaming: bool,
    duration_secs: Option<f32>,
    collapsed: bool,
    low_motion: bool,
) -> Vec<Line<'static>> {
    render_thinking_with_analysis(
        content,
        width,
        streaming,
        duration_secs,
        collapsed,
        low_motion,
        true,
    )
    .0
}

pub(crate) fn render_thinking_with_analysis(
    content: &str,
    width: u16,
    streaming: bool,
    duration_secs: Option<f32>,
    collapsed: bool,
    low_motion: bool,
    highlight: bool,
) -> (Vec<Line<'static>>, bool) {
    render_thinking_with_preview_limit(
        content,
        width,
        streaming,
        duration_secs,
        collapsed,
        low_motion,
        highlight,
        Locale::En,
        THINKING_COMPLETED_PREVIEW_LINE_LIMIT,
        markdown_render::detected_palette_mode(),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_thinking_with_preview_limit(
    content: &str,
    width: u16,
    streaming: bool,
    duration_secs: Option<f32>,
    collapsed: bool,
    low_motion: bool,
    highlight: bool,
    locale: Locale,
    completed_preview_lines: usize,
    palette_mode: palette::PaletteMode,
) -> (Vec<Line<'static>>, bool) {
    let state = thinking_visual_state(streaming, duration_secs);
    let style = thinking_style();
    // 12% reasoning surface tint over the app ink — the only deliberately
    // warm element in the transcript. Dropped on Ansi-16 terminals where the
    // tint would distort the named palette.
    let depth = cached_color_depth();
    let body_bg = palette::reasoning_surface_tint(depth);
    let body_style = match (highlight, body_bg) {
        (true, Some(bg)) => style.italic().bg(bg),
        (_, None) | (false, Some(_)) => style.italic(),
    };
    let mut lines = Vec::new();

    let label = if streaming {
        format!("{}…", tr(locale, MessageId::WhaleStateThinking))
    } else if let Some(duration) = duration_secs.filter(|duration| *duration >= 1.0) {
        tr(locale, MessageId::TranscriptThoughtFor).replace(
            "{duration}",
            &codewhale_command_contract::elapsed::format_elapsed_ms((duration * 1000.0) as u64),
        )
    } else {
        tr(locale, MessageId::TranscriptThought).into_owned()
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("{REASONING_OPENER} "),
            Style::default().fg(thinking_state_accent(state)),
        ),
        Span::styled(label, Style::default().fg(thinking_state_accent(state))),
    ]));

    let content_width = width.saturating_sub(3).max(1);
    // #6196: compute only the projection being shown. The previous order ran
    // the collapsed preview render first and threw it away whenever the body
    // was expanded — a full extra body render per streaming beat.
    let (rendered, expandable) = if collapsed {
        collapsed_thinking_body(
            content,
            width,
            streaming,
            body_style,
            completed_preview_lines,
            palette_mode,
        )
    } else if content.trim().is_empty() {
        (Vec::new(), false)
    } else {
        let body: Vec<Line<'static>> = markdown_render::render_markdown_tagged_with_palette(
            content,
            content_width,
            body_style,
            palette_mode,
        )
        .into_iter()
        .map(|line| line.line)
        .collect();
        // The collapse affordance mirrors the collapsed preview's "more than
        // the preview would show" test, derived from this single render
        // instead of rendering the preview too: streaming content outgrows
        // the streaming preview; settled content shows more than the
        // completed preview, or differs from its explicit summary.
        let expandable = if streaming {
            body.len() > THINKING_STREAMING_PREVIEW_LINE_LIMIT
        } else {
            extract_explicit_reasoning_summary(content).is_some_and(|summary| {
                summary.trim() != content.trim() || body.len() > THINKING_SUMMARY_LINE_LIMIT
            }) || body.len() > completed_preview_lines
        };
        (body, expandable)
    };

    let rail_style = Style::default().fg(thinking_state_accent(state));
    let cursor_style = Style::default().fg(palette::ACCENT_REASONING_LIVE);

    if rendered.is_empty() && streaming {
        let mut spans = vec![Span::styled(REASONING_RAIL.to_string(), rail_style)];
        spans.push(Span::styled("reasoning...", body_style.italic()));
        if !low_motion {
            spans.push(Span::styled(format!(" {REASONING_CURSOR}"), cursor_style));
        }
        lines.push(Line::from(spans));
    }

    let last_idx = rendered.len().saturating_sub(1);
    for (idx, line) in rendered.into_iter().enumerate() {
        let mut spans = vec![Span::styled(REASONING_RAIL.to_string(), rail_style)];
        spans.extend(line.spans);
        // Mark only the live tail; styling every line would churn the block.
        if streaming && !low_motion && idx == last_idx {
            spans.push(Span::styled(format!(" {REASONING_CURSOR}"), cursor_style));
        }
        lines.push(Line::from(spans));
    }

    if collapsed && expandable {
        // The header owns the reveal affordance: it never displaces a live
        // tail row, and settled calm reasoning remains a single line.
        lines[0]
            .spans
            .push(Span::styled(" ›", Style::default().fg(palette::TEXT_MUTED)));
    }

    (lines, expandable)
}

fn collapsed_thinking_body(
    content: &str,
    width: u16,
    streaming: bool,
    style: Style,
    completed_preview_lines: usize,
    palette_mode: palette::PaletteMode,
) -> (Vec<Line<'static>>, bool) {
    if !streaming && completed_preview_lines == 0 {
        return (Vec::new(), !content.trim().is_empty());
    }
    let (body_text, without_explicit_summary): (std::borrow::Cow<'_, str>, bool) = if streaming {
        // #861 RC4 / #1324: an in-flight block has no meaningful completed
        // summary. Render raw content; the limit below keeps its newest lines.
        (std::borrow::Cow::Borrowed(content), false)
    } else {
        match extract_explicit_reasoning_summary(content) {
            Some(summary) => (std::borrow::Cow::Owned(summary), false),
            None => (std::borrow::Cow::Borrowed(content), true),
        }
    };
    let limit = if streaming {
        THINKING_STREAMING_PREVIEW_LINE_LIMIT
    } else if without_explicit_summary {
        completed_preview_lines
    } else {
        THINKING_SUMMARY_LINE_LIMIT
    };
    // #6196: the streaming preview keeps only the newest `limit` rendered
    // lines, so rendering the whole body every beat made streaming cost grow
    // with message size. Render a self-contained tail of the source instead;
    // the settled (`!streaming`) path still renders everything once per
    // revision, which the transcript cache already amortizes.
    let render_source: &str = if streaming {
        streaming_preview_tail_source(&body_text, limit.saturating_add(1))
    } else {
        &body_text
    };
    // #4146/#4148 used to scrub snake_case here. That rule could not tell
    // CodeWhale identifiers from user identifiers: paths, env vars, and
    // module names became bare ellipses while the full body remained one
    // keypress away. Keep the default view readable; do not revive the scrub.
    let mut lines = if render_source.trim().is_empty() {
        Vec::new()
    } else {
        markdown_render::render_markdown_tagged_with_palette(
            render_source,
            width.saturating_sub(3).max(1),
            style,
            palette_mode,
        )
        .into_iter()
        .map(|line| line.line)
        .collect()
    };
    let truncated = lines.len() > limit;
    if truncated {
        if streaming {
            // Follow the live cursor: discard the head, not the newest lines.
            lines.drain(0..lines.len() - limit);
        } else {
            lines.truncate(limit);
        }
    }
    let meaningful = truncated || (!streaming && body_text.trim() != content.trim());
    (lines, meaningful)
}

/// A trailing slice of the streaming reasoning body that renders
/// independently of the lines above it (#6196).
///
/// The slice cannot start mid-construct: a cut inside a fenced code block
/// would re-classify its lines as paragraphs, and a cut inside a table group
/// would re-render it as a fresh table. One forward pass mirrors the parser's
/// own fence rule (`push_parsed_line`) to learn, per line boundary, whether
/// the boundary sits inside an open fence; the start is then moved back past
/// any construct it would split. Collecting `min_source_lines` complete
/// lines is enough for the caller's purposes: every source line renders to
/// one or more rows, so `limit + 1` source lines always yield more than
/// `limit` rendered rows and the "truncated" verdict survives.
fn streaming_preview_tail_source(body: &str, min_source_lines: usize) -> &str {
    // (byte offset, whether the boundary above this line is inside an open
    // fence). One entry per line; cheap next to the render it bounds.
    let mut lines: Vec<(usize, bool)> = Vec::new();
    let mut open_fence: Option<(char, usize)> = None;
    let mut offset = 0usize;
    for piece in body.split_inclusive('\n') {
        let raw_line = piece
            .strip_suffix('\n')
            .map_or(piece, |line| line.strip_suffix('\r').unwrap_or(line));
        lines.push((offset, open_fence.is_some()));
        let trimmed = raw_line.trim_start();
        let fence_char = if trimmed.starts_with('~') { '~' } else { '`' };
        let fence_len = trimmed.chars().take_while(|c| *c == fence_char).count();
        if fence_len >= 3 {
            match open_fence {
                Some((open_char, open_len))
                    if fence_char == open_char
                        && fence_len >= open_len
                        && trimmed[fence_len..].trim().is_empty() =>
                {
                    open_fence = None;
                }
                None => open_fence = Some((fence_char, fence_len)),
                Some(_) => {}
            }
        }
        offset += piece.len();
    }

    if lines.len() <= min_source_lines {
        // Fewer lines than the window needs: render the whole body.
        return body;
    }
    // Walk the desired start back to a boundary that splits no construct:
    // never inside an open fence (that boundary's line is code content) and
    // never mid-table (a table group is a run of `|`-prefixed lines).
    let mut index = lines.len() - min_source_lines;
    while index > 0 {
        let (line_offset, inside_before) = lines[index];
        let line = body[line_offset..].lines().next().unwrap_or("");
        if !inside_before && !line.trim_start().starts_with('|') {
            break;
        }
        index -= 1;
    }
    &body[lines[index].0..]
}

pub(super) fn render_hidden_thinking_activity(
    _width: u16,
    duration_secs: Option<f32>,
    low_motion: bool,
) -> Vec<Line<'static>> {
    let state = ThinkingVisualState::Live;
    let mut header_spans = vec![
        Span::styled(
            format!("{REASONING_OPENER} "),
            Style::default().fg(thinking_state_accent(state)),
        ),
        // A hidden live block needs one receipt, not stacked variants of the
        // same state ("reasoning live" plus "reasoning hidden; working").
        Span::styled("reasoning hidden", thinking_title_style()),
    ];
    if let Some(dur) = duration_secs {
        header_spans.push(Span::styled(" · ", Style::default().fg(palette::TEXT_DIM)));
        header_spans.push(Span::styled(
            codewhale_command_contract::elapsed::format_elapsed_ms((dur * 1000.0) as u64),
            thinking_meta_style(),
        ));
    }
    if !low_motion {
        header_spans.push(Span::styled(
            format!(" {REASONING_CURSOR}"),
            Style::default().fg(palette::ACCENT_REASONING_LIVE),
        ));
    }
    vec![Line::from(header_spans)]
}

fn thinking_style() -> Style {
    Style::default().fg(palette::TEXT_REASONING)
}

fn thinking_visual_state(streaming: bool, duration_secs: Option<f32>) -> ThinkingVisualState {
    if streaming {
        ThinkingVisualState::Live
    } else if duration_secs.is_some() {
        ThinkingVisualState::Done
    } else {
        ThinkingVisualState::Idle
    }
}

fn thinking_title_style() -> Style {
    Style::default()
        .fg(palette::TEXT_SOFT)
        .add_modifier(Modifier::BOLD)
}

fn thinking_meta_style() -> Style {
    Style::default().fg(palette::TEXT_DIM)
}

fn thinking_state_accent(state: ThinkingVisualState) -> Color {
    match state {
        ThinkingVisualState::Live => palette::ACCENT_REASONING_LIVE,
        ThinkingVisualState::Done => palette::TEXT_DIM,
        ThinkingVisualState::Idle => palette::TEXT_DIM,
    }
}

/// Once-initialised colour depth for the terminal session. Avoids re-reading
/// `COLORTERM` / `TERM` env vars on every frame.
static COLOR_DEPTH: std::sync::OnceLock<palette::ColorDepth> = std::sync::OnceLock::new();

pub(super) fn cached_color_depth() -> palette::ColorDepth {
    *COLOR_DEPTH.get_or_init(palette::ColorDepth::detect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined_text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn tail_source_extends_back_past_fences_and_tables() {
        // Short bodies render whole.
        assert_eq!(
            streaming_preview_tail_source("one\ntwo\n", 12),
            "one\ntwo\n"
        );

        let mut fenced = String::from("```rust\n");
        for i in 0..30 {
            fenced.push_str(&format!("let v{i} = {i};\n"));
        }
        fenced.push_str("```\nafter the block\n");
        // A 2-line window would start at the closing fence (which parses as
        // an opener when orphaned); the slice must start at the real fence
        // opener so the code lines keep their classification.
        let slice = streaming_preview_tail_source(&fenced, 2);
        assert!(slice.starts_with("```rust"));
        assert!(slice.ends_with("after the block\n"));

        // A table group must not be split either.
        let table = "intro\n| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
        assert_eq!(streaming_preview_tail_source(table, 2), table);

        // An unterminated fence owns everything after its opener; the slice
        // must extend back to the opener, not cut inside the block.
        let open = "```\ncode a\ncode b\ncode c\n";
        let slice = streaming_preview_tail_source(open, 2);
        assert!(slice.starts_with("```\ncode a"));
    }

    #[test]
    fn collapsed_streaming_preview_renders_only_the_newest_lines() {
        let mut body = String::new();
        for i in 0..40 {
            body.push_str(&format!("head marker {i}\n"));
        }
        for i in 0..20 {
            body.push_str(&format!("tail marker {i}\n"));
        }
        let (lines, expandable) = render_thinking_with_preview_limit(
            &body,
            100,
            true,
            None,
            true,
            false,
            false,
            Locale::En,
            THINKING_COMPLETED_PREVIEW_LINE_LIMIT,
            palette::PaletteMode::Dark,
        );
        assert!(expandable, "a long streaming body must offer expand");
        // header (including the affordance) + three preview rows
        assert_eq!(lines.len(), 1 + THINKING_STREAMING_PREVIEW_LINE_LIMIT);
        let text = joined_text(&lines);
        assert!(text.iter().any(|t| t.contains("tail marker 19")));
        assert!(text.iter().any(|t| t.contains("tail marker 17")));
        assert!(
            !text.iter().any(|t| t.contains("tail marker 16")),
            "the window must drop the head: {text:?}"
        );
        assert!(!text.iter().any(|t| t.contains("head marker 39")));
    }

    #[test]
    fn collapsed_streaming_preview_keeps_open_fence_classification() {
        let mut body = String::from("```\n");
        for i in 0..40 {
            body.push_str(&format!("code line {i}\n"));
        }
        let (lines, _) = render_thinking_with_preview_limit(
            &body,
            100,
            true,
            None,
            true,
            false,
            false,
            Locale::En,
            THINKING_COMPLETED_PREVIEW_LINE_LIMIT,
            palette::PaletteMode::Dark,
        );
        // Code rows carry the two-space code prefix after the rail; if the
        // tail slice started inside the open fence they would render as
        // paragraphs and lose it.
        let text = joined_text(&lines);
        let code_prefix = format!("{REASONING_RAIL}  ");
        assert!(
            text.iter().any(|t| t.starts_with(&code_prefix)),
            "visible code rows must keep the code prefix: {text:?}"
        );
    }

    #[test]
    fn expanded_streaming_thinking_parses_the_body_once() {
        // #6196: the expanded path used to run the collapsed preview render
        // first and throw it away — two full body renders per beat.
        let mut body = String::from("```\n");
        for i in 0..40 {
            body.push_str(&format!("expanded line {i}\n"));
        }
        markdown_render::reset_parse_invocation_count();
        let (lines, expandable) = render_thinking_with_preview_limit(
            &body,
            100,
            true,
            None,
            false,
            false,
            false,
            Locale::En,
            THINKING_COMPLETED_PREVIEW_LINE_LIMIT,
            palette::PaletteMode::Dark,
        );
        assert_eq!(
            markdown_render::parse_invocation_count(),
            1,
            "the expanded view must render the body exactly once"
        );
        assert!(expandable);
        assert!(lines.len() > THINKING_STREAMING_PREVIEW_LINE_LIMIT);
    }
}

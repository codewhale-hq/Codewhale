//! Preserved tool-output rendering and line selection.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use codewhale_palette as palette;

use super::constants::{
    TOOL_OUTPUT_HEAD_LINES, TOOL_OUTPUT_TAIL_LINES, TOOL_TEXT_LIMIT, TRANSCRIPT_RAIL,
};
use super::{
    RenderMode, details_affordance_line, looks_like_file_path, render_card_detail_line,
    render_card_detail_line_single, render_card_detail_line_single_styled,
    render_card_detail_line_styled, tool_value_style, truncate_text,
};

/// Columns the first output row gives up to its label: `result: ` or
/// `output: `. Both labels are the same width, which is what lets one cached
/// set of rows serve either card.
const FIRST_ROW_LABEL_COLUMNS: usize = "result: ".len();

pub(super) fn render_tool_output_mode(
    output: &str,
    width: u16,
    line_limit: usize,
    mode: RenderMode,
) -> Vec<Line<'static>> {
    render_preserved_output_mode(output, width, line_limit, mode, "result")
}

pub(super) fn render_exec_output_mode(
    output: &str,
    width: u16,
    line_limit: usize,
    mode: RenderMode,
) -> Vec<Line<'static>> {
    render_preserved_output_mode(output, width, line_limit, mode, "output")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputRow {
    pub text: String,
    pub intact: bool,
    /// SGR-styled segments of `text` when the source line carried colour
    /// (`cargo`, `git`, `gh` with colour forced on, anything run through a
    /// PTY). Concatenated they equal `text`; `None` for plain rows so the
    /// common path allocates nothing extra.
    pub styled: Option<Vec<StyledSegment>>,
}

/// One run of `OutputRow::text` with the style the tool's own SGR codes
/// asked for. Only what is painted keeps the colour: the model, the session
/// store, the pager, clipboard and exports still see stripped text.
pub type StyledSegment = (String, Style);

/// Heuristic: does the output look like a unified diff? Returns true when
/// the output contains at least one hunk header (`@@`) or a `diff --git`
/// line, which are reliable markers of unified diff content (#380).
pub(crate) fn output_looks_like_diff(output: &str) -> bool {
    let mut lines = output.lines();
    // Check first 5 lines for diff markers
    for _ in 0..5 {
        let Some(line) = lines.next() else { break };
        let trimmed = line.trim();
        if trimmed.starts_with("@@") || trimmed.starts_with("diff --git") {
            return true;
        }
    }
    false
}

fn summarize_string_value(text: &str, max_len: usize, count_only: bool) -> String {
    let trimmed = text.trim();
    let len = trimmed.chars().count();
    if count_only || len > max_len {
        return format!("<{len} chars>");
    }
    truncate_text(trimmed, max_len)
}

fn summarize_inline_value(value: &Value, max_len: usize, count_only: bool) -> String {
    match value {
        Value::String(s) => summarize_string_value(s, max_len, count_only),
        Value::Array(items) => format!("<{} items>", items.len()),
        Value::Object(map) => format!("<{} keys>", map.len()),
        Value::Bool(b) => b.to_string(),
        Value::Number(num) => num.to_string(),
        Value::Null => "null".to_string(),
    }
}

fn is_noisy_tool_arg_key(key: &str) -> bool {
    matches!(
        key,
        "limit"
            | "max_count"
            | "max_output_tokens"
            | "offset"
            | "page"
            | "page_size"
            | "per_page"
            | "response_length"
            | "timeout_ms"
            | "yield_time_ms"
    )
}

#[must_use]
pub fn summarize_tool_args(input: &Value) -> Option<String> {
    let obj = input.as_object()?;
    if obj.is_empty() {
        return None;
    }

    let mut parts = Vec::new();

    if let Some(value) = obj.get("path") {
        parts.push(format!(
            "path: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("command") {
        parts.push(format!(
            "command: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("query") {
        parts.push(format!(
            "query: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("prompt") {
        parts.push(format!(
            "prompt: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("text") {
        parts.push(format!(
            "text: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("pattern") {
        parts.push(format!(
            "pattern: {}",
            summarize_inline_value(value, 80, false)
        ));
    }
    if let Some(value) = obj.get("model") {
        parts.push(format!(
            "model: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("profile") {
        parts.push(format!(
            "profile: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("level") {
        parts.push(format!(
            "level: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("file_id") {
        parts.push(format!(
            "file_id: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("task_id") {
        parts.push(format!(
            "task_id: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("voice_id") {
        parts.push(format!(
            "voice_id: {}",
            summarize_inline_value(value, 40, false)
        ));
    }
    if let Some(value) = obj.get("content") {
        parts.push(format!(
            "content: {}",
            summarize_inline_value(value, 0, true)
        ));
    }

    if parts.is_empty()
        && let Some((key, value)) = obj
            .iter()
            .find(|(key, _)| !is_noisy_tool_arg_key(key.as_str()))
    {
        return Some(format!(
            "{}: {}",
            key,
            summarize_inline_value(value, 80, false)
        ));
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
}

#[must_use]
pub fn summarize_tool_output(output: &str) -> String {
    if let Ok(json) = serde_json::from_str::<Value>(output) {
        if let Some(obj) = json.as_object() {
            if let Some(error) = obj.get("error").or(obj.get("status_msg")) {
                return format!("Error: {}", summarize_inline_value(error, 120, false));
            }

            let mut parts = Vec::new();

            if let Some(status) = obj.get("status").and_then(|v| v.as_str()) {
                parts.push(format!("status: {status}"));
            }
            if let Some(message) = obj.get("message").and_then(|v| v.as_str()) {
                parts.push(truncate_text(message, TOOL_TEXT_LIMIT));
            }
            if let Some(task_id) = obj.get("task_id").and_then(|v| v.as_str()) {
                parts.push(format!("task_id: {task_id}"));
            }
            if let Some(file_id) = obj.get("file_id").and_then(|v| v.as_str()) {
                parts.push(format!("file_id: {file_id}"));
            }
            if let Some(url) = obj
                .get("file_url")
                .or_else(|| obj.get("url"))
                .and_then(|v| v.as_str())
            {
                parts.push(format!("url: {}", truncate_text(url, 120)));
            }
            if let Some(data) = obj.get("data") {
                parts.push(format!("data: {}", summarize_inline_value(data, 80, true)));
            }

            if !parts.is_empty() {
                return parts.join(" | ");
            }

            if let Some(content) = obj
                .get("content")
                .or(obj.get("result"))
                .or(obj.get("output"))
            {
                return summarize_inline_value(content, TOOL_TEXT_LIMIT, false);
            }
        }

        return summarize_inline_value(&json, TOOL_TEXT_LIMIT, true);
    }

    truncate_text(output, TOOL_TEXT_LIMIT)
}

/// Summary information extracted from an MCP tool output payload.
pub struct McpOutputSummary {
    pub content: Option<String>,
    pub is_image: bool,
    pub is_error: Option<bool>,
}

/// Summarize raw MCP output into UI-friendly content.
#[must_use]
pub fn summarize_mcp_output(output: &str) -> McpOutputSummary {
    if let Ok(json) = serde_json::from_str::<Value>(output) {
        let is_error = json
            .get("isError")
            .and_then(serde_json::Value::as_bool)
            .or_else(|| json.get("is_error").and_then(serde_json::Value::as_bool));

        if let Some(blocks) = json.get("content").and_then(|v| v.as_array()) {
            let mut lines = Vec::new();
            let mut is_image = false;

            for block in blocks {
                let block_type = block
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                match block_type {
                    "text" => {
                        let text = block.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if !text.is_empty() {
                            lines.push(format!("- text: {}", truncate_text(text, 200)));
                        }
                    }
                    "image" | "image_url" => {
                        is_image = true;
                        let url = block
                            .get("url")
                            .or_else(|| block.get("image_url"))
                            .and_then(|v| v.as_str());
                        if let Some(url) = url {
                            lines.push(format!("- image: {}", truncate_text(url, 200)));
                        } else {
                            lines.push("- image".to_string());
                        }
                    }
                    "resource" | "resource_link" => {
                        let uri = block
                            .get("uri")
                            .or_else(|| block.get("url"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("<resource>");
                        lines.push(format!("- resource: {}", truncate_text(uri, 200)));
                    }
                    other => {
                        lines.push(format!("- {other} content"));
                    }
                }
            }

            return McpOutputSummary {
                content: if lines.is_empty() {
                    None
                } else {
                    Some(lines.join("\n"))
                },
                is_image,
                is_error,
            };
        }
    }

    McpOutputSummary {
        content: Some(summarize_tool_output(output)),
        is_image: output_is_image(output),
        is_error: None,
    }
}

#[must_use]
pub fn output_is_image(output: &str) -> bool {
    // Sniff the extensions case-insensitively over the raw bytes. Lowercasing
    // the whole output first copied every byte of a payload that can run to
    // hundreds of kilobytes, once per MCP completion, to answer a question
    // about eight ASCII suffixes.
    const IMAGE_EXTENSIONS: [&str; 8] = [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tiff", ".ppm",
    ];

    let bytes = output.as_bytes();
    IMAGE_EXTENSIONS.iter().any(|ext| {
        let needle = ext.as_bytes();
        bytes
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    })
}

fn render_preserved_output_mode(
    output: &str,
    width: u16,
    line_limit: usize,
    mode: RenderMode,
    first_label: &str,
) -> Vec<Line<'static>> {
    debug_assert_eq!(first_label.len() + 2, FIRST_ROW_LABEL_COLUMNS);
    let mut lines = Vec::new();
    if output.trim().is_empty() {
        // #3031: In compact/Live mode, suppress "(no output)" — the tool
        // header already carries the success/failure status. Transcript
        // mode still records it for exports/clipboard/pager.
        if mode == RenderMode::Transcript {
            lines.push(Line::from(Span::styled(
                "  (no output)",
                Style::default().fg(palette::TEXT_MUTED).italic(),
            )));
        }
        return lines;
    }

    // Hash once; reuse for both the rows cache and the indices cache below.
    let content_hash = crate::tui::output_rows_cache::hash_str(output);
    let all_lines =
        crate::tui::output_rows_cache::get_or_compute_rows_with_hash(content_hash, width, || {
            output_rows(output, width)
        });

    if matches!(mode, RenderMode::Transcript) {
        // Full-content path: emit every wrapped line with no head/tail split,
        // no "+N more" affordance.
        for (idx, row) in all_lines.iter().enumerate() {
            render_output_row(
                &mut lines,
                if idx == 0 { Some(first_label) } else { None },
                row,
                width,
            );
        }
        return lines;
    }

    let selected = crate::tui::output_rows_cache::get_or_compute_indices(
        content_hash,
        width,
        line_limit,
        || selected_output_indices(&all_lines, line_limit),
    );
    // Blank rows are dropped without a marker, so the count is of rows that
    // carry text. Rows hidden after the last one shown are announced too: the
    // tail can stop short of the end when it keeps a result row instead. A gap
    // of exactly one row shows that row: a marker would take the same space.
    let emit_gap = |lines: &mut Vec<Line<'static>>, hidden: &[OutputRow]| {
        let mut text_rows = hidden.iter().filter(|row| !is_blank_row(row));
        let (Some(only), None) = (text_rows.next(), text_rows.next()) else {
            let omitted = hidden.iter().filter(|row| !is_blank_row(row)).count();
            if omitted > 0 {
                lines.push(details_affordance_line(
                    &format!(
                        "{omitted} lines omitted \u{00B7} {}",
                        crate::tui::key_shortcuts::tool_details_shortcut_action_hint("output")
                    ),
                    Style::default().fg(palette::TEXT_MUTED),
                ));
            }
            return;
        };
        render_output_row(lines, None, only, width);
    };
    let mut next = 0usize;
    for (rendered_idx, idx) in selected.iter().copied().enumerate() {
        emit_gap(&mut lines, &all_lines[next..idx]);

        let row = &all_lines[idx];
        render_output_row(
            &mut lines,
            if rendered_idx == 0 {
                Some(first_label)
            } else {
                None
            },
            row,
            width,
        );
        next = idx + 1;
    }
    emit_gap(&mut lines, &all_lines[next..]);

    lines
}

fn output_rows(output: &str, width: u16) -> Vec<OutputRow> {
    let wrap_width = width.saturating_sub(4).max(1) as usize;
    // The first row is painted after its label, so it has less room than the
    // rows under it. Wrapped at the full width it was wrapped a second time
    // when painted, which left the last word or two stranded on a row of
    // their own.
    let first_width = usize::from(width)
        .saturating_sub(UnicodeWidthStr::width(TRANSCRIPT_RAIL) + FIRST_ROW_LABEL_COLUMNS)
        .clamp(1, wrap_width);
    let mut rows = Vec::new();
    let mut sanitized = String::with_capacity(output.len());
    for line in output.lines() {
        sanitized.clear();
        crate::tui::osc8::strip_ansi_into(line, &mut sanitized);
        let styled = styled_segments(line, &sanitized);
        let intact = is_path_or_url_like(&sanitized);
        if intact {
            rows.push(OutputRow {
                text: sanitized.clone(),
                intact: true,
                styled,
            });
        } else {
            let parts = if rows.is_empty() {
                wrap_first_row(&sanitized, first_width, wrap_width)
            } else {
                wrap_text(&sanitized, wrap_width)
            };
            let mut styled_parts = styled.map(|segments| split_segments(&segments, &parts));
            for (idx, wrapped) in parts.into_iter().enumerate() {
                rows.push(OutputRow {
                    text: wrapped,
                    intact: false,
                    styled: styled_parts
                        .as_mut()
                        .map(|split| std::mem::take(&mut split[idx])),
                });
            }
        }
    }
    if rows.is_empty() {
        rows.push(OutputRow {
            text: String::new(),
            intact: false,
            styled: None,
        });
    }
    rows
}

/// Parse the SGR codes in `line` into styled segments whose text
/// concatenates to `plain` (the fully stripped line). Returns `None` when the
/// line carries no escape at all, when nothing in it sets a style, or when
/// the parse disagrees with the plain strip — the plain path is then the
/// truth, exactly as before.
fn styled_segments(line: &str, plain: &str) -> Option<Vec<StyledSegment>> {
    use ansi_to_tui::IntoText;

    if !line.contains('\x1b') {
        return None;
    }
    let mut kept = String::with_capacity(line.len());
    crate::tui::osc8::strip_ansi_keep_sgr_into(line, &mut kept);
    let text = kept.into_text().ok()?;
    let mut segments: Vec<StyledSegment> = Vec::new();
    for parsed in text.lines {
        for span in parsed.spans {
            if span.content.is_empty() {
                continue;
            }
            let style = tool_style(span.style);
            match segments.last_mut() {
                Some((last, last_style)) if *last_style == style => {
                    last.push_str(&span.content);
                }
                _ => segments.push((span.content.into_owned(), style)),
            }
        }
    }
    let round_trip: String = segments.iter().map(|(text, _)| text.as_str()).collect();
    if round_trip != plain || segments.iter().all(|(_, style)| *style == Style::default()) {
        return None;
    }
    Some(segments)
}

/// What the tool asked for, expressed relative to the cell's own ink. A
/// tool's *reset* (`ESC[0m`, or `Color::Reset`) means "back to the
/// terminal default", and in the transcript that default is the value style
/// the cell already paints — so resets become "nothing set" instead of a
/// `Style::reset()` that would wipe the cell's dim/state colour when
/// patched over it. Only positive requests (a colour, bold, underline)
/// survive.
fn tool_style(style: Style) -> Style {
    let keep = |colour: Option<ratatui::style::Color>| {
        colour.filter(|c| *c != ratatui::style::Color::Reset)
    };
    let mut out = Style::default().add_modifier(style.add_modifier);
    if let Some(fg) = keep(style.fg) {
        out = out.fg(fg);
    }
    if let Some(bg) = keep(style.bg) {
        out = out.bg(bg);
    }
    out
}

/// Re-split styled segments along the boundaries `wrap_text` chose, so each
/// wrapped part keeps the colours of the characters it holds. `parts` must
/// concatenate to the segments' text (which is how `wrap_text` splits).
pub(super) fn split_segments(
    segments: &[StyledSegment],
    parts: &[String],
) -> Vec<Vec<StyledSegment>> {
    let mut chars = segments
        .iter()
        .flat_map(|(text, style)| text.chars().map(move |ch| (ch, *style)));
    parts
        .iter()
        .map(|part| {
            let mut out: Vec<StyledSegment> = Vec::new();
            for (ch, style) in chars.by_ref().take(part.chars().count()) {
                match out.last_mut() {
                    Some((last, last_style)) if *last_style == style => last.push(ch),
                    _ => out.push((ch.to_string(), style)),
                }
            }
            out
        })
        .collect()
}

fn is_blank_row(row: &OutputRow) -> bool {
    row.text.trim().is_empty()
}

fn selected_output_indices(rows: &[OutputRow], line_limit: usize) -> Vec<usize> {
    if rows.len() <= line_limit || line_limit == 0 {
        return (0..rows.len()).collect();
    }

    // Once rows have to go, blank ones go first. Commands open and close
    // with blank lines (`npm test` starts with one, `cargo test` ends with
    // one), and a three-row preview that spends a row on one shows nothing.
    let visible: Vec<usize> = (0..rows.len())
        .filter(|&idx| !is_blank_row(&rows[idx]))
        .collect();
    if visible.len() <= line_limit {
        return visible;
    }

    // A tight preview gives the larger share to the tail: one opening row
    // says what ran, the end says how it went. The usual 20-row budget still
    // keeps 10 + 6.
    let head = TOOL_OUTPUT_HEAD_LINES.min(line_limit / 2).max(1);
    let tail = TOOL_OUTPUT_TAIL_LINES.min(line_limit.saturating_sub(head));
    let mut selected = std::collections::BTreeSet::new();
    selected.extend(visible[..head].iter().copied());
    selected.extend(tail_output_indices(rows, &visible[head..], tail));

    let budget = line_limit.saturating_sub(selected.len());
    if budget > 0 {
        let mut important: Vec<(usize, usize)> = visible[head..]
            .iter()
            .copied()
            .filter(|idx| !selected.contains(idx))
            .filter_map(|idx| output_importance_rank(&rows[idx].text).map(|rank| (idx, rank)))
            .collect();
        important.sort_by_key(|(idx, rank)| (*rank, *idx));
        for (idx, _) in important.into_iter().take(budget) {
            selected.insert(idx);
        }
    }

    // The importance pass only fires on lines that look like errors, warnings
    // or paths. Plain output — a list of names, a table, a build log with
    // nothing alarming in it — matches none of them, so the card used to show
    // `head + tail` rows and silently forfeit the rest of its budget. A
    // 20-line command then rendered 16 rows and claimed the other four were
    // "omitted". Spend whatever is left by growing the head downward, which
    // keeps the shown region contiguous and readable top-down.
    for &idx in &visible[head..] {
        if selected.len() >= line_limit {
            break;
        }
        selected.insert(idx);
    }

    selected.into_iter().collect()
}

/// Choose `tail` rows from the closing `TOOL_OUTPUT_TAIL_LINES` of
/// `candidates` (non-blank row indices, in order).
///
/// With the full tail budget that is simply the closing rows. A tight preview
/// has fewer slots than the closing block has rows, and a run's totals are
/// often not its very last lines (a timing line follows them), so rows that
/// state an outcome take the slots first and the last rows fill the rest.
fn tail_output_indices(rows: &[OutputRow], candidates: &[usize], tail: usize) -> Vec<usize> {
    let window_len = TOOL_OUTPUT_TAIL_LINES.max(tail).min(candidates.len());
    let window = &candidates[candidates.len() - window_len..];
    let mut picked: Vec<usize> = window
        .iter()
        .rev()
        .copied()
        .filter(|&idx| states_outcome(&rows[idx].text))
        .take(tail)
        .collect();
    for &idx in window.iter().rev() {
        if picked.len() >= tail {
            break;
        }
        if !picked.contains(&idx) {
            picked.push(idx);
        }
    }
    picked
}

/// Does the row say that something passed or failed? Plain words only, the
/// same way `output_importance_rank` spots errors — no tool's format is parsed.
fn states_outcome(line: &str) -> bool {
    const OUTCOME_WORDS: [&str; 10] = [
        "pass", "passed", "passes", "passing", "fail", "failed", "fails", "failing", "failure",
        "failures",
    ];
    line.split(|ch: char| !ch.is_ascii_alphabetic())
        .any(|word| {
            OUTCOME_WORDS
                .iter()
                .any(|outcome| word.eq_ignore_ascii_case(outcome))
        })
}

fn output_importance_rank(line: &str) -> Option<usize> {
    let lower = line.to_ascii_lowercase();
    if [
        "error",
        "failed",
        "failure",
        "fatal",
        "panic",
        "exception",
        "traceback",
        "denied",
        "not found",
        "no such file",
        "cannot",
        "can't",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return Some(0);
    }
    if lower.contains("warning") || lower.contains("warn") {
        return Some(1);
    }
    if is_path_or_url_like(line) {
        return Some(2);
    }
    None
}

fn is_path_or_url_like(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.contains("://") || trimmed.starts_with("file:") {
        return true;
    }
    let has_separator = trimmed.contains('/') || trimmed.contains('\\');
    let has_extension = trimmed
        .split_whitespace()
        .any(|part| part.rsplit_once('.').is_some_and(|(_, ext)| ext.len() <= 8));
    has_separator && has_extension
}

/// Detect whether a line contains a `path:line` pattern that could be
/// opened by `first_file_line_reference`. Returns a distinctive style
/// (underline + blue) when the pattern matches, or `None` otherwise.
/// The style is applied over the existing value style so the line
/// remains readable.
fn file_line_style(text: &str) -> Option<Style> {
    let trimmed = text.trim();
    if let Some((before, after)) = trimmed.rsplit_once(':')
        && !before.is_empty()
        && after.chars().all(|c| c.is_ascii_digit())
        && looks_like_file_path(before)
    {
        Some(
            Style::default()
                .fg(palette::WHALE_ACTION)
                .add_modifier(ratatui::style::Modifier::UNDERLINED),
        )
    } else {
        None
    }
}

/// Apply inline diff highlighting to a single text line.
///
/// Returns the appropriate style for the line based on its prefix:
/// - Lines starting with `+` (after trimming) => `palette::DIFF_ADDED` (green)
/// - Lines starting with `-` (after trimming) => `palette::STATUS_ERROR` (red)
/// - Lines starting with `@@` => `palette::WHALE_ACTION` (cyan/blue)
/// - All other lines => None (use default style)
fn diff_line_style(text: &str) -> Option<Style> {
    let trimmed = text.trim_start();
    if trimmed.starts_with("@@") {
        Some(Style::default().fg(palette::WHALE_ACTION))
    } else if trimmed.starts_with('+') && !trimmed.starts_with("+++") {
        Some(Style::default().fg(palette::DIFF_ADDED))
    } else if trimmed.starts_with('-') && !trimmed.starts_with("---") {
        Some(Style::default().fg(palette::STATUS_ERROR))
    } else {
        None
    }
}

fn render_output_row(
    lines: &mut Vec<Line<'static>>,
    label: Option<&str>,
    row: &OutputRow,
    width: u16,
) {
    // #374: apply file:line highlighting when the row text contains
    // a `path:line` pattern. Diff style takes precedence (colored
    // prefix lines should stay colored), but if no diff style matched,
    // check for a file:line pattern and highlight it distinctively.
    let diff_style = diff_line_style(&row.text);
    let file_style = file_line_style(&row.text);
    let value_style = diff_style.or(file_style).unwrap_or_else(tool_value_style);
    if let Some(segments) = &row.styled {
        if row.intact {
            lines.push(render_card_detail_line_single_styled(
                label,
                segments,
                value_style,
            ));
        } else {
            lines.extend(render_card_detail_line_styled(
                label,
                &row.text,
                segments,
                value_style,
                width,
            ));
        }
    } else if row.intact {
        lines.push(render_card_detail_line_single(
            label,
            &row.text,
            value_style,
        ));
    } else {
        lines.extend(render_card_detail_line(
            label,
            &row.text,
            value_style,
            width,
        ));
    }
}

pub(super) fn wrap_plain_line(line: &str, style: Style, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for part in wrap_text(line, width.max(1) as usize) {
        lines.push(Line::from(Span::styled(part, style)));
    }
    lines
}

/// [`wrap_text`] with a narrower first row. The parts still concatenate to
/// `text`, so the byte length of the first one is where the rest begins.
fn wrap_first_row(text: &str, first_width: usize, width: usize) -> Vec<String> {
    let mut parts = wrap_text(text, first_width);
    if parts.len() > 1 {
        let rest = wrap_text(&text[parts[0].len()..], width);
        parts.truncate(1);
        parts.extend(rest);
    }
    parts
}

/// Wrap `text` to `width` columns, ending each row between words when it
/// can. A word longer than a row is still split between graphemes.
///
/// Nothing is dropped or added: the rows concatenate back to `text`, with
/// the whitespace a row was broken at left on the end of that row. Tool
/// output keeps its column alignment, and `split_segments` can lay the
/// colours back over the rows by counting characters.
pub(super) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let mut current = String::new();

    for grapheme in text.graphemes(true) {
        let tentative = if current.is_empty() {
            grapheme.to_string()
        } else {
            let mut t = current.clone();
            t.push_str(grapheme);
            t
        };

        if UnicodeWidthStr::width(tentative.as_str()) > width && !current.is_empty() {
            let carried = last_word_start(&current, grapheme, width)
                .map(|start| current.split_off(start))
                .unwrap_or_default();
            lines.push(std::mem::replace(&mut current, carried));
        }

        current.push_str(grapheme);
    }

    lines.push(current);

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// Where the last word of a full `row` starts, when that word should move
/// down to the next row together with `next`, the grapheme that did not fit.
///
/// `None` means break right where the row ends: it already ends between
/// words, it is one unbroken word, everything before the word is
/// indentation, or the word would not fit on the next row either.
fn last_word_start(row: &str, next: &str, width: usize) -> Option<usize> {
    let is_space = |text: &str| text.chars().all(char::is_whitespace);
    let words = row.trim_end();
    if words.len() < row.len() && !is_space(next) {
        return None;
    }
    let (space, gap) = words
        .grapheme_indices(true)
        .rev()
        .find(|(_, grapheme)| is_space(grapheme))?;
    let start = space + gap.len();
    let fits = UnicodeWidthStr::width(&row[start..]) + UnicodeWidthStr::width(next) <= width;
    (fits && !row[..space].trim().is_empty()).then_some(start)
}

#[cfg(test)]
mod ansi_colour_tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    fn styled_text(rows: &[OutputRow]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.styled
                    .as_ref()
                    .map(|segments| segments.iter().map(|(t, _)| t.as_str()).collect())
                    .unwrap_or_default()
            })
            .collect()
    }

    #[test]
    fn plain_line_carries_no_styled_segments() {
        let rows = output_rows("   Compiling codewhale v0.9.12", 80);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "   Compiling codewhale v0.9.12");
        assert!(rows[0].styled.is_none());
    }

    #[test]
    fn cargo_style_line_keeps_its_green_bold_verb() {
        let line = "\x1b[1m\x1b[32m   Compiling\x1b[0m codewhale v0.9.12";
        let rows = output_rows(line, 80);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "   Compiling codewhale v0.9.12");
        let segments = rows[0]
            .styled
            .as_ref()
            .expect("coloured line keeps segments");
        assert_eq!(segments[0].0, "   Compiling");
        assert_eq!(segments[0].1.fg, Some(Color::Green));
        assert!(segments[0].1.add_modifier.contains(Modifier::BOLD));
        assert_eq!(segments[1].0, " codewhale v0.9.12");
        assert_eq!(segments[1].1, Style::default());
        assert_eq!(styled_text(&rows), vec![rows[0].text.clone()]);
    }

    #[test]
    fn osc8_link_is_stripped_while_its_sgr_colour_survives() {
        let line =
            "see \x1b]8;;https://example.com/x\x1b\\\x1b[31mthe docs\x1b[0m\x1b]8;;\x1b\\ now";
        let rows = output_rows(line, 80);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "see the docs now");
        let segments = rows[0]
            .styled
            .as_ref()
            .expect("SGR inside an OSC 8 wrapper");
        assert_eq!(segments[0], ("see ".to_string(), Style::default()));
        assert_eq!(segments[1].0, "the docs");
        assert_eq!(segments[1].1.fg, Some(Color::Red));
        assert_eq!(segments[2], (" now".to_string(), Style::default()));
    }

    #[test]
    fn wrapped_rows_split_the_colour_along_wrap_boundaries() {
        let line = format!("\x1b[33m{}\x1b[0m{}", "y".repeat(12), "p".repeat(8));
        // width 12 → the labelled first row holds 2 characters and the rows
        // under it 8: rows of 2/8/8/2.
        let rows = output_rows(&line, 12);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            styled_text(&rows),
            rows.iter().map(|r| r.text.clone()).collect::<Vec<_>>()
        );
        let third = rows[2].styled.as_ref().unwrap();
        assert_eq!(
            third[0],
            ("yy".to_string(), Style::default().fg(Color::Yellow))
        );
        assert_eq!(third[1], ("pppppp".to_string(), Style::default()));
    }

    #[test]
    fn painted_span_patches_tool_colour_over_the_cell_style() {
        let rows = output_rows("\x1b[31merror\x1b[0m: boom", 80);
        let mut lines = Vec::new();
        render_output_row(&mut lines, Some("output"), &rows[0], 80);
        assert_eq!(lines.len(), 1);
        let spans = &lines[0].spans;
        // rail, label, gap, "error", ": boom"
        assert_eq!(spans[3].content, "error");
        assert_eq!(spans[3].style.fg, Some(Color::Red));
        assert_eq!(spans[4].content, ": boom");
        assert_eq!(spans[4].style, tool_value_style());
        let plain: String = spans[3..].iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(plain, "error: boom");
    }

    #[test]
    fn coloured_intact_path_stays_on_one_line_with_the_plain_hitbox() {
        let path = "crates/tui/src/tui/history/tool_output.rs:812";
        let coloured = format!("\x1b[35m{path}\x1b[0m");
        let plain_rows = output_rows(path, 20);
        let styled_rows = output_rows(&coloured, 20);
        assert_eq!(plain_rows.len(), 1);
        assert_eq!(styled_rows.len(), 1);
        assert!(plain_rows[0].intact && styled_rows[0].intact);
        assert!(styled_rows[0].styled.is_some());

        let mut plain = Vec::new();
        render_output_row(&mut plain, Some("output"), &plain_rows[0], 20);
        let mut styled = Vec::new();
        render_output_row(&mut styled, Some("output"), &styled_rows[0], 20);
        assert_eq!(plain.len(), 1, "plain intact row is one line");
        assert_eq!(styled.len(), 1, "coloured intact row is one line too");
        let text = |line: &Line<'static>| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert_eq!(text(&styled[0]), text(&plain[0]));
        // Same prefix (rail + label + gap), so the click region is identical.
        assert_eq!(
            styled[0].spans[..3]
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>(),
            plain[0].spans[..3]
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
        );
        assert_eq!(styled[0].spans[3].style.fg, Some(Color::Magenta));
    }

    #[test]
    fn a_line_that_only_resets_stays_on_the_plain_path() {
        let rows = output_rows("done\x1b[0m", 80);
        assert_eq!(rows[0].text, "done");
        assert!(rows[0].styled.is_none());
    }

    #[test]
    fn image_sniffing_is_case_insensitive_over_the_raw_bytes() {
        assert!(output_is_image("saved to /tmp/Chart.PNG"));
        assert!(output_is_image("shot.jpeg"));
        assert!(output_is_image(".WEBP"));
        assert!(!output_is_image("no image here"));
        // The historical `contains` contract is preserved: a name that merely
        // embeds an extension still counts.
        assert!(output_is_image("weird.pngx"));
        // A byte-window scan must not panic on multi-byte text.
        assert!(!output_is_image("日本語のテキストのみ"));
        assert!(!output_is_image(""));
    }

    #[test]
    fn wrap_text_breaks_between_graphemes() {
        // An emoji-presentation heart and a keycap are one two-column
        // grapheme each, the way Ratatui counts cells.
        for text in ["ab\u{2764}\u{fe0f}cd", "ab1\u{fe0f}\u{20e3}cd"] {
            for width in 2..=6 {
                let lines = wrap_text(text, width);
                let rejoined: Vec<&str> =
                    lines.iter().flat_map(|line| line.graphemes(true)).collect();
                assert_eq!(
                    rejoined,
                    text.graphemes(true).collect::<Vec<_>>(),
                    "width {width} split a grapheme: {lines:?}"
                );
                for line in &lines {
                    assert!(line.width() <= width, "line {line:?} exceeds width {width}");
                }
            }
        }
    }

    #[test]
    fn wrap_text_ends_rows_between_words() {
        assert_eq!(
            wrap_text("Bring a refillable water bottle.", 22),
            ["Bring a refillable ", "water bottle."]
        );
        // A row that is exactly full still ends on a whole word.
        assert_eq!(wrap_text("aaa bbb ccc", 7), ["aaa ", "bbb ccc"]);
        // Only a word longer than the row is split, and indentation is not
        // mistaken for a place to break.
        assert_eq!(
            wrap_text("    averyverylongtoken tail", 10),
            ["    averyv", "erylongtok", "en tail"]
        );
        assert_eq!(
            wrap_text(&"y".repeat(20), 8),
            ["y".repeat(8), "y".repeat(8), "y".repeat(4)]
        );
    }

    #[test]
    fn wrap_text_keeps_every_character_and_stays_within_the_width() {
        for text in [
            "name      size   modified",
            "aaa   bbb",
            "word          x",
            "   ",
            "日本語のテキスト と english mixed 語語語語語語",
            "a \u{301}b c d e f",
            "tab\tseparated\tvalues",
        ] {
            for width in 1..=14 {
                let lines = wrap_text(text, width);
                assert_eq!(lines.concat(), text, "width {width} changed the text");
                for line in &lines {
                    assert!(
                        line.width() <= width || line.graphemes(true).count() == 1,
                        "line {line:?} exceeds width {width}"
                    );
                    assert_eq!(
                        wrap_text(line, width),
                        std::slice::from_ref(line),
                        "a wrapped row must not wrap again at width {width}"
                    );
                }
            }
        }
    }

    /// The tutorial frame that showed `refillable wate / r bott / le.`: one
    /// long MCP result line, wrapped once for the card and once more behind
    /// the `result:` label.
    #[test]
    fn long_result_line_wraps_between_words_and_only_once() {
        let output = "- text: Next shoreline cleanup: Saturday at 09:00, North Pier. \
                      Bring gloves and a refillable water bottle. (Demo data.)";
        for width in [60, 100, 110] {
            let lines = render_tool_output_mode(output, width, 20, RenderMode::Live);
            let rows: Vec<String> = lines
                .iter()
                .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect();
            assert_eq!(
                rows.len(),
                output_rows(output, width).len(),
                "painting wrapped a row a second time at width {width}: {rows:#?}"
            );
            let words: Vec<&str> = rows
                .iter()
                .flat_map(|row| row.split_whitespace())
                .filter(|word| *word != TRANSCRIPT_RAIL.trim() && *word != "result:")
                .collect();
            assert_eq!(
                words,
                output.split_whitespace().collect::<Vec<_>>(),
                "a word was split at width {width}: {rows:#?}"
            );
            for row in &rows {
                assert!(row.width() <= usize::from(width), "{row:?} exceeds {width}");
            }
        }
    }
}

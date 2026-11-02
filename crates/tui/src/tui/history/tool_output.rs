//! Preserved tool-output rendering and line selection.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;

use super::constants::{
    TOOL_OUTPUT_HEAD_LINES, TOOL_OUTPUT_TAIL_LINES, TOOL_TEXT_LIMIT, TRANSCRIPT_RAIL,
};
use super::{
    RenderMode, details_affordance_line, looks_like_file_path, render_card_detail_line_single,
    render_card_detail_line_single_styled, tool_value_style, truncate_text,
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
    locale: Locale,
) -> Vec<Line<'static>> {
    render_preserved_output_mode(output, width, line_limit, mode, Some("result"), locale)
}

pub(super) fn render_exec_output_mode(
    output: &str,
    width: u16,
    line_limit: usize,
    mode: RenderMode,
    locale: Locale,
    labeled: bool,
) -> Vec<Line<'static>> {
    render_preserved_output_mode(
        output,
        width,
        line_limit,
        mode,
        labeled.then_some("output"),
        locale,
    )
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

const OUTPUT_DISPLAY_ROWS: usize = 512;
const MAX_SELECTED_ROWS: usize = (OUTPUT_DISPLAY_ROWS - 1) / 2;
const MAX_ROW_BYTES: usize = 4 * 1024;
const MAX_STYLE_RUNS: usize = 64;
const MAX_STYLED_LINE_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputProjection {
    pub rows: Vec<OutputRow>,
    pub omitted_before: Vec<usize>,
    pub omitted_after: usize,
    pub total_rows: usize,
    pub total_nonblank: usize,
    pub shortened: bool,
}

impl OutputProjection {
    pub fn allocated_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>()
            .saturating_add(
                self.rows
                    .capacity()
                    .saturating_mul(std::mem::size_of::<OutputRow>()),
            )
            .saturating_add(
                self.omitted_before
                    .capacity()
                    .saturating_mul(std::mem::size_of::<usize>()),
            );
        for row in &self.rows {
            bytes = bytes.saturating_add(row.text.capacity());
            if let Some(segments) = &row.styled {
                bytes = bytes.saturating_add(
                    segments
                        .capacity()
                        .saturating_mul(std::mem::size_of::<StyledSegment>()),
                );
                for (text, _) in segments {
                    bytes = bytes.saturating_add(text.capacity());
                }
            }
        }
        bytes
    }
}

/// One run of `OutputRow::text` with the style the tool's own SGR codes
/// asked for. Only the painted preview uses these segments; source copying
/// retains the stored tool output independently.
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
    first_label: Option<&str>,
    locale: Locale,
) -> Vec<Line<'static>> {
    if let Some(label) = first_label {
        debug_assert_eq!(label.len() + 2, FIRST_ROW_LABEL_COLUMNS);
    }
    let mut lines = Vec::new();
    if mode == RenderMode::Source {
        lines.push(render_card_detail_line_single(
            first_label,
            output,
            tool_value_style(),
        ));
        return lines;
    }
    if output.trim().is_empty() {
        // #3031: In compact/Live mode, suppress "(no output)" — the tool
        // header already carries the success/failure status.
        if mode == RenderMode::Transcript {
            lines.push(Line::from(Span::styled(
                "  (no output)",
                Style::default().fg(palette::TEXT_MUTED).italic(),
            )));
        }
        return lines;
    }

    let transcript = mode == RenderMode::Transcript;
    let limit = if transcript || line_limit == 0 {
        MAX_SELECTED_ROWS
    } else {
        line_limit.min(MAX_SELECTED_ROWS).max(1)
    };
    let content_hash = crate::tui::output_rows_cache::hash_output(output);
    let projection = crate::tui::output_rows_cache::get_or_compute_rows_with_hash(
        content_hash,
        width,
        limit,
        transcript,
        || output_projection(output, width, limit),
    );
    let emit_gap = |lines: &mut Vec<Line<'static>>, omitted: usize| {
        if omitted > 0 {
            lines.push(details_affordance_line(
                &format!(
                    "{} · {}",
                    tr(locale, MessageId::ApprovalMoreLines)
                        .replace("{count}", &omitted.to_string())
                        .trim_start(),
                    crate::tui::key_shortcuts::tool_details_shortcut_action_hint(&tr(
                        locale,
                        MessageId::FooterHintOutput
                    ),)
                ),
                Style::default().fg(palette::TEXT_MUTED),
            ));
        }
    };
    for (index, row) in projection.rows.iter().enumerate() {
        emit_gap(&mut lines, projection.omitted_before[index]);
        render_output_row(
            &mut lines,
            if index == 0 { first_label } else { None },
            row,
            width,
        );
    }
    emit_gap(&mut lines, projection.omitted_after);
    if projection.shortened {
        lines.push(super::render_spillover_annotation(width, locale));
    }
    lines
}

fn output_projection(output: &str, width: u16, limit: usize) -> OutputProjection {
    use std::collections::{BTreeSet, BinaryHeap, VecDeque};
    let mut total = 0usize;
    let mut nonblank = 0usize;
    let mut first = Vec::with_capacity(limit);
    let mut fill = Vec::with_capacity(limit);
    let mut tail = VecDeque::with_capacity(TOOL_OUTPUT_TAIL_LINES);
    let mut important = BinaryHeap::with_capacity(limit + TOOL_OUTPUT_TAIL_LINES);
    let head = TOOL_OUTPUT_HEAD_LINES.min(limit / 2).max(1);
    let mut shortened = false;
    for_each_output_row(output, width, |row, clipped| {
        let index = total;
        total = total.saturating_add(1);
        shortened |= clipped;
        if first.len() < limit {
            first.push(index);
        }
        if is_blank_row(&row) {
            return;
        }
        nonblank = nonblank.saturating_add(1);
        if fill.len() < limit {
            fill.push(index);
        }
        if nonblank <= head {
            return;
        }
        if tail.len() == TOOL_OUTPUT_TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back((index, states_outcome(&row.text)));
        if let Some(rank) = output_importance_rank(&row.text) {
            important.push((rank, index));
            if important.len() > limit + TOOL_OUTPUT_TAIL_LINES {
                important.pop();
            }
        }
    });
    let selected: BTreeSet<usize> = if total <= limit {
        first.into_iter().collect()
    } else if nonblank <= limit {
        fill.into_iter().collect()
    } else {
        let tail_budget = TOOL_OUTPUT_TAIL_LINES.min(limit.saturating_sub(head));
        let mut selected: BTreeSet<usize> = fill.iter().take(head).copied().collect();
        let mut picked = Vec::with_capacity(tail_budget);
        for &(index, outcome) in tail.iter().rev() {
            if outcome && picked.len() < tail_budget {
                picked.push(index);
            }
        }
        for &(index, _) in tail.iter().rev() {
            if picked.len() >= tail_budget {
                break;
            }
            if !picked.contains(&index) {
                picked.push(index);
            }
        }
        selected.extend(picked);
        let mut ranked = important.into_vec();
        ranked.sort_unstable();
        for (_, index) in ranked {
            if selected.len() >= limit {
                break;
            }
            selected.insert(index);
        }
        for index in fill {
            if selected.len() >= limit {
                break;
            }
            selected.insert(index);
        }
        selected
    };
    let mut projection = OutputProjection {
        rows: Vec::with_capacity(limit.saturating_mul(2).saturating_add(1)),
        omitted_before: Vec::with_capacity(limit.saturating_mul(2).saturating_add(1)),
        total_rows: total,
        total_nonblank: nonblank,
        shortened,
        ..Default::default()
    };
    let mut index = 0usize;
    let mut hidden = 0usize;
    let mut only = None;
    for_each_output_row(output, width, |row, _| {
        if selected.contains(&index) {
            if hidden == 1 {
                projection
                    .rows
                    .push(only.take().expect("one hidden row was retained"));
                projection.omitted_before.push(0);
                hidden = 0;
            }
            only = None;
            projection.rows.push(row);
            projection.omitted_before.push(hidden);
            hidden = 0;
        } else if !is_blank_row(&row) {
            hidden = hidden.saturating_add(1);
            if hidden == 1 {
                only = Some(row);
            } else {
                only = None;
            }
        }
        index = index.saturating_add(1);
    });
    if hidden == 1 {
        projection
            .rows
            .push(only.expect("one hidden row was retained"));
        projection.omitted_before.push(0);
    } else {
        projection.omitted_after = hidden;
    }
    projection
}

struct OutputRowStream {
    text: String,
    styled: Vec<StyledSegment>,
    source_styled: bool,
    width: usize,
    first_width: usize,
    current_width: usize,
    intact: bool,
    clipped: bool,
    style_clipped: bool,
    emitted: bool,
    label_pending: bool,
}

impl OutputRowStream {
    fn new(first_width: usize, width: usize, intact: bool, source_styled: bool) -> Self {
        Self {
            text: String::new(),
            styled: Vec::new(),
            source_styled,
            width,
            first_width,
            current_width: 0,
            intact,
            clipped: false,
            style_clipped: false,
            emitted: false,
            label_pending: true,
        }
    }

    fn emit(&mut self, emit: &mut impl FnMut(OutputRow, bool)) {
        if !self.text.trim().is_empty() {
            self.label_pending = false;
        }
        let styled = std::mem::take(&mut self.styled);
        let has_style = self.source_styled && !styled.is_empty();
        emit(
            OutputRow {
                text: std::mem::take(&mut self.text),
                intact: self.intact && !self.clipped,
                styled: (has_style && !self.style_clipped).then_some(styled),
            },
            self.clipped || self.style_clipped,
        );
        self.current_width = 0;
        self.emitted = true;
        self.clipped = false;
        self.style_clipped = false;
    }

    fn push(
        &mut self,
        grapheme: codewhale_tui_kit::ui_text::DisplayGrapheme<'_>,
        styles: &[Style],
        emit: &mut impl FnMut(OutputRow, bool),
    ) {
        if self.intact && self.clipped {
            return;
        }
        if self.intact && grapheme.clipped_bytes > 0 {
            self.text.clear();
            self.text.push('…');
            self.styled.clear();
            self.clipped = true;
            return;
        }
        if self.intact && self.text.len().saturating_add(grapheme.text.len()) > MAX_ROW_BYTES {
            self.text.clear();
            self.text.push('…');
            self.styled.clear();
            self.clipped = true;
            return;
        }
        if grapheme.text.len() > MAX_ROW_BYTES {
            if !self.text.is_empty() {
                self.emit(emit);
            }
            self.text.push('…');
            self.clipped = true;
            self.emit(emit);
            return;
        }
        let limit = if self.label_pending {
            self.first_width
        } else {
            self.width
        };
        let byte_limit = self.text.len().saturating_add(grapheme.text.len()) > MAX_ROW_BYTES;
        if !self.intact
            && !self.text.is_empty()
            && (byte_limit || self.current_width.saturating_add(grapheme.width) > limit)
        {
            let start = (!byte_limit)
                .then(|| last_word_start(&self.text, grapheme.text, limit))
                .flatten();
            let carry = start.map(|at| self.text.split_off(at)).unwrap_or_default();
            let carry_style = if !self.style_clipped && !self.styled.is_empty() {
                let parts = [self.text.clone(), carry.clone()];
                let mut split = split_segments(&self.styled, &parts);
                self.styled = std::mem::take(&mut split[0]);
                std::mem::take(&mut split[1])
            } else {
                Vec::new()
            };
            let carry_clipped = self.style_clipped && !carry.is_empty();
            self.emit(emit);
            self.current_width = UnicodeWidthStr::width(carry.as_str());
            self.text = carry;
            self.styled = carry_style;
            self.style_clipped = carry_clipped;
        }
        self.text.push_str(grapheme.text);
        self.current_width = self.current_width.saturating_add(grapheme.width);
        self.clipped |= grapheme.clipped_bytes > 0;
        if !self.style_clipped {
            for &(ch, tag) in grapheme.cells {
                let style = styles.get(tag).copied().unwrap_or_default();
                if let Some((text, previous)) = self.styled.last_mut()
                    && *previous == style
                {
                    text.push(ch);
                } else if self.styled.len() < MAX_STYLE_RUNS {
                    self.styled.push((ch.to_string(), style));
                } else {
                    self.styled.clear();
                    self.style_clipped = true;
                    break;
                }
            }
        }
    }

    fn finish(mut self, emit: &mut impl FnMut(OutputRow, bool)) {
        if !self.text.is_empty() || !self.emitted {
            self.emit(emit);
        }
    }
}

#[derive(Default)]
struct PathLike {
    prefix: [char; 5],
    prefix_chars: usize,
    previous: [char; 2],
    url: bool,
    separator: bool,
    extension: bool,
    suffix: Option<usize>,
    bytes: usize,
}

impl PathLike {
    fn push(&mut self, text: &str) {
        self.bytes = self.bytes.saturating_add(text.len());
        for ch in text.chars() {
            if self.prefix_chars < 5 && (self.prefix_chars > 0 || !ch.is_whitespace()) {
                self.prefix[self.prefix_chars] = ch;
                self.prefix_chars += 1;
            }
            self.url |= self.previous == [':', '/'] && ch == '/';
            self.previous = [self.previous[1], ch];
            self.separator |= matches!(ch, '/' | '\\');
            if ch.is_whitespace() {
                self.extension |= self.suffix.is_some_and(|bytes| bytes <= 8);
                self.suffix = None;
            } else if ch == '.' {
                self.suffix = Some(0);
            } else if let Some(bytes) = self.suffix.as_mut() {
                *bytes = bytes.saturating_add(ch.len_utf8());
            }
        }
    }

    fn finish(self) -> bool {
        self.url
            || self.prefix == ['f', 'i', 'l', 'e', ':']
            || self.separator
                && (self.bytes > MAX_ROW_BYTES
                    || self.extension
                    || self.suffix.is_some_and(|bytes| bytes <= 8))
    }
}

fn for_each_output_row(output: &str, width: u16, mut emit: impl FnMut(OutputRow, bool)) {
    use codewhale_tui_kit::ui_text::DisplayNormalizer;
    let wrap_width = width.saturating_sub(4).max(1) as usize;
    let first_width = usize::from(width)
        .saturating_sub(UnicodeWidthStr::width(TRANSCRIPT_RAIL) + FIRST_ROW_LABEL_COLUMNS)
        .clamp(1, wrap_width);
    let mut first = true;
    let mut emitted = false;
    for line in output.lines() {
        let requires_strip = line
            .bytes()
            .any(|byte| byte == 0x1b || byte < 0x20 && !matches!(byte, b'\r' | b'\t'));
        let mut path = PathLike::default();
        let plain = if requires_strip {
            let mut plain = String::with_capacity(line.len().min(MAX_STYLED_LINE_BYTES));
            let mut bounded = true;
            codewhale_sanitize::sanitize::visit_ansi_text(line, |part| {
                path.push(part);
                if bounded && plain.len().saturating_add(part.len()) <= MAX_STYLED_LINE_BYTES {
                    plain.push_str(part);
                } else {
                    bounded = false;
                    plain.clear();
                }
            });
            bounded.then_some(std::borrow::Cow::Owned(plain))
        } else {
            path.push(line);
            (line.len() <= MAX_STYLED_LINE_BYTES).then_some(std::borrow::Cow::Borrowed(line))
        };
        let styled = plain
            .as_deref()
            .filter(|_| line.len() <= MAX_STYLED_LINE_BYTES)
            .and_then(|plain| styled_segments(line, plain));
        let styles: Vec<Style> = styled
            .as_ref()
            .map(|segments| segments.iter().map(|(_, style)| *style).collect())
            .unwrap_or_default();
        let mut stream = OutputRowStream::new(
            if first { first_width } else { wrap_width },
            wrap_width,
            path.finish(),
            !styles.is_empty(),
        );
        stream.style_clipped =
            styled.is_none() && line.len() > MAX_STYLED_LINE_BYTES && line.contains('\x1b');
        let mut normalizer = DisplayNormalizer::new(0);
        let mut emit_row = |row: OutputRow, clipped| {
            if !row.text.trim().is_empty() {
                first = false;
            }
            emitted = true;
            emit(row, clipped);
        };
        {
            let mut emit_grapheme = |grapheme: codewhale_tui_kit::ui_text::DisplayGrapheme<'_>| {
                if grapheme.clipped_bytes == 0 {
                    stream.push(grapheme, &styles, &mut emit_row);
                } else {
                    let mut offset = 0usize;
                    for part in grapheme.text.graphemes(true) {
                        let count = part.chars().count();
                        stream.push(
                            codewhale_tui_kit::ui_text::DisplayGrapheme {
                                text: part,
                                cells: &grapheme.cells[offset..offset + count],
                                width: UnicodeWidthStr::width(part),
                                clipped_bytes: grapheme.clipped_bytes,
                                escaped: grapheme.escaped,
                            },
                            &styles,
                            &mut emit_row,
                        );
                        offset += count;
                    }
                }
            };
            if let Some(segments) = &styled {
                for (tag, (text, _)) in segments.iter().enumerate() {
                    normalizer.push(text, tag, &mut emit_grapheme);
                }
            } else if let Some(plain) = &plain {
                normalizer.push(plain, 0, &mut emit_grapheme);
            } else if requires_strip {
                codewhale_sanitize::sanitize::visit_ansi_text(line, |part| {
                    normalizer.push(part, 0, &mut emit_grapheme)
                });
            } else {
                normalizer.push(line, 0, &mut emit_grapheme);
            }
            normalizer.finish(&mut emit_grapheme);
        }
        stream.finish(&mut emit_row);
    }
    if !emitted {
        emit(
            OutputRow {
                text: String::new(),
                intact: false,
                styled: None,
            },
            false,
        );
    }
}

#[cfg(test)]
fn output_rows(output: &str, width: u16) -> Vec<OutputRow> {
    let mut rows = Vec::new();
    for_each_output_row(output, width, |row, _| rows.push(row));
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
    codewhale_tui_kit::osc8::strip_ansi_keep_sgr_into(line, &mut kept);
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
    let contains = |needle: &str| {
        line.as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
    };
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
    .any(|needle| contains(needle))
    {
        return Some(0);
    }
    if contains("warning") || contains("warn") {
        return Some(1);
    }
    is_path_or_url_like(line).then_some(2)
}

fn is_path_or_url_like(line: &str) -> bool {
    let mut path = PathLike::default();
    path.push(line);
    path.finish()
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
    _width: u16,
) {
    // #374: apply file:line highlighting when the row text contains
    // a `path:line` pattern. Diff style takes precedence (colored
    // prefix lines should stay colored), but if no diff style matched,
    // check for a file:line pattern and highlight it distinctively.
    let diff_style = diff_line_style(&row.text);
    let file_style = file_line_style(&row.text);
    let value_style = diff_style.or(file_style).unwrap_or_else(tool_value_style);
    if let Some(segments) = &row.styled {
        lines.push(render_card_detail_line_single_styled(
            label,
            segments,
            value_style,
        ));
    } else {
        lines.push(render_card_detail_line_single(
            label,
            &row.text,
            value_style,
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

/// Wrap `text` to `width` columns, ending each row between words when it
/// can. A word longer than a row is still split between graphemes.
///
/// Nothing is dropped or added: the rows concatenate back to `text`, with
/// the whitespace a row was broken at left on the end of that row. Tool
/// output keeps its column alignment, and `split_segments` can lay the
/// colours back over the rows by counting characters.
pub(super) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for_each_wrapped_text(text, width, |part| lines.push(part.to_string()));
    lines
}

pub(super) fn for_each_wrapped_text(text: &str, width: usize, mut emit: impl FnMut(&str)) {
    if width == 0 || text.is_empty() {
        emit(text);
        return;
    }
    let mut start = 0usize;
    for (index, grapheme) in text.grapheme_indices(true) {
        let end = index + grapheme.len();
        if start < index && UnicodeWidthStr::width(&text[start..end]) > width {
            let split = last_word_start(&text[start..index], grapheme, width)
                .map_or(index, |at| start + at);
            emit(&text[start..split]);
            start = split;
        }
    }
    emit(&text[start..]);
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
            let lines = render_tool_output_mode(output, width, 20, RenderMode::Live, Locale::En);
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

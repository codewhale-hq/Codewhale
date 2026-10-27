//! Shared text helpers for TUI selection and clipboard workflows.

use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::history::HistoryCell;
use crate::tui::osc8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyLineSeparator {
    None,
    Space,
    Newline,
}

impl CopyLineSeparator {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Space => " ",
            Self::Newline => "\n",
        }
    }
}

pub(crate) fn truncate_line_to_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text_display_width(text) <= max_width {
        return text.to_string();
    }
    // For very small budgets, take whole graphemes until the next one would
    // exceed the display width. Never split an emoji or combining sequence.
    if max_width <= 3 {
        let mut out = String::new();
        let mut width = 0usize;
        for grapheme in text.graphemes(true) {
            let grapheme_width = grapheme_display_width(grapheme);
            if width + grapheme_width > max_width {
                break;
            }
            out.push_str(grapheme);
            width += grapheme_width;
        }
        return out;
    }

    let mut out = String::new();
    let mut width = 0usize;
    let limit = max_width.saturating_sub(3);
    for grapheme in text.graphemes(true) {
        let grapheme_width = grapheme_display_width(grapheme);
        if width + grapheme_width > limit {
            break;
        }
        out.push_str(grapheme);
        width += grapheme_width;
    }
    out.push_str("...");
    out
}

/// Truncate `text` to `max_width` display columns, preferring whole words.
pub(crate) fn semantic_truncate(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text_display_width(text) <= max_width {
        return text.to_string();
    }

    const ELLIPSIS: char = '…';
    let ellipsis_width = char_display_width(ELLIPSIS);
    let limit = max_width.saturating_sub(ellipsis_width);
    if limit == 0 {
        return ELLIPSIS.to_string();
    }

    let mut width = 0usize;
    let mut cut_byte = 0usize;
    let mut last_word_end = None;
    let mut in_word = false;
    for (byte_idx, grapheme) in text.grapheme_indices(true) {
        let grapheme_width = grapheme_display_width(grapheme);
        if width + grapheme_width > limit {
            break;
        }
        width += grapheme_width;
        cut_byte = byte_idx + grapheme.len();
        if grapheme.chars().all(char::is_whitespace) {
            if in_word {
                last_word_end = Some(byte_idx);
                in_word = false;
            }
        } else if breaks_after_cjk(grapheme) {
            // Han and kana have no spaces to wait for: every character is a
            // word end. Without this, one space early in a Chinese or
            // Japanese line cuts everything after it.
            last_word_end = Some(cut_byte);
            in_word = false;
        } else {
            in_word = true;
        }
    }
    if cut_byte == 0 {
        return ELLIPSIS.to_string();
    }

    let mut body = if let Some(word_end) = last_word_end {
        text[..word_end].trim_end()
    } else {
        text[..cut_byte].trim_end()
    };
    if body.is_empty() {
        body = text[..cut_byte].trim_end();
    }
    let mut out = body.to_string();
    out.push(ELLIPSIS);
    out
}

/// A cut may follow a Han, kana or CJK punctuation character, except an
/// opening bracket, which would be left dangling before the ellipsis.
fn breaks_after_cjk(grapheme: &str) -> bool {
    let Some(first) = grapheme.chars().next() else {
        return false;
    };
    crate::tui::translation::is_cjk(first)
        && !matches!(
            first,
            '（' | '「' | '『' | '【' | '〔' | '《' | '〈' | '［' | '｛'
        )
}

pub(crate) fn semantic_truncate_with_affixes(
    prefix: &str,
    text: &str,
    suffix: &str,
    max_width: usize,
) -> String {
    let fixed_width = text_display_width(prefix) + text_display_width(suffix);
    if fixed_width > max_width {
        return semantic_truncate(&format!("{prefix}{text}{suffix}"), max_width);
    }
    format!(
        "{prefix}{}{suffix}",
        semantic_truncate_between_affixes(prefix, text, suffix, max_width)
    )
}

pub(crate) fn semantic_truncate_between_affixes(
    prefix: &str,
    text: &str,
    suffix: &str,
    max_width: usize,
) -> String {
    let fixed_width = text_display_width(prefix) + text_display_width(suffix);
    if fixed_width > max_width {
        return String::new();
    }
    semantic_truncate(text, max_width - fixed_width)
}

pub(super) fn history_cell_to_text(cell: &HistoryCell, width: u16) -> String {
    // Error detail/copy is a diagnostic boundary, not a screenshot of the
    // live wrapping. Preserve the exact source message so narrow terminals do
    // not insert newlines into hostnames, env vars, commands, or URLs.
    if let HistoryCell::Error { message, .. } = cell {
        return message.clone();
    }
    cell.transcript_lines(width)
        .into_iter()
        .map(line_to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Serialize one complete history cell for Copy Message.
///
/// User, assistant and system cells have canonical source text. Returning it directly
/// preserves authored Markdown and hard line breaks while keeping role glyphs,
/// continuation rails, and visual wrapping out of the clipboard. Transcript
/// drag selections reuse this same projection per intersected cell
/// (`selection_to_markdown`, #6156); only the rendered-text fallback still
/// serializes the live cache, where Markdown has already been transformed and
/// user-message soft wraps do not carry join metadata.
///
/// Complex cells intentionally retain the full-transcript representation.
/// Tool and thinking transcript renderers include semantic headers and complete
/// output that can differ from the capped or folded live cache.
pub(super) fn history_cell_to_clipboard_text(cell: &HistoryCell, width: u16) -> String {
    match cell {
        HistoryCell::User { content }
        | HistoryCell::Assistant { content, .. }
        | HistoryCell::System { content } => content.clone(),
        _ => history_cell_to_text(cell, width),
    }
}

fn line_to_string(line: Line<'static>) -> String {
    let mut out = String::new();
    append_spans_plain(line.spans.iter(), &mut out);
    out
}

/// Convert a rendered transcript line to plain text, stripping OSC-8 link
/// escape sequences. The caller is responsible for shifting selection columns
/// to account for any visual-only rail prefix (see
/// `TranscriptViewCache::rail_prefix_width`).
pub(super) fn line_to_plain(line: &Line<'static>) -> String {
    let mut out = String::new();
    append_spans_plain(line.spans.iter(), &mut out);
    out
}

fn append_spans_plain<'a, I>(spans: I, out: &mut String)
where
    I: Iterator<Item = &'a Span<'a>>,
{
    for span in spans {
        if span.content.contains('\x1b') {
            osc8::strip_into(&span.content, out);
        } else {
            out.push_str(span.content.as_ref());
        }
    }
}

pub(crate) fn text_display_width(text: &str) -> usize {
    text.graphemes(true).map(grapheme_display_width).sum()
}

/// Visible width of one grapheme: ratatui strips control characters before
/// painting, so they occupy no cells. Every other grapheme keeps the shared
/// [`grapheme_display_width`] contract.
fn visible_grapheme_width(grapheme: &str) -> usize {
    if grapheme.chars().any(|c| c.is_control()) {
        0
    } else {
        grapheme_display_width(grapheme)
    }
}

/// Display width in painted terminal columns: control characters are
/// invisible (ratatui strips them), so they add nothing.
///
/// Mouse selection coordinates are terminal cells, so the copy path must
/// measure in this space: the fixed-4 [`text_display_width`] would shift
/// every column after a tab away from what the user dragged over. The two
/// agree on text without control characters.
pub(crate) fn text_visible_width(text: &str) -> usize {
    text.graphemes(true).map(visible_grapheme_width).sum()
}

/// Slice `[start, end)` in painted terminal columns, the space the mouse
/// reports.
///
/// A grapheme overlapping the window is kept whole under the same strict
/// rule for every width: zero-width control spans join the output only when
/// the window strictly covers their position, so interior tabs survive while
/// un-aimable edge touches stay out.
pub(crate) fn slice_visible_columns(text: &str, start: usize, end: usize) -> String {
    if end <= start {
        return String::new();
    }

    let mut out = String::new();
    let mut col = 0usize;
    for grapheme in text.graphemes(true) {
        let grapheme_start = col;
        let grapheme_end = col.saturating_add(visible_grapheme_width(grapheme));
        if grapheme_end > start && grapheme_start < end {
            out.push_str(grapheme);
        }
        col = grapheme_end;
        if col >= end {
            break;
        }
    }
    out
}

pub(super) fn char_display_width(ch: char) -> usize {
    match ch {
        '\t' => 4,
        // Enclosed Alphanumerics (U+2460-U+24FF), Dingbat Circled Digits
        // (U+2776-U+2793), and Circled Numbers on Black Square (U+3248-U+324F)
        // have East Asian Width "Ambiguous" but are rendered as 2 columns in
        // CJK terminals. unicode-width's width() conservatively reports 1; we
        // match the terminal. (#4479)
        '\u{2460}'..='\u{24FF}' | '\u{2776}'..='\u{2793}' | '\u{3248}'..='\u{324F}' => 2,
        _ => {
            // `width()` returns `None` for control/unassigned chars (default them to
            // one column so layout doesn't collapse) and `Some(0)` for genuinely
            // zero-width chars — combining marks, ZWJ, zero-width spaces — which must
            // stay 0 so display-width math (truncation, slicing, overflow, copy)
            // matches what the terminal actually renders.
            UnicodeWidthChar::width(ch).unwrap_or(1)
        }
    }
}

/// Measure one extended grapheme using the same string-level Unicode rules as
/// Ratatui. String width intentionally differs from the sum of codepoint widths
/// for keycaps, ZWJ emoji, modifiers, and other terminal ligatures.
///
/// Keycap sequences (such as 1\u{fe0f}\u{20e3}) that lack an FE0F variation
/// selector still render as 2 columns in terminals, but unicode-width's
/// `grapheme.width()` only reports 1. We force 2 when U+20E3 is present in a
/// multi-codepoint grapheme. (#4479)
pub(super) fn grapheme_display_width(grapheme: &str) -> usize {
    if grapheme == "\t" {
        return 4;
    }
    if let Some(ch) = grapheme.chars().next()
        && ch.len_utf8() == grapheme.len()
    {
        return char_display_width(ch);
    }
    // Keycap sequences always render as 2 columns. unicode-width's
    // `width()` undercounts the non-FE0F variant to 1.
    if grapheme.contains('\u{20e3}') {
        return 2;
    }
    UnicodeWidthStr::width(grapheme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    #[test]
    fn line_to_plain_strips_osc_8_wrapper() {
        let wrapped = format!(
            "\x1b]8;;{}\x1b\\{}\x1b]8;;\x1b\\",
            "https://example.com", "https://example.com"
        );
        let line = Line::from(vec![
            Span::raw("see "),
            Span::raw(wrapped),
            Span::raw(" for details"),
        ]);
        let text = line_to_plain(&line);
        assert_eq!(text, "see https://example.com for details");
    }

    #[test]
    fn line_to_plain_passes_through_plain_spans() {
        let line = Line::from(vec![Span::raw("plain "), Span::raw("text")]);
        let text = line_to_plain(&line);
        assert_eq!(text, "plain text");
    }

    #[test]
    fn line_to_plain_includes_all_spans() {
        // Visual-only rail spans are stripped by the caller using
        // TranscriptViewCache::rail_prefix_width — line_to_plain itself
        // is a faithful span-to-string pass-through.
        let line = Line::from(vec![Span::raw("\u{2502} "), Span::raw("tool output")]);
        let text = line_to_plain(&line);
        assert_eq!(text, "\u{2502} tool output");
    }

    #[test]
    fn slice_text_respects_column_bounds() {
        let text = "hello world";
        assert_eq!(slice_visible_columns(text, 0, 5), "hello");
        assert_eq!(slice_visible_columns(text, 6, 11), "world");
        assert_eq!(slice_visible_columns(text, 0, 0), "");
        assert_eq!(slice_visible_columns(text, 0, 100), text);
    }

    #[test]
    fn slice_text_handles_multibyte_characters() {
        let text = "a─b"; // U+2500 is 1 display column on supported terminals
        assert_eq!(slice_visible_columns(text, 1, 2), "─");
        assert_eq!(slice_visible_columns(text, 0, 3), text);
    }

    #[test]
    fn slice_text_truncates_at_end() {
        let text = "ab";
        assert_eq!(slice_visible_columns(text, 1, 5), "b");
    }

    #[test]
    fn visible_width_ignores_stripped_controls() {
        assert_eq!(text_visible_width("\t"), 0);
        assert_eq!(text_visible_width("\ta"), 1);
        assert_eq!(text_visible_width("ab\t"), 2);
        assert_eq!(text_visible_width("a\tb"), 2);
    }

    #[test]
    fn visible_slice_keeps_interior_controls_only() {
        // Interior tab strictly inside the window survives.
        assert_eq!(slice_visible_columns("ab\tcdef", 0, 4), "ab\tcd");
        // Zero-width span at the window edge stays out, like any grapheme.
        assert_eq!(slice_visible_columns("\txy", 0, 2), "xy");
        assert_eq!(slice_visible_columns("ab", 0, 2), "ab");
    }

    // --- Unicode / CJK / terminal-width QA (issue #3488) -------------------
    // These exercise the production width helpers directly so the assertions
    // track the same code path the renderer uses.

    #[test]
    fn text_display_width_counts_cjk_as_two_columns() {
        assert_eq!(text_display_width("中文"), 4); // two wide glyphs
        assert_eq!(text_display_width("Hello世界"), 9); // 5 ASCII + 2×2
        // Full-width (ambiguous→wide) punctuation is two columns each.
        assert_eq!(text_display_width("，。！？"), 8);
    }

    #[test]
    fn text_display_width_treats_zero_width_marks_as_zero() {
        // A combining mark adds no column: "e" + U+0301 renders as one cell.
        // (Regression guard: the old `.max(1)` counted it as 1, over-reporting
        // width and causing premature truncation / border drift on text with
        // combining marks or ZWJ emoji sequences.)
        assert_eq!(text_display_width("e\u{0301}"), 1);
        assert_eq!(text_display_width("cafe\u{0301}"), 4);
        // The complete ZWJ emoji is one two-column grapheme, matching Ratatui.
        assert_eq!(text_display_width("\u{1F469}\u{200D}\u{1F4BB}"), 2);
    }

    #[test]
    fn text_display_width_keeps_control_and_tab_widths() {
        // Control chars still occupy a column (avoid layout collapse); tab = 4.
        assert_eq!(text_display_width("a\u{0007}b"), 3);
        assert_eq!(text_display_width("\t"), 4);
        assert_eq!(text_display_width("\ta"), 5);
    }

    #[test]
    fn truncate_line_to_width_respects_display_width_not_byte_len() {
        // No truncation when the string already fits by display width.
        assert_eq!(truncate_line_to_width("中文", 10), "中文");
        // Oversized: reserve 3 cols for the ellipsis, fill the rest by width.
        let out = truncate_line_to_width("中文测试", 7);
        assert_eq!(out, "中文...");
        assert_eq!(text_display_width(&out), 7);
        // Never split a wide glyph across the boundary, and never emit U+FFFD.
        let clipped = truncate_line_to_width("界界界界界", 5);
        assert!(text_display_width(&clipped) <= 5);
        assert!(!clipped.contains('\u{FFFD}'));
    }

    #[test]
    fn semantic_truncate_prefers_word_boundaries() {
        let out = semantic_truncate("hello world foo bar", 14);
        assert_eq!(out, "hello world…");
        assert!(text_display_width(&out) <= 14);
    }

    #[test]
    fn semantic_truncate_falls_back_with_long_words_and_wide_glyphs() {
        let long_word = semantic_truncate("supercalifragilistic", 8);
        assert_eq!(long_word, "superca…");
        assert!(text_display_width(&long_word) <= 8);

        let cjk = semantic_truncate("中文测试文本", 7);
        assert_eq!(cjk, "中文测…");
        assert!(text_display_width(&cjk) <= 7);
    }

    #[test]
    fn semantic_truncate_breaks_between_cjk_after_a_space() {
        // The space after "Operate" used to be the only word end, so the
        // Chinese clause after it was dropped whole.
        let out = semantic_truncate("Operate 把你的提示词变成目标", 20);
        assert_eq!(out, "Operate 把你的提示…");
        assert!(text_display_width(&out) <= 20);

        // An opening bracket is not left dangling before the ellipsis.
        assert_eq!(semantic_truncate("模型已切换（原为", 13), "模型已切换…");
    }

    #[test]
    fn semantic_truncate_handles_empty_and_tiny_budgets() {
        assert_eq!(semantic_truncate("", 10), "");
        assert_eq!(semantic_truncate("hello", 0), "");
        assert_eq!(semantic_truncate("hello", 1), "…");
    }

    #[test]
    fn semantic_truncate_between_affixes_reserves_fixed_columns() {
        let hint = semantic_truncate_between_affixes(
            " > [ ] Prefix stability  (",
            "whether system/tools stayed cacheable",
            ")",
            49,
        );
        let row = format!(" > [ ] Prefix stability  ({hint})");
        assert_eq!(hint, "whether system/tools…");
        assert!(text_display_width(&row) <= 49);
    }

    #[test]
    fn slice_text_slices_cjk_by_display_column() {
        // Columns:  中=[0,2) 文=[2,4) a=[4,5) b=[5,6)
        let text = "中文ab";
        assert_eq!(slice_visible_columns(text, 0, 2), "中");
        assert_eq!(slice_visible_columns(text, 2, 4), "文");
        assert_eq!(slice_visible_columns(text, 4, 6), "ab");
    }

    // --- New #3488 fixtures: CJK/wide-glyph truncation on selector-style rows.
    // truncate_line_to_width is the production helper behind sidebar (file_tree),
    // statusline (footer_ui), hotbar, and picker (mouse_ui) row rendering, so
    // these exercise the same truncation path those surfaces use.

    #[test]
    fn truncate_line_to_width_full_width_cjk_lands_on_glyph_boundary() {
        // Each Han glyph is two columns. With an odd budget the truncation must
        // land on a whole-glyph boundary (reserving three columns for the
        // ellipsis), never leaving a half-rendered wide cell or emitting U+FFFD.
        let title = "项目报告结果"; // 6 glyphs, 12 columns
        let out = truncate_line_to_width(title, 7);
        // Budget 7 -> limit 4 columns -> two glyphs fit, then the ellipsis.
        assert_eq!(out, "项目...");
        assert_eq!(text_display_width(&out), 7);
        // The kept prefix is composed only of whole wide glyphs (each 2 cols),
        // proving the boundary glyph was dropped whole, not split.
        let prefix = out.strip_suffix("...").expect("ellipsis present");
        assert!(prefix.chars().all(|c| char_display_width(c) == 2));
        assert!(!out.contains('\u{FFFD}'));
    }

    #[test]
    fn truncate_line_to_width_mixed_ascii_cjk_row_keeps_ellipsis_within_budget() {
        // A sidebar/selector row mixing an ASCII label with a CJK title, wider
        // than the column budget, must truncate with a trailing ellipsis that
        // still fits by display width and must not split a wide glyph.
        let row = "Task: 数据库迁移任务 done"; // ASCII label + 7 Han glyphs
        let budget = 12;
        let out = truncate_line_to_width(row, budget);
        assert!(out.ends_with("..."), "expected ellipsis, got {out:?}");
        // Ellipsis-and-content fit within the budget by *display* width.
        assert!(text_display_width(&out) <= budget);
        // The non-ellipsis prefix stays within budget-minus-ellipsis, so the
        // wide glyph on the boundary was dropped whole rather than half-drawn.
        let prefix = out.strip_suffix("...").expect("ellipsis present");
        assert!(text_display_width(prefix) <= budget - 3);
        assert!(!out.contains('\u{FFFD}'));
        // The semantic ASCII prefix survives truncation.
        assert!(out.starts_with("Task:"));
    }

    #[test]
    fn truncate_line_to_width_dense_cjk_selector_row_survives_narrow_widths() {
        // Picker/selector rows degrade through truncate_line_to_width when the
        // terminal is narrow. A dense row with a leading marker glyph and CJK
        // content must stay within budget at tiny widths, without panicking or
        // emitting a replacement char from a mid-glyph byte split.
        let row = "▸ 中文项目 · main"; // marker + CJK + separator + branch
        for width in [1usize, 2, 3, 4, 6, 8] {
            let out = truncate_line_to_width(row, width);
            assert!(
                text_display_width(&out) <= width,
                "width={width}: {out:?} exceeds budget"
            );
            assert!(
                !out.contains('\u{FFFD}'),
                "width={width}: truncation split a wide glyph"
            );
        }
    }

    // --- keycap / grapheme regression guard (#4479) ---------------------------
    // Fully qualified keycap sequences render as two columns. Codepoint sums
    // report one; the canonical string/grapheme contract reports two.

    #[test]
    fn text_display_width_treats_keycap_sequence_as_two_columns() {
        for keycap in [
            "1\u{fe0f}\u{20e3}",
            "9\u{fe0f}\u{20e3}",
            "#\u{fe0f}\u{20e3}",
        ] {
            assert_eq!(text_display_width(keycap), 2);
            assert_eq!(text_display_width(keycap), UnicodeWidthStr::width(keycap));
        }
        // A digit directly followed by U+20E3 (without FE0F variation selector)
        // still renders as a 2-column keycap in terminals. We force this in
        // grapheme_display_width when the grapheme contains U+20E3.
        // A standalone U+20E3 is a zero-width combining mark.
        assert_eq!(text_display_width("1\u{20e3}"), 2);
        assert_eq!(text_display_width("\u{20e3}"), 0);
    }

    #[test]
    fn circled_digit_display_width() {
        assert_eq!(char_display_width('\u{2460}'), 2);
        assert_eq!(char_display_width('\u{2461}'), 2);
        assert_eq!(char_display_width('\u{24ea}'), 2);
        assert_eq!(char_display_width('\u{2776}'), 2);
        assert_eq!(text_display_width("\u{2460}\u{2461}\u{2462}"), 6);
        assert_eq!(text_display_width("Step \u{2460}: init"), 13);
        assert_eq!(text_display_width("A\u{24d0}B"), 4);
    }

    #[test]
    fn unicode_width_reports_circled_digits_as_two_columns() {
        // Regression guard for the unicode-width patch (#4479): Ratatui
        // renders text through UnicodeWidthChar::width(), so the patch must
        // make even the raw crate API report 2 columns for ambiguous-width
        // characters — otherwise Ratatui places them in 1 cell while the
        // terminal paints 2, shifting every downstream column.
        assert_eq!(UnicodeWidthChar::width('\u{2460}'), Some(2));
        assert_eq!(UnicodeWidthChar::width('\u{24ea}'), Some(2));
        assert_eq!(UnicodeWidthStr::width("\u{2460}\u{2461}\u{2462}"), 6);
    }

    #[test]
    fn slice_text_does_not_split_keycap_sequence() {
        let row = "step 1\u{fe0f}\u{20e3} done";
        // The keycap occupies columns [5, 7). Any overlapping selection keeps
        // the complete grapheme; no isolated FE0F/U+20E3 mark may escape.
        for (start, end) in [(0, 7), (5, 6), (6, 7)] {
            let sliced = slice_visible_columns(row, start, end);
            assert!(
                sliced.contains("1\u{fe0f}\u{20e3}"),
                "range=({start}, {end}) split keycap: {sliced:?}"
            );
        }
    }

    #[test]
    fn truncate_line_to_width_always_stays_within_budget_with_keycap() {
        // Budgets from zero through wide, with and without surrounding text.
        let cases = [
            "1\u{fe0f}\u{20e3}",
            "A 1\u{fe0f}\u{20e3} B",
            "step 2\u{fe0f}\u{20e3} and 3\u{fe0f}\u{20e3} continue",
        ];
        for text in &cases {
            for budget in 0..=text_display_width(text) + 4 {
                let out = truncate_line_to_width(text, budget);
                let width = text_display_width(&out);
                assert!(
                    width <= budget,
                    "budget={budget} text={text:?} -> {out:?} (width={width})"
                );
                assert!(!out.ends_with('\u{fe0f}'));
                assert!(!out.starts_with('\u{20e3}'));
            }
        }
    }

    #[test]
    fn clipboard_text_for_assistant_uses_source_without_visual_rails() {
        let content = "A long assistant response that will wrap at a narrow width.";
        let cell = HistoryCell::Assistant {
            content: content.to_string(),
            streaming: false,
        };

        let rendered = history_cell_to_text(&cell, 12);
        assert_ne!(rendered, content, "test setup must exercise rendering");
        assert!(
            rendered.contains('\n'),
            "test setup must exercise visual wrapping: {rendered:?}"
        );
        assert_eq!(history_cell_to_clipboard_text(&cell, 12), content);
    }

    #[test]
    fn clipboard_text_preserves_authored_rail_glyphs() {
        let content = "● literal role glyph\n▏ literal rail glyph";
        let cell = HistoryCell::Assistant {
            content: content.to_string(),
            streaming: false,
        };

        assert_eq!(history_cell_to_clipboard_text(&cell, 10), content);
    }

    #[test]
    fn clipboard_text_preserves_markdown_source_and_hard_breaks() {
        let content = r#"Heading

```rust
fn main() {
    println!("hello");
}
```

After code."#;
        let cell = HistoryCell::Assistant {
            content: content.to_string(),
            streaming: false,
        };

        assert_eq!(history_cell_to_clipboard_text(&cell, 16), content);
        let diagnostic = serde_json::json!({"prompt": "Use \"quoted\" input\nwith a second line"});
        let source = serde_json::to_string_pretty(&diagnostic).unwrap();
        let cell = HistoryCell::System {
            content: source.clone(),
        };
        let copied = history_cell_to_clipboard_text(&cell, 16);
        assert_eq!(copied, source);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&copied).unwrap(),
            diagnostic
        );
    }

    #[test]
    fn clipboard_text_for_user_uses_source_text() {
        let content = "user text that wraps visually";
        let cell = HistoryCell::User {
            content: content.to_string(),
        };

        let rendered = history_cell_to_text(&cell, 8);
        assert_ne!(rendered, content, "test setup must exercise rendering");
        assert!(
            rendered.contains('\n'),
            "test setup must exercise visual wrapping: {rendered:?}"
        );
        assert_eq!(history_cell_to_clipboard_text(&cell, 8), content);
    }

    #[test]
    fn clipboard_text_for_thinking_keeps_full_transcript_semantics() {
        let content = "First paragraph lede.\n\
            Second sentence of the first paragraph.\n\n\
            Second paragraph: deeper analysis follows.\n\
            More detail in paragraph two.\n\n\
            Third paragraph: even more reasoning.\n\
            With another line.\n\n\
            Fourth paragraph: the conclusion.\n\
            And one more line for good measure.\n\n\
            Fifth paragraph: final verification.\n\
            One last supporting detail.";
        let cell = HistoryCell::Thinking {
            content: content.to_string(),
            streaming: false,
            duration_secs: Some(3.2),
        };

        let live = cell
            .lines_with_options(
                80,
                crate::tui::history::TranscriptRenderOptions {
                    low_motion: true,
                    ..crate::tui::history::TranscriptRenderOptions::default()
                },
            )
            .into_iter()
            .map(line_to_string)
            .collect::<Vec<_>>()
            .join("\n");
        let transcript = history_cell_to_text(&cell, 80);
        let copied = history_cell_to_clipboard_text(&cell, 80);

        assert!(!live.contains("Fifth paragraph"), "{live:?}");
        assert!(transcript.contains("Fifth paragraph"), "{transcript:?}");
        assert_eq!(copied, transcript);
    }

    #[test]
    fn clipboard_text_for_tool_keeps_full_transcript_semantics() {
        use crate::tui::history::{GenericToolCell, ToolCell, ToolStatus};

        let cell = HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "exec_shell".to_string(),
            status: ToolStatus::Success,
            input_summary: Some("cargo test".to_string()),
            output: Some("complete tool output".to_string()),
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        }));

        let transcript = history_cell_to_text(&cell, 80);
        assert!(
            transcript.contains("complete tool output"),
            "{transcript:?}"
        );
        assert_eq!(history_cell_to_clipboard_text(&cell, 80), transcript);
    }
}

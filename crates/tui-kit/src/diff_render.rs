//! Diff rendering helpers for TUI previews.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use codewhale_palette as palette;

const LINE_NUMBER_WIDTH: usize = 4;

/// Below this word-level similarity a replaced line pair is rewritten, not
/// edited, and emphasising the changed words would light up the whole row.
const INTRALINE_MIN_RATIO: f32 = 0.5;

/// Pairing stops here so a huge hunk cannot buffer word segments for every
/// replaced line; longer runs render line by line.
const INTRALINE_MAX_RUN: usize = 64;

/// A run of text inside a changed line and whether it is part of the change.
type Segment = (String, bool);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFileSummary {
    pub path: String,
    pub added: usize,
    pub deleted: usize,
    pub hunks: usize,
}

/// A rendered diff preview with an exact count of rows not retained.
///
/// The renderer still scans the complete diff so summaries and omission
/// counts stay truthful, but it never accumulates more than the requested
/// number of body rows. This keeps narrow, generated diffs from first
/// materializing an unbounded `Vec<Line>` only to be truncated by a caller.
#[derive(Debug, Clone)]
pub struct BoundedDiffRender {
    pub lines: Vec<Line<'static>>,
    pub origins: Vec<usize>,
    pub omitted_rows: usize,
}

pub fn render_diff(diff: &str, width: u16) -> Vec<Line<'static>> {
    render_diff_bounded(diff, width, usize::MAX).lines
}

/// Render a diff summary and at most `max_body_rows` rows of diff evidence.
#[must_use]
pub fn render_diff_bounded(diff: &str, width: u16, max_body_rows: usize) -> BoundedDiffRender {
    let summaries = summarize_diff(diff);
    let mut rendered = render_diff_body_bounded(diff, width, max_body_rows);
    if !summaries.is_empty() {
        let mut lines = render_diff_summary(&summaries, width);
        let mut origins = vec![0; lines.len()];
        origins.extend(rendered.origins.iter().map(|origin| origin + 1));
        lines.append(&mut rendered.lines);
        rendered.lines = lines;
        rendered.origins = origins;
    }
    rendered
}

/// Render only the diff body. Callers that already own a semantic summary use
/// this form so the bounded preview budget is spent on the actual red/green
/// evidence instead of a second, generic summary.
/// Render at most `max_rows` body rows while counting every omitted wrapped
/// row. Allocation is bounded by the retained preview, one source line's
/// wrapped representation, and the current `-`/`+` run: its line slices
/// (borrowed, pointer-sized) plus word segments for at most
/// `INTRALINE_MAX_RUN` replaced pairs — never by the size of the complete
/// diff.
#[must_use]
pub fn render_diff_body_bounded(diff: &str, width: u16, max_rows: usize) -> BoundedDiffRender {
    let mut collector = BoundedLineCollector::new(max_rows);
    let mut old_line: Option<usize> = None;
    let mut new_line: Option<usize> = None;

    let mut lines = diff.lines().peekable();
    'line: while let Some(raw) = lines.next() {
        if raw.starts_with("diff --git") || raw.starts_with("index ") {
            collector.extend(render_header_line(raw, width));
            continue;
        }

        if raw.starts_with("--- ") || raw.starts_with("+++ ") {
            collector.extend(render_header_line(raw, width));
            continue;
        }

        if raw.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_hunk_header(raw) {
                old_line = Some(old_start);
                new_line = Some(new_start);
            }
            collector.extend(render_hunk_header(raw, width));
            continue;
        }

        if is_no_newline_marker(raw) {
            collector.extend(render_header_line(raw, width));
            continue;
        }

        if is_added(raw) {
            let content = &raw[1..];
            collector.extend(render_diff_line(
                content,
                width,
                old_line,
                new_line,
                '+',
                added_style(),
                None,
            ));
            if let Some(line) = new_line.as_mut() {
                *line = line.saturating_add(1);
            }
            continue;
        }

        if is_deleted(raw) {
            // A deleted run followed by an added run of the same length is a
            // set of replaced lines: emphasise the words that changed within
            // each pair. Any other shape renders line by line as before.
            let mut removed = vec![&raw[1..]];
            let mut added: Vec<&str> = Vec::new();
            let mut removed_markers: Vec<(usize, &str)> = Vec::new();
            let mut added_markers: Vec<(usize, &str)> = Vec::new();
            loop {
                if let Some(marker) = lines.next_if(|next| is_no_newline_marker(next)) {
                    if added.is_empty() {
                        removed_markers.push((removed.len() - 1, marker));
                    } else {
                        added_markers.push((added.len() - 1, marker));
                    }
                } else if added.is_empty()
                    && let Some(next) = lines.next_if(|next| is_deleted(next))
                {
                    if removed.len() < INTRALINE_MAX_RUN {
                        removed.push(&next[1..]);
                    } else {
                        flush_plain_run(
                            &mut collector,
                            &removed,
                            &removed_markers,
                            '-',
                            width,
                            &mut old_line,
                            &mut new_line,
                        );
                        render_plain_diff_line(
                            &mut collector,
                            &next[1..],
                            width,
                            &mut old_line,
                            &mut new_line,
                            '-',
                        );
                        continue 'line;
                    }
                } else if let Some(next) = lines.next_if(|next| is_added(next)) {
                    if added.len() < INTRALINE_MAX_RUN {
                        added.push(&next[1..]);
                    } else {
                        flush_plain_run(
                            &mut collector,
                            &removed,
                            &removed_markers,
                            '-',
                            width,
                            &mut old_line,
                            &mut new_line,
                        );
                        flush_plain_run(
                            &mut collector,
                            &added,
                            &added_markers,
                            '+',
                            width,
                            &mut old_line,
                            &mut new_line,
                        );
                        render_plain_diff_line(
                            &mut collector,
                            &next[1..],
                            width,
                            &mut old_line,
                            &mut new_line,
                            '+',
                        );
                        continue 'line;
                    }
                } else {
                    break;
                }
            }

            let pairs: Vec<Option<(Vec<Segment>, Vec<Segment>)>> =
                if removed.len() == added.len() && removed.len() <= INTRALINE_MAX_RUN {
                    removed
                        .iter()
                        .zip(&added)
                        .map(|(old, new)| intraline_segments(old, new))
                        .collect()
                } else {
                    Vec::new()
                };

            for (idx, content) in removed.iter().enumerate() {
                let emphasis = pairs
                    .get(idx)
                    .and_then(|pair| pair.as_ref().map(|(old, _)| old.as_slice()));
                collector.extend(render_diff_line(
                    content,
                    width,
                    old_line,
                    new_line,
                    '-',
                    deleted_style(),
                    emphasis,
                ));
                if let Some(line) = old_line.as_mut() {
                    *line = line.saturating_add(1);
                }
                for &(marker_idx, marker) in &removed_markers {
                    if marker_idx == idx {
                        collector.extend(render_header_line(marker, width));
                    }
                }
            }
            for (idx, content) in added.iter().enumerate() {
                let emphasis = pairs
                    .get(idx)
                    .and_then(|pair| pair.as_ref().map(|(_, new)| new.as_slice()));
                collector.extend(render_diff_line(
                    content,
                    width,
                    old_line,
                    new_line,
                    '+',
                    added_style(),
                    emphasis,
                ));
                if let Some(line) = new_line.as_mut() {
                    *line = line.saturating_add(1);
                }
                for &(marker_idx, marker) in &added_markers {
                    if marker_idx == idx {
                        collector.extend(render_header_line(marker, width));
                    }
                }
            }
            continue;
        }

        if let Some(content) = raw.strip_prefix(' ') {
            collector.extend(render_diff_line(
                content,
                width,
                old_line,
                new_line,
                ' ',
                Style::default().fg(palette::TEXT_PRIMARY),
                None,
            ));
            if let Some(line) = old_line.as_mut() {
                *line = line.saturating_add(1);
            }
            if let Some(line) = new_line.as_mut() {
                *line = line.saturating_add(1);
            }
            continue;
        }

        collector.extend(render_header_line(raw, width));
    }

    collector.finish()
}

struct BoundedLineCollector {
    lines: Vec<Line<'static>>,
    origins: Vec<usize>,
    max_rows: usize,
    total_rows: usize,
    source_rows: usize,
}

impl BoundedLineCollector {
    fn new(max_rows: usize) -> Self {
        Self {
            lines: Vec::with_capacity(max_rows.min(256)),
            origins: Vec::with_capacity(max_rows.min(256)),
            max_rows,
            total_rows: 0,
            source_rows: 0,
        }
    }

    fn extend(&mut self, rows: Vec<Line<'static>>) {
        self.total_rows = self.total_rows.saturating_add(rows.len());
        let remaining = self.max_rows.saturating_sub(self.lines.len());
        let kept = rows.len().min(remaining);
        self.lines.extend(rows.into_iter().take(remaining));
        self.origins
            .extend(std::iter::repeat_n(self.source_rows, kept));
        self.source_rows += 1;
    }

    fn finish(self) -> BoundedDiffRender {
        BoundedDiffRender {
            omitted_rows: self.total_rows.saturating_sub(self.lines.len()),
            lines: self.lines,
            origins: self.origins,
        }
    }
}

#[must_use]
pub fn summarize_diff(diff: &str) -> Vec<DiffFileSummary> {
    let mut summaries = Vec::new();
    let mut current: Option<DiffFileSummary> = None;

    for raw in diff.lines() {
        if raw.starts_with("diff --git ") {
            if let Some(summary) = current.take()
                && summary.has_changes()
            {
                summaries.push(summary);
            }
            current = Some(DiffFileSummary {
                path: parse_diff_git_path(raw).unwrap_or_else(|| "<file>".to_string()),
                added: 0,
                deleted: 0,
                hunks: 0,
            });
            continue;
        }

        if raw.starts_with("+++ ") {
            let path = raw
                .trim_start_matches("+++ ")
                .trim_start_matches("b/")
                .to_string();
            if path != "/dev/null" {
                current
                    .get_or_insert_with(|| DiffFileSummary {
                        path: path.clone(),
                        added: 0,
                        deleted: 0,
                        hunks: 0,
                    })
                    .path = path.clone();
            }
            continue;
        }

        if raw.starts_with("@@") {
            current
                .get_or_insert_with(|| DiffFileSummary {
                    path: "<file>".to_string(),
                    added: 0,
                    deleted: 0,
                    hunks: 0,
                })
                .hunks += 1;
            continue;
        }

        if raw.starts_with('+') && !raw.starts_with("+++") {
            current
                .get_or_insert_with(|| DiffFileSummary {
                    path: "<file>".to_string(),
                    added: 0,
                    deleted: 0,
                    hunks: 0,
                })
                .added += 1;
        } else if raw.starts_with('-') && !raw.starts_with("---") {
            current
                .get_or_insert_with(|| DiffFileSummary {
                    path: "<file>".to_string(),
                    added: 0,
                    deleted: 0,
                    hunks: 0,
                })
                .deleted += 1;
        }
    }

    if let Some(summary) = current
        && summary.has_changes()
    {
        summaries.push(summary);
    }

    summaries
}

#[must_use]
pub fn diff_summary_label(diff: &str) -> Option<String> {
    let summaries = summarize_diff(diff);
    if summaries.is_empty() {
        return None;
    }
    let files = summaries.len();
    let added: usize = summaries.iter().map(|summary| summary.added).sum();
    let deleted: usize = summaries.iter().map(|summary| summary.deleted).sum();
    Some(format!(
        "{files} file{} +{added} -{deleted}",
        if files == 1 { "" } else { "s" }
    ))
}

impl DiffFileSummary {
    fn has_changes(&self) -> bool {
        self.added > 0 || self.deleted > 0 || self.hunks > 0
    }
}

fn parse_diff_git_path(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let _diff = parts.next()?;
    let _git = parts.next()?;
    let _old = parts.next()?;
    let new = parts.next()?;
    Some(new.trim_start_matches("b/").to_string())
}

fn render_diff_summary(summaries: &[DiffFileSummary], width: u16) -> Vec<Line<'static>> {
    let files = summaries.len();
    let added: usize = summaries.iter().map(|summary| summary.added).sum();
    let deleted: usize = summaries.iter().map(|summary| summary.deleted).sum();
    let hunks: usize = summaries.iter().map(|summary| summary.hunks).sum();

    let mut lines = Vec::new();
    lines.extend(wrap_with_style(
        &format!(
            "summary: {files} file{}, +{added} -{deleted}, {hunks} hunk{}",
            if files == 1 { "" } else { "s" },
            if hunks == 1 { "" } else { "s" },
        ),
        Style::default()
            .fg(palette::TEXT_PRIMARY)
            .add_modifier(Modifier::BOLD),
        width,
    ));
    for summary in summaries {
        let row = format!(
            "  {}  +{} -{}  {} hunk{}",
            summary.path,
            summary.added,
            summary.deleted,
            summary.hunks,
            if summary.hunks == 1 { "" } else { "s" },
        );
        lines.extend(wrap_with_style(
            &row,
            Style::default().fg(palette::TEXT_MUTED),
            width,
        ));
    }
    lines
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 3 {
        return None;
    }
    let old = parts[1].trim_start_matches('-');
    let new = parts[2].trim_start_matches('+');
    let old_start = old.split(',').next()?.parse::<usize>().ok()?;
    let new_start = new.split(',').next()?.parse::<usize>().ok()?;
    Some((old_start, new_start))
}

fn render_header_line(line: &str, width: u16) -> Vec<Line<'static>> {
    let style = Style::default()
        .fg(palette::WHALE_ACTION)
        .add_modifier(Modifier::BOLD);
    wrap_with_style(line, style, width)
}

fn render_hunk_header(line: &str, width: u16) -> Vec<Line<'static>> {
    let style = Style::default().fg(palette::WHALE_ACTION);
    wrap_with_style(line, style, width)
}

fn is_added(raw: &str) -> bool {
    raw.starts_with('+') && !raw.starts_with("+++")
}

fn is_deleted(raw: &str) -> bool {
    raw.starts_with('-') && !raw.starts_with("---")
}

fn is_no_newline_marker(raw: &str) -> bool {
    raw.starts_with("\\ No newline")
}

fn added_style() -> Style {
    Style::default()
        .fg(palette::DIFF_ADDED)
        .bg(palette::DIFF_ADDED_BG)
}

fn deleted_style() -> Style {
    Style::default()
        .fg(palette::STATUS_ERROR)
        .bg(palette::DIFF_DELETED_BG)
}

/// Split a replaced line pair into word runs (unicode word boundaries, so
/// punctuation stays out of the emphasis), flagging the runs that differ.
///
/// Returns `None` when the pair shares too few *words* to read as an edit —
/// whitespace and punctuation tokens do not count, so two lines that agree
/// only on syntax are painted whole, the way they always were.
fn intraline_segments(old: &str, new: &str) -> Option<(Vec<Segment>, Vec<Segment>)> {
    let diff = TextDiff::from_unicode_words(old, new);
    let mut old_segments: Vec<Segment> = Vec::new();
    let mut new_segments: Vec<Segment> = Vec::new();
    let mut changed = false;
    let mut shared_words = 0usize;
    let mut total_words = 0usize;
    for change in diff.iter_all_changes() {
        let text = change.value();
        let is_word = text.chars().any(char::is_alphanumeric);
        match change.tag() {
            ChangeTag::Equal => {
                if is_word {
                    shared_words += 2;
                    total_words += 2;
                }
                push_segment(&mut old_segments, text, false);
                push_segment(&mut new_segments, text, false);
            }
            ChangeTag::Delete => {
                changed = true;
                total_words += usize::from(is_word);
                push_segment(&mut old_segments, text, true);
            }
            ChangeTag::Insert => {
                changed = true;
                total_words += usize::from(is_word);
                push_segment(&mut new_segments, text, true);
            }
        }
    }
    if !changed || total_words == 0 {
        return None;
    }
    let ratio = shared_words as f32 / total_words as f32;
    (ratio >= INTRALINE_MIN_RATIO).then_some((old_segments, new_segments))
}

fn push_segment(segments: &mut Vec<Segment>, text: &str, emphasised: bool) {
    match segments.last_mut() {
        Some((run, flag)) if *flag == emphasised => run.push_str(text),
        _ => segments.push((text.to_string(), emphasised)),
    }
}

/// Paint the wrapped chunks of a changed line, mapping every non-whitespace
/// character back to its source flag by position. `wrap_text` only drops,
/// collapses, or re-inserts whitespace (the indent lead comes back on every
/// continuation chunk), so the non-whitespace sequence is the invariant.
/// Whitespace joins an emphasised run only when both its neighbours are in
/// it. Returns `None` if the invariant ever fails, and the caller paints the
/// whole line plainly rather than emphasising the wrong word.
fn emphasised_chunks(
    chunks: &[String],
    style: Style,
    segments: &[Segment],
) -> Option<Vec<Vec<Span<'static>>>> {
    let source: Vec<(char, bool)> = segments
        .iter()
        .flat_map(|(run, flag)| {
            run.chars()
                .filter(|ch| !ch.is_whitespace())
                .map(move |ch| (ch, *flag))
        })
        .collect();
    let emphasis = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
    let mut cursor = 0usize;
    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let mut spans = Vec::new();
        let mut run = String::new();
        let mut run_flag = false;
        let mut prev_flag = false;
        for ch in chunk.chars() {
            let flag = if ch.is_whitespace() {
                prev_flag && source.get(cursor).is_some_and(|(_, next)| *next)
            } else {
                let (expected, flag) = *source.get(cursor)?;
                debug_assert_eq!(
                    expected, ch,
                    "wrapped chunk diverged from its source line at {cursor}"
                );
                if expected != ch {
                    return None;
                }
                cursor += 1;
                prev_flag = flag;
                flag
            };
            if flag != run_flag && !run.is_empty() {
                let painted = if run_flag { emphasis } else { style };
                spans.push(Span::styled(std::mem::take(&mut run), painted));
            }
            run_flag = flag;
            run.push(ch);
        }
        if !run.is_empty() {
            let painted = if run_flag { emphasis } else { style };
            spans.push(Span::styled(run, painted));
        }
        out.push(spans);
    }
    debug_assert_eq!(
        cursor,
        source.len(),
        "wrapped chunks did not consume the whole source line"
    );
    (cursor == source.len()).then_some(out)
}

fn render_plain_diff_line(
    collector: &mut BoundedLineCollector,
    content: &str,
    width: u16,
    old_line: &mut Option<usize>,
    new_line: &mut Option<usize>,
    marker: char,
) {
    let style = match marker {
        '-' => deleted_style(),
        '+' => added_style(),
        _ => Style::default().fg(palette::TEXT_PRIMARY),
    };
    collector.extend(render_diff_line(
        content, width, *old_line, *new_line, marker, style, None,
    ));
    match marker {
        '-' => {
            if let Some(line) = old_line.as_mut() {
                *line = line.saturating_add(1);
            }
        }
        '+' => {
            if let Some(line) = new_line.as_mut() {
                *line = line.saturating_add(1);
            }
        }
        _ => {}
    }
}

fn flush_plain_run(
    collector: &mut BoundedLineCollector,
    lines: &[&str],
    markers: &[(usize, &str)],
    sign: char,
    width: u16,
    old_line: &mut Option<usize>,
    new_line: &mut Option<usize>,
) {
    for (idx, content) in lines.iter().enumerate() {
        render_plain_diff_line(collector, content, width, old_line, new_line, sign);
        for &(marker_idx, marker) in markers {
            if marker_idx == idx {
                collector.extend(render_header_line(marker, width));
            }
        }
    }
}

fn render_diff_line(
    content: &str,
    width: u16,
    old_line: Option<usize>,
    new_line: Option<usize>,
    marker: char,
    style: Style,
    emphasis: Option<&[Segment]>,
) -> Vec<Line<'static>> {
    let prefix = format_line_numbers(old_line, new_line, marker);
    let prefix_width = prefix.width();
    // The whole logical row carries the change tint — numbers included. A
    // bare gutter next to a painted body read as two unrelated strips.
    let gutter_style = match style.bg {
        Some(bg) => Style::default().fg(palette::TEXT_MUTED).bg(bg),
        None => Style::default().fg(palette::TEXT_MUTED),
    };
    let available = width.saturating_sub(prefix_width as u16).max(1) as usize;
    let wrapped = wrap_text(content, available);
    let mut painted = emphasis.and_then(|segments| emphasised_chunks(&wrapped, style, segments));

    let mut out = Vec::new();
    for (idx, chunk) in wrapped.into_iter().enumerate() {
        let gutter = if idx == 0 {
            Span::styled(prefix.clone(), gutter_style)
        } else {
            Span::styled(" ".repeat(prefix_width), gutter_style)
        };
        let mut spans = vec![gutter];
        match painted.as_mut() {
            Some(rows) => spans.append(&mut rows[idx]),
            None => spans.push(Span::styled(chunk, style)),
        }
        out.push(Line::from(spans));
    }

    if out.is_empty() {
        out.push(Line::from(vec![Span::styled(prefix, gutter_style)]));
    }

    out
}

fn format_line_numbers(old_line: Option<usize>, new_line: Option<usize>, marker: char) -> String {
    let old = old_line
        .filter(|_| marker != '+')
        .map(|value| format!("{value:>LINE_NUMBER_WIDTH$}"))
        .unwrap_or_else(|| " ".repeat(LINE_NUMBER_WIDTH));
    let new = new_line
        .filter(|_| marker != '-')
        .map(|value| format!("{value:>LINE_NUMBER_WIDTH$}"))
        .unwrap_or_else(|| " ".repeat(LINE_NUMBER_WIDTH));
    format!("{old} {new} {marker} ")
}

fn wrap_with_style(text: &str, style: Style, width: u16) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for part in wrap_text(text, width.max(1) as usize) {
        out.push(Line::from(Span::styled(part, style)));
    }
    if out.is_empty() {
        out.push(Line::from(Span::styled("", style)));
    }
    out
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let lead = text
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .collect::<String>();
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    let lead_width = lead.width();
    let mut current = lead.clone();
    let mut current_width = lead_width;
    let mut has_word = false;

    for word in trimmed.split_whitespace() {
        let word_width = word.width();
        if word_width > width {
            if has_word {
                lines.push(std::mem::take(&mut current));
                current = lead.clone();
                current_width = lead_width;
            }
            push_word_breaking_graphemes(word, width, &mut current, &mut current_width, &mut lines);
            has_word = current_width > lead_width;
            continue;
        }
        let additional = if has_word { word_width + 1 } else { word_width };
        if current_width + additional > width && has_word {
            lines.push(current);
            current = lead.clone();
            current_width = lead_width;
            has_word = false;
        }
        if has_word {
            current.push(' ');
            current_width += 1;
        }
        if current_width + word_width > width && !has_word && lead_width > 0 {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        if current_width == 0 && lead_width > 0 && word_width + lead_width <= width {
            current = lead.clone();
            current_width = lead_width;
        }
        current.push_str(word);
        current_width += word_width;
        has_word = true;
    }

    if has_word || !current.is_empty() {
        lines.push(current);
    } else {
        lines.push(String::new());
    }

    lines
}

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

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn diff_content_text(line: &Line<'static>) -> Option<String> {
        line.spans
            .get(1..)
            .filter(|rest| !rest.is_empty())
            .map(|rest| rest.iter().map(|span| span.content.as_ref()).collect())
    }

    fn emphasised_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .filter(|span| span.style.add_modifier.contains(Modifier::REVERSED))
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn rendered_body(diff: &str, width: u16) -> Vec<Line<'static>> {
        render_diff_body_bounded(diff, width, usize::MAX).lines
    }

    /// Text between the gutter and the first emphasised span, so a test can
    /// pin where on the row the emphasis starts.
    fn text_before_emphasis(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .skip(1)
            .take_while(|span| !span.style.add_modifier.contains(Modifier::REVERSED))
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn emphasis_per_row(diff: &str, width: u16) -> Vec<String> {
        rendered_body(diff, width)
            .iter()
            .skip(1) // hunk header
            .map(emphasised_text)
            .collect()
    }

    #[test]
    fn replaced_line_pair_emphasises_only_the_changed_words() {
        let diff = "\
@@ -1,1 +1,1 @@
-    let total = price * quantity;
+    let total = price * count;
";
        let rendered = rendered_body(diff, 80);
        assert_eq!(emphasis_per_row(diff, 80), vec!["quantity", "count"]);
        let content = rendered
            .iter()
            .filter_map(diff_content_text)
            .collect::<Vec<_>>();
        assert_eq!(
            content,
            vec![
                "    let total = price * quantity;".to_string(),
                "    let total = price * count;".to_string()
            ],
            "emphasis must not alter the line text"
        );
    }

    #[test]
    fn every_pair_in_a_replaced_run_is_emphasised_in_order() {
        let diff = "\
@@ -1,2 +1,2 @@
-let alpha = 1;
-let beta = 2;
+let alpha = 10;
+let beta = 20;
";
        assert_eq!(emphasis_per_row(diff, 80), vec!["1", "2", "10", "20"]);
    }

    #[test]
    fn a_rewrite_inside_a_replaced_run_stays_plain_while_its_neighbour_is_emphasised() {
        let diff = "\
@@ -1,2 +1,2 @@
-let x = 1;
-fn old_name() {}
+let x = 2;
+return None;
";
        assert_eq!(emphasis_per_row(diff, 80), vec!["1", "", "2", ""]);
    }

    #[test]
    fn lines_sharing_only_syntax_are_not_emphasised() {
        // Whitespace, `=` and `;` agree; every word differs but `let`.
        let diff = "\
@@ -1,1 +1,1 @@
-let alpha = beta;
+let gamma = delta;
";
        assert_eq!(emphasis_per_row(diff, 80), vec!["", ""]);
    }

    #[test]
    fn unequal_runs_render_without_emphasis() {
        let unequal = "\
@@ -1,2 +1,1 @@
-let a = 1;
-let b = 2;
+let a = 1; let b = 2;
";
        assert_eq!(emphasis_per_row(unequal, 80), vec!["", "", ""]);
    }

    #[test]
    fn emphasis_follows_unicode_word_boundaries() {
        let diff = "\
@@ -1,1 +1,1 @@
-café au lait, naïve
+café au thé, naïve
";
        assert_eq!(emphasis_per_row(diff, 80), vec!["lait", "thé"]);
    }

    #[test]
    fn no_newline_marker_does_not_break_pairing() {
        let diff = "\
@@ -1,1 +1,1 @@
-old line
\\ No newline at end of file
+new line
\\ No newline at end of file
";
        let rendered = rendered_body(diff, 80);
        let rows: Vec<String> = rendered.iter().map(emphasised_text).collect();
        assert_eq!(rows, vec!["", "old", "", "new", ""]);
        let text: Vec<String> = rendered.iter().map(line_text).collect();
        assert_eq!(
            text.iter()
                .filter(|row| row.contains("No newline at end of file"))
                .count(),
            2,
            "markers are still shown: {text:?}"
        );
    }

    #[test]
    fn pure_insertion_no_newline_marker_is_a_header_row() {
        let diff = "\
@@ -1,0 +1,1 @@
+inserted
\\ No newline at end of file
 context
";
        let rendered = rendered_body(diff, 80);
        let marker = rendered
            .iter()
            .find(|line| line_text(line).contains("No newline at end of file"))
            .expect("marker row");
        assert!(marker.spans[0].style.add_modifier.contains(Modifier::BOLD));

        let context = rendered
            .iter()
            .find(|line| line_text(line).contains("context"))
            .expect("context row");
        assert!(
            line_text(context).starts_with("   1    2   "),
            "context numbering was changed by the marker: {context:?}"
        );
    }

    #[test]
    fn emphasis_lands_on_the_right_chunk_of_a_wrapped_indented_line() {
        // Gutter is 12 columns; width 40 leaves 28 for text. With the 8-space
        // indent re-inserted on every continuation chunk, the line wraps as
        // "        let total =" / "        compute(alpha, beta," /
        // "        gamma, quantity);" — the change sits on the third chunk.
        let diff = "\
@@ -1,1 +1,1 @@
-        let total = compute(alpha, beta, gamma, quantity);
+        let total = compute(alpha, beta, gamma, count);
";
        let rendered = rendered_body(diff, 40);
        let rows: Vec<String> = rendered.iter().map(emphasised_text).collect();
        assert_eq!(rows, vec!["", "", "", "quantity", "", "", "count"]);
        assert_eq!(
            diff_content_text(&rendered[3]).as_deref(),
            Some("        gamma, quantity);")
        );
        assert_eq!(text_before_emphasis(&rendered[3]), "        gamma, ");
        assert_eq!(text_before_emphasis(&rendered[6]), "        gamma, ");
        // Row text is untouched by the emphasis.
        assert_eq!(
            diff_content_text(&rendered[6]).as_deref(),
            Some("        gamma, count);")
        );
    }

    #[test]
    fn emphasis_survives_wrapping_without_changing_text() {
        let diff = "\
@@ -1,1 +1,1 @@
-alpha beta gamma delta epsilon zeta eta theta iota kappa
+alpha beta gamma delta epsilon zeta eta THETA iota kappa
";
        let rendered = rendered_body(diff, 30);
        let rows: Vec<String> = rendered.iter().map(emphasised_text).collect();
        // 18 text columns: "alpha beta gamma" / "delta epsilon zeta" /
        // "eta theta iota" / "kappa" — the change sits on the third chunk.
        assert_eq!(rows, vec!["", "", "", "theta", "", "", "", "THETA", ""]);
        assert_eq!(text_before_emphasis(&rendered[3]), "eta ");
        assert_eq!(text_before_emphasis(&rendered[7]), "eta ");
        // Wrapping drops the space at each break; everything else survives.
        let body: String = rendered
            .iter()
            .skip(1)
            .filter_map(diff_content_text)
            .collect::<String>()
            .split_whitespace()
            .collect();
        assert_eq!(
            body,
            "alphabetagammadeltaepsilonzetaetathetaiotakappa\
             alphabetagammadeltaepsilonzetaetaTHETAiotakappa"
        );
    }

    #[test]
    fn summarizes_multi_file_diff() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,3 @@
 line
+new
-old
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -10,0 +11,2 @@
+one
+two
";

        let summaries = summarize_diff(diff);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].path, "src/a.rs");
        assert_eq!(summaries[0].added, 1);
        assert_eq!(summaries[0].deleted, 1);
        assert_eq!(summaries[1].path, "src/b.rs");
        assert_eq!(summaries[1].added, 2);
        assert_eq!(summaries[1].deleted, 0);
        assert_eq!(diff_summary_label(diff).as_deref(), Some("2 files +3 -1"));
    }

    #[test]
    fn render_diff_prepends_summary_and_gutter_markers() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,3 @@
 line
+new
-old
";

        let rendered = render_diff(diff, 80);
        let text = rendered.iter().map(line_text).collect::<Vec<_>>();
        assert!(text[0].contains("summary: 1 file, +1 -1, 1 hunk"));
        assert!(text.iter().any(|line| line.contains("src/a.rs +1 -1")));
        assert!(
            text.iter().any(|line| line.contains(" + new")),
            "added line should carry + gutter: {text:?}"
        );
        assert!(
            text.iter().any(|line| line.contains(" - old")),
            "deleted line should carry - gutter: {text:?}"
        );
    }

    #[test]
    fn render_diff_tints_the_gutter_with_the_row() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,3 @@
 line
+new
-old
";

        let rendered = render_diff(diff, 80);
        let gutter_bg = |needle: &str| {
            rendered
                .iter()
                .find(|line| line_text(line).contains(needle))
                .expect("diff row renders")
                .spans
                .first()
                .expect("gutter span")
                .style
                .bg
        };
        assert_eq!(
            gutter_bg("+ new"),
            Some(palette::DIFF_ADDED_BG),
            "added numbers share the added tint"
        );
        assert_eq!(
            gutter_bg("- old"),
            Some(palette::DIFF_DELETED_BG),
            "deleted numbers share the deleted tint"
        );
        assert_eq!(
            gutter_bg(" line"),
            None,
            "context numbers stay on the bare ground"
        );
    }

    #[test]
    fn wrap_text_preserves_leading_whitespace_without_extra_space() {
        assert_eq!(wrap_text("    let y = 2;", 80), vec!["    let y = 2;"]);
        assert_eq!(
            wrap_text("        println!(\"hello\");", 80),
            vec!["        println!(\"hello\");"]
        );
    }

    #[test]
    fn render_diff_preserves_leading_whitespace_exactly() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,3 @@
 fn main() {
+    let y = 2;
+        println!(\"{y}\");
 }
";

        let rendered = render_diff(diff, 80);
        let content = rendered
            .iter()
            .filter_map(diff_content_text)
            .collect::<Vec<_>>();

        assert!(
            content.iter().any(|line| line == "    let y = 2;"),
            "added line should keep exact 4-space indent: {content:?}"
        );
        assert!(
            content
                .iter()
                .any(|line| line == "        println!(\"{y}\");"),
            "added line should keep exact 8-space indent: {content:?}"
        );
    }

    #[test]
    fn wrap_text_breaks_overlong_cjk_runs() {
        let text = "这是一个非常长的中文字符串".repeat(10);
        let lines = wrap_text(&text, 16);

        for line in &lines {
            assert!(line.width() <= 16, "line {line:?} exceeds width 16");
        }

        assert_eq!(lines.join(""), text);
    }

    #[test]
    fn wrap_text_breaks_overlong_words_between_graphemes() {
        // A ZWJ family and a skin-toned thumbs-up are one two-column grapheme
        // each, the way Ratatui counts cells.
        let word = "ab\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{1f44d}\u{1f3fd}cd";
        for width in 2..=6 {
            let lines = wrap_text(word, width);
            let rejoined: Vec<&str> = lines.iter().flat_map(|line| line.graphemes(true)).collect();
            assert_eq!(
                rejoined,
                word.graphemes(true).collect::<Vec<_>>(),
                "width {width} split a grapheme: {lines:?}"
            );
            for line in &lines {
                assert!(line.width() <= width, "line {line:?} exceeds width {width}");
            }
        }
    }

    #[test]
    fn bounded_body_retains_only_budget_and_counts_wrapped_omissions() {
        let mut diff = String::from(
            "diff --git a/src/generated.rs b/src/generated.rs\n\
             --- a/src/generated.rs\n\
             +++ b/src/generated.rs\n\
             @@ -1,0 +1,3000 @@\n",
        );
        for index in 0..3_000 {
            use std::fmt::Write as _;
            writeln!(
                diff,
                "+    generated_{index:04} = a deliberately long value that wraps narrowly"
            )
            .expect("append generated diff");
        }

        let full_row_count = render_diff_body_bounded(&diff, 32, usize::MAX).lines.len();
        let rendered = render_diff_body_bounded(&diff, 32, 14);

        assert_eq!(rendered.lines.len(), 14);
        assert_eq!(
            rendered.omitted_rows,
            full_row_count.saturating_sub(rendered.lines.len())
        );
        let retained = rendered.lines.iter().map(line_text).collect::<Vec<_>>();
        assert!(
            retained
                .iter()
                .any(|line| line.contains("@@ -1,0 +1,3000 @@"))
        );
        assert!(
            retained
                .iter()
                .any(|line| line.contains(" +     generated_0000")),
            "retained rows preserve gutters and leading whitespace: {retained:?}"
        );
    }
}

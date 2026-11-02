//! Diff rendering helpers for TUI previews.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use codewhale_palette as palette;

const LINE_NUMBER_WIDTH: usize = 4;
const MAX_RETAINED_ROWS: usize = 512;
const MAX_ROW_BYTES: usize = 32 * 1024;
const INTRALINE_MAX_BYTES: usize = 8 * 1024;
const INTRALINE_MAX_TOKENS: usize = 256;
const INTRALINE_MAX_WORK: usize = 65_536;
const INTRALINE_RUN_BYTES: usize = 128 * 1024;
const INTRALINE_RUN_WORK: usize = 1_048_576;

/// Below this word-level similarity a replaced line pair is rewritten, not
/// edited, and emphasising the changed words would light up the whole row.
const INTRALINE_MIN_RATIO: f32 = 0.5;

/// Pairing stops here so a huge hunk cannot buffer word segments for every
/// replaced line; longer runs render line by line.
const INTRALINE_MAX_RUN: usize = 64;

/// A run of text inside a changed line and whether it is part of the change.
type Segment = (String, bool);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFileSummary<'a> {
    pub path: &'a str,
    pub added: usize,
    pub deleted: usize,
    pub hunks: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffTotals {
    pub files: usize,
    pub added: usize,
    pub deleted: usize,
    pub hunks: usize,
}

/// A rendered diff preview with an exact count of rows not retained.
///
/// The renderer scans the complete diff so summaries and omission counts stay
/// truthful, retaining only a finite row window and bounded text per row.
#[derive(Debug, Clone)]
pub struct BoundedDiffRender {
    pub lines: Vec<Line<'static>>,
    pub origins: Vec<usize>,
    pub omitted_rows: usize,
    pub omitted_summary_rows: usize,
    pub omitted_body_rows: usize,
    pub clipped_rows: usize,
    pub clipped_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiffSearchResult {
    pub total_matches: usize,
    pub selected_index: usize,
    pub selected_row: Option<usize>,
    pub selected_origin: Option<usize>,
}

pub fn render_diff(diff: &str, width: u16) -> Vec<Line<'static>> {
    let mut rendered = render_diff_bounded(diff, width, MAX_RETAINED_ROWS);
    if rendered.omitted_rows > 0 {
        rendered.lines.pop();
        let notice = format!("+{} …", rendered.omitted_rows.saturating_add(1));
        let notice = if crate::ui_text::text_display_width(&notice) <= usize::from(width.max(1)) {
            notice
        } else {
            "…".to_string()
        };
        rendered.lines.push(Line::from(Span::styled(
            notice,
            Style::default().fg(palette::TEXT_MUTED),
        )));
    }
    rendered.lines
}

/// Render a diff summary and body within one finite total row budget.
#[must_use]
pub fn render_diff_bounded(diff: &str, width: u16, max_rows: usize) -> BoundedDiffRender {
    render_diff_window(diff, width, 0, max_rows)
}

#[must_use]
pub fn render_diff_window(
    diff: &str,
    width: u16,
    start_row: usize,
    max_rows: usize,
) -> BoundedDiffRender {
    let mut collector = BoundedLineCollector::new(start_row, max_rows);
    render_diff_into(diff, width, true, &mut collector);
    collector.finish()
}

/// Render only the diff body. Callers that already own a semantic summary use
/// this form so the bounded preview budget is spent on the actual red/green
/// evidence instead of a second, generic summary.
/// Allocation is bounded by the retained rows and the current replacement run;
/// skipped wrapped rows and no-newline markers do not accumulate in vectors.
#[must_use]
pub fn render_diff_body_bounded(diff: &str, width: u16, max_rows: usize) -> BoundedDiffRender {
    let mut collector = BoundedLineCollector::new(0, max_rows);
    render_diff_into(diff, width, false, &mut collector);
    collector.finish()
}

#[must_use]
pub fn search_diff(diff: &str, width: u16, query: &str, match_index: usize) -> DiffSearchResult {
    let query = query.trim();
    if query.is_empty() {
        return DiffSearchResult::default();
    }
    let mut collector = BoundedLineCollector::new(0, 0);
    collector.search = Some((query.to_ascii_lowercase(), match_index));
    render_diff_into(diff, width, true, &mut collector);
    collector.search_result
}

#[must_use]
pub fn search_diff_from_origin(
    diff: &str,
    width: u16,
    query: &str,
    origin: usize,
) -> DiffSearchResult {
    let query = query.trim();
    if query.is_empty() {
        return DiffSearchResult::default();
    }
    let mut collector = BoundedLineCollector::new(0, 0);
    collector.search = Some((query.to_ascii_lowercase(), 0));
    collector.search_origin = Some(origin);
    render_diff_into(diff, width, true, &mut collector);
    collector.search_result
}

#[must_use]
pub fn diff_row_for_origin(diff: &str, width: u16, origin: usize) -> usize {
    let mut collector = BoundedLineCollector::new(0, 0);
    collector.anchor_origin = Some(origin);
    render_diff_into(diff, width, true, &mut collector);
    collector
        .anchor_row
        .unwrap_or_else(|| collector.total_rows.saturating_sub(1))
}

fn render_diff_into(diff: &str, width: u16, summary: bool, collector: &mut BoundedLineCollector) {
    let totals = summary.then(|| diff_totals(diff)).unwrap_or_default();
    let origin_offset = usize::from(totals.files > 0);
    if totals.files > 0 {
        render_diff_summary(diff, totals, width, collector);
        collector.summary_rows = collector.total_rows;
        collector.summary_retained = collector.lines.len();
    }
    let mut old_line = None;
    let mut new_line = None;
    let mut lines = diff.lines().enumerate().peekable();
    'line: while let Some((origin, raw)) = lines.next() {
        let origin = origin.saturating_add(origin_offset);
        if raw.starts_with("diff --git")
            || raw.starts_with("index ")
            || raw.starts_with("--- ")
            || raw.starts_with("+++ ")
        {
            render_header_line(raw, width, origin, collector);
            continue;
        }
        if raw.starts_with("@@") {
            if let Some((old_start, new_start)) = parse_hunk_header(raw) {
                old_line = Some(old_start);
                new_line = Some(new_start);
            }
            render_styled_text(
                &[raw],
                Style::default().fg(palette::WHALE_ACTION),
                width,
                origin,
                collector,
            );
            continue;
        }
        if is_no_newline_marker(raw) {
            render_header_line(raw, width, origin, collector);
            continue;
        }
        if is_added(raw) {
            render_plain_diff_line(
                collector,
                &raw[1..],
                width,
                origin,
                &mut old_line,
                &mut new_line,
                '+',
            );
            continue;
        }
        if is_deleted(raw) {
            // A deleted run followed by an added run of the same length is a
            // set of replaced lines: emphasise the words that changed within
            // each pair. Any other shape renders line by line as before.
            let mut removed = vec![RunLine::new(&raw[1..], origin)];
            let mut added: Vec<RunLine<'_>> = Vec::new();
            loop {
                if let Some((marker_origin, marker)) =
                    lines.next_if(|(_, next)| is_no_newline_marker(next))
                {
                    let line = if added.is_empty() {
                        removed.last_mut()
                    } else {
                        added.last_mut()
                    };
                    if let Some(line) = line {
                        line.marker(diff, marker, marker_origin.saturating_add(origin_offset));
                    }
                } else if added.is_empty()
                    && let Some((next_origin, next)) = lines.next_if(|(_, next)| is_deleted(next))
                {
                    let origin = next_origin.saturating_add(origin_offset);
                    if removed.len() < INTRALINE_MAX_RUN {
                        removed.push(RunLine::new(&next[1..], origin));
                    } else {
                        flush_plain_run(
                            collector,
                            &removed,
                            '-',
                            width,
                            &mut old_line,
                            &mut new_line,
                        );
                        render_plain_diff_line(
                            collector,
                            &next[1..],
                            width,
                            origin,
                            &mut old_line,
                            &mut new_line,
                            '-',
                        );
                        continue 'line;
                    }
                } else if let Some((next_origin, next)) = lines.next_if(|(_, next)| is_added(next))
                {
                    let origin = next_origin.saturating_add(origin_offset);
                    if added.len() < INTRALINE_MAX_RUN {
                        added.push(RunLine::new(&next[1..], origin));
                    } else {
                        flush_plain_run(
                            collector,
                            &removed,
                            '-',
                            width,
                            &mut old_line,
                            &mut new_line,
                        );
                        flush_plain_run(
                            collector,
                            &added,
                            '+',
                            width,
                            &mut old_line,
                            &mut new_line,
                        );
                        render_plain_diff_line(
                            collector,
                            &next[1..],
                            width,
                            origin,
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
            let mut byte_budget = INTRALINE_RUN_BYTES;
            let mut work_budget = INTRALINE_RUN_WORK;
            let pairs: Vec<_> = if removed.len() == added.len() {
                removed
                    .iter()
                    .zip(&added)
                    .map(|(old, new)| {
                        intraline_segments(
                            old.content,
                            new.content,
                            &mut byte_budget,
                            &mut work_budget,
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            for (sign, run) in [('-', &removed), ('+', &added)] {
                for (index, line) in run.iter().enumerate() {
                    let emphasis =
                        pairs
                            .get(index)
                            .and_then(|pair| pair.as_ref())
                            .map(|(old, new)| {
                                if sign == '-' {
                                    old.as_slice()
                                } else {
                                    new.as_slice()
                                }
                            });
                    let style = if sign == '-' {
                        deleted_style()
                    } else {
                        added_style()
                    };
                    render_diff_line(
                        line.content,
                        width,
                        old_line,
                        new_line,
                        sign,
                        style,
                        emphasis,
                        line.origin,
                        collector,
                    );
                    increment_line(sign, &mut old_line, &mut new_line);
                    line.render_markers(width, collector);
                }
            }
            continue;
        }
        if let Some(content) = raw.strip_prefix(' ') {
            render_diff_line(
                content,
                width,
                old_line,
                new_line,
                ' ',
                Style::default().fg(palette::TEXT_PRIMARY),
                None,
                origin,
                collector,
            );
            increment_line(' ', &mut old_line, &mut new_line);
            continue;
        }
        render_header_line(raw, width, origin, collector);
    }
}

struct RunLine<'a> {
    content: &'a str,
    origin: usize,
    markers: &'a str,
    marker_origin: usize,
}

impl<'a> RunLine<'a> {
    fn new(content: &'a str, origin: usize) -> Self {
        Self {
            content,
            origin,
            markers: "",
            marker_origin: 0,
        }
    }

    fn marker(&mut self, diff: &'a str, marker: &'a str, origin: usize) {
        let start = if self.markers.is_empty() {
            self.marker_origin = origin;
            marker.as_ptr() as usize - diff.as_ptr() as usize
        } else {
            self.markers.as_ptr() as usize - diff.as_ptr() as usize
        };
        let end = marker.as_ptr() as usize - diff.as_ptr() as usize + marker.len();
        self.markers = &diff[start..end];
    }

    fn render_markers(&self, width: u16, collector: &mut BoundedLineCollector) {
        for (index, marker) in self.markers.lines().enumerate() {
            render_header_line(
                marker,
                width,
                self.marker_origin.saturating_add(index),
                collector,
            );
        }
    }
}

struct BoundedLineCollector {
    lines: Vec<Line<'static>>,
    origins: Vec<usize>,
    start_row: usize,
    max_rows: usize,
    total_rows: usize,
    summary_rows: usize,
    summary_retained: usize,
    clipped_rows: usize,
    clipped_bytes: usize,
    search: Option<(String, usize)>,
    search_origin: Option<usize>,
    search_origin_found: bool,
    search_result: DiffSearchResult,
    anchor_origin: Option<usize>,
    anchor_row: Option<usize>,
}

#[derive(Clone, Copy)]
struct RowCapture {
    start: usize,
    end: usize,
    search: bool,
    bytes: usize,
    columns: usize,
}

impl RowCapture {
    fn keeps(self, row: usize) -> bool {
        self.search || (self.start..self.end).contains(&row)
    }
}

impl BoundedLineCollector {
    fn new(start_row: usize, max_rows: usize) -> Self {
        let max_rows = max_rows.min(MAX_RETAINED_ROWS);
        Self {
            lines: Vec::with_capacity(max_rows.min(256)),
            origins: Vec::with_capacity(max_rows.min(256)),
            start_row,
            max_rows,
            total_rows: 0,
            summary_rows: 0,
            summary_retained: 0,
            clipped_rows: 0,
            clipped_bytes: 0,
            search: None,
            search_origin: None,
            search_origin_found: false,
            search_result: DiffSearchResult::default(),
            anchor_origin: None,
            anchor_row: None,
        }
    }

    fn capture(&self, bytes: usize, columns: usize) -> RowCapture {
        RowCapture {
            start: self.start_row.saturating_sub(self.total_rows),
            end: self
                .start_row
                .saturating_add(self.max_rows)
                .saturating_sub(self.total_rows),
            search: self.search.is_some(),
            bytes,
            columns,
        }
    }

    fn push(
        &mut self,
        row: WrappedRow,
        origin: usize,
        prefix: &str,
        make_line: impl FnOnce(String) -> Line<'static>,
    ) {
        if self.anchor_row.is_none() && self.anchor_origin.is_some_and(|target| origin >= target) {
            self.anchor_row = Some(self.total_rows);
        }
        if let Some((query, selected)) = &self.search {
            let mut text = String::with_capacity(prefix.len().saturating_add(row.text.len()));
            text.push_str(prefix);
            text.push_str(&row.text);
            if text.to_ascii_lowercase().contains(query) {
                let selected = match self.search_origin {
                    Some(target) => {
                        let first = self.search_result.total_matches == 0;
                        let anchored = !self.search_origin_found && origin >= target;
                        self.search_origin_found |= anchored;
                        first || anchored
                    }
                    None => self.search_result.total_matches == *selected,
                };
                if selected {
                    self.search_result.selected_index = self.search_result.total_matches;
                    self.search_result.selected_row = Some(self.total_rows);
                    self.search_result.selected_origin = Some(origin);
                }
                self.search_result.total_matches =
                    self.search_result.total_matches.saturating_add(1);
            }
        }
        if self.total_rows >= self.start_row && self.lines.len() < self.max_rows {
            if row.clipped_bytes > 0 {
                self.clipped_rows = self.clipped_rows.saturating_add(1);
                self.clipped_bytes = self.clipped_bytes.saturating_add(row.clipped_bytes);
            }
            self.lines.push(make_line(row.text));
            self.origins.push(origin);
        }
        self.total_rows = self.total_rows.saturating_add(1);
    }

    fn finish(self) -> BoundedDiffRender {
        BoundedDiffRender {
            omitted_rows: self.total_rows.saturating_sub(self.lines.len()),
            omitted_summary_rows: self.summary_rows.saturating_sub(self.summary_retained),
            omitted_body_rows: self
                .total_rows
                .saturating_sub(self.summary_rows)
                .saturating_sub(self.lines.len().saturating_sub(self.summary_retained)),
            lines: self.lines,
            origins: self.origins,
            clipped_rows: self.clipped_rows,
            clipped_bytes: self.clipped_bytes,
        }
    }
}

#[must_use]
pub fn summarize_diff(diff: &str) -> impl Iterator<Item = DiffFileSummary<'_>> {
    let mut lines = diff.lines();
    let mut current: Option<DiffFileSummary<'_>> = None;
    std::iter::from_fn(move || {
        for raw in lines.by_ref() {
            if raw.starts_with("diff --git ") {
                let previous = current.take();
                current = Some(DiffFileSummary {
                    path: parse_diff_git_path(raw).unwrap_or("<file>"),
                    added: 0,
                    deleted: 0,
                    hunks: 0,
                });
                if let Some(summary) = previous
                    && summary.has_changes()
                {
                    return Some(summary);
                }
            } else if raw.starts_with("+++ ") {
                let path = raw.trim_start_matches("+++ ").trim_start_matches("b/");
                if path != "/dev/null" {
                    current.get_or_insert_with(DiffFileSummary::empty).path = path;
                }
            } else if raw.starts_with("@@") {
                let summary = current.get_or_insert_with(DiffFileSummary::empty);
                summary.hunks = summary.hunks.saturating_add(1);
            } else if is_added(raw) {
                let summary = current.get_or_insert_with(DiffFileSummary::empty);
                summary.added = summary.added.saturating_add(1);
            } else if is_deleted(raw) {
                let summary = current.get_or_insert_with(DiffFileSummary::empty);
                summary.deleted = summary.deleted.saturating_add(1);
            }
        }
        current.take().filter(DiffFileSummary::has_changes)
    })
}

#[must_use]
pub fn diff_totals(diff: &str) -> DiffTotals {
    summarize_diff(diff).fold(DiffTotals::default(), |mut totals, summary| {
        totals.files = totals.files.saturating_add(1);
        totals.added = totals.added.saturating_add(summary.added);
        totals.deleted = totals.deleted.saturating_add(summary.deleted);
        totals.hunks = totals.hunks.saturating_add(summary.hunks);
        totals
    })
}

#[must_use]
pub fn diff_summary_label(diff: &str) -> Option<String> {
    let totals = diff_totals(diff);
    (totals.files > 0).then(|| {
        format!(
            "{} file{} +{} -{}",
            totals.files,
            if totals.files == 1 { "" } else { "s" },
            totals.added,
            totals.deleted,
        )
    })
}

impl DiffFileSummary<'_> {
    fn empty() -> Self {
        Self {
            path: "<file>",
            added: 0,
            deleted: 0,
            hunks: 0,
        }
    }

    fn has_changes(&self) -> bool {
        self.added > 0 || self.deleted > 0 || self.hunks > 0
    }
}

fn parse_diff_git_path(line: &str) -> Option<&str> {
    line.split_whitespace()
        .nth(3)
        .map(|path| path.trim_start_matches("b/"))
}

fn render_diff_summary(
    diff: &str,
    totals: DiffTotals,
    width: u16,
    collector: &mut BoundedLineCollector,
) {
    render_styled_text(
        &[&format!(
            "summary: {} file{}, +{} -{}, {} hunk{}",
            totals.files,
            if totals.files == 1 { "" } else { "s" },
            totals.added,
            totals.deleted,
            totals.hunks,
            if totals.hunks == 1 { "" } else { "s" },
        )],
        Style::default()
            .fg(palette::TEXT_PRIMARY)
            .add_modifier(Modifier::BOLD),
        width,
        0,
        collector,
    );
    for summary in summarize_diff(diff) {
        let counts = format!(
            "  +{} -{}  {} hunk{}",
            summary.added,
            summary.deleted,
            summary.hunks,
            if summary.hunks == 1 { "" } else { "s" }
        );
        render_styled_text(
            &["  ", summary.path, &counts],
            Style::default().fg(palette::TEXT_MUTED),
            width,
            0,
            collector,
        );
    }
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let mut parts = line.split_whitespace();
    parts.next()?;
    let old = parts.next()?.trim_start_matches('-');
    let new = parts.next()?.trim_start_matches('+');
    Some((
        old.split(',').next()?.parse().ok()?,
        new.split(',').next()?.parse().ok()?,
    ))
}

fn render_header_line(line: &str, width: u16, origin: usize, collector: &mut BoundedLineCollector) {
    render_styled_text(
        &[line],
        Style::default()
            .fg(palette::WHALE_ACTION)
            .add_modifier(Modifier::BOLD),
        width,
        origin,
        collector,
    );
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
/// Over-budget pairs and pairs sharing too few words stay plainly tinted.
fn intraline_segments(
    old: &str,
    new: &str,
    byte_budget: &mut usize,
    work_budget: &mut usize,
) -> Option<(Vec<Segment>, Vec<Segment>)> {
    let bytes = old.len().checked_add(new.len())?;
    if bytes > INTRALINE_MAX_BYTES || bytes > *byte_budget {
        return None;
    }
    let old_tokens = old
        .split_word_bounds()
        .take(INTRALINE_MAX_TOKENS + 1)
        .count();
    let new_tokens = new
        .split_word_bounds()
        .take(INTRALINE_MAX_TOKENS + 1)
        .count();
    let work = old_tokens.checked_mul(new_tokens)?.max(bytes);
    if old_tokens > INTRALINE_MAX_TOKENS
        || new_tokens > INTRALINE_MAX_TOKENS
        || work > INTRALINE_MAX_WORK
        || work > *work_budget
    {
        return None;
    }
    *byte_budget -= bytes;
    *work_budget -= work;
    let diff = TextDiff::from_unicode_words(old, new);
    let mut old_segments = Vec::new();
    let mut new_segments = Vec::new();
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

/// Paint a retained wrapped chunk, mapping its non-whitespace characters back
/// to source flags. A mismatch keeps the whole row plainly tinted.
fn emphasised_chunk(
    chunk: &str,
    style: Style,
    source: &[(char, bool)],
    mut cursor: usize,
) -> Option<Vec<Span<'static>>> {
    let emphasis = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_flag = false;
    let mut prev_flag = false;
    for ch in chunk.chars() {
        let flag = if ch.is_whitespace() {
            prev_flag && source.get(cursor).is_some_and(|(_, next)| *next)
        } else {
            let (expected, flag) = *source.get(cursor)?;
            if expected != ch {
                return None;
            }
            cursor += 1;
            prev_flag = flag;
            flag
        };
        if flag != run_flag && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_flag { emphasis } else { style },
            ));
        }
        run_flag = flag;
        run.push(ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_flag { emphasis } else { style }));
    }
    Some(spans)
}

fn increment_line(marker: char, old_line: &mut Option<usize>, new_line: &mut Option<usize>) {
    if marker != '+'
        && let Some(line) = old_line.as_mut()
    {
        *line = line.saturating_add(1);
    }
    if marker != '-'
        && let Some(line) = new_line.as_mut()
    {
        *line = line.saturating_add(1);
    }
}

fn render_plain_diff_line(
    collector: &mut BoundedLineCollector,
    content: &str,
    width: u16,
    origin: usize,
    old_line: &mut Option<usize>,
    new_line: &mut Option<usize>,
    marker: char,
) {
    let style = match marker {
        '-' => deleted_style(),
        '+' => added_style(),
        _ => Style::default().fg(palette::TEXT_PRIMARY),
    };
    render_diff_line(
        content, width, *old_line, *new_line, marker, style, None, origin, collector,
    );
    increment_line(marker, old_line, new_line);
}

fn flush_plain_run(
    collector: &mut BoundedLineCollector,
    lines: &[RunLine<'_>],
    sign: char,
    width: u16,
    old_line: &mut Option<usize>,
    new_line: &mut Option<usize>,
) {
    for line in lines {
        render_plain_diff_line(
            collector,
            line.content,
            width,
            line.origin,
            old_line,
            new_line,
            sign,
        );
        line.render_markers(width, collector);
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
    origin: usize,
    collector: &mut BoundedLineCollector,
) {
    let width = width.max(1);
    let full_prefix = format_line_numbers(old_line, new_line, marker);
    let prefix = if full_prefix.width() < usize::from(width) {
        full_prefix
    } else if width > 2 {
        format!("{marker} ")
    } else if width == 2 {
        marker.to_string()
    } else {
        String::new()
    };
    let prefix_width = prefix.width();
    // The whole logical row carries the change tint — numbers included. A
    // bare gutter next to a painted body read as two unrelated strips.
    let gutter_style = match style.bg {
        Some(bg) => Style::default().fg(palette::TEXT_MUTED).bg(bg),
        None => Style::default().fg(palette::TEXT_MUTED),
    };
    let available = usize::from(width.saturating_sub(prefix_width as u16).max(1));
    let source = emphasis.map(|segments| {
        segments
            .iter()
            .flat_map(|(run, flag)| {
                run.chars()
                    .filter(|ch| !ch.is_whitespace())
                    .map(move |ch| (ch, *flag))
            })
            .collect::<Vec<_>>()
    });
    let capture = collector.capture(MAX_ROW_BYTES.saturating_sub(prefix.len()), available);
    let continuation = " ".repeat(prefix_width);
    wrap_parts(&[content], available, prefix_width, capture, |row| {
        let gutter = if row.index == 0 {
            prefix.as_str()
        } else {
            continuation.as_str()
        };
        let nonspace_start = row.nonspace_start;
        let clipped = row.clipped_bytes > 0;
        collector.push(row, origin, gutter, |text| {
            let mut spans = vec![Span::styled(gutter.to_string(), gutter_style)];
            if !clipped
                && let Some(painted) = source
                    .as_ref()
                    .and_then(|source| emphasised_chunk(&text, style, source, nonspace_start))
            {
                spans.extend(painted);
            } else {
                spans.push(Span::styled(text, style));
            }
            Line::from(spans)
        });
    });
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

fn render_styled_text(
    parts: &[&str],
    style: Style,
    width: u16,
    origin: usize,
    collector: &mut BoundedLineCollector,
) {
    let capture = collector.capture(MAX_ROW_BYTES, usize::from(width.max(1)));
    wrap_parts(parts, usize::from(width.max(1)), 0, capture, |row| {
        collector.push(row, origin, "", |text| {
            Line::from(Span::styled(text, style))
        });
    });
}

struct WrappedRow {
    text: String,
    index: usize,
    nonspace_start: usize,
    clipped_bytes: usize,
}

struct RowBuffer {
    text: String,
    bytes: usize,
    nonspace_start: usize,
    nonspace_end: usize,
    keep: bool,
    limit: usize,
    columns: usize,
    stopped: bool,
    extra_clipped_bytes: usize,
}

impl RowBuffer {
    fn new(keep: bool, limit: usize, columns: usize, nonspace_start: usize) -> Self {
        Self {
            text: String::new(),
            bytes: 0,
            nonspace_start,
            nonspace_end: nonspace_start,
            keep,
            limit,
            columns,
            stopped: false,
            extra_clipped_bytes: 0,
        }
    }

    fn push(&mut self, text: &str) {
        self.append(text, text.chars().filter(|ch| !ch.is_whitespace()).count());
    }

    fn whitespace(&mut self, text: &str) {
        self.append(text, 0);
    }

    fn lead(&mut self, lead: &DisplayLead) {
        self.whitespace(&lead.text);
        let omitted = lead.bytes.saturating_sub(lead.text.len());
        self.bytes = self.bytes.saturating_add(omitted);
        self.stopped |= omitted > 0;
    }

    fn append(&mut self, text: &str, nonspace: usize) {
        self.bytes = self.bytes.saturating_add(text.len());
        self.nonspace_end = self.nonspace_end.saturating_add(nonspace);
        if !self.keep || self.stopped {
            return;
        }
        let remaining = self.limit.saturating_sub(self.text.len());
        let kept = if text.len() <= remaining {
            text.len()
        } else {
            self.stopped = true;
            text.grapheme_indices(true)
                .take_while(|(start, grapheme)| start.saturating_add(grapheme.len()) <= remaining)
                .map(|(start, grapheme)| start + grapheme.len())
                .last()
                .unwrap_or(0)
        };
        self.reserve(kept);
        self.text.push_str(&text[..kept]);
    }

    fn reserve(&mut self, additional: usize) {
        let needed = self.text.len().saturating_add(additional);
        if needed > self.text.capacity() {
            let capacity = needed
                .max(self.text.capacity().saturating_mul(2))
                .max(64)
                .min(self.limit);
            self.text
                .reserve_exact(capacity.saturating_sub(self.text.len()));
        }
    }

    fn finish(mut self, index: usize) -> WrappedRow {
        let mut clipped_bytes = 0;
        if self.keep && self.bytes > self.text.len() {
            let marker = "…";
            let byte_limit = self.limit.saturating_sub(marker.len());
            let column_limit = self
                .columns
                .saturating_sub(crate::ui_text::text_display_width(marker));
            let mut width = 0usize;
            let mut kept = 0usize;
            for (start, grapheme) in self.text.grapheme_indices(true) {
                let next_width =
                    width.saturating_add(crate::ui_text::grapheme_display_width(grapheme));
                let end = start.saturating_add(grapheme.len());
                if next_width > column_limit || end > byte_limit {
                    break;
                }
                width = next_width;
                kept = end;
            }
            self.text.truncate(kept);
            clipped_bytes = self.bytes.saturating_sub(self.text.len());
            self.reserve(marker.len());
            self.text.push_str(marker);
        }
        WrappedRow {
            text: self.text,
            index,
            nonspace_start: self.nonspace_start,
            clipped_bytes: clipped_bytes.saturating_add(self.extra_clipped_bytes),
        }
    }
}

struct DisplayLead {
    text: String,
    bytes: usize,
    width: usize,
}

fn needs_normalization(text: &str) -> bool {
    if text.is_ascii() {
        return text.bytes().any(|byte| byte < b' ' || byte == 127);
    }
    text.len() > crate::ui_text::MAX_DISPLAY_GRAPHEME_BYTES
        || text.chars().any(|ch| {
            codewhale_secrets::sanitize::is_bidi_control(ch)
                || ch.is_control()
                || matches!(ch, '\u{2028}' | '\u{2029}')
        })
}

fn word_width(word: &str) -> Option<(usize, bool)> {
    let normalize = needs_normalization(word);
    if !normalize {
        return Some((crate::ui_text::text_display_width(word), false));
    }
    let mut width = 0usize;
    let mut any = false;
    crate::ui_text::display_graphemes(std::iter::once((word, 0)), 0, |grapheme| {
        any |= !grapheme.text.is_empty();
        width = width.saturating_add(grapheme.width);
    });
    any.then_some((width, true))
}

fn word_graphemes(word: &str, normalize: bool, mut emit: impl FnMut(&str, usize)) {
    if normalize {
        crate::ui_text::display_graphemes(std::iter::once((word, 0)), 0, |grapheme| {
            let mut clipped = grapheme.clipped_bytes;
            for text in grapheme.text.graphemes(true) {
                emit(text, clipped);
                clipped = 0;
            }
        });
    } else {
        for text in word.graphemes(true) {
            emit(text, 0);
        }
    }
}

fn wrap_parts(
    parts: &[&str],
    width: usize,
    start_column: usize,
    capture: RowCapture,
    mut emit: impl FnMut(WrappedRow),
) {
    let width = width.max(1);
    let mut lead_parts = Vec::with_capacity(parts.len());
    for part in parts {
        let length = part.len() - part.trim_start().len();
        lead_parts.push(&part[..length]);
        if length < part.len() {
            break;
        }
    }
    let mut lead_buffer = RowBuffer::new(true, MAX_ROW_BYTES, usize::MAX, 0);
    let mut lead_width = 0usize;
    if lead_parts.iter().any(|part| needs_normalization(part)) {
        crate::ui_text::display_graphemes(
            lead_parts.iter().map(|&part| (part, 0)),
            start_column,
            |grapheme| {
                lead_buffer.whitespace(grapheme.text);
                lead_width = lead_width.saturating_add(grapheme.width);
            },
        );
    } else {
        for part in &lead_parts {
            lead_buffer.whitespace(part);
            lead_width = lead_width.saturating_add(crate::ui_text::text_display_width(part));
        }
    }
    let lead = DisplayLead {
        text: lead_buffer.text,
        bytes: lead_buffer.bytes,
        width: lead_width,
    };
    let mut row_index = 0usize;
    let mut current = RowBuffer::new(capture.keeps(row_index), capture.bytes, capture.columns, 0);
    let repeat_lead = lead.width < width;
    let repeat_width = if repeat_lead { lead.width } else { 0 };
    let mut current_width = 0usize;
    let mut has_word = false;
    let flush = |current: RowBuffer, row_index: &mut usize, emit: &mut dyn FnMut(WrappedRow)| {
        let next_nonspace = current.nonspace_end;
        emit(current.finish(*row_index));
        *row_index = row_index.saturating_add(1);
        RowBuffer::new(
            capture.keeps(*row_index),
            capture.bytes,
            capture.columns,
            next_nonspace,
        )
    };
    if repeat_lead {
        current.lead(&lead);
        current_width = lead.width;
    } else {
        let mut visit = |text: &str| {
            let cell_width = crate::ui_text::grapheme_display_width(text);
            if current_width.saturating_add(cell_width) > width && current_width > 0 {
                let next =
                    RowBuffer::new(false, capture.bytes, capture.columns, current.nonspace_end);
                current = flush(
                    std::mem::replace(&mut current, next),
                    &mut row_index,
                    &mut emit,
                );
                current_width = 0;
            }
            if cell_width > width {
                current.push("…");
                current.extra_clipped_bytes =
                    current.extra_clipped_bytes.saturating_add(text.len());
                current_width = current_width.saturating_add(1);
            } else {
                current.whitespace(text);
                current_width = current_width.saturating_add(cell_width);
            }
        };
        if lead_parts.iter().any(|part| needs_normalization(part)) {
            crate::ui_text::display_graphemes(
                lead_parts.iter().map(|&part| (part, 0)),
                start_column,
                |grapheme| {
                    for text in grapheme.text.graphemes(true) {
                        visit(text);
                    }
                },
            );
        } else {
            for part in &lead_parts {
                for text in part.graphemes(true) {
                    visit(text);
                }
            }
        }
    }
    for word in parts.iter().flat_map(|part| part.split_whitespace()) {
        let Some((word_width, normalize)) = word_width(word) else {
            continue;
        };
        if word_width > width {
            if has_word {
                current = flush(current, &mut row_index, &mut emit);
                if repeat_lead {
                    current.lead(&lead);
                }
                current_width = repeat_width;
            }
            word_graphemes(word, normalize, |text, clipped| {
                let grapheme_width = crate::ui_text::grapheme_display_width(text);
                if current_width.saturating_add(grapheme_width) > width && current_width > 0 {
                    let next =
                        RowBuffer::new(false, capture.bytes, capture.columns, current.nonspace_end);
                    current = flush(
                        std::mem::replace(&mut current, next),
                        &mut row_index,
                        &mut emit,
                    );
                    current_width = 0;
                }
                if grapheme_width > width {
                    current.push("…");
                    current.extra_clipped_bytes = current
                        .extra_clipped_bytes
                        .saturating_add(text.len())
                        .saturating_add(clipped);
                    current_width = current_width.saturating_add(1);
                } else {
                    current.push(text);
                    current.extra_clipped_bytes =
                        current.extra_clipped_bytes.saturating_add(clipped);
                    current_width = current_width.saturating_add(grapheme_width);
                }
            });
            has_word = current.nonspace_end > current.nonspace_start;
            continue;
        }
        let additional = if has_word {
            word_width.saturating_add(1)
        } else {
            word_width
        };
        if current_width.saturating_add(additional) > width && has_word {
            current = flush(current, &mut row_index, &mut emit);
            if repeat_lead {
                current.lead(&lead);
            }
            current_width = repeat_width;
            has_word = false;
        }
        if has_word {
            current.whitespace(" ");
            current_width = current_width.saturating_add(1);
        }
        if current_width.saturating_add(word_width) > width && !has_word && current_width > 0 {
            current = flush(current, &mut row_index, &mut emit);
            current_width = 0;
        }
        if current_width == 0
            && current.bytes == 0
            && repeat_width > 0
            && word_width.saturating_add(repeat_width) <= width
        {
            current.lead(&lead);
            current_width = repeat_width;
        }
        if normalize {
            crate::ui_text::display_graphemes(std::iter::once((word, 0)), 0, |grapheme| {
                current.push(grapheme.text);
                current.extra_clipped_bytes = current
                    .extra_clipped_bytes
                    .saturating_add(grapheme.clipped_bytes);
            });
        } else {
            current.push(word);
        }
        current_width = current_width.saturating_add(word_width);
        has_word = true;
    }
    if has_word || current.bytes > 0 || row_index == 0 {
        emit(current.finish(row_index));
    }
}

#[cfg(test)]
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    wrap_parts(
        &[text],
        width,
        0,
        RowCapture {
            start: 0,
            end: MAX_RETAINED_ROWS,
            search: false,
            bytes: MAX_ROW_BYTES,
            columns: width.max(1),
        },
        |row| {
            if row.index < MAX_RETAINED_ROWS {
                lines.push(row.text);
            }
        },
    );
    lines
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
        render_diff_body_bounded(diff, width, MAX_RETAINED_ROWS).lines
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

        let summaries = summarize_diff(diff).collect::<Vec<_>>();
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

        let full_row_count = render_diff_body_bounded(&diff, 32, 0).omitted_rows;
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

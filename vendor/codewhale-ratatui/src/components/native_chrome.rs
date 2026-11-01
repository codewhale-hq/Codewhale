//! Portable extraction of the mounted Codewhale TUI composer and its
//! borderless workflow progress rows. Source: `crates/tui/src/tui/widgets/
//! workbar.rs`, `widgets/mod.rs`, `composer_chrome.rs`, and `ui/frame.rs`.
//! Caller data replaces App and WorkflowPanel; geometry, density, columns,
//! failure bars, prompt and send chrome retain their native grammar.
//!
//! Adapted from Codewhale, licensed under the MIT License:
//! Copyright (c) 2024-2025 DeepSeek-TUI Contributors
//!
//! Permission is hereby granted, free of charge, to any person obtaining a
//! copy of this software and associated documentation files (the "Software"),
//! to deal in the Software without restriction, including without limitation
//! the rights to use, copy, modify, merge, publish, distribute, sublicense,
//! and/or sell copies of the Software, and to permit persons to whom the
//! Software is furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included
//! in all copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
//! THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
//! FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
//! DEALINGS IN THE SOFTWARE.

use std::{borrow::Cow, time::Duration};

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{Paint, Role, Theme, TuiGround, TuiInk, glyphs, text};

const BAR_CELLS: usize = 20;
const MAX_RUN_ROWS: usize = 6;
const GAP: &str = "  ";

/// The workflow lifecycle reported by the host, without inferred outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowRunState {
    Pending,
    Running,
    Succeeded,
    Degraded,
    Failed,
    Cancelled,
}
impl WorkflowRunState {
    fn mark(self, ascii: bool) -> &'static str {
        glyphs::pick(
            match self {
                Self::Pending => "○",
                Self::Running => "•",
                Self::Succeeded => "✓",
                Self::Degraded => "◆",
                Self::Failed => "✕",
                Self::Cancelled => "⊘",
            },
            ascii,
        )
    }
    fn ink(self, theme: &Theme) -> Style {
        match self {
            Self::Pending | Self::Cancelled => theme.fg(Role::Muted),
            Self::Running => theme.tui_ink(TuiInk::Working),
            Self::Succeeded => theme.tui_ink(TuiInk::Success),
            Self::Degraded => theme.tui_ink(TuiInk::Warning),
            Self::Failed => theme.fg(Role::Danger),
        }
    }
}

/// Host-reported row outcomes and facts for one run. Successful and failed
/// counts stay separate; cancellation never contributes to the success bar.
#[derive(Clone, Debug)]
pub struct WorkflowRun<'a> {
    pub title: Cow<'a, str>,
    pub state: WorkflowRunState,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub total: usize,
    pub elapsed: Option<Duration>,
    pub tokens: Option<u64>,
    pub queued: usize,
    pub reason: Option<Cow<'a, str>>,
}
impl<'a> WorkflowRun<'a> {
    pub fn new(title: impl Into<Cow<'a, str>>, state: WorkflowRunState) -> Self {
        Self {
            title: title.into(),
            state,
            succeeded: 0,
            failed: 0,
            cancelled: 0,
            total: 0,
            elapsed: None,
            tokens: None,
            queued: 0,
            reason: None,
        }
    }
    pub const fn outcomes(
        mut self,
        succeeded: usize,
        failed: usize,
        cancelled: usize,
        total: usize,
    ) -> Self {
        self.succeeded = succeeded;
        self.failed = failed;
        self.cancelled = cancelled;
        self.total = total;
        self
    }
    pub const fn elapsed(mut self, elapsed: Duration) -> Self {
        self.elapsed = Some(elapsed);
        self
    }
    pub const fn tokens(mut self, tokens: u64) -> Self {
        self.tokens = Some(tokens);
        self
    }
    pub const fn queued(mut self, queued: usize) -> Self {
        self.queued = queued;
        self
    }
    pub fn reason(mut self, reason: impl Into<Cow<'a, str>>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

/// Localized words replace the native English defaults without changing the
/// row grammar. Count fields also accept templates: `done` may contain
/// `{done}` and `{total}`; `failed`, `cancelled`, `queued` and `more` may
/// contain `{count}`. Templates preserve a language's word and count order.
/// A host may change the shortcut shown beside folded runs.
#[derive(Clone, Debug)]
pub struct WorkflowProgressWords {
    pub done: Cow<'static, str>,
    pub failed: Cow<'static, str>,
    pub cancelled: Cow<'static, str>,
    pub queued: Cow<'static, str>,
    pub no_tasks: Cow<'static, str>,
    pub large: Cow<'static, str>,
    pub gaps: Cow<'static, str>,
    pub stopped: Cow<'static, str>,
    pub more: Cow<'static, str>,
    pub manage: Cow<'static, str>,
}
impl Default for WorkflowProgressWords {
    fn default() -> Self {
        Self {
            done: "done".into(),
            failed: "failed".into(),
            cancelled: "cancelled".into(),
            queued: "queued".into(),
            no_tasks: "No tasks yet".into(),
            large: "Large workflow".into(),
            gaps: "finished with gaps".into(),
            stopped: "stopped".into(),
            more: "more".into(),
            manage: "to manage".into(),
        }
    }
}

/// The native workbar: borderless progress, one row per workflow, directly
/// below the posture bar and above metrics in the mounted TUI compositor.
#[derive(Clone, Debug)]
pub struct WorkflowProgress<'a> {
    pub runs: Vec<WorkflowRun<'a>>,
    pub words: WorkflowProgressWords,
}
impl<'a> WorkflowProgress<'a> {
    pub fn new(runs: Vec<WorkflowRun<'a>>) -> Self {
        Self {
            runs,
            words: WorkflowProgressWords::default(),
        }
    }
    pub fn words(mut self, words: WorkflowProgressWords) -> Self {
        self.words = words;
        self
    }
    pub fn desired_rows(&self) -> u16 {
        Self::desired_rows_for(self.runs.len())
    }
    /// Reserve the native row budget before constructing the visible runs.
    pub const fn desired_rows_for(runs: usize) -> u16 {
        if runs > MAX_RUN_ROWS {
            (MAX_RUN_ROWS + 1) as u16
        } else {
            runs as u16
        }
    }
    /// Shared columns align visible runs. Narrow widths shed bar, tokens,
    /// tail reservation and elapsed in the same order as the native workbar.
    pub fn lines(&self, width: u16, max_rows: usize, theme: &Theme) -> Vec<Line<'static>> {
        let width = usize::from(width);
        let max_rows = max_rows.min(usize::from(self.desired_rows()));
        if self.runs.is_empty() || max_rows == 0 || width < 8 {
            return Vec::new();
        }
        let shown = if self.runs.len() > max_rows {
            max_rows.saturating_sub(1)
        } else {
            self.runs.len()
        };
        let cells: Vec<_> = self.runs[..shown]
            .iter()
            .map(|run| RunCells::new(run, &self.words, theme.ascii()))
            .collect();
        let layout = ProgressLayout::fit(&cells, width.saturating_sub(1), theme.ascii());
        let mut rows: Vec<_> = cells
            .iter()
            .map(|row| row.line(&layout, width, theme))
            .collect();
        let hidden = self.runs.len() - shown;
        if hidden > 0 {
            let more = if self.words.more.contains("{count}") {
                safe(&self.words.more).replace("{count}", &hidden.to_string())
            } else {
                format!("+{hidden} {}", safe(&self.words.more))
            };
            let line = format!(
                " {more} {} {} {}",
                glyphs::pick("·", theme.ascii()),
                glyphs::pick("↓", theme.ascii()),
                safe(&self.words.manage)
            );
            rows.push(Line::styled(plain_clip(&line, width), theme.fg(Role::Hint)));
        }
        rows
    }
}
impl Paint for WorkflowProgress<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        for (index, line) in self
            .lines(area.width, usize::from(area.height), theme)
            .into_iter()
            .enumerate()
        {
            buf.set_line(area.x, area.y + index as u16, &line, area.width);
        }
    }
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        self.desired_rows()
    }
}

fn safe(value: &str) -> String {
    text::display_safe(value).into_owned()
}
fn counted(value: &str, count: usize) -> String {
    let value = safe(value);
    if value.contains("{count}") {
        value.replace("{count}", &count.to_string())
    } else {
        format!("{count} {value}")
    }
}
fn sentence(value: &str) -> String {
    let flat = value
        .split_whitespace()
        .map(safe)
        .collect::<Vec<_>>()
        .join(" ");
    let mut end = flat.len();
    let mut chars = flat.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == ';'
            || (matches!(ch, '.' | '!' | '?') && chars.peek().is_some_and(|(_, c)| *c == ' '))
        {
            end = if matches!(ch, '.' | ';') {
                index
            } else {
                index + ch.len_utf8()
            };
            break;
        }
    }
    flat[..end].trim_end_matches(['.', ' ']).trim().to_owned()
}
fn reason(value: &str) -> String {
    let value = value.trim_start();
    let value = value
        .strip_prefix('[')
        .and_then(|v| v.split_once("] "))
        .filter(|(tag, _)| {
            !tag.is_empty()
                && tag.len() <= 24
                && tag
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .map_or(value, |(_, rest)| rest);
    sentence(value)
}
fn elapsed(value: Duration) -> String {
    if value.is_zero() {
        "0s".into()
    } else if value < Duration::from_secs(1) {
        format!("{}ms", value.as_millis())
    } else if value.as_secs() < 60 {
        format!("{}s", value.as_secs())
    } else {
        format!("{}m {:02}s", value.as_secs() / 60, value.as_secs() % 60)
    }
}
fn tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}k", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}
fn bar_cells(succeeded: usize, failed: usize, total: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let cells = |n: usize| {
        let count = n as u128;
        let total = total as u128;
        let result =
            ((count * BAR_CELLS as u128 + total / 2) / total).min(BAR_CELLS as u128) as usize;
        if n > 0 { result.max(1) } else { 0 }
    };
    let failed = cells(failed);
    (cells(succeeded).min(BAR_CELLS - failed), failed)
}

struct RunCells {
    state: WorkflowRunState,
    name: String,
    done_cells: usize,
    failed_cells: usize,
    done: String,
    problems: String,
    elapsed: String,
    tokens: Option<String>,
    chips: Vec<String>,
    reason: Option<String>,
}
impl RunCells {
    fn new(run: &WorkflowRun<'_>, words: &WorkflowProgressWords, ascii: bool) -> Self {
        let sep = if ascii { " . " } else { " · " };
        let done = if run.total > 0 {
            if words.done.contains("{done}") || words.done.contains("{total}") {
                safe(&words.done)
                    .replace("{done}", &run.succeeded.to_string())
                    .replace("{total}", &run.total.to_string())
            } else {
                format!("{}/{} {}", run.succeeded, run.total, safe(&words.done))
            }
        } else if run.failed == 0 {
            safe(&words.no_tasks)
        } else {
            String::new()
        };
        let mut problems = Vec::new();
        if run.failed > 0 {
            problems.push(counted(&words.failed, run.failed));
        }
        if run.cancelled > 0 {
            problems.push(counted(&words.cancelled, run.cancelled));
        }
        let problems = if problems.is_empty() {
            String::new()
        } else {
            format!(
                "{}{text}",
                if done.is_empty() { "" } else { sep },
                text = problems.join(sep)
            )
        };
        let (done_cells, failed_cells) = bar_cells(run.succeeded, run.failed, run.total);
        let mut chips = Vec::new();
        if run.total >= 25 {
            chips.push(format!(
                "{} {}",
                if ascii { "!" } else { "⚠" },
                safe(&words.large)
            ));
        }
        if run.queued > 0 {
            chips.push(format!(
                "{} {}",
                glyphs::pick("·", ascii),
                counted(&words.queued, run.queued)
            ));
        }
        let why = run.reason.as_deref().map(reason).filter(|s| !s.is_empty());
        let reason = match run.state {
            WorkflowRunState::Failed => Some(why.unwrap_or_else(|| {
                if words.failed.contains("{count}") {
                    counted(&words.failed, run.failed)
                } else {
                    safe(&words.failed)
                }
            })),
            WorkflowRunState::Degraded => Some(why.map_or_else(
                || safe(&words.gaps),
                |why| format!("{}{sep}{why}", safe(&words.gaps)),
            )),
            WorkflowRunState::Cancelled => Some(safe(&words.stopped)),
            _ => None,
        };
        let name = sentence(&run.title);
        Self {
            state: run.state,
            name: text::truncate_words(if name.is_empty() { "workflow" } else { &name }, 40, ascii)
                .into_owned(),
            done_cells,
            failed_cells,
            done,
            problems,
            elapsed: run.elapsed.map(elapsed).unwrap_or_default(),
            tokens: run
                .tokens
                .map(|n| format!("{}{n}", glyphs::pick("↓", ascii), n = tokens(n))),
            chips,
            reason,
        }
    }
    fn progress_width(&self) -> usize {
        text::width(&self.done) + text::width(&self.problems)
    }
    fn tail_width(&self) -> usize {
        self.chips.iter().map(|s| text::width(s) + 1).sum::<usize>()
            + self.reason.as_deref().map_or(0, |s| text::width(s) + 1)
    }
    fn line(&self, layout: &ProgressLayout, width: usize, theme: &Theme) -> Line<'static> {
        let mut spans = vec![
            Span::raw(" "),
            Span::styled(
                format!("{} ", self.state.mark(theme.ascii())),
                self.state.ink(theme),
            ),
            Span::styled(
                text::pad(
                    &text::truncate_words(&self.name, layout.name_room, theme.ascii()),
                    layout.name_cols,
                    theme.ascii(),
                ),
                theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
            ),
        ];
        if layout.bar {
            spans.extend([
                Span::raw(GAP),
                Span::styled(
                    glyphs::pick("█", theme.ascii()).repeat(self.done_cells),
                    theme.tui_ink(TuiInk::Success),
                ),
                Span::styled(
                    glyphs::pick("×", theme.ascii()).repeat(self.failed_cells),
                    theme.fg(Role::Danger),
                ),
                Span::styled(
                    glyphs::pick("░", theme.ascii())
                        .repeat(BAR_CELLS - self.done_cells - self.failed_cells),
                    theme.fg(Role::Hint),
                ),
            ]);
        }
        spans.extend([
            Span::raw(GAP),
            Span::styled(self.done.clone(), theme.fg(Role::Muted)),
            Span::styled(self.problems.clone(), theme.fg(Role::Danger)),
            Span::raw(" ".repeat(layout.progress_cols.saturating_sub(self.progress_width()))),
        ]);
        if layout.elapsed_cols > 0 {
            spans.extend([
                Span::raw(GAP),
                Span::styled(
                    text::pad(&self.elapsed, layout.elapsed_cols, theme.ascii()),
                    theme.fg(Role::Muted),
                ),
            ]);
        }
        if layout.tokens_cols > 0 {
            spans.extend([
                Span::raw(GAP),
                Span::styled(
                    text::pad(
                        self.tokens.as_deref().unwrap_or(""),
                        layout.tokens_cols,
                        theme.ascii(),
                    ),
                    theme.fg(Role::Muted),
                ),
            ]);
        }
        let mut used = spans.iter().map(|s| text::width(&s.content)).sum::<usize>();
        for (index, chip) in self.chips.iter().enumerate() {
            let gap = if index == 0 { GAP } else { " " };
            spans.extend([
                Span::raw(gap),
                Span::styled(chip.clone(), theme.tui_ink(TuiInk::Warning)),
            ]);
            used += gap.len() + text::width(chip);
        }
        if let Some(reason) = &self.reason {
            let gap = if self.chips.is_empty() { GAP } else { " " };
            let room = width.saturating_sub(used + gap.len());
            if room >= 12.min(text::width(reason)) {
                spans.extend([
                    Span::raw(gap),
                    Span::styled(
                        text::truncate_words(reason, room, theme.ascii()).into_owned(),
                        self.state.ink(theme),
                    ),
                ]);
            }
        }
        clip(spans, width)
    }
}
struct ProgressLayout {
    name_room: usize,
    name_cols: usize,
    bar: bool,
    progress_cols: usize,
    elapsed_cols: usize,
    tokens_cols: usize,
}
impl ProgressLayout {
    fn fit(rows: &[RunCells], width: usize, ascii: bool) -> Self {
        let widest = |f: &dyn Fn(&RunCells) -> usize| rows.iter().map(f).max().unwrap_or(0);
        let name_want = widest(&|r| text::width(&r.name));
        let mut tail_want = widest(&RunCells::tail_width).min(24);
        let mut layout = Self {
            name_room: 0,
            name_cols: 0,
            bar: width >= 72,
            progress_cols: widest(&RunCells::progress_width),
            elapsed_cols: widest(&|r| text::width(&r.elapsed)),
            tokens_cols: widest(&|r| r.tokens.as_deref().map_or(0, text::width)),
        };
        let room = loop {
            let fixed = 2
                + if layout.bar { BAR_CELLS + GAP.len() } else { 0 }
                + GAP.len()
                + layout.progress_cols
                + if layout.elapsed_cols > 0 {
                    GAP.len() + layout.elapsed_cols
                } else {
                    0
                }
                + if layout.tokens_cols > 0 {
                    GAP.len() + layout.tokens_cols
                } else {
                    0
                };
            let room = width.saturating_sub(fixed + tail_want);
            if room >= 12.min(name_want) {
                break name_want.min(room.max(12.min(name_want)));
            }
            if layout.bar {
                layout.bar = false;
            } else if layout.tokens_cols > 0 {
                layout.tokens_cols = 0;
            } else if tail_want > 0 {
                tail_want = 0;
            } else if layout.elapsed_cols > 0 {
                layout.elapsed_cols = 0;
            } else {
                break width.saturating_sub(fixed).max(1).min(name_want.max(1));
            }
        };
        layout.name_room = room;
        layout.name_cols = widest(&|r| text::width(&text::truncate_words(&r.name, room, ascii)));
        layout
    }
}
fn clip(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut used = 0;
    let mut output = Vec::new();
    for span in spans {
        let cells = text::width(&span.content);
        if used + cells <= width {
            used += cells;
            output.push(span);
        } else {
            let room = width.saturating_sub(used);
            if room > 0 {
                output.push(Span::styled(plain_clip(&span.content, room), span.style));
            }
            break;
        }
    }
    Line::from(output)
}
fn plain_clip(value: &str, width: usize) -> String {
    let mut cells = 0;
    value
        .graphemes(true)
        .take_while(|g| {
            let next = text::width(g);
            if cells + next > width {
                false
            } else {
                cells += next;
                true
            }
        })
        .collect()
}

/// The mounted composer's content floor and total-row cap while a menu is open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NativeComposerDensity {
    Compact,
    #[default]
    Comfortable,
    Spacious,
}
impl NativeComposerDensity {
    pub const fn max_rows(self) -> u16 {
        match self {
            Self::Compact => 7,
            Self::Comfortable => 9,
            Self::Spacious => 12,
        }
    }
}

/// Shared render and hit-test geometry. Text never claims the submit target
/// or its breathing cell. A compact/quiet enclosure has no submit rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeComposerGeometry {
    pub inner: Rect,
    pub text: Rect,
    pub submit: Option<Rect>,
    pub prompt_x: Option<u16>,
}

/// The actual mounted native composer chrome, over caller-owned text.
/// Editing, submit dispatch, menus and input-method state belong to the host.
#[derive(Clone, Debug)]
pub struct NativeComposer<'a> {
    pub text: Cow<'a, str>,
    pub placeholder: Cow<'a, str>,
    pub focused: bool,
    pub enclosed: bool,
    pub density: NativeComposerDensity,
    pub can_submit: bool,
    pub submit_hint: Option<Cow<'a, str>>,
    pub target: Option<Cow<'a, str>>,
    /// Cursor index in graphemes of the displayed text, defaulting to its end.
    pub cursor: Option<usize>,
    pub menu_rows: usize,
}
impl<'a> NativeComposer<'a> {
    pub fn new(text: impl Into<Cow<'a, str>>) -> Self {
        Self {
            text: text.into(),
            placeholder: "Write a task or use /.".into(),
            focused: false,
            enclosed: true,
            density: NativeComposerDensity::Comfortable,
            can_submit: false,
            submit_hint: None,
            target: None,
            cursor: None,
            menu_rows: 0,
        }
    }
    pub const fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
    pub const fn enclosed(mut self, enclosed: bool) -> Self {
        self.enclosed = enclosed;
        self
    }
    pub const fn density(mut self, density: NativeComposerDensity) -> Self {
        self.density = density;
        self
    }
    pub const fn can_submit(mut self, can_submit: bool) -> Self {
        self.can_submit = can_submit;
        self
    }
    pub const fn cursor(mut self, grapheme: usize) -> Self {
        self.cursor = Some(grapheme);
        self
    }
    pub const fn menu_rows(mut self, rows: usize) -> Self {
        self.menu_rows = rows;
        self
    }
    pub fn placeholder(mut self, placeholder: impl Into<Cow<'a, str>>) -> Self {
        self.placeholder = placeholder.into();
        self
    }
    pub fn submit_hint(mut self, hint: impl Into<Cow<'a, str>>) -> Self {
        self.submit_hint = Some(hint.into());
        self
    }
    pub fn target(mut self, target: impl Into<Cow<'a, str>>) -> Self {
        self.target = Some(target.into());
        self
    }
    pub fn has_panel(&self, area: Rect) -> bool {
        self.geometry(area).submit.is_some()
    }
    pub fn geometry(&self, area: Rect) -> NativeComposerGeometry {
        crate::native_composer_geometry(area, self.enclosed, false)
    }
    pub fn desired_height(&self, width: u16, available: u16) -> u16 {
        self.frame(None, None).desired_height(width, available)
    }
    pub fn cursor_position(&self, area: Rect) -> Option<Position> {
        self.frame(None, None).plan(area).cursor
    }
    fn frame(
        &self,
        theme: Option<&Theme>,
        area: Option<Rect>,
    ) -> crate::NativeComposerFrame<'static> {
        use crate::{NativeComposerFrame, NativeComposerMenu, NativeComposerStyles};
        let width = area.map(|area| area.width);
        let panel = area.is_some_and(|area| self.has_panel(area));
        let value = multiline_safe(&self.text);
        let count = self.cursor.unwrap_or_else(|| value.graphemes(true).count());
        let cursor = value
            .graphemes(true)
            .take(count)
            .map(|g| g.chars().count())
            .sum();
        let title = |label: String, room: Option<u16>| match room {
            Some(room) => {
                text::truncate_words(&label, usize::from(room), theme.is_some_and(Theme::ascii))
                    .into_owned()
            }
            None => label,
        };
        let background = theme
            .map(|t| t.tui_ground(TuiGround::Composer))
            .unwrap_or_default();
        let role = |role| theme.map(|t| t.fg(role)).unwrap_or_default();
        let border = background.patch(role(if self.focused {
            Role::Accent
        } else {
            Role::Border
        }));
        let submit = background.patch(if self.can_submit {
            theme.map(|t| t.tui_ink(TuiInk::Info)).unwrap_or_default()
        } else {
            role(Role::Dim)
        });
        NativeComposerFrame {
            text: Cow::Owned(value),
            cursor,
            selection: None,
            placeholder: Line::styled(
                multiline_safe(&self.placeholder),
                background.patch(theme.map(|t| t.tui_ink(TuiInk::Soft)).unwrap_or_default()),
            ),
            enclosed: self.enclosed,
            density: self.density,
            history_search: false,
            focused: self.focused,
            can_submit: self.can_submit,
            ascii: theme.is_some_and(Theme::ascii),
            top_title: None,
            top_right: self.target.as_ref().map(|target| {
                Line::styled(
                    title(
                        format!(" {} ", safe(target)),
                        width.map(|width| width.saturating_sub(2)),
                    ),
                    background
                        .patch(role(Role::Attention))
                        .add_modifier(Modifier::BOLD),
                )
            }),
            hint: self
                .submit_hint
                .as_ref()
                .filter(|_| !self.text.trim().is_empty())
                .map(|hint| {
                    Line::styled(
                        title(
                            format!(" {} ", safe(hint)),
                            width.map(|width| width.saturating_sub(u16::from(panel) * 2)),
                        ),
                        background.patch(role(Role::Accent)),
                    )
                }),
            quiet_hint: self
                .submit_hint
                .as_ref()
                .filter(|_| !self.text.trim().is_empty())
                .map(|hint| {
                    Line::styled(
                        title(
                            format!(" {} ", safe(hint)),
                            width.map(|width| width.saturating_sub(u16::from(panel) * 2)),
                        ),
                        background.patch(role(Role::Accent)),
                    )
                }),
            styles: NativeComposerStyles {
                background,
                border,
                quiet_border: background.patch(role(Role::Border)),
                text: background.patch(role(Role::Foreground)),
                selection: background.patch(role(Role::Foreground)),
                prompt: background.patch(role(Role::Accent)),
                submit: if self.can_submit {
                    submit.add_modifier(Modifier::BOLD)
                } else {
                    submit
                },
            },
            menu: NativeComposerMenu {
                reserved_rows: self.menu_rows,
                ..Default::default()
            },
        }
    }
}
impl Paint for NativeComposer<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_symbol(" ");
            }
        }
        self.frame(Some(theme), Some(area)).render(area, buf);
    }
    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        self.desired_height(width, self.density.max_rows())
    }
}
fn multiline_safe(value: &str) -> String {
    value.split('\n').map(safe).collect::<Vec<_>>().join("\n")
}

//! Pure mounted composer projection over host-owned editor and menu facts.
//! Source positions count Unicode scalar values in the original editor text.
//! Display sanitation never mutates that text or its source-position authority.

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

use crate::{NativeComposerDensity, NativeComposerGeometry, Paint, Theme, text};
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Widget, Wrap},
};
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug)]
pub enum NativeComposerMenuItem {
    Line(Line<'static>),
    Columns {
        name: String,
        description: String,
        prefix: Span<'static>,
        marker: Span<'static>,
        name_style: Style,
        description_style: Style,
    },
}
#[derive(Clone, Debug, Default)]
pub struct NativeComposerMenu {
    pub items: Vec<NativeComposerMenuItem>,
    pub selected: usize,
    pub reserved_rows: usize,
    pub pointer_rows: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeComposerStyles {
    pub background: Style,
    pub border: Style,
    pub quiet_border: Style,
    pub text: Style,
    pub selection: Style,
    pub prompt: Style,
    pub submit: Style,
}

/// Complete render facts. Editing, bindings, menu filtering, history and IME
/// remain host-owned; the frame projects their one current snapshot.
#[derive(Clone, Debug)]
pub struct NativeComposerFrame<'a> {
    pub text: Cow<'a, str>,
    pub cursor: usize,
    pub selection: Option<(usize, usize)>,
    pub placeholder: Line<'static>,
    pub enclosed: bool,
    pub density: NativeComposerDensity,
    pub history_search: bool,
    pub focused: bool,
    pub can_submit: bool,
    pub ascii: bool,
    pub top_title: Option<Line<'static>>,
    pub top_right: Option<Line<'static>>,
    pub hint: Option<Line<'static>>,
    pub quiet_hint: Option<Line<'static>>,
    pub styles: NativeComposerStyles,
    pub menu: NativeComposerMenu,
}
#[derive(Clone, Debug)]
pub struct NativeComposerPlan {
    pub area: Rect,
    pub geometry: NativeComposerGeometry,
    pub cursor: Option<Position>,
    pub scroll_offset: usize,
    pub top_padding: usize,
    pub input_rows_budget: usize,
    pub source_rows: Vec<(usize, String)>,
    pub menu_rects: Vec<(usize, Rect)>,
    pub truncated: Vec<(Rect, String)>,
    prompt_position: Option<Position>,
    lines: Vec<Line<'static>>,
    frame: NativeComposerFrame<'static>,
}

fn safe_span(span: &Span<'static>) -> Span<'static> {
    Span::styled(text::display_safe(&span.content).into_owned(), span.style)
}
fn safe_line(line: &Line<'static>) -> Line<'static> {
    let mut line = line.clone();
    line.spans = line.spans.iter().map(safe_span).collect();
    line
}
pub fn native_composer_input_budget(height: u16, menu: usize) -> usize {
    usize::from(height).saturating_sub(menu).max(1)
}
pub fn native_composer_top_padding(rows: usize, budget: usize) -> usize {
    budget.saturating_sub(rows.max(1).min(budget.max(1))) / 2
}
pub fn native_composer_content_geometry(inner: Rect, history_search: bool) -> (Rect, u16) {
    let inset = if !history_search && inner.width >= 3 {
        2
    } else {
        0
    };
    (
        Rect::new(
            inner.x.saturating_add(inset),
            inner.y,
            inner.width.saturating_sub(inset),
            inner.height,
        ),
        inset,
    )
}
pub fn native_composer_geometry(
    area: Rect,
    enclosed: bool,
    history_search: bool,
) -> NativeComposerGeometry {
    let panel = enclosed && area.width >= NATIVE_COMPOSER_PANEL_MIN_WIDTH && area.height >= 3;
    let (inner, submit) = if panel {
        let submit = Rect::new(
            area.right().saturating_sub(5),
            area.bottom().saturating_sub(2),
            3,
            1,
        );
        let x = area.x.saturating_add(1);
        (
            Rect::new(
                x,
                area.y.saturating_add(1),
                submit.x.saturating_sub(1).saturating_sub(x),
                area.height.saturating_sub(2),
            ),
            Some(submit),
        )
    } else if area.height >= 2 {
        (
            Rect::new(
                area.x,
                area.y.saturating_add(1),
                area.width,
                area.height - 1,
            ),
            None,
        )
    } else {
        (area, None)
    };
    let (text, inset) = native_composer_content_geometry(inner, history_search);
    NativeComposerGeometry {
        inner,
        text,
        submit,
        prompt_x: (inset > 0).then_some(inner.x),
    }
}
pub fn native_composer_height(
    rows: usize,
    menu: usize,
    available: u16,
    density: NativeComposerDensity,
    panel: bool,
) -> u16 {
    let available = available.max(1);
    let floor = match density {
        NativeComposerDensity::Compact => 1,
        NativeComposerDensity::Comfortable => 2,
        NativeComposerDensity::Spacious => 3,
    };
    let border = if panel && available >= 3 {
        2
    } else {
        usize::from(available >= 2)
    };
    let cap = if menu == 0 {
        available
    } else {
        available.min(density.max_rows())
    };
    rows.max(floor)
        .saturating_add(menu)
        .saturating_add(border)
        .clamp(1, usize::from(cap)) as u16
}

/// Display width after the kit's control/bidi guard; raw source scalars remain
/// in each row for caret/selection/pointer projection.
pub fn native_composer_grapheme_width(value: &str) -> usize {
    text::display_safe(value).width()
}
fn source_width(value: &str) -> usize {
    text::display_safe(value).width()
}

/// Lossless soft wrapping: row concatenation reproduces each logical source
/// line, including hidden source scalars; the painter sanitizes display only.
pub fn native_composer_wrap_text(value: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![value.to_owned()];
    }
    if value.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut cells = 0;
    let mut break_at = None;
    macro_rules! flush {
        () => {{
            match break_at.take() {
                Some((byte, _)) if byte < current.len() => {
                    let remainder = current.split_off(byte);
                    lines.push(std::mem::replace(&mut current, remainder));
                    cells = source_width(&current)
                }
                _ => {
                    lines.push(std::mem::take(&mut current));
                    cells = 0
                }
            }
        }};
    }
    for g in value.graphemes(true) {
        if g == "\n" {
            break_at = None;
            lines.push(std::mem::take(&mut current));
            cells = 0;
            continue;
        }
        let count = native_composer_grapheme_width(g);
        if cells + count > width && cells != 0 {
            flush!()
        }
        current.push_str(g);
        cells += count;
        if g == " " && !current.trim_start().is_empty() {
            break_at = Some((current.len(), cells))
        }
        if cells >= width {
            flush!()
        }
    }
    lines.push(current);
    lines
}
pub fn native_composer_source_rows(value: &str, width: usize) -> Vec<(usize, String)> {
    if value.is_empty() || width == 0 {
        return vec![(0, String::new())];
    }
    let mut result = Vec::new();
    let mut start = 0;
    for raw in value.split('\n') {
        for row in native_composer_wrap_text(raw, width) {
            let chars = row.chars().count();
            result.push((start, row));
            start += chars
        }
        start += 1
    }
    result
}
pub fn native_composer_source_cursor(rows: &[(usize, String)], cursor: usize) -> (usize, usize) {
    let row = rows
        .iter()
        .rposition(|(start, _)| *start <= cursor)
        .unwrap_or(0);
    let Some((start, line)) = rows.get(row) else {
        return (0, 0);
    };
    let end = line
        .char_indices()
        .nth(cursor.saturating_sub(*start))
        .map(|(at, _)| at)
        .unwrap_or(line.len());
    (row, source_width(&line[..end]))
}
#[derive(Clone, Debug)]
pub struct NativeComposerSourcePlan {
    pub rows: Vec<(usize, String)>,
    pub visible: Vec<(usize, String)>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub scroll_offset: usize,
}
pub fn native_composer_source_plan(
    input: &str,
    cursor: usize,
    width: usize,
    budget: usize,
) -> NativeComposerSourcePlan {
    let rows = native_composer_source_rows(input, width.max(1));
    let (row, col) = native_composer_source_cursor(&rows, cursor);
    let budget = budget.max(1);
    let mut top = (row + 1).saturating_sub(budget);
    if top + budget > rows.len() {
        top = rows.len().saturating_sub(budget)
    }
    NativeComposerSourcePlan {
        visible: rows.iter().skip(top).take(budget).cloned().collect(),
        cursor_row: row.saturating_sub(top),
        cursor_col: col.min(width.saturating_sub(1)),
        scroll_offset: top,
        rows,
    }
}
pub fn native_composer_source_at(
    input: &str,
    width: usize,
    column: usize,
    row: usize,
    scroll: usize,
    padding: usize,
) -> usize {
    let rows = native_composer_source_rows(input, width.max(1));
    let absolute = row.saturating_sub(padding).saturating_add(scroll);
    let Some((start, line)) = rows.get(absolute) else {
        return input.chars().count();
    };
    let mut chars = 0;
    let mut cells = 0;
    for g in line.graphemes(true) {
        let n = native_composer_grapheme_width(g);
        if cells + n > column {
            break;
        }
        cells += n;
        chars += g.chars().count()
    }
    start + chars
}
fn selected_line(
    value: &str,
    start: usize,
    selection: Option<(usize, usize)>,
    styles: NativeComposerStyles,
) -> Line<'static> {
    let Some((mut a, mut b)) = selection else {
        return Line::styled(text::display_safe(value).into_owned(), styles.text);
    };
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    let offsets: Vec<_> = value
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(value.len()))
        .collect();
    let len = value.chars().count();
    let a = a.saturating_sub(start).min(len);
    let b = b.saturating_sub(start).min(len);
    let mut spans = Vec::new();
    for (from, to, style) in [
        (0, a, styles.text),
        (a, b, styles.selection),
        (b, len, styles.text),
    ] {
        if to > from {
            spans.push(Span::styled(
                text::display_safe(&value[offsets[from]..offsets[to]]).into_owned(),
                style,
            ))
        }
    }
    Line::from(spans)
}
fn truncate_columns(value: &str, width: usize) -> String {
    let safe = text::display_safe(value);
    if safe.width() <= width {
        return safe.into_owned();
    }
    let budget = if width > 3 { width - 3 } else { width };
    let mut result = String::new();
    let mut used = 0;
    for g in safe.graphemes(true) {
        let n = g.width();
        if used + n > budget {
            break;
        }
        result.push_str(g);
        used += n;
    }
    if width > 3 {
        result.push_str("...");
    }
    result
}
// The prompt inset is raw chrome, independent of placeholder/input ink.
// Fold line style into its original spans before adding unstyled padding.
fn prompt_inset_line(mut line: Line<'static>, inset: u16) -> Line<'static> {
    if inset > 0 {
        for span in &mut line.spans {
            span.style = line.style.patch(span.style);
        }
        line.style = Style::default();
        line.spans.insert(0, Span::raw("  "));
    }
    line
}

impl NativeComposerFrame<'_> {
    pub fn desired_height(&self, width: u16, available: u16) -> u16 {
        let panel = native_composer_geometry(
            Rect::new(0, 0, width, available),
            self.enclosed,
            self.history_search,
        )
        .submit
        .is_some();
        let geometry = native_composer_geometry(
            Rect::new(0, 0, width, if panel { 3 } else { 1 }),
            self.enclosed,
            self.history_search,
        );
        native_composer_height(
            native_composer_source_rows(&self.text, usize::from(geometry.text.width.max(1))).len(),
            self.menu.reserved_rows,
            available,
            self.density,
            panel,
        )
    }
    pub fn plan(&self, area: Rect) -> NativeComposerPlan {
        let geometry = native_composer_geometry(area, self.enclosed, self.history_search);
        let budget = native_composer_input_budget(
            geometry.inner.height,
            self.menu.reserved_rows.max(self.menu.items.len()),
        );
        let source = native_composer_source_plan(
            &self.text,
            self.cursor,
            usize::from(geometry.text.width.max(1)),
            budget,
        );
        let visual = if self.text.is_empty() {
            1
        } else {
            source.visible.len()
        };
        let padding = native_composer_top_padding(visual, budget);
        let mut lines = vec![Line::from(""); padding];
        let inset = geometry.text.x.saturating_sub(geometry.inner.x);
        if self.text.is_empty() {
            lines.push(prompt_inset_line(safe_line(&self.placeholder), inset))
        } else {
            for (start, value) in &source.visible {
                lines.push(prompt_inset_line(
                    selected_line(value, *start, self.selection, self.styles),
                    inset,
                ))
            }
        }
        let mut menu_rects = Vec::new();
        let mut truncated = Vec::new();
        let slots = usize::from(geometry.inner.height)
            .saturating_sub(visual)
            .saturating_sub(padding)
            .saturating_sub(1)
            .max(1);
        let total = self.menu.items.len();
        let selected = self.menu.selected.min(total.saturating_sub(1));
        let half = slots / 2;
        let top = if total <= slots || selected <= half {
            0
        } else if selected + half >= total {
            total.saturating_sub(slots)
        } else {
            selected.saturating_sub(half)
        };
        let bottom = top.saturating_add(slots).min(total);
        let width = usize::from(geometry.inner.width.max(1));
        let label_width = self
            .menu
            .items
            .iter()
            .take(bottom)
            .skip(top)
            .filter_map(|item| match item {
                NativeComposerMenuItem::Columns { name, .. } => {
                    Some(text::display_safe(name).width())
                }
                _ => None,
            })
            .max()
            .unwrap_or(22)
            .min(width.saturating_sub(4))
            .max(8);
        for (index, item) in self.menu.items.iter().enumerate().take(bottom).skip(top) {
            let y = geometry
                .inner
                .y
                .saturating_add(composer_wrapped_rows(&lines, geometry.inner.width));
            let mut rect = Rect::new(geometry.inner.x, y, geometry.inner.width, 0);
            let mut full_text = None;
            let line = match item {
                NativeComposerMenuItem::Line(line) => safe_line(line),
                NativeComposerMenuItem::Columns {
                    name,
                    description,
                    prefix,
                    marker,
                    name_style,
                    description_style,
                } => {
                    let name = text::display_safe(name);
                    let description = text::display_safe(description);
                    let prefix = safe_span(prefix);
                    let marker = safe_span(marker);
                    let mut label = truncate_columns(&name, label_width);
                    while label.width() < label_width {
                        label.push(' ')
                    }
                    let capacity = width.saturating_sub(
                        1 + marker.content.width() + prefix.content.width() + label_width + 2,
                    );
                    let desc = truncate_columns(&description, capacity);
                    if name.width() > label_width || description.width() > capacity {
                        full_text = Some(if description.trim().is_empty() {
                            name.into_owned()
                        } else {
                            format!("{name}  {description}")
                        });
                    }
                    Line::from(vec![
                        Span::raw(" "),
                        marker,
                        prefix,
                        Span::styled(label, *name_style),
                        Span::styled("  ", *description_style),
                        Span::styled(desc, *description_style),
                    ])
                }
            };
            rect.height = composer_wrapped_rows(std::slice::from_ref(&line), geometry.inner.width)
                .min(geometry.inner.bottom().saturating_sub(y));
            if !rect.is_empty() {
                if self.menu.pointer_rows {
                    menu_rects.push((index, rect));
                }
                if let Some(full) = full_text {
                    truncated.push((rect, full));
                }
            }
            lines.push(line);
        }
        let x = geometry
            .text
            .x
            .saturating_add(u16::try_from(source.cursor_col).unwrap_or(u16::MAX));
        let y = geometry
            .text
            .y
            .saturating_add(u16::try_from(source.cursor_row + padding).unwrap_or(u16::MAX));
        let prompt_position = geometry
            .text
            .contains(Position::new(x, y))
            .then_some(Position::new(x, y));
        let cursor = if self.focused { prompt_position } else { None };
        let frame = NativeComposerFrame {
            text: Cow::Owned(self.text.to_string()),
            cursor: self.cursor,
            selection: self.selection,
            placeholder: self.placeholder.clone(),
            enclosed: self.enclosed,
            density: self.density,
            history_search: self.history_search,
            focused: self.focused,
            can_submit: self.can_submit,
            ascii: self.ascii,
            top_title: self.top_title.clone(),
            top_right: self.top_right.clone(),
            hint: self.hint.clone(),
            quiet_hint: self.quiet_hint.clone(),
            styles: self.styles,
            menu: self.menu.clone(),
        };
        NativeComposerPlan {
            area,
            geometry,
            cursor,
            scroll_offset: source.scroll_offset,
            top_padding: padding,
            input_rows_budget: budget,
            source_rows: source.rows,
            menu_rects,
            truncated,
            prompt_position,
            lines,
            frame,
        }
    }
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> NativeComposerPlan {
        let plan = self.plan(area.intersection(buf.area));
        plan.paint(buf);
        plan
    }
}
impl NativeComposerPlan {
    pub fn paint(&self, buf: &mut Buffer) {
        if self.area.is_empty() {
            return;
        }
        let f = &self.frame;
        let area = self.area;
        let panel = self.geometry.submit.is_some();
        let mut block = Block::default().style(f.styles.background);
        if panel {
            block = block
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(f.styles.border)
        } else if area.height >= 2 {
            block = block
                .borders(Borders::TOP)
                .border_style(f.styles.quiet_border)
        }
        if f.ascii {
            block = block.border_set(ASCII_BORDER)
        }
        block.render(area, buf);
        if area.height >= 2 {
            let mut top = Block::default()
                .borders(Borders::TOP)
                .border_style(if panel {
                    f.styles.border
                } else {
                    f.styles.quiet_border
                })
                .style(f.styles.background);
            if f.ascii {
                top = top.border_set(ASCII_BORDER);
            }
            if panel && let Some(title) = &f.top_title {
                top = top.title(safe_line(title))
            }
            if let Some(right) = &f.top_right {
                top = top.title_top(safe_line(right).right_aligned())
            }
            if !panel && let Some(hint) = &f.quiet_hint {
                top = top.title(safe_line(hint))
            }
            top.render(area, buf);
            if panel {
                let mut bottom = Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(f.styles.border)
                    .style(f.styles.background);
                if f.ascii {
                    bottom = bottom.border_set(ASCII_BORDER);
                }
                if let Some(hint) = &f.hint {
                    bottom = bottom.title_bottom(safe_line(hint))
                }
                bottom.render(area, buf);
                for (x, y, mark) in [
                    (area.x, area.y, "╭"),
                    (area.right() - 1, area.y, "╮"),
                    (area.x, area.bottom() - 1, "╰"),
                    (area.right() - 1, area.bottom() - 1, "╯"),
                ] {
                    buf[(x, y)]
                        .set_symbol(if f.ascii { "+" } else { mark })
                        .set_style(f.styles.border.patch(f.styles.background));
                }
            }
        }
        Paragraph::new(self.lines.clone())
            .style(f.styles.background)
            .wrap(Wrap { trim: false })
            .render(self.geometry.inner, buf);
        if let (Some(x), Some(cursor)) = (self.geometry.prompt_x, self.prompt_position) {
            buf[(x, cursor.y)]
                .set_symbol(if f.ascii { ">" } else { "❯" })
                .set_style(f.styles.prompt);
        }
        if let Some(rect) = self.geometry.submit {
            let mark = match (f.can_submit, f.ascii) {
                (true, false) => "[↵]",
                (true, true) => "[>]",
                (false, false) => "[·]",
                (false, true) => "[.]",
            };
            buf.set_stringn(rect.x, rect.y, mark, 3, f.styles.submit);
        }
    }
}
impl Paint for NativeComposerFrame<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, _theme: &Theme) {
        self.render(area, buf);
    }
    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        self.desired_height(width, self.density.max_rows())
    }
}

const ASCII_BORDER: ratatui::symbols::border::Set<'static> = ratatui::symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

pub const NATIVE_COMPOSER_PANEL_MIN_WIDTH: u16 = 12;
fn composer_wrapped_rows(lines: &[Line<'static>], width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    u16::try_from(
        Paragraph::new(lines.to_vec())
            .wrap(Wrap { trim: false })
            .line_count(width),
    )
    .unwrap_or(u16::MAX)
}
#[derive(Clone, Copy, Debug)]
pub enum NativeComposerRowColumn {
    DisplayCells,
    SourceScalars,
}
/// Pure projection for an existing editor's visual-row command. The host
/// decides whether keys/wheel claim that command and applies the result.
pub fn native_composer_step_row(
    input: &str,
    cursor: usize,
    width: usize,
    delta: isize,
    column: NativeComposerRowColumn,
) -> Option<usize> {
    if input.is_empty() || delta == 0 {
        return None;
    }
    let rows = native_composer_source_rows(input, width.max(1));
    if rows.len() < 2 {
        return None;
    }
    let cursor = cursor.min(input.chars().count());
    let (row, cells) = native_composer_source_cursor(&rows, cursor);
    let target = if delta < 0 {
        row.saturating_sub(delta.unsigned_abs())
    } else {
        row.saturating_add(delta as usize).min(rows.len() - 1)
    };
    if row == target {
        return None;
    }
    let (start, text) = &rows[target];
    let offset = match column {
        NativeComposerRowColumn::SourceScalars => {
            cursor.saturating_sub(rows[row].0).min(text.chars().count())
        }
        NativeComposerRowColumn::DisplayCells => {
            let mut count = 0;
            let mut used = 0;
            for g in text.graphemes(true) {
                let width = native_composer_grapheme_width(g);
                if used + width > cells {
                    break;
                }
                used += width;
                count += g.chars().count();
            }
            count
        }
    };
    Some(start.saturating_add(offset).min(input.chars().count()))
}

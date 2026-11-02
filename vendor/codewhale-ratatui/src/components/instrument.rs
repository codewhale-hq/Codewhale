//! Codewhale's instrument rooms and saved-session list.
//!
//! Extracted from `crates/tui/src/tui/views/mod.rs::{render_underwater_surface,
//! action_footer_lines,place_footer_lines,render_panel_scroll_rail}` and
//! `session_picker.rs::{build_list_lines,format_session_line}` at
//! `a79ce5c4d5ed1a5f7032185710c27343a900351c` in Hmbown/CodeWhale.
//! Callers provide display facts and own filtering, persistence, and actions.

//!
//! MIT License
//!
//! Copyright (c) 2024-2025 DeepSeek-TUI Contributors
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
    style::Modifier,
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Padding, Paragraph, Widget},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{KeyHint, Paint, PickerState, Role, Theme, TuiGround, text};

/// The native room shell, including a wrapping, bottom-anchored action rail.
#[derive(Clone, Debug)]
pub struct InstrumentSurface<'a> {
    pub title: Cow<'a, str>,
    pub actions: Vec<KeyHint>,
    pub quiet_gutter: bool,
}

/// Geometry returned by [`InstrumentSurface::areas`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstrumentAreas {
    pub surface: Rect,
    pub body: Rect,
    pub footer: Rect,
}

impl<'a> InstrumentSurface<'a> {
    #[must_use]
    pub fn new(title: impl Into<Cow<'a, str>>) -> Self {
        Self {
            title: title.into(),
            actions: Vec::new(),
            quiet_gutter: false,
        }
    }

    #[must_use]
    pub fn actions(mut self, actions: Vec<KeyHint>) -> Self {
        self.actions = actions;
        self
    }

    /// Keep one quiet row before the action rail when four body rows remain.
    #[must_use]
    pub const fn quiet_gutter(mut self, enabled: bool) -> Self {
        self.quiet_gutter = enabled;
        self
    }

    fn block(&self, area: Rect, theme: &Theme) -> Block<'static> {
        let surface = surface_area(area);
        let title = native_safe(&self.title, theme);
        let title = text::truncate_words(
            &title,
            usize::from(surface.width.saturating_sub(4)),
            theme.ascii(),
        );
        let mut block = Block::default()
            .title(Line::from(Span::styled(
                format!(" {title} "),
                theme.fg(Role::Accent).add_modifier(Modifier::BOLD),
            )))
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(theme.fg(Role::Border))
            .style(theme.tui_ground(TuiGround::Surface))
            .padding(Padding::new(1, 1, u16::from(area.height >= 24), 0));
        if theme.ascii() {
            block = block.border_set(border::Set {
                horizontal_top: "-",
                horizontal_bottom: "-",
                ..border::PLAIN
            });
        }
        block
    }

    /// Layout against the same rectangle used for painting. For a clipped
    /// buffer, intersect that rectangle with `buf.area` first.
    #[must_use]
    pub fn areas(&self, area: Rect, theme: &Theme) -> InstrumentAreas {
        let surface = surface_area(area);
        let inner = self.block(area, theme).inner(surface);
        let (body, footer) = footer_areas(
            inner,
            action_lines(&self.actions, inner.width, theme).len(),
            self.quiet_gutter,
        );
        InstrumentAreas {
            surface,
            body,
            footer,
        }
    }

    /// Paint the native shell and action rail, returning its content rectangle.
    pub fn draw(&self, area: Rect, buf: &mut Buffer, theme: &Theme) -> Rect {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return area;
        }
        Clear.render(area, buf);
        Block::default()
            .style(theme.tui_ground(TuiGround::Surface))
            .render(area, buf);
        let block = self.block(area, theme);
        let inner = block.inner(surface_area(area));
        block.render(surface_area(area), buf);
        Self::draw_footer(inner, buf, &self.actions, theme, self.quiet_gutter)
    }

    /// Reuse the native action rail inside a centered choice or another host
    /// surface. Hints wrap as whole actions, with a bold padded key and one
    /// space between actions; no action is discarded to make the row fit.
    pub fn draw_footer(
        area: Rect,
        buf: &mut Buffer,
        actions: &[KeyHint],
        theme: &Theme,
        quiet_gutter: bool,
    ) -> Rect {
        let area = area.intersection(buf.area);
        let lines = action_lines(actions, area.width, theme);
        let (body, footer) = footer_areas(area, lines.len(), quiet_gutter);
        if !footer.is_empty() {
            Paragraph::new(lines).render(footer, buf);
        }
        body
    }
}

impl Paint for InstrumentSurface<'_> {
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        1
    }
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.draw(area, buf, theme);
    }
}

fn surface_area(area: Rect) -> Rect {
    let x = u16::from(area.width >= 44);
    let y = u16::from(area.height >= 24);
    Rect::new(
        area.x.saturating_add(x),
        area.y.saturating_add(y),
        area.width.saturating_sub(2 * x),
        area.height.saturating_sub(2 * y),
    )
}

fn footer_areas(inner: Rect, line_count: usize, quiet: bool) -> (Rect, Rect) {
    let height = u16::try_from(line_count)
        .unwrap_or(u16::MAX)
        .min(inner.height);
    let gutter = u16::from(height > 0 && quiet && inner.height >= height.saturating_add(4));
    let footer = Rect::new(
        inner.x,
        inner.bottom().saturating_sub(height),
        inner.width,
        height,
    );
    let body = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(height.saturating_add(gutter)),
    );
    (body, footer)
}

fn action_lines(actions: &[KeyHint], width: u16, theme: &Theme) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut current = Vec::new();
    let mut used = 0usize;
    for action in actions {
        let key = native_safe(&action.keys, theme);
        let label = native_safe(&action.verb, theme);
        let needed = text::width(&key) + 2 + text::width(&label);
        if !current.is_empty() && used + 1 + needed > usize::from(width) {
            result.push(Line::from(std::mem::take(&mut current)));
            used = 0;
        }
        if !current.is_empty() {
            current.push(Span::raw(" "));
            used += 1;
        }
        let key_role = if action.enabled {
            Role::Accent
        } else {
            Role::Dim
        };
        let label_role = if action.enabled {
            Role::Muted
        } else {
            Role::Dim
        };
        current.push(Span::styled(
            format!(" {key} "),
            theme.fg(key_role).add_modifier(Modifier::BOLD),
        ));
        current.push(Span::styled(label, theme.fg(label_role)));
        used += needed;
    }
    if !current.is_empty() {
        result.push(Line::from(current));
    }
    result
}

/// A saved-session display row. `updated` is formatted by the host, so the
/// library needs neither a clock nor a session store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow<'a> {
    pub id: Cow<'a, str>,
    pub title: Cow<'a, str>,
    pub message_count: usize,
    pub mode: Option<Cow<'a, str>>,
    pub updated: Cow<'a, str>,
    pub current: bool,
    pub fork: bool,
    pub archived: bool,
}

impl<'a> SessionRow<'a> {
    #[must_use]
    pub fn new(id: impl Into<Cow<'a, str>>, title: impl Into<Cow<'a, str>>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            message_count: 0,
            mode: None,
            updated: Cow::Borrowed(""),
            current: false,
            fork: false,
            archived: false,
        }
    }
    #[must_use]
    pub const fn messages(mut self, count: usize) -> Self {
        self.message_count = count;
        self
    }
    #[must_use]
    pub fn mode(mut self, mode: impl Into<Cow<'a, str>>) -> Self {
        self.mode = Some(mode.into());
        self
    }
    #[must_use]
    pub fn updated(mut self, updated: impl Into<Cow<'a, str>>) -> Self {
        self.updated = updated.into();
        self
    }
    #[must_use]
    pub const fn current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }
    #[must_use]
    pub const fn fork(mut self, fork: bool) -> Self {
        self.fork = fork;
        self
    }
    #[must_use]
    pub const fn archived(mut self, archived: bool) -> Self {
        self.archived = archived;
        self
    }

    fn line(&self, words: &SessionListWords<'_>, theme: &Theme) -> String {
        let id = native_safe(&self.id, theme);
        let id = id.get(..8).unwrap_or(&id);
        let raw_title = if self.title == "Session" {
            id.to_string()
        } else {
            native_safe(&self.title, theme)
        };
        let title = session_truncate(&raw_title, 32);
        let unit = if self.message_count == 1 {
            &words.message
        } else {
            &words.messages
        };
        let mut flags = String::new();
        for (enabled, label) in [
            (self.current, &words.current),
            (self.fork, &words.fork),
            (self.archived, &words.archived),
        ] {
            if enabled {
                flags.push_str(" | ");
                flags.push_str(&native_safe(label, theme));
            }
        }
        let mode = self.mode.as_ref().map_or_else(
            || native_safe(&words.unknown_mode, theme),
            |mode| native_safe(mode, theme).to_ascii_lowercase(),
        );
        format!(
            "{id} | {title} | {} {}{flags} | {mode} | {}",
            self.message_count,
            native_safe(unit, theme),
            native_safe(&self.updated, theme)
        )
    }
}

/// Localizable copy used by the native session list. Query, status, sort
/// labels, titles, and timestamps remain caller-provided text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionListWords<'a> {
    pub new_title: Cow<'a, str>,
    pub confirm_delete: Cow<'a, str>,
    pub empty_title: Cow<'a, str>,
    pub empty_hint: Cow<'a, str>,
    /// Contains `{start}`, `{end}`, and `{total}`.
    pub showing_range: Cow<'a, str>,
    pub message: Cow<'a, str>,
    pub messages: Cow<'a, str>,
    pub current: Cow<'a, str>,
    pub fork: Cow<'a, str>,
    pub archived: Cow<'a, str>,
    pub unknown_mode: Cow<'a, str>,
}

impl Default for SessionListWords<'_> {
    fn default() -> Self {
        Self {
            new_title: "New title: ".into(),
            confirm_delete: "Confirm delete (y/n)".into(),
            empty_title: "No saved sessions yet.".into(),
            empty_hint: "Send a message to start one — it saves automatically.".into(),
            showing_range: "Showing {start}-{end} / {total}".into(),
            message: "msg".into(),
            messages: "msgs".into(),
            current: "current".into(),
            fork: "fork".into(),
            archived: "archived".into(),
            unknown_mode: "unknown".into(),
        }
    }
}

/// The portable saved-session list used by the current `/sessions` room.
/// Supply already-filtered/sorted rows and update [`PickerState`] in the host.
#[derive(Clone, Debug)]
pub struct SessionList<'a> {
    pub rows: Vec<SessionRow<'a>>,
    pub state: PickerState,
    pub scope_sort: Cow<'a, str>,
    pub query: Option<Cow<'a, str>>,
    pub rename: Option<Cow<'a, str>>,
    pub status: Option<Cow<'a, str>>,
    pub confirm_delete: bool,
    pub words: SessionListWords<'a>,
}

/// A visible source-row index and its one-line mouse target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionHitbox {
    pub index: usize,
    pub area: Rect,
}

impl<'a> SessionList<'a> {
    #[must_use]
    pub fn new(rows: Vec<SessionRow<'a>>) -> Self {
        Self {
            rows,
            state: PickerState::default(),
            scope_sort: "scope and sort · recent".into(),
            query: None,
            rename: None,
            status: None,
            confirm_delete: false,
            words: SessionListWords::default(),
        }
    }
    #[must_use]
    pub const fn state(mut self, state: PickerState) -> Self {
        self.state = state;
        self
    }
    #[must_use]
    pub fn scope_sort(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.scope_sort = label.into();
        self
    }
    #[must_use]
    pub fn query(mut self, query: impl Into<Cow<'a, str>>) -> Self {
        self.query = Some(query.into());
        self
    }
    #[must_use]
    pub fn rename(mut self, title: impl Into<Cow<'a, str>>) -> Self {
        self.rename = Some(title.into());
        self
    }
    #[must_use]
    pub fn status(mut self, status: impl Into<Cow<'a, str>>) -> Self {
        self.status = Some(status.into());
        self
    }
    #[must_use]
    pub const fn confirm_delete(mut self, confirm: bool) -> Self {
        self.confirm_delete = confirm;
        self
    }
    #[must_use]
    pub fn words(mut self, words: SessionListWords<'a>) -> Self {
        self.words = words;
        self
    }

    fn layout(&self, area: Rect) -> SessionLayout {
        let header = 1 + usize::from(self.confirm_delete || self.status.is_some());
        let visible = usize::from(area.height)
            .saturating_sub(header + usize::from(!self.rows.is_empty()))
            .max(1);
        let offset = self.state.visible_offset(self.rows.len(), visible);
        let scrolls =
            area.width >= 2 && area.height > 0 && self.rows.len().saturating_add(header) > visible;
        SessionLayout {
            header,
            visible,
            offset,
            content: Rect::new(
                area.x,
                area.y,
                area.width.saturating_sub(u16::from(scrolls)),
                area.height,
            ),
            scrolls,
        }
    }

    /// Visible item targets, excluding the header, range label, and rail.
    /// Pass the clipped paint area when the buffer only covers part of a view.
    #[must_use]
    pub fn hitboxes(&self, area: Rect, _theme: &Theme) -> Vec<SessionHitbox> {
        if area.is_empty() || self.rows.is_empty() {
            return Vec::new();
        }
        let layout = self.layout(area);
        self.rows
            .iter()
            .enumerate()
            .skip(layout.offset)
            .take(layout.visible)
            .enumerate()
            .filter_map(|(slot, (index, _))| {
                let y = area
                    .y
                    .saturating_add(u16::try_from(layout.header + slot).unwrap_or(u16::MAX));
                (y < area.bottom() && layout.content.width > 0).then_some(SessionHitbox {
                    index,
                    area: Rect::new(area.x, y, layout.content.width, 1),
                })
            })
            .collect()
    }

    #[must_use]
    pub fn item_at(&self, area: Rect, position: Position, theme: &Theme) -> Option<usize> {
        self.hitboxes(area, theme)
            .into_iter()
            .find_map(|hit| hit.area.contains(position).then_some(hit.index))
    }
}

struct SessionLayout {
    header: usize,
    visible: usize,
    offset: usize,
    content: Rect,
    scrolls: bool,
}

impl Paint for SessionList<'_> {
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        u16::try_from(
            1 + usize::from(self.confirm_delete || self.status.is_some())
                + if self.rows.is_empty() {
                    2
                } else {
                    self.rows.len() + 1
                },
        )
        .unwrap_or(u16::MAX)
    }
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let layout = self.layout(area);
        if layout.scrolls {
            scroll_rail(
                area,
                buf,
                self.rows.len().saturating_add(layout.header),
                layout.offset,
                layout.visible,
                theme,
            );
        }
        let width = usize::from(layout.content.width);
        let header = if let Some(query) = &self.query {
            format!("/{}", native_safe(query, theme))
        } else if let Some(rename) = &self.rename {
            format!(
                "{}{}_",
                native_safe(&self.words.new_title, theme),
                native_safe(rename, theme)
            )
        } else {
            native_safe(&self.scope_sort, theme)
        };
        let mut lines = vec![Line::styled(
            session_truncate(&header, width),
            theme.fg(Role::Muted),
        )];
        if self.confirm_delete {
            lines.push(Line::styled(
                session_truncate(&native_safe(&self.words.confirm_delete, theme), width),
                theme.fg(Role::Attention).add_modifier(Modifier::BOLD),
            ));
        } else if let Some(status) = &self.status {
            lines.push(Line::styled(
                session_truncate(&native_safe(status, theme), width),
                theme.fg(Role::Accent),
            ));
        }
        if self.rows.is_empty() {
            lines.push(Line::styled(
                session_truncate(&native_safe(&self.words.empty_title, theme), width),
                theme.fg(Role::Muted),
            ));
            lines.push(Line::styled(
                session_truncate(&native_safe(&self.words.empty_hint, theme), width),
                theme.fg(Role::Hint),
            ));
        } else {
            for (slot, (index, row)) in self
                .rows
                .iter()
                .enumerate()
                .skip(layout.offset)
                .take(layout.visible)
                .enumerate()
            {
                let prefix = if slot < 9 {
                    format!("{}. ", slot + 1)
                } else {
                    "   ".into()
                };
                let style = if index == self.state.selected.min(self.rows.len().saturating_sub(1)) {
                    theme
                        .fg(Role::Foreground)
                        .patch(theme.bg(Role::Selected))
                        .add_modifier(Modifier::BOLD)
                } else {
                    theme.fg(if row.current {
                        Role::Accent
                    } else {
                        Role::Foreground
                    })
                };
                lines.push(Line::styled(
                    session_truncate(&format!("{prefix}{}", row.line(&self.words, theme)), width),
                    style,
                ));
            }
            if self.rows.len() > layout.visible {
                let label = native_safe(&self.words.showing_range, theme)
                    .replace("{start}", &layout.offset.saturating_add(1).to_string())
                    .replace(
                        "{end}",
                        &layout
                            .offset
                            .saturating_add(layout.visible)
                            .min(self.rows.len())
                            .to_string(),
                    )
                    .replace("{total}", &self.rows.len().to_string());
                lines.push(Line::styled(
                    session_truncate(&label, width),
                    theme.fg(Role::Dim),
                ));
            }
        }
        Paragraph::new(lines).render(layout.content, buf);
    }
}

fn scroll_rail(
    area: Rect,
    buf: &mut Buffer,
    total: usize,
    offset: usize,
    visible: usize,
    theme: &Theme,
) {
    let height = usize::from(area.height);
    if height == 0 || area.width < 2 || total <= visible.max(1) {
        return;
    }
    let thumb = height
        .saturating_mul(visible)
        .div_ceil(total)
        .clamp(1, height);
    let max_offset = total.saturating_sub(visible);
    let top = (height - thumb)
        .saturating_mul(offset.min(max_offset))
        .checked_div(max_offset)
        .unwrap_or(0);
    for i in 0..height {
        let on = i >= top && i < top + thumb;
        let glyph = match (on, theme.ascii()) {
            (true, false) => "█",
            (false, false) => "│",
            (true, true) => "#",
            (false, true) => "|",
        };
        buf[(area.right() - 1, area.y + i as u16)]
            .set_symbol(glyph)
            .set_style(theme.fg(if on { Role::Muted } else { Role::Border }));
    }
}

/// The session list's native three-dot truncation and conservative title
/// budget. Grapheme boundaries and degenerate widths are made safe here.
fn session_truncate(value: &str, max: usize) -> String {
    if text::width(value) <= max {
        return value.into();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in value.graphemes(true) {
        let width = text::width(g);
        if if max > 3 {
            used + width >= max - 3
        } else {
            used + width > max
        } {
            break;
        }
        out.push_str(g);
        used += width;
    }
    if max > 3 {
        out.push_str("...");
    }
    out
}

fn native_safe(value: &str, theme: &Theme) -> String {
    let safe = text::display_safe(value);
    if !theme.ascii() {
        return safe.into_owned();
    }
    safe.chars()
        .map(|c| match c {
            '↑' => "Up".into(),
            '↓' => "Dn".into(),
            '←' => "Left".into(),
            '→' => "Right".into(),
            '·' => ".".into(),
            '—' | '–' => "-".into(),
            '↵' => ">".into(),
            '⇆' => "<>".into(),
            '×' => "x".into(),
            '✓' => "+".into(),
            '▸' => ">".into(),
            '…' => "...".into(),
            _ => c.to_string(),
        })
        .collect()
}

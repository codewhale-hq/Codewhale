//! An empty state: what is true, and the one thing to do about it.
//!
//! ```text
//!         ⣠⣶⣶⣄
//!        ⠿⣿⣿⣿⡿          (the whale mark, one ink, only where it fits)
//!
//!     No workflow runs yet
//!   Runs appear here once one starts.
//!   Start one with /workflow
//! ```
//!
//! It has one sentence (the title), an optional body and at most one action:
//! a list that is empty should never need more. The mark is identity, not
//! delight: the kit's whale in `Primary`, drawn only when the area has the
//! rows for it (the 16-column compact whale, a few rows above the text).
//! Braille has no honest ASCII form, so ASCII-safe terminals and areas too
//! small for the whale get a one-cell glyph before the title instead (`○`,
//! `o` in ASCII). The words are all parameters; the kit owns none of them.
//!
//! Replaces `EmptyState` in the engine's `views/mod.rs` and the inline empty
//! messages in `workflows_manager.rs`, `widgets/mod.rs` and `underwater.rs`
//! (`Hmbown/CodeWhale` `58b1dd3dd`).

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Widget,
};

use crate::{
    Paint, Role, Theme, WhaleState, glyphs, text,
    whale::{Grid, rasterize, scene},
};

/// What stands above the title.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmptyMark {
    /// The whale where it fits (16x8 cells, or 8x4 in less room), else a
    /// glyph before the title. The default.
    #[default]
    Whale,
    /// The glyph before the title, never the whale.
    Glyph,
    /// Words only.
    None,
}

/// The whale's viewport in cells: the kit's compact size. Smaller viewports
/// pick lower-detail scenes that stop reading as a whale, so below this
/// there is a glyph instead.
const WHALE: (u16, u16) = (16, 8);
/// Body lines shown at most.
const BODY_LINES: usize = 3;

/// An empty area's message. Build it, then [`Paint::paint`] it into the area
/// the content would have filled; it centres itself there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmptyState<'a> {
    title: Cow<'a, str>,
    body: Option<Cow<'a, str>>,
    action: Option<Cow<'a, str>>,
    mark: EmptyMark,
}

impl<'a> EmptyState<'a> {
    /// `title` says what is true: "No workflow runs yet".
    #[must_use]
    pub fn new(title: impl Into<Cow<'a, str>>) -> Self {
        Self {
            title: title.into(),
            body: None,
            action: None,
            mark: EmptyMark::default(),
        }
    }

    /// A sentence of context, shown in `Muted` and wrapped to the width.
    #[must_use]
    pub fn body(mut self, body: impl Into<Cow<'a, str>>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// The one suggested next step ("Start one with /workflow"), in
    /// `Primary` on its own line.
    #[must_use]
    pub fn action(mut self, action: impl Into<Cow<'a, str>>) -> Self {
        self.action = Some(action.into());
        self
    }

    #[must_use]
    pub const fn mark(mut self, mark: EmptyMark) -> Self {
        self.mark = mark;
        self
    }

    /// The lines that fit `width` and `height`, with the mark chosen.
    fn plan(&self, width: u16, height: u16, ascii: bool) -> Plan {
        let inner = usize::from(width.saturating_sub(4).max(1));
        let body = self
            .body
            .as_deref()
            .map(|b| wrap(&text::display_safe(b), inner, BODY_LINES, ascii))
            .unwrap_or_default();
        let action = self.action.is_some();

        // Rows for text. Squeeze the body, then the action, never the title.
        let rows = |body: usize, action: bool| 1 + body + usize::from(action);
        let (keep_body, keep_action) = if usize::from(height) >= rows(body.len(), action) {
            (body.len(), action)
        } else if usize::from(height) >= rows(0, action) {
            (0, action)
        } else {
            (0, false)
        };
        let text_rows = rows(keep_body, keep_action);
        let spare = usize::from(height).saturating_sub(text_rows);

        let art = (self.mark == EmptyMark::Whale && !ascii && width >= WHALE.0 + 8)
            .then(Art::whale)
            .flatten()
            // One row of air between the whale and the title.
            .filter(|art| spare > usize::from(art.rows));
        let glyph = art.is_none() && self.mark != EmptyMark::None;
        Plan {
            body: body.into_iter().take(keep_body).collect(),
            action: keep_action,
            art,
            glyph,
        }
    }
}

/// The whale's Braille cells with the empty rows at the top and bottom cut.
struct Art {
    grid: Grid,
    first: u16,
    rows: u16,
}

impl Art {
    fn whale() -> Option<Self> {
        let grid = rasterize(
            scene(WhaleState::Rest, u32::from(WHALE.0) * 2)?,
            WHALE.0,
            WHALE.1,
            0.5,
        );
        let blank = |r: u16| (0..grid.cols).all(|c| grid.cell(c, r) == 0);
        let first = (0..grid.rows).find(|&r| !blank(r))?;
        let last = (0..grid.rows).rev().find(|&r| !blank(r))?;
        Some(Self {
            first,
            rows: last - first + 1,
            grid,
        })
    }
}

struct Plan {
    body: Vec<String>,
    action: bool,
    art: Option<Art>,
    glyph: bool,
}

impl Plan {
    fn rows(&self) -> u16 {
        let art = self.art.as_ref().map_or(0, |a| a.rows + 1);
        art + 1 + u16::try_from(self.body.len()).unwrap_or(0) + u16::from(self.action)
    }
}

/// Greedy word wrap to `width` cells, at most `max` lines, the last cut with
/// an ellipsis. A word wider than the line is cut inside it.
fn wrap(body: &str, width: usize, max: usize, ascii: bool) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in body.split_whitespace() {
        let fits = line.is_empty() || text::width(&line) + 1 + text::width(word) <= width;
        if !fits {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    let clipped = lines.len() > max;
    lines.truncate(max);
    let last = lines.len().saturating_sub(1);
    for (i, l) in lines.iter_mut().enumerate() {
        if i == last && clipped {
            l.push(' ');
            l.push_str(glyphs::pick(glyphs::ELLIPSIS, ascii));
        }
        *l = text::truncate(l, width, ascii).into_owned();
    }
    lines
}

impl Paint for EmptyState<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let ascii = theme.ascii();
        let plan = self.plan(area.width, area.height, ascii);
        let top = area.y + area.height.saturating_sub(plan.rows()) / 2;
        let mut y = top;
        let line = |y: u16| Rect {
            y,
            height: 1,
            ..area
        };

        if let Some(art) = &plan.art {
            let x0 = area.x + (area.width - art.grid.cols) / 2;
            let ink = theme.fg(Role::Accent);
            for r in 0..art.rows {
                for c in 0..art.grid.cols {
                    let ch = art.grid.char_at(c, art.first + r);
                    if ch != ' ' {
                        let mut tmp = [0u8; 4];
                        buf[(x0 + c, y + r)]
                            .set_symbol(ch.encode_utf8(&mut tmp))
                            .set_style(ink);
                    }
                }
            }
            y += art.rows + 1;
        }

        let title = text::display_safe(&self.title);
        let mut spans = Vec::new();
        let mut room = usize::from(area.width);
        if plan.glyph {
            let mark = glyphs::pick(glyphs::AVAILABLE, ascii);
            spans.push(Span::styled(format!("{mark} "), theme.fg(Role::Muted)));
            room = room.saturating_sub(text::width(mark) + 1);
        }
        spans.push(Span::styled(
            text::truncate(&title, room, ascii).into_owned(),
            theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
        ));
        Line::from(spans).centered().render(line(y), buf);
        y += 1;

        for body in &plan.body {
            Line::from(Span::styled(body.clone(), theme.fg(Role::Muted)))
                .centered()
                .render(line(y), buf);
            y += 1;
        }
        if plan.action
            && let Some(action) = &self.action
        {
            let action = text::display_safe(action);
            let shown = text::truncate_words(&action, usize::from(area.width), ascii);
            Line::from(Span::styled(shown.into_owned(), theme.fg(Role::Accent)))
                .centered()
                .render(line(y), buf);
        }
    }

    fn height(&self, width: u16, theme: &Theme) -> u16 {
        // As many rows as it would take with room to spare.
        self.plan(width, u16::MAX, theme.ascii()).rows()
    }
}

//! Panels with depth, and the one horizon.
//!
//! Depth is elevation: raised things sit nearer the surface, the stage and
//! things put away sit deeper. Where the terminal paints grounds, depth is a
//! ground; where it cannot (16 colors, `NO_COLOR`, an unmeasured ground, or a
//! depth where two grounds quantize to one color), depth becomes an edge,
//! because a fill nobody can see separates nothing.
//!
//! Replaces the engine's three modal treatments (`render_modal_surface` with
//! its shadow, `render_underwater_surface` with its two rules, and hand-rolled
//! `Clear` blocks) and lifts `centered_modal_area` (`crates/tui/src/tui/
//! views/mod.rs`, `Hmbown/CodeWhale` `58b1dd3dd`).

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget},
};

use crate::{KeyHints, Paint, Role, Theme, color, glyphs, text};

/// How far from the surface a panel sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Depth {
    /// Rails and things put away.
    Deep,
    /// The stage: full-screen sheets. No edge; a two-cell gutter.
    Stage,
    /// A card on the stage.
    Raised,
    /// A decision over everything else: always edged in `BorderStrong`.
    Overlay,
}

impl Depth {
    #[must_use]
    pub const fn ground(self) -> Role {
        match self {
            Depth::Deep => Role::Sidebar,
            Depth::Stage => Role::Background,
            Depth::Raised | Depth::Overlay => Role::Surface,
        }
    }
}

/// A titled panel. [`Panel::draw`] paints it and returns the content area.
#[derive(Clone, Debug)]
pub struct Panel<'a> {
    pub title: Option<Cow<'a, str>>,
    /// Muted text at the right of the title row: a count, "changed 2 min ago".
    pub aside: Option<Cow<'a, str>>,
    pub depth: Depth,
    /// Light the edge in `Primary`: this panel has the keyboard.
    pub focused: bool,
    pub hints: Option<&'a KeyHints>,
}

impl<'a> Panel<'a> {
    #[must_use]
    pub fn new(depth: Depth) -> Self {
        Self {
            title: None,
            aside: None,
            depth,
            focused: false,
            hints: None,
        }
    }

    #[must_use]
    pub fn title(mut self, title: impl Into<Cow<'a, str>>) -> Self {
        self.title = Some(title.into());
        self
    }

    #[must_use]
    pub fn aside(mut self, aside: impl Into<Cow<'a, str>>) -> Self {
        self.aside = Some(aside.into());
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn hints(mut self, hints: &'a KeyHints) -> Self {
        self.hints = Some(hints);
        self
    }

    fn edged(&self, theme: &Theme) -> bool {
        match self.depth {
            Depth::Overlay => true,
            // Light 256-color `Surface` and `Background` are both white.
            Depth::Raised => !theme.grounds_differ(Role::Surface, Role::Background),
            Depth::Deep | Depth::Stage => false,
        }
    }

    /// Where everything goes in `area`, without painting. [`Panel::draw`]
    /// paints from this, and [`Panel::body`] and [`Dialog`] size from it, so
    /// the three cannot disagree.
    fn layout(&self, area: Rect, theme: &Theme) -> PanelLayout {
        let edged = self.edged(theme);
        let mut inner = area;
        if edged {
            inner = Block::default().borders(Borders::ALL).inner(area);
        }

        let gutter = match self.depth {
            Depth::Stage if inner.width >= 24 => 2,
            _ if inner.width >= 8 => 1,
            _ => 0,
        };
        let vpad = u16::from(edged && inner.height >= 6);
        inner = Rect {
            x: inner.x + gutter,
            y: inner.y + vpad,
            width: inner.width.saturating_sub(gutter * 2),
            height: inner.height.saturating_sub(vpad * 2),
        };

        let mut title_row = None;
        if self.title.is_some() && inner.height > 0 {
            title_row = Some(Rect { height: 1, ..inner });
            let used = 1 + u16::from(inner.height >= 6);
            inner.y += used.min(inner.height);
            inner.height = inner.height.saturating_sub(used);
        }

        let mut rail = None;
        if let Some(hints) = self.hints {
            let lines = hints.lines(inner.width, theme);
            let h = u16::try_from(lines.len())
                .unwrap_or(u16::MAX)
                .min(inner.height);
            if h > 0 {
                let rect = Rect {
                    y: inner.bottom() - h,
                    height: h,
                    ..inner
                };
                rail = Some((rect, lines));
                let gap = u16::from(inner.height >= h + 4);
                inner.height = inner.height.saturating_sub(h + gap);
            }
        }
        PanelLayout {
            edged,
            title_row,
            rail,
            body: inner,
        }
    }

    /// The content area [`Panel::draw`] returns for `area`, without
    /// painting: for sizing a popup to what goes in it.
    #[must_use]
    pub fn body(&self, area: Rect, theme: &Theme) -> Rect {
        if area.is_empty() {
            return area;
        }
        self.layout(area, theme).body
    }

    /// Paint the panel and return the area left for content.
    pub fn draw(&self, area: Rect, buf: &mut Buffer, theme: &Theme) -> Rect {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return area;
        }
        Clear.render(area, buf);
        let ground = theme.bg(self.depth.ground());
        buf.set_style(area, ground);

        let layout = self.layout(area, theme);
        if layout.edged {
            let edge_role = if self.focused {
                Role::Accent
            } else if self.depth == Depth::Overlay {
                Role::BorderStrong
            } else {
                Role::Border
            };
            let set = if theme.ascii() {
                border::Set {
                    top_left: "+",
                    top_right: "+",
                    bottom_left: "+",
                    bottom_right: "+",
                    vertical_left: "|",
                    vertical_right: "|",
                    horizontal_top: "-",
                    horizontal_bottom: "-",
                }
            } else {
                border::PLAIN
            };
            Block::default()
                .borders(Borders::ALL)
                .border_set(set)
                .border_style(theme.fg(edge_role).patch(ground))
                .render(area, buf);
        }

        if let (Some(title), Some(row)) = (&self.title, layout.title_row) {
            let title = text::display_safe(title);
            // The title keeps its room: an aside that cannot sit beside it
            // is cut to a third of the row, or dropped.
            let row_w = usize::from(row.width);
            let aside = self.aside.as_deref().map(text::display_safe).and_then(|a| {
                if text::width(&title) + 2 + text::width(&a) <= row_w {
                    Some(a.into_owned())
                } else {
                    let cut = text::truncate(&a, row_w / 3, theme.ascii());
                    (row_w / 3 >= 4 && !cut.is_empty()).then(|| cut.into_owned())
                }
            });
            let aside_w = aside.as_deref().map_or(0, |a| text::width(a) + 2);
            let title_w = row_w.saturating_sub(aside_w);
            let title = text::truncate(&title, title_w, theme.ascii());
            Line::from(Span::styled(
                title.into_owned(),
                theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
            ))
            .render(row, buf);
            if let Some(aside) = aside
                && aside_w > 0
                && aside_w <= usize::from(row.width)
            {
                Line::from(Span::styled(aside, theme.fg(Role::Muted)))
                    .right_aligned()
                    .render(row, buf);
            }
        }

        if let Some((rect, lines)) = layout.rail {
            Paragraph::new(lines).render(rect, buf);
        }
        layout.body
    }
}

/// The geometry of a [`Panel`] in an area.
struct PanelLayout {
    edged: bool,
    title_row: Option<Rect>,
    rail: Option<(Rect, Vec<Line<'static>>)>,
    body: Rect,
}

impl Paint for Panel<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.draw(area, buf, theme);
    }
}

/// A centered popup rect: starts from the preferred size, never exceeds the
/// frame (keeping a one-cell margin when it can), and never drops below the
/// minimum unless the frame is smaller. From the engine's
/// `centered_modal_area` (#3732).
#[must_use]
pub fn centered(
    area: Rect,
    preferred_width: u16,
    preferred_height: u16,
    min_width: u16,
    min_height: u16,
) -> Rect {
    if area.is_empty() {
        return area;
    }
    let avail_width = area.width.saturating_sub(2).max(1);
    let avail_height = area.height.saturating_sub(2).max(1);
    let width = preferred_width.clamp(min_width.min(avail_width), avail_width);
    let height = preferred_height.clamp(min_height.min(avail_height), avail_height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// How wide a [`Dialog`] wants to be before the terminal clamps it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum DialogWidth {
    /// A yes-or-no question: 48 cells.
    Narrow,
    /// Most decisions: 60 cells.
    #[default]
    Standard,
    /// Detail worth reading before deciding: 72 cells.
    Wide,
}

impl DialogWidth {
    /// The preferred width in cells.
    #[must_use]
    pub const fn cells(self) -> u16 {
        match self {
            DialogWidth::Narrow => 48,
            DialogWidth::Standard => 60,
            DialogWidth::Wide => 72,
        }
    }
}

/// `preferred` cells of a `total`-cell axis, leaving `margin` cells of
/// backdrop when the axis can spare them, never below `floor` unless the
/// axis itself is smaller, and never more than the axis.
fn fit_axis(total: u16, preferred: u16, margin: u16, floor: u16) -> u16 {
    preferred.min(total.saturating_sub(margin).max(floor.min(total)))
}

/// A decision, centered over everything else: confirmations, approvals,
/// destructive actions. Always edged in `BorderStrong`, so it stays legible
/// where grounds do not paint (16 colors, `NO_COLOR`), with no shadow and no
/// rounded corners. Replaces the engine's modal surface with its shadow and
/// the hand-rolled `Clear` blocks.
///
/// ```
/// # use codewhale_ratatui::{Dialog, DialogWidth, KeyHint, KeyHints, Theme};
/// # use ratatui::{buffer::Buffer, layout::Rect};
/// # let (theme, area) = (Theme::detect(), Rect::new(0, 0, 80, 24));
/// # let mut buf = Buffer::empty(area);
/// let hints = KeyHints::new(vec![KeyHint::new("y", "stop"), KeyHint::new("n", "keep running")]);
/// let body = Dialog::new()
///     .title("Stop the running workflow?")
///     .width(DialogWidth::Narrow)
///     .body_rows(2)
///     .hints(&hints)
///     .draw(area, &mut buf, &theme);
/// // paint the question into `body`
/// # let _ = body;
/// ```
#[derive(Clone, Debug, Default)]
pub struct Dialog<'a> {
    pub title: Option<Cow<'a, str>>,
    pub width: DialogWidth,
    /// Rows of body the caller will fill. The dialog is as tall as that
    /// needs, with padding and a blank row between its parts, then clamped
    /// to the terminal. The body it returns has at least this many rows
    /// unless the terminal is too short.
    pub body_rows: u16,
    pub hints: Option<&'a KeyHints>,
}

impl<'a> Dialog<'a> {
    /// The narrowest a dialog gets while the terminal can still spare it a
    /// margin.
    const MIN_WIDTH: u16 = 20;
    const MIN_HEIGHT: u16 = 5;

    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn title(mut self, title: impl Into<Cow<'a, str>>) -> Self {
        self.title = Some(title.into());
        self
    }

    #[must_use]
    pub fn width(mut self, width: DialogWidth) -> Self {
        self.width = width;
        self
    }

    #[must_use]
    pub fn body_rows(mut self, rows: u16) -> Self {
        self.body_rows = rows;
        self
    }

    #[must_use]
    pub fn hints(mut self, hints: &'a KeyHints) -> Self {
        self.hints = Some(hints);
        self
    }

    fn panel(&self) -> Panel<'a> {
        Panel {
            title: self.title.clone(),
            aside: None,
            depth: Depth::Overlay,
            focused: false,
            hints: self.hints,
        }
    }

    fn width_in(&self, area_width: u16) -> u16 {
        fit_axis(area_width, self.width.cells(), 4, Self::MIN_WIDTH)
    }

    /// Rows that leave `body_rows` of body at `width`, before clamping.
    /// Starts from the roomy layout (padding, a blank row under the title and
    /// above the hints) and grows until the panel really leaves that body.
    fn rows_for(&self, width: u16, theme: &Theme) -> u16 {
        let panel = self.panel();
        let want = self.body_rows.max(1);
        let hint_rows = self.hints.map_or(0, |h| h.height(width, theme));
        let chrome = 4
            + if self.title.is_some() { 2 } else { 0 }
            + if hint_rows > 0 { hint_rows + 1 } else { 0 };
        let start = want.saturating_add(chrome);
        let limit = start.saturating_add(8);
        (start..=limit)
            .find(|&h| panel.body(Rect::new(0, 0, width, h), theme).height >= want)
            .unwrap_or(limit)
    }

    /// Where the dialog goes in `area`: centered, at most the width asked
    /// for, never wider or taller than `area`, with a two-cell margin at the
    /// sides and one row above and below when `area` can spare them.
    #[must_use]
    pub fn rect(&self, area: Rect, theme: &Theme) -> Rect {
        let width = self.width_in(area.width);
        let rows = self.rows_for(width, theme);
        let height = fit_axis(area.height, rows, 2, Self::MIN_HEIGHT);
        Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        }
    }

    /// Paint the dialog over whatever is in `area` and return the body area
    /// for the caller to fill.
    pub fn draw(&self, area: Rect, buf: &mut Buffer, theme: &Theme) -> Rect {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return area;
        }
        self.panel().draw(self.rect(area, theme), buf, theme)
    }
}

impl Paint for Dialog<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.draw(area, buf, theme);
    }

    /// Rows the dialog wants at `width`, before the terminal clamps it.
    fn height(&self, width: u16, theme: &Theme) -> u16 {
        self.rows_for(self.width_in(width), theme)
    }
}

/// The edge a [`Sheet`] is anchored to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SheetEdge {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
}

impl SheetEdge {
    /// Whether the sheet's size is a height (it spans the width).
    #[must_use]
    pub const fn spans_width(self) -> bool {
        matches!(self, SheetEdge::Top | SheetEdge::Bottom)
    }
}

/// A raised surface anchored to one edge of the screen: settings, managers
/// and pickers that slide in over the work. The ground separates it where
/// grounds paint; where they do not (16 colors, `NO_COLOR`, a 256-color
/// ground that collapses) [`Panel`] draws an edge, so a sheet is never an
/// unmarked patch of text. Replaces the engine's underwater surface, whose
/// two rules broke the one-horizon rule.
#[derive(Clone, Debug, Default)]
pub struct Sheet<'a> {
    pub title: Option<Cow<'a, str>>,
    /// Muted text at the right of the title row.
    pub aside: Option<Cow<'a, str>>,
    pub edge: SheetEdge,
    /// Rows (top and bottom) or columns (left and right) wanted; half the
    /// screen when unset.
    pub size: Option<u16>,
    /// The most rows or columns the sheet takes, however much is wanted.
    pub max_size: Option<u16>,
    pub focused: bool,
    pub hints: Option<&'a KeyHints>,
}

impl<'a> Sheet<'a> {
    /// A sheet anchored to the bottom edge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn edge(mut self, edge: SheetEdge) -> Self {
        self.edge = edge;
        self
    }

    #[must_use]
    pub fn title(mut self, title: impl Into<Cow<'a, str>>) -> Self {
        self.title = Some(title.into());
        self
    }

    #[must_use]
    pub fn aside(mut self, aside: impl Into<Cow<'a, str>>) -> Self {
        self.aside = Some(aside.into());
        self
    }

    #[must_use]
    pub fn size(mut self, size: u16) -> Self {
        self.size = Some(size);
        self
    }

    #[must_use]
    pub fn max_size(mut self, max: u16) -> Self {
        self.max_size = Some(max);
        self
    }

    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    #[must_use]
    pub fn hints(mut self, hints: &'a KeyHints) -> Self {
        self.hints = Some(hints);
        self
    }

    /// Where the sheet goes in `area`: against its edge, across the whole
    /// other axis, and as large as asked for, no larger than `max_size` or
    /// `area`.
    #[must_use]
    pub fn rect(&self, area: Rect) -> Rect {
        let total = if self.edge.spans_width() {
            area.height
        } else {
            area.width
        };
        let size = self
            .size
            .unwrap_or_else(|| total.div_ceil(2))
            .min(self.max_size.unwrap_or(u16::MAX))
            .min(total);
        match self.edge {
            SheetEdge::Bottom => Rect {
                y: area.bottom() - size,
                height: size,
                ..area
            },
            SheetEdge::Top => Rect {
                height: size,
                ..area
            },
            SheetEdge::Left => Rect {
                width: size,
                ..area
            },
            SheetEdge::Right => Rect {
                x: area.right() - size,
                width: size,
                ..area
            },
        }
    }

    fn panel(&self) -> Panel<'a> {
        Panel {
            title: self.title.clone(),
            aside: self.aside.clone(),
            depth: Depth::Raised,
            focused: self.focused,
            hints: self.hints,
        }
    }

    /// Paint the sheet over whatever is in `area` and return the body area
    /// for the caller to fill.
    pub fn draw(&self, area: Rect, buf: &mut Buffer, theme: &Theme) -> Rect {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return area;
        }
        self.panel().draw(self.rect(area), buf, theme)
    }
}

impl Paint for Sheet<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.draw(area, buf, theme);
    }
}

/// The horizon: one full-width rule, drawn once per frame, above the place
/// where the person types. Sugimoto's seascapes, not a table border.
///
/// On a truecolor ground we painted, its ends fade into it, so it reads as a
/// horizon rather than a box edge. Everywhere else it is a plain `Border`
/// line; ASCII-safe terminals draw `-`.
#[derive(Clone, Debug, Default)]
pub struct HorizonRule<'a> {
    /// Muted words at the left: what the space below is for.
    pub label: Option<Cow<'a, str>>,
    /// Muted words at the right.
    pub aside: Option<Cow<'a, str>>,
}

impl<'a> HorizonRule<'a> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.label = Some(label.into());
        self
    }

    #[must_use]
    pub fn aside(mut self, aside: impl Into<Cow<'a, str>>) -> Self {
        self.aside = Some(aside.into());
        self
    }
}

impl Paint for HorizonRule<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let row = Rect { height: 1, ..area };
        let rule = glyphs::pick("─", theme.ascii());
        let line_style = theme.fg(Role::Border);
        // Fade only into a ground we painted: after `without_base_ground` the
        // ground is the terminal's own, whose color we do not know.
        let fade = theme.depth() == color::ColorDepth::TrueColor && theme.paints_base_ground();
        let width = row.width;
        let ramp = (width / 8).min(6);
        for i in 0..width {
            let mut style = line_style;
            if fade && ramp > 0 {
                let from_edge = i.min(width - 1 - i);
                if from_edge < ramp {
                    let alpha = f32::from(from_edge + 1) / f32::from(ramp + 1);
                    style = Style::default().fg(color::blend(
                        theme.token(Role::Border),
                        theme.token(Role::Background),
                        alpha,
                    ));
                }
            }
            buf[(row.x + i, row.y)].set_symbol(rule).set_style(style);
        }
        let muted = theme.fg(Role::Muted);
        if let Some(label) = &self.label {
            let label = text::display_safe(label);
            let budget = usize::from(width.saturating_sub(ramp.max(2) * 2 + 4)) / 2;
            let label = format!(" {} ", text::truncate(&label, budget, theme.ascii()));
            let x = row.x + ramp.max(2).min(width);
            buf.set_stringn(x, row.y, &label, usize::from(row.right() - x), muted);
        }
        if let Some(aside) = &self.aside {
            let aside = text::display_safe(aside);
            let budget = usize::from(width.saturating_sub(ramp.max(2) * 2 + 4)) / 2;
            let aside = format!(" {} ", text::truncate(&aside, budget, theme.ascii()));
            let w = u16::try_from(text::width(&aside)).unwrap_or(u16::MAX);
            let x = row.right().saturating_sub(w + ramp.max(2));
            if x > row.x {
                buf.set_stringn(x, row.y, &aside, usize::from(w), muted);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_clamps_to_the_frame() {
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(centered(area, 68, 10, 44, 8), Rect::new(6, 7, 68, 10));
        let small = Rect::new(0, 0, 30, 6);
        let r = centered(small, 68, 10, 44, 8);
        assert!(r.width <= 28 && r.height <= 4);
    }

    /// A raised card must be told from the stage in every profile: by its
    /// ground where the two grounds differ, by an edge everywhere else.
    #[test]
    fn a_raised_panel_is_always_distinguishable() {
        use crate::testing::{Profile, render, text};
        for profile in Profile::ALL {
            for theme in [profile.theme(), profile.theme().without_base_ground()] {
                let buf = render(12, 4, |area, buf| {
                    Panel::new(Depth::Raised).draw(area, buf, &theme);
                });
                let edged = text(&buf).starts_with(['┌', '+']);
                let grounded = theme.grounds_differ(Role::Surface, Role::Background);
                assert!(
                    edged || grounded,
                    "{}: raised panel has neither an edge nor its own ground",
                    profile.name()
                );
            }
        }
        let light256 = Profile::Light256.theme();
        assert!(!light256.grounds_differ(Role::Surface, Role::Background));
    }

    #[test]
    fn the_horizon_does_not_fade_into_a_ground_it_did_not_paint() {
        use crate::testing::{Profile, render};
        let theme = Profile::DarkTrue.theme().without_base_ground();
        let buf = render(40, 1, |area, buf| {
            HorizonRule::new().paint(area, buf, &theme)
        });
        for x in 0..40 {
            assert_eq!(
                buf[(x, 0)].fg,
                theme.color(Role::Border).unwrap(),
                "col {x}"
            );
        }
        let painted = Profile::DarkTrue.theme();
        let buf = render(40, 1, |area, buf| {
            HorizonRule::new().paint(area, buf, &painted);
        });
        assert_ne!(buf[(0, 0)].fg, painted.color(Role::Border).unwrap());
    }
}

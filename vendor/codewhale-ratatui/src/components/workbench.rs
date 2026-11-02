//! Composable workspace geometry, quiet module headers, composer context,
//! and the optional Codewhale workbar. Everything shown is a caller fact;
//! these components own neither a session nor a timer.

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
};

use crate::{Paint, Role, Theme, text};

/// Paint one clipped row without asking a terminal widget to walk beyond
/// an offset buffer or the last representable terminal coordinate.
pub(crate) fn row(area: Rect, buf: &mut Buffer, line: &Line<'_>) {
    let area = area.intersection(buf.area);
    if !area.is_empty() {
        buf.set_line(area.x, area.y, line, area.width);
    }
}

fn safe(value: &str) -> String {
    text::display_safe(value).into_owned()
}

fn rule(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let mark = if theme.ascii() { "-" } else { "─" };
    row(
        area,
        buf,
        &Line::styled(mark.repeat(usize::from(area.width)), theme.fg(Role::Border)),
    );
}

/// Regions inside a [`WorkspaceFrame`]. A hidden side pane receives no
/// rectangle: the host can expose it through its own Details action.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceAreas {
    pub main: Rect,
    pub side: Option<Rect>,
    pub footer: Rect,
}

/// A project and branch header, reading canvas, optional inset dock, and a
/// quiet footer. The host paints its conversation and modules in `areas()`.
#[derive(Clone, Debug)]
pub struct WorkspaceFrame<'a> {
    pub project: Cow<'a, str>,
    pub branch: Cow<'a, str>,
    pub mode: Cow<'a, str>,
    pub footer: Cow<'a, str>,
    pub side_width: u16,
    pub minimum_main_width: u16,
}

impl<'a> WorkspaceFrame<'a> {
    #[must_use]
    pub fn new(project: impl Into<Cow<'a, str>>) -> Self {
        Self {
            project: project.into(),
            branch: Cow::Borrowed(""),
            mode: Cow::Borrowed("Workspace"),
            footer: Cow::Borrowed(""),
            side_width: 36,
            minimum_main_width: 56,
        }
    }
    #[must_use]
    pub fn branch(mut self, branch: impl Into<Cow<'a, str>>) -> Self {
        self.branch = branch.into();
        self
    }
    #[must_use]
    pub fn mode(mut self, mode: impl Into<Cow<'a, str>>) -> Self {
        self.mode = mode.into();
        self
    }
    #[must_use]
    pub fn footer(mut self, footer: impl Into<Cow<'a, str>>) -> Self {
        self.footer = footer.into();
        self
    }
    #[must_use]
    pub fn side_width(mut self, width: u16) -> Self {
        self.side_width = width;
        self
    }
    #[must_use]
    pub fn minimum_main_width(mut self, width: u16) -> Self {
        self.minimum_main_width = width;
        self
    }

    /// Layout is pure and deterministic. Pass the intersection of the
    /// requested area and the buffer when painting into an offset buffer.
    #[must_use]
    pub fn areas(&self, area: Rect) -> WorkspaceAreas {
        if area.is_empty() {
            return WorkspaceAreas::default();
        }
        let header = if area.height >= 4 { 3 } else { 1 };
        let footer_height = u16::from(area.height > header + 1);
        let footer = Rect::new(
            area.x,
            area.bottom().saturating_sub(footer_height),
            area.width,
            footer_height,
        );
        let body = Rect::new(
            area.x,
            area.y.saturating_add(header),
            area.width,
            area.height.saturating_sub(header + footer_height),
        );
        let gutter = u16::from(area.width >= 8) * 2;
        let required = usize::from(self.minimum_main_width)
            + usize::from(self.side_width)
            + usize::from(2 * gutter)
            + 1;
        let side_visible = self.side_width >= 8 && usize::from(body.width) >= required;
        if side_visible {
            let split = body.right().saturating_sub(self.side_width + 1);
            WorkspaceAreas {
                main: Rect::new(
                    body.x.saturating_add(gutter),
                    body.y,
                    split.saturating_sub(body.x).saturating_sub(2 * gutter),
                    body.height,
                ),
                side: Some(Rect::new(
                    split.saturating_add(3),
                    body.y,
                    self.side_width.saturating_sub(4),
                    body.height,
                )),
                footer,
            }
        } else {
            WorkspaceAreas {
                main: Rect::new(
                    body.x.saturating_add(gutter),
                    body.y,
                    body.width.saturating_sub(2 * gutter),
                    body.height,
                ),
                side: None,
                footer,
            }
        }
    }
}

impl Paint for WorkspaceFrame<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        buf.set_style(
            area,
            theme.bg(Role::Background).patch(theme.fg(Role::Foreground)),
        );
        let gutter = u16::from(area.width >= 8) * 2;
        let header = Rect::new(
            area.x.saturating_add(gutter),
            area.y,
            area.width.saturating_sub(2 * gutter),
            1,
        );
        let identity = if self.branch.is_empty() {
            safe(&self.project)
        } else {
            format!("{} / {}", safe(&self.project), safe(&self.branch))
        };
        let brand = "Codewhale";
        let mode = safe(&self.mode);
        let mode_width = text::width(&mode);
        let reserve = if usize::from(header.width) > 48 + mode_width {
            mode_width + 2
        } else {
            0
        };
        let left = usize::from(header.width).saturating_sub(reserve);
        let mut spans = vec![Span::styled(
            text::truncate(brand, left, theme.ascii()).into_owned(),
            theme.fg(Role::Accent).add_modifier(Modifier::BOLD),
        )];
        if left > brand.len() + 3 {
            spans.push(Span::styled("  /  ", theme.fg(Role::BorderStrong)));
            spans.push(Span::styled(
                text::truncate_words(
                    &identity,
                    left.saturating_sub(brand.len() + 5),
                    theme.ascii(),
                )
                .into_owned(),
                theme.fg(Role::Muted),
            ));
        }
        row(header, buf, &Line::from(spans));
        if reserve > 0 {
            row(
                Rect::new(
                    header.right().saturating_sub(mode_width as u16),
                    header.y,
                    mode_width as u16,
                    1,
                ),
                buf,
                &Line::styled(
                    mode,
                    theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
                ),
            );
        }
        if area.height >= 4 {
            rule(
                Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
                buf,
                theme,
            );
        }
        let regions = self.areas(area);
        if let Some(side) = regions.side {
            let panel = Rect::new(
                side.x.saturating_sub(2),
                area.y.saturating_add(2),
                side.width.saturating_add(4),
                regions.footer.y.saturating_sub(area.y.saturating_add(2)),
            );
            buf.set_style(panel, theme.bg(Role::Sidebar));
            let x = panel.x.saturating_sub(1);
            for y in panel.y..panel.bottom() {
                row(
                    Rect::new(x, y, 1, 1),
                    buf,
                    &Line::styled(
                        if theme.ascii() { "|" } else { "│" },
                        theme.fg(Role::Border),
                    ),
                );
            }
        }
        if !regions.footer.is_empty() {
            buf.set_style(regions.footer, theme.bg(Role::Sidebar));
            row(
                Rect::new(
                    regions.footer.x.saturating_add(gutter),
                    regions.footer.y,
                    regions.footer.width.saturating_sub(2 * gutter),
                    1,
                ),
                buf,
                &Line::styled(
                    text::truncate_words(
                        &safe(&self.footer),
                        usize::from(regions.footer.width.saturating_sub(2 * gutter)),
                        theme.ascii(),
                    )
                    .into_owned(),
                    theme.fg(Role::Muted),
                ),
            );
        }
    }
}

/// One thin module title and optional right-aligned fact, with a hairline.
#[derive(Clone, Debug)]
pub struct PaneHeader<'a> {
    pub title: Cow<'a, str>,
    pub meta: Cow<'a, str>,
    pub focused: bool,
    pub divider: bool,
}
impl<'a> PaneHeader<'a> {
    #[must_use]
    pub fn new(title: impl Into<Cow<'a, str>>) -> Self {
        Self {
            title: title.into(),
            meta: Cow::Borrowed(""),
            focused: false,
            divider: true,
        }
    }
    #[must_use]
    pub fn meta(mut self, meta: impl Into<Cow<'a, str>>) -> Self {
        self.meta = meta.into();
        self
    }
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
    #[must_use]
    pub fn divider(mut self, divider: bool) -> Self {
        self.divider = divider;
        self
    }
}
impl Paint for PaneHeader<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let title = safe(&self.title);
        let meta = safe(&self.meta);
        let meta_width = text::width(&meta);
        let show_meta =
            !meta.is_empty() && meta_width + text::width(&title) + 2 <= usize::from(area.width);
        let width =
            usize::from(area.width).saturating_sub(if show_meta { meta_width + 2 } else { 0 });
        row(
            area,
            buf,
            &Line::styled(
                text::truncate_words(&title, width, theme.ascii()).into_owned(),
                theme.fg(Role::Foreground).add_modifier(Modifier::BOLD),
            ),
        );
        if show_meta {
            row(
                Rect::new(
                    area.right().saturating_sub(meta_width as u16),
                    area.y,
                    meta_width as u16,
                    1,
                ),
                buf,
                &Line::styled(meta, theme.fg(Role::Muted)),
            );
        }
        if self.divider && area.height > 1 {
            rule(
                Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
                buf,
                theme,
            );
        }
        if self.focused {
            row(
                Rect::new(
                    area.x,
                    area.y
                        .saturating_add(u16::from(self.divider && area.height > 1)),
                    1,
                    1,
                ),
                buf,
                &Line::styled(
                    if theme.ascii() { ">" } else { "╸" },
                    theme.fg(Role::Accent),
                ),
            );
        }
    }
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        1 + u16::from(self.divider)
    }
}

/// A composer-adjacent fact. Larger priorities survive a narrow ribbon.
#[derive(Clone, Debug)]
pub struct ContextItem<'a> {
    pub label: Cow<'a, str>,
    pub value: Cow<'a, str>,
    pub priority: u8,
    pub role: Role,
}
impl<'a> ContextItem<'a> {
    #[must_use]
    pub fn new(label: impl Into<Cow<'a, str>>, value: impl Into<Cow<'a, str>>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
            priority: 0,
            role: Role::Foreground,
        }
    }
    #[must_use]
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }
    #[must_use]
    pub fn role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }
    fn content(&self) -> String {
        if self.label.is_empty() {
            safe(&self.value)
        } else {
            format!("{} {}", safe(&self.label), safe(&self.value))
        }
    }
}

/// Facts in declaration order, folded by priority with an explicit count.
#[derive(Clone, Debug)]
pub struct ContextRibbon<'a> {
    pub items: Vec<ContextItem<'a>>,
    pub details_label: Cow<'a, str>,
}
impl<'a> ContextRibbon<'a> {
    #[must_use]
    pub fn new(items: Vec<ContextItem<'a>>) -> Self {
        Self {
            items,
            details_label: Cow::Borrowed("details"),
        }
    }
    #[must_use]
    pub fn details_label(mut self, label: impl Into<Cow<'a, str>>) -> Self {
        self.details_label = label.into();
        self
    }
}
impl Paint for ContextRibbon<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() || self.items.is_empty() {
            return;
        }
        let separator = if theme.ascii() { " / " } else { " · " };
        let mut kept: Vec<usize> = (0..self.items.len()).collect();
        loop {
            let omitted = self.items.len() - kept.len();
            let more = if omitted == 0 {
                String::new()
            } else {
                format!("+{omitted} {}", safe(&self.details_label))
            };
            if kept.len() == 1 && omitted > 0 && usize::from(area.width) <= text::width(&more) + 3 {
                kept.clear();
                continue;
            }
            let total = kept
                .iter()
                .map(|&i| text::width(&self.items[i].content()))
                .sum::<usize>()
                + 3 * kept.len().saturating_sub(1)
                + if omitted > 0 {
                    usize::from(!kept.is_empty()) * 3 + text::width(&more)
                } else {
                    0
                };
            if total <= usize::from(area.width) || kept.len() <= 1 {
                let remaining = if kept.is_empty() {
                    0
                } else {
                    usize::from(area.width).saturating_sub(if omitted > 0 {
                        text::width(&more) + 3
                    } else {
                        0
                    })
                };
                let mut spans = Vec::new();
                for (position, &index) in kept.iter().enumerate() {
                    if position > 0 {
                        spans.push(Span::styled(separator, theme.fg(Role::BorderStrong)));
                    }
                    let value = self.items[index].content();
                    let fitted = if kept.len() == 1 {
                        text::truncate_words(&value, remaining, theme.ascii()).into_owned()
                    } else {
                        value
                    };
                    spans.push(Span::styled(fitted, theme.fg(self.items[index].role)));
                }
                if omitted > 0 {
                    if remaining > 0 {
                        spans.push(Span::styled(separator, theme.fg(Role::BorderStrong)));
                    }
                    spans.push(Span::styled(
                        text::truncate_words(
                            &more,
                            usize::from(area.width)
                                .saturating_sub(remaining + usize::from(remaining > 0) * 3),
                            theme.ascii(),
                        )
                        .into_owned(),
                        theme.fg(Role::Muted),
                    ));
                }
                row(area, buf, &Line::from(spans));
                break;
            }
            let drop = kept
                .iter()
                .enumerate()
                .min_by_key(|&(position, &index)| {
                    (self.items[index].priority, std::cmp::Reverse(position))
                })
                .map(|(position, _)| position)
                .unwrap_or(0);
            kept.remove(drop);
        }
    }
}

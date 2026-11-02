//! Codewhale's work dock: Tasks, Fleet, Jobs, Terminal, Files, Notes, Context, Git, Cost.
//!
//! Extracted from `CodeWhale/crates/tui/src/tui/work_surface/{model,input,
//! render/{mod,layout,rows}}.rs` at `a79ce5c4d5ed1a5f7032185710c27343a900351c`.
//! The host supplies rows and dispatches actions; the dock retains the native
//! tab fit, row columns, placement, goal header, and overflow behavior.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::{Paint, Role, Theme, TuiInk, glyphs, text};

use super::workbench::row;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WorkbarPanel {
    #[default]
    Tasks,
    Fleet,
    Jobs,
    Terminal,
    Files,
    Notes,
    Context,
    Git,
    Cost,
}

impl WorkbarPanel {
    pub const ORDER: [Self; 9] = [
        Self::Tasks,
        Self::Fleet,
        Self::Jobs,
        Self::Terminal,
        Self::Files,
        Self::Notes,
        Self::Context,
        Self::Git,
        Self::Cost,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tasks => "Tasks",
            Self::Fleet => "Fleet",
            Self::Jobs => "Jobs",
            Self::Terminal => "Terminal",
            Self::Files => "Files",
            Self::Notes => "Notes",
            Self::Context => "Context",
            Self::Git => "Git",
            Self::Cost => "Cost",
        }
    }

    #[must_use]
    pub const fn empty_label(self) -> &'static str {
        match self {
            Self::Tasks => "no to-dos yet",
            Self::Fleet => "no agents have run this session",
            Self::Jobs => "nothing running in the background",
            Self::Terminal => "",
            Self::Files => "no files touched this session",
            Self::Notes => "/note add <text> to keep a note",
            Self::Context => "context budget unknown",
            Self::Git => "reading git status…",
            Self::Cost => "no priced turns yet",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        Self::ORDER[(self.index() + 1) % Self::ORDER.len()]
    }

    #[must_use]
    pub fn prev(self) -> Self {
        Self::ORDER[(self.index() + Self::ORDER.len() - 1) % Self::ORDER.len()]
    }

    fn index(self) -> usize {
        Self::ORDER
            .iter()
            .position(|panel| *panel == self)
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkbarPlacement {
    #[default]
    Bottom,
    Top,
    Left,
    Right,
    Off,
}

impl WorkbarPlacement {
    #[must_use]
    pub const fn is_strip(self) -> bool {
        matches!(self, Self::Top | Self::Bottom)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkbarTone {
    Heading,
    Live,
    Attention,
    Failure,
    Success,
    #[default]
    Muted,
}

impl WorkbarTone {
    fn style(self, theme: &Theme) -> Style {
        match self {
            Self::Heading | Self::Muted => theme.fg(Role::Muted),
            Self::Live => theme.tui_ink(TuiInk::Working),
            Self::Success => theme.tui_ink(TuiInk::Success),
            Self::Attention => theme.tui_ink(TuiInk::Warning),
            Self::Failure => theme.fg(Role::Danger),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkbarTab {
    pub panel: WorkbarPanel,
    /// `None` for fact views, matching the native unbadged Context/Git/Cost.
    pub count: Option<usize>,
}

impl WorkbarTab {
    #[must_use]
    pub const fn new(panel: WorkbarPanel) -> Self {
        Self { panel, count: None }
    }

    #[must_use]
    pub const fn count(mut self, count: usize) -> Self {
        self.count = if matches!(
            self.panel,
            WorkbarPanel::Context | WorkbarPanel::Git | WorkbarPanel::Cost
        ) {
            None
        } else {
            Some(count)
        };
        self
    }
}

/// Fleet columns drop tokens, then the receipt, then identity as room shrinks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkbarAgent {
    pub role: String,
    pub status: String,
    pub objective: String,
    pub model: Option<String>,
    pub elapsed_seconds: Option<u64>,
    pub tokens: Option<u64>,
    pub remaining: Option<u32>,
}

impl WorkbarAgent {
    #[must_use]
    pub fn new(
        role: impl Into<String>,
        status: impl Into<String>,
        objective: impl Into<String>,
    ) -> Self {
        Self {
            role: role.into(),
            status: status.into(),
            objective: objective.into(),
            ..Self::default()
        }
    }
    #[must_use]
    pub fn model(mut self, value: impl Into<String>) -> Self {
        self.model = Some(value.into());
        self
    }
    #[must_use]
    pub const fn elapsed_seconds(mut self, value: u64) -> Self {
        self.elapsed_seconds = Some(value);
        self
    }
    #[must_use]
    pub const fn tokens(mut self, value: u64) -> Self {
        self.tokens = Some(value);
        self
    }
    #[must_use]
    pub const fn remaining(mut self, value: u32) -> Self {
        self.remaining = Some(value);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkbarRow {
    pub id: String,
    pub mark: String,
    pub label: String,
    pub detail: String,
    pub tone: WorkbarTone,
    pub selectable: bool,
    pub agent: Option<WorkbarAgent>,
}

impl WorkbarRow {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            mark: "○".into(),
            label: label.into(),
            detail: String::new(),
            tone: WorkbarTone::Muted,
            selectable: true,
            agent: None,
        }
    }
    #[must_use]
    pub fn mark(mut self, value: impl Into<String>) -> Self {
        self.mark = value.into();
        self
    }
    #[must_use]
    pub fn detail(mut self, value: impl Into<String>) -> Self {
        self.detail = value.into();
        self
    }
    #[must_use]
    pub const fn tone(mut self, value: WorkbarTone) -> Self {
        self.tone = value;
        self
    }
    #[must_use]
    pub const fn selectable(mut self, value: bool) -> Self {
        self.selectable = value;
        self
    }
    #[must_use]
    pub fn agent(mut self, value: WorkbarAgent) -> Self {
        self.agent = Some(value);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkbarTarget {
    Panel(WorkbarPanel),
    Row(String),
    Close,
    Divider,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkbarHitbox {
    pub target: WorkbarTarget,
    pub area: Rect,
}

/// Split the host area once, then paint the dock with this effective placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkbarRegions {
    pub content: Rect,
    pub dock: Option<Rect>,
    pub placement: WorkbarPlacement,
}

/// Geometry used by painting, pointer targets, and keyboard scrolling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkbarLayout {
    pub body: Rect,
    pub content: Rect,
    pub visible_rows: usize,
    pub offset: usize,
    pub overflow: bool,
    pub more_row: bool,
    pub goal_height: u16,
    pub progress_height: u16,
}

impl WorkbarLayout {
    #[must_use]
    pub const fn progress_shares_goal_row(width: u16, has_goal: bool) -> bool {
        has_goal && width >= 72
    }

    /// Fit headers, visible rows, overflow reserve and offset inside the caller's
    /// existing body viewport. Placement/focus/action policy is not inferred.
    #[must_use]
    pub fn for_body(
        body: Rect,
        total_rows: usize,
        offset: usize,
        has_goal: bool,
        has_progress: bool,
    ) -> Self {
        let goal_height = u16::from(has_goal && body.height >= 2);
        let fold = Self::progress_shares_goal_row(body.width, goal_height > 0);
        let progress_height =
            u16::from(has_progress && !fold && body.height.saturating_sub(goal_height) >= 2);
        let header = goal_height.saturating_add(progress_height);
        let list_height = body.height.saturating_sub(header);
        let overflow = total_rows > usize::from(list_height);
        let more_row = overflow && list_height >= 2;
        let visible_rows = usize::from(list_height).saturating_sub(usize::from(more_row));
        let offset = offset.min(total_rows.saturating_sub(visible_rows.max(1)));
        let inset = u16::from(body.width >= 16);
        let content = Rect {
            x: body.x.saturating_add(inset),
            y: body.y.saturating_add(header),
            width: body
                .width
                .saturating_sub(inset.saturating_mul(2))
                .saturating_sub(u16::from(overflow)),
            height: list_height,
        };
        Self {
            body,
            content,
            visible_rows,
            offset,
            overflow,
            more_row,
            goal_height,
            progress_height,
        }
    }
}

/// The fitted body rail, over caller-supplied symbols and live styles.
/// Both the native Engine and Workbar use this paint path. No scroll state is
/// retained; the current admitted offset/row counts enter on each draw.
#[derive(Clone, Copy, Debug)]
pub struct WorkbarScrollbar<'a> {
    pub offset: usize,
    pub visible: usize,
    pub total: usize,
    pub thumb: &'a str,
    pub track: &'a str,
    pub thumb_style: Style,
    pub track_style: Style,
}

impl WorkbarScrollbar<'_> {
    pub fn paint(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() || self.total == 0 {
            return;
        }
        let height = usize::from(area.height);
        let thumb_height = height.saturating_mul(self.visible) / self.total;
        let thumb_height = thumb_height.max(1).min(height);
        let start = self
            .offset
            .saturating_mul(height.saturating_sub(thumb_height))
            / self.total.saturating_sub(self.visible).max(1);
        let x = area.right().saturating_sub(1);
        for row in 0..height {
            let active = row >= start && row < start.saturating_add(thumb_height);
            if let Some(cell) = buf.cell_mut((x, area.y.saturating_add(row as u16))) {
                cell.set_symbol(if active { self.thumb } else { self.track })
                    .set_style(if active {
                        self.thumb_style
                    } else {
                        self.track_style
                    });
            }
        }
    }
}

/// The actual Codewhale dock, with application projections replaced by rows.
#[derive(Clone, Debug)]
pub struct Workbar {
    pub panel: WorkbarPanel,
    pub rows: Vec<WorkbarRow>,
    pub tabs: Vec<WorkbarTab>,
    pub placement: WorkbarPlacement,
    pub goal: Option<String>,
    pub goal_paused: bool,
    pub progress: Option<String>,
    pub selected: Option<String>,
    pub hovered: Option<String>,
    pub opened: Option<String>,
    pub focused: bool,
    pub offset: usize,
    pub max_height: u16,
    pub side_width: u16,
    pub explicit: bool,
}

impl Workbar {
    #[must_use]
    pub fn new(panel: WorkbarPanel, rows: Vec<WorkbarRow>) -> Self {
        let tabs = WorkbarPanel::ORDER
            .into_iter()
            .filter(|value| {
                *value == panel
                    || matches!(
                        value,
                        WorkbarPanel::Context | WorkbarPanel::Git | WorkbarPanel::Cost
                    )
            })
            .map(WorkbarTab::new)
            .collect();
        Self {
            panel,
            rows,
            tabs,
            placement: WorkbarPlacement::Bottom,
            goal: None,
            goal_paused: false,
            progress: None,
            selected: None,
            hovered: None,
            opened: None,
            focused: false,
            offset: 0,
            max_height: 5,
            side_width: 30,
            explicit: true,
        }
    }
    #[must_use]
    pub fn tabs(mut self, value: Vec<WorkbarTab>) -> Self {
        self.tabs = value;
        self
    }
    #[must_use]
    pub const fn placement(mut self, value: WorkbarPlacement) -> Self {
        self.placement = value;
        self
    }
    #[must_use]
    pub fn goal(mut self, value: impl Into<String>) -> Self {
        self.goal = Some(value.into());
        self
    }
    #[must_use]
    pub const fn paused_goal(mut self, paused: bool) -> Self {
        self.goal_paused = paused;
        self
    }
    #[must_use]
    pub fn progress(mut self, value: impl Into<String>) -> Self {
        self.progress = Some(value.into());
        self
    }
    #[must_use]
    pub fn selected(mut self, value: impl Into<String>) -> Self {
        self.selected = Some(value.into());
        self
    }
    #[must_use]
    pub fn hovered(mut self, value: impl Into<String>) -> Self {
        self.hovered = Some(value.into());
        self
    }
    #[must_use]
    pub fn opened(mut self, value: impl Into<String>) -> Self {
        self.opened = Some(value.into());
        self
    }
    #[must_use]
    pub const fn focused(mut self, value: bool) -> Self {
        self.focused = value;
        self
    }
    #[must_use]
    pub const fn offset(mut self, value: usize) -> Self {
        self.offset = value;
        self
    }
    #[must_use]
    pub fn max_height(mut self, value: u16) -> Self {
        self.max_height = value.clamp(3, 16);
        self
    }
    #[must_use]
    pub fn side_width(mut self, value: u16) -> Self {
        self.side_width = value.clamp(26, 80);
        self
    }
    /// Automatic docks disappear when there are neither rows nor a live goal.
    #[must_use]
    pub const fn explicit(mut self, value: bool) -> Self {
        self.explicit = value;
        self
    }

    fn goal_text(&self) -> Option<String> {
        let goal = safe(self.goal.as_deref()?);
        if goal.trim().is_empty() {
            return None;
        }
        Some(format!(
            "Goal{}: {}",
            if self.goal_paused { " (paused)" } else { "" },
            goal.trim()
        ))
    }

    fn requested_height(&self, width: u16) -> u16 {
        if width == 0
            || self.placement == WorkbarPlacement::Off
            || (self.rows.is_empty() && !self.explicit && self.goal_text().is_none())
        {
            return 0;
        }
        if !self.placement.is_strip() {
            return 0;
        }
        let goal = u16::from(self.goal_text().is_some());
        let progress = u16::from(
            self.progress.is_some() && !WorkbarLayout::progress_shares_goal_row(width, goal > 0),
        );
        (u16::try_from(self.rows.len().max(usize::from(self.explicit)))
            .unwrap_or(u16::MAX)
            .saturating_add(goal)
            .saturating_add(progress)
            .saturating_add(2))
        .clamp(5.min(self.max_height), self.max_height)
    }

    /// Native auto-fit ceiling: half the terminal, the configured height, and
    /// the rows the conversation can spare. Empty automatic docks collapse.
    #[must_use]
    pub fn height_for(&self, width: u16, terminal_height: u16, rail_budget: u16) -> u16 {
        let cap = self
            .max_height
            .min((terminal_height / 2).clamp(5, 16))
            .min(rail_budget);
        if cap < 5 && (!self.explicit || cap < 3) {
            return 0;
        }
        self.requested_height(width).min(cap)
    }

    #[must_use]
    pub fn regions(&self, host: Rect) -> WorkbarRegions {
        let host = representable(host);
        let empty = self.rows.is_empty() && !self.explicit && self.goal_text().is_none();
        if host.is_empty() || self.placement == WorkbarPlacement::Off || empty {
            return WorkbarRegions {
                content: host,
                dock: None,
                placement: self.placement,
            };
        }
        let mut placement = self.placement;
        if matches!(placement, WorkbarPlacement::Left | WorkbarPlacement::Right) && host.width < 72
        {
            placement = WorkbarPlacement::Top;
        }
        if placement.is_strip() {
            let height = self
                .clone()
                .placement(placement)
                .requested_height(host.width)
                .min(host.height / 2);
            if height < 3 {
                return WorkbarRegions {
                    content: host,
                    dock: None,
                    placement,
                };
            }
            let (content, dock) = if placement == WorkbarPlacement::Bottom {
                (
                    Rect {
                        height: host.height - height,
                        ..host
                    },
                    Rect {
                        y: host.bottom() - height,
                        height,
                        ..host
                    },
                )
            } else {
                (
                    Rect {
                        y: host.y.saturating_add(height),
                        height: host.height - height,
                        ..host
                    },
                    Rect { height, ..host },
                )
            };
            return WorkbarRegions {
                content,
                dock: Some(dock),
                placement,
            };
        }
        let width = self.side_width.min(host.width.saturating_sub(40));
        if width < 26 {
            return self.clone().placement(WorkbarPlacement::Top).regions(host);
        }
        let (content, dock) = if placement == WorkbarPlacement::Left {
            (
                Rect {
                    x: host.x.saturating_add(width),
                    width: host.width - width,
                    ..host
                },
                Rect { width, ..host },
            )
        } else {
            (
                Rect {
                    width: host.width - width,
                    ..host
                },
                Rect {
                    x: host.right() - width,
                    width,
                    ..host
                },
            )
        };
        WorkbarRegions {
            content,
            dock: Some(dock),
            placement,
        }
    }

    #[must_use]
    pub fn layout(&self, area: Rect) -> WorkbarLayout {
        let area = representable(area);
        let body = match self.placement {
            WorkbarPlacement::Top => Rect {
                y: area.y.saturating_add(1).min(area.bottom()),
                height: area.height.saturating_sub(2),
                ..area
            },
            WorkbarPlacement::Bottom => Rect {
                y: area.y.saturating_add(2).min(area.bottom()),
                height: area.height.saturating_sub(2),
                ..area
            },
            WorkbarPlacement::Left => Rect {
                width: area.width.saturating_sub(1),
                ..area
            },
            WorkbarPlacement::Right => Rect {
                x: area.x.saturating_add(1).min(area.right()),
                width: area.width.saturating_sub(1),
                ..area
            },
            WorkbarPlacement::Off => Rect { height: 0, ..area },
        };
        WorkbarLayout::for_body(
            body,
            self.rows.len(),
            self.offset,
            self.placement.is_strip() && self.goal_text().is_some(),
            self.placement.is_strip() && self.progress.is_some(),
        )
    }

    fn dock_tab_row(&self, area: Rect, theme: Option<&Theme>) -> super::DockTabRow<'_> {
        let styles = theme.map_or_else(super::DockTabStyles::default, |theme| {
            super::DockTabStyles {
                idle: theme.fg(Role::Muted).patch(theme.bg(Role::Sidebar)),
                active: theme
                    .fg(Role::Foreground)
                    .patch(theme.bg(Role::Selected))
                    .add_modifier(Modifier::BOLD),
                close: theme.fg(Role::Hint),
                ..Default::default()
            }
        });
        let close = if self.focused && area.width >= 60 {
            " Esc × "
        } else {
            " × "
        };
        super::DockTabRow {
            tabs: &self.tabs,
            active: self.panel,
            bottom: self.placement == WorkbarPlacement::Bottom,
            close: theme.map_or_else(|| close.into(), |theme| decorative(close, theme).into()),
            hovered: None,
            pressed: None,
            styles,
        }
    }

    #[must_use]
    /// Intersect `area` with the render buffer before querying a clipped view.
    pub fn hitboxes(&self, area: Rect) -> Vec<WorkbarHitbox> {
        let area = representable(area);
        if area.is_empty() || self.placement == WorkbarPlacement::Off {
            return Vec::new();
        }
        let layout = self.layout(area);
        let mut hitboxes = Vec::new();
        if self.placement.is_strip() && area.height >= 2 {
            hitboxes.extend(
                self.dock_tab_row(area, None)
                    .plan(area)
                    .hitboxes()
                    .into_iter()
                    .map(|(target, area)| WorkbarHitbox {
                        target: match target {
                            super::DockTabTarget::Panel(panel) => WorkbarTarget::Panel(panel),
                            super::DockTabTarget::Close => WorkbarTarget::Close,
                        },
                        area,
                    }),
            );
        }
        if layout.content.width > 0 {
            hitboxes.extend(
                self.rows
                    .iter()
                    .skip(layout.offset)
                    .take(layout.visible_rows)
                    .enumerate()
                    .filter(|(_, row)| row.selectable)
                    .map(|(index, value)| WorkbarHitbox {
                        target: WorkbarTarget::Row(value.id.clone()),
                        area: Rect::new(
                            layout.content.x,
                            layout.content.y.saturating_add(index as u16),
                            layout.content.width,
                            1,
                        ),
                    }),
            );
        }
        hitboxes.push(WorkbarHitbox {
            target: WorkbarTarget::Divider,
            area: self.divider(area),
        });
        hitboxes
    }

    #[must_use]
    pub fn target_at(&self, area: Rect, column: u16, row: u16) -> Option<WorkbarTarget> {
        self.hitboxes(area)
            .into_iter()
            .find(|hitbox| hitbox.area.contains((column, row).into()))
            .map(|hitbox| hitbox.target)
    }

    fn divider(&self, area: Rect) -> Rect {
        match self.placement {
            WorkbarPlacement::Top => Rect {
                y: area.bottom().saturating_sub(1),
                height: 1,
                ..area
            },
            WorkbarPlacement::Bottom => Rect { height: 1, ..area },
            WorkbarPlacement::Left => Rect {
                x: area.right().saturating_sub(1),
                width: 1,
                ..area
            },
            WorkbarPlacement::Right => Rect { width: 1, ..area },
            WorkbarPlacement::Off => Rect { height: 0, ..area },
        }
    }

    fn row_style(&self, value: &WorkbarRow, theme: &Theme) -> Style {
        let mut style = value.tone.style(theme).patch(theme.bg(Role::Sidebar));
        if value.tone == WorkbarTone::Heading {
            style = style.add_modifier(Modifier::BOLD);
        }
        if !value.selectable {
            return style;
        }
        if self.opened.as_deref() == Some(&value.id) {
            style = style
                .patch(theme.fg(Role::Accent))
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        if self.focused && self.selected.as_deref() == Some(&value.id) {
            style = style
                .patch(theme.bg(Role::Selected))
                .add_modifier(Modifier::BOLD);
        } else if self.hovered.as_deref() == Some(&value.id) {
            style = style.patch(theme.bg(Role::Hover));
        }
        style
    }
}

impl Paint for Workbar {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty()
            || self.placement == WorkbarPlacement::Off
            || (self.rows.is_empty() && !self.explicit && self.goal_text().is_none())
        {
            return;
        }
        let layout = self.layout(area);
        for y in area.y..area.bottom() {
            row(
                Rect::new(area.x, y, area.width, 1),
                buf,
                &Line::styled(
                    " ".repeat(usize::from(area.width)),
                    theme.fg(Role::Foreground).patch(theme.bg(Role::Sidebar)),
                ),
            );
        }
        if self.placement.is_strip() && area.height >= 2 {
            self.dock_tab_row(area, Some(theme)).plan(area).paint(buf);
        }
        if let Some(goal) = self.goal_text().filter(|_| layout.goal_height > 0) {
            let goal = decorative(&goal, theme);
            let receipt = self
                .progress
                .as_deref()
                .filter(|_| {
                    WorkbarLayout::progress_shares_goal_row(
                        layout.body.width,
                        layout.goal_height > 0,
                    )
                })
                .map(|value| decorative(value, theme));
            let reserved = receipt
                .as_deref()
                .map(|value| text::width(value) + 2)
                .unwrap_or(0);
            let width = usize::from(layout.content.width);
            let goal =
                text::truncate(&goal, width.saturating_sub(reserved), theme.ascii()).into_owned();
            let mut spans = vec![Span::styled(
                goal.clone(),
                theme
                    .tui_ink(if self.goal_paused {
                        TuiInk::Warning
                    } else {
                        TuiInk::Working
                    })
                    .add_modifier(Modifier::BOLD),
            )];
            if let Some(receipt) = receipt {
                let gap = width
                    .saturating_sub(text::width(&goal))
                    .saturating_sub(text::width(&receipt));
                spans.push(Span::styled(
                    format!("{}{receipt}", " ".repeat(gap)),
                    theme.fg(Role::Muted),
                ));
            }
            row(
                Rect {
                    y: layout.body.y,
                    height: 1,
                    ..layout.content
                },
                buf,
                &Line::from(spans),
            );
        }
        if let Some(progress) = self
            .progress
            .as_deref()
            .filter(|_| layout.progress_height > 0)
        {
            row(
                Rect {
                    y: layout.body.y.saturating_add(layout.goal_height),
                    height: 1,
                    ..layout.content
                },
                buf,
                &Line::styled(
                    text::truncate(
                        &decorative(progress, theme),
                        usize::from(layout.content.width),
                        theme.ascii(),
                    )
                    .into_owned(),
                    theme.fg(Role::Muted),
                ),
            );
        }
        let visible: Vec<_> = self
            .rows
            .iter()
            .skip(layout.offset)
            .take(layout.visible_rows)
            .collect();
        let identity_cap = usize::from(layout.content.width) * 2 / 5;
        let identity_column = visible
            .iter()
            .map(|value| text::width(&agent_identity(value, identity_cap)))
            .max()
            .unwrap_or(0);
        let status_column = visible
            .iter()
            .filter_map(|value| value.agent.as_ref())
            .map(|facts| text::width(&safe(&facts.status)))
            .max()
            .unwrap_or(0);
        let ordinal_count = self
            .rows
            .iter()
            .filter(|value| value.id.starts_with("graph:"))
            .count();
        let ordinal_width = ordinal_count.max(1).to_string().len();
        for (index, value) in visible.iter().enumerate() {
            let target = Rect::new(
                layout.content.x,
                layout.content.y.saturating_add(index as u16),
                layout.content.width,
                1,
            );
            let ordinal = if self.placement.is_strip() && value.id.starts_with("graph:") {
                let number = self
                    .rows
                    .iter()
                    .take(layout.offset + index + 1)
                    .filter(|value| value.id.starts_with("graph:"))
                    .count();
                format!("{number:>ordinal_width$} · ")
            } else {
                String::new()
            };
            let mark = if self.opened.as_deref() == Some(&value.id) && value.selectable {
                "▾"
            } else {
                &value.mark
            };
            let prefix = format!(
                "{}{} ",
                decorative(&ordinal, theme),
                decorative(mark, theme)
            );
            let width = usize::from(target.width);
            let style = self.row_style(value, theme);
            if let Some(facts) = &value.agent {
                let selected = self.focused && self.selected.as_deref() == Some(&value.id);
                let mut normal = theme.fg(Role::Foreground).patch(theme.bg(if selected {
                    Role::Selected
                } else if self.hovered.as_deref() == Some(&value.id) {
                    Role::Hover
                } else {
                    Role::Sidebar
                }));
                let mut muted = theme.fg(Role::Muted).patch(normal);
                // Secondary columns keep their own muted ink.
                muted = muted.patch(theme.fg(Role::Muted));
                if selected {
                    normal = normal.add_modifier(Modifier::BOLD);
                    muted = muted.add_modifier(Modifier::BOLD);
                }
                if self.opened.as_deref() == Some(&value.id) {
                    normal = normal
                        .patch(theme.fg(Role::Accent))
                        .add_modifier(Modifier::UNDERLINED);
                    muted = muted
                        .patch(theme.fg(Role::Accent))
                        .add_modifier(Modifier::UNDERLINED);
                }
                let columns = agent_columns(
                    facts,
                    width,
                    text::width(&prefix),
                    &agent_identity(value, identity_cap),
                    identity_column,
                    status_column,
                    theme,
                );
                row(
                    target,
                    buf,
                    &Line::from(vec![
                        Span::styled(prefix, normal),
                        Span::styled(columns.0, muted),
                        Span::styled(columns.1, normal),
                        Span::styled(columns.2, muted),
                    ]),
                );
            } else {
                let label = decorative(&value.label, theme);
                let label = text::truncate(
                    &label,
                    width.saturating_sub(text::width(&prefix)).max(1),
                    theme.ascii(),
                );
                let detail_candidate = if value.tone != WorkbarTone::Heading && target.width >= 44 {
                    format!("  {}", decorative(&value.detail, theme))
                } else {
                    String::new()
                };
                let detail_budget =
                    width.saturating_sub(text::width(&prefix) + text::width(&label));
                let detail = if detail_budget >= 4 {
                    text::truncate(&detail_candidate, detail_budget, theme.ascii()).into_owned()
                } else {
                    String::new()
                };
                let gap = width.saturating_sub(
                    text::width(&prefix) + text::width(&label) + text::width(&detail),
                );
                row(
                    target,
                    buf,
                    &Line::styled(format!("{prefix}{label}{}{detail}", " ".repeat(gap)), style),
                );
            }
        }
        if visible.is_empty() && self.explicit && layout.content.height > 0 {
            row(
                Rect {
                    height: 1,
                    ..layout.content
                },
                buf,
                &Line::styled(
                    text::truncate(
                        &decorative(self.panel.empty_label(), theme),
                        usize::from(layout.content.width),
                        theme.ascii(),
                    )
                    .into_owned(),
                    theme.fg(Role::Muted),
                ),
            );
        }
        if layout.more_row {
            let remaining = self
                .rows
                .len()
                .saturating_sub(layout.offset + visible.len());
            let value = if remaining == 0 {
                String::new()
            } else {
                format!("{} {remaining} more", if theme.ascii() { "v" } else { "↓" })
            };
            let value = text::truncate(&value, usize::from(layout.content.width), theme.ascii());
            let pad = usize::from(layout.content.width).saturating_sub(text::width(&value));
            row(
                Rect::new(
                    layout.content.x,
                    layout.content.bottom().saturating_sub(1),
                    layout.content.width,
                    1,
                ),
                buf,
                &Line::styled(format!("{}{value}", " ".repeat(pad)), theme.fg(Role::Muted)),
            );
        }
        let divider = self.divider(area);
        let mark = if self.placement.is_strip() {
            if theme.ascii() { "-" } else { "─" }
        } else if theme.ascii() {
            "|"
        } else {
            "│"
        };
        for y in divider.y..divider.bottom() {
            row(
                Rect::new(divider.x, y, divider.width, 1),
                buf,
                &Line::styled(
                    mark.repeat(usize::from(divider.width)),
                    theme.fg(Role::Border),
                ),
            );
        }
        if layout.overflow && layout.content.height > 0 && layout.body.width > 0 {
            WorkbarScrollbar {
                offset: layout.offset,
                visible: layout.visible_rows,
                total: self.rows.len(),
                thumb: if theme.ascii() { "|" } else { "┃" },
                track: if theme.ascii() { "|" } else { "│" },
                thumb_style: theme.tui_ink(TuiInk::Working),
                track_style: theme.fg(Role::Border),
            }
            .paint(
                Rect::new(
                    layout.body.right().saturating_sub(1),
                    layout.content.y,
                    1,
                    layout.content.height,
                ),
                buf,
            );
        }
    }

    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        self.requested_height(width)
    }
}

fn safe(value: &str) -> String {
    text::display_safe(&value.replace(['\n', '\r', '\t'], " ")).into_owned()
}

fn representable(area: Rect) -> Rect {
    Rect {
        width: area.width.min(u16::MAX.saturating_sub(area.x)),
        height: area.height.min(u16::MAX.saturating_sub(area.y)),
        ..area
    }
}

fn decorative(value: &str, theme: &Theme) -> String {
    let value = safe(value);
    if !theme.ascii() {
        return value;
    }
    value
        .chars()
        .map(|value| {
            match value {
                '✎' => return "*".to_owned(),
                '⎇' => return "@".to_owned(),
                '◔' => return "o".to_owned(),
                '±' => return "+/-".to_owned(),
                '−' => return "-".to_owned(),
                _ => {}
            }
            let mut bytes = [0; 4];
            let glyph = value.encode_utf8(&mut bytes);
            glyphs::ascii_fallback(glyph)
                .map(str::to_owned)
                .unwrap_or_else(|| glyph.to_owned())
        })
        .collect()
}

fn agent_identity(value: &WorkbarRow, cap: usize) -> String {
    let Some(facts) = &value.agent else {
        return String::new();
    };
    for candidate in [&value.label, &facts.role] {
        let candidate = safe(candidate);
        if !candidate.is_empty() && text::width(&candidate) <= cap {
            return candidate;
        }
    }
    String::new()
}

fn elapsed(seconds: u64) -> String {
    if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
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

fn agent_receipt(facts: &WorkbarAgent, tier: usize, theme: &Theme) -> String {
    if tier >= 2 {
        return String::new();
    }
    let mut parts = Vec::new();
    if let Some(value) = &facts.model {
        let value = safe(value);
        if !value.is_empty() {
            parts.push(value);
        }
    }
    if let Some(value) = facts.elapsed_seconds {
        parts.push(elapsed(value));
    }
    if tier == 0
        && let Some(value) = facts.tokens
    {
        parts.push(format!(
            "{} {} tokens",
            if theme.ascii() { "v" } else { "↓" },
            tokens(value)
        ));
    }
    if let Some(value) = facts.remaining.filter(|value| *value > 0) {
        parts.push(format!("{value} left"));
    }
    parts.join(if theme.ascii() { " . " } else { " · " })
}

fn agent_columns(
    facts: &WorkbarAgent,
    width: usize,
    prefix: usize,
    identity: &str,
    identity_column: usize,
    status_column: usize,
    theme: &Theme,
) -> (String, String, String) {
    for tier in 0..4 {
        let receipt = agent_receipt(facts, tier, theme);
        let identity = if tier == 3 || identity_column == 0 {
            String::new()
        } else {
            format!("{}  ", text::pad(identity, identity_column, theme.ascii()))
        };
        let status = if tier == 3 || status_column == 0 {
            String::new()
        } else {
            format!(
                "{}  ",
                text::pad(&safe(&facts.status), status_column, theme.ascii())
            )
        };
        let secondary = format!("{identity}{status}");
        let receipt_cost = if receipt.is_empty() {
            0
        } else {
            text::width(&receipt) + 2
        };
        let budget = width
            .saturating_sub(prefix)
            .saturating_sub(text::width(&secondary))
            .saturating_sub(receipt_cost);
        if budget < 24 && tier != 3 {
            continue;
        }
        let objective = safe(&facts.objective);
        let objective = text::truncate(&objective, budget, theme.ascii()).into_owned();
        let gap = width
            .saturating_sub(prefix)
            .saturating_sub(text::width(&secondary))
            .saturating_sub(text::width(&objective))
            .saturating_sub(text::width(&receipt));
        return (
            secondary,
            objective,
            format!("{}{receipt}", " ".repeat(gap)),
        );
    }
    (String::new(), String::new(), String::new())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkbarOutcome {
    Ignored,
    Changed,
    Panel(WorkbarPanel),
    Activate(String),
    Close,
    ReleaseFocus,
}

/// Optional native keyboard state. Enter returns a row id for host dispatch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkbarState {
    pub panel: WorkbarPanel,
    pub selected: Option<String>,
    pub focused: bool,
    pub offset: usize,
}

impl WorkbarState {
    #[must_use]
    pub fn apply(&self, mut workbar: Workbar) -> Workbar {
        workbar.panel = self.panel;
        workbar.selected = self.selected.clone();
        workbar.focused = self.focused;
        workbar.offset = self.offset;
        workbar
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        rows: &[WorkbarRow],
        visible_rows: usize,
    ) -> WorkbarOutcome {
        if key.kind == KeyEventKind::Release {
            return WorkbarOutcome::Ignored;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        if control
            && matches!(
                key.code,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Char(']')
            )
        {
            self.panel =
                if key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.panel.prev()
                } else {
                    self.panel.next()
                };
            self.selected = None;
            self.offset = 0;
            return WorkbarOutcome::Panel(self.panel);
        }
        if visible_rows == 0 {
            self.focused = false;
            return WorkbarOutcome::Ignored;
        }
        if !self.focused {
            if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::ALT) {
                self.focused = true;
                if !rows
                    .iter()
                    .any(|value| value.selectable && self.selected.as_deref() == Some(&value.id))
                {
                    self.selected = rows
                        .iter()
                        .find(|value| value.selectable)
                        .map(|value| value.id.clone());
                }
                return WorkbarOutcome::Changed;
            }
            return WorkbarOutcome::Ignored;
        }
        if matches!(key.code, KeyCode::Char(_))
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            self.focused = false;
            return WorkbarOutcome::ReleaseFocus;
        }
        match key.code {
            KeyCode::Esc => {
                self.focused = false;
                self.selected = None;
                WorkbarOutcome::Close
            }
            KeyCode::Left | KeyCode::Right => {
                self.panel = if key.code == KeyCode::Left {
                    self.panel.prev()
                } else {
                    self.panel.next()
                };
                self.selected = None;
                self.offset = 0;
                WorkbarOutcome::Panel(self.panel)
            }
            KeyCode::Enter => self
                .selected
                .as_ref()
                .filter(|id| {
                    rows.iter()
                        .any(|value| &value.id == *id && value.selectable)
                })
                .map(|id| WorkbarOutcome::Activate(id.clone()))
                .unwrap_or(WorkbarOutcome::Ignored),
            KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End => {
                let selectable: Vec<_> = rows
                    .iter()
                    .enumerate()
                    .filter(|(_, value)| value.selectable)
                    .collect();
                if selectable.is_empty() {
                    self.selected = None;
                    return WorkbarOutcome::Ignored;
                }
                let current = selectable
                    .iter()
                    .position(|(_, value)| self.selected.as_deref() == Some(&value.id))
                    .unwrap_or(0);
                let next = match key.code {
                    KeyCode::Up => current.saturating_sub(1),
                    KeyCode::Down => (current + 1).min(selectable.len() - 1),
                    KeyCode::Home => 0,
                    _ => selectable.len() - 1,
                };
                let (index, value) = selectable[next];
                self.selected = Some(value.id.clone());
                if index < self.offset {
                    self.offset = index;
                } else if index >= self.offset.saturating_add(visible_rows.max(1)) {
                    self.offset = index.saturating_add(1).saturating_sub(visible_rows.max(1));
                }
                WorkbarOutcome::Changed
            }
            KeyCode::PageUp => {
                self.offset = self.offset.saturating_sub(visible_rows.max(1));
                WorkbarOutcome::Changed
            }
            KeyCode::PageDown => {
                self.offset = self
                    .offset
                    .saturating_add(visible_rows.max(1))
                    .min(rows.len().saturating_sub(visible_rows.max(1)));
                WorkbarOutcome::Changed
            }
            _ => WorkbarOutcome::Ignored,
        }
    }
}

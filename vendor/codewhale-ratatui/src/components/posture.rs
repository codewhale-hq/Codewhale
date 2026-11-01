//! The two rows beneath Codewhale's composer, extracted from the current
//! TUI's `phase_strip.rs::TidelineFooter` and `infoline.rs::InfoLine`.
//!
//! The posture bar says permission, mode and current activity. The metrics
//! line says model, context and measured numbers. Both take caller facts;
//! neither discovers routes, counts work, starts a clock or dispatches keys.

use std::borrow::Cow;

use ratatui::{
    buffer::Buffer,
    layout::{Margin, Rect},
    style::{Modifier, Style},
};
use unicode_segmentation::UnicodeSegmentation;

use crate::{Paint, Role, Theme, TuiInk, glyphs, text};

const ITEM_JOIN: &str = "   ";
const COUNT_JOIN: &str = ", ";
const HELP_GAP: usize = 2;
const SHED_TURN_CLOCK: u8 = 1;
const SHED_SESSION_CLOCK: u8 = 2;
const SHED_PERMISSION_KEY: u8 = 3;
const SHED_HINT: u8 = 4;
const SHED_COUNTS: u8 = 5;
const SHED_CAP_WARNING: u8 = 6;
const SHED_MODE_KEY: u8 = 7;
const SHED_MODE: u8 = 8;
const MAX_SHED: u8 = SHED_MODE;
const COMPACT_SHED: u8 = SHED_COUNTS;

/// One caller-authored posture word and its semantic ink.
#[derive(Clone, Debug)]
pub struct PostureFact<'a> {
    pub text: Cow<'a, str>,
    pub role: Role,
    pub ink: Option<TuiInk>,
}

impl<'a> PostureFact<'a> {
    #[must_use]
    pub fn new(value: impl Into<Cow<'a, str>>, role: Role) -> Self {
        Self {
            text: value.into(),
            role,
            ink: None,
        }
    }
    /// Choose an exact native permission, mode or status ink.
    #[must_use]
    pub fn ink(mut self, ink: TuiInk) -> Self {
        self.ink = Some(ink);
        self
    }
}

/// Codewhale's one-row posture bar, immediately below the composer.
///
/// Permission is the floor. Optional clocks, keys, hints and counts shed
/// before mode; the right notice never covers permission. At 80% context,
/// the native `surface soon — /compact` warning replaces the ordinary hint.
#[derive(Clone, Debug)]
pub struct PostureBar<'a> {
    pub permission: PostureFact<'a>,
    pub permission_key: Option<Cow<'a, str>>,
    pub mode: Option<PostureFact<'a>>,
    pub mode_key: Option<Cow<'a, str>>,
    pub turn_clock: Option<PostureFact<'a>>,
    pub counts: Vec<PostureFact<'a>>,
    pub session_clock: Option<PostureFact<'a>>,
    pub hint: Option<PostureFact<'a>>,
    pub context_percent: u8,
    pub cap_warning: Cow<'a, str>,
    pub right: Option<PostureFact<'a>>,
    pub compact: bool,
}

impl<'a> PostureBar<'a> {
    #[must_use]
    pub fn new(permission: impl Into<Cow<'a, str>>) -> Self {
        Self {
            permission: PostureFact::new(permission, Role::Attention).ink(TuiInk::PermissionAsk),
            permission_key: None,
            mode: None,
            mode_key: None,
            turn_clock: None,
            counts: Vec::new(),
            session_clock: None,
            hint: None,
            context_percent: 0,
            cap_warning: Cow::Borrowed("surface soon — /compact"),
            right: None,
            compact: false,
        }
    }

    #[must_use]
    pub fn permission_role(mut self, role: Role) -> Self {
        self.permission.role = role;
        self.permission.ink = None;
        self
    }

    #[must_use]
    pub fn permission_ink(mut self, ink: TuiInk) -> Self {
        self.permission.ink = Some(ink);
        self
    }

    #[must_use]
    pub fn mode_ink(mut self, value: impl Into<Cow<'a, str>>, ink: TuiInk) -> Self {
        self.mode = Some(PostureFact::new(value, ink.fallback_role()).ink(ink));
        self
    }

    #[must_use]
    pub fn permission_key(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.permission_key = Some(value.into());
        self
    }

    #[must_use]
    pub fn mode(mut self, value: impl Into<Cow<'a, str>>, role: Role) -> Self {
        self.mode = Some(PostureFact::new(value, role));
        self
    }

    #[must_use]
    pub fn mode_key(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.mode_key = Some(value.into());
        self
    }

    #[must_use]
    pub fn turn_clock(mut self, value: impl Into<Cow<'a, str>>, role: Role) -> Self {
        self.turn_clock = Some(PostureFact::new(value, role));
        self
    }

    #[must_use]
    pub fn session_clock(mut self, value: impl Into<Cow<'a, str>>, role: Role) -> Self {
        self.session_clock = Some(PostureFact::new(value, role));
        self
    }

    #[must_use]
    pub fn counts(mut self, values: Vec<PostureFact<'a>>) -> Self {
        self.counts = values;
        self
    }

    #[must_use]
    pub fn hint(mut self, value: impl Into<Cow<'a, str>>, role: Role) -> Self {
        self.hint = Some(PostureFact::new(value, role));
        self
    }

    #[must_use]
    pub fn right(mut self, value: impl Into<Cow<'a, str>>, role: Role) -> Self {
        self.right = Some(PostureFact::new(value, role));
        self
    }

    #[must_use]
    pub fn context_percent(mut self, percent: u8) -> Self {
        self.context_percent = percent.min(100);
        self
    }

    /// Localize the native cap warning without changing its priority.
    #[must_use]
    pub fn cap_warning(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.cap_warning = value.into();
        self
    }

    #[must_use]
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    fn at_context_cap(&self) -> bool {
        self.context_percent >= 80
    }

    fn items(&self, shed: u8, ascii: bool) -> Vec<PostureItem> {
        let mut items = vec![PostureItem {
            text: format!(
                "{} {}",
                project(glyphs::CURRENT, ascii),
                project(&self.permission.text, ascii)
            ),
            key: self
                .permission_key
                .as_deref()
                .filter(|_| shed < SHED_PERMISSION_KEY)
                .map(|key| format!("  {}", project(key, ascii))),
            role: self.permission.role,
            ink: self.permission.ink,
            bold: true,
            joined: false,
            count_index: None,
        }];
        if let Some(mode) = self.mode.as_ref().filter(|_| shed < SHED_MODE) {
            items.push(PostureItem {
                text: project(&mode.text, ascii),
                key: self
                    .mode_key
                    .as_deref()
                    .filter(|_| shed < SHED_MODE_KEY)
                    .map(|key| format!(" ({})", project(key, ascii))),
                role: mode.role,
                ink: mode.ink,
                bold: false,
                joined: false,
                count_index: None,
            });
        }
        // Preserve the native turn-only clock rung: removing the session
        // half must not make the remaining clock shed a rung too early.
        let turn_shed = if self.session_clock.is_none() {
            SHED_SESSION_CLOCK
        } else {
            SHED_TURN_CLOCK
        };
        if let Some(clock) = self.turn_clock.as_ref().filter(|_| shed < turn_shed) {
            items.push(PostureItem::fact(clock, ascii));
        }
        if shed < SHED_COUNTS {
            for (index, count) in self.counts.iter().enumerate() {
                let count_text = project(&count.text, ascii);
                let (value, key) = match count_text.strip_suffix(" (Ctrl+])") {
                    Some(label) => (label.to_owned(), Some(" (Ctrl+])".to_owned())),
                    None => (count_text, None),
                };
                items.push(PostureItem {
                    text: value,
                    key,
                    role: count.role,
                    ink: count.ink,
                    bold: false,
                    joined: index > 0,
                    count_index: Some(index),
                });
            }
        }
        if let Some(clock) = self
            .session_clock
            .as_ref()
            .filter(|_| shed < SHED_SESSION_CLOCK)
        {
            items.push(PostureItem::fact(clock, ascii));
        }
        let hint_rung = if self.at_context_cap() {
            SHED_CAP_WARNING
        } else {
            SHED_HINT
        };
        if shed < hint_rung {
            if self.at_context_cap() {
                items.push(PostureItem::fact(
                    &PostureFact::new(format!("▲ {}", self.cap_warning), Role::Attention)
                        .ink(TuiInk::Warning),
                    ascii,
                ));
            } else if let Some(hint) = &self.hint {
                items.push(PostureItem::fact(hint, ascii));
            }
        }
        items
    }

    fn layout(&self, area: Rect, theme: &Theme) -> Option<PostureLayout> {
        if area.width < 8 || area.height == 0 {
            return None;
        }
        let area = area.inner(Margin::new(1, 0));
        let ascii = theme.ascii();
        let width = usize::from(area.width);
        let floor = left_width(&self.items(MAX_SHED, ascii));
        let right = self.right.as_ref().map(|fact| {
            (
                clip_native(&project(&fact.text, ascii), width.saturating_sub(floor + 1)),
                fact.role,
            )
        });
        let right_width = right
            .as_ref()
            .map_or(0, |(value, _)| text::width(value) + 1);
        let budget = width.saturating_sub(right_width);
        let first = if self.compact { COMPACT_SHED } else { 0 };
        let items = (first..=MAX_SHED)
            .map(|shed| self.items(shed, ascii))
            .find(|items| left_width(items) <= budget)
            .unwrap_or_else(|| self.items(MAX_SHED, ascii));
        Some(PostureLayout {
            area,
            budget,
            items,
            right,
        })
    }

    /// Geometry of the live-count affordances in the painted row.
    /// Pass the same area used for painting, intersected with the buffer.
    #[must_use]
    pub fn count_hitboxes(&self, area: Rect, theme: &Theme) -> Vec<(usize, Rect)> {
        let Some(layout) = self.layout(area, theme) else {
            return Vec::new();
        };
        layout.count_hitboxes(theme.ascii())
    }
}

struct PostureItem {
    text: String,
    key: Option<String>,
    role: Role,
    ink: Option<TuiInk>,
    bold: bool,
    joined: bool,
    count_index: Option<usize>,
}

impl PostureItem {
    fn fact(fact: &PostureFact<'_>, ascii: bool) -> Self {
        Self {
            text: project(&fact.text, ascii),
            key: None,
            role: fact.role,
            ink: fact.ink,
            bold: false,
            joined: false,
            count_index: None,
        }
    }

    fn width(&self) -> usize {
        text::width(&self.text) + self.key.as_deref().map_or(0, text::width)
    }

    fn separator(&self) -> &'static str {
        if self.joined { COUNT_JOIN } else { ITEM_JOIN }
    }
}

fn left_width(items: &[PostureItem]) -> usize {
    items.iter().map(PostureItem::width).sum::<usize>()
        + items
            .iter()
            .skip(1)
            .map(|item| text::width(item.separator()))
            .sum::<usize>()
}

struct PostureLayout {
    area: Rect,
    budget: usize,
    items: Vec<PostureItem>,
    right: Option<(String, Role)>,
}

impl PostureLayout {
    fn clip(&self, x: usize, value: &str) -> String {
        clip_native(
            value,
            (usize::from(self.area.x) + self.budget).saturating_sub(x),
        )
    }

    fn count_hitboxes(&self, ascii: bool) -> Vec<(usize, Rect)> {
        let mut out = Vec::new();
        let mut x = usize::from(self.area.x);
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                x += text::width(&project(item.separator(), ascii));
            }
            let value = self.clip(x, &item.text);
            let key = item
                .key
                .as_deref()
                .map(|key| self.clip(x + text::width(&item.text), key))
                .unwrap_or_default();
            if let Some(index) = item.count_index
                && !value.is_empty()
            {
                out.push((
                    index,
                    Rect::new(
                        x as u16,
                        self.area.y,
                        (text::width(&value) + text::width(&key)) as u16,
                        1,
                    ),
                ));
            }
            x += item.width();
        }
        out
    }
}

impl Paint for PostureBar<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let Some(layout) = self.layout(area.intersection(buf.area), theme) else {
            return;
        };
        let mut x = usize::from(layout.area.x);
        for (index, item) in layout.items.iter().enumerate() {
            if index > 0 {
                let separator = project(item.separator(), theme.ascii());
                put(
                    layout.area,
                    buf,
                    x,
                    &layout.clip(x, &separator),
                    theme.fg(Role::Dim),
                );
                x += text::width(&separator);
            }
            let base = item
                .ink
                .map_or_else(|| theme.fg(item.role), |ink| theme.tui_ink(ink));
            let style = if item.bold {
                base.add_modifier(Modifier::BOLD)
            } else {
                base
            };
            put(layout.area, buf, x, &layout.clip(x, &item.text), style);
            if let Some(key) = &item.key {
                let key_x = x + text::width(&item.text);
                put(
                    layout.area,
                    buf,
                    key_x,
                    &layout.clip(key_x, key),
                    theme.fg(Role::Hint),
                );
            }
            x += item.width();
        }
        if let Some((value, role)) = &layout.right
            && !value.is_empty()
        {
            let x = usize::from(layout.area.right()).saturating_sub(text::width(value));
            put(layout.area, buf, x, value, theme.fg(*role));
        }
    }

    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        u16::from(width > 0)
    }
}

/// Identity and native shedding priority of a metrics-line segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MetricKind {
    Model,
    Context,
    Cost,
    BillingTier,
    OutputTokens,
    Ttft,
    Rate,
    Cache,
    Balance,
    Goal,
    Workspace,
    GitBranch,
    Update,
}

impl MetricKind {
    const SHED_BEFORE_HELP: u8 = 7;

    /// Higher priorities shed first; model and context never shed.
    #[must_use]
    pub const fn shed_priority(self) -> u8 {
        match self {
            Self::Cache => 6,
            Self::Rate | Self::Ttft | Self::Cost => 5,
            Self::OutputTokens | Self::BillingTier => 7,
            Self::Balance | Self::Workspace | Self::GitBranch => 4,
            Self::Goal | Self::Update => 3,
            Self::Model | Self::Context => 0,
        }
    }
}

/// A reported number or route name. Empty labels paint just the value.
#[derive(Clone, Debug)]
pub struct MetricSegment<'a> {
    pub kind: MetricKind,
    pub label: Cow<'a, str>,
    pub value: Cow<'a, str>,
    pub role: Role,
}

impl<'a> MetricSegment<'a> {
    #[must_use]
    pub fn new(
        kind: MetricKind,
        label: impl Into<Cow<'a, str>>,
        value: impl Into<Cow<'a, str>>,
    ) -> Self {
        Self {
            kind,
            label: label.into(),
            value: value.into(),
            role: Role::Muted,
        }
    }

    #[must_use]
    pub fn role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }
}

/// A segment's exact visible cells, including the space between label and value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetricHitbox {
    pub kind: MetricKind,
    pub area: Rect,
}

/// Codewhale's one-row model, context and session metrics line.
///
/// Missing measurements are omitted by leaving out their segments. The
/// native shed ladder preserves model and context, removes secondary counts
/// before help, and clips at the right edge only below the identity floor.
#[derive(Clone, Debug)]
pub struct MetricsLine<'a> {
    pub segments: Vec<MetricSegment<'a>>,
    pub help_hint: Cow<'a, str>,
    pub hovered: Option<MetricKind>,
    pub compact: bool,
}

impl<'a> MetricsLine<'a> {
    #[must_use]
    pub fn new(segments: Vec<MetricSegment<'a>>) -> Self {
        Self {
            segments,
            help_hint: Cow::Borrowed(""),
            hovered: None,
            compact: false,
        }
    }

    #[must_use]
    pub fn help_hint(mut self, value: impl Into<Cow<'a, str>>) -> Self {
        self.help_hint = value.into();
        self
    }

    #[must_use]
    pub fn hovered(mut self, kind: MetricKind) -> Self {
        self.hovered = Some(kind);
        self
    }

    #[must_use]
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    fn layout(&self, area: Rect, theme: &Theme) -> MetricLayout {
        let ascii = theme.ascii();
        let help = project(&self.help_hint, ascii);
        let mut kept: Vec<PreparedMetric> = self
            .segments
            .iter()
            .filter(|segment| {
                !self.compact || segment.kind.shed_priority() < MetricKind::SHED_BEFORE_HELP
            })
            .map(|segment| PreparedMetric {
                kind: segment.kind,
                label: project(&segment.label, ascii),
                value: project(&segment.value, ascii),
                role: segment.role,
            })
            .collect();
        let left_width = |segments: &[PreparedMetric]| -> usize {
            segments.iter().map(PreparedMetric::width).sum::<usize>()
                + text::width(ITEM_JOIN) * segments.len().saturating_sub(1)
        };
        let needed = |left: usize, show_help: bool| -> usize {
            left + if show_help && !help.is_empty() {
                HELP_GAP + text::width(&help)
            } else {
                0
            }
        };
        let sheddable = |segments: &[PreparedMetric], min_priority: u8| -> Option<usize> {
            segments
                .iter()
                .enumerate()
                .filter(|(_, segment)| segment.kind.shed_priority() >= min_priority.max(1))
                .max_by_key(|(_, segment)| segment.kind.shed_priority())
                .map(|(index, _)| index)
        };
        let mut show_help = !help.is_empty() && !self.compact;
        while needed(left_width(&kept), show_help) > usize::from(area.width) {
            if let Some(index) = sheddable(&kept, MetricKind::SHED_BEFORE_HELP) {
                kept.remove(index);
            } else if show_help {
                show_help = false;
            } else if let Some(index) = sheddable(&kept, 1) {
                kept.remove(index);
            } else {
                break;
            }
        }
        MetricLayout {
            kept,
            help: show_help.then_some(help),
        }
    }

    /// Geometry for all surviving readings. Model, context and update acquire
    /// hover styling; hosts choose which readings dispatch an action.
    /// Use the same clipped area as painting when the buffer is smaller.
    #[must_use]
    pub fn hitboxes(&self, area: Rect, theme: &Theme) -> Vec<MetricHitbox> {
        if area.is_empty() {
            return Vec::new();
        }
        let layout = self.layout(area, theme);
        let mut x = usize::from(area.x);
        let mut out = Vec::new();
        let right = usize::from(area.right());
        for (index, segment) in layout.kept.iter().enumerate() {
            if index > 0 {
                x += text::width(ITEM_JOIN);
            }
            let end = (x + segment.width()).min(right);
            if x < end {
                out.push(MetricHitbox {
                    kind: segment.kind,
                    area: Rect::new(x as u16, area.y, (end - x) as u16, 1),
                });
            }
            x += segment.width();
        }
        out
    }

    #[must_use]
    pub fn context_hitbox(&self, area: Rect, theme: &Theme) -> Option<Rect> {
        self.hitboxes(area, theme)
            .into_iter()
            .find(|hit| hit.kind == MetricKind::Context)
            .map(|hit| hit.area)
    }
}

struct PreparedMetric {
    kind: MetricKind,
    label: String,
    value: String,
    role: Role,
}

impl PreparedMetric {
    fn width(&self) -> usize {
        text::width(&self.value)
            + if self.label.is_empty() {
                0
            } else {
                text::width(&self.label) + 1
            }
    }
}

struct MetricLayout {
    kept: Vec<PreparedMetric>,
    help: Option<String>,
}

impl Paint for MetricsLine<'_> {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buf.area);
        if area.is_empty() {
            return;
        }
        let layout = self.layout(area, theme);
        let mut x = usize::from(area.x);
        for (index, segment) in layout.kept.iter().enumerate() {
            if index > 0 {
                put(area, buf, x, ITEM_JOIN, theme.fg(Role::Dim));
                x += text::width(ITEM_JOIN);
            }
            let hovered = matches!(
                segment.kind,
                MetricKind::Model | MetricKind::Context | MetricKind::Update
            ) && self.hovered == Some(segment.kind);
            let style = if hovered {
                theme
                    .fg(segment.role)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                theme.fg(segment.role)
            };
            if !segment.label.is_empty() {
                let label_role = if matches!(segment.role, Role::Danger | Role::Attention) {
                    segment.role
                } else {
                    Role::Muted
                };
                put(area, buf, x, &segment.label, theme.fg(label_role));
                x += text::width(&segment.label) + 1;
            }
            put(area, buf, x, &segment.value, style);
            x += text::width(&segment.value);
        }
        if let Some(help) = layout.help {
            let x = usize::from(area.right()).saturating_sub(text::width(&help));
            put(area, buf, x, &help, theme.fg(Role::Hint));
        }
    }

    fn height(&self, width: u16, _theme: &Theme) -> u16 {
        u16::from(width > 0)
    }
}

// Native punctuation projection, with the kit's existing text sanitation.
// Authored glyphs change in ASCII mode; language text stays intact.
fn project(value: &str, ascii: bool) -> String {
    let safe = text::display_safe(value);
    if !ascii {
        return safe.into_owned();
    }
    if let Some(fallback) = glyphs::ascii_fallback(&safe) {
        return fallback.to_owned();
    }
    safe.chars()
        .map(|ch| {
            let mut bytes = [0; 4];
            glyphs::ascii_fallback(ch.encode_utf8(&mut bytes))
                .map(str::to_owned)
                .unwrap_or_else(|| ch.to_string())
        })
        .collect()
}

// phase_strip uses ui_text::truncate_line_to_width, whose ellipsis is three
// ASCII cells even on Unicode terminals and whose tiny budgets have no dot.
fn clip_native(value: &str, max: usize) -> String {
    if text::width(value) <= max {
        return value.to_owned();
    }
    let limit = max.saturating_sub(if max > 3 { 3 } else { 0 });
    let mut out = String::new();
    let mut used = 0;
    for grapheme in value.graphemes(true) {
        let width = text::width(grapheme);
        if used + width > limit {
            break;
        }
        out.push_str(grapheme);
        used += width;
    }
    if max > 3 {
        out.push_str("...");
    }
    out
}

fn put(area: Rect, buf: &mut Buffer, x: usize, value: &str, style: Style) {
    let right = usize::from(area.right());
    if x >= usize::from(area.x) && x < right && !value.is_empty() {
        buf.set_stringn(x as u16, area.y, value, right - x, style);
    }
}

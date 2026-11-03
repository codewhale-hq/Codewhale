//! The shell's metrics line — one row of session numbers, painted under the
//! posture bar at the bottom of the screen.
//!
//! It used to be a top bar. The founder's call (SHELL-DESIGN-20260901 §2.0):
//! *"Putting the info at the bottom is a better idea, because then you scroll
//! up and it feels intentional. Move the top/side bar to the bottom."* Then
//! (2026-09-02): *less always-on information* — the repository and branch
//! moved to the launch header and the git bottom view, and the DeepSeek
//! harness session metrics came back on screen in their place.
//!
//! The row, left to right, separated by three spaces:
//!
//! ```text
//! deepseek-v4   ctx 22%   $0.14   ttft 400ms   38 tok/s   ↓ 1.2K      Ctrl+/ help
//! ```
//!
//! The model is the one route fact the user checks before a turn, and it
//! stays clickable to the picker; the context reading stays clickable to the
//! inspector. Both are the floor and never shed. Everything else is a
//! metric: the session cost (the same number `/cost`, the roster and the
//! price widget print), time to first token, output rate and output tokens —
//! measured latency/rate averages persist between receipts; the output count
//! updates during streaming. Missing measurements remain absent.
//!
//! The context reading is painted here and only here — the posture bar above
//! used to print the same percentage a second time from the same snapshot —
//! and at every fullness, not only from 50% up (#5950).
//!
//! Shed order as width drops: cache, output count and billing tier, the help
//! hint, then rate and TTFT, then cost and balance, followed by goal and update
//! ([`InfoSegmentId::shed_priority`]). The model and `ctx NN%` never shed; below that floor the row clips at its
//! right edge.
//!
//! `/statusline` and `tui.status_items` select the session metrics;
//! [`crate::tui::ui::frame::info_segments`] also includes an active goal or
//! an available-update notice. Shedding decides what survives the
//! width that is left. `tui.metrics_line` sizes the row (#5950): `hidden`
//! gives the line back to the transcript, and `compact` starts the shed
//! pass with secondary counts and the help hint already gone; TTFT and rate
//! remain when selected and space allows
//! ([`InfoLine::compact`]).
//!
//! Interaction: segment geometry is recorded for parity tests, but only the
//! model/route segment, context reading and available-update notice advertise
//! actions in the live shell. Status-only facts do not brighten on hover or
//! pretend to be controls.
//!
//! Color: semantic ink only ([`ChromeInk`]); no hex, per the status-bar color
//! grammar. ASCII-safe mode substitutes every glyph through
//! the shared kit. Engine facts and action dispatch remain with the callers.

use codewhale_palette::{ChromeInk, UiTheme};
use codewhale_ratatui::{
    Caps, MetricSegment, MetricsLine, Paint, Role, Theme, color::ColorDepth, detect::Appearance,
};
use ratatui::{buffer::Buffer, layout::Rect, style::Color, widgets::Widget};

/// The kit owns segment identities and their one shared shed priority.
/// Existing callers retain the same model/context action identities.
pub use codewhale_ratatui::MetricKind as InfoSegmentId;

/// One metrics-line segment.
#[derive(Debug, Clone)]
pub struct InfoSegment {
    pub id: InfoSegmentId,
    pub label: String,
    pub value: String,
    pub ink: ChromeInk,
}

impl InfoSegment {
    #[must_use]
    pub fn new(id: InfoSegmentId, label: &str, value: impl Into<String>, ink: ChromeInk) -> Self {
        Self {
            id,
            label: label.to_string(),
            value: value.into(),
            ink,
        }
    }
}

/// What the caller owes the metrics line. Everything is injected so renders
/// are deterministic (golden buffers) and wall-clock keyed by the owner,
/// never frame-count keyed (spec §5e).
pub struct InfoLine<'a> {
    pub theme: &'a UiTheme,
    /// The single right-hand key hint, e.g. `Ctrl+/ help`. Empty means the
    /// caller has no hint to advertise.
    pub help_hint: &'a str,
    /// Segments in display order.
    pub segments: &'a [InfoSegment],
    /// Actionable segment under the mouse. [`InfoSegmentId::Model`] and
    /// [`InfoSegmentId::Context`] and [`InfoSegmentId::Update`] advertise
    /// hover feedback in the live shell; each owns a click action.
    pub hovered: Option<InfoSegmentId>,
    /// ASCII-safe / NO_COLOR mode: every glyph goes through
    /// the kit's native punctuation projection; language text stays intact.
    pub ascii_safe: bool,
    /// `tui.metrics_line = "compact"` (#5950): the shed pass starts with
    /// secondary counts and the help hint already gone.
    /// Selected TTFT and rate readings remain; width sheds the rest.
    pub compact: bool,
}

impl<'a> InfoLine<'a> {
    #[must_use]
    pub fn new(theme: &'a UiTheme, help_hint: &'a str, segments: &'a [InfoSegment]) -> Self {
        Self {
            theme,
            help_hint,
            segments,
            hovered: None,
            ascii_safe: false,
            compact: false,
        }
    }

    #[must_use]
    pub fn ascii_safe(mut self, ascii_safe: bool) -> Self {
        self.ascii_safe = ascii_safe;
        self
    }

    #[must_use]
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    #[must_use]
    pub fn hovered(mut self, hovered: Option<InfoSegmentId>) -> Self {
        self.hovered = hovered;
        self
    }
}

// A full-color role theme supplies distinct identities, not product colors.
// Named native palettes can collapse Hint/Dim or permission inks; use the
// uncollapsed role table so custom UiTheme slots remain independent. The
// existing backend applies terminal color capabilities after this adapter.
pub(super) fn source_theme(ascii: bool) -> Theme {
    Theme::new(Caps {
        depth: ColorDepth::TrueColor,
        ascii,
        appearance: Appearance::Dark,
    })
}

pub(super) fn ink_role(ink: ChromeInk) -> Role {
    match ink {
        ChromeInk::Outcome | ChromeInk::Active => Role::Live,
        // These three roles are private style identities in this adapter;
        // they do not paint grounds or change permission authority.
        ChromeInk::PermissionAsk => Role::Surface,
        ChromeInk::PermissionAutoReview => Role::Hover,
        ChromeInk::PermissionFullAccess => Role::Selected,
        ChromeInk::Waiting => Role::BorderStrong,
        ChromeInk::Attention => Role::Attention,
        ChromeInk::PolicyAct
        | ChromeInk::PolicyPlan
        | ChromeInk::PolicyOperate
        | ChromeInk::Identity
        | ChromeInk::Info => Role::Accent,
        ChromeInk::MetadataValue => Role::Foreground,
        ChromeInk::Metadata => Role::Muted,
        ChromeInk::MetadataHint => Role::Hint,
        ChromeInk::MetadataDim => Role::Dim,
        ChromeInk::Failure => Role::Danger,
    }
}

const STYLE_INKS: [ChromeInk; 12] = [
    ChromeInk::Active,
    ChromeInk::PermissionAsk,
    ChromeInk::PermissionAutoReview,
    ChromeInk::PermissionFullAccess,
    ChromeInk::Waiting,
    ChromeInk::Attention,
    ChromeInk::Info,
    ChromeInk::MetadataValue,
    ChromeInk::Metadata,
    ChromeInk::MetadataHint,
    ChromeInk::MetadataDim,
    ChromeInk::Failure,
];

impl InfoLine<'_> {
    fn kit(&self) -> MetricsLine<'_> {
        let mut line = MetricsLine::new(
            self.segments
                .iter()
                .map(|segment| {
                    MetricSegment::new(segment.id, segment.label.as_str(), segment.value.as_str())
                        .role(ink_role(segment.ink))
                })
                .collect(),
        )
        .help_hint(self.help_hint)
        .compact(self.compact);
        if let Some(hovered) = self.hovered {
            line = line.hovered(hovered);
        }
        line
    }
}

/// The kit computes the visible context reading's exact pointer target.
#[must_use]
pub fn context_meter_hitbox(info: &InfoLine<'_>, area: Rect) -> Option<Rect> {
    info.kit()
        .context_hitbox(area, &source_theme(info.ascii_safe))
}

impl Widget for InfoLine<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        paint_native_row(&self.kit(), area, buf, self.theme, self.ascii_safe);
    }
}

/// Adapt the kit's two native chrome rows to the live host ink slots.
pub(super) fn paint_native_row(
    component: &impl Paint,
    area: Rect,
    buf: &mut Buffer,
    theme: &UiTheme,
    ascii: bool,
) {
    let area = area.intersection(buf.area);
    if area.is_empty() {
        return;
    }
    let area = Rect { height: 1, ..area };
    let source = source_theme(ascii);
    let inks = STYLE_INKS.map(|ink| (source.color(ink_role(ink)), ink.color(theme)));
    // Render one borrowed row with a foreground sentinel. Only cells the
    // kit writes are copied back, preserving untouched content and all
    // host-owned backgrounds/modifiers. Wide-character continuation cells
    // reset exactly as they do in a direct Ratatui render.
    let untouched = Color::Indexed(0);
    let mut row = Buffer::empty(area);
    for x in area.left()..area.right() {
        row[(x, area.y)].clone_from(&buf[(x, area.y)]);
        row[(x, area.y)].fg = untouched;
    }
    component.paint(area, &mut row, &source);
    for x in area.left()..area.right() {
        let cell = &mut row[(x, area.y)];
        if cell.fg == untouched {
            continue;
        }
        if let Some((_, color)) = inks.iter().find(|(identity, _)| *identity == Some(cell.fg)) {
            cell.fg = *color;
        }
        buf[(x, area.y)].clone_from(cell);
    }
}

/// Host-owned action routing keeps its existing typed facade.
#[derive(Debug, Clone)]
pub struct InfoLineHitbox {
    pub id: InfoSegmentId,
    pub area: Rect,
}

/// Geometry comes from the same kit layout as painting, with no caller
/// width calculation, shedding pass or punctuation projection.
#[must_use]
pub fn infoline_hitboxes(info: &InfoLine<'_>, area: Rect) -> Vec<InfoLineHitbox> {
    info.kit()
        .hitboxes(area, &source_theme(info.ascii_safe))
        .into_iter()
        .map(|hit| InfoLineHitbox {
            id: hit.kind,
            area: hit.area,
        })
        .collect()
}

#[cfg(test)]
mod tests;

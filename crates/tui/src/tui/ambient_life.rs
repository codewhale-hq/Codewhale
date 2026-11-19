//! Ambient ocean life for the underwater transcript field.

use codewhale_ratatui::Paint as _;
use ratatui::{buffer::Buffer, layout::Rect, style::Color, text::Line};
use unicode_width::UnicodeWidthChar;

use crate::tui::ocean::{self, OceanColumn};

#[path = "ambient_life/pet_cameo.rs"]
mod pet_cameo;
#[path = "ambient_life/pet_sim.rs"]
pub mod pet_sim;
#[path = "ambient_life/pet_widget.rs"]
pub mod pet_widget;

pub const AMBIENT_MIN_WIDTH: u16 = crate::tui::ocean::AMBIENT_MIN_WIDTH;
pub const AMBIENT_MIN_HEIGHT: u16 = crate::tui::ocean::AMBIENT_MIN_HEIGHT;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AmbientFrameStats {
    pub marks_built: u32,
    pub marks_painted: u32,
    pub marks_skipped_text: u32,
    pub marks_clipped: u32,
    pub cells_written: u32,
}

/// Optional whale cameo trigger (e.g. successful turn completion).
#[derive(Debug, Clone, Copy, Default)]
pub struct WhaleCameo {
    pub elapsed_ms: Option<u128>,
    /// Anchor column within the field (composer / center).
    pub anchor_x: u16,
    pub anchor_y: u16,
}

/// How the ambient scene is shaped by live agent activity. The underwater
/// used to be phase-agnostic: same fish, same pace, whether the agent was
/// thinking, running tools, or orchestrating sub-agents. Each treatment is a
/// bounded parameter shift — never a second scene graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AmbientActivity {
    #[default]
    Baseline,
    Reasoning,
    /// Read-shaped exploration: quieter than generic tool work, brighter
    /// than hidden reasoning — skimming, not digging.
    Reading,
    Tools,
    Subagents,
    Verifying,
}

impl AmbientActivity {
    #[must_use]
    pub fn from_kind(kind: crate::tui::underwater::LiveActivityKind) -> Self {
        match kind {
            crate::tui::underwater::LiveActivityKind::Reasoning => Self::Reasoning,
            crate::tui::underwater::LiveActivityKind::Reading => Self::Reading,
            crate::tui::underwater::LiveActivityKind::UsingTool => Self::Tools,
            crate::tui::underwater::LiveActivityKind::UsingSubagents => Self::Subagents,
            crate::tui::underwater::LiveActivityKind::Verifying => Self::Verifying,
            _ => Self::Baseline,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn render_ambient_life(
    area: Rect,
    buf: &mut Buffer,
    theme: &codewhale_ratatui::Theme,
    ink: Color,
    lines: &[Line<'static>],
    elapsed_ms: u128,
    presence: f32,
    whale: WhaleCameo,
    activity: AmbientActivity,
    protected: &[Rect],
) -> AmbientFrameStats {
    let mut stats = AmbientFrameStats::default();
    if area.width < AMBIENT_MIN_WIDTH
        || area.height < AMBIENT_MIN_HEIGHT
        || !presence.is_finite()
        || presence <= 0.0
    {
        return stats;
    }
    let elapsed = std::time::Duration::from_millis(u64::try_from(elapsed_ms).unwrap_or(u64::MAX));
    codewhale_ratatui::Habitat::new(elapsed, codewhale_ratatui::MotionMode::Full)
        .ink(Some(ink))
        .presence(presence)
        .protected(protected.to_vec())
        .paint(area, buf, theme);
    pet_cameo::paint(area, buf, ink, lines, presence, whale, activity, &mut stats);
    stats
}

const TEXT_CLEARANCE_ROWS: u16 = 1;

const TEXT_CLEARANCE_COLS: usize = 2;

#[must_use]
fn is_open_water(lines: &[Line<'_>], x: u16, y: u16, width: u16) -> bool {
    let first = usize::from(y.saturating_sub(TEXT_CLEARANCE_ROWS));
    let last = usize::from(y.saturating_add(TEXT_CLEARANCE_ROWS));
    !(first..=last).any(|row| {
        lines
            .get(row)
            .and_then(occupied_text_bounds)
            .is_some_and(|(start, end)| span_touches_text(x, width, start, end))
    })
}

#[must_use]
fn span_touches_text(x: u16, width: u16, start: usize, end: usize) -> bool {
    usize::from(x) < end.saturating_add(TEXT_CLEARANCE_COLS)
        && usize::from(x).saturating_add(usize::from(width))
            > start.saturating_sub(TEXT_CLEARANCE_COLS)
}

/// Width-only occupied-text measurement (no per-line String allocation).
#[must_use]
pub fn occupied_text_bounds(line: &Line<'_>) -> Option<(usize, usize)> {
    if line.spans.is_empty() {
        return None;
    }
    let mut total = 0usize;
    let mut leading = 0usize;
    let mut seen_non_ws = false;
    let mut trailing_run = 0usize;

    for span in &line.spans {
        for ch in span.content.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            total = total.saturating_add(w);
            if ch.is_whitespace() {
                if !seen_non_ws {
                    leading = leading.saturating_add(w);
                } else {
                    trailing_run = trailing_run.saturating_add(w);
                }
            } else {
                seen_non_ws = true;
                trailing_run = 0;
            }
        }
    }
    if !seen_non_ws {
        return None;
    }
    Some((leading, total.saturating_sub(trailing_run)))
}

/// Cached ocean row colors invalidated only when phase/dimensions/palette/breath tick.
/// Shared across widgets that paint the same [`OceanColumn`] within a frame.
#[derive(Debug, Clone, Default)]
pub struct OceanRampCache {
    colors: Vec<Color>,
    height: u16,
    top: u16,
    elapsed_bucket: u128,
    phase_tag: u8,
    ramp_fingerprint: u64,
}

impl OceanRampCache {
    /// Return a per-row color ramp, recomputing only when inputs change.
    pub fn colors_for(
        &mut self,
        column: &OceanColumn,
        height: u16,
        top: u16,
        elapsed_ms: u128,
        phase_tag: u8,
        ramp_fingerprint: u64,
    ) -> &[Color] {
        // The breath and completion fade are continuous. Bucket at a 60 FPS
        // floor so Ghostty's smooth-motion lane is not quantized back to the
        // old 80 ms atmosphere cadence; slower terminals still call this only
        // when they actually draw.
        let bucket = elapsed_ms / 16;
        if self.colors.len() == usize::from(height)
            && self.height == height
            && self.top == top
            && self.elapsed_bucket == bucket
            && self.phase_tag == phase_tag
            && self.ramp_fingerprint == ramp_fingerprint
        {
            return &self.colors;
        }
        self.colors.clear();
        self.colors.reserve(usize::from(height));
        for local_y in 0..height {
            self.colors
                .push(column.color_at_y(top.saturating_add(local_y)));
        }
        self.height = height;
        self.top = top;
        self.elapsed_bucket = bucket;
        self.phase_tag = phase_tag;
        self.ramp_fingerprint = ramp_fingerprint;
        &self.colors
    }
}

thread_local! {
    static FRAME_RAMP: std::cell::RefCell<OceanRampCache> =
        const { std::cell::RefCell::new(OceanRampCache {
            colors: Vec::new(),
            height: 0,
            top: 0,
            elapsed_bucket: 0,
            phase_tag: 0,
            ramp_fingerprint: 0,
        }) };
}

#[must_use]
pub fn frame_ocean_ramp(
    column: &OceanColumn,
    height: u16,
    top: u16,
    elapsed_ms: u128,
    phase_tag: u8,
    ramp_fingerprint: u64,
) -> Vec<Color> {
    FRAME_RAMP.with(|cache| {
        cache
            .borrow_mut()
            .colors_for(column, height, top, elapsed_ms, phase_tag, ramp_fingerprint)
            .to_vec()
    })
}

#[cfg(test)]
#[path = "ambient_life/tests.rs"]
mod tests;

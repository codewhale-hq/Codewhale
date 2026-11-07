//! Terminal-native underwater field for the Codewhale transcript.
//!
//! The field is atmosphere, never content: ordinary shell cells share its
//! water column while semantic surfaces such as selections, errors, and code
//! keep their own backgrounds. It belongs to the `underwater` theme alone
//! (`ThemeId::Underwater`); every other theme leaves the terminal's ground
//! untouched. Motion inside the field remains governed separately by
//! `low_motion`/`fancy_animations`. The native kit owns the complete ramp and
//! column sampling kernel; host semantic finishing and backend capability/
//! contrast policy remain separate until their differences are reconciled.

use std::time::Duration;

use codewhale_ratatui::{
    MotionMode, OceanColumn as NativeColumn, OceanPhase, OceanRamp as NativeRamp,
};
use ratatui::{buffer::Buffer, layout::Rect, style::Color};

use crate::tui::underwater::ShellPhase;
use codewhale_palette::UiTheme;

/// Minimum empty-water size that earns decorative ambient life when the
/// underwater theme is selected. Below this, content and controls own
/// every cell. Shared by the renderer and idle animation scheduler so redraws
/// are never scheduled for invisible life.
pub const AMBIENT_MIN_WIDTH: u16 = 40;
pub const AMBIENT_MIN_HEIGHT: u16 = 10;

/// Ambient-life ink pair, independent of the Deepsea ramp and shaped by what
/// the agent is doing so the marks themselves carry the state at a glance:
/// reasoning dims toward the deep, tool work brightens like a faster current,
/// and a sub-agent pod swims in seafoam — the hue reserved for orchestration.
#[must_use]
pub fn ambient_inks_for_activity(
    theme: &UiTheme,
    activity: crate::tui::ambient_life::AmbientActivity,
) -> (Color, Color) {
    use crate::tui::ambient_life::AmbientActivity;
    let sky = match activity {
        AmbientActivity::Subagents => rgb(theme.accent_secondary).unwrap_or((79, 209, 197)),
        _ => rgb(theme.info).unwrap_or((106, 174, 242)),
    };
    // `mix(sky, base, t)`: larger `t` sits closer to the background — dimmer.
    let (toward_base_a, toward_base_b) = match activity {
        AmbientActivity::Reasoning => (0.58, 0.44),
        AmbientActivity::Reading => (0.50, 0.36),
        AmbientActivity::Tools => (0.30, 0.18),
        AmbientActivity::Subagents => (0.34, 0.22),
        AmbientActivity::Verifying | AmbientActivity::Baseline => (0.42, 0.28),
    };
    // Only the underwater theme owns a painted base column; everywhere else
    // the terminal's own ground (Color::Reset) is the base and the inks fall
    // back to the theme's info lane.
    let mix_base = rgb(theme.surface_bg)
        .or_else(|| OceanRamp::for_theme(theme).and_then(|ramp| rgb(ramp.middle)));
    match mix_base {
        Some(base) => (
            color(mix(sky, base, toward_base_a)),
            color(mix(sky, base, toward_base_b)),
        ),
        None => (theme.info, theme.info),
    }
}

/// Length of the completion breath (the column's settle flourish), ms.
pub const COMPLETION_BREATH_MS: u128 = NativeRamp::COMPLETION_BREATH.as_millis();

/// Extra ms after the breath during which ambient life eases out of view.
pub const SETTLE_MS: u128 = 600;
pub(crate) const COMPLETION_SETTLE_MS: u128 = COMPLETION_BREATH_MS + SETTLE_MS;

/// Ms over which animated life ramps in when a working phase begins.
pub const RAMP_MS: u128 = 450;

/// Smoothstep easing: 0 at t=0, 1 at t=1, zero velocity at both ends.
#[must_use]
pub fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Life presence (0..=1) as a pure function of the monotonic clocks. There is
/// deliberately NO per-frame mutable state here: the same inputs always yield
/// the same output, which keeps ambient-life renders deterministic.
///
/// Rules:
/// - A turn just ended (`completion_elapsed_ms` within the breath) holds full
///   presence so ambient life keeps swimming through the settle flourish.
/// - After the breath, presence eases out over [`SETTLE_MS`] so the water
///   settles instead of snapping from animated to frozen.
/// - Browsing history or the pristine empty state is user-driven: full
///   presence immediately.
/// - A Working/Verifying phase ramps in from `turn_elapsed_ms` over
///   [`RAMP_MS`], giving bursty fast streams a calm, bounded onset.
/// - Everything else is fully static.
#[must_use]
pub fn life_presence(
    completion_elapsed_ms: Option<u128>,
    turn_elapsed_ms: Option<u128>,
    animated: bool,
    browsing_history: bool,
    empty_state: bool,
) -> f32 {
    if let Some(elapsed) = completion_elapsed_ms {
        if elapsed < COMPLETION_BREATH_MS {
            return 1.0;
        }
        let t = (elapsed - COMPLETION_BREATH_MS) as f32 / SETTLE_MS as f32;
        return 1.0 - smoothstep(t);
    }
    if !animated {
        return 0.0;
    }
    if browsing_history || empty_state {
        return 1.0;
    }
    match turn_elapsed_ms {
        Some(elapsed) => smoothstep(elapsed as f32 / RAMP_MS as f32),
        None => 1.0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OceanRamp {
    pub surface: Color,
    pub middle: Color,
    pub deep: Color,
    pub ambient: Color,
    /// Tint for phases that are blocked on the user (Waiting / Approval).
    /// The whole water field warms toward this so "needs you" is legible
    /// from across the room, not only in the phase strip.
    pub attention: Color,
    /// Tint for the Failed outcome: a steady cast, not a pulse — it reports,
    /// it does not ask.
    pub failure: Color,
}

/// One continuous water column shared by every shell band in a frame.
///
/// Individual widgets still own their foreground and semantic surfaces, but
/// ordinary shell backgrounds sample this column with their absolute row.
/// That keeps the header, work strip, transcript, phase line, and composer
/// from each restarting the same miniature gradient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OceanColumn {
    ramp: OceanRamp,
    top: u16,
    height: u16,
    elapsed_ms: u128,
    completion_elapsed_ms: Option<u128>,
    phase: ShellPhase,
    animated: bool,
    /// Fixed-point (0..=1000) life presence; keeps `Eq` derivable.
    presence: u16,
    context_percent: u8,
    paint_caps: Option<codewhale_ratatui::Caps>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OceanRampCacheIdentity {
    ramp: OceanRamp,
    top: u16,
    height: u16,
    phase_tag: u8,
    animated: bool,
    completion_active: bool,
    presence: u16,
    context_percent: u8,
}

impl OceanRampCacheIdentity {
    fn fingerprint(self) -> u64 {
        const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;

        [
            color_cache_code(self.ramp.surface),
            color_cache_code(self.ramp.middle),
            color_cache_code(self.ramp.deep),
            color_cache_code(self.ramp.ambient),
            color_cache_code(self.ramp.attention),
            color_cache_code(self.ramp.failure),
            u32::from(self.top),
            u32::from(self.height),
            u32::from(self.phase_tag),
            u32::from(self.animated),
            u32::from(self.completion_active),
            u32::from(self.presence),
            u32::from(self.context_percent),
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .fold(OFFSET_BASIS, |state, byte| {
            (state ^ u64::from(byte)).wrapping_mul(PRIME)
        })
    }
}

fn color_cache_code(value: Color) -> u32 {
    match value {
        Color::Reset => 0,
        Color::Black => 1,
        Color::Red => 2,
        Color::Green => 3,
        Color::Yellow => 4,
        Color::Blue => 5,
        Color::Magenta => 6,
        Color::Cyan => 7,
        Color::Gray => 8,
        Color::DarkGray => 9,
        Color::LightRed => 10,
        Color::LightGreen => 11,
        Color::LightYellow => 12,
        Color::LightBlue => 13,
        Color::LightMagenta => 14,
        Color::LightCyan => 15,
        Color::White => 16,
        Color::Indexed(index) => 0x0100_0000 | u32::from(index),
        Color::Rgb(red, green, blue) => 0x0200_0000 | u32::from_be_bytes([0, red, green, blue]),
    }
}

impl OceanColumn {
    // Eight args mirroring the eight column fields; a params struct would
    // only rename the call sites without removing a single decision.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        ramp: OceanRamp,
        viewport: Rect,
        elapsed_ms: u128,
        completion_elapsed_ms: Option<u128>,
        phase: ShellPhase,
        animated: bool,
        presence: u16,
        context_percent: u8,
    ) -> Self {
        Self {
            ramp,
            top: viewport.y,
            height: viewport.height.max(1),
            elapsed_ms,
            completion_elapsed_ms,
            phase,
            animated,
            presence,
            context_percent: context_percent.min(100),
            paint_caps: None,
        }
    }

    #[must_use]
    pub fn color_at_y(self, y: u16) -> Color {
        let viewport = Rect::new(0, self.top, 0, self.height);
        self.native()
            .color_at_y_with_ramp(y, viewport, self.ramp.native())
    }

    fn native(self) -> NativeColumn {
        // The host samples and gates these monotonic clocks once. The pure
        // kit path owns the complete phase/context/presence/settle kernel;
        // ColorCompatBackend remains the current terminal paint authority.
        let motion = if self.animated || self.presence > 0 {
            MotionMode::Full
        } else {
            MotionMode::Still
        };
        let mut column = NativeColumn::new(phase_duration(self.elapsed_ms), motion)
            .phase(native_phase(self.phase))
            .context_percent(self.context_percent)
            .presence(self.presence);
        if let Some(elapsed) = self.completion_elapsed_ms {
            column = column.completion_elapsed(completion_duration(elapsed));
        }
        column
    }

    fn completion_active(self) -> bool {
        self.phase == ShellPhase::Done
            && (self.animated || self.presence > 0)
            && self
                .completion_elapsed_ms
                .is_some_and(|elapsed| elapsed < COMPLETION_BREATH_MS)
    }

    /// Elapsed milliseconds of the completion breath, when active. Ambient
    /// life uses this to time the rare whale cameo on successful turns.
    #[must_use]
    pub fn completion_elapsed_ms(self) -> Option<u128> {
        self.completion_elapsed_ms
    }

    /// Compact phase discriminator for [`crate::tui::ambient_life::OceanRampCache`].
    #[must_use]
    pub fn phase_tag(self) -> u8 {
        match self.phase {
            ShellPhase::Idle => 0,
            ShellPhase::Typing => 1,
            ShellPhase::Working => 2,
            ShellPhase::Verifying => 3,
            ShellPhase::Waiting => 4,
            ShellPhase::Approval => 5,
            ShellPhase::Done => 6,
            ShellPhase::Failed => 7,
        }
    }

    fn ramp_cache_identity(self) -> OceanRampCacheIdentity {
        OceanRampCacheIdentity {
            ramp: self.ramp,
            top: self.top,
            height: self.height,
            phase_tag: self.phase_tag(),
            animated: self.animated,
            completion_active: self.completion_active(),
            presence: self.presence,
            context_percent: self.context_percent,
        }
    }

    /// Deterministic fingerprint of every column input owned by the ramp cache.
    /// Actual colors are encoded explicitly; this never depends on randomized
    /// hashing or debug formatting.
    #[must_use]
    pub fn ramp_fingerprint(self) -> u64 {
        self.ramp_cache_identity().fingerprint()
    }

    #[must_use]
    pub fn with_viewport(mut self, viewport: Rect) -> Self {
        self.top = viewport.y;
        self.height = viewport.height.max(1);
        self
    }

    /// Borrowed backend facts for this render snapshot; no detector or store.
    #[must_use]
    pub fn with_paint_caps(mut self, caps: Option<codewhale_ratatui::Caps>) -> Self {
        self.paint_caps = caps;
        self
    }

    fn paint_theme(self, ground: Color) -> Option<codewhale_ratatui::Theme> {
        let mut caps = self.paint_caps?;
        // Equality-matched opaque ground is actual pane evidence. A terminal
        // Reset/named ink supplies no RGB evidence; an OS hint cannot fill it.
        caps.appearance = codewhale_ratatui::detect::appearance_for_background(ground)?;
        Some(
            codewhale_ratatui::Theme::new(caps)
                .ground(codewhale_ratatui::Ground::Ocean)
                .tui_palette(codewhale_ratatui::TuiPalette::Underwater),
        )
    }

    /// Complete matching facade. The kit owns cell iteration and all guards;
    /// the same backend adapter reports exact visible custom-theme ink.
    pub fn paint_matching_native(
        self,
        area: Rect,
        buf: &mut Buffer,
        background: Color,
        ui_theme: &UiTheme,
        protected: &[Rect],
    ) {
        let facts = codewhale_ratatui::ocean::OceanPaintFacts {
            protected,
            ..codewhale_ratatui::ocean::OceanPaintFacts::new(background)
        };
        self.paint_native(area, buf, ui_theme, &facts);
    }

    pub(crate) fn paint_native(
        self,
        area: Rect,
        buf: &mut Buffer,
        ui_theme: &UiTheme,
        facts: &codewhale_ratatui::ocean::OceanPaintFacts<'_>,
    ) {
        if ui_theme.name != codewhale_palette::UNDERWATER_UI_THEME.name {
            return;
        }
        let Some(theme) = self.paint_theme(facts.ground) else {
            return;
        };
        let inks = codewhale_ratatui::ocean::OceanContrastInks {
            border: Some(ui_theme.border),
            border_strong: Some(ui_theme.text_hint),
            dim: Some(ui_theme.text_dim),
        };
        self.native()
            .ramp(self.ramp.native())
            .viewport(Rect::new(0, self.top, 0, self.height))
            .contrast_inks(inks)
            .apply_native(area, buf, &theme, facts, |cell, water| {
                crate::tui::color_compat::project_ocean_ink(cell, water, theme.depth(), ui_theme)
            });
    }

    pub(crate) fn paint_caustics(
        self,
        area: Rect,
        buf: &mut Buffer,
        facts: &codewhale_ratatui::ocean::OceanCausticFacts<'_>,
    ) {
        let Some(theme) = self.paint_theme(facts.paint.ground) else {
            return;
        };
        self.native()
            .ramp(self.ramp.native())
            .viewport(Rect::new(0, self.top, 0, self.height))
            .apply_caustics(area, buf, &theme, facts);
    }

    // Existing pure fixture facade explicitly supplies a synthetic backend.
    // Runtime callers all use the live fact-bearing native facade above.
    #[cfg(test)]
    fn paint_matching(self, area: Rect, buf: &mut Buffer, background: Color) {
        let caps = crate::tui::color_compat::ColorCompatBackend::new(
            std::io::sink(),
            codewhale_palette::ColorDepth::TrueColor,
            codewhale_palette::PaletteMode::Dark,
        )
        .native_ocean_caps();
        self.with_paint_caps(Some(caps)).paint_matching_native(
            area,
            buf,
            background,
            &codewhale_palette::UNDERWATER_UI_THEME,
            &[],
        );
    }
}

impl OceanRamp {
    #[must_use]
    pub fn for_theme(theme: &UiTheme) -> Option<Self> {
        // The painted field exists only under the underwater theme; every
        // other theme leaves the terminal's ground alone. A user-supplied
        // `background_color` rewrites the underwater surfaces through
        // `with_background_color` and remains the source of truth there.
        if theme.name != codewhale_palette::UNDERWATER_UI_THEME.name {
            return None;
        }

        let default = *theme == codewhale_palette::UNDERWATER_UI_THEME;
        Some(Self {
            // The authored Codewhale water column: unmistakably blue all the
            // way to the floor. These restrained ocean shades sit between the
            // shell's ink surfaces and its ambient blue, so the field gains
            // depth without becoming a saturated blue panel.
            surface: if default {
                Color::Rgb(0x0b, 0x2c, 0x53)
            } else {
                NativeRamp::SURFACE
            },
            middle: if default {
                Color::Rgb(0x09, 0x22, 0x41)
            } else {
                NativeRamp::MIDDLE
            },
            deep: NativeRamp::DEEP,
            ambient: if default {
                Color::Rgb(0x2a, 0x5e, 0x91)
            } else {
                NativeRamp::AMBIENT
            },
            attention: theme.warning,
            failure: theme.error_fg,
        })
    }

    fn native(self) -> NativeRamp {
        NativeRamp::new(
            self.surface,
            self.middle,
            self.deep,
            self.ambient,
            self.attention,
            self.failure,
        )
    }

    #[cfg(test)]
    #[must_use]
    pub fn color_at_context(self, row: u16, height: u16, context_percent: u8) -> Color {
        self.native().color_at_context(row, height, context_percent)
    }

    #[cfg(test)]
    #[must_use]
    pub fn color_at_phase_context(
        self,
        row: u16,
        height: u16,
        elapsed_ms: u128,
        phase: ShellPhase,
        context_percent: u8,
    ) -> Color {
        self.native().color_at_phase_context(
            row,
            height,
            phase_duration(elapsed_ms),
            native_phase(phase),
            context_percent,
        )
    }

    #[cfg(test)]
    #[must_use]
    pub fn color_at_completion_context(
        self,
        row: u16,
        height: u16,
        elapsed_ms: u128,
        context_percent: u8,
    ) -> Color {
        self.native().color_at_completion_context(
            row,
            height,
            completion_duration(elapsed_ms),
            context_percent,
        )
    }
}

fn native_phase(phase: ShellPhase) -> OceanPhase {
    match phase {
        ShellPhase::Idle => OceanPhase::Idle,
        ShellPhase::Typing => OceanPhase::Typing,
        ShellPhase::Working => OceanPhase::Working,
        ShellPhase::Verifying => OceanPhase::Verifying,
        ShellPhase::Waiting => OceanPhase::Waiting,
        ShellPhase::Approval => OceanPhase::Approval,
        ShellPhase::Done => OceanPhase::Done,
        ShellPhase::Failed => OceanPhase::Failed,
    }
}

fn phase_duration(elapsed_ms: u128) -> Duration {
    Duration::from_millis((elapsed_ms % 90_000) as u64)
}

fn completion_duration(elapsed_ms: u128) -> Duration {
    Duration::from_millis(elapsed_ms.min(COMPLETION_BREATH_MS) as u64)
}

#[must_use]
fn rgb(value: Color) -> Option<(u8, u8, u8)> {
    match value {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

#[must_use]
fn color((r, g, b): (u8, u8, u8)) -> Color {
    Color::Rgb(r, g, b)
}

#[must_use]
pub fn mix_colors(from: Color, to: Color, amount: f32) -> Color {
    match (rgb(from), rgb(to)) {
        (Some(from), Some(to)) => color(mix(from, to, amount)),
        _ => from,
    }
}

#[must_use]
pub fn scale_color(value: Color, brightness: f32) -> Color {
    let Some((r, g, b)) = rgb(value) else {
        return value;
    };
    color((
        (f32::from(r) * brightness).round().clamp(0.0, 255.0) as u8,
        (f32::from(g) * brightness).round().clamp(0.0, 255.0) as u8,
        (f32::from(b) * brightness).round().clamp(0.0, 255.0) as u8,
    ))
}

#[must_use]
fn mix(from: (u8, u8, u8), to: (u8, u8, u8), amount: f32) -> (u8, u8, u8) {
    let amount = amount.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| {
        (f32::from(a) + (f32::from(b) - f32::from(a)) * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    (
        channel(from.0, to.0),
        channel(from.1, to.1),
        channel(from.2, to.2),
    )
}

#[cfg(test)]
#[path = "ocean/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ocean/guarded_legacy.rs"]
mod guarded_legacy;
#[cfg(test)]
#[path = "ocean/guarded_tests.rs"]
mod guarded_tests;

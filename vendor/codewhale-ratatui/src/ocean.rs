//! Codewhale's terminal-native underwater column, as a finishing pass.
//!
//! Adapted from `Hmbown/CodeWhale`'s `crates/tui/src/tui/ocean.rs` at
//! `a79ce5c4d5ed1a5f7032185710c27343a900351c`: the authored three stops,
//! quadratic depth curve, context rise, 90-second phase breath, steady
//! attention/failure tint and 800-millisecond completion breath. The host
//! supplies time and phase; this module owns no clock, event loop or theme.
//!
//! Paint ordinary components first, then apply the column to their ordinary
//! `Background` and `Sidebar` cells. Raised surfaces, selections, diff/code
//! grounds, custom fills, symbols, inks and modifiers remain theirs. The
//! authored dark field remains exact on Ocean; other measured truecolor
//! themes draw depth and ambient light from their own grounds and accent.
//! Terminal-owned grounds, unknown appearances and lower depths stay unchanged.
//! Native chrome can join the same column with [`OceanColumn::apply_matching`]
//! over its own region and explicit base ground. Native cached-row painting,
//! semantic-surface projection and sparse caustics are adapted from the same
//! MIT-licensed Engine at `e7de150f3621740c982e7e6c90d69ac48ccc80d5`;
//! capability facts, palette adaptation, cache and redraw lifecycle stay host-owned.

use std::time::Duration;

use ratatui::{
    buffer::{Buffer, Cell},
    layout::{Alignment, Rect},
    style::{Color, Modifier},
    text::Line,
};

use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Ground, MotionMode, Paint, Role, Theme,
    color::{ColorDepth, blend, contrast_ratio, relative_luminance, rgb},
    detect::Appearance,
};

/// A caller-reported phase, matching the native terminal's `ShellPhase`.
/// It is visual input, never a state machine or evidence of work completing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OceanPhase {
    #[default]
    Idle,
    Typing,
    Working,
    Verifying,
    Waiting,
    Approval,
    Done,
    Failed,
}

impl OceanPhase {
    const fn needs_attention(self) -> bool {
        matches!(self, Self::Waiting | Self::Approval | Self::Failed)
    }
}

/// A depth ramp in the active theme's own colors. Semantic tints come from [`Theme`].
/// [`Self::for_theme`] resolves the guarded default; [`Self::new`] supplies
/// exact caller colors for pure math. Neither replaces a theme table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OceanRamp {
    surface: Color,
    middle: Color,
    deep: Color,
    ambient: Color,
    attention: Color,
    failure: Color,
}

impl OceanRamp {
    /// The native sunlit stop, `#102a45`.
    pub const SURFACE: Color = rgb(0x102a45);
    /// The native middle control point, `#0a1e33`.
    pub const MIDDLE: Color = rgb(0x0a1e33);
    /// The native deep stop, `#061320`.
    pub const DEEP: Color = rgb(0x061320);
    /// The native ambient light, `#264866`.
    pub const AMBIENT: Color = rgb(0x264866);
    /// The authored completion pulse ends after 800 milliseconds.
    pub const COMPLETION_BREATH: Duration = Duration::from_millis(800);

    /// An explicit caller-owned ramp. Pure sampling may use these exact
    /// colors; guarded column painting still requires the normal theme and
    /// capability gates and preserves its contrast policy.
    #[must_use]
    pub const fn new(
        surface: Color,
        middle: Color,
        deep: Color,
        ambient: Color,
        attention: Color,
        failure: Color,
    ) -> Self {
        Self {
            surface,
            middle,
            deep,
            ambient,
            attention,
            failure,
        }
    }

    /// Resolve a measured truecolor field from the theme's painted RGB grounds.
    /// Native Ocean keeps its authored stops; terminal-owned bases stay unpainted.
    #[must_use]
    pub fn for_theme(theme: &Theme) -> Option<Self> {
        if theme.depth() != ColorDepth::TrueColor
            || !theme.caps().paints_tokens()
            || !theme.paints_base_ground()
        {
            return None;
        }
        let background = theme.color(Role::Background)?;
        if !matches!(background, Color::Rgb(..)) {
            return None;
        }
        let attention = theme.color(Role::Attention)?;
        let failure = theme.color(Role::Danger)?;
        if theme.caps().appearance == Appearance::Dark
            && theme.ground_kind() == Ground::Ocean
            && theme
                .native_palette()
                .is_none_or(|palette| palette == crate::TuiPalette::Underwater)
        {
            return Some(Self::new(
                Self::SURFACE,
                Self::MIDDLE,
                Self::DEEP,
                Self::AMBIENT,
                attention,
                failure,
            ));
        }
        let mut brightest = background;
        let mut darkest = background;
        let mut high = relative_luminance(background)?;
        let mut low = high;
        for role in [Role::Surface, Role::Hover, Role::Sidebar] {
            let color = theme.color(role)?;
            if !matches!(color, Color::Rgb(..)) {
                return None;
            }
            let luminance = relative_luminance(color)?;
            if luminance > high {
                brightest = color;
                high = luminance;
            }
            if luminance < low {
                darkest = color;
                low = luminance;
            }
        }
        let light = theme.caps().appearance == Appearance::Light;
        let surface = mix_toward(background, brightest, if light { 0.45 } else { 0.35 });
        Some(Self::new(
            surface,
            background,
            mix_toward(background, darkest, if light { 0.45 } else { 0.65 }),
            mix_toward(surface, theme.color(Role::Accent)?, 0.20),
            attention,
            failure,
        ))
    }

    /// The native quadratic surface → middle → deep curve. Explicit context
    /// fullness (0–100) raises the abyss; omit it at the component level when
    /// the host has no measured context value. Rows outside the column clamp.
    #[must_use]
    pub fn color_at_context(self, row: u16, height: u16, context_percent: u8) -> Color {
        let rise = f32::from(context_percent.min(100)) / 100.0;
        if height <= 1 {
            return mix_toward(self.surface, self.deep, rise);
        }
        let position = (f32::from(row.min(height - 1)) / f32::from(height - 1) + rise).min(1.0);
        let toward_middle = mix_toward(self.surface, self.middle, position);
        let toward_deep = mix_toward(self.middle, self.deep, position);
        mix_toward(toward_middle, toward_deep, position)
    }

    /// The native phase treatment at caller time. Waiting/approval are warm
    /// and failure is steady; neither depends on elapsed time.
    #[must_use]
    pub fn color_at_phase_context(
        self,
        row: u16,
        height: u16,
        elapsed: Duration,
        phase: OceanPhase,
        context_percent: u8,
    ) -> Color {
        if phase.needs_attention() {
            return self.color_at_attention_context(row, height, phase, context_percent);
        }
        let base = self.color_at_context(row, height, context_percent);
        let depth = depth_at(row, height, context_percent);
        let cycle = (elapsed.as_millis() % 90_000) as f32 / 90_000.0;
        let breath = (cycle * std::f32::consts::TAU).sin() * 0.5 + 0.5;
        let (bias, phase_depth) = match phase {
            OceanPhase::Idle => (0.035, 1.0 - depth),
            OceanPhase::Typing => (0.025, 1.0 - depth),
            OceanPhase::Working => (0.045, 0.35 + depth * 0.65),
            OceanPhase::Verifying => (0.055, 0.65 + (1.0 - depth) * 0.35),
            OceanPhase::Done => (0.018, 1.0 - depth),
            OceanPhase::Waiting | OceanPhase::Approval | OceanPhase::Failed => unreachable!(),
        };
        mix_toward(base, self.ambient, breath * bias * phase_depth)
    }

    /// The native steady attention/failure tint, also used in reduced motion.
    #[must_use]
    pub fn color_at_attention_context(
        self,
        row: u16,
        height: u16,
        phase: OceanPhase,
        context_percent: u8,
    ) -> Color {
        let base = self.color_at_context(row, height, context_percent);
        match phase {
            OceanPhase::Waiting | OceanPhase::Approval => mix_toward(
                base,
                self.attention,
                0.10 * (0.6 + 0.4 * (1.0 - depth_at(row, height, context_percent))),
            ),
            OceanPhase::Failed => mix_toward(base, self.failure, 0.09),
            _ => base,
        }
    }

    /// Dark completion brightness: 88% → 112% at 320 ms → 100% at 800 ms.
    /// Light grounds receive a gentle accent reflection over the same interval.
    /// The component uses it only for a reported `Done` phase in
    /// [`MotionMode::Full`]; a stale completion clock cannot mask failure.
    #[must_use]
    pub fn color_at_completion_context(
        self,
        row: u16,
        height: u16,
        elapsed: Duration,
        context_percent: u8,
    ) -> Color {
        let base = self.color_at_context(row, height, context_percent);
        let t = elapsed.as_millis().min(800) as f32 / 800.0;
        if relative_luminance(base).is_some_and(|luminance| luminance >= 0.5) {
            let pulse = if t <= 0.4 { t / 0.4 } else { (1.0 - t) / 0.6 };
            return mix_toward(
                base,
                self.ambient,
                0.12 * pulse * pulse * (3.0 - 2.0 * pulse),
            );
        }
        let brightness = if t <= 0.4 {
            0.88 + (1.12 - 0.88) * (t / 0.4)
        } else {
            1.12 + (1.0 - 1.12) * ((t - 0.4) / 0.6)
        };
        let Color::Rgb(r, g, b) = base else {
            return base;
        };
        let scale = |c| (f32::from(c) * brightness).round().clamp(0.0, 255.0) as u8;
        Color::Rgb(scale(r), scale(g), scale(b))
    }
}

// Explicit native ramps may carry a terminal-owned or indexed tint. There
// is no RGB interpolation evidence in that case; retain the source color.
fn mix_toward(from: Color, to: Color, amount: f32) -> Color {
    if matches!((from, to), (Color::Rgb(..), Color::Rgb(..))) {
        blend(to, from, amount)
    } else {
        from
    }
}

fn depth_at(row: u16, height: u16, context_percent: u8) -> f32 {
    if height <= 1 {
        0.0
    } else {
        (f32::from(row.min(height - 1)) / f32::from(height - 1)
            + f32::from(context_percent.min(100)) / 100.0)
            .min(1.0)
    }
}

/// One shared native water column over caller-owned components.
///
/// The host supplies elapsed time and decides when a visible field deserves
/// another redraw. [`MotionMode::Reduced`] and [`MotionMode::Still`] ignore
/// both clocks and keep steady attention/failure tint. ASCII changes no text
/// here: this finishing pass paints backgrounds only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OceanColumn {
    ramp: Option<OceanRamp>,
    elapsed: Duration,
    motion: MotionMode,
    phase: OceanPhase,
    completion_elapsed: Option<Duration>,
    viewport: Option<Rect>,
    context_percent: u8,
    presence: u16,
    contrast_inks: Option<OceanContrastInks>,
}

impl OceanColumn {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            ramp: None,
            elapsed,
            motion,
            phase: OceanPhase::Idle,
            completion_elapsed: None,
            viewport: None,
            context_percent: 0,
            presence: 1000,
            contrast_inks: None,
        }
    }

    /// Supply the host's actual ramp without granting paint capability.
    /// `color_at_y`, `apply` and `apply_matching` keep their existing theme,
    /// terminal, semantic-surface and contrast guards.
    #[must_use]
    pub const fn ramp(mut self, ramp: OceanRamp) -> Self {
        self.ramp = Some(ramp);
        self
    }

    #[must_use]
    pub const fn phase(mut self, phase: OceanPhase) -> Self {
        self.phase = phase;
        self
    }

    /// Caller-measured context fullness, clamped to 100. The default leaves
    /// the authored column at its normal depth and asserts no usage fact.
    #[must_use]
    pub const fn context_percent(mut self, context_percent: u8) -> Self {
        self.context_percent = if context_percent > 100 {
            100
        } else {
            context_percent
        };
        self
    }

    /// Caller time since an actual successful completion. Honored only in
    /// `Done` with Full motion, and only before the native 800 ms boundary.
    #[must_use]
    pub const fn completion_elapsed(mut self, elapsed: Duration) -> Self {
        self.completion_elapsed = Some(elapsed);
        self
    }

    /// Share this viewport across separate bands so the gradient continues
    /// through them. Without it, the paint area is the complete column.
    #[must_use]
    pub const fn viewport(mut self, viewport: Rect) -> Self {
        self.viewport = Some(viewport);
        self
    }

    /// Host-sampled ambient presence, 0–1000 as in the native renderer.
    /// This eases only phase breathing; attention/failure remain steady and
    /// the successful completion pulse keeps its own gated clock.
    #[must_use]
    pub const fn presence(mut self, presence: u16) -> Self {
        self.presence = if presence > 1000 { 1000 } else { presence };
        self
    }

    /// Pure sampling at an absolute row. `viewport` supplies the default
    /// column bounds unless the builder already specifies shared bounds.
    /// Returns `None` where a measured RGB field is unavailable.
    #[must_use]
    pub fn color_at_y(&self, y: u16, viewport: Rect, theme: &Theme) -> Option<Color> {
        let ramp = OceanRamp::for_theme(theme)?;
        Some(self.color_at_y_with_ramp(y, viewport, ramp))
    }

    /// Pure sampling under a caller-owned rendering policy.
    /// Like the ramp's math helpers, this paints nothing and has no terminal
    /// capability gate; guarded sampling and painting keep their own gates.
    /// A builder-supplied `ramp` overrides this fallback ramp; `viewport`
    /// likewise retains the shared absolute column. Motion, completion,
    /// phase, context and presence follow the same kernel as guarded paint.
    /// Hosts must keep their existing paint/terminal guards around the result.
    #[must_use]
    pub fn color_at_y_with_ramp(&self, y: u16, viewport: Rect, ramp: OceanRamp) -> Color {
        let viewport = self.viewport.unwrap_or(viewport);
        let ramp = self.ramp.unwrap_or(ramp);
        let height = viewport.height.max(1);
        let row = y.saturating_sub(viewport.y).min(height - 1);
        if self.motion.animates()
            && self.phase == OceanPhase::Done
            && let Some(elapsed) = self
                .completion_elapsed
                .filter(|t| *t < OceanRamp::COMPLETION_BREATH)
        {
            return ramp.color_at_completion_context(row, height, elapsed, self.context_percent);
        }
        if self.phase.needs_attention() {
            return ramp.color_at_attention_context(row, height, self.phase, self.context_percent);
        }
        let base = ramp.color_at_context(row, height, self.context_percent);
        if !self.motion.animates() || self.presence == 0 {
            return base;
        }
        let phase = ramp.color_at_phase_context(
            row,
            height,
            self.elapsed,
            self.phase,
            self.context_percent,
        );
        mix_toward(base, phase, f32::from(self.presence) / 1000.0)
    }

    /// Finish ordinary `Background` and `Sidebar` cells, clipped to the
    /// buffer. Colors shared with semantic fills remain exact; explicit
    /// chrome regions can opt in through `apply_matching`. Visible inks must retain their
    /// contrast floor; unknown terminal inks and reversed cells are spared.
    pub fn apply(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let semantic = [
            Role::Surface,
            Role::Hover,
            Role::Selected,
            Role::DiffAddedTint,
            Role::DiffRemovedTint,
            Role::Accent,
            Role::Live,
            Role::Attention,
            Role::Danger,
        ]
        .map(|role| theme.bg(role).bg);
        let grounds = [Role::Background, Role::Sidebar].map(|role| {
            theme
                .bg(role)
                .bg
                .filter(|color| !semantic.contains(&Some(*color)))
        });
        self.apply_grounds(area, buf, theme, &grounds);
    }

    /// Continue the column through cells with the caller's explicit base
    /// `ground`, restricted to `area`. Use [`Self::viewport`] to share one
    /// absolute water column across conversation, composer and footer bands.
    ///
    /// Other grounds, symbols, inks and modifiers remain untouched. The
    /// existing theme, clipping and contrast policy still applies, including
    /// protection for unknown inks and reversed cells. The caller chooses a
    /// region containing ordinary chrome: a semantic surface using this same
    /// base color must be kept outside that region.
    pub fn apply_matching(&self, area: Rect, buf: &mut Buffer, theme: &Theme, ground: Color) {
        self.apply_grounds(area, buf, theme, &[Some(ground)]);
    }

    /// Native finishing inputs: cached samples and semantic rectangles are
    /// caller facts, while capability, clipping, reverse and contrast guards
    /// are the same guard kernel used by `apply` and `apply_matching`.
    /// `project_ink` returns the host backend's actual proposed visible ink;
    /// it never mutates the source cell, symbols or modifiers.
    pub fn apply_native(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        facts: &OceanPaintFacts<'_>,
        project_ink: impl Fn(&Cell, Color) -> Color,
    ) {
        self.apply_grounds_with(area, buf, theme, &[Some(facts.ground)], facts, project_ink);
    }

    /// Map the three decorative/supporting floors to exact live host colors.
    /// Other inks keep the text floor, including unrecognized custom colors.
    #[must_use]
    pub const fn contrast_inks(mut self, inks: OceanContrastInks) -> Self {
        self.contrast_inks = Some(inks);
        self
    }

    fn apply_grounds(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        grounds: &[Option<Color>],
    ) {
        self.apply_grounds_with(
            area,
            buf,
            theme,
            grounds,
            &OceanPaintFacts::new(Color::Reset),
            |cell, _| cell.fg,
        );
    }

    fn apply_grounds_with(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        grounds: &[Option<Color>],
        facts: &OceanPaintFacts<'_>,
        project_ink: impl Fn(&Cell, Color) -> Color,
    ) {
        let Some(ramp) = OceanRamp::for_theme(theme) else {
            return;
        };
        let viewport = self.viewport.unwrap_or(area);
        let area = area.intersection(buf.area);
        for y in area.top()..area.bottom() {
            let water = facts
                .samples
                .get(usize::from(y.saturating_sub(facts.sample_top)))
                .filter(|_| y >= facts.sample_top)
                .copied()
                .unwrap_or_else(|| self.color_at_y_with_ramp(y, viewport, ramp));
            let mut previous_ink = None;
            for x in area.left()..area.right() {
                if facts
                    .protected
                    .iter()
                    .any(|rect| rect.contains((x, y).into()))
                {
                    continue;
                }
                let cell = &mut buf[(x, y)];
                if grounds.contains(&Some(cell.bg)) && !cell.modifier.contains(Modifier::REVERSED) {
                    let safe = cell.symbol() == " " || {
                        let ink = project_ink(cell, water);
                        if let Some((previous, safe)) = previous_ink
                            && previous == ink
                        {
                            safe
                        } else {
                            let safe = ink_is_safe(ink, water, theme, self.contrast_inks);
                            previous_ink = Some((ink, safe));
                            safe
                        }
                    };
                    if safe {
                        cell.set_bg(water);
                    }
                }
            }
        }
    }

    /// Sparse caustics can finish only already painted ordinary water.
    /// They share the same capability/motion guard and never cross a semantic
    /// rectangle, a different ground, visible ink or a reversed cell.
    pub fn apply_caustics(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        facts: &OceanCausticFacts<'_>,
    ) {
        if !self.motion.animates() || area.width < 40 || area.height < 10 {
            return;
        }
        let Some(ramp) = OceanRamp::for_theme(theme) else {
            return;
        };
        let ramp = self.ramp.unwrap_or(ramp);
        let viewport = self.viewport.unwrap_or(area);
        let clipped = area.intersection(buf.area);
        let band = facts.band_rows.min(area.height);
        for y in clipped.y..clipped.bottom().min(area.y.saturating_add(band)) {
            let local_y = y.saturating_sub(area.y);
            let water = facts
                .paint
                .samples
                .get(usize::from(y.saturating_sub(facts.paint.sample_top)))
                .filter(|_| y >= facts.paint.sample_top)
                .copied()
                .unwrap_or_else(|| self.color_at_y_with_ramp(y, viewport, ramp));
            let depth = 1.0 - f32::from(local_y) / f32::from(band.max(1));
            for x in clipped.x..clipped.right() {
                let local_x = x.saturating_sub(area.x);
                if local_x % 3 != 0
                    || facts
                        .paint
                        .protected
                        .iter()
                        .any(|r| r.contains((x, y).into()))
                {
                    continue;
                }
                let cell = &mut buf[(x, y)];
                if cell.bg != water
                    || cell.modifier.contains(Modifier::REVERSED)
                    || !(cell.symbol() == " " || cell.symbol().is_empty())
                {
                    continue;
                }
                let brightness =
                    ocean_caustic_brightness(facts.elapsed, local_x, local_y, depth * depth);
                if let Color::Rgb(r, g, b) = water {
                    if relative_luminance(water).is_some_and(|luminance| luminance >= 0.5) {
                        cell.set_bg(mix_toward(water, ramp.ambient, (brightness - 1.0) * 1.5));
                        continue;
                    }
                    let scale =
                        |value| (f32::from(value) * brightness).round().clamp(0.0, 255.0) as u8;
                    cell.set_bg(Color::Rgb(scale(r), scale(g), scale(b)));
                }
            }
        }
    }
}

impl Paint for OceanColumn {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.apply(area, buf, theme);
    }
}

fn ink_is_safe(ink: Color, water: Color, theme: &Theme, live: Option<OceanContrastInks>) -> bool {
    if let Some(live) = live {
        let floor = if live.border == Some(ink) {
            1.0
        } else if live.border_strong == Some(ink) || live.dim == Some(ink) {
            3.0
        } else {
            4.5
        };
        return contrast_ratio(ink, water).is_some_and(|ratio| ratio >= floor);
    }
    let floor = if [Role::Border, Role::BorderStrong, Role::Dim]
        .into_iter()
        .any(|role| theme.color(role) == Some(ink))
    {
        // Border is purely decorative and deliberately has no text floor.
        if theme.color(Role::Border) == Some(ink) {
            1.0
        } else {
            3.0
        }
    } else {
        4.5
    };
    contrast_ratio(ink, water).is_some_and(|ratio| ratio >= floor)
}

/// Actual live host colors for the existing decorative/supporting floors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OceanContrastInks {
    pub border: Option<Color>,
    pub border_strong: Option<Color>,
    pub dim: Option<Color>,
}

/// Bounded frame-derived presentation facts; no cache or surface store.
#[derive(Clone, Copy, Debug)]
pub struct OceanPaintFacts<'a> {
    pub ground: Color,
    pub sample_top: u16,
    pub samples: &'a [Color],
    pub protected: &'a [Rect],
}
impl OceanPaintFacts<'_> {
    #[must_use]
    pub const fn new(ground: Color) -> Self {
        Self {
            ground,
            sample_top: 0,
            samples: &[],
            protected: &[],
        }
    }
}

/// The host reports elapsed time and its unoccupied surface-band height.
#[derive(Clone, Copy, Debug)]
pub struct OceanCausticFacts<'a> {
    pub paint: OceanPaintFacts<'a>,
    pub elapsed: Duration,
    pub band_rows: u16,
}

/// The native continuous 960ms travelling crest; owns no clock or loop.
#[must_use]
pub fn ocean_caustic_brightness(
    elapsed: Duration,
    local_x: u16,
    local_y: u16,
    depth_fade: f32,
) -> f32 {
    let time = (elapsed.as_millis() % 960) as f64 / 960.0;
    let slot = (u32::from(local_x / 3) + u32::from(local_y)) % 4;
    let phase = (time + f64::from(slot) / 4.0) * std::f64::consts::TAU;
    let crest = ((phase.cos() + 1.0) * 0.5).powi(8);
    1.0 + 0.08 * (crest as f32) * depth_fade.clamp(0.0, 1.0)
}

/// Explicit styled grounds are semantic even when a custom theme aliases
/// their RGB to an ordinary pane. Project guarded prewrapped source once;
/// callers pass these frame-only rectangles to native finishing.
#[must_use]
pub fn ocean_semantic_surfaces(
    rows: &[Line<'static>],
    area: Rect,
    measure_grapheme: impl Fn(&str) -> usize,
) -> Vec<Rect> {
    let measure = |value: &str| {
        crate::text::display_safe(value)
            .graphemes(true)
            .fold(0usize, |width, g| width.saturating_add(measure_grapheme(g)))
    };
    let mut regions = Vec::new();
    for (index, line) in rows.iter().take(usize::from(area.height)).enumerate() {
        let y = area
            .y
            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
        if line.style.bg.is_some() {
            if area.width > 0 {
                regions.push(Rect::new(area.x, y, area.width, 1));
            }
            continue;
        }
        let width = line.spans.iter().fold(0usize, |width, span| {
            width.saturating_add(measure(&span.content))
        });
        let padding = usize::from(area.width).saturating_sub(width);
        let mut column = match line.alignment.unwrap_or(Alignment::Left) {
            Alignment::Left => 0,
            Alignment::Center => padding / 2,
            Alignment::Right => padding,
        };
        for span in &line.spans {
            let end = column.saturating_add(measure(&span.content));
            let clipped_end = end.min(usize::from(area.width));
            if span.style.bg.is_some() && column < clipped_end {
                regions.push(Rect::new(
                    area.x.saturating_add(column as u16),
                    y,
                    (clipped_end - column) as u16,
                    1,
                ));
            }
            column = end;
        }
    }
    regions
}

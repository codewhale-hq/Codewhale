//! The native marine habitat: a fish school, a far shoal of minnows, one
//! visiting jellyfish, marine snow, bubbles and a distant whale.
//!
//! This is the one ocean Codewhale draws, built for the Engine's Underwater
//! field through [`Habitat`]. These components own no clock, simulation,
//! input handler or runtime state. The host supplies elapsed time, motion
//! policy, an optional ink and a presence. Reduced and still motion remove
//! ambient life; a host may separately paint its existing [`crate::Whale`].
//!
//! Every mark lerps from the water it sits on toward its ink at a slow,
//! quantised glow that stays well below text, so a frame changes only the
//! cells a creature moves through. Positions are pure functions of elapsed
//! time: a host may change ink or presence without moving anything.
//!
//! Paint content first, then habitat. Every creature needs open water: no
//! visible glyph within one row and two columns of its whole silhouette, and
//! no overlap with [`Habitat::protected`]. Multi-row creatures are withheld
//! as a unit rather than split or moved around text. No component clears its
//! area or repaints existing text. Painting into a fresh buffer each frame
//! lets creatures move without leaving trails.
//!
//! Known limits: creatures interact only through that open-water rule; the
//! distant whale is withheld for the frame when any part of it would touch
//! text; ASCII themes keep the fish, jellyfish and bubbles only; non-truecolor
//! themes, unknown grounds or non-RGB water or ink fall back to the plain role
//! colour without a glow.

use std::time::Duration;

use ratatui::{
    buffer::{Buffer, CellDiffOption},
    layout::Rect,
    style::{Color, Style},
};

use crate::{MotionMode, Paint, Role, Theme, color::ColorDepth};

#[path = "habitat_poses.rs"]
mod poses;

/// A bounded marine population. Auto follows the Engine's terminal tiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HabitatDensity {
    #[default]
    Auto,
    /// Three fish, one bubble stream, four motes, no shoal.
    Sparse,
    /// Five fish, three minnows, two bubble streams, seven motes.
    Normal,
    /// Six fish, five minnows, three bubble streams, ten motes. There is
    /// still at most one jellyfish and one whale.
    Rich,
}

impl HabitatDensity {
    #[must_use]
    pub const fn for_area(self, area: Rect) -> Self {
        match self {
            Self::Auto if area.width < 56 || area.height < 12 => Self::Sparse,
            Self::Auto if area.width < 88 || area.height < 20 => Self::Normal,
            Self::Auto => Self::Rich,
            other => other,
        }
    }

    fn fish(self) -> usize {
        match self {
            Self::Sparse => 3,
            Self::Normal => 5,
            Self::Rich | Self::Auto => 6,
        }
    }

    fn minnows(self) -> usize {
        match self {
            Self::Sparse => 0,
            Self::Normal => 3,
            Self::Rich | Self::Auto => 5,
        }
    }

    fn bubbles(self) -> usize {
        match self {
            Self::Sparse => 1,
            Self::Normal => 2,
            Self::Rich | Self::Auto => 3,
        }
    }

    fn motes(self) -> usize {
        match self {
            Self::Sparse => 4,
            Self::Normal => 7,
            Self::Rich | Self::Auto => 10,
        }
    }
}

const CLEAR_COLS: u16 = 2;
const CLEAR_ROWS: u16 = 1;
const GLOW_STEPS: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Brush {
    ink: Option<Color>,
    presence: u16,
}

impl Brush {
    fn visible(self) -> bool {
        self.presence > 0
    }

    fn amount(self, glow: f32) -> f32 {
        let amount = glow * f32::from(self.presence) / 1_000.0;
        (amount.clamp(0.0, 1.0) * GLOW_STEPS).round() / GLOW_STEPS
    }

    fn style(self, theme: &Theme, role: Role, glow: f32, water: Color) -> Option<Style> {
        let amount = self.amount(glow);
        if amount == 0.0 {
            return None;
        }
        let can_glow = theme.paints_grounds() && theme.depth() == ColorDepth::TrueColor;
        let ink = self.ink.unwrap_or_else(|| theme.token(role));
        Some(match crate::color::glow_over(ink, water, amount) {
            Some(color) if can_glow => Style::default().fg(color),
            _ => theme.fg(role),
        })
    }

    fn put(
        self,
        buf: &mut Buffer,
        at: Rect,
        chars: impl IntoIterator<Item = char>,
        theme: &Theme,
        role: Role,
        glow: f32,
    ) -> bool {
        if self.amount(glow) == 0.0 {
            return false;
        }
        let mut painted = false;
        for (offset, ch) in chars.into_iter().take(usize::from(at.width)).enumerate() {
            if ch == ' ' {
                continue;
            }
            let Some(cell) = buf.cell_mut((at.x + offset as u16, at.y)) else {
                continue;
            };
            let Some(style) = self.style(theme, role, glow, cell.bg) else {
                continue;
            };
            cell.set_char(ch).set_style(style);
            painted = true;
        }
        painted
    }
}

macro_rules! brush_builders {
    () => {
        /// Ink for every mark, lerped from the water by the creature's glow.
        /// `None` (the default) uses the creature's role token.
        #[must_use]
        pub const fn ink(mut self, ink: Option<Color>) -> Self {
            self.brush.ink = ink;
            self
        }

        /// Scales every glow, `0.0..=1.0`. Zero paints nothing; it lets a host
        /// fade life in and out without moving it.
        #[must_use]
        pub fn presence(mut self, presence: f32) -> Self {
            self.brush.presence = (presence.clamp(0.0, 1.0) * 1_000.0).round() as u16;
            self
        }
    };
}

const WEDGE: &[(i32, u16)] = &[(0, 0), (-1, 4), (1, 6), (-1, 9), (1, 11), (0, 14)];
const SCHOOL_CELL_MS: u128 = 760;
const FISH_WAVE_MS: u128 = 3_600;
const FISH_GLOW: (f32, f32) = (0.30, 0.62);

/// One loose school near the floor, with four native tail poses that step
/// with half-cell travel. ASCII terminals keep an eyed lead and smaller
/// members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FishSchool {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub density: HabitatDensity,
    brush: Brush,
}

impl FishSchool {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            density: HabitatDensity::Auto,
            brush: Brush {
                ink: None,
                presence: 1_000,
            },
        }
    }

    #[must_use]
    pub const fn density(mut self, density: HabitatDensity) -> Self {
        self.density = density;
        self
    }

    brush_builders!();
}

impl FishSchool {
    fn paint_except(&self, area: Rect, buf: &mut Buffer, theme: &Theme, protected: &[Rect]) {
        let area = area.intersection(buf.area);
        if !self.motion.animates() || !self.brush.visible() || area.width < 4 || area.height < 4 {
            return;
        }
        let count = self.density.for_area(area).fish();
        let span = WEDGE[count - 1].1 + 4;
        let travel = u128::from(area.width) + u128::from(span);
        let cycle_ms = travel * SCHOOL_CELL_MS;
        let t = self.elapsed.as_millis();
        let clock = t.saturating_add(cycle_ms / 2);
        let right = swims_right(clock / cycle_ms);
        let step = ((clock % cycle_ms) * travel * 2 / cycle_ms) as i32;
        let mut marks = Vec::with_capacity(count);
        for (member, &(dy, behind)) in WEDGE.iter().take(count).enumerate() {
            let bob = sine_bob(t, 3_000 + jitter(member as u128 + 41) % 2_600, 1);
            let drift = i32::from(sine_bob(t, 5_200 + jitter(member as u128 + 617) % 3_400, 2)) - 1;
            let dot_x = if right {
                step - i32::from(behind) * 2 - 8 + drift
            } else {
                i32::from(area.width) * 2 - step + i32::from(behind) * 2 - drift
            };
            let (body, width, x) = if theme.ascii() {
                let body = match (right, member == 0) {
                    (true, true) => "><o>",
                    (true, false) => "><>",
                    (false, true) => "<o><",
                    (false, false) => "<><",
                };
                let width = body.len() as u16;
                let x = if right {
                    step / 2 - i32::from(behind) - i32::from(width)
                } else {
                    i32::from(area.width) - step / 2 + i32::from(behind)
                };
                (body, width, x)
            } else {
                let pose = (step.div_euclid(2) as usize + member) % 4;
                (
                    poses::fish(right, pose, dot_x.rem_euclid(2) as usize, usize::from(bob)),
                    4,
                    dot_x.div_euclid(2),
                )
            };
            let y =
                i32::from(area.height) - 3 + dy + if theme.ascii() { i32::from(bob) } else { 0 };
            let Some(at) = local_rect(area, x, y, width, 1) else {
                continue;
            };
            if open_water(at, buf, protected) {
                let glow = FISH_GLOW.0
                    + (FISH_GLOW.1 - FISH_GLOW.0) * wave01(t, FISH_WAVE_MS, member as u128 * 320);
                marks.push((at, body, glow));
            }
        }
        // All members see the host's original text, rather than mistaking a
        // neighbouring member of their wedge for a word.
        for (at, body, glow) in marks {
            self.brush
                .put(buf, at, body.chars(), theme, Role::Hint, glow);
        }
    }
}

impl Paint for FishSchool {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.paint_except(area, buf, theme, &[]);
    }
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        6
    }
}

const SHOAL_CELL_MS: u128 = 1_150;
const SHOAL_GLOW: (f32, f32) = (0.16, 0.34);

/// A distant shoal of minnows crossing mid-water against the school, slower
/// and dimmer. Native braille only; ASCII themes leave it out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shoal {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub density: HabitatDensity,
    brush: Brush,
}

impl Shoal {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            density: HabitatDensity::Auto,
            brush: Brush {
                ink: None,
                presence: 1_000,
            },
        }
    }

    #[must_use]
    pub const fn density(mut self, density: HabitatDensity) -> Self {
        self.density = density;
        self
    }

    brush_builders!();
}

impl Shoal {
    fn paint_except(&self, area: Rect, buf: &mut Buffer, theme: &Theme, protected: &[Rect]) {
        let area = area.intersection(buf.area);
        let count = self.density.for_area(area).minnows();
        if !self.motion.animates()
            || !self.brush.visible()
            || theme.ascii()
            || count == 0
            || area.width < 3
            || area.height < 6
        {
            return;
        }
        let t = self.elapsed.as_millis();
        let span = (count as u16).saturating_mul(5).saturating_add(3);
        let travel = u128::from(area.width.saturating_add(span).max(1));
        let cycle_ms = travel * SHOAL_CELL_MS;
        let clock = t.saturating_add(cycle_ms / 3);
        let right = !swims_right((clock / cycle_ms).wrapping_add(3));
        let step = ((clock % cycle_ms) * travel * 2 / cycle_ms) as i32;
        let band = i32::from(area.height.saturating_mul(3) / 5) * 2;
        let mut marks = Vec::with_capacity(count);
        for member in 0..count {
            let lag = member as i32 * 10 + (jitter(member as u128 + 211) % 4) as i32;
            let dot_x = if right {
                step - lag - 6
            } else {
                i32::from(area.width) * 2 - step + lag
            };
            let bob = i32::from(sine_bob(t, 4_300 + jitter(member as u128 + 97) % 2_900, 1));
            let half_row = band + [0, 3, 1, 4, 2][member % 5] - 2 + bob;
            let Some(at) = local_rect(area, dot_x.div_euclid(2), half_row.div_euclid(2), 3, 1)
            else {
                continue;
            };
            if open_water(at, buf, protected) {
                let pose = (step.div_euclid(2) as usize + member) % 2;
                let body = poses::minnow(
                    right,
                    pose,
                    dot_x.rem_euclid(2) as usize,
                    half_row.rem_euclid(2) as usize,
                );
                let glow = SHOAL_GLOW.0
                    + (SHOAL_GLOW.1 - SHOAL_GLOW.0) * wave01(t, 3_100, member as u128 * 530);
                marks.push((at, body, glow));
            }
        }
        for (at, body, glow) in marks {
            self.brush
                .put(buf, at, body.chars(), theme, Role::Dim, glow);
        }
    }
}

impl Paint for Shoal {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.paint_except(area, buf, theme, &[]);
    }
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        8
    }
}

const JELLY_ROW_MS: u128 = 9_400;
const JELLY_VISIT_ROWS: u128 = 6;
const JELLY_CYCLE_ROWS: u128 = 20;
const JELLY_PULSE_MS: u128 = 5_200;
const JELLY_ARM_LAG_MS: u128 = 620;
const JELLY_GLOW: (f32, f32) = (0.18, 0.44);

/// One complete bell with two lagging arms. The standalone component is
/// always present under Full motion. `visitor(true)` gives it the Engine's
/// slow six-row rise and long absence between visits, suitable for a habitat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jellyfish {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub visitor: bool,
    /// Keep a visitor's rise cycling during the Engine's long absence.
    /// Collision checks remain authoritative even for a held visit.
    pub hold_visit: bool,
    brush: Brush,
}

impl Jellyfish {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            visitor: false,
            hold_visit: false,
            brush: Brush {
                ink: None,
                presence: 1_000,
            },
        }
    }

    #[must_use]
    pub const fn visitor(mut self, visitor: bool) -> Self {
        self.visitor = visitor;
        self
    }

    #[must_use]
    pub const fn hold_visit(mut self, hold: bool) -> Self {
        self.hold_visit = hold;
        self
    }

    brush_builders!();
}

impl Jellyfish {
    fn paint_except(&self, area: Rect, buf: &mut Buffer, theme: &Theme, protected: &[Rect]) {
        let area = area.intersection(buf.area);
        if !self.motion.animates() || !self.brush.visible() || area.width < 7 || area.height < 5 {
            return;
        }
        let t = self.elapsed.as_millis();
        let compact = theme.ascii() && area.width < 56;
        let width = if compact { 3 } else { 5 };
        let wobble = sine_bob(t, 8_300, 2);
        let (x, y, dy) = if self.visitor {
            if area.height < 9 {
                return;
            }
            let cycle = t.saturating_add(3_100) % (JELLY_ROW_MS * JELLY_CYCLE_ROWS);
            if !self.hold_visit && cycle >= JELLY_ROW_MS * JELLY_VISIT_ROWS {
                return;
            }
            let cycle = cycle % (JELLY_ROW_MS * JELLY_VISIT_ROWS);
            let dots = i32::from(area.height) * 4 - 32 - (cycle * 4 / JELLY_ROW_MS) as i32;
            let x = (u32::from(area.width) * 5 / 6) as u16;
            (
                i32::from(x.min(area.width - width - 1)),
                dots.div_euclid(4),
                dots.rem_euclid(4) as usize,
            )
        } else {
            (
                i32::from((area.width - width) / 2),
                i32::from((area.height - 3) / 2),
                usize::from(sine_bob(t, 9_400, 3)),
            )
        };
        let Some(target) = local_rect(area, x, y, width, 3) else {
            return;
        };
        if target.x == area.x
            || target.y == area.y
            || target.right() == area.right()
            || target.bottom() == area.bottom()
            || !open_water(target, buf, protected)
        {
            return;
        }
        let bell = JELLY_GLOW.0 + (JELLY_GLOW.1 - JELLY_GLOW.0) * wave01(t, JELLY_PULSE_MS, 0);
        let arms = JELLY_GLOW.0
            + (JELLY_GLOW.1 - JELLY_GLOW.0)
                * wave01(t.saturating_sub(JELLY_ARM_LAG_MS), JELLY_PULSE_MS, 0);
        if self.brush.amount(bell) == 0.0 || self.brush.amount(arms) == 0.0 {
            return;
        }
        let rows: [&str; 3] = if theme.ascii() {
            let pulse = ((t % JELLY_PULSE_MS) * 2 / JELLY_PULSE_MS) as usize;
            let sway = ((t.saturating_add(3_100) / 1_100) % 4) as usize;
            let tops = if compact {
                [".-.", "'-'"]
            } else {
                [".-~-.", ".'-'."]
            };
            let skirts = if compact {
                ["\\_/", "(_)"]
            } else {
                ["\\___/", "(___)"]
            };
            let trails = if compact {
                ["| |", "/ \\", "| /", "\\ |"]
            } else {
                [" | | ", " / \\ ", " | / ", " \\ | "]
            };
            [tops[pulse], skirts[pulse], trails[sway]]
        } else {
            let pose = ((t % JELLY_PULSE_MS) * 8 / JELLY_PULSE_MS) as usize * 2;
            let rows = poses::jelly(pose, usize::from(wobble % 2), dy);
            [&rows[0], &rows[1], &rows[2]]
        };
        for (row, body) in rows.into_iter().enumerate() {
            let at = Rect {
                y: target.y + row as u16,
                height: 1,
                ..target
            };
            let (role, glow) = if row == 0 {
                (Role::Hint, bell)
            } else {
                (Role::Dim, arms)
            };
            self.brush.put(buf, at, body.chars(), theme, role, glow);
        }
    }
}

impl Paint for Jellyfish {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.paint_except(area, buf, theme, &[]);
    }
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        7
    }
}

const BUBBLE_RISE_ROWS: u128 = 5;
const BUBBLE_RISE_MS: u128 = 5_200;
const BUBBLE_GLOW: (f32, f32) = (0.34, 0.72);

/// One, two or three irregular bubble streams. Their short five-row ascent
/// stays near the floor, fades as it rises and gives way to prose and other
/// marine silhouettes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BubbleField {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub density: HabitatDensity,
    brush: Brush,
}

impl BubbleField {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            density: HabitatDensity::Auto,
            brush: Brush {
                ink: None,
                presence: 1_000,
            },
        }
    }

    #[must_use]
    pub const fn density(mut self, density: HabitatDensity) -> Self {
        self.density = density;
        self
    }

    brush_builders!();
}

impl BubbleField {
    fn paint_except(&self, area: Rect, buf: &mut Buffer, theme: &Theme, protected: &[Rect]) {
        let area = area.intersection(buf.area);
        if !self.motion.animates() || !self.brush.visible() || area.width < 3 || area.height < 3 {
            return;
        }
        let t = self.elapsed.as_millis();
        for stream in 0..self.density.for_area(area).bubbles() {
            let seed = stream as u128;
            let phase = jitter(seed) % 9_000;
            let period = BUBBLE_RISE_MS + jitter(seed + 313) % 2_600;
            let rise = (t.saturating_add(phase) % period) * BUBBLE_RISE_ROWS / period;
            let lane = (jitter(seed + 977) % u128::from(area.width)) as i32;
            let drift = i32::from(sine_bob(t.saturating_add(phase), 2_100, 2)) - 1;
            let x = (lane + drift).clamp(0, i32::from(area.width) - 1);
            let y = (i32::from(area.height) - 2 - rise as i32).max(0);
            let Some(at) = local_rect(area, x, y, 1, 1) else {
                continue;
            };
            if !open_water(at, buf, protected) {
                continue;
            }
            let glyph = match (theme.ascii(), rise) {
                (true, 0..=1) => ".",
                (true, 2..=3) => "o",
                (true, _) => "O",
                (false, 0..=1) => "·",
                (false, 2..=3) => "˚",
                (false, _) => "°",
            };
            let fade = 1.0 - 0.75 * rise as f32 / BUBBLE_RISE_ROWS as f32;
            let glow = glint(t, 2_600 + phase % 700, 600, BUBBLE_GLOW.0, phase) * BUBBLE_GLOW.1;
            self.brush
                .put(buf, at, glyph.chars(), theme, Role::Dim, glow * fade);
        }
    }
}

impl Paint for BubbleField {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.paint_except(area, buf, theme, &[]);
    }
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        8
    }
}

const SNOW_DOT_MS: u128 = 1_700;
const SNOW_GLOW: (f32, f32) = (0.10, 0.30);

/// Marine snow: single braille dots drifting down and sideways through open
/// water, one dot step at a time. Native braille only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarineSnow {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub density: HabitatDensity,
    brush: Brush,
}

impl MarineSnow {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            density: HabitatDensity::Auto,
            brush: Brush {
                ink: None,
                presence: 1_000,
            },
        }
    }

    #[must_use]
    pub const fn density(mut self, density: HabitatDensity) -> Self {
        self.density = density;
        self
    }

    brush_builders!();
}

impl MarineSnow {
    fn paint_except(&self, area: Rect, buf: &mut Buffer, theme: &Theme, protected: &[Rect]) {
        let area = area.intersection(buf.area);
        if !self.motion.animates() || !self.brush.visible() || theme.ascii() || area.is_empty() {
            return;
        }
        let t = self.elapsed.as_millis();
        let width_dots = i64::from(area.width) * 2;
        let span = u128::from(area.height) * 4;
        for mote in 0..self.density.for_area(area).motes() {
            let seed = mote as u128;
            let fall_ms = SNOW_DOT_MS + jitter(seed + 401) % 900;
            let y_dots = (t.saturating_add(jitter(seed + 19) % (span * fall_ms)) / fall_ms) % span;
            let drift_ms = 7_000 + jitter(seed + 59) % 5_000;
            let sway_ms = 5_600.0 + (jitter(seed + 83) % 3_100) as f64;
            let sway = ((t as f64 / sway_ms) * std::f64::consts::TAU).sin() * 1.4;
            let base = (jitter(seed + 733) % width_dots as u128) as i64;
            let x_dots =
                (base + (t / drift_ms) as i64 + sway.round() as i64).rem_euclid(width_dots);
            let Some(at) = local_rect(area, (x_dots / 2) as i32, (y_dots / 4) as i32, 1, 1) else {
                continue;
            };
            if !open_water(at, buf, protected) {
                continue;
            }
            let glow = SNOW_GLOW.0
                + (SNOW_GLOW.1 - SNOW_GLOW.0)
                    * wave01(t, 6_700 + jitter(seed + 7) % 4_000, seed * 911);
            let glyph = poses::mote((x_dots % 2) as usize, (y_dots % 4) as usize);
            self.brush
                .put(buf, at, glyph.chars(), theme, Role::Dim, glow);
        }
    }
}

impl Paint for MarineSnow {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.paint_except(area, buf, theme, &[]);
    }
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        8
    }
}

const WHALE_CELL_MS: u128 = 900;
const WHALE_CYCLE_MS: u128 = 210_000;
const WHALE_ABSENCE_MS: u128 = 60_000;
const WHALE_FIRST_MS: u128 = 12_000;
const WHALE_COLS: i32 = 10;
const WHALE_FLOOR_GAP: u16 = 8;
const WHALE_FLUKES: [usize; 6] = [0, 1, 3, 2, 3, 1];
const WHALE_GLOW: (f32, f32) = (0.16, 0.28);

/// A distant whale that glides across the deep water every few minutes,
/// first about twelve seconds in, sliding in and out at the edges. Native
/// braille only. `hold_visit(true)` keeps it crossing for a preview.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DistantWhale {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub hold_visit: bool,
    brush: Brush,
}

impl DistantWhale {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            hold_visit: false,
            brush: Brush {
                ink: None,
                presence: 1_000,
            },
        }
    }

    #[must_use]
    pub const fn hold_visit(mut self, hold: bool) -> Self {
        self.hold_visit = hold;
        self
    }

    brush_builders!();

    /// Whether the whale is crossing `area` at this moment, before any
    /// collision check. A habitat keeps an unheld jellyfish away while the
    /// whale is painted.
    #[must_use]
    pub fn visiting(&self, area: Rect, theme: &Theme) -> bool {
        self.crossing(area, theme).is_some()
    }

    fn glow_at(&self) -> f32 {
        WHALE_GLOW.0 + (WHALE_GLOW.1 - WHALE_GLOW.0) * wave01(self.elapsed.as_millis(), 8_200, 0)
    }

    fn crossing(&self, area: Rect, theme: &Theme) -> Option<(bool, i32)> {
        if !self.motion.animates()
            || theme.ascii()
            || area.width == 0
            || area.height < WHALE_FLOOR_GAP + 2
            || self.brush.amount(self.glow_at()) == 0.0
        {
            return None;
        }
        let t = self.elapsed.as_millis();
        let crossing_ms = (u128::from(area.width) + WHALE_COLS as u128 + 2) * WHALE_CELL_MS;
        let cycle = WHALE_CYCLE_MS.max(crossing_ms.saturating_add(WHALE_ABSENCE_MS));
        let clock = t.saturating_add(cycle - WHALE_FIRST_MS.min(cycle));
        let period = if self.hold_visit { crossing_ms } else { cycle };
        let pos = clock % period;
        if pos >= crossing_ms {
            return None;
        }
        let right = swims_right((clock / period).wrapping_add(7));
        let step = (pos * 2 / WHALE_CELL_MS) as i32;
        let dot_x = if right {
            step - WHALE_COLS * 2
        } else {
            i32::from(area.width) * 2 - step
        };
        Some((right, dot_x))
    }
}

impl DistantWhale {
    fn paint_except(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        protected: &[Rect],
    ) -> bool {
        let area = area.intersection(buf.area);
        let Some((right, dot_x)) = self.crossing(area, theme) else {
            return false;
        };
        let t = self.elapsed.as_millis();
        let dot_y = i32::from(area.height - WHALE_FLOOR_GAP) * 4 + i32::from(sine_bob(t, 6_400, 3));
        let pose = WHALE_FLUKES[((t / 1_100) % WHALE_FLUKES.len() as u128) as usize];
        let rows = poses::whale(
            right,
            pose,
            dot_x.rem_euclid(2) as usize,
            dot_y.rem_euclid(4) as usize,
        );
        let left = dot_x.div_euclid(2);
        let skip = usize::try_from(-left).unwrap_or(0);
        let x = left.max(0);
        let take = ((i32::from(area.width) - x).clamp(0, WHALE_COLS) as usize)
            .min((WHALE_COLS as usize).saturating_sub(skip));
        let Some(target) = local_rect(area, x, dot_y.div_euclid(4), take as u16, 3) else {
            return false;
        };
        if take == 0 || !open_water(target, buf, protected) {
            return false;
        }
        let glow = self.glow_at();
        let mut painted = false;
        for (row, glyph) in rows.iter().enumerate() {
            let at = Rect {
                y: target.y + row as u16,
                height: 1,
                ..target
            };
            painted |= self
                .brush
                .put(buf, at, glyph.chars().skip(skip), theme, Role::Dim, glow);
        }
        painted
    }
}

impl Paint for DistantWhale {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.paint_except(area, buf, theme, &[]);
    }
    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        10
    }
}

/// The whole ocean, painted over the host's existing content. The distant
/// whale is placed first and keeps the jellyfish away while it crosses; the
/// school owns the floor band, the shoal mid-water, and bubbles and marine
/// snow take whatever open water remains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Habitat {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub density: HabitatDensity,
    pub fish: bool,
    pub shoal: bool,
    pub jellyfish: bool,
    pub bubbles: bool,
    pub snow: bool,
    pub whale: bool,
    pub hold_jellyfish_visit: bool,
    /// Host-owned overlays and other areas that must remain undisturbed,
    /// including their blank cells. Coordinates are absolute buffer positions.
    pub protected: Vec<Rect>,
    brush: Brush,
}

impl Habitat {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            density: HabitatDensity::Auto,
            fish: true,
            shoal: true,
            jellyfish: true,
            bubbles: true,
            snow: true,
            whale: true,
            hold_jellyfish_visit: false,
            protected: Vec::new(),
            brush: Brush {
                ink: None,
                presence: 1_000,
            },
        }
    }

    #[must_use]
    pub const fn density(mut self, density: HabitatDensity) -> Self {
        self.density = density;
        self
    }
    #[must_use]
    pub const fn fish(mut self, show: bool) -> Self {
        self.fish = show;
        self
    }
    #[must_use]
    pub const fn shoal(mut self, show: bool) -> Self {
        self.shoal = show;
        self
    }
    #[must_use]
    pub const fn jellyfish(mut self, show: bool) -> Self {
        self.jellyfish = show;
        self
    }
    #[must_use]
    pub const fn bubbles(mut self, show: bool) -> Self {
        self.bubbles = show;
        self
    }
    #[must_use]
    pub const fn snow(mut self, show: bool) -> Self {
        self.snow = show;
        self
    }
    #[must_use]
    pub const fn whale(mut self, show: bool) -> Self {
        self.whale = show;
        self
    }

    brush_builders!();

    /// Protect filled blank overlays, focus surfaces or other host-owned
    /// regions. A creature whose clearance touches a region is withheld as a
    /// complete silhouette.
    #[must_use]
    pub fn protected(mut self, areas: impl Into<Vec<Rect>>) -> Self {
        self.protected = areas.into();
        self
    }

    #[must_use]
    pub fn exclude_rect(mut self, area: Rect) -> Self {
        self.protected.push(area);
        self
    }

    /// Keep the corner visitor present for a preview or dedicated marine
    /// surface, even while the distant whale crosses. False (the default)
    /// retains its long absence between visits. Full-silhouette collision
    /// checks still give all existing text priority.
    #[must_use]
    pub const fn hold_jellyfish_visit(mut self, hold: bool) -> Self {
        self.hold_jellyfish_visit = hold;
        self
    }
}

impl Paint for Habitat {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let (t, motion, density, brush) = (self.elapsed, self.motion, self.density, self.brush);
        let protected = &self.protected;
        let whale_painted = self.whale
            && DistantWhale {
                brush,
                ..DistantWhale::new(t, motion)
            }
            .paint_except(area, buf, theme, protected);
        if self.jellyfish && (self.hold_jellyfish_visit || !whale_painted) {
            Jellyfish {
                brush,
                ..Jellyfish::new(t, motion)
                    .visitor(true)
                    .hold_visit(self.hold_jellyfish_visit)
            }
            .paint_except(area, buf, theme, protected);
        }
        if self.fish {
            FishSchool {
                brush,
                ..FishSchool::new(t, motion).density(density)
            }
            .paint_except(area, buf, theme, protected);
        }
        if self.shoal {
            Shoal {
                brush,
                ..Shoal::new(t, motion).density(density)
            }
            .paint_except(area, buf, theme, protected);
        }
        if self.bubbles {
            BubbleField {
                brush,
                ..BubbleField::new(t, motion).density(density)
            }
            .paint_except(area, buf, theme, protected);
        }
        if self.snow {
            MarineSnow {
                brush,
                ..MarineSnow::new(t, motion).density(density)
            }
            .paint_except(area, buf, theme, protected);
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        16
    }
}

// Signed local coordinates make off-screen travel and saturated absolute
// buffer edges ordinary clipping, rather than u16 wraparound.
fn local_rect(area: Rect, x: i32, y: i32, width: u16, height: u16) -> Option<Rect> {
    if x < 0
        || y < 0
        || x + i32::from(width) > i32::from(area.width)
        || y + i32::from(height) > i32::from(area.height)
    {
        return None;
    }
    Some(Rect::new(
        area.x + x as u16,
        area.y + y as u16,
        width,
        height,
    ))
}

fn open_water(at: Rect, buf: &Buffer, protected: &[Rect]) -> bool {
    let x0 = at.x.saturating_sub(CLEAR_COLS).max(buf.area.x);
    let y0 = at.y.saturating_sub(CLEAR_ROWS).max(buf.area.y);
    let x1 = at.right().saturating_add(CLEAR_COLS).min(buf.area.right());
    let y1 = at
        .bottom()
        .saturating_add(CLEAR_ROWS)
        .min(buf.area.bottom());
    let clearance = Rect::new(x0, y0, x1 - x0, y1 - y0);
    if protected
        .iter()
        .any(|r| !clearance.intersection(*r).is_empty())
    {
        return false;
    }
    (y0..y1).all(|y| {
        if x0 > buf.area.x && crate::text::width(buf[(x0 - 1, y)].symbol()) > 1 {
            return false;
        }
        (x0..x1).all(|x| {
            let cell = &buf[(x, y)];
            cell.diff_option != CellDiffOption::Skip
                && cell.symbol().chars().all(char::is_whitespace)
        })
    })
}

fn swims_right(cycle: u128) -> bool {
    (cycle.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 7) & 1 == 0
}

// Engine's deterministic phase spread; no RNG and no animation state.
fn jitter(seed: u128) -> u128 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in seed.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    u128::from(hash)
}

fn sine_bob(elapsed: u128, period: u128, amplitude: u16) -> u16 {
    let fraction = (elapsed % period) as f64 / period as f64;
    (((fraction * std::f64::consts::TAU).sin() + 1.0) * 0.5 * f64::from(amplitude)).round() as u16
}

fn wave01(elapsed: u128, period: u128, phase: u128) -> f32 {
    let fraction = (elapsed.saturating_add(phase) % period) as f64 / period as f64;
    let s = (fraction * std::f64::consts::PI).sin();
    (s * s) as f32
}

fn glint(elapsed: u128, period: u128, width: u128, floor: f32, phase: u128) -> f32 {
    let pos = elapsed.saturating_add(phase) % period;
    if pos >= width {
        return floor;
    }
    let bump = 0.5 * (1.0 - (pos as f64 / width as f64 * std::f64::consts::TAU).cos());
    floor + (1.0 - floor) * bump as f32
}

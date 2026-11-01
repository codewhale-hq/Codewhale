//! The native marine habitat: a small school, one jellyfish and rising bubbles.
//!
//! Travel and authored dot poses are adapted from Codewhale Engine's
//! `ambient_life.rs` at `a79ce5c4d`; the pose module carries its MIT notice.
//! These components own no clock, simulation, input handler or runtime state.
//! The host supplies elapsed time and motion policy. Reduced and still motion
//! remove ambient life, as the Engine does; a host may separately paint its
//! existing [`crate::Whale`] in the host's reported state.
//!
//! Paint content first, then habitat. Every creature requires clear water
//! including a row and column of clearance around its full silhouette. No
//! component clears its area or repaints existing text. Filled blank overlays
//! need explicit protection through [`Habitat::protected`]. Painting into a fresh
//! buffer each frame lets creatures move without leaving trails.

use std::time::Duration;

use ratatui::{buffer::Buffer, layout::Rect};

use crate::{MotionMode, Paint, Role, Theme};

#[path = "habitat_poses.rs"]
mod poses;

/// A bounded marine population. Auto follows the Engine's terminal tiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HabitatDensity {
    #[default]
    Auto,
    /// Three fish, two bubble streams.
    Sparse,
    /// Five fish, four bubble streams.
    Normal,
    /// Seven fish, six bubble streams. There is still at most one jellyfish.
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
            Self::Rich | Self::Auto => 7,
        }
    }

    fn bubbles(self) -> usize {
        match self {
            Self::Sparse => 2,
            Self::Normal => 4,
            Self::Rich | Self::Auto => 6,
        }
    }
}

const WEDGE: &[(i32, u16)] = &[(0, 0), (-1, 4), (1, 6), (-1, 9), (1, 11), (0, 14), (-1, 17)];
const SCHOOL_CELL_MS: u128 = 380;

/// One loose school near the floor, with four native tail poses and half-cell
/// travel. ASCII terminals retain the Engine's eyed lead and smaller members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FishSchool {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub density: HabitatDensity,
}

impl FishSchool {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            density: HabitatDensity::Auto,
        }
    }

    #[must_use]
    pub const fn density(mut self, density: HabitatDensity) -> Self {
        self.density = density;
        self
    }
}

impl FishSchool {
    fn paint_except(&self, area: Rect, buf: &mut Buffer, theme: &Theme, protected: &[Rect]) {
        let area = area.intersection(buf.area);
        if !self.motion.animates() || area.width < 4 || area.height < 4 {
            return;
        }
        let count = self.density.for_area(area).fish();
        let span = WEDGE[count - 1].1 + 4;
        let travel = u128::from(area.width) + u128::from(span);
        let cycle_ms = travel * SCHOOL_CELL_MS;
        let t = self.elapsed.as_millis();
        // A fresh scene starts mid-crossing, rather than with an empty ocean.
        let clock = t.saturating_add(cycle_ms / 2);
        let right = ((clock / cycle_ms).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 7) & 1 == 0;
        let step = ((clock % cycle_ms) * travel * 2 / cycle_ms) as i32;
        let mut marks = Vec::with_capacity(count);
        for (member, &(dy, behind)) in WEDGE.iter().take(count).enumerate() {
            let bob = sine_bob(t, 3_000 + jitter(member as u128 + 41) % 2_600, 1);
            let body = if theme.ascii() {
                match (right, member == 0) {
                    (true, true) => "><o>",
                    (true, false) => "><>",
                    (false, true) => "<o><",
                    (false, false) => "<><",
                }
            } else {
                let drift =
                    i32::from(sine_bob(t, 5_200 + jitter(member as u128 + 617) % 3_400, 2)) - 1;
                let dot_x = if right {
                    step - i32::from(behind) * 2 - 8 + drift
                } else {
                    i32::from(area.width) * 2 - step + i32::from(behind) * 2 - drift
                };
                poses::fish(
                    right,
                    ((t / 300 + member as u128) % 4) as usize,
                    dot_x.rem_euclid(2) as usize,
                    usize::from(bob),
                )
            };
            let width = if theme.ascii() { body.len() as u16 } else { 4 };
            let x = if theme.ascii() {
                if right {
                    step / 2 - i32::from(behind) - i32::from(width)
                } else {
                    i32::from(area.width) - step / 2 + i32::from(behind)
                }
            } else {
                let drift =
                    i32::from(sine_bob(t, 5_200 + jitter(member as u128 + 617) % 3_400, 2)) - 1;
                let dot_x = if right {
                    step - i32::from(behind) * 2 - 8 + drift
                } else {
                    i32::from(area.width) * 2 - step + i32::from(behind) * 2 - drift
                };
                dot_x.div_euclid(2)
            };
            let y =
                i32::from(area.height) - 3 + dy + if theme.ascii() { i32::from(bob) } else { 0 };
            let Some(at) = local_rect(area, x, y, width, 1) else {
                continue;
            };
            if open_water(at, buf, protected) {
                let role = if member == 0 {
                    Role::Accent
                } else {
                    Role::Hint
                };
                marks.push((at, body, role));
            }
        }
        // All school members see the host's original text, rather than
        // mistaking a neighbouring member of their wedge for a word.
        for (at, body, role) in marks {
            buf.set_stringn(at.x, at.y, body, usize::from(at.width), theme.fg(role));
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
}

impl Jellyfish {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            visitor: false,
            hold_visit: false,
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
}

impl Jellyfish {
    fn paint_except(&self, area: Rect, buf: &mut Buffer, theme: &Theme, protected: &[Rect]) {
        let area = area.intersection(buf.area);
        if !self.motion.animates() || area.width < 7 || area.height < 5 {
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
            const ROW_MS: u128 = 9_400;
            let cycle = t.saturating_add(3_100) % (ROW_MS * 32);
            if !self.hold_visit && cycle >= ROW_MS * 6 {
                return;
            }
            let cycle = cycle % (ROW_MS * 6);
            let dots = i32::from(area.height) * 4 - 32 - (cycle * 4 / ROW_MS) as i32;
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
        // Whole-silhouette clearance, including blank cells between arms.
        // Withhold rather than splitting the bell or dodging around live text.
        if target.x == area.x
            || target.y == area.y
            || target.right() == area.right()
            || target.bottom() == area.bottom()
            || !open_water(target, buf, protected)
        {
            return;
        }
        if theme.ascii() {
            let pulse = ((t % 5_200) * 2 / 5_200) as usize;
            let arms = ((t.saturating_add(3_100) / 1_100) % 4) as usize;
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
            for (row, body) in [tops[pulse], skirts[pulse], trails[arms]]
                .into_iter()
                .enumerate()
            {
                buf.set_stringn(
                    target.x,
                    target.y + row as u16,
                    body,
                    usize::from(width),
                    theme.fg(Role::Hint),
                );
            }
        } else {
            let pose = ((t % 5_200) * 16 / 5_200) as usize;
            for (row, body) in poses::jelly(pose, usize::from(wobble % 2), dy)
                .iter()
                .enumerate()
            {
                let role = if row == 0 { Role::Hint } else { Role::Dim };
                buf.set_stringn(
                    target.x,
                    target.y + row as u16,
                    body,
                    usize::from(width),
                    theme.fg(role),
                );
            }
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

/// Two, four or six irregular bubble streams. Their short five-row ascent
/// stays near the floor and gives way to prose and other marine silhouettes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BubbleField {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub density: HabitatDensity,
}

impl BubbleField {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            density: HabitatDensity::Auto,
        }
    }

    #[must_use]
    pub const fn density(mut self, density: HabitatDensity) -> Self {
        self.density = density;
        self
    }
}

impl BubbleField {
    fn paint_except(&self, area: Rect, buf: &mut Buffer, theme: &Theme, protected: &[Rect]) {
        let area = area.intersection(buf.area);
        if !self.motion.animates() || area.width < 3 || area.height < 3 {
            return;
        }
        let t = self.elapsed.as_millis();
        for stream in 0..self.density.for_area(area).bubbles() {
            let seed = stream as u128;
            let phase = jitter(seed) % 9_000;
            let period = 3_200 + jitter(seed + 313) % 2_600;
            let rise = (t.saturating_add(phase) % period) * 5 / period;
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
            buf.set_stringn(at.x, at.y, glyph, 1, theme.fg(Role::Dim));
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

/// The three marine layers, painted over the host's existing content. The
/// school owns the lower band, one occasional jellyfish visits above it,
/// and bubbles occupy whatever clear water remains. No new whale is created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Habitat {
    pub elapsed: Duration,
    pub motion: MotionMode,
    pub density: HabitatDensity,
    pub fish: bool,
    pub jellyfish: bool,
    pub bubbles: bool,
    pub hold_jellyfish_visit: bool,
    /// Host-owned overlays and other areas that must remain undisturbed,
    /// including their blank cells. Coordinates are absolute buffer positions.
    pub protected: Vec<Rect>,
}

impl Habitat {
    #[must_use]
    pub const fn new(elapsed: Duration, motion: MotionMode) -> Self {
        Self {
            elapsed,
            motion,
            density: HabitatDensity::Auto,
            fish: true,
            jellyfish: true,
            bubbles: true,
            hold_jellyfish_visit: false,
            protected: Vec::new(),
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
    pub const fn jellyfish(mut self, show: bool) -> Self {
        self.jellyfish = show;
        self
    }
    #[must_use]
    pub const fn bubbles(mut self, show: bool) -> Self {
        self.bubbles = show;
        self
    }
    /// Protect filled blank overlays, focus surfaces or other host-owned
    /// regions. A creature that touches a region's one-cell clearance is
    /// withheld as a complete silhouette.
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
    /// surface. False (the default) retains its long absence between visits.
    /// Full-silhouette collision checks still give all existing text priority.
    #[must_use]
    pub const fn hold_jellyfish_visit(mut self, hold: bool) -> Self {
        self.hold_jellyfish_visit = hold;
        self
    }
}

impl Paint for Habitat {
    fn paint(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        if self.fish {
            FishSchool::new(self.elapsed, self.motion)
                .density(self.density)
                .paint_except(area, buf, theme, &self.protected);
        }
        if self.jellyfish {
            Jellyfish::new(self.elapsed, self.motion)
                .visitor(true)
                .hold_visit(self.hold_jellyfish_visit)
                .paint_except(area, buf, theme, &self.protected);
        }
        if self.bubbles {
            BubbleField::new(self.elapsed, self.motion)
                .density(self.density)
                .paint_except(area, buf, theme, &self.protected);
        }
    }

    fn height(&self, _width: u16, _theme: &Theme) -> u16 {
        16
    }
}

// Signed local coordinates make off-screen school travel and saturated
// absolute buffer edges ordinary clipping, rather than u16 wraparound.
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
    let x0 = at.x.saturating_sub(1).max(buf.area.x);
    let y0 = at.y.saturating_sub(1).max(buf.area.y);
    let x1 = at.right().saturating_add(1).min(buf.area.right());
    let y1 = at.bottom().saturating_add(1).min(buf.area.bottom());
    let clearance = Rect::new(x0, y0, x1 - x0, y1 - y0);
    if protected
        .iter()
        .any(|r| !clearance.intersection(*r).is_empty())
    {
        return false;
    }
    (y0..y1).all(|y| (x0..x1).all(|x| buf[(x, y)].symbol().chars().all(char::is_whitespace)))
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

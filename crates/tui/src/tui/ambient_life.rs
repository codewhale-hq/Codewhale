//! Ambient ocean life for the underwater transcript field.
//!
//! One clear owner for the fish school, jellyfish, bubbles, and the rare
//! whale cameo — nothing else lives in the water (2026-07-23 product
//! decision: seaweed and bio-dust are gone). Motion stays inside the
//! existing delta/interpolation path: this module never requests frames on
//! its own.
//!
//! Native silhouettes use the shared 2×4 braille cell: fish move in half
//! columns with a one-dot bob; jellyfish rise in quarter rows. A bounded pose
//! table owns no clock or simulation. ASCII-safe terminals retain the original
//! silhouettes through the same habitat, population and collision path.
//!
//! Motion language (shared with the rest of the shell): every mark can lerp
//! between the water and its ink at a time-varying brightness. Fish carry a
//! travelling sin² wave, jellyfish a slow band-bounded pulse that opens and
//! closes the dome while the tentacles trail it by ~0.6 s, bubbles an
//! occasional raised-cosine glint. Phases are wall-clock keyed and entity
//! periods deliberately never match, so nothing strobes in sync.
//!
//! The jellyfish is a *visitor*, not scenery: one at most, present for roughly
//! a fifth of a ~5-minute cycle and dimmer than everything around it. See the
//! `JELLY_VISIT_*` constants for the rarity knobs and why they are set where
//! they are.
//!
//! Fish swim on a wrap-around path: they exit one edge and re-enter the
//! other still facing their travel direction, so facing always equals
//! velocity by construction. Direction may only change while the school is
//! fully off-screen.
//!
//! The aquarium has a habitat and it defers to whatever is composed above it.
//! Collision is one rule — [`is_open_water`]: a mark may only land in a
//! horizontal span that carries no text and has none within
//! [`TEXT_CLEARANCE_ROWS`] of it, measured off the rendered lines rather than
//! guessed from fractions of the field. Everything else follows from it. A
//! short status line therefore leaves honest water beside it instead of
//! claiming the whole row. The school rides a band off the
//! floor ([`SCHOOL_FLOOR_GAP`]); bubbles rise a few rows from the floor and
//! dissolve ([`BUBBLE_MAX_RISE_ROWS`]); the jellyfish only surfaces where
//! [`deep_water_rows`] says the water is deep enough to hold it *and* the
//! school; and the surface caustics stop at the first row of the composition.
//! Light above, life below, words in between — and as a transcript fills the
//! field the water closes row by row until nothing moves behind the text the
//! reader is actually reading.
//!
//! Two clocks feed this module and neither is a token counter. Positions ride
//! `App::sample_ambient_clock_ms`, which advances by real elapsed time clamped
//! to `App::AMBIENT_MAX_STEP_MS` per draw, so drift speed is identical at 16 ms
//! and 33 ms frames and a stalled-then-resumed frame cannot jump a creature.
//! Sideways *placement*, by contrast, is a function of the transcript text
//! under the silhouette — which does change with token throughput — so it is
//! bounded by [`JELLY_MAX_TEXT_DODGE_COLS`].
//!
//! Under reduced motion there is no ambient life at all: `ocean::life_presence`
//! returns 0 and rendering exits before building marks or initializing pet
//! tapes. Reduced motion spends no simulation work on invisible creatures.
//!
//! `render_ambient_life` returns per-frame budget counters
//! ([`AmbientFrameStats`]): marks built always splits exactly into painted +
//! text-skipped + clipped. Counting is a handful of `u32` increments — no
//! allocation, no frame requests.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::ocean::{self, OceanColumn};

#[path = "ambient_life/native_poses.rs"]
mod native_poses;
#[path = "ambient_life/pet_cameo.rs"]
mod pet_cameo;
#[path = "ambient_life/pet_sim.rs"]
pub mod pet_sim;
#[path = "ambient_life/pet_widget.rs"]
pub mod pet_widget;

/// Depth layers for parallax. Nearer life is larger, faster, and more visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Depth {
    Background,
    Midground,
    Foreground,
}

impl Depth {
    #[must_use]
    fn ink_index(self) -> usize {
        match self {
            Self::Background => 1,
            Self::Midground | Self::Foreground => 0,
        }
    }
}

/// Creature density tier mirrored from shell width/height.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifeDensity {
    Sparse,
    Normal,
    Rich,
}

impl LifeDensity {
    #[must_use]
    pub fn from_area(area: Rect) -> Self {
        if area.width < 56 || area.height < 12 {
            Self::Sparse
        } else if area.width < 88 || area.height < 20 {
            Self::Normal
        } else {
            Self::Rich
        }
    }

    #[must_use]
    fn school_size(self) -> usize {
        // One loose wedge of real fish; two schools compete with the whale.
        match self {
            Self::Sparse => 3,
            Self::Normal => 5,
            Self::Rich => 7,
        }
    }

    #[must_use]
    fn jellyfish_count(self) -> usize {
        // At most one jellyfish in the water at a time, at every tier. Two
        // put a pulsing silhouette in *both* side lanes, which is what made
        // them read as resident scenery instead of a passing visitor. The
        // rarity knob that matters is the visit duty cycle
        // ([`JELLY_VISIT_CYCLE_SLOTS`]), not the population.
        match self {
            Self::Sparse | Self::Normal | Self::Rich => 1,
        }
    }

    #[must_use]
    fn bubble_streams(self) -> usize {
        // Raised from 1/2/2 (founder, "screw the cap … more alive more
        // ocean"). Bubbles are the cheapest life in the field: one mark
        // each, no silhouette to degrade, and `water()` already refuses any
        // column the composition has claimed, so a denser field thins itself
        // automatically as a transcript fills.
        match self {
            Self::Sparse => 2,
            Self::Normal => 4,
            Self::Rich => 6,
        }
    }
}

/// Lower floors so smaller windows still retain some life (was 68×15).
/// Keep in sync with [`crate::tui::ocean::AMBIENT_MIN_WIDTH`].
pub const AMBIENT_MIN_WIDTH: u16 = crate::tui::ocean::AMBIENT_MIN_WIDTH;
pub const AMBIENT_MIN_HEIGHT: u16 = crate::tui::ocean::AMBIENT_MIN_HEIGHT;

/// Snapshot of ambient positions for one frame (memoized once per draw).
#[derive(Debug, Clone)]
struct FrameMarks {
    marks: Vec<AmbientMark>,
}

#[derive(Debug, Clone, Copy)]
struct AmbientMark {
    x: u16,
    y: u16,
    glyph: &'static str,
    /// Multi-row creature identity. Every part relocates or is withheld as one
    /// unit so a jellyfish never degrades into a detached dome or tentacles.
    jellyfish: Option<usize>,
    depth: Depth,
    style_mod: Option<Modifier>,
    /// Time-varying glow in `[0, 1]`: the mark's ink is lerped from the
    /// painted water toward full ink at this amount. `None` renders the
    /// plain habitat ink.
    brightness: Option<f32>,
}

/// Per-frame render budget counters. `marks_built` splits exactly into
/// `marks_painted + marks_skipped_text + marks_clipped`. `cells_written`
/// counts individual cell writes: a multi-cell glyph counts each of its
/// cells, and two overlapping marks count the shared cell once per write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AmbientFrameStats {
    pub marks_built: u32,
    pub marks_painted: u32,
    pub marks_skipped_text: u32,
    pub marks_clipped: u32,
    pub cells_written: u32,
}

/// Bounded school (7), jellyfish (4), bubbles (6), plus at most three
/// 18-by-6 dot-whale widgets including their labels. No particle allocations
/// or simulation steps occur per paint after the fixed cameo tapes are cached.
#[cfg(test)]
pub const MAX_FRAME_MARKS: u32 = 17 + pet_cameo::MAX_MARKS;

/// Optional pointer reaction for fish dart / bubble rise.
#[derive(Debug, Clone, Copy, Default)]
pub struct AmbientCursor {
    pub column: u16,
    pub row: u16,
    /// When set, fish flee from this point for ~800 ms of shared ocean clock.
    pub flee_elapsed_ms: Option<u128>,
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

/// Render ambient life into empty water cells of `area`.
///
/// Returns per-frame budget counters for tests and debug tooling; the
/// counting itself is a few `u32` increments, never an allocation.
#[allow(clippy::too_many_arguments)]
pub fn render_ambient_life(
    area: Rect,
    buf: &mut Buffer,
    inks: (Color, Color),
    lines: &[Line<'static>],
    elapsed_ms: u128,
    presence: f32,
    cursor: AmbientCursor,
    whale: WhaleCameo,
    activity: AmbientActivity,
) -> AmbientFrameStats {
    if area.width < AMBIENT_MIN_WIDTH
        || area.height < AMBIENT_MIN_HEIGHT
        || !presence.is_finite()
        || presence <= 0.0
    {
        return AmbientFrameStats::default();
    }

    // Geometry always samples the same clock. Scaling its absolute age when
    // activity changes teleports the scene; activity already owns ink/cameos.
    let density = LifeDensity::from_area(area);
    let mut stats = AmbientFrameStats::default();
    // Positions always ride the live monotonic clock; `presence` fades the
    // marks in and out, so the animated/static boundary eases instead of
    // snapping fish between t=0 and their mid-path positions.
    let frame = build_frame_marks(
        area,
        elapsed_ms,
        density,
        lines,
        cursor,
        crate::tui::color_compat::ascii_safe_enabled(),
        &mut stats,
    );
    paint_marks(area, buf, inks, lines, &frame, presence, &mut stats);
    pet_cameo::paint(
        area, buf, inks.0, lines, presence, whale, activity, &mut stats,
    );
    stats
}

#[allow(clippy::too_many_arguments)]
fn build_frame_marks(
    area: Rect,
    elapsed_ms: u128,
    density: LifeDensity,
    lines: &[Line<'static>],
    cursor: AmbientCursor,
    ascii_safe: bool,
    stats: &mut AmbientFrameStats,
) -> FrameMarks {
    let mut marks = Vec::with_capacity(48);
    let t = elapsed_ms;

    // Where the water is. The old rule was a guess at where the composition
    // sat — fifths of the field — and it was wrong on every real screen: at
    // 80×24 it reserved two rows in the middle of the field while the
    // wordmark, caption, and invitation lived three rows lower, so a fish
    // surfaced in the one-row gap between the caption and the invitation.
    // Now the field is measured, not guessed: [`is_open_water`] asks the
    // rendered lines directly.
    let water = |x: u16, y: u16, width: u16| is_open_water(lines, x, y, width);

    // --- One loose fish school along the floor ---
    // The school enters one edge, crosses, and exits the other; direction
    // may only change while it is fully off-screen, so facing always equals
    // velocity. A travelling sin² brightness wave runs through the wedge.
    let school_size = density.school_size().min(SCHOOL_WEDGE.len());
    let school_span = SCHOOL_WEDGE
        .iter()
        .take(school_size)
        .map(|(_, dx)| *dx)
        .max()
        .unwrap_or(0)
        .saturating_add(LEAD_FISH_RIGHT.len() as u16);
    let travel = u128::from(area.width.saturating_add(school_span).max(1));
    let cycle_ms = travel.saturating_mul(SCHOOL_CELL_MS);
    // Half-cycle head start: freshly opened water shows the school
    // mid-crossing instead of an empty entry beat.
    let school_clock = t.saturating_add(cycle_ms / 2);
    let cycle_index = school_clock / cycle_ms;
    let cycle_frac = (school_clock % cycle_ms) as f64 / cycle_ms as f64;
    let cycle_step = (cycle_frac * travel as f64).round() as i32;
    let cycle_dot_step = (cycle_frac * travel as f64 * 2.0).floor() as i32;
    let swims_right = school_swims_right(cycle_index);
    // The school has one home: the deep water just off the floor. It used to
    // alternate between an upper and a lower band, which is most of why the
    // aquarium read as decoration sprinkled over the whole field instead of
    // as depth beneath it. Direction still alternates — that is the part a
    // viewer reads as "the fish came back" — but the band does not.
    let anchor_y = school_band_row(area);
    let ptr = cursor.column.saturating_sub(area.x);
    let ptr_y = cursor.row.saturating_sub(area.y);
    for (m, (dy, dx)) in SCHOOL_WEDGE.iter().take(school_size).enumerate() {
        let ascii_body = fish_body(swims_right, m == 0);
        let body_w = if ascii_safe {
            ascii_body.width() as u16
        } else {
            4
        };
        // Nose position in wrap space; trailers sit `dx` columns behind the
        // lead relative to travel, so the wedge follows instead of leading.
        // Right-swimmers enter from the left edge, left-swimmers from the
        // right edge — both facing exactly the way they move.
        // Formation drift. Every fish used to sit at an exact offset in the
        // wedge, so seven animals crossed the field as one rigid object —
        // the single biggest reason the water read as decoration rather than
        // life. Each fish now eases one dot fore and aft of its slot on its
        // own slow period, so the wedge breathes while it travels.
        //
        // The period is deliberately off both the bob (`3_400 + m * 640`)
        // and the tail cycle (300 ms), per this module's rule that entity
        // periods never match so nothing strobes in step. One dot of
        // amplitude over ~6 s is far slower than the crossing speed, so a
        // fish never travels against the school and `facing == velocity`
        // still holds by construction. Costs no marks: the school's
        // population, band and budget are unchanged.
        let drift = i32::from(sine_bob(
            t,
            5_200 + entity_jitter(m as u128 + 617) % 3_400,
            2,
        )) - 1;
        let x_dots = if swims_right {
            cycle_dot_step - i32::from(*dx) * 2 - i32::from(body_w) * 2 + drift
        } else {
            i32::from(area.width) * 2 - cycle_dot_step + i32::from(*dx) * 2 - drift
        };
        let mut x_i32 = if ascii_safe {
            if swims_right {
                cycle_step - i32::from(*dx) - i32::from(body_w)
            } else {
                i32::from(area.width) - cycle_step + i32::from(*dx)
            }
        } else {
            x_dots.div_euclid(2)
        };
        // Native fish bob by one dot inside a cell, never by a whole text row.
        let bob = sine_bob(t, 3_000 + entity_jitter(m as u128 + 41) % 2_600, 1);
        let y_i32 =
            i32::from(anchor_y) + i32::from(*dy) + if ascii_safe { i32::from(bob) } else { 0 };
        let body = if ascii_safe {
            ascii_body
        } else {
            native_poses::fish(
                swims_right,
                ((t / 300 + m as u128) % 4) as usize,
                x_dots.rem_euclid(2) as usize,
                usize::from(bob),
            )
        };
        // Fish dart sideways away from the scatter anchor (nearby only).
        if let Some(flee_ms) = cursor.flee_elapsed_ms {
            let flee = i32::from(fish_flee_offset(flee_ms));
            if x_i32.abs_diff(i32::from(ptr)) < 16 && y_i32.abs_diff(i32::from(ptr_y)) < 6 {
                // Horizontal only. The old ±1 row kick pushed the outer
                // fish off the school's band and straight into the row the
                // composition had already claimed, so a scatter punched a
                // hole in the wedge exactly when the eye was on it.
                if x_i32 >= i32::from(ptr) {
                    x_i32 += flee;
                } else {
                    x_i32 -= flee;
                }
            }
        }
        let max_x = i32::from(area.width.saturating_sub(body_w));
        let max_y = i32::from(area.height.saturating_sub(1));
        if x_i32 < 0 || x_i32 > max_x || y_i32 < 0 || y_i32 > max_y {
            continue; // off-screen while wrapping
        }
        let y = y_i32 as u16;
        // Never swim through the composition or the row of air around it.
        if !water(x_i32 as u16, y, body_w) {
            continue;
        }
        let brightness = FISH_BRIGHTNESS_FLOOR
            + (1.0 - FISH_BRIGHTNESS_FLOOR)
                * wave01(t, FISH_WAVE_MS, (m as u128).saturating_mul(320));
        marks.push(AmbientMark {
            x: x_i32 as u16,
            y,
            glyph: body,
            jellyfish: None,
            depth: if m == 0 {
                Depth::Foreground
            } else {
                Depth::Midground
            },
            style_mod: None,
            brightness: Some(brightness),
        });
    }

    // --- Jellyfish: a pulsing dome with lagging tentacles ---
    // Native braille shapes have a contracting bell and a wave travelling
    // down two trailing arms; fractional placement still fits the 5×3-cell
    // habitat. ASCII keeps two dome rows above two swaying strokes, with a
    // 3-cell compact silhouette. Both representations share the rare visit,
    // shallow glow and whole-silhouette clearance below.
    //
    // It only visits water deep enough to hold it: three rows of silhouette,
    // a row of clear water, and the school's own band, measured up from the
    // floor. At 80×24 the composition leaves four rows of water and the
    // jellyfish used to land inside the school — a five-cell pulsing
    // silhouette and a wedge of fish sharing four rows of a 24-row terminal
    // is the definition of not earning the space. Below the budget it simply
    // does not come up.
    let jellyfish_count = density.jellyfish_count();
    for j in 0..jellyfish_count {
        let phase = 3_100u128.saturating_add((j as u128) * 4_700);
        let lane_x = if j % 2 == 0 {
            area.width.saturating_mul(5) / 6
        } else {
            area.width / 6
        };
        let wobble = sine_bob(t, 5_200 + phase, 1);
        let compact = density == LifeDensity::Sparse;
        let (dome_top, dome_skirt, tentacle_cols): (&[&str], &[&str], &[u16]) = if compact {
            (JELLY_DOME_TOP_COMPACT, JELLY_DOME_SKIRT_COMPACT, &[0, 2])
        } else {
            // Two tentacles hanging from the rim, not three abreast. Three
            // adjacent one-cell strokes spend most of their sway table
            // rendering as `||\` or `|||` — a solid bar of punctuation under
            // the bell, which is what the dogfood frame actually showed.
            (JELLY_DOME_TOP_FRAMES, JELLY_DOME_SKIRT_FRAMES, &[1, 3])
        };
        let dome_w = if ascii_safe {
            dome_top[0].width() as u16
        } else {
            5
        };
        let wobble_dots = sine_bob(t, 5_200 + phase, 2);
        let x = lane_x
            .saturating_add(if ascii_safe { wobble } else { wobble_dots / 2 })
            .min(area.width.saturating_sub(dome_w + 1));
        if deep_water_rows(area, lines, x, dome_w) < JELLY_MIN_DEEP_ROWS {
            continue;
        }
        // A visit is a short, slow rise near the floor followed by a long
        // absence: the jelly climbs [`JELLY_VISIT_ROWS`] rows and then spends
        // the rest of the cycle out of sight. Native movement samples quarter
        // rows; the ASCII fallback retains its slow whole-row steps.
        let rise_period = JELLY_RISE_ROW_MS.saturating_add((j as u128) * JELLY_RISE_ROW_STAGGER_MS);
        let cycle_duration = rise_period.saturating_mul(JELLY_VISIT_CYCLE_SLOTS);
        let cycle_pos = t.saturating_add(phase) % cycle_duration;
        let visit_duration = rise_period.saturating_mul(u128::from(JELLY_VISIT_ROWS));
        if cycle_pos >= visit_duration {
            continue; // still down in the dark between visits
        }
        let visit_progress = cycle_pos as f64 / visit_duration as f64;
        let risen = (visit_progress * f64::from(JELLY_VISIT_ROWS)).round() as u16;
        let y_dots = i32::from(area.height.saturating_sub(JELLY_FLOOR_GAP)) * 4
            - (visit_progress * f64::from(JELLY_VISIT_ROWS) * 4.0).floor() as i32;
        let y = if ascii_safe {
            area.height
                .saturating_sub(JELLY_FLOOR_GAP)
                .saturating_sub(risen)
        } else {
            y_dots.div_euclid(4).max(0) as u16
        };
        if y == 0 || !water(x, y, dome_w) {
            continue;
        }
        let dome_pulse = wave01(t, JELLY_PULSE_MS, phase);
        let dome_brightness = jelly_glow(dome_pulse);
        let tentacle_pulse = wave01(
            t.saturating_sub(JELLY_TENTACLE_LAG_MS),
            JELLY_PULSE_MS,
            phase,
        );
        let tentacle_brightness = jelly_glow(tentacle_pulse);
        // The dome opens/closes on the smooth continuous phase curve; the parked
        // pose holds the half-pulsed (contracted) frame.
        let pulse_frame = usize::from(dome_pulse > 0.5);
        let skirt_row = y.saturating_add(1);
        let tentacle_row = y.saturating_add(2);
        // Treat the silhouette as one visual unit. The former per-row quiet
        // band checks deliberately allowed the dome, skirt, or tentacles to
        // disappear independently, which is exactly the broken punctuation
        // visible in the v0.9.2 dogfood screenshot.
        if tentacle_row >= area.height
            || ![y, skirt_row, tentacle_row]
                .into_iter()
                .all(|row| water(x, row, dome_w))
        {
            continue;
        }
        if !ascii_safe {
            let pose = ((t.saturating_add(phase) % JELLY_PULSE_MS) * 16 / JELLY_PULSE_MS) as usize;
            for (row, glyph) in native_poses::jelly(
                pose,
                usize::from(wobble_dots % 2),
                y_dots.rem_euclid(4) as usize,
            )
            .iter()
            .enumerate()
            {
                marks.push(AmbientMark {
                    x,
                    y: y + row as u16,
                    glyph,
                    jellyfish: Some(j),
                    depth: Depth::Background,
                    style_mod: None,
                    brightness: Some(if row == 0 {
                        dome_brightness
                    } else {
                        tentacle_brightness
                    }),
                });
            }
            continue;
        }
        for (row, glyph) in [
            (y, dome_top[pulse_frame]),
            (skirt_row, dome_skirt[pulse_frame]),
        ] {
            marks.push(AmbientMark {
                x,
                y: row,
                glyph,
                jellyfish: Some(j),
                // Background ink, same as the tentacles: the dome used to sit
                // a layer nearer than everything else in the side lanes,
                // which is most of why it drew the eye.
                depth: Depth::Background,
                style_mod: None,
                brightness: Some(dome_brightness),
            });
        }
        for (col, &dx) in tentacle_cols.iter().enumerate() {
            // Each column runs the sway table with its own phase offset
            // so the trio lags left-to-right; the parked pose holds a
            // mid-sway frame.
            let frame = t
                .saturating_add(phase)
                .saturating_add((col as u128) * JELLY_TENTACLE_PHASE_STEP_MS)
                / JELLY_TENTACLE_SWAY_MS;
            let sway = JELLY_TENTACLE_FRAMES[(frame as usize) % JELLY_TENTACLE_FRAMES.len()];
            marks.push(AmbientMark {
                x: x.saturating_add(dx),
                y: tentacle_row,
                glyph: sway,
                jellyfish: Some(j),
                depth: Depth::Background,
                style_mod: None,
                brightness: Some(tentacle_brightness),
            });
        }
    }

    // --- Marine snow & rising bubble streams floating upward ---
    // Floating particles rise smoothly through the water column, dissolving
    // gently with continuous time-based floating physics.
    for b in 0..density.bubble_streams() {
        // Irregular phase, period and lane. Two fixed lanes at `width/8`
        // and `7*width/8` meant extra streams stacked into the same two
        // columns and rose on an arithmetic beat; spread them over the whole
        // width and let `water()` below reject any column the composition
        // owns, which is the one placement rule this module has.
        let phase = entity_jitter(b as u128) % 9_000;
        let column = (entity_jitter(b as u128 + 977) % u128::from(area.width.max(1))) as u16;
        let rise_period = BUBBLE_RISE_MS.saturating_add(entity_jitter(b as u128 + 313) % 2_600);
        let cycle = (t.saturating_add(phase) % rise_period) as f64 / rise_period as f64;
        let boost = if cursor.flee_elapsed_ms.is_some() && column.abs_diff(ptr) < 10 {
            2
        } else {
            0
        };
        // Continuous horizontal floating drift
        let drift_phase = (t.saturating_add(phase) as f64 / 2_100.0) * std::f64::consts::TAU;
        let drift = (drift_phase.sin() * 0.6).round() as i16;
        let col = (column as i16 + drift).clamp(0, (area.width.saturating_sub(1)) as i16) as u16;

        let rise = ((cycle * f64::from(BUBBLE_MAX_RISE_ROWS)).round() as u16)
            .saturating_add(boost)
            .min(BUBBLE_MAX_RISE_ROWS);
        let y = area.height.saturating_sub(2).saturating_sub(rise);
        if !water(col, y, 1) {
            continue;
        }
        // Size is a function of height risen, not of discrete clock jumps.
        let glyph = bubble_glyph(rise);
        let brightness = glint01(
            t,
            BUBBLE_GLINT_MS.saturating_add(phase % 700),
            600,
            BUBBLE_BRIGHTNESS_FLOOR,
            phase,
        ) * bubble_dissolve(rise);
        marks.push(AmbientMark {
            x: col,
            y,
            glyph,
            jellyfish: None,
            depth: Depth::Foreground,
            style_mod: None,
            brightness: Some(brightness),
        });
    }

    stats.marks_built = marks.len() as u32;
    FrameMarks { marks }
}

/// Loose diagonal wedge for the school: `(row_offset, columns_behind_lead)`.
/// The slight row spread is what makes it read as a school, not a text row.
///
/// Three rows, not five. The ±2 rows put the wedge across a fifth of a 24-row
/// terminal, which reads as fish scattered over the screen rather than as one
/// shoal; at ±1 (plus each fish's own bob) the school still has depth but
/// stays a single object the eye can take in at once.
const SCHOOL_WEDGE: &[(i16, u16)] = &[(0, 0), (-1, 4), (1, 6), (-1, 9), (1, 11), (0, 14), (-1, 17)];

/// Rows between the school's centre line and the bottom of the field. With the
/// ±1 wedge and a one-row bob the shoal occupies `height-4 ..= height-1`: the
/// deep water, clear of anything the composition is using.
const SCHOOL_FLOOR_GAP: u16 = 3;

/// The row the school centres on, in field-local coordinates. Public so the
/// compositor can aim a scatter at the shoal instead of guessing where it is.
#[must_use]
pub fn school_band_row(area: Rect) -> u16 {
    area.height.saturating_sub(SCHOOL_FLOOR_GAP)
}

/// Wall-clock milliseconds per column of school travel (~2.6 cells/s).
const SCHOOL_CELL_MS: u128 = 380;
/// Travelling brightness-wave period through the wedge.
const FISH_WAVE_MS: u128 = 2_200;
/// Fish are small: never let one sink into the gradient.
const FISH_BRIGHTNESS_FLOOR: f32 = 0.45;

/// Lead fish silhouettes (ASCII only — width == len). Members drop the eye.
const LEAD_FISH_RIGHT: &str = "><o>";
const LEAD_FISH_LEFT: &str = "<o><";

/// Jellyfish silhouette frames — pure ASCII by construction so the
/// ascii_safe tier needs no fallback mapping for them (len == width).
///
/// Full dome (Rich/Normal), two rows with an open/closed pulse pair: a
/// rounded arc over the bell's rim.
///
/// The skirt is the bell's lower rim and nothing else: it carries the pulse by
/// flaring (`\` `/`) and contracting (`(` `)`), the way a real bell swims. It
/// holds no interior glyphs on purpose — an earlier pair put marks inside the
/// rim (`(v_v)` / `(v.v)`), which read as two eyes and a mouth. The motion the
/// silhouette is meant to sell lives in the tentacle row below, not in the
/// skirt.
///
/// Both contracted frames are left-right symmetric on purpose. The former
/// `.'-.'` and `'.'` were not — a dot on one side and an apostrophe on the
/// other — and an asymmetric five-cell arc does not read as a bell at all; in
/// the 80×24 dogfood frame it read as three unrelated rows of punctuation.
const JELLY_DOME_TOP_FRAMES: &[&str] = &[".-~-.", ".'-'."];
const JELLY_DOME_SKIRT_FRAMES: &[&str] = &["\\___/", "(___)"];
/// Compact dome for the Sparse (narrow) tier: same two-row read at 3 cells.
const JELLY_DOME_TOP_COMPACT: &[&str] = &[".-.", "'-'"];
const JELLY_DOME_SKIRT_COMPACT: &[&str] = &["\\_/", "(_)"];
/// Tentacle sway frames (all width-1). Each column runs the same table with
/// a phase offset so the pair lags instead of strobing in sync.
const JELLY_TENTACLE_FRAMES: &[&str] = &["|", "/", "|", "\\"];

/// How far sideways a jellyfish may dodge to clear transcript text before it
/// is withheld for the frame instead.
///
/// Placement is a pure function of the text under the silhouette, so during a
/// fast stream it is effectively a function of token throughput: a growing
/// line pushes the anchor one column per character, and a wrap or a scroll
/// collapses that row's occupied bounds and snaps the anchor back tens of
/// columns in a single frame. On screen that reads as teleporting, and it only
/// shows up on models fast enough to change those bounds every frame — which
/// is why slow providers never surfaced it.
///
/// Bounding the dodge keeps the behavior the silhouette was actually given
/// (ease around a word that happens to brush its lane) and turns everything
/// larger into the same quiet withhold the fish already use. Worst-case
/// frame-to-frame movement is therefore `2 * JELLY_MAX_TEXT_DODGE_COLS`, at
/// the single moment a left-hand candidate overtakes a right-hand one.
const JELLY_MAX_TEXT_DODGE_COLS: u16 = 3;

// --- Jellyfish rarity ------------------------------------------------------
// The jellyfish is the loudest thing in the water: a five-cell silhouette that
// changes glyph as it pulses, parked in a side lane. Before v0.9.4 it was also
// permanently resident, which is the combination that made it obnoxious rather
// than incidental. Everything below is one knob with one stated intent, so the
// balance can be retuned without re-deriving it from the motion code.

/// Wall-clock milliseconds a jellyfish spends on each row of its rise
/// (~9.4 s). Native placement samples quarter rows within this duration;
/// ASCII-safe placement keeps the original slow whole-row cadence.
const JELLY_RISE_ROW_MS: u128 = 9_400;
/// Per-jelly rise-rate stagger, so two jellyfish (should a tier ever want
/// them again) can never step in lockstep.
const JELLY_RISE_ROW_STAGGER_MS: u128 = 1_400;
/// Rows climbed in a single visit — about 56 s of presence.
const JELLY_VISIT_ROWS: u16 = 6;
/// Rows between the jellyfish's dome and the bottom of the field. The
/// silhouette is three rows tall, so this leaves exactly one row of clear
/// water between its tentacles and the top of the school's band — the
/// jellyfish is a visitor in the same water, not a passenger on the shoal.
const JELLY_FLOOR_GAP: u16 = 8;
/// Unbroken water rows (measured up from the floor) a jellyfish needs before
/// it will surface at all: its own three rows, the gap, and the school's band.
/// Same number as [`JELLY_FLOOR_GAP`] by construction — the dome's row is the
/// deepest row it touches.
const JELLY_MIN_DEEP_ROWS: u16 = JELLY_FLOOR_GAP;
/// Row-slots in one full visit cycle. Slots at or past [`JELLY_VISIT_ROWS`]
/// are spent out of sight, and that gap is *the* rarity knob: at 32 slots the
/// cycle is ~5 min and a jellyfish is present under a fifth of the time —
/// occasionally noticed, never resident. Raise it to make them rarer; lower
/// it to bring them back. It must stay `> JELLY_VISIT_ROWS` or the jelly
/// becomes permanent again.
const JELLY_VISIT_CYCLE_SLOTS: u128 = 32;

// --- Jellyfish motion and glow ---------------------------------------------

/// Dome pulse period. Slow on purpose: a pulse fast enough to notice in
/// peripheral vision is a pulse that interrupts reading.
const JELLY_PULSE_MS: u128 = 5_200;
/// The tentacles repeat the dome pulse this much later. Held at ~12% of
/// [`JELLY_PULSE_MS`] — the lag is what sells "jellyfish", so it scales with
/// the pulse rather than staying an absolute number.
const JELLY_TENTACLE_LAG_MS: u128 = 620;
/// Wall-clock milliseconds per tentacle sway frame.
const JELLY_TENTACLE_SWAY_MS: u128 = 2_600;
/// Per-column sway phase offset, so the two tentacles never move in sync.
/// Keep this a non-divisor of [`JELLY_TENTACLE_SWAY_MS`] or the pair strobes.
const JELLY_TENTACLE_PHASE_STEP_MS: u128 = 700;
/// Dimmest point of the pulse: still legible against the water, no lower.
const JELLY_BRIGHTNESS_FLOOR: f32 = 0.28;
/// Brightest point of the pulse. Deliberately well short of full ink — the
/// jellyfish used to swing floor-to-1.0, and that swing (not its presence)
/// is what pulled the eye off the transcript.
const JELLY_BRIGHTNESS_CEIL: f32 = 0.62;

/// Map a `[0, 1]` pulse onto the jellyfish's shallow glow band.
#[must_use]
fn jelly_glow(pulse: f32) -> f32 {
    JELLY_BRIGHTNESS_FLOOR + (JELLY_BRIGHTNESS_CEIL - JELLY_BRIGHTNESS_FLOOR) * pulse
}

/// Bubbles stay mostly steady with occasional glints, not a constant wave.
const BUBBLE_BRIGHTNESS_FLOOR: f32 = 0.55;
/// Rows a bubble climbs before it dissolves. Short on purpose: a bubble that
/// crosses the whole field is a moving speck with no source and no end.
const BUBBLE_MAX_RISE_ROWS: u16 = 5;
/// Wall-clock milliseconds for one bubble to make that climb.
const BUBBLE_RISE_MS: u128 = 3_200;
/// Base period of the raised-cosine glint.
const BUBBLE_GLINT_MS: u128 = 2_600;
/// How much of its brightness a bubble keeps at the top of its rise.
const BUBBLE_DISSOLVE_CEIL: f32 = 0.25;

/// Bubbles grow as they rise. Keyed to height, never to the clock.
#[must_use]
fn bubble_glyph(rise: u16) -> &'static str {
    match rise {
        0..=1 => "·",
        2..=3 => "˚",
        _ => "°",
    }
}

/// Linear fade across the rise: full at the floor, nearly gone at the top.
#[must_use]
fn bubble_dissolve(rise: u16) -> f32 {
    let span = f32::from(BUBBLE_MAX_RISE_ROWS.max(1));
    let remaining = f32::from(BUBBLE_MAX_RISE_ROWS.saturating_sub(rise)) / span;
    BUBBLE_DISSOLVE_CEIL + (1.0 - BUBBLE_DISSOLVE_CEIL) * remaining
}

/// One soft sin² hump per `period_ms`, wall-clock keyed, in `[0, 1]`.
#[must_use]
fn wave01(elapsed_ms: u128, period_ms: u128, phase_ms: u128) -> f32 {
    if period_ms == 0 {
        return 1.0;
    }
    let frac = (elapsed_ms.saturating_add(phase_ms) % period_ms) as f64 / period_ms as f64;
    let s = (frac * std::f64::consts::PI).sin();
    (s * s) as f32
}

/// Mostly `floor`, with a raised-cosine glint to full brightness for
/// `glint_ms` out of every `period_ms`.
#[must_use]
fn glint01(elapsed_ms: u128, period_ms: u128, glint_ms: u128, floor: f32, phase_ms: u128) -> f32 {
    if period_ms == 0 || glint_ms == 0 {
        return floor;
    }
    let pos = elapsed_ms.saturating_add(phase_ms) % period_ms;
    if pos >= glint_ms {
        return floor;
    }
    let frac = pos as f64 / glint_ms as f64;
    let bump = 0.5 * (1.0 - (frac * std::f64::consts::TAU).cos());
    floor + (1.0 - floor) * bump as f32
}

/// Stateless per-crossing travel direction. Direction only ever changes
/// between cycles — while the school is fully off-screen — so a turn is
/// never visible as an in-place flip.
#[must_use]
fn school_swims_right(cycle_index: u128) -> bool {
    (cycle_index.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 7) & 1 == 0
}

/// Rows of clear air the composition keeps on each side of every line it
/// writes. One row is enough: it is the difference between a fish swimming
/// *behind* a block of text and a fish surfacing in the gap between two of its
/// lines, which is what the 80×24 frame showed between the caption and the
/// invitation.
const TEXT_CLEARANCE_ROWS: u16 = 1;

/// True when the horizontal span at `(x, y)` — and the same span on every row
/// within [`TEXT_CLEARANCE_ROWS`] — carries no rendered text. One column of
/// horizontal air is reserved on both sides so a fish never touches the prose,
/// while short left-aligned transcript lines still leave real water to their
/// right.
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
    usize::from(x) < end.saturating_add(1)
        && usize::from(x).saturating_add(usize::from(width)) > start.saturating_sub(1)
}

/// Unbroken open-water rows measured up from the bottom of the field: how much
/// deep water the composition has left for the aquarium to live in.
#[must_use]
fn deep_water_rows(area: Rect, lines: &[Line<'_>], x: u16, width: u16) -> u16 {
    let mut rows = 0u16;
    let mut y = area.height;
    while y > 0 {
        y -= 1;
        if !is_open_water(lines, x, y, width) {
            break;
        }
        rows = rows.saturating_add(1);
    }
    rows
}

fn paint_marks(
    area: Rect,
    buf: &mut Buffer,
    inks: (Color, Color),
    lines: &[Line<'static>],
    frame: &FrameMarks,
    presence: f32,
    stats: &mut AmbientFrameStats,
) {
    if presence <= 0.0 {
        // Fully static water: nothing to paint (all marks invisible).
        return;
    }
    let presence = presence.clamp(0.0, 1.0);
    #[derive(Clone, Copy)]
    enum SkipReason {
        Text,
        Clipped,
    }

    #[derive(Clone, Copy)]
    enum Placement {
        Anchor { original: u16, placed: u16 },
        Skip(SkipReason),
    }
    let mut placements: [Option<Placement>; 2] = [None, None];
    let population_overflow = frame
        .marks
        .iter()
        .filter_map(|mark| mark.jellyfish)
        .any(|jellyfish| jellyfish >= placements.len());
    debug_assert!(
        !population_overflow,
        "jellyfish population exceeded its bound"
    );
    for (jellyfish, placement) in placements.iter_mut().enumerate() {
        let marks = || {
            frame
                .marks
                .iter()
                .filter(move |mark| mark.jellyfish == Some(jellyfish))
        };
        let Some(original) = marks().map(|mark| mark.x).min() else {
            continue;
        };
        let mut group_end = 0u16;
        for mark in marks() {
            let offset = mark.x.saturating_sub(original);
            let width = u16::try_from(UnicodeWidthStr::width(mark.glyph)).unwrap_or(u16::MAX);
            group_end = group_end.max(offset.saturating_add(width));
        }
        let Some(right_edge) = area.width.checked_sub(group_end) else {
            *placement = Some(Placement::Skip(SkipReason::Clipped));
            continue;
        };

        let mut best: Option<(u16, u16)> = None;
        let mut consider = |candidate: i64| {
            let Ok(candidate) = u16::try_from(candidate) else {
                return;
            };
            // Bounded dodge. Anything further than the cap is a relocation
            // rather than a drift, so it is refused here and the silhouette
            // is withheld instead — see [`JELLY_MAX_TEXT_DODGE_COLS`].
            let dodge = candidate.abs_diff(original);
            if dodge > JELLY_MAX_TEXT_DODGE_COLS {
                return;
            }
            let fits = candidate <= right_edge
                && marks().all(|mark| {
                    let x = candidate.saturating_add(mark.x.saturating_sub(original));
                    let width =
                        u16::try_from(UnicodeWidthStr::width(mark.glyph)).unwrap_or(u16::MAX);
                    is_open_water(lines, x, mark.y, width)
                });
            if fits {
                let ranked = (dodge, candidate);
                if best.is_none_or(|current| ranked < current) {
                    best = Some(ranked);
                }
            }
        };
        consider(i64::from(original));
        consider(0);
        consider(i64::from(right_edge));
        for mark in marks() {
            let offset = mark.x.saturating_sub(original);
            let mark_end = offset.saturating_add(
                u16::try_from(UnicodeWidthStr::width(mark.glyph)).unwrap_or(u16::MAX),
            );
            let first = usize::from(mark.y.saturating_sub(TEXT_CLEARANCE_ROWS));
            let last = usize::from(mark.y.saturating_add(TEXT_CLEARANCE_ROWS));
            for (start, end) in
                (first..=last).filter_map(|row| lines.get(row).and_then(occupied_text_bounds))
            {
                if let Ok(start) = i64::try_from(start) {
                    consider(start - 1 - i64::from(mark_end));
                }
                if let Ok(end) = i64::try_from(end) {
                    consider(end + 1 - i64::from(offset));
                }
            }
        }
        *placement = Some(match best {
            Some((_, placed)) => Placement::Anchor { original, placed },
            None => Placement::Skip(SkipReason::Text),
        });
    }

    for mark in &frame.marks {
        let mark_placement = mark
            .jellyfish
            .map(|index| placements.get(index).copied().flatten());
        let (mark_x, preflighted) = match mark_placement {
            Some(None) => {
                stats.marks_clipped += 1;
                continue;
            }
            Some(Some(Placement::Anchor { original, placed })) => (
                placed
                    .checked_add(mark.x.saturating_sub(original))
                    .expect("preflight accepted a clipped jellyfish"),
                true,
            ),
            Some(Some(Placement::Skip(SkipReason::Text))) => {
                stats.marks_skipped_text += 1;
                continue;
            }
            Some(Some(Placement::Skip(SkipReason::Clipped))) => {
                stats.marks_clipped += 1;
                continue;
            }
            None => (mark.x, false),
        };
        if !preflighted {
            let mark_width = UnicodeWidthStr::width(mark.glyph);
            // Clipped is checked before text collision so a mark that fails
            // both is charged to the bound it could never satisfy.
            if mark_x.saturating_add(mark_width as u16) > area.width {
                stats.marks_clipped += 1;
                continue;
            }
            if !is_open_water(
                lines,
                mark_x,
                mark.y,
                u16::try_from(mark_width).unwrap_or(u16::MAX),
            ) {
                stats.marks_skipped_text += 1;
                continue;
            }
        }
        stats.marks_painted += 1;
        let ink = if mark.depth.ink_index() == 1 {
            inks.1
        } else {
            inks.0
        };
        for (offset, ch) in mark.glyph.chars().enumerate() {
            let cell = &mut buf[(area.x + mark_x + offset as u16, area.y + mark.y)];
            // Glow language: lerp the mark's ink up from the water the cell
            // already sits in, at the entity's time-varying brightness. The
            // overall lerp is additionally scaled by life presence so marks
            // fade in/out with the animated/static boundary.
            let fg = match (mark.brightness, cell.style().bg) {
                (Some(amount), Some(water)) => {
                    ocean::mix_colors(water, ink, (amount * presence).clamp(0.0, 1.0))
                }
                (Some(amount), None) => ocean::scale_color(ink, amount.clamp(0.0, 1.0).max(0.4)),
                (None, Some(water)) => ocean::mix_colors(water, ink, presence),
                (None, None) => ocean::scale_color(ink, presence),
            };
            let mut style = Style::default().fg(fg);
            if let Some(m) = mark.style_mod {
                style = style.add_modifier(m);
            }
            cell.set_symbol(&ch.to_string());
            cell.set_style(style);
            stats.cells_written += 1;
        }
    }
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

/// Deterministic per-entity jitter.
///
/// Every period in this module used to be an arithmetic series — bubble
/// phases at `b * 1_900`, fish bobs at `3_400 + m * 640` — so the field read
/// as a mechanism keeping time rather than as animals. This spreads entity
/// constants irregularly while staying a pure function of the index, which
/// the delta/interpolation path requires: the module still owns no clock, no
/// simulation and no RNG state, and two runs at the same `t` paint the same
/// frame.
#[must_use]
fn entity_jitter(seed: u128) -> u128 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in seed.to_le_bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    u128::from(hash)
}

fn sine_bob(elapsed_ms: u128, period_ms: u128, amplitude: u16) -> u16 {
    if period_ms == 0 || amplitude == 0 {
        return 0;
    }
    let phase = (elapsed_ms % period_ms) as f64 / period_ms as f64;
    let s = (phase * std::f64::consts::TAU).sin();
    // Map [-1,1] → [0, amplitude]
    (((s + 1.0) * 0.5) * f64::from(amplitude)).round() as u16
}

/// One-shot flee arc keyed to Working transition / pointer motion.
#[must_use]
pub fn fish_flee_offset(elapsed_ms: u128) -> u16 {
    let progress = elapsed_ms.min(800) as f32 / 800.0;
    let excursion = (progress * std::f32::consts::PI).sin() * 9.0;
    excursion.round().clamp(0.0, 9.0) as u16
}

/// One fish silhouette family for the whole school: the lead carries an eye
/// (`><o>`), members are plain `><>`. Never mix lone `>` arrows in — that
/// reads as broken punctuation. All bodies are ASCII so `len() == width`.
#[must_use]
fn fish_body(facing_right: bool, lead: bool) -> &'static str {
    match (facing_right, lead) {
        (true, true) => LEAD_FISH_RIGHT,
        (true, false) => "><>",
        (false, true) => LEAD_FISH_LEFT,
        (false, false) => "<><",
    }
}

/// Count fish silhouettes in rendered text by facing: `(rightward, leftward)`.
///
/// Recognizes the ASCII bodies and every native braille pose, so a render
/// test can assert the school without knowing which family painted it. The
/// native poses carry no eye (ad20493), so only the ASCII lead is
/// distinguishable from its followers.
#[cfg(test)]
pub(crate) fn fish_silhouette_counts(text: &str) -> (usize, usize) {
    let native = |right: bool| {
        let poses: std::collections::BTreeSet<&'static str> = (0..4)
            .flat_map(|pose| (0..2).flat_map(move |dx| (0..2).map(move |dy| (pose, dx, dy))))
            .map(|(pose, dx, dy)| native_poses::fish(right, pose, dx, dy))
            .collect();
        poses
            .into_iter()
            .map(|pose| text.matches(pose).count())
            .sum::<usize>()
    };
    let ascii_right = text.matches("><>").count() + text.matches(LEAD_FISH_RIGHT).count();
    let ascii_left = text.matches("<><").count() + text.matches(LEAD_FISH_LEFT).count();
    (ascii_right + native(true), ascii_left + native(false))
}

/// Subtle caustic shimmer applied to empty water cells when the field would
/// otherwise read as a static ramp. Cheap: one phase lookup per cell, only
/// when `animated` and density allows.
pub fn apply_caustic_shimmer(
    area: Rect,
    buf: &mut Buffer,
    column: &OceanColumn,
    elapsed_ms: u128,
    animated: bool,
    lines: &[Line<'static>],
) {
    if !animated || area.width < AMBIENT_MIN_WIDTH || area.height < AMBIENT_MIN_HEIGHT {
        return;
    }
    let ceiling = (0..area.height)
        .find(|row| {
            lines
                .get(usize::from(*row))
                .and_then(occupied_text_bounds)
                .is_some()
        })
        .unwrap_or(area.height);
    let band = (area.height / 3).max(2).min(ceiling);
    let ramp = frame_ocean_ramp(
        column,
        area.height,
        area.y,
        elapsed_ms,
        column.phase_tag(),
        column.ramp_fingerprint(),
    );
    let protected = codewhale_ratatui::ocean::ocean_semantic_surfaces(
        lines,
        area,
        codewhale_tui_kit::ui_text::grapheme_display_width,
    );
    let paint = codewhale_ratatui::ocean::OceanPaintFacts {
        ground: ramp
            .first()
            .copied()
            .unwrap_or_else(|| column.color_at_y(area.y)),
        sample_top: area.y,
        samples: &ramp,
        protected: &protected,
    };
    let facts = codewhale_ratatui::ocean::OceanCausticFacts {
        paint,
        elapsed: std::time::Duration::from_millis((elapsed_ms % 960) as u64),
        band_rows: band,
    };
    column.paint_caustics(area, buf, &facts);
}

#[cfg(test)]
fn caustic_brightness(elapsed_ms: u128, local_x: u16, local_y: u16, depth_fade: f32) -> f32 {
    codewhale_ratatui::ocean::ocean_caustic_brightness(
        std::time::Duration::from_millis((elapsed_ms % 960) as u64),
        local_x,
        local_y,
        depth_fade,
    )
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

/// Process-local per-frame ocean ramp shared by chat field, caustics, and
/// other widgets that paint the same column.
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

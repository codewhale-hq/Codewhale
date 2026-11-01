use super::*;
use ratatui::text::Span;

fn dot_cameo(area: Rect, age: u128, activity: AmbientActivity) -> (Buffer, AmbientFrameStats) {
    let mut buf = Buffer::empty(area);
    let mut stats = AmbientFrameStats::default();
    pet_cameo::paint(
        area,
        &mut buf,
        Color::Cyan,
        &[],
        1.0,
        WhaleCameo {
            elapsed_ms: Some(age),
            anchor_x: area.x + area.width / 2,
            anchor_y: area.y + area.height / 2,
        },
        activity,
        &mut stats,
    );
    (buf, stats)
}

#[test]
fn dot_cameo_is_bounded_and_independent_of_draw_history() {
    let area = Rect::new(7, 11, 80, 24);
    let (first, stats) = dot_cameo(area, 500, AmbientActivity::Baseline);
    assert!(stats.marks_painted > 0);
    dot_cameo(area, 1_900, AmbientActivity::Subagents);
    dot_cameo(Rect::new(0, 0, 140, 40), 100, AmbientActivity::Baseline);
    assert_eq!(first, dot_cameo(area, 500, AmbientActivity::Baseline).0);
    assert_ne!(first, dot_cameo(area, 900, AmbientActivity::Baseline).0);
    assert_eq!(
        dot_cameo(area, 2_400, AmbientActivity::Baseline).1,
        AmbientFrameStats::default()
    );
    assert_eq!(
        dot_cameo(area, u128::MAX, AmbientActivity::Subagents).1,
        AmbientFrameStats::default()
    );
}

#[test]
fn dot_cameo_withholds_whole_body_when_label_touches_wide_text() {
    let area = Rect::new(7, 11, 80, 24);
    let (clear, clear_stats) = dot_cameo(area, 500, AmbientActivity::Baseline);
    let label_cell = clear
        .content
        .iter()
        .position(|cell| cell.symbol() == "o")
        .unwrap();
    let (x, y) = (
        label_cell % area.width as usize,
        label_cell / area.width as usize,
    );
    let mut lines = vec![Line::from(""); area.height as usize];
    lines[y] = Line::from(format!("{}鲸", " ".repeat(x)));
    let mut buf = Buffer::empty(area);
    buf.set_line(area.x, area.y + y as u16, &lines[y], area.width);
    let before = buf.clone();
    let mut stats = AmbientFrameStats::default();
    pet_cameo::paint(
        area,
        &mut buf,
        Color::Cyan,
        &lines,
        1.0,
        WhaleCameo {
            elapsed_ms: Some(500),
            anchor_x: area.x + 40,
            anchor_y: area.y + 12,
        },
        AmbientActivity::Baseline,
        &mut stats,
    );
    assert_eq!(
        before, buf,
        "a label collision must withhold every particle too"
    );
    assert_eq!(stats.marks_skipped_text, clear_stats.marks_built);
    assert_eq!(stats.marks_painted, 0);
}

#[test]
fn narrow_pod_members_are_withheld_instead_of_stacked() {
    let area = Rect::new(0, 0, 40, 12);
    let (buf, stats) = dot_cameo(area, 500, AmbientActivity::Subagents);
    let text: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert_eq!(text.matches("agent · pod").count(), 1);
    assert!(stats.marks_clipped > 0);
    assert_eq!(stats.marks_built, stats.marks_painted + stats.marks_clipped);
}

#[test]
fn reduced_motion_builds_and_paints_no_ambient_creatures() {
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    let before = buf.clone();
    let stats = render_ambient_life(
        area,
        &mut buf,
        (Color::Cyan, Color::Blue),
        &[],
        900,
        0.0,
        AmbientCursor::default(),
        WhaleCameo {
            elapsed_ms: Some(500),
            anchor_x: 40,
            anchor_y: 16,
        },
        AmbientActivity::Subagents,
    );
    assert_eq!(buf, before);
    assert_eq!(stats, AmbientFrameStats::default());
}

#[test]
fn pet_settle_clock_does_not_extend_the_ocean_light_pulse() {
    let area = Rect::new(0, 0, 80, 24);
    let ramp = ocean::OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).unwrap();
    let column = |age| {
        OceanColumn::new(
            ramp,
            area,
            900,
            age,
            crate::tui::underwater::ShellPhase::Done,
            false,
            500,
            40,
        )
    };
    let settled = column(None);
    let pet_tail = column(Some(900));
    assert_eq!(pet_tail.completion_elapsed_ms(), Some(900));
    assert_eq!(pet_tail.ramp_fingerprint(), settled.ramp_fingerprint());
    for y in 0..area.height {
        assert_eq!(pet_tail.color_at_y(y), settled.color_at_y(y));
    }
}

#[test]
fn pet_widget_keeps_unknown_distinct_from_sleep_and_keeps_its_label() {
    use pet_sim::{ChannelId, PetSim, PetState};
    use ratatui::widgets::Widget;
    let mut sim = PetSim::whale();
    let mut state = PetState {
        channel: ChannelId::Other,
        lit: 0.3,
        ..PetState::rest()
    };
    sim.step(1.0 / 30.0, &state, false, 1.0);
    assert!(!sim.frame.hollow, "sleep is still observed");
    state.observed = 0.15;
    sim.step(1.0 / 30.0, &state, false, 1.0);
    assert!(sim.frame.hollow);
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 8));
    pet_widget::PetWidget {
        sim: &sim,
        state: &state,
    }
    .render(buf.area, &mut buf);
    let text: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("other · drift · resting"));
    let mut narrow = Buffer::empty(Rect::new(0, 0, 18, 6));
    let before = narrow.clone();
    pet_widget::PetWidget {
        sim: &sim,
        state: &state,
    }
    .render(narrow.area, &mut narrow);
    assert_eq!(narrow, before, "never truncate away the uncertainty cue");
}

#[test]
fn ambient_min_dimensions_allow_small_windows() {
    const {
        assert!(AMBIENT_MIN_WIDTH < 68);
        assert!(AMBIENT_MIN_HEIGHT < 15);
    }
}

#[test]
fn occupied_text_bounds_skips_string_join() {
    let line = Line::from(vec![Span::raw("  hello  "), Span::raw("world  ")]);
    let (start, end) = occupied_text_bounds(&line).expect("bounds");
    assert_eq!(start, 2);
    assert!(end > start);
}

#[test]
fn density_scales_with_area() {
    assert_eq!(
        LifeDensity::from_area(Rect::new(0, 0, 40, 10)),
        LifeDensity::Sparse
    );
    assert_eq!(
        LifeDensity::from_area(Rect::new(0, 0, 100, 30)),
        LifeDensity::Rich
    );
}

#[test]
fn fish_school_uses_one_silhouette_family() {
    // Never mix lone `>` with full fish bodies; the lead just gains an
    // eye within the same family.
    assert_eq!(fish_body(true, false), "><>");
    assert_eq!(fish_body(false, false), "<><");
    assert_eq!(fish_body(true, true), "><o>");
    assert_eq!(fish_body(false, true), "<o><");
}

fn frame_at(t: u128) -> FrameMarks {
    let area = Rect::new(0, 0, 100, 30);
    let mut stats = AmbientFrameStats::default();
    build_frame_marks(
        area,
        t,
        LifeDensity::from_area(area),
        &[],
        AmbientCursor::default(),
        true,
        &mut stats,
    )
}

#[test]
fn subagent_activity_surfaces_a_whale_pod() {
    let whale = WhaleCameo {
        elapsed_ms: Some(500), // Spout phase
        anchor_x: 50,
        anchor_y: 20,
    };
    let render = |activity: AmbientActivity| -> Vec<String> {
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        render_ambient_life(
            buf.area,
            &mut buf,
            (Color::Cyan, Color::Blue),
            &[],
            12_000,
            1.0,
            AmbientCursor::default(),
            whale,
            activity,
        );
        buf.content
            .iter()
            .filter(|cell| {
                cell.symbol()
                    .chars()
                    .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
            })
            .map(|cell| cell.symbol().to_string())
            .collect()
    };
    let baseline = render(AmbientActivity::Baseline);
    let pod = render(AmbientActivity::Subagents);
    assert!(
        pod.len() > baseline.len(),
        "a subagent pod must surface more whale marks ({}) than the single cameo ({})",
        pod.len(),
        baseline.len(),
    );
}

#[test]
fn activity_changes_never_resample_creature_geometry() {
    // Regression: scaling absolute clock age moved the school by tens of
    // columns at a Reasoning → Tools transition. Same clock means same pose;
    // activity still owns ink and the explicitly triggered completion cameo.
    for elapsed in [0, 60_000, 600_000, 3_600_000] {
        let render = |activity| {
            let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
            render_ambient_life(
                buf.area,
                &mut buf,
                (Color::Cyan, Color::Blue),
                &[],
                elapsed,
                1.0,
                AmbientCursor::default(),
                WhaleCameo::default(),
                activity,
            );
            buf
        };
        let baseline = render(AmbientActivity::Baseline);
        for activity in [
            AmbientActivity::Reasoning,
            AmbientActivity::Reading,
            AmbientActivity::Tools,
            AmbientActivity::Subagents,
            AmbientActivity::Verifying,
        ] {
            assert_eq!(
                baseline,
                render(activity),
                "geometry changed at {elapsed}ms: {activity:?}"
            );
        }
    }
}

#[test]
fn fish_always_swim_the_way_they_face() {
    // Wrap-around construction: within one crossing, a right-facing
    // school only ever moves right, and a left-facing school only left.
    let area_travel = 100u128 + 21; // width + wedge span (see constants)
    let cycle_ms = area_travel * SCHOOL_CELL_MS;
    for cycle in 0u128..6 {
        // The school clock carries a half-cycle head start, so sampling
        // at cycle*cycle_ms lands mid-crossing of `cycle`.
        let t1 = cycle * cycle_ms;
        let t2 = t1 + SCHOOL_CELL_MS * 3;
        let lead = |t: u128| {
            frame_at(t)
                .marks
                .into_iter()
                .find(|mark| mark.glyph.contains('o'))
        };
        let (Some(a), Some(b)) = (lead(t1), lead(t2)) else {
            continue; // school off-screen at this sample — fine
        };
        let expect_right = school_swims_right(cycle);
        if expect_right {
            assert_eq!(a.glyph, "><o>", "cycle {cycle} facing");
            assert!(b.x >= a.x, "cycle {cycle}: right-facing fish moved left");
        } else {
            assert_eq!(a.glyph, "<o><", "cycle {cycle} facing");
            assert!(b.x <= a.x, "cycle {cycle}: left-facing fish moved right");
        }
    }
    // Both directions must actually occur across nearby cycles.
    let dirs: Vec<bool> = (0u128..12).map(school_swims_right).collect();
    assert!(
        dirs.iter().any(|d| *d) && dirs.iter().any(|d| !*d),
        "{dirs:?}"
    );
}

#[test]
fn water_holds_only_fish_bubbles_and_jellyfish() {
    // Seaweed and bio-dust are gone (2026-07-23): every mark is a fish
    // body, a bubble glyph, or a jellyfish part.
    for t in [0u128, 7_500, 33_000, 61_000, 120_000] {
        for mark in frame_at(t).marks {
            let ok = matches!(mark.glyph, "><>" | "<><" | "><o>" | "<o><")
                || matches!(mark.glyph, "·" | "˚" | "°")
                || JELLY_DOME_TOP_FRAMES.contains(&mark.glyph)
                || JELLY_DOME_SKIRT_FRAMES.contains(&mark.glyph)
                || JELLY_DOME_TOP_COMPACT.contains(&mark.glyph)
                || JELLY_DOME_SKIRT_COMPACT.contains(&mark.glyph)
                || JELLY_TENTACLE_FRAMES.contains(&mark.glyph);
            assert!(ok, "unexpected ambient glyph {:?} at t={t}", mark.glyph);
        }
    }
}

#[test]
fn ambient_glyphs_are_ascii_or_have_fallbacks() {
    // Every glyph the water can paint is either pure ASCII (fish and
    // the whole jellyfish silhouette, by construction) or carries a
    // glyphs::ascii_fallback entry (bubbles, whale cameo) so
    // CODEWHALE_ASCII_SAFE=1 covers the whole field.
    let mut jellyfish: Vec<&str> = Vec::new();
    jellyfish.extend(JELLY_DOME_TOP_FRAMES);
    jellyfish.extend(JELLY_DOME_SKIRT_FRAMES);
    jellyfish.extend(JELLY_DOME_TOP_COMPACT);
    jellyfish.extend(JELLY_DOME_SKIRT_COMPACT);
    jellyfish.extend(JELLY_TENTACLE_FRAMES);
    for glyph in jellyfish {
        assert!(glyph.is_ascii(), "jellyfish glyph {glyph:?} must be ASCII");
    }
    for glyph in ["><>", "<><", "><o>", "<o><"] {
        assert!(glyph.is_ascii(), "fish glyph {glyph:?} must be ASCII");
    }
    for glyph in ["·", "˚", "°", "≈≈>", "≈", "～"] {
        assert!(
            glyph.is_ascii() || codewhale_tui_kit::glyphs::ascii_fallback(glyph).is_some(),
            "ambient glyph {glyph:?} lacks an ASCII fallback"
        );
    }
}

#[test]
fn jellyfish_reads_as_dome_with_lagging_tentacles() {
    // Find a frame where a full jelly is on-screen and assert its
    // structure: a two-row dome at least four cells wide, exactly two
    // tentacle columns one row below the skirt, and both dome and
    // tentacles holding their brightness floors.
    let mut seen = false;
    for probe in 0..240u128 {
        let t = probe * 500;
        let frame = frame_at(t);
        let Some(top) = frame
            .marks
            .iter()
            .find(|mark| JELLY_DOME_TOP_FRAMES.contains(&mark.glyph))
        else {
            continue;
        };
        let dome_w = UnicodeWidthStr::width(top.glyph) as u16;
        assert!(dome_w >= 4, "rich dome too narrow: {:?}", top.glyph);
        let Some(skirt) = frame.marks.iter().find(|mark| {
            JELLY_DOME_SKIRT_FRAMES.contains(&mark.glyph) && mark.x == top.x && mark.y == top.y + 1
        }) else {
            panic!("visible jellyfish dome lost its skirt: {frame:?}");
        };
        let tentacles: Vec<&AmbientMark> = frame
            .marks
            .iter()
            .filter(|mark| {
                JELLY_TENTACLE_FRAMES.contains(&mark.glyph)
                    && mark.y == skirt.y + 1
                    && mark.x > top.x
                    && mark.x < top.x + dome_w
            })
            .collect();
        assert_eq!(
            tentacles.len(),
            JELLY_TENTACLE_COLUMNS,
            "visible jellyfish must keep all tentacles: {frame:?}"
        );
        let dome_glow = top.brightness.expect("dome pulses");
        assert!(dome_glow >= JELLY_BRIGHTNESS_FLOOR - f32::EPSILON);
        for tentacle in &tentacles {
            let glow = tentacle.brightness.expect("tentacle pulses");
            assert!(glow >= JELLY_BRIGHTNESS_FLOOR - f32::EPSILON);
        }
        seen = true;
        break;
    }
    assert!(seen, "no complete jellyfish found in 120s of frames");
}

#[test]
fn jellyfish_tentacles_sway_out_of_phase_and_dome_pulses() {
    // Over a sweep: the dome shows both pulse frames within a few
    // JELLY_PULSE_MS periods, each tentacle column cycles its sway
    // frames, and the trio is not always in lockstep (per-column phase
    // offset — the lag is what sells "jellyfish").
    let mut dome_frames = std::collections::BTreeSet::new();
    let mut column_frames: [std::collections::BTreeSet<&str>; JELLY_TENTACLE_COLUMNS] =
        Default::default();
    let mut saw_desync = false;
    for probe in 0..480u128 {
        let frame = frame_at(probe * 100);
        let Some(top) = frame
            .marks
            .iter()
            .find(|mark| JELLY_DOME_TOP_FRAMES.contains(&mark.glyph))
        else {
            continue;
        };
        dome_frames.insert(top.glyph);
        let mut pair = [""; JELLY_TENTACLE_COLUMNS];
        let mut found = 0usize;
        for (col, slot) in pair.iter_mut().enumerate() {
            if let Some(tentacle) = frame.marks.iter().find(|mark| {
                JELLY_TENTACLE_FRAMES.contains(&mark.glyph)
                    && mark.y == top.y + 2
                    && mark.x == top.x + 1 + 2 * col as u16
            }) {
                *slot = tentacle.glyph;
                column_frames[col].insert(tentacle.glyph);
                found += 1;
            }
        }
        if found == JELLY_TENTACLE_COLUMNS && pair.iter().any(|glyph| *glyph != pair[0]) {
            saw_desync = true;
        }
    }
    assert_eq!(
        dome_frames.len(),
        JELLY_DOME_TOP_FRAMES.len(),
        "dome pulse should show every frame: {dome_frames:?}"
    );
    for (col, set) in column_frames.iter().enumerate() {
        assert!(set.len() > 1, "tentacle column {col} never swayed");
    }
    assert!(saw_desync, "tentacle columns strobed in lockstep");
}

/// Tentacle columns hanging from the full-size bell. Named so the structural
/// assertions read as one decision rather than a repeated magic number.
const JELLY_TENTACLE_COLUMNS: usize = 2;

#[test]
fn short_transcript_rows_leave_a_safe_ocean_corridor() {
    // The active transcript commonly puts short status/tool lines on every
    // visible row. They own their cells and a one-column gutter, not the whole
    // terminal width: fish and the t=0 jellyfish remain eligible in the clear
    // right-hand water without touching a single transcript cell.
    let area = Rect::new(0, 0, 80, 24);
    let lines: Vec<Line<'static>> = (0..usize::from(area.height))
        .map(|row| Line::from(Span::raw(format!("status row {row:02}"))))
        .collect();
    let mut buf = Buffer::empty(area);
    for (row, line) in lines.iter().enumerate() {
        buf.set_line(area.x, area.y + row as u16, line, area.width);
    }

    let mut stats = AmbientFrameStats::default();
    let frame = build_frame_marks(
        area,
        0,
        LifeDensity::from_area(area),
        &lines,
        AmbientCursor::default(),
        true,
        &mut stats,
    );
    paint_marks(
        area,
        &mut buf,
        (Color::Cyan, Color::Blue),
        &lines,
        &frame,
        1.0,
        &mut stats,
    );
    let rendered: String = (0..area.height)
        .map(|row| {
            (0..area.width)
                .map(|column| buf[(column, row)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("><") || rendered.contains("<o"),
        "short active rows should leave room for fish:\n{rendered}"
    );
    assert!(
        rendered.contains(JELLY_DOME_TOP_FRAMES[0]) || rendered.contains(JELLY_DOME_TOP_FRAMES[1]),
        "the right corridor should keep the rare visitor eligible at t=0:\n{rendered}"
    );
    assert!(stats.marks_painted > 0, "{stats:?}");
    for (row, line) in lines.iter().enumerate() {
        let text = line.spans[0].content.as_ref();
        for (column, ch) in text.chars().enumerate() {
            assert_eq!(
                buf[(column as u16, row as u16)].symbol(),
                ch.to_string(),
                "ambient life overwrote transcript cell ({column},{row})"
            );
        }
    }
}

#[test]
fn a_jellyfish_needs_water_deep_enough_to_hold_it_and_the_school() {
    // Deep-water budget: with the composition sitting where the 80x24 idle
    // screen puts it there are four rows of water left, which is the school's
    // band and nothing else. The jellyfish does not come up into it.
    let area = Rect::new(0, 0, 80, 18);
    let mut lines: Vec<Line<'static>> = vec![Line::default(); usize::from(area.height)];
    for row in [9usize, 10, 12] {
        lines[row] = Line::from(Span::raw("X".repeat(75)));
    }
    let jelly_lane = area.width.saturating_mul(5) / 6;
    assert!(deep_water_rows(area, &lines, jelly_lane, 5) < JELLY_MIN_DEEP_ROWS);
    let mut saw_fish = false;
    for probe in 0..600u128 {
        let mut stats = AmbientFrameStats::default();
        let frame = build_frame_marks(
            area,
            probe * 250,
            LifeDensity::from_area(area),
            &lines,
            AmbientCursor::default(),
            true,
            &mut stats,
        );
        for mark in &frame.marks {
            assert!(
                !JELLY_DOME_TOP_FRAMES.contains(&mark.glyph)
                    && !JELLY_DOME_SKIRT_FRAMES.contains(&mark.glyph)
                    && !JELLY_TENTACLE_FRAMES.contains(&mark.glyph),
                "shallow water surfaced a jellyfish: {mark:?}"
            );
            saw_fish |= mark.glyph.contains("><");
        }
    }
    assert!(saw_fish, "the school should still hold its band");
}

#[test]
fn bubbles_grow_as_they_rise_and_dissolve_before_the_top() {
    // Size and brightness are functions of height risen, not of the clock:
    // the old table swapped glyph every 320 ms in place, which is a flicker
    // rather than a rise, and the old clamp parked a spent bubble as an
    // unattached speck at the top of the field.
    assert_eq!(bubble_glyph(0), "·");
    assert_eq!(bubble_glyph(BUBBLE_MAX_RISE_ROWS), "°");
    assert!(bubble_dissolve(0) > bubble_dissolve(BUBBLE_MAX_RISE_ROWS));
    assert!((bubble_dissolve(0) - 1.0).abs() < f32::EPSILON);
    assert!(bubble_dissolve(BUBBLE_MAX_RISE_ROWS) <= BUBBLE_DISSOLVE_CEIL + f32::EPSILON);
}

#[test]
fn sparse_water_gets_a_compact_jellyfish() {
    // Narrow fallback: the Sparse tier swaps the full 5-cell dome for a
    // 3-cell one with two tentacles — a different silhouette, not just
    // fewer jellies.
    let area = Rect::new(0, 0, 48, 12);
    let mut saw_compact = false;
    let mut saw_full = false;
    let mut saw_two_tentacles = false;
    for probe in 0..240u128 {
        let mut stats = AmbientFrameStats::default();
        let frame = build_frame_marks(
            area,
            probe * 500,
            LifeDensity::from_area(area),
            &[],
            AmbientCursor::default(),
            true,
            &mut stats,
        );
        for mark in &frame.marks {
            saw_compact |= JELLY_DOME_TOP_COMPACT.contains(&mark.glyph);
            saw_full |= JELLY_DOME_TOP_FRAMES.contains(&mark.glyph);
        }
        let Some(top) = frame
            .marks
            .iter()
            .find(|mark| JELLY_DOME_TOP_COMPACT.contains(&mark.glyph))
        else {
            continue;
        };
        let tentacles = frame
            .marks
            .iter()
            .filter(|mark| JELLY_TENTACLE_FRAMES.contains(&mark.glyph) && mark.y == top.y + 2)
            .count();
        assert_eq!(
            tentacles, 2,
            "visible compact jelly must keep both tentacles: {frame:?}"
        );
        saw_two_tentacles = true;
    }
    assert!(saw_compact, "sparse water never showed the compact dome");
    assert!(!saw_full, "sparse water used the full-size dome");
    assert!(saw_two_tentacles, "compact jelly lost its tentacles");
}

#[test]
fn animated_tentacle_frames_never_collapse_to_punctuation_dots() {
    assert!(
        JELLY_TENTACLE_FRAMES
            .iter()
            .all(|glyph| matches!(*glyph, "|" | "/" | "\\")),
        "every animation frame must retain a legible tentacle stroke"
    );
}

#[test]
fn motion_is_a_deterministic_function_of_elapsed_time() {
    // v0.9.4: positions always ride the monotonic clock (presence fades the
    // marks in/out instead of parking them), so a fixed t must produce the
    // exact same complete jellyfish: dome, skirt, and tentacles visible.
    let area = Rect::new(0, 0, 100, 30);
    let build = || {
        let mut stats = AmbientFrameStats::default();
        build_frame_marks(
            area,
            0,
            LifeDensity::from_area(area),
            &[],
            AmbientCursor::default(),
            true,
            &mut stats,
        )
    };
    let first = build();
    let second = build();
    let pose = |frame: &FrameMarks| {
        frame
            .marks
            .iter()
            .map(|mark| (mark.glyph, mark.x, mark.y))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        pose(&first),
        pose(&second),
        "motion must be a pure function of t"
    );
    let top = first
        .marks
        .iter()
        .find(|mark| JELLY_DOME_TOP_FRAMES.contains(&mark.glyph))
        .expect("jellyfish dome at t=0");
    assert!(
        first
            .marks
            .iter()
            .any(|mark| { JELLY_DOME_SKIRT_FRAMES.contains(&mark.glyph) && mark.y == top.y + 1 }),
        "jellyfish lost its skirt at t=0"
    );
    let tentacles = first
        .marks
        .iter()
        .filter(|mark| JELLY_TENTACLE_FRAMES.contains(&mark.glyph) && mark.y == top.y + 2)
        .count();
    assert!(
        tentacles >= JELLY_TENTACLE_COLUMNS,
        "jellyfish lost its tentacles at t=0"
    );
}

#[test]
fn glow_helpers_stay_bounded_with_floors() {
    for t in (0u128..12_000).step_by(97) {
        let w = wave01(t, FISH_WAVE_MS, 0);
        assert!((0.0..=1.0).contains(&w), "wave01 out of range: {w}");
        let g = glint01(t, 2_600, 600, BUBBLE_BRIGHTNESS_FLOOR, 0);
        assert!(
            (BUBBLE_BRIGHTNESS_FLOOR..=1.0).contains(&g),
            "glint01 lost its floor: {g}"
        );
    }
}

#[test]
fn frame_stats_account_for_every_mark() {
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    let stats = render_ambient_life(
        area,
        &mut buf,
        (Color::Cyan, Color::Blue),
        &[],
        12_000,
        1.0,
        AmbientCursor::default(),
        WhaleCameo::default(),
        AmbientActivity::Baseline,
    );
    assert_eq!(
        stats.marks_built,
        stats.marks_painted + stats.marks_skipped_text + stats.marks_clipped,
        "every built mark is painted, text-skipped, or clipped: {stats:?}"
    );
    assert!(stats.marks_painted > 0, "empty water should paint life");
    // cells_written counts each cell of a multi-cell glyph (and counts
    // a shared cell once per overlapping mark), so the honest bound is
    // per painted mark — at most the widest glyph (the 5-cell dome) —
    // rather than per area cell.
    assert!(stats.cells_written >= stats.marks_painted);
    assert!(
        stats.cells_written <= stats.marks_painted * 5,
        "cells_written out of proportion: {stats:?}"
    );
}

#[test]
fn frame_stats_stay_within_the_render_budget() {
    // Largest Rich field with three bounded dot-whale widgets. Every
    // visible dot cell and label character participates in the same budget.
    let area = Rect::new(0, 0, 160, 40);
    let mut buf = Buffer::empty(area);
    let whale = WhaleCameo {
        elapsed_ms: Some(500), // Spout: cameo glyph plus spray
        anchor_x: 80,
        anchor_y: 26,
    };
    let stats = render_ambient_life(
        area,
        &mut buf,
        (Color::Cyan, Color::Blue),
        &[],
        12_000,
        1.0,
        AmbientCursor::default(),
        whale,
        AmbientActivity::Subagents,
    );
    assert!(
        stats.marks_built <= MAX_FRAME_MARKS,
        "frame budget blown: {stats:?}"
    );
    assert_eq!(
        stats.marks_built,
        stats.marks_painted + stats.marks_skipped_text + stats.marks_clipped,
        "every built mark is accounted for: {stats:?}"
    );
}

#[test]
fn frame_stats_never_overwrite_text() {
    // Property: on a text-covered field, colliding marks are charged to
    // marks_skipped_text and no transcript cell is overwritten.
    //
    // Two stages guard this and they answer different questions. The build
    // stage keeps ambient life out of the composition's rows entirely
    // (`is_open_water`) — a design decision about where the aquarium lives.
    // The paint stage refuses to write over text whatever the build stage
    // decided — a correctness backstop, and the one this test exercises,
    // using the whale cameo: the cameo is an event fired at a deliberate
    // anchor, so it is the one mark that is allowed to aim at occupied water
    // and must therefore be withheld here rather than painted.
    let area = Rect::new(0, 0, 100, 30);
    let mut buf = Buffer::empty(area);
    let lines: Vec<Line<'static>> = (0..usize::from(area.height))
        .map(|_| Line::from(Span::raw("X".repeat(usize::from(area.width)))))
        .collect();
    for (i, line) in lines.iter().enumerate() {
        buf.set_line(area.x, area.y + i as u16, line, area.width);
    }
    let stats = render_ambient_life(
        area,
        &mut buf,
        (Color::Cyan, Color::Blue),
        &lines,
        12_000,
        1.0,
        AmbientCursor::default(),
        WhaleCameo {
            elapsed_ms: Some(500),
            anchor_x: 10,
            anchor_y: 20,
        },
        AmbientActivity::Baseline,
    );
    assert!(
        stats.marks_skipped_text > 0,
        "full-width text should skip colliding marks: {stats:?}"
    );
    assert_eq!(
        stats.marks_painted, 0,
        "a fully composed field is not water: {stats:?}"
    );
    assert_eq!(
        stats.marks_built,
        stats.marks_painted + stats.marks_skipped_text + stats.marks_clipped,
        "every built mark is accounted for: {stats:?}"
    );
    for (i, line) in lines.iter().enumerate() {
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        for (x, ch) in text.chars().enumerate() {
            assert_eq!(
                buf[(area.x + x as u16, area.y + i as u16)].symbol(),
                ch.to_string(),
                "ambient life overwrote transcript cell ({x},{i})"
            );
        }
    }
}

fn full_jellyfish_frame(x: u16, y: u16) -> FrameMarks {
    let mark = |x, y, glyph, depth| AmbientMark {
        x,
        y,
        glyph,
        jellyfish: Some(0),
        depth,
        style_mod: None,
        brightness: None,
    };
    FrameMarks {
        marks: vec![
            mark(x, y, JELLY_DOME_TOP_FRAMES[0], Depth::Midground),
            mark(x, y + 1, JELLY_DOME_SKIRT_FRAMES[0], Depth::Midground),
            mark(x + 1, y + 2, "|", Depth::Background),
            mark(x + 2, y + 2, "/", Depth::Background),
            mark(x + 3, y + 2, "\\", Depth::Background),
        ],
    }
}

fn paint_fixture(
    area: Rect,
    lines: &[Line<'static>],
    frame: &FrameMarks,
) -> (Buffer, AmbientFrameStats) {
    let mut buf = Buffer::empty(area);
    for (row, line) in lines.iter().enumerate() {
        buf.set_line(area.x, area.y + row as u16, line, area.width);
    }
    let mut stats = AmbientFrameStats {
        marks_built: frame.marks.len() as u32,
        ..AmbientFrameStats::default()
    };
    paint_marks(
        area,
        &mut buf,
        (Color::Cyan, Color::Blue),
        lines,
        frame,
        1.0,
        &mut stats,
    );
    (buf, stats)
}

#[test]
fn jellyfish_relocates_as_one_nearest_silhouette() {
    let area = Rect::new(0, 0, 24, 8);
    let mut lines = vec![Line::default(); usize::from(area.height)];
    lines[2] = Line::from(Span::raw("          X"));
    let (buf, stats) = paint_fixture(area, &lines, &full_jellyfish_frame(10, 2));

    assert_eq!(buf[(10, 2)].symbol(), "X");
    assert_eq!(buf[(12, 2)].symbol(), ".");
    assert_eq!(buf[(12, 3)].symbol(), "\\");
    assert_eq!(buf[(13, 4)].symbol(), "|");
    assert_eq!(buf[(14, 4)].symbol(), "/");
    assert_eq!(buf[(15, 4)].symbol(), "\\");
    assert_eq!(stats.marks_painted, 5);
    assert_eq!(stats.marks_skipped_text, 0);
    assert_eq!(stats.marks_clipped, 0);
    assert_eq!(stats.cells_written, 13);
}

#[test]
fn jellyfish_hides_rather_than_vaulting_past_a_long_line() {
    // This used to place the silhouette at x=33 — a 17-column vault from its
    // lane, chosen purely from the width of the text on that row. Under a
    // fast stream that width changes every frame, so the vault was the
    // teleport users saw. Past the dodge cap the jelly is withheld instead,
    // which is the same quiet outcome the fish already have.
    let area = Rect::new(0, 0, 100, 8);
    let mut lines = vec![Line::default(); usize::from(area.height)];
    lines[2] = Line::from(Span::raw("X".repeat(32)));
    let (_, stats) = paint_fixture(area, &lines, &full_jellyfish_frame(16, 2));

    assert_eq!(stats.marks_painted, 0);
    assert_eq!(stats.marks_skipped_text, 5);
    assert_eq!(stats.marks_clipped, 0);
}

#[test]
fn jellyfish_never_teleports_while_a_line_streams_across_its_lane() {
    // Regression for the DeepSeek-V4-Flash report: replay a line growing
    // straight through the silhouette's lane and assert the painted anchor
    // never jumps. The clock is held fixed, so the only moving input is the
    // transcript text — the coupling that actually scaled with token
    // throughput.
    //
    // The burst size is the whole point. A slow provider adds about a
    // character per frame and the old unbounded dodge slid along with it,
    // which is why this never looked broken on slow models. A fast one adds a
    // burst per frame, and the dodge moved by the whole burst at once.
    const STREAM_BURST_CHARS: usize = 9;
    let area = Rect::new(0, 0, 100, 8);
    let lane = 40u16;
    let mut previous: Option<u16> = None;
    for chars in (0..80usize).step_by(STREAM_BURST_CHARS) {
        let mut lines = vec![Line::default(); usize::from(area.height)];
        lines[2] = Line::from(Span::raw("X".repeat(chars)));
        lines[3] = Line::from(Span::raw("X".repeat(chars.saturating_sub(3))));
        let (buf, _) = paint_fixture(area, &lines, &full_jellyfish_frame(lane, 2));
        let anchor = (0..area.width).find(|x| buf[(*x, 2)].symbol() == ".");
        if let (Some(anchor), Some(previous)) = (anchor, previous) {
            assert!(
                anchor.abs_diff(previous) <= 2 * JELLY_MAX_TEXT_DODGE_COLS,
                "jellyfish teleported {} columns at {chars} streamed chars",
                anchor.abs_diff(previous)
            );
        }
        if let Some(anchor) = anchor {
            assert!(
                anchor.abs_diff(lane) <= JELLY_MAX_TEXT_DODGE_COLS,
                "jellyfish left its lane by {} columns",
                anchor.abs_diff(lane)
            );
        }
        previous = anchor;
    }
}

#[test]
fn sparse_jellyfish_row_keeps_a_safe_gap_over_text() {
    let area = Rect::new(0, 0, 12, 6);
    let mut lines = vec![Line::default(); usize::from(area.height)];
    lines[2] = Line::from(Span::raw("   X"));
    let mark = |x| AmbientMark {
        x,
        y: 2,
        glyph: "|",
        jellyfish: Some(0),
        depth: Depth::Background,
        style_mod: None,
        brightness: None,
    };
    let frame = FrameMarks {
        marks: vec![mark(0), mark(6)],
    };
    let (buf, stats) = paint_fixture(area, &lines, &frame);

    assert_eq!(buf[(0, 2)].symbol(), "|");
    assert_eq!(buf[(3, 2)].symbol(), "X");
    assert_eq!(buf[(6, 2)].symbol(), "|");
    assert_eq!(stats.marks_painted, 2);
}

#[test]
fn fully_blocked_jellyfish_is_suppressed_and_accounted() {
    let area = Rect::new(0, 0, 24, 8);
    let mut lines = vec![Line::default(); usize::from(area.height)];
    for line in lines.iter_mut().take(5).skip(2) {
        *line = Line::from(Span::raw("X".repeat(24)));
    }
    let (buf, stats) = paint_fixture(area, &lines, &full_jellyfish_frame(10, 2));

    assert_eq!(stats.marks_painted, 0);
    assert_eq!(stats.marks_skipped_text, 5);
    assert_eq!(stats.marks_clipped, 0);
    assert_eq!(stats.cells_written, 0);
    assert_eq!(stats.marks_built, stats.marks_skipped_text);
    for row in 2..=4 {
        for column in 0..area.width {
            assert_eq!(buf[(column, row)].symbol(), "X");
        }
    }
}

#[test]
fn too_wide_jellyfish_is_truthfully_clipped_as_a_group() {
    let area = Rect::new(0, 0, 4, 8);
    let lines = vec![Line::default(); usize::from(area.height)];
    let (_, stats) = paint_fixture(area, &lines, &full_jellyfish_frame(0, 2));

    assert_eq!(stats.marks_painted, 0);
    assert_eq!(stats.marks_skipped_text, 0);
    assert_eq!(stats.marks_clipped, 5);
    assert_eq!(stats.marks_built, stats.marks_clipped);
}

#[test]
fn caustic_brightness_cross_fades_without_80ms_steps() {
    let samples: Vec<f32> = (0..=120)
        .map(|frame| caustic_brightness(frame * 16, 18, 2, 1.0))
        .collect();

    assert!(samples.iter().all(|value| (1.0..=1.08).contains(value)));
    assert!(
        samples
            .windows(2)
            .all(|pair| (pair[1] - pair[0]).abs() < 0.02),
        "adjacent 60 FPS caustic frames must cross-fade instead of toggling"
    );
    assert!(
        samples.windows(2).any(|pair| pair[0] != pair[1]),
        "the continuous caustic must still move"
    );
    assert_eq!(
        caustic_brightness(0, 0, 0, 1.0),
        caustic_brightness(0, 12, 0, 1.0),
        "the authored caustic topology repeats every 12 columns"
    );
    assert_eq!(samples.first(), samples.get(60), "960 ms closes one cycle");

    let base = Color::Rgb(64, 96, 128);
    let colors: Vec<Color> = (0..=60)
        .map(|frame| ocean::scale_color(base, caustic_brightness(frame * 16, 18, 2, 1.0)))
        .collect();
    assert!(
        colors.windows(2).any(|pair| pair[0] != pair[1]),
        "the cross-fade must survive truecolor channel quantization"
    );
    assert_eq!(colors.first(), colors.last(), "RGB output closes one cycle");

    // Depth fade: the light dissolves to nothing at the band floor instead of
    // stopping at full amplitude — the hard cutoff drew a visible horizontal
    // line across tall windows.
    for frame in 0..=60u128 {
        assert_eq!(
            caustic_brightness(frame * 16, 18, 2, 0.0),
            1.0,
            "a fully faded caustic must leave the water untouched"
        );
    }
}

fn native_frame_at(area: Rect, elapsed_ms: u128) -> FrameMarks {
    build_frame_marks(
        area,
        elapsed_ms,
        LifeDensity::from_area(area),
        &[],
        AmbientCursor::default(),
        false,
        &mut AmbientFrameStats::default(),
    )
}

#[test]
fn native_pose_tables_have_fixed_cell_bounds_and_safe_glyphs() {
    for right in [false, true] {
        for pose in 0..4 {
            for dx in 0..2 {
                for dy in 0..2 {
                    let row = native_poses::fish(right, pose, dx, dy);
                    assert_eq!(row.width(), 4);
                    assert!(row.chars().all(|c| c == ' '
                        || codewhale_tui_kit::glyphs::braille_ascii_fallback(c).is_some()));
                }
            }
        }
    }
    for pose in 0..16 {
        for dx in 0..2 {
            for dy in 0..4 {
                let rows = native_poses::jelly(pose, dx, dy);
                assert_eq!(rows.len(), 3);
                for row in rows {
                    assert_eq!(row.width(), 5);
                    assert!(row.chars().all(|c| c == ' '
                        || codewhale_tui_kit::glyphs::braille_ascii_fallback(c).is_some()));
                }
                assert!(rows.iter().all(|r| r.chars().count() == 5));
            }
        }
    }
}

#[test]
fn native_fish_move_within_the_old_whole_cell_dwell() {
    let area = Rect::new(0, 0, 100, 30);
    let a = native_frame_at(area, 190);
    let b = native_frame_at(area, 380);
    let (a, b) = (&a.marks[0], &b.marks[0]);
    assert_eq!(
        (a.x, a.y),
        (b.x, b.y),
        "both samples share the same cell anchor"
    );
    assert_ne!(
        a.glyph, b.glyph,
        "half-cell travel must change the raster within a cell"
    );
}

#[test]
fn native_motion_sampling_is_independent_of_cadence_and_history() {
    let area = Rect::new(0, 0, 100, 30);
    let pose = |t| {
        native_frame_at(area, t)
            .marks
            .iter()
            .map(|mark| (mark.x, mark.y, mark.glyph))
            .collect::<Vec<_>>()
    };
    // Deliberately skip, reverse and change the draw cadence before landing
    // on shared timestamps. A per-paint simulation would make these differ.
    for target in [500, 1_000, 5_200, 9_400, 56_400, 300_800] {
        let expected = pose(target);
        for step in [16, 33, 117] {
            for t in (0..target as usize).step_by(step) {
                let _ = pose(t as u128);
            }
            assert_eq!(expected, pose(target));
        }
        let _ = pose(target + 10_000);
        assert_eq!(expected, pose(target));
    }
}

#[test]
fn native_temporal_sweep_retains_population_and_paint_bounds() {
    for area in [
        Rect::new(0, 0, 40, 12),
        Rect::new(0, 0, 80, 24),
        Rect::new(0, 0, 140, 40),
    ] {
        // Cover rise, departure, long absence and next arrival, not just t=0.
        let mut saw_jelly = false;
        let mut saw_absence = false;
        for t in (0..310_000).step_by(250) {
            let frame = native_frame_at(area, t);
            let count = frame.marks.iter().filter(|m| m.jellyfish.is_some()).count();
            assert!(
                count == 0 || count == 3,
                "native jelly must be one complete group at {t}"
            );
            saw_jelly |= count == 3;
            saw_absence |= count == 0;
            // Bound from the one constant, not a second copy of the
            // number: a hardcoded 13 here outlived the budget it was meant
            // to mirror when the bubble streams grew.
            assert!(
                frame.marks.len() as u32 <= super::MAX_FRAME_MARKS,
                "frame built {} marks, budget is {}",
                frame.marks.len(),
                super::MAX_FRAME_MARKS
            );
            let mut buf = Buffer::empty(area);
            let mut stats = AmbientFrameStats {
                marks_built: frame.marks.len() as u32,
                ..Default::default()
            };
            paint_marks(
                area,
                &mut buf,
                (Color::Cyan, Color::Blue),
                &[],
                &frame,
                1.0,
                &mut stats,
            );
            assert_eq!(
                stats.marks_built,
                stats.marks_painted + stats.marks_skipped_text + stats.marks_clipped
            );
            assert!(stats.cells_written <= stats.marks_painted * 5);
        }
        assert!(
            saw_jelly && saw_absence,
            "visit lifecycle missing at {area:?}"
        );
    }
}

#[test]
fn native_jelly_stays_whole_as_wide_text_streams_through_its_lane() {
    let area = Rect::new(0, 0, 80, 24);
    let mut frame = native_frame_at(area, 0);
    frame.marks.retain(|mark| mark.jellyfish.is_some());
    assert_eq!(frame.marks.len(), 3);
    let y = frame.marks[0].y;
    for width in 0..=40 {
        let mut lines = vec![Line::default(); area.height as usize];
        lines[y as usize] = Line::from("鲸".repeat(width));
        let mut buf = Buffer::empty(area);
        buf.set_line(0, y, &lines[y as usize], area.width);
        let before = buf.clone();
        let mut stats = AmbientFrameStats {
            marks_built: 3,
            ..Default::default()
        };
        paint_marks(
            area,
            &mut buf,
            (Color::Cyan, Color::Blue),
            &lines,
            &frame,
            1.0,
            &mut stats,
        );
        assert!(
            stats.marks_painted == 0 || stats.marks_painted == 3,
            "partial jelly at width {width}"
        );
        for x in 0..(width as u16 * 2).min(area.width) {
            assert_eq!(buf[(x, y)], before[(x, y)], "painted over wide text at {x}");
        }
    }
}

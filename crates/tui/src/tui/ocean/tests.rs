use super::*;

fn distance(a: Color, b: Color) -> u16 {
    let (ar, ag, ab) = rgb(a).expect("RGB color");
    let (br, bg, bb) = rgb(b).expect("RGB color");
    ar.abs_diff(br) as u16 + ag.abs_diff(bg) as u16 + ab.abs_diff(bb) as u16
}

fn relative_luminance(value: Color) -> f64 {
    let (r, g, b) = rgb(value).expect("contrast colors must be RGB");
    let linearize = |component: u8| {
        let srgb = f64::from(component) / 255.0;
        if srgb <= 0.04045 {
            srgb / 12.92
        } else {
            ((srgb + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b)
}

fn contrast_ratio(foreground: Color, background: Color) -> f64 {
    let foreground = relative_luminance(foreground);
    let background = relative_luminance(background);
    let (lighter, darker) = if foreground >= background {
        (foreground, background)
    } else {
        (background, foreground)
    };
    (lighter + 0.05) / (darker + 0.05)
}

#[test]
fn whale_ramp_is_perceptibly_deep_not_merely_non_equal() {
    let ramp =
        OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).expect("underwater ramp");
    assert_eq!(ramp.surface, Color::Rgb(0x0b, 0x2c, 0x53));
    assert_eq!(ramp.middle, Color::Rgb(0x09, 0x22, 0x41));
    assert_eq!(ramp.deep, Color::Rgb(0x06, 0x13, 0x20));
    assert!(
        distance(ramp.surface, ramp.deep) >= 32,
        "the selected underwater treatment must read at a glance"
    );
    assert_ne!(
        ramp.color_at_context(0, 20, 0),
        ramp.color_at_context(19, 20, 0)
    );
}

#[test]
fn whale_column_stays_blue_and_gently_banded_at_full_screen_depth() {
    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    let ramp = OceanRamp::for_theme(&theme).expect("underwater ramp");
    let mut previous = ramp.color_at_context(0, 80, 0);

    for row in 0..80 {
        let current = ramp.color_at_context(row, 80, 0);
        let (red, green, blue) = rgb(current).expect("RGB ocean color");
        assert!(
            blue > green && green > red,
            "row {row} lost the authored blue-ocean ordering: {current:?}"
        );
        assert!(
            relative_luminance(current) > relative_luminance(codewhale_palette::WHALE_BG),
            "row {row} fell back into the near-black shell field"
        );
        assert!(
            distance(previous, current) <= 4,
            "row {row} introduced a hard depth seam"
        );
        previous = current;
    }

    assert_eq!(ramp.color_at_context(0, 80, 0), ramp.surface);
    assert_eq!(ramp.color_at_context(79, 80, 0), ramp.deep);
}

#[test]
fn whale_ocean_keeps_text_and_semantic_roles_readable() {
    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    let ramp = OceanRamp::for_theme(&theme).expect("underwater ramp");
    let foregrounds = [
        ("body", theme.text_body),
        ("soft", theme.text_soft),
        ("muted", theme.text_muted),
        ("hint", theme.text_hint),
        ("action", theme.accent_primary),
        ("live", theme.status_working),
        ("human", theme.accent_action),
        ("warning", theme.warning),
        ("danger", theme.error_fg),
        ("act mode", theme.mode_agent),
        ("plan mode", theme.mode_plan),
        ("operate", theme.mode_operate),
        ("full-access mode", theme.mode_yolo),
        ("success", theme.success),
    ];

    for (background_name, background) in [
        ("ocean surface", ramp.surface),
        ("ocean middle", ramp.middle),
        ("ocean deep", ramp.deep),
    ] {
        for (foreground_name, foreground) in foregrounds {
            let ratio = contrast_ratio(foreground, background);
            assert!(
                ratio >= 4.5,
                "Whale {foreground_name} on {background_name} contrast {ratio:.2} is below 4.50"
            );
        }
    }
}

#[test]
fn underwater_custom_background_keeps_the_field() {
    let custom = Color::Rgb(0x12, 0x1a, 0x2d);
    let theme = codewhale_palette::UNDERWATER_UI_THEME.with_background_color(custom);
    let ramp = OceanRamp::for_theme(&theme).expect("the field survives a background override");

    assert_ne!(ramp.surface, ramp.deep);
}

#[test]
fn themes_other_than_underwater_own_no_field() {
    use codewhale_palette::{SELECTABLE_THEMES, ThemeId};

    for id in SELECTABLE_THEMES {
        let ramp = OceanRamp::for_theme(&id.ui_theme());
        if matches!(id, ThemeId::Underwater) {
            assert!(ramp.is_some(), "{} owns the painted field", id.name());
        } else {
            assert_eq!(
                ramp,
                None,
                "{} must leave the terminal's ground alone",
                id.name()
            );
        }
    }

    // A custom background repaints surfaces; it never grants a field to a
    // theme that does not own one.
    let custom = Color::Rgb(0x12, 0x1a, 0x2d);
    assert_eq!(
        OceanRamp::for_theme(&codewhale_palette::UI_THEME.with_background_color(custom)),
        None
    );
}

#[test]
fn inherited_terminal_background_reports_no_ramp() {
    assert_eq!(
        OceanRamp::for_theme(&codewhale_palette::TERMINAL_UI_THEME),
        None
    );
}

#[test]
fn solarized_light_preserves_its_canonical_base3_background() {
    let theme = codewhale_palette::SOLARIZED_LIGHT_UI_THEME;

    assert_eq!(theme.surface_bg, Color::Rgb(0xfd, 0xf6, 0xe3));
    assert_eq!(OceanRamp::for_theme(&theme), None);
}

#[test]
fn solarized_light_custom_background_stays_field_free() {
    let custom = Color::Rgb(0x1a, 0x1b, 0x26);
    let theme = codewhale_palette::SOLARIZED_LIGHT_UI_THEME.with_background_color(custom);
    assert_eq!(OceanRamp::for_theme(&theme), None);
    assert_eq!(theme.surface_bg, custom);
}

#[test]
fn terminal_native_themes_keep_reset_shells_while_underwater_paints_the_column() {
    for theme in [
        codewhale_palette::UI_THEME,
        codewhale_palette::LIGHT_UI_THEME,
    ] {
        assert_eq!(
            OceanRamp::for_theme(&theme),
            None,
            "{} must not grow a field",
            theme.name
        );
        for shell_surface in [
            theme.surface_bg,
            theme.panel_bg,
            theme.composer_bg,
            theme.header_bg,
            theme.footer_bg,
        ] {
            assert_eq!(shell_surface, Color::Reset, "{} shell", theme.name);
        }
    }

    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    let ramp = OceanRamp::for_theme(&theme).expect("underwater ramp");
    for painted in [ramp.surface, ramp.middle, ramp.deep, ramp.ambient] {
        assert_ne!(painted, Color::Reset, "underwater paint");
    }

    let area = Rect::new(0, 0, 4, 4);
    let mut buf = Buffer::empty(area);
    // The shell has already painted its surface; paint_matching only re-inks
    // cells wearing that exact background.
    for y in 0..area.height {
        for x in 0..area.width {
            buf[(x, y)].set_bg(theme.surface_bg);
        }
    }
    let column = OceanColumn::new(ramp, area, 0, None, ShellPhase::Idle, false, 0, 0);
    column.paint_matching(area, &mut buf, theme.surface_bg);
    assert_ne!(buf[(0, 0)].bg, Color::Reset);
    assert_ne!(buf[(0, area.height - 1)].bg, Color::Reset);
    assert_ne!(buf[(0, 0)].bg, buf[(0, area.height - 1)].bg);
}

#[test]
fn ambient_ink_matches_sunk_sky_shades_and_survives_reset_surfaces() {
    // The underwater theme's authored RGB ramp gives its shell two sunk sky
    // shades; seafoam remains live-work ink.
    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    let ramp = OceanRamp::for_theme(&theme).expect("underwater ramp");
    let baseline = crate::tui::ambient_life::AmbientActivity::Baseline;
    let (primary, secondary) = ambient_inks_for_activity(&theme, baseline);
    assert_ne!(primary, ramp.ambient);
    assert_ne!(primary, secondary);
    assert_ne!(primary, theme.accent_secondary);

    // Terminal-owned surfaces have no RGB base; the raw secondary accent
    // lets the terminal's own palette color the life.
    let terminal = codewhale_palette::TERMINAL_UI_THEME;
    assert_eq!(
        ambient_inks_for_activity(&terminal, baseline),
        (terminal.info, terminal.info)
    );
}

#[test]
fn ambient_ink_reads_the_activity_at_a_glance() {
    use crate::tui::ambient_life::AmbientActivity;
    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    let baseline = ambient_inks_for_activity(&theme, AmbientActivity::Baseline);
    let reasoning = ambient_inks_for_activity(&theme, AmbientActivity::Reasoning);
    let tools = ambient_inks_for_activity(&theme, AmbientActivity::Tools);
    let subagents = ambient_inks_for_activity(&theme, AmbientActivity::Subagents);

    // Each activity wears its own water: dim deep for reasoning, bright
    // current for tools, seafoam for orchestration.
    assert_ne!(reasoning, baseline);
    assert_ne!(tools, baseline);
    assert_ne!(subagents, baseline);
    assert_ne!(subagents, tools);
    // Verifying keeps the metered baseline treatment.
    assert_eq!(
        ambient_inks_for_activity(&theme, AmbientActivity::Verifying),
        baseline
    );
}

#[test]
fn attention_phases_tint_the_water_even_when_life_has_settled() {
    let viewport = Rect::new(0, 0, 80, 24);
    let ramp =
        OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).expect("underwater ramp");
    // presence 0 + animated false is the fully settled, reduced-motion case —
    // exactly where the old treatment went neutral and a blocked session was
    // indistinguishable from an idle one across the room.
    let waiting = OceanColumn::new(ramp, viewport, 0, None, ShellPhase::Approval, false, 0, 0);
    let failed = OceanColumn::new(ramp, viewport, 0, None, ShellPhase::Failed, false, 0, 0);
    let idle = OceanColumn::new(ramp, viewport, 0, None, ShellPhase::Idle, false, 0, 0);

    assert_ne!(waiting.color_at_y(0), idle.color_at_y(0));
    assert_ne!(failed.color_at_y(0), idle.color_at_y(0));
    assert_ne!(waiting.color_at_y(0), failed.color_at_y(0));

    // The tint is steady across time and motion settings alike.
    let later = OceanColumn::new(ramp, viewport, 700, None, ShellPhase::Approval, true, 0, 0);
    assert_eq!(waiting.color_at_y(0), later.color_at_y(0));
}

/// A full context window reads as the trench: row 0 at 100% matches the
/// bottom row at 0%, so the abyss visibly rises as context fills.
#[test]
fn context_fill_drags_the_water_column_toward_the_deep() {
    let ramp =
        OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).expect("underwater ramp");
    let surface_row = ramp.color_at_context(0, 24, 0);
    let abyss_row = ramp.color_at_context(0, 24, 100);
    assert_ne!(surface_row, abyss_row);
    assert_eq!(abyss_row, ramp.color_at_context(23, 24, 0));
}

#[test]
fn shimmer_is_subtle_and_concentrated_near_the_surface() {
    let ramp =
        OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).expect("underwater ramp");
    let surface_a = ramp.color_at_phase_context(0, 20, 0, ShellPhase::Idle, 0);
    let surface_b = ramp.color_at_phase_context(0, 20, 22_500, ShellPhase::Idle, 0);
    let deep_a = ramp.color_at_phase_context(19, 20, 0, ShellPhase::Idle, 0);
    let deep_b = ramp.color_at_phase_context(19, 20, 22_500, ShellPhase::Idle, 0);

    let surface_shift = distance(surface_a, surface_b);
    assert!(
        (1..=8).contains(&surface_shift),
        "surface shift was {surface_shift}"
    );
    assert_eq!(
        deep_a, deep_b,
        "the floor should stay perceptually anchored"
    );
}

#[test]
fn attention_phases_carry_their_own_water_and_work_phases_have_distinct_depth_bias() {
    let ramp =
        OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).expect("underwater ramp");
    // Attention tints are steady — the color itself is the signal, and a
    // slow breath read as flicker rather than intent — but never neutral:
    // each attention phase differs from the plain water.
    for phase in [
        ShellPhase::Waiting,
        ShellPhase::Approval,
        ShellPhase::Failed,
    ] {
        assert_eq!(
            ramp.color_at_phase_context(4, 20, 0, phase, 0),
            ramp.color_at_phase_context(4, 20, 45_000, phase, 0)
        );
        assert_ne!(
            ramp.color_at_phase_context(4, 20, 0, phase, 0),
            ramp.color_at_context(4, 20, 0)
        );
    }
    assert_ne!(
        ramp.color_at_phase_context(10, 20, 22_500, ShellPhase::Working, 0),
        ramp.color_at_phase_context(10, 20, 22_500, ShellPhase::Verifying, 0)
    );
}

#[test]
fn tall_columns_darken_continuously_without_an_anchor_shelf() {
    // The old two-segment ramp met at 0.42 with zero color velocity on both
    // sides: on a tall window that shelf read as a horizontal seam. The
    // Bézier column must keep moving through the former anchor zone.
    let ramp =
        OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).expect("underwater ramp");
    let height = 120;
    let anchor = 50; // ~0.42 of 120
    let above = ramp.color_at_context(anchor - 6, height, 0);
    let at = ramp.color_at_context(anchor, height, 0);
    let below = ramp.color_at_context(anchor + 6, height, 0);
    assert_ne!(above, at, "water must still darken entering the old anchor");
    assert_ne!(at, below, "water must still darken leaving the old anchor");
    assert_eq!(ramp.color_at_context(0, height, 0), ramp.surface);
    assert_eq!(ramp.color_at_context(height - 1, height, 0), ramp.deep);
}

#[test]
fn completion_breath_peaks_once_then_settles() {
    let ramp =
        OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).expect("underwater ramp");
    let start = ramp.color_at_completion_context(0, 20, 0, 0);
    let peak = ramp.color_at_completion_context(0, 20, 320, 0);
    let settled = ramp.color_at_completion_context(0, 20, 800, 0);
    assert_ne!(start, peak);
    assert_ne!(peak, settled);
    assert_eq!(settled, ramp.color_at_context(0, 20, 0));
}

#[test]
fn cache_fingerprint_changes_when_only_ramp_colors_change() {
    let viewport = Rect::new(3, 5, 80, 24);
    let first_ramp = OceanRamp {
        surface: Color::Rgb(1, 2, 3),
        middle: Color::Rgb(4, 5, 6),
        deep: Color::Rgb(7, 8, 9),
        ambient: Color::Rgb(10, 11, 12),
        attention: Color::Rgb(240, 180, 60),
        failure: Color::Rgb(220, 80, 80),
    };
    let second_ramp = OceanRamp {
        surface: Color::Rgb(21, 22, 23),
        middle: Color::Rgb(24, 25, 26),
        deep: Color::Rgb(27, 28, 29),
        ambient: Color::Rgb(30, 31, 32),
        attention: Color::Rgb(240, 180, 60),
        failure: Color::Rgb(220, 80, 80),
    };
    let first = OceanColumn::new(
        first_ramp,
        viewport,
        22_500,
        None,
        ShellPhase::Working,
        true,
        1000,
        0,
    );
    let second = OceanColumn::new(
        second_ramp,
        viewport,
        22_500,
        None,
        ShellPhase::Working,
        true,
        1000,
        0,
    );

    assert_ne!(first.color_at_y(viewport.y), second.color_at_y(viewport.y));
    assert_ne!(
        first.ramp_fingerprint(),
        second.ramp_fingerprint(),
        "visibly different palettes must not reuse the same row-color cache"
    );
}

#[test]
fn each_ramp_color_participates_in_the_typed_cache_identity() {
    let viewport = Rect::new(3, 5, 80, 24);
    let ramp = OceanRamp {
        surface: Color::Rgb(1, 2, 3),
        middle: Color::Rgb(4, 5, 6),
        deep: Color::Rgb(7, 8, 9),
        ambient: Color::Rgb(10, 11, 12),
        attention: Color::Rgb(240, 180, 60),
        failure: Color::Rgb(220, 80, 80),
    };
    let baseline = OceanColumn::new(
        ramp,
        viewport,
        22_500,
        None,
        ShellPhase::Working,
        true,
        1000,
        0,
    );
    let alternatives = [
        OceanRamp {
            surface: Color::Rgb(101, 2, 3),
            ..ramp
        },
        OceanRamp {
            middle: Color::Rgb(104, 5, 6),
            ..ramp
        },
        OceanRamp {
            deep: Color::Rgb(107, 8, 9),
            ..ramp
        },
        OceanRamp {
            ambient: Color::Rgb(110, 11, 12),
            ..ramp
        },
        OceanRamp {
            attention: Color::Rgb(255, 200, 90),
            ..ramp
        },
        OceanRamp {
            failure: Color::Rgb(255, 90, 90),
            ..ramp
        },
    ];

    for alternative in alternatives {
        let changed = OceanColumn::new(
            alternative,
            viewport,
            22_500,
            None,
            ShellPhase::Working,
            true,
            1000,
            0,
        );
        assert_ne!(
            baseline.ramp_cache_identity(),
            changed.ramp_cache_identity()
        );
        assert_ne!(baseline.ramp_fingerprint(), changed.ramp_fingerprint());
    }
}

#[test]
fn identical_semantic_cache_inputs_have_identical_identity() {
    let ramp =
        OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).expect("underwater ramp");
    let viewport = Rect::new(3, 5, 80, 24);
    let first = OceanColumn::new(
        ramp,
        viewport,
        22_500,
        None,
        ShellPhase::Working,
        true,
        1000,
        0,
    );
    let second = OceanColumn::new(
        ramp,
        viewport,
        22_500,
        None,
        ShellPhase::Working,
        true,
        1000,
        0,
    );

    assert_eq!(first.ramp_cache_identity(), second.ramp_cache_identity());
    assert_eq!(first.ramp_fingerprint(), second.ramp_fingerprint());
}

#[test]
fn split_shell_surfaces_share_one_absolute_row_column() {
    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    let ramp = OceanRamp::for_theme(&theme).expect("underwater ramp");
    let viewport = Rect::new(0, 0, 12, 12);
    let header = Rect::new(0, 0, 12, 2);
    let composer = Rect::new(0, 10, 12, 2);
    let mut buf = Buffer::empty(viewport);
    for y in header.top()..header.bottom() {
        for x in header.left()..header.right() {
            buf[(x, y)].set_bg(theme.header_bg);
        }
    }
    for y in composer.top()..composer.bottom() {
        for x in composer.left()..composer.right() {
            buf[(x, y)].set_bg(theme.composer_bg);
        }
    }
    buf[(4, 10)].set_bg(theme.selection_bg);

    let column = OceanColumn::new(ramp, viewport, 0, None, ShellPhase::Idle, false, 0, 0);
    column.paint_matching(header, &mut buf, theme.header_bg);
    column.paint_matching(composer, &mut buf, theme.composer_bg);

    assert_eq!(buf[(0, 0)].bg, ramp.color_at_context(0, 12, 0));
    assert_eq!(buf[(0, 11)].bg, ramp.color_at_context(11, 12, 0));
    assert_ne!(buf[(0, 1)].bg, buf[(0, 10)].bg);
    assert_eq!(
        buf[(4, 10)].bg,
        theme.selection_bg,
        "semantic surfaces must survive the shell Deepsea pass"
    );
}

#[test]
fn full_viewport_water_column_reaches_both_terminal_edges() {
    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    let ramp = OceanRamp::for_theme(&theme).expect("underwater ramp");
    let viewport = Rect::new(0, 0, 120, 32);
    let mut buf = Buffer::empty(viewport);
    for y in viewport.top()..viewport.bottom() {
        for x in viewport.left()..viewport.right() {
            buf[(x, y)].set_bg(theme.surface_bg);
        }
    }
    buf[(60, 16)].set_bg(theme.selection_bg);

    let column = OceanColumn::new(ramp, viewport, 0, None, ShellPhase::Idle, false, 0, 0);
    column.paint_matching(viewport, &mut buf, theme.surface_bg);

    for y in viewport.top()..viewport.bottom() {
        let expected = ramp.color_at_context(y, viewport.height, 0);
        assert_eq!(buf[(viewport.left(), y)].bg, expected);
        assert_eq!(buf[(viewport.right() - 1, y)].bg, expected);
    }
    assert_eq!(
        buf[(60, 16)].bg,
        theme.selection_bg,
        "semantic surfaces must remain protected inside the full-width water column"
    );
}

// ---- v0.9.4: life presence eases the animated/static boundary ----

#[test]
fn life_presence_is_pure_and_bounded() {
    // Same inputs -> same output; presence never leaves 0..=1.
    let inputs = [
        (None, None, false, false, false),
        (None, None, true, false, false),
        (Some(0), Some(0), true, false, false),
        (Some(500), Some(10_000), false, false, false),
        (Some(2_000), None, true, false, false),
        (None, Some(30_000), true, true, false),
    ];
    for (completion, turn, animated, browsing, empty) in inputs {
        let a = life_presence(completion, turn, animated, browsing, empty);
        let b = life_presence(completion, turn, animated, browsing, empty);
        assert_eq!(a, b, "life_presence must be a pure function of its inputs");
        assert!(
            (0.0..=1.0).contains(&a),
            "presence must stay in 0..=1, got {a}"
        );
    }
}

#[test]
fn life_presence_holds_full_through_completion_breath_then_settles() {
    // During the breath the water keeps full life so the settle flourish is
    // accompanied by swimming fish, then presence eases out.
    assert_eq!(life_presence(Some(0), None, false, false, false), 1.0);
    assert_eq!(
        life_presence(Some(COMPLETION_BREATH_MS - 1), None, false, false, false),
        1.0
    );
    assert_eq!(
        life_presence(Some(COMPLETION_BREATH_MS), None, false, false, false),
        1.0,
        "presence holds at the breath boundary before easing"
    );
    let mid = life_presence(
        Some(COMPLETION_BREATH_MS + SETTLE_MS / 2),
        None,
        false,
        false,
        false,
    );
    assert!(
        mid > 0.0 && mid < 1.0,
        "presence must be mid-fade during the settle window, got {mid}"
    );
    assert_eq!(
        life_presence(
            Some(COMPLETION_BREATH_MS + SETTLE_MS),
            None,
            false,
            false,
            false
        ),
        0.0,
        "presence reaches zero at the end of the settle window"
    );
}

#[test]
fn life_presence_ramps_in_from_turn_anchor_with_bounded_velocity() {
    // Working/Verifying ramps in over RAMP_MS; the ramp is monotone and
    // zero-velocity at both ends (smoothstep), so bursty streams ease in.
    let at = |ms: u128| life_presence(None, Some(ms), true, false, false);
    assert_eq!(at(0), 0.0);
    assert_eq!(at(RAMP_MS / 2), 0.5, "smoothstep midpoint is 0.5");
    assert_eq!(at(RAMP_MS), 1.0);
    assert_eq!(at(RAMP_MS * 10), 1.0);
    // Monotone non-decreasing across the ramp.
    let mut prev = 0.0f32;
    for step in 1..=16u128 {
        let ms = step * RAMP_MS / 16;
        let value = at(ms);
        assert!(value >= prev, "presence must not regress while ramping");
        prev = value;
    }
}

#[test]
fn life_presence_user_driven_states_are_immediate() {
    // Browsing history and the pristine empty state are deliberate user
    // surfaces: full presence with no ramp.
    assert_eq!(life_presence(None, Some(0), true, true, false), 1.0);
    assert_eq!(life_presence(None, Some(0), true, false, true), 1.0);
    // Fully static contexts are exactly zero.
    assert_eq!(life_presence(None, None, false, false, false), 0.0);
    assert_eq!(life_presence(None, Some(50_000), false, false, false), 0.0);
}

#[test]
fn live_semantic_tints_change_absolute_column_samples_and_cache_identity() {
    let default_theme = codewhale_palette::UNDERWATER_UI_THEME;
    let custom_theme = UiTheme {
        warning: Color::Rgb(235, 175, 55),
        error_fg: Color::Rgb(210, 70, 60),
        ..default_theme
    };
    let viewport = Rect::new(7, 9, 80, 24);
    for phase in [ShellPhase::Approval, ShellPhase::Failed] {
        let ordinary = OceanColumn::new(
            OceanRamp::for_theme(&default_theme).unwrap(),
            viewport,
            0,
            None,
            phase,
            false,
            0,
            30,
        );
        let custom = OceanColumn::new(
            OceanRamp::for_theme(&custom_theme).unwrap(),
            viewport,
            0,
            None,
            phase,
            false,
            0,
            30,
        );
        assert_ne!(
            ordinary.color_at_y(viewport.y),
            custom.color_at_y(viewport.y)
        );
        assert_ne!(ordinary.ramp_fingerprint(), custom.ramp_fingerprint());
        let moved = custom.with_viewport(Rect::new(7, 19, 80, 24));
        assert_eq!(custom.color_at_y(9), moved.color_at_y(19));
    }
}

#[test]
fn a_stale_success_clock_cannot_mask_attention_or_failure_water() {
    let ramp = OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).unwrap();
    let viewport = Rect::new(7, 9, 80, 24);
    for phase in [
        ShellPhase::Waiting,
        ShellPhase::Approval,
        ShellPhase::Failed,
    ] {
        for (animated, presence) in [(false, 0), (true, 1000)] {
            let plain = OceanColumn::new(ramp, viewport, 0, None, phase, animated, presence, 30);
            let stale =
                OceanColumn::new(ramp, viewport, 0, Some(320), phase, animated, presence, 30);
            assert_eq!(plain.ramp_fingerprint(), stale.ramp_fingerprint());
            assert_eq!(
                stale.completion_elapsed_ms(),
                Some(320),
                "the pet clock stays caller-owned"
            );
            for y in viewport.top()..viewport.bottom() {
                assert_eq!(plain.color_at_y(y), stale.color_at_y(y));
            }
        }
    }
}

#[test]
fn motion_off_ignores_success_pulse_without_losing_the_reported_pet_clock() {
    let ramp = OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).unwrap();
    let viewport = Rect::new(7, 9, 80, 24);
    let plain = OceanColumn::new(ramp, viewport, 0, None, ShellPhase::Done, false, 0, 30);
    let reported = OceanColumn::new(ramp, viewport, 0, Some(320), ShellPhase::Done, false, 0, 30);
    assert_eq!(reported.completion_elapsed_ms(), Some(320));
    assert_eq!(plain.ramp_fingerprint(), reported.ramp_fingerprint());
    for y in viewport.top()..viewport.bottom() {
        assert_eq!(plain.color_at_y(y), reported.color_at_y(y));
    }
}

#[test]
fn semantic_finishing_clips_the_request_and_preserves_reversed_cells() {
    use ratatui::style::Modifier;
    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    let ramp = OceanRamp::for_theme(&theme).unwrap();
    let viewport = Rect::new(7, 9, 80, 24);
    let area = Rect::new(11, 13, 8, 4);
    let mut buf = Buffer::empty(area);
    for cell in &mut buf.content {
        cell.set_bg(theme.surface_bg);
    }
    buf[(12, 13)].modifier.insert(Modifier::REVERSED);
    buf[(13, 13)].set_bg(theme.selection_bg);
    let before = buf.clone();
    let column = OceanColumn::new(ramp, viewport, 0, None, ShellPhase::Idle, false, 0, 30);
    column.paint_matching(Rect::new(0, 0, 120, 80), &mut buf, theme.surface_bg);
    assert_eq!(buf[(12, 13)], before[(12, 13)]);
    assert_eq!(buf[(13, 13)], before[(13, 13)]);
    assert_eq!(buf[(11, 13)].bg, ramp.color_at_context(4, 24, 30));
}

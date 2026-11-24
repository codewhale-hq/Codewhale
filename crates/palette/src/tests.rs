use super::adapt::{
    ColorDepth, adapt_bg, adapt_bg_for_palette_mode, adapt_bg_for_theme, adapt_color,
    adapt_fg_for_depth, adapt_fg_for_palette_mode, adapt_fg_for_theme, blend, luma, nearest_ansi16,
    pulse_brightness, reasoning_surface_tint, rgb_to_ansi256,
};
use super::detect::{
    BackgroundSource, PaletteMode, palette_mode_for_background,
    palette_mode_from_apple_interface_style, resolve_terminal_background,
};
use super::ids::{SELECTABLE_THEMES, ThemeId, normalize_hex_rgb_color, normalize_theme_name};
use super::rgb::*;
use super::themes::{
    CATPPUCCIN_MOCHA_UI_THEME, GRAYSCALE_UI_THEME, LIGHT_UI_THEME, MATRIX_UI_THEME,
    SOLARIZED_LIGHT_UI_THEME, TERMINAL_UI_THEME, TOKYO_NIGHT_UI_THEME, UI_THEME, UiTheme,
    parse_hex_rgb_color, theme_label_for_mode, ui_theme_from_settings,
};
use super::tokens::{
    ACCENT_REASONING_LIVE, DIFF_ADDED, DIFF_ADDED_BG, DIFF_DELETED_BG, GRAYSCALE_BORDER,
    GRAYSCALE_ELEVATED, GRAYSCALE_PANEL, GRAYSCALE_REASONING, GRAYSCALE_SURFACE,
    GRAYSCALE_TEXT_BODY, GRAYSCALE_TEXT_HINT, GRAYSCALE_TEXT_SOFT, LIGHT_ACTION, LIGHT_BORDER,
    LIGHT_DANGER, LIGHT_ELEVATED, LIGHT_ERROR, LIGHT_HUMAN, LIGHT_LIVE, LIGHT_PANEL,
    LIGHT_REASONING, LIGHT_SELECTION_BG, LIGHT_SUCCESS, LIGHT_SUCCESS_FG, LIGHT_SURFACE,
    LIGHT_TEXT_BODY, LIGHT_TEXT_HINT, LIGHT_WARNING, MODE_AGENT, MODE_PLAN, MODE_YOLO,
    SELECTION_BG, SOLARIZED_PANEL, SOLARIZED_SURFACE, SOLARIZED_TEXT_BODY, SOLARIZED_TEXT_HINT,
    STATUS_ERROR, STATUS_WARNING, SURFACE_ERROR, SURFACE_REASONING, SURFACE_REASONING_TINT,
    SURFACE_TOOL_ACTIVE, TEXT_BODY, TEXT_HINT, TEXT_REASONING, TEXT_TOOL_OUTPUT, WHALE_ACTION,
    WHALE_BG, WHALE_ERROR, WHALE_HUMAN, WHALE_LIVE, WHALE_PANEL,
};
use ratatui::style::Color;

#[test]
fn palette_mode_parses_colorfgbg_background_slot() {
    assert_eq!(
        PaletteMode::from_colorfgbg("0;15"),
        Some(PaletteMode::Light)
    );
    assert_eq!(PaletteMode::from_colorfgbg("15;0"), Some(PaletteMode::Dark));
    assert_eq!(
        PaletteMode::from_colorfgbg("7;default;15"),
        Some(PaletteMode::Light)
    );
    assert_eq!(PaletteMode::from_colorfgbg("not-a-color"), None);
}

#[test]
fn palette_mode_detect_prefers_colorfgbg_over_macos_fallback() {
    assert_eq!(
        resolve_terminal_background(None, Some("0;15"), Some(PaletteMode::Dark)).mode(),
        PaletteMode::Light
    );
    assert_eq!(
        resolve_terminal_background(None, Some("15;0"), Some(PaletteMode::Light)).mode(),
        PaletteMode::Dark
    );
}

#[test]
fn palette_mode_detect_uses_macos_fallback_when_colorfgbg_missing_or_invalid() {
    assert_eq!(
        resolve_terminal_background(None, None, Some(PaletteMode::Light)).mode(),
        PaletteMode::Light
    );
    assert_eq!(
        resolve_terminal_background(None, Some("not-a-color"), Some(PaletteMode::Light)).mode(),
        PaletteMode::Light
    );
    assert_eq!(
        resolve_terminal_background(None, None, None).mode(),
        PaletteMode::Dark
    );
}

#[test]
fn apple_interface_style_maps_dark_and_missing_key_to_expected_modes() {
    assert_eq!(
        palette_mode_from_apple_interface_style("Dark\n"),
        PaletteMode::Dark
    );
    assert_eq!(
        palette_mode_from_apple_interface_style("Light\n"),
        PaletteMode::Light
    );
    assert_eq!(
        palette_mode_from_apple_interface_style(""),
        PaletteMode::Light
    );
}

#[test]
fn ui_theme_selects_light_variant() {
    let theme = UiTheme::for_mode(PaletteMode::Light);
    assert_eq!(theme, LIGHT_UI_THEME);
    assert_eq!(theme.surface_bg, Color::Reset);
    assert_eq!(theme.text_body, LIGHT_TEXT_BODY);
}

#[test]
fn whale_pair_flat_shells_are_terminal_native_without_erasing_semantic_surfaces() {
    for theme in [UI_THEME, LIGHT_UI_THEME] {
        for shell_surface in [
            theme.surface_bg,
            theme.panel_bg,
            theme.composer_bg,
            theme.header_bg,
            theme.footer_bg,
        ] {
            assert_eq!(shell_surface, Color::Reset, "{} shell", theme.name);
        }

        for semantic_surface in [
            theme.elevated_bg,
            theme.selection_bg,
            theme.error_surface,
            theme.diff_added_bg,
            theme.diff_deleted_bg,
        ] {
            assert_ne!(semantic_surface, Color::Reset, "{} semantics", theme.name);
        }
    }
}

#[test]
fn ui_theme_selects_grayscale_variant() {
    let theme = UiTheme::for_mode(PaletteMode::Grayscale);
    assert_eq!(theme, GRAYSCALE_UI_THEME);
    assert_eq!(theme.surface_bg, GRAYSCALE_SURFACE);
    assert_eq!(theme.panel_bg, GRAYSCALE_PANEL);
    assert_eq!(theme.text_body, GRAYSCALE_TEXT_BODY);
}

#[test]
fn ui_theme_selects_solarized_light_variant() {
    let theme = UiTheme::for_mode(PaletteMode::SolarizedLight);
    assert_eq!(theme, SOLARIZED_LIGHT_UI_THEME);
    assert_eq!(theme.surface_bg, SOLARIZED_SURFACE);
    assert_eq!(theme.panel_bg, SOLARIZED_PANEL);
    assert_eq!(theme.text_body, SOLARIZED_TEXT_BODY);
}

#[test]
fn theme_names_normalize_common_grayscale_aliases() {
    assert_eq!(normalize_theme_name("system"), Some("system"));
    assert_eq!(normalize_theme_name("default"), Some("system"));
    assert_eq!(normalize_theme_name("whale"), Some("dark"));
    assert_eq!(normalize_theme_name("transparent"), Some("terminal"));
    assert_eq!(normalize_theme_name("inherit"), Some("terminal"));
    assert_eq!(normalize_theme_name("black-white"), Some("grayscale"));
    assert_eq!(normalize_theme_name("mono"), Some("grayscale"));
    assert_eq!(normalize_theme_name("solarized"), Some("solarized-light"));
    assert_eq!(theme_label_for_mode(PaletteMode::Grayscale), "grayscale");
}

#[test]
fn terminal_theme_resets_surfaces_and_remaps_direct_palette_constants() {
    assert_eq!(ThemeId::from_name("terminal"), Some(ThemeId::Terminal));
    assert_eq!(TERMINAL_UI_THEME.surface_bg, Color::Reset);
    assert_eq!(TERMINAL_UI_THEME.footer_bg, Color::Reset);
    assert_eq!(TERMINAL_UI_THEME.text_body, Color::Reset);

    assert_eq!(
        adapt_bg_for_theme(WHALE_BG, ThemeId::Terminal, &TERMINAL_UI_THEME),
        Color::Reset
    );
    assert_eq!(
        adapt_bg_for_theme(DIFF_ADDED_BG, ThemeId::Terminal, &TERMINAL_UI_THEME),
        Color::Reset
    );
    assert_eq!(
        adapt_fg_for_theme(TEXT_BODY, ThemeId::Terminal, &TERMINAL_UI_THEME),
        Color::Reset
    );
    assert_eq!(
        adapt_fg_for_theme(DIFF_ADDED, ThemeId::Terminal, &TERMINAL_UI_THEME),
        Color::Green
    );
}

#[test]
fn terminal_and_matrix_preserve_agent_plan_and_full_access_mode_slots() {
    for (theme_id, theme) in [
        (ThemeId::Terminal, TERMINAL_UI_THEME),
        (ThemeId::Matrix, MATRIX_UI_THEME),
    ] {
        for (source, expected, role) in [
            (MODE_AGENT, theme.mode_agent, "agent"),
            (MODE_PLAN, theme.mode_plan, "plan"),
            (MODE_YOLO, theme.mode_yolo, "full access"),
        ] {
            assert_eq!(
                adapt_fg_for_theme(source, theme_id, &theme),
                expected,
                "theme '{}' must map the raw {role} token to its mode slot",
                theme_id.name(),
            );
        }
    }
}

#[test]
fn community_remap_keeps_selection_tool_and_error_background_domains() {
    let mut theme = TOKYO_NIGHT_UI_THEME;
    theme.selection_bg = Color::Rgb(1, 2, 3);
    theme.elevated_bg = Color::Rgb(4, 5, 6);
    theme.error_surface = Color::Rgb(7, 8, 9);
    theme.diff_deleted_bg = Color::Rgb(10, 11, 12);

    assert_eq!(
        adapt_bg_for_theme(SELECTION_BG, ThemeId::TokyoNight, &theme),
        theme.selection_bg
    );
    assert_eq!(
        adapt_bg_for_theme(SURFACE_TOOL_ACTIVE, ThemeId::TokyoNight, &theme),
        theme.elevated_bg
    );
    assert_eq!(
        adapt_bg_for_theme(SURFACE_ERROR, ThemeId::TokyoNight, &theme),
        theme.error_surface
    );
    assert_eq!(
        adapt_bg_for_theme(DIFF_DELETED_BG, ThemeId::TokyoNight, &theme),
        theme.diff_deleted_bg
    );
}

#[test]
fn light_palette_has_quiet_layer_separation() {
    assert_eq!(LIGHT_SURFACE, Color::Rgb(244, 247, 251));
    assert_eq!(LIGHT_PANEL, Color::Rgb(255, 253, 248));
    assert_eq!(LIGHT_ELEVATED, Color::Rgb(232, 238, 248));
    assert_eq!(LIGHT_BORDER, Color::Rgb(169, 184, 207));
    assert_eq!(LIGHT_SELECTION_BG, Color::Rgb(238, 246, 255));
    assert_ne!(LIGHT_SURFACE, LIGHT_PANEL);
    assert_ne!(LIGHT_PANEL, LIGHT_ELEVATED);
}

#[test]
fn solarized_light_does_not_mutate_whale_light_text() {
    assert_eq!(
        LIGHT_TEXT_BODY,
        Color::Rgb(
            LIGHT_TEXT_BODY_RGB.0,
            LIGHT_TEXT_BODY_RGB.1,
            LIGHT_TEXT_BODY_RGB.2
        )
    );
    assert_ne!(LIGHT_TEXT_BODY, SOLARIZED_TEXT_BODY);
}

#[test]
fn dark_palette_uses_soft_body_text_and_warm_reasoning() {
    assert_eq!(
        TEXT_BODY,
        Color::Rgb(
            WHALE_TEXT_BODY_RGB.0,
            WHALE_TEXT_BODY_RGB.1,
            WHALE_TEXT_BODY_RGB.2
        )
    );
    assert_eq!(
        TEXT_REASONING,
        Color::Rgb(
            WHALE_REASONING_TEXT_RGB.0,
            WHALE_REASONING_TEXT_RGB.1,
            WHALE_REASONING_TEXT_RGB.2
        )
    );
    assert_eq!(
        ACCENT_REASONING_LIVE,
        Color::Rgb(
            WHALE_REASONING_TEXT_RGB.0,
            WHALE_REASONING_TEXT_RGB.1,
            WHALE_REASONING_TEXT_RGB.2
        )
    );
    assert_ne!(TEXT_REASONING, TEXT_TOOL_OUTPUT);
    assert_ne!(TEXT_BODY, Color::White);
}

#[test]
fn ui_theme_applies_custom_background_to_base_surfaces() {
    let custom = Color::Rgb(26, 27, 38);
    let theme = UiTheme::for_mode(PaletteMode::Dark).with_background_color(custom);

    assert_eq!(theme.surface_bg, custom);
    assert_eq!(theme.header_bg, custom);
    assert_eq!(theme.footer_bg, custom);
    assert_eq!(
        theme.composer_bg, UI_THEME.composer_bg,
        "custom background must not erase panel contrast"
    );
}

#[test]
fn hex_rgb_color_parser_accepts_hashless_and_normalizes() {
    assert_eq!(parse_hex_rgb_color("#1a1B26"), Some(Color::Rgb(26, 27, 38)));
    assert_eq!(parse_hex_rgb_color("1a1b26"), Some(Color::Rgb(26, 27, 38)));
    assert_eq!(
        normalize_hex_rgb_color("#1A1B26").as_deref(),
        Some("#1a1b26")
    );
    assert_eq!(parse_hex_rgb_color("#123"), None);
    assert_eq!(parse_hex_rgb_color("#zzzzzz"), None);
}

#[test]
fn light_palette_maps_dark_surfaces_and_text() {
    assert_eq!(
        adapt_bg_for_palette_mode(WHALE_BG, PaletteMode::Light),
        LIGHT_SURFACE
    );
    assert_eq!(
        adapt_bg_for_palette_mode(WHALE_PANEL, PaletteMode::Light),
        LIGHT_PANEL
    );
    assert_eq!(
        adapt_fg_for_palette_mode(Color::White, LIGHT_SURFACE, PaletteMode::Light),
        LIGHT_TEXT_BODY
    );
    assert_eq!(
        adapt_fg_for_palette_mode(TEXT_HINT, LIGHT_SURFACE, PaletteMode::Light),
        LIGHT_TEXT_HINT
    );
    assert_eq!(
        adapt_fg_for_palette_mode(WHALE_ACTION, LIGHT_SURFACE, PaletteMode::Light),
        LIGHT_ACTION
    );
    assert_eq!(
        adapt_fg_for_palette_mode(WHALE_LIVE, LIGHT_SURFACE, PaletteMode::Light),
        LIGHT_LIVE
    );
    assert_eq!(
        adapt_fg_for_palette_mode(WHALE_HUMAN, LIGHT_SURFACE, PaletteMode::Light),
        LIGHT_HUMAN
    );
    assert_eq!(
        adapt_fg_for_palette_mode(STATUS_WARNING, LIGHT_SURFACE, PaletteMode::Light),
        LIGHT_WARNING
    );
    assert_eq!(
        adapt_fg_for_palette_mode(STATUS_ERROR, LIGHT_SURFACE, PaletteMode::Light),
        LIGHT_DANGER
    );
    assert_ne!(LIGHT_LIVE, LIGHT_SUCCESS_FG);
}

#[test]
fn solarized_light_palette_maps_dark_surfaces_and_text_to_solarized_roles() {
    assert_eq!(
        adapt_bg_for_palette_mode(WHALE_BG, PaletteMode::SolarizedLight),
        SOLARIZED_SURFACE
    );
    assert_eq!(
        adapt_bg_for_palette_mode(WHALE_PANEL, PaletteMode::SolarizedLight),
        SOLARIZED_PANEL
    );
    assert_eq!(
        adapt_fg_for_palette_mode(Color::White, SOLARIZED_SURFACE, PaletteMode::SolarizedLight),
        SOLARIZED_TEXT_BODY
    );
    assert_eq!(
        adapt_fg_for_palette_mode(TEXT_HINT, SOLARIZED_SURFACE, PaletteMode::SolarizedLight),
        SOLARIZED_TEXT_HINT
    );
}

#[test]
fn grayscale_palette_maps_brand_hues_to_neutral_roles() {
    assert_eq!(
        adapt_bg_for_palette_mode(WHALE_BG, PaletteMode::Grayscale),
        GRAYSCALE_SURFACE
    );
    assert_eq!(
        adapt_bg_for_palette_mode(WHALE_PANEL, PaletteMode::Grayscale),
        GRAYSCALE_PANEL
    );
    assert_eq!(
        adapt_bg_for_palette_mode(SURFACE_REASONING, PaletteMode::Grayscale),
        GRAYSCALE_REASONING
    );
    assert_eq!(
        adapt_fg_for_palette_mode(WHALE_ACTION, GRAYSCALE_SURFACE, PaletteMode::Grayscale),
        GRAYSCALE_TEXT_SOFT
    );
    assert_eq!(
        adapt_fg_for_palette_mode(WHALE_ERROR, GRAYSCALE_SURFACE, PaletteMode::Grayscale),
        GRAYSCALE_TEXT_BODY
    );
    assert_eq!(
        adapt_fg_for_palette_mode(TEXT_HINT, GRAYSCALE_SURFACE, PaletteMode::Grayscale),
        GRAYSCALE_TEXT_HINT
    );
}

#[test]
fn grayscale_luma_handles_bright_rgb_without_overflow() {
    assert_eq!(luma(255, 255, 255), 255);
    assert_eq!(
        adapt_fg_for_palette_mode(
            Color::Rgb(255, 255, 255),
            GRAYSCALE_SURFACE,
            PaletteMode::Grayscale
        ),
        GRAYSCALE_TEXT_BODY
    );
}

#[test]
fn ui_theme_from_settings_applies_theme_and_background() {
    let theme = ui_theme_from_settings("grayscale", Some("#111111"));
    assert_eq!(theme.mode, PaletteMode::Grayscale);
    assert_eq!(theme.surface_bg, Color::Rgb(17, 17, 17));
    assert_eq!(theme.header_bg, Color::Rgb(17, 17, 17));
    assert_eq!(theme.footer_bg, Color::Rgb(17, 17, 17));
    assert_eq!(theme.panel_bg, GRAYSCALE_PANEL);
    assert_eq!(theme.elevated_bg, GRAYSCALE_ELEVATED);
    assert_eq!(theme.border, GRAYSCALE_BORDER);
}

#[test]
fn adapt_color_passes_through_truecolor() {
    let c = Color::Rgb(53, 120, 229);
    assert_eq!(adapt_color(c, ColorDepth::TrueColor), c);
}

#[test]
fn adapt_color_maps_rgb_to_indexed_on_ansi256() {
    let c = Color::Rgb(53, 120, 229);
    assert!(matches!(
        adapt_color(c, ColorDepth::Ansi256),
        Color::Indexed(_)
    ));
}

#[test]
fn adapt_bg_maps_rgb_to_indexed_on_ansi256() {
    assert!(matches!(
        adapt_bg(SURFACE_REASONING, ColorDepth::Ansi256),
        Color::Indexed(_)
    ));
}

#[test]
fn adapt_color_drops_to_named_on_ansi16() {
    // Sky: blue-dominant and bright → LightBlue, not terminal cyan.
    assert_eq!(
        adapt_color(WHALE_ACTION, ColorDepth::Ansi16),
        Color::LightBlue
    );
    // Rose Red is intentionally bright enough to use the terminal's
    // bright red slot.
    assert_eq!(
        adapt_color(WHALE_ERROR, ColorDepth::Ansi16),
        Color::LightRed
    );
}

#[test]
fn action_blue_is_not_human_gold() {
    assert_ne!(WHALE_ACTION, WHALE_HUMAN);
}

#[test]
fn stable_dark_and_light_ids_expose_blue_stage_product_names() {
    assert_eq!(ThemeId::from_name("dark"), Some(ThemeId::Whale));
    assert_eq!(ThemeId::Whale.display_name(), "Blue Stage");
    assert_eq!(ThemeId::from_name("light"), Some(ThemeId::WhaleLight));
    assert_eq!(ThemeId::WhaleLight.display_name(), "Blue Stage Light");
}

#[test]
fn community_theme_info_keeps_the_sky_live_role_on_ansi16() {
    assert_eq!(
        adapt_fg_for_depth(
            CATPPUCCIN_MOCHA_UI_THEME.info,
            CATPPUCCIN_MOCHA_UI_THEME.info,
            ColorDepth::Ansi16,
            &CATPPUCCIN_MOCHA_UI_THEME,
        ),
        Color::LightCyan,
    );
    assert_eq!(
        adapt_fg_for_depth(
            CATPPUCCIN_MOCHA_UI_THEME.status_working,
            CATPPUCCIN_MOCHA_UI_THEME.status_working,
            ColorDepth::Ansi16,
            &CATPPUCCIN_MOCHA_UI_THEME,
        ),
        Color::LightCyan,
    );
}

#[test]
fn every_selectable_theme_keeps_action_and_working_roles_distinct_on_ansi16() {
    for theme_id in SELECTABLE_THEMES {
        // Grayscale deliberately collapses colored semantic lanes to neutral
        // luminance tiers before terminal-depth adaptation.
        if *theme_id == ThemeId::Grayscale {
            continue;
        }
        let ui = theme_id.ui_theme();
        let is_light = matches!(ui.mode, PaletteMode::Light | PaletteMode::SolarizedLight);
        assert_eq!(
            adapt_fg_for_depth(
                ui.accent_primary,
                ui.accent_primary,
                ColorDepth::Ansi16,
                &ui,
            ),
            if is_light {
                Color::Blue
            } else {
                Color::LightBlue
            },
            "theme '{}' lost the action lane",
            theme_id.name(),
        );
        assert_eq!(
            adapt_fg_for_depth(
                ui.status_working,
                ui.status_working,
                ColorDepth::Ansi16,
                &ui,
            ),
            if is_light {
                Color::Cyan
            } else {
                Color::LightCyan
            },
            "theme '{}' lost the live working lane",
            theme_id.name(),
        );
    }
}

#[test]
fn adapt_bg_preserves_diff_hues_on_ansi256() {
    assert_eq!(
        adapt_bg(DIFF_ADDED_BG, ColorDepth::Ansi256),
        Color::Indexed(22)
    );
    assert_eq!(
        adapt_bg(DIFF_DELETED_BG, ColorDepth::Ansi256),
        Color::Indexed(52)
    );
    assert_eq!(
        adapt_bg(LIGHT_SUCCESS, ColorDepth::Ansi256),
        Color::Indexed(158)
    );
    assert_eq!(
        adapt_bg(LIGHT_ERROR, ColorDepth::Ansi256),
        Color::Indexed(224)
    );
}

#[test]
fn adapt_bg_disables_tints_on_ansi16() {
    assert_eq!(
        adapt_bg(SURFACE_REASONING, ColorDepth::Ansi16),
        Color::Reset
    );
    assert_eq!(
        adapt_bg(SURFACE_REASONING, ColorDepth::TrueColor),
        SURFACE_REASONING
    );
}

#[test]
fn reasoning_tint_is_none_on_ansi16() {
    assert!(reasoning_surface_tint(ColorDepth::Ansi16).is_none());
    assert!(reasoning_surface_tint(ColorDepth::TrueColor).is_some());
    assert!(matches!(
        reasoning_surface_tint(ColorDepth::Ansi256),
        Some(Color::Indexed(_))
    ));
}

#[test]
fn light_palette_maps_reasoning_tint_to_light_surface() {
    assert_eq!(
        SURFACE_REASONING_TINT,
        Color::Rgb(
            WHALE_REASONING_TINT_RGB.0,
            WHALE_REASONING_TINT_RGB.1,
            WHALE_REASONING_TINT_RGB.2
        )
    );
    assert_eq!(
        adapt_bg_for_palette_mode(SURFACE_REASONING_TINT, PaletteMode::Light),
        LIGHT_REASONING
    );
    assert_eq!(
        adapt_bg_for_palette_mode(
            reasoning_surface_tint(ColorDepth::TrueColor).expect("truecolor tint"),
            PaletteMode::Light,
        ),
        LIGHT_REASONING
    );
}

#[test]
fn blend_at_zero_returns_bg_at_one_returns_fg() {
    let fg = Color::Rgb(200, 100, 50);
    let bg = Color::Rgb(0, 0, 0);
    assert_eq!(blend(fg, bg, 0.0), bg);
    assert_eq!(blend(fg, bg, 1.0), fg);
}

#[test]
fn blend_at_half_is_midpoint() {
    let mid = blend(Color::Rgb(200, 100, 0), Color::Rgb(0, 0, 0), 0.5);
    assert_eq!(mid, Color::Rgb(100, 50, 0));
}

#[test]
fn pulse_brightness_swings_within_envelope() {
    // The pulse rides between 30%..100% — never below 30% of the source.
    let src = ACCENT_REASONING_LIVE;
    let mut min_r = u8::MAX;
    let mut max_r = 0u8;
    for ms in (0u64..2000).step_by(50) {
        if let Color::Rgb(r, _, _) = pulse_brightness(src, ms) {
            min_r = min_r.min(r);
            max_r = max_r.max(r);
        }
    }
    let Color::Rgb(src_r, _, _) = src else {
        panic!("expected RGB");
    };
    // Trough should land near 30% of source; crest near source itself.
    let lower = (f32::from(src_r) * 0.30).round() as u8;
    assert!(min_r <= lower + 2, "trough too high: {min_r}");
    assert!(max_r + 2 >= src_r, "crest too low: {max_r}");
}

#[test]
fn pulse_passes_named_colors_unchanged() {
    // Named palette entries don't blend meaningfully — leave them alone.
    assert_eq!(pulse_brightness(Color::Reset, 0), Color::Reset);
    assert_eq!(pulse_brightness(Color::Cyan, 1234), Color::Cyan);
}

#[test]
fn nearest_ansi16_routes_known_brand_colors() {
    // Codewhale keeps action, live, human, and danger distinct where ANSI-16 allows it.
    assert_eq!(nearest_ansi16(106, 174, 242), Color::LightBlue); // Cobalt action
    assert_eq!(nearest_ansi16(246, 196, 83), Color::LightYellow); // Signal Gold
    assert_eq!(nearest_ansi16(79, 209, 197), Color::LightCyan); // Seafoam
    assert_eq!(nearest_ansi16(38, 62, 92), Color::Blue); // Border
    assert_eq!(nearest_ansi16(54, 187, 212), Color::LightCyan); // Aqua
    assert_eq!(nearest_ansi16(255, 134, 178), Color::LightRed); // Rose danger
    assert_eq!(nearest_ansi16(3, 7, 13), Color::Black); // Deep field
}

#[test]
fn rgb_to_ansi256_uses_stable_extended_palette() {
    assert!(rgb_to_ansi256(53, 120, 229) >= 16);
    assert!(rgb_to_ansi256(11, 21, 38) >= 16);
}

#[test]
fn color_depth_detect_is_safe_without_env() {
    // Don't try to pin the result — env may be anything in CI. Just
    // exercise the path so a panic would surface.
    let _ = ColorDepth::detect();
    let _ = adapt_color(WHALE_BG, ColorDepth::detect());
}

/// no-color.org contract (spec TIDELINE §5d gap): `NO_COLOR` present and
/// non-empty suppresses colors even on a truecolor terminal;
/// an empty value does not count.
#[test]
fn no_color_forces_the_mono_depth_even_on_truecolor() {
    fn read(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        move |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| std::ffi::OsString::from(v))
        }
    }
    let depth = ColorDepth::detect_with(read(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]));
    assert_eq!(
        depth,
        ColorDepth::Monochrome,
        "NO_COLOR wins over COLORTERM"
    );
    for color in [
        Color::Reset,
        Color::Red,
        Color::Indexed(9),
        Color::Rgb(103, 184, 214),
    ] {
        assert_eq!(adapt_color(color, depth), Color::Reset);
        assert_eq!(adapt_bg(color, depth), Color::Reset);
    }
    assert!(reasoning_surface_tint(depth).is_none());
    let depth = ColorDepth::detect_with(read(&[("NO_COLOR", ""), ("COLORTERM", "truecolor")]));
    assert_eq!(
        depth,
        ColorDepth::TrueColor,
        "empty NO_COLOR does not count (no-color.org)"
    );
    let depth = ColorDepth::detect_with(read(&[("COLORTERM", "truecolor")]));
    assert_eq!(depth, ColorDepth::TrueColor, "no NO_COLOR: normal detect");
}

// === #4833: contrast floor ===

use super::contrast::{
    AA_BODY_CONTRAST, contrast_ratio, effective_surface, enforce_contrast, meets_contrast,
    relative_luminance, symbol_needs_text_contrast, theme_contrast_violations,
    theme_uses_terminal_owned_surfaces,
};
use super::detect::TerminalBackground;
use super::osc11::{ProbeSplit, ProbeStep, parse_osc11_reply};
use super::tokens::{
    MODE_OPERATE, STATUS_SUCCESS, TEXT_MUTED, TEXT_SECONDARY, TEXT_SOFT, USER_BODY,
};

const WHITE: Color = Color::Rgb(0xFF, 0xFF, 0xFF);
const BLACK: Color = Color::Rgb(0x00, 0x00, 0x00);

/// Every dark-palette token that renders *text*. Frame chrome (`BORDER_COLOR`)
/// is deliberately absent — see `symbol_needs_text_contrast`.
const DARK_TEXT_TOKENS: &[Color] = &[
    TEXT_BODY,
    TEXT_SOFT,
    TEXT_SECONDARY,
    TEXT_MUTED,
    TEXT_HINT,
    TEXT_REASONING,
    TEXT_TOOL_OUTPUT,
    USER_BODY,
    WHALE_ACTION,
    WHALE_LIVE,
    WHALE_HUMAN,
    WHALE_ERROR,
    MODE_AGENT,
    MODE_PLAN,
    MODE_OPERATE,
    MODE_YOLO,
    STATUS_ERROR,
    STATUS_WARNING,
    STATUS_SUCCESS,
    DIFF_ADDED,
];

fn approx(actual: f32, expected: f32, tolerance: f32) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected} ± {tolerance}, got {actual}"
    );
}

#[test]
fn relative_luminance_matches_wcag_reference_values() {
    approx(relative_luminance(WHITE).unwrap(), 1.0, 1e-4);
    approx(relative_luminance(BLACK).unwrap(), 0.0, 1e-4);
    // WCAG worked example: #808080 has relative luminance 0.2159.
    approx(
        relative_luminance(Color::Rgb(0x80, 0x80, 0x80)).unwrap(),
        0.2159,
        1e-3,
    );
    // Terminal-defined colors have no knowable RGB, so no luminance.
    assert_eq!(relative_luminance(Color::Reset), None);
    assert_eq!(relative_luminance(Color::White), None);
    assert_eq!(relative_luminance(Color::Indexed(7)), None);
    // The xterm cube and gray ramp are fixed by spec, so they are knowable.
    assert!(relative_luminance(Color::Indexed(231)).is_some());
}

#[test]
fn contrast_ratio_matches_known_pairs() {
    approx(contrast_ratio(BLACK, WHITE).unwrap(), 21.0, 1e-3);
    approx(contrast_ratio(WHITE, WHITE).unwrap(), 1.0, 1e-4);
    // #767676 on white is the canonical "smallest AA-passing gray".
    approx(
        contrast_ratio(Color::Rgb(0x76, 0x76, 0x76), WHITE).unwrap(),
        4.54,
        0.01,
    );
    // Symmetric in its arguments.
    assert_eq!(
        contrast_ratio(TEXT_BODY, WHALE_BG),
        contrast_ratio(WHALE_BG, TEXT_BODY)
    );
    // An unknowable side yields no ratio, and `meets_contrast` refuses to call
    // that a pass.
    assert_eq!(contrast_ratio(TEXT_BODY, Color::Reset), None);
    assert!(!meets_contrast(TEXT_BODY, Color::Reset, AA_BODY_CONTRAST));
}

#[test]
fn light_surface_lifts_body_text_that_no_whitelist_adapted() {
    // The #4833 shape: dark-tuned ivory body text reaching a near-white
    // terminal with no light adaptation applied, because detection said Dark.
    let before = contrast_ratio(TEXT_BODY, WHITE).unwrap();
    assert!(
        before < AA_BODY_CONTRAST,
        "precondition: unadapted body text is illegible on white ({before})"
    );

    let lifted = enforce_contrast(TEXT_BODY, WHITE, AA_BODY_CONTRAST);
    let after = contrast_ratio(lifted, WHITE).unwrap();
    assert!(
        after >= AA_BODY_CONTRAST,
        "body text must clear AA on a light surface, got {after}"
    );

    // The same holds for the reporter's paler surface and for the secondary
    // tiers that collapsed alongside body text.
    let reported_surface = Color::Rgb(0xF7, 0xF7, 0xF5);
    for token in [TEXT_BODY, TEXT_SOFT, TEXT_SECONDARY, TEXT_HINT] {
        let lifted = enforce_contrast(token, reported_surface, AA_BODY_CONTRAST);
        let ratio = contrast_ratio(lifted, reported_surface).unwrap();
        assert!(
            ratio >= AA_BODY_CONTRAST,
            "{token:?} still below floor on light surface: {ratio}"
        );
    }
}

#[test]
fn enforce_contrast_lifts_by_the_smallest_amount_that_clears_the_floor() {
    let lifted = enforce_contrast(TEXT_BODY, WHITE, AA_BODY_CONTRAST);
    let ratio = contrast_ratio(lifted, WHITE).unwrap();
    assert!(
        (AA_BODY_CONTRAST..AA_BODY_CONTRAST + 0.1).contains(&ratio),
        "expected a minimal lift to ~{AA_BODY_CONTRAST}, got {ratio}"
    );
    // Already-compliant colors are returned byte-identical.
    assert_eq!(
        enforce_contrast(LIGHT_TEXT_BODY, WHITE, AA_BODY_CONTRAST),
        LIGHT_TEXT_BODY
    );
}

#[test]
fn dark_surface_leaves_every_text_token_untouched() {
    // The no-regression guarantee for today's users: on the surfaces a dark
    // terminal actually presents, no shipped text token is rewritten.
    for surface in [
        WHALE_BG,
        WHALE_PANEL,
        BLACK,
        Color::Rgb(0x1E, 0x1E, 0x1E), // VS Code dark
        Color::Rgb(0x0C, 0x0C, 0x0C), // Windows Terminal default
    ] {
        for token in DARK_TEXT_TOKENS {
            assert_eq!(
                enforce_contrast(*token, surface, AA_BODY_CONTRAST),
                *token,
                "{token:?} was rewritten on dark surface {surface:?}"
            );
        }
    }
}

#[test]
fn light_theme_tokens_already_clear_the_floor_on_their_own_surfaces() {
    for surface in [LIGHT_SURFACE, LIGHT_PANEL, LIGHT_ELEVATED] {
        for token in [
            LIGHT_TEXT_BODY,
            LIGHT_TEXT_HINT,
            LIGHT_ACTION,
            LIGHT_LIVE,
            LIGHT_HUMAN,
            LIGHT_WARNING,
            LIGHT_DANGER,
            LIGHT_SUCCESS_FG,
        ] {
            let ratio = contrast_ratio(token, surface).unwrap();
            assert!(
                ratio >= AA_BODY_CONTRAST,
                "{token:?} on {surface:?} is {ratio}, below the floor"
            );
            assert_eq!(enforce_contrast(token, surface, AA_BODY_CONTRAST), token);
        }
    }
}

#[test]
fn enforce_contrast_declines_when_it_cannot_know_the_colors() {
    // Named/indexed colors are remapped by the user's terminal profile, and
    // `Reset` is the terminal's own choice. Rewriting either would be a guess.
    assert_eq!(
        enforce_contrast(Color::White, WHITE, AA_BODY_CONTRAST),
        Color::White
    );
    assert_eq!(
        enforce_contrast(Color::Indexed(250), WHITE, AA_BODY_CONTRAST),
        Color::Indexed(250)
    );
    assert_eq!(
        enforce_contrast(TEXT_BODY, Color::Reset, AA_BODY_CONTRAST),
        TEXT_BODY
    );
    // 4.5:1 is reachable from *every* surface — the worst case is the
    // luminance where black and white tie, and even there the better pole
    // clears 4.58:1. So the floor never silently gives up.
    for gray in (0u8..=255).step_by(5) {
        let surface = Color::Rgb(gray, gray, gray);
        let lifted = enforce_contrast(TEXT_BODY, surface, AA_BODY_CONTRAST);
        let ratio = contrast_ratio(lifted, surface).unwrap();
        assert!(
            ratio >= AA_BODY_CONTRAST,
            "gray {gray:#04x} left body text at {ratio}:1"
        );
    }

    // An unreachable floor (AAA on a mid-gray) returns the better pole rather
    // than pretending it succeeded.
    let mid = Color::Rgb(0x80, 0x80, 0x80);
    assert_eq!(enforce_contrast(TEXT_BODY, mid, 7.0), BLACK);
}

#[test]
fn effective_surface_prefers_painted_background_then_measurement() {
    // A painted cell knows its own surface.
    assert_eq!(
        effective_surface(WHALE_PANEL, Some(WHITE)),
        Some(WHALE_PANEL)
    );
    // An unpainted cell falls through to what detection measured.
    assert_eq!(effective_surface(Color::Reset, Some(WHITE)), Some(WHITE));
    // With no measurement there is no surface — the floor stands down.
    assert_eq!(effective_surface(Color::Reset, None), None);
    // A measurement we cannot resolve is not a measurement.
    assert_eq!(effective_surface(Color::Reset, Some(Color::Reset)), None);
}

#[test]
fn text_contrast_floor_applies_to_glyphs_not_frame_chrome() {
    assert!(symbol_needs_text_contrast("a"));
    assert!(symbol_needs_text_contrast("字"));
    assert!(symbol_needs_text_contrast("→"));
    assert!(!symbol_needs_text_contrast(" "));
    assert!(!symbol_needs_text_contrast(""));
    assert!(!symbol_needs_text_contrast("─"));
    assert!(!symbol_needs_text_contrast("│"));
    assert!(!symbol_needs_text_contrast("█"));
    assert!(!symbol_needs_text_contrast("▏"));
    assert!(!symbol_needs_text_contrast("●"));
}

#[test]
fn background_luminance_decides_polarity_without_a_color_list() {
    assert_eq!(palette_mode_for_background(WHITE), Some(PaletteMode::Light));
    assert_eq!(palette_mode_for_background(BLACK), Some(PaletteMode::Dark));
    assert_eq!(
        palette_mode_for_background(LIGHT_SURFACE),
        Some(PaletteMode::Light)
    );
    assert_eq!(
        palette_mode_for_background(SOLARIZED_SURFACE),
        Some(PaletteMode::Light)
    );
    assert_eq!(
        palette_mode_for_background(WHALE_BG),
        Some(PaletteMode::Dark)
    );
    assert_eq!(
        palette_mode_for_background(Color::Rgb(0x28, 0x2C, 0x34)),
        Some(PaletteMode::Dark)
    );
    assert_eq!(palette_mode_for_background(Color::Reset), None);
}

#[test]
fn unknown_background_keeps_the_dark_default_and_offers_no_surface() {
    let unknown = TerminalBackground::unknown();
    assert_eq!(unknown.mode(), PaletteMode::Dark);
    assert_eq!(unknown.color(), None);
    assert_eq!(unknown.source(), BackgroundSource::Unknown);
    // This is the #4833 trigger environment: a terminal that sets no
    // COLORFGBG and is not macOS. Detection still answers Dark — but it says
    // so with `Unknown` provenance and no color, so nothing downstream
    // mistakes the guess for a measurement.
    let resolved = resolve_terminal_background(None, None, None);
    assert_eq!(resolved, unknown);
    assert_eq!(effective_surface(Color::Reset, resolved.color()), None);
}

#[test]
fn measured_background_outranks_env_hints_and_records_provenance() {
    // A white terminal that also exports a dark-looking COLORFGBG: the
    // measurement wins, and it carries the color the floor needs.
    let measured = resolve_terminal_background(Some((0xFF, 0xFF, 0xFF)), Some("15;0"), None);
    assert_eq!(measured.mode(), PaletteMode::Light);
    assert_eq!(measured.color(), Some(WHITE));
    assert_eq!(measured.source(), BackgroundSource::Osc11);

    // COLORFGBG with a resolvable xterm index yields a real color too.
    let indexed = resolve_terminal_background(None, Some("0;231"), None);
    assert_eq!(indexed.mode(), PaletteMode::Light);
    assert_eq!(indexed.color(), Some(Color::Indexed(231)));
    assert_eq!(indexed.source(), BackgroundSource::ColorFgBg);

    // Indices 0..=15 are terminal-profile defined: mode only, no color.
    let ansi = resolve_terminal_background(None, Some("0;15"), None);
    assert_eq!(ansi.mode(), PaletteMode::Light);
    assert_eq!(ansi.color(), None);
    assert_eq!(ansi.source(), BackgroundSource::ColorFgBg);

    // macOS appearance describes the OS, not the terminal — no color.
    let macos = resolve_terminal_background(None, None, Some(PaletteMode::Light));
    assert_eq!(macos.mode(), PaletteMode::Light);
    assert_eq!(macos.color(), None);
    assert_eq!(macos.source(), BackgroundSource::MacOsAppearance);
}

#[test]
fn osc11_replies_parse_across_the_shapes_terminals_emit() {
    assert_eq!(
        parse_osc11_reply("\u{1b}]11;rgb:ffff/ffff/ffff"),
        Some((255, 255, 255))
    );
    assert_eq!(
        parse_osc11_reply("]11;rgb:0000/0000/0000\u{7}"),
        Some((0, 0, 0))
    );
    // 8-bit channels, and a mid value that must scale rather than truncate.
    assert_eq!(parse_osc11_reply("rgb:1e/1e/1e"), Some((30, 30, 30)));
    assert_eq!(
        parse_osc11_reply("rgb:8000/8000/8000"),
        Some((128, 128, 128))
    );
    assert_eq!(parse_osc11_reply("rgb:f/f/f"), Some((255, 255, 255)));
    // Hash forms.
    assert_eq!(parse_osc11_reply("]11;#282c34"), Some((0x28, 0x2C, 0x34)));
    assert_eq!(parse_osc11_reply("#fff"), Some((255, 255, 255)));
    // Anything we cannot read is `None`, never a fabricated color.
    assert_eq!(parse_osc11_reply(""), None);
    assert_eq!(parse_osc11_reply("\u{1b}]11;"), None);
    assert_eq!(parse_osc11_reply("rgb:ff/ff"), None);
    assert_eq!(parse_osc11_reply("rgb:ff/ff/ff/ff"), None);
    assert_eq!(parse_osc11_reply("rgb:zz/zz/zz"), None);
    assert_eq!(parse_osc11_reply("#ff00"), None);
}

/// Drive a whole byte stream through the split the way the reader does,
/// including the one-byte lookahead after an `ESC \` terminator.
fn split_probe_stream(query: &[u8], stream: &[u8], csi: bool) -> (Vec<u8>, Vec<u8>) {
    let mut split = ProbeSplit::for_query(query, csi);
    let mut index = 0;
    while index < stream.len() {
        let byte = stream[index];
        index += 1;
        match split.feed(byte) {
            ProbeStep::Continue => {}
            ProbeStep::Done | ProbeStep::Overflow => break,
            ProbeStep::AwaitStringTerminator => {
                if let Some(next) = stream.get(index) {
                    split.finish_string_terminator(*next);
                }
                break;
            }
        }
    }
    split.finish()
}

#[test]
fn a_probe_reply_never_swallows_the_line_typed_before_it() {
    // #5925: `/plugin install …` typed at launch arrived before the terminal
    // answered the OSC 11 query. The reply is ours; every other byte is the
    // user's and must come back out, in order.
    let (reply, carried) = split_probe_stream(
        b"\x1b]11;?\x1b\\",
        b"/plugin install /tmp/bundle\r\x1b]11;rgb:ffff/ffff/ffff\x1b\\",
        false,
    );
    assert_eq!(
        parse_osc11_reply(&String::from_utf8_lossy(&reply)),
        Some((255, 255, 255)),
        "the reply still parses"
    );
    assert_eq!(
        carried,
        b"/plugin install /tmp/bundle\r".to_vec(),
        "not one typed byte is consumed or reordered"
    );
}

#[test]
fn a_probe_that_is_never_answered_still_hands_back_what_was_typed() {
    let (reply, carried) = split_probe_stream(b"\x1b]11;?\x1b\\", b"/plugin list\r", false);
    assert!(reply.is_empty(), "no reply arrived");
    assert_eq!(carried, b"/plugin list\r".to_vec());
}

#[test]
fn an_escape_that_is_not_the_reply_is_the_users_keystroke() {
    // A bare `Esc`, then typing, then the real reply.
    let (reply, carried) = split_probe_stream(
        b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\",
        b"\x1bhi\x1b_Gi=31;OK\x1b\\",
        false,
    );
    assert_eq!(reply, b"\x1b_Gi=31;OK".to_vec());
    assert_eq!(carried, b"\x1bhi".to_vec());
}

#[test]
fn a_keystroke_after_the_string_terminator_escape_is_kept() {
    // The reply ends `ESC \`; if the byte after `ESC` is not `\` it belongs
    // to the user and must not vanish with the terminator.
    let (reply, carried) = split_probe_stream(b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0/0/0\x1bx", false);
    assert_eq!(reply, b"\x1b]11;rgb:0/0/0".to_vec());
    assert_eq!(carried, b"x".to_vec());
}

#[test]
fn a_da_reply_stops_at_its_final_byte_and_keeps_the_rest_of_the_line() {
    let (reply, carried) = split_probe_stream(b"\x1b[c", b"\x1b[?62;4c/plugin list\r", true);
    assert_eq!(reply, b"\x1b[?62;4c".to_vec());
    // Bytes after the DA reply were never read by the split; the reader
    // leaves them in the tty for the input pump.
    assert!(carried.is_empty(), "{carried:?}");
}

#[test]
fn measured_light_background_selects_the_light_theme_end_to_end() {
    // Detection → mode → theme: an OSC 11 answer of white must reach the
    // light UiTheme, which is what actually repaints the frame.
    let measured = resolve_terminal_background(Some((0xFA, 0xFA, 0xFA)), None, None);
    assert_eq!(UiTheme::for_mode(measured.mode()), LIGHT_UI_THEME);
    let dark = resolve_terminal_background(Some((0x1E, 0x1E, 0x1E)), None, None);
    assert_eq!(UiTheme::for_mode(dark.mode()), UI_THEME);
}

// === #4813: cross-theme contrast audit ===

#[test]
fn grayscale_background_roles_survive_direct_and_token_render_paths() {
    let theme = GRAYSCALE_UI_THEME;
    for (token, expected) in [
        (WHALE_BG, theme.surface_bg),
        (WHALE_PANEL, theme.panel_bg),
        (SURFACE_TOOL_ACTIVE, theme.elevated_bg),
        (SELECTION_BG, theme.selection_bg),
        (LIGHT_SELECTION_BG, theme.selection_bg),
        (SURFACE_REASONING, GRAYSCALE_REASONING),
        (DIFF_ADDED_BG, theme.diff_added_bg),
        (DIFF_DELETED_BG, theme.diff_deleted_bg),
    ] {
        let resolved = adapt_bg_for_palette_mode(token, PaletteMode::Grayscale);
        assert_eq!(resolved, expected, "token {token:?}");
        assert_eq!(
            adapt_bg_for_palette_mode(expected, PaletteMode::Grayscale),
            expected,
            "direct theme role {expected:?} must not be bucketed twice",
        );
    }
}

#[test]
fn rendered_workbench_selection_tracks_every_selectable_theme() {
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        style::{Modifier, Style},
        widgets::{Block, Paragraph, Widget},
    };

    // Menus and Config paint legacy semantic tokens; the home/composer use
    // direct UiTheme roles. The final cell remap must make those surfaces
    // agree, including the selected row's padding, on every picker choice.
    for theme_id in SELECTABLE_THEMES {
        let theme = theme_id.ui_theme();
        for width in [40, 80] {
            let area = Rect::new(0, 0, width, 3);
            let mut buffer = Buffer::empty(area);
            Block::default()
                .style(Style::default().bg(WHALE_BG))
                .render(area, &mut buffer);
            Paragraph::new("Current workspace")
                .style(Style::default().fg(TEXT_BODY))
                .render(Rect::new(0, 0, width, 1), &mut buffer);
            Paragraph::new("> Recent session")
                .style(
                    Style::default()
                        .fg(super::tokens::SELECTION_TEXT)
                        .bg(SELECTION_BG)
                        .add_modifier(Modifier::BOLD),
                )
                .render(Rect::new(0, 1, width, 1), &mut buffer);

            // The same two semantic adaptation stages as ColorCompatBackend,
            // before terminal-depth quantization or any contrast correction.
            for cell in &mut buffer.content {
                cell.fg = adapt_fg_for_theme(cell.fg, *theme_id, &theme);
                cell.bg = adapt_bg_for_theme(cell.bg, *theme_id, &theme);
                cell.fg = adapt_fg_for_palette_mode(cell.fg, cell.bg, theme.mode);
                cell.bg = adapt_bg_for_palette_mode(cell.bg, theme.mode);
            }
            assert_eq!(buffer[(0, 0)].fg, theme.text_body, "{} body", theme.name);
            assert_eq!(buffer[(0, 0)].bg, theme.surface_bg, "{} field", theme.name);
            for x in 0..width {
                let selected = &buffer[(x, 1)];
                assert_eq!(selected.fg, theme.text_body, "{} selected ink", theme.name);
                assert_eq!(selected.bg, theme.selection_bg, "{} selection", theme.name);
                assert!(selected.modifier.contains(Modifier::BOLD));
            }
            if let Some(ratio) =
                super::contrast::contrast_ratio(buffer[(0, 1)].fg, buffer[(0, 1)].bg)
            {
                assert!(ratio >= 4.5, "{} selected text: {ratio}", theme.name);
            }
        }
    }
}

#[test]
fn every_selectable_theme_clears_the_text_floor() {
    let mut terminal_owned = Vec::new();
    for theme_id in SELECTABLE_THEMES {
        let theme = theme_id.ui_theme();
        if theme_uses_terminal_owned_surfaces(&theme) {
            terminal_owned.push(theme_id.name());
            continue;
        }
        let violations = theme_contrast_violations(&theme);
        assert!(
            violations.is_empty(),
            "theme '{}' fails the contrast audit: {violations:?}",
            theme_id.name(),
        );
    }
    // System resolves to one member of the built-in Whale pair. Those Flat
    // shells and Terminal are intentionally host-owned; community themes stay
    // painted and therefore remain fully auditable here.
    assert_eq!(terminal_owned, ["system", "terminal", "dark", "light"]);
}

#[test]
fn terminal_native_theme_exemptions_are_explicit() {
    // Reset surfaces are terminal-defined, so the audit records zero
    // violations for those pairs. That is *not* a pass — unresolvable pairs
    // are skipped, never counted as clearing the floor. Deepsea's authored
    // RGB ramp is audited separately by the ocean tests.
    for theme in [TERMINAL_UI_THEME, UI_THEME, LIGHT_UI_THEME] {
        assert!(theme_uses_terminal_owned_surfaces(&theme));
        assert_eq!(theme.surface_bg, Color::Reset);
        assert!(theme_contrast_violations(&theme).is_empty());
    }
    // A painted community theme never gets the exemption.
    assert!(!theme_uses_terminal_owned_surfaces(&TOKYO_NIGHT_UI_THEME));
}

#[test]
fn high_contrast_grayscale_theme_clears_body_floor_on_every_surface() {
    // The picker tagline claims "Color-minimal high contrast" — hold the
    // grayscale theme to the full body-text floor on every surface, not the
    // 3:1 secondary-chrome floor.
    let theme = GRAYSCALE_UI_THEME;
    for fg in [theme.text_body, theme.text_soft, theme.text_muted] {
        for bg in [
            theme.surface_bg,
            theme.panel_bg,
            theme.composer_bg,
            theme.elevated_bg,
        ] {
            let ratio = contrast_ratio(fg, bg).expect("grayscale colors are all resolvable RGB");
            assert!(
                ratio >= AA_BODY_CONTRAST,
                "grayscale {fg:?} on {bg:?} is {ratio}:1, below the {AA_BODY_CONTRAST}:1 floor",
            );
        }
    }
}

#[test]
fn violation_report_names_pair_and_ratio() {
    let mut bad = TOKYO_NIGHT_UI_THEME;
    bad.text_muted = bad.surface_bg;
    let violations = theme_contrast_violations(&bad);
    // The muted-on-surface color fails against all four text surfaces.
    let muted_pairs: Vec<_> = violations
        .iter()
        .filter(|violation| violation.pair.starts_with("text_muted on "))
        .collect();
    assert_eq!(muted_pairs.len(), 4);
    let violation = violations
        .iter()
        .find(|violation| violation.pair == "text_muted on surface_bg")
        .expect("the report must name the failing pair");
    assert_eq!(violation.fg, bad.text_muted);
    assert_eq!(violation.bg, bad.surface_bg);
    // Identical colors sit at 1:1 — far under the floor the report carries.
    assert!(violation.ratio < 1.1);
    assert!(violation.ratio < violation.floor);
    assert_eq!(violation.floor, AA_BODY_CONTRAST);
}

#[test]
fn direct_field_paint_follows_the_terminal_owned_shell() {
    // Widgets that paint `bg(WHALE_BG)` directly follow the theme's shell
    // instead of laying a navy patch over the terminal's own ground.
    assert_eq!(
        adapt_bg_for_theme(WHALE_BG, ThemeId::Whale, &UI_THEME),
        Color::Reset
    );
    assert_eq!(
        adapt_bg_for_theme(WHALE_BG, ThemeId::WhaleLight, &LIGHT_UI_THEME),
        Color::Reset
    );
    // A user `background_color` override is the field for direct paints too.
    let custom = UI_THEME.with_background_color(Color::Rgb(1, 2, 3));
    assert_eq!(
        adapt_bg_for_theme(WHALE_BG, ThemeId::Whale, &custom),
        Color::Rgb(1, 2, 3)
    );
    // Semantic surfaces keep their paint on the whale pair.
    assert_eq!(
        adapt_bg_for_theme(SELECTION_BG, ThemeId::Whale, &UI_THEME),
        SELECTION_BG
    );
}

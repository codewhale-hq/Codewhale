//! Palette audit tests to prevent color drift.
//!
//! These tests ensure that deprecated colors are not used directly in
//! user-visible code. Backward-compatible DeepSeek aliases should point
//! at the current Codewhale semantic tokens instead of stale brand RGBs.

use ratatui::style::Color;

fn color_to_rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::Gray => (128, 128, 128),
        Color::DarkGray => (169, 169, 169),
        Color::Red => (255, 0, 0),
        Color::LightRed => (255, 102, 102),
        Color::Green => (0, 255, 0),
        Color::LightGreen => (102, 255, 102),
        Color::Yellow => (255, 255, 0),
        Color::LightYellow => (255, 255, 153),
        Color::Blue => (0, 0, 255),
        Color::LightBlue => (102, 153, 255),
        Color::Magenta => (255, 0, 255),
        Color::LightMagenta => (255, 153, 255),
        Color::Cyan => (0, 255, 255),
        Color::LightCyan => (153, 255, 255),
        _ => panic!("unsupported color variant for contrast test: {color:?}"),
    }
}

fn linearize_srgb(component: u8) -> f64 {
    let srgb = f64::from(component) / 255.0;
    if srgb <= 0.04045 {
        srgb / 12.92
    } else {
        ((srgb + 0.055) / 1.055).powf(2.4)
    }
}

fn relative_luminance(color: Color) -> f64 {
    let (r, g, b) = color_to_rgb(color);
    0.2126 * linearize_srgb(r) + 0.7152 * linearize_srgb(g) + 0.0722 * linearize_srgb(b)
}

fn contrast_ratio(foreground: Color, background: Color) -> f64 {
    let fg = relative_luminance(foreground);
    let bg = relative_luminance(background);
    if fg >= bg {
        (fg + 0.05) / (bg + 0.05)
    } else {
        (bg + 0.05) / (fg + 0.05)
    }
}

fn assert_min_contrast(label: &str, foreground: Color, background: Color, min_ratio: f64) {
    let ratio = contrast_ratio(foreground, background);
    assert!(
        ratio >= min_ratio,
        "{label} contrast {ratio:.2} is below minimum {min_ratio:.2}"
    );
}

// NOTE: The deprecated color audit (DEEPSEEK_AQUA) was removed because
// the deprecated constant no longer exists in the palette.

#[test]
fn verify_status_success_uses_success_token() {
    assert_eq!(
        codewhale_palette::STATUS_SUCCESS,
        Color::Rgb(
            codewhale_palette::WHALE_SUCCESS_RGB.0,
            codewhale_palette::WHALE_SUCCESS_RGB.1,
            codewhale_palette::WHALE_SUCCESS_RGB.2
        ),
        "STATUS_SUCCESS should use the current success token"
    );
    assert_ne!(
        codewhale_palette::STATUS_SUCCESS,
        codewhale_palette::WHALE_ACTION,
        "STATUS_SUCCESS should not regress to the primary accent"
    );
}

#[test]
fn whale_roles_are_pinned_and_non_colliding() {
    assert_eq!(codewhale_palette::WHALE_BG_RGB, (7, 12, 29));
    assert_eq!(codewhale_palette::WHALE_PANEL_RGB, (16, 28, 64));
    assert_eq!(codewhale_palette::WHALE_ELEVATED_RGB, (26, 44, 99));
    assert_eq!(codewhale_palette::WHALE_ACTION_RGB, (148, 190, 255));
    assert_eq!(
        codewhale_palette::WHALE_ACCENT_SECONDARY_RGB,
        (79, 209, 197)
    );
    assert_eq!(codewhale_palette::WHALE_HUMAN_RGB, (240, 190, 130));
    assert_eq!(codewhale_palette::WHALE_WARNING_RGB, (255, 122, 89));
    assert_eq!(codewhale_palette::WHALE_ERROR_RGB, (244, 168, 160));
    assert_eq!(codewhale_palette::WHALE_MODE_AGENT_RGB, (126, 180, 232));
    assert_eq!(codewhale_palette::WHALE_MODE_YOLO_RGB, (255, 112, 160));
    assert_eq!(codewhale_palette::WHALE_MODE_PLAN_RGB, (185, 220, 236));
    assert_eq!(codewhale_palette::WHALE_MODE_OPERATE_RGB, (173, 136, 255));
    assert_eq!(codewhale_palette::LIGHT_SUCCESS_FG_RGB, (20, 118, 61));
    assert_eq!(codewhale_palette::LIGHT_MODE_AGENT_RGB, (22, 54, 178));
    assert_eq!(codewhale_palette::LIGHT_MODE_PLAN_RGB, (52, 92, 128));
    assert_eq!(codewhale_palette::LIGHT_OPERATE_RGB, (112, 71, 184));
    assert_eq!(codewhale_palette::LIGHT_MODE_YOLO_RGB, (181, 35, 90));
    assert_eq!(
        codewhale_palette::LIGHT_USER_BODY,
        codewhale_palette::LIGHT_SUCCESS_FG
    );

    let ui = codewhale_palette::UI_THEME;
    assert_eq!(ui.accent_primary, codewhale_palette::WHALE_ACTION);
    assert_eq!(ui.info, codewhale_palette::WHALE_ACTION);
    assert_eq!(ui.status_working, codewhale_palette::WHALE_LIVE);
    assert_eq!(ui.accent_action, codewhale_palette::WHALE_HUMAN);
    assert_eq!(ui.warning, codewhale_palette::STATUS_WARNING);
    assert_eq!(ui.error_fg, codewhale_palette::WHALE_ERROR);
    assert_eq!(ui.mode_operate, codewhale_palette::MODE_OPERATE);
    assert_ne!(
        ui.mode_plan, ui.accent_action,
        "Plan is structural; Signal Gold is reserved for human attention"
    );
    assert_ne!(
        ui.status_working, ui.success,
        "live and done need separate ink"
    );
    assert_ne!(ui.accent_action, ui.warning, "human asks are not warnings");
    assert_ne!(
        ui.warning, ui.error_fg,
        "warning and danger must not collapse"
    );

    let foreground_domains = [
        ("action", codewhale_palette::WHALE_ACTION),
        ("live", codewhale_palette::WHALE_LIVE),
        ("human", codewhale_palette::WHALE_HUMAN),
        ("success", codewhale_palette::STATUS_SUCCESS),
        ("warning", codewhale_palette::STATUS_WARNING),
        ("danger", codewhale_palette::WHALE_ERROR),
        ("agent mode", codewhale_palette::MODE_AGENT),
        ("full-access mode", codewhale_palette::MODE_YOLO),
        ("plan mode", codewhale_palette::MODE_PLAN),
        ("operate mode", codewhale_palette::MODE_OPERATE),
        ("reasoning", codewhale_palette::TEXT_REASONING),
        ("diff added", codewhale_palette::DIFF_ADDED),
    ];
    for (index, (left_name, left)) in foreground_domains.iter().enumerate() {
        for (right_name, right) in foreground_domains.iter().skip(index + 1) {
            assert_ne!(
                left, right,
                "raw foreground adaptation domains '{left_name}' and '{right_name}' collide"
            );
        }
    }

    let background_domains = [
        ("base", codewhale_palette::WHALE_BG),
        ("panel", codewhale_palette::WHALE_PANEL),
        ("composer", codewhale_palette::WHALE_COMPOSER),
        ("elevated", codewhale_palette::SURFACE_ELEVATED),
        ("tool", codewhale_palette::SURFACE_TOOL),
        ("tool active", codewhale_palette::SURFACE_TOOL_ACTIVE),
        ("reasoning", codewhale_palette::SURFACE_REASONING),
        ("reasoning tint", codewhale_palette::SURFACE_REASONING_TINT),
        (
            "reasoning active",
            codewhale_palette::SURFACE_REASONING_ACTIVE,
        ),
        ("success", codewhale_palette::SURFACE_SUCCESS),
        ("error", codewhale_palette::SURFACE_ERROR),
        ("selection", codewhale_palette::SELECTION_BG),
        ("diff added", codewhale_palette::DIFF_ADDED_BG),
        ("diff deleted", codewhale_palette::DIFF_DELETED_BG),
    ];
    for (index, (left_name, left)) in background_domains.iter().enumerate() {
        for (right_name, right) in background_domains.iter().skip(index + 1) {
            assert_ne!(
                left, right,
                "raw background adaptation domains '{left_name}' and '{right_name}' collide"
            );
        }
    }
}

#[test]
fn contrast_guardrails_for_key_ui_pairs() {
    let min_readable = 4.5;

    assert_min_contrast(
        "TEXT_BODY on WHALE_BG",
        codewhale_palette::TEXT_BODY,
        codewhale_palette::WHALE_BG,
        min_readable,
    );
    assert_min_contrast(
        "TEXT_SECONDARY on WHALE_BG",
        codewhale_palette::TEXT_SECONDARY,
        codewhale_palette::WHALE_BG,
        min_readable,
    );
    assert_min_contrast(
        "TEXT_HINT on WHALE_BG",
        codewhale_palette::TEXT_HINT,
        codewhale_palette::WHALE_BG,
        min_readable,
    );
    assert_min_contrast(
        "STATUS_WARNING on WHALE_BG",
        codewhale_palette::STATUS_WARNING,
        codewhale_palette::WHALE_BG,
        min_readable,
    );
    assert_min_contrast(
        "STATUS_ERROR on WHALE_BG",
        codewhale_palette::STATUS_ERROR,
        codewhale_palette::WHALE_BG,
        min_readable,
    );
    assert_min_contrast(
        "SELECTION_TEXT on SELECTION_BG",
        codewhale_palette::SELECTION_TEXT,
        codewhale_palette::SELECTION_BG,
        min_readable,
    );
    assert_min_contrast(
        "TEXT_PRIMARY on SURFACE_ELEVATED",
        codewhale_palette::TEXT_PRIMARY,
        codewhale_palette::SURFACE_ELEVATED,
        min_readable,
    );
    for (label, foreground) in [
        ("action", codewhale_palette::UI_THEME.accent_primary),
        ("live", codewhale_palette::UI_THEME.status_working),
        ("human", codewhale_palette::UI_THEME.accent_action),
        ("warning", codewhale_palette::UI_THEME.warning),
        ("danger", codewhale_palette::UI_THEME.error_fg),
        ("act mode", codewhale_palette::UI_THEME.mode_agent),
        ("plan mode", codewhale_palette::UI_THEME.mode_plan),
        ("operate", codewhale_palette::UI_THEME.mode_operate),
        ("full-access mode", codewhale_palette::UI_THEME.mode_yolo),
        ("success", codewhale_palette::UI_THEME.success),
    ] {
        assert_min_contrast(
            label,
            foreground,
            codewhale_palette::SURFACE_ELEVATED,
            min_readable,
        );
    }
    let light_foregrounds = [
        ("body", codewhale_palette::LIGHT_UI_THEME.text_body),
        ("soft", codewhale_palette::LIGHT_UI_THEME.text_soft),
        ("muted", codewhale_palette::LIGHT_UI_THEME.text_muted),
        ("hint", codewhale_palette::LIGHT_UI_THEME.text_hint),
        ("action", codewhale_palette::LIGHT_UI_THEME.accent_primary),
        ("live", codewhale_palette::LIGHT_UI_THEME.status_working),
        ("human", codewhale_palette::LIGHT_UI_THEME.accent_action),
        ("warning", codewhale_palette::LIGHT_UI_THEME.warning),
        ("danger", codewhale_palette::LIGHT_UI_THEME.error_fg),
        ("act mode", codewhale_palette::LIGHT_UI_THEME.mode_agent),
        ("plan mode", codewhale_palette::LIGHT_UI_THEME.mode_plan),
        ("operate", codewhale_palette::LIGHT_UI_THEME.mode_operate),
        (
            "full-access mode",
            codewhale_palette::LIGHT_UI_THEME.mode_yolo,
        ),
        ("success", codewhale_palette::LIGHT_UI_THEME.success),
        ("user", codewhale_palette::LIGHT_USER_BODY),
    ];
    for (background_name, background) in [
        ("surface", codewhale_palette::LIGHT_SURFACE),
        ("panel", codewhale_palette::LIGHT_PANEL),
        ("raised", codewhale_palette::LIGHT_ELEVATED),
        ("selection", codewhale_palette::LIGHT_SELECTION_BG),
        ("reasoning", codewhale_palette::LIGHT_REASONING),
        ("success tint", codewhale_palette::LIGHT_SUCCESS),
        ("error tint", codewhale_palette::LIGHT_ERROR),
    ] {
        for (foreground_name, foreground) in light_foregrounds {
            assert_min_contrast(
                &format!("light {foreground_name} on {background_name}"),
                foreground,
                background,
                min_readable,
            );
        }
    }
    assert_min_contrast(
        "light user row on raised",
        codewhale_palette::LIGHT_USER_BODY,
        codewhale_palette::LIGHT_ELEVATED,
        min_readable,
    );
    assert_min_contrast(
        "light work-surface success hover on raised",
        codewhale_palette::LIGHT_UI_THEME.success,
        codewhale_palette::LIGHT_UI_THEME.elevated_bg,
        min_readable,
    );
}

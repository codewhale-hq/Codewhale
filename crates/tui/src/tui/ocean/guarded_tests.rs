use super::*;
use crate::tui::color_compat::ColorCompatBackend;
use codewhale_ratatui::ocean::{OceanPaintFacts, ocean_semantic_surfaces};
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

fn caps(depth: codewhale_palette::ColorDepth) -> codewhale_ratatui::Caps {
    ColorCompatBackend::new(std::io::sink(), depth, codewhale_palette::PaletteMode::Dark)
        .native_ocean_caps()
}
fn column(area: Rect, phase: ShellPhase, elapsed: u128) -> OceanColumn {
    OceanColumn::new(
        OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).unwrap(),
        area,
        elapsed,
        None,
        phase,
        true,
        750,
        30,
    )
    .with_paint_caps(Some(caps(codewhale_palette::ColorDepth::TrueColor)))
}
fn ordinary(area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    for cell in &mut buf.content {
        cell.set_bg(codewhale_palette::UNDERWATER_UI_THEME.surface_bg)
            .set_fg(Color::Rgb(255, 255, 255));
    }
    buf
}
#[test]
fn guarded_ocean_backend_depth_is_forwarded_and_explicit_pane_supplies_ground_evidence() {
    use codewhale_ratatui::{color::ColorDepth as Depth, detect::Appearance};
    for (host, native) in [
        (codewhale_palette::ColorDepth::TrueColor, Depth::TrueColor),
        (codewhale_palette::ColorDepth::Ansi256, Depth::Ansi256),
        (codewhale_palette::ColorDepth::Ansi16, Depth::Ansi16),
        (codewhale_palette::ColorDepth::Monochrome, Depth::Monochrome),
    ] {
        let reported = caps(host);
        assert_eq!(reported.depth, native);
        assert_eq!(
            reported.appearance,
            Appearance::Unknown,
            "OS-dark mode is not measured ground"
        );
        let area = Rect::new(7, 5, 12, 4);
        let mut buf = ordinary(area);
        let before = buf.clone();
        let column = column(area, ShellPhase::Idle, 0).with_paint_caps(Some(reported));
        column.paint_matching_native(
            area,
            &mut buf,
            codewhale_palette::UNDERWATER_UI_THEME.surface_bg,
            &codewhale_palette::UNDERWATER_UI_THEME,
            &[],
        );
        if native == Depth::TrueColor {
            assert_ne!(buf, before, "actual dark pane supplies its own evidence");
        } else {
            assert_eq!(
                buf, before,
                "fallback retains the selected flat pane ground"
            );
        }
    }
    let area = Rect::new(7, 5, 12, 4);
    let col = column(area, ShellPhase::Idle, 0);
    for ground in [Color::Reset, Color::White, Color::Rgb(250, 250, 250)] {
        let mut buf = ordinary(area);
        buf.set_style(area, Style::default().bg(ground));
        let before = buf.clone();
        col.paint_matching_native(
            area,
            &mut buf,
            ground,
            &codewhale_palette::UNDERWATER_UI_THEME,
            &[],
        );
        assert_eq!(
            buf, before,
            "unknown or known-light explicit grounds cannot earn a dark field"
        );
    }
}
#[test]
fn guarded_ocean_whole_buffer_matches_frozen_safe_painter_across_phase_size_and_cache_rows() {
    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    for phase in [
        ShellPhase::Idle,
        ShellPhase::Typing,
        ShellPhase::Working,
        ShellPhase::Verifying,
        ShellPhase::Waiting,
        ShellPhase::Approval,
        ShellPhase::Done,
        ShellPhase::Failed,
    ] {
        for (width, height) in [(4, 3), (40, 10), (80, 24)] {
            for elapsed in [0, 320, 22500, 89999] {
                let area = Rect::new(7, 5, width, height);
                let col = column(area, phase, elapsed);
                let mut actual = ordinary(area);
                for x in area.x..area.right() {
                    actual[(x, area.y)].set_symbol("x");
                }
                let mut expected = actual.clone();
                guarded_legacy::LegacyColumn(col).paint_matching(
                    area,
                    &mut expected,
                    theme.surface_bg,
                );
                col.paint_matching_native(area, &mut actual, theme.surface_bg, &theme, &[]);
                assert_eq!(
                    actual, expected,
                    "phase={phase:?} area={area:?} elapsed={elapsed}"
                );
                // Actual transcript path preserves the same frame ramp cache receipts.
                let ramp = crate::tui::ambient_life::frame_ocean_ramp(
                    &col,
                    area.height,
                    area.y,
                    elapsed,
                    col.phase_tag(),
                    col.ramp_fingerprint(),
                );
                let facts = OceanPaintFacts {
                    ground: theme.surface_bg,
                    sample_top: area.y,
                    samples: &ramp,
                    protected: &[],
                };
                let mut cached = ordinary(area);
                let mut old = cached.clone();
                guarded_legacy::LegacyWater {
                    ocean_column: Some(col),
                    lines: vec![],
                    background: theme.surface_bg,
                    ocean_elapsed_ms: elapsed,
                }
                .paint(area, &mut old);
                col.paint_native(area, &mut cached, &theme, &facts);
                assert_eq!(cached, old);
            }
        }
    }
}
#[test]
fn guarded_ocean_semantic_alias_reverse_unknown_ink_and_style_facts_survive_final_shell_pass() {
    let theme = codewhale_palette::UNDERWATER_UI_THEME;
    let area = Rect::new(7, 5, 20, 4);
    let col = column(area, ShellPhase::Idle, 0);
    let rows = vec![Line::from(vec![
        Span::raw("plain "),
        Span::styled(
            "selected",
            Style::default()
                .bg(theme.surface_bg)
                .add_modifier(Modifier::ITALIC),
        ),
    ])];
    let protected = ocean_semantic_surfaces(
        &rows,
        area,
        codewhale_tui_kit::ui_text::grapheme_display_width,
    );
    assert_eq!(protected, vec![Rect::new(13, 5, 8, 1)]);
    let mut buf = ordinary(area);
    buf[(7, 6)].set_symbol("x").set_fg(Color::Reset);
    buf[(8, 6)].set_symbol("x").set_fg(Color::Magenta);
    buf[(9, 6)].set_symbol("x").set_fg(col.color_at_y(6));
    buf[(10, 6)].set_bg(theme.selection_bg);
    buf[(11, 6)]
        .modifier
        .insert(Modifier::REVERSED | Modifier::BOLD);
    let before = buf.clone();
    col.paint_matching_native(area, &mut buf, theme.surface_bg, &theme, &protected);
    for x in [8, 10, 11] {
        assert_eq!(buf[(x, 6)], before[(x, 6)]);
    }
    // The terminal backend lifts water-coloured glyphs to visible body ink;
    // the guard projects that same adaptation before admitting the backdrop.
    for x in [7, 9] {
        assert_eq!(buf[(x, 6)].fg, before[(x, 6)].fg);
        assert_eq!(buf[(x, 6)].modifier, before[(x, 6)].modifier);
        assert_eq!(buf[(x, 6)].bg, col.color_at_y(6));
    }
    for x in 13..21 {
        assert_eq!(
            buf[(x, 5)],
            before[(x, 5)],
            "explicit style aliases remain semantic"
        );
    }
    assert_eq!(buf[(7, 5)].bg, col.color_at_y(5));
}
#[test]
fn guarded_ocean_uses_backend_projected_custom_ink_and_live_supporting_floor() {
    let mut theme = codewhale_palette::UNDERWATER_UI_THEME;
    theme.text_body = Color::Rgb(255, 255, 255);
    theme.border = Color::Rgb(111, 149, 181);
    let area = Rect::new(7, 5, 8, 4);
    let col = column(area, ShellPhase::Idle, 0);
    let mut buf = ordinary(area);
    buf[(7, 5)]
        .set_symbol("x")
        .set_fg(codewhale_palette::TEXT_BODY);
    buf[(8, 5)]
        .set_symbol("|")
        .set_fg(theme.border)
        .modifier
        .insert(Modifier::ITALIC);
    let before = buf.clone();
    let projected = crate::tui::color_compat::project_ocean_ink(
        &buf[(7, 5)],
        col.color_at_y(5),
        caps(codewhale_palette::ColorDepth::TrueColor).depth,
        &theme,
    );
    // Underwater retains its body token; custom supporting ink still follows
    // the live theme and the backend's contrast projection.
    assert_eq!(projected, codewhale_palette::TEXT_BODY);
    col.paint_matching_native(area, &mut buf, theme.surface_bg, &theme, &[]);
    assert_eq!(buf[(7, 5)].bg, col.color_at_y(5));
    assert_eq!(
        buf[(7, 5)].fg,
        before[(7, 5)].fg,
        "guard never rewrites source ink"
    );
    assert_eq!(buf[(8, 5)].modifier, before[(8, 5)].modifier);
    assert_eq!(buf[(8, 5)].bg, col.color_at_y(5));
}
#[test]
fn guarded_ocean_caustic_safe_pixels_match_frozen_math_and_spare_semantic_blank_cells() {
    let area = Rect::new(7, 5, 80, 24);
    let col = column(area, ShellPhase::Working, 480);
    let mut actual = ordinary(area);
    col.paint_matching_native(
        area,
        &mut actual,
        codewhale_palette::UNDERWATER_UI_THEME.surface_bg,
        &codewhale_palette::UNDERWATER_UI_THEME,
        &[],
    );
    let mut expected = actual.clone();
    guarded_legacy::apply_caustic_shimmer(area, &mut expected, &col, 480, true, &[]);
    crate::tui::ambient_life::apply_caustic_shimmer(area, &mut actual, &col, 480, true, &[]);
    assert_eq!(actual, expected);
    let rows = [Line::styled(
        "",
        Style::default().bg(col.color_at_y(area.y)),
    )];
    let mut buf = ordinary(area);
    col.paint_matching_native(
        area,
        &mut buf,
        codewhale_palette::UNDERWATER_UI_THEME.surface_bg,
        &codewhale_palette::UNDERWATER_UI_THEME,
        &[],
    );
    buf[(10, 6)].set_bg(Color::Rgb(73, 89, 107));
    buf[(13, 6)].modifier.insert(Modifier::REVERSED);
    let before = buf.clone();
    crate::tui::ambient_life::apply_caustic_shimmer(area, &mut buf, &col, 480, true, &rows);
    for x in area.x..area.right() {
        assert_eq!(buf[(x, area.y)], before[(x, area.y)]);
    }
    assert_eq!(buf[(10, 6)], before[(10, 6)]);
    assert_eq!(buf[(13, 6)], before[(13, 6)]);
}
#[test]
fn guarded_ocean_missing_capability_still_motion_and_clip_cannot_modify_guards() {
    let area = Rect::new(7, 5, 80, 24);
    let col = column(area, ShellPhase::Working, 480);
    for col in [
        col.with_paint_caps(None),
        OceanColumn::new(
            OceanRamp::for_theme(&codewhale_palette::UNDERWATER_UI_THEME).unwrap(),
            area,
            480,
            None,
            ShellPhase::Working,
            false,
            0,
            0,
        )
        .with_paint_caps(Some(caps(codewhale_palette::ColorDepth::TrueColor))),
    ] {
        let mut buf = ordinary(area);
        // Establish real ordinary water first, so missing caps and Still
        // must stop a pass that would otherwise have eligible pixels.
        column(area, ShellPhase::Working, 480).paint_matching_native(
            area,
            &mut buf,
            codewhale_palette::UNDERWATER_UI_THEME.surface_bg,
            &codewhale_palette::UNDERWATER_UI_THEME,
            &[],
        );
        let before = buf.clone();
        crate::tui::ambient_life::apply_caustic_shimmer(area, &mut buf, &col, 480, true, &[]);
        assert_eq!(buf, before);
    }
    let mut buf = ordinary(area);
    let before = buf.clone();
    let requested = Rect::new(9, 7, 8, 3);
    col.paint_matching_native(
        requested,
        &mut buf,
        codewhale_palette::UNDERWATER_UI_THEME.surface_bg,
        &codewhale_palette::UNDERWATER_UI_THEME,
        &[],
    );
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if !requested.contains((x, y).into()) {
                assert_eq!(buf[(x, y)], before[(x, y)]);
            }
        }
    }
}

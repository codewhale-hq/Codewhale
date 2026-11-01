//! Host contracts around the shared native posture renderer.

use codewhale_palette::{ChromeInk, UI_THEME, UiTheme};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use super::{TidelineFooter, render_tideline_footer};

fn custom_theme() -> UiTheme {
    UiTheme {
        status_working: Color::Rgb(1, 2, 3),
        permission_ask: Color::Rgb(2, 3, 4),
        permission_auto_review: Color::Rgb(3, 4, 5),
        permission_full_access: Color::Rgb(4, 5, 6),
        accent_action: Color::Rgb(5, 6, 7),
        warning: Color::Rgb(6, 7, 8),
        accent_primary: Color::Rgb(7, 8, 9),
        text_soft: Color::Rgb(8, 9, 10),
        text_muted: Color::Rgb(9, 10, 11),
        text_hint: Color::Rgb(10, 11, 12),
        text_dim: Color::Rgb(11, 12, 13),
        error_fg: Color::Rgb(12, 13, 14),
        ..UI_THEME
    }
}

#[test]
fn posture_retains_every_host_ink_including_the_pinned_right_fact() {
    let theme = custom_theme();
    for ink in [
        ChromeInk::Outcome,
        ChromeInk::PermissionAsk,
        ChromeInk::PermissionAutoReview,
        ChromeInk::PermissionFullAccess,
        ChromeInk::Waiting,
        ChromeInk::Attention,
        ChromeInk::Active,
        ChromeInk::PolicyAct,
        ChromeInk::PolicyPlan,
        ChromeInk::PolicyOperate,
        ChromeInk::Identity,
        ChromeInk::Info,
        ChromeInk::MetadataValue,
        ChromeInk::Metadata,
        ChromeInk::MetadataHint,
        ChromeInk::MetadataDim,
        ChromeInk::Failure,
    ] {
        let counts = [("C (Ctrl+])".into(), ink)];
        let footer = TidelineFooter::new(&theme, ("P", ink))
            .permission_key(Some("K"))
            .mode_chip(Some(("M", ink)))
            .mode_key(Some("Tab"))
            .turn_clock(Some(("W", ink)))
            .counts(&counts)
            .session_clock(Some(("S", ink)))
            .hint(Some(("H", ink)))
            .right(Some(("R", ink)));
        let area = Rect::new(3, 2, 100, 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 110, 5));
        buf.set_style(
            buf.area,
            Style::default()
                .fg(crate::tui::infoline::source_theme(false)
                    .color(codewhale_ratatui::Role::Accent)
                    .unwrap())
                .bg(Color::Rgb(30, 40, 50))
                .add_modifier(Modifier::ITALIC),
        );
        let before = buf.clone();
        let targets = render_tideline_footer(area, &mut buf, &footer);
        let column = |symbol: &str| {
            (area.left()..area.right())
                .find(|x| buf[(*x, area.y)].symbol() == symbol)
                .unwrap_or_else(|| panic!("{ink:?}: {symbol} did not paint"))
        };
        for symbol in ["P", "M", "W", "C", "S", "H", "R"] {
            let cell = &buf[(column(symbol), area.y)];
            assert_eq!(cell.fg, ink.color(&theme), "{ink:?}: {symbol}");
            assert_eq!(cell.bg, Color::Rgb(30, 40, 50));
            assert!(cell.modifier.contains(Modifier::ITALIC));
            assert_eq!(cell.modifier.contains(Modifier::BOLD), symbol == "P");
        }
        assert_eq!(buf[(column("K"), area.y)].fg, theme.text_hint);
        assert_eq!(buf[(column("M") + 2, area.y)].fg, theme.text_hint);
        assert_eq!(targets, [(0, Rect::new(column("C"), area.y, 10, 1))]);
        assert_eq!(buf[(column("C") + 2, area.y)].fg, theme.text_hint);
        assert_eq!(buf[(column("M") - 1, area.y)].fg, theme.text_dim);
        for position in [(3, 2), (102, 2), (60, 2), (3, 3), (109, 4)] {
            assert_eq!(buf[position], before[position], "{ink:?}: {position:?}");
        }
    }
}

#[test]
fn posture_counts_keep_cjk_combining_and_ascii_pointer_geometry_at_every_width() {
    let counts = [
        ("模型e\u{301}↓\0 (Ctrl+])".into(), ChromeInk::Active),
        ("作業 2".into(), ChromeInk::Waiting),
    ];
    for ascii in [false, true] {
        for width in 0..=45 {
            let area = Rect::new(3, 2, width, 2);
            let mut buf = Buffer::empty(Rect::new(0, 0, 55, 5));
            buf.set_style(buf.area, Style::default().bg(Color::Rgb(30, 40, 50)));
            let before = buf.clone();
            let footer = TidelineFooter::new(&UI_THEME, ("許可\u{202e}", ChromeInk::PermissionAsk))
                .counts(&counts)
                .ascii_safe(ascii);
            let targets = render_tideline_footer(area, &mut buf, &footer);
            assert_eq!(buf[(2, 2)], before[(2, 2)]);
            assert_eq!(buf[(area.right(), 2)], before[(area.right(), 2)]);
            assert_eq!(buf[(3, 3)], before[(3, 3)], "one reserved row");
            if width >= 34 {
                assert_eq!(
                    targets,
                    [(0, Rect::new(13, 2, 15, 1)), (1, Rect::new(30, 2, 6, 1))]
                );
                for (x, symbol) in [
                    (13, "模"),
                    (15, "型"),
                    (17, "e\u{301}"),
                    (20, "("),
                    (30, "作"),
                    (32, "業"),
                    (35, "2"),
                ] {
                    assert_eq!(buf[(x, 2)].symbol(), symbol, "{width}: {symbol}");
                }
                assert_eq!(buf[(18, 2)].symbol(), if ascii { "v" } else { "↓" });
            } else {
                assert!(targets.is_empty(), "whole count group sheds at {width}");
            }
            if width >= 8 {
                assert_eq!(buf[(4, 2)].symbol(), if ascii { "." } else { "●" });
                assert_eq!(buf[(6, 2)].symbol(), "許");
                assert_eq!(buf[(8, 2)].symbol(), "可");
            }
        }
    }
}

#[test]
fn posture_cap_warning_keeps_its_live_attention_ink_and_priority() {
    let theme = custom_theme();
    for percent in [79, 80, 100, 255] {
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);
        let footer = TidelineFooter::new(&theme, ("full access", ChromeInk::PermissionFullAccess))
            .hint(Some(("hint", ChromeInk::Failure)))
            .context_percent(percent)
            .right(Some(("failed", ChromeInk::Failure)));
        render_tideline_footer(area, &mut buf, &footer);
        let row: String = buf.content().iter().map(|cell| cell.symbol()).collect();
        if percent < 80 {
            assert!(row.contains("hint"));
            assert!(!row.contains("surface soon"));
        } else {
            assert!(!row.contains("hint"));
            assert!(row.contains("▲ surface soon — /compact"));
            let warning = (0..area.width)
                .find(|x| buf[(*x, 0)].symbol() == "▲")
                .unwrap();
            assert_eq!(buf[(warning, 0)].fg, theme.warning);
        }
        assert_eq!(buf[(1, 0)].fg, theme.permission_full_access);
        assert_eq!(buf[(area.right() - 2, 0)].fg, theme.error_fg);
    }
}

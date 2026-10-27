//! Acceptance of the adopted mounted composer against its frozen painter.
use super::*;
use codewhale_runtime::config::Config;
use std::path::PathBuf;

fn app() -> App {
    let mut app = App::new(
        crate::tui::test_support::test_tui_options(PathBuf::from(".")),
        &Config::default(),
    );
    app.launch.visible = false;
    app.composer.vim_enabled = false;
    app.ui_locale = Locale::En;
    app.theme_id = palette::ThemeId::Underwater;
    app.ui_theme = palette::UNDERWATER_UI_THEME;
    app
}
#[test]
fn mounted_kit_exact_buffer_caret_height_and_menu_facts_match_legacy() {
    for &locale in Locale::shipped() {
        for case in 0..8 {
            let mut app = app();
            app.ui_locale = locale;
            app.input = match case {
                0 => "",
                1 => "hello",
                2 => "a CJK 鲸鱼 draft with cafe\u{0301} and a long https://example.com/path",
                3 => "1\u{fe0f}\u{20e3} 👩\u{200d}💻\n\nlast row",
                4 => "select these words across rows",
                5 => "/test",
                6 => "@builder",
                _ => "",
            }
            .into();
            app.cursor_position = app.input.chars().count();
            if case == 4 {
                app.selection_anchor = Some(2);
            }
            if case == 7 {
                app.start_history_search();
            }
            let slash = if case == 5 {
                vec![
                    SlashMenuEntry {
                        name: "/test".into(),
                        description: "Run checks for this project".into(),
                        is_skill: false,
                        alias_hint: Some("jiancha".into()),
                    },
                    SlashMenuEntry {
                        name: "/review".into(),
                        description: "Review every changed source file".into(),
                        is_skill: true,
                        alias_hint: None,
                    },
                ]
            } else {
                Vec::new()
            };
            let mentions = if case == 6 {
                vec!["builder".into(), "reviewer 鲸鱼".into()]
            } else {
                Vec::new()
            };
            for density in [
                ComposerDensity::Compact,
                ComposerDensity::Comfortable,
                ComposerDensity::Spacious,
            ] {
                app.composer_density = density;
                for enclosed in [false, true] {
                    app.composer_border = enclosed;
                    for width in [20, 40, 80, 120] {
                        for height in [3, 5, 8, 12] {
                            let area = Rect::new(7, 5, width, height);
                            let canvas = Rect::new(2, 3, width + 12, height + 8);
                            let mut actual = Buffer::empty(canvas);
                            for cell in &mut actual.content {
                                cell.set_symbol("~")
                                    .set_style(Style::default().bg(Color::Rgb(11, 23, 37)));
                            }
                            let mut expected = actual.clone();
                            let legacy =
                                legacy_composer::ComposerWidget::new(&app, 12, &slash, &mentions);
                            let widget = ComposerWidget::new(&app, 12, &slash, &mentions);
                            assert_eq!(widget.desired_height(width), legacy.desired_height(width));
                            assert_eq!(widget.cursor_pos(area), legacy.cursor_pos(area));
                            assert_eq!(
                                active_composer_submit_rect(&app, area),
                                legacy_composer::active_composer_submit_rect(&app, area)
                            );
                            assert_eq!(
                                wrap_input_lines_for_mouse(&app.input, usize::from(width)),
                                legacy_composer::wrap_input_lines_for_mouse(
                                    &app.input,
                                    usize::from(width)
                                )
                            );
                            legacy.render(area, &mut expected);
                            let old_boxes = app.viewport.last_slash_menu_hitboxes.borrow().clone();
                            widget.render(area, &mut actual);
                            assert_eq!(
                                actual, expected,
                                "locale={locale:?} case={case} density={density:?} enclosed={enclosed} area={area:?}"
                            );
                            let plan = widget.plan(area);
                            let boxes = app.viewport.last_slash_menu_hitboxes.borrow().clone();
                            assert_eq!(boxes, plan.menu_rects);
                            // Full wrapped option rows intentionally replace old one-row targets.
                            if boxes.iter().all(|(_, rect)| rect.height == 1) {
                                assert_eq!(boxes, old_boxes);
                            }
                            for (_, rect) in boxes {
                                assert_eq!(rect.intersection(plan.geometry.inner), rect);
                            }
                        }
                    }
                }
            }
        }
    }
}
#[test]
fn mounted_kit_live_custom_theme_slots_remain_exact() {
    let mut app = app();
    app.input = "select words".into();
    app.cursor_position = 6;
    app.selection_anchor = Some(0);
    app.composer_border = true;
    app.ui_theme.composer_bg = Color::Rgb(9, 21, 33);
    app.ui_theme.accent_primary = Color::Rgb(41, 52, 63);
    app.ui_theme.selection_bg = Color::Rgb(71, 82, 93);
    app.ui_theme.info = Color::Rgb(101, 112, 123);
    let widget = ComposerWidget::new(&app, 9, &[], &[]);
    let area = Rect::new(7, 5, 80, 7);
    let mut buf = Buffer::empty(area);
    widget.render(area, &mut buf);
    let plan = widget.plan(area);
    assert_eq!(buf[(area.x, area.y)].fg, app.ui_theme.accent_primary);
    assert_eq!(
        buf[(plan.geometry.text.x, plan.cursor.unwrap().y)].bg,
        app.ui_theme.selection_bg
    );
    let submit = plan.geometry.submit.unwrap();
    assert_eq!(
        buf[(submit.x, submit.y)].fg,
        palette::chrome_style(&app.ui_theme, palette::ChromeInk::Info)
            .fg
            .unwrap()
    );
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            assert_eq!(
                buf[(x, y)].bg,
                if x >= plan.geometry.text.x
                    && x < plan.cursor.unwrap().x
                    && y == plan.cursor.unwrap().y
                {
                    app.ui_theme.selection_bg
                } else {
                    app.ui_theme.composer_bg
                }
            );
        }
    }
}
#[test]
fn mounted_kit_shared_centered_padding_drives_source_pointer_projection() {
    let mut app = app();
    app.composer_border = true;
    app.input = "hello".into();
    app.cursor_position = 2;
    let widget = ComposerWidget::new(&app, 9, &[], &[]);
    let plan = widget.plan(Rect::new(7, 5, 40, 7));
    assert_eq!(plan.top_padding, 2);
    let cursor = plan.cursor.unwrap();
    let index = codewhale_ratatui::native_composer_source_at(
        &app.input,
        usize::from(plan.geometry.text.width),
        usize::from(cursor.x - plan.geometry.text.x),
        usize::from(cursor.y - plan.geometry.text.y),
        plan.scroll_offset,
        plan.top_padding,
    );
    assert_eq!(
        index, app.cursor_position,
        "viewport must retain the painter's centered padding, not bottom padding"
    );
    app.input = "a\t\u{202e}中b".into();
    app.cursor_position = 4;
    let plan = ComposerWidget::new(&app, 9, &[], &[]).plan(Rect::new(7, 5, 40, 7));
    let cursor = plan.cursor.unwrap();
    assert_eq!(
        codewhale_ratatui::native_composer_source_at(
            &app.input,
            usize::from(plan.geometry.text.width),
            usize::from(cursor.x - plan.geometry.text.x),
            usize::from(cursor.y - plan.geometry.text.y),
            plan.scroll_offset,
            plan.top_padding
        ),
        4
    );
    assert_eq!(app.input, "a\t\u{202e}中b");
}
#[test]
fn mounted_kit_resize_to_empty_withdraws_slash_targets() {
    let mut app = app();
    app.input = "/test".into();
    app.cursor_position = 5;
    let slash = vec![SlashMenuEntry {
        name: "/test".into(),
        description: "test project".into(),
        is_skill: false,
        alias_hint: None,
    }];
    let widget = ComposerWidget::new(&app, 9, &slash, &[]);
    let area = Rect::new(7, 5, 80, 9);
    let mut buf = Buffer::empty(area);
    widget.render(area, &mut buf);
    assert!(!app.viewport.last_slash_menu_hitboxes.borrow().is_empty());
    for zero in [Rect::new(7, 5, 0, 9), Rect::new(7, 5, 80, 0)] {
        widget.render(zero, &mut buf);
        assert!(app.viewport.last_slash_menu_hitboxes.borrow().is_empty());
        assert!(widget.cursor_pos(zero).is_none());
        assert!(active_composer_submit_rect(&app, zero).is_none());
    }
}

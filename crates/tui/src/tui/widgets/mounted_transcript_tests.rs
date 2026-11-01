//! Whole mounted transcript acceptance against private frozen production code.
use super::*;
use crate::tui::selection::TranscriptSelectionPoint;
use codewhale_runtime::config::Config;
use std::path::PathBuf;

fn app(locale: Locale, case: usize) -> App {
    let mut app = App::new(
        crate::tui::test_support::test_tui_options(PathBuf::from(".")),
        &Config::default(),
    );
    app.launch.visible = false;
    app.ui_locale = locale;
    app.theme_id = palette::ThemeId::Terminal;
    app.ui_theme = palette::TERMINAL_UI_THEME;
    app.low_motion = true;
    app.fancy_animations = false;
    if case == 0 {
        return app;
    }
    app.push_history_cell(HistoryCell::User {
        content: "Review this source and its exact source copy".into(),
    });
    app.push_history_cell(HistoryCell::Assistant {
        content: "A linked [guide](https://example.test/private-target) with 鲸鱼 cafe\u{0301} 1\u{20e3}① and 👩\u{200d}💻.\n\n```rust\nlet source = exact_source;\n```".into(), streaming: case == 2,
    });
    if case >= 3 {
        app.use_mouse_capture = true;
        for n in 0..8 {
            app.push_history_cell(HistoryCell::User {
                content: format!("User turn {n}"),
            });
            app.push_history_cell(HistoryCell::Assistant {
                content: format!("Receipt {n} with enough text to wrap in a narrow transcript."),
                streaming: false,
            });
        }
        app.viewport.transcript_scroll =
            codewhale_tui_kit::scrolling::TranscriptScroll::at_line(if case == 4 { 3 } else { 0 });
    }
    if case == 4 {
        app.viewport.transcript_selection.anchor = Some(TranscriptSelectionPoint {
            line_index: 3,
            column: 1,
        });
        app.viewport.transcript_selection.head = Some(TranscriptSelectionPoint {
            line_index: 5,
            column: 9,
        });
    }
    if case == 5 {
        // A user theme keeps independent source colors across native chrome.
        app.ui_theme.surface_bg = Color::Rgb(13, 29, 47);
        app.ui_theme.border = Color::Rgb(61, 79, 101);
        app.ui_theme.status_working = Color::Rgb(127, 149, 173);
        app.ui_theme.selection_bg = Color::Rgb(181, 197, 211);
        app.push_history_cell(HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "read_file".into(),
            status: ToolStatus::Success,
            input_summary: Some("src/main.rs".into()),
            output: Some("kept source and warning details".into()),
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        })));
    }
    app
}
fn guarded(area: Rect) -> Buffer {
    let mut buf = Buffer::empty(Rect::new(2, 3, area.width + 12, area.height + 8));
    for cell in &mut buf.content {
        cell.set_symbol("~")
            .set_style(Style::default().bg(Color::Rgb(11, 23, 37)));
    }
    buf
}
fn geometry(app: &App) -> (Option<Rect>, usize, usize, usize, Option<Rect>) {
    (
        app.viewport.last_transcript_area,
        app.viewport.last_transcript_top,
        app.viewport.last_transcript_visible,
        app.viewport.last_transcript_total,
        app.viewport.jump_to_latest_button_area,
    )
}
#[test]
fn mounted_transcript_complete_buffer_and_viewport_match_frozen_renderer() {
    for &locale in Locale::shipped() {
        for case in 0..6 {
            let (mut current, mut old) = (app(locale, case), app(locale, case));
            for width in [20, 40, 80, 120] {
                for height in [3, 5, 12, 20] {
                    let area = Rect::new(7, 5, width, height);
                    let mut actual = guarded(area);
                    let mut expected = actual.clone();
                    legacy_transcript::snapshot(&mut old, area, 0).render(area, &mut expected);
                    ChatWidget::new_with_ocean_elapsed(&mut current, area, 0)
                        .render(area, &mut actual);
                    assert_eq!(
                        actual, expected,
                        "locale={locale:?} case={case} area={area:?}"
                    );
                    assert_eq!(geometry(&current), geometry(&old));
                    assert_eq!(current.history.len(), old.history.len());
                    assert_eq!(
                        current.viewport.transcript_cache.line_meta(),
                        old.viewport.transcript_cache.line_meta()
                    );
                }
            }
        }
    }
}
#[test]
fn mounted_transcript_stream_resize_and_scroll_lock_preserve_cache_source() {
    let (mut current, mut old) = (app(Locale::En, 2), app(Locale::En, 2));
    for (chunk, width, height, top) in [
        ("First source", 80, 12, None),
        ("First source plus 鲸鱼\n```rust\nlet x", 20, 5, None),
        (
            "First source plus 鲸鱼\n```rust\nlet x = 1;\n```\n[guide](https://example.test/source)",
            40,
            3,
            Some(0),
        ),
        ("Final source and receipt", 80, 20, Some(0)),
    ] {
        for app in [&mut current, &mut old] {
            app.history[1] = HistoryCell::Assistant {
                content: chunk.into(),
                streaming: chunk != "Final source and receipt",
            };
            app.bump_history_cell(1);
            if let Some(top) = top {
                app.viewport.transcript_scroll =
                    codewhale_tui_kit::scrolling::TranscriptScroll::at_line(top);
                app.user_scrolled_during_stream = true;
            }
        }
        let area = Rect::new(7, 5, width, height);
        let mut actual = guarded(area);
        let mut expected = actual.clone();
        legacy_transcript::snapshot(&mut old, area, 0).render(area, &mut expected);
        ChatWidget::new_with_ocean_elapsed(&mut current, area, 0).render(area, &mut actual);
        assert_eq!(actual, expected);
        assert_eq!(geometry(&current), geometry(&old));
        assert_eq!(
            current.user_scrolled_during_stream,
            old.user_scrolled_during_stream
        );
        assert_eq!(
            current.viewport.transcript_cache.lines(),
            old.viewport.transcript_cache.lines()
        );
        match &current.history[1] {
            HistoryCell::Assistant { content, .. } => assert_eq!(content, chunk),
            _ => panic!("source authority changed"),
        }
    }
}
#[test]
fn mounted_transcript_selection_preserves_native_cjk_keycap_and_style_grammar() {
    for text in [
        "中 cafe\u{0301} words",
        "1\u{20e3}① 1\u{fe0f}\u{20e3} words",
        "👩\u{200d}💻 joined emoji",
    ] {
        let line = Line::from(vec![
            Span::styled(
                text,
                Style::default()
                    .fg(Color::Rgb(31, 43, 59))
                    .add_modifier(Modifier::ITALIC),
            ),
            Span::styled(" tail", Style::default().bg(Color::Rgb(71, 83, 97))),
        ]);
        for start in 0..8 {
            for end in start..12 {
                let selection = Style::default()
                    .bg(Color::Rgb(101, 113, 127))
                    .add_modifier(Modifier::REVERSED);
                assert_eq!(
                    codewhale_ratatui::transcript_selected_spans_measured(
                        &line,
                        start,
                        end,
                        selection,
                        grapheme_display_width
                    ),
                    legacy_transcript::selected_spans(&line, start, end, selection)
                );
            }
        }
    }
}
#[test]
fn mounted_transcript_links_never_hyperlink_jump_chrome_or_paint_targets() {
    let mut app = app(Locale::En, 3);
    let area = Rect::new(7, 5, 40, 8);
    let mut widget = ChatWidget::new_with_ocean_elapsed(&mut app, area, 0);
    widget.lines = vec![Line::from("Read this visible guide"); 8];
    widget.transcript_area = area;
    widget.scrollbar = Some(codewhale_ratatui::TranscriptScrollFacts {
        top: 0,
        visible: 8,
        total: 20,
    });
    widget.jump_to_latest_button = codewhale_ratatui::transcript_jump_rect(area, true);
    widget.line_links = vec![
        vec![codewhale_tui_kit::osc8::LineLink {
            col_start: 0,
            col_end: 100,
            target: "https://example.test/opaque-target".into()
        }];
        8
    ];
    let mut buf = guarded(area);
    widget.render(area, &mut buf);
    let plan = widget.viewport().plan(area);
    let button = plan.jump.unwrap();
    let regions = codewhale_tui_kit::osc8::FRAME_LINKS.with(|regions| regions.borrow().clone());
    assert!(!regions.is_empty());
    for region in regions {
        let rect = Rect::new(
            region.col_start,
            region.row,
            region.col_end - region.col_start + 1,
            1,
        );
        assert_eq!(rect.intersection(plan.link_area), rect);
        assert!(rect.intersection(button).is_empty());
    }
    let shown: String = buf.content.iter().map(|cell| cell.symbol()).collect();
    assert!(!shown.contains("opaque-target"));
    assert_eq!(buf[(button.x + 1, button.y + 1)].symbol(), "↓");
}

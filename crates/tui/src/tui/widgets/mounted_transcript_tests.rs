//! Whole mounted transcript acceptance.
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
#[test]
fn mounted_transcript_stream_resize_and_scroll_lock_preserve_cache_source() {
    let mut current = app(Locale::En, 2);
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
        current.history[1] = HistoryCell::Assistant {
            content: chunk.into(),
            streaming: chunk != "Final source and receipt",
        };
        current.bump_history_cell(1);
        if let Some(top) = top {
            current.viewport.transcript_scroll =
                codewhale_tui_kit::scrolling::TranscriptScroll::at_line(top);
            current.user_scrolled_during_stream = true;
        }
        let area = Rect::new(7, 5, width, height);
        let mut actual = guarded(area);
        ChatWidget::new_with_ocean_elapsed(&mut current, area, 0).render(area, &mut actual);
        assert!(current.viewport.last_transcript_area.is_some());
        match &current.history[1] {
            HistoryCell::Assistant { content, .. } => assert_eq!(content, chunk),
            _ => panic!("source authority changed"),
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

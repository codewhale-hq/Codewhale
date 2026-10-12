use super::tests::create_test_app;
use super::*;
use crate::tui::clipboard::ClipboardHandler;
use crate::tui::history::HistoryCell;
use crate::tui::selection::TranscriptSelectionPoint;

fn composer() -> (App, Rect) {
    let mut app = create_test_app();
    app.launch.visible = false;
    app.clipboard = ClipboardHandler::for_test(false, false);
    app.clipboard.enable_primary_for_test();
    app.clipboard.write_text("REGULAR_CLIPBOARD").unwrap();
    app.viewport.last_composer_area = Some(Rect::new(0, 20, 80, 3));
    let input = Rect::new(1, 21, 78, 1);
    app.viewport.last_composer_content = Some(input);
    let text = crate::tui::widgets::composer_content_geometry(input, false).text_area;
    (app, text)
}

fn event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn composer_selection_publishes_primary_quietly_even_when_released_outside() {
    let (mut app, text) = composer();
    app.input = "alpha beta".into();
    handle_mouse_event(
        &mut app,
        event(MouseEventKind::Down(MouseButton::Left), text.x, text.y),
    );
    handle_mouse_event(
        &mut app,
        event(MouseEventKind::Drag(MouseButton::Left), text.x + 5, text.y),
    );
    let status = app.status_message.clone();
    handle_mouse_event(
        &mut app,
        event(MouseEventKind::Up(MouseButton::Left), 90, 25),
    );
    assert_eq!(app.clipboard.read_primary_text().as_deref(), Some("alpha"));
    assert_eq!(app.selected_text(), "alpha");
    assert_eq!(app.clipboard.last_written_text(), Some("REGULAR_CLIPBOARD"));
    assert_eq!(app.status_message, status);
    copy_active_selection(&mut app);
    assert_eq!(app.clipboard.last_written_text(), Some("alpha"));
    assert_eq!(app.clipboard.read_primary_text().as_deref(), Some("alpha"));
}

#[test]
fn transcript_selection_uses_clean_text_and_preserves_regular_clipboard() {
    let (mut app, _) = composer();
    app.history = vec![HistoryCell::User {
        content: "COPY_TRANSCRIPT".into(),
    }];
    app.resync_history_revisions();
    app.viewport.transcript_cache.ensure(
        &app.history,
        &app.history_revisions,
        80,
        app.transcript_render_options(),
    );
    let last = app
        .viewport
        .transcript_cache
        .lines()
        .len()
        .saturating_sub(1);
    app.viewport.transcript_selection.anchor = Some(TranscriptSelectionPoint {
        line_index: 0,
        column: 0,
    });
    app.viewport.transcript_selection.head = Some(TranscriptSelectionPoint {
        line_index: last,
        column: 80,
    });
    app.viewport.transcript_selection.dragging = true;
    let expected = selection_to_text(&app).unwrap();
    let status = app.status_message.clone();
    handle_mouse_event(
        &mut app,
        event(MouseEventKind::Up(MouseButton::Left), 90, 10),
    );
    assert_eq!(app.clipboard.read_primary_text(), Some(expected));
    assert_eq!(app.clipboard.last_written_text(), Some("REGULAR_CLIPBOARD"));
    assert_eq!(app.status_message, status);
    assert!(!app.viewport.transcript_selection.dragging);
}

#[test]
fn middle_click_inserts_guarded_text_at_pointer_without_cutting_or_submitting() {
    let (mut app, text) = composer();
    app.input = "abc".into();
    app.selection_anchor = Some(0);
    app.cursor_position = 3;
    app.clipboard.write_primary_text("X\r\n/quit\n").unwrap();
    handle_mouse_event(
        &mut app,
        event(
            MouseEventKind::Down(MouseButton::Middle),
            text.x + 1,
            text.y,
        ),
    );
    assert_eq!(app.input, "aX\n/quit\nbc");
    assert!(app.selection_anchor.is_none());
    assert!(app.pending_composer_submit.is_none());
    assert!(app.pending_launch_action.is_none());
    assert_eq!(app.clipboard.last_written_text(), Some("REGULAR_CLIPBOARD"));
    handle_mouse_event(
        &mut app,
        event(MouseEventKind::Up(MouseButton::Middle), text.x + 1, text.y),
    );
    assert_eq!(app.input, "aX\n/quit\nbc", "release must not paste twice");
}

#[test]
fn middle_click_respects_overlays_missing_composer_and_remote_clipboard() {
    for mode in ["modal", "disabled", "ssh", "unavailable", "other-platform"] {
        let (mut app, text) = composer();
        app.input = "KEEP_DRAFT".into();
        app.cursor_position = 4;
        app.selection_anchor = Some(1);
        app.clipboard
            .write_primary_text("SHOULD_NOT_PASTE")
            .unwrap();
        match mode {
            "modal" => {
                super::open_context_menu(
                    &mut app,
                    event(MouseEventKind::Down(MouseButton::Right), text.x, text.y),
                );
            }
            "disabled" => app.viewport.last_composer_area = None,
            "ssh" => {
                app.clipboard = ClipboardHandler::for_test(true, false);
                app.clipboard.enable_primary_for_test();
            }
            "unavailable" => {
                app.clipboard = ClipboardHandler::unavailable_for_test(false);
                app.clipboard.enable_primary_for_test();
            }
            _ => app.clipboard = ClipboardHandler::for_test(false, false),
        }
        let status = app.status_message.clone();
        handle_mouse_event(
            &mut app,
            event(
                MouseEventKind::Down(MouseButton::Middle),
                text.x + 1,
                text.y,
            ),
        );
        assert_eq!(app.input, "KEEP_DRAFT", "{mode}");
        assert_eq!(app.cursor_position, 4, "{mode}");
        assert_eq!(app.selection_anchor, Some(1), "{mode}");
        assert!(app.pending_composer_submit.is_none(), "{mode}");
        assert_eq!(app.status_message, status, "{mode}");
    }
}

#[test]
fn transcript_drag_released_over_composer_keeps_caret_and_clears_drag() {
    let (mut app, text) = composer();
    app.input = "draft".into();
    app.cursor_position = 5;
    app.history = vec![HistoryCell::User {
        content: "SELECTED_LINE".into(),
    }];
    app.resync_history_revisions();
    app.viewport.transcript_cache.ensure(
        &app.history,
        &app.history_revisions,
        80,
        app.transcript_render_options(),
    );
    app.viewport.transcript_selection.anchor = Some(TranscriptSelectionPoint {
        line_index: 0,
        column: 0,
    });
    app.viewport.transcript_selection.head = Some(TranscriptSelectionPoint {
        line_index: 0,
        column: 80,
    });
    app.viewport.transcript_selection.dragging = true;
    let expected = selection_to_text(&app).unwrap();
    // Dragging across the composer must not move its caret or start a
    // composer selection.
    handle_mouse_event(
        &mut app,
        event(MouseEventKind::Drag(MouseButton::Left), text.x + 2, text.y),
    );
    assert_eq!(app.cursor_position, 5);
    assert!(app.selection_anchor.is_none());
    assert!(app.viewport.transcript_selection.dragging);
    // Releasing over the composer finishes the transcript drag.
    handle_mouse_event(
        &mut app,
        event(MouseEventKind::Up(MouseButton::Left), text.x + 2, text.y),
    );
    assert!(!app.viewport.transcript_selection.dragging);
    assert_eq!(app.cursor_position, 5);
    assert_eq!(app.input, "draft");
    assert_eq!(app.clipboard.read_primary_text(), Some(expected));
    assert_eq!(app.clipboard.last_written_text(), Some("REGULAR_CLIPBOARD"));
}

fn select_all_transcript_lines(app: &mut App) {
    let last = app
        .viewport
        .transcript_cache
        .lines()
        .len()
        .saturating_sub(1);
    app.viewport.transcript_selection.anchor = Some(TranscriptSelectionPoint {
        line_index: 0,
        column: 0,
    });
    app.viewport.transcript_selection.head = Some(TranscriptSelectionPoint {
        line_index: last,
        column: 80,
    });
}

fn ensure_transcript(app: &mut App) {
    app.resync_history_revisions();
    app.viewport.transcript_cache.ensure(
        &app.history,
        &app.history_revisions,
        80,
        app.transcript_render_options(),
    );
}

#[test]
fn transcript_copy_defaults_to_markdown_source_and_names_cell_count() {
    let (mut app, _) = composer();
    assert!(
        app.viewport.selection_copy_markdown,
        "markdown copy defaults on"
    );
    let content = "# Title\n\n```rust\nlet x = 1;\n```".to_string();
    app.history = vec![HistoryCell::Assistant {
        content: content.clone(),
        streaming: false,
    }];
    ensure_transcript(&mut app);
    select_all_transcript_lines(&mut app);
    let toasts_before = app.status_toasts.len();
    copy_active_selection(&mut app);
    assert_eq!(app.clipboard.last_written_text(), Some(content.as_str()));
    assert_eq!(app.status_toasts.len(), toasts_before + 1);
    let toast = app.status_toasts.back().expect("markdown copy toast");
    assert!(
        toast.text.contains("(1)"),
        "toast names the projected cell count, got {:?}",
        toast.text
    );
    assert!(
        app.status_message.is_none(),
        "toast path must not touch the legacy status sink"
    );
}

#[test]
fn transcript_copy_falls_back_to_rendered_text_when_markdown_is_off() {
    let (mut app, _) = composer();
    app.viewport.selection_copy_markdown = false;
    app.history = vec![HistoryCell::Assistant {
        content: "# Title\n\n```rust\nlet x = 1;\n```".to_string(),
        streaming: false,
    }];
    ensure_transcript(&mut app);
    select_all_transcript_lines(&mut app);
    let expected = selection_to_text(&app).expect("rendered fallback text");
    let toasts_before = app.status_toasts.len();
    copy_active_selection(&mut app);
    assert_eq!(app.clipboard.last_written_text(), Some(expected.as_str()));
    assert_eq!(app.status_toasts.len(), toasts_before + 1);
    assert_eq!(
        app.status_toasts.back().map(|toast| toast.text.as_str()),
        Some("Copied to the clipboard")
    );
}

#[test]
fn transcript_copy_joins_cells_with_blank_lines_and_counts_them() {
    let (mut app, _) = composer();
    app.history = vec![
        HistoryCell::User {
            content: "first question".into(),
        },
        HistoryCell::Assistant {
            content: "second answer".into(),
            streaming: false,
        },
    ];
    ensure_transcript(&mut app);
    select_all_transcript_lines(&mut app);
    // A partial drag still rounds out to whole cells.
    app.viewport.transcript_selection.anchor = Some(TranscriptSelectionPoint {
        line_index: 0,
        column: 2,
    });
    let toasts_before = app.status_toasts.len();
    copy_active_selection(&mut app);
    assert_eq!(
        app.clipboard.last_written_text(),
        Some("first question\n\nsecond answer")
    );
    assert_eq!(app.status_toasts.len(), toasts_before + 1);
    let toast = app.status_toasts.back().expect("multi-cell copy toast");
    assert!(
        toast.text.contains("(2)"),
        "toast names the projected cell count, got {:?}",
        toast.text
    );
}

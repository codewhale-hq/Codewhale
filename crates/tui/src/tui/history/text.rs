use super::HistoryCell;
use codewhale_tui_kit::ui_text::line_to_plain;

pub(crate) fn history_cell_to_text(cell: &HistoryCell, width: u16) -> String {
    // Error detail/copy is a diagnostic boundary, not a screenshot of the
    // live wrapping. Preserve the exact source message so narrow terminals do
    // not insert newlines into hostnames, env vars, commands, or URLs.
    if let HistoryCell::Error { message, .. } = cell {
        return message.clone();
    }
    cell.transcript_lines(width)
        .into_iter()
        .map(|line| line_to_plain(&line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Serialize one complete history cell for Copy Message.
///
/// User, assistant and system cells have canonical source text. Returning it directly
/// preserves authored Markdown and hard line breaks while keeping role glyphs,
/// continuation rails, and visual wrapping out of the clipboard. Transcript
/// drag selections reuse this same projection per intersected cell
/// (`selection_to_markdown`, #6156); only the rendered-text fallback still
/// serializes the live cache, where Markdown has already been transformed and
/// user-message soft wraps do not carry join metadata.
///
/// Complex cells intentionally retain the full-transcript representation.
/// Tool and thinking transcript renderers include semantic headers and complete
/// output that can differ from the capped or folded live cache.
pub(crate) fn history_cell_to_clipboard_text(cell: &HistoryCell, width: u16) -> String {
    match cell {
        HistoryCell::User { content }
        | HistoryCell::Assistant { content, .. }
        | HistoryCell::System { content } => content.clone(),
        _ => history_cell_to_text(cell, width),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_text_for_assistant_uses_source_without_visual_rails() {
        let content = "A long assistant response that will wrap at a narrow width.";
        let cell = HistoryCell::Assistant {
            content: content.to_string(),
            streaming: false,
        };

        let rendered = history_cell_to_text(&cell, 12);
        assert_ne!(rendered, content, "test setup must exercise rendering");
        assert!(
            rendered.contains('\n'),
            "test setup must exercise visual wrapping: {rendered:?}"
        );
        assert_eq!(history_cell_to_clipboard_text(&cell, 12), content);
    }

    #[test]
    fn clipboard_text_preserves_authored_rail_glyphs() {
        let content = "● literal role glyph\n▏ literal rail glyph";
        let cell = HistoryCell::Assistant {
            content: content.to_string(),
            streaming: false,
        };

        assert_eq!(history_cell_to_clipboard_text(&cell, 10), content);
    }

    #[test]
    fn clipboard_text_preserves_markdown_source_and_hard_breaks() {
        let content = r#"Heading

```rust
fn main() {
    println!("hello");
}
```

After code."#;
        let cell = HistoryCell::Assistant {
            content: content.to_string(),
            streaming: false,
        };

        assert_eq!(history_cell_to_clipboard_text(&cell, 16), content);
        let diagnostic = serde_json::json!({"prompt": "Use \"quoted\" input\nwith a second line"});
        let source = serde_json::to_string_pretty(&diagnostic).unwrap();
        let cell = HistoryCell::System {
            content: source.clone(),
        };
        let copied = history_cell_to_clipboard_text(&cell, 16);
        assert_eq!(copied, source);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&copied).unwrap(),
            diagnostic
        );
    }

    #[test]
    fn clipboard_text_for_user_uses_source_text() {
        let content = "user text that wraps visually";
        let cell = HistoryCell::User {
            content: content.to_string(),
        };

        let rendered = history_cell_to_text(&cell, 8);
        assert_ne!(rendered, content, "test setup must exercise rendering");
        assert!(
            rendered.contains('\n'),
            "test setup must exercise visual wrapping: {rendered:?}"
        );
        assert_eq!(history_cell_to_clipboard_text(&cell, 8), content);
    }

    #[test]
    fn clipboard_text_for_thinking_keeps_full_transcript_semantics() {
        let content = "First paragraph lede.\n\
            Second sentence of the first paragraph.\n\n\
            Second paragraph: deeper analysis follows.\n\
            More detail in paragraph two.\n\n\
            Third paragraph: even more reasoning.\n\
            With another line.\n\n\
            Fourth paragraph: the conclusion.\n\
            And one more line for good measure.\n\n\
            Fifth paragraph: final verification.\n\
            One last supporting detail.";
        let cell = HistoryCell::Thinking {
            content: content.to_string(),
            streaming: false,
            duration_secs: Some(3.2),
        };

        let live = cell
            .lines_with_options(
                80,
                crate::tui::history::TranscriptRenderOptions {
                    low_motion: true,
                    ..crate::tui::history::TranscriptRenderOptions::default()
                },
            )
            .into_iter()
            .map(|line| line_to_plain(&line))
            .collect::<Vec<_>>()
            .join("\n");
        let transcript = history_cell_to_text(&cell, 80);
        let copied = history_cell_to_clipboard_text(&cell, 80);

        assert!(!live.contains("Fifth paragraph"), "{live:?}");
        assert!(transcript.contains("Fifth paragraph"), "{transcript:?}");
        assert_eq!(copied, transcript);
    }

    #[test]
    fn clipboard_text_for_tool_keeps_full_transcript_semantics() {
        use crate::tui::history::{GenericToolCell, ToolCell, ToolStatus};

        let cell = HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: "exec_shell".to_string(),
            status: ToolStatus::Success,
            input_summary: Some("cargo test".to_string()),
            output: Some("complete tool output".to_string()),
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        }));

        let transcript = history_cell_to_text(&cell, 80);
        assert!(
            transcript.contains("complete tool output"),
            "{transcript:?}"
        );
        assert_eq!(history_cell_to_clipboard_text(&cell, 80), transcript);
    }
}

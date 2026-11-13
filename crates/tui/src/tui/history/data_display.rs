//! Transcript cell for `show_data`: the kit's `DataDisplay`, painted fully
//! drawn. Known limits: no draw-in motion in the transcript, and the chart is
//! painted with the dark truecolor role theme that the backend then adapts,
//! like the other kit adapters here, so ASCII-only terminals get the
//! Unicode glyphs.

use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_ratatui::color::ColorDepth;
use codewhale_ratatui::detect::Appearance;
use codewhale_ratatui::{Caps, DataDisplay, DataWords, Paint, Theme};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::{ToolStatus, render_compact_kv, render_tool_header_with_summary};

const INDENT: &str = "  ";

#[derive(Debug, Clone)]
pub struct DataDisplayCell {
    pub display: Result<DataDisplay, String>,
    pub status: ToolStatus,
}

impl DataDisplayCell {
    #[must_use]
    pub fn from_input(input: &serde_json::Value, status: ToolStatus) -> Self {
        let display = codewhale_engine::tools::show_data::parse_data_display(input)
            .map_err(|error| error.to_string());
        let status = if display.is_err() {
            ToolStatus::Failed
        } else {
            status
        };
        Self { display, status }
    }

    pub fn from_history(
        call: &codewhale_models::ContentBlock,
        input: &serde_json::Value,
        messages: &[codewhale_models::Message],
    ) -> Self {
        use codewhale_models::ContentBlock;
        let mut cell = Self::from_input(input, ToolStatus::Stopped);
        let Some(key) = call
            .tool_call_key()
            .filter(|key| !key.as_str().trim().is_empty())
        else {
            return cell;
        };
        let matching: Vec<_> = messages
            .iter()
            .flat_map(|message| &message.content)
            .filter(|block| block.tool_call_key().as_ref() == Some(&key))
            .collect();
        if matching
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
            .count()
            != 1
        {
            return cell;
        }
        let results: Vec<_> = matching
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } => Some((tool_use_id, content, is_error)),
                _ => None,
            })
            .collect();
        if let [(tool_use_id, content, is_error)] = results.as_slice() {
            let ContentBlock::ToolUse { id, .. } = call else {
                return cell;
            };
            if *tool_use_id != id {
                return cell;
            }
            if **is_error == Some(true) {
                cell.status = ToolStatus::Failed;
                cell.display = Err((*content).clone());
            } else if cell.display.is_ok() {
                cell.status = ToolStatus::Success;
            }
        }
        cell
    }

    pub fn lines_with_motion_and_locale(
        &self,
        width: u16,
        low_motion: bool,
        locale: Locale,
    ) -> Vec<Line<'static>> {
        let label = self
            .display
            .as_ref()
            .ok()
            .map(|display| codewhale_ratatui::text::display_safe(display.label()));
        let mut lines = vec![render_tool_header_with_summary(
            "show_data",
            label.as_deref().filter(|label| !label.trim().is_empty()),
            None,
            self.status,
            None,
            low_motion,
            locale,
        )];
        match &self.display {
            Ok(display) if self.status != ToolStatus::Stopped => {
                lines.extend(chart_lines(display, width, locale))
            }
            Ok(_) => {}
            Err(error) => lines.extend(render_compact_kv(
                &tr(locale, MessageId::ToolReceiptFailed),
                &codewhale_ratatui::text::display_safe(error),
                Style::default().fg(palette::STATUS_ERROR),
                width,
            )),
        }
        lines
    }
}

fn chart_lines(display: &DataDisplay, width: u16, locale: Locale) -> Vec<Line<'static>> {
    let inner = width.saturating_sub(INDENT.len() as u16).max(1);
    let theme = Theme::new(Caps {
        depth: ColorDepth::TrueColor,
        ascii: false,
        appearance: Appearance::Dark,
    });
    let words = DataWords {
        now: tr(locale, MessageId::DataChartNow),
        min: tr(locale, MessageId::DataChartMin),
        max: tr(locale, MessageId::DataChartMax),
        empty: tr(locale, MessageId::DataChartEmpty),
        points: tr(locale, MessageId::DataChartPoints),
    };
    let view = display.view().with_words(&words);
    let height = view.height(inner, &theme).max(1);
    let area = Rect::new(0, 0, inner, height);
    let mut buf = Buffer::empty(area);
    view.paint(area, &mut buf, &theme);

    (0..height)
        .map(|y| {
            let mut spans = vec![Span::raw(INDENT)];
            let mut run = String::new();
            let mut run_style = Style::default();
            let mut x = 0;
            while x < inner {
                let cell = &buf[(x, y)];
                let style = cell.style();
                if style != run_style && !run.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut run), run_style));
                }
                run_style = style;
                run.push_str(cell.symbol());
                x += u16::try_from(codewhale_ratatui::text::width(cell.symbol()).max(1))
                    .unwrap_or(1);
            }
            let trimmed = run.trim_end();
            if !trimmed.is_empty() || run_style != Style::default() {
                let keep = if run_style == Style::default() {
                    trimmed.to_string()
                } else {
                    run
                };
                spans.push(Span::styled(keep, run_style));
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn show_data_input_paints_a_chart_or_names_the_refusal() {
        let bars = DataDisplayCell::from_input(
            &json!({
                "kind": "bars",
                "label": "Tokens by model",
                "unit": "tokens",
                "items": [
                    { "label": "opus", "value": 400.0 },
                    { "label": "haiku", "value": 100.0 }
                ]
            }),
            ToolStatus::Success,
        );
        assert_eq!(bars.status, ToolStatus::Success);
        let rendered = text(&bars.lines_with_motion_and_locale(60, true, Locale::En));
        assert!(rendered.contains("Tokens by model"), "{rendered}");
        assert!(rendered.contains("opus"), "{rendered}");
        assert!(rendered.contains("haiku"), "{rendered}");
        assert!(rendered.contains('\u{2588}'), "{rendered}");

        let refused = DataDisplayCell::from_input(
            &json!({ "kind": "trend", "label": "x", "values": [1.0], "rows": 40 }),
            ToolStatus::Running,
        );
        assert_eq!(refused.status, ToolStatus::Failed);
        let rendered = text(&refused.lines_with_motion_and_locale(60, true, Locale::En));
        assert!(rendered.contains("40 rows"), "{rendered}");
    }
    #[test]
    fn show_data_wide_labels_fit_and_survive_session_restore() {
        use crate::tui::history::{HistoryCell, ToolCell, history_cells_from_message_with_results};
        use codewhale_engine::session_manager::{SessionManager, create_saved_session};
        use codewhale_models::{ContentBlock, Message, Role};

        let input = json!({
            "kind": "bars", "label": "Tokens",
            "items": [{ "label": "模型", "value": 42 }]
        });
        let message = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "chart-1".into(),
                name: "show_data".into(),
                input: input.clone(),
                caller: None,
                thought_signature: None,
            }],
        };
        let tmp = tempfile::tempdir().unwrap();
        let manager = SessionManager::new(tmp.path().join("sessions")).unwrap();
        let result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "chart-1".into(),
                content: "Shown".into(),
                is_error: None,
                content_blocks: None,
            }],
        };
        let session = create_saved_session(&[message, result], "test-model", tmp.path(), 0, None);
        manager.save_session(&session).unwrap();
        let loaded = manager.load_session(&session.metadata.id).unwrap();
        let cells = history_cells_from_message_with_results(&loaded.messages[0], &loaded.messages);
        let [HistoryCell::Tool(ToolCell::DataDisplay(restored))] = cells.as_slice() else {
            panic!("expected restored chart");
        };
        let call = &loaded.messages[0].content[0];
        let unfinished = DataDisplayCell::from_history(call, &input, &loaded.messages[..1]);
        assert_eq!(unfinished.status, ToolStatus::Stopped);
        assert!(
            !text(&unfinished.lines_with_motion_and_locale(60, true, Locale::En)).contains("模型")
        );
        let mut failed_messages = loaded.messages.clone();
        if let ContentBlock::ToolResult {
            is_error, content, ..
        } = &mut failed_messages[1].content[0]
        {
            *is_error = Some(true);
            *content = "Denied by policy".into();
        }
        let failed = DataDisplayCell::from_history(call, &input, &failed_messages);
        assert_eq!(failed.status, ToolStatus::Failed);
        assert!(
            text(&failed.lines_with_motion_and_locale(60, true, Locale::En))
                .contains("Denied by policy")
        );
        let mut ambiguous = loaded.messages.clone();
        ambiguous.push(loaded.messages[0].clone());
        assert_eq!(
            DataDisplayCell::from_history(call, &input, &ambiguous).status,
            ToolStatus::Stopped
        );
        let live = DataDisplayCell::from_input(&input, ToolStatus::Success);
        for width in [20, 40, 60, 100] {
            let lines = chart_lines(restored.display.as_ref().unwrap(), width, Locale::En);
            assert!(text(&lines).contains("模型"));
            assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
            assert_eq!(
                restored.lines_with_motion_and_locale(width, true, Locale::En),
                live.lines_with_motion_and_locale(width, true, Locale::En)
            );
        }
    }
}

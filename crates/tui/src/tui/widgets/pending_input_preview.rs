//! Pending-input preview widget for the composer area.
//!
//! Renders queued and in-turn follow-ups above the composer when a turn is
//! in flight, so typed input doesn't disappear silently. The backing state
//! still distinguishes queue vs send-now origins, but the UI renders one
//! coherent pending-input list.
//!
//! Empty state renders zero rows so the composer doesn't gain wasted height
//! when there's nothing to show.
//!
//! Wired into `ui.rs::render` between the chat area and the composer; the user
//! can see when typed input has been captured for later delivery.

use codewhale_ratatui::{
    Paint, PendingCard, PendingCardContext, PendingCardStyles, PendingCardWords,
};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::tui::widgets::Renderable;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;
use codewhale_tui_kit::menu_style;

/// Description of the keybinding the hint line at the bottom should advertise
/// for the "edit last queued message" action.
#[derive(Debug, Clone)]
pub struct EditBinding {
    pub label: &'static str,
}

impl EditBinding {
    pub const UP: EditBinding = EditBinding { label: "↑" };
}

/// Widget showing pending input while a turn is in progress.
#[derive(Debug, Clone)]
pub struct PendingInputPreview {
    pub locale: Locale,
    pub context_items: Vec<ContextPreviewItem>,
    pub pending_steers: Vec<String>,
    pub queued_messages: Vec<String>,
    pub editing_queued_message: Option<String>,
    pub edit_binding: EditBinding,
    /// "Approval needed in {agent} — /agents", one row per child agent
    /// waiting on the person whose card is not on top (approvals C1).
    pub pending_approvals: Vec<String>,
}

/// Compact pre-send context row shown above the composer. `included=false`
/// marks unconfirmed, missing, or skipped context distinctly from files/media
/// already known to be sent or inlined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextPreviewItem {
    pub kind: String,
    pub label: String,
    pub detail: Option<String>,
    pub included: bool,
    pub removable: bool,
    pub selected: bool,
}

impl PendingInputPreview {
    pub fn new() -> Self {
        Self {
            locale: Locale::En,
            context_items: Vec::new(),
            pending_steers: Vec::new(),
            queued_messages: Vec::new(),
            editing_queued_message: None,
            edit_binding: EditBinding::UP,
            pending_approvals: Vec::new(),
        }
    }

    fn kit(&self) -> PendingCard<'_> {
        let mut card = PendingCard::new(PendingCardWords {
            context_header: tr(self.locale, MessageId::PendingContextHeader),
            inputs_header: tr(self.locale, MessageId::PendingInputsHeader),
            sending_prefix: tr(self.locale, MessageId::PendingSendingIntoTurnPrefix),
            editing_prefix: tr(self.locale, MessageId::PendingEditingFollowUpPrefix),
            editing_restore: tr(self.locale, MessageId::PendingEscRestore),
            queued_prefix: tr(self.locale, MessageId::PendingQueuedFollowUpPrefix),
            queued_one_prefix: tr(self.locale, MessageId::PendingQueuedOnePrefix),
            queued_many_prefix: tr(self.locale, MessageId::PendingQueuedManyPrefix),
            queued_controls: tr(self.locale, MessageId::PendingSendNowControls)
                .replace("{key}", self.edit_binding.label)
                .into(),
            compact_controls: tr(self.locale, MessageId::PendingSendNowDropControls)
                .replace("{key}", self.edit_binding.label)
                .into(),
            // These are the existing context suffixes; their copy has not
            // acquired new localization authority in this rendering slice.
            removable: "removable".into(),
            selected_remove: "Backspace/Delete removes".into(),
        });
        card.context = self
            .context_items
            .iter()
            .map(|item| PendingCardContext {
                kind: item.kind.as_str().into(),
                label: item.label.as_str().into(),
                detail: item.detail.as_deref().map(Into::into),
                included: item.included,
                removable: item.removable,
                selected: item.selected,
            })
            .collect();
        card.sending = self
            .pending_steers
            .iter()
            .map(|value| value.as_str().into())
            .collect();
        card.queued = self
            .queued_messages
            .iter()
            .map(|value| value.as_str().into())
            .collect();
        card.editing = self.editing_queued_message.as_deref().map(Into::into);
        card.priority_rows = self
            .pending_approvals
            .iter()
            .map(|value| value.as_str().into())
            .collect();
        card.styles = Some(PendingCardStyles {
            input: Style::default()
                .fg(palette::TEXT_DIM)
                .add_modifier(Modifier::DIM),
            warning: Style::default().fg(palette::STATUS_WARNING),
            context_muted: Style::default().fg(palette::TEXT_MUTED),
            context_label: Style::default().fg(palette::TEXT_PRIMARY),
            selected: menu_style::selected_row_bg_style().fg(palette::SELECTION_TEXT),
        });
        card
    }
}

impl Default for PendingInputPreview {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for PendingInputPreview {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        // The existing backend owns terminal punctuation/color projection.
        // This pure card receives exact host styles and authors one row plan.
        self.kit()
            .paint(area, buf, &crate::tui::infoline::source_theme(false));
    }
    fn desired_height(&self, width: u16) -> u16 {
        self.kit()
            .height(width, &crate::tui::infoline::source_theme(false))
    }
}

#[cfg(test)]
#[path = "pending_input_preview/legacy_fixture.rs"]
mod legacy_fixture;

#[cfg(test)]
mod tests {
    use super::*;

    fn render_to_string(widget: &PendingInputPreview, width: u16) -> Vec<String> {
        let height = widget.desired_height(width);
        if height == 0 {
            return Vec::new();
        }
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        widget.render(Rect::new(0, 0, width, height), &mut buf);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn render_in_area(widget: &PendingInputPreview, width: u16, height: u16) -> Vec<String> {
        let mut buf = Buffer::empty(Rect::new(0, 0, width, height));
        widget.render(Rect::new(0, 0, width, height), &mut buf);
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn empty_widget_has_zero_height() {
        let preview = PendingInputPreview::new();
        assert_eq!(preview.desired_height(40), 0);
    }

    #[test]
    fn single_queued_message_renders_header_item_and_hint() {
        let mut preview = PendingInputPreview::new();
        preview.queued_messages.push("Hello, world!".to_string());
        let rows = render_to_string(&preview, 40);
        assert_eq!(rows.len(), 2, "got rows: {rows:?}");
        assert!(rows[0].contains("Queued #1: Hello, world!"));
        assert!(rows[1].contains("Enter send now"));
        assert!(rows[1].contains("↑ edit"));
        assert!(rows[1].contains("/queue drop 1"));
    }

    #[test]
    fn compact_queue_keeps_send_control_in_one_two_and_three_row_areas() {
        let mut preview = PendingInputPreview::new();
        preview
            .queued_messages
            .push("ship the compact fix".to_string());

        for (width, height) in [(40, 1), (40, 2), (60, 3)] {
            let rows = render_in_area(&preview, width, height);
            assert!(
                rows.iter().any(|row| row.contains("Enter send now")),
                "send control clipped at {width}x{height}: {rows:?}"
            );
        }
    }

    #[test]
    fn editing_queued_message_renders_explicit_state_and_restore_hint() {
        let mut preview = PendingInputPreview::new();
        preview.editing_queued_message = Some("revise before sending".to_string());

        let rows = render_to_string(&preview, 80);

        assert!(rows[0].contains("Pending inputs"));
        assert!(
            rows.iter()
                .any(|row| row.contains("Editing follow-up: revise before sending")),
            "missing editing label: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Esc restores the queued follow-up")),
            "missing restore hint: {rows:?}"
        );
        assert!(
            !rows.iter().any(|row| row.contains("edit last queued")),
            "editing mode should not also advertise opening a queued edit: {rows:?}"
        );
    }

    #[test]
    fn context_items_render_before_queue_buckets() {
        let mut preview = PendingInputPreview::new();
        preview.context_items.push(ContextPreviewItem {
            kind: "file".to_string(),
            label: "src/main.rs".to_string(),
            detail: Some("included".to_string()),
            included: true,
            removable: false,
            selected: false,
        });
        preview.context_items.push(ContextPreviewItem {
            kind: "missing".to_string(),
            label: "nope.txt".to_string(),
            detail: Some("not found".to_string()),
            included: false,
            removable: false,
            selected: false,
        });
        let rows = render_to_string(&preview, 64);
        assert!(rows[0].contains("Context for next send"));
        assert!(rows[1].contains("[file] src/main.rs"));
        assert!(rows[2].contains("[missing] nope.txt"));
    }

    #[test]
    fn selected_removable_attachment_renders_delete_hint() {
        let mut preview = PendingInputPreview::new();
        preview.context_items.push(ContextPreviewItem {
            kind: "image".to_string(),
            label: "/tmp/pasted.png".to_string(),
            detail: Some("attached media".to_string()),
            included: true,
            removable: true,
            selected: true,
        });

        let rows = render_to_string(&preview, 96);

        assert!(
            rows.iter()
                .any(|row| row.contains("Backspace/Delete removes"))
        );
        assert!(rows.iter().any(|row| row.contains("▸")));
    }

    #[test]
    fn pending_steer_renders_without_queue_edit_hint() {
        let mut preview = PendingInputPreview::new();
        preview.pending_steers.push("Please continue.".to_string());
        let rows = render_to_string(&preview, 80);
        assert!(
            rows.iter().any(|r| r.contains("Pending inputs")),
            "missing pending input header: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.contains("Esc")),
            "unexpected Esc hint: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.contains("edit last queued")),
            "unexpected edit hint in pending-steer-only view: {rows:?}"
        );
    }

    #[test]
    fn all_pending_inputs_render_as_one_list() {
        let mut preview = PendingInputPreview::new();
        preview.pending_steers.push("steer".to_string());
        preview.queued_messages.push("queued".to_string());
        let rows = render_to_string(&preview, 60);
        assert!(rows[0].contains("Pending inputs"));
        assert_eq!(
            rows.iter().filter(|r| r.contains("Pending inputs")).count(),
            1
        );
        assert!(rows.iter().any(|r| r.contains("steer")));
        assert!(rows.iter().any(|r| r.contains("queued")));
        assert!(rows.iter().any(|r| r.contains("↑")));
        assert!(rows.iter().any(|r| r.contains("Enter send now")));
    }

    #[test]
    fn pending_input_copy_does_not_teach_steer() {
        let mut preview = PendingInputPreview::new();
        preview.pending_steers.push("please continue".to_string());
        preview.queued_messages.push("next".to_string());
        let joined = render_to_string(&preview, 80)
            .join("\n")
            .to_ascii_lowercase();
        assert!(
            !joined.contains("steer"),
            "pending-input copy leaked internal vocabulary: {joined}"
        );
        assert!(joined.contains("sending into this turn"));
        assert!(joined.contains("queued follow-up"));
    }

    #[test]
    fn pending_input_rows_label_each_delivery_mode() {
        let mut preview = PendingInputPreview::new();
        preview.pending_steers.push("steer".to_string());
        preview.queued_messages.push("queued".to_string());
        preview.editing_queued_message = Some("editing".to_string());

        let rows = render_to_string(&preview, 80);

        assert!(
            rows.iter()
                .any(|row| row.contains("Sending into this turn: steer")),
            "missing pending send-now label: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Queued follow-up #1: queued")),
            "missing queued-follow-up label: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.contains("Editing follow-up: editing")),
            "missing queued-edit label: {rows:?}"
        );
    }

    #[test]
    fn queued_only_preview_truncates_instead_of_hiding_controls() {
        let mut preview = PendingInputPreview::new();
        preview
            .queued_messages
            .push("alpha beta gamma delta epsilon zeta".to_string());

        let rows = render_to_string(&preview, 34);

        assert_eq!(rows.len(), 2, "got rows: {rows:?}");
        assert!(rows[0].contains("Queued #1: alpha"));
        assert!(rows[0].contains('…'));
        assert!(rows[1].contains("Enter send now"));
    }

    #[test]
    fn multiline_queued_message_collapses_to_one_truncated_summary() {
        let mut preview = PendingInputPreview::new();
        preview
            .queued_messages
            .push("line1\nline2\nline3\nline4\nline5\nline6\nline7".to_string());
        let rows = render_to_string(&preview, 40);
        assert_eq!(rows.len(), 2, "got rows: {rows:?}");
        assert!(rows[0].contains("Queued #1: line1 line2"));
        assert!(rows[0].contains('…'));
        assert!(rows[1].contains("Enter send now"));
        assert!(rows[1].contains("↑ edit"));
    }

    #[test]
    fn long_url_does_not_explode_into_ellipsis_rows() {
        let mut preview = PendingInputPreview::new();
        preview.queued_messages.push(
            "example.test/api/v1/projects/alpha/releases/2026-02-17/build/1234567890/artifacts/x"
                .to_string(),
        );
        let rows = render_to_string(&preview, 36);
        assert_eq!(rows.len(), 2, "got rows: {rows:?}");
        assert!(rows[0].contains("Queued #1:"));
        assert!(rows[1].contains("Enter send now"));
    }

    #[test]
    fn narrow_width_renders_nothing() {
        let mut preview = PendingInputPreview::new();
        preview.queued_messages.push("hi".to_string());
        assert_eq!(preview.desired_height(2), 0);
    }
    // Append inside pending_input_preview.rs's existing tests module.
    // This frozen production counterpart measures both cell contents and styles;
    // it does not restate the new kit's implementation as its own expectation.
    fn legacy_counterpart(preview: &PendingInputPreview) -> legacy_fixture::PendingInputPreview {
        legacy_fixture::PendingInputPreview {
            locale: preview.locale,
            context_items: preview
                .context_items
                .iter()
                .map(|item| legacy_fixture::ContextPreviewItem {
                    kind: item.kind.clone(),
                    label: item.label.clone(),
                    detail: item.detail.clone(),
                    included: item.included,
                    removable: item.removable,
                    selected: item.selected,
                })
                .collect(),
            pending_steers: preview.pending_steers.clone(),
            queued_messages: preview.queued_messages.clone(),
            editing_queued_message: preview.editing_queued_message.clone(),
            edit_binding: legacy_fixture::EditBinding {
                label: preview.edit_binding.label,
            },
            pending_approvals: preview.pending_approvals.clone(),
        }
    }

    fn guarded_preview_buffer(area: Rect) -> Buffer {
        // A nonzero buffer origin catches accidental use of local coordinates.
        // A guard around the whole requested rectangle catches stray painting;
        // cells inside are initially empty with a caller-owned background
        // and modifier, including wide-character continuation cells.
        let canvas = Rect::new(2, 3, area.width + 12, area.height + 8);
        let mut buffer = Buffer::empty(canvas);
        for y in canvas.y..canvas.bottom() {
            for x in canvas.x..canvas.right() {
                buffer[(x, y)].set_style(
                    Style::default()
                        .fg(ratatui::style::Color::Rgb(13, 41, 67))
                        .bg(ratatui::style::Color::Rgb(19, 47, 73))
                        .add_modifier(Modifier::UNDERLINED),
                );
                if x < area.x || x >= area.right() || y < area.y || y >= area.bottom() {
                    buffer[(x, y)].set_symbol("~");
                }
            }
        }
        buffer
    }

    fn assert_preview_matches_legacy(preview: &PendingInputPreview, case: &str) {
        let legacy = legacy_counterpart(preview);
        for width in [0, 2, 4, 12, 40, 80] {
            assert_eq!(
                preview.desired_height(width),
                legacy.desired_height(width),
                "height case={case} locale={:?} width={width}",
                preview.locale,
            );
            for height in [0, 1, 2, 3, 12] {
                let area = Rect::new(7, 6, width, height);
                let mut actual = guarded_preview_buffer(area);
                let mut expected = actual.clone();
                preview.render(area, &mut actual);
                legacy.render(area, &mut expected);
                assert_eq!(
                    actual, expected,
                    "buffer case={case} locale={:?} area={area:?}",
                    preview.locale,
                );
            }
        }
    }

    fn parity_context(included: bool, removable: bool, selected: bool) -> ContextPreviewItem {
        ContextPreviewItem {
            kind: "file 文件".to_string(),
            label: "資料/cafe\u{0301}-①-1\u{20e3}-👩‍💻.txt".to_string(),
            detail: Some("CJK 你好 and cafe\u{0301} ① 1\u{20e3} attachment".to_string()),
            included,
            removable,
            selected,
        }
    }

    #[test]
    fn kit_pending_preview_matches_frozen_native_in_every_shipped_locale_and_geometry() {
        let long_url = "https://example.test/api/v1/projects/資料/releases/2026-10-02/build/1234567890/artifacts/abcdefghijklmnopqrstuvwxyz";
        for &locale in Locale::shipped() {
            let mut empty = PendingInputPreview::new();
            empty.locale = locale;
            let mut queued = empty.clone();
            queued
                .queued_messages
                .push(format!("你好 cafe\u{0301} {long_url}"));
            let mut queued_many = queued.clone();
            queued_many
                .queued_messages
                .push("second follow-up".to_string());
            let mut mixed = queued_many.clone();
            mixed.context_items.push(parity_context(true, true, false));
            mixed.context_items.push(parity_context(false, false, true));
            mixed.pending_steers.push(format!(
                "你好 cafe\u{0301} ① 1\u{20e3} 👩‍💻 first paragraph\nsecond paragraph {long_url}\nthird\nfourth"
            ));
            mixed.editing_queued_message =
                Some("revise 你好 cafe\u{0301}\nnext paragraph".to_string());
            mixed.edit_binding = EditBinding { label: "Alt+↑" };
            let mut context_only = empty.clone();
            context_only
                .context_items
                .push(parity_context(false, true, true));
            context_only
                .context_items
                .push(parity_context(true, false, false));
            let mut priority_only = empty.clone();
            priority_only.pending_approvals = vec![
                "Approval needed in 資料/cafe\u{0301} — /agents".to_string(),
                "Another child needs input — /agents".to_string(),
            ];
            let mut child_and_queued = queued.clone();
            child_and_queued.pending_approvals = priority_only.pending_approvals.clone();
            for (case, preview) in [
                ("empty", empty),
                ("queued-only", queued),
                ("queued-many", queued_many),
                ("mixed", mixed),
                ("context-only", context_only),
                ("priority-only", priority_only),
                ("child-and-queued", child_and_queued),
            ] {
                assert_preview_matches_legacy(&preview, case);
            }
        }
    }

    #[test]
    fn kit_pending_context_flags_remain_independent_of_each_other() {
        for &locale in Locale::shipped() {
            for included in [false, true] {
                for removable in [false, true] {
                    for selected in [false, true] {
                        let mut preview = PendingInputPreview::new();
                        preview.locale = locale;
                        preview
                            .context_items
                            .push(parity_context(included, removable, selected));
                        assert_preview_matches_legacy(
                            &preview,
                            &format!(
                                "included={included} removable={removable} selected={selected}"
                            ),
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn kit_pending_control_and_bidi_sanitization_precedes_measurement_and_paint() {
        // Sanitization is an intentional safety change, so this uses explicit
        // sanitized user text rather than comparing against the old renderer.
        for &locale in Locale::shipped() {
            let mut safe = PendingInputPreview::new();
            safe.locale = locale;
            safe.context_items.push(ContextPreviewItem {
                kind: "ABCD".to_string(),
                label: "ABCD".to_string(),
                detail: Some("ABCD".to_string()),
                included: false,
                removable: true,
                selected: true,
            });
            safe.pending_steers.push("ABCD".to_string());
            safe.queued_messages.push("ABCD".to_string());
            safe.editing_queued_message = Some("ABCD".to_string());
            safe.pending_approvals.push("ABCD".to_string());
            let mut unsafe_text = safe.clone();
            let value = "A\u{202e}B\u{2066}C\u{001b}\0\t\rD".to_string();
            unsafe_text.context_items[0].kind = value.clone();
            unsafe_text.context_items[0].label = value.clone();
            unsafe_text.context_items[0].detail = Some(value.clone());
            unsafe_text.pending_steers[0] = value.clone();
            unsafe_text.queued_messages[0] = value.clone();
            unsafe_text.editing_queued_message = Some(value.clone());
            unsafe_text.pending_approvals[0] = value;
            for width in [4, 12, 40, 80] {
                assert_eq!(
                    unsafe_text.desired_height(width),
                    safe.desired_height(width)
                );
                let area = Rect::new(7, 6, width, 12);
                let mut actual = guarded_preview_buffer(area);
                let mut expected = actual.clone();
                unsafe_text.render(area, &mut actual);
                safe.render(area, &mut expected);
                assert_eq!(
                    actual, expected,
                    "sanitization locale={locale:?} width={width}"
                );
            }
        }
    }

    #[test]
    fn kit_pending_multiline_body_keeps_three_rows_and_one_overflow_marker() {
        for &locale in Locale::shipped() {
            let mut preview = PendingInputPreview::new();
            preview.locale = locale;
            preview
                .pending_steers
                .push("one\ntwo\nthree\nfour\nfive\nsix\nseven".to_string());
            assert_eq!(preview.desired_height(160), 5, "locale={locale:?}");
            let rows = render_to_string(&preview, 160);
            assert_eq!(rows.len(), 5);
            assert!(rows[1].ends_with("one"));
            assert!(rows[2].ends_with("two"));
            assert!(rows[3].ends_with("three"));
            assert_eq!(rows[4].trim(), "…");
            assert!(!rows.iter().any(|row| row.contains("four")));
        }
    }
}

//! Checklist and todo transcript rendering helpers.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use unicode_width::UnicodeWidthStr;

use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;

use super::{
    RenderMode, TRANSCRIPT_RAIL, ToolStatus, render_card_detail_line_single,
    render_tool_header_with_family_and_summary, tool_status_label, tool_value_style, truncate_text,
    wrap_text,
};

pub(crate) fn is_checklist_tool_name(name: &str) -> bool {
    matches!(
        name,
        "work_update"
            | "checklist_write"
            | "checklist_add"
            | "checklist_update"
            | "todo_write"
            | "todo_add"
            | "todo_update"
    )
}

#[derive(Debug, Clone)]
pub(super) struct ChecklistItemSnapshot {
    pub(super) content: String,
    pub(super) status: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ChecklistSnapshot {
    pub(super) items: Vec<ChecklistItemSnapshot>,
    pub(super) completion_pct: u8,
    pub(super) completed: usize,
    pub(super) total: usize,
}

/// Pull a structured checklist snapshot out of the tool's text output.
/// The tool emits a leading human-readable line followed by JSON, so we
/// scan for the first `{` and parse from there. Returns `None` if the
/// payload is missing the expected `items` array.
pub(super) fn parse_checklist_snapshot(output: &str) -> Option<ChecklistSnapshot> {
    let json_start = output.find('{')?;
    let parsed: Value = serde_json::from_str(&output[json_start..]).ok()?;
    let items_value = parsed.get("items")?.as_array()?;

    let items: Vec<ChecklistItemSnapshot> = items_value
        .iter()
        .map(|item| ChecklistItemSnapshot {
            content: item
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            status: item
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("pending")
                .to_string(),
        })
        .collect();

    if items.is_empty() {
        return None;
    }

    let completed = items
        .iter()
        .filter(|item| item.status.eq_ignore_ascii_case("completed"))
        .count();
    let total = items.len();
    let completion_pct = parsed
        .get("completion_pct")
        .and_then(Value::as_u64)
        .map(|pct| u8::try_from(pct.min(100)).unwrap_or(100))
        .unwrap_or_else(|| {
            (completed * 100)
                .checked_div(total)
                .and_then(|pct| u8::try_from(pct).ok())
                .unwrap_or(0)
        });

    Some(ChecklistSnapshot {
        items,
        completion_pct,
        completed,
        total,
    })
}

/// One parsed "Updated todo #N to STATUS" prefix line emitted by
/// `todo_update` / `checklist_update`. Used by [`render_checklist_change_card`]
/// to show a compact state-change line instead of the full item list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChecklistChange {
    pub(super) id: u32,
    pub(super) status: String,
}

/// Parse the leading line of a checklist-update tool output. Returns
/// `None` for non-update outputs (e.g. `todo_write` snapshots, errors,
/// or an unexpected format) so the caller falls back to the full-list
/// renderer.
pub(super) fn parse_update_prefix(output: &str) -> Option<ChecklistChange> {
    // The tool output shape is `Updated todo #3 to in_progress\n{ ... }`.
    // We tolerate `checklist` or `todo` as the noun and any reasonable
    // status word (the snapshot lookup in the renderer is the source of
    // truth for the title — we just need the id+status pair).
    let first = output.lines().next()?.trim();
    let rest = first
        .strip_prefix("Updated todo #")
        .or_else(|| first.strip_prefix("Updated checklist #"))?;
    let (id_str, after) = rest.split_once(' ')?;
    let id: u32 = id_str.parse().ok()?;
    let status = after.strip_prefix("to ")?.trim().to_string();
    if status.is_empty() {
        return None;
    }
    Some(ChecklistChange { id, status })
}

/// Render a compact one-line state-change card for `todo_update` /
/// `checklist_update` calls (#403). Shows the changed item's marker,
/// title, and old -> new status, with a `M/N · pct%` progress summary
/// in the header. The full list is still available through the tool
/// detail record.
pub(super) fn render_checklist_change_card(
    name: &str,
    status: ToolStatus,
    snapshot: &ChecklistSnapshot,
    change: &ChecklistChange,
    width: u16,
    low_motion: bool,
    locale: Locale,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let header_summary = format!(
        "{}/{} \u{00B7} {}%",
        snapshot.completed, snapshot.total, snapshot.completion_pct
    );
    let family = crate::tui::widgets::tool_card::tool_family_for_name(name);
    lines.push(render_tool_header_with_family_and_summary(
        family,
        Some(&header_summary),
        tool_status_label(status),
        status,
        None,
        low_motion,
    ));

    // Look up the title from the snapshot. `id` in tool input is
    // 1-indexed; `items` is 0-indexed.
    let item = (change.id as usize)
        .checked_sub(1)
        .and_then(|idx| snapshot.items.get(idx));
    let title = item
        .map(|i| i.content.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "(missing title)".to_string());

    let (marker, marker_color) = checklist_status_marker(&change.status);
    let prefix = format!("{marker} ");
    let prefix_width =
        UnicodeWidthStr::width(TRANSCRIPT_RAIL) + UnicodeWidthStr::width(prefix.as_str());
    let id_label = format!("Todo #{}", change.id);
    let arrow = " \u{2192} ";
    let status_label = checklist_status_label(&change.status, locale);
    let title_budget = usize::from(width)
        .saturating_sub(prefix_width)
        .saturating_sub(UnicodeWidthStr::width(id_label.as_str()))
        .saturating_sub(UnicodeWidthStr::width(arrow))
        .saturating_sub(UnicodeWidthStr::width(status_label.as_str()))
        .saturating_sub(2)
        .max(8);
    let title_truncated = truncate_text(title.as_str(), title_budget);

    let spans = vec![
        Span::styled(
            "\u{258F} ".to_string(),
            Style::default().fg(palette::TEXT_DIM),
        ),
        Span::styled(prefix, Style::default().fg(marker_color)),
        Span::styled(id_label, Style::default().fg(palette::TEXT_DIM)),
        Span::styled(": ".to_string(), Style::default().fg(palette::TEXT_DIM)),
        Span::styled(title_truncated, tool_value_style()),
        Span::styled(arrow.to_string(), Style::default().fg(palette::TEXT_DIM)),
        Span::styled(status_label, Style::default().fg(marker_color)),
    ];
    lines.push(Line::from(spans));

    // Tease that the full list is still available without leaving the
    // transcript. Mirrors the same affordance used by other tool cells.
    lines.push(render_card_detail_line_single(
        None,
        &format!(
            "{} item{} \u{00B7} {}",
            snapshot.total,
            if snapshot.total == 1 { "" } else { "s" },
            crate::tui::key_shortcuts::tool_details_shortcut_action_hint("list")
        ),
        Style::default().fg(palette::TEXT_MUTED),
    ));
    lines
}

fn checklist_status_label(status: &str, locale: Locale) -> String {
    use crate::tools::todo::TodoStatus;
    let id = match TodoStatus::from_str(status) {
        Some(TodoStatus::Pending) => MessageId::TodoStatusPending,
        Some(TodoStatus::InProgress) => MessageId::TodoStatusInProgress,
        Some(TodoStatus::Completed) => MessageId::TodoStatusCompleted,
        Some(TodoStatus::Cancelled) => MessageId::TodoStatusCancelled,
        None => return status.to_string(),
    };
    tr(locale, id).into_owned()
}

fn checklist_status_marker(status: &str) -> (&'static str, Color) {
    match status.to_ascii_lowercase().as_str() {
        "completed" | "done" => ("\u{2611}", palette::STATUS_SUCCESS), // ☑
        "in_progress" | "inprogress" | "running" => ("\u{25D0}", palette::WHALE_ACTION), // ◐
        "blocked" | "failed" => ("\u{2717}", palette::STATUS_ERROR),   // ✗
        "cancelled" | "canceled" | "skipped" => ("\u{2298}", palette::TEXT_MUTED), // ⊘
        _ => ("\u{2610}", palette::TEXT_MUTED),                        // ☐ pending
    }
}

const CHECKLIST_LIVE_ITEM_LIMIT: usize = 8;

pub(super) fn render_checklist_card(
    name: &str,
    status: ToolStatus,
    snapshot: &ChecklistSnapshot,
    width: u16,
    low_motion: bool,
    mode: RenderMode,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let header_summary = format!(
        "{}/{} \u{00B7} {}%",
        snapshot.completed, snapshot.total, snapshot.completion_pct
    );
    let family = crate::tui::widgets::tool_card::tool_family_for_name(name);
    lines.push(render_tool_header_with_family_and_summary(
        family,
        Some(&header_summary),
        tool_status_label(status),
        status,
        None,
        low_motion,
    ));

    let cap = match mode {
        RenderMode::Live => CHECKLIST_LIVE_ITEM_LIMIT,
        RenderMode::Transcript => snapshot.items.len(),
    };
    let visible: Vec<&ChecklistItemSnapshot> = snapshot.items.iter().take(cap).collect();
    let omitted = snapshot.items.len().saturating_sub(visible.len());

    for item in visible {
        let (marker, color) = checklist_status_marker(&item.status);
        let prefix = format!("{marker} ");
        // Reserve room for the rail + marker prefix when wrapping content.
        let prefix_width =
            UnicodeWidthStr::width(TRANSCRIPT_RAIL) + UnicodeWidthStr::width(prefix.as_str());
        let content_width = usize::from(width).saturating_sub(prefix_width).max(1);
        for (idx, part) in wrap_text(item.content.trim(), content_width)
            .into_iter()
            .enumerate()
        {
            let mut spans = vec![Span::styled(
                "\u{258F} ".to_string(),
                Style::default().fg(palette::TEXT_DIM),
            )];
            if idx == 0 {
                spans.push(Span::styled(prefix.clone(), Style::default().fg(color)));
            } else {
                spans.push(Span::raw(
                    " ".repeat(UnicodeWidthStr::width(prefix.as_str())),
                ));
            }
            spans.push(Span::styled(part, tool_value_style()));
            lines.push(Line::from(spans));
        }
    }

    if omitted > 0 {
        lines.push(render_card_detail_line_single(
            None,
            &format!(
                "+{omitted} more \u{00B7} {}",
                crate::tui::key_shortcuts::tool_details_shortcut_action_hint("list")
            ),
            Style::default().fg(palette::TEXT_DIM),
        ));
    }

    lines
}

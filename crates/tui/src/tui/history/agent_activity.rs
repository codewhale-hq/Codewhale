//! Compact transcript rendering for agent and activity metadata cells.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use codewhale_localization::{Locale, MessageId, tr};
use codewhale_palette as palette;

use super::{GenericToolCell, render_tool_header_with_family_and_summary, truncate_text};

pub(super) fn render_agent_compact(
    cell: &GenericToolCell,
    low_motion: bool,
    locale: Locale,
) -> Vec<Line<'static>> {
    let family = crate::tui::widgets::tool_card::ToolFamily::Delegate;
    let agent_id = cell
        .output
        .as_deref()
        .and_then(extract_agent_id)
        .map(str::to_string)
        .unwrap_or_else(|| delegate_identity_fallback(cell));
    // Inspections and joins must not draw the same "delegate done" line as a
    // spawn — during a fan-out session every peek/status/wait would otherwise
    // read as yet another completed delegate (#4112, dogfood A5). The action
    // is stamped at the front of the args summary by tool_routing.
    let state_label = match (agent_inspection_action(cell), cell.status) {
        (Some(AgentCompactAction::Check), super::ToolStatus::Running) => {
            Some(tr(locale, MessageId::AgentActivityChecking))
        }
        (Some(AgentCompactAction::Check), super::ToolStatus::Success) => {
            Some(tr(locale, MessageId::AgentActivityChecked))
        }
        (Some(AgentCompactAction::Wait), super::ToolStatus::Running) => {
            Some(tr(locale, MessageId::AgentActivityWaiting))
        }
        (Some(AgentCompactAction::Wait), super::ToolStatus::Success) => {
            Some(tr(locale, MessageId::AgentActivityWaited))
        }
        _ => None,
    };
    vec![render_tool_header_with_family_and_summary(
        family,
        Some(agent_id.as_str()),
        state_label.as_deref(),
        cell.status,
        None,
        low_motion,
        locale,
    )]
}

enum AgentCompactAction {
    /// Read-only inspection: peek / status / progress / list / inspect.
    Check,
    /// Blocking join: wait / join / await / block.
    Wait,
}

/// Whether this `agent` cell is a read-only inspection or join rather than a
/// spawn — those stay compact even in Transcript mode.
pub(super) fn is_agent_inspection(cell: &GenericToolCell) -> bool {
    agent_inspection_action(cell).is_some()
}

fn agent_inspection_action(cell: &GenericToolCell) -> Option<AgentCompactAction> {
    let summary = cell.input_summary.as_deref()?;
    let action = summary.strip_prefix("action:")?.trim_start();
    let action = action.split_whitespace().next().unwrap_or("");
    match action.trim_end_matches(',') {
        "peek" | "progress" | "status" | "list" | "inspect" => Some(AgentCompactAction::Check),
        "wait" | "join" | "await" | "block" => Some(AgentCompactAction::Wait),
        _ => None,
    }
}

pub(super) fn render_activity_group(
    cell: &GenericToolCell,
    width: u16,
    locale: Locale,
) -> Vec<Line<'static>> {
    let fallback = super::tool_run::sentence_case_activity(
        tr(locale, MessageId::ActivityUpdatedMetadata).into_owned(),
    );
    let summary = cell.input_summary.as_deref().unwrap_or(&fallback);
    let suffix = cell
        .output_summary
        .as_deref()
        .map_or(" ›".to_string(), |count| format!(" {count} ›"));
    let budget = usize::from(width)
        .saturating_sub(unicode_width::UnicodeWidthStr::width(suffix.as_str()))
        .max(1);
    vec![Line::from(Span::styled(
        format!("{}{suffix}", truncate_text(summary, budget)),
        Style::default().fg(palette::TEXT_MUTED),
    ))]
}

fn delegate_identity_fallback(cell: &GenericToolCell) -> String {
    if let Some(summary) = cell.input_summary.as_deref() {
        let summary = summary.trim();
        if let Some(rest) = summary.strip_prefix("role:") {
            let role = rest.split_whitespace().next().unwrap_or(rest).trim();
            if !role.is_empty() {
                return role.to_string();
            }
        }
        if let Some(rest) = summary.strip_prefix("prompt:") {
            let title = rest.trim();
            if !title.is_empty() {
                let slug: String = title
                    .chars()
                    .take(24)
                    .map(|ch| {
                        if ch.is_ascii_alphanumeric() {
                            ch.to_ascii_lowercase()
                        } else {
                            '-'
                        }
                    })
                    .collect();
                let slug = slug.trim_matches('-');
                if !slug.is_empty() {
                    return slug.to_string();
                }
            }
        }
    }
    // #4148: never surface the raw internal fallback token ("unknown child")
    // in the default transcript. When we can't resolve a concrete role, slug,
    // or agent id, a friendly, non-leaky label reads best next to the
    // "delegate" verb ("delegate running · subagent").
    "subagent".to_string()
}

pub(super) fn extract_agent_id(output: &str) -> Option<&str> {
    let key = "\"agent_id\"";
    let key_idx = output.find(key)?;
    let rest = &output[key_idx + key.len()..];
    let colon = rest.find(':')?;
    let after_colon = rest[colon + 1..].trim_start();
    let after_colon = after_colon.strip_prefix('"')?;
    let end = after_colon.find('"')?;
    let id = &after_colon[..end];
    (!id.is_empty()).then_some(id)
}

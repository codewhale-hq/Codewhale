//! Per-row composition: the columns a sub-agent row resolves to at a given
//! width, and the style every row is painted with.

use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

use crate::tui::app::App;
use crate::tui::work_surface::model::{AgentRowFacts, WorkRow, WorkTone};
use codewhale_tui_kit::ui_text::truncate_line_to_width;

/// Gap between the agent-type column and the objective.
pub(super) const AGENT_ROLE_GUTTER: usize = 2;
/// Minimum gap between the objective and the right-aligned receipt.
const AGENT_RECEIPT_GUTTER: usize = 2;
/// Columns the objective must keep before an optional column may stay. Below
/// this the objective is a shrug — "Streaming d…" answers nothing — so the
/// optional column loses instead.
const AGENT_OBJECTIVE_MIN: usize = 24;

/// How much of a sub-agent row survives at the current width.
///
/// Degradation order, widest to narrowest: the token figure goes first, then
/// the remaining receipt, then the agent identity column. The objective is the
/// last
/// thing to go — a fleet row that cannot say what the agent is doing has
/// stopped being worth a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentRowTier {
    /// Type, objective, elapsed, tokens.
    Full,
    /// Type, objective, elapsed.
    NoTokens,
    /// Type, objective.
    NoReceipt,
    /// Objective only.
    ObjectiveOnly,
}

const AGENT_ROW_TIERS: [AgentRowTier; 4] = [
    AgentRowTier::Full,
    AgentRowTier::NoTokens,
    AgentRowTier::NoReceipt,
    AgentRowTier::ObjectiveOnly,
];

/// A sub-agent row resolved to painted columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct AgentRowText {
    /// Agent-type column, padded to the shared width. Empty once dropped.
    pub(super) role: String,
    /// Status word column (`running`, `completed`, …), padded to the shared
    /// width. Dropped only with the identity column: a fleet row that cannot
    /// say its state in words has lost the fact the owner asked for back
    /// (2026-08-04 regression report).
    pub(super) status: String,
    pub(super) objective: String,
    /// `deepseek-v4-pro · 12m 33s · ↓ 111.9k tokens`. Empty once dropped.
    pub(super) receipt: String,
    /// Spaces separating the objective from the receipt.
    pub(super) gap: usize,
}

/// The right-aligned receipt at a given tier. A figure the runtime never
/// reported is absent, never zero: an agent with no usage envelope shows no
/// token count at all.
pub(super) fn agent_receipt(facts: &AgentRowFacts, tier: AgentRowTier) -> String {
    let model = facts
        .model
        .as_deref()
        .filter(|model| !model.is_empty())
        .filter(|_| matches!(tier, AgentRowTier::Full | AgentRowTier::NoTokens))
        .map(str::to_string);
    let elapsed = facts
        .elapsed_secs
        .filter(|_| matches!(tier, AgentRowTier::Full | AgentRowTier::NoTokens))
        .map(codewhale_command_contract::elapsed::format_elapsed_secs);
    let tokens = facts
        .tokens
        .filter(|_| tier == AgentRowTier::Full)
        .map(|tokens| {
            format!(
                "↓ {} tokens",
                crate::tui::footer_ui::format_token_count_compact(tokens)
            )
        });
    // Only paint a remaining chip when a real ledger reported unsettled
    // work. `None` (no list) and `Some(0)` (list fully settled) stay quiet —
    // a fabricated `0 left` is strip noise.
    let todos_left = facts
        .todos_remaining
        .filter(|n| *n > 0)
        .filter(|_| matches!(tier, AgentRowTier::Full | AgentRowTier::NoTokens))
        .map(|n| format!("{n} left"));
    [model, elapsed, tokens, todos_left]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Ceiling on the shared identity column, as a fraction of the row. The
/// column is shared, so without a cap a single long nickname would widen it
/// for every row and starve every objective on the surface. An identity wider
/// than this is dropped for *that* row only.
const AGENT_IDENTITY_CAP_NUMERATOR: usize = 2;
const AGENT_IDENTITY_CAP_DENOMINATOR: usize = 5;

/// Widest identity the shared column will carry at this row width.
pub(super) fn agent_identity_cap(width: usize) -> usize {
    width
        .saturating_mul(AGENT_IDENTITY_CAP_NUMERATOR)
        .saturating_div(AGENT_IDENTITY_CAP_DENOMINATOR)
}

/// Which spelling of a sub-agent's identity fits the column: its nickname
/// first, then its fleet role, then nothing.
///
/// Names are never truncated, only dropped. `Fluke the Deep…` and
/// `general-purpo…` both misidentify an agent, and roles that share a prefix
/// would become indistinguishable. A shell row's label is a command rather
/// than a name, so it is truncated instead of dropped: falling back to the
/// bare `shell` role would label every command row the same (#6931).
pub(super) fn agent_identity(row: &WorkRow, cap: usize) -> std::borrow::Cow<'_, str> {
    use std::borrow::Cow;
    let Some(facts) = row.agent.as_ref() else {
        return Cow::Borrowed("");
    };
    if row.id.0.starts_with("shell:")
        && !row.label.is_empty()
        && UnicodeWidthStr::width(row.label.as_str()) > cap
    {
        return Cow::Owned(truncate_line_to_width(&row.label, cap));
    }
    for candidate in [row.label.as_str(), facts.role_label.as_str()] {
        if !candidate.is_empty() && UnicodeWidthStr::width(candidate) <= cap {
            return Cow::Borrowed(candidate);
        }
    }
    Cow::Borrowed("")
}

/// Shared width of the identity column across the rows painted this frame, so
/// the objectives line up the way a fleet listing should read. Rows whose
/// identity exceeded the cap contribute nothing, so one outlier cannot widen
/// the column for everyone else.
pub(super) fn agent_identity_column(rows: &[&WorkRow], cap: usize) -> usize {
    rows.iter()
        .filter(|row| row.agent.is_some())
        .map(|row| UnicodeWidthStr::width(agent_identity(row, cap).as_ref()))
        .max()
        .unwrap_or(0)
}

/// Shared width of the status-word column across the rows painted this frame.
/// Statuses come from a fixed vocabulary, so no cap is needed.
pub(super) fn agent_status_column(rows: &[&WorkRow]) -> usize {
    rows.iter()
        .filter_map(|row| row.agent.as_ref())
        .map(|facts| UnicodeWidthStr::width(facts.status.as_str()))
        .max()
        .unwrap_or(0)
}

/// Fit one sub-agent row into `width`, dropping optional columns in
/// [`AGENT_ROW_TIERS`] order until the objective has room to say something.
/// Every column truncates; nothing ever wraps.
pub(super) fn layout_agent_row(
    width: usize,
    prefix_width: usize,
    identity: &str,
    identity_column: usize,
    status_column: usize,
    facts: &AgentRowFacts,
) -> AgentRowText {
    for tier in AGENT_ROW_TIERS {
        let receipt = agent_receipt(facts, tier);
        let role = if tier == AgentRowTier::ObjectiveOnly || identity_column == 0 {
            String::new()
        } else {
            // A row whose own identity was dropped still reserves the column,
            // so every objective on the surface stays on the same axis.
            let pad = identity_column.saturating_sub(UnicodeWidthStr::width(identity));
            format!("{identity}{}", " ".repeat(pad))
        };
        // The status word degrades with the identity: it survives the loss of
        // tokens and elapsed, and yields only when the row is down to the
        // objective alone.
        let status = if tier == AgentRowTier::ObjectiveOnly || status_column == 0 {
            String::new()
        } else {
            let pad = status_column.saturating_sub(UnicodeWidthStr::width(facts.status.as_str()));
            format!("{}{}", facts.status, " ".repeat(pad))
        };
        let role_cost = if role.is_empty() {
            0
        } else {
            UnicodeWidthStr::width(role.as_str()).saturating_add(AGENT_ROLE_GUTTER)
        };
        let status_cost = if status.is_empty() {
            0
        } else {
            UnicodeWidthStr::width(status.as_str()).saturating_add(AGENT_ROLE_GUTTER)
        };
        let receipt_cost = if receipt.is_empty() {
            0
        } else {
            UnicodeWidthStr::width(receipt.as_str()).saturating_add(AGENT_RECEIPT_GUTTER)
        };
        let budget = width
            .saturating_sub(prefix_width)
            .saturating_sub(role_cost)
            .saturating_sub(status_cost)
            .saturating_sub(receipt_cost);
        if budget < AGENT_OBJECTIVE_MIN && tier != AgentRowTier::ObjectiveOnly {
            continue;
        }
        let objective = truncate_line_to_width(&facts.objective, budget);
        let gap = width
            .saturating_sub(prefix_width)
            .saturating_sub(role_cost)
            .saturating_sub(status_cost)
            .saturating_sub(UnicodeWidthStr::width(objective.as_str()))
            .saturating_sub(UnicodeWidthStr::width(receipt.as_str()));
        return AgentRowText {
            role,
            status,
            objective,
            receipt,
            gap,
        };
    }
    AgentRowText::default()
}

/// Normal-text and muted styles for one sub-agent row.
///
/// Three colour roles and no more: the objective is normal text, every
/// secondary figure (type, `(+N)`, elapsed, tokens) is muted, and
/// `accent_primary` means "this is the row you have selected" and nothing
/// else. Status is carried by the glyph, never by colour.
pub(super) fn agent_row_styles(
    app: &App,
    selected: bool,
    hovered: bool,
    opened: bool,
) -> (Style, Style) {
    let bg = if selected {
        app.ui_theme.selection_bg
    } else if hovered {
        app.ui_theme.elevated_bg
    } else {
        app.ui_theme.panel_bg
    };
    let mut normal = Style::default().fg(app.ui_theme.text_body).bg(bg);
    let mut muted = Style::default().fg(app.ui_theme.text_muted).bg(bg);
    if opened {
        normal = normal.fg(app.ui_theme.accent_primary);
        muted = muted.fg(app.ui_theme.accent_primary);
    }
    if selected {
        normal = normal.add_modifier(Modifier::BOLD);
        muted = muted.add_modifier(Modifier::BOLD);
    }
    if opened {
        normal = normal.add_modifier(Modifier::UNDERLINED);
        muted = muted.add_modifier(Modifier::UNDERLINED);
    }
    (normal, muted)
}

/// The one place a `WorkTone` becomes ink. Failure red is spent here and
/// nowhere else in the work surface; `tone_color_reserves_failure_red` pins
/// that against every selectable theme.
///
/// Headings (group headers like `▾ Subagents 2`) are muted structure, not
/// interaction — accent_primary is reserved for selection/focus. GrokBuild
/// uses the same gray-on-header treatment.
pub(super) fn tone_color(tone: WorkTone, theme: &codewhale_palette::UiTheme) -> Color {
    match tone {
        WorkTone::Heading | WorkTone::Muted => theme.text_muted,
        WorkTone::Live => theme.status_working,
        WorkTone::Attention => theme.warning,
        WorkTone::Failure => theme.error_fg,
        WorkTone::Success => theme.success,
    }
}

pub(super) fn row_style(
    app: &App,
    row: &WorkRow,
    selected: bool,
    hovered: bool,
    opened: bool,
) -> Style {
    let fg = tone_color(row.tone, &app.ui_theme);
    let mut style = Style::default().fg(fg).bg(app.ui_theme.panel_bg);
    if row.tone == WorkTone::Heading {
        style = style.add_modifier(Modifier::BOLD);
    }
    if !row.selectable {
        return style;
    }
    if opened {
        style = style
            .fg(app.ui_theme.accent_primary)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    }
    if selected {
        style = style
            .fg(codewhale_palette::enforce_contrast(
                fg,
                app.ui_theme.selection_bg,
                4.5,
            ))
            .bg(app.ui_theme.selection_bg)
            .add_modifier(Modifier::BOLD);
    } else if hovered {
        style = style.bg(app.ui_theme.elevated_bg);
    }
    style
}

#[cfg(test)]
mod tests {
    use super::{WorkTone, tone_color};

    /// Failure red is reserved for actual failure, on every preset — not just
    /// the whale default (`docs/design/STATUS_BAR_COLOR_GRAMMAR.md`). Waiting,
    /// blocked and stale work used to share `error_fg` with a crashed agent,
    /// which spent the loudest ink in the palette on the routine case.
    #[test]
    fn tone_color_reserves_failure_red() {
        for theme_id in codewhale_palette::SELECTABLE_THEMES {
            let theme = theme_id.ui_theme();
            for tone in [
                WorkTone::Heading,
                WorkTone::Live,
                WorkTone::Attention,
                WorkTone::Success,
                WorkTone::Muted,
            ] {
                assert_ne!(
                    tone_color(tone, &theme),
                    theme.error_fg,
                    "theme '{}' spends Failure red on {tone:?}",
                    theme_id.name()
                );
            }
            assert_eq!(
                tone_color(WorkTone::Failure, &theme),
                theme.error_fg,
                "theme '{}' must still paint a real failure red",
                theme_id.name()
            );
        }
    }
}

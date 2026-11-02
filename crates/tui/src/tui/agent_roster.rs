//! Receipts-only projection of every agent that ran this session (#5479).
//!
//! Terminal and transcript rendering for the agent roster. Data structures
//! and projections are in [`codewhale_engine::agent_roster`].

use std::collections::BTreeMap;

use codewhale_engine::agent_roster::{
    AgentRosterRow, all_rows_have_usage, format_duration, format_tokens, roster_totals,
};

/// Absent receipts render as `—`. See the truth rule in the module docs.
fn or_dash(value: Option<String>) -> String {
    value.unwrap_or_else(|| "—".to_string())
}

/// Render the roster as transcript text.
///
/// The parent session is row zero (`● main`); workflow parents collapse their
/// children into `n/m done` and the children are indented beneath them.
#[must_use]
pub fn render_agent_roster(rows: &[AgentRosterRow], parent_label: &str) -> String {
    if rows.is_empty() {
        return format!(
            "● {parent_label}\n\nNo agents have run in this session yet. \
             Spawn one with the `agent` tool, or `/fleet` to set up roles."
        );
    }

    let mut children_by_parent: BTreeMap<&str, Vec<&AgentRosterRow>> = BTreeMap::new();
    for row in rows {
        if let Some(parent) = row.parent_run_id.as_deref() {
            children_by_parent.entry(parent).or_default().push(row);
        }
    }

    let mut out = format!("● {parent_label}\n");
    for row in rows {
        // A child is printed under its parent, not again at the top level.
        if row
            .parent_run_id
            .as_deref()
            .is_some_and(|parent| rows.iter().any(|candidate| candidate.run_id == parent))
        {
            continue;
        }
        out.push_str(&render_row(row, rows, 1));
        append_descendants(&mut out, row.run_id.as_str(), &children_by_parent, rows, 2);
    }
    out.push_str(&render_totals(rows));
    out
}

/// Footer totals, labelled honestly.
///
/// When only some rows carry a usage receipt the line says so, because a bare
/// total silently implies it covers every agent listed above it.
fn render_totals(rows: &[AgentRosterRow]) -> String {
    let (input, output) = roster_totals(rows);
    if input.is_none() && output.is_none() {
        return format!(
            "\n{} agent{} · no usage receipts recorded\n",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        );
    }
    let coverage = if all_rows_have_usage(rows) {
        String::new()
    } else {
        let reported = rows
            .iter()
            .filter(|row| row.input_tokens.is_some() || row.output_tokens.is_some())
            .count();
        format!(" (receipts from {reported} of {} agents)", rows.len())
    };
    format!(
        "\n{} agent{} · {} · {}{coverage}\n",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        or_dash(input.map(|t| format!("↓ {}", format_tokens(t)))),
        or_dash(output.map(|t| format!("↑ {}", format_tokens(t)))),
    )
}

fn append_descendants(
    out: &mut String,
    parent_run_id: &str,
    children_by_parent: &BTreeMap<&str, Vec<&AgentRosterRow>>,
    all: &[AgentRosterRow],
    depth: usize,
) {
    for child in children_by_parent.get(parent_run_id).into_iter().flatten() {
        out.push_str(&render_row(child, all, depth));
        append_descendants(
            out,
            child.run_id.as_str(),
            children_by_parent,
            all,
            depth + 1,
        );
    }
}

fn render_row(row: &AgentRosterRow, all: &[AgentRosterRow], depth: usize) -> String {
    let indent = "  ".repeat(depth);
    let elapsed = or_dash(row.millis.map(format_duration));
    let input = or_dash(row.input_tokens.map(|t| format!("↓ {}", format_tokens(t))));
    let output = or_dash(row.output_tokens.map(|t| format!("↑ {}", format_tokens(t))));
    let activity = match row.workflow_progress(all) {
        Some((done, total)) => format!("{done}/{total} agents done"),
        None => or_dash(row.activity.clone()),
    };
    format!(
        "{indent}{glyph} {name}   {activity}   {elapsed} · {input} · {output}\n",
        glyph = row.state.glyph(),
        name = row.display_name,
    )
}

#[cfg(test)]
mod tests;

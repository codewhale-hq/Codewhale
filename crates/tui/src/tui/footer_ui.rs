use std::cell::RefCell;
use std::collections::HashMap;

use crate::tui::app::App;
use crate::tui::history::summarize_tool_output;
use crate::tui::output_rows_cache::hash_str;
use crate::tui::subagent_routing::{active_fanout_counts, running_agent_count};
use codewhale_tui_kit::ui_text::truncate_line_to_width;

/// Seconds the current turn has gone without observable stream activity.
pub(crate) fn provider_wait_idle_secs(app: &App) -> u64 {
    app.turn_last_activity_at
        .or(app.turn_started_at)
        .map(|at| at.elapsed().as_secs())
        .unwrap_or(0)
}

/// Threshold after which a provider wait with a planned fanout is logged as
/// a structured incident (once per turn).
const PROVIDER_WAIT_INCIDENT_SECS: u64 = 120;

/// Log a compact structured incident when the parent turn has spent a long
/// time in provider wait while a sub-agent fanout plan is present (#3095).
pub(crate) fn maybe_log_provider_wait_incident(app: &mut App) {
    if app.provider_wait_incident_logged || !app.is_loading {
        return;
    }
    let elapsed = match app.turn_started_at {
        Some(at) => at.elapsed().as_secs(),
        None => return,
    };
    if elapsed < PROVIDER_WAIT_INCIDENT_SECS {
        return;
    }
    let fanout = active_fanout_counts(app);
    let pending_dispatch = app.pending_subagent_dispatch.is_some();
    if fanout.is_none() && !pending_dispatch {
        return;
    }
    let (fanout_running, fanout_total) = fanout.unwrap_or((0, 0));
    app.provider_wait_incident_logged = true;
    codewhale_runtime::logging::warn(format!(
        "provider-wait incident: provider={} model={} elapsed_secs={elapsed} \
         idle_secs={} stream_idle_budget_secs={} max_subagents={} \
         fanout_running={fanout_running} fanout_total={fanout_total} \
         running_agents={} pending_dispatch={pending_dispatch}",
        app.provider_identity_for_persistence(),
        app.model,
        provider_wait_idle_secs(app),
        app.stream_chunk_timeout_secs,
        app.max_subagents,
        running_agent_count(app),
    ));
}

thread_local! {
    /// Objective summaries keyed by agent id (#6213 T7). The objective is
    /// immutable per agent, so `summarize_tool_output` — which JSON-parses the
    /// whole assignment — only has to run once per agent instead of once per
    /// `AgentProgress` event. `(length, hash)` of the objective guards the
    /// memo, so even an id reuse with different text recomputes.
    static OBJECTIVE_SUMMARIES: RefCell<HashMap<String, (usize, u64, String)>> =
        RefCell::new(HashMap::new());
}

pub(crate) fn subagent_objective_summary(app: &App, id: &str) -> Option<String> {
    let agent = app
        .subagent_cache
        .iter()
        .find(|agent| agent.agent_id == id)?;
    memoized_objective_summary(id, &agent.assignment.objective)
}

/// Memoized body of [`subagent_objective_summary`], split out so the memo can
/// be exercised without an `App`.
fn memoized_objective_summary(id: &str, objective: &str) -> Option<String> {
    OBJECTIVE_SUMMARIES.with(|cache| {
        let mut cache = cache.borrow_mut();
        let (len, hash) = (objective.len(), hash_str(objective));
        if let Some((cached_len, cached_hash, summary)) = cache.get(id)
            && *cached_len == len
            && *cached_hash == hash
        {
            return (!summary.is_empty()).then(|| summary.clone());
        }
        let summary = summarize_tool_output(objective);
        // Bounded: live agents per session are few; a full map means the
        // process has seen an unusual number of agents, so start over.
        if cache.len() >= 256 {
            cache.clear();
        }
        cache.insert(id.to_string(), (len, hash, summary.clone()));
        (!summary.is_empty()).then_some(summary)
    })
}

pub(crate) fn friendly_subagent_progress(
    app: &App,
    id: &str,
    status: &str,
    routine_wait: bool,
) -> String {
    if !routine_wait {
        return summarize_tool_output(status);
    }

    if let Some(summary) = subagent_objective_summary(app, id) {
        return format!("working on {summary}");
    }
    // Stored entries are always friendly rewrites (the event handler stores
    // `display`, never raw text), so no content check is needed here.
    if let Some(existing) = app.agent_progress.get(id)
        && existing != "working"
        && existing != "in the current"
    {
        return existing.clone();
    }
    "working".to_string()
}

pub(crate) fn one_line_summary(text: &str, max_width: usize) -> String {
    let mut cleaned = String::with_capacity(text.len());
    codewhale_tui_kit::osc8::strip_ansi_into(text, &mut cleaned);
    truncate_line_to_width(
        &cleaned.split_whitespace().collect::<Vec<_>>().join(" "),
        max_width,
    )
}

/// Objective + paused flag for the live goal, or `None` when no goal should
/// render (unset, or terminal Hunted/Escaped). Shared by the classic footer
/// chip and the ocean topbar chip so every shell surfaces the same state
/// (#39: the ocean shell has no sidebar, so without a topbar chip a goal set
/// via `create_goal` was invisible there).
pub(crate) fn active_goal_chip_state(app: &App) -> Option<(String, bool)> {
    let (objective, paused) = match (&app.goal.objective, &app.paused_goal_objective) {
        (Some(objective), _) => {
            if matches!(
                app.goal.status,
                codewhale_engine::tools::goal::GoalStatus::Complete
                    | codewhale_engine::tools::goal::GoalStatus::Blocked
            ) {
                return None;
            }
            (
                objective.clone(),
                app.goal.status == codewhale_engine::tools::goal::GoalStatus::Paused,
            )
        }
        (None, Some(objective)) => (objective.clone(), true),
        (None, None) => return None,
    };
    if objective.trim().is_empty() {
        return None;
    }
    Some((objective, paused))
}

pub(crate) fn format_token_count_compact(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
pub(crate) fn format_context_budget(used: i64, max: u32) -> String {
    let max_u64 = u64::from(max);
    let max_i64 = i64::from(max);

    if used > max_i64 {
        return format!(
            ">{}/{}",
            format_token_count_compact(max_u64),
            format_token_count_compact(max_u64)
        );
    }

    let used_u64 = u64::try_from(used.max(0)).unwrap_or(0);
    format!(
        "{}/{}",
        format_token_count_compact(used_u64),
        format_token_count_compact(max_u64)
    )
}

#[cfg(test)]
mod tests {
    use super::{memoized_objective_summary, one_line_summary};

    #[test]
    fn one_line_summary_strips_ansi_before_collapsing_text() {
        let summary = one_line_summary("read \x1b[38;2;6;174;242mfile.rs\x1b[0m", 80);
        assert_eq!(summary, "read file.rs");
        assert!(!summary.contains("38;2"));
    }

    #[test]
    fn objective_summary_memo_revalidates_on_content_change() {
        let id = "agent_memo_test";
        // Both objectives have the same length, so only the content hash can
        // tell them apart — the memo must not serve the first summary for the
        // second objective.
        let first = memoized_objective_summary(id, r#"{"message":"alpha"}"#);
        assert_eq!(first.as_deref(), Some("alpha"));
        let second = memoized_objective_summary(id, r#"{"message":"beta!"}"#);
        assert_eq!(second.as_deref(), Some("beta!"));
        // Unchanged objective: the memo path returns the same summary.
        let again = memoized_objective_summary(id, r#"{"message":"beta!"}"#);
        assert_eq!(again.as_deref(), Some("beta!"));
    }
}

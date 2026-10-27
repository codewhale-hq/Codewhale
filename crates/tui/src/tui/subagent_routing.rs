//! Sub-agent and background-task routing helpers for the TUI loop.

use std::time::{Duration, Instant};

use crate::task_manager::{TaskRecord, TaskStatus, TaskSummary};
use crate::tools::subagent::{
    AgentWorkerStatus, MailboxMessage, SubAgentResult, SubAgentStatus,
    subagent_progress_tool_display_name,
};
use crate::tui::app::{
    AgentCurrentActivity, AgentCurrentActivityStatus, AgentProgressMeta, AgentRecentAction, App,
    MAX_AGENT_RECENT_ACTIONS, TaskPanelEntry, TaskPanelEntryKind, bound_agent_activity_text,
};
use crate::tui::history::{HistoryCell, SubAgentCell, summarize_tool_output};
use crate::tui::pager::PagerView;
use crate::tui::tool_routing::refreshes_workspace_context_on_completion;
use crate::tui::widgets::agent_card::{
    AgentLifecycle, DelegateCard, FanoutCard, apply_to_delegate, apply_to_fanout,
};
use crate::tui::workspace_context;
use codewhale_config::AppMode;

/// Keep settled cards visible briefly, then archive them from the compact
/// live projection. Their transcript card and persisted agent record remain
/// reachable through the Agents register.
const SUBAGENT_TERMINAL_CARD_TTL: Duration = Duration::from_secs(45);
const SUBAGENT_TERMINAL_CARD_MAX_RETAINED: usize = 24;

pub(super) fn running_agent_count(app: &App) -> usize {
    let mut ids: std::collections::HashSet<&str> =
        app.agent_progress.keys().map(String::as_str).collect();
    for agent in app
        .subagent_cache
        .iter()
        .filter(|agent| matches!(agent.status, SubAgentStatus::Running))
    {
        ids.insert(agent.agent_id.as_str());
    }
    ids.len()
}

/// Describe detached workers that deliberately survive a parent-turn stop.
///
/// `agent` starts are detached from the turn cancellation token, so a plain
/// "Request cancelled" receipt is incomplete whenever live workers remain.
/// Use stable UI labels where available and raw ids as a lossless fallback;
/// sorting keeps the receipt deterministic across HashMap iteration order.
pub(super) fn parent_stop_status(app: &App, base: &str) -> String {
    let mut ids = std::collections::BTreeSet::new();
    ids.extend(app.agent_progress.keys().cloned());
    ids.extend(
        app.subagent_cache
            .iter()
            .filter(|agent| matches!(agent.status, SubAgentStatus::Running))
            .map(|agent| agent.agent_id.clone()),
    );
    if ids.is_empty() {
        return base.to_string();
    }

    let labels = ids
        .into_iter()
        .map(|id| {
            app.agent_label_map
                .get(&id)
                .filter(|label| !label.trim().is_empty())
                .cloned()
                .unwrap_or(id)
        })
        .collect::<Vec<_>>();
    format!(
        "{base}; detached workers continue (none canceled): {}",
        labels.join(", ")
    )
}

pub(super) fn active_fanout_counts(app: &App) -> Option<(usize, usize)> {
    // Read running count from the canonical slot states on the active
    // FanoutCard, if one exists. Used by `rlm` and any future multi-child
    // dispatch the parent agent makes via repeated `agent`.
    if let Some(idx) = app.last_fanout_card_index
        && let Some(HistoryCell::SubAgent(SubAgentCell::Fanout(card))) = app.history.get(idx)
    {
        let running = card
            .workers
            .iter()
            .filter(|slot| matches!(slot.status, AgentLifecycle::Running))
            .count();
        return Some((running, card.worker_count()));
    }
    None
}

/// True when this child settled by being *parked* at the parent's turn end
/// rather than by asking anyone anything (#5906).
///
/// The runtime parks a turn-owned child with a `needs_input` note that reads
/// like a question ("Resume this parked child with ..."), so every surface
/// that keys off `needs_input` used to render a parked husk with the same
/// "waiting for input" label as a child a user can actually answer. The
/// checkpoint records which of the two it is; this is the one place the UI
/// asks, so no surface has to sniff the reason string.
pub(crate) fn subagent_is_parked(agent: &SubAgentResult) -> bool {
    agent
        .checkpoint
        .as_ref()
        .is_some_and(|checkpoint| checkpoint.parked_at_turn_end)
}

/// The one-line recovery a parked row shows instead of a pending question:
/// nobody will answer it, so it names the two ways out that actually exist.
pub(crate) fn parked_recovery_detail(app: &App) -> String {
    codewhale_localization::tr(
        app.ui_locale,
        codewhale_localization::MessageId::AgentStatusParkedRecovery,
    )
    .into_owned()
}

pub(super) fn reconcile_subagent_activity_state(app: &mut App) {
    reconcile_subagent_activity_state_at(app, Instant::now());
}

pub(super) fn apply_subagent_terminal_projection(
    app: &mut App,
    agent_id: &str,
    status: SubAgentStatus,
    result: Option<String>,
) -> bool {
    app.agent_progress.remove(agent_id);

    let worker_status = worker_status_for_terminal_projection(&status);
    let safe_result = result.map(|result| bound_agent_activity_text(&result));
    let meta = app
        .agent_progress_meta
        .entry(agent_id.to_string())
        .or_default();
    // A parked child projects as `Interrupted` here. Interrupted-over-Waiting
    // and interrupted-over-Parked are both "the settled state the surface
    // already established, restated" — do not downgrade either (#5906).
    let sticky = meta
        .current_activity
        .as_ref()
        .map(|activity| activity.status)
        .filter(|status| {
            matches!(
                status,
                AgentCurrentActivityStatus::Waiting | AgentCurrentActivityStatus::Parked
            )
        });
    let activity_status = match sticky {
        Some(status) if worker_status == AgentWorkerStatus::Interrupted => status,
        _ => worker_status.into(),
    };
    let step = meta
        .current_activity
        .as_ref()
        .and_then(|activity| activity.step);
    meta.current_activity = Some(AgentCurrentActivity::bounded(
        activity_status,
        safe_result.clone(),
        None,
        step,
    ));
    meta.current_tool = None;

    let Some(agent) = app
        .subagent_cache
        .iter_mut()
        .find(|agent| agent.agent_id == agent_id)
    else {
        reconcile_subagent_activity_state(app);
        return false;
    };

    agent.worker_status = Some(worker_status);
    agent.status = status;
    if let Some(result) = safe_result {
        agent.result = Some(result);
    }
    reconcile_subagent_activity_state(app);
    true
}

pub(crate) fn worker_status_for_terminal_projection(status: &SubAgentStatus) -> AgentWorkerStatus {
    match status {
        SubAgentStatus::Running => AgentWorkerStatus::Running,
        SubAgentStatus::Completed => AgentWorkerStatus::Completed,
        SubAgentStatus::Interrupted(_) => AgentWorkerStatus::Interrupted,
        SubAgentStatus::Failed(_) | SubAgentStatus::BudgetExhausted => AgentWorkerStatus::Failed,
        SubAgentStatus::Cancelled => AgentWorkerStatus::Cancelled,
    }
}

pub(super) fn reconcile_subagent_activity_state_at(app: &mut App, now: Instant) {
    reconcile_terminal_subagent_card_retention(app, now);

    let cached_agents = app.subagent_cache.clone();
    let running_agents: Vec<(String, String)> = cached_agents
        .iter()
        .filter(|agent| matches!(agent.status, SubAgentStatus::Running))
        .map(|agent| {
            (
                agent.agent_id.clone(),
                summarize_tool_output(&agent.assignment.objective),
            )
        })
        .collect();

    let running_ids: std::collections::HashSet<String> =
        running_agents.iter().map(|(id, _)| id.clone()).collect();
    // Evict a progress row only when the authoritative cache actually knows
    // the agent and reports it non-running. A progress-only entry — an agent
    // whose AgentSpawned/AgentList delivery was dropped under channel
    // pressure so the cache has never seen it — must survive until the cache
    // supersedes it, or spawned agents flicker in and out of the sidebar.
    let cached_ids: std::collections::HashSet<String> = cached_agents
        .iter()
        .map(|agent| agent.agent_id.clone())
        .collect();
    app.agent_progress
        .retain(|id, _| running_ids.contains(id) || !cached_ids.contains(id));
    let progress_ids: std::collections::HashSet<String> =
        app.agent_progress.keys().cloned().collect();
    app.agent_progress_meta
        .retain(|id, _| cached_ids.contains(id) || progress_ids.contains(id));

    for (id, objective) in &running_agents {
        app.agent_progress
            .entry(id.clone())
            .or_insert_with(|| objective.clone());
    }

    let recovery_detail = parked_recovery_detail(app);
    for agent in &cached_agents {
        let meta = app
            .agent_progress_meta
            .entry(agent.agent_id.clone())
            .or_insert_with(|| AgentProgressMeta {
                parent_run_id: agent.parent_run_id.clone(),
                spawn_depth: agent.spawn_depth,
                ..AgentProgressMeta::default()
            });
        meta.parent_run_id = agent.parent_run_id.clone();
        meta.spawn_depth = agent.spawn_depth;

        let existing = meta.current_activity.clone();
        let parked = subagent_is_parked(agent);
        // Parked outranks `needs_input`: the park note *is* phrased as a
        // question, and reading it as one is exactly the bug (#5906).
        let mut structured_status = if parked {
            AgentCurrentActivityStatus::Parked
        } else if agent.needs_input.is_some() {
            AgentCurrentActivityStatus::Waiting
        } else if let Some(worker_status) = agent.worker_status {
            worker_status.into()
        } else if matches!(agent.status, SubAgentStatus::Running) {
            existing
                .as_ref()
                .map(|activity| activity.status)
                .unwrap_or(AgentCurrentActivityStatus::Running)
        } else {
            worker_status_for_terminal_projection(&agent.status).into()
        };
        if structured_status == AgentCurrentActivityStatus::Interrupted
            && existing
                .as_ref()
                .is_some_and(|activity| activity.status == AgentCurrentActivityStatus::Waiting)
        {
            structured_status = AgentCurrentActivityStatus::Waiting;
        }

        let detail = parked
            .then(|| recovery_detail.clone())
            .or_else(|| {
                agent
                    .needs_input
                    .as_ref()
                    .map(|needs_input| needs_input.question.clone())
            })
            .or_else(|| {
                existing
                    .as_ref()
                    .filter(|activity| activity.status == structured_status)
                    .and_then(|activity| activity.detail.clone())
            })
            .or_else(|| agent.result.clone());
        let current_tool = existing
            .as_ref()
            .filter(|_| structured_status == AgentCurrentActivityStatus::RunningTool)
            .and_then(|activity| activity.current_tool.clone());
        let step = (agent.steps_taken > 0)
            .then_some(agent.steps_taken)
            .or_else(|| existing.as_ref().and_then(|activity| activity.step));
        meta.current_activity = Some(AgentCurrentActivity::bounded(
            structured_status,
            detail,
            current_tool.clone(),
            step,
        ));
        meta.current_tool = current_tool;
    }

    if running_ids.is_empty() {
        app.agent_activity_started_at = None;
    } else if app.agent_activity_started_at.is_none() {
        app.agent_activity_started_at = Some(Instant::now());
    }

    reconcile_cards_with_snapshots(app);
}

fn reconcile_terminal_subagent_card_retention(app: &mut App, now: Instant) {
    let current_ids: std::collections::HashSet<String> = app
        .subagent_cache
        .iter()
        .map(|agent| agent.agent_id.clone())
        .collect();
    app.subagent_terminal_seen_at
        .retain(|id, _| current_ids.contains(id));

    for agent in &app.subagent_cache {
        if matches!(agent.status, SubAgentStatus::Running) {
            app.subagent_terminal_seen_at.remove(&agent.agent_id);
        } else {
            app.subagent_terminal_seen_at
                .entry(agent.agent_id.clone())
                .or_insert(now);
        }
    }

    app.subagent_cache.retain(|agent| {
        if matches!(agent.status, SubAgentStatus::Running) {
            return true;
        }
        app.subagent_terminal_seen_at
            .get(&agent.agent_id)
            .and_then(|seen_at| now.checked_duration_since(*seen_at))
            .is_none_or(|age| age <= SUBAGENT_TERMINAL_CARD_TTL)
    });

    let mut terminal_seen: Vec<(String, Instant)> = app
        .subagent_cache
        .iter()
        .filter(|agent| !matches!(agent.status, SubAgentStatus::Running))
        .filter_map(|agent| {
            app.subagent_terminal_seen_at
                .get(&agent.agent_id)
                .map(|seen_at| (agent.agent_id.clone(), *seen_at))
        })
        .collect();
    terminal_seen.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let keep_terminal_ids: std::collections::HashSet<String> = terminal_seen
        .into_iter()
        .take(SUBAGENT_TERMINAL_CARD_MAX_RETAINED)
        .map(|(id, _)| id)
        .collect();
    app.subagent_cache.retain(|agent| {
        matches!(agent.status, SubAgentStatus::Running)
            || keep_terminal_ids.contains(agent.agent_id.as_str())
    });

    let kept_ids: std::collections::HashSet<String> = app
        .subagent_cache
        .iter()
        .map(|agent| agent.agent_id.clone())
        .collect();
    app.subagent_terminal_seen_at
        .retain(|id, _| kept_ids.contains(id));
}

/// Sync in-transcript card slots that still render as running against the
/// canonical manager snapshot statuses. A card can miss its terminal mailbox
/// envelope (e.g. API-timeout interruption observed only via `AgentList`),
/// which would otherwise leave the fanout/delegate UI counting the agent as
/// running indefinitely.
fn reconcile_cards_with_snapshots(app: &mut App) {
    let non_running: Vec<(String, AgentLifecycle)> = app
        .subagent_cache
        .iter()
        .filter_map(|agent| {
            let lifecycle = match &agent.status {
                SubAgentStatus::Running => return None,
                SubAgentStatus::Interrupted(_) => AgentLifecycle::Interrupted,
                SubAgentStatus::Completed => AgentLifecycle::Completed,
                SubAgentStatus::Failed(_) => AgentLifecycle::Failed,
                SubAgentStatus::Cancelled => AgentLifecycle::Cancelled,
                SubAgentStatus::BudgetExhausted => AgentLifecycle::Failed,
            };
            Some((agent.agent_id.clone(), lifecycle))
        })
        .collect();
    for (agent_id, lifecycle) in non_running {
        let Some(&idx) = app.subagent_card_index.get(&agent_id) else {
            continue;
        };
        let updated = match app.history.get_mut(idx) {
            Some(HistoryCell::SubAgent(SubAgentCell::Delegate(card)))
                if card.agent_id == agent_id
                    && matches!(
                        card.status,
                        AgentLifecycle::Pending | AgentLifecycle::Running
                    ) =>
            {
                card.status = lifecycle;
                true
            }
            Some(HistoryCell::SubAgent(SubAgentCell::Fanout(card))) => {
                match card.workers.iter_mut().find(|slot| {
                    slot.agent_id == agent_id
                        && matches!(
                            slot.status,
                            AgentLifecycle::Pending | AgentLifecycle::Running
                        )
                }) {
                    Some(slot) => {
                        slot.status = lifecycle;
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        };
        if updated {
            app.bump_history_cell(idx);
        }
    }
}

fn subagent_status_rank(status: &SubAgentStatus) -> u8 {
    match status {
        SubAgentStatus::Running => 0,
        SubAgentStatus::Interrupted(_) => 1,
        SubAgentStatus::Failed(_) => 2,
        SubAgentStatus::Completed => 3,
        SubAgentStatus::Cancelled => 4,
        SubAgentStatus::BudgetExhausted => 2,
    }
}

pub(super) fn sort_subagents_in_place(agents: &mut [SubAgentResult]) {
    agents.sort_by(|a, b| {
        subagent_status_rank(&a.status)
            .cmp(&subagent_status_rank(&b.status))
            .then_with(|| a.agent_type.as_str().cmp(b.agent_type.as_str()))
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });
}

pub(super) fn subagent_message_refreshes_workspace_context(message: &MailboxMessage) -> bool {
    matches!(
        message,
        MailboxMessage::ToolCallCompleted { tool_name, .. }
            if refreshes_workspace_context_on_completion(tool_name)
    )
}

/// Route a `MailboxMessage` envelope to the matching in-transcript card,
/// allocating a `DelegateCard` or `FanoutCard` on first sight (issue #128).
pub(super) fn handle_subagent_mailbox_for_turn(
    app: &mut App,
    turn_id: &str,
    seq: u64,
    message: &MailboxMessage,
) -> bool {
    // Accumulate sub-agent token costs for the real-time footer counter (#166).
    if let MailboxMessage::TokenUsage {
        agent_id: _,
        source_id,
        route,
        usage,
    } = message
    {
        // The child's own route truth always wins and is never guessed from
        // provider identity: `route` is the immutable envelope its client was
        // frozen with at construction, so its billing mode, billing surface
        // and endpoint fingerprint are the child's dispatch receipt. A child
        // whose route froze as Unknown stays Unknown.
        //
        // Sub-agent spend joins the parent total, so it also joins the
        // completeness counters `/cost` reports against that total.
        let stable_source = if source_id.trim().is_empty() {
            format!("mailbox:{turn_id}:{seq}")
        } else {
            source_id.clone()
        };
        let source_fingerprint = crate::cost_status::usage_source_fingerprint(&stable_source);
        if crate::cost_status::usage_source_seen(&stable_source) {
            // The runtime sink runs synchronously before mailbox publication.
            // Fold its identity and money into App together, before a following
            // TurnComplete can snapshot either half. A post-seal response has
            // no mailbox event and is swept by the event loop instead.
            let first_live_delivery = !app
                .session
                .subagent_usage_sources
                .contains(&source_fingerprint);
            let pending_runtime_cost = crate::cost_status::drain();
            if !pending_runtime_cost.is_empty() {
                app.absorb_pending_background_cost(&pending_runtime_cost);
            }
            if first_live_delivery
                && app
                    .session
                    .subagent_usage_sources
                    .contains(&source_fingerprint)
            {
                record_agent_current_activity(app, message);
            }
            return false;
        }
        if app
            .session
            .subagent_usage_sources
            .insert(source_fingerprint)
        {
            // Update the Work-row receipt under the same replay guard as
            // cost. A mailbox replay must not make either total grow twice.
            // Usage confirms or replaces the child's frozen spawn model; the
            // parent's configured/default route is never child evidence.
            record_agent_current_activity(app, message);
            let audit = route.audit(usage);
            app.record_turn_cost_audit(&audit);
            app.record_turn_cost_route_receipt(route.receipt(&audit));
            if let Some(cost) = audit.estimate {
                app.accrue_subagent_cost_estimate(cost);
            }
        }
        // No transcript card changed. The event-loop redraw still repaints
        // the Work row and footer from the updated receipt state.
        return false;
    }

    // Resolve (or allocate) the target cell for this envelope. ChildSpawned
    // is special — it always belongs to the active fanout card if one
    // exists; otherwise it seeds a new one.
    let display_message = bounded_mailbox_message(message);
    let agent_id = display_message.agent_id().to_string();
    record_agent_current_activity(app, message);
    if subagent_message_refreshes_workspace_context(message) {
        workspace_context::refresh_now(app, Instant::now());
    }

    if matches!(message, MailboxMessage::ChildSpawned { .. })
        && let Some(idx) = app.last_fanout_card_index
        && let Some(HistoryCell::SubAgent(SubAgentCell::Fanout(card))) = app.history.get_mut(idx)
    {
        let updated = apply_to_fanout(card, &display_message);
        app.subagent_card_index.insert(agent_id, idx);
        if updated {
            app.bump_history_cell(idx);
        }
        return updated;
    }

    // Existing card for this agent_id? Mutate in place.
    if let Some(&idx) = app.subagent_card_index.get(&agent_id) {
        let updated = match app.history.get_mut(idx) {
            Some(HistoryCell::SubAgent(SubAgentCell::Delegate(card))) => {
                apply_to_delegate(card, &display_message)
            }
            Some(HistoryCell::SubAgent(SubAgentCell::Fanout(card))) => {
                apply_to_fanout(card, &display_message)
            }
            _ => false,
        };
        if updated {
            // idx is already in scope from the outer
            // `if let Some(&idx) = app.subagent_card_index.get(&agent_id)`.
            app.bump_history_cell(idx);
        }
        return updated;
    }

    // No existing card — only `Started` reasonably opens one. Anything else
    // for an unknown agent_id is dropped (likely arrived after the cell was
    // cleared, e.g. session-resume edge cases).
    let agent_type = match &display_message {
        MailboxMessage::Started { agent_type, .. } => agent_type.clone(),
        MailboxMessage::Completed { .. }
        | MailboxMessage::Failed { .. }
        | MailboxMessage::Interrupted { .. }
        | MailboxMessage::Cancelled { .. } => "unknown".to_string(),
        _ => return false,
    };

    let dispatch_kind = app.pending_subagent_dispatch.as_deref();
    let is_fanout = matches!(dispatch_kind, Some("rlm_open" | "rlm_eval" | "rlm"));

    if is_fanout {
        // A completion-first delivery opens the slot settled, as the delegate
        // path does; recording it as running would leave a finished worker
        // counted as running with nothing left to settle it (U05-04).
        let first_status = match &display_message {
            MailboxMessage::Completed { .. } => AgentLifecycle::Completed,
            MailboxMessage::Failed { .. } => AgentLifecycle::Failed,
            MailboxMessage::Interrupted { .. } => AgentLifecycle::Interrupted,
            MailboxMessage::Cancelled { .. } => AgentLifecycle::Cancelled,
            _ => AgentLifecycle::Running,
        };
        // Reuse the active fanout card for sibling spawns; otherwise create
        // one anchored at this position so subsequent siblings join it.
        if let Some(idx) = app.last_fanout_card_index
            && let Some(HistoryCell::SubAgent(SubAgentCell::Fanout(card))) =
                app.history.get_mut(idx)
        {
            let updated = card.claim_pending_worker(&agent_id, first_status);
            app.subagent_card_index.insert(agent_id, idx);
            if updated {
                app.bump_history_cell(idx);
            }
            updated
        } else {
            let mut card = FanoutCard::new(dispatch_kind.unwrap_or("rlm_eval").to_string());
            card.upsert_worker(&agent_id, first_status);
            app.add_message(HistoryCell::SubAgent(SubAgentCell::Fanout(card)));
            let idx = app.history.len().saturating_sub(1);
            app.last_fanout_card_index = Some(idx);
            app.subagent_card_index.insert(agent_id, idx);
            app.bump_history_cell(idx);
            true
        }
    } else {
        let mut card = DelegateCard::new(agent_id.clone(), agent_type.clone());
        apply_to_delegate(&mut card, &display_message);
        app.add_message(HistoryCell::SubAgent(SubAgentCell::Delegate(card)));
        let idx = app.history.len().saturating_sub(1);
        app.subagent_card_index.insert(agent_id.clone(), idx);
        // Single delegate consumes the pending dispatch label so a follow-on
        // tool call doesn't accidentally inherit it.
        app.pending_subagent_dispatch = None;
        // idx was just inserted on the line above — no need to re-query.
        app.bump_history_cell(idx);
        true
    }
}

#[cfg(test)]
pub(super) fn handle_subagent_mailbox(app: &mut App, seq: u64, message: &MailboxMessage) -> bool {
    handle_subagent_mailbox_for_turn(app, "test-turn", seq, message)
}

fn bounded_mailbox_message(message: &MailboxMessage) -> MailboxMessage {
    match message {
        MailboxMessage::Progress { agent_id, status } => MailboxMessage::Progress {
            agent_id: agent_id.clone(),
            status: bound_agent_activity_text(status),
        },
        MailboxMessage::ToolCallStarted {
            agent_id,
            tool_name,
            step,
        } => MailboxMessage::ToolCallStarted {
            agent_id: agent_id.clone(),
            tool_name: bound_agent_activity_text(subagent_progress_tool_display_name(tool_name)),
            step: *step,
        },
        MailboxMessage::ToolCallCompleted {
            agent_id,
            tool_name,
            step,
            ok,
        } => MailboxMessage::ToolCallCompleted {
            agent_id: agent_id.clone(),
            tool_name: bound_agent_activity_text(subagent_progress_tool_display_name(tool_name)),
            step: *step,
            ok: *ok,
        },
        MailboxMessage::Completed { agent_id, summary } => MailboxMessage::Completed {
            agent_id: agent_id.clone(),
            summary: bound_agent_activity_text(summary),
        },
        MailboxMessage::Failed { agent_id, error } => MailboxMessage::Failed {
            agent_id: agent_id.clone(),
            error: bound_agent_activity_text(error),
        },
        MailboxMessage::Interrupted { agent_id, reason } => MailboxMessage::Interrupted {
            agent_id: agent_id.clone(),
            reason: bound_agent_activity_text(reason),
        },
        // Item text is model-authored and reaches the transcript, so it gets
        // the same redaction/bounding every other displayed child string gets.
        // Ids, statuses, and counts are preserved exactly — bounding must not
        // change what the ledger says.
        MailboxMessage::WorkState { agent_id, todo } => MailboxMessage::WorkState {
            agent_id: agent_id.clone(),
            todo: crate::tools::todo::TodoListSnapshot {
                items: todo
                    .items
                    .iter()
                    .map(|item| crate::tools::todo::TodoItem {
                        content: bound_agent_activity_text(&item.content),
                        ..item.clone()
                    })
                    .collect(),
                ..todo.clone()
            },
        },
        _ => message.clone(),
    }
}

fn record_agent_current_activity(app: &mut App, message: &MailboxMessage) {
    let agent_id = message.agent_id().to_string();
    let meta = app.agent_progress_meta.entry(agent_id).or_default();
    meta.last_progress_at = Some(Instant::now());
    if let MailboxMessage::TokenUsage { route, usage, .. } = message {
        // The child's own used-token tally (input + output), matching the
        // worker budget's `usage_total_tokens`. Counting only completions made
        // the strip look "stuck" on tiny numbers while the child was burning
        // context. Absent until a real envelope lands, so an agent with no
        // reported usage shows no number instead of a zero.
        let turn_total =
            u64::from(usage.input_tokens).saturating_add(u64::from(usage.output_tokens));
        meta.received_tokens = Some(meta.received_tokens.unwrap_or(0).saturating_add(turn_total));
        meta.resolved_provider = Some(route.provider.as_str().to_string());
        meta.resolved_model = Some(bound_agent_activity_text(
            &codewhale_runtime::safe_label::sanitize_persisted_route_label(&route.model),
        ))
        .filter(|model| !model.trim().is_empty());
        return;
    }
    if let MailboxMessage::WorkState { todo, .. } = message {
        // Work state is a separate fact from what the agent is doing right
        // now — publishing a ledger update must not invent an activity
        // transition. It does update the remaining-to-do chip for the strip.
        if todo.is_empty() {
            meta.todos_remaining = None;
        } else {
            let remaining = todo
                .items
                .iter()
                .filter(|item| !item.status.is_settled())
                .count();
            meta.todos_remaining = Some(u32::try_from(remaining).unwrap_or(u32::MAX));
        }
        return;
    }
    if let MailboxMessage::ToolCallCompleted {
        tool_name,
        step,
        ok,
        ..
    } = message
    {
        if meta.recent_actions.len() == MAX_AGENT_RECENT_ACTIONS {
            meta.recent_actions.pop_front();
        }
        meta.recent_actions.push_back(AgentRecentAction::bounded(
            subagent_progress_tool_display_name(tool_name),
            *step,
            *ok,
        ));
    }
    let previous = meta.current_activity.clone();

    let (status, detail, current_tool, step) = match message {
        MailboxMessage::Started { agent_type, .. } => (
            AgentCurrentActivityStatus::Running,
            Some(format!("started {agent_type}")),
            None,
            None,
        ),
        MailboxMessage::Progress { status, .. } => (
            previous
                .as_ref()
                .map(|activity| activity.status)
                .unwrap_or(AgentCurrentActivityStatus::Running),
            Some(status.clone()),
            previous
                .as_ref()
                .and_then(|activity| activity.current_tool.clone()),
            previous.as_ref().and_then(|activity| activity.step),
        ),
        MailboxMessage::ToolCallStarted {
            tool_name, step, ..
        } => (
            AgentCurrentActivityStatus::RunningTool,
            None,
            Some(subagent_progress_tool_display_name(tool_name).to_string()),
            Some(*step),
        ),
        MailboxMessage::ToolCallCompleted {
            tool_name,
            step,
            ok,
            ..
        } => (
            AgentCurrentActivityStatus::Running,
            Some(format!(
                "{} {}",
                subagent_progress_tool_display_name(tool_name),
                if *ok { "completed" } else { "failed" }
            )),
            None,
            Some(*step),
        ),
        MailboxMessage::ChildSpawned { parent_id, .. } => (
            AgentCurrentActivityStatus::Starting,
            Some(format!("spawned by {parent_id}")),
            None,
            None,
        ),
        MailboxMessage::Completed { summary, .. } => (
            AgentCurrentActivityStatus::Done,
            Some(summary.clone()),
            None,
            previous.as_ref().and_then(|activity| activity.step),
        ),
        MailboxMessage::Failed { error, .. } => (
            AgentCurrentActivityStatus::Failed,
            Some(error.clone()),
            None,
            previous.as_ref().and_then(|activity| activity.step),
        ),
        MailboxMessage::Interrupted { reason, .. } => (
            AgentCurrentActivityStatus::Waiting,
            Some(reason.clone()),
            None,
            previous.as_ref().and_then(|activity| activity.step),
        ),
        MailboxMessage::Cancelled { .. } => (
            AgentCurrentActivityStatus::Canceled,
            None,
            None,
            previous.as_ref().and_then(|activity| activity.step),
        ),
        MailboxMessage::TokenUsage { .. } => unreachable!("token usage handled above"),
        MailboxMessage::WorkState { .. } => unreachable!("work state handled above"),
    };

    // A settled agent is final, as on its transcript card (U05-04): mailbox
    // producers are concurrent, so a Started/Progress/tool envelope can land
    // after the terminal one. It must not move the Work row back to active.
    // Only settlements whose continuation is a *new* agent latch; `Waiting`
    // (a question) resumes this same agent. The manager snapshot
    // (`reconcile_subagent_activity_state`) stays the authority either way.
    let settled = previous.as_ref().is_some_and(|activity| {
        matches!(
            activity.status,
            AgentCurrentActivityStatus::Done
                | AgentCurrentActivityStatus::Failed
                | AgentCurrentActivityStatus::Canceled
                | AgentCurrentActivityStatus::Interrupted
                | AgentCurrentActivityStatus::Parked
        )
    });
    if !settled {
        meta.current_activity = Some(AgentCurrentActivity::bounded(
            status,
            detail,
            current_tool.clone(),
            step,
        ));
        meta.current_tool = current_tool;
    }
    if let MailboxMessage::ToolCallCompleted {
        tool_name,
        ok: true,
        ..
    } = message
        && is_file_mutation_tool(tool_name)
    {
        meta.files_touched = meta.files_touched.saturating_add(1);
    }
}

fn is_file_mutation_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "apply_patch" | "fim_edit" | "Write" | "Edit"
    )
}

pub(super) fn task_mode_label(mode: AppMode) -> &'static str {
    mode.as_setting()
}

pub(super) fn task_summary_to_panel_entry(summary: TaskSummary) -> TaskPanelEntry {
    TaskPanelEntry {
        exit_code: None,
        id: summary.id,
        status: task_status_label(summary.status).to_string(),
        prompt_summary: summary.prompt_summary,
        duration_ms: summary.duration_ms,
        kind: TaskPanelEntryKind::Background,
        stale: false,
        elapsed_since_output_ms: None,
        owner_agent_id: None,
        owner_agent_name: None,
        current_tool: None,
        role: None,
        files_touched: 0,
    }
}

fn task_status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Queued => "queued",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Canceled => "canceled",
    }
}

pub(super) fn format_task_list(tasks: &[TaskSummary]) -> String {
    if tasks.is_empty() {
        return "No tasks found.".to_string();
    }

    let show_session = tasks.iter().any(|task| task.owner_session_id.is_some());
    let mut lines = vec![format!("Tasks ({})", tasks.len())];
    // Build headers with the same format strings as the rows so the ID
    // column (21-char `task_` ids) can never drift out of alignment again.
    if show_session {
        lines.push(format!(
            "{:<21}  {:<9}  {:<12}  {:>8}  {}",
            "ID", "Status", "Session", "Time", "Title"
        ));
    } else {
        lines.push(format!(
            "{:<21}  {:<9}  {:>8}  {}",
            "ID", "Status", "Time", "Title"
        ));
    }
    lines.push("------------------------------------------------------------".to_string());
    for task in tasks {
        let duration = task
            .duration_ms
            .map(codewhale_command_contract::elapsed::format_elapsed_ms)
            .unwrap_or_else(|| "-".to_string());
        let owner_session = task.owner_session_id.as_deref().unwrap_or("-");
        let owner_session = if owner_session.chars().count() > 12 {
            format!("{}…", owner_session.chars().take(11).collect::<String>())
        } else {
            owner_session.to_string()
        };
        if show_session {
            lines.push(format!(
                "{:<21}  {:<9}  {:<12}  {:>8}  {}",
                task.id,
                task_status_label(task.status),
                owner_session,
                duration,
                task.prompt_summary
            ));
        } else {
            lines.push(format!(
                "{:<21}  {:<9}  {:>8}  {}",
                task.id,
                task_status_label(task.status),
                duration,
                task.prompt_summary
            ));
        }
    }
    lines.push("Use /task show <id> for timeline details.".to_string());
    lines.join("\n")
}

pub(super) fn open_task_pager(app: &mut App, task: &TaskRecord) {
    let width = app
        .viewport
        .last_transcript_area
        .map(|area| area.width)
        .unwrap_or(100)
        .saturating_sub(4);
    app.view_stack.push(PagerView::from_text(
        format!("Task {}", task.id),
        &format_task_detail(task),
        width.max(60),
    ));
}

fn format_task_detail(task: &TaskRecord) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Task: {}", task.id));
    lines.push(format!("Status: {}", task_status_label(task.status)));
    lines.push(format!("Mode: {}", task.mode));
    lines.push(format!("Model: {}", task.model));
    lines.push(format!(
        "Workspace: {}",
        codewhale_util::utils::display_path(&task.workspace)
    ));
    if let Some(owner_session_id) = task.owner_session_id.as_deref() {
        lines.push(format!("Owning Session: {owner_session_id}"));
    }
    if let Some(thread_id) = task.thread_id.as_ref() {
        lines.push(format!("Runtime Thread: {thread_id}"));
    }
    if let Some(turn_id) = task.turn_id.as_ref() {
        lines.push(format!("Runtime Turn: {turn_id}"));
    }
    if task.runtime_event_count > 0 {
        lines.push(format!("Runtime Events: {}", task.runtime_event_count));
    }
    lines.push(format!("Created: {}", task.created_at));
    if let Some(started_at) = task.started_at {
        lines.push(format!("Started: {started_at}"));
    }
    if let Some(ended_at) = task.ended_at {
        lines.push(format!("Ended: {ended_at}"));
    }
    if let Some(duration) = task.duration_ms {
        lines.push(format!(
            "Duration: {}",
            codewhale_command_contract::elapsed::format_elapsed_ms(duration)
        ));
    }
    lines.push(String::new());
    lines.push("Prompt:".to_string());
    lines.push(task.prompt.clone());

    if let Some(summary) = task.result_summary.as_ref() {
        lines.push(String::new());
        lines.push("Result Summary:".to_string());
        lines.push(summary.clone());
    }
    if let Some(path) = task.result_detail_path.as_ref() {
        lines.push(format!("Result Artifact: {}", path.display()));
    }
    if let Some(error) = task.error.as_ref() {
        lines.push(String::new());
        lines.push(format!("Error: {error}"));
    }

    lines.push(String::new());
    lines.push("Tool Calls:".to_string());
    if task.tool_calls.is_empty() {
        lines.push("- (none)".to_string());
    } else {
        for tool in &task.tool_calls {
            let status = match tool.status {
                crate::task_manager::TaskToolStatus::Running => "running",
                crate::task_manager::TaskToolStatus::Success => "success",
                crate::task_manager::TaskToolStatus::Failed => "failed",
                crate::task_manager::TaskToolStatus::Canceled => "canceled",
            };
            let mut line = format!(
                "- {} [{}] {}",
                tool.name,
                status,
                tool.output_summary.as_deref().unwrap_or("(no summary)")
            );
            if let Some(duration) = tool.duration_ms {
                line.push_str(&format!(" ({:.2}s)", duration as f64 / 1000.0));
            }
            lines.push(line);
            if let Some(path) = tool.detail_path.as_ref() {
                lines.push(format!("  detail: {}", path.display()));
            }
            if let Some(path) = tool.patch_ref.as_ref() {
                lines.push(format!("  patch: {}", path.display()));
            }
        }
    }

    lines.push(String::new());
    lines.push("Timeline:".to_string());
    if task.timeline.is_empty() {
        lines.push("- (none)".to_string());
    } else {
        for entry in &task.timeline {
            lines.push(format!(
                "- [{}] {}: {}",
                entry.timestamp, entry.kind, entry.summary
            ));
            if let Some(path) = entry.detail_path.as_ref() {
                lines.push(format!("  detail: {}", path.display()));
            }
        }
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_manager::{TaskStatus, TaskSummary};
    use crate::tools::subagent::SubAgentAssignment;
    use crate::tui::app::{InitialInput, TuiOptions};
    use crate::tui::widgets::agent_card::AgentLifecycle;
    use chrono::Utc;
    use codewhale_agent_contract::role::FleetRole;
    use codewhale_runtime::config::Config;
    use std::path::PathBuf;

    fn test_options() -> TuiOptions {
        TuiOptions {
            model: "test-model".to_string(),
            allow_shell: true,
            max_subagents: 4,
            start_in_agent_mode: true,
            initial_input: None::<InitialInput>,
            startup_notice: None,
            ..crate::tui::test_support::test_tui_options(PathBuf::from("."))
        }
    }

    fn test_route(
        provider: codewhale_config::ProviderKind,
        model: &str,
    ) -> crate::cost_status::EffectiveRouteEnvelope {
        crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            provider,
            provider.as_str(),
            model,
            Some(provider.provider().default_base_url()),
            Utc::now(),
        )
    }

    fn task_summary(id: &str, status: TaskStatus, duration_ms: Option<u64>) -> TaskSummary {
        TaskSummary {
            execution_binding_known: true,
            id: id.to_string(),
            status,
            prompt_summary: "Fix task list output".to_string(),
            name: None,
            model: "deepseek-v4-pro".to_string(),
            model_provider: None,
            model_provider_id: None,
            mode: "agent".to_string(),
            workspace: PathBuf::from("/tmp"),
            created_at: Utc::now(),
            started_at: None,
            ended_at: None,
            duration_ms,
            lifecycle_seq: 1,
            error: None,
            terminal_reason: None,
            thread_id: None,
            turn_id: None,
            owner_session_id: None,
        }
    }

    fn subagent_result(id: &str, status: SubAgentStatus) -> SubAgentResult {
        SubAgentResult {
            usage: None,
            name: id.to_string(),
            agent_id: id.to_string(),
            context_mode: "fresh".to_string(),
            fork_context: false,
            workspace: None,
            git_branch: None,
            agent_type: FleetRole::Worker,
            assignment: SubAgentAssignment {
                native_preset: None,
                objective: format!("objective-{id}"),
                role: Some("worker".to_string()),
            },
            model: "deepseek-v4-flash".to_string(),
            nickname: None,
            status,
            worker_status: None,
            runtime_permissions: None,
            parent_run_id: None,
            spawn_depth: 0,
            child_route: None,
            result: None,
            steps_taken: 0,
            checkpoint: None,
            needs_input: None,
            duration_ms: 0,
            started_at: None,
            from_prior_session: false,
            idle_ms: None,
            heartbeat_timeout_ms: None,
        }
    }

    #[test]
    fn task_list_includes_title_header_and_time_column() {
        let output = format_task_list(&[
            task_summary("task_12345678", TaskStatus::Running, None),
            task_summary("task_abcdef12", TaskStatus::Completed, Some(1234)),
        ]);

        assert!(output.contains(&format!(
            "{:<21}  {:<9}  {:>8}  {}",
            "ID", "Status", "Time", "Title"
        )));
        assert!(output.contains(&format!(
            "{:<21}  {:<9}  {:>8}  {}",
            "task_12345678", "running", "-", "Fix task list output"
        )));
        assert!(output.contains(&format!(
            "{:<21}  {:<9}  {:>8}  {}",
            "task_abcdef12", "completed", "1s", "Fix task list output"
        )));
    }

    #[test]
    fn task_list_shows_owner_session_when_present() {
        let mut task = task_summary("task_owned", TaskStatus::Running, None);
        task.owner_session_id = Some("session-123456".to_string());

        let output = format_task_list(&[task]);

        assert!(output.contains(&format!(
            "{:<21}  {:<9}  {:<12}  {:>8}  {}",
            "ID", "Status", "Session", "Time", "Title"
        )));
        // Owner ids are truncated to 11 chars + '…' so the rendered value
        // stays inside the 12-wide Session column and cannot drift the
        // remaining columns out of alignment.
        assert!(output.contains("session-123…"), "{output}");
    }

    #[test]
    fn mailbox_progress_reports_transcript_change_only_for_visible_card_updates() {
        let mut app = App::new(test_options(), &Config::default());
        let started = MailboxMessage::started("agent_live", FleetRole::Worker);
        assert!(
            handle_subagent_mailbox(&mut app, 1, &started),
            "first started envelope creates a visible card"
        );

        let progress =
            MailboxMessage::progress("agent_live", "step 1/100: requesting model response");
        assert!(
            !handle_subagent_mailbox(&mut app, 2, &progress),
            "low-signal progress for an already-running card is a no-op"
        );

        let tool = MailboxMessage::ToolCallStarted {
            agent_id: "agent_live".to_string(),
            tool_name: "read_file".to_string(),
            step: 1,
        };
        assert!(
            handle_subagent_mailbox(&mut app, 3, &tool),
            "tool progress still updates the visible transcript card"
        );
        assert_eq!(
            app.agent_progress_meta["agent_live"]
                .current_tool
                .as_deref(),
            Some("read_file")
        );

        let completed = MailboxMessage::ToolCallCompleted {
            agent_id: "agent_live".to_string(),
            tool_name: "read_file".to_string(),
            step: 1,
            ok: true,
        };
        assert!(handle_subagent_mailbox(&mut app, 4, &completed));
        assert_eq!(app.agent_progress_meta["agent_live"].current_tool, None);

        let wrote = MailboxMessage::ToolCallCompleted {
            agent_id: "agent_live".to_string(),
            tool_name: "apply_patch".to_string(),
            step: 2,
            ok: true,
        };
        assert!(handle_subagent_mailbox(&mut app, 5, &wrote));
        assert_eq!(app.agent_progress_meta["agent_live"].files_touched, 1);
    }

    #[test]
    fn canonical_child_file_activity_counts_only_successful_mutations() {
        let mut app = App::new(test_options(), &Config::default());

        for (step, tool_name) in ["read_file", "list_dir", "file_search", "grep_files"]
            .into_iter()
            .enumerate()
        {
            record_agent_current_activity(
                &mut app,
                &MailboxMessage::ToolCallCompleted {
                    agent_id: "agent_files".to_string(),
                    tool_name: tool_name.to_string(),
                    step: step as u32,
                    ok: true,
                },
            );
        }
        assert_eq!(app.agent_progress_meta["agent_files"].files_touched, 0);

        for (step, tool_name) in ["write_file", "edit_file", "apply_patch"]
            .into_iter()
            .enumerate()
        {
            record_agent_current_activity(
                &mut app,
                &MailboxMessage::ToolCallCompleted {
                    agent_id: "agent_files".to_string(),
                    tool_name: tool_name.to_string(),
                    step: (step + 10) as u32,
                    ok: true,
                },
            );
        }
        assert_eq!(app.agent_progress_meta["agent_files"].files_touched, 3);

        record_agent_current_activity(
            &mut app,
            &MailboxMessage::ToolCallCompleted {
                agent_id: "agent_files".to_string(),
                tool_name: "write_file".to_string(),
                step: 20,
                ok: false,
            },
        );
        assert_eq!(app.agent_progress_meta["agent_files"].files_touched, 3);
    }

    #[test]
    fn recent_actions_are_three_bounded_structured_tool_outcomes() {
        let mut app = App::new(test_options(), &Config::default());
        let agent_id = "agent_recent";
        for step in 1..=5 {
            record_agent_current_activity(
                &mut app,
                &MailboxMessage::ToolCallCompleted {
                    agent_id: agent_id.to_string(),
                    tool_name: format!("\u{1b}[31mtool_{step}\u{1b}[0m"),
                    step,
                    ok: step != 4,
                },
            );
        }
        record_agent_current_activity(
            &mut app,
            &MailboxMessage::Progress {
                agent_id: agent_id.to_string(),
                status: "tool_99 completed".to_string(),
            },
        );

        let actions = &app.agent_progress_meta[agent_id].recent_actions;
        assert_eq!(actions.len(), MAX_AGENT_RECENT_ACTIONS);
        assert_eq!(
            actions.iter().map(|action| action.step).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert!(!actions.iter().any(|action| action.step == 99));
        assert!(actions.iter().all(|action| !action.tool.contains('\u{1b}')));
        assert!(!actions[1].ok);
    }

    #[test]
    fn token_usage_records_only_the_effective_child_route_facts() {
        let mut app = App::new(test_options(), &Config::default());
        let changed = handle_subagent_mailbox(
            &mut app,
            91,
            &MailboxMessage::TokenUsage {
                agent_id: "agent_route".to_string(),
                source_id: "response-route".to_string(),
                route: Box::new(test_route(
                    codewhale_config::ProviderKind::Openrouter,
                    "vendor/model-real",
                )),
                usage: codewhale_models::Usage::default(),
            },
        );

        assert!(!changed, "route facts do not allocate a transcript card");
        let meta = &app.agent_progress_meta["agent_route"];
        assert_eq!(meta.resolved_provider.as_deref(), Some("openrouter"));
        assert_eq!(meta.resolved_model.as_deref(), Some("vendor/model-real"));
        assert!(meta.current_activity.is_none());
    }

    #[test]
    fn token_usage_accumulates_input_plus_output_across_child_turns() {
        let mut app = App::new(test_options(), &Config::default());
        let route = test_route(
            codewhale_config::ProviderKind::Deepseek,
            "deepseek-v4-flash",
        );
        handle_subagent_mailbox(
            &mut app,
            1,
            &MailboxMessage::TokenUsage {
                agent_id: "agent_spend".to_string(),
                source_id: "response-1".to_string(),
                route: Box::new(route.clone()),
                usage: codewhale_models::Usage {
                    input_tokens: 1_000,
                    output_tokens: 40,
                    ..Default::default()
                },
            },
        );
        handle_subagent_mailbox(
            &mut app,
            2,
            &MailboxMessage::TokenUsage {
                agent_id: "agent_spend".to_string(),
                source_id: "response-2".to_string(),
                route: Box::new(route),
                usage: codewhale_models::Usage {
                    input_tokens: 2_000,
                    output_tokens: 60,
                    ..Default::default()
                },
            },
        );

        assert_eq!(
            app.agent_progress_meta["agent_spend"].received_tokens,
            Some(3_100),
            "work-bar tally must match worker budget total (input+output)"
        );
    }

    #[test]
    fn typed_mailbox_lifecycle_projects_running_waiting_failed_and_done() {
        let mut app = App::new(test_options(), &Config::default());

        assert!(handle_subagent_mailbox(
            &mut app,
            1,
            &MailboxMessage::started("agent_running", FleetRole::Worker),
        ));
        assert_eq!(
            app.agent_progress_meta["agent_running"]
                .current_activity
                .as_ref()
                .map(|activity| activity.status),
            Some(AgentCurrentActivityStatus::Running)
        );

        assert!(handle_subagent_mailbox(
            &mut app,
            2,
            &MailboxMessage::ToolCallStarted {
                agent_id: "agent_running".to_string(),
                tool_name: "read_file".to_string(),
                step: 3,
            },
        ));
        let running = app.agent_progress_meta["agent_running"]
            .current_activity
            .as_ref()
            .expect("running tool projection");
        assert_eq!(running.status, AgentCurrentActivityStatus::RunningTool);
        assert_eq!(running.current_tool.as_deref(), Some("read_file"));
        assert_eq!(running.step, Some(3));

        assert!(handle_subagent_mailbox(
            &mut app,
            3,
            &MailboxMessage::Interrupted {
                agent_id: "agent_running".to_string(),
                reason: "approval needed".to_string(),
            },
        ));
        let waiting = app.agent_progress_meta["agent_running"]
            .current_activity
            .as_ref()
            .expect("waiting projection");
        assert_eq!(waiting.status, AgentCurrentActivityStatus::Waiting);
        assert_eq!(waiting.detail.as_deref(), Some("approval needed"));

        for (seq, agent_id, terminal, expected) in [
            (
                4,
                "agent_failed",
                MailboxMessage::Failed {
                    agent_id: "agent_failed".to_string(),
                    error: "verification failed".to_string(),
                },
                AgentCurrentActivityStatus::Failed,
            ),
            (
                5,
                "agent_done",
                MailboxMessage::Completed {
                    agent_id: "agent_done".to_string(),
                    summary: "verification complete".to_string(),
                },
                AgentCurrentActivityStatus::Done,
            ),
        ] {
            assert!(handle_subagent_mailbox(&mut app, seq, &terminal));
            assert_eq!(
                app.agent_progress_meta[agent_id]
                    .current_activity
                    .as_ref()
                    .map(|activity| activity.status),
                Some(expected)
            );
        }
    }

    #[test]
    fn reconcile_projects_typed_status_when_activity_detail_is_missing() {
        let mut app = App::new(test_options(), &Config::default());
        let mut agent = subagent_result("agent_model_wait", SubAgentStatus::Running);
        agent.worker_status = Some(AgentWorkerStatus::ModelWait);
        app.subagent_cache.push(agent);

        reconcile_subagent_activity_state_at(&mut app, Instant::now());

        let activity = app.agent_progress_meta["agent_model_wait"]
            .current_activity
            .as_ref()
            .expect("typed activity fallback");
        assert_eq!(activity.status, AgentCurrentActivityStatus::ModelWait);
        assert_eq!(activity.detail, None);
    }

    #[test]
    fn mailbox_compact_projection_redacts_secrets_and_control_sequences() {
        let mut app = App::new(test_options(), &Config::default());
        let agent_id = "agent_safe_projection";
        assert!(handle_subagent_mailbox(
            &mut app,
            1,
            &MailboxMessage::started(agent_id, FleetRole::Worker),
        ));
        let secret = "sk-mailbox-secret-1234567890";
        let raw = format!(
            "\u{1b}[31mrunning\u{1b}[0m\napi_key={secret}\n\u{1b}]8;;https://example.invalid\u{7}details\u{1b}]8;;\u{7}\u{1}"
        );
        assert!(handle_subagent_mailbox(
            &mut app,
            2,
            &MailboxMessage::progress(agent_id, raw.clone()),
        ));

        let activity = app.agent_progress_meta[agent_id]
            .current_activity
            .as_ref()
            .expect("safe activity projection");
        let detail = activity.detail.as_deref().expect("safe detail");
        assert!(detail.contains("[redacted]"), "{detail:?}");
        assert!(!detail.contains(secret), "{detail:?}");
        assert!(!detail.contains('\u{1b}'), "{detail:?}");
        assert!(!detail.contains("example.invalid"), "{detail:?}");

        let card_index = app.subagent_card_index[agent_id];
        let HistoryCell::SubAgent(SubAgentCell::Delegate(card)) = &app.history[card_index] else {
            panic!("expected delegate card");
        };
        let rendered = card
            .render_lines(120, &codewhale_palette::UI_THEME)
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
            .collect::<String>();
        assert!(rendered.contains("[redacted]"), "{rendered:?}");
        assert!(!rendered.contains(secret), "{rendered:?}");
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
        assert!(!rendered.contains("example.invalid"), "{rendered:?}");
        assert!(
            raw.contains(secret),
            "source mailbox payload stays untouched"
        );
        assert!(
            raw.contains('\u{1b}'),
            "source mailbox payload stays untouched"
        );
    }

    #[test]
    fn reconcile_keeps_progress_only_rows_until_cache_knows_the_agent() {
        let mut app = App::new(test_options(), &Config::default());

        // A progress-first agent: its AgentSpawned/AgentList delivery was
        // dropped under channel pressure, so the authoritative cache has
        // never seen it. Its sidebar row must survive reconciliation.
        app.agent_progress
            .insert("agent_orphan".to_string(), "step 2/10".to_string());
        app.agent_progress_meta.insert(
            "agent_orphan".to_string(),
            AgentProgressMeta {
                parent_run_id: None,
                spawn_depth: 0,
                ..AgentProgressMeta::default()
            },
        );

        // A terminal agent the cache DOES know about: its stale progress row
        // must still be evicted.
        app.subagent_cache
            .push(subagent_result("agent_done", SubAgentStatus::Completed));
        app.agent_progress
            .insert("agent_done".to_string(), "step 9/10".to_string());
        app.agent_progress_meta.insert(
            "agent_done".to_string(),
            AgentProgressMeta {
                parent_run_id: None,
                spawn_depth: 0,
                ..AgentProgressMeta::default()
            },
        );

        reconcile_subagent_activity_state_at(&mut app, Instant::now());

        assert!(
            app.agent_progress.contains_key("agent_orphan"),
            "progress-only agent unknown to the cache must survive reconcile"
        );
        assert!(
            app.agent_progress_meta.contains_key("agent_orphan"),
            "progress-only meta unknown to the cache must survive reconcile"
        );
        assert!(
            !app.agent_progress.contains_key("agent_done"),
            "cache-known terminal agent progress must still be evicted"
        );
        assert_eq!(
            app.agent_progress_meta["agent_done"]
                .current_activity
                .as_ref()
                .map(|activity| activity.status),
            Some(AgentCurrentActivityStatus::Done),
            "cache-known terminal agents retain a bounded terminal projection"
        );

        // Once the authoritative cache reports the orphan as terminal, the
        // normal eviction applies and the row is released.
        app.subagent_cache
            .push(subagent_result("agent_orphan", SubAgentStatus::Completed));
        reconcile_subagent_activity_state_at(&mut app, Instant::now());
        assert!(
            !app.agent_progress.contains_key("agent_orphan"),
            "cache supersedes the progress-only row once it knows the agent"
        );
        assert_eq!(
            app.agent_progress_meta["agent_orphan"]
                .current_activity
                .as_ref()
                .map(|activity| activity.status),
            Some(AgentCurrentActivityStatus::Done)
        );
    }

    #[test]
    fn terminal_cards_archive_after_forty_five_seconds_without_losing_history() {
        let mut app = App::new(test_options(), &Config::default());
        let agent_id = "agent_archive";
        assert!(handle_subagent_mailbox(
            &mut app,
            1,
            &MailboxMessage::started(agent_id, FleetRole::Worker),
        ));
        app.subagent_cache
            .push(subagent_result(agent_id, SubAgentStatus::Completed));

        let observed = Instant::now();
        reconcile_subagent_activity_state_at(&mut app, observed);
        reconcile_subagent_activity_state_at(&mut app, observed + SUBAGENT_TERMINAL_CARD_TTL);
        assert!(
            app.subagent_cache
                .iter()
                .any(|agent| agent.agent_id == agent_id),
            "the terminal card stays visible through its full 45-second grace period"
        );

        reconcile_subagent_activity_state_at(
            &mut app,
            observed + SUBAGENT_TERMINAL_CARD_TTL + Duration::from_millis(1),
        );
        assert!(
            !app.subagent_cache
                .iter()
                .any(|agent| agent.agent_id == agent_id),
            "the compact live cache must archive the settled card after the grace period"
        );
        assert!(
            app.subagent_card_index.contains_key(agent_id),
            "archiving a compact card must not delete its transcript record"
        );
    }

    #[test]
    fn apply_subagent_terminal_projection_clears_live_progress_and_card_state() {
        let mut app = App::new(test_options(), &Config::default());
        let started = MailboxMessage::started("agent_done", FleetRole::Worker);
        assert!(handle_subagent_mailbox(&mut app, 1, &started));
        let card_idx = app.subagent_card_index["agent_done"];
        let initial_revision = app.history_revisions[card_idx];

        app.subagent_cache
            .push(subagent_result("agent_done", SubAgentStatus::Running));
        app.agent_progress
            .insert("agent_done".to_string(), "step 4/10".to_string());
        app.agent_progress_meta.insert(
            "agent_done".to_string(),
            AgentProgressMeta {
                parent_run_id: None,
                spawn_depth: 0,
                ..AgentProgressMeta::default()
            },
        );

        assert!(apply_subagent_terminal_projection(
            &mut app,
            "agent_done",
            SubAgentStatus::Cancelled,
            Some("cancelled by user".to_string())
        ));

        assert!(!app.agent_progress.contains_key("agent_done"));
        assert_eq!(
            app.agent_progress_meta["agent_done"]
                .current_activity
                .as_ref()
                .map(|activity| activity.status),
            Some(AgentCurrentActivityStatus::Canceled)
        );
        let agent = app
            .subagent_cache
            .iter()
            .find(|agent| agent.agent_id == "agent_done")
            .expect("projected agent remains cached");
        assert_eq!(agent.status, SubAgentStatus::Cancelled);
        assert_eq!(agent.worker_status, Some(AgentWorkerStatus::Cancelled));
        assert_eq!(agent.result.as_deref(), Some("cancelled by user"));
        assert_eq!(running_agent_count(&app), 0);
        assert_ne!(
            app.history_revisions[card_idx], initial_revision,
            "terminal projection should invalidate the stale running card"
        );
        match &app.history[card_idx] {
            HistoryCell::SubAgent(SubAgentCell::Delegate(card)) => {
                assert_eq!(card.status, AgentLifecycle::Cancelled);
            }
            cell => panic!("expected delegate card, got {cell:?}"),
        }
    }

    #[test]
    fn parent_stop_status_names_only_workers_that_continue_detached() {
        let mut app = App::new(test_options(), &Config::default());
        app.subagent_cache
            .push(subagent_result("agent_b", SubAgentStatus::Running));
        app.subagent_cache
            .push(subagent_result("agent_done", SubAgentStatus::Completed));
        app.agent_progress
            .insert("agent_a".to_string(), "running tool".to_string());
        app.agent_label_map
            .insert("agent_a".to_string(), "Agent 1".to_string());
        app.agent_label_map
            .insert("agent_b".to_string(), "Southern Right".to_string());
        app.agent_label_map
            .insert("agent_done".to_string(), "Finished worker".to_string());

        let status = parent_stop_status(&app, "Request cancelled");
        assert_eq!(
            status,
            "Request cancelled; detached workers continue (none canceled): Agent 1, Southern Right"
        );
        assert!(!status.contains("Finished worker"));
    }

    #[test]
    fn parent_stop_status_is_unchanged_without_detached_workers() {
        let app = App::new(test_options(), &Config::default());
        assert_eq!(
            parent_stop_status(&app, "Request cancelled"),
            "Request cancelled"
        );
    }

    #[test]
    fn completion_before_started_allocates_recovery_delegate_card() {
        let mut app = App::new(test_options(), &Config::default());
        let completed = MailboxMessage::Completed {
            agent_id: "agent_early".to_string(),
            summary: "recovered after early completion".to_string(),
        };
        assert!(
            handle_subagent_mailbox(&mut app, 1, &completed),
            "completion-first delivery must still open a card"
        );
        assert!(app.subagent_card_index.contains_key("agent_early"));

        let started = MailboxMessage::started("agent_early", FleetRole::Worker);
        assert!(handle_subagent_mailbox(&mut app, 2, &started));
        match app.history.last() {
            Some(HistoryCell::SubAgent(SubAgentCell::Delegate(card))) => {
                assert_eq!(card.agent_id, "agent_early");
                assert_ne!(card.agent_type, "…");
            }
            other => panic!("expected delegate card, got {other:?}"),
        }
    }

    /// #4810: each child card carries that child's own ledger — never the
    /// parent's, never a sibling's, and never as transcript messages.
    #[test]
    fn sibling_child_cards_render_disjoint_todo_lists() {
        use crate::tools::todo::{TodoItem, TodoListSnapshot, TodoStatus};

        fn snapshot(id: u32, content: &str) -> TodoListSnapshot {
            TodoListSnapshot {
                items: vec![TodoItem {
                    id,
                    content: content.to_string(),
                    status: TodoStatus::InProgress,
                }],
                completion_pct: 0,
                in_progress_id: Some(id),
            }
        }

        fn card_text(app: &App, agent_id: &str) -> String {
            let idx = app.subagent_card_index[agent_id];
            let HistoryCell::SubAgent(SubAgentCell::Delegate(card)) = &app.history[idx] else {
                panic!("expected a delegate card for {agent_id}");
            };
            assert_eq!(card.agent_id, agent_id);
            card.render_lines(120, &app.ui_theme)
                .into_iter()
                .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
                .collect()
        }

        let mut app = App::new(test_options(), &Config::default());
        // The parent's own ledger exists and must never surface on a child.
        let parent_item = "PARENT ONLY: ship the release";

        for (seq, id) in ["agent_left", "agent_right"].into_iter().enumerate() {
            assert!(handle_subagent_mailbox(
                &mut app,
                seq as u64 + 1,
                &MailboxMessage::started(id, FleetRole::Worker),
            ));
        }
        assert!(handle_subagent_mailbox(
            &mut app,
            3,
            &MailboxMessage::WorkState {
                agent_id: "agent_left".to_string(),
                todo: snapshot(1, "LEFT: map the call sites"),
            },
        ));
        assert!(handle_subagent_mailbox(
            &mut app,
            4,
            &MailboxMessage::WorkState {
                agent_id: "agent_right".to_string(),
                todo: snapshot(1, "RIGHT: write the migration"),
            },
        ));

        let left = card_text(&app, "agent_left");
        let right = card_text(&app, "agent_right");
        assert!(left.contains("LEFT: map the call sites"), "{left}");
        assert!(!left.contains("RIGHT:"), "sibling leak: {left}");
        assert!(!left.contains(parent_item), "parent leak: {left}");
        assert!(right.contains("RIGHT: write the migration"), "{right}");
        assert!(!right.contains("LEFT:"), "sibling leak: {right}");
        assert!(!right.contains(parent_item), "parent leak: {right}");

        // A nested child of agent_left gets its own card and its own ledger;
        // neither its parent's card nor its uncle's card absorbs it.
        assert!(handle_subagent_mailbox(
            &mut app,
            5,
            &MailboxMessage::started("agent_nested", FleetRole::Worker),
        ));
        assert!(handle_subagent_mailbox(
            &mut app,
            6,
            &MailboxMessage::WorkState {
                agent_id: "agent_nested".to_string(),
                todo: snapshot(1, "NESTED: read one file"),
            },
        ));
        assert!(card_text(&app, "agent_nested").contains("NESTED: read one file"));
        assert!(!card_text(&app, "agent_left").contains("NESTED:"));
        assert!(!card_text(&app, "agent_right").contains("NESTED:"));

        // Nothing entered the transcript as an ordinary message: every To-do
        // row lives inside a sub-agent card.
        assert!(
            !app.history.iter().any(|cell| {
                !matches!(cell, HistoryCell::SubAgent(_))
                    && format!("{cell:?}").contains("map the call sites")
            }),
            "child To-do must not become an ordinary transcript message"
        );
    }

    /// Work state is a ledger fact, not an activity transition: it must not
    /// invent or overwrite what the agent is currently doing.
    #[test]
    fn work_state_updates_todos_remaining_without_rewriting_activity() {
        use crate::tools::todo::{TodoItem, TodoListSnapshot, TodoStatus};

        let mut app = App::new(test_options(), &Config::default());
        assert!(handle_subagent_mailbox(
            &mut app,
            1,
            &MailboxMessage::started("agent_todos", FleetRole::Worker),
        ));
        assert!(handle_subagent_mailbox(
            &mut app,
            2,
            &MailboxMessage::ToolCallStarted {
                agent_id: "agent_todos".to_string(),
                tool_name: "read_file".to_string(),
                step: 1,
            },
        ));
        assert!(handle_subagent_mailbox(
            &mut app,
            3,
            &MailboxMessage::WorkState {
                agent_id: "agent_todos".to_string(),
                todo: TodoListSnapshot {
                    items: vec![
                        TodoItem {
                            id: 1,
                            content: "done already".to_string(),
                            status: TodoStatus::Completed,
                        },
                        TodoItem {
                            id: 2,
                            content: "still cooking".to_string(),
                            status: TodoStatus::InProgress,
                        },
                        TodoItem {
                            id: 3,
                            content: "not yet".to_string(),
                            status: TodoStatus::Pending,
                        },
                    ],
                    completion_pct: 33,
                    in_progress_id: Some(2),
                },
            },
        ));

        let meta = &app.agent_progress_meta["agent_todos"];
        assert_eq!(meta.todos_remaining, Some(2));
        let activity = meta.current_activity.as_ref().expect("activity");
        assert_eq!(activity.status, AgentCurrentActivityStatus::RunningTool);
        assert_eq!(activity.current_tool.as_deref(), Some("read_file"));

        // Empty publish clears the chip source (no list → no figure).
        assert!(handle_subagent_mailbox(
            &mut app,
            4,
            &MailboxMessage::WorkState {
                agent_id: "agent_todos".to_string(),
                todo: TodoListSnapshot::default(),
            },
        ));
        assert_eq!(app.agent_progress_meta["agent_todos"].todos_remaining, None);
    }

    #[test]
    fn work_state_envelope_does_not_rewrite_current_activity() {
        let mut app = App::new(test_options(), &Config::default());
        assert!(handle_subagent_mailbox(
            &mut app,
            1,
            &MailboxMessage::started("agent_x", FleetRole::Worker),
        ));
        assert!(handle_subagent_mailbox(
            &mut app,
            2,
            &MailboxMessage::ToolCallStarted {
                agent_id: "agent_x".to_string(),
                tool_name: "read_file".to_string(),
                step: 2,
            },
        ));
        assert!(handle_subagent_mailbox(
            &mut app,
            3,
            &MailboxMessage::WorkState {
                agent_id: "agent_x".to_string(),
                todo: crate::tools::todo::TodoListSnapshot {
                    items: vec![crate::tools::todo::TodoItem {
                        id: 1,
                        content: "keep reading".to_string(),
                        status: crate::tools::todo::TodoStatus::InProgress,
                    }],
                    completion_pct: 0,
                    in_progress_id: Some(1),
                },
            },
        ));

        let activity = app.agent_progress_meta["agent_x"]
            .current_activity
            .as_ref()
            .expect("activity");
        assert_eq!(activity.status, AgentCurrentActivityStatus::RunningTool);
        assert_eq!(activity.current_tool.as_deref(), Some("read_file"));
    }

    /// Displayed child ledger text goes through the same redaction the rest of
    /// the child's displayed strings do.
    #[test]
    fn work_state_item_text_is_redacted_before_it_reaches_the_card() {
        let mut app = App::new(test_options(), &Config::default());
        assert!(handle_subagent_mailbox(
            &mut app,
            1,
            &MailboxMessage::started("agent_secret", FleetRole::Worker),
        ));
        let secret = "sk-mailbox-secret-1234567890";
        assert!(handle_subagent_mailbox(
            &mut app,
            2,
            &MailboxMessage::WorkState {
                agent_id: "agent_secret".to_string(),
                todo: crate::tools::todo::TodoListSnapshot {
                    items: vec![crate::tools::todo::TodoItem {
                        id: 4,
                        content: format!("rotate api_key={secret}"),
                        status: crate::tools::todo::TodoStatus::InProgress,
                    }],
                    completion_pct: 0,
                    in_progress_id: Some(4),
                },
            },
        ));

        let idx = app.subagent_card_index["agent_secret"];
        let HistoryCell::SubAgent(SubAgentCell::Delegate(card)) = &app.history[idx] else {
            panic!("expected delegate card");
        };
        let rendered: String = card
            .render_lines(120, &codewhale_palette::UI_THEME)
            .into_iter()
            .flat_map(|line| line.spans.into_iter().map(|span| span.content.into_owned()))
            .collect();
        assert!(!rendered.contains(secret), "{rendered}");
        assert!(rendered.contains("[redacted]"), "{rendered}");
        assert!(
            rendered.contains("#4"),
            "item identity is preserved: {rendered}"
        );
    }

    #[test]
    fn fanout_completion_burst_preserves_started_to_done_ordering() {
        let mut app = App::new(test_options(), &Config::default());
        app.pending_subagent_dispatch = Some("rlm_eval".to_string());
        for (seq, id) in ["agent_a", "agent_b"].into_iter().enumerate() {
            assert!(handle_subagent_mailbox(
                &mut app,
                seq as u64 + 1,
                &MailboxMessage::started(id, FleetRole::Scout),
            ));
        }
        assert!(handle_subagent_mailbox(
            &mut app,
            3,
            &MailboxMessage::Completed {
                agent_id: "agent_a".to_string(),
                summary: "a done".to_string(),
            },
        ));
        let Some(HistoryCell::SubAgent(SubAgentCell::Fanout(card))) = app.history.last() else {
            panic!("expected fanout card");
        };
        assert_eq!(card.workers.len(), 2);
        assert_eq!(card.workers[0].status, AgentLifecycle::Completed);
        assert_eq!(card.workers[1].status, AgentLifecycle::Running);
    }
}

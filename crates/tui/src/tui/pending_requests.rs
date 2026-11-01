//! Child-agent requests waiting on the person (approvals C1).
//!
//! A child's approval card can be hidden (Esc), buried under another view, or
//! arrive while the parent is idle. This store is the one source for what is
//! still waiting: the footer row, `/agents` re-opening a card, and retiring a
//! card that was answered elsewhere all read it. It is keyed by approval id,
//! so parallel child calls each keep their own entry.

use std::time::Instant;

use crate::tui::app::App;
use crate::tui::approval::ApprovalOwner;
use codewhale_engine::core::events::Event as EngineEvent;
use codewhale_engine::tools::subagent::{AgentWorkerStatus, SubAgentManager};
use codewhale_localization::MessageId;

/// Everything needed to re-raise a hidden child card.
#[derive(Debug, Clone)]
pub(crate) struct PendingChildRequest {
    pub agent_id: String,
    pub tool_name: String,
    pub description: String,
    pub input: serde_json::Value,
    pub approval_key: String,
    pub approval_grouping_key: String,
    pub intent_summary: Option<String>,
    pub requested_at: Instant,
}

/// The owning agent id of a child approval id
/// (`agent:{agent_id}:approval:{boot}:{n}`), or `None` for a parent id.
#[must_use]
pub(crate) fn child_agent_id(approval_id: &str) -> Option<&str> {
    if !SubAgentManager::is_child_approval_id(approval_id) {
        return None;
    }
    approval_id
        .strip_prefix("agent:")?
        .split_once(":approval:")
        .map(|(agent_id, _)| agent_id)
        .filter(|agent_id| !agent_id.is_empty())
}

/// Owner label for a child's card: stable agent label plus Fleet role.
pub(crate) fn owner_for(app: &mut App, agent_id: &str) -> ApprovalOwner {
    let label = app.ensure_agent_label(agent_id);
    let role = app
        .subagent_cache
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .map(|agent| agent.agent_type.as_str().to_string());
    ApprovalOwner {
        agent_id: agent_id.to_string(),
        label,
        role,
    }
}

/// Remember a child request that is now shown to the person.
pub(crate) fn record(app: &mut App, approval_id: &str, request: PendingChildRequest) {
    app.pending_child_requests
        .insert(approval_id.to_string(), request);
    app.needs_redraw = true;
}

/// The request was answered (here, on the web, or by the engine ending the
/// wait): forget it and retire its card wherever it sits in the stack.
pub(crate) fn resolve(app: &mut App, approval_id: &str) -> bool {
    let known = app.pending_child_requests.remove(approval_id).is_some();
    let removed_card = app.view_stack.remove_tool_decision_by_id(approval_id);
    if known || removed_card {
        app.needs_redraw = true;
    }
    known || removed_card
}

/// The engine ended the wait (the agent's work ended, or the decision was
/// made elsewhere): retire the card and entry, and withdraw the web
/// mirror's copy so no surface keeps offering a decision nobody is waiting
/// on. A local decision uses [`resolve`] instead — the mirror is told the
/// decision itself.
pub(crate) fn retire(app: &mut App, approval_id: &str) -> bool {
    let retired = resolve(app, approval_id);
    let withdrawn = app.remote_control.withdraw_pending_approval(approval_id);
    retired || withdrawn
}

/// Approval retirement and child bookkeeping, run before any session
/// or idle filter (approvals M1, minor 4):
///
/// - agent lifecycle events record which conversation owns each agent, so a
///   request from another conversation's child is never shown here;
/// - a progress event that names a child approval id and is no longer
///   waiting ends that wait by identity — answered anywhere, cancelled,
///   stopped — whatever conversation is active. The engine sends it without
///   back-pressure drops.
/// - a typed withdrawal retires any approval card and its web mirror by id.
///
/// Returns `true` when the event only retires a request: nothing else should
/// process it.
pub(crate) fn observe_engine_event(app: &mut App, event: &EngineEvent) -> bool {
    match event {
        EngineEvent::ApprovalWithdrawn { id } => {
            retire(app, id);
            true
        }
        EngineEvent::AgentSpawned {
            owner_session_id,
            id,
            ..
        }
        | EngineEvent::AgentComplete {
            owner_session_id,
            id,
            ..
        } => {
            note_agent_session(app, id, owner_session_id);
            false
        }
        EngineEvent::AgentProgress {
            owner_session_id,
            id,
            activity,
            ..
        } => {
            note_agent_session(app, id, owner_session_id);
            match activity.approval_id.as_deref() {
                Some(approval_id)
                    if activity.worker_status != AgentWorkerStatus::WaitingForUser =>
                {
                    retire(app, approval_id);
                    activity.worker_status.is_terminal()
                }
                _ => false,
            }
        }
        _ => false,
    }
}

fn note_agent_session(app: &mut App, agent_id: &str, owner_session_id: &str) {
    if !owner_session_id.is_empty() {
        app.child_agent_sessions
            .insert(agent_id.to_string(), owner_session_id.to_string());
    }
}

/// Whether `approval_id` is a child request from an agent known to belong to
/// a conversation other than the active one. An agent this host has not seen
/// yet is not foreign: its request is shown rather than lost.
#[must_use]
pub(crate) fn is_foreign_child_request(app: &App, approval_id: &str) -> bool {
    child_agent_id(approval_id)
        .and_then(|agent_id| app.child_agent_sessions.get(agent_id))
        .is_some_and(|owner| app.current_session_id.as_deref() != Some(owner.as_str()))
}

/// The agent finished or stopped: nothing it asked for is pending any more.
pub(crate) fn clear_for_agent(app: &mut App, agent_id: &str) {
    let ids: Vec<String> = app
        .pending_child_requests
        .iter()
        .filter(|(_, request)| request.agent_id == agent_id)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        retire(app, &id);
    }
}

/// A different conversation owns none of this one's pending requests
/// (session switch, `/new`, `/clear`). The engine side needs no answer from
/// here: every conversation boundary finalizes the old conversation's running
/// children (`finalize_session_close_for_session`), which withdraws their
/// pending requests and records each as `cancelled` — never as a denial.
/// A late request from such a child is answered `unavailable` on arrival
/// (`is_foreign_child_request`).
pub(crate) fn clear_all(app: &mut App) {
    let ids: Vec<String> = app.pending_child_requests.keys().cloned().collect();
    for id in ids {
        retire(app, &id);
    }
}

/// One footer row per agent that is waiting on the person and whose card is
/// not the view on top: "Approval needed in {agent} — /agents".
pub(crate) fn footer_rows(app: &App) -> Vec<String> {
    let top = app.view_stack.top_tool_decision_id();
    let mut agents: Vec<&str> = Vec::new();
    for (id, request) in &app.pending_child_requests {
        if top == Some(id.as_str()) || agents.contains(&request.agent_id.as_str()) {
            continue;
        }
        agents.push(request.agent_id.as_str());
    }
    agents
        .into_iter()
        .map(|agent_id| {
            let label = app
                .agent_label_map
                .get(agent_id)
                .cloned()
                .unwrap_or_else(|| agent_id.to_string());
            app.tr(MessageId::PendingApprovalInAgent)
                .replace("{agent}", &label)
        })
        .collect()
}

/// Re-raise the agent's hidden cards (oldest first) so the person can answer
/// them after `/agents` or "Go to agent". Cards already in the stack stay put.
pub(crate) fn repush_for_agent(
    app: &mut App,
    agent_id: &str,
    default_selection: codewhale_runtime::config::ApprovalDefaultSelection,
    timeout: Option<std::time::Duration>,
) -> bool {
    let mut waiting: Vec<(String, PendingChildRequest)> = app
        .pending_child_requests
        .iter()
        .filter(|(id, request)| {
            request.agent_id == agent_id && !app.view_stack.contains_tool_decision_id(id)
        })
        .map(|(id, request)| (id.clone(), request.clone()))
        .collect();
    waiting.sort_by_key(|(_, request)| request.requested_at);
    let pushed = !waiting.is_empty();
    for (id, request) in waiting.into_iter().rev() {
        crate::tui::ui::push_approval_request_view(
            app,
            &id,
            &request.tool_name,
            &request.description,
            &request.input,
            &request.approval_key,
            &request.approval_grouping_key,
            request.intent_summary.as_deref(),
            default_selection,
            timeout,
        );
    }
    pushed
}

//! UI-side approval disposition and durable denial receipts.

use crate::core::engine::EngineHandle;
use crate::tui::app::{App, StatusToastLevel};
use crate::tui::history::HistoryCell;
use codewhale_execpolicy::ApprovalMode;
use codewhale_localization::MessageId;
use codewhale_runtime::audit::log_sensitive_event;

pub(super) fn is_session_approved_for_tool(
    app: &App,
    _tool_name: &str,
    grouping_key: &str,
) -> bool {
    // Session grants match the grouping key only (command family / host /
    // patch paths). A bare tool name is never session-wide: approving one
    // shell command used to auto-approve the entire shell tool for the
    // session. The `contains(tool_name)` clause was the escalation (ops R2).
    app.approval_session_approved.contains(grouping_key)
}

pub(super) fn is_session_denied_for_key(app: &App, approval_key: &str) -> bool {
    app.approval_session_denied.contains(approval_key)
}

/// A Deny holds for the rest of the user turn it was given in: the model's
/// retry loop must not re-prompt for the same call, but the user's next
/// message is a new intent and may deserve a different answer.
pub(super) fn end_turn_scoped_denials(app: &mut App) {
    app.approval_session_denied.clear();
}

/// A different conversation (a session switch or resume) inherits neither
/// this conversation's denials nor its "approve for session" grants: both
/// describe work the user was looking at here. `/new` and `/clear` do the
/// same in `reset_conversation_state`.
pub(super) fn reset_approval_scope_for_new_conversation(app: &mut App) {
    app.approval_session_denied.clear();
    app.approval_session_approved.clear();
    crate::tui::pending_requests::clear_all(app);
}

pub(super) fn session_denied_notice(app: &App, tool_name: &str) -> String {
    app.tr(MessageId::ApprovalAutoDeniedSession)
        .replace("{tool}", tool_name)
}

pub(super) fn surface_session_denied_notice(app: &mut App, tool_name: &str) {
    let notice = session_denied_notice(app, tool_name);
    app.push_status_toast(notice.clone(), StatusToastLevel::Warning, Some(12_000));

    // Tool completion and turn completion can replace the one-line status
    // before the next frame is painted. Keep the recovery path in the
    // transcript as a settled receipt as well, where it survives that event
    // ordering and remains available to screen readers and scrollback.
    let latest_transcript_cell = app
        .active_cell
        .as_ref()
        .and_then(|cell| cell.entries().last())
        .or_else(|| app.history.last());
    let already_latest_receipt = matches!(
        latest_transcript_cell,
        Some(HistoryCell::System { content }) if content == &notice
    );
    if !already_latest_receipt {
        let receipt = HistoryCell::System { content: notice };
        if let Some(active_cell) = app.active_cell.as_mut() {
            // Never grow committed history underneath an active cell: tool
            // lookup indices address `history ++ active_cell`, so changing
            // history.len() mid-turn would retarget the pending completion.
            active_cell.push_untracked(receipt);
            app.bump_active_cell_revision();
        } else {
            app.add_message(receipt);
        }
    }
}

pub(super) async fn auto_deny_session_approval(
    app: &mut App,
    engine_handle: &EngineHandle,
    id: &str,
    tool_name: &str,
    approval_key: &str,
) {
    log_sensitive_event(
        "tool.approval.auto_deny_session",
        serde_json::json!({
            "tool_name": tool_name,
            "approval_key": approval_key,
            "session_id": app.current_session_id,
        }),
    );
    // The notice claims the call was denied; say so only when the denial
    // reached the engine (U03-06). A failed send means the engine's approval
    // mailbox is closed, so nothing is left waiting on this decision.
    if let Err(error) = engine_handle
        .deny_tool_call_by(
            id.to_string(),
            crate::approval_log::ApprovalDecider::SessionRule,
        )
        .await
    {
        tracing::warn!(tool_name, %error, "session-rule denial did not reach the engine");
        return;
    }
    surface_session_denied_notice(app, tool_name);
}

pub(super) fn app_auto_approve_enabled(app: &App) -> bool {
    app.approval_mode == ApprovalMode::Bypass
}

/// Build the UI-side TurnAuthority for approval disposition (#4412).
///
/// Shell/trust bits do not affect disposition; mode + approval_mode + the
/// full-access shape (Bypass) are what the shared resolver consults.
fn app_turn_authority_for_approvals(app: &App) -> crate::core::authority::TurnAuthority {
    crate::core::authority::TurnAuthority::from_effective_fields(
        app.mode,
        true,
        false,
        app_auto_approve_enabled(app),
        app.approval_mode,
    )
}

/// Who answered an `AutoApprove` disposition: the posture when it allows the
/// call on its own, otherwise the remembered session rule that did.
pub(super) fn auto_approval_decider(
    app: &App,
    approval_force_prompt: bool,
) -> crate::approval_log::ApprovalDecider {
    use crate::core::authority::ApprovalRequestDisposition;
    match crate::core::authority::resolve_approval_request_disposition(
        &app_turn_authority_for_approvals(app),
        false,
        false,
        approval_force_prompt,
        false,
    ) {
        ApprovalRequestDisposition::AutoApprove => crate::approval_log::ApprovalDecider::Posture,
        _ => crate::approval_log::ApprovalDecider::SessionRule,
    }
}

pub(super) fn resolve_ui_approval_disposition(
    app: &App,
    tool_name: &str,
    grouping_key: &str,
    approval_key: &str,
    approval_force_prompt: bool,
) -> crate::core::authority::ApprovalRequestDisposition {
    let disposition = crate::core::authority::resolve_approval_request_disposition(
        &app_turn_authority_for_approvals(app),
        is_session_approved_for_tool(app, tool_name, grouping_key),
        is_session_denied_for_key(app, approval_key),
        approval_force_prompt,
        // This namespace is minted by the Engine, not by plugin card text.
        approval_key.starts_with("extcall:ext:"),
    );
    // Full Access holds a required prompt only because a programme with no
    // person at the card must not be stranded by one. This surface always has
    // that person, and a Computer Use consent or script exists for no other
    // reason than to carry their answer, so ask instead of denying.
    if disposition
        == crate::core::authority::ApprovalRequestDisposition::AutoDenyFullAccessPolicyHold
        && crate::tools::approval_cache::computer_use_human_decision_tool(tool_name)
    {
        return crate::core::authority::ApprovalRequestDisposition::Prompt;
    }
    disposition
}

/// Answer, explicitly, a request that must not open a card here, so nothing
/// waits on a card that never shows (approvals C1):
///
/// - a child agent's approval from another conversation (the agent is known
///   to belong elsewhere) is answered `unavailable`;
/// - while the parent is idle or its turn was cancelled locally, a request
///   the parent owns can only be stale: an approval or sandbox elevation is
///   answered `unavailable`, a question is cancelled. Neither is recorded
///   as the person's denial.
///
/// A child agent's request from this conversation is never stale on the
/// idle/cancel basis: the child is still running and waiting on the person,
/// so it falls through to the normal handler. Returns `true` when the event
/// was consumed here.
pub(super) async fn resolve_stale_parent_request(
    app: &App,
    engine_handle: &EngineHandle,
    event: &crate::core::events::Event,
) -> bool {
    use crate::core::events::Event;
    if let Event::ApprovalRequired { id, tool_name, .. } = event
        && crate::tui::pending_requests::is_foreign_child_request(app, id)
    {
        log_sensitive_event(
            "tool.approval.foreign_session_child_resolved",
            serde_json::json!({
                "tool_name": tool_name,
                "session_id": app.current_session_id,
            }),
        );
        let _ = engine_handle.deny_tool_call_unavailable(id.clone()).await;
        return true;
    }
    if !(app.suppress_stream_events_until_turn_complete || !app.is_loading) {
        return false;
    }
    match event {
        Event::ApprovalRequired { id, tool_name, .. }
            if !crate::tools::subagent::SubAgentManager::is_child_approval_id(id) =>
        {
            log_sensitive_event(
                "tool.approval.stale_parent_resolved",
                serde_json::json!({
                    "tool_name": tool_name,
                    "session_id": app.current_session_id,
                }),
            );
            let _ = engine_handle.deny_tool_call_unavailable(id.clone()).await;
            true
        }
        Event::ElevationRequired {
            tool_id, tool_name, ..
        } => {
            log_sensitive_event(
                "tool.sandbox.stale_elevation_resolved",
                serde_json::json!({
                    "tool_name": tool_name,
                    "session_id": app.current_session_id,
                }),
            );
            let _ = engine_handle
                .deny_tool_call_unavailable(tool_id.clone())
                .await;
            true
        }
        Event::UserInputRequired { id, .. }
            if !crate::tools::subagent::SubAgentManager::is_child_approval_id(id) =>
        {
            log_sensitive_event(
                "tool.user_input.stale_parent_resolved",
                serde_json::json!({
                    "tool_id": id,
                    "session_id": app.current_session_id,
                }),
            );
            let _ = engine_handle.cancel_user_input(id.clone()).await;
            true
        }
        _ => false,
    }
}

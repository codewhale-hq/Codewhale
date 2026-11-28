use crate::tui::app::App;

/// Reset the active conversation without choosing the next session id.
pub(crate) fn reset_conversation_state(app: &mut App) -> bool {
    // Work state is the only contended portion. Acquire and clear it first so
    // `/clear` and `/new` are all-or-nothing rather than losing conversation
    // state while leaving an old To-do attached to the next session.
    if !app.clear_todos() {
        return false;
    }
    // Explicit reset discards the queue it was invoked on. Capture its owner
    // before `/clear` or `/new` installs the next session id.
    if let Some(lease) = app.offline_queue_lease.clone() {
        crate::tui::persistence_actor::persist(
            crate::tui::persistence_actor::PersistRequest::ClearOfflineQueue { lease },
        );
    }
    // Atomically retire background accounting before zeroing the session.
    // Late reports retain the old scope token and are discarded instead of
    // appearing in the new conversation.
    let _settled_old_cost_scope = codewhale_engine::cost_status::close_current_scope();
    app.clear_history();
    app.mark_history_updated();
    app.clear_api_messages();
    app.system_prompt = None;
    app.goal = crate::tui::app::HostGoalState::default();
    app.last_known_goal_state = None;
    app.pending_goal_controls.clear();
    app.pending_goal_controls
        .push_back(crate::tui::app::PendingGoalControl {
            goal_id: None,
            intent: crate::tui::app::GoalControlIntent::SetStatus {
                status: codewhale_engine::tools::goal::GoalStatus::Active,
                clear: true,
            },
            dispatched: false,
        });
    app.goal_continuation_waiting = false;
    app.paused_goal_objective = None;
    app.paused = false;
    app.pausable = false;
    app.viewport.transcript_selection.clear();
    app.clear_visible_queued_messages();
    app.held_restored_queue = 0;
    app.queued_draft = None;
    app.session.total_tokens = 0;
    app.session.total_conversation_tokens = 0;
    app.last_billed_input_tokens = None;
    app.session.reset_token_breakdown();
    app.session.session_cost = 0.0;
    app.session.session_cost_cny = 0.0;
    app.session.subagent_cost = 0.0;
    app.session.subagent_cost_cny = 0.0;
    app.session.subagent_usage_sources.clear();
    app.session.displayed_cost_high_water = 0.0;
    app.session.displayed_cost_high_water_cny = 0.0;
    app.reset_cost_coverage();
    app.tool_log.clear();
    app.tool_cells.clear();
    app.tool_details_by_cell.clear();
    app.exploring_entries.clear();
    app.ignored_tool_calls.clear();
    app.pending_tool_uses.clear();
    app.last_exec_wait_command = None;
    app.session.last_prompt_tokens = None;
    app.session.last_completion_tokens = None;
    app.session.last_prompt_cache_hit_tokens = None;
    app.session.last_prompt_cache_miss_tokens = None;
    app.session.last_reasoning_replay_tokens = None;
    app.session.turn_cache_history.clear();
    app.session.last_cache_inspection = None;
    app.session.last_warmup_key = None;
    app.session.last_tool_catalog = None;
    app.session.last_base_url = None;
    // A fresh conversation inherits neither this one's denials nor its
    // "approve for session" grants (UX-8).
    app.approval_session_denied.clear();
    app.approval_session_approved.clear();
    app.auto_review_blocked_receipts.clear();
    app.pending_auto_review_retry = None;
    // Nor this one's child-agent approval cards, footer rows, or web-mirror
    // copies: the reset finalizes those children engine-side.
    crate::tui::pending_requests::clear_all(app);
    true
}

//! Session durability: snapshot/restore, recovery after a crash or stall,
//! and workspace/worktree switching.
//!
//! Moved verbatim out of `ui.rs`.

use super::*;

pub(crate) struct OfflineQueueTransition {
    lease: Arc<codewhale_engine::session_manager::OfflineQueueLease>,
    restored: Option<OfflineQueueState>,
}

/// A session load/resume failure must survive past the next footer update.
///
/// The status line is replaced almost immediately, which left a failed
/// resume looking like a silent new session — the screen even offered to
/// resume the id it had just created (#6138). Keep both: the transcript
/// error cell is the durable record, the status line the immediate one.
pub(crate) fn surface_session_load_failure(app: &mut App, message: String) {
    app.add_message(crate::tui::history::HistoryCell::Error {
        message: message.clone(),
        severity: codewhale_error_taxonomy::ErrorSeverity::Error,
    });
    app.status_message = Some(message);
}

/// Complete all fallible queue work before a session switch mutates the App.
/// A second editor must fail without touching either composer or queue file.
pub(crate) fn prepare_offline_queue_transition(
    app: &App,
    session_id: &str,
) -> Result<Option<OfflineQueueTransition>, String> {
    if app
        .offline_queue_lease
        .as_ref()
        .is_some_and(|lease| lease.session_id() == session_id)
    {
        return Ok(None);
    }
    let manager = SessionManager::default_location().map_err(|error| error.to_string())?;
    let lease = manager
        .acquire_offline_queue_lease(session_id)
        .map_err(|error| error.to_string())?;
    let restored = manager
        .load_offline_queue_state(session_id)
        .map_err(|error| {
            format!("Could not restore queued input for session {session_id}: {error}")
        })?;
    Ok(Some(OfflineQueueTransition { lease, restored }))
}

pub(crate) fn install_offline_queue_transition(
    app: &mut App,
    transition: Option<OfflineQueueTransition>,
) -> bool {
    let Some(transition) = transition else {
        return false;
    };
    // The request retains the old Arc until the actor finishes its write.
    // Acquiring the next lease does not release the previous editor early.
    persist_offline_queue_state(app);
    if app.queued_draft.take().is_some() {
        app.clear_input();
    }
    app.queued_messages.clear();
    app.held_restored_queue = 0;
    app.set_current_session_id(Some(transition.lease.session_id().to_string()));
    app.offline_queue_lease = Some(transition.lease);
    transition
        .restored
        .is_some_and(|state| restore_matching_offline_queue_state(app, state))
}

/// The editable composer is the durable draft. Keep `queued_draft` itself as
/// the original message so Escape can still cancel the edit in this window.
pub(crate) fn offline_queue_projection(
    app: &App,
) -> (VecDeque<QueuedMessage>, Option<QueuedMessage>) {
    let draft = app.queued_draft.as_ref().map(|original| {
        let mut edited = original.clone();
        edited.display.clone_from(&app.input);
        edited
    });
    (app.queued_messages.clone(), draft)
}

#[derive(Clone)]
pub(crate) struct SavedWorkProjection {
    pub(super) owner: Option<String>,
    pub(super) state: Option<codewhale_engine::session_manager::SessionWorkState>,
}

pub(crate) fn saved_work_projection(app: &App, session: &SavedSession) -> SavedWorkProjection {
    SavedWorkProjection {
        owner: app.current_session_id.clone(),
        state: session.work_state.clone(),
    }
}

pub(crate) async fn publish_saved_work_projection(
    app: &mut App,
    projection: &SavedWorkProjection,
) -> Result<bool, String> {
    let Some(work) = app.runtime_services.work.clone() else {
        return Ok(false);
    };
    let published = work
        .publish_pending_matching(
            projection.owner.as_deref(),
            projection
                .state
                .as_ref()
                .and_then(|state| state.graph.as_ref()),
        )
        .await?;
    if !published && work.has_pending_publish() {
        return Err("a newer Work update remains pending".to_string());
    }
    Ok(published)
}

#[cfg(test)]
pub(crate) async fn publish_pending_work_projection(app: &mut App) -> Result<bool, String> {
    let Some(work) = app.runtime_services.work.clone() else {
        return Ok(false);
    };
    work.publish_pending().await
}

pub(crate) async fn persist_pending_work_checkpoint(app: &mut App) -> Result<bool, String> {
    let Some(work) = app.runtime_services.work.clone() else {
        return Ok(false);
    };
    if !work.has_pending_publish() {
        return Ok(false);
    }
    let manager = SessionManager::default_location()
        .map_err(|err| format!("could not open sessions directory: {err}"))?;
    let session = build_session_snapshot(app, &manager)?;
    let projection = saved_work_projection(app, &session);
    if app.current_session_id.is_none() {
        app.set_current_session_id(Some(session.metadata.id.clone()));
    }
    if !persistence_actor::try_persist(PersistRequest::SaveCheckpoint { session }) {
        return Err("persistence actor is unavailable".to_string());
    }
    publish_saved_work_projection(app, &projection).await
}

pub(crate) fn persist_with_pending_work_boundary(
    app: &mut App,
    request: PersistRequest,
    projection: &SavedWorkProjection,
) -> Result<(), String> {
    let has_pending = app
        .runtime_services
        .work
        .as_ref()
        .is_some_and(|work| work.has_pending_publish());
    if !has_pending {
        return persistence_actor::persist_checked(request);
    }
    if !persistence_actor::try_persist(request) {
        return Err("persistence actor is unavailable".to_string());
    }
    app.publish_saved_work_state(projection.owner.as_deref(), projection.state.as_ref())
        .map(|_| ())
}

pub(crate) fn restore_matching_offline_queue_state(
    app: &mut App,
    state: OfflineQueueState,
) -> bool {
    if state.session_id.as_deref() != app.current_session_id.as_deref()
        || state.session_id.is_none()
    {
        return false;
    }
    app.queued_messages = state
        .messages
        .into_iter()
        .map(queued_session_to_ui)
        .collect();
    app.held_restored_queue = app.queued_messages.len();
    if let Some(draft) = state.draft.map(queued_session_to_ui) {
        app.input.clone_from(&draft.display);
        app.cursor_position = app.input.chars().count();
        app.active_skill.clone_from(&draft.skill_instruction);
        app.active_skill_provenance
            .clone_from(&draft.skill_provenance);
        app.queued_draft = Some(draft);
    } else {
        app.queued_draft = None;
    }
    app.needs_redraw = true;
    true
}

/// Running rows that never veto recovery: a prior-session row has no live
/// actor in this process, and a current child silent beyond its heartbeat
/// bound (at least `TOOL_HANG_WATCHDOG_TIMEOUT`) may have lost its terminal event (#6184).
/// Elapsed time alone does not establish that a current child is stalled.
pub(crate) fn suspect_running_agents(app: &App, now: Instant) -> Vec<String> {
    app.subagent_cache
        .iter()
        .filter(|agent| matches!(agent.status, SubAgentStatus::Running))
        .filter(|agent| {
            agent.from_prior_session
                || app
                    .agent_progress_meta
                    .get(&agent.agent_id)
                    .and_then(|meta| meta.last_progress_at)
                    .or(agent.started_at)
                    .is_some_and(|at| {
                        now.saturating_duration_since(at)
                            > TOOL_HANG_WATCHDOG_TIMEOUT.max(Duration::from_millis(
                                agent.heartbeat_timeout_ms.unwrap_or(0),
                            ))
                    })
        })
        .map(|agent| agent.agent_id.clone())
        .collect()
}

/// Running sub-agents that still legitimately hold the turn open.
pub(crate) fn live_running_agent_count(app: &App, now: Instant) -> usize {
    let suspects = suspect_running_agents(app, now);
    let mut ids: std::collections::HashSet<&str> =
        app.agent_progress.keys().map(String::as_str).collect();
    for agent in app
        .subagent_cache
        .iter()
        .filter(|agent| matches!(agent.status, SubAgentStatus::Running))
    {
        ids.insert(agent.agent_id.as_str());
    }
    ids.retain(|id| !suspects.iter().any(|suspect| suspect == id));
    ids.len()
}

/// Queued follow-ups the stalled turn is holding back, as a sentence suffix.
fn held_queue_note(app: &App) -> String {
    match app.queued_messages.len() {
        0 => String::new(),
        1 => " 1 queued message is held until the turn ends.".to_string(),
        n => format!(" {n} queued messages are held until the turn ends."),
    }
}

/// Log, record under `crashes/`, and name a stall the UI watchdog saw.
fn record_ui_stall(app: &App, phase: &str, since_progress: Duration, bound: Duration) {
    let suspects = suspect_running_agents(app, Instant::now());
    let detail = (!suspects.is_empty()).then(|| {
        format!(
            "sub-agent(s) from a prior session or with no progress for 10m, treated as suspect: {}",
            suspects.join(", ")
        )
    });
    codewhale_runtime::stall_report::report_stall(&codewhale_runtime::stall_report::StallReport {
        source: "ui",
        phase: phase.to_string(),
        detail,
        turn_id: app.runtime_turn_id.clone(),
        provider_request: app
            .active_turn
            .as_ref()
            .and_then(|turn| turn.route.as_ref())
            .map(|route| format!("{} / {}", route.provider_identity, route.model)),
        since_progress,
        bound: Some(bound),
    });
}

/// The UI watchdog, supervised by the engine heartbeat (#6184). Suspect
/// sub-agents no longer veto recovery, and an engine-reported stall is shown
/// with the phase it stalled in.
///
/// Recovering a turn that had started is all-or-nothing (#6800): the engine's
/// turn is cancelled with the UI's, through the cancellation the cancel key
/// issues. Clearing only the UI left the engine owning the turn, so the next
/// message was refused and no outcome was ever recorded. The engine's
/// terminal event for that turn then arrives as it does after a local cancel.
///
/// Known limitation: a turn wedged somewhere that does not observe
/// cancellation still holds the engine; this only removes the disagreement.
pub(crate) fn reconcile_turn_liveness_supervised(
    app: &mut App,
    now: Instant,
    heartbeat: &codewhale_engine::core::engine::turn_heartbeat::HeartbeatSnapshot,
    engine: &EngineHandle,
) -> bool {
    let turn_in_progress = matches!(app.runtime_turn_status.as_deref(), Some("in_progress"));
    if (app.is_loading || matches!(app.runtime_turn_status.as_deref(), Some("in_progress")))
        && let Some(stall) = heartbeat.stall.as_ref()
    {
        // Coalesced by text while visible, so one toast per stall episode.
        let text = format!("{}{}", stall.status_line(), held_queue_note(app));
        app.push_status_toast(text, StatusToastLevel::Error, None);
    }
    let has_live_agents = live_running_agent_count(app, now) > 0;
    let recovered = reconcile_turn_liveness_with(app, now, has_live_agents, Some(heartbeat));
    if recovered && turn_in_progress && app.runtime_turn_status.is_none() {
        engine.cancel_with_reason(codewhale_engine::core::engine::CancelReason::Stalled);
        app.suppress_stream_events_until_turn_complete = true;
    }
    recovered
}

/// Unsupervised form (no engine heartbeat), kept for focused tests.
#[cfg(test)]
pub(crate) fn reconcile_turn_liveness(
    app: &mut App,
    now: Instant,
    has_running_agents: bool,
) -> bool {
    reconcile_turn_liveness_with(app, now, has_running_agents, None)
}

pub(crate) fn reconcile_turn_liveness_with(
    app: &mut App,
    now: Instant,
    has_running_agents: bool,
    heartbeat: Option<&codewhale_engine::core::engine::turn_heartbeat::HeartbeatSnapshot>,
) -> bool {
    // The engine is inside a wait it bounds itself and has not reported as
    // overdue (a quiet model, a live stream). Its watchdog owns that bound;
    // the UI does not second-guess it with a timer of its own.
    let engine_owns_wait = heartbeat.is_some_and(|snapshot| snapshot.engine_owns_live_wait());
    // #6872: human decisions can wait indefinitely. Their configured timeout,
    // answer or withdrawal owns the wait, including buried or hidden cards.
    let awaiting_human_decision = app.pending_user_input_prompt.is_some()
        || app.view_stack.contains_kind(ModalKind::Approval)
        || app.view_stack.contains_kind(ModalKind::Elevation)
        || app
            .pending_child_requests
            .keys()
            .any(|id| !crate::tui::pending_requests::is_foreign_child_request(app, id));
    if app.is_loading
        && app.runtime_turn_status.is_none()
        && !has_running_agents
        && !app.is_compacting
        && !app.is_purging
        && app.dispatch_started_at.is_some_and(|started| {
            now.saturating_duration_since(started) > DISPATCH_WATCHDOG_TIMEOUT
        })
    {
        if let Some(started) = app.dispatch_started_at {
            record_ui_stall(
                app,
                "while dispatching the message to the engine",
                now.saturating_duration_since(started),
                DISPATCH_WATCHDOG_TIMEOUT,
            );
        }
        // #2739: the user's prompt was already appended to api_messages
        // before dispatch, but the turn never reached `in_progress`. Persist
        // it before clearing turn state so `--continue` keeps the prompt
        // instead of loading the previous save.
        persist_recovery_snapshot(app);
        settle_pending_human_requests(app);
        app.is_loading = false;
        app.dispatch_started_at = None;
        app.turn_started_at = None;
        app.turn_last_activity_at = None;
        app.pending_turn_route = None;
        app.pending_auto_route_receipt = None;
        app.active_turn = None;
        app.suppress_stream_events_until_turn_complete = false;
        app.push_status_toast(
            "Turn dispatch timed out; the engine may have stopped. Please try again.",
            StatusToastLevel::Error,
            None,
        );
        return true;
    }

    if app.is_loading
        && matches!(
            app.runtime_turn_status.as_deref(),
            Some("completed" | "interrupted" | "failed")
        )
        && !has_running_agents
        && !app.is_compacting
        && !app.is_purging
    {
        settle_pending_human_requests(app);
        app.is_loading = false;
        app.dispatch_started_at = None;
        app.turn_started_at = None;
        app.turn_last_activity_at = None;
        app.pending_turn_route = None;
        app.pending_auto_route_receipt = None;
        app.active_turn = None;
        app.suppress_stream_events_until_turn_complete = false;
        app.push_status_toast(
            "Recovered from an inconsistent busy state.",
            StatusToastLevel::Warning,
            None,
        );
        return true;
    }

    // Branch 3: turn started but never completed — engine may have
    // panicked, sub-agent may be stuck, or the completion event was lost.
    if app.is_loading
        && matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
        && !has_running_agents
        && !engine_owns_wait
        && !awaiting_human_decision
        && !app.is_compacting
        && !active_turn_has_running_tool(app)
        && let Some(last_activity) = app.turn_last_activity_at.or(app.turn_started_at)
        && now.saturating_duration_since(last_activity) > turn_stall_watchdog_timeout(app)
    {
        record_ui_stall(
            app,
            "waiting for the turn's completion signal",
            now.saturating_duration_since(last_activity),
            turn_stall_watchdog_timeout(app),
        );
        recover_stalled_runtime_turn(
            app,
            "Turn stalled — no completion signal received. Please try again.",
            StatusToastLevel::Error,
        );
        return true;
    }

    if app.is_loading
        && matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
        && !has_running_agents
        && !awaiting_human_decision
        && !app.is_compacting
        && !app.is_purging
        && active_turn_has_running_tool(app)
        && let Some(last_activity) = app.turn_last_activity_at.or(app.turn_started_at)
        && now.saturating_duration_since(last_activity) > TOOL_HANG_WATCHDOG_TIMEOUT
    {
        record_ui_stall(
            app,
            "while a tool ran with no progress",
            now.saturating_duration_since(last_activity),
            TOOL_HANG_WATCHDOG_TIMEOUT,
        );
        recover_stalled_runtime_turn(
            app,
            "Tool stalled with no progress for 10m — recovered; the command may still be running in the background. Run /jobs to see it and /jobs cancel <id> to stop it.",
            StatusToastLevel::Error,
        );
        return true;
    }

    false
}

/// #2739: persist the current in-memory session state before a recovery or
/// cancellation path clears turn bookkeeping. Without this snapshot, the
/// just-finalised partial turn lives only in `app.api_messages` and is never
/// written to disk, so `--continue` loads the *previous* save — effectively
/// losing the entire in-progress turn.
pub(crate) fn persist_recovery_snapshot(app: &mut App) {
    if let Ok(manager) = SessionManager::default_location()
        && let Ok(mut session) = build_session_snapshot(app, &manager)
    {
        if let Some(turn) = app.active_turn.as_ref()
            && app.runtime_turn_id.as_deref() == Some(turn.turn_id.as_str())
            && !app.suppress_stream_events_until_turn_complete
            && app
                .api_messages
                .last()
                .is_some_and(|message| message.role == codewhale_models::Role::User)
            && let Some(HistoryCell::Assistant {
                content,
                streaming: true,
            }) = app
                .streaming_message_index
                .and_then(|index| app.history.get(index))
            && !content.is_empty()
            && let Some(journal) = session.journal.as_mut()
        {
            session.leaf_id = Some(journal.append_message(Message::interrupted_assistant(content)));
            session.metadata.message_count += 1;
        }
        let projection = saved_work_projection(app, &session);
        if app.current_session_id.is_none() {
            app.set_current_session_id(Some(session.metadata.id.clone()));
        }
        if let Err(err) = persist_with_pending_work_boundary(
            app,
            PersistRequest::SaveCheckpoint { session },
            &projection,
        ) {
            app.status_message = Some(format!(
                "To-do list update pending while saving recovery snapshot ({err})"
            ));
        }
    }
}

pub(crate) fn persist_full_reset_snapshot(app: &mut App) {
    let session = SessionManager::default_location()
        .map_err(|err| err.to_string())
        .and_then(|manager| build_session_snapshot(app, &manager));
    let session = match session {
        Ok(session) => session,
        Err(err) => {
            let id = app
                .current_session_id
                .as_deref()
                .map(codewhale_engine::session_manager::truncate_id)
                .unwrap_or("—");
            let message = app
                .tr(MessageId::SessionSaveFailed)
                .replace("{id}", id)
                .replace("{error}", &err);
            app.add_message(crate::tui::history::HistoryCell::Error {
                message: message.clone(),
                severity: codewhale_error_taxonomy::ErrorSeverity::Error,
            });
            app.status_message = Some(message);
            return;
        }
    };
    let projection = saved_work_projection(app, &session);
    app.set_current_session_id(Some(session.metadata.id.clone()));
    if let Err(err) = persist_with_pending_work_boundary(
        app,
        PersistRequest::CompletedCommit { session },
        &projection,
    ) {
        app.status_message = Some(format!(
            "To-do list update pending while saving reset snapshot ({err})"
        ));
    }
}

pub(crate) fn maybe_throttled_recovery_snapshot(
    app: &mut App,
    now: Instant,
    last_snapshot_at: &mut Option<Instant>,
) {
    if !app.is_loading && !matches!(app.runtime_turn_status.as_deref(), Some("in_progress")) {
        return;
    }
    if last_snapshot_at
        .is_some_and(|last| now.saturating_duration_since(last) < RECOVERY_SNAPSHOT_INTERVAL)
    {
        return;
    }
    persist_recovery_snapshot(app);
    *last_snapshot_at = Some(now);
}

pub(crate) fn recover_stalled_runtime_turn(app: &mut App, message: &str, level: StatusToastLevel) {
    settle_pending_human_requests(app);
    // Capture the turn identity before the reset below clears it; the
    // outbox event must name the turn that stalled.
    let stalled_turn_id = app.runtime_turn_id.clone();
    let stalled_session_id = app.hooks.session_id().to_string();
    // Finalize in-flight thinking / assistant / tool cells so the
    // transcript doesn't show permanent spinners after recovery.
    streaming_thinking::finalize_current(app);
    app.finalize_streaming_assistant_as_interrupted();
    app.finalize_active_cell_as_interrupted();
    app.streaming_state.reset();
    app.streaming_message_index = None;
    app.streaming_thinking_active_entry = None;

    // #2739: persist the partial turn's api_messages before clearing
    // turn state. Without this snapshot the stalled/cancelled turn's
    // messages are held only in memory and --continue sees the
    // *previous* save, losing the entire in-progress turn.
    persist_recovery_snapshot(app);

    app.is_loading = false;
    // #6800: fail an unadmitted dispatch back now instead of after its bound.
    app.cancel_in_flight_dispatch();
    app.turn_started_at = None;
    app.turn_last_activity_at = None;
    app.runtime_turn_status = None;
    app.runtime_turn_id = None;
    app.dispatch_started_at = None;
    app.pending_turn_route = None;
    app.pending_auto_route_receipt = None;
    app.active_turn = None;
    app.suppress_stream_events_until_turn_complete = false;
    // Per-turn scroll lock — clear so the next turn auto-scrolls.
    app.user_scrolled_during_stream = false;
    // #6184: queued follow-ups drain only on a TurnComplete this recovered
    // turn will never send. Hand the latest one back to the composer so one
    // Enter resends it (the rest drain after that turn), and say so.
    let held = app.queued_messages.len();
    let message = if held > 0 && app.pop_last_queued_into_draft() {
        let rest = held - 1;
        let tail = if rest == 0 {
            String::new()
        } else {
            format!(" {rest} more queued message(s) send after it.")
        };
        format!(
            "{message} Your queued message is back in the composer — press Enter to resend it.{tail}"
        )
    } else {
        format!("{message}{}", held_queue_note(app))
    };
    let message = message.as_str();
    app.push_status_toast(message, level, None);
    // Lifecycle outbox (`[lifecycle_outbox]`): the first scriptable stall
    // signal. Until now a wedged turn was only visible as this toast; with
    // the outbox enabled a supervisor can react to the same moment.
    // No-op when the feature is disabled.
    app.lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
        event: "turn_stalled".to_string(),
        kind: "turn.stalled".to_string(),
        thread_id: stalled_session_id,
        turn_id: stalled_turn_id,
        item_id: None,
        payload: serde_json::json!({
            "message": codewhale_hooks::bounded_text(
                message,
                codewhale_hooks::OUTBOX_DETAIL_MAX_CHARS,
            ),
            "workspace": app.workspace.display().to_string(),
        }),
    });
}

pub(crate) fn recover_engine_event_disconnect(app: &mut App) -> bool {
    let had_live_work = app.is_loading
        || app.is_compacting
        || app.manual_compaction_queued
        || app.is_purging
        || matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
        || app.pending_turn_route.is_some()
        || app.pending_user_input_prompt.is_some()
        || app.active_turn.is_some()
        || app.suppress_stream_events_until_turn_complete
        || app.streaming_message_index.is_some()
        || app.streaming_thinking_active_entry.is_some()
        || app
            .active_cell
            .as_ref()
            .is_some_and(|cell| !cell.is_empty());

    if !had_live_work {
        return false;
    }

    settle_pending_human_requests(app);

    streaming_thinking::finalize_current(app);
    app.finalize_streaming_assistant_as_interrupted();
    app.finalize_active_cell_as_interrupted();
    app.streaming_state.reset();
    app.streaming_message_index = None;
    app.streaming_thinking_active_entry = None;

    // #2739: persist partial turn before clearing state.
    persist_recovery_snapshot(app);

    app.is_loading = false;
    app.is_compacting = false;
    app.active_compaction = None;
    app.manual_compaction_queued = false;
    app.deferred_manual_compaction = None;
    app.is_purging = false;
    app.turn_started_at = None;
    app.turn_last_activity_at = None;
    app.runtime_turn_status = None;
    app.runtime_turn_id = None;
    app.dispatch_started_at = None;
    app.pending_turn_route = None;
    app.pending_auto_route_receipt = None;
    app.active_turn = None;
    app.suppress_stream_events_until_turn_complete = false;
    app.user_scrolled_during_stream = false;

    for msg in app.drain_pending_steers() {
        app.queue_message(msg);
    }

    app.add_message(HistoryCell::Error {
        message: "Engine stopped before completing the turn. Check ~/.codewhale/crashes and retry."
            .to_string(),
        severity: codewhale_error_taxonomy::ErrorSeverity::Error,
    });
    app.push_status_toast(
        "Engine stopped before completing the turn.",
        StatusToastLevel::Error,
        None,
    );
    true
}

pub(crate) fn capture_turn_started_metadata(app: &mut App, event: &EngineEvent) {
    match event {
        EngineEvent::TurnStarted {
            turn_id,
            created_at,
            route,
            submission_id: _,
        } => {
            app.ocean_completion_started_at = None;
            let auto_route_receipt = if route.as_ref().is_some_and(|route| route.auto_model) {
                app.pending_auto_route_receipt.take()
            } else if route.is_some() {
                app.pending_auto_route_receipt = None;
                None
            } else {
                None
            };
            // Bind the prompt-suggestion authority to the receipt the engine minted
            // from the client it installed for this turn. Deliberately not read
            // from `config`: web config events are drained ahead of engine events,
            // so config here may already describe a different key or endpoint than
            // the one this turn is actually running on.
            let suggestion_authority = route
                .as_ref()
                .and_then(crate::tui::prompt_suggestion::capture_route_authority);
            app.active_turn = Some(ActiveTurnMetadata {
                turn_id: turn_id.clone(),
                created_at: *created_at,
                route: route.clone(),
                auto_route_receipt,
                suggestion_authority,
            });
            app.pending_turn_route = None;
        }
        // The dispatch boundary is the billing truth: refresh the active turn's
        // route with the envelope that was actually put on the wire. Receipts
        // already taken at `TurnStarted` are preserved — this event narrows the
        // route, it never re-opens an authority decision.
        EngineEvent::RouteDispatched { turn_id, route } => {
            if let Some(active) = app
                .active_turn
                .as_mut()
                .filter(|active| active.turn_id == *turn_id)
            {
                if route.auto_model && active.auto_route_receipt.is_none() {
                    active.auto_route_receipt = app.pending_auto_route_receipt.take();
                } else if !route.auto_model {
                    app.pending_auto_route_receipt = None;
                    active.auto_route_receipt = None;
                }
                if active.suggestion_authority.is_none() {
                    active.suggestion_authority =
                        crate::tui::prompt_suggestion::capture_route_authority(route);
                }
                active.route = Some(route.clone());
            }
        }
        _ => {}
    }
}

pub(crate) fn record_turn_activity(app: &mut App, event: &EngineEvent, now: Instant) {
    if matches!(event, EngineEvent::TurnStarted { .. }) {
        app.turn_last_activity_at = Some(now);
        return;
    }

    if app.is_loading || matches!(app.runtime_turn_status.as_deref(), Some("in_progress")) {
        app.turn_last_activity_at = Some(now);
    }
}

pub(crate) fn persist_offline_queue_state(app: &App) {
    let Some(lease) = app
        .offline_queue_lease
        .as_ref()
        .filter(|lease| app.current_session_id.as_deref() == Some(lease.session_id()))
    else {
        let lease_owner = app
            .offline_queue_lease
            .as_ref()
            .map(|lease| lease.session_id());
        if lease_owner.is_some() || !app.queued_messages.is_empty() || app.queued_draft.is_some() {
            tracing::warn!(
                target: "persistence",
                what = "offline-queue",
                session_id = app.current_session_id.as_deref().unwrap_or("none"),
                lease_owner = lease_owner.unwrap_or("none"),
                queued = app.queued_messages.len(),
                draft = app.queued_draft.is_some(),
                "offline queue not persisted: this window holds no queue lease for the current session",
            );
        }
        return;
    };
    if app.queued_messages.is_empty() && app.queued_draft.is_none() {
        persistence_actor::persist(PersistRequest::ClearOfflineQueue {
            lease: Arc::clone(lease),
        });
        return;
    }
    let (messages, draft) = offline_queue_projection(app);
    let converted = messages
        .iter()
        .map(queued_ui_to_session)
        .collect::<Result<Vec<_>>>();
    let draft = draft.as_ref().map(queued_ui_to_session).transpose();
    let (messages, draft) = match converted
        .and_then(|messages| draft.map(|draft| (messages, draft)))
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(target: "persistence", %error, "offline queue serialization refused");
            return;
        }
    };
    let state = OfflineQueueState {
        messages,
        draft,
        ..OfflineQueueState::default()
    };
    persistence_actor::persist(PersistRequest::OfflineQueue {
        state,
        lease: Arc::clone(lease),
    });
}

pub(crate) fn restore_queued_message(app: &mut App, index: Option<usize>, message: QueuedMessage) {
    if message.extension_command.is_some() {
        app.restore_unsent_message(message);
        return;
    }
    if let Some(index) = index
        && index <= app.queued_messages.len()
    {
        if index < app.held_restored_queue {
            app.held_restored_queue += 1;
        }
        app.queued_messages.insert(index, message);
    } else {
        app.queue_message(message);
    }
}

pub(crate) fn restore_queued_or_draft_message(
    app: &mut App,
    recovery: DispatchRecovery,
    message: QueuedMessage,
) {
    if message.extension_command.is_some() {
        app.restore_unsent_message(message);
        return;
    }
    match recovery {
        DispatchRecovery::Draft => {
            app.input.clone_from(&message.display);
            app.cursor_position = app.input.chars().count();
            app.active_skill = message.skill_instruction.clone();
            app.active_skill_provenance = message.skill_provenance.clone();
            app.queued_draft = Some(message);
            app.needs_redraw = true;
        }
        DispatchRecovery::Queued { restore_index } => {
            restore_queued_message(app, restore_index, message);
        }
        DispatchRecovery::Immediate | DispatchRecovery::Initial => app.queue_message(message),
    }
}

pub(crate) fn recover_unstarted_external_message(
    app: &mut App,
    message: QueuedMessage,
    recovery: DispatchRecovery,
    error: &str,
) {
    app.dispatch_in_flight = false;
    match recovery {
        DispatchRecovery::Immediate | DispatchRecovery::Initial => {
            restore_failed_immediate_submit(app, message, &anyhow::Error::msg(error.to_string()));
        }
        DispatchRecovery::Draft => {
            restore_queued_or_draft_message(app, recovery, message);
            app.status_message = Some(format!("{error}; queued draft restored"));
        }
        DispatchRecovery::Queued { restore_index } => {
            restore_queued_message(app, restore_index, message);
            app.status_message = Some(format!(
                "{error}; {} queued follow-up(s) restored",
                app.queued_message_count()
            ));
        }
    }
    app.push_status_toast(
        error.to_string(),
        StatusToastLevel::Error,
        Some(App::STICKY_ERROR_TTL_MS),
    );
    app.needs_redraw = true;
}

pub(crate) fn restore_message_submit_denial(
    app: &mut App,
    message: QueuedMessage,
    recovery: DispatchRecovery,
) {
    let denial = app
        .status_message
        .clone()
        .unwrap_or_else(|| "message_submit hook blocked submission".to_string());
    app.dispatch_in_flight = false;
    match recovery {
        DispatchRecovery::Immediate | DispatchRecovery::Initial => {
            app.restore_unsent_message(message);
        }
        DispatchRecovery::Draft => {
            restore_queued_or_draft_message(app, recovery, message);
        }
        DispatchRecovery::Queued { restore_index } => {
            restore_queued_message(app, restore_index, message);
        }
    }
    app.status_message = Some(denial.clone());
    app.push_status_toast(denial, StatusToastLevel::Warning, Some(6_000));
    app.needs_redraw = true;
}

/// Resume one recent-work row from the startup card by session id. Mirrors
/// `/resume <id>`: the card dissolves and the saved session loads through
/// the normal `LoadSession` path; a session that vanished behind the card
/// leaves the card up with a status saying why instead of stranding the
/// user on an empty stage.
pub(crate) fn resume_launch_session(app: &mut App, session_id: &str) -> commands::CommandResult {
    let failed = |app: &mut App, err: &str| {
        app.launch.status = Some(
            app.tr(MessageId::LaunchResumeFailed)
                .replace("{error}", err),
        );
        commands::CommandResult::ok()
    };
    let manager = match codewhale_engine::session_manager::SessionManager::default_location() {
        Ok(manager) => manager,
        Err(err) => return failed(app, &err.to_string()),
    };
    let saved = match manager.load_session_snapshot(session_id) {
        Ok(saved) => saved,
        Err(err) => return failed(app, &err.to_string()),
    };
    let path = manager
        .sessions_dir()
        .join(format!("{}.json", saved.metadata.id));
    if !path.exists() {
        return failed(app, "saved session file is gone");
    }
    app.launch.dissolve_card(app.ambient_clock_ms);
    commands::CommandResult::action(AppAction::LoadSession(path))
}

/// `LaunchAction::McpRemedy` (#6085): type the remedy the problems row
/// prints into the composer — `/mcp login <name>` or `/mcp`. Typing beats
/// copying (no clipboard dependency over SSH), and the user reads the
/// command before a second Enter sends it.
pub(crate) fn type_launch_mcp_remedy(app: &mut App) {
    let Some(command) = crate::tui::underwater::mcp_remedy_command(app) else {
        return;
    };
    // Home can be revisited with an unsent draft. The manager exposes the
    // same remedy without replacing user-authored composer content.
    if !app.input.is_empty() {
        app.launch.dissolve_card(app.ambient_clock_ms);
        open_mcp_extensions(app);
        return;
    }
    app.input = command;
    app.cursor_position = app.input.chars().count();
    app.launch.menu_selected = None;
    app.launch.status = None;
}

pub(crate) fn begin_launch_session(
    app: &mut App,
    workspace: Option<PathBuf>,
) -> commands::CommandResult {
    let session_id = uuid::Uuid::new_v4().to_string();
    let transition = match prepare_offline_queue_transition(app, &session_id) {
        Ok(transition) => transition,
        Err(error) => return commands::CommandResult::error(error),
    };
    install_offline_queue_transition(app, transition);
    if let Some(workspace) = workspace {
        app.workspace = workspace;
    }
    app.set_current_session_id(Some(session_id.clone()));
    app.current_session_metadata = None;
    app.window_title = None;
    app.composer_color = None;
    app.session_title = Some(app.tr(MessageId::SessionsNewSessionTitle).into_owned());
    app.launch.dismiss();
    app.launch.status = None;
    app.status_message = None;
    commands::CommandResult::action(AppAction::SyncSession {
        session_id: Some(session_id),
        messages: Vec::new(),
        system_prompt: None,
        model: app.model.clone(),
        workspace: app.workspace.clone(),
        mode: app.mode,
    })
}

pub(crate) async fn sync_runtime_workspace_state(
    task_manager: &SharedTaskManager,
    workspace: PathBuf,
) {
    task_manager.set_default_workspace(workspace).await;
}

pub(crate) async fn switch_workspace(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    task_manager: &SharedTaskManager,
    config: &Config,
    workspace: PathBuf,
) {
    if app.is_loading {
        let busy = app.tr(MessageId::WorkspaceSwitchBusy).into_owned();
        app.status_message = Some(busy.clone());
        app.add_message(HistoryCell::System { content: busy });
        return;
    }

    let shown = workspace.display().to_string();
    if app.workspace == workspace {
        app.status_message = Some(
            app.tr(MessageId::WorkspaceUnchanged)
                .replace("{path}", &shown),
        );
        return;
    }

    apply_workspace_runtime_state(app, config, workspace.clone());
    sync_runtime_workspace_state(task_manager, workspace.clone()).await;

    replace_tui_engine(app, config, engine_handle).await;
    if !app.api_messages.is_empty() {
        let _ = engine_handle
            .send(Op::SyncSession {
                session_id: app.current_session_id.clone(),
                messages: app.api_messages.as_ref().clone(),
                system_prompt: app.system_prompt.clone(),
                system_prompt_override: false,
                model: app.model.clone(),
                workspace: workspace.clone(),
                mode: app.mode,
            })
            .await;
    }

    let switched = app
        .tr(MessageId::WorkspaceSwitched)
        .replace("{path}", &shown);
    app.add_message(HistoryCell::System { content: switched });
    app.status_message = Some(app.tr(MessageId::WorkspaceStatus).replace("{path}", &shown));
}

/// A message submitted with no usable key (#6566). Nothing reached a model:
/// the caller has already rolled back the optimistic echo, so the text goes
/// back into the composer — not lost, and sent once when the person presses
/// Enter after connecting, not doubled. One transcript line says what
/// happened and the provider picker opens.
///
/// A new user never chose a provider, so the line does not name the built-in
/// default's key or print its help page. A returning user whose saved route
/// lost its key also gets the one command that saves it.
pub(crate) fn keep_unsent_message_for_connect(app: &mut App, message: QueuedMessage, error: &str) {
    if message.extension_command.is_some() {
        app.restore_unsent_message(message);
        app.status_message = Some(error.to_string());
        return;
    }
    tracing::warn!(
        error = %error,
        "user message not sent: no usable credential; restored to composer"
    );
    app.restore_unsent_message(message);

    let new_user = app.onboarding_had_provider_step;
    let mut content = app.tr(MessageId::DispatchNotSentNoModel).into_owned();
    if !new_user
        && let Some(save) = error
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("codewhale auth set"))
    {
        content.push('\n');
        content.push_str(
            &app.tr(MessageId::DispatchNotSentSaveKey)
                .replace("{command}", save),
        );
    }
    app.add_message(HistoryCell::System { content });
    app.onboarding_needs_api_key = true;
    // From the composer, the saved route is the one missing its key: this is
    // missing-key recovery, as after `/logout`, so Esc returns to the
    // composer. A first-run launch with an initial prompt is still in
    // onboarding and keeps its remaining steps (Esc walks back as before).
    if app.onboarding == OnboardingState::None {
        app.onboarding_missing_key_recovery = true;
        app.onboarding_provider = app.api_provider;
    }
    app.onboarding = OnboardingState::Provider;
    // The footer keeps the provider's full message for a returning user; a
    // new user's footer says only that no model is connected.
    let reason = if new_user {
        app.tr(MessageId::LaunchNoModelConnected).into_owned()
    } else {
        error.to_string()
    };
    let status = app
        .tr(MessageId::DispatchNotSentStatus)
        .replace("{reason}", &reason);
    app.status_message = Some(status.clone());
    app.set_sticky_status(
        status,
        StatusToastLevel::Error,
        Some(App::STICKY_ERROR_TTL_MS),
    );
    app.needs_redraw = true;
}

/// The engine reported this turn's message as never sent: a key rejected
/// before any model output (#6566). Take the message back, and its bubble
/// out of the live transcript when nothing has landed after it, so sending it
/// again shows it once. `None` when no dispatched message is on record.
pub(crate) fn take_back_unsent_submission(app: &mut App) -> Option<QueuedMessage> {
    let submission = app.unanswered_submission.take()?;
    let cell = submission.history_cell;
    let bubble_is_last = cell + 1 == app.history.len()
        && matches!(
            &app.history[cell],
            HistoryCell::User { content } if content == &submission.message.display
        );
    if bubble_is_last {
        app.truncate_history_to(cell);
    }
    Some(submission.message)
}

pub(crate) fn restore_failed_immediate_submit(
    app: &mut App,
    message: QueuedMessage,
    error: &anyhow::Error,
) {
    if message.extension_command.is_some() {
        app.restore_unsent_message(message);
        app.status_message = Some(error.to_string());
        return;
    }
    tracing::warn!(
        error = %error,
        "immediate user message dispatch failed; restored composer"
    );
    app.input = message.display;
    app.cursor_position = app.input.chars().count();
    app.active_skill = message.skill_instruction;
    app.active_skill_provenance = message.skill_provenance;
    let status = tr(app.ui_locale, MessageId::ComposerDispatchFailedRestored)
        .replace("{error}", &error.to_string());
    app.status_message = Some(status.clone());
    app.set_sticky_status(
        status,
        StatusToastLevel::Error,
        Some(App::STICKY_ERROR_TTL_MS),
    );
    app.needs_redraw = true;
}

/// Show the default recommended Hotbar slots. Since #3807 an absent `hotbar`
/// key means "hidden", so `/hotbar on` persists the explicit default bindings
/// rather than deleting the key. This is an explicit reset, so any custom
/// bindings are replaced with the recommended set.
pub(crate) fn restore_hotbar_defaults(app: &mut App, config: &mut Config) {
    let defaults = codewhale_config::default_hotbar_bindings_toml();
    match codewhale_runtime::config_persistence::persist_hotbar_bindings(
        app.config_path.as_deref(),
        &defaults,
    ) {
        Ok(path) => {
            config.hotbar = Some(defaults);
            app.status_message = Some(format!(
                "Hotbar enabled with the default slots ({}). Customize with `/hotbar`.",
                path.display()
            ));
        }
        Err(err) => {
            app.status_message = Some(format!("Failed to enable the Hotbar: {err}"));
            app.add_message(HistoryCell::System {
                content: format!("Failed to enable the Hotbar: {err}"),
            });
        }
    }
    app.needs_redraw = true;
}

pub(crate) fn persist_rules_from_approval(
    app: &mut App,
    config: &mut Config,
    rules: &[codewhale_config::ToolAskRule],
) {
    let action = rules.first().map(|rule| rule.action);
    match codewhale_config::ConfigStore::load(app.config_path.clone()).and_then(|mut store| {
        let added = match action {
            Some(codewhale_execpolicy::PermissionAction::Ask) => store.append_ask_rules(rules)?,
            Some(codewhale_execpolicy::PermissionAction::Allow) => {
                store.append_allow_rules(rules)?
            }
            Some(codewhale_execpolicy::PermissionAction::Deny) => {
                anyhow::bail!("the approval UI cannot persist deny rules")
            }
            None => 0,
        };
        let permissions_path = store.permissions_path();
        config
            .exec_policy_engine
            .set_ruleset(store.permissions().ruleset());
        Ok((added, permissions_path))
    }) {
        Ok((added, path)) if added > 0 => {
            let action = match action {
                Some(codewhale_execpolicy::PermissionAction::Allow) => "allow",
                _ => "ask",
            };
            app.status_message = Some(format!(
                "Saved {added} {action} permission rule(s) to {}",
                path.display()
            ));
        }
        Ok((_added, path)) => {
            let action = match action {
                Some(codewhale_execpolicy::PermissionAction::Allow) => "Allow",
                _ => "Ask",
            };
            app.status_message = Some(format!(
                "{action} permission rule already saved in {}",
                path.display()
            ));
        }
        Err(err) => {
            app.status_message = Some(format!("Failed to save permission rule: {err:#}"));
        }
    }
}

pub(crate) fn mirror_saved_model_in_config(
    config: &mut Config,
    identity: &ProviderIdentity,
    model: String,
) -> Result<(), String> {
    config.verify_provider_identity(identity)?;
    if identity.provider == ProviderKind::Deepseek {
        config.default_text_model = Some(model);
        return Ok(());
    }
    config
        .set_provider_model_override(identity, Some(model))
        .map_err(|error| error.to_string())
}

pub(crate) fn mirror_saved_context_window_in_config(
    config: &mut Config,
    identity: &ProviderIdentity,
    context_window: u32,
) -> Result<(), String> {
    config.verify_provider_identity(identity)?;
    if identity.provider == ProviderKind::Moonshot {
        config
            .provider_config_for_mut(identity)
            .map_err(|error| error.to_string())?
            .context_window = Some(context_window);
    }
    Ok(())
}

pub(crate) fn mirror_saved_api_key_in_config(
    config: &mut Config,
    identity: &ProviderIdentity,
    api_key: String,
) -> Result<(), String> {
    config.verify_provider_identity(identity)?;
    let provider = identity.provider;
    // These shared auth leaves are intrinsic released credential contracts;
    // presentation names cannot select them.
    if provider == ProviderKind::Deepseek {
        let auth_owner = config.builtin_provider_identity(ProviderKind::Deepseek)?;
        config
            .set_provider_api_key_override(&auth_owner, Some(api_key))
            .map_err(|error| error.to_string())?;
        config.auth_mode = Some("api_key".to_string());
        return Ok(());
    }
    let pin_kimi_code_base_url = provider == ProviderKind::Moonshot
        && config.provider_config_for(identity).is_some_and(|entry| {
            codewhale_runtime::config::provider_config_uses_kimi_imported_token(entry)
                && entry
                    .base_url
                    .as_deref()
                    .is_none_or(|base_url| base_url.trim().is_empty())
        });
    let auth_owner = if provider == ProviderKind::SiliconflowCN {
        config.builtin_provider_identity(ProviderKind::Siliconflow)?
    } else {
        identity.clone()
    };
    let entry = config
        .provider_config_for_mut(&auth_owner)
        .map_err(|error| error.to_string())?;
    if pin_kimi_code_base_url {
        entry.base_url =
            Some(codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL.to_string());
    }
    entry.auth_mode = Some("api_key".to_string());
    entry.api_key = Some(api_key);
    entry.external_credentials = None;
    if matches!(provider, ProviderKind::Xai | ProviderKind::Anthropic) {
        entry.oauth_credential_generation = None;
    }
    Ok(())
}

pub(crate) fn loaded_session_requires_engine_respawn(
    app: &App,
    previous_provider: ProviderKind,
    previous_provider_identity: &str,
    previous_workspace: &Path,
) -> bool {
    app.api_provider != previous_provider
        || app.provider_identity_for_persistence() != previous_provider_identity
        || app.workspace != previous_workspace
}

pub(crate) fn restore_loaded_session_provider(
    app: &mut App,
    config: &mut Config,
    identity: ProviderIdentity,
) -> Result<(), String> {
    let provider = identity.provider;
    config.scope_to_provider_identity(&identity)?;
    app.set_provider_identity_record(identity.clone());
    app.billing_presentation = codewhale_engine::route_billing::for_route(config, &identity);
    app.max_subagents = config
        .max_subagents_for_provider(&identity)
        .clamp(1, codewhale_runtime::config::MAX_SUBAGENTS);
    app.provider_chain = (identity.key.as_str() == provider.as_str()
        && provider != ProviderKind::Antigravity)
        .then(|| codewhale_config::ProviderChain::new(provider, &config.fallback_providers))
        .filter(|chain| chain.providers().len() > 1);
    app.last_fallback_reason = None;
    app.model_ids_passthrough = config.model_ids_pass_through();
    if !app.auto_model {
        let requested = app
            .reasoning_effort_preference
            .unwrap_or(app.reasoning_effort);
        app.reasoning_effort = requested.normalize_for_route_identity(
            provider,
            identity.key.as_str(),
            &config.active_route_base_url(),
            &app.model,
        );
    }
    app.set_active_context_window_override(config, &identity);
    app.active_route_limits = app.context_window_override_limits();
    app.active_route_base_url = config.active_route_base_url();
    app.active_context_window = app.configured_context_window_for(&app.model);
    app.active_context_window_source = app
        .configured_context_window_for(&app.model)
        .map(|resolution| resolution.source)
        .unwrap_or(codewhale_engine::route_runtime::ContextWindowSource::Fallback);
    Ok(())
}

pub(crate) fn resolve_loaded_session_route(app: &mut App, config: &Config) {
    let identity = match app
        .admitted_provider_identity()
        .cloned()
        .and_then(|identity| {
            config.verify_provider_identity(&identity)?;
            Ok(identity)
        }) {
        Ok(identity) => identity,
        Err(reason) => {
            app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
            return;
        }
    };
    app.set_active_context_window_override(config, &identity);
    if app.auto_model {
        app.active_route_limits = app.context_window_override_limits();
        app.active_route_base_url = config.active_route_base_url();
        app.active_context_window = app.configured_context_window_for(&app.model);
        app.active_context_window_source = app
            .configured_context_window_for(&app.model)
            .map(|resolution| resolution.source)
            .unwrap_or(codewhale_engine::route_runtime::ContextWindowSource::Fallback);
        return;
    }

    match codewhale_engine::route_runtime::resolve_runtime_route_for_identity(
        config,
        &identity,
        Some(&app.model),
    ) {
        Ok(resolution) => {
            app.set_active_route_resolution(
                resolution.candidate.endpoint().base_url.clone(),
                resolution.candidate.limits(),
                resolution.context_window,
            );
        }
        Err(_) => {
            app.active_route_limits = app.context_window_override_limits();
            app.active_route_base_url = config.active_route_base_url();
            app.active_context_window = app.configured_context_window_for(&app.model);
            app.active_context_window_source = app
                .configured_context_window_for(&app.model)
                .map(|resolution| resolution.source)
                .unwrap_or(codewhale_engine::route_runtime::ContextWindowSource::Fallback);
        }
    }
}

/// Derive a short display title from the API message list.
///
/// Tries several strategies in order:
/// 1. If the first user message starts with a known slash command (`/goal`,
///    `/fleet`, `/workflow`, etc.), use the command + first argument.
/// 2. Otherwise, take the first meaningful line and cut it at a natural
///    phrase boundary (period, comma, colon, or word boundary) within
///    `SESSION_TITLE_MAX_CHARS`, never splitting mid-word.
///
/// Never leaks raw prompt text — the result is always a concise label.
pub(crate) fn derive_session_title(messages: &[Message]) -> Option<String> {
    let text = codewhale_engine::session_manager::conversation_title_prompt(messages)?;

    let first_line = codewhale_engine::session_manager::sanitize_session_title(
        text.lines().next().unwrap_or("").trim(),
    );
    let first_line = first_line.trim();
    if first_line.is_empty() {
        return None;
    }

    // Slash command: extract command name + first reasonable argument.
    if let Some(rest) = first_line.strip_prefix('/') {
        let parts: Vec<&str> = rest.split_whitespace().collect();
        return match parts.as_slice() {
            [] => None,
            [cmd] => Some(format!("/{cmd}")),
            [cmd, arg, ..] => {
                let arg_short = short_title_truncate(arg, 24);
                Some(format!("/{cmd} {arg_short}"))
            }
        };
    }

    Some(short_title_truncate(first_line, SESSION_TITLE_MAX_CHARS))
}

#[cfg(test)]
mod derived_title_tests {
    use super::*;
    use codewhale_models::Role;

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    #[test]
    fn derived_titles_drop_terminal_controls_and_bidi_format_chars() {
        // The first user message can carry pasted escape sequences; the
        // derived session name must never persist them.
        let msgs = [user("Fix \u{1b}]0;PWNED\u{7}the\u{202e} build 会議")];
        assert_eq!(
            derive_session_title(&msgs).as_deref(),
            Some("Fix ]0;PWNEDthe build 会議")
        );
        // Controls alone leave no title to derive.
        assert_eq!(derive_session_title(&[user("\u{1b}\u{7}\u{200b}")]), None);
    }

    #[test]
    fn live_title_uses_the_same_user_prompt_after_runtime_handoffs() {
        let handoff = codewhale_engine::runtime_handoff::operate_contract_runtime_message();
        assert_eq!(derive_session_title(std::slice::from_ref(&handoff)), None);
        let messages = [handoff, user("/goal Fix the diagnostic display")];
        assert_eq!(
            derive_session_title(&messages).as_deref(),
            Some("/goal Fix")
        );
        assert_eq!(
            codewhale_engine::session_manager::conversation_title_prompt(&messages),
            Some("/goal Fix the diagnostic display")
        );
    }
}

#[cfg(test)]
mod stall_outbox_tests {
    use super::*;
    use crate::tui::app::TuiOptions;

    /// `recover_stalled_runtime_turn` must emit a `turn_stalled` lifecycle
    /// outbox event naming the wedged turn — the first scriptable stall
    /// signal. The outbox is opt-in, so the test enables it through config.
    #[tokio::test]
    async fn stalled_turn_emits_turn_stalled_outbox_event() {
        let _lock = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let outbox_path = dir.path().join("outbox.jsonl");

        let config = {
            let __cw_config_init_0_0 = Some(codewhale_config::LifecycleOutboxToml {
                path: Some(outbox_path.clone()),
                webhook_url: None,
                webhook_token: None,
            });
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.lifecycle_outbox = __cw_config_init_0_0;
            __cw_config_init_0_value
        };
        let options = TuiOptions {
            start_in_agent_mode: true,
            ..crate::tui::test_support::test_tui_options(dir.path())
        };
        let mut app = App::new(options, &config);
        assert!(app.lifecycle_outbox.is_enabled());
        let expected_workspace = app.workspace.display().to_string();

        app.runtime_turn_id = Some("turn-1".to_string());
        app.runtime_turn_status = Some("in_progress".to_string());
        app.is_loading = true;
        recover_stalled_runtime_turn(
            &mut app,
            "Turn stalled — no completion signal received",
            StatusToastLevel::Error,
        );

        // The outbox writer task drains asynchronously; wait for the line.
        let mut lines = Vec::new();
        for _ in 0..200 {
            if let Ok(text) = tokio::fs::read_to_string(&outbox_path).await {
                lines = text
                    .lines()
                    .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("json"))
                    .collect();
                if !lines.is_empty() {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert_eq!(lines.len(), 1, "expected one turn_stalled outbox line");
        let line = &lines[0];
        assert_eq!(line["event"], "turn_stalled");
        assert_eq!(line["kind"], "turn.stalled");
        assert_eq!(line["turn_id"], "turn-1");
        assert_eq!(line["schema_version"], 1);
        assert_eq!(line["seq"], 1);
        // Every payload carries the workspace for consumer-side routing.
        assert_eq!(
            line["payload"]["workspace"],
            serde_json::json!(expected_workspace)
        );
        // The stall message is engine-authored and safe, but still bounded
        // and never raw tool/environment content.
        let message = line["payload"]["message"].as_str().expect("message");
        assert!(message.contains("stalled"));
        assert!(
            message.chars().count() <= codewhale_hooks::OUTBOX_DETAIL_MAX_CHARS,
            "stall message must be bounded"
        );
    }

    /// A disabled outbox (config without a path) must make stall recovery
    /// behave exactly as before: the toast still lands, no file is written.
    #[tokio::test]
    async fn stalled_turn_without_outbox_config_writes_nothing() {
        let _lock = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let options = TuiOptions {
            start_in_agent_mode: true,
            ..crate::tui::test_support::test_tui_options(dir.path())
        };
        let mut app = App::new(options, &Config::default());
        assert!(!app.lifecycle_outbox.is_enabled());

        app.runtime_turn_id = Some("turn-1".to_string());
        app.runtime_turn_status = Some("in_progress".to_string());
        app.is_loading = true;
        recover_stalled_runtime_turn(
            &mut app,
            "Turn stalled — no completion signal received",
            StatusToastLevel::Error,
        );

        // Recovery still clears the wedged turn state and posts the toast.
        assert!(app.runtime_turn_id.is_none());
        assert!(!app.is_loading);
        assert!(!app.status_toasts.is_empty());
        assert!(
            !dir.path().join("outbox.jsonl").exists(),
            "no outbox file must be created when the feature is off"
        );
    }
}

#[cfg(test)]
mod launch_resume_tests {
    use super::*;

    /// A recent-work row that vanished behind the card must leave the card
    /// up with a status — never strand the user on an empty stage.
    #[test]
    fn resume_missing_session_leaves_the_card_up_with_a_status() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(dir.path()),
            &Config::default(),
        );
        app.launch.visible = true;
        let result = resume_launch_session(&mut app, "no-such-session-000000");
        assert!(result.action.is_none(), "nothing to load");
        assert!(app.launch.visible, "the card stays up");
        let status = app.launch.status.as_deref().expect("a status");
        assert!(
            status.contains("Resume failed"),
            "the status says why: {status}"
        );
    }

    /// U1 / #6566: a keyless first message goes back into the composer, leaves
    /// one durable transcript line, opens the provider picker, and a later
    /// routine acknowledgement ("Auto-compaction enabled") does not wipe the
    /// error from the footer.
    #[test]
    fn keyless_submit_leaves_a_durable_recovery_that_config_acks_cannot_erase() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(dir.path()),
            &Config::default(),
        );
        app.onboarding_had_provider_step = true;
        let cells_before = app.history.len();
        keep_unsent_message_for_connect(
            &mut app,
            crate::tui::app::QueuedMessage::new("hello".to_string(), None),
            "DeepSeek API key not found",
        );
        assert_eq!(app.input, "hello", "the unsent message is not lost");
        assert_eq!(app.history.len(), cells_before + 1);
        let Some(HistoryCell::System { content }) = app.history.last() else {
            panic!("keyless submit must leave a transcript line");
        };
        assert!(content.starts_with("No model is connected"), "{content}");
        // A new user never chose DeepSeek; the line must not blame its key.
        assert!(!content.contains("DeepSeek"), "{content}");
        assert_eq!(app.onboarding, OnboardingState::Provider);
        assert!(app.onboarding_needs_api_key);

        app.status_message = Some("Make room automatically: on".to_string());
        let shown = app
            .active_status_toast(crate::tui::underwater::ShellPhase::Idle)
            .expect("footer notice");
        assert_eq!(shown.level, StatusToastLevel::Error);
        assert!(shown.text.contains("Message not sent"), "{}", shown.text);
        assert!(!shown.text.contains("DeepSeek"), "{}", shown.text);
    }

    /// A returning user's line names the command that saves the missing key,
    /// and Esc from the picker it opens returns to the composer with the
    /// message still there, not to the welcome screen.
    #[test]
    fn keyless_submit_names_the_key_and_esc_returns_to_the_composer() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(dir.path()),
            &Config::default(),
        );
        app.onboarding = OnboardingState::None;
        app.onboarding_missing_key_recovery = false;
        app.onboarding_had_provider_step = false;
        keep_unsent_message_for_connect(
            &mut app,
            crate::tui::app::QueuedMessage::new("hello".to_string(), None),
            "DeepSeek API key not found.\n\n 1. Get a key:  https://platform.deepseek.com/api_keys\n 2. Save it (works in every folder, no OS prompts):\n        codewhale auth set --provider deepseek\n\n Alternatives:\n   • export DEEPSEEK_API_KEY=<your-key>. Failed to configure provider route deepseek / deepseek-flash.",
        );
        let Some(HistoryCell::System { content }) = app.history.last() else {
            panic!("keyless submit must leave a transcript line");
        };
        assert!(
            content.contains("codewhale auth set --provider deepseek"),
            "{content}"
        );
        assert!(content.contains("F3"), "{content}");
        // The footer keeps the full help page; the transcript keeps two facts.
        assert!(!content.contains("Alternatives"), "{content}");
        assert!(!content.contains("Failed to configure"), "{content}");
        assert!(app.onboarding_missing_key_recovery);
        assert!(app.onboarding_recovers_configured_route());

        back_from_provider_onboarding(&mut app);
        assert_eq!(app.onboarding, OnboardingState::None);
        assert!(app.onboarding_needs_api_key);
        assert_eq!(app.input, "hello");
    }

    /// The prominent new-session entry begins a fresh session in place.
    #[test]
    fn new_session_begins_a_fresh_session_and_leaves_the_card() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(dir.path()),
            &Config::default(),
        );
        app.launch.visible = true;
        let result = begin_launch_session(&mut app, None);
        assert!(!app.launch.visible, "the session began");
        assert!(
            app.current_session_id.is_some(),
            "a fresh session id was minted"
        );
        assert!(
            matches!(result.action, Some(AppAction::SyncSession { .. })),
            "the engine syncs the fresh session"
        );
    }
}

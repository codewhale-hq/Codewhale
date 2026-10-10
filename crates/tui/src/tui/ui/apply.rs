//! `apply_*` helpers: committing an already-resolved choice to `App`, the
//! engine, and persisted settings.
//!
//! Moved verbatim out of `ui.rs`.

use super::observer_hooks::{
    execute_subagent_observer_hook, subagent_failure_notice,
    surface_observer_hook_submission_failure,
};
use super::task_projection::refresh_active_task_panel;
use super::*;

/// Record the model frozen into a child's runtime at spawn time.
///
/// This is child-route evidence, not an inference from the parent session. A
/// later usage envelope may confirm or replace it with the provider's
/// effective route while also adding provider and token facts.
pub(crate) fn record_agent_spawned_route(app: &mut App, agent_id: &str, model: &str) {
    let model =
        bound_agent_activity_text(&crate::cost_status::sanitize_persisted_route_label(model));
    app.agent_progress_meta
        .entry(agent_id.to_string())
        .or_default()
        .resolved_model = Some(model).filter(|model| !model.trim().is_empty());
}

/// Apply the normal spawn status first, then submit its observer event.
/// Submission diagnostics go to the independent toast queue, so they remain
/// visible without replacing the agent's authoritative lifecycle status.
pub(crate) fn apply_agent_spawned_status_and_observer(
    app: &mut App,
    agent_id: &str,
    prompt: &str,
    prompt_summary: &str,
) {
    let label = app.ensure_agent_label(agent_id);
    codewhale_telemetry::session_counters().bump(codewhale_telemetry::Counter::SubagentSpawn);
    app.push_status_toast_record(
        StatusToast::new(
            format!(
                "{} · {label} · {}",
                app.tr(MessageId::SubagentsStatusRunning),
                bound_agent_activity_text(prompt_summary)
            ),
            StatusToastLevel::Info,
            Some(4_000),
        )
        .for_event(format!("subagent-start:{agent_id}")),
    );
    if let Err(error) =
        execute_subagent_observer_hook(app, HookEvent::SubagentSpawn, agent_id, "prompt", prompt)
    {
        surface_observer_hook_submission_failure(app, error);
    }
}

/// Completion counterpart to [`apply_agent_spawned_status_and_observer`].
pub(crate) fn apply_agent_complete_status_and_observer(
    app: &mut App,
    agent_id: &str,
    result: &str,
    status: &SubAgentStatus,
) {
    let label = app.agent_display_label(agent_id);
    let level = match status {
        SubAgentStatus::Completed => StatusToastLevel::Success,
        SubAgentStatus::Failed(_) | SubAgentStatus::BudgetExhausted => StatusToastLevel::Error,
        SubAgentStatus::Interrupted(_) | SubAgentStatus::Cancelled => StatusToastLevel::Warning,
        SubAgentStatus::Running => StatusToastLevel::Info,
    };
    let failure = subagent_failure_notice(result);
    let detail = failure.as_deref().unwrap_or(result);
    let message = format!(
        "{} · {label} · {}",
        app.tr(notifications::subagent_terminal_label(status)),
        bound_agent_activity_text(detail)
    );
    if level == StatusToastLevel::Error {
        app.set_sticky_status(message, level, Some(App::STICKY_ERROR_TTL_MS));
    } else {
        app.push_status_toast_record(
            StatusToast::new(message, level, Some(5_000))
                .for_event(format!("subagent-terminal:{agent_id}")),
        );
    }
    if let Err(error) =
        execute_subagent_observer_hook(app, HookEvent::SubagentComplete, agent_id, "result", result)
    {
        surface_observer_hook_submission_failure(app, error);
    }
}

pub(crate) fn apply_coordination_detail_projection(
    app: &mut App,
    projection: crate::tools::subagent::CoordinationDetailProjection,
) {
    // §2.6: when this process does not own the workspace coordination flock,
    // say so on the sticky status strip. A silent "running (543s)" row on a
    // settled turn is a lie; surface the lock loss the same way we surface
    // other session hazards.
    //
    // Exception: a same-process handover. A model/provider switch spawns the
    // new engine before the old engine's manager has dropped the flock, and
    // flock treats the second fd in this same process as a conflict. That
    // state self-heals on the next projection retry (#5036), and a 30-second
    // warning blaming "another Codewhale process" would be false (owner
    // report, 2026-08-04) — so it stays off the sticky strip.
    if !projection.process_lock_held {
        let note = projection
            .process_lock_note
            .as_deref()
            .unwrap_or("another Codewhale process owns delegated coordination for this workspace");
        let same_process_handover =
            note.contains(crate::tools::subagent::COORDINATION_SAME_PROCESS_HANDOVER);
        // The strip is one row. The old copy opened with the diagnosis
        // ("Delegated coordination unavailable — ") and buried the cause
        // behind a `{note}` carrying a pid, an absolute workspace path, and an
        // errno, so a truncated strip showed `Delegated coordination
        // unavailable — an…` and taught the user nothing. Lead with the fact
        // that explains it — a second session is open here — and leave the pid
        // and path to the coordination detail view, which already renders
        // `process_lock_note` in full.
        let message = if note.contains(crate::tools::subagent::COORDINATION_LOCK_TIMEOUT_MARKER) {
            "Timed out claiming delegated coordination for this workspace — job rows still settle locally.".to_string()
        } else {
            "Another Codewhale session in this workspace owns delegated coordination — job rows still settle locally.".to_string()
        };
        // Demoted from sticky 30s to transient 5s — two sessions in same workspace
        // should not feel broken; job rows still settle locally. The detail view
        // still shows the full pid/path via `process_lock_note`.
        let already = app
            .status_toasts
            .iter()
            .any(|toast| toast.text.contains("delegated coordination"));
        if !already && !same_process_handover {
            app.push_status_toast(
                message,
                crate::tui::app::StatusToastLevel::Info,
                Some(5_000),
            );
        }
    }
    app.coordination_detail = Some(projection);
}

pub(crate) fn apply_alt_4_shortcut(app: &mut App, _modifiers: KeyModifiers) {
    rail_panel_shortcut(app, crate::tui::work_surface::RailPanel::Files);
}

pub(crate) fn apply_alt_0_shortcut(app: &mut App, modifiers: KeyModifiers) {
    // Ctrl+Alt+0 toggles the rail off and back to the default bottom
    // placement. Plain Alt+0 is unbound: it used to select the retired
    // auto-collapse mode.
    if modifiers.contains(KeyModifiers::CONTROL) {
        if app.work_surface.placement == crate::tui::work_surface::WorkSurfacePlacement::Off {
            app.work_surface.placement = crate::tui::work_surface::WorkSurfacePlacement::Bottom;
            app.status_message = Some("Workbar: bottom placement".to_string());
        } else {
            app.work_surface.placement = crate::tui::work_surface::WorkSurfacePlacement::Off;
            app.status_message = Some("Workbar is off".to_string());
        }
        app.needs_redraw = true;
    }
}

pub(crate) fn apply_picker_session_rename_to_active_app(
    app: &mut App,
    metadata: crate::session_manager::SessionMetadata,
) -> bool {
    if app.current_session_id.as_deref() != Some(metadata.id.as_str()) {
        return false;
    }
    app.session_title = Some(metadata.title.clone());
    app.current_session_metadata = Some(metadata);
    true
}

/// Translate an `EngineEvent::Error` into UI state updates.
///
/// The engine's `recoverable` flag (mirrored on `ErrorEnvelope`) decides
/// whether the session flips into offline mode: stream stalls, chunk
/// timeouts, transient network errors, and rate-limit/server hiccups arrive
/// recoverable and must NOT flip into offline. Hard failures (auth, billing,
/// invalid request) arrive non-recoverable; those flip offline so subsequent
/// messages get queued instead of silently lost mid-flight.
///
/// `severity` drives transcript color: red for `Error`/`Critical`, amber for
/// `Warning`, dim for `Info`.
pub(crate) fn apply_engine_error_to_app(
    app: &mut App,
    envelope: crate::error_taxonomy::ErrorEnvelope,
) {
    let recoverable = envelope.recoverable;
    let message = envelope.message.clone();
    let severity = envelope.severity;
    let turn_was_in_progress =
        app.is_loading || matches!(app.runtime_turn_status.as_deref(), Some("in_progress"));
    // A recoverable error can precede tool decisions in the same turn. Keep
    // routing those events until TurnComplete; marking the UI idle here drops
    // ApprovalRequired and leaves the engine waiting for an invisible decision.
    // An idle or locally cancelled turn must never be reactivated by an error.
    let turn_remains_active =
        recoverable && turn_was_in_progress && !app.suppress_stream_events_until_turn_complete;
    // The engine decides whether the question was taken back out of the
    // session; the host only follows, so the two cannot disagree (#6566).
    let credential_rejected_before_output = turn_was_in_progress
        && envelope.code == crate::error_taxonomy::CREDENTIAL_REJECTED_UNSENT_CODE;
    streaming_thinking::finalize_current(app);
    if turn_was_in_progress {
        app.finalize_streaming_assistant_as_interrupted();
        app.finalize_active_cell_as_interrupted();
        if !turn_remains_active {
            app.runtime_turn_status = Some("failed".to_string());
        }
    }
    app.streaming_state.reset();
    app.streaming_message_index = None;
    app.streaming_thinking_active_entry = None;
    // Before the error line lands, so the question's bubble is still the last
    // cell and can come out with it. A draft the person already started in
    // the composer is left alone, and so is the bubble that holds their text.
    let unsent_message = if credential_rejected_before_output && app.input.is_empty() {
        take_back_unsent_submission(app)
    } else {
        None
    };

    // #455 (observer-only): fire `on_error` hooks so operators can
    // page on auth / billing / invalid-request failures without
    // tailing the audit log. Read-only — the hook can react but not
    // suppress the error from reaching the transcript. Fast-path
    // skip when no hooks configured.
    if app
        .hooks
        .has_hooks_for_event(crate::hooks::HookEvent::OnError)
    {
        let context = app.base_hook_context().with_error(&message);
        if let Err(error) = app.submit_hooks(crate::hooks::HookEvent::OnError, context) {
            surface_observer_hook_submission_failure(app, error);
        }
    }

    app.add_message(HistoryCell::Error {
        message: message.clone(),
        severity,
    });
    app.is_loading = turn_remains_active;
    if !turn_remains_active {
        app.dispatch_started_at = None;
    }
    app.turn_error_posted = true;
    app.turn_error_notice = Some(message.clone());
    if credential_rejected_before_output {
        // #6566: the provider refused the key before any model output, so the
        // engine takes the question back out of the session. Give it back to
        // the person with the one next step, instead of an error with no way
        // forward and a message they must retype. The whole message comes
        // back, a skill it invoked included.
        match unsent_message {
            Some(message) => app.restore_unsent_message(message),
            None => {
                app.restore_last_submitted_prompt_if_empty();
            }
        }
        app.add_message(HistoryCell::System {
            content: app.tr(MessageId::AuthRejectedRecovery).into_owned(),
        });
    }
    if matches!(
        envelope.category,
        crate::error_taxonomy::ErrorCategory::Authentication
    ) && app.api_key_env_only
    {
        app.offline_mode = true;
        app.onboarding_needs_api_key = true;
        // The key was rejected, not missing: Esc returns to the composer and
        // the picker focuses the configured route, as missing-key recovery
        // does, instead of walking back through first-run screens.
        app.onboarding_missing_key_recovery = true;
        app.onboarding = OnboardingState::Provider;
        let provider = app.api_provider;
        let config_path = match crate::config::resolve_load_config_path(app.config_path.clone()) {
            Ok(Some(path)) => path.display().to_string(),
            Ok(None) => "~/.codewhale/config.toml".to_string(),
            Err(error) => error.to_string(),
        };
        let notice = tr(app.ui_locale, MessageId::OnboardApiKeyRejectedEnv)
            .replace("{provider}", provider.as_str())
            .replace("{env}", &provider.provider().env_vars().join(" / "))
            .replace("{path}", &config_path);
        // The setup screen covers the transcript, so it must say why it
        // opened. The log records the reason without the provider's message,
        // which the transcript already carries.
        crate::logging::warn(format!(
            "{} rejected the environment API key; opening provider setup",
            provider.as_str()
        ));
        app.onboarding_key_rejected = Some(notice.clone());
        app.push_status_toast(
            notice,
            StatusToastLevel::Error,
            Some(App::STICKY_ERROR_TTL_MS),
        );
        return;
    }
    if recoverable
        && matches!(
            envelope.category,
            crate::error_taxonomy::ErrorCategory::Network
                | crate::error_taxonomy::ErrorCategory::RateLimit
                | crate::error_taxonomy::ErrorCategory::Timeout
        )
        && app.advance_fallback(message.clone()).is_some()
    {
        let position = app.fallback_chain_position().unwrap_or(0);
        let total = app.fallback_chain_len();
        app.push_status_toast(
            app.tr(MessageId::NotificationProviderFallback)
                .replace("{provider}", app.api_provider.as_str())
                .replace("{position}", &position.to_string())
                .replace("{total}", &total.saturating_sub(1).to_string()),
            StatusToastLevel::Warning,
            Some(8_000),
        );
        return;
    }
    if !recoverable {
        app.offline_mode = true;
    }
    // Error is already in the transcript as HistoryCell::Error above;
    // don't emit a redundant status_message that would become a sticky
    // toast in the footer — that duplicates the transcript entry.
}

/// Apply the gate result on the event loop. Returns `true` when dispatch may
/// continue; a denial leaves the original message out of history/model input.
pub(crate) fn apply_message_submit_outcome(
    app: &mut App,
    message: &mut QueuedMessage,
    outcome: crate::hooks::MessageSubmitOutcome,
) -> bool {
    if let Some(warning) = outcome.warning() {
        app.status_message = Some(warning.to_string());
    }
    match outcome {
        crate::hooks::MessageSubmitOutcome::Unchanged { .. } => true,
        crate::hooks::MessageSubmitOutcome::Replaced { text, .. } => {
            // A queued message was already echoed with its pre-hook text.
            // Retarget that one cell so dispatch reuses it instead of adding a
            // second User cell beside the stale echo (U02-03).
            if message.history_echoed
                && let Some(idx) =
                    crate::tui::ui::dispatch::echoed_user_turn_cell(app, &message.display)
            {
                app.history[idx] = HistoryCell::User {
                    content: text.clone(),
                };
                app.bump_history_cell(idx);
            }
            message.display = text;
            true
        }
        crate::hooks::MessageSubmitOutcome::Blocked { reason } => {
            app.status_message = Some(reason);
            false
        }
    }
}

fn visible_goal_as_durable(
    app: &App,
) -> Result<Option<crate::session_manager::SessionGoalState>, String> {
    let Some(objective) = app.goal.objective.as_deref() else {
        return Ok(None);
    };
    let elapsed_seconds = app
        .goal
        .started_at
        .map(|started| started.elapsed().as_secs())
        .unwrap_or(app.goal.time_used_seconds)
        .max(app.goal.time_used_seconds);
    crate::session_manager::SessionGoalState::from_runtime(&GoalSnapshot {
        objective: Some(objective.to_string()),
        status: app.goal.status.as_str().to_string(),
        token_budget: app.goal.token_budget,
        tokens_used: app.goal.tokens_used,
        time_used_seconds: app.goal.time_used_seconds,
        continuation_count: app.goal.continuation_count,
        elapsed_seconds: Some(elapsed_seconds),
        pause_reason: app.goal.pause_reason,
        ..Default::default()
    })
    .map_err(|error| error.to_string())
}

fn desired_goal_state(
    app: &App,
    intent: &GoalControlIntent,
) -> Result<Option<crate::session_manager::SessionGoalState>, String> {
    let mut base = if app.pending_goal_controls.is_empty() {
        match app.last_known_goal_state.clone() {
            Some(goal) => Some(goal),
            None => visible_goal_as_durable(app)?,
        }
    } else {
        // Accepted controls compose over the latest durable target, not the
        // older visible projection that is still waiting on GoalUpdated.
        app.last_known_goal_state.clone()
    };
    match intent {
        GoalControlIntent::SetStatus { clear: true, .. } => Ok(None),
        GoalControlIntent::SetStatus {
            status,
            clear: false,
        } => {
            let goal = base
                .as_mut()
                .ok_or_else(|| "No goal is available for this control.".to_string())?;
            goal.status = match status {
                GoalStatus::Active => crate::session_manager::SessionGoalStatus::Active,
                GoalStatus::Paused => crate::session_manager::SessionGoalStatus::Paused,
                GoalStatus::Complete => crate::session_manager::SessionGoalStatus::Complete,
                GoalStatus::Blocked => crate::session_manager::SessionGoalStatus::Blocked,
            };
            if *status == GoalStatus::Active {
                goal.goal_id = Some(uuid::Uuid::new_v4().to_string());
                goal.last_gap_fingerprint = None;
                goal.repeated_gap_count = 0;
                goal.last_gap_pass = None;
            }
            goal.pause_reason = (*status == GoalStatus::Paused)
                .then_some(crate::tools::goal::GoalPauseReason::User);
            Ok(base)
        }
        GoalControlIntent::SetObjective {
            objective,
            token_budget,
        } => crate::session_manager::SessionGoalState::from_runtime(&GoalSnapshot {
            goal_id: Some(uuid::Uuid::new_v4().to_string()),
            objective: Some(objective.clone()),
            status: GoalStatus::Active.as_str().to_string(),
            token_budget: *token_budget,
            elapsed_seconds: Some(0),
            ..Default::default()
        })
        .map_err(|error| error.to_string()),
    }
}

fn persist_accepted_goal_state(
    app: &mut App,
    desired: Option<&crate::session_manager::SessionGoalState>,
) -> Result<(), String> {
    let manager = SessionManager::default_location()
        .map_err(|error| format!("could not open the session store: {error}"))?;
    if app.current_session_id.is_none() {
        let session = build_session_snapshot(app, &manager)?;
        let session_id = session.metadata.id.clone();
        if !persistence_actor::try_persist(PersistRequest::SaveCheckpoint { session }) {
            return Err("the persistence worker is unavailable".to_string());
        }
        app.current_session_id = Some(session_id);
    }
    let session_id = app
        .current_session_id
        .as_deref()
        .ok_or_else(|| "session id is not established".to_string())?;
    manager
        .save_session_goal(session_id, desired)
        .map_err(|error| error.to_string())
}

fn goal_control_op(intent: &GoalControlIntent, goal_id: Option<String>) -> Op {
    match intent {
        GoalControlIntent::SetStatus { status, clear } => Op::SetGoalStatus {
            goal_id,
            status: *status,
            clear: *clear,
        },
        GoalControlIntent::SetObjective {
            objective,
            token_budget,
        } => Op::SetGoalObjective {
            goal_id,
            objective: objective.clone(),
            token_budget: *token_budget,
        },
    }
}

/// Retry accepted goal controls without ever awaiting mailbox capacity on the
/// input loop. FIFO order is retained until each authoritative receipt lands.
pub(crate) fn flush_pending_goal_controls(app: &mut App, engine_handle: &EngineHandle) -> bool {
    for pending in &mut app.pending_goal_controls {
        if pending.dispatched {
            continue;
        }
        if engine_handle
            .try_send(goal_control_op(&pending.intent, pending.goal_id.clone()))
            .is_err()
        {
            return engine_handle.tx_op.is_closed();
        }
        pending.dispatched = true;
    }
    false
}

fn goal_control_matches(
    intent: &GoalControlIntent,
    durable: Option<&crate::session_manager::SessionGoalState>,
) -> bool {
    match intent {
        GoalControlIntent::SetStatus { clear: true, .. } => durable.is_none(),
        GoalControlIntent::SetStatus {
            status,
            clear: false,
        } => durable.is_some_and(|goal| {
            goal.status
                == match status {
                    GoalStatus::Active => crate::session_manager::SessionGoalStatus::Active,
                    GoalStatus::Paused => crate::session_manager::SessionGoalStatus::Paused,
                    GoalStatus::Complete => crate::session_manager::SessionGoalStatus::Complete,
                    GoalStatus::Blocked => crate::session_manager::SessionGoalStatus::Blocked,
                }
        }),
        GoalControlIntent::SetObjective {
            objective,
            token_budget,
        } => durable.is_some_and(|goal| {
            goal.objective == *objective
                && goal.status == crate::session_manager::SessionGoalStatus::Active
                && goal.token_budget == *token_budget
        }),
    }
}

fn accept_goal_control(app: &mut App, engine_handle: &EngineHandle, intent: GoalControlIntent) {
    let desired = match desired_goal_state(app, &intent) {
        Ok(desired) => desired,
        Err(error) => {
            surface_goal_persistence_failure(app, &error);
            return;
        }
    };
    if let Err(error) = persist_accepted_goal_state(app, desired.as_ref()) {
        surface_goal_persistence_failure(app, &error);
        return;
    }

    if matches!(
        intent,
        GoalControlIntent::SetStatus {
            status: GoalStatus::Complete,
            clear: false
        }
    ) {
        crate::audit::log_sensitive_event(
            "goal.user_completed",
            serde_json::json!({ "accepted": true }),
        );
    }
    app.last_known_goal_state = desired;
    app.pending_goal_controls.push_back(PendingGoalControl {
        goal_id: app
            .last_known_goal_state
            .as_ref()
            .and_then(|goal| goal.goal_id.clone()),
        intent,
        dispatched: false,
    });
    let runtime_closed = flush_pending_goal_controls(app, engine_handle);
    app.add_message(HistoryCell::System {
        content: app.tr(MessageId::GoalControlAccepted).to_string(),
    });
    if runtime_closed {
        app.push_status_toast(
            app.tr(MessageId::GoalControlRuntimeUnavailable).to_string(),
            StatusToastLevel::Warning,
            None,
        );
    }
}

pub(crate) fn apply_goal_snapshot_to_app(app: &mut App, snapshot: &GoalSnapshot) -> bool {
    let durable_goal = match crate::session_manager::SessionGoalState::from_runtime(snapshot) {
        Ok(goal) => goal,
        Err(error) => {
            tracing::warn!("ignoring invalid runtime goal snapshot: {error}");
            return false;
        }
    };
    let pending_desired = app.last_known_goal_state.clone();
    let matched_pending = app.pending_goal_controls.front().is_some_and(|pending| {
        pending.dispatched
            && pending.goal_id.as_deref().is_none_or(|id| {
                Some(id)
                    == durable_goal
                        .as_ref()
                        .and_then(|goal| goal.goal_id.as_deref())
            })
            && goal_control_matches(&pending.intent, durable_goal.as_ref())
    });
    // Accepted controls own the durable target until their exact revision's
    // receipt arrives. An earlier pass cannot restore pre-resume stall state.
    if !app.pending_goal_controls.is_empty() && !matched_pending {
        return false;
    }
    let durable_changed = app.last_known_goal_state != durable_goal;
    if matched_pending {
        app.pending_goal_controls.pop_front();
    }
    // An explicit engine-side clear is represented by the one canonical empty
    // state emitted by GoalState::snapshot. Require both fields so a malformed
    // objective-less Active/Blocked update cannot erase valid visible state.
    if snapshot.objective.is_none() && snapshot.status.trim() == "none" {
        let changed = app.goal.objective.is_some()
            || app.goal.token_budget.is_some()
            || app.goal.tokens_used != 0
            || app.goal.time_used_seconds != 0
            || app.goal.continuation_count != 0
            || app.goal.started_at.is_some()
            || app.goal.finished_at.is_some()
            || app.goal.status != GoalStatus::default();
        app.goal = crate::tui::app::HostGoalState::default();
        app.last_known_goal_state = if app.pending_goal_controls.is_empty() {
            None
        } else {
            pending_desired
        };
        return changed || matched_pending || durable_changed;
    }

    let Some(objective) = snapshot
        .objective
        .as_deref()
        .map(str::trim)
        .filter(|objective| !objective.is_empty())
    else {
        tracing::warn!(
            "ignoring objective-less runtime goal snapshot with non-clear status: {}",
            snapshot.status
        );
        return false;
    };
    let Some(status) = goal_status_from_snapshot(snapshot) else {
        tracing::warn!("ignoring unknown runtime goal status: {}", snapshot.status);
        return false;
    };
    let verdict = status;
    let objective_changed = app.goal.objective.as_deref() != Some(objective);
    let progress_changed = app.goal.progress != snapshot.progress;
    let changed = objective_changed
        || app.goal.token_budget != snapshot.token_budget
        || app.goal.tokens_used != snapshot.tokens_used
        || app.goal.time_used_seconds != snapshot.time_used_seconds
        || app.goal.continuation_count != snapshot.continuation_count
        || app.goal.pause_reason != snapshot.pause_reason
        || progress_changed
        || app.goal.status != verdict;
    if !changed {
        app.last_known_goal_state = if app.pending_goal_controls.is_empty() {
            durable_goal
        } else {
            pending_desired
        };
        return matched_pending || durable_changed;
    }

    // The runtime introduced a new active objective (the model called
    // `create_goal`, or a restored session carried one): say so once, in one
    // line, so the user knows a persistent goal is now driving turns and how
    // to stop it. `/goal <objective>` sets the objective before this snapshot lands,
    // so a user-declared goal does not repeat its own receipt.
    if objective_changed && verdict == GoalStatus::Active {
        // Operate set it from the prompt (or the model did while operating);
        // the objective is the prompt the user just typed, so the receipt
        // says what Operate will do with it instead of echoing it.
        let content = if app.mode == AppMode::Operate {
            app.tr(codewhale_localization::MessageId::GoalReceiptSetOperate)
                .into_owned()
        } else {
            app.tr(codewhale_localization::MessageId::GoalReceiptSet)
                .replace("{objective}", objective)
        };
        app.add_message(crate::tui::history::HistoryCell::System { content });
    }
    // A fresh reported-progress receipt reads like the model's own status
    // line: percent with a bar, then the optional now/next lines it wrote.
    // Paused/complete goals keep their last report silent — the lifecycle
    // receipt already spoke.
    if progress_changed
        && verdict == GoalStatus::Active
        && let Some(progress) = snapshot.progress.as_ref()
    {
        let mut content = app
            .tr(codewhale_localization::MessageId::GoalProgressReceipt)
            .replace("{percent}", &progress.percent.to_string())
            .replace(
                "{bar}",
                &crate::tools::goal::goal_progress_bar(progress.percent),
            );
        if let Some(now) = progress.now.as_deref() {
            content.push('\n');
            content.push_str(
                &app.tr(codewhale_localization::MessageId::GoalProgressNow)
                    .replace("{note}", now),
            );
        }
        if let Some(next) = progress.next.as_deref() {
            content.push('\n');
            content.push_str(
                &app.tr(codewhale_localization::MessageId::GoalProgressNext)
                    .replace("{note}", next),
            );
        }
        app.add_message(crate::tui::history::HistoryCell::System { content });
    }
    app.goal.progress = snapshot.progress.clone();
    app.goal.objective = Some(objective.to_string());
    app.goal.token_budget = snapshot.token_budget;
    app.goal.tokens_used = snapshot.tokens_used;
    app.goal.time_used_seconds = snapshot.time_used_seconds;
    app.goal.continuation_count = snapshot.continuation_count;
    app.goal.pause_reason = snapshot.pause_reason;
    app.goal.status = verdict;
    if objective_changed || app.goal.started_at.is_none() {
        let now = Instant::now();
        let elapsed = std::time::Duration::from_secs(snapshot.elapsed_seconds.unwrap_or_default());
        app.goal.started_at = now.checked_sub(elapsed).or(Some(now));
    }
    // Freeze the elapsed timer the first time a goal leaves the active state.
    // Paused (Wounded) goals freeze too — usage snapshots keep arriving while
    // paused, and clearing here would silently un-freeze a timer the user just
    // paused (matching close_hunt, which records the pause instant). Only an
    // explicit resume back to Hunting re-arms the timer.
    match verdict {
        GoalStatus::Complete | GoalStatus::Blocked | GoalStatus::Paused => {
            if app.goal.finished_at.is_none() {
                app.goal.finished_at = Some(Instant::now());
            }
        }
        GoalStatus::Active => app.goal.finished_at = None,
    }
    app.last_known_goal_state = if app.pending_goal_controls.is_empty() {
        durable_goal
    } else {
        pending_desired
    };
    true
}

/// Apply an explicit mode selection from a user shortcut (Alt+A/P/Y).
///
/// Uses `select_mode`, not `set_mode`, so an explicitly chosen mode is also the
/// startup default next launch – matching the Tab cycle and hotbar paths.
pub(crate) async fn apply_mode_update(
    app: &mut App,
    engine_handle: &EngineHandle,
    config: &Config,
    mode: AppMode,
) -> bool {
    let outcome = app.select_mode(mode);
    app.report_mode_selection(mode, outcome);
    if mode == AppMode::Operate {
        present_operate_board(app, config).await;
        // First contact with the fleet, not first launch, owns its intro.
        app.maybe_show_feature_intro();
    }
    if outcome.changed_live_state() {
        sync_mode_update(app, engine_handle).await;
        true
    } else {
        false
    }
}

/// Apply the legacy YOLO shortcut (Alt+Y): a permission change, not a mode
/// change. Same persist/report/sync contract as [`apply_mode_update`]; the
/// startup default written is the mode actually installed (Act).
pub(crate) async fn apply_yolo_compat_update(
    app: &mut App,
    engine_handle: &EngineHandle,
    _config: &Config,
) -> bool {
    let outcome = app.select_yolo_compat();
    app.report_mode_selection(AppMode::Agent, outcome);
    if outcome.changed_live_state() {
        sync_mode_update(app, engine_handle).await;
        true
    } else {
        false
    }
}

/// Entering Operate attaches to the recorded operation (a fresh one only
/// when none exists or the last was cancelled), shows the localized lead
/// plan, and keeps always-on mode durable by reinstalling the hourly lead
/// keepalive bound to this workspace. Burn rate is optional; default is
/// unbounded.
async fn present_operate_board(app: &mut App, config: &Config) {
    let store = match crate::operate::OperationStore::open(crate::operate::default_operate_dir()) {
        Ok(store) => store,
        Err(error) => {
            app.add_message(crate::tui::history::HistoryCell::System {
                content: format!("Operate store unavailable: {error}"),
            });
            return;
        }
    };
    let Some(automations) = app
        .runtime_services
        .automations
        .as_ref()
        .map(std::sync::Arc::clone)
    else {
        app.add_message(crate::tui::history::HistoryCell::System {
            content: "Operate keep-alive not installed: automation service unavailable".to_string(),
        });
        return;
    };
    let model = app.model_selection_for_persistence();
    let identity = match config.resolve_persisted_provider_identity(
        Some(app.api_provider.as_str()),
        app.provider_id_for_persistence(),
    ) {
        Ok(identity) => identity,
        Err(error) => {
            app.add_message(crate::tui::history::HistoryCell::System {
                content: format!("Operate keep-alive not installed: {error}"),
            });
            return;
        }
    };
    let (lead_model, credentials) = {
        let manager = automations.lock().await;
        match crate::operate::keepalive_readiness(&manager, config, Some((&identity, &model))) {
            Ok(credentials) => credentials,
            Err(error) => {
                app.add_message(crate::tui::history::HistoryCell::System {
                    content: format!("Operate keep-alive not installed: {error}"),
                });
                return;
            }
        }
    };
    let operation = match crate::operate::attach_or_start_operation(
        &store,
        &app.workspace,
        None,
        None,
        credentials,
        &lead_model,
    ) {
        Ok(mut operation) => {
            if credentials && !operation.direction.is_empty() && operation.lead_plan.is_none() {
                operation.plan_from_direction();
                if let Err(error) = store.save(&operation) {
                    app.add_message(crate::tui::history::HistoryCell::System {
                        content: format!("Operate plan not saved: {error}"),
                    });
                }
            }
            operation
        }
        Err(error) => {
            app.add_message(crate::tui::history::HistoryCell::System {
                content: format!("Operate did not start: {error}"),
            });
            return;
        }
    };
    // Always-on is durable only if the keepalive automation exists: entering
    // Operate (re)installs it for this workspace, kicking an immediate
    // lead-plan step when the attached operation still needs one.
    let needs_lead_plan = !operation
        .lead_plan
        .as_ref()
        .is_some_and(|plan| !plan.slices.is_empty());
    {
        let manager = automations.lock().await;
        if let Err(error) = crate::operate::upsert_keepalive(
            &manager,
            &app.workspace,
            needs_lead_plan,
            config,
            Some((&identity, &model)),
        ) {
            app.add_message(crate::tui::history::HistoryCell::System {
                content: format!("Operate keep-alive not installed: {error}"),
            });
        }
    }
    app.add_message(crate::tui::history::HistoryCell::System {
        content: crate::operate::render_plan_board_locale(&operation, app.ui_locale),
    });
}

pub(crate) async fn apply_model_and_compaction_update(
    engine_handle: &EngineHandle,
    compaction: crate::compaction::CompactionConfig,
    mode: AppMode,
    route_limits: Option<codewhale_config::route::RouteLimits>,
) {
    let _ = engine_handle
        .send(Op::SetModel {
            model: compaction.model.clone(),
            mode,
            route_limits,
        })
        .await;
    let _ = engine_handle
        .send(Op::SetCompaction { config: compaction })
        .await;
}

/// Apply the choice made in the `/model` picker (#39): mutate App state so
/// the next turn uses the new model/effort, push the change to the running
/// engine via `Op::SetModel`/`Op::SetCompaction`, and surface a one-line
/// status describing what changed. Startup persistence is intentionally owned
/// by the picker's explicit Shift+D action in the view-event handler.
// The model/effort transition needs both the previous and next model+effort
// plus the engine, app, and config handles; bundling them into a struct here
// would only obscure a straightforward orchestration step.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_model_picker_choice(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    model: String,
    target_identity: Option<crate::config::ProviderIdentity>,
    effort: crate::reasoning_preference::ReasoningEffort,
    previous_model: String,
    previous_effort: crate::reasoning_preference::ReasoningEffort,
    save_as_startup_default: bool,
) {
    if app.reject_setting_change_while_busy(
        codewhale_localization::MessageId::SettingSubjectModelAndThinking,
    ) {
        note_startup_default_not_saved(app, save_as_startup_default);
        return;
    }
    let Some(target_identity) = target_identity else {
        app.push_status_toast(
            "The selected model has no admitted provider route.",
            StatusToastLevel::Error,
            Some(8_000),
        );
        note_startup_default_not_saved(app, save_as_startup_default);
        return;
    };
    if let Err(reason) = config.verify_provider_identity(&target_identity) {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        note_startup_default_not_saved(app, save_as_startup_default);
        return;
    }
    let target_provider = target_identity.provider;
    let target_key = target_identity.key.to_string();
    let model_is_auto = model.trim().eq_ignore_ascii_case("auto");
    let preserve_auto_effort =
        app.reasoning_effort_preference.is_some() || effort != previous_effort;
    if app.admitted_provider_identity().ok() != Some(&target_identity) {
        switch_provider(
            app,
            engine_handle,
            config,
            target_identity.clone(),
            (!model_is_auto).then_some(model.clone()),
        )
        .await;
        if app.api_provider != target_provider
            || app.provider_identity_for_persistence() != target_key
        {
            // The switch was refused (missing credentials, bad route). The
            // live route is still the old one, so persisting it as the startup
            // default would silently pin the route the user just tried to leave.
            note_startup_default_not_saved(app, save_as_startup_default);
            return;
        }
        if !model_is_auto {
            apply_picker_effort_choice(app, engine_handle, effort, previous_effort).await;
            if save_as_startup_default {
                app.status_message = Some(app.save_live_route_as_startup_default());
            }
            return;
        }
    }

    let current_identity = match app.admitted_provider_identity().cloned() {
        Ok(identity) => identity,
        Err(reason) => {
            app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
            note_startup_default_not_saved(app, save_as_startup_default);
            return;
        }
    };
    let model_changed = model != previous_model || app.auto_model != model_is_auto;
    let mut resolved_model = model.clone();
    let mut route_base_url = config.active_route_base_url();
    if !model_is_auto {
        match crate::route_runtime::resolve_runtime_route_for_identity(
            config,
            &current_identity,
            Some(&model),
        ) {
            Ok(resolution) => {
                resolved_model = resolution.candidate.wire_model_id().as_str().to_string();
                route_base_url = resolution.candidate.endpoint().base_url.clone();
                if model_changed {
                    app.set_active_context_window_override(config, &current_identity);
                    app.set_active_route_resolution(
                        route_base_url.clone(),
                        resolution.candidate.limits(),
                        resolution.context_window.source,
                    );
                }
            }
            Err(reason) => {
                app.status_message = Some(reason);
                note_startup_default_not_saved(app, save_as_startup_default);
                return;
            }
        }
    } else if model_changed {
        app.set_active_context_window_override(config, &current_identity);
        app.active_route_limits = app.context_window_override_limits();
        app.active_route_base_url = route_base_url.clone();
        app.active_context_window_source = app
            .configured_context_window_for(&app.model)
            .map(|resolution| resolution.source)
            .unwrap_or(crate::route_runtime::ContextWindowSource::Fallback);
    }

    let effective_effort = if model_is_auto {
        effort
    } else {
        effort.normalize_for_route(app.api_provider, &route_base_url, &resolved_model)
    };
    let effort_changed = effort != previous_effort;

    if model_changed {
        app.set_model_selection(resolved_model.clone());
        let provider_identity = app.provider_identity_for_persistence().to_string();
        app.provider_models
            .insert(provider_identity.clone(), resolved_model.clone());
        app.note_route_used(&provider_identity, &resolved_model);
        app.clear_model_scoped_telemetry();
    }
    let preference_changed = if model_is_auto && !preserve_auto_effort {
        app.reasoning_effort_preference.take().is_some()
    } else {
        let changed = app.reasoning_effort_preference != Some(effort);
        app.reasoning_effort_preference = Some(effort);
        changed
    };
    let live_effort_changed = effective_effort != app.reasoning_effort;
    if !model_is_auto || preserve_auto_effort {
        app.reasoning_effort = effective_effort;
    } else {
        app.reasoning_effort = ReasoningEffort::Auto;
    }
    if live_effort_changed || preference_changed {
        app.invalidate_route_receipts_for_reasoning_change();
    }
    if model_changed || live_effort_changed || preference_changed {
        app.update_model_compaction_budget();
    }

    // A model pick is session-local by default. Keep the exact live route in
    // memory and offer an explicit save decision; only Shift+D in the picker
    // writes a startup default.
    let route_provider = app.provider_identity_for_persistence().to_string();
    app.note_session_route_change(&route_provider, &resolved_model);

    if model_changed {
        apply_model_and_compaction_update(
            engine_handle,
            app.compaction_config(),
            app.mode,
            app.active_route_limits,
        )
        .await;
    }

    let model_summary = if model_is_auto {
        "auto (per-turn model)".to_string()
    } else {
        resolved_model.clone()
    };
    let previous_effort_summary = previous_effort.display_label_for_provider(app.api_provider);
    let applied_effort = app.reasoning_effort;
    let effort_summary = if applied_effort == ReasoningEffort::Auto {
        "auto (per-turn thinking)".to_string()
    } else {
        applied_effort
            .display_label_for_provider(app.api_provider)
            .to_string()
    };

    let summary = match (model_changed, effort_changed) {
        (true, true) => format!(
            "Model: {previous_model} → {model_summary} · thinking: {previous_effort_summary} → {effort_summary}"
        ),
        (true, false) => {
            format!("Model: {previous_model} → {model_summary} · thinking {effort_summary}")
        }
        (false, true) => format!(
            "Thinking: {previous_effort_summary} → {effort_summary} · model {model_summary}"
        ),
        (false, false) => {
            format!("Model unchanged: {model_summary} · thinking {effort_summary}")
        }
    };
    app.status_message = Some(summary);
    // Setup progress records that a concrete route was selected successfully;
    // it is a local receipt, not a claim that the route became the default.
    if model_changed || !model_is_auto {
        record_provider_model_setup_progress(app, config);
    }
    if save_as_startup_default {
        app.status_message = Some(app.save_live_route_as_startup_default());
    }
}

pub(crate) async fn apply_picker_effort_choice(
    app: &mut App,
    engine_handle: &EngineHandle,
    effort: ReasoningEffort,
    previous_effort: ReasoningEffort,
) {
    if app
        .reject_setting_change_while_busy(codewhale_localization::MessageId::SettingSubjectThinking)
    {
        return;
    }
    let effective_effort = if app.auto_model {
        effort
    } else {
        effort.normalize_for_route(app.api_provider, &app.active_route_base_url, &app.model)
    };
    let live_changed = effective_effort != app.reasoning_effort;
    let preference_changed = app.reasoning_effort_preference != Some(effort);
    let selection_changed = effort != previous_effort || live_changed;

    if live_changed || preference_changed {
        app.reasoning_effort = effective_effort;
        app.reasoning_effort_preference = Some(effort);
    }
    if selection_changed {
        app.invalidate_route_receipts_for_reasoning_change();
        app.update_model_compaction_budget();
    }

    let persist_warning = app
        .startup_defaults
        .apply_blocking(
            crate::tui::startup_defaults::StartupDefaults::reasoning_effort(effort.as_setting()),
        )
        .err()
        .map(|err| format!(" (not persisted: {err})"));

    if live_changed {
        apply_model_and_compaction_update(
            engine_handle,
            app.compaction_config(),
            app.mode,
            app.active_route_limits,
        )
        .await;
    }

    let persisted = persist_warning.is_none();
    let mut summary = if selection_changed {
        format!(
            "Thinking: {} → {} · model {}",
            previous_effort.display_label_for_provider(app.api_provider),
            effort.display_label_for_provider(app.api_provider),
            app.model_display_label()
        )
    } else {
        let mut summary = format!(
            "Thinking unchanged: {} · model {}",
            effort.display_label_for_provider(app.api_provider),
            app.model_display_label()
        );
        if persisted {
            summary.push_str(" · ");
            summary.push_str(&app.tr(codewhale_localization::MessageId::SavedAsStartupDefault));
        }
        summary
    };
    if let Some(warning) = persist_warning {
        summary.push_str(&warning);
    }
    app.status_message = Some(summary);
}

pub(crate) async fn apply_provider_fallback_switch(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    rollback: ProviderFallbackRollback,
) {
    let ProviderFallbackRollback {
        identity: previous_identity,
        chain: previous_chain,
    } = rollback;
    let previous_provider = previous_identity.provider;
    let target = app.api_provider;
    let previous_model = app.model.clone();

    let target_capture = match app.admitted_provider_identity().cloned() {
        Ok(identity) => identity,
        Err(reason) => {
            app.set_provider_identity_record(previous_identity);
            app.provider_chain = previous_chain;
            app.status_message = Some(reason);
            return;
        }
    };
    let resolved_route = match resolve_runtime_route_for_identity(config, &target_capture, None) {
        Ok(route) => route,
        Err(reason) => {
            app.set_provider_identity_record(previous_identity.clone());
            app.provider_chain = previous_chain.clone();
            app.last_fallback_reason = Some(format!(
                "Fallback provider {} route was rejected: {reason}",
                target.as_str()
            ));
            app.status_message = Some(format!(
                "Fallback provider {} rejected; provider remains {}.",
                target.as_str(),
                previous_provider.as_str()
            ));
            return;
        }
    };
    let target_identity = resolved_route.identity.clone();
    let resolved_endpoint = resolved_route.candidate.endpoint().base_url.clone();
    let next_config = resolved_route.config;
    let new_model = resolved_route.model;
    let context_window_source = resolved_route.context_window.source;

    if let Err(err) = CodewhaleClient::from_candidate(&next_config, &resolved_route.candidate) {
        app.set_provider_identity_record(previous_identity);
        app.provider_chain = previous_chain;
        app.last_fallback_reason = Some(format!(
            "Fallback provider {} was unavailable: {err}",
            target.as_str()
        ));
        app.status_message = Some(format!(
            "Fallback provider {} unavailable; provider remains {}.",
            target.as_str(),
            previous_provider.as_str()
        ));
        return;
    }
    *config = *next_config;
    app.refresh_notification_settings(config);
    app.set_provider_identity_record(target_identity.clone());
    app.billing_presentation = crate::route_billing::for_route(config, &target_identity);

    let new_base_url = resolved_endpoint;
    let new_endpoint = display_base_url_host(&new_base_url);
    let cache_scope_changed = previous_provider != target || previous_model != new_model;
    app.model_ids_passthrough = config.model_ids_pass_through();
    app.set_model_selection(new_model.clone());
    app.apply_provider_switch_reasoning_effort(target, &new_base_url, None);
    app.set_active_context_window_override(config, &target_identity);
    app.set_active_route_resolution(
        new_base_url.clone(),
        resolved_route.candidate.limits(),
        context_window_source,
    );
    app.update_model_compaction_budget();
    if cache_scope_changed {
        app.clear_model_scoped_telemetry();
    } else {
        app.session.last_prompt_tokens = None;
        app.session.last_completion_tokens = None;
    }

    let _ = engine_handle.send(Op::Shutdown).await;
    let engine_config = build_engine_config(app, config);
    *engine_handle = spawn_tui_engine(engine_config, config);

    if !app.api_messages.is_empty() {
        let _ = engine_handle
            .send(Op::SyncSession {
                session_id: app.current_session_id.clone(),
                messages: app.api_messages.as_ref().clone(),
                system_prompt: app.system_prompt.clone(),
                system_prompt_override: false,
                model: app.model.clone(),
                workspace: app.workspace.clone(),
                mode: app.mode,
            })
            .await;
    }
    let _ = engine_handle
        .send(Op::SetCompaction {
            config: app.compaction_config(),
        })
        .await;

    app.add_message(HistoryCell::System {
        content: format!(
            "Provider fallback: {} -> {}\nModel: {} -> {}\nEndpoint: {}",
            previous_provider.as_str(),
            target.as_str(),
            previous_model,
            new_model,
            new_endpoint
        ),
    });
    app.status_message = Some(format!(
        "Fallback provider: {} via {}",
        target.as_str(),
        new_endpoint
    ));
}

pub(super) fn reject_inline_inference_while_runtime_chat_owns_run(
    app: &mut App,
    result: &commands::CommandResult,
) -> bool {
    let blocked_inline_inference = matches!(
        result.action.as_ref(),
        Some(AppAction::VoiceCapture | AppAction::CacheWarmup)
    ) && app.remote_control.runtime_chat_blocks_local_dispatch();
    if !blocked_inline_inference {
        return false;
    }
    if matches!(result.action.as_ref(), Some(AppAction::VoiceCapture)) {
        // `/voice` toggles this before returning the action. Restore the state
        // so a retry after relay settlement starts capture rather than merely
        // toggling the stale flag off.
        app.voice_enabled = false;
    }
    let notice = app
        .tr(MessageId::SettingLockedDuringTurn)
        .replace("{setting}", "Codewhale Runtime");
    app.push_status_toast(notice, crate::tui::app::StatusToastLevel::Info, Some(6_000));
    true
}

pub(crate) fn apply_notification_update(
    app: &mut App,
    config: &mut Config,
    update: crate::config::NotificationConfigUpdate,
) -> Result<()> {
    let mut notifications = config.notifications_config();
    let setting = update.setting();
    notifications.apply_update(update).map_err(|_| {
        anyhow::anyhow!(
            app.tr(MessageId::ConfigCommandInvalidValue)
                .replace("{key}", &format!("notifications.{}", setting.key()))
                .replace("{value}", &app.tr(MessageId::ConfigUnavailable))
                .replace("{choices}", setting.choices())
        )
    })?;
    config.notifications = Some(notifications);
    app.refresh_notification_settings(config);
    Ok(())
}

/// Roll back only this idle Engine conversation, then require a durability
/// receipt before a retry can reach inference. A save failure leaves the
/// acknowledged undo visible, reports the error, and sends no replacement turn.
async fn apply_conversation_undo(
    app: &mut App,
    engine: &EngineHandle,
    sync: codewhale_command_contract::facets::SessionSyncPayload,
) -> Result<()> {
    anyhow::ensure!(
        !app.is_loading
            && !app.dispatch_in_flight
            && !app.remote_control.runtime_chat_blocks_local_dispatch(),
        "wait for the active turn to finish before undoing its conversation"
    );
    let before = engine.get_session_snapshot().await?;
    let before_prompt =
        crate::compaction::strip_compaction_summaries(before.system_prompt.as_ref());
    anyhow::ensure!(
        app.current_session_id
            .as_deref()
            .is_none_or(|id| id == before.session_id)
            && sync.session_id == app.current_session_id
            && sync.workspace == before.workspace
            && sync.model == before.model
            && before.mode == app.mode.as_setting()
            && crate::compaction::strip_compaction_summaries(app.system_prompt.as_ref())
                == before_prompt
            && crate::compaction::strip_compaction_summaries(sync.system_prompt.as_ref())
                == before_prompt
            && before.messages.as_slice() == app.api_messages.as_slice()
            && sync.messages.len() < before.messages.len()
            && before.messages.starts_with(&sync.messages),
        "{}",
        app.tr(MessageId::ConversationChangedBeforeUndo)
    );
    let id = before.session_id.clone();
    // A live rewind retains the checkpoint already owned by these messages.
    // The Engine alone restores it and invalidates dependent caches.
    let expected =
        crate::runtime_handoff::project_owned_messages_for_restore(sync.messages.clone());
    let (tx, receive) = tokio::sync::oneshot::channel();
    engine
        .send(Op::RewindConversation {
            expected: Box::new(before),
            messages: sync.messages,
            tx,
        })
        .await?;
    // This receipt comes from the same Engine operation that compares and
    // installs history. A queued update between preflight and rewind refuses.
    let installed = receive.await?.ok_or_else(|| {
        anyhow::anyhow!(
            app.tr(MessageId::ConversationChangedBeforeUndo)
                .into_owned()
        )
    })?;
    anyhow::ensure!(
        installed.session_id == id && installed.messages == expected,
        "the Engine did not acknowledge the conversation rollback"
    );
    while !app.history.is_empty() {
        let last_is_user = matches!(app.history.last(), Some(HistoryCell::User { .. }));
        app.pop_history();
        if last_is_user {
            break;
        }
    }
    app.set_api_messages(Arc::new(installed.messages));
    app.current_session_id = Some(id);
    app.tool_cells.clear();
    app.tool_details_by_cell.clear();
    app.exploring_entries.clear();
    app.ignored_tool_calls.clear();
    app.mark_history_updated();
    let manager = tokio::task::spawn_blocking(SessionManager::default_location).await??;
    let session = build_session_snapshot(app, &manager).map_err(anyhow::Error::msg)?;
    // CompletedCommit supersedes an older queued checkpoint as well as its
    // snapshot. A plain snapshot could let crash recovery revive the old turn.
    anyhow::ensure!(
        persistence_actor::try_persist(PersistRequest::CompletedCommit { session }),
        "the session persistence worker is unavailable"
    );
    let (reply, receive) = tokio::sync::oneshot::channel();
    anyhow::ensure!(
        persistence_actor::try_persist(PersistRequest::FlushAndReport { reply }),
        "could not request the session save receipt"
    );
    let report = receive.await?;
    anyhow::ensure!(
        report.failures.is_empty(),
        "the session save failed: {:?}",
        report.failures
    );
    publish_pending_work_projection(app)
        .await
        .map_err(anyhow::Error::msg)?;
    Ok(())
}

// The event loop awaits this dispatcher at several call sites. In debug
// builds, embedding its entire state machine at each site gives the caller
// separate large stack slots even though only one action runs at a time.
// Construct it here so callers carry one pointer, as modal dispatch already does.
pub(crate) fn apply_command_result<'a>(
    terminal: &'a mut AppTerminal,
    app: &'a mut App,
    engine_handle: &'a mut EngineHandle,
    task_manager: &'a SharedTaskManager,
    config: &'a mut Config,
    result: commands::CommandResult,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<bool>> + 'a>> {
    Box::pin(async move {
        let outcome =
            apply_command_result_inner(terminal, app, engine_handle, task_manager, config, result)
                .await;
        // A save the command made may have moved legacy top-level `base_url` /
        // `api_key` into their provider tables (#6394); say so once.
        for notice in codewhale_config::legacy_root::take_notices() {
            app.push_status_toast(notice, StatusToastLevel::Info, Some(10_000));
        }
        outcome
    })
}

async fn apply_command_result_inner(
    terminal: &mut AppTerminal,
    app: &mut App,
    engine_handle: &mut EngineHandle,
    task_manager: &SharedTaskManager,
    config: &mut Config,
    result: commands::CommandResult,
) -> Result<bool> {
    // These two actions await participant inference inline on the UI event
    // loop. Waiting behind Runtime Chat's exclusive writer here would
    // deadlock: this same loop must drain the terminal projection/server
    // cursor that releases the writer. Fail closed before displaying the
    // command's optimistic message or invoking recorder/provider code.
    if reject_inline_inference_while_runtime_chat_owns_run(app, &result) {
        return Ok(false);
    }
    let conversation_message = matches!(result.action, Some(AppAction::ConversationUndo { .. }))
        .then(|| result.message.clone())
        .flatten();
    if let Some(msg) = result.message
        && !matches!(
            result.action,
            Some(AppAction::OpenCommandReview { .. } | AppAction::ConversationUndo { .. })
        )
    {
        app.add_message(HistoryCell::System { content: msg });
    }

    if let Some(action) = result.action {
        match action {
            AppAction::Quit => {
                let _ = engine_handle.send(Op::Shutdown).await;
                return Ok(true);
            }
            AppAction::LoadSession(path) => {
                // Session files can be large; this is the UI action path, so
                // the read must not park a Tokio worker (blocking-call
                // convention, #6149).
                let parsed: SavedSession = match tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|err| err.to_string())
                    .and_then(|raw| serde_json::from_str(&raw).map_err(|err| err.to_string()))
                {
                    Ok(session) => session,
                    Err(err) => {
                        crate::tui::ui::session_state::surface_session_load_failure(
                            app,
                            format!("Failed to load session from {}: {err}", path.display()),
                        );
                        return Ok(false);
                    }
                };
                // `/load` shares the attach contract of `resume` and the
                // picker (`SessionManager::attach_session_file`); the lease is
                // committed only once the session is applied.
                let attached = SessionManager::default_location()
                    .and_then(|manager| manager.attach_session_file(parsed, &path));
                let (session, lease) = match attached {
                    Ok(attached) => attached,
                    Err(err) => {
                        crate::tui::ui::session_state::surface_session_load_failure(
                            app,
                            format!("Failed to resume session {}: {err}", path.display()),
                        );
                        return Ok(false);
                    }
                };
                let fresh_config =
                    match Config::load(app.config_path.clone(), app.config_profile.as_deref()) {
                        Ok(config) => config,
                        Err(err) => {
                            crate::tui::ui::session_state::surface_session_load_failure(
                                app,
                                format!("Failed to load live config for session restore: {err}"),
                            );
                            return Ok(false);
                        }
                    };
                let resumed_id = session.metadata.id.clone();
                let title = crate::session_manager::sanitize_session_title(&session.metadata.title);
                crate::runtime_threads::prepare_canonical_sessions_root().await;
                let respawn = match apply_loaded_session_config_snapshot(
                    app,
                    config,
                    session,
                    fresh_config,
                    true,
                ) {
                    Ok(outcome) => {
                        lease.commit();
                        outcome
                    }
                    Err(err) => {
                        crate::tui::ui::session_state::surface_session_load_failure(
                            app,
                            format!("Failed to restore session: {err}"),
                        );
                        return Ok(false);
                    }
                };
                sync_runtime_workspace_state(task_manager, app.workspace.clone()).await;
                if respawn {
                    let _ = engine_handle.send(Op::Shutdown).await;
                    *engine_handle = spawn_tui_engine(build_engine_config(app, config), config);
                } else {
                    let _ = engine_handle
                        .send(Op::SetModel {
                            model: app.model.clone(),
                            mode: app.mode,
                            route_limits: app.active_route_limits,
                        })
                        .await;
                }
                let _ = engine_handle
                    .send(Op::SyncSession {
                        session_id: app.current_session_id.clone(),
                        messages: app.api_messages.as_ref().clone(),
                        system_prompt: app.system_prompt.clone(),
                        system_prompt_override: false,
                        model: app.model.clone(),
                        workspace: app.workspace.clone(),
                        mode: app.mode,
                    })
                    .await;
                let _ = engine_handle
                    .send(Op::SetCompaction {
                        config: app.compaction_config(),
                    })
                    .await;
                // Restore may have queued a legacy configuration notice.
                // Admit it first so the confirmed resume remains the latest
                // toast instead of being immediately covered on the next draw.
                app.sync_status_message_to_toasts();
                app.push_status_toast_record(
                    StatusToast::new(
                        app.tr(MessageId::SessionsResumed)
                            .replace("{title}", &title),
                        StatusToastLevel::Success,
                        Some(4_000),
                    )
                    .for_event(format!("session-resumed:{resumed_id}")),
                );
                // A loaded session is the working screen. The launch card's
                // recent rows reach here through `/resume`-shaped dispatch;
                // leaving the launch stage visible over the restored
                // transcript is what made those rows read as dead (#4).
                app.launch.dismiss();
                app.launch.status = None;
            }
            AppAction::SyncSession {
                session_id,
                messages,
                system_prompt,
                model,
                workspace,
                mode,
            } => {
                let mut session_id = session_id;
                let is_full_reset = messages.is_empty() && system_prompt.is_none();
                if is_full_reset && session_id.is_none() {
                    let new_session_id = uuid::Uuid::new_v4().to_string();
                    session_id = Some(new_session_id);
                }
                if let Some(session_id) = session_id.as_deref() {
                    let transition = match prepare_offline_queue_transition(app, session_id) {
                        Ok(transition) => transition,
                        Err(error) => {
                            app.push_status_toast(error, StatusToastLevel::Error, Some(6_000));
                            return Ok(false);
                        }
                    };
                    install_offline_queue_transition(app, transition);
                }
                let workspace_changed = task_manager.default_workspace().await != workspace;
                if workspace_changed {
                    apply_workspace_runtime_state(app, config, workspace.clone());
                    sync_runtime_workspace_state(task_manager, workspace.clone()).await;
                }
                let identity =
                    match app
                        .admitted_provider_identity()
                        .cloned()
                        .and_then(|identity| {
                            config.verify_provider_identity(&identity)?;
                            Ok(identity)
                        }) {
                        Ok(identity) => identity,
                        Err(reason) => {
                            app.status_message = Some(format!(
                                "Failed to restore saved session provider: {reason}"
                            ));
                            return Ok(false);
                        }
                    };
                let provider_changed =
                    config.active_provider_identity().as_ref().ok() != Some(&identity);
                if provider_changed {
                    restore_loaded_session_provider(app, config, identity.clone())
                        .map_err(anyhow::Error::msg)?;
                    config.set_provider_model_override(&identity, Some(model.clone()))?;
                    let prepared = config
                        .active_provider_identity()
                        .map_err(anyhow::Error::msg)?;
                    app.set_provider_identity_record(prepared);
                }
                // Re-resolve from the live config even when the provider did
                // not change. The command layer intentionally has no Config
                // handle, so its provisional limits cannot include current
                // provider overrides.
                resolve_loaded_session_route(app, config);
                app.update_model_compaction_budget();
                if provider_changed || workspace_changed {
                    let _ = engine_handle.send(Op::Shutdown).await;
                    *engine_handle = spawn_tui_engine(build_engine_config(app, config), config);
                }
                // SyncSession carries the conversation but not resolved route
                // limits. Refresh the engine's model first so a loaded,
                // forked, or freshly reset session cannot retain the previous
                // route's context/output facts.
                let _ = engine_handle
                    .send(Op::SetModel {
                        model: model.clone(),
                        mode,
                        route_limits: app.active_route_limits,
                    })
                    .await;
                let _ = engine_handle
                    .send(Op::SyncSession {
                        session_id,
                        messages,
                        system_prompt,
                        system_prompt_override: false,
                        model,
                        workspace,
                        mode,
                    })
                    .await;
                let _ = engine_handle
                    .send(Op::SetCompaction {
                        config: app.compaction_config(),
                    })
                    .await;
                if is_full_reset {
                    persist_full_reset_snapshot(app);
                }
            }
            AppAction::SetWorkspaceTrust { trusted, save } => {
                let result = crate::commands::set_workspace_trust(app, trusted, save).await;
                sync_mode_update(app, engine_handle).await;
                match result {
                    Ok(()) => {
                        app.push_status_toast(
                            format!(
                                "/trust: {} ({})",
                                tr(
                                    app.ui_locale,
                                    if trusted {
                                        MessageId::ConfigValueOn
                                    } else {
                                        MessageId::ConfigValueOff
                                    }
                                ),
                                tr(
                                    app.ui_locale,
                                    if save {
                                        MessageId::ConfigScopeSaved
                                    } else {
                                        MessageId::ConfigScopeSession
                                    }
                                ),
                            ),
                            StatusToastLevel::Info,
                            None,
                        );
                        // The toast fades and the footer does not show trust
                        // mode: leave a line that says what changed.
                        app.add_message(HistoryCell::System {
                            content: crate::commands::trust_change_note(trusted, save),
                        });
                    }
                    Err(error) => app.push_status_toast(
                        tr(app.ui_locale, MessageId::AutomationEditorSaveFailed)
                            .replace("{error}", &format!("/trust: {error:#}")),
                        StatusToastLevel::Error,
                        None,
                    ),
                }
            }
            AppAction::ModeChanged(_mode) => {
                sync_mode_update(app, engine_handle).await;
            }
            AppAction::ApprovalPolicyPersisted { policy } => {
                config.approval_policy = policy;
                sync_mode_update(app, engine_handle).await;
            }
            AppAction::PermissionRulesChanged => {
                match codewhale_config::load_permissions_snapshot(app.config_path.clone()) {
                    Ok(snapshot) => {
                        let ruleset = snapshot.permissions().ruleset();
                        // Config and every running EngineConfig share this
                        // policy store. Publish once: replaying an older Op
                        // after a later edit would roll the live policy back.
                        config.exec_policy_engine.set_ruleset(ruleset);
                    }
                    Err(error) => {
                        app.status_message = Some(
                            tr(app.ui_locale, MessageId::PermissionsOperationFailed)
                                .replace("{error}", &format!("{error:#}")),
                        );
                    }
                }
            }
            AppAction::PluginRegistryChanged => {
                // Revoke a disabled or untrusted plugin's host code now, not at
                // the next turn's rebuild.
                crate::extension_host::plugins_changed(std::sync::Arc::clone(&app.plugin_registry));
                let command_errors = crate::commands::user_registry::install_plugin_registry(
                    &app.workspace,
                    app.plugin_registry.as_ref(),
                );
                app.hooks = app.hooks.rebind(
                    crate::hooks::HooksConfig::load_with_project_and_plugins(
                        config.hooks_config(),
                        &app.workspace,
                        Some(app.plugin_registry.as_ref()),
                    ),
                    app.workspace.clone(),
                );
                app.runtime_services.hook_executor = Some(std::sync::Arc::new(app.hooks.clone()));
                if !command_errors.is_empty() {
                    app.set_sticky_status(
                        format!(
                            "Plugin runtime activation failed: {}",
                            command_errors.join("; ")
                        ),
                        StatusToastLevel::Error,
                        None,
                    );
                }
                let _ = engine_handle.send(Op::Shutdown).await;
                *engine_handle = spawn_tui_engine(build_engine_config(app, config), config);
                if !app.api_messages.is_empty() {
                    let _ = engine_handle
                        .send(Op::SyncSession {
                            session_id: app.current_session_id.clone(),
                            messages: app.api_messages.as_ref().clone(),
                            system_prompt: app.system_prompt.clone(),
                            system_prompt_override: false,
                            model: app.model.clone(),
                            workspace: app.workspace.clone(),
                            mode: app.mode,
                        })
                        .await;
                }
            }
            AppAction::ConversationUndo {
                sync,
                retry_input,
                edit_replacement,
            } => {
                if let Err(error) = apply_conversation_undo(app, engine_handle, sync).await {
                    // A refused `/edit` rollback has already consumed the
                    // revision from the composer: hand it back, with edit mode
                    // re-armed, so the user can retry instead of retyping.
                    if edit_replacement && let Some(content) = retry_input {
                        let restored = build_queued_message(app, content);
                        restore_failed_immediate_submit(app, restored, &error);
                        app.edit_in_progress = true;
                    }
                    app.push_status_toast(
                        format!("Conversation rollback failed; retry was not sent: {error:#}"),
                        StatusToastLevel::Error,
                        None,
                    );
                    return Ok(false);
                }
                if let Some(message) = conversation_message {
                    app.add_message(HistoryCell::System { content: message });
                }
                if let Some(content) = retry_input {
                    let queued = build_queued_message(app, content);
                    // The rollback required an idle app, so this always resolves to an
                    // immediate send; the disposition is kept for parity with `SendMessage`.
                    dispatch_composer_message(
                        app,
                        config,
                        engine_handle,
                        queued,
                        DispatchRecovery::Immediate,
                        ComposerSubmitAction::Submit(app.decide_submit_disposition()),
                    )
                    .await?;
                }
            }
            AppAction::RunExtensionCommand {
                command,
                name,
                input,
            } => {
                use crate::extension_host::command::CommandOutcome;
                // The origin labels the output: it is the plugin's text, not
                // Codewhale's.
                let origin = command.origin.clone();
                app.status_message = Some(format!("Running /{name} ({origin})..."));
                // Awaited here like `/balance`; the call is bounded by its
                // deadline and cancelled in the host when it expires.
                match crate::extension_host::run_command_for_plugins(
                    &command,
                    &input,
                    app.current_session_id.as_deref(),
                    app.extension_plugin_view().as_ref(),
                )
                .await
                {
                    Ok(CommandOutcome::Show { text }) => {
                        if text.trim().is_empty() {
                            app.status_message = Some(format!("/{name} completed"));
                        } else {
                            app.status_message = None;
                            app.add_message(HistoryCell::System {
                                content: format!("/{name} ({origin})\n{text}"),
                            });
                        }
                    }
                    Ok(CommandOutcome::Submit { prompt, note }) => {
                        app.status_message = None;
                        let mut content = format!("/{name} ({origin}) submitted a prompt");
                        if let Some(note) = note {
                            content.push_str(&format!("\n{note}"));
                        }
                        app.add_message(HistoryCell::System { content });
                        let queued = build_queued_message(app, prompt);
                        dispatch_composer_message(
                            app,
                            config,
                            engine_handle,
                            queued,
                            DispatchRecovery::Immediate,
                            ComposerSubmitAction::Submit(app.decide_submit_disposition()),
                        )
                        .await?;
                    }
                    Err(error) => {
                        app.status_message = None;
                        app.add_message(HistoryCell::System {
                            content: format!("Error: /{name} ({origin}): {error}"),
                        });
                    }
                }
            }
            AppAction::SendMessage(content) => {
                let queued = build_queued_message(app, content);
                dispatch_composer_message(
                    app,
                    config,
                    engine_handle,
                    queued,
                    DispatchRecovery::Immediate,
                    ComposerSubmitAction::Submit(app.decide_submit_disposition()),
                )
                .await?;
            }
            AppAction::WorkflowInstruction {
                display,
                instruction,
            } => {
                let queued = QueuedMessage::new(display, Some(instruction));
                dispatch_composer_message(
                    app,
                    config,
                    engine_handle,
                    queued,
                    DispatchRecovery::Immediate,
                    ComposerSubmitAction::Submit(app.decide_submit_disposition()),
                )
                .await?;
            }
            AppAction::SetGoalStatus { status, clear } => {
                accept_goal_control(
                    app,
                    engine_handle,
                    GoalControlIntent::SetStatus { status, clear },
                );
            }
            AppAction::SetGoalObjective {
                objective,
                token_budget,
            } => {
                accept_goal_control(
                    app,
                    engine_handle,
                    GoalControlIntent::SetObjective {
                        objective,
                        token_budget,
                    },
                );
            }
            AppAction::OpenTextPager { title, content } => {
                open_text_pager(app, title, content);
            }
            AppAction::OpenDiffPager { title, diff } => {
                open_diff_pager(app, title, &diff);
                app.needs_redraw = true;
            }
            AppAction::OpenCommandReview {
                title,
                content,
                command,
            } => {
                let width = app
                    .viewport
                    .last_transcript_area
                    .map_or(80, |area| area.width);
                app.view_stack
                    .push(crate::tui::pager::PagerView::command_review(
                        title,
                        &content,
                        width.saturating_sub(2),
                        command,
                        app.ui_locale,
                    ));
                app.needs_redraw = true;
            }
            AppAction::VoiceCapture => {
                use commands::voice::VoiceCaptureOutcome;
                match commands::voice::capture_and_transcribe(app, config).await {
                    Ok(VoiceCaptureOutcome::Insert(text)) => {
                        app.insert_str(&text);
                        app.status_message = Some(format!(
                            "{}: {text}",
                            tr(app.ui_locale, MessageId::VoiceTranscribed)
                        ));
                    }
                    Ok(VoiceCaptureOutcome::Send(content)) => {
                        app.status_message =
                            Some(tr(app.ui_locale, MessageId::VoiceTranscribed).to_string());
                        let queued = build_queued_message(app, content);
                        dispatch_composer_message(
                            app,
                            config,
                            engine_handle,
                            queued,
                            DispatchRecovery::Immediate,
                            ComposerSubmitAction::Submit(app.decide_submit_disposition()),
                        )
                        .await?;
                    }
                    Err(err) => {
                        app.voice_enabled = false;
                        app.status_message = Some(err);
                    }
                }
            }
            AppAction::ListSubAgents => {
                // #3802: non-blocking send — refresh op, safe to drop.
                let _ = engine_handle.try_send(Op::ListSubAgents);
            }
            AppAction::PreviewOutboundRequest {
                json,
                base_prompt_only,
                hypothetical_prompt,
            } => {
                // Split of authority: the host resolves the next turn's route
                // with the same planner it would use to send one, and the
                // engine — the only place that can rebuild the tool catalog,
                // MCP state, gates, system prompt, and prepared body — turns
                // that plan into a manifest.
                let inputs =
                    build_preview_request_inputs(app, config, engine_handle, hypothetical_prompt)
                        .await;
                // #6150: the input path never awaits a full op channel; a
                // rejected preview is reported and retryable.
                if let Err(err) = engine_handle.try_send(Op::PreviewOutboundRequest {
                    inputs: Box::new(inputs),
                    json,
                    base_prompt_only,
                }) {
                    app.status_message = Some(format!("Cannot preview request: {err}"));
                }
            }
            AppAction::CancelSubAgent { agent_id } => {
                app.status_message = Some(format!("Cancelling {agent_id}..."));
                if engine_handle
                    .try_send(Op::CancelSubAgent {
                        agent_id: agent_id.clone(),
                    })
                    .is_err()
                {
                    app.status_message = Some(format!("Could not cancel {agent_id}"));
                }
            }
            AppAction::RouterSetup { request } => {
                crate::tui::views::router_setup::handle_router_request(
                    app,
                    config,
                    task_manager,
                    request,
                )
                .await;
            }
            AppAction::FetchBalance => {
                let provider = app.api_provider;
                if !crate::config::provider_has_balance_api(provider) {
                    app.add_message(HistoryCell::System {
                        content: format!(
                            "Balance check is not supported for {} yet. Check the provider dashboard for account balance details.",
                            provider.provider().display_name()
                        ),
                    });
                } else {
                    let api_key = config.active_route_api_key().unwrap_or_default();
                    if api_key.trim().is_empty() {
                        app.add_message(HistoryCell::System {
                            content: format!(
                                "No API key configured for {}.",
                                provider.provider().display_name()
                            ),
                        });
                    } else {
                        let base_url = config.active_route_base_url();
                        match fetch_provider_balance(provider, &api_key, &base_url).await {
                            Some(info) => {
                                if let Ok(mut guard) =
                                    balance_cell_for_route(app, provider, &api_key, &base_url)
                                        .lock()
                                {
                                    *guard = Some(info.clone());
                                }
                                app.last_balance_fetch = Some(Instant::now());
                                app.add_message(HistoryCell::System {
                                    content: info.report(provider.provider().display_name()),
                                });
                            }
                            None => {
                                let fallback = app
                                    .balance_cell
                                    .lock()
                                    .ok()
                                    .and_then(|guard| guard.clone())
                                    .and_then(|info| {
                                        info.chip_label().map(|amount| {
                                            format!(
                                                "Could not refresh {} balance; last known: {amount}",
                                                provider.provider().display_name()
                                            )
                                        })
                                    });
                                app.add_message(HistoryCell::System {
                                    content: fallback.unwrap_or_else(|| {
                                        format!(
                                            "Could not fetch {} account balance. Check the provider dashboard.",
                                            provider.provider().display_name()
                                        )
                                    }),
                                });
                            }
                        }
                    }
                }
            }
            AppAction::FetchModels => {
                app.status_message = Some("Fetching models...".to_string());
                match fetch_available_models(config).await {
                    Ok(models) => {
                        app.add_message(HistoryCell::System {
                            content: format_helpers::available_models_message(
                                app.ui_locale,
                                app.provider_identity_for_persistence(),
                                &app.model,
                                &models,
                                &crate::fleet::members::fleet_models(&app.workspace),
                            ),
                        });
                        app.status_message = Some(format!("Found {} model(s)", models.len()));
                    }
                    Err(error) => {
                        app.add_message(HistoryCell::System {
                            content: format!(
                                "Failed to fetch models from {}: {error}",
                                config
                                    .active_provider_identity()
                                    .ok()
                                    .as_ref()
                                    .map(|identity| identity
                                        .compatibility()
                                        .map(|row| row.label)
                                        .unwrap_or(identity.key.as_str()))
                                    .unwrap_or("unavailable")
                            ),
                        });
                    }
                }
            }
            AppAction::RefreshModelsDevCatalog => {
                app.status_message = Some("Refreshing Models.dev catalog...".to_string());
                let message = match crate::models_dev_live::refresh(true).await {
                    Ok(count) => {
                        let status = crate::models_dev_live::status();
                        let source = if status.source_label.is_empty() {
                            "unknown"
                        } else {
                            status.source_label.as_str()
                        };
                        format!(
                            "Models.dev catalog refreshed: {count} offerings ({:?}, source {source})",
                            status.freshness
                        )
                    }
                    Err(err) => {
                        let status = crate::models_dev_live::status();
                        format!(
                            "Models.dev refresh failed ({err}); keeping prior/bundled rows ({} offerings, {:?})",
                            status.offering_count, status.freshness
                        )
                    }
                };
                app.add_message(HistoryCell::System {
                    content: message.clone(),
                });
                app.status_message = Some(message);
                // `/model refresh` also forces the cloud facts overlay when the
                // (off-by-default) channel is enabled.
                let cloud_settings = config.cloud_facts_config().settings();
                codewhale_cloud_facts::configure(&cloud_settings);
                if cloud_settings.enabled {
                    let now = codewhale_config::catalog::now_unix();
                    let cloud = match codewhale_cloud_facts::refresh(&cloud_settings, true).await {
                        Ok(outcome) => {
                            format!(
                                "Cloud facts refreshed: {outcome:?} ({})",
                                codewhale_cloud_facts::status().label(now)
                            )
                        }
                        Err(err) => format!(
                            "Cloud facts refresh failed ({err}); {}",
                            codewhale_cloud_facts::status().label(now)
                        ),
                    };
                    app.add_message(HistoryCell::System { content: cloud });
                }
            }
            AppAction::CacheWarmup => {
                app.status_message = Some("Warming prompt cache...".to_string());
                match run_cache_warmup(app, config).await {
                    Ok(outcome) => {
                        app.session.last_base_url = Some(outcome.base_url.clone());
                        app.session.last_warmup_key = Some(CacheWarmupKey::from_inspection(
                            &outcome.provider_identity,
                            &outcome.model,
                            &outcome.base_url,
                            &outcome.inspection,
                        ));
                        let mut message = format_helpers::cache_warmup_result(&outcome.usage);
                        if let Some(key) = app.session.last_warmup_key.as_ref() {
                            message.push_str(&format!("\nWarmup key: {}", key.hash_short()));
                        }
                        // Append prefix-cache stability info.
                        if app.prefix_checks_total > 0 {
                            let changes = app.prefix_change_count;
                            let total = app.prefix_checks_total;
                            let stable = total.saturating_sub(changes);
                            let pct = app
                                .prefix_stability_pct
                                .map(|p| format!("{p}%"))
                                .unwrap_or_else(|| "--".to_string());
                            message.push_str(&format!(
                                "\n\nPrefix stability: {pct} ({stable}/{total} checks stable, {changes} change{})",
                                if changes == 1 { "" } else { "s" }
                            ));
                            if let Some(ref desc) = app.last_prefix_change_desc {
                                message.push_str(&format!("\nLast prefix change: {desc}"));
                            }
                        }
                        app.add_message(HistoryCell::System { content: message });
                        app.status_message = Some("Cache warmup complete".to_string());
                    }
                    Err(error) => {
                        app.add_message(HistoryCell::System {
                            content: format!("Cache warmup failed: {error}"),
                        });
                        app.status_message = Some("Cache warmup failed".to_string());
                    }
                }
            }
            AppAction::SwitchProvider { provider, model } => {
                let identity = match config.resolve_provider_selection_identity(provider.as_str()) {
                    Ok(identity) => identity,
                    Err(reason) => {
                        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
                        return Ok(false);
                    }
                };
                switch_provider(app, engine_handle, config, identity, model).await;
                let api_key = config.active_route_api_key().unwrap_or_default();
                let base_url = config.active_route_base_url();
                schedule_balance_fetch(app, &api_key, &base_url, false);
            }
            AppAction::SwitchModelRoute { identity, model } => {
                if let Err(reason) = config.verify_provider_identity(&identity) {
                    app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
                    return Ok(false);
                }
                let previous_model = if app.auto_model {
                    "auto".to_string()
                } else {
                    app.model.clone()
                };
                // Hotbar route actions do not carry an effort choice. Preserve
                // the raw global preference instead of feeding a fixed
                // route's normalized live tier back through the picker path.
                let previous_effort = app
                    .reasoning_effort_preference
                    .unwrap_or(app.reasoning_effort);
                apply_model_picker_choice(
                    app,
                    engine_handle,
                    config,
                    model,
                    Some(identity),
                    previous_effort,
                    previous_model,
                    previous_effort,
                    // A hotbar route switch is a session action, not a
                    // statement about what the next launch should open with.
                    false,
                )
                .await;
            }
            AppAction::UpdateCompaction(compaction) => {
                if app.is_loading || app.is_compacting {
                    let queued = try_apply_model_and_compaction_update(
                        engine_handle,
                        compaction,
                        app.mode,
                        app.active_route_limits,
                    );
                    app.status_message = Some(if queued {
                        "Config change queued; the active turn remains responsive.".to_string()
                    } else {
                        "Config change deferred; it will apply to the next turn.".to_string()
                    });
                } else {
                    apply_model_and_compaction_update(
                        engine_handle,
                        compaction,
                        app.mode,
                        app.active_route_limits,
                    )
                    .await;
                }
            }
            AppAction::UpdateStreamChunkTimeout(timeout_secs) => {
                // #6150: the input path never awaits a full op channel.
                if engine_handle
                    .try_send(Op::SetStreamChunkTimeout { timeout_secs })
                    .is_err()
                {
                    app.status_message =
                        Some("Engine busy — setting not applied; try again".to_string());
                }
            }
            AppAction::UpdateSubagentRuntimeConfig {
                enabled,
                max_subagents,
                launch_concurrency,
                max_spawn_depth,
                api_timeout_secs,
                heartbeat_timeout_secs,
            } => {
                if engine_handle
                    .try_send(Op::SetSubagentRuntimeConfig {
                        enabled,
                        max_subagents,
                        launch_concurrency,
                        max_spawn_depth,
                        api_timeout_secs,
                        heartbeat_timeout_secs,
                    })
                    .is_err()
                {
                    app.status_message =
                        Some("Engine busy — setting not applied; try again".to_string());
                }
            }
            AppAction::UpdateSearchProvider { provider } => {
                // Reserve before committing the config change so a full
                // channel cannot desync the engine from it.
                match engine_handle.tx_op.clone().try_reserve_owned() {
                    Ok(permit) => {
                        let effective_provider = config.set_search_provider(provider);
                        engine_handle.send_reserved_op(
                            permit,
                            Op::SetSearchProvider {
                                provider: effective_provider,
                            },
                        );
                    }
                    Err(_) => {
                        app.status_message =
                            Some("Engine busy — provider not applied; try again".to_string());
                    }
                }
            }
            AppAction::UpdatePromptSuggestion { enabled } => {
                config.prompt_suggestion = Some(enabled);
            }
            AppAction::UpdateNotification { update } => {
                if let Err(error) = apply_notification_update(app, config, update) {
                    app.push_status_toast(error.to_string(), StatusToastLevel::Error, Some(6_000));
                }
            }
            AppAction::SetAdvisorEnabled { enabled } => {
                if engine_handle
                    .try_send(Op::SetAdvisorEnabled { enabled })
                    .is_err()
                {
                    app.status_message =
                        Some("Engine busy — setting not applied; try again".to_string());
                }
            }
            AppAction::OpenConfigView => {
                if app.view_stack.top_kind() != Some(ModalKind::Config) {
                    app.view_stack.push(ConfigView::new_for_app(app));
                }
            }
            AppAction::OpenWorktreeManager => {
                if app.view_stack.top_kind() != Some(ModalKind::WorktreeManager) {
                    // Non-blocking: git_status caches; manager never shells on paint.
                    crate::git_status::refresh_if_stale(&app.workspace);
                    app.view_stack
                        .push(crate::tui::worktree_manager::WorktreeManagerView::new(
                            app.workspace.clone(),
                        ));
                }
            }
            AppAction::OpenModelPicker => {
                if app.view_stack.top_kind() != Some(ModalKind::ModelPicker) {
                    // Slash `/model` and the picker share one grammar: the
                    // composer must not keep a leftover `/model` buffer
                    // (or a held paste-burst) under the picker query.
                    app.clear_input();
                    app.paste_burst.clear_after_explicit_paste();
                    app.view_stack
                        .push(crate::tui::model_picker::ModelPickerView::new(app, config));
                }
            }
            AppAction::OpenProviderPicker => {
                open_provider_picker(app, config, engine_handle).await;
            }
            AppAction::OpenProviderSetup { provider } => {
                if app.view_stack.top_kind() != Some(ModalKind::ProviderPicker) {
                    let runtime_status = query_provider_runtime_status(engine_handle).await;
                    app.view_stack.push(
                        crate::tui::provider_picker::ProviderPickerView::new_for_setup(
                            app.api_provider,
                            provider,
                            config,
                            runtime_status,
                        )
                        .with_locale(app.ui_locale)
                        .with_provider_health(&app.provider_health),
                    );
                    app.status_message = Some("Provider setup catalog opened.".to_string());
                }
            }
            AppAction::OpenDs4Setup => {
                if app.view_stack.top_kind() != Some(ModalKind::ProviderPicker) {
                    let runtime_status = query_provider_runtime_status(engine_handle).await;
                    app.view_stack.push(
                        crate::tui::provider_picker::ProviderPickerView::new_for_ds4_setup(
                            app.api_provider,
                            config,
                            runtime_status,
                        )
                        .with_locale(app.ui_locale)
                        .with_provider_health(&app.provider_health),
                    );
                }
            }
            AppAction::EditProjectHooks => {
                edit_project_hooks_from_tui(terminal, app, config);
            }
            AppAction::StartXaiDeviceLogin => {
                let _switched =
                    run_xai_device_login_from_tui(terminal, app, engine_handle, config).await?;
            }
            AppAction::StartClaudeLogin => {
                let _ = run_claude_login_from_tui(terminal, app, engine_handle, config).await?;
            }
            AppAction::StartClaudeRevoke => {
                let path = app.config_path.clone();
                let result = tokio::task::spawn_blocking(move || {
                    crate::oauth::revoke_owned_login(
                        crate::oauth::OAuthProvider::Claude,
                        path.as_deref(),
                        None,
                    )
                })
                .await
                .map_err(|error| anyhow::anyhow!("Claude sign-out worker failed: {error}"))
                .and_then(|result| result);
                if result.is_ok() {
                    let identity = config
                        .builtin_provider_identity(ProviderKind::Anthropic)
                        .map_err(anyhow::Error::msg)?;
                    let entry = config.provider_config_for_mut(&identity)?;
                    entry.oauth_credential_generation = None;
                }
                let (message, level) = match result {
                    Ok(()) => (
                        "Removed Codewhale's saved Claude sign-in.".to_string(),
                        StatusToastLevel::Info,
                    ),
                    Err(error) => (
                        format!("Claude sign-out failed: {error}"),
                        StatusToastLevel::Error,
                    ),
                };
                app.push_status_toast(message, level, Some(8_000));
            }
            AppAction::StartChatgptPkceLogin => {
                let _switched =
                    run_chatgpt_pkce_login_from_tui(terminal, app, engine_handle, config).await?;
            }
            AppAction::StartChatgptRevoke => {
                run_chatgpt_revoke_from_tui(app, config).await;
            }
            AppAction::StartPluginLogin { provider } => {
                run_plugin_oauth_from_tui(terminal, app, config, provider, false).await?;
            }
            AppAction::StartPluginLogout { provider } => {
                run_plugin_oauth_from_tui(terminal, app, config, provider, true).await?;
            }
            AppAction::StartOrcarouterPkceLogin => {
                let _switched =
                    run_orcarouter_pkce_login_from_tui(terminal, app, engine_handle, config)
                        .await?;
            }
            AppAction::StartOrcarouterRevoke => {
                run_orcarouter_revoke_from_tui(app, config).await;
            }
            AppAction::SetScreenMode(mode) => {
                // The terminal transition is the only fallible part; a failed
                // probe leaves the previous screen live and says why.
                match switch_screen_mode(terminal, app, mode) {
                    Ok(()) => {
                        let screen = match mode {
                            crate::tui::app::ScreenMode::Fullscreen => {
                                app.tr(MessageId::ScreenModeFullscreenNotice)
                            }
                            crate::tui::app::ScreenMode::Inline => {
                                app.tr(MessageId::ScreenModeInlineNotice)
                            }
                        };
                        let capture = if app.use_mouse_capture {
                            app.tr(MessageId::ScreenModeMouseCaptureOn)
                        } else {
                            app.tr(MessageId::ScreenModeMouseCaptureOff)
                        };
                        app.add_message(HistoryCell::System {
                            content: format!("{screen} {capture}"),
                        });
                    }
                    Err(reason) => {
                        let unchanged = app
                            .tr(MessageId::ScreenModeUnchanged)
                            .replace("{reason}", &reason);
                        app.add_message(HistoryCell::System {
                            content: unchanged.clone(),
                        });
                        app.push_status_toast(
                            unchanged.trim_end_matches('.').to_string(),
                            StatusToastLevel::Warning,
                            Some(8_000),
                        );
                    }
                }
            }
            AppAction::OpenModePicker => {
                if app.view_stack.top_kind() != Some(ModalKind::ModePicker) {
                    app.view_stack
                        .push(crate::tui::views::mode_picker::ModePickerView::new(
                            app.mode,
                            app.ui_locale,
                        ));
                }
            }
            AppAction::OpenStatusPicker => {
                if app.view_stack.top_kind() != Some(ModalKind::StatusPicker) {
                    app.view_stack
                        .push(crate::tui::views::status_picker::StatusPickerView::new(
                            &app.status_items,
                            app.api_provider,
                            app.ui_locale,
                        ));
                }
            }
            AppAction::ReviewIssueReport { id, change } => {
                if let Err(error) = super::feedback_host::start(app, config, id, change) {
                    tracing::warn!(target:"feedback_host",%error,"feedback request refused before admission");
                    app.add_message(HistoryCell::System {
                        content: app.tr(MessageId::FeedbackUnavailable).into_owned(),
                    });
                }
            }
            AppAction::OpenFeedbackPicker => {
                if app.view_stack.top_kind() != Some(ModalKind::FeedbackPicker) {
                    app.view_stack
                        .push(crate::tui::feedback_picker::FeedbackPickerView::new());
                }
            }
            AppAction::OpenThemePicker => {
                if app.view_stack.top_kind() != Some(ModalKind::ThemePicker) {
                    // Capture the active theme name straight from `app` so
                    // Esc can revert through the same ConfigUpdated channel.
                    // Avoids re-reading settings.toml from disk on every
                    // `/theme` invocation.
                    let original = app.theme_name.clone();
                    app.view_stack
                        .push_boxed(crate::tui::theme_picker::ThemePickerView::boxed(
                            original,
                            app.ui_locale,
                            app.background_color_override,
                        ));
                }
            }
            AppAction::OpenSkillsManager => {
                if app.view_stack.top_kind() != Some(ModalKind::SkillsManager) {
                    app.view_stack
                        .push(crate::tui::views::skills_manager::SkillsManagerView::new(
                            app,
                        ));
                }
            }
            AppAction::OpenWorkflowsManager => {
                crate::tui::views::workflows_manager::open(app);
            }
            AppAction::OpenExtensions { tab } => {
                if app.view_stack.top_kind() != Some(ModalKind::Extensions) {
                    app.view_stack
                        .push(crate::tui::views::extensions::ExtensionsView::new(app, tab));
                }
            }
            AppAction::OpenFleetList => {
                if app.view_stack.top_kind() != Some(ModalKind::FleetList) {
                    app.view_stack
                        .push(crate::tui::views::fleet_list::FleetListView::new(
                            app, config,
                        ));
                }
            }
            AppAction::OpenFleetRoster => {
                if app.view_stack.top_kind() != Some(ModalKind::FleetRoster) {
                    app.view_stack
                        .push(crate::tui::views::fleet_roster::FleetRosterView::new(
                            app, config,
                        ));
                }
                // `/fleet` is where the one-time Fleet intro belongs.
                app.maybe_show_feature_intro();
            }
            AppAction::OpenFleetSetup => {
                open_fleet_setup_target(app, config, None);
            }
            AppAction::FleetAddModel {
                provider,
                model,
                roles,
            } => {
                use crate::commands::{fleet_catalog_rejection, fleet_provider_rejection};
                let locale = app.ui_locale;
                // The live `config` is the provider truth: the startup
                // snapshot went stale after any in-session provider change.
                let rejection = fleet_provider_rejection(app, config, &provider)
                    .or_else(|| fleet_catalog_rejection(locale, &provider, &model));
                let content = match rejection {
                    Some(rejection) => rejection,
                    None => match crate::fleet::members::add_fleet_model(
                        &app.workspace,
                        &provider,
                        &model,
                        &roles,
                    ) {
                        Ok(change) => {
                            if !matches!(
                                change,
                                crate::fleet::members::FleetModelChange::Unchanged { .. }
                            ) {
                                app.fleet_roster_stale = true;
                            }
                            crate::fleet::members::change_receipt(
                                locale, &provider, &model, &change,
                            )
                        }
                        Err(error) => tr(locale, MessageId::FleetAddFailed)
                            .replace("{error}", &error.message(locale)),
                    },
                };
                app.add_message(HistoryCell::System { content });
            }
            AppAction::FleetRemoveModel { provider, model } => {
                let locale = app.ui_locale;
                let content = match crate::fleet::members::remove_fleet_model(
                    &app.workspace,
                    &provider,
                    &model,
                ) {
                    Ok(change) => {
                        app.fleet_roster_stale = true;
                        crate::fleet::members::change_receipt(locale, &provider, &model, &change)
                    }
                    Err(error) => tr(locale, MessageId::FleetRemoveFailed)
                        .replace("{error}", &error.message(locale)),
                };
                app.add_message(HistoryCell::System { content });
            }
            AppAction::OpenHotbarSetup => {
                if app.view_stack.top_kind() != Some(ModalKind::HotbarSetup) {
                    app.view_stack
                        .push(crate::tui::hotbar::setup::HotbarSetupView::new(app, config));
                }
            }
            AppAction::OpenSetupWizard => {
                if app.view_stack.top_kind() != Some(ModalKind::SetupWizard) {
                    let _ = app.next_draft_gen();
                    app.view_stack
                        .push(crate::tui::setup::SetupWizardView::new_for_app(app, config));
                }
            }
            AppAction::OpenSetupWizardAt { step } => {
                if app.view_stack.top_kind() != Some(ModalKind::SetupWizard) {
                    let _ = app.next_draft_gen();
                    app.view_stack
                        .push(crate::tui::setup::SetupWizardView::new_for_app_at(
                            app, config, step,
                        ));
                }
            }
            AppAction::UseBundledConstitution => use_bundled_constitution(app, config),
            AppAction::PreviewEffectiveBasePrompt => preview_effective_base_prompt(app, config),
            AppAction::DisableHotbar => disable_hotbar(app, config),
            AppAction::RestoreHotbarDefaults => restore_hotbar_defaults(app, config),
            AppAction::OpenExternalUrl { url, label } => match open_external_url(&url) {
                Ok(()) => {
                    app.status_message = Some(format!("Opened {label} in your browser"));
                }
                Err(err) => {
                    app.add_message(HistoryCell::System {
                        content: format!(
                            "Could not open {label} automatically: {err}\n\nThe URL is printed above."
                        ),
                    });
                }
            },
            AppAction::OpenContextInspector => {
                open_context_inspector(app);
            }
            AppAction::OpenLiveTranscript => {
                open_live_transcript_overlay(app);
            }
            AppAction::OpenTurnInspector => {
                open_turn_inspector_pager(app);
            }
            AppAction::CompactContext { focus } => {
                try_queue_manual_compaction(app, config, engine_handle, focus);
            }
            AppAction::PurgeContext => {
                if engine_handle.try_send(Op::PurgeContext).is_err() {
                    app.status_message =
                        Some("Engine busy — purge not sent; try again".to_string());
                } else {
                    app.status_message = Some("Agent purging context...".to_string());
                }
            }
            AppAction::TaskAdd { prompt } => {
                let owner_session_id = app
                    .current_session_id
                    .clone()
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                app.current_session_id = Some(owner_session_id.clone());
                let request = NewTaskRequest {
                    prompt: prompt.clone(),
                    name: None,
                    model: Some(app.model.clone()),
                    model_provider: Some(app.api_provider.as_str().to_string()),
                    model_provider_id: Some(app.provider_identity_for_persistence().to_string()),
                    workspace: Some(app.workspace.clone()),
                    mode: Some(task_mode_label(app.mode).to_string()),
                    allow_shell: Some(app.allow_shell),
                    trust_mode: Some(app.trust_mode),
                    auto_approve: Some(app_auto_approve_enabled(app)),
                    // Same as the task tool: the task's own thread runs under
                    // the posture this session is in, not under whatever a
                    // legacy bit happens to mean today.
                    permission_posture: Some(
                        crate::runtime_policy::approval_wire(app.approval_mode).to_string(),
                    ),
                    owner_session_id: Some(owner_session_id),
                };
                match task_manager.add_task(request).await {
                    Ok(task) => {
                        app.add_message(HistoryCell::System {
                            content: format!(
                                "Task queued: {} ({})",
                                task.id,
                                summarize_tool_output(&task.prompt)
                            ),
                        });
                        app.status_message = Some(format!("Queued {}", task.id));
                    }
                    Err(err) => {
                        app.add_message(HistoryCell::System {
                            content: format!("Failed to queue task: {err}"),
                        });
                    }
                }
                refresh_active_task_panel(app, task_manager).await;
            }
            AppAction::TaskList => {
                let tasks = match app.current_session_id.as_deref() {
                    Some(session_id) => {
                        task_manager
                            .list_tasks_for_owner(Some(30), None, session_id)
                            .await
                    }
                    None => Ok(Vec::new()),
                };
                refresh_active_task_panel(app, task_manager).await;
                app.add_message(HistoryCell::System {
                    content: match tasks {
                        Ok(tasks) => format_task_list(&tasks),
                        Err(_) => codewhale_localization::tr(
                            app.ui_locale,
                            codewhale_localization::MessageId::TaskInventoryUnavailable,
                        )
                        .to_string(),
                    },
                });
            }
            AppAction::RemoteControl(action) => match action {
                crate::remote_control::RemoteControlAction::Start => {
                    start_remote_control_session(app, config);
                }
                crate::remote_control::RemoteControlAction::Stop => {
                    app.remote_control.stop();
                    let status = app.remote_control.status_line();
                    app.sticky_status = None;
                    app.status_message = Some(status);
                }
            },
            AppAction::TaskShow { id } => {
                let task = match app.current_session_id.as_deref() {
                    Some(session_id) => {
                        task_manager
                            .get_task_for_interactive_session(&id, session_id)
                            .await
                    }
                    None => Err(anyhow::anyhow!("Task not found: {id}")),
                };
                match task {
                    Ok(task) => open_task_pager(app, &task),
                    Err(err) => {
                        app.add_message(HistoryCell::System {
                            content: format!("Task lookup failed: {err}"),
                        });
                    }
                }
            }
            AppAction::TaskCancel { id } => {
                let cancellation = match app.current_session_id.as_deref() {
                    Some(session_id) => {
                        task_manager
                            .cancel_task_for_interactive_session(&id, session_id)
                            .await
                    }
                    None => Err(anyhow::anyhow!("Task not found: {id}")),
                };
                match cancellation {
                    Ok(cancellation) => {
                        app.add_message(HistoryCell::System {
                            content: format!(
                                "Task {} status: {:?}",
                                cancellation.task.id, cancellation.task.status
                            ),
                        });
                    }
                    Err(err) => {
                        app.add_message(HistoryCell::System {
                            content: format!("Task cancel failed: {err}"),
                        });
                    }
                }
                refresh_active_task_panel(app, task_manager).await;
            }
            AppAction::Automation(action) => {
                crate::tui::automation_routing::handle_action(app, config, action, task_manager)
                    .await;
            }
            AppAction::ShellJob(action) => {
                handle_shell_job_action(app, action);
                // Immediately sync the task panel after cancel/poll so the
                // Activity sidebar stays accurate without waiting for the
                // next 2.5 s periodic refresh (#2937).
                refresh_active_task_panel(app, task_manager).await;
            }
            AppAction::Mcp(action) => {
                handle_mcp_ui_action(app, engine_handle, config, action).await;
            }
            AppAction::SwitchWorkspace { workspace } => {
                switch_workspace(app, engine_handle, task_manager, config, workspace).await;
            }
            AppAction::SwitchProfile { profile } => {
                let previous_profile = app.config_profile.clone();
                match Config::load(app.config_path.clone(), Some(&profile)).and_then(|new_config| {
                    validated_profile_default_route(&new_config)
                        .map(|validated_route| (new_config, validated_route))
                }) {
                    Ok((new_config, validated_route)) => {
                        let new_model = validated_route.model.clone();
                        apply_validated_profile_config(
                            app,
                            config,
                            &profile,
                            new_config,
                            &validated_route,
                        );
                        crate::config::initialize_cloud_facts(config);
                        // Rebuild the engine with the new config so API key/model/base URL take effect.
                        let _ = engine_handle.send(Op::Shutdown).await;
                        let engine_config = build_engine_config(app, config);
                        *engine_handle = spawn_tui_engine(engine_config, config);
                        if !app.api_messages.is_empty() {
                            let _ = engine_handle
                                .send(Op::SyncSession {
                                    session_id: app.current_session_id.clone(),
                                    messages: app.api_messages.as_ref().clone(),
                                    system_prompt: app.system_prompt.clone(),
                                    system_prompt_override: false,
                                    model: app.model.clone(),
                                    workspace: app.workspace.clone(),
                                    mode: app.mode,
                                })
                                .await;
                        }
                        let provider = app.provider_identity_for_persistence();
                        let content = app
                            .tr(MessageId::ProfileSwitched)
                            .replace("{model}", &new_model)
                            .replace("{provider}", provider)
                            .replace("{name}", &profile);
                        app.add_message(HistoryCell::System { content });
                        app.status_message =
                            Some(app.tr(MessageId::ProfileStatus).replace("{name}", &profile));
                    }
                    Err(err) => {
                        app.config_profile = previous_profile;
                        app.status_message = Some(
                            app.tr(MessageId::ProfileSwitchFailed)
                                .replace("{name}", &profile)
                                .replace("{error}", &err.to_string()),
                        );
                    }
                }
            }
            AppAction::ShareSession { html } => {
                // The page was rendered and redacted by `/share confirm`
                // through the `/export` projection; only upload happens here.
                let status = match crate::commands::share::perform_share(html).await {
                    Ok(url) => {
                        format!("Session shared as a secret gist (unlisted, not private): {url}")
                    }
                    Err(err) => format!("Share failed: {err}"),
                };
                app.add_message(HistoryCell::System {
                    content: status.clone(),
                });
                app.status_message = Some(status);
            }
        }
    }

    Ok(false)
}

/// Commit a successfully loaded profile and its validated route as one snapshot.
fn apply_validated_profile_config(
    app: &mut App,
    config: &mut Config,
    profile: &str,
    next_config: Config,
    route: &crate::route_runtime::ValidatedRuntimeRoute,
) {
    *config = next_config;
    app.config_profile = Some(profile.to_string());
    app.configured_models = config.custom_models.clone().unwrap_or_default();
    app.refresh_notification_settings(config);
    app.set_provider_identity_record(route.identity.clone());
    app.billing_presentation = crate::route_billing::for_route(config, &route.identity);
    app.set_model_selection(route.model.clone());
    app.set_active_context_window_override(config, &route.identity);
    app.set_active_route_resolution(
        route.candidate.endpoint().base_url.clone(),
        route.candidate.limits(),
        route.context_window.source,
    );
    app.update_model_compaction_budget();
    app.session.last_prompt_tokens = None;
    app.session.last_completion_tokens = None;
}

/// Open this workspace's `.codewhale/hooks.toml` in `$EDITOR`.
///
/// The Hooks screen could only ever be read: it listed what was configured
/// and offered no way to configure anything. Rather than grow a second
/// authority over hook definitions inside the TUI, this hands the file to the
/// editor the user already has, seeds it with a commented template the first
/// time, and reloads the hook set on return so the screen reflects the edit
/// immediately.
fn edit_project_hooks_from_tui(terminal: &mut AppTerminal, app: &mut App, config: &Config) {
    let path = app.workspace.join(".codewhale").join("hooks.toml");
    // A link in `.codewhale` or at hooks.toml is never created through, and the
    // editor is not pointed at one: the file must really live in the workspace.
    if let Err(error) = crate::fleet::files::reject_linked_path(&app.workspace, &path) {
        app.push_status_toast(
            format!("Could not use {}: {error}", path.display()),
            StatusToastLevel::Warning,
            Some(8_000),
        );
        return;
    }
    if !path.exists()
        && let Err(error) = crate::fs_confined::write(
            &app.workspace,
            &path,
            crate::hooks::PROJECT_HOOKS_TEMPLATE.as_bytes(),
        )
    {
        app.push_status_toast(
            format!("Could not create {}: {error}", path.display()),
            StatusToastLevel::Warning,
            Some(8_000),
        );
        return;
    }

    let outcome = crate::tui::external_editor::spawn_editor_for_path(
        terminal,
        app.use_alt_screen(),
        app.use_mouse_capture,
        app.use_bracketed_paste,
        &path,
        // Open the file, not a position in it: this edits hooks.toml whole.
        None,
    );
    app.needs_redraw = true;

    match outcome {
        Ok(crate::tui::external_editor::EditorOutcome::Edited(_)) => {
            app.hooks = app.hooks.rebind(
                crate::hooks::HooksConfig::load_with_project_and_plugins(
                    config.hooks_config(),
                    &app.workspace,
                    Some(app.plugin_registry.as_ref()),
                ),
                app.workspace.clone(),
            );
            app.runtime_services.hook_executor = Some(std::sync::Arc::new(app.hooks.clone()));
            let reloaded = app.hooks.config();
            let mut content = format!(
                "Reloaded hooks from {} — {} configured.",
                path.display(),
                reloaded.hooks.len()
            );
            // Project hooks are executable repository configuration; an
            // untrusted workspace parses them and then ignores them, which is
            // a silent no-op unless it is said out loud.
            if !crate::hooks::workspace_allows_project_hooks(&app.workspace) {
                content.push_str(
                    " Project hooks are not approved for these exact contents. Use /hooks review, \
                     then /hooks approve <digest> after reviewing the commands.",
                );
            }
            if !reloaded.problems.is_empty() {
                content.push_str(&format!(
                    " {} entr{} rejected — see the Hooks screen.",
                    reloaded.problems.len(),
                    if reloaded.problems.len() == 1 {
                        "y"
                    } else {
                        "ies"
                    }
                ));
            }
            app.add_message(HistoryCell::System { content });
        }
        Ok(crate::tui::external_editor::EditorOutcome::Unchanged) => {
            app.push_status_toast(
                "Hooks unchanged.".to_string(),
                StatusToastLevel::Info,
                Some(4_000),
            );
        }
        Ok(crate::tui::external_editor::EditorOutcome::Cancelled) | Err(_) => {
            app.push_status_toast(
                format!("Editor did not save {}", path.display()),
                StatusToastLevel::Warning,
                Some(6_000),
            );
        }
    }
}

pub(crate) fn apply_workspace_runtime_state(app: &mut App, config: &Config, workspace: PathBuf) {
    app.workspace = workspace.clone();
    app.coordination_detail = None;
    app.plugin_registry = app.plugin_registry.rediscover_for_workspace(&workspace);
    for error in crate::commands::user_registry::install_plugin_registry(
        &workspace,
        app.plugin_registry.as_ref(),
    ) {
        tracing::warn!(target: "plugins", "{error}");
    }
    // A plugin that failed to load used to be invisible until someone
    // happened to open /plugin. Surface a one-line hint instead of leaving
    // the discovery result buried in the trace log; warnings stay quiet.
    let plugin_load_errors = app
        .plugin_registry
        .diagnostics()
        .iter()
        .filter(|diagnostic| {
            diagnostic.level == crate::plugins::types::PluginDiagnosticLevel::Error
        })
        .count();
    if plugin_load_errors > 0 {
        app.status_message = Some(if plugin_load_errors == 1 {
            "1 plugin failed to load — /plugin for details".to_string()
        } else {
            format!("{plugin_load_errors} plugins failed to load — /plugin for details")
        });
    }
    app.active_skill = None;
    app.active_skill_provenance = None;
    // Switching workspace reloads the hook set (project hooks are per-repo)
    // but stays inside the same TUI session, so the session id is preserved.
    app.hooks = app.hooks.rebind(
        crate::hooks::HooksConfig::load_with_project_and_plugins(
            config.hooks_config(),
            &workspace,
            Some(app.plugin_registry.as_ref()),
        ),
        workspace.clone(),
    );
    app.skills_dir = crate::tui::app::resolve_skills_dir(&workspace, &config.skills_dir(), config);
    app.skills_discovery_mode =
        crate::skills::SkillDiscoveryMode::from_config(&config.skills_config());
    app.project_context_pack_enabled = config.project_context_pack_enabled();
    app.refresh_skill_cache();
    app.workspace_context = None;
    app.workspace_is_linked_worktree = false;
    if let Ok(mut cell) = app.workspace_context_cell.lock() {
        *cell = None;
    }
    app.workspace_context_refreshed_at = None;
    app.file_tree = None;

    let shell_manager = crate::tools::shell::new_shared_shell_manager(workspace);
    app.runtime_services.shell_manager = Some(shell_manager);
    app.runtime_services.hook_executor = Some(std::sync::Arc::new(app.hooks.clone()));
}

pub(crate) fn apply_hotbar_setup_saved(
    app: &mut App,
    config: &mut Config,
    bindings: Vec<codewhale_config::HotbarBindingToml>,
) {
    match crate::config_persistence::persist_hotbar_bindings(app.config_path.as_deref(), &bindings)
    {
        Ok(path) => {
            config.hotbar = Some(bindings);
            app.status_message = Some(format!("Hotbar bindings saved to {}", path.display()));
        }
        Err(err) => {
            app.status_message = Some(format!("Failed to save Hotbar bindings: {err}"));
            app.add_message(HistoryCell::System {
                content: format!("Failed to save Hotbar bindings: {err}"),
            });
        }
    }
    app.needs_redraw = true;
}

pub(crate) fn settle_user_input_request(app: &mut App, tool_id: &str) -> bool {
    app.retire_action_notices(Some(tool_id));
    let removed_view = app.view_stack.remove_user_input_by_id(tool_id);
    let matched = app
        .pending_user_input_prompt
        .as_ref()
        .is_some_and(|(id, _)| id == tool_id);
    if matched {
        app.pending_user_input_prompt = None;
    }
    app.needs_redraw |= removed_view || matched;
    matched
}

pub(crate) fn settle_pending_human_requests(app: &mut App) {
    if let Some((id, _)) = app.pending_user_input_prompt.as_ref() {
        let id = id.clone();
        settle_user_input_request(app, &id);
    }
    // A completed/cancelled parent turn cannot still await an approval.
    // Children own their separate lifecycle and may legitimately remain live.
    for id in app.view_stack.tool_decision_request_ids() {
        if !crate::tools::subagent::SubAgentManager::is_child_approval_id(&id) {
            crate::tui::pending_requests::retire(app, &id);
            app.retire_action_notices(Some(&id));
        }
    }
}

/// The Engine's terminal tool event retires the exact request even when its
/// presentation is filtered after a local cancel. An outer Code Mode call's
/// completion cannot settle a different, inner request id.
pub(crate) fn observe_human_request_settlement(app: &mut App, event: &EngineEvent) {
    match event {
        EngineEvent::ToolCallComplete { id, .. } => {
            settle_user_input_request(app, id);
            crate::tui::pending_requests::retire(app, id);
        }
        EngineEvent::TurnComplete { .. }
            if !(app.suppress_stream_events_until_turn_complete && app.is_loading) =>
        {
            settle_pending_human_requests(app);
        }
        _ => {}
    }
}

pub(crate) fn note_human_decision_delivered(app: &mut App, tool_id: &str) {
    if (app.is_loading || matches!(app.runtime_turn_status.as_deref(), Some("in_progress")))
        && !app.suppress_stream_events_until_turn_complete
        && !crate::tui::pending_requests::is_foreign_child_request(app, tool_id)
    {
        // The reply resumes Engine work before its next event reaches this
        // frame. Give that work its own inactivity window after a long wait.
        app.turn_last_activity_at = Some(Instant::now());
    }
}

pub(crate) fn apply_user_input_submission_result(app: &mut App, tool_id: &str, result: Result<()>) {
    match result {
        Ok(()) => {
            if settle_user_input_request(app, tool_id) {
                note_human_decision_delivered(app, tool_id);
            }
        }
        Err(error) => {
            tracing::warn!(tool_id, error = %error, "user input submit failed");
            if let Some((id, request)) = app
                .pending_user_input_prompt
                .as_ref()
                .filter(|(id, _)| id == tool_id)
                .cloned()
            {
                app.view_stack.push(UserInputView::new(id, request));
            }
            app.push_status_toast_record(
                StatusToast::new(
                    app.tr(MessageId::NotificationInputSubmitFailed)
                        .replace("{error}", &error.to_string()),
                    StatusToastLevel::Error,
                    Some(App::STICKY_ERROR_TTL_MS),
                )
                .for_event(format!("input-submit:{tool_id}")),
            );
        }
    }
}

pub(crate) async fn apply_approval_decision(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    event: ApprovalDecisionEvent,
) {
    if event.decision == ReviewDecision::ApprovedForSession {
        // Only the lossy grouping key is stored: a session grant is scoped to
        // the command family (e.g. `shell:git status`), never to the whole
        // tool — approving one shell command must not approve every shell
        // command for the session (ops R2). The tool name is recorded as
        // audit evidence, not as a grant.
        crate::audit::log_sensitive_event(
            "tool.approval.session_grant",
            serde_json::json!({
                "tool_name": event.tool_name,
                "grouping_key": event.approval_grouping_key,
            }),
        );
        app.approval_session_approved
            .insert(event.approval_grouping_key.clone());
    }

    if matches!(
        event.decision,
        ReviewDecision::Approved | ReviewDecision::ApprovedForSession
    ) && !event.persistent_rules.is_empty()
        && !event.timed_out
    {
        persist_rules_from_approval(app, config, &event.persistent_rules);
    }

    // A child's card was answered here: its pending entry is done. An Abort
    // on a child's card only hides it (the entry stays for the footer).
    if event.decision != ReviewDecision::Abort {
        crate::tui::pending_requests::resolve(app, &event.tool_id);
    }

    match event.decision {
        // A child's card never stops the parent's turn (approvals C1).
        ReviewDecision::Abort
            if crate::tools::subagent::SubAgentManager::is_child_approval_id(&event.tool_id) => {}
        ReviewDecision::Approved | ReviewDecision::ApprovedForSession => {
            // Mirror mode: clear the shared-approval gate so a late web
            // decision acks "no longer pending" instead of double-answering.
            app.remote_control
                .resolve_pending_approval(&event.tool_id, true);
            if engine_handle
                .approve_tool_call(event.tool_id.clone())
                .await
                .is_ok()
            {
                note_human_decision_delivered(app, &event.tool_id);
                app.retire_action_notices(Some(&event.tool_id));
            }
        }
        ReviewDecision::Denied => {
            // Cache the denial so the model retry-loop doesn't re-prompt for
            // the exact same approval_key (#360). Only the key (per-call
            // unique) is stored — NOT the tool_name, which would block all
            // future invocations of the same tool type (#1377).
            if !event.timed_out {
                app.approval_session_denied.insert(event.approval_key);
            }
            app.remote_control
                .resolve_pending_approval(&event.tool_id, false);
            // A bound expiry carries its own outcome (#6101) so the receipt
            // distinguishes "no answer within the window" from an operator
            // denial.
            let denied = if event.timed_out {
                engine_handle
                    .deny_tool_call_timed_out(event.tool_id.clone())
                    .await
            } else {
                engine_handle.deny_tool_call(event.tool_id.clone()).await
            };
            if denied.is_ok() {
                note_human_decision_delivered(app, &event.tool_id);
                app.retire_action_notices(Some(&event.tool_id));
            }
        }
        ReviewDecision::Abort => {
            engine_handle.cancel();
            mark_active_turn_cancelled_locally(app);
            app.status_message = Some(parent_stop_status(app, "Request cancelled"));
        }
    }
}

pub(crate) fn apply_setup_runtime_preset(
    app: &mut App,
    config: &mut Config,
    preset: crate::tui::setup::SetupRuntimePreset,
    state: codewhale_config::SetupState,
) -> Result<String> {
    if let Some(source) = config.runtime_preset_blocker(
        app.config_path.as_deref(),
        app.config_profile.as_deref(),
        &app.workspace,
    ) {
        anyhow::bail!(
            "Runtime presets cannot override {source}; change that controlling source first"
        );
    }
    if preset == crate::tui::setup::SetupRuntimePreset::HighTrustLocal {
        let approval = config.approval_policy_control(
            app.config_path.as_deref(),
            app.config_profile.as_deref(),
            &app.workspace,
        );
        if !approval.editable_root() {
            anyhow::bail!(
                "Full Access cannot override {}; change that controlling source first",
                approval.label()
            );
        }
    }

    let settings_path = Settings::path().context("failed to resolve settings path")?;
    let settings_snapshot = RuntimePresetFileSnapshot::capture(settings_path)?;
    // The preset's settings read, its config-document write, and its settings
    // write are one durable transaction with file-snapshot rollback. Hold the
    // settings transaction lock across all of it so a concurrent writer (a queued
    // mode/thinking drain, the Shift+Tab posture write) can neither be lost by
    // this save nor be reverted by the rollback.
    // Every durable write happens inside this closure, so the settings lock is
    // released before live state moves below.
    crate::settings::with_settings_transaction(|settings_transaction| {
        let mut settings = settings_transaction
            .load()
            .context("failed to load settings")?;
        settings.default_mode = preset.default_mode().to_string();
        settings.permission_posture = Some(preset.permission_posture().to_string());

        // Persist into the same file Config::load actually selected. A missing
        // explicit env target remains authoritative for both reads and writes;
        // an invalid target fails here instead of selecting a different file.
        let selected_config_path =
            crate::config::resolve_load_config_path(app.config_path.clone())?
                .or_else(|| app.config_path.clone());
        let config_path =
            crate::config_persistence::config_toml_path(selected_config_path.as_deref())
                .context("failed to resolve config path")?;
        let config_snapshot = RuntimePresetFileSnapshot::capture(config_path.clone())?;
        if let Err(error) =
            crate::config_persistence::mutate_config_document(&config_path, |document| {
                if let Some(policy) = preset.approval_policy() {
                    crate::config_persistence::set_document_value(
                        document,
                        &["approval_policy"],
                        policy,
                    )?;
                } else {
                    crate::config_persistence::unset_document_value(
                        document,
                        &["approval_policy"],
                    )?;
                }
                crate::config_persistence::set_document_value(
                    document,
                    &["allow_shell"],
                    preset.allow_shell(),
                )?;
                crate::config_persistence::set_document_value(
                    document,
                    &["sandbox_mode"],
                    preset.sandbox_mode(),
                )
            })
            .context("failed to persist runtime posture")
        {
            return Err(runtime_preset_error_with_rollback(
                error,
                &[&settings_snapshot, &config_snapshot],
            ));
        }
        if let Err(error) = settings_transaction
            .save(&settings)
            .context("failed to save settings")
        {
            return Err(runtime_preset_error_with_rollback(
                error,
                &[&settings_snapshot, &config_snapshot],
            ));
        }
        if let Err(error) = state
            .save()
            .context("failed to persist setup runtime posture state")
        {
            return Err(runtime_preset_error_with_rollback(
                error,
                &[&settings_snapshot, &config_snapshot],
            ));
        }
        Ok(())
    })?;

    // Durable writes succeeded as one transaction. Only now may live state
    // move to the new posture.
    if let Some(policy) = preset.approval_policy() {
        config.approval_policy = Some(policy.to_string());
        app.mark_approval_policy_locked();
    } else {
        config.approval_policy = None;
        app.clear_saved_approval_policy_lock();
    }
    config.allow_shell = Some(preset.allow_shell());
    config.sandbox_mode = Some(preset.sandbox_mode().to_string());
    app.configured_sandbox_mode = config.sandbox_mode.clone();
    app.configured_sandbox_network = config.sandbox_network_access;

    let approval_mode = ApprovalMode::from_config_value(
        preset
            .approval_policy()
            .unwrap_or(preset.permission_posture()),
    )
    .unwrap_or(ApprovalMode::Suggest);
    let trust_mode = match preset {
        crate::tui::setup::SetupRuntimePreset::AskFirst => false,
        crate::tui::setup::SetupRuntimePreset::NormalAgent => app.agent_trust_baseline(),
        crate::tui::setup::SetupRuntimePreset::HighTrustLocal => true,
    };
    app.set_agent_runtime_baseline(preset.allow_shell(), trust_mode, approval_mode);
    let mode = AppMode::from_setting(preset.default_mode());
    app.set_mode(mode);
    app.needs_redraw = true;

    Ok(format!("Applied {}.", preset.result_summary()))
}

pub(crate) fn apply_backtrack(app: &mut App, depth: usize) {
    let Some(history_idx) = find_user_cell_index_from_tail(app, depth) else {
        app.status_message = Some("Backtrack target no longer present".to_string());
        return;
    };

    // Snapshot the user text before truncating so we can refill the
    // composer.
    let user_text = match app.history.get(history_idx) {
        Some(HistoryCell::User { content }) => content.clone(),
        _ => String::new(),
    };

    // Trim the visible transcript at the chosen user cell. Per-cell
    // revisions and tool-cell maps are kept consistent through
    // `App::truncate_history_to`.
    app.truncate_history_to(history_idx);

    // Trim the API-message log at the matching user PROMPT. `depth` counts
    // visible `HistoryCell::User` cells (real prompts), but a naive
    // `role == "user"` walk over `api_messages` over-counts: tool results are
    // stored as `role == "user"` messages too, so in any turn with tool calls
    // the cut would land mid-turn on a tool_result — leaving a dangling
    // assistant tool_use with no matching result and a transcript the provider
    // rejects. Count only messages that actually yield a User cell, the same
    // predicate `apply_loaded_session` uses.
    if let Some(idx) = backtrack_api_cut_index(&app.api_messages, depth) {
        app.truncate_api_messages(idx);
    }

    // Hand the dropped text back to the user so they can edit + resend.
    app.input = user_text;
    app.cursor_position = app.input.chars().count();

    // Close the overlay, refresh sticky-tail flag, and surface a hint.
    if app.view_stack.top_kind() == Some(ModalKind::LiveTranscript) {
        app.view_stack.pop();
    }
    // Backtrack rewinds the conversation only. State the file fact first,
    // and name the command that puts the files back.
    app.status_message =
        Some("Files not changed. /undo puts them back. Conversation rewound.".to_string());
    app.scroll_to_bottom();
    app.mark_history_updated();
    app.needs_redraw = true;
}

pub(crate) async fn apply_provider_picker_custom_provider(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    provider_id: String,
    base_url: String,
    model: Option<String>,
    api_key_env: Option<String>,
) -> bool {
    let written = match crate::config_persistence::persist_custom_provider(
        app.config_path.as_deref(),
        &provider_id,
        &base_url,
        model.as_deref(),
        api_key_env.as_deref(),
    ) {
        Ok(path) => path,
        Err(err) => {
            app.add_message(HistoryCell::System {
                content: format!("Failed to save custom provider {provider_id}: {err}"),
            });
            app.status_message = Some("Custom provider was not saved.".to_string());
            return false;
        }
    };

    config.provider = Some(provider_id.clone());
    let entry = config
        .providers
        .get_or_insert_with(ProvidersConfig::default)
        .custom
        .entry(provider_id.clone())
        .or_default();
    entry.kind = Some("openai-compatible".to_string());
    entry.base_url = Some(base_url.trim().trim_end_matches('/').to_string());
    if provider_id == "ds4" && crate::config::base_url_uses_local_host(&base_url) {
        entry.context_window = Some(100_000);
    }
    entry.model = model.clone().and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    });
    let keyless_local = provider_id == "ds4"
        && api_key_env
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && crate::config::base_url_uses_local_host(&base_url);
    entry.api_key_env = api_key_env.and_then(|value| {
        let value = value.trim().to_string();
        (!value.is_empty()).then_some(value)
    });
    entry.auth_mode = keyless_local.then(|| "none".to_string());

    app.status_message = Some(format!(
        "Custom provider {provider_id} saved to {}",
        written.display()
    ));
    let identity = match config.resolve_provider_pin_identity(&provider_id) {
        Ok(identity) if identity.provider == ProviderKind::Custom => identity,
        Ok(_) => {
            app.push_status_toast(
                "Saved custom route changed transport identity.",
                StatusToastLevel::Error,
                Some(8_000),
            );
            return false;
        }
        Err(reason) => {
            app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
            return false;
        }
    };
    switch_provider(app, engine_handle, config, identity, model).await
}

async fn reopen_provider_picker_list(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &Config,
    selected_provider_id: Option<String>,
    catalog_view: bool,
) {
    let runtime_status = query_provider_runtime_status(engine_handle).await;
    app.provider_picker_memory = Some(crate::tui::app::ProviderPickerMemory {
        catalog_view,
        selected_provider_id,
    });
    app.view_stack.push(
        crate::tui::provider_picker::ProviderPickerView::new_with_runtime_status_and_memory(
            app.api_provider,
            config,
            runtime_status,
            app.provider_picker_memory.as_ref(),
        )
        .with_locale(app.ui_locale)
        .with_provider_health(&app.provider_health),
    );
    app.needs_redraw = true;
}

pub(crate) async fn apply_provider_picker_test_connection(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    identity: crate::config::ProviderIdentity,
    catalog_view: bool,
) {
    apply_provider_picker_test_connection_with_verifier(
        app,
        engine_handle,
        config,
        identity,
        catalog_view,
        &LiveProviderKeyVerifier,
    )
    .await;
}

/// One plain sentence for a key the provider did not accept, with the next
/// step, in place of the provider's raw reply (#6566). Only a failure with no
/// plain reading keeps a sanitized, bounded excerpt of that reply.
fn plain_key_verification_error(app: &App, reason: &str, api_key: &str) -> String {
    use crate::error_taxonomy::ErrorCategory;
    match provider_verification_error_category(reason) {
        ErrorCategory::Authentication => app.tr(MessageId::ProviderKeyRejected).into_owned(),
        ErrorCategory::Authorization => app.tr(MessageId::ProviderKeyForbidden).into_owned(),
        ErrorCategory::Network | ErrorCategory::Timeout => {
            app.tr(MessageId::ProviderKeyUnreachable).into_owned()
        }
        _ => app
            .tr(MessageId::ProviderKeyCheckFailed)
            .replace("{reason}", &sanitize_probe_status(reason, api_key)),
    }
}

fn sanitize_probe_status(reason: &str, api_key: &str) -> String {
    let mut text = reason.to_string();
    if let Some(rest) = reason.strip_prefix("HTTP ")
        && let Some((code, body)) = rest.split_once(':')
        && let Ok(status) = code.trim().parse::<u16>()
    {
        text = crate::llm_client::sanitize_http_error_body(None, status, body.trim());
    }
    let secret = api_key.trim();
    if !secret.is_empty() {
        text = text.replace(secret, "***");
    }
    crate::utils::truncate_with_ellipsis(text.trim(), 120, "…")
}

pub(crate) async fn apply_provider_picker_test_connection_with_verifier(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    identity: crate::config::ProviderIdentity,
    catalog_view: bool,
    verifier: &dyn ProviderKeyVerifier,
) {
    if let Err(reason) = config.verify_provider_identity(&identity) {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        return;
    }
    let provider = identity.provider;
    let mut scoped_config = config.clone();
    if let Err(reason) = scoped_config.scope_to_provider_identity(&identity) {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        return;
    }
    let selected_id = Some(identity.key.to_string());
    if !crate::client::provider_api_key_verification_is_observed(provider) {
        app.push_status_toast(
            app.tr(MessageId::ProviderTestConnectionNoEndpoint)
                .replace("{provider}", identity.key.as_str()),
            StatusToastLevel::Warning,
            Some(8_000),
        );
        reopen_provider_picker_list(app, engine_handle, config, selected_id, catalog_view).await;
        return;
    }
    let api_key = match scoped_config.active_route_api_key_read_only() {
        Ok(key) if !key.trim().is_empty() => key,
        _ => {
            app.push_status_toast(
                app.tr(MessageId::ProviderTestConnectionNeedKey)
                    .replace("{provider}", identity.key.as_str()),
                StatusToastLevel::Warning,
                Some(8_000),
            );
            reopen_provider_picker_list(app, engine_handle, config, selected_id, catalog_view)
                .await;
            return;
        }
    };
    let base_url = scoped_config.active_route_base_url();
    let model = scoped_config.default_model();
    let outcome = verifier.verify(provider, &api_key, &base_url).await;
    if let Err(reason) = config.verify_provider_identity(&identity) {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        return;
    }
    match outcome {
        Ok(roster) => {
            publish_verified_roster(&identity, &base_url, roster);
            app.provider_health.record_models_probe_success(
                &scoped_config,
                &identity,
                &model,
                crate::route_receipt::CredentialGeneration::derive(
                    &base_url,
                    &codewhale_secrets::normalize_api_key(&api_key),
                ),
            );
            app.push_status_toast(
                app.tr(MessageId::ProviderConnectionChecked).into_owned(),
                StatusToastLevel::Success,
                Some(8_000),
            );
        }
        Err(reason) => {
            let safe = sanitize_probe_status(&reason, &api_key);
            app.provider_health.record_models_probe_failure(
                &scoped_config,
                &identity,
                &model,
                crate::route_receipt::CredentialGeneration::derive(
                    &base_url,
                    &codewhale_secrets::normalize_api_key(&api_key),
                ),
                provider_verification_error_category(&reason),
                &safe,
            );
            app.push_status_toast(
                app.tr(MessageId::ProviderTestConnectionFailed)
                    .replace("{provider}", identity.key.as_str())
                    .replace("{error}", &safe),
                StatusToastLevel::Error,
                Some(8_000),
            );
        }
    }
    reopen_provider_picker_list(app, engine_handle, config, selected_id, catalog_view).await;
}

pub(crate) async fn apply_provider_picker_api_key(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    identity: crate::config::ProviderIdentity,
    api_key: String,
    base_url: Option<String>,
) {
    apply_provider_picker_api_key_with_verifier(
        app,
        engine_handle,
        config,
        identity,
        api_key,
        base_url,
        &LiveProviderKeyVerifier,
    )
    .await;
}

pub(crate) async fn apply_provider_picker_api_key_with_verifier(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    identity: crate::config::ProviderIdentity,
    api_key: String,
    base_url_override: Option<String>,
    verifier: &dyn ProviderKeyVerifier,
) {
    if let Err(reason) = config.verify_provider_identity(&identity) {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        return;
    }
    let provider = identity.provider;
    let mut scoped_config = config.clone();
    if let Err(reason) = scoped_config.scope_to_provider_identity(&identity) {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        return;
    }
    // #4526: a billing route chosen in the wizard is applied to the scoped
    // clone only, so the key is probed against the endpoint it will be saved
    // for without touching the on-disk config before the user confirms.
    if let Some(base_url) = base_url_override.clone()
        && let Err(reason) = scoped_config.set_provider_base_url_override(&identity, Some(base_url))
    {
        app.push_status_toast(reason.to_string(), StatusToastLevel::Error, Some(8_000));
        return;
    }
    // #3875: verify the key against the provider before opening the rest of
    // the guided flow. Nothing is persisted until the confirm stage.
    // Resolve the effective route, including compatibility routes whose
    // endpoint is selected by auth mode (notably a legacy Kimi CLI import).
    // This prevents a replacement Kimi Code API key from being probed against
    // the ordinary Moonshot endpoint.
    let base_url = scoped_config.active_route_base_url();
    let outcome = verifier.verify(provider, &api_key, &base_url).await;
    if let Err(reason) = config.verify_provider_identity(&identity) {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        return;
    }
    match outcome {
        Ok(roster) => {
            // Before the model pick reads the route roster: list what this
            // key can call today, not catalog rows the provider has retired.
            publish_verified_roster(&identity, &base_url, roster);
            // Keep the readiness row aligned with the live check the wizard
            // just completed. This probe only proves the endpoint and
            // credentials are reachable: the model is chosen after the probe,
            // so record a distinct connection-checked state rather than
            // claiming the model is ready. Providers without a real `/models`
            // probe remain unchecked.
            if crate::client::provider_api_key_verification_is_observed(provider) {
                let verified_model = scoped_config.default_model();
                app.provider_health.record_models_probe_success(
                    &scoped_config,
                    &identity,
                    &verified_model,
                    crate::route_receipt::CredentialGeneration::derive(
                        &base_url,
                        &codewhale_secrets::normalize_api_key(&api_key),
                    ),
                );
            }
            // Key is valid — continue the guided flow at model pick without
            // writing the secret yet.
            let runtime_status = query_provider_runtime_status(engine_handle).await;
            if let Some(picker) =
                crate::tui::provider_picker::ProviderPickerView::new_for_model_pick_after_validation(
                    app.api_provider,
                    &identity,
                    &scoped_config,
                    runtime_status,
                    api_key,
                    base_url_override,
                )
                .map(|picker| {
                    picker
                        .with_locale(app.ui_locale)
                        .with_provider_health(&app.provider_health)
                })
            {
                app.view_stack.push(picker);
                app.status_message = Some(
                    app.tr(MessageId::ProviderConnectionCheckedPickModel)
                        .into_owned(),
                );
            } else {
                app.status_message = Some(format!(
                    "{} {}",
                    app.tr(MessageId::ProviderConnectionChecked),
                    app.tr(MessageId::ProviderPickerNotReopened)
                ));
            }
            app.needs_redraw = true;
        }
        Err(reason) => {
            // Verification failed - keep the picker open at the key-entry
            // stage with the provider's actual error so the user can fix
            // the key instead of dead-ending with a status toast. Name the
            // endpoint the probe actually used: a 401 from the wrong host
            // (a legacy root `base_url` leaking into this route, say) is
            // otherwise indistinguishable from a bad key. The provider's raw
            // reply (often truncated JSON) is not the message: say what went
            // wrong and what to do next in plain words (#6566).
            let plain = plain_key_verification_error(app, &reason, &api_key);
            let reason = match crate::llm_client::base_url_authority(&base_url) {
                Some(authority) => format!("{plain} ({authority})"),
                None => plain.clone(),
            };
            let runtime_status = query_provider_runtime_status(engine_handle).await;
            if let Some(picker) =
                crate::tui::provider_picker::ProviderPickerView::new_for_key_entry_with_error(
                    app.api_provider,
                    &identity,
                    &scoped_config,
                    runtime_status,
                    reason,
                )
                .map(|picker| {
                    picker
                        .with_locale(app.ui_locale)
                        .with_provider_health(&app.provider_health)
                })
            {
                app.view_stack.push(picker);
                app.status_message = Some(plain);
            } else {
                app.status_message = Some(format!(
                    "{plain} {}",
                    app.tr(MessageId::ProviderPickerNotReopened)
                ));
            }
            app.needs_redraw = true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_provider_picker_setup_confirmed(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    identity: crate::config::ProviderIdentity,
    api_key: String,
    model: String,
    context_window: Option<u32>,
    base_url: Option<String>,
) -> bool {
    use crate::config::{
        save_api_key_for_identity, save_provider_base_url_for_identity,
        save_provider_context_window_for_identity, save_provider_model_for_identity,
    };

    if let Err(reason) = config.verify_provider_identity(&identity) {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        return false;
    }
    if identity.persisted_id().is_none() {
        app.push_status_toast(
            "Choose the exact custom provider table before changing its credentials or endpoint.",
            StatusToastLevel::Error,
            Some(8_000),
        );
        return false;
    }
    let provider = identity.provider;

    let model = model.trim().to_string();
    if model.is_empty() {
        app.add_message(HistoryCell::System {
            content: format!(
                "Cannot finish {} setup: default model is empty.\nProvider unchanged.",
                provider.as_str()
            ),
        });
        return false;
    }

    // #4526: the wizard's billing-route choice is written before the key so the
    // credential is saved onto the route it was verified against. It lands only
    // in that provider's own `base_url`; failing here aborts before any secret
    // is persisted rather than leaving a key on the wrong endpoint.
    if let Some(base_url) = base_url.as_deref() {
        if let Err(err) = save_provider_base_url_for_identity(&identity, config, base_url) {
            app.add_message(HistoryCell::System {
                content: format!(
                    "Failed to save {} endpoint `{base_url}`: {err}\nProvider unchanged.",
                    provider.as_str()
                ),
            });
            return false;
        }
        if let Err(reason) =
            config.set_provider_base_url_override(&identity, Some(base_url.to_string()))
        {
            app.push_status_toast(reason.to_string(), StatusToastLevel::Error, Some(8_000));
            return false;
        }
    }

    // Persist key first via the existing comment-preserving path, then pin the
    // chosen default model on the same document when the provider uses a
    // `[providers.<name>]` table.
    let mut save_confirmation = None;
    match save_api_key_for_identity(&identity, config, &api_key) {
        Ok(saved) => {
            // #5195: name where the key actually landed (secret store backend
            // + credential-free config metadata) and the scope it is visible
            // from — credential writes are rescoped to the user-global config,
            // so the key is available in every folder.
            let destination = saved.describe();
            if let Err(err) = save_provider_model_for_identity(&identity, config, &model) {
                app.add_message(HistoryCell::System {
                    content: format!(
                        "Saved {} API key to {destination} (available in all folders), but failed to pin model `{model}`: {err}",
                        provider.as_str(),
                    ),
                });
            } else if let Some(context_window) = context_window {
                if let Err(err) =
                    save_provider_context_window_for_identity(&identity, config, context_window)
                {
                    app.add_message(HistoryCell::System {
                        content: format!(
                            "Saved {} API key and model to {destination} (available in all folders), but failed to save context window: {err}",
                            provider.as_str(),
                        ),
                    });
                } else {
                    save_confirmation = Some(format!(
                        "Saved {} API key, model, and context window to {destination} (available in all folders)",
                        provider.as_str(),
                    ));
                }
            } else {
                save_confirmation = Some(format!(
                    "Saved {} API key and model to {destination} (available in all folders)",
                    provider.as_str(),
                ));
            }
            app.api_key_env_only = false;
        }
        Err(err) => {
            app.add_message(HistoryCell::System {
                content: format!(
                    "Failed to save {} API key: {err}\nProvider unchanged.",
                    provider.as_str()
                ),
            });
            return false;
        }
    }

    if let Err(reason) = config
        .scope_to_provider_identity(&identity)
        .and_then(|()| mirror_saved_model_in_config(config, &identity, model.clone()))
        .and_then(|()| {
            context_window.map_or(Ok(()), |window| {
                mirror_saved_context_window_in_config(config, &identity, window)
            })
        })
        .and_then(|()| mirror_saved_api_key_in_config(config, &identity, api_key))
    {
        app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
        return false;
    }
    let switched = switch_provider(app, engine_handle, config, identity, Some(model)).await;
    // The switch overwrites the status line with the route summary (the full
    // summary also lands in the transcript), so the save confirmation is
    // applied last — it is the answer to the action the user just confirmed.
    if switched && let Some(confirmation) = save_confirmation {
        app.status_message = Some(confirmation);
    }
    switched
}

pub(crate) async fn apply_codewhale_owned_login(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    provider: ProviderKind,
    pending: crate::oauth::PendingOAuthLogin,
    status_prefix: &str,
    login_kind: &str,
) -> bool {
    let path = app.config_path.clone();
    let mut live = config.clone();
    let activation = tokio::task::spawn_blocking(move || {
        crate::oauth::activate_login(pending, path.as_deref(), Some(&mut live))
            .map(|activation| (activation, live))
    })
    .await
    .map_err(|error| anyhow::anyhow!("OAuth activation worker failed: {error}"))
    .and_then(|result| result);
    match activation {
        Ok((activation, live)) => {
            config.refresh_provider_routes_from(&live);
            // The account line goes to the transcript: the status line is
            // overwritten by the route summary once the switch lands.
            let locale = app.ui_locale;
            let content = match activation.env_override_warning(locale) {
                Some(warning) => format!("{}\n{warning}", activation.summary(locale)),
                None => activation.summary(locale),
            };
            app.add_message(HistoryCell::System { content });
            app.status_message = Some(format!(
                "{status_prefix}; activated {} via {}",
                codewhale_config::quote_os_path(&activation.auth_path),
                codewhale_config::quote_os_path(&activation.config_path)
            ));
            app.api_key_env_only = false;
        }
        Err(err) => {
            app.add_message(HistoryCell::System {
                content: format!(
                    "Failed to finalize {} {login_kind}: {err:#}\nProvider unchanged.",
                    provider.as_str()
                ),
            });
            return false;
        }
    }

    if provider == ProviderKind::OpenaiCodex {
        let mut selected = config.clone();
        selected.provider = Some(provider.as_str().to_string());
        if crate::codex_model_cache::update_from_chatgpt(&selected)
            .await
            .is_err()
        {
            app.push_status_toast(
                format!(
                    "{}  codewhale models --update --provider openai-codex",
                    app.tr(MessageId::ProviderNoCatalogModels)
                ),
                StatusToastLevel::Warning,
                Some(App::STICKY_ERROR_TTL_MS),
            );
            return false;
        }
    }
    let identity = match config.builtin_provider_identity(provider) {
        Ok(identity) => identity,
        Err(reason) => {
            app.push_status_toast(reason, StatusToastLevel::Error, Some(8_000));
            return false;
        }
    };
    switch_provider(app, engine_handle, config, identity, None).await
}

pub(crate) async fn apply_codewhale_owned_xai_login(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    pending: crate::oauth::PendingOAuthLogin,
    status_prefix: &str,
) -> bool {
    apply_codewhale_owned_login(
        app,
        engine_handle,
        config,
        ProviderKind::Xai,
        pending,
        status_prefix,
        "device login",
    )
    .await
}

pub(crate) async fn apply_codewhale_owned_chatgpt_login(
    app: &mut App,
    engine_handle: &mut EngineHandle,
    config: &mut Config,
    pending: crate::oauth::PendingOAuthLogin,
    status_prefix: &str,
) -> bool {
    apply_codewhale_owned_login(
        app,
        engine_handle,
        config,
        ProviderKind::OpenaiCodex,
        pending,
        status_prefix,
        "ChatGPT sign-in",
    )
    .await
}

/// `/auth chatgpt-revoke`. The remote revoke is one blocking HTTP round trip
/// per stored token under the OAuth lifecycle lock, so it runs on the blocking
/// pool instead of the event loop. It targets the session's own config file
/// and clears the live route afterwards so the header stops claiming OAuth.
pub(crate) async fn run_chatgpt_revoke_from_tui(app: &mut App, config: &mut Config) {
    let config_path = app.config_path.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        crate::oauth::revoke_owned_login(
            crate::oauth::OAuthProvider::Chatgpt,
            config_path.as_deref(),
            None,
        )
    })
    .await
    .map_err(|err| anyhow::anyhow!("ChatGPT revoke task was lost: {err}"))
    .and_then(|result| result);
    // Clear the live route even when remote revocation could not be confirmed;
    // local credential removal may already have succeeded in that outcome.
    let live_clear = config.clear_codewhale_owned_chatgpt_oauth();
    let message = match (outcome, live_clear) {
        (Ok(()), Ok(())) => {
            "Revoked Codewhale-owned ChatGPT tokens. Codex CLI consent is unchanged.".to_string()
        }
        (Ok(()), Err(err)) => format!(
            "ChatGPT tokens were revoked, but the live route could not be refreshed: {err:#}"
        ),
        (Err(err), Ok(())) => format!("ChatGPT revoke failed: {err:#}"),
        (Err(err), Err(live_err)) => format!(
            "ChatGPT revoke failed: {err:#}. The live route could not be refreshed: {live_err:#}"
        ),
    };
    app.add_message(HistoryCell::System {
        content: message.clone(),
    });
    app.status_message = Some(message);
    app.needs_redraw = true;
}

/// `/auth orcarouter-revoke`. OrcaRouter mints a durable API key with no remote
/// revocation endpoint this client owns, so revoke is local-only: clear the
/// `orcarouter` secret-store slot, the route's saved key, and the in-memory
/// override. Re-authenticating is a fresh PKCE sign-in or a freshly pasted key.
pub(crate) async fn run_orcarouter_revoke_from_tui(app: &mut App, config: &mut Config) {
    let provider = ProviderKind::Orcarouter;
    let provider_name = provider.as_str().to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        crate::config::clear_active_provider_api_key(&provider_name)
    })
    .await
    .map_err(|err| anyhow::anyhow!("OrcaRouter revoke task was lost: {err}"))
    .and_then(|result| result);
    let live_clear = match config.builtin_provider_identity(provider) {
        Ok(identity) => config
            .set_provider_api_key_override(&identity, None)
            .map_err(|error| anyhow::anyhow!(error.to_string())),
        Err(err) => Err(anyhow::anyhow!(err)),
    };
    let message = match (outcome, live_clear) {
        (Ok(()), Ok(())) => "Removed Codewhale's saved OrcaRouter credential.".to_string(),
        (Ok(()), Err(err)) => {
            format!("OrcaRouter credential removed; the live route could not be refreshed: {err:#}")
        }
        (Err(err), Ok(())) => format!("OrcaRouter revoke failed: {err:#}"),
        (Err(err), Err(live_err)) => format!(
            "OrcaRouter revoke failed: {err:#}. The live route could not be refreshed: {live_err:#}"
        ),
    };
    app.add_message(HistoryCell::System {
        content: message.clone(),
    });
    app.status_message = Some(message);
    app.needs_redraw = true;
}

#[cfg(test)]
pub(crate) fn apply_loaded_session(
    app: &mut App,
    config: &mut Config,
    session: &SavedSession,
) -> Result<(), String> {
    apply_loaded_session_with_goal(app, config, session.clone(), None)
}

/// Install a loaded session as the live conversation. The session is taken
/// by value because it is consumed: its journal, history, artifacts and
/// metadata move into `app` instead of being cloned beside a copy the caller
/// would drop right after (memory note M3). On `Err` nothing was installed.
pub(crate) fn apply_loaded_session_with_goal(
    app: &mut App,
    config: &mut Config,
    mut session: SavedSession,
    goal: Option<&crate::session_manager::SessionGoalState>,
) -> Result<(), String> {
    let mut recovered_binding = None;
    if let Some(binding) = session.metadata.runtime_store.as_ref()
        && let Some(tasks) = app.runtime_services.task_manager.as_ref()
        && tasks.session_store_binding().as_ref() != Some(binding)
    {
        // A switch can rebind the conversation but cannot carry the saved
        // store's durable work into the running host, so it may only adopt a
        // store there is nothing to lose from leaving: one that is missing, or
        // one that exists and is provably empty *and* provably unheld, with no
        // scope-pinned automation. A force-quit leaves the second shape — the
        // store is on disk, ownerless and holding zero events — and refusing
        // it protected nothing while making the session unopenable (#6207).
        let refusal = if binding
            .is_missing_session_store()
            .map_err(|error| error.to_string())?
        {
            None
        } else {
            binding
                .adoption_refusal()
                .map_err(|error| error.to_string())?
        };
        if refusal.is_none() {
            recovered_binding = tasks.session_store_binding();
        }
        if let Some(crate::runtime_threads::StoreAdoptionRefusal::HeldByLiveProcess) = refusal {
            // A fresh `codewhale resume` would meet the same live holder, so
            // name the step that actually frees the store (#6418).
            return Err(format!(
                "This session's saved Runtime store is open in another running \
                 Codewhale process. Close that session there, then open this one \
                 again, or run `codewhale resume {}` after it exits.",
                session.metadata.id
            ));
        }
        if recovered_binding.is_none() {
            let reason = refusal
                .map(|refusal| format!(" ({refusal})"))
                .unwrap_or_default();
            // Name the real condition and the path that actually works. The
            // old wording ("resume it in a new Codewhale process") sent users
            // in circles: starting a new process and then picking the session
            // from `/resume` lands here again, because that is this same
            // switch path. Opening the session *at launch* is a different
            // route — `TaskManager::start` passes the saved binding through to
            // `open_for_session`, which validates the existing store and
            // adopts it (runtime_threads.rs, `validate_existing_store` then
            // `open_inner`). So the advice has to say which one (#6207, #6225).
            return Err(format!(
                "This session's saved Runtime store belongs to a different host{reason}. \
                 Switching to it from inside a running session cannot carry that \
                 store's queued work across, but opening it directly can: run \
                 `codewhale resume {}` from your shell.",
                session.metadata.id
            ));
        }
    }
    if app.session_transition_blocked() {
        return Err(
            "runtime work is active; wait for the current turn, maintenance, and background tasks to finish, or cancel that specific work before switching sessions".to_string(),
        );
    }
    if let Some(goal) = goal {
        goal.validate()
            .map_err(|error| format!("saved session goal is invalid: {error}"))?;
    }
    let provider_identity = config.resolve_persisted_provider_identity(
        Some(&session.metadata.model_provider),
        session.metadata.model_provider_id.as_deref(),
    )?;
    let restored_route = resolve_runtime_route_for_identity(
        config,
        &provider_identity,
        Some(&session.metadata.model),
    )
    .map_err(|reason| {
        format!(
            "saved session provider '{}' could not be resolved from the live config: {reason}. Codewhale will not fall back",
            provider_identity.key
        )
    })?;
    // Restore/validate the contended state before mutating conversation or
    // workspace fields. A failed session switch must leave the current session
    // wholly intact.
    let queue_transition = prepare_offline_queue_transition(app, &session.metadata.id)?;
    if let Some(binding) = recovered_binding.as_ref() {
        // Only the conversation is recovered into this idle host. Its missing
        // runtime's tasks and approvals are never imported or re-admitted.
        // Repair its binding before changing live Work state. If Work restore
        // is contended, the current conversation stays intact and a retry can
        // use this durably repaired binding to the same host.
        let mut recovered = session.clone();
        let abandoned = recovered.metadata.runtime_store.replace(binding.clone());
        let manager = SessionManager::default_location()
            .map_err(|error| format!("Session recovery could not be saved: {error}"))?;
        manager
            .save_session(&recovered)
            .map_err(|error| format!("Session recovery could not be saved: {error}"))?;
        // The conversation now lives in this host's store. The empty store it
        // left is set aside here, where it is abandoned, unless another
        // document still binds it (#6144 P1a) — otherwise it stayed on disk
        // with nothing pointing at it.
        if let Some(abandoned) = abandoned {
            crate::session_reconcile::retire_unbound_store_in_background(
                manager,
                abandoned.data_dir,
                "conversation rebound to another host's store",
            );
        }
    }
    app.restore_work_state(
        &session.metadata.id,
        &session.metadata.workspace,
        session.work_state.as_ref(),
    )?;
    install_offline_queue_transition(app, queue_transition);
    // All fallible preflight is complete. Retire the old session's background
    // accounting atomically before mutating live state; any late old-scope
    // provider response is rejected by `cost_status::report`.
    let _settled_old_cost_scope = crate::cost_status::close_current_scope();
    *config = *restored_route.config;
    app.refresh_notification_settings(config);
    app.restore_api_messages_from_owned(&mut session);
    app.clear_history();
    app.tool_cells.clear();
    app.tool_details_by_cell.clear();
    app.active_cell = None;
    app.active_tool_details.clear();
    app.active_tool_entry_completed_at.clear();
    app.active_cell_revision = app.active_cell_revision.wrapping_add(1);
    app.exploring_cell = None;
    app.exploring_entries.clear();
    app.ignored_tool_calls.clear();
    app.pending_tool_uses.clear();
    app.last_exec_wait_command = None;
    let messages = app.api_messages.clone();
    let mut message_to_cell = std::collections::HashMap::new();
    // Failed-turn notices are replayed where they happened: after the
    // messages that existed when the turn ended (clamped to the transcript).
    let mut turn_outcomes = session.turn_outcomes.iter().peekable();
    let mut replay_outcomes_through = |app: &mut App, message_count: usize, last: bool| {
        while let Some(outcome) =
            turn_outcomes.next_if(|outcome| last || outcome.after_message_count <= message_count)
        {
            app.extend_history(std::iter::once(HistoryCell::Error {
                message: outcome.error.clone(),
                severity: crate::error_taxonomy::ErrorSeverity::Warning,
            }));
        }
    };
    replay_outcomes_through(app, 0, messages.is_empty());
    for (message_index, msg) in messages.iter().enumerate() {
        let mut cells = history_cells_from_message(msg);
        if msg.role == "user"
            && session
                .context_references
                .iter()
                .any(|record| record.message_index == message_index)
        {
            for cell in &mut cells {
                if let HistoryCell::User { content } = cell {
                    *content = compact_user_context_display(content);
                }
            }
        }
        let base = app.history.len();
        if msg.role == "user"
            && let Some(offset) = cells
                .iter()
                .position(|cell| matches!(cell, HistoryCell::User { .. }))
        {
            message_to_cell.insert(message_index, base + offset);
        }
        app.extend_history(cells);
        replay_outcomes_through(app, message_index + 1, message_index + 1 == messages.len());
    }
    app.rebuild_completed_assistant_outputs_from_restored_history();
    app.sync_context_references_from_session(&session.context_references, &message_to_cell);
    app.mark_history_updated();
    app.viewport.transcript_selection.clear();
    // Goal state is session-owned just like Work state. A legacy/no-goal
    // session clears the previous session's objective; a durable sidecar
    // rebuilds both the visible hunt and the EngineConfig seeded below.
    app.goal = crate::tui::app::HostGoalState::default();
    app.last_known_goal_state = None;
    app.pending_goal_controls.clear();
    if let Some(goal) = goal {
        let snapshot = goal.to_runtime_snapshot();
        let _ = apply_goal_snapshot_to_app(app, &snapshot);
    }
    restore_loaded_session_provider(app, config, provider_identity)?;
    // Session records do not own a reasoning preference. `set_model_selection`
    // restores the raw explicit global preference for Auto (or releases an
    // implicit fixed-route default) instead of reusing normalized live state.
    app.set_model_selection(session.metadata.model.clone());
    if app.auto_model
        && let Some(saved) = session.last_auto_route.as_ref()
        && !saved.provider_identity.trim().is_empty()
        && !saved.model.trim().is_empty()
    {
        app.last_effective_provider = Some(saved.provider);
        app.last_effective_provider_identity = Some(saved.provider_identity.clone());
        app.last_effective_model = Some(saved.model.clone());
        app.last_auto_route_receipt = Some(saved.receipt.clone());
        app.last_effective_reasoning_effort = saved.effective_reasoning_effort.map(Into::into);
    }
    resolve_loaded_session_route(app, config);
    if !app.auto_model {
        let requested = app
            .reasoning_effort_preference
            .unwrap_or(app.reasoning_effort);
        app.reasoning_effort =
            requested.normalize_for_route(app.api_provider, &app.active_route_base_url, &app.model);
    }
    app.provider_models.insert(
        app.provider_identity_for_persistence().to_string(),
        app.model_selection_for_persistence(),
    );
    app.update_model_compaction_budget();
    apply_workspace_runtime_state(app, config, session.metadata.workspace.clone());
    if let Some(mode) = session.metadata.mode.as_deref().and_then(AppMode::parse) {
        app.set_mode(mode);
    }
    app.session.total_tokens = u32::try_from(session.metadata.total_tokens).unwrap_or(u32::MAX);
    app.session.total_conversation_tokens = app.session.total_tokens;
    let restored_parent = crate::pricing::CostEstimate {
        usd: session.metadata.cost.session_cost_usd,
        cny: session.metadata.cost.session_cost_cny,
    }
    .sanitized();
    let restored_background = crate::pricing::CostEstimate {
        usd: session.metadata.cost.subagent_cost_usd,
        cny: session.metadata.cost.subagent_cost_cny,
    }
    .sanitized();
    // A restored session has no live billed receipt; the estimate rules the
    // meter until the next model call reports one.
    app.last_billed_input_tokens = None;
    app.session.session_cost = restored_parent.usd;
    app.session.session_cost_cny = restored_parent.cny;
    app.session.subagent_cost = restored_background.usd;
    app.session.subagent_cost_cny = restored_background.cny;
    app.session.subagent_usage_sources = session
        .metadata
        .cost
        .usage_source_fingerprints
        .iter()
        .cloned()
        .collect();
    crate::cost_status::restore_usage_source_ledger(
        session
            .metadata
            .cost
            .usage_source_fingerprints
            .iter()
            .cloned(),
        &session.metadata.cost.missing_usage_sources,
        session.metadata.cost.missing_usage_overflowed,
    );
    // Coverage is restored *with* the money, and the live counters are cleared
    // first: whatever the previous session in this process priced is not inside
    // the total being loaded, so carrying those counters over would describe the
    // wrong total (#4318).
    app.reset_cost_coverage();
    app.session.missing_usage_sources = session.metadata.cost.missing_usage_sources.clone();
    app.session.missing_usage_overflowed = session.metadata.cost.missing_usage_overflowed;
    app.session.cost_priced_turns = session.metadata.cost.priced_turns;
    app.session.cost_unpriced_turns = session.metadata.cost.unpriced_turns;
    app.session.cost_cny_priced_turns = session.metadata.cost.cny_priced_turns;
    app.session.cost_cny_unpriced_turns = session.metadata.cost.cny_unpriced_turns;
    app.session.cost_unpriced_reasons = session.metadata.cost.unpriced_reasons.clone();
    app.session.cost_cny_unpriced_reasons = session.metadata.cost.cny_unpriced_reasons.clone();
    app.session.cost_unpriced_classes = session.metadata.cost.unpriced_classes.clone();
    app.session.cost_pricing_provenances = session.metadata.cost.pricing_provenances.clone();
    app.session.cost_live_pricing_defects = session.metadata.cost.live_pricing_defects.clone();
    app.session.cost_live_pricing_unusable_defects =
        session.metadata.cost.live_pricing_unusable_defects.clone();
    app.session.cost_route_receipts = session.metadata.cost.route_receipts.clone();
    // A pre-coverage session deserializes its new fields from serde defaults,
    // which are indistinguishable from "complete total, zero turns". Flag it so
    // `/cost` says the coverage is unknown rather than claiming completeness,
    // including for an all-zero record.
    app.session.cost_coverage_unknown_legacy = session.metadata.cost.coverage_is_legacy_unknown();
    // Restore the high-water marks from persisted metadata so the
    // monotonic cost guarantee (#244) survives session restarts.
    // Take the max with the current totals — old sessions without
    // persisted high-water fields deserialise to 0.0 and fall back to
    // the restored total with no regression.
    let total_restored_usd = session.metadata.cost.total_usd();
    let total_restored_cny = session.metadata.cost.total_cny();
    let restored_high_water = crate::pricing::CostEstimate {
        usd: session.metadata.cost.displayed_cost_high_water_usd,
        cny: session.metadata.cost.displayed_cost_high_water_cny,
    }
    .sanitized();
    app.session.displayed_cost_high_water = restored_high_water.usd.max(total_restored_usd);
    app.session.displayed_cost_high_water_cny = restored_high_water.cny.max(total_restored_cny);
    app.session.last_prompt_tokens = None;
    app.session.last_completion_tokens = None;
    app.session.last_prompt_cache_hit_tokens = None;
    app.session.last_prompt_cache_miss_tokens = None;
    app.session.last_reasoning_replay_tokens = None;
    // Accumulated token breakdown is per-runtime-session; reset on load.
    app.session.reset_token_breakdown();
    // The metrics strip shares that scope: it describes this runtime
    // session's calls, not the restored transcript's.
    app.session_metrics = crate::tui::session_metrics::SessionMetrics::default();
    app.session.turn_cache_history.clear();
    // Restore cumulative turn duration so the footer "worked" chip
    // persists across session restarts (#2038).
    app.cumulative_turn_duration =
        std::time::Duration::from_secs(session.metadata.cumulative_turn_secs);
    app.current_session_id = Some(session.metadata.id.clone());
    app.session_title = Some(session.metadata.title.clone());
    app.current_session_metadata = Some(session.metadata);
    reset_approval_scope_for_new_conversation(app);
    if let Some(binding) = recovered_binding {
        if let Some(metadata) = app.current_session_metadata.as_mut() {
            metadata.runtime_store = Some(binding);
        }
        app.push_status_toast(
            app.tr(MessageId::RuntimeStoreRecovered).into_owned(),
            StatusToastLevel::Warning,
            None,
        );
    }
    app.session_artifacts = session.artifacts;
    app.session_turn_outcomes = session.turn_outcomes;
    app.window_title = session.window_title;
    app.workspace_context = None;
    app.workspace_is_linked_worktree = false;
    app.workspace_context_refreshed_at = None;
    app.system_prompt = session.system_prompt.map(SystemPrompt::Text);
    app.scroll_to_bottom();
    Ok(())
}

pub(crate) fn apply_loaded_session_config_snapshot(
    app: &mut App,
    config: &mut Config,
    session: SavedSession,
    mut next_config: Config,
    force_engine_respawn: bool,
) -> Result<bool, String> {
    if force_engine_respawn {
        // File `/load` supplies a freshly loaded disk snapshot, but the live
        // Config also contains CLI and workspace/project overlays that are not
        // represented by that file. Refresh the provider registry atomically
        // over the effective Config instead of dropping permission controls.
        let mut effective_config = config.clone();
        effective_config.refresh_provider_routes_from(&next_config);
        next_config = effective_config;
    }
    let previous_provider = app.api_provider;
    let previous_provider_identity = app.provider_identity_for_persistence().to_string();
    let previous_workspace = app.workspace.clone();
    let goal = SessionManager::default_location()
        .and_then(|manager| manager.load_session_goal(&session.metadata.id))
        .map_err(|error| format!("saved session goal could not be loaded: {error}"))?;
    apply_loaded_session_with_goal(app, &mut next_config, session, goal.as_ref())?;
    // A file load reads a fresh disk snapshot. Even when the route's enum and
    // exact identity are unchanged, endpoint, key, headers, TLS, or retry
    // settings may have changed. Rebuild from that same validated snapshot so
    // compaction and other pre-turn engine work cannot retain the old client.
    let respawn = force_engine_respawn
        || loaded_session_requires_engine_respawn(
            app,
            previous_provider,
            &previous_provider_identity,
            &previous_workspace,
        );
    *config = next_config;
    app.configured_models = config.custom_models.clone().unwrap_or_default();
    crate::config::initialize_cloud_facts(config);
    app.refresh_notification_settings(config);
    Ok(respawn)
}

#[cfg(test)]
mod profile_snapshot_tests {
    use super::*;

    fn profile_fixture(model: &str, base_url: &str) -> Config {
        let mut config: Config = toml::from_str(include_str!(
            "../../../../config/tests/fixtures/custom_models.toml"
        ))
        .expect("profile fixture");
        config.set_legacy_root(Some("profile-snapshot-local-fixture".to_string()), None);
        config.default_text_model = Some(model.to_string());
        config.providers.as_mut().unwrap().deepseek.base_url = Some(base_url.to_string());
        let declaration = &mut config.custom_models.as_mut().unwrap()[0];
        declaration.id = model.to_string();
        declaration.base_url = base_url.to_string();
        config
    }

    #[test]
    fn profile_switch_replaces_metadata_and_validated_route_snapshot() {
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let _env = crate::test_support::lock_test_env();
                let home = tempfile::tempdir().unwrap();
                let _home = crate::test_support::EnvVarGuard::set(
                    "CODEWHALE_HOME",
                    home.path().as_os_str(),
                );
                let mut config = profile_fixture("old-preview", "https://old.example.test/v1");
                let mut options = crate::test_support::test_tui_options(home.path());
                options.model = config.default_model();
                let mut app = App::new(options, &config);
                assert_eq!(app.configured_models[0].id, "old-preview");

                let next = profile_fixture("new-preview", "https://new.example.test/v1");
                let route = validated_profile_default_route(&next).unwrap();
                assert_eq!(
                    route.context_window.source,
                    crate::route_runtime::ContextWindowSource::UserDeclared,
                );
                let expected_models = next.custom_models.clone().unwrap();
                apply_validated_profile_config(&mut app, &mut config, "new", next, &route);
                assert_eq!(app.config_profile.as_deref(), Some("new"));
                assert_eq!(app.configured_models, expected_models);
                assert_eq!(app.configured_models, config.custom_models.clone().unwrap());
                assert_eq!(app.model, "new-preview");
                assert_eq!(
                    app.active_route_base_url,
                    route.candidate.endpoint().base_url
                );
                assert_eq!(app.active_route_limits, Some(route.candidate.limits()));
                assert_eq!(
                    app.active_context_window_source,
                    route.context_window.source
                );

                let mut empty = profile_fixture("no-metadata", "https://empty.example.test/v1");
                empty.custom_models = None;
                let empty_route = validated_profile_default_route(&empty).unwrap();
                apply_validated_profile_config(&mut app, &mut config, "empty", empty, &empty_route);
                assert!(app.configured_models.is_empty());
                assert!(config.custom_models.is_none());
                assert_eq!(app.config_profile.as_deref(), Some("empty"));
                assert_eq!(app.model, "no-metadata");
                assert_eq!(
                    app.active_route_base_url,
                    empty_route.candidate.endpoint().base_url
                );
                assert_eq!(
                    app.active_context_window_source,
                    empty_route.context_window.source
                );
                assert_ne!(
                    app.active_context_window_source,
                    crate::route_runtime::ContextWindowSource::UserDeclared,
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
}

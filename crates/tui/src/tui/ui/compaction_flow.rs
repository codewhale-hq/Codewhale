//! Compaction UI state: manual/automatic compaction queueing, settlement,
//! receipts, and cancel behavior (TUI_MODULARIZATION.md slice 7). The engine
//! owns the actual summarization; this module projects its lifecycle into the
//! UI and never awaits the bounded engine mailbox from the event loop.

use super::*;

/// Queue a live compaction update without waiting on the engine mailbox.
///
/// Config edits are valid while a turn is streaming, but awaiting a bounded
/// engine mailbox from the UI event loop can make the whole TUI appear frozen
/// when the turn is busy. A dropped refresh is safe: the next turn rebuilds
/// its compaction config from `App`, and the status message tells the user
/// whether the update was queued or deferred.
///
/// The model and its compaction budget are one route update: both mailbox
/// slots are reserved before either op is sent, so a full mailbox defers the
/// whole update instead of applying the model without its budget (U03-05).
pub(crate) fn try_apply_model_and_compaction_update(
    engine_handle: &EngineHandle,
    compaction: codewhale_engine::compaction::CompactionConfig,
    mode: AppMode,
    route_limits: Option<codewhale_config::route::RouteLimits>,
) -> bool {
    let Ok(model_permit) = engine_handle.tx_op.clone().try_reserve_owned() else {
        return false;
    };
    let Ok(compaction_permit) = engine_handle.tx_op.clone().try_reserve_owned() else {
        return false;
    };
    engine_handle.send_reserved_op(
        model_permit,
        Op::SetModel {
            model: compaction.model.clone(),
            mode,
            route_limits,
        },
    );
    engine_handle.send_reserved_op(compaction_permit, Op::SetCompaction { config: compaction });
    true
}

pub(crate) fn set_explicit_compaction_status(
    app: &mut App,
    text: String,
    level: StatusToastLevel,
    sticky: bool,
) {
    app.status_message = Some(text.clone());
    // This lifecycle reducer assigns the semantic level explicitly. Mark the
    // legacy status bridge as synchronized so it cannot add a second,
    // keyword-classified toast with a different level on the next frame.
    app.last_status_message_seen = Some(text.clone());
    if sticky {
        app.set_sticky_status(text, level, Some(App::STICKY_ERROR_TTL_MS));
    } else {
        app.push_status_toast(text, level, Some(5_000));
    }
}

/// Queue manual compaction without ever awaiting the bounded engine mailbox
/// from the terminal event loop.
///
/// During an active turn, a successful send is intentionally deferred until
/// the engine returns to its outer operation loop. Full and closed mailboxes
/// are rejected immediately with an actionable receipt, so `/compact` cannot
/// freeze keyboard input or rendering.
pub(crate) fn try_queue_manual_compaction(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    focus: Option<String>,
) {
    if app.is_compacting || app.manual_compaction_queued {
        let text = app
            .tr(MessageId::ContextCompactionAlreadyRunning)
            .into_owned();
        add_compaction_receipt(app, &text);
        set_explicit_compaction_status(app, text, StatusToastLevel::Warning, false);
        return;
    }

    let route = match validated_app_runtime_route(app, config) {
        Ok(route) => route,
        Err(error) => {
            let text = app
                .tr(MessageId::ContextCompactionRouteInvalid)
                .replace("{error}", &error.to_string());
            add_compaction_receipt(app, &text);
            set_explicit_compaction_status(app, text, StatusToastLevel::Error, true);
            return;
        }
    };
    let mut compaction = compaction_for_validated_route(app, &route);
    compaction.focus = focus.clone();
    let request_id = format!("compact_{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let op = Op::CompactContext {
        id: request_id.clone(),
        route: Box::new(route.into_resolved()),
        compaction: Box::new(compaction),
    };

    match engine_handle.try_send(op) {
        Ok(()) => {
            app.manual_compaction_queued = true;
            app.manual_compaction_id = Some(request_id);
            let id = if app.is_loading {
                MessageId::ContextCompactionQueued
            } else {
                MessageId::ContextManualCompacting
            };
            let text = app.tr(id).into_owned();
            // Queued-behind-a-turn is a state the user must be able to find
            // again after the 5s toast: leave it in the transcript too.
            if app.is_loading {
                add_compaction_receipt(app, &text);
            }
            set_explicit_compaction_status(app, text, StatusToastLevel::Info, false);
        }
        Err(error) => {
            let full = error
                .downcast_ref::<tokio::sync::mpsc::error::TrySendError<Op>>()
                .is_some_and(|send_error| {
                    matches!(send_error, tokio::sync::mpsc::error::TrySendError::Full(_))
                });
            if full {
                // A saturated mailbox is a timing accident of the active turn,
                // not a user error. Queue client-side and let the event loop
                // retry once the engine drains a slot; the user sees the same
                // queued receipt as the ordinary behind-a-turn path.
                app.manual_compaction_queued = true;
                app.manual_compaction_id = Some(request_id);
                app.deferred_manual_compaction = Some(focus);
                let text = app.tr(MessageId::ContextCompactionQueued).into_owned();
                add_compaction_receipt(app, &text);
                set_explicit_compaction_status(app, text, StatusToastLevel::Info, false);
            } else {
                let text = app.tr(MessageId::ContextCompactionQueueClosed).into_owned();
                add_compaction_receipt(app, &text);
                set_explicit_compaction_status(app, text, StatusToastLevel::Error, true);
            }
        }
    }
}

/// Retry a manual compaction that was deferred by a full engine mailbox.
///
/// Called once per event-loop iteration. Silent by design: the queued receipt
/// was already written when the request was deferred, a still-full mailbox
/// just waits for the next iteration, and a compaction that started or
/// settled in the meantime supersedes the request entirely (handled by
/// `apply_compaction_started`/`settle_compaction`).
pub(crate) fn flush_deferred_manual_compaction(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
) {
    if app.deferred_manual_compaction.is_none() || app.is_compacting {
        return;
    }
    let route = match validated_app_runtime_route(app, config) {
        Ok(route) => route,
        Err(error) => {
            app.deferred_manual_compaction = None;
            app.manual_compaction_queued = false;
            app.manual_compaction_id = None;
            let text = app
                .tr(MessageId::ContextCompactionRouteInvalid)
                .replace("{error}", &error.to_string());
            add_compaction_receipt(app, &text);
            set_explicit_compaction_status(app, text, StatusToastLevel::Error, true);
            return;
        }
    };
    let focus = app.deferred_manual_compaction.clone().unwrap_or_default();
    let Some(request_id) = app.manual_compaction_id.clone() else {
        app.deferred_manual_compaction = None;
        app.manual_compaction_queued = false;
        return;
    };
    let mut compaction = compaction_for_validated_route(app, &route);
    compaction.focus = focus;
    let op = Op::CompactContext {
        id: request_id,
        route: Box::new(route.into_resolved()),
        compaction: Box::new(compaction),
    };
    match engine_handle.try_send(op) {
        Ok(()) => {
            app.deferred_manual_compaction = None;
        }
        Err(error) => {
            let full = error
                .downcast_ref::<tokio::sync::mpsc::error::TrySendError<Op>>()
                .is_some_and(|send_error| {
                    matches!(send_error, tokio::sync::mpsc::error::TrySendError::Full(_))
                });
            if !full {
                app.deferred_manual_compaction = None;
                app.manual_compaction_queued = false;
                app.manual_compaction_id = None;
                let text = app.tr(MessageId::ContextCompactionQueueClosed).into_owned();
                add_compaction_receipt(app, &text);
                set_explicit_compaction_status(app, text, StatusToastLevel::Error, true);
            }
        }
    }
}

pub(crate) fn apply_compaction_started(app: &mut App, id: String, auto: bool) {
    if app.sticky_status.as_ref().is_some_and(|status| {
        matches!(
            status.kind,
            crate::tui::app::StatusToastKind::ContextPressure(_)
        )
    }) {
        app.clear_sticky_status();
    }
    app.context_pressure_warning_dismissed = None;
    if !auto {
        app.manual_compaction_queued = false;
        if app.manual_compaction_id.as_deref() == Some(id.as_str()) {
            app.manual_compaction_id = None;
        }
    }
    // A compaction is running; a deferred manual request is now redundant.
    // Dropping it must also release the queued flag when the running pass is
    // automatic, or `/compact` would report "already in progress" forever.
    if app.deferred_manual_compaction.take().is_some() && auto {
        app.manual_compaction_queued = false;
        app.manual_compaction_id = None;
    }
    app.active_compaction = Some(ActiveCompaction { id, auto });
    app.is_compacting = true;
    if !auto {
        let text = app.tr(MessageId::ContextManualCompacting).into_owned();
        set_explicit_compaction_status(app, text, StatusToastLevel::Info, false);
    }
}

/// Clear the compaction-in-flight state for a terminal lifecycle event.
///
/// An exact id match clears normally. A terminal event with NO tracked
/// compaction is still authoritative (the started event can be lost to a
/// dropped drain or session switch): without this, `is_compacting`/
/// `manual_compaction_queued` stayed latched and every later `/compact` was
/// silently rejected as "already in progress". A stale event while a NEWER
/// compaction is live must not clear it (or report anything) — that live
/// pass gets its own terminal event. Returns whether the event settled.
pub(crate) fn settle_compaction(app: &mut App, id: &str, auto: bool) -> bool {
    if app
        .active_compaction
        .as_ref()
        .is_some_and(|active| active.id != id || active.auto != auto)
    {
        return false;
    }
    app.active_compaction = None;
    app.is_compacting = false;
    if !auto {
        app.manual_compaction_queued = false;
        app.manual_compaction_id = None;
    }
    // A settled pass makes a still-deferred manual request redundant (the
    // context was just compacted). Dropping it releases the queued flag so a
    // later `/compact` is not rejected as "already in progress".
    if app.deferred_manual_compaction.take().is_some() {
        app.manual_compaction_queued = false;
        app.manual_compaction_id = None;
    }
    true
}

/// Durable transcript receipt for a compaction outcome.
///
/// Outcome feedback used to be toast-only, and the engine emits
/// `TurnComplete` immediately after the compaction event — both land in the
/// same UI drain batch, so the turn's "done" status replaced the completion
/// toast before a single frame was drawn. `/compact` looked like a no-op
/// even when the summary committed (the v0.9.6 release blocker).
pub(crate) fn add_compaction_receipt(app: &mut App, message: &str) {
    app.add_message(HistoryCell::System {
        content: message.to_string(),
    });
}

pub(crate) fn apply_compaction_completed(
    app: &mut App,
    id: &str,
    auto: bool,
    message: String,
    messages_before: Option<usize>,
    messages_after: Option<usize>,
    summary_prompt: Option<String>,
) {
    if settle_compaction(app, id, auto) {
        // The billed prompt receipt described the pre-compaction context;
        // after the rewrite the local estimate is the honest signal until
        // the next model call bills the new, smaller prompt (#5577).
        app.last_billed_input_tokens = None;
        let keep = codewhale_engine::compaction::inspect_compaction_keep(&app.api_messages);
        let path = if summary_prompt
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
        {
            codewhale_engine::compaction::CompactionPath::Summary
        } else {
            codewhale_engine::compaction::CompactionPath::PruneOnly
        };
        let after = messages_after.unwrap_or(app.api_messages.len());
        let before = messages_before.unwrap_or(after);
        app.last_compaction = Some(codewhale_engine::compaction::LastCompactionSnapshot {
            auto,
            coverage: codewhale_engine::compaction::CompactionCoverage {
                path,
                last_round_messages: keep.last_round_messages,
                last_round_tool_results: keep.last_round_tool_results,
                last_round_assistant: keep.last_round_assistant,
                dropped_messages: before.saturating_sub(after),
                anchors_chars: codewhale_engine::compaction::pinned_anchors_text(Some(
                    &app.workspace,
                ))
                .map(|text| text.chars().count())
                .unwrap_or(0),
                // Only the summary path builds a replacement history, so only
                // it spent a verbatim budget (#5956).
                retained_user_message_tokens: match path {
                    codewhale_engine::compaction::CompactionPath::Summary => {
                        app.compaction_retained_user_message_tokens
                    }
                    codewhale_engine::compaction::CompactionPath::PruneOnly => 0,
                },
                operator_instructions_applied: matches!(
                    path,
                    codewhale_engine::compaction::CompactionPath::Summary
                ) && app.compaction_summary_instructions.is_some(),
            },
            messages_before: before,
            messages_after: after,
        });
        // Automatic maintenance stays in the context inspector and event
        // receipts; it does not insert a ceremony into the user's task.
        if !auto {
            add_compaction_receipt(app, &message);
            set_explicit_compaction_status(app, message, StatusToastLevel::Success, false);
        }
    }
}

pub(crate) fn apply_compaction_failed(app: &mut App, id: &str, auto: bool, message: String) {
    if settle_compaction(app, id, auto) {
        add_compaction_receipt(app, &message);
        // A pass the user asked for keeps its sticky footer error. An
        // automatic pass is the engine's own recovery: the transcript receipt
        // records it, and when the turn then fails its error line is the
        // headline. Echoing the same failure in the footer made one failure
        // read as three (experience mark 2).
        if !auto {
            set_explicit_compaction_status(app, message, StatusToastLevel::Error, true);
        }
    }
}

pub(crate) fn apply_compaction_cancelled(app: &mut App, id: &str, auto: bool, message: String) {
    if settle_compaction(app, id, auto) {
        add_compaction_receipt(app, &message);
        set_explicit_compaction_status(app, message, StatusToastLevel::Info, false);
    }
}

/// Esc/Ctrl+C during a compact that is serving an in-flight turn must stop
/// the turn. Compact-only (manual `/compact` with no model request) still
/// cancels just the pass.
#[must_use]
pub(crate) fn compact_interrupt_should_stop_turn(app: &App) -> bool {
    (app.is_compacting || app.manual_compaction_queued)
        && (app.is_loading || matches!(app.runtime_turn_status.as_deref(), Some("in_progress")))
}

/// Cancel the exact queued or running pass without cancelling an unrelated
/// model turn. A locally deferred request has never entered the engine, so it
/// can settle synchronously with no provider call; all dispatched requests
/// wait for the authoritative typed terminal event.
pub(crate) fn try_cancel_compaction(app: &mut App, engine_handle: &EngineHandle) -> bool {
    if !app.is_compacting && !app.manual_compaction_queued {
        return false;
    }

    if !app.is_compacting && app.deferred_manual_compaction.take().is_some() {
        app.manual_compaction_queued = false;
        app.manual_compaction_id = None;
        let message = "Making room stopped before it started".to_string();
        add_compaction_receipt(app, &message);
        set_explicit_compaction_status(app, message, StatusToastLevel::Info, false);
        return true;
    }

    let id = app
        .active_compaction
        .as_ref()
        .map(|active| active.id.clone())
        .or_else(|| app.manual_compaction_id.clone());
    let Some(id) = id else {
        return false;
    };

    match engine_handle.cancel_compaction(id) {
        Ok(()) => {
            set_explicit_compaction_status(
                app,
                "Stopping making room…".to_string(),
                StatusToastLevel::Info,
                false,
            );
        }
        Err(error) => {
            let message = format!("Could not stop making room: {error}");
            add_compaction_receipt(app, &message);
            set_explicit_compaction_status(app, message, StatusToastLevel::Error, true);
        }
    }
    true
}

#[cfg(test)]
pub(crate) fn maybe_warn_context_pressure(app: &mut App) {
    let config = app.compaction_config();
    maybe_warn_context_pressure_for_config(app, &config);
}

pub(crate) fn maybe_warn_context_pressure_for_config(
    app: &mut App,
    config: &codewhale_engine::compaction::CompactionConfig,
) {
    if config.enabled {
        app.dismiss_context_pressure_warning();
        app.context_pressure_warning_dismissed = None;
        return;
    }
    let max = config
        .effective_context_window
        .unwrap_or_else(|| app.effective_context_window());
    let Some((used, max, percent)) = context_usage_snapshot_for_window(app, max) else {
        return;
    };

    let configured_threshold = app.auto_compact_threshold_percent.clamp(10.0, 100.0);
    let warning_threshold = CONTEXT_SUGGEST_COMPACT_THRESHOLD_PERCENT.min(configured_threshold);
    if percent < warning_threshold {
        app.context_pressure_warning_dismissed = None;
        if app.sticky_status.as_ref().is_some_and(|status| {
            matches!(
                status.kind,
                crate::tui::app::StatusToastKind::ContextPressure(_)
            )
        }) {
            app.clear_sticky_status();
        }
        app.context_pressure_warning_dismissed = None;
        return;
    }
    let pressure_level = if percent >= CONTEXT_CRITICAL_THRESHOLD_PERCENT {
        codewhale_runtime::context_budget::PressureLevel::Critical
    } else if percent >= CONTEXT_WARNING_THRESHOLD_PERCENT {
        codewhale_runtime::context_budget::PressureLevel::High
    } else {
        codewhale_runtime::context_budget::PressureLevel::Medium
    };
    if app
        .context_pressure_warning_dismissed
        .is_some_and(|dismissed| pressure_level <= dismissed)
    {
        return;
    }

    // #5239: the meter drives real budgets off this window, so an unverified
    // one must say so next to the numbers that depend on it.
    let window_note = if app.effective_context_window_source().is_verified() {
        ""
    } else {
        ", unverified window"
    };

    let recommendation = "Making room automatically is off. Turn on auto_compact or use /compact.";

    if percent >= CONTEXT_CRITICAL_THRESHOLD_PERCENT {
        set_context_pressure_status(
            app,
            format!(
                "Context critical: {percent:.0}% ({used}/{max} tokens{window_note}). {recommendation}"
            ),
            pressure_level,
        );
        return;
    }

    let status_prefix = if percent >= CONTEXT_WARNING_THRESHOLD_PERCENT {
        "Context high"
    } else {
        "Context building"
    };
    set_context_pressure_status(
        app,
        format!(
            "{status_prefix}: {percent:.0}% ({used}/{max} tokens{window_note}). {recommendation}"
        ),
        pressure_level,
    );
}

fn set_context_pressure_status(
    app: &mut App,
    text: String,
    pressure_level: codewhale_runtime::context_budget::PressureLevel,
) {
    let can_replace = app.sticky_status.as_ref().is_none_or(|status| {
        matches!(
            status.kind,
            crate::tui::app::StatusToastKind::ContextPressure(_)
        )
    });
    if !can_replace {
        return;
    }
    app.status_message = Some(text.clone());
    app.last_status_message_seen = Some(text.clone());
    // No TTL: this warning stays visible until compaction or explicit Esc
    // dismissal instead of being pushed out by later transcript activity.
    app.sticky_status = Some(crate::tui::app::StatusToast::context_pressure(
        text,
        pressure_level,
    ));
    app.needs_redraw = true;
}

#[cfg(test)]
mod config_update_tests {
    use super::*;
    use codewhale_engine::core::engine::mock_engine_handle;
    use codewhale_engine::core::ops::Op;

    #[tokio::test]
    async fn live_compaction_update_queues_without_waiting_on_engine() {
        let mut mock = mock_engine_handle();
        let compaction = codewhale_engine::compaction::CompactionConfig {
            enabled: false,
            token_threshold: 123,
            model: "deepseek-v4-flash".to_string(),
            effective_context_window: Some(128_000),
            cache_summary: true,
            focus: None,
            runtime_cost_owner: None,
            workspace: None,
            image_input: codewhale_engine::model_profile::SupportState::Unknown,
            summary_instructions: None,
            retained_user_message_tokens:
                codewhale_runtime::config::DEFAULT_COMPACTION_RETAINED_USER_MESSAGE_TOKENS,
        };

        assert!(try_apply_model_and_compaction_update(
            &mock.handle,
            compaction.clone(),
            AppMode::Agent,
            None,
        ));

        assert!(matches!(
            mock.rx_op.recv().await,
            Some(Op::SetModel {
                model,
                mode: AppMode::Agent,
                route_limits: None,
            }) if model == compaction.model
        ));
        assert!(matches!(
            mock.rx_op.recv().await,
            Some(Op::SetCompaction { config }) if config == compaction
        ));
    }

    /// U03-05: with room for only one op, neither half of the route update
    /// is queued; a model without its compaction budget is a half-applied
    /// route with nothing to roll it back.
    #[tokio::test]
    async fn live_compaction_update_is_all_or_nothing_when_one_slot_is_free() {
        let mut mock = mock_engine_handle();
        let mut fillers = 0;
        while mock.handle.tx_op.capacity() > 1 {
            mock.handle
                .try_send(Op::ListSubAgents)
                .expect("filler op fits");
            fillers += 1;
        }
        let compaction = codewhale_engine::compaction::CompactionConfig {
            model: "deepseek-v4-flash".to_string(),
            ..codewhale_engine::compaction::CompactionConfig::default()
        };

        assert!(!try_apply_model_and_compaction_update(
            &mock.handle,
            compaction,
            AppMode::Agent,
            None,
        ));

        for _ in 0..fillers {
            assert!(matches!(mock.rx_op.try_recv(), Ok(Op::ListSubAgents)));
        }
        assert!(
            mock.rx_op.try_recv().is_err(),
            "no half of the route update may be queued"
        );
    }
}

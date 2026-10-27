//! Getting a composed user message into a turn: dispatch, steering, and the
//! offline/queued message paths.
//!
//! Moved verbatim out of `ui.rs`.

use super::*;
use crate::core::ops::TurnSpec;
use codewhale_models::Role;

pub(crate) fn dispatch_hotbar_slot(
    app: &mut App,
    config: &Config,
    slot: u8,
) -> Result<Option<HotbarDispatch>> {
    let known_action_ids = app
        .hotbar_actions
        .iter()
        .map(|action| action.id())
        .collect::<Vec<_>>();
    let bindings = config.resolve_hotbar_bindings(&known_action_ids).bindings;
    let Some(action_id) = bindings
        .iter()
        .find(|binding| binding.slot == slot)
        .map(|binding| binding.action.clone())
    else {
        return Ok(None);
    };

    let Some(action) = app.hotbar_actions.get(&action_id) else {
        app.status_message = Some(format!(
            "Hotbar slot {slot} action is not available: {action_id}"
        ));
        app.needs_redraw = true;
        return Ok(Some(HotbarDispatch::Handled));
    };

    if let Some(reason) = action.disabled_reason(app) {
        app.status_message = Some(format!(
            "Hotbar slot {slot} action is not available: {reason}"
        ));
        app.needs_redraw = true;
        return Ok(Some(HotbarDispatch::Handled));
    }

    action.dispatch(app, config).map(Some)
}

pub(crate) fn queued_ui_to_session(msg: &QueuedMessage) -> Result<QueuedSessionMessage> {
    anyhow::ensure!(
        msg.extension_command.is_none(),
        "Extension command prompts cannot be persisted as user input"
    );
    Ok(QueuedSessionMessage {
        display: msg.display.clone(),
        skill_instruction: msg.skill_instruction.clone(),
        skill_provenance: msg.skill_provenance.clone(),
    })
}

pub(crate) fn queued_session_to_ui(msg: QueuedSessionMessage) -> QueuedMessage {
    QueuedMessage {
        display: msg.display,
        skill_instruction: msg.skill_instruction,
        skill_provenance: msg.skill_provenance,
        extension_command: None,
        // Persistence does not carry this flag; restore may re-echo or rely on
        // pending preview. Live Queue path sets true after painting.
        history_echoed: false,
    }
}

/// Echo a freshly submitted turn into the transcript before it waits on the
/// queue / offline bucket. Ops contract: every submit paints `HistoryCell::User`
/// before the model runs (queued included).
pub(crate) fn echo_queued_user_turn(app: &mut App, message: &mut QueuedMessage) {
    if message.history_echoed {
        return;
    }
    app.add_message(HistoryCell::User {
        content: message.display.clone(),
    });
    message.history_echoed = true;
    app.needs_redraw = true;
    app.scroll_to_bottom();
}

/// Paint command input as an attributed receipt and composer input as a user
/// turn, reusing its queue-time echo. Return the displayed cell's index.
pub(crate) fn paint_user_turn_cell(
    app: &mut App,
    message: &QueuedMessage,
    content: String,
) -> usize {
    if let Some((command, _)) = message.extension_command.as_ref() {
        app.add_message(HistoryCell::System {
            content: format!("{} command input:\n{content}", command.origin),
        });
        return app.history.len().saturating_sub(1);
    }
    if message.history_echoed
        && let Some(idx) = echoed_user_turn_cell(app, &message.display)
    {
        app.history[idx] = HistoryCell::User { content };
        app.bump_history_cell(idx);
        return idx;
    }
    app.add_message(HistoryCell::User { content });
    app.history.len().saturating_sub(1)
}

/// The newest transcript cell that queue-time echo painted for `display`.
pub(crate) fn echoed_user_turn_cell(app: &App, display: &str) -> Option<usize> {
    app.history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, cell)| match cell {
            HistoryCell::User { content } if content == display => Some(idx),
            _ => None,
        })
}

pub(crate) fn enqueue_offline_message(app: &mut App, message: QueuedMessage) {
    app.queue_message(message);
    persist_offline_queue_state(app);
}

pub(crate) fn push_assistant_message(
    app: &mut App,
    text: String,
    thinking: Option<String>,
    tool_uses: PendingToolUses,
) {
    let mut blocks = Vec::new();
    if let Some(thinking) = thinking {
        blocks.push(ContentBlock::Thinking {
            thinking,
            signature: None,
            state: None,
            redacted_data: None,
        });
    }
    if !text.is_empty() {
        blocks.push(ContentBlock::Text {
            text,
            cache_control: None,
        });
    }
    blocks.extend(tool_uses);

    let has_sendable_content = blocks.iter().any(|block| {
        matches!(
            block,
            ContentBlock::Text { .. } | ContentBlock::ToolUse { .. }
        )
    });
    if has_sendable_content {
        app.push_api_message(Message {
            role: Role::Assistant,
            content: blocks,
        });
    }
}

pub(crate) fn replace_matching_assistant_text(
    app: &mut App,
    original_text: &str,
    translated_text: String,
) -> bool {
    for message in app.api_messages_mut().iter_mut().rev() {
        if message.role != "assistant"
            && message.role != codewhale_models::INTERRUPTED_ASSISTANT_ROLE
        {
            continue;
        }
        for block in &mut message.content {
            if let ContentBlock::Text { text, .. } = block
                && text == original_text
            {
                *text = translated_text;
                return true;
            }
        }
    }
    false
}

pub(crate) fn build_queued_message(app: &mut App, input: String) -> QueuedMessage {
    let skill_instruction = app.active_skill.take();
    let skill_provenance = app.active_skill_provenance.take();
    QueuedMessage::new(input, skill_instruction).with_skill_provenance(skill_provenance)
}

pub(crate) fn allowed_tools_for_message(
    configured: Option<Vec<String>>,
    message: &QueuedMessage,
) -> Option<Vec<String>> {
    if message.is_workflow_draft() {
        // `/workflow <objective>` is review-first. The model may draft and ask
        // for confirmation, but the host makes execution impossible in the
        // same turn even if the provider ignores that instruction.
        Some(Vec::new())
    } else {
        configured
    }
}

pub(crate) async fn submit_initial_input_if_ready(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
) -> Result<()> {
    if !app.auto_submit_initial_input {
        return Ok(());
    }

    if app.onboarding != OnboardingState::None || app.redaction_gate {
        if app.status_message.is_none() && !app.input.trim().is_empty() {
            app.status_message = Some(INITIAL_PROMPT_DEFERRED_STATUS.to_string());
        }
        return Ok(());
    }

    app.auto_submit_initial_input = false;
    if let Some(input) = app.submit_input() {
        if app.status_message.as_deref() == Some(INITIAL_PROMPT_DEFERRED_STATUS) {
            app.status_message = None;
        }
        let queued = build_queued_message(app, input);
        dispatch_user_message_with_recovery(
            app,
            config,
            engine_handle,
            queued,
            DispatchRecovery::Initial,
        )
        .await?;
    }
    Ok(())
}

pub(crate) fn message_from_submitted_input(
    app: &mut App,
    input: String,
) -> (QueuedMessage, DispatchRecovery) {
    if let Some(mut draft) = app.queued_draft.take() {
        draft.display = input;
        (draft, DispatchRecovery::Draft)
    } else {
        (
            build_queued_message(app, input),
            DispatchRecovery::Immediate,
        )
    }
}

pub(crate) fn take_next_queued_message(app: &mut App) -> Option<(QueuedMessage, DispatchRecovery)> {
    if app.input.is_empty() {
        return app.remove_queued_message(0).map(|message| {
            (
                message,
                DispatchRecovery::Queued {
                    restore_index: Some(0),
                },
            )
        });
    }
    None
}

pub(crate) async fn send_next_queued_message_now(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
) -> Result<bool> {
    let Some((message, recovery)) = take_next_queued_message(app) else {
        return Ok(false);
    };
    send_taken_queued_message_now(app, config, engine_handle, message, recovery).await?;
    Ok(true)
}

pub(crate) async fn send_queued_message_at_index_now(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    index: usize,
) -> Result<bool> {
    let Some(message) = app.remove_queued_message(index) else {
        app.status_message = Some("Queued message not found".to_string());
        return Ok(true);
    };
    send_taken_queued_message_now(
        app,
        config,
        engine_handle,
        message,
        DispatchRecovery::Queued {
            restore_index: Some(index),
        },
    )
    .await?;
    Ok(true)
}

pub(crate) async fn send_taken_queued_message_now(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    message: QueuedMessage,
    recovery: DispatchRecovery,
) -> Result<()> {
    if app.offline_mode {
        restore_queued_or_draft_message(app, recovery, message);
        app.status_message = Some(
            app.tr(MessageId::ToastOfflineQueuedCount)
                .replace("{count}", &app.queued_message_count().to_string()),
        );
        return Ok(());
    }

    if app.dispatch_in_flight {
        // A spawned dispatch is still resolving route/sending its op (#4605):
        // there is no turn to steer into yet. Re-queue; the completion/turn
        // lifecycle will drive the next drain.
        restore_queued_or_draft_message(app, recovery, message);
        app.status_message = Some(queued_follow_up_toast(app));
        return Ok(());
    }
    if app.is_loading {
        match steer_user_message(app, config, engine_handle, message.clone()).await {
            Ok(true) => app.push_status_toast(
                app.tr(MessageId::ToastSentIntoTurn).into_owned(),
                StatusToastLevel::Info,
                Some(1_500),
            ),
            Ok(false) => {
                restore_queued_or_draft_message(app, recovery, message);
                app.push_status_toast(
                    app.tr(MessageId::ToastHookBlockedFollowUp).into_owned(),
                    StatusToastLevel::Warning,
                    Some(4_000),
                );
            }
            Err(err) => {
                restore_queued_or_draft_message(app, recovery, message);
                app.status_message = Some(format!(
                    "{} ({err})",
                    app.tr(MessageId::ToastCouldNotSendIntoTurn)
                ));
            }
        }
    } else if let Err(_err) =
        dispatch_user_message_with_recovery(app, config, engine_handle, message, recovery).await
    {
        // The completion closure re-queued the message and set the status.
    } else {
        app.status_message = Some(app.tr(MessageId::ToastSentIntoTurn).into_owned());
    }
    Ok(())
}

pub(crate) fn validate_extension_command_context(app: &App, message: &QueuedMessage) -> Result<()> {
    let Some((command, session_id)) = message.extension_command.as_ref() else {
        return Ok(());
    };
    anyhow::ensure!(
        app.current_session_id.as_deref() == Some(session_id.as_str())
            && app.workspace == command.workspace,
        "Extension command belongs to a different session or workspace"
    );
    anyhow::ensure!(
        !app.offline_mode
            && app.agent_focus.is_none()
            && !app.paused
            && app.paused_goal_objective.is_none()
            && !app.remote_control.runtime_chat_blocks_local_dispatch(),
        "Extension command prompts require the active root session online and unpaused"
    );
    crate::extension_host::command::validate_selected_registration(command, session_id)
        .map_err(anyhow::Error::msg)
}

pub(crate) fn queued_message_content_for_app(
    app: &App,
    message: &QueuedMessage,
    cwd: Option<PathBuf>,
    git_cache: &mut crate::tui::git_mention::GitMentionCache,
) -> Result<String> {
    if message.extension_command.is_some() {
        validate_extension_command_context(app, message)?;
        return Ok(message.display.clone());
    }
    if let Some(provenance) = message.skill_provenance.as_ref() {
        provenance
            .verify_for(&app.workspace, Some(app.extension_plugin_view().as_ref()))
            .map_err(anyhow::Error::msg)?;
    }
    // Pass the process CWD explicitly so the resolver's two-pass logic can
    // honor the user's launch directory when it differs from `--workspace`
    // (issue #101 — file mentions silently routing to the wrong root).
    // The completion index is the composer's already-built fuzzy scan: a
    // bounded fallback for exact misses, with no submit-time tree walk (#4365).
    let completion_index = app.composer.mention_discovery.fuzzy_candidates(
        &app.workspace,
        &app.composer.mention_cwd,
        app.mention_walk_depth,
        app.workspace_follow_symlinks,
    );
    // Stabilize macOS screencapture temp references before anything else sees
    // the text: macOS deletes those Temporary Items dirs minutes after capture.
    let stabilization_dir = crate::tui::file_mention::screenshot_stabilization_dir(&app.workspace);
    let display = crate::tui::file_mention::stabilize_screenshot_references(
        &message.display,
        &stabilization_dir,
    );
    let user_request = crate::tui::file_mention::user_request_with_file_mentions_cached(
        &display,
        &app.workspace,
        cwd,
        git_cache,
        completion_index,
    );
    if let Some(skill_instruction) = message.skill_instruction.as_ref() {
        Ok(format!(
            "{skill_instruction}\n\n---\n\nUser request: {user_request}"
        ))
    } else {
        Ok(user_request)
    }
}

pub(crate) fn dispatch_completion_permit(
    app: &App,
) -> std::result::Result<
    tokio::sync::mpsc::OwnedPermit<crate::tui::app::DispatchApplyFn>,
    &'static str,
> {
    let sender = app
        .dispatch_completion_tx
        .clone()
        .ok_or("dispatch completion mailbox is unavailable")?;
    sender.try_reserve_owned().map_err(|error| match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => "dispatch completion mailbox is full",
        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
            "dispatch completion mailbox is closed"
        }
    })
}

#[cfg(test)]
pub(crate) async fn dispatch_user_message(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    message: QueuedMessage,
) -> Result<()> {
    dispatch_user_message_with_recovery(
        app,
        config,
        engine_handle,
        message,
        DispatchRecovery::Immediate,
    )
    .await
}

pub(crate) async fn dispatch_user_message_with_recovery(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    mut message: QueuedMessage,
    recovery: DispatchRecovery,
) -> Result<()> {
    if message.extension_command.is_some() {
        let admitted = validate_extension_command_context(app, &message).and_then(|()| {
            anyhow::ensure!(
                !app.is_loading
                    && !app.dispatch_in_flight
                    && !app.suppress_stream_events_until_turn_complete,
                "Extension command prompts require an idle session"
            );
            Ok(())
        });
        if let Err(error) = admitted {
            app.restore_unsent_message(message);
            app.status_message = Some(error.to_string());
            return Err(error);
        }
    }
    if app.redaction_gate {
        recover_unstarted_external_message(app, message, recovery, INITIAL_PROMPT_DEFERRED_STATUS);
        return Ok(());
    }
    let stop_words = config.stop_words();
    if message.extension_command.is_none() && is_stop_word(&message.display, &stop_words).is_some()
    {
        engine_handle.cancel();
        app.stopped_turn = true;
        app.status_message = Some("Turn stopped. Tool calls blocked for this turn.".to_string());
        return Ok(());
    }
    app.stopped_turn = false;

    // #1364: run mutable `message_submit` hooks before dispatch. Hooks see the
    // user's display text and may replace or block it before file mentions,
    // skill wrapping, history, and model input are resolved.
    // Fast-path skip when no hooks configured.
    if app
        .hooks
        .has_hooks_for_event(codewhale_config::hooks::HookEvent::MessageSubmit)
    {
        let context = app.base_hook_context().with_message(&message.display);
        let strict_gates = app.hooks.matched_strict_gate_labels(
            codewhale_config::hooks::HookEvent::MessageSubmit,
            &context,
        );
        let hooks = app.hooks.clone();
        let original_text = message.display.clone();

        if app.dispatch_completion_tx.is_some() {
            // The foreground transform is a gate, but its child wait belongs
            // on the blocking pool, never on the terminal event loop. Result
            // delivery reserves bounded mailbox capacity before any work or
            // state mutation, so the recovery closure cannot be dropped.
            let completion_permit = match dispatch_completion_permit(app) {
                Ok(permit) => permit,
                Err(error) => {
                    recover_unstarted_external_message(app, message, recovery, error);
                    return Err(anyhow::Error::msg(error));
                }
            };
            app.dispatch_in_flight = true;
            tokio::spawn(async move {
                let outcome = match tokio::task::spawn_blocking(move || {
                    hooks.execute_message_submit_transform_for_dispatch(&context, &original_text)
                })
                .await
                {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        tracing::error!(target: "hooks", %error, "message_submit executor task was lost");
                        lost_message_submit_outcome(&strict_gates)
                    }
                };
                let apply: crate::tui::app::DispatchApplyFn = Box::new(
                    move |app: &mut App,
                          engine_handle: &EngineHandle,
                          config: &Config|
                          -> anyhow::Result<()> {
                        if !apply_message_submit_outcome(app, &mut message, outcome) {
                            app.dispatch_in_flight = false;
                            restore_message_submit_denial(app, message, recovery);
                            return Ok(());
                        }
                        let _ = start_user_dispatch(app, config, engine_handle, message, recovery);
                        Ok(())
                    },
                );
                completion_permit.send(apply);
            });
            return Ok(());
        }

        // Unit tests intentionally omit the event-loop completion channel.
        // Keep those synchronous from the test's perspective while still
        // running the blocking child wait off the async runtime worker.
        let outcome = match tokio::task::spawn_blocking(move || {
            hooks.execute_message_submit_transform_for_dispatch(&context, &original_text)
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::error!(target: "hooks", %error, "message_submit executor task was lost");
                lost_message_submit_outcome(&strict_gates)
            }
        };
        if !apply_message_submit_outcome(app, &mut message, outcome) {
            restore_message_submit_denial(app, message, recovery);
            return Ok(());
        }
    }

    if app.dispatch_completion_tx.is_some() {
        return start_user_dispatch(app, config, engine_handle, message, recovery);
    }

    let prepare = match prepare_user_dispatch(app, config, message.clone()) {
        Ok(prepare) => prepare,
        Err(error) => {
            recover_unstarted_external_message(app, message, recovery, &error.to_string());
            return Err(error);
        }
    };
    run_prepared_dispatch(app, config, engine_handle, prepare, recovery).await
}

pub(crate) fn lost_message_submit_outcome(
    strict_gates: &[String],
) -> crate::hooks::MessageSubmitOutcome {
    if strict_gates.is_empty() {
        crate::hooks::MessageSubmitOutcome::Unchanged {
            warning: Some(
                "message_submit hook executor did not run; submission continued because no strict gate matched"
                    .to_string(),
            ),
        }
    } else {
        crate::hooks::MessageSubmitOutcome::Blocked {
            reason: "message_submit hook executor did not run; a strict gate blocked submission"
                .to_string(),
        }
    }
}

pub(crate) fn prepare_user_dispatch(
    app: &mut App,
    config: &Config,
    message: QueuedMessage,
) -> Result<UserDispatchPrepare> {
    anyhow::ensure!(!app.redaction_gate, "{INITIAL_PROMPT_DEFERRED_STATUS}");
    let _ = app.maybe_nudge_plugin_for_prompt(&message.display);

    // Plan paused-command changes without touching App or the engine pause
    // gate. Route selection can await and client preflight can fail; neither
    // may resume or discard a paused command unless a turn is ready to send.
    let extension_command = message.extension_command.is_some();
    if extension_command {
        validate_extension_command_context(app, &message)?;
        anyhow::ensure!(
            !app.is_loading && !app.suppress_stream_events_until_turn_complete,
            "Extension command prompts require an idle session"
        );
    }
    let paused_dispatch = if extension_command {
        PausedCommandDispatch::None
    } else {
        plan_paused_command_message(app, &message.display)
    };

    let cwd = std::env::current_dir().ok();
    // One cache for this submit: the references pass and the payload pass
    // otherwise each shell out for `@git`/`@diff`, making git compute a large
    // working-tree diff twice to attach it once (#4067 review follow-up).
    let mut git_cache = crate::tui::git_mention::GitMentionCache::default();
    let completion_index = app.composer.mention_discovery.fuzzy_candidates(
        &app.workspace,
        &app.composer.mention_cwd,
        app.mention_walk_depth,
        app.workspace_follow_symlinks,
    );
    let references = if extension_command {
        Vec::new()
    } else {
        crate::tui::file_mention::context_references_from_input_cached(
            &message.display,
            &app.workspace,
            cwd.clone(),
            &mut git_cache,
            completion_index,
        )
    };
    let mut content = queued_message_content_for_app(app, &message, cwd, &mut git_cache)?;
    if let Some(note) = paused_dispatch.note() {
        content.push_str(note);
    }
    let (app_route_identity, route_config) =
        app_scoped_runtime_config(app, config).map_err(anyhow::Error::msg)?;

    let should_auto_resolve = auto_router::should_resolve_auto_model_selection(app);
    let auto_router_context = auto_router::recent_auto_router_context(&app.api_messages);

    // Capture the App state before any optimistic mutation so a failure can
    // roll back cleanly.
    let snapshot = UserDispatchSnapshot {
        is_loading: app.is_loading,
        suppress_stream_events_until_turn_complete: app.suppress_stream_events_until_turn_complete,
        runtime_turn_status: app.runtime_turn_status.clone(),
        receipt_text: app.receipt_text.clone(),
        receipt_started_at: app.receipt_started_at,
        tool_evidence: app.tool_evidence.clone(),
        history_len: app.history.len(),
        history_revisions_len: app.history_revisions.len(),
        api_messages_len: app.api_messages.len(),
        last_send_at: app.last_send_at,
    };

    // --- Sync prepare: show the user message and spinner immediately so the
    // event loop can repaint before network I/O (#4605). The async phase runs
    // the auto-model route, compaction, and engine send off the render thread.
    app.is_loading = true;
    app.runtime_turn_status = None;
    app.clear_receipt();
    app.tool_evidence.clear();
    app.needs_redraw = true;

    let message_index = app.api_messages.len();
    // Already painted at Queue time — reuse that cell for reference
    // recording instead of duplicating the bubble.
    let history_cell = paint_user_turn_cell(app, &message, message.display.clone());
    app.scroll_to_bottom();
    // Anchor the tail-flash to the moment the user message appears, not to
    // the async dispatch completion (which can lag by a route plan). The
    // failure path restores the pre-send timestamp from the snapshot.
    app.last_send_at = Some(Instant::now());
    if !extension_command {
        app.push_api_message(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: content.clone(),
                cache_control: None,
            }],
        });
    }

    let goal_objective = if extension_command {
        app.goal.objective.clone()
    } else {
        paused_dispatch.goal_objective(app)
    };
    let allowed_tools = allowed_tools_for_message(app.active_allowed_tools.clone(), &message);

    Ok(UserDispatchPrepare {
        message,
        content,
        references,
        paused_dispatch,
        app_route_identity,
        route_config,
        goal_objective,
        goal_status: app.goal.status,
        goal_token_budget: app.goal.token_budget,
        mode: app.mode,
        api_provider: app.api_provider,
        app_model: app.model.clone(),
        auto_model: app.auto_model,
        reasoning_effort: app.reasoning_effort,
        allow_shell: app.allow_shell,
        trust_mode: app.trust_mode,
        auto_approve: app_auto_approve_enabled(app),
        approval_mode: app.approval_mode,
        translation_enabled: app.translation_enabled,
        allowed_tools,
        hook_executor: app.runtime_services.hook_executor.clone(),
        verbosity: app.verbosity.clone(),
        provenance: if extension_command {
            UserInputProvenance::Runtime
        } else {
            UserInputProvenance::ExternalUser
        },
        auto_router_context,
        should_auto_resolve,
        auto_compact_user_configured: app.auto_compact_user_configured,
        auto_compact: app.auto_compact,
        auto_compact_threshold_percent: app.auto_compact_threshold_percent,
        snapshot,
        cost_scope: crate::cost_status::scope_token(),
        message_index,
        history_cell,
    })
}

pub(crate) fn start_user_dispatch(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    message: QueuedMessage,
    recovery: DispatchRecovery,
) -> Result<()> {
    let completion_permit = match dispatch_completion_permit(app) {
        Ok(permit) => permit,
        Err(error) => {
            recover_unstarted_external_message(app, message, recovery, error);
            return Err(anyhow::Error::msg(error));
        }
    };
    let recovery_message = message.clone();
    let prepare = match prepare_user_dispatch(app, config, message) {
        Ok(prepare) => prepare,
        Err(error) => {
            recover_unstarted_external_message(app, recovery_message, recovery, &error.to_string());
            return Err(error);
        }
    };
    app.dispatch_in_flight = true;
    // #6800: a local cancel (Esc, stall recovery) trips this token so the
    // dispatch fails back to the composer at once instead of holding
    // `dispatch_in_flight` — and queueing every new send — for the full
    // `DISPATCH_TASK_BOUND` while it waits on engine admission.
    let cancel = tokio_util::sync::CancellationToken::new();
    app.dispatch_cancel = Some(cancel.clone());
    // Supervised: `spawned_dispatch_execute` owns the whole dispatch future,
    // so its completion callback always arrives — on success, on a panic, on
    // a local cancel, or when the dispatch exceeds its bound (#6184).
    tokio::spawn(spawned_dispatch_execute(
        prepare,
        recovery,
        engine_handle.clone(),
        completion_permit,
        cancel,
    ));
    Ok(())
}

/// Longest a dispatch may spend routing and waiting for engine admission
/// before it is failed back to the composer (#6184). An engine whose op
/// mailbox never frees (a wedged turn) used to hold the dispatch — and the
/// user's message — forever.
pub(crate) const DISPATCH_TASK_BOUND: std::time::Duration = std::time::Duration::from_secs(60);

pub(crate) async fn spawned_dispatch_execute(
    prepare: UserDispatchPrepare,
    recovery: DispatchRecovery,
    engine_handle: EngineHandle,
    completion_permit: tokio::sync::mpsc::OwnedPermit<crate::tui::app::DispatchApplyFn>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let apply = supervised_dispatch(
        prepare,
        recovery,
        DISPATCH_TASK_BOUND,
        cancel,
        |prepare, recovery| spawned_dispatch_inner(prepare, recovery, engine_handle),
    )
    .await;
    completion_permit.send(apply);
}

/// Run one dispatch future under supervision. A dropped JoinHandle used to
/// turn a panic or a hang into a dispatch that never reported back: the
/// completion permit was dropped, `dispatch_in_flight` stayed set and the
/// message sat in limbo. Every outcome now yields a callback; a panic or an
/// overrun also leaves a log line and a `crashes/` record. A tripped `cancel`
/// token abandons the dispatch and fails it back through the same error
/// closure, so the unsent message returns to the composer exactly as on any
/// other dispatch failure (#6800).
pub(crate) async fn supervised_dispatch<F, Fut>(
    prepare: UserDispatchPrepare,
    recovery: DispatchRecovery,
    bound: std::time::Duration,
    cancel: tokio_util::sync::CancellationToken,
    run: F,
) -> crate::tui::app::DispatchApplyFn
where
    F: FnOnce(UserDispatchPrepare, DispatchRecovery) -> Fut,
    Fut: std::future::Future<Output = crate::tui::app::DispatchApplyFn>,
{
    use futures_util::FutureExt as _;
    let fallback = prepare.clone();
    let started = std::time::Instant::now();
    let supervised = std::panic::AssertUnwindSafe(run(prepare, recovery)).catch_unwind();
    let outcome = tokio::select! {
        biased;
        () = cancel.cancelled() => {
            return build_dispatch_error_closure(
                fallback,
                recovery,
                "Message dispatch was cancelled before it reached the engine".to_string(),
            );
        }
        outcome = tokio::time::timeout(bound, supervised) => outcome,
    };
    match outcome {
        Ok(Ok(apply)) => apply,
        Ok(Err(panic)) => {
            let detail = codewhale_util::utils::panic_message(&*panic);
            codewhale_util::utils::record_caught_panic("user-dispatch", &detail);
            build_dispatch_error_closure(
                fallback,
                recovery,
                format!("Message dispatch hit an internal error: {detail}"),
            )
        }
        Err(_elapsed) => {
            codewhale_runtime::stall_report::report_stall(
                &codewhale_runtime::stall_report::StallReport {
                    source: "ui",
                    phase: "while dispatching the message (route planning / engine admission)"
                        .to_string(),
                    detail: Some(format!(
                        "{} / {}",
                        fallback
                            .app_route_identity
                            .compatibility()
                            .map_or(fallback.app_route_identity.key.as_str(), |row| row.label),
                        fallback.app_model
                    )),
                    turn_id: None,
                    provider_request: None,
                    since_progress: started.elapsed(),
                    bound: Some(bound),
                },
            );
            build_dispatch_error_closure(
                fallback,
                recovery,
                format!(
                    "Message dispatch stalled for {}s before the engine accepted it; your message was restored. Press Esc to cancel the running turn, then retry.",
                    bound.as_secs()
                ),
            )
        }
    }
}

/// Keep classifier receipts owned until the UI admits the operation to Engine.
/// Dropping a reserved dispatch (including a closed completion mailbox) must
/// settle its already-incurred usage in the original session scope.
pub(super) struct UnacceptedDispatchUsage {
    pub(super) scope: crate::cost_status::CostScopeToken,
    pub(super) batch: Option<crate::cost_status::RuntimeUsageBatch>,
}

impl Drop for UnacceptedDispatchUsage {
    fn drop(&mut self) {
        if let Some(batch) = self.batch.as_ref() {
            crate::cost_status::report_runtime_usage_batch(self.scope, None, batch);
        }
    }
}

pub(crate) async fn spawned_dispatch_inner(
    prepare: UserDispatchPrepare,
    recovery: DispatchRecovery,
    engine_handle: EngineHandle,
) -> crate::tui::app::DispatchApplyFn {
    if let Some((command, session_id)) = prepare.message.extension_command.as_ref()
        && let Err(error) =
            crate::extension_host::command::validate_reference(command, session_id).await
    {
        return build_dispatch_error_closure(prepare, recovery, error);
    }
    if !prepare.auto_model {
        crate::provider_lake::refresh_unverified_local_context(
            &prepare.route_config,
            &prepare.app_route_identity,
            &prepare.app_model,
        )
        .await;
    }
    // Bound in its own statement: the planner borrows `prepare`, and the error
    // arm moves it into the failure closure.
    let plan_result = plan_turn_route(TurnRoutePlanRequest {
        route_config: &prepare.route_config,
        app_route_identity: &prepare.app_route_identity,
        api_provider: prepare.api_provider,
        app_model: &prepare.app_model,
        auto_model: prepare.auto_model,
        reasoning_effort: prepare.reasoning_effort,
        mode: prepare.mode,
        content: &prepare.content,
        auto_router_context: &prepare.auto_router_context,
        should_auto_resolve: prepare.should_auto_resolve,
        allow_auto_router_response_cache: true,
        preflight_required: engine_handle.client_preflight_required(),
        auto_compact_user_configured: prepare.auto_compact_user_configured,
        auto_compact: prepare.auto_compact,
        auto_compact_threshold_percent: prepare.auto_compact_threshold_percent,
    })
    .await;
    let planned = match plan_result {
        Ok(planned) => planned,
        Err(err) => return build_dispatch_error_closure(prepare, recovery, err),
    };

    let PlannedTurnRoute {
        route: turn_route,
        compaction: turn_compaction,
        effective_provider,
        effective_model,
        effective_provider_identity,
        effective_provider_label,
        selected_reasoning_effort,
        effective_reasoning_effort,
        auto_controls_reasoning,
        auto_selection,
        initial_routed_usage,
        routing_source: _,
    } = planned;
    let effective_reasoning_tier = selected_reasoning_effort
        .unwrap_or(prepare.reasoning_effort)
        .normalize_for_route(
            effective_provider,
            &turn_route.candidate.endpoint().base_url,
            &turn_route.model,
        );
    let effective_reasoning_receipt = reasoning_effort_receipt_for_route(
        effective_reasoning_tier,
        effective_provider,
        &turn_route.candidate.endpoint().base_url,
        &turn_route.model,
    );

    let mut usage = UnacceptedDispatchUsage {
        scope: prepare.cost_scope,
        batch: Some(initial_routed_usage.clone()),
    };
    let op = Op::SendMessage(TurnSpec {
        profile_constitution: None,
        max_output_tokens: None,
        content: prepare.content.clone(),
        images: Vec::new(),
        mode: prepare.mode,
        route: Box::new(turn_route),
        compaction: Box::new(turn_compaction.clone()),
        initial_routed_usage: Box::new(initial_routed_usage),
        goal_objective: prepare.goal_objective.clone(),
        goal_token_budget: prepare.goal_token_budget,
        goal_status: prepare.goal_status,
        reasoning_effort: effective_reasoning_effort,
        reasoning_effort_auto: auto_controls_reasoning,
        auto_model: prepare.auto_model,
        allow_shell: prepare.allow_shell,
        trust_mode: prepare.trust_mode,
        auto_approve: prepare.auto_approve,
        approval_mode: prepare.approval_mode,
        translation_enabled: prepare.translation_enabled,
        allowed_tools: prepare.allowed_tools.clone(),
        dynamic_tools: Vec::new(),
        hook_executor: prepare.hook_executor.clone(),
        verbosity: prepare.verbosity.clone(),
        provenance: prepare.provenance,
        // Interactive TUI submissions do not correlate submissions.
        submission_id: None,
    });
    // Reserve capacity off the render thread, but do not let Engine start
    // until the completion callback has installed the UI's acceptance state.
    // Separate completion/event mailboxes otherwise allow TurnStarted (or
    // TurnComplete) to arrive before a callback that resets those newer facts.
    let permit = match engine_handle.tx_op.clone().reserve_owned().await {
        Ok(permit) => permit,
        Err(err) => return build_dispatch_error_closure(prepare, recovery, err.to_string()),
    };
    let outcome = UserDispatchOutcome {
        turn_compaction,
        effective_provider,
        effective_model,
        effective_provider_identity,
        effective_provider_label,
        effective_reasoning_effort: effective_reasoning_receipt,
        auto_selection,
    };
    Box::new(move |app, current_engine, config| {
        // Admission stays serialized by this flag until its callback retires,
        // even if the user replaced the Engine/session while routing waited.
        app.dispatch_in_flight = false;
        // This request has no admitted Op and cannot emit TurnComplete. Retire
        // its local cancellation even after replacement, but leave a previous
        // admitted turn's suppression for that turn's terminal event to retire.
        if !prepare.snapshot.suppress_stream_events_until_turn_complete {
            app.suppress_stream_events_until_turn_complete = false;
        }
        if !engine_handle.tx_op.same_channel(&current_engine.tx_op)
            || prepare.cost_scope != crate::cost_status::scope_token()
        {
            anyhow::bail!("Message dispatch belongs to a previous engine or session");
        }
        if !app.is_loading || engine_handle.tx_op.is_closed() {
            let error = if engine_handle.tx_op.is_closed() {
                "Engine stopped before accepting the message"
            } else {
                "Message dispatch was cancelled before it reached the engine"
            };
            return build_dispatch_error_closure(prepare, recovery, error.to_string())(
                app,
                &engine_handle,
                config,
            );
        }
        if let Err(error) = validate_extension_command_context(app, &prepare.message) {
            return build_dispatch_error_closure(prepare, recovery, error.to_string())(
                app,
                &engine_handle,
                config,
            );
        }
        build_dispatch_success_closure(prepare, outcome)(app, &engine_handle, config)?;
        // Existing Engine admission binds cancellation controls and the Op in
        // one FIFO. No await separates the UI checkpoint from this handoff.
        engine_handle.send_reserved_op(permit, op);
        drop(usage.batch.take());
        Ok(())
    })
}

pub(crate) fn build_dispatch_success_closure(
    prepare: UserDispatchPrepare,
    outcome: UserDispatchOutcome,
) -> crate::tui::app::DispatchApplyFn {
    Box::new(
        move |app: &mut App, engine_handle: &EngineHandle, config: &Config| -> anyhow::Result<()> {
            app.dispatch_in_flight = false;
            prepare.paused_dispatch.apply(app, engine_handle);

            let dispatch_started_at = Instant::now();
            app.is_loading = true;
            app.dispatch_started_at = Some(dispatch_started_at);
            app.runtime_turn_status = None;
            // last_send_at was already anchored in the sync prepare phase so
            // the tail-flash starts together with the visible user cell.
            app.last_submitted_prompt = prepare
                .message
                .extension_command
                .is_none()
                .then(|| prepare.message.display.clone());
            app.unanswered_submission = Some(crate::tui::app::UnansweredSubmission {
                message: prepare.message.clone(),
                history_cell: prepare.history_cell,
            });
            app.clear_receipt();
            app.tool_evidence.clear();

            app.system_prompt = Some(build_app_system_prompt_with_goal(
                app,
                config,
                app.goal.objective.as_deref(),
            ));
            // Composer API messages and input cells were prepared synchronously;
            // command messages arrive from Core. Record accepted references now.
            app.record_context_references(
                prepare.history_cell,
                prepare.message_index,
                prepare.references,
            );
            app.scroll_to_bottom();

            app.last_effective_reasoning_effort = Some(outcome.effective_reasoning_effort);
            if prepare.auto_model {
                app.last_effective_model = Some(outcome.effective_model.clone());
                app.last_effective_provider = Some(outcome.effective_provider);
                app.last_effective_provider_identity =
                    Some(outcome.effective_provider_identity.clone());
                if let Some(selection) = outcome.auto_selection.as_ref() {
                    app.last_auto_route_receipt = selection.receipt.clone();
                    let status = app
                        .tr(MessageId::AutoRouteSelectedToast)
                        .replace("{provider}", &outcome.effective_provider_label)
                        .replace("{model}", &outcome.effective_model)
                        .replace("{source}", selection.source.label());
                    app.push_status_toast(status, StatusToastLevel::Info, Some(6_000));
                }
            } else {
                app.last_effective_model = None;
                app.last_effective_provider = None;
                app.last_effective_provider_identity = None;
                app.last_auto_route_receipt = None;
            }
            app.pending_auto_route_receipt = outcome
                .auto_selection
                .as_ref()
                .and_then(|selection| selection.receipt.clone());
            app.pending_turn_route = Some((
                outcome.effective_provider,
                outcome.effective_model,
                prepare.auto_model,
            ));

            maybe_warn_context_pressure_for_config(app, &outcome.turn_compaction);
            if let Some(message) = crate::plugins::plugin_reload_nudge(
                app.plugin_registry.as_ref(),
                &mut app.plugin_reload_nudge_stamp,
            ) {
                app.push_status_toast(message, StatusToastLevel::Warning, Some(8_000));
            }
            app.session.last_prompt_tokens = None;
            app.session.last_completion_tokens = None;
            app.session.last_prompt_cache_hit_tokens = None;
            app.session.last_prompt_cache_miss_tokens = None;
            app.session.last_reasoning_replay_tokens = None;

            if let Ok(manager) = SessionManager::default_location()
                && let Ok(session) = build_session_snapshot(app, &manager)
            {
                if app.current_session_id.is_none() {
                    app.current_session_id = Some(session.metadata.id.clone());
                }
                if let Err(err) = persist_with_pending_work_boundary(
                    app,
                    PersistRequest::SaveCheckpoint { session },
                ) {
                    app.status_message = Some(format!(
                        "To-do list update pending: turn checkpoint could not be queued ({err})"
                    ));
                }
            }

            Ok(())
        },
    )
}

/// Missing-credential / auth preflight failures. The message never reached a
/// model, so it returns to the composer with a transcript line saying why
/// (`keep_unsent_message_for_connect`) rather than as an echo that has to be
/// typed again — which lost the text and then showed it twice (#6566).
pub(crate) fn is_missing_credential_dispatch_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("api key not found")
        || lower.contains("access token")
        || (lower.contains("credential")
            && (lower.contains("not found")
                || lower.contains("missing")
                || lower.contains("unavailable")
                || lower.contains("unsupported")))
}

pub(crate) fn build_dispatch_error_closure(
    prepare: UserDispatchPrepare,
    recovery: DispatchRecovery,
    error: String,
) -> crate::tui::app::DispatchApplyFn {
    Box::new(
        move |app: &mut App,
              _engine_handle: &EngineHandle,
              _config: &Config|
              -> anyhow::Result<()> {
            app.dispatch_in_flight = false;
            // No operation was admitted, including route/reservation failures:
            // retire only cancellation introduced by this dispatch. A previous
            // admitted turn may still need to suppress its queued events.
            if !prepare.snapshot.suppress_stream_events_until_turn_complete {
                app.suppress_stream_events_until_turn_complete = false;
            }
            if prepare.cost_scope != crate::cost_status::scope_token() {
                anyhow::bail!("Message dispatch belongs to a previous session");
            }
            app.remote_control.fail_active_dispatch(&error);
            // Roll back the optimistic sync prepare mutations.
            app.is_loading = prepare.snapshot.is_loading;
            app.runtime_turn_status = prepare.snapshot.runtime_turn_status.clone();
            app.receipt_text = prepare.snapshot.receipt_text.clone();
            app.receipt_started_at = prepare.snapshot.receipt_started_at;
            app.tool_evidence = prepare.snapshot.tool_evidence.clone();
            let missing_credential = is_missing_credential_dispatch_error(&error);
            // Nothing was sent, so the optimistic echo goes too. A message
            // that is not in the transcript can be sent again without
            // appearing twice (#6566).
            app.history.truncate(prepare.snapshot.history_len);
            app.prune_transcript_index_state(prepare.snapshot.history_len);
            app.history_revisions
                .truncate(prepare.snapshot.history_revisions_len);
            // Never rewind the version: a cache keyed by (version, len) that
            // saw the rolled-back echo would match a different cell that
            // later lands at the same version and length.
            app.history_version = app.history_version.wrapping_add(1);
            app.truncate_api_messages(prepare.snapshot.api_messages_len);
            app.last_send_at = prepare.snapshot.last_send_at;
            app.needs_redraw = true;

            match recovery {
                DispatchRecovery::Immediate => {
                    if missing_credential {
                        keep_unsent_message_for_connect(app, prepare.message, &error);
                    } else {
                        restore_failed_immediate_submit(
                            app,
                            prepare.message,
                            &anyhow::Error::msg(error.clone()),
                        );
                    }
                }
                DispatchRecovery::Queued { restore_index } => {
                    restore_queued_message(app, restore_index, prepare.message);
                    app.status_message = Some(
                        app.tr(MessageId::DispatchFailedQueued)
                            .replace("{error}", &error)
                            .replace("{count}", &app.queued_message_count().to_string()),
                    );
                }
                DispatchRecovery::Draft => {
                    restore_queued_or_draft_message(app, DispatchRecovery::Draft, prepare.message);
                    app.status_message = Some(format!(
                        "Message dispatch failed ({error}); queued draft restored"
                    ));
                }
                DispatchRecovery::Initial => {
                    if missing_credential {
                        keep_unsent_message_for_connect(app, prepare.message, &error);
                    } else {
                        let initial_error = app
                            .tr(MessageId::DispatchFailedInitial)
                            .replace("{error}", &error);
                        restore_failed_immediate_submit(
                            app,
                            prepare.message,
                            &anyhow::Error::msg(initial_error),
                        );
                    }
                }
            }

            Err(anyhow::Error::msg(error))
        },
    )
}

pub(crate) fn parse_queue_send_command(input: &str) -> Option<Result<usize, String>> {
    let rest = strip_queue_command_prefix(input.trim())?;
    let mut parts = rest.split_whitespace();
    let action = parts.next()?;
    if !action.eq_ignore_ascii_case("send") && !action.eq_ignore_ascii_case("now") {
        return None;
    }
    let Some(raw_index) = parts.next() else {
        return Some(Err("Usage: /queue send <n>".to_string()));
    };
    if parts.next().is_some() {
        return Some(Err("Usage: /queue send <n>".to_string()));
    }
    let Ok(index) = raw_index.parse::<usize>() else {
        return Some(Err("Use a positive number".to_string()));
    };
    if index == 0 {
        return Some(Err("Use 1 or more".to_string()));
    }
    Some(Ok(index - 1))
}

pub(crate) fn strip_queue_command_prefix(input: &str) -> Option<&str> {
    for prefix in ["/queue", "/queued"] {
        if let Some(rest) = input.strip_prefix(prefix)
            && (rest.is_empty() || rest.chars().next().is_some_and(char::is_whitespace))
        {
            return Some(rest);
        }
    }
    None
}

pub(crate) async fn steer_user_message(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    mut message: QueuedMessage,
) -> Result<bool> {
    anyhow::ensure!(
        message.extension_command.is_none(),
        "Extension command prompts cannot steer an active turn"
    );
    let stop_words = config.stop_words();
    if is_stop_word(&message.display, &stop_words).is_some() {
        engine_handle.cancel();
        app.stopped_turn = true;
        app.status_message = Some("Turn stopped. Tool calls blocked for this turn.".to_string());
        return Ok(false);
    }
    app.stopped_turn = false;
    // Same-turn steering is an engine-bound external-user path just like a
    // fresh dispatch. Run the mutable gate exactly once on the blocking pool
    // before pause state, history, references, or engine input are touched.
    if app
        .hooks
        .has_hooks_for_event(codewhale_config::hooks::HookEvent::MessageSubmit)
    {
        let context = app.base_hook_context().with_message(&message.display);
        let strict_gates = app.hooks.matched_strict_gate_labels(
            codewhale_config::hooks::HookEvent::MessageSubmit,
            &context,
        );
        let hooks = app.hooks.clone();
        let original_text = message.display.clone();
        let outcome = match tokio::task::spawn_blocking(move || {
            hooks.execute_message_submit_transform_for_dispatch(&context, &original_text)
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::error!(target: "hooks", %error, "steer message_submit executor task was lost");
                lost_message_submit_outcome(&strict_gates)
            }
        };
        if !apply_message_submit_outcome(app, &mut message, outcome) {
            return Ok(false);
        }
    }

    let paused_snapshot = snapshot_steer_paused_state(app);
    let paused_dispatch = plan_paused_command_message(app, &message.display);
    let paused_note = paused_dispatch.note().map(str::to_string);
    paused_dispatch.apply(app, engine_handle);
    let cwd = std::env::current_dir().ok();
    // Same single-submit cache as the other send path — see #4067 follow-up.
    let mut git_cache = crate::tui::git_mention::GitMentionCache::default();
    let completion_index = app.composer.mention_discovery.fuzzy_candidates(
        &app.workspace,
        &app.composer.mention_cwd,
        app.mention_walk_depth,
        app.workspace_follow_symlinks,
    );
    let references = crate::tui::file_mention::context_references_from_input_cached(
        &message.display,
        &app.workspace,
        cwd.clone(),
        &mut git_cache,
        completion_index,
    );
    let mut content = queued_message_content_for_app(app, &message, cwd, &mut git_cache)?;
    if let Some(note) = paused_note.as_deref() {
        content.push_str(note);
    }
    // Send exactly what the engine will store. `turn_loop` commits a steer as
    // `pending.commit().trim()`, so a composer newline or an appended note
    // left the held copy differing from the record by whitespace alone --
    // `accepted_steer_index` then never matched, the steer was never
    // promoted, and the "sending into this turn" card kept showing a message
    // the transcript had already delivered. Trimming here keeps that match an
    // exact comparison, which is the stronger invariant.
    let content = content.trim().to_string();
    let message_index = app.api_messages.len();

    // A shell wait blocks the turn boundary that consumes steer input. Release
    // only this session's live waits; the owned commands keep running (#6909).
    if let Err(err) = request_active_shell_wait_detach(app) {
        restore_steer_paused_state(app, &paused_snapshot);
        engine_handle.set_paused(paused_snapshot.paused);
        return Err(err.context("could not release shell wait before steering"));
    }

    if let Err(err) = engine_handle.steer(content.clone()).await {
        restore_steer_paused_state(app, &paused_snapshot);
        engine_handle.set_paused(paused_snapshot.paused);
        return Err(err);
    }
    app.last_submitted_prompt = Some(message.display.clone());

    // #6190: the steer channel accepting the text is not the turn accepting
    // it. The engine commits a steer at the next step boundary and discards
    // one whose turn has already moved on, so painting a settled cell and
    // pushing `api_messages` here produced two defects at once: the cell sat
    // above the assistant content the record places before it, and a dropped
    // steer left a transcript entry the model never saw. Hold it as in-flight
    // instead — it renders in the "sending into turn" preview until the
    // engine's own `SessionUpdated` shows it, which is also where it learns
    // its real message index.
    app.inflight_steers
        .push_back(crate::tui::app::InflightSteer {
            message,
            content,
            sent_after_index: message_index,
            references,
        });
    app.needs_redraw = true;

    app.status_message = Some("Steering current turn...".to_string());
    Ok(true)
}

/// Promote every in-flight steer the engine's record now contains.
///
/// Called from `apply_engine_session_projection` after the projection lands,
/// so the transcript cell is appended in the position the record gives it:
/// below the assistant work that preceded the steer, as the newest entry.
/// Matching is on the exact text handed to `EngineHandle::steer`, which the
/// engine stores as the accepted user message's first text block, searched
/// from the index the steer was sent after so an identical earlier message
/// cannot claim it.
pub(crate) fn settle_accepted_steers(app: &mut App) {
    if app.inflight_steers.is_empty() {
        return;
    }
    let mut claimed: Vec<usize> = Vec::new();
    let mut unsettled = VecDeque::new();
    for steer in std::mem::take(&mut app.inflight_steers) {
        let Some(index) = accepted_steer_index(app, &steer, &claimed) else {
            unsettled.push_back(steer);
            continue;
        };
        claimed.push(index);
        // Settle the streaming thinking/tool content that chronologically
        // preceded the steer before the steer's own cell is appended.
        app.flush_active_cell();
        let display = format!("+ {}", steer.message.display);
        let history_cell = paint_user_turn_cell(app, &steer.message, display);
        app.record_context_references(history_cell, index, steer.references);
        app.needs_redraw = true;
    }
    app.inflight_steers = unsettled;
}

fn accepted_steer_index(
    app: &App,
    steer: &crate::tui::app::InflightSteer,
    claimed: &[usize],
) -> Option<usize> {
    let start = steer.sent_after_index.min(app.api_messages.len());
    app.api_messages
        .iter()
        .enumerate()
        .skip(start)
        .find(|(index, message)| {
            !claimed.contains(index)
                && message.role == Role::User
                && matches!(
                    message.content.first(),
                    Some(ContentBlock::Text { text, .. }) if text == &steer.content
                )
        })
        .map(|(index, _)| index)
}

/// A turn that ended without accepting a steer must not swallow it (#6190,
/// #6297). The message becomes a queued follow-up — the queue is the one path
/// that actually drains into a turn, where the engine's context-pressure gate
/// sees it like any other send — instead of a display-only "rejected" string
/// that nothing ever dispatches.
pub(crate) fn settle_unaccepted_steers_at_turn_end(app: &mut App) {
    if app.inflight_steers.is_empty() {
        return;
    }
    let deferred = std::mem::take(&mut app.inflight_steers);
    app.queued_messages
        .extend(deferred.into_iter().map(|steer| steer.message));
    app.needs_redraw = true;
}

pub(crate) fn snapshot_steer_paused_state(app: &App) -> SteerPausedSnapshot {
    SteerPausedSnapshot {
        paused: app.paused,
        pausable: app.pausable,
        paused_goal_objective: app.paused_goal_objective.clone(),
        objective: app.goal.objective.clone(),
        tokens_used: app.goal.tokens_used,
        time_used_seconds: app.goal.time_used_seconds,
        continuation_count: app.goal.continuation_count,
    }
}

pub(crate) fn restore_steer_paused_state(app: &mut App, snapshot: &SteerPausedSnapshot) {
    app.paused = snapshot.paused;
    app.pausable = snapshot.pausable;
    app.paused_goal_objective = snapshot.paused_goal_objective.clone();
    app.goal.objective = snapshot.objective.clone();
    app.goal.tokens_used = snapshot.tokens_used;
    app.goal.time_used_seconds = snapshot.time_used_seconds;
    app.goal.continuation_count = snapshot.continuation_count;
}

pub(crate) async fn attempt_steer_with_queue_fallback(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    message: QueuedMessage,
    recovery: DispatchRecovery,
) -> bool {
    match steer_user_message(app, config, engine_handle, message.clone()).await {
        Ok(true) => {
            app.push_status_toast(
                app.tr(MessageId::ToastSentIntoTurn).into_owned(),
                StatusToastLevel::Info,
                Some(1_500),
            );
            true
        }
        Ok(false) => {
            restore_queued_or_draft_message(app, recovery, message);
            app.push_status_toast(
                app.tr(MessageId::ToastHookBlockedFollowUp).into_owned(),
                StatusToastLevel::Warning,
                Some(4_000),
            );
            false
        }
        Err(err) => {
            restore_queued_or_draft_message(app, recovery, message);
            let status = format!("{} ({err})", app.tr(MessageId::ToastCouldNotSendIntoTurn));
            app.status_message = Some(status.clone());
            app.push_status_toast(status, StatusToastLevel::Warning, Some(4_000));
            false
        }
    }
}

/// Park a draft on the queued-messages bucket for dispatch after TurnComplete.
/// Unlike a steer, the message is NOT forwarded immediately — it waits for
/// the current turn to finish, then dispatches as a normal user message.
pub(crate) async fn queue_follow_up(app: &mut App, message: QueuedMessage) -> Result<()> {
    let mut message = message;
    echo_queued_user_turn(app, &mut message);
    enqueue_offline_message(app, message);
    let toast = queued_follow_up_toast(app);
    app.status_message = Some(toast.clone());
    app.push_status_toast(toast, StatusToastLevel::Info, Some(3_000));
    Ok(())
}

fn queued_follow_up_toast(app: &App) -> String {
    if app.offline_mode {
        return app.tr(MessageId::ToastQueuedOffline).into_owned();
    }
    let count = app.queued_message_count();
    if count <= 1 {
        app.tr(MessageId::ToastQueuedFollowUp).into_owned()
    } else {
        app.tr(MessageId::ToastQueuedFollowUpCount)
            .replace("{count}", &count.to_string())
    }
}

pub(crate) async fn dispatch_composer_message(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    message: QueuedMessage,
    recovery: DispatchRecovery,
    action: ComposerSubmitAction,
) -> Result<()> {
    if message.extension_command.is_some() {
        return dispatch_user_message_with_recovery(app, config, engine_handle, message, recovery)
            .await;
    }
    // The ordinary web mirror does not block local prompts. Isolated Runtime
    // Chat is different: it owns a provider-backed native turn that is not
    // reflected by the interactive engine's `is_loading` bit. Preserve the
    // local message in the queue until that exact turn and its terminal relay
    // receipt have settled, so one attached run never has two inference loops.
    if app.remote_control.runtime_chat_blocks_local_dispatch() {
        let mut message = message;
        echo_queued_user_turn(app, &mut message);
        enqueue_offline_message(app, message);
        let queued = app
            .tr(MessageId::AgentRailQueuedCount)
            .replace("{count}", &app.queued_message_count().to_string());
        let status = format!(
            "Runtime Chat · {} · {queued}",
            app.tr(MessageId::PhaseFinishing)
        );
        app.push_status_toast(status, StatusToastLevel::Info, Some(4_000));
        return Ok(());
    }

    // Agent focus: the composer addresses one child's fork, not the main
    // session. The follow-up is real runtime work (Op::FollowUpSubAgent); the
    // main transcript keeps a receipt line and the focused view echoes the
    // message until the child's own transcript carries it.
    if let Some(focus) = app.agent_focus.as_ref() {
        let agent_id = focus.agent_id.clone();
        let label = focus.label.clone();
        let text = message.display.clone();
        crate::tui::agent_focus::echo_user_follow_up(app, &text);
        let receipt = app
            .tr(codewhale_localization::MessageId::AgentFocusFollowUpQueued)
            .replace("{agent}", &label);
        app.push_history_cell(crate::tui::history::HistoryCell::System { content: receipt });
        // #6150: the input path never awaits a full op channel. The follow-up
        // is retryable; a rejected send surfaces immediately.
        if let Err(err) = engine_handle.try_send(crate::core::ops::Op::FollowUpSubAgent {
            agent_id: agent_id.clone(),
            text,
        }) {
            let reason = if err
                .downcast_ref::<tokio::sync::mpsc::error::TrySendError<crate::core::ops::Op>>()
                .is_some_and(|e| matches!(e, tokio::sync::mpsc::error::TrySendError::Full(_)))
            {
                "engine busy"
            } else {
                "engine unavailable"
            };
            let failed = app
                .tr(codewhale_localization::MessageId::AgentFocusFollowUpFailed)
                .replace("{agent}", &label)
                .replace("{reason}", reason);
            app.status_message = Some(failed.clone());
            app.push_status_toast(failed, StatusToastLevel::Warning, Some(5_000));
        }
        return Ok(());
    }
    let disposition = match action {
        _ if is_stop_word(&message.display, &config.stop_words()).is_some() => {
            SubmitDisposition::Immediate
        }
        ComposerSubmitAction::Submit(disposition) => disposition,
        ComposerSubmitAction::SendQueuedNow | ComposerSubmitAction::Noop => {
            // The caller extracted a non-empty input, so these can only arise
            // if state changed between key resolution and dispatch. Queueing
            // is lossless and preserves ordering in that narrow race.
            SubmitDisposition::Queue
        }
    };
    match disposition {
        SubmitDisposition::Immediate => {
            let _ =
                dispatch_user_message_with_recovery(app, config, engine_handle, message, recovery)
                    .await;
            Ok(())
        }
        SubmitDisposition::Queue => {
            let mut message = message;
            echo_queued_user_turn(app, &mut message);
            enqueue_offline_message(app, message);
            // A second, empty Enter inside the window sends this now.
            app.arm_double_tap_window();
            let toast = queued_follow_up_toast(app);
            app.status_message = Some(toast.clone());
            app.push_status_toast(toast, StatusToastLevel::Info, Some(3_000));
            Ok(())
        }
        SubmitDisposition::Steer => {
            attempt_steer_with_queue_fallback(app, config, engine_handle, message, recovery).await;
            Ok(())
        }
        SubmitDisposition::QueueFollowUp => queue_follow_up(app, message).await,
    }
}

#[cfg(test)]
pub(crate) async fn submit_or_steer_message(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    message: QueuedMessage,
    recovery: DispatchRecovery,
) -> Result<()> {
    let action = ComposerSubmitAction::Submit(app.decide_submit_disposition());
    dispatch_composer_message(app, config, engine_handle, message, recovery, action).await
}

/// Drain `app.pending_steers` into a single `QueuedMessage` ready for
/// `dispatch_user_message`. Returns `None` if the queue was empty (caller
/// then falls back to `app.queued_messages`). Skill instruction is taken
/// from the first message that supplies one — multiple steers shouldn't
/// double-up the system framing.
pub(crate) fn merge_pending_steers(app: &mut App) -> Option<QueuedMessage> {
    let drained = app.drain_pending_steers();
    if drained.is_empty() {
        return None;
    }
    if drained.len() == 1 {
        return drained.into_iter().next();
    }
    let mut skill_instruction: Option<String> = None;
    let mut skill_provenance = None;
    let mut bodies: Vec<String> = Vec::with_capacity(drained.len());
    for msg in drained {
        if skill_instruction.is_none() {
            skill_instruction = msg.skill_instruction;
            skill_provenance = msg.skill_provenance;
        }
        bodies.push(msg.display);
    }
    Some(
        QueuedMessage::new(bodies.join("\n\n"), skill_instruction)
            .with_skill_provenance(skill_provenance),
    )
}

//! Remote-control bridge: `/rc` event draining, local-turn attachment, and
//! session start projection, extracted from the composition root
//! (TUI_MODULARIZATION.md slice 3). The controller in `crate::remote_control`
//! owns connection state; this module only projects its events into the UI.

use super::*;

pub(crate) async fn drain_remote_control_events(
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
) -> Result<bool> {
    // A connection can become ready while a local approval card still owns
    // the decision. Keep that card local; once it closes, bind the same
    // already-running typed turn on the next loop tick. If the turn ended in
    // the meantime this remains an ordinary idle attachment.
    let mut changed = try_attach_active_local_turn_to_remote(app);
    while let Some(event) = app.remote_control.try_next_event() {
        changed = true;
        match event {
            crate::remote_control::RemoteEvent::Notice(message) => {
                app.add_message(HistoryCell::System {
                    content: message.clone(),
                });
                app.status_message = Some(message.clone());
                app.sticky_status =
                    Some(StatusToast::new(message, StatusToastLevel::Warning, None));
            }
            crate::remote_control::RemoteEvent::Connected {
                account_ref,
                runner_id,
                attachment,
                links,
                ..
            } => {
                app.remote_control
                    .upload_snapshot(&attachment.run_id, &app.api_messages);
                let active_local_turn = local_turn_is_active(app);
                let attached_active_turn = try_attach_active_local_turn_to_remote(app);
                let status = crate::remote_control::remote_control_banner(
                    &account_ref,
                    &runner_id,
                    links.run_url.as_deref(),
                );
                let mirror_note = if active_local_turn && !attached_active_turn {
                    "A local approval card still owns the current decision; the web joins this turn once it closes."
                } else {
                    "Web mirror connected. Both surfaces can prompt and decide; one turn runs at a time."
                };
                app.add_message(HistoryCell::System {
                    content: format!("{status}\n\n{mirror_note}"),
                });
                if let Some(run_url) = links.run_url.as_deref() {
                    app.add_message(HistoryCell::System {
                        content: crate::remote_control::remote_control_link_notice(run_url),
                    });
                }
                app.status_message = Some(status.clone());
                app.sticky_status = Some(StatusToast::new(status, StatusToastLevel::Warning, None));
            }
            crate::remote_control::RemoteEvent::Attachment { attachment, .. } => {
                // Reconnect responses carry the server's current cursor and
                // snapshot receipt. `try_next_event` applies that truth before
                // this handler, so this is either a no-op or one bounded retry.
                app.remote_control
                    .upload_snapshot(&attachment.run_id, &app.api_messages);
            }
            crate::remote_control::RemoteEvent::RuntimeCursor { .. } => {
                // The controller has already retired the acknowledged prefix.
            }
            crate::remote_control::RemoteEvent::RuntimeChatHostReleased => {
                // Internal provider-config handoff; the controller has already
                // reopened the isolated host and queued its fresh catalog.
            }
            crate::remote_control::RemoteEvent::RuntimeChatProjection(_) => {
                // The controller journaled this isolated native event before
                // exposing it to the UI bridge. It is web-Chat output, not a
                // mutation of the current interactive TUI transcript.
            }
            crate::remote_control::RemoteEvent::FailedPreLease(error) => {
                let status = format!("WEB MIRROR · could not start · {error} · /rc to retry");
                report_remote_control_failure(app, status);
            }
            crate::remote_control::RemoteEvent::Failed(error) => {
                let status = format!(
                    "WEB MIRROR LOST · {error} · this terminal is unaffected; reconnecting waits briefly for the server lease to drain"
                );
                report_remote_control_failure(app, status);
            }
            crate::remote_control::RemoteEvent::Stopped => {
                app.sticky_status = None;
                app.status_message = Some("Web mirror stopped.".to_string());
            }
            crate::remote_control::RemoteEvent::OwnershipRestored { approvals } => {
                app.sticky_status = None;
                app.status_message = Some(
                    "The web mirror lease expired; pending approvals stay actionable here."
                        .to_string(),
                );
                // Mirror semantics: approval cards were never hidden from
                // this terminal, so there is nothing to re-show. The drained
                // list only tells us the web can no longer answer them.
                let _ = approvals;
            }
            crate::remote_control::RemoteEvent::Command {
                run_id,
                seq,
                command,
            } => {
                match app.remote_control.claim_command(&run_id, seq, &command) {
                    Ok(true) => {}
                    Ok(false) => {
                        // The native Runtime operation key makes Chat replay
                        // safe. Re-enter it and reissue the terminal command
                        // acknowledgement: the previous provider submission
                        // may be durable even though its acknowledgement POST
                        // was lost with a worker failure.
                        if let crate::remote_control::RemoteCommand::RuntimeChatPrompt(prompt) =
                            &command
                        {
                            match app.remote_control.apply_runtime_chat_prompt(prompt).await {
                                Ok(()) => app
                                    .remote_control
                                    .acknowledge(&run_id, seq, &command, "applied", None),
                                Err(error) => app.remote_control.acknowledge(
                                    &run_id,
                                    seq,
                                    &command,
                                    "failed",
                                    Some(error),
                                ),
                            }
                        }
                        continue;
                    }
                    Err(error) => {
                        app.remote_control.acknowledge(
                            &run_id,
                            seq,
                            &command,
                            "failed",
                            Some(error.clone()),
                        );
                        app.remote_control.stop();
                        app.sticky_status = None;
                        app.status_message = Some(error);
                        continue;
                    }
                }
                if matches!(
                    &command,
                    crate::remote_control::RemoteCommand::RuntimeChatPrompt(_)
                ) && local_turn_is_active(app)
                {
                    app.remote_control.acknowledge(
                        &run_id,
                        seq,
                        &command,
                        "failed",
                        Some(
                            "Finish or interrupt the active local turn before starting Runtime Chat."
                                .to_string(),
                        ),
                    );
                    continue;
                }
                match command.clone() {
                    crate::remote_control::RemoteCommand::RuntimeChatPrompt(prompt) => {
                        match app.remote_control.apply_runtime_chat_prompt(&prompt).await {
                            Ok(()) => app
                                .remote_control
                                .acknowledge(&run_id, seq, &command, "applied", None),
                            Err(error) => app.remote_control.acknowledge(
                                &run_id,
                                seq,
                                &command,
                                "failed",
                                Some(error),
                            ),
                        }
                    }
                    crate::remote_control::RemoteCommand::Prompt { turn_id, prompt } => {
                        if app.is_loading || app.dispatch_in_flight {
                            app.remote_control.acknowledge(
                                &run_id,
                                seq,
                                &command,
                                "failed",
                                Some(
                                    "A turn is already running; the next prompt starts when it finishes."
                                        .to_string(),
                                ),
                            );
                            continue;
                        }
                        app.remote_control
                            .upload_snapshot(&run_id, &app.api_messages);
                        if let Err(error) = app.remote_control.activate_prompt(&run_id, &turn_id) {
                            app.remote_control.acknowledge(
                                &run_id,
                                seq,
                                &command,
                                "failed",
                                Some(error),
                            );
                            continue;
                        }
                        let message = QueuedMessage::new(prompt, None);
                        app.remote_control.set_applying_remote_command(true);
                        let result = dispatch_user_message_with_recovery(
                            app,
                            config,
                            engine_handle,
                            message,
                            DispatchRecovery::Immediate,
                        )
                        .await;
                        app.remote_control.set_applying_remote_command(false);
                        match result {
                            Ok(()) if app.is_loading || app.dispatch_in_flight => {
                                app.remote_control
                                    .acknowledge(&run_id, seq, &command, "applied", None);
                            }
                            Ok(()) => {
                                app.remote_control.fail_active_dispatch(
                                    "The remote prompt was blocked before dispatch.",
                                );
                                app.remote_control.acknowledge(
                                    &run_id,
                                    seq,
                                    &command,
                                    "failed",
                                    Some(
                                        "The remote prompt was blocked before dispatch."
                                            .to_string(),
                                    ),
                                );
                            }
                            Err(error) => {
                                app.remote_control.fail_active_dispatch(&error.to_string());
                                app.remote_control.acknowledge(
                                    &run_id,
                                    seq,
                                    &command,
                                    "failed",
                                    Some(error.to_string()),
                                );
                            }
                        }
                    }
                    crate::remote_control::RemoteCommand::Approval { gate, approved } => {
                        // Keep the gate pending until the engine takes the
                        // decision: a failed send leaves it retryable (U03-07).
                        let Some(tool_id) = app.remote_control.pending_approval_tool_id(&gate)
                        else {
                            app.remote_control.acknowledge(
                                &run_id,
                                seq,
                                &command,
                                "failed",
                                Some("This approval is no longer pending.".to_string()),
                            );
                            continue;
                        };
                        let result = if approved {
                            engine_handle.approve_tool_call(tool_id.clone()).await
                        } else {
                            engine_handle.deny_tool_call(tool_id.clone()).await
                        };
                        match result {
                            Ok(()) => {
                                note_human_decision_delivered(app, &tool_id);
                                let _ = app.remote_control.take_pending_approval(&gate);
                                app.retire_action_notices(Some(&tool_id));
                                // First decision wins: the web answered this
                                // gate, so dismiss exactly the matching card —
                                // never an unrelated approval that happens to
                                // be on top (concurrent approvals, fleet).
                                // The card may sit under another view
                                // (a child's card, a pager): remove it at
                                // any depth, and forget its pending entry.
                                if app.view_stack.remove_approval_for_gate(&gate) {
                                    app.needs_redraw = true;
                                }
                                crate::tui::pending_requests::resolve(app, &tool_id);
                                let (message_id, level) = if approved {
                                    (
                                        MessageId::NotificationWebApproved,
                                        StatusToastLevel::Success,
                                    )
                                } else {
                                    (MessageId::NotificationWebDenied, StatusToastLevel::Warning)
                                };
                                app.push_status_toast_record(
                                    StatusToast::new(app.tr(message_id), level, Some(5_000))
                                        .for_event(format!("web-decision:{tool_id}")),
                                );
                                app.remote_control
                                    .acknowledge(&run_id, seq, &command, "applied", None);
                            }
                            Err(error) => app.remote_control.acknowledge(
                                &run_id,
                                seq,
                                &command,
                                "failed",
                                Some(error.to_string()),
                            ),
                        }
                    }
                    crate::remote_control::RemoteCommand::Control {
                        runtime_chat: Some(scope),
                        turn_id: Some(turn_id),
                        ..
                    } => {
                        match app
                            .remote_control
                            .interrupt_runtime_chat(&run_id, &scope, &turn_id)
                            .await
                        {
                            Ok(()) => app
                                .remote_control
                                .acknowledge(&run_id, seq, &command, "applied", None),
                            Err(error) => app.remote_control.acknowledge(
                                &run_id,
                                seq,
                                &command,
                                "failed",
                                Some(error),
                            ),
                        }
                    }
                    crate::remote_control::RemoteCommand::Control { turn_id, .. } => {
                        let exact_active_turn = turn_id.as_deref().is_some_and(|turn_id| {
                            app.remote_control.active_turn_matches(&run_id, turn_id)
                        });
                        if !exact_active_turn {
                            app.remote_control.acknowledge(
                                &run_id,
                                seq,
                                &command,
                                "failed",
                                Some("This turn no longer owns active Work.".to_string()),
                            );
                            continue;
                        }
                        engine_handle.cancel();
                        mark_active_turn_cancelled_locally(app);
                        app.remote_control
                            .acknowledge(&run_id, seq, &command, "applied", None);
                    }
                }
            }
        }
    }
    // A Connected event and the local approval decision may be drained in the
    // same UI iteration. Re-check after the event batch so the current turn is
    // attached without waiting for another key or frame.
    changed |= try_attach_active_local_turn_to_remote(app);
    Ok(changed)
}

fn local_turn_is_active(app: &App) -> bool {
    app.is_loading
        || app.dispatch_in_flight
        || matches!(app.runtime_turn_status.as_deref(), Some("in_progress"))
}

/// Attach `/rc` to the current local turn only after the server has supplied
/// a real run id and no pre-attachment approval card still owns the decision.
/// There is no await between the state check and the controller mutation, so a
/// terminal event cannot race this single-threaded ownership transition.
/// Attach `/rc` to the current local turn only after the server has supplied
/// a real run id and no pre-attachment approval card still owns the decision.
/// There is no await between the state check and the controller mutation, so a
/// terminal event cannot race this single-threaded ownership transition.
fn try_attach_active_local_turn_to_remote(app: &mut App) -> bool {
    if app.remote_control.runtime_chat_blocks_local_dispatch() {
        return false;
    }
    if !local_turn_is_active(app) {
        // A dispatch can fail before its typed TurnStarted receipt. In that
        // case there is no turn to hand off and the connected attachment is
        // simply idle, so do not strand a synthetic active lease.
        return app.remote_control.release_unstarted_local_turn();
    }
    if app
        .view_stack
        .contains_kind(crate::tui::views::ModalKind::Approval)
    {
        return false;
    }
    // `runtime_turn_id` intentionally survives the end of a turn for saved
    // receipts. It is authoritative for this handoff only while the matching
    // typed status is still in progress; a new dispatch otherwise parks until
    // its own TurnStarted arrives instead of binding the previous turn id.
    let turn_id = if matches!(app.runtime_turn_status.as_deref(), Some("in_progress")) {
        app.runtime_turn_id.as_deref()
    } else {
        None
    };
    app.remote_control.attach_current_local_turn(turn_id)
}

fn report_remote_control_failure(app: &mut App, error: String) {
    let error = codewhale_config::persistence::redact_secrets(&error);
    tracing::warn!(%error, "remote-control failure");
    app.add_message(HistoryCell::System {
        content: error.clone(),
    });
    app.status_message = None;
    app.set_sticky_status(error, StatusToastLevel::Error, None);
}

fn report_remote_control_start_failure(app: &mut App, error: String) {
    app.remote_control.record_start_failure(&error);
    if app.remote_control.status() == crate::remote_control::Status::Failed {
        report_remote_control_failure(app, error);
    } else {
        app.push_status_toast(
            codewhale_config::persistence::redact_secrets(&error),
            StatusToastLevel::Error,
            Some(12_000),
        );
    }
}

pub(crate) fn start_remote_control_session(app: &mut App, config: &Config) {
    let session_id = app
        .current_session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    app.current_session_id = Some(session_id.clone());
    // The target is the folder, not the session: repeated `/rc` runs in the
    // same folder reuse one enrollment grant instead of minting a new one.
    let target_ref = crate::remote_control::target_ref(&app.workspace);
    let workspace_label = app
        .workspace
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("Codewhale session")
        .to_string();
    let git_remote = codewhale_engine::git_status::observed_git_repo(&app.workspace);
    let runtime_commit = option_env!("CODEWHALE_BUILD_COMMIT")
        .unwrap_or("")
        .to_string();
    // The crash-recoverable delivery journal is mandatory outside tests: it is
    // what lets an interrupted session prove which terminal/approval events
    // never reached the account before handing the session back.
    let journal_dir = match codewhale_config::codewhale_home() {
        Ok(home) => home.join("remote-control"),
        Err(_) => {
            let error =
                "Remote control needs a writable Codewhale home directory for its delivery journal."
                    .to_string();
            report_remote_control_start_failure(app, error);
            return;
        }
    };
    if let Err(error) = app.remote_control.prepare_remote_control_session_journal(
        &journal_dir,
        &target_ref,
        &session_id,
    ) {
        report_remote_control_start_failure(app, error);
        return;
    }
    if let Err(error) = app.remote_control.configure_runtime_chat(
        config.clone(),
        std::sync::Arc::clone(&app.plugin_registry),
        journal_dir.join("runtime-chat"),
        target_ref.clone(),
        session_id.clone(),
    ) {
        report_remote_control_start_failure(app, error);
        return;
    }
    match app
        .remote_control
        .start(crate::remote_control::RemoteStart {
            workspace_label,
            target_ref,
            session_id,
            runtime_version: env!("CARGO_PKG_VERSION").to_string(),
            runtime_commit,
            journal_dir: Some(journal_dir),
            git_remote,
        }) {
        Ok(()) => {
            let status = app.remote_control.status_line();
            app.status_message = Some(status.clone());
            app.sticky_status = Some(StatusToast::new(status, StatusToastLevel::Warning, None));
        }
        Err(error) => {
            report_remote_control_start_failure(app, error);
        }
    }
}

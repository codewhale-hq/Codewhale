//! Observer-hook projection: subagent and turn-end hook payload construction,
//! preview bounding, and completion classification (TUI_MODULARIZATION.md
//! slice 4). The executor lives in `crate::hooks`; this module builds the
//! payloads the UI submits and classifies completion results for display.

use super::*;

pub(super) fn execute_subagent_observer_hook(
    app: &App,
    event: HookEvent,
    agent_id: &str,
    text_field: &str,
    text: &str,
) -> Result<(), String> {
    let (preview, truncated) = bounded_subagent_hook_preview(text);

    // Lifecycle outbox (`[lifecycle_outbox]`): fires even when no shell hook
    // is configured for this event — the outbox is independent of the hook
    // command list. Preview is bounded (preview ceiling) and only ever the
    // preview text, never the raw prompt/result. No-op when disabled.
    match &event {
        HookEvent::SubagentSpawn => {
            app.lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
                event: "subagent_spawn".to_string(),
                kind: "subagent.spawned".to_string(),
                thread_id: app.hooks.session_id().to_string(),
                turn_id: app.runtime_turn_id.clone(),
                item_id: None,
                payload: serde_json::json!({
                    "agent_id": agent_id,
                    "subagent": agent_id,
                    "workspace": app.workspace.display().to_string(),
                    "prompt_preview": codewhale_hooks::bounded_text(
                        &preview,
                        codewhale_hooks::OUTBOX_PREVIEW_MAX_CHARS,
                    ),
                    "prompt_truncated": truncated,
                }),
            });
        }
        HookEvent::SubagentComplete => {
            let status = subagent_completion_status(text).unwrap_or_else(|| "unknown".to_string());
            app.lifecycle_outbox.emit(codewhale_hooks::LifecycleEvent {
                event: "subagent_complete".to_string(),
                kind: "subagent.completed".to_string(),
                thread_id: app.hooks.session_id().to_string(),
                turn_id: app.runtime_turn_id.clone(),
                item_id: None,
                payload: serde_json::json!({
                    "agent_id": agent_id,
                    "subagent": agent_id,
                    "workspace": app.workspace.display().to_string(),
                    "status": status,
                    "result_preview": codewhale_hooks::bounded_text(
                        &preview,
                        codewhale_hooks::OUTBOX_PREVIEW_MAX_CHARS,
                    ),
                    "result_truncated": truncated,
                }),
            });
        }
        _ => {}
    }

    if !app.hooks.has_hooks_for_event(event) {
        return Ok(());
    }

    let context = app.base_hook_context().with_message(&preview);
    let mut payload = serde_json::json!({
        "event": event.as_str(),
        "agent_id": agent_id,
        "session_id": context.session_id.as_deref(),
        "workspace": context.workspace.as_ref().map(|path| path.display().to_string()),
        "mode": context.mode.as_deref(),
        "model": context.model.as_deref(),
        "total_tokens": context.total_tokens,
    });
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            format!("{text_field}_preview"),
            serde_json::Value::String(preview),
        );
        object.insert(
            format!("{text_field}_truncated"),
            serde_json::Value::Bool(truncated),
        );
    }

    if event == HookEvent::SubagentComplete {
        payload["status"] = serde_json::Value::String(
            subagent_completion_status(text).unwrap_or_else(|| "unknown".to_string()),
        );
    }

    app.hooks.submit_json_observer(event, context, payload)
}

pub(super) fn execute_turn_end_observer_hook(
    app: &App,
    turn: Option<&ActiveTurnMetadata>,
    usage: &Usage,
    billing_surface: Option<&str>,
    duration: Duration,
    error: Option<&str>,
) -> Result<(), String> {
    if !app.hooks.has_hooks_for_event(HookEvent::TurnEnd) {
        return Ok(());
    }

    let metadata = turn_end_observer_metadata(turn);
    let context = app.base_hook_context();
    let payload = crate::hooks::turn_end_payload(TurnEndPayloadInput {
        context: &context,
        created_at: metadata.created_at,
        model_backed: metadata.route.is_some(),
        provider: metadata.route.map(|route| route.provider_identity.as_str()),
        billing_surface: metadata.route.and(billing_surface),
        model: metadata.route.map(|route| route.model.as_str()),
        turn_id: metadata.turn_id.as_ref(),
        status: app.runtime_turn_status.as_deref().unwrap_or("unknown"),
        error,
        duration,
        usage,
        totals: TurnEndTotals {
            session_tokens: app.session.total_tokens,
            conversation_tokens: app.session.total_conversation_tokens,
            input_tokens: app.session.total_input_tokens,
            output_tokens: app.session.total_output_tokens,
        },
        tool_count: app.tool_evidence.len(),
        queued_message_count: app.queued_message_count(),
    });
    app.hooks
        .submit_json_observer(HookEvent::TurnEnd, context, payload)
}

pub(super) fn surface_observer_hook_submission_failure(app: &mut App, error: String) {
    app.surface_observer_hook_submission_failure(error);
}

/// Why the agent is waiting on the person, in the payload of
/// [`HookEvent::WaitingForUser`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionWaitReason {
    Approval,
    UserInput,
    GoalContinuation,
}

impl SessionWaitReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::UserInput => "user_input",
            Self::GoalContinuation => "goal_continuation",
        }
    }
}

/// The session's wait reason right now, when one exists.
pub(super) fn session_wait_reason(app: &App) -> Option<SessionWaitReason> {
    if app.view_stack.top_kind() == Some(crate::tui::views::ModalKind::Approval) {
        return Some(SessionWaitReason::Approval);
    }
    if app.pending_user_input_prompt.is_some() {
        return Some(SessionWaitReason::UserInput);
    }
    if app.goal_continuation_waiting {
        return Some(SessionWaitReason::GoalContinuation);
    }
    None
}

/// The hook a turn-state edge fires, if any. Pure so the transition table is
/// directly testable: into `Waiting` is `waiting_for_user`; into `Idle` from
/// work or a wait is `session_idle`; into `InProgress` is `session_busy`.
/// Repeated observations of the same state are silent.
pub(super) fn session_state_transition_event(
    previous: crate::tui::control_socket::TurnState,
    current: crate::tui::control_socket::TurnState,
) -> Option<HookEvent> {
    use crate::tui::control_socket::TurnState;
    match (previous, current) {
        (TurnState::InProgress | TurnState::Waiting, TurnState::Idle) => {
            Some(HookEvent::SessionIdle)
        }
        (TurnState::Idle | TurnState::InProgress, TurnState::Waiting) => {
            Some(HookEvent::WaitingForUser)
        }
        (TurnState::Idle | TurnState::Waiting, TurnState::InProgress) => {
            Some(HookEvent::SessionBusy)
        }
        _ => None,
    }
}

/// Fire the session-state hooks on transitions of the shared
/// [`crate::tui::control_socket::turn_state_from_app`] projection (#6004):
/// `waiting_for_user` when a wait begins, `session_idle` when the session
/// settles back to idle after work or a wait, and `session_busy` when work
/// begins or resumes. The first observed state is
/// recorded without firing so startup never emits a spurious transition.
pub(super) fn execute_session_state_transition_hooks(
    app: &App,
    previous: &mut Option<crate::tui::control_socket::TurnState>,
) {
    use crate::tui::control_socket::turn_state_from_app;
    let current = turn_state_from_app(app);
    let previous = previous.replace(current);
    let Some(previous) = previous else {
        return;
    };
    let Some(event) = session_state_transition_event(previous, current) else {
        return;
    };
    if !app.hooks.has_hooks_for_event(event) {
        return;
    }
    let mut payload = serde_json::json!({
        "from": turn_state_name(previous),
        "to": turn_state_name(current),
    });
    if event == HookEvent::WaitingForUser
        && let Some(reason) = session_wait_reason(app)
    {
        payload["reason"] = serde_json::Value::String(reason.as_str().to_string());
    }
    if event == HookEvent::SessionIdle
        && let Some(status) = app.runtime_turn_status.as_deref()
    {
        payload["last_turn_status"] = serde_json::Value::String(status.to_string());
    }
    if let Err(error) = app
        .hooks
        .submit_json_observer(event, app.base_hook_context(), payload)
    {
        tracing::warn!("session-state hook submission failed: {error}");
    }
}

fn turn_state_name(state: crate::tui::control_socket::TurnState) -> &'static str {
    match state {
        crate::tui::control_socket::TurnState::Idle => "idle",
        crate::tui::control_socket::TurnState::InProgress => "in_progress",
        crate::tui::control_socket::TurnState::Waiting => "waiting",
    }
}

/// Fire `session_error` for a turn whose terminal status is failed (#6004).
/// Transient tool failures the agent absorbs never reach this: only the
/// turn-ending failure fires it, so an alert here means the agent stopped.
pub(super) fn execute_session_error_hook(app: &App, error: Option<&str>) {
    if !app.hooks.has_hooks_for_event(HookEvent::SessionError) {
        return;
    }
    let payload = serde_json::json!({
        "status": "failed",
        "error": error.unwrap_or_default(),
    });
    if let Err(error) =
        app.hooks
            .submit_json_observer(HookEvent::SessionError, app.base_hook_context(), payload)
    {
        tracing::warn!("session-error hook submission failed: {error}");
    }
}

pub(super) struct TurnEndObserverMetadata<'a> {
    pub(super) turn_id: std::borrow::Cow<'a, str>,
    pub(super) created_at: chrono::DateTime<chrono::Utc>,
    pub(super) route: Option<&'a crate::core::events::TurnRoute>,
}

pub(super) fn turn_end_observer_metadata(
    turn: Option<&ActiveTurnMetadata>,
) -> TurnEndObserverMetadata<'_> {
    turn.map_or_else(
        || TurnEndObserverMetadata {
            // Manual compaction, purge, and shell-only completions predate the
            // TurnStarted lifecycle event. Preserve their observer contract
            // with a distinct non-model identity instead of borrowing a stale
            // model turn id.
            turn_id: std::borrow::Cow::Owned(format!("lifecycle_{}", uuid::Uuid::new_v4())),
            created_at: chrono::Utc::now(),
            route: None,
        },
        |turn| TurnEndObserverMetadata {
            turn_id: std::borrow::Cow::Borrowed(&turn.turn_id),
            created_at: turn.created_at,
            route: turn.route.as_ref(),
        },
    )
}

pub(super) fn bounded_subagent_hook_preview(text: &str) -> (String, bool) {
    if text.len() <= SUBAGENT_HOOK_PREVIEW_LIMIT {
        return (text.to_string(), false);
    }
    let safe_end = text
        .char_indices()
        .take_while(|(idx, ch)| idx + ch.len_utf8() <= SUBAGENT_HOOK_PREVIEW_LIMIT)
        .last()
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);
    (format!("{}...[truncated]", &text[..safe_end]), true)
}

pub(super) fn subagent_completion_status(result: &str) -> Option<String> {
    const START: &str = "<codewhale:subagent.done>";
    const END: &str = "</codewhale:subagent.done>";

    if let Some(start) = result.find(START).map(|idx| idx + START.len())
        && let Some(end) = result[start..].find(END).map(|idx| idx + start)
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&result[start..end])
        && let Some(status) = value.get("status").and_then(serde_json::Value::as_str)
    {
        return Some(status.to_string());
    }

    let summary = result.lines().find_map(|line| {
        let trimmed = line.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })?;
    let summary = summary.to_ascii_lowercase();
    if matches!(summary.as_str(), "cancelled" | "canceled")
        || summary.starts_with("cancelled:")
        || summary.starts_with("canceled:")
    {
        Some("cancelled".to_string())
    } else if summary == "failed" || summary.starts_with("failed:") {
        Some("failed".to_string())
    } else if summary == "interrupted" || summary.starts_with("interrupted:") {
        Some("interrupted".to_string())
    } else {
        None
    }
}

pub(super) fn subagent_failure_notice(result: &str) -> Option<String> {
    const START: &str = "<codewhale:subagent.done>";
    const END: &str = "</codewhale:subagent.done>";
    let start = result.find(START)? + START.len();
    let end = result[start..].find(END)? + start;
    let value = serde_json::from_str::<serde_json::Value>(&result[start..end]).ok()?;
    (value.get("event").and_then(serde_json::Value::as_str) == Some("subagent.failed"))
        .then(|| {
            let name = value
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let agent_id = value
                .get("agent_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let class = value
                .get("failure_class")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unavailable");
            let steps = value
                .get("steps")
                .and_then(serde_json::Value::as_u64)
                .map_or_else(|| "?".to_string(), |steps| steps.to_string());
            let elapsed_ms = value
                .get("elapsed_ms")
                .and_then(serde_json::Value::as_u64)
                .map_or_else(|| "?".to_string(), |elapsed| elapsed.to_string());
            let transcript_handle = value
                .get("transcript_handle")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unavailable");
            format!(
                "{name} ({agent_id}) · {class} · {steps} steps · {elapsed_ms} ms · inspect {transcript_handle}"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::control_socket::TurnState;
    use crate::tui::control_socket::turn_state_from_app;

    #[test]
    fn session_state_transition_table_fires_only_on_real_edges() {
        use HookEvent::*;
        use TurnState::{Idle, InProgress, Waiting};
        let states = [Idle, InProgress, Waiting];
        let expected = [
            [None, Some(SessionBusy), Some(WaitingForUser)],
            [Some(SessionIdle), None, Some(WaitingForUser)],
            [Some(SessionIdle), Some(SessionBusy), None],
        ];
        for (row, previous) in states.into_iter().enumerate() {
            for (column, current) in states.into_iter().enumerate() {
                assert_eq!(
                    session_state_transition_event(previous, current),
                    expected[row][column],
                    "{previous:?} -> {current:?}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn session_state_transitions_dispatch_real_ordered_payloads_without_duplicates() {
        use crate::hooks::HookExecutor;
        use codewhale_config::hooks::{Hook, HooksConfig};
        use std::path::{Path, PathBuf};
        use std::time::{Duration, Instant};

        fn paths_with_extension(dir: &Path, extension: &str) -> Vec<PathBuf> {
            std::fs::read_dir(dir)
                .expect("receipt directory")
                .map(|entry| entry.expect("receipt entry").path())
                .filter(|path| path.extension().is_some_and(|ext| ext == extension))
                .collect()
        }

        fn wait_for_paths(
            dir: &Path,
            extension: &str,
            count: usize,
            deadline: Instant,
        ) -> Vec<PathBuf> {
            loop {
                let paths = paths_with_extension(dir, extension);
                assert!(
                    Instant::now() < deadline,
                    "expected {count} .{extension} receipts, got {}",
                    paths.len()
                );
                if paths.len() >= count {
                    return paths;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        struct ReleaseBarriers {
            workspace: PathBuf,
            submitted: usize,
        }
        impl Drop for ReleaseBarriers {
            fn drop(&mut self) {
                if std::fs::write(self.workspace.join("release"), "release").is_err() {
                    return;
                }
                // Keep the release file alive during assertion unwinding too.
                // This wait must not panic, and is bounded beyond the hooks'
                // own ten-second timeout if a child cannot report completion.
                let deadline = Instant::now() + Duration::from_secs(12);
                while Instant::now() < deadline {
                    let released = std::fs::read_dir(&self.workspace)
                        .map(|entries| {
                            entries
                                .filter_map(Result::ok)
                                .filter(|entry| {
                                    entry
                                        .path()
                                        .extension()
                                        .is_some_and(|ext| ext == "released")
                                })
                                .count()
                        })
                        .unwrap_or_default();
                    if released >= self.submitted {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }

        let _env_lock = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().expect("isolated workspace");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", dir.path());
        let mut release = ReleaseBarriers {
            workspace: dir.path().to_path_buf(),
            submitted: 0,
        };
        std::fs::write(
            dir.path().join("receipt.sh"),
            r#"set -eu
if [ "$1" = barrier ]; then
    barrier=$(mktemp ./barrier.XXXXXX)
    mv "$barrier" "$barrier.ready"
    while [ ! -f ./release ]; do sleep 0.01; done
    mv "$barrier.ready" "$barrier.released"
    exit 0
fi
receipt=$(mktemp ./receipt.XXXXXX)
{ printf '%s\n' "$1"; cat; } > "$receipt"
mv "$receipt" "$receipt.done"
"#,
        )
        .expect("receipt command");
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(dir.path()),
            &codewhale_runtime::config::Config::default(),
        );
        app.hooks = HookExecutor::new(
            HooksConfig {
                enabled: true,
                hooks: vec![
                    Hook::new(HookEvent::SessionBusy, "sh ./receipt.sh session_busy"),
                    Hook::new(
                        HookEvent::WaitingForUser,
                        "sh ./receipt.sh waiting_for_user",
                    ),
                    Hook::new(HookEvent::SessionIdle, "sh ./receipt.sh session_idle"),
                    Hook::new(HookEvent::SessionEnd, "sh ./receipt.sh barrier").with_timeout(10),
                ],
                ..HooksConfig::default()
            },
            dir.path().to_path_buf(),
        );

        // Startup is silent in every possible initial state, including a
        // restored busy or waiting session. Repeated observations stay silent.
        for state in [TurnState::Idle, TurnState::InProgress, TurnState::Waiting] {
            app.is_loading = state != TurnState::Idle;
            app.goal_continuation_waiting = state == TurnState::Waiting;
            let mut previous = None;
            execute_session_state_transition_hooks(&app, &mut previous);
            execute_session_state_transition_hooks(&app, &mut previous);
            assert_eq!(previous, Some(state));
        }
        app.is_loading = false;
        app.goal_continuation_waiting = false;
        let mut previous = None;
        execute_session_state_transition_hooks(&app, &mut previous);

        let mut seen = std::collections::HashSet::new();
        let mut receipts = Vec::new();
        for state in [
            TurnState::InProgress,
            TurnState::Waiting,
            TurnState::InProgress,
            TurnState::Idle,
        ] {
            app.is_loading = state != TurnState::Idle;
            app.runtime_turn_status = Some(
                if state == TurnState::Idle {
                    "completed"
                } else {
                    "in_progress"
                }
                .to_string(),
            );
            app.pending_user_input_prompt = (state == TurnState::Waiting).then(|| {
                (
                    "hook-fixture-question".to_string(),
                    crate::tools::user_input::UserInputRequest {
                        questions: Vec::new(),
                    },
                )
            });
            execute_session_state_transition_hooks(&app, &mut previous);
            execute_session_state_transition_hooks(&app, &mut previous);
            assert_eq!(previous, Some(state));
            // Advance only after this command records its payload. Concurrent
            // dispatcher workers do not promise command completion order.
            let paths = wait_for_paths(
                dir.path(),
                "done",
                receipts.len() + 1,
                Instant::now() + Duration::from_secs(2),
            );
            assert_eq!(paths.len(), receipts.len() + 1, "extra transition command");
            let path = paths
                .into_iter()
                .find(|path| !seen.contains(path))
                .expect("new receipt");
            let raw = std::fs::read_to_string(&path).expect("atomic receipt");
            let (event, payload) = raw.split_once('\n').expect("event and JSON stdin");
            receipts.push((
                event.to_string(),
                serde_json::from_str::<serde_json::Value>(payload).expect("JSON stdin"),
            ));
            seen.insert(path);
        }

        // Two foreground barriers park both persistent dispatcher workers.
        // FIFO receipt of jobs then proves all preceding jobs have completed,
        // including any erroneous startup or same-state submission. The total
        // deadline is shorter than either barrier's timeout, so one worker
        // cannot time out and masquerade as both workers becoming ready.
        let deadline = Instant::now() + Duration::from_secs(2);
        for _ in 0..2 {
            app.hooks
                .submit_observer(HookEvent::SessionEnd, app.base_hook_context())
                .expect("barrier submission");
            release.submitted += 1;
        }
        wait_for_paths(dir.path(), "ready", 2, deadline);
        let completed_count = paths_with_extension(dir.path(), "done").len();
        drop(release);
        wait_for_paths(
            dir.path(),
            "released",
            2,
            Instant::now() + Duration::from_secs(2),
        );

        assert_eq!(
            completed_count, 4,
            "startup and same-state calls must be silent"
        );
        assert_eq!(
            receipts,
            vec![
                (
                    "session_busy".to_string(),
                    serde_json::json!({"from": "idle", "to": "in_progress"})
                ),
                (
                    "waiting_for_user".to_string(),
                    serde_json::json!({"from": "in_progress", "to": "waiting", "reason": "user_input"})
                ),
                (
                    "session_busy".to_string(),
                    serde_json::json!({"from": "waiting", "to": "in_progress"})
                ),
                (
                    "session_idle".to_string(),
                    serde_json::json!({"from": "in_progress", "to": "idle", "last_turn_status": "completed"})
                ),
            ]
        );
    }

    fn test_app() -> App {
        let config = codewhale_runtime::config::Config::default();
        App::new(
            crate::tui::test_support::test_tui_options(std::env::current_dir().unwrap()),
            &config,
        )
    }

    #[test]
    fn turn_state_projection_covers_every_wait_on_the_person() {
        let mut app = test_app();
        assert_eq!(turn_state_from_app(&app), TurnState::Idle);

        app.is_loading = true;
        assert_eq!(turn_state_from_app(&app), TurnState::InProgress);
        app.runtime_turn_status = Some("in_progress".to_string());

        app.pending_user_input_prompt = Some((
            "q1".to_string(),
            crate::tools::user_input::UserInputRequest {
                questions: Vec::new(),
            },
        ));
        assert_eq!(turn_state_from_app(&app), TurnState::Waiting);
        assert_eq!(
            session_wait_reason(&app),
            Some(SessionWaitReason::UserInput)
        );
        assert_eq!(
            session_state_transition_event(TurnState::InProgress, turn_state_from_app(&app)),
            Some(codewhale_config::hooks::HookEvent::WaitingForUser)
        );
        app.pending_user_input_prompt = None;
        assert_eq!(turn_state_from_app(&app), TurnState::InProgress);

        app.goal_continuation_waiting = true;
        assert_eq!(turn_state_from_app(&app), TurnState::Waiting);
        assert_eq!(
            session_wait_reason(&app),
            Some(SessionWaitReason::GoalContinuation)
        );
        app.goal_continuation_waiting = false;

        app.view_stack.push(
            crate::tui::approval::ApprovalView::new_with_default_selection(
                crate::tui::approval::ApprovalRequest::new_with_intent(
                    "a1",
                    "exec_shell",
                    "run the tests",
                    &serde_json::json!({"cmd": "cargo test"}),
                    "key",
                    None,
                    &app.workspace,
                ),
                codewhale_localization::Locale::En,
                codewhale_runtime::config::ApprovalDefaultSelection::default(),
            ),
        );
        assert_eq!(turn_state_from_app(&app), TurnState::Waiting);
        assert_eq!(session_wait_reason(&app), Some(SessionWaitReason::Approval));
    }
}

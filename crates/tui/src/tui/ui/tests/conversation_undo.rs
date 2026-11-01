//! Exercise command dispatch, the actual UI apply path, Engine requests and disk.
use super::*;
use codewhale_engine::core::engine::{Engine, EngineConfig};
use codewhale_engine::core::events::TurnOutcomeStatus;
use codewhale_engine::llm_client::mock::{MockLlmClient, canned};
use codewhale_models::{ContentBlock, Message};

fn prompts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| {
            matches!(
                codewhale_models::turn_classify::classify_user_turn_prompt(message),
                codewhale_models::turn_classify::UserTurnPromptKind::Editable
            )
        })
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } if !text.starts_with("<turn_meta>") => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect()
}

async fn settled(app: &mut App, handle: &EngineHandle, answer: &str) {
    let mut events = handle.rx_event.write().await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.recv())
            .await
            .expect("turn deadline")
            .expect("Engine event");
        if let EngineEvent::TurnComplete { status, error, .. } = event {
            assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
            break;
        }
    }
    drop(events);
    let snapshot = handle
        .get_session_snapshot()
        .await
        .expect("settled snapshot");
    app.current_session_id = Some(snapshot.session_id);
    app.set_api_messages(Arc::new(snapshot.messages));
    app.system_prompt = snapshot.system_prompt;
    app.model = snapshot.model;
    app.is_loading = false;
    app.push_history_cell(HistoryCell::Assistant {
        content: answer.into(),
        streaming: false,
    });
}

fn sync_snapshot(snapshot: &codewhale_engine::core::ops::SessionSnapshot) -> Op {
    Op::SyncSession {
        session_id: Some(snapshot.session_id.clone()),
        messages: snapshot.messages.clone(),
        system_prompt: snapshot.system_prompt.clone(),
        system_prompt_override: false,
        model: snapshot.model.clone(),
        workspace: snapshot.workspace.clone(),
        mode: AppMode::parse(&snapshot.mode).unwrap(),
    }
}

async fn flush_persistence(actor: &persistence_actor::PersistActorHandle) {
    let (reply, receive) = tokio::sync::oneshot::channel();
    assert!(actor.try_send(PersistRequest::FlushAndReport { reply }));
    assert!(receive.await.unwrap().failures.is_empty());
}

#[test]
fn undo_retry_apply_path_keeps_engine_request_and_reopened_session_consistent() {
    const PROBE: &str = "CODEWHALE_UNDO_RETRY_APPLY_PROBE";
    if std::env::var_os(PROBE).is_none() {
        // The production actor is process-global. Never replace another
        // test's actor or leave this fixture's closed sender in its process.
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tui::ui::tests::conversation_undo::undo_retry_apply_path_keeps_engine_request_and_reopened_session_consistent",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(PROBE, "1")
            .output()
            .expect("isolated UI lifecycle");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let _environment = crate::test_support::lock_test_env();
    let home = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _user_home = crate::test_support::EnvVarGuard::set("HOME", home.path());
    let _profile = crate::test_support::EnvVarGuard::set("USERPROFILE", home.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut config = Config::default().with_legacy_root(
            Some("mock-credential".into()),
            Some("http://127.0.0.1:1/v1".into()),
        );
        config.set_feature("mcp", false).unwrap();
        config.set_feature("subagents", false).unwrap();
        let mut app = App::new(
            crate::tui::test_support::test_tui_options(workspace.path()),
            &config,
        );
        app.onboarding_needs_api_key = false;
        app.offline_mode = false;
        app.mode = AppMode::Agent;
        let mock = Arc::new(MockLlmClient::new(vec![
            canned::simple_text_turn("kept answer"),
            canned::simple_text_turn("undone answer"),
            canned::simple_text_turn("retry answer one"),
            canned::simple_text_turn("retry answer two"),
            canned::simple_text_turn("answer after reopen"),
            canned::simple_text_turn("edited answer"),
        ]));
        let (engine, mut handle) = Engine::new_with_model_client(
            EngineConfig {
                workspace: workspace.path().to_path_buf(),
                model: app.model.clone(),
                snapshots_enabled: false,
                subagents_enabled: false,
                terminal_chrome_enabled: false,
                ..EngineConfig::default()
            },
            &config,
            mock.clone(),
        );
        let engine_task = tokio::spawn(engine.run());
        let manager = SessionManager::default_location().unwrap();
        let (actor, actor_task) =
            persistence_actor::spawn_persistence_actor(SessionManager::default_location().unwrap());
        persistence_actor::init_actor(actor.clone());
        let tasks = TaskManager::start(
            TaskManagerConfig::from_runtime(&config, workspace.path().into(), None, Some(1)),
            config.clone(),
            app.plugin_registry.clone(),
            "undo-fixture-tasks",
            None,
        )
        .await
        .unwrap();
        let mut backend = ColorCompatBackend::new(
            std::io::stdout(),
            codewhale_palette::ColorDepth::Monochrome,
            codewhale_palette::PaletteMode::Dark,
        );
        backend.set_terminal_size(Size::new(80, 24));
        let mut terminal = Terminal::new(backend).unwrap();

        for (prompt, answer) in [
            ("keep this", "kept answer"),
            ("retry this", "undone answer"),
        ] {
            apply_command_result(
                &mut terminal,
                &mut app,
                &mut handle,
                &tasks,
                &mut config,
                commands::CommandResult {
                    message: None,
                    action: Some(AppAction::SendMessage(prompt.into())),
                    is_error: false,
                },
            )
            .await
            .unwrap();
            settled(&mut app, &handle, answer).await;
        }
        // Seed the Engine's typed compaction format, then consume the real
        // SessionUpdated projection. Current projections strip the legacy
        // system-prompt carrier; history must retain the checkpoint on undo.
        let checkpoint_text = format!("{}\nRetained earlier facts", codewhale_engine::compaction::SUMMARY_HEADER);
        let checkpoint = codewhale_engine::compaction::compaction_checkpoint_message(
            &SystemPrompt::Text(checkpoint_text.clone()),
        );
        let mut compacted = handle.get_session_snapshot().await.unwrap();
        let stable_prompt = compacted.system_prompt.as_ref()
            .map(codewhale_engine::compaction::summary_prompt_text).unwrap_or_default();
        compacted.messages.insert(0, checkpoint.clone());
        compacted.system_prompt = Some(SystemPrompt::Text(format!(
            "{stable_prompt}\n\n<!-- compaction-summary:begin -->\n{checkpoint_text}\n<!-- compaction-summary:end -->"
        )));
        handle.send(sync_snapshot(&compacted)).await.unwrap();
        loop {
            let event = handle.rx_event.write().await.recv().await.unwrap();
            if matches!(&event, EngineEvent::SessionUpdated { messages, .. } if messages.contains(&checkpoint)) {
                assert!(super::super::event_loop::apply_engine_session_projection(&mut app, &config, event));
                break;
            }
        }
        assert!(codewhale_engine::compaction::extract_compaction_summary(app.system_prompt.as_ref()).is_none());
        assert!(app.api_messages.contains(&checkpoint));
        let id = app.current_session_id.clone().unwrap();
        let old = build_session_snapshot(&mut app, &manager).unwrap();
        let path = manager.save_session(&old).unwrap();
        // The stale in-flight recovery record must not revive the undone turn.
        assert!(actor.try_send(PersistRequest::SaveCheckpoint { session: old }));
        flush_persistence(&actor).await;
        assert!(
            serde_json::to_string(
                &manager
                    .load_session_checkpoint(&id)
                    .unwrap()
                    .unwrap()
                    .messages
            )
            .unwrap()
            .contains("undone answer")
        );

        for answer in ["retry answer one", "retry answer two"] {
            let result = commands::execute("/retry", &mut app);
            assert!(matches!(
                result.action,
                Some(AppAction::ConversationUndo { .. })
            ));
            apply_command_result(
                &mut terminal,
                &mut app,
                &mut handle,
                &tasks,
                &mut config,
                result,
            )
            .await
            .unwrap();
            settled(&mut app, &handle, answer).await;
            let requests = mock.captured_requests();
            let last = requests.last().unwrap();
            assert_eq!(prompts(&last.messages), ["keep this", "retry this"]);
            assert!(last.messages.contains(&checkpoint), "retry request lost retained checkpoint");
            assert!(
                !serde_json::to_string(&last.messages)
                    .unwrap()
                    .contains("undone answer")
            );
            let reopened = manager.load_session(&id).unwrap();
            assert!(reopened.messages.contains(&checkpoint), "durable undo lost retained checkpoint");
            assert_eq!(
                prompts(&reopened.messages),
                ["keep this"],
                "rollback is durable before replacement inference"
            );
            // Dispatch starts a fresh recovery checkpoint. This fixture reads
            // TurnComplete directly, so it does not run the UI completion save
            // that would retire that new checkpoint. Inspect its history rather
            // than incorrectly expecting the replacement turn to have none.
            flush_persistence(&actor).await;
            let checkpoint = manager.load_session_checkpoint(&id).unwrap().unwrap();
            assert_eq!(prompts(&checkpoint.messages), ["keep this", "retry this"]);
            let checkpoint_text = serde_json::to_string(&checkpoint.messages).unwrap();
            assert!(!checkpoint_text.contains("undone answer"));
            assert!(!checkpoint_text.contains("retry answer"));
        }
        assert_eq!(mock.captured_requests().len(), 4);

        // Force a real Engine update between successful UI preflight and the
        // rewind operation. The proxy only orders mailbox operations: snapshots,
        // mutation, refusal and provider calls still belong to the real Engine.
        // Same-length history changes must be caught, as must each context field.
        let alternate_workspace = TempDir::new().unwrap();
        for (changed_field, before_preflight) in [
            ("messages", false), ("identity", false), ("model", false),
            ("workspace", false), ("prompt", false), ("mode", false),
            ("prompt", true), ("mode", true),
        ] {
            let original = handle.get_session_snapshot().await.unwrap();
            let mut newer = original.clone();
            match changed_field {
                "messages" => {
                    let ContentBlock::Text { text, .. } =
                        &mut newer.messages.last_mut().unwrap().content[0]
                    else {
                        panic!("fixture answer should be text");
                    };
                    *text = "a newer owned answer".into();
                }
                "identity" => newer.session_id = "newer-conversation".into(),
                "model" => newer.model = "newer-model".into(),
                "workspace" => newer.workspace = alternate_workspace.path().to_path_buf(),
                "prompt" => newer.system_prompt = Some(SystemPrompt::Text("newer instructions".into())),
                "mode" => newer.mode = AppMode::Plan.as_setting().into(),
                _ => unreachable!(),
            }
            let replacement = sync_snapshot(&newer);
            let real_handle = handle.clone();
            let queued = real_handle.clone();
            let (proxy_tx, mut proxy_rx) = tokio::sync::mpsc::channel(8);
            handle.tx_op = proxy_tx;
            let interleaving = tokio::spawn(async move {
                let mut replacement = Some(replacement);
                while let Some(op) = proxy_rx.recv().await {
                    let is_preflight = matches!(&op, Op::GetSessionSnapshot { .. });
                    if is_preflight && before_preflight && let Some(replacement) = replacement.take() {
                        queued.send(replacement).await.unwrap();
                    }
                    queued.send(op).await.unwrap();
                    if is_preflight && let Some(replacement) = replacement.take() {
                        // FIFO puts B after snapshot A, but before the UI can
                        // enqueue its conditional rewind through this proxy.
                        queued.send(replacement).await.unwrap();
                    }
                }
                assert!(replacement.is_none(), "preflight snapshot must have run");
            });
            app.input = "keep this unsent draft".into();
            app.cursor_position = 7;
            let visible = app.api_messages.clone();
            let visible_id = app.current_session_id.clone();
            let visible_model = app.model.clone();
            let visible_workspace = app.workspace.clone();
            let visible_prompt = app.system_prompt.clone();
            let history_len = app.history.len();
            let result = commands::execute("/retry", &mut app);
            apply_command_result(
                &mut terminal,
                &mut app,
                &mut handle,
                &tasks,
                &mut config,
                result,
            )
            .await
            .unwrap();
            handle = real_handle;
            interleaving.await.unwrap();
            let current = handle.get_session_snapshot().await.unwrap();
            let installed = codewhale_engine::core::ops::SessionSnapshot {
                instruction_assembly: None,
                ..newer.clone()
            };
            assert!(current == installed, "queued {changed_field} update must not be overwritten");
            assert!(app.api_messages == visible, "visible history changed on refusal");
            assert!(app.current_session_id == visible_id, "visible identity changed on refusal");
            assert!(app.model == visible_model, "visible model changed on refusal");
            assert!(app.workspace == visible_workspace, "visible workspace changed on refusal");
            assert!(app.system_prompt == visible_prompt, "visible prompt changed on refusal");
            assert_eq!(app.history.len(), history_len);
            assert!(matches!(app.history.last(), Some(HistoryCell::Assistant { content, .. }) if content == "retry answer two"));
            assert_eq!(app.input, "keep this unsent draft");
            assert_eq!(app.cursor_position, 7);
            assert_eq!(mock.captured_requests().len(), 4, "refusal must not dispatch retry inference");
            assert!(app.status_toasts.back().unwrap().text.contains(
                app.tr(MessageId::ConversationChangedBeforeUndo).as_ref()
            ));
            // Restore only the fixture between independent race cases.
            handle.send(sync_snapshot(&original)).await.unwrap();
            assert!(
                handle.get_session_snapshot().await.unwrap()
                    == codewhale_engine::core::ops::SessionSnapshot {
                        instruction_assembly: None,
                        ..original
                    }
            );
        }

        // Runtime Chat ownership and a locally active turn must refuse before
        // even staging a different Engine history or altering the transcript.
        let before = app.api_messages.clone();
        app.remote_control.block_runtime_chat_dispatch_for_tests();
        let result = commands::execute("/retry", &mut app);
        apply_command_result(
            &mut terminal,
            &mut app,
            &mut handle,
            &tasks,
            &mut config,
            result,
        )
        .await
        .unwrap();
        assert_eq!(app.api_messages, before);
        assert_eq!(mock.captured_requests().len(), 4);
        app.remote_control = Default::default();
        app.is_loading = true;
        let result = commands::execute("/retry", &mut app);
        apply_command_result(
            &mut terminal,
            &mut app,
            &mut handle,
            &tasks,
            &mut config,
            result,
        )
        .await
        .unwrap();
        assert_eq!(app.api_messages, before);
        app.is_loading = false;

        let result = commands::execute("/undo", &mut app);
        apply_command_result(
            &mut terminal,
            &mut app,
            &mut handle,
            &tasks,
            &mut config,
            result,
        )
        .await
        .unwrap();
        let engine_saved = handle.get_session_snapshot().await.unwrap();
        let reopened = SessionManager::default_location()
            .unwrap()
            .load_session(&id)
            .unwrap();
        assert_eq!(prompts(&engine_saved.messages), ["keep this"]);
        assert_eq!(prompts(&app.api_messages), ["keep this"]);
        assert_eq!(prompts(&reopened.messages), ["keep this"]);
        assert!(
            manager.load_session_checkpoint(&id).unwrap().is_none(),
            "standalone undo retires the checkpoint without starting new turn work"
        );
        assert!(
            !serde_json::to_string(&reopened.messages)
                .unwrap()
                .contains("retry answer")
        );
        assert_eq!(
            mock.captured_requests().len(),
            4,
            "undo performs no inference"
        );

        // Resume the durable record through the existing UI projection and
        // SyncSession apply action, then inspect the actual next model request.
        // File LoadSession always respawns a real client, so use the same
        // loaded-session projection with our injected model client here.
        apply_loaded_session(&mut app, &mut config, &reopened).unwrap();
        let restore = AppAction::SyncSession {
            session_id: app.current_session_id.clone(),
            messages: app.api_messages.as_ref().clone(),
            system_prompt: app.system_prompt.clone(),
            model: app.model.clone(),
            workspace: app.workspace.clone(),
            mode: app.mode,
        };
        for action in [restore, AppAction::SendMessage("after reopen".into())] {
            apply_command_result(
                &mut terminal,
                &mut app,
                &mut handle,
                &tasks,
                &mut config,
                commands::CommandResult {
                    message: None,
                    action: Some(action),
                    is_error: false,
                },
            )
            .await
            .unwrap();
        }
        settled(&mut app, &handle, "answer after reopen").await;
        let requests = mock.captured_requests();
        assert_eq!(requests.len(), 5);
        assert!(requests.last().unwrap().messages.contains(&checkpoint), "reopened provider request lost checkpoint");
        assert_eq!(
            prompts(&requests.last().unwrap().messages),
            ["keep this", "after reopen"]
        );
        let inbound = serde_json::to_string(&requests.last().unwrap().messages).unwrap();
        assert!(!inbound.contains("retry this"));
        assert!(!inbound.contains("undone answer"));
        assert!(!inbound.contains("retry answer"));

        // `/edit` then submit replaces the last exchange: the old prompt and
        // its answer leave the transcript, the provider request and the saved
        // session, and the edited prompt takes their place.
        let loaded = commands::execute("/edit", &mut app);
        assert!(!loaded.is_error, "{:?}", loaded.message);
        assert_eq!(app.input, "after reopen");
        assert!(app.edit_in_progress);
        let result = super::super::event_loop::edit_replacement_result(&mut app, "edited prompt")
            .expect("a pending edit stages a replacement");
        assert!(!app.edit_in_progress);
        apply_command_result(
            &mut terminal,
            &mut app,
            &mut handle,
            &tasks,
            &mut config,
            result,
        )
        .await
        .unwrap();
        settled(&mut app, &handle, "edited answer").await;
        let requests = mock.captured_requests();
        assert_eq!(requests.len(), 6);
        assert_eq!(
            prompts(&requests.last().unwrap().messages),
            ["keep this", "edited prompt"]
        );
        let inbound = serde_json::to_string(&requests.last().unwrap().messages).unwrap();
        assert!(!inbound.contains("after reopen"));
        assert_eq!(prompts(&app.api_messages), ["keep this", "edited prompt"]);
        assert_eq!(
            prompts(&manager.load_session(&id).unwrap().messages),
            ["keep this"],
            "the edit rollback is durable before the replacement turn"
        );
        // With no edit pending, a submit is an ordinary turn.
        assert!(
            super::super::event_loop::edit_replacement_result(&mut app, "plain").is_none()
        );

        // Return to the retained exchange before probing durable-save failure.
        let result = commands::execute("/undo", &mut app);
        apply_command_result(
            &mut terminal,
            &mut app,
            &mut handle,
            &tasks,
            &mut config,
            result,
        )
        .await
        .unwrap();
        assert_eq!(prompts(&app.api_messages), ["keep this"]);

        // Portable save failure: block the canonical file with a directory.
        // Preserve the last durable snapshot as a sibling for inspection.
        let before_failure = handle.get_session_snapshot().await.unwrap();
        std::fs::rename(&path, path.with_extension("before-failure")).unwrap();
        std::fs::create_dir(&path).unwrap();
        app.input = "unrelated draft".into();
        app.cursor_position = app.input.chars().count();
        let result = commands::execute("/retry", &mut app);
        apply_command_result(
            &mut terminal,
            &mut app,
            &mut handle,
            &tasks,
            &mut config,
            result,
        )
        .await
        .unwrap();
        assert_eq!(app.input, "unrelated draft");
        assert!(app.queued_messages.is_empty());
        assert!(app.history.iter().any(|cell| matches!(cell, HistoryCell::System { content } if content == "Retry prompt was not sent. Copy it from here to retry:\nkeep this")));
        assert_eq!(mock.captured_requests().len(), 6);
        handle.send(sync_snapshot(&before_failure)).await.unwrap();
        assert_eq!(handle.get_session_snapshot().await.unwrap().messages, before_failure.messages);
        app.set_api_messages(Arc::new(before_failure.messages));
        app.push_history_cell(HistoryCell::User { content: "keep this".into() });
        let loaded = commands::execute("/edit", &mut app);
        assert!(!loaded.is_error, "{:?}", loaded.message);
        let result = super::super::event_loop::edit_replacement_result(&mut app, "replacement after failed save")
            .expect("a pending edit stages a replacement");
        apply_command_result(
            &mut terminal,
            &mut app,
            &mut handle,
            &tasks,
            &mut config,
            result,
        )
        .await
        .unwrap();
        assert_eq!(
            mock.captured_requests().len(),
            6,
            "save failure must forbid retry inference"
        );
        assert!(
            app.status_toasts
                .back()
                .unwrap()
                .text
                .contains("session save failed")
        );
        assert!(
            app.api_messages.as_slice() == [checkpoint.clone()],
            "acknowledged undo remains visible after save failure"
        );
        assert_eq!(app.input, "replacement after failed save");
        assert!(!app.edit_in_progress);
        assert!(super::super::event_loop::edit_replacement_result(&mut app, "replacement after failed save").is_none());
        assert!(app.status_toasts.back().unwrap().text.contains("Conversation was rewound"));
        assert!(
            handle
                .get_session_snapshot()
                .await
                .unwrap()
                .messages == [checkpoint]
        );

        handle.send(Op::Shutdown).await.unwrap();
        engine_task.await.unwrap();
        app.set_api_messages(Arc::new(reopened.messages));
        app.push_history_cell(HistoryCell::User {
            content: "keep this".into(),
        });
        let result = commands::execute("/retry", &mut app);
        let before = app.api_messages.clone();
        apply_command_result(
            &mut terminal,
            &mut app,
            &mut handle,
            &tasks,
            &mut config,
            result,
        )
        .await
        .unwrap();
        assert_eq!(
            app.api_messages, before,
            "closed Engine must leave the visible conversation alone"
        );
        assert!(
            app.status_toasts
                .back()
                .unwrap()
                .text
                .contains("retry was not sent")
        );
        assert_eq!(mock.captured_requests().len(), 6);
        tasks.shutdown_and_wait().await.unwrap();
        assert!(actor.try_send(PersistRequest::Shutdown));
        actor_task.await.unwrap();
    });
}

/// A refused `/edit` rollback has already consumed the revision from the
/// composer. It must come back, with edit mode re-armed, not be lost.
#[tokio::test]
async fn refused_edit_rollback_returns_the_revision_to_the_composer() {
    use codewhale_engine::task_manager::{
        TaskExecutionResult, TaskManagerConfig, TaskStatus, TaskTerminalReason,
    };
    struct Idle;
    #[async_trait::async_trait]
    impl codewhale_engine::task_manager::TaskExecutor for Idle {
        async fn execute(
            &self,
            _: codewhale_engine::task_manager::ExecutionTask,
            _: tokio::sync::mpsc::Sender<codewhale_engine::task_manager::TaskExecutionEvent>,
            _: tokio_util::sync::CancellationToken,
        ) -> TaskExecutionResult {
            TaskExecutionResult {
                status: TaskStatus::Completed,
                result_text: None,
                error: None,
                terminal_reason: TaskTerminalReason::Completed,
            }
        }
    }
    let root = TempDir::new().unwrap();
    let tasks = TaskManager::start_with_executor(
        TaskManagerConfig {
            data_dir: root.path().into(),
            worker_count: 1,
            default_workspace: root.path().into(),
            default_model: "fixture".into(),
            default_mode: "plan".into(),
            allow_shell: false,
            trust_mode: false,
            execution_limits: codewhale_engine::task_manager::TaskExecutionLimits::default(),
        },
        Arc::new(Idle),
    )
    .await
    .unwrap();
    let mut backend = ColorCompatBackend::new(
        std::io::stdout(),
        codewhale_palette::ColorDepth::Monochrome,
        codewhale_palette::PaletteMode::Dark,
    );
    backend.set_terminal_size(Size::new(80, 24));
    let mut terminal = Terminal::new(backend).unwrap();
    let mut config = Config::default();
    let mut engine = mock_engine_handle();

    let mut app = create_test_app();
    app.push_history_cell(HistoryCell::User {
        content: "original prompt".into(),
    });
    let loaded = commands::execute("/edit", &mut app);
    assert!(!loaded.is_error, "{:?}", loaded.message);
    assert!(app.edit_in_progress);
    // The user revises the prompt and presses Enter: the composer is consumed
    // and the replacement is staged behind a rollback.
    app.input = "revised prompt".into();
    let submitted = std::mem::take(&mut app.input);
    let result = super::super::event_loop::edit_replacement_result(&mut app, &submitted)
        .expect("a pending edit stages a replacement");
    assert!(!app.edit_in_progress);
    assert!(app.input.is_empty());
    // Force the refusal: the rollback is only allowed on an idle app.
    app.is_loading = true;

    apply_command_result(
        &mut terminal,
        &mut app,
        &mut engine.handle,
        &tasks,
        &mut config,
        result,
    )
    .await
    .unwrap();

    assert_eq!(app.input, "revised prompt", "the revision is restored");
    assert_eq!(app.cursor_position, "revised prompt".chars().count());
    assert!(app.edit_in_progress, "edit mode is active again");
    assert!(
        app.history.iter().any(
            |cell| matches!(cell, HistoryCell::User { content } if content == "original prompt")
        ),
        "the refused rollback leaves the original exchange alone"
    );
    assert!(
        app.status_toasts
            .iter()
            .any(|toast| toast.text.contains("rollback failed")),
        "the refusal is still reported"
    );
    assert!(
        engine.rx_op.try_recv().is_err(),
        "nothing reached the engine"
    );
    tasks.shutdown_and_wait().await.unwrap();
}

use super::*;
use codewhale_engine::conversation_modes::{CREATOR_NOT_READY_REASON, CreatorAvailabilityGuard};
use codewhale_engine::core::engine::Engine;
use codewhale_engine::core::ops::Op;

fn creator_app() -> App {
    let mut app = create_test_app();
    app.install_conversation_mode(Some("creator".to_string()), None);
    app
}

fn first_message(app: &mut App) {
    app.push_api_message(text_message("user", "first message"));
}

fn sent_presets(rx: &mut tokio::sync::mpsc::Receiver<Op>) -> Vec<Option<String>> {
    let mut presets = Vec::new();
    while let Ok(op) = rx.try_recv() {
        if let Op::SyncSession { agent_preset, .. } = op {
            presets.push(agent_preset);
        }
    }
    presets
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn mode_change_after_the_first_message_is_refused_as_locked() {
    let _available = CreatorAvailabilityGuard::set(None);
    let mut app = create_test_app();
    let mut engine = mock_engine_handle();
    let config = Config::default();

    let done =
        crate::tui::conversation_mode::set_in_place(&mut app, &config, &engine.handle, "creator")
            .await
            .expect("a blank conversation changes in place");
    assert!(done.contains("Creator mode"), "{done}");
    assert_eq!(app.conversation_mode.as_deref(), Some("creator"));
    assert_eq!(
        sent_presets(&mut engine.rx_op),
        vec![Some("creator".to_string())]
    );

    first_message(&mut app);
    for requested in ["standard", "creator"] {
        let result = crate::tui::conversation_mode::set_in_place(
            &mut app,
            &config,
            &engine.handle,
            requested,
        )
        .await;
        if requested == "creator" {
            assert!(result.is_ok(), "naming the current mode is not a change");
        } else {
            let refusal = result.expect_err("locked after the first message");
            assert!(refusal.contains("fixed"), "{refusal}");
            assert!(refusal.contains("/new"), "{refusal}");
        }
    }
    assert_eq!(app.conversation_mode.as_deref(), Some("creator"));
    assert!(
        sent_presets(&mut engine.rx_op).is_empty(),
        "a refusal sends nothing"
    );

    let result = commands::execute("/mode standard", &mut app);
    assert!(matches!(
        result.action,
        Some(AppAction::SetConversationMode { ref id }) if id == "standard"
    ));
    let mut blank = create_test_app();
    blank
        .queued_messages
        .push_back(crate::tui::app::QueuedMessage::new(
            "queued".to_string(),
            None,
        ));
    assert!(
        blank.conversation_mode_locked(),
        "a queued message locks the mode"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn unknown_and_unavailable_modes_are_refused_with_their_words() {
    let mut app = create_test_app();
    let engine = mock_engine_handle();
    let config = Config::default();
    {
        let _available = CreatorAvailabilityGuard::set(None);
        let refusal = crate::tui::conversation_mode::set_in_place(
            &mut app,
            &config,
            &engine.handle,
            "minimal",
        )
        .await
        .expect_err("unknown id");
        assert!(
            refusal.contains("minimal") && refusal.contains("standard, creator"),
            "{refusal}"
        );
    }
    let _missing = CreatorAvailabilityGuard::set(Some(CREATOR_NOT_READY_REASON));
    let refusal =
        crate::tui::conversation_mode::set_in_place(&mut app, &config, &engine.handle, "creator")
            .await
            .expect_err("unavailable");
    assert!(
        refusal.contains("Not ready yet in this version."),
        "{refusal}"
    );
    assert_eq!(app.conversation_mode, None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_saved_unavailable_mode_loads_its_history_and_says_why() {
    let _guard = CreatorAvailabilityGuard::set(Some(CREATOR_NOT_READY_REASON));
    let hostile = [
        "creator".to_string(),
        "nonsense".to_string(),
        "../x".to_string(),
        String::new(),
        "x".repeat(1_000_000),
        "line\nbreak\u{1b}[31m".to_string(),
    ];
    for stored in hostile {
        let mut app = create_test_app();
        let mut session = saved_session_with_messages(vec![text_message("user", "history line")]);
        session.agent_preset = Some(stored.clone());
        apply_loaded_session(&mut app, &mut Config::default(), &session)
            .expect("the history loads");
        assert_eq!(app.api_messages.len(), 1, "history is shown");
        assert_eq!(
            app.conversation_mode.as_deref(),
            Some(stored.as_str()),
            "never silently Standard"
        );
        assert!(app.conversation_mode_unavailable.is_some());
        let label = app.conversation_mode_label().expect("a label");
        assert!(label.contains("unavailable"), "{label}");
        assert!(label.chars().count() <= 80 && !label.contains('\n') && !label.contains('\u{1b}'));
        assert!(app.history.iter().any(|cell| matches!(
            cell,
            HistoryCell::System { content } if content.contains("which is not available here")
        )));
    }
    let mut app = create_test_app();
    let mut session = saved_session_with_messages(vec![text_message("user", "history line")]);
    session.agent_preset = Some("creator".to_string());
    apply_loaded_session(&mut app, &mut Config::default(), &session).expect("loads");
    assert!(app.history.iter().any(|cell| matches!(
        cell,
        HistoryCell::System { content } if content.contains("Not ready yet in this version.")
    )));

    let mut standard = create_test_app();
    let session = saved_session_with_messages(vec![text_message("user", "history line")]);
    assert_eq!(session.agent_preset, None);
    apply_loaded_session(&mut standard, &mut Config::default(), &session).expect("loads");
    assert_eq!(standard.conversation_mode, None);
    assert_eq!(standard.conversation_mode_label(), None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn clear_fork_and_resume_keep_the_conversation_mode() {
    let _available = CreatorAvailabilityGuard::set(None);
    let _home = crate::test_support::SealedHome::new();

    let mut app = creator_app();
    first_message(&mut app);
    let cleared = commands::execute("/clear", &mut app);
    assert!(matches!(
        cleared.action,
        Some(AppAction::SyncSession { agent_preset: Some(ref preset), .. }) if preset == "creator"
    ));
    assert_eq!(app.conversation_mode.as_deref(), Some("creator"));

    let mut resumed = create_test_app();
    let mut session = saved_session_with_messages(vec![text_message("user", "older")]);
    session.agent_preset = Some("creator".to_string());
    apply_loaded_session(&mut resumed, &mut Config::default(), &session).expect("resume");
    assert_eq!(resumed.conversation_mode.as_deref(), Some("creator"));
    assert_eq!(resumed.conversation_mode_unavailable, None);
    assert_eq!(resumed.agent_preset().as_deref(), Some("creator"));

    let manager = SessionManager::default_location().unwrap();
    let mut snapshot = build_session_snapshot(&mut resumed, &manager).expect("snapshot");
    assert_eq!(snapshot.agent_preset.as_deref(), Some("creator"));
    snapshot.make_storage_compatible();
    let raw = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(raw["agent_preset"], "creator");

    let mut plain = create_test_app();
    let snapshot = build_session_snapshot(&mut plain, &manager).expect("snapshot");
    let raw = serde_json::to_value(&snapshot).unwrap();
    assert!(raw.get("agent_preset").is_none(), "Standard stores no key");

    let mut parent = creator_app();
    first_message(&mut parent);
    parent.current_session_id = Some(uuid::Uuid::new_v4().to_string());
    parent.capture_session_fixture_source();
    let forked = commands::execute("/fork", &mut parent);
    assert!(!forked.is_error, "{:?}", forked.message);
    assert!(matches!(
        forked.action,
        Some(AppAction::SyncSession { agent_preset: Some(ref preset), .. }) if preset == "creator"
    ));
    let fork_id = parent.current_session_id.clone().unwrap();
    let on_disk = manager.load_session(&fork_id).expect("fork record");
    assert_eq!(on_disk.agent_preset.as_deref(), Some("creator"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_resync_of_the_open_conversation_keeps_its_mode() {
    let _available = CreatorAvailabilityGuard::set(None);
    let _home = crate::test_support::SealedHome::new();
    let workspace = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
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
    app.install_conversation_mode(Some("creator".to_string()), None);
    first_message(&mut app);
    app.set_current_session_id(Some(uuid::Uuid::new_v4().to_string()));

    let (engine, mut handle) = Engine::new_with_model_client(
        codewhale_engine::core::engine::EngineConfig {
            workspace: workspace.path().to_path_buf(),
            model: app.model.clone(),
            snapshots_enabled: false,
            subagents_enabled: false,
            terminal_chrome_enabled: false,
            ..Default::default()
        },
        &config,
        Arc::new(codewhale_engine::llm_client::mock::MockLlmClient::new(
            Vec::new(),
        )),
    );
    let run = tokio::spawn(engine.run());
    let tasks = {
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
        TaskManager::start_with_executor(
            TaskManagerConfig {
                data_dir: workspace.path().into(),
                worker_count: 1,
                default_workspace: workspace.path().into(),
                default_model: "fixture".into(),
                default_mode: "plan".into(),
                allow_shell: false,
                trust_mode: false,
                execution_limits: codewhale_engine::task_manager::TaskExecutionLimits::default(),
                human_waits_answerable: false,
            },
            Arc::new(Idle),
        )
        .await
        .unwrap()
    };
    let mut backend = ColorCompatBackend::new(
        std::io::stdout(),
        codewhale_palette::ColorDepth::Monochrome,
        codewhale_palette::PaletteMode::Dark,
    );
    backend.set_terminal_size(ratatui::layout::Size::new(80, 24));
    let mut terminal = Terminal::new(backend).unwrap();

    for action in [
        AppAction::SwitchWorkspace {
            workspace: other.path().to_path_buf(),
        },
        AppAction::PluginRegistryChanged,
    ] {
        let result = commands::CommandResult {
            message: None,
            action: Some(action),
            is_error: false,
        };
        tokio::time::timeout(
            Duration::from_secs(60),
            apply_command_result(
                &mut terminal,
                &mut app,
                &mut handle,
                &tasks,
                &mut config,
                result,
            ),
        )
        .await
        .expect("the re-sync finished")
        .unwrap();
        let snapshot = tokio::time::timeout(Duration::from_secs(20), handle.get_session_snapshot())
            .await
            .expect("the Engine answered")
            .expect("snapshot");
        assert_eq!(snapshot.agent_preset.as_deref(), Some("creator"));
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(app.conversation_mode.as_deref(), Some("creator"));
    }
    let _ = handle.send(Op::Shutdown).await;
    run.abort();
}

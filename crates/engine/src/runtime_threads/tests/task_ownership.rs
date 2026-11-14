use super::*;
use crate::task_manager::{TaskManager, TaskManagerConfig};

fn fixture_config() -> Config {
    let mut config = Config::default().with_legacy_root(
        Some("local-fixture-key".into()),
        Some("http://127.0.0.1:1/v1".into()),
    );
    config.set_feature("mcp", false).unwrap();
    config.set_feature("subagents", false).unwrap();
    config
}

#[tokio::test]
async fn idle_runtime_engine_cycles_are_drained_before_same_scope_reopen() -> Result<()> {
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let cfg = fixture_config();
    let runtime_config = test_manager_config(root.path().join("runtime"));
    let runtime = Arc::new(RuntimeThreadManager::open(
        cfg.clone(),
        root.path().into(),
        runtime_config.clone(),
    )?);
    let tasks = TaskManager::start_with_runtime_manager(
        TaskManagerConfig::from_runtime(&cfg, root.path().into(), None, Some(1)),
        cfg.clone(),
        runtime.clone(),
    )
    .await?;
    let thread = runtime
        .create_thread(CreateThreadRequest::default())
        .await?;
    // Load the real production Engine with its actual strong Runtime services.
    // No turn is submitted, so constructing this Engine makes no provider call.
    let engine = runtime.get_engine(&thread.id).await?;
    engine.get_session_snapshot().await?;
    runtime.spawn_goal_continuation(thread.id.clone(), 3_600);
    let weak = Arc::downgrade(&tasks);
    tasks.shutdown_and_wait().await?;
    assert!(runtime.active.lock().await.engines.is_empty());
    assert!(runtime.get_engine(&thread.id).await.is_err());
    assert!(
        RuntimeThreadManager::open(cfg.clone(), root.path().into(), runtime_config.clone())
            .is_err(),
        "retained Runtime handle still owns its scope"
    );
    drop(engine);
    drop(tasks);
    drop(runtime);
    assert!(
        weak.upgrade().is_none(),
        "idle Engine service cycle must have ended"
    );
    let reopened = RuntimeThreadManager::open(cfg, root.path().into(), runtime_config)?;
    reopened.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_waits_actual_engine_exit_and_terminal_monitor_before_releasing_scope()
-> Result<()> {
    use crate::core::engine::Engine;
    use crate::llm_client::mock::{MockLlmClient, canned};
    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let cfg = fixture_config();
    let manager_cfg = test_manager_config(root.path().join("runtime"));
    let runtime = Arc::new(RuntimeThreadManager::open(
        cfg.clone(),
        root.path().into(),
        manager_cfg.clone(),
    )?);
    let tasks = TaskManager::start_with_runtime_manager(
        TaskManagerConfig::from_runtime(&cfg, root.path().into(), None, Some(1)),
        cfg.clone(),
        runtime.clone(),
    )
    .await?;
    let thread = runtime
        .create_thread(CreateThreadRequest::default())
        .await?;
    let model = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "owned fixture",
    )]));
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            workspace: root.path().into(),
            session_id: Some(thread.id.clone()),
            model: thread.model.clone(),
            subagents_enabled: false,
            snapshots_enabled: false,
            memory_enabled: false,
            terminal_chrome_enabled: false,
            runtime_services: crate::tools::spec::RuntimeToolServices {
                task_manager: Some(tasks.clone()),
                active_thread_id: Some(thread.id.clone()),
                dynamic_tool_executor: Some(Arc::new(runtime.as_ref().clone())),
                ..Default::default()
            },
            ..EngineConfig::default()
        },
        &cfg,
        model.clone(),
    );
    runtime
        .install_test_engine(&thread.id, handle.clone())
        .await?;
    let (release, blocked) = oneshot::channel();
    let worker = tokio::spawn(async move {
        blocked.await.unwrap();
        engine.run().await;
    });
    runtime.engine_workers.lock().push((
        thread.id.clone(),
        handle.clone(),
        retained_completion(worker),
    ));
    let turn = runtime
        .start_turn(
            &thread.id,
            StartTurnRequest {
                prompt: "fixture admission".into(),
                ..Default::default()
            },
        )
        .await?;
    let drain = tokio::spawn({
        let tasks = tasks.clone();
        async move { tasks.shutdown_and_wait().await }
    });
    tokio::time::timeout(Duration::from_secs(5), runtime.cancel_token.cancelled()).await?;
    assert!(
        !drain.is_finished(),
        "a cancellation request cannot prove Engine exit"
    );
    assert!(
        RuntimeThreadManager::open(cfg.clone(), root.path().into(), manager_cfg.clone()).is_err()
    );
    assert!(
        runtime
            .start_turn(
                &thread.id,
                StartTurnRequest {
                    prompt: "late admission".into(),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    let completion = runtime.engine_workers.lock()[0].2.clone();
    tokio::time::timeout(Duration::from_secs(5), async {
        while completion.try_lock().is_ok() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    drain.abort();
    assert!(drain.await.unwrap_err().is_cancelled());
    let retry = tokio::spawn({
        let tasks = tasks.clone();
        async move { tasks.shutdown_and_wait().await }
    });
    sleep(Duration::from_millis(25)).await;
    assert!(
        !retry.is_finished(),
        "retry must still own and await the original Engine join"
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(15), retry).await???;
    drop(completion);
    assert!(runtime.store.load_turn(&turn.id)?.status != RuntimeTurnStatus::InProgress);
    drop(handle);
    drop(tasks);
    drop(runtime);
    let reopened = RuntimeThreadManager::open(cfg, root.path().into(), manager_cfg)?;
    reopened.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn shutdown_drains_accepted_user_input_receipt_after_caller_disconnects() -> Result<()> {
    let root = tempfile::tempdir()?;
    let runtime = Arc::new(test_manager(root.path().join("runtime"))?);
    let thread = runtime
        .create_thread(CreateThreadRequest::default())
        .await?;
    let mut harness = mock_engine_handle();
    runtime
        .install_test_engine(&thread.id, harness.handle.clone())
        .await?;
    runtime.register_pending_user_input(
        &thread.id,
        PendingUserInputRequest {
            id: "input_drain".into(),
            turn_id: "turn_drain".into(),
            request: crate::tools::user_input::UserInputRequest {
                require_typed_answers: false,
                questions: Vec::new(),
            },
        },
    );
    let hold_receipt = runtime.event_emit.lock().await;
    let submission = tokio::spawn({
        let runtime = runtime.clone();
        let thread_id = thread.id.clone();
        async move {
            runtime
                .submit_user_input(
                    &thread_id,
                    "input_drain",
                    crate::tools::user_input::UserInputResponse {
                        answers: Vec::new(),
                    },
                )
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.turn_monitors.lock().is_empty() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    submission.abort();
    assert!(submission.await.unwrap_err().is_cancelled());
    let drain = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown_and_wait().await }
    });
    tokio::time::timeout(Duration::from_secs(5), runtime.cancel_token.cancelled()).await?;
    sleep(Duration::from_millis(25)).await;
    assert!(
        !drain.is_finished(),
        "accepted detached receipt is still part of shutdown"
    );
    drop(hold_receipt);
    let (drained, consumed) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(5), drain),
        harness.recv_user_input_submission(),
    );
    drained???;
    assert!(
        consumed.is_some(),
        "shutdown must include Engine acceptance"
    );
    let events = runtime.events_since(&thread.id, None)?;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event == "user_input.answered")
            .count(),
        1
    );
    assert!(runtime.pending_user_inputs.lock().is_empty());
    Ok(())
}

#[tokio::test]
async fn shutdown_fences_recovery_readers_without_publishing_late_receipts() -> Result<()> {
    let root = tempfile::tempdir()?;
    let runtime = Arc::new(test_manager(root.path().join("runtime"))?);
    let thread = runtime
        .create_thread(CreateThreadRequest::default())
        .await?;
    let turn = sample_turn(&thread.id, "turn_recovery_drain", RuntimeTurnStatus::Failed);
    runtime.store.save_turn(&turn)?;
    runtime.queue_recovery_receipt(RecoveredTurnReceipt {
        turn,
        unresolved_dynamic_tools: Vec::new(),
    });
    let hold = runtime.recovery_flush.lock().await;
    let reader = tokio::spawn({
        let runtime = runtime.clone();
        let id = thread.id.clone();
        async move { runtime.get_thread(&id).await }
    });
    tokio::task::yield_now().await;
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown_and_wait().await }
    });
    tokio::time::timeout(Duration::from_secs(5), runtime.cancel_token.cancelled()).await?;
    assert!(
        !shutdown.is_finished(),
        "shutdown must fence pending recovery producers"
    );
    drop(hold);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), reader)
            .await??
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(5), shutdown).await???;
    assert!(
        runtime.recovery_receipts.lock().contains_key(&thread.id),
        "unadmitted recovery remains queued for the next owner"
    );
    assert!(
        runtime
            .events_since(&thread.id, None)?
            .iter()
            .all(|event| event.event != "turn.completed")
    );
    assert!(runtime.get_thread_detail(&thread.id).await.is_err());
    Ok(())
}

#[tokio::test]
async fn shutdown_drains_an_already_started_recovery_flush() -> Result<()> {
    let root = tempfile::tempdir()?;
    let runtime = Arc::new(test_manager(root.path().join("runtime"))?);
    let thread = runtime
        .create_thread(CreateThreadRequest::default())
        .await?;
    let turn = sample_turn(
        &thread.id,
        "turn_started_recovery",
        RuntimeTurnStatus::Failed,
    );
    runtime.store.save_turn(&turn)?;
    runtime.queue_recovery_receipt(RecoveredTurnReceipt {
        turn,
        unresolved_dynamic_tools: Vec::new(),
    });
    let hold = runtime.event_emit.lock().await;
    let reader = tokio::spawn({
        let runtime = runtime.clone();
        let id = thread.id.clone();
        async move { runtime.get_thread(&id).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.recovery_flush.try_lock().is_ok() {
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await?;
    let shutdown = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.shutdown_and_wait().await }
    });
    tokio::time::timeout(Duration::from_secs(5), runtime.cancel_token.cancelled()).await?;
    sleep(Duration::from_millis(25)).await;
    assert!(
        !shutdown.is_finished(),
        "accepted recovery write must settle before drain succeeds"
    );
    drop(hold);
    tokio::time::timeout(Duration::from_secs(5), reader).await???;
    tokio::time::timeout(Duration::from_secs(5), shutdown).await???;
    assert!(!runtime.recovery_receipts.lock().contains_key(&thread.id));
    assert_eq!(
        runtime
            .events_since(&thread.id, None)?
            .iter()
            .filter(|event| event.event == "turn.completed")
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn runtime_turn_origin_table_and_unattended_auto_review_tasks() -> Result<()> {
    use crate::automation_manager::{AutomationManager, fire_due_triggers_shared};
    use crate::core::authority::auto_review::RunOrigin;
    use crate::core::engine::{EngineHostProfile, TurnNarrowing};
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_app_server_protocol::runtime::watch_event::{
        WatchEventReceipt, WatchEventSource, WatchEventSourceKind, WatchScheduleSource,
    };

    let _env = crate::test_support::lock_test_env();
    let root = tempfile::tempdir()?;
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let cfg = fixture_config();
    let workspace = root.path().to_path_buf();
    let plugins = Arc::new(codewhale_plugins::PluginRegistry::empty(&workspace));

    let owner_mgr = RuntimeThreadManager::open_owner_host(
        cfg.clone(),
        workspace.clone(),
        test_manager_config(root.path().join("rt-owner")),
        plugins.clone(),
        None,
        None,
    )?;
    let bg_mgr = RuntimeThreadManager::open(
        cfg.clone(),
        workspace.clone(),
        test_manager_config(root.path().join("rt-bg")),
    )?;
    let headless_mgr = RuntimeThreadManager::open_host(
        cfg.clone(),
        workspace.clone(),
        test_manager_config(root.path().join("rt-headless")),
        plugins.clone(),
        EngineHostProfile::Headless,
        None,
        None,
    )?;
    let acp_mgr = RuntimeThreadManager::open_host(
        cfg.clone(),
        workspace.clone(),
        test_manager_config(root.path().join("rt-acp")),
        plugins,
        EngineHostProfile::Acp,
        None,
        None,
    )?;

    let scheduled = RuntimeTurnInputSource::Scheduled {
        task_id: "task-1".to_string(),
    };
    let watch = RuntimeTurnInputSource::WatchEvent(Box::new(WatchEventReceipt {
        version: 1,
        platform_account_id: "acct-1".to_string(),
        project_id: "proj-1".to_string(),
        watch_id: "watch-1".to_string(),
        watch_revision: 1,
        definition_sha256: "0".repeat(64),
        source_kind: WatchEventSourceKind::Schedule,
        occurrence_sha256: "1".repeat(64),
        event_sha256: "2".repeat(64),
        run_id: "run-1".to_string(),
        room_thread_root_id: "thr-1".to_string(),
        source: WatchEventSource::Schedule(WatchScheduleSource {
            slot: "2026-11-13T00:00:00Z".to_string(),
        }),
    }));
    let ext_cmd = RuntimeTurnInputSource::ExtensionCommand {
        reference: crate::extension_host::command::ExtensionCommandRef {
            handle: 1,
            plugin_id: "ext-a".to_string(),
            generation: 1,
            origin: "extension:ext-a".to_string(),
            workspace: workspace.clone(),
            selection: None,
            scope: None,
            content_hash: "0".repeat(64),
        },
        operation_fingerprint: "fp-1".to_string(),
        cancel: CancellationToken::new(),
    };
    let mail = RuntimeTurnInputSource::AgentMail {
        message_id: "msg-1".to_string(),
        persisted_summary: "mail".to_string(),
    };
    let goal = RuntimeTurnInputSource::GoalContinuation {
        continuation_index: 1,
        expected_goal: Box::new(codewhale_protocol::ThreadGoal {
            thread_id: "thr-1".to_string(),
            goal_id: "goal-1".to_string(),
            objective: "finish".to_string(),
            status: codewhale_protocol::ThreadGoalStatus::Active,
            token_budget: None,
            tokens_used: 0,
            time_used_seconds: 0,
            continuation_count: 0,
            created_at: 0,
            updated_at: 0,
            last_gap_fingerprint: None,
            repeated_gap_count: 0,
            last_gap_pass: None,
            pause_reason: None,
        }),
    };
    let sources = [
        (
            &RuntimeTurnInputSource::ExternalUser,
            RunOrigin::Interactive,
            true,
        ),
        (&scheduled, RunOrigin::Background, false),
        (&watch, RunOrigin::Background, false),
        (&ext_cmd, RunOrigin::Background, false),
        (&mail, RunOrigin::Background, false),
        (&goal, RunOrigin::Background, false),
        (
            &RuntimeTurnInputSource::AsyncCompletionWake,
            RunOrigin::Background,
            false,
        ),
    ];

    for (source, owner_origin, owner_flag) in sources {
        assert_eq!(
            owner_mgr.mint_turn_origin_for_source(source, TurnNarrowing::Inherit),
            (owner_origin, owner_flag)
        );
        assert_eq!(
            owner_mgr.mint_turn_origin_for_source(source, TurnNarrowing::Acp),
            (RunOrigin::Headless, false)
        );
        assert_eq!(
            bg_mgr.mint_turn_origin_for_source(source, TurnNarrowing::Inherit),
            (RunOrigin::Background, false)
        );
        assert_eq!(
            headless_mgr.mint_turn_origin_for_source(source, TurnNarrowing::Inherit),
            (RunOrigin::Headless, false)
        );
        assert_eq!(
            acp_mgr.mint_turn_origin_for_source(source, TurnNarrowing::Inherit),
            (RunOrigin::Headless, false)
        );
    }

    bg_mgr.shutdown_and_wait().await?;
    headless_mgr.shutdown_and_wait().await?;
    acp_mgr.shutdown_and_wait().await?;

    let owner_arc = Arc::new(owner_mgr);
    let outside_dir = tempfile::tempdir()?;
    let outside_victim = outside_dir.path().join("unattended-target");
    std::fs::write(&outside_victim, "keep\n")?;
    let destructive_args = serde_json::json!({
        "action": "run",
        "command": format!("rm -rf \"{}\"", outside_victim.display()),
    })
    .to_string();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call_destruct_task", "Bash", &destructive_args),
        canned::simple_text_turn("task done"),
        canned::tool_call_turn("call_destruct_watch", "Bash", &destructive_args),
        canned::simple_text_turn("watch done"),
        canned::tool_call_turn("call_destruct_trigger", "Bash", &destructive_args),
        canned::simple_text_turn("trigger done"),
    ]));
    owner_arc.set_test_model_client(mock.clone());

    let task_thread = owner_arc
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            permission_posture: Some("auto-review".to_string()),
            ..Default::default()
        })
        .await?;
    let task_turn = owner_arc
        .start_task_turn(
            &task_thread.id,
            StartTurnRequest {
                prompt: "run scheduled destructive check".to_string(),
                ..Default::default()
            },
            "task-unattended-1",
        )
        .await?;
    let settled_task = wait_for_terminal_turn(owner_arc.as_ref(), &task_turn.id).await?;
    assert_eq!(
        settled_task.status,
        RuntimeTurnStatus::Completed,
        "task failed: {:?}",
        settled_task.error
    );
    let task_events = owner_arc.events_since(&task_thread.id, None)?;
    assert!(
        !task_events.iter().any(|ev| ev.event == "approval.required"),
        "scheduled task must not open an approval card: {task_events:?}"
    );
    assert!(
        task_events.iter().any(|ev| {
            ev.event == "item.failed" && ev.payload.to_string().contains("Built-in safety gate")
        }),
        "scheduled task must hit Auto-Review unattended floor: {task_events:?}"
    );
    assert_eq!(mock.call_count(), 2);

    let watch_thread = owner_arc
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            permission_posture: Some("auto-review".to_string()),
            ..Default::default()
        })
        .await?;
    let (watch_turn, _) = owner_arc
        .start_turn_with_source(
            &watch_thread.id,
            StartTurnRequest {
                prompt: "run automation watch destructive check".to_string(),
                ..Default::default()
            },
            watch.clone(),
            None,
            false,
        )
        .await?;
    let settled_watch = wait_for_terminal_turn(owner_arc.as_ref(), &watch_turn.id).await?;
    assert_eq!(
        settled_watch.status,
        RuntimeTurnStatus::Completed,
        "watch automation failed: {:?}",
        settled_watch.error
    );
    let watch_events = owner_arc.events_since(&watch_thread.id, None)?;
    assert!(
        !watch_events
            .iter()
            .any(|ev| ev.event == "approval.required"),
        "automation watch must not open an approval card: {watch_events:?}"
    );
    assert!(
        watch_events.iter().any(|ev| {
            ev.event == "item.failed" && ev.payload.to_string().contains("Built-in safety gate")
        }),
        "automation watch must hit Auto-Review unattended floor: {watch_events:?}"
    );
    assert_eq!(mock.call_count(), 4);

    let tasks = TaskManager::start_with_runtime_manager(
        TaskManagerConfig::from_runtime(&cfg, workspace.clone(), None, Some(1)),
        cfg.clone(),
        owner_arc.clone(),
    )
    .await?;
    let automations = Arc::new(tokio::sync::Mutex::new(AutomationManager::open(
        root.path().join("automations"),
    )?));
    let trigger_thread = owner_arc
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace.clone()),
            permission_posture: Some("auto-review".to_string()),
            ..Default::default()
        })
        .await?;
    {
        let mut mgr = automations.lock().await;
        mgr.bind_task_manager(&tasks)?;
        let mut trigger =
            mgr.create_trigger(crate::automation_manager::CreateDelayedTriggerRequest {
                destination_thread_id: Some(trigger_thread.id.clone()),
                fire_at: chrono::Utc::now() + chrono::Duration::hours(1),
                message: "run delayed destructive check".to_string(),
                workspace: Some(workspace),
                owner_session_id: Some(trigger_thread.id.clone()),
                parent_trigger_id: None,
                disallowed_tools: Vec::new(),
            })?;
        trigger.fire_at = chrono::Utc::now() - chrono::Duration::minutes(1);
        mgr.save_trigger(&trigger)?;
    }
    fire_due_triggers_shared(&automations, &tasks).await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let events = owner_arc.events_since(&trigger_thread.id, None)?;
        assert!(
            !events.iter().any(|ev| ev.event == "approval.required"),
            "send_later task must never open an approval card"
        );
        if let Some(done) = events.iter().find(|ev| ev.event == "turn.completed") {
            assert_eq!(done.payload["turn"]["status"], "completed", "{events:?}");
            assert!(
                events.iter().any(|ev| {
                    ev.event == "item.failed"
                        && ev.payload.to_string().contains("Built-in safety gate")
                }),
                "send_later task must hit Auto-Review unattended floor: {events:?}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for send_later Auto-Review turn completion"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    assert!(outside_victim.exists());
    assert_eq!(mock.call_count(), 6);
    tasks.shutdown_and_wait().await?;
    Ok(())
}

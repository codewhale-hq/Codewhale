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

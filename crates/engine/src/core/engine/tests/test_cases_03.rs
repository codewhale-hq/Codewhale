#[tokio::test]
async fn queued_failed_turn_cancels_older_goal_continuation_without_third_call() {
    let objective = "stop after the intervening failure";
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "deterministic queued turn failure".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some(objective.to_string()),
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();

    handle
        .send(active_goal_message_op(
            &config,
            "queued turn that will fail",
            objective,
            None,
        ))
        .await
        .expect("queue failing ordinary turn");
    engine.schedule_goal_continuation(Vec::new()).await;
    assert!(engine.has_scheduled_goal_continuation());
    let run_task = tokio::spawn(engine.run());

    let session = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("queued failure did not settle")
        .expect("post-failure session snapshot");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the stale synthetic token must not make a third provider call"
    );
    let goal = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(goal.status, "blocked");
    assert!(
        goal.blocker
            .as_deref()
            .is_some_and(|blocker| blocker.contains("deterministic queued turn failure")),
        "{goal:?}"
    );
    let prompt = system_prompt_text(session.system_prompt.expect("blocked system prompt"));
    assert!(!prompt.contains("<session_goal>"), "{prompt}");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn configured_goal_delay_is_cancellable_without_starting_another_turn() {
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "the delayed provider turn must not start".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_continuation_delay_seconds: 300,
            ..EngineConfig::default()
        },
        &config,
        client,
    );

    engine.schedule_goal_continuation(Vec::new()).await;
    let run_task = tokio::spawn(engine.run());

    {
        let mut events = handle.rx_event.write().await;
        let waiting = tokio::time::timeout(model_turn_event_timeout(), events.recv())
            .await
            .expect("missing continuation wait event")
            .expect("engine event channel closed");
        assert!(matches!(
            waiting,
            Event::GoalContinuationWaiting { delay_seconds: 300 }
        ));
    }

    handle.cancel();
    {
        let mut events = handle.rx_event.write().await;
        let ended = tokio::time::timeout(model_turn_event_timeout(), async {
            loop {
                if let Some(Event::GoalContinuationWaitEnded { interrupted }) = events.recv().await
                {
                    break interrupted;
                }
            }
        })
        .await
        .expect("cancel did not end the continuation delay");
        assert!(
            ended,
            "the wait receipt must identify an explicit interrupt"
        );
    }

    let _ = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("engine did not accept controls after cancelling the delay")
        .expect("session snapshot after cancelled delay");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "cancelling the quiet period must happen before provider dispatch"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    tokio::time::timeout(model_turn_event_timeout(), run_task)
        .await
        .expect("engine did not shut down after delay cancellation")
        .expect("engine task");
}

#[tokio::test]
async fn sync_session_boundary_discards_delayed_goal_and_runtime_mcp_capabilities() {
    let (mut engine, _handle) = Engine::new(
        EngineConfig {
            goal_continuation_delay_seconds: 300,
            ..EngineConfig::default()
        },
        &Config::default(),
    );
    engine.session.id = "session-a".to_string();
    engine.schedule_goal_continuation(Vec::new()).await;
    assert!(engine.has_scheduled_goal_continuation());
    engine.ensure_mcp_pool().await.expect("initialize MCP pool");
    assert!(engine.mcp_pool.is_some());

    assert_eq!(
        engine.install_synced_session_id("session-a".to_string()),
        None
    );
    assert!(engine.has_scheduled_goal_continuation());
    assert!(
        engine.mcp_pool.is_some(),
        "same-id reload keeps runtime state"
    );

    assert_eq!(
        engine.install_synced_session_id("session-b".to_string()),
        Some("session-a".to_string())
    );
    assert!(!engine.has_scheduled_goal_continuation());
    assert!(
        engine.mcp_pool.is_none(),
        "same-workspace B must not inherit A's runtime-added MCP servers"
    );
    assert_eq!(
        engine.install_synced_session_id("session-a".to_string()),
        Some("session-b".to_string())
    );
    assert!(
        !engine.has_scheduled_goal_continuation() && engine.mcp_pool.is_none(),
        "A -> B -> A must not resurrect process-local state from the first A"
    );
}

#[tokio::test]
async fn sync_session_boundary_rejects_an_already_enqueued_goal_token() {
    let (mut engine, _handle) = Engine::new(
        EngineConfig {
            goal_continuation_delay_seconds: 0,
            ..EngineConfig::default()
        },
        &Config::default(),
    );
    engine.session.id = "session-a".to_string();
    engine.schedule_goal_continuation(Vec::new()).await;
    assert!(
        engine
            .scheduled_goal_continuation
            .as_ref()
            .is_some_and(|scheduled| scheduled.enqueued),
        "fixture must place A's token in the engine mailbox"
    );

    engine.install_synced_session_id("session-b".to_string());
    let input = engine
        .next_run_input(false)
        .await
        .expect("queued continuation token");
    let EngineRunInput::Operation(op) = input else {
        panic!("expected queued continuation operation");
    };
    let Op::ContinueGoal {
        dynamic_tools,
        engine_schedule_id,
    } = *op
    else {
        panic!("expected queued continuation token");
    };
    assert!(
        engine
            .take_scheduled_goal_continuation(engine_schedule_id, dynamic_tools)
            .is_none(),
        "B must reject A's already-enqueued synthetic turn token"
    );
}

#[tokio::test]
async fn cancellation_after_delay_expiry_beats_queued_continuation_dispatch() {
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "the raced provider turn must not start".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_continuation_delay_seconds: 300,
            ..EngineConfig::default()
        },
        &config,
        client,
    );

    engine.schedule_goal_continuation(Vec::new()).await;
    // Deterministically place the fixture at the timer/mailbox boundary: the
    // quiet period expired and its one coalesced token is already queued, but
    // the engine has not consumed it yet.
    engine
        .scheduled_goal_continuation
        .as_mut()
        .expect("scheduled continuation")
        .ready_at = None;
    engine.try_flush_pending_goal_continuation();
    assert!(
        engine
            .scheduled_goal_continuation
            .as_ref()
            .is_some_and(|scheduled| scheduled.enqueued)
    );
    handle.cancel();
    let run_task = tokio::spawn(engine.run());

    let _ = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("engine did not settle the delay-expiry cancellation race")
        .expect("session snapshot after expiry race");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "cancelled delayed token must be discarded before provider dispatch"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    tokio::time::timeout(model_turn_event_timeout(), run_task)
        .await
        .expect("engine did not shut down after expiry race")
        .expect("engine task");
}

#[tokio::test]
async fn configured_goal_delay_expires_into_exactly_one_continuation() {
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "stop after proving delayed dispatch".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some("dispatch once after the cadence".to_string()),
            goal_continuation_delay_seconds: 1,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    engine
        .config
        .goal_state
        .lock()
        .expect("goal lock")
        .sync_from_host_status(
            Some("dispatch once after the cadence"),
            None,
            crate::tools::goal::GoalStatus::Active,
        );
    engine.schedule_goal_continuation(Vec::new()).await;
    let run_task = tokio::spawn(engine.run());

    let (mut saw_waiting, mut saw_ready, mut saw_started) = (false, false, false);
    {
        let mut events = handle.rx_event.write().await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = events.recv().await {
                match event {
                    Event::GoalContinuationWaiting { delay_seconds: 1 } => saw_waiting = true,
                    Event::GoalContinuationWaitEnded { interrupted: false } => saw_ready = true,
                    Event::TurnStarted { .. } => {
                        saw_started = true;
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .expect("configured delay did not dispatch its continuation");
    }
    assert!(saw_waiting && saw_ready && saw_started);

    let _ = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("delayed failing turn did not settle")
        .expect("session snapshot after delayed dispatch");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one delayed schedule must create exactly one provider request"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    tokio::time::timeout(model_turn_event_timeout(), run_task)
        .await
        .expect("engine did not shut down after delayed dispatch")
        .expect("engine task");
}

#[tokio::test]
async fn goal_pause_during_configured_delay_cancels_pending_continuation() {
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "the paused provider turn must not start".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some("coordinate until paused".to_string()),
            goal_continuation_delay_seconds: 300,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();
    goal_state.lock().expect("goal lock").sync_from_host_status(
        Some("coordinate until paused"),
        None,
        crate::tools::goal::GoalStatus::Active,
    );
    engine.schedule_goal_continuation(Vec::new()).await;
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SetGoalStatus {
            goal_id: None,
            status: crate::tools::goal::GoalStatus::Paused,
            clear: false,
        })
        .await
        .expect("pause delayed goal");
    {
        let mut events = handle.rx_event.write().await;
        let interrupted = tokio::time::timeout(model_turn_event_timeout(), async {
            loop {
                if let Some(Event::GoalContinuationWaitEnded { interrupted }) = events.recv().await
                {
                    break interrupted;
                }
            }
        })
        .await
        .expect("goal pause did not end the continuation delay");
        assert!(
            interrupted,
            "a pause is an explicit interruption, not a ready-to-run receipt"
        );
    }
    let _ = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("goal pause did not settle during delay")
        .expect("session snapshot after goal pause");

    assert_eq!(
        goal_state.lock().expect("goal lock").snapshot().status,
        "paused"
    );
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a goal status control must beat the delayed continuation"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    tokio::time::timeout(model_turn_event_timeout(), run_task)
        .await
        .expect("engine did not shut down after pausing delayed goal")
        .expect("engine task");
}

#[tokio::test]
async fn host_injected_goal_continuation_waits_out_the_quiet_period() {
    // Host-managed sessions never run the engine-owned scheduler
    // (schedule_goal_continuation is gated on !host_managed_turns), so the
    // host injects ContinueGoal with engine_schedule_id None and this arm is
    // the only dispatch site. A positive configured delay must be awaited
    // before the provider request starts.
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "the host-injected continuation must wait first".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some("wait out the cadence".to_string()),
            goal_continuation_delay_seconds: 1,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    engine
        .config
        .goal_state
        .lock()
        .expect("goal lock")
        .sync_from_host_status(
            Some("wait out the cadence"),
            None,
            crate::tools::goal::GoalStatus::Active,
        );
    let run_task = tokio::spawn(engine.run());

    let queued_at = Instant::now();
    handle
        .send(Op::ContinueGoal {
            dynamic_tools: Vec::new(),
            engine_schedule_id: None,
        })
        .await
        .expect("queue host-injected continuation");
    {
        let mut events = handle.rx_event.write().await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(event) = events.recv().await {
                if matches!(event, Event::TurnStarted { .. }) {
                    break;
                }
            }
        })
        .await
        .expect("host-injected continuation did not dispatch after the quiet period");
    }
    let elapsed = queued_at.elapsed();
    assert!(
        elapsed >= Duration::from_millis(900),
        "dispatch must wait out the configured quiet period, dispatched after {elapsed:?}"
    );

    let _ = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("host-injected delayed turn did not settle")
        .expect("session snapshot after delayed host-injected dispatch");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the host-injected token must dispatch exactly one provider request"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    tokio::time::timeout(model_turn_event_timeout(), run_task)
        .await
        .expect("engine did not shut down after host-injected delay")
        .expect("engine task");
}

#[tokio::test]
async fn host_injected_goal_continuation_with_zero_delay_dispatches_immediately() {
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "zero-delay host-injected dispatch proof".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some("dispatch without a cadence".to_string()),
            goal_continuation_delay_seconds: 0,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    engine
        .config
        .goal_state
        .lock()
        .expect("goal lock")
        .sync_from_host_status(
            Some("dispatch without a cadence"),
            None,
            crate::tools::goal::GoalStatus::Active,
        );
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::ContinueGoal {
            dynamic_tools: Vec::new(),
            engine_schedule_id: None,
        })
        .await
        .expect("queue zero-delay host-injected continuation");
    {
        let mut events = handle.rx_event.write().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(event) = events.recv().await {
                if matches!(event, Event::TurnStarted { .. }) {
                    break;
                }
            }
        })
        .await
        .expect("a zero-delay host-injected continuation must dispatch immediately");
    }

    let _ = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("zero-delay host-injected turn did not settle")
        .expect("session snapshot after zero-delay dispatch");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "zero delay must not suppress the host-injected dispatch"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    tokio::time::timeout(model_turn_event_timeout(), run_task)
        .await
        .expect("engine did not shut down after zero-delay dispatch")
        .expect("engine task");
}

#[tokio::test]
async fn cancellation_during_host_injected_continuation_wait_never_dispatches() {
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "a cancelled host-injected wait must never start".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some("cancel mid-cadence".to_string()),
            goal_continuation_delay_seconds: 1,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    engine
        .config
        .goal_state
        .lock()
        .expect("goal lock")
        .sync_from_host_status(
            Some("cancel mid-cadence"),
            None,
            crate::tools::goal::GoalStatus::Active,
        );
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::ContinueGoal {
            dynamic_tools: Vec::new(),
            engine_schedule_id: None,
        })
        .await
        .expect("queue host-injected continuation");
    // Enter the quiet period, then cancel: the biased wait must drop the
    // pending pass instead of dispatching when the period would have elapsed.
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.cancel();

    // A dispatch bug would surface a TurnStarted once the 1s period expires;
    // a correct cancellation settles back into the mailbox loop silently.
    let dispatched = {
        let mut events = handle.rx_event.write().await;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let Some(event) = events.recv().await else {
                    break false;
                };
                if matches!(event, Event::TurnStarted { .. }) {
                    break true;
                }
            }
        })
        .await
        .unwrap_or(false)
    };
    assert!(
        !dispatched,
        "cancellation during the host-injected quiet period must never dispatch"
    );

    // The engine must keep accepting controls after the cancelled wait.
    let _ = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("engine did not accept controls after the cancelled wait")
        .expect("session snapshot after cancelled wait");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the cancelled host-injected token must never reach the provider"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    tokio::time::timeout(model_turn_event_timeout(), run_task)
        .await
        .expect("engine did not shut down after cancelled wait")
        .expect("engine task");
}

#[tokio::test]
async fn queued_not_started_turn_cancels_older_goal_continuation() {
    let objective = "stop when the queued turn cannot start";
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "model must never be called".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some(objective.to_string()),
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();
    engine.model_client = None;
    engine.codewhale_client_error = Some("deterministic missing model client".to_string());

    handle
        .send(active_goal_message_op(
            &config,
            "queued turn that cannot start",
            objective,
            None,
        ))
        .await
        .expect("queue not-started ordinary turn");
    engine.schedule_goal_continuation(Vec::new()).await;
    let run_task = tokio::spawn(engine.run());

    let session = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("not-started turn did not settle")
        .expect("post-rejection session snapshot");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "neither the rejected turn nor its stale token may call the provider"
    );
    let goal = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(goal.status, "blocked");
    assert!(
        goal.blocker
            .as_deref()
            .is_some_and(|blocker| blocker.contains("could not be started")),
        "{goal:?}"
    );
    let prompt = system_prompt_text(session.system_prompt.expect("blocked system prompt"));
    assert!(!prompt.contains("<session_goal>"), "{prompt}");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn queued_interrupted_turn_cancels_older_goal_continuation_without_third_call() {
    let objective = "pause after the intervening cancellation";
    let request_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_request = std::sync::Arc::new(tokio::sync::Notify::new());
    let model = std::sync::Arc::new(FirstRequestGatedGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        request_entered: std::sync::Arc::clone(&request_entered),
        release_request,
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some(objective.to_string()),
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();

    handle
        .send(active_goal_message_op(
            &config,
            "queued turn that will be cancelled",
            objective,
            None,
        ))
        .await
        .expect("queue interruptible ordinary turn");
    engine.schedule_goal_continuation(Vec::new()).await;
    let run_task = tokio::spawn(engine.run());
    tokio::time::timeout(model_turn_event_timeout(), request_entered.notified())
        .await
        .expect("queued request was never entered");
    handle.cancel();

    let session = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("interrupted turn did not settle")
        .expect("post-interruption session snapshot");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the stale synthetic token must not make a third provider call"
    );
    let goal = goal_state.lock().expect("goal lock").snapshot();
    // Interrupted ordinary turns cancel stale auto-continuation only; the goal
    // stays Active so the next user message continues without /goal resume.
    assert_eq!(goal.status, "active");
    assert_eq!(goal.blocker, None);
    assert_eq!(goal.pause_reason, None);
    let prompt = system_prompt_text(session.system_prompt.expect("active system prompt"));
    assert!(prompt.contains("<session_goal>"), "{prompt}");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn initial_goal_failure_projects_blocked_state() {
    let objective = "block the initial failed goal turn";
    let leaked_secret = "sk-initial-goal-secret-123456";
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: format!("initial provider failure: {leaked_secret}"),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());

    handle
        .send(active_goal_message_op(
            &config,
            "start a goal whose first turn fails",
            objective,
            None,
        ))
        .await
        .expect("send initial goal turn");
    let session = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("initial goal failure did not settle")
        .expect("post-failure session snapshot");

    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let goal = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(goal.objective.as_deref(), Some(objective));
    assert_eq!(goal.status, "blocked");
    let blocker = goal.blocker.as_deref().expect("failure blocker");
    assert!(blocker.contains("initial provider failure"), "{blocker}");
    assert!(!blocker.contains(leaked_secret), "{blocker}");
    let prompt = system_prompt_text(session.system_prompt.expect("blocked system prompt"));
    assert!(!prompt.contains("<session_goal>"), "{prompt}");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

/// A goal the runtime stopped (its turn failed) resumes when the person
/// writes again; the host still reports Blocked because it only learns of
/// the resume from this turn's GoalUpdated.
#[tokio::test]
async fn user_message_resumes_a_goal_only_the_runtime_blocked() {
    let objective = "resume after a runtime stop";
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "turn deadline elapsed".to_string(),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());
    let settle = || async {
        tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
            .await
            .expect("turn did not settle")
            .expect("session snapshot")
    };

    handle
        .send(active_goal_message_op(&config, "start", objective, None))
        .await
        .expect("send goal turn");
    settle().await;
    let blocked = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(blocked.status, "blocked");

    let Op::SendMessage(mut spec) = active_goal_message_op(&config, "continue", objective, None)
    else {
        unreachable!()
    };
    spec.goal_status = crate::tools::goal::GoalStatus::Blocked;
    handle
        .send(Op::SendMessage(spec))
        .await
        .expect("send continue");
    settle().await;
    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let resumed = goal_state.lock().expect("goal lock").snapshot();
    assert_ne!(
        resumed.goal_id, blocked.goal_id,
        "the continue turn ran as a resumed goal revision"
    );

    // A blocker the model reported is a judgement: the next message is an
    // ordinary turn and the goal stays blocked on that report.
    goal_state
        .lock()
        .expect("goal lock")
        .mark_blocked("needs the staging credentials".to_string())
        .unwrap();
    let reported = goal_state.lock().expect("goal lock").snapshot();
    let Op::SendMessage(mut spec) = active_goal_message_op(&config, "continue", objective, None)
    else {
        unreachable!()
    };
    spec.goal_status = crate::tools::goal::GoalStatus::Blocked;
    handle
        .send(Op::SendMessage(spec))
        .await
        .expect("send ordinary turn");
    settle().await;
    let after = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(after.status, "blocked");
    assert_eq!(after.goal_id, reported.goal_id);
    assert_eq!(
        after.blocker.as_deref(),
        Some("needs the staging credentials")
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn initial_goal_interruption_keeps_goal_active() {
    let objective = "keep goal active after interrupted turn";
    let request_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_request = std::sync::Arc::new(tokio::sync::Notify::new());
    let model = std::sync::Arc::new(FirstRequestGatedGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        request_entered: std::sync::Arc::clone(&request_entered),
        release_request,
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());

    handle
        .send(active_goal_message_op(
            &config,
            "start a goal whose first turn is cancelled",
            objective,
            None,
        ))
        .await
        .expect("send initial goal turn");
    tokio::time::timeout(model_turn_event_timeout(), request_entered.notified())
        .await
        .expect("initial request was never entered");
    handle.cancel();

    let session = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("initial interruption did not settle")
        .expect("post-interruption session snapshot");
    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let goal = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(goal.objective.as_deref(), Some(objective));
    assert_eq!(goal.status, "active");
    assert_eq!(goal.blocker, None);
    assert_eq!(goal.pause_reason, None);
    let prompt = system_prompt_text(session.system_prompt.expect("active system prompt"));
    // Durable goals stay in the prompt after interrupt so the next turn continues.
    assert!(prompt.contains("<session_goal>"), "{prompt}");
    assert!(prompt.contains(objective), "{prompt}");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn saturated_goal_controls_run_before_ready_idle_child_completion() {
    use crate::tools::subagent::SubAgentCompletion;

    let stale_tool = DynamicToolSpec {
        namespace: Some("goal-regression".to_string()),
        name: "stale".to_string(),
        description: "stale tool catalog".to_string(),
        input_schema: json!({"type": "object"}),
        defer_loading: false,
    };
    let fresh_tool = DynamicToolSpec {
        name: "fresh".to_string(),
        description: "fresh tool catalog".to_string(),
        ..stale_tool.clone()
    };
    let config = goal_custom_route_config();
    let (mut engine, handle) = Engine::new(
        EngineConfig {
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &config,
    );

    for index in 0..ENGINE_OP_CHANNEL_CAPACITY {
        let status = if index + 1 == ENGINE_OP_CHANNEL_CAPACITY {
            crate::tools::goal::GoalStatus::Paused
        } else {
            crate::tools::goal::GoalStatus::Active
        };
        handle
            .tx_op
            .try_send(Op::SetGoalStatus {
                goal_id: None,
                status,
                clear: false,
            })
            .unwrap_or_else(|error| panic!("fill ordering mailbox slot {index}: {error}"));
    }
    assert_eq!(handle.tx_op.capacity(), 0, "fixture must saturate mailbox");
    engine
        .tx_subagent_completion
        .try_send(SubAgentCompletion {
            owner_session_id: engine.session.id.clone(),
            agent_id: "agent_ready_during_backpressure".to_string(),
            payload: "ready child completion".to_string(),
        })
        .expect("queue ready idle child completion");
    engine.schedule_goal_continuation(vec![stale_tool]).await;
    assert!(
        engine.has_scheduled_goal_continuation(),
        "a live schedule must activate temporary op priority"
    );

    // Inspect the exact production receive helper without running handlers:
    // all controls that filled the mailbox, including the final pause, must be
    // selected before the already-ready idle child completion.
    for index in 0..ENGINE_OP_CHANNEL_CAPACITY {
        let input = tokio::time::timeout(model_turn_event_timeout(), engine.next_run_input(false))
            .await
            .expect("backpressured operation receive timed out")
            .expect("engine input");
        let EngineRunInput::Operation(op) = input else {
            panic!("idle child completion beat queued control {index}");
        };
        let Op::SetGoalStatus { status, clear, .. } = *op else {
            panic!("unexpected operation before queued control {index}");
        };
        assert!(!clear);
        let expected = if index + 1 == ENGINE_OP_CHANNEL_CAPACITY {
            crate::tools::goal::GoalStatus::Paused
        } else {
            crate::tools::goal::GoalStatus::Active
        };
        assert_eq!(status, expected);
        if index == 0 {
            // Refresh after capacity opens. The existing token must retain its
            // FIFO position behind the remaining controls while carrying the
            // newest runtime tool catalog when it is eventually consumed.
            engine
                .schedule_goal_continuation(vec![fresh_tool.clone()])
                .await;
        }
    }

    let token = engine
        .next_run_input(false)
        .await
        .expect("backpressured continuation token");
    let EngineRunInput::Operation(token) = token else {
        panic!("idle child completion beat the backpressured continuation token");
    };
    let Op::ContinueGoal {
        dynamic_tools,
        engine_schedule_id,
    } = *token
    else {
        panic!("expected engine-owned continuation token");
    };
    assert!(dynamic_tools.is_empty());
    let continued_tools = engine
        .take_scheduled_goal_continuation(engine_schedule_id, dynamic_tools)
        .expect("engine-owned continuation token must consume its schedule marker");
    assert_eq!(continued_tools, vec![fresh_tool]);
    assert!(!engine.has_scheduled_goal_continuation());

    let child = engine
        .next_run_input(false)
        .await
        .expect("ready idle child completion");
    let EngineRunInput::SubAgentCompletion(child) = child else {
        panic!("unexpected operation after backpressure drain");
    };
    assert_eq!(child.agent_id, "agent_ready_during_backpressure");
}

#[tokio::test]
async fn unsaturated_goal_control_runs_before_ready_idle_child_completion() {
    use crate::tools::subagent::SubAgentCompletion;

    let config = goal_custom_route_config();
    let (mut engine, handle) = Engine::new(
        EngineConfig {
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &config,
    );

    handle
        .tx_op
        .try_send(Op::SetGoalStatus {
            goal_id: None,
            status: crate::tools::goal::GoalStatus::Paused,
            clear: false,
        })
        .expect("queue unsaturated pause");
    engine
        .tx_subagent_completion
        .try_send(SubAgentCompletion {
            owner_session_id: engine.session.id.clone(),
            agent_id: "agent_ready_without_backpressure".to_string(),
            payload: "ready child completion".to_string(),
        })
        .expect("queue ready idle child completion");
    engine.schedule_goal_continuation(Vec::new()).await;
    assert!(engine.has_scheduled_goal_continuation());
    assert!(
        handle.tx_op.capacity() > 0,
        "fixture must leave the mailbox unsaturated"
    );

    let first = engine
        .next_run_input(false)
        .await
        .expect("queued pause must be selected");
    let EngineRunInput::Operation(first) = first else {
        panic!("ready child completion beat an unsaturated queued pause");
    };
    assert!(matches!(
        *first,
        Op::SetGoalStatus {
            goal_id: None,
            status: crate::tools::goal::GoalStatus::Paused,
            clear: false
        }
    ));

    let token = engine
        .next_run_input(false)
        .await
        .expect("scheduled continuation token");
    let EngineRunInput::Operation(token) = token else {
        panic!("ready child completion beat the live continuation token");
    };
    let Op::ContinueGoal {
        dynamic_tools,
        engine_schedule_id,
    } = *token
    else {
        panic!("expected continuation token behind pause");
    };
    engine
        .take_scheduled_goal_continuation(engine_schedule_id, dynamic_tools)
        .expect("consume live continuation schedule");
    assert!(!engine.has_scheduled_goal_continuation());

    let child = engine
        .next_run_input(false)
        .await
        .expect("idle child completion after schedule consumption");
    let EngineRunInput::SubAgentCompletion(child) = child else {
        panic!("normal child fairness did not resume after schedule consumption");
    };
    assert_eq!(child.agent_id, "agent_ready_without_backpressure");

    engine.config.goal_state.lock().expect("goal").replace(
        "finish the existing goal",
        None,
        Some("same-goal".to_string()),
    );
    engine
        .handle_set_goal_status(GoalStatus::Active, false, Some("unused-new-id".to_string()))
        .await;
    let scheduled_id = engine
        .scheduled_goal_continuation
        .as_ref()
        .expect("idle kick")
        .id;
    engine
        .handle_set_goal_status(GoalStatus::Active, false, None)
        .await;
    assert_eq!(
        engine.scheduled_goal_continuation.as_ref().unwrap().id,
        scheduled_id
    );
    assert_eq!(
        engine
            .config
            .goal_state
            .lock()
            .unwrap()
            .snapshot()
            .goal_id
            .as_deref(),
        Some("same-goal")
    );
    engine.cancel_scheduled_goal_continuation(true).await;
    engine.cancel_token.cancel();
    engine
        .handle_set_goal_status(GoalStatus::Active, false, None)
        .await;
    assert!(!engine.has_scheduled_goal_continuation());
}

#[tokio::test]
async fn cross_turn_token_budget_exhaustion_does_not_pause_goal() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let budget_turn = vec![
        canned::message_start("mock_goal_budget"),
        canned::text_block_start(0),
        canned::text_delta(0, "budget spent"),
        canned::block_stop(0),
        canned::message_delta(
            "end_turn",
            Some(Usage {
                input_tokens: 8,
                output_tokens: 3,
                ..Usage::default()
            }),
        ),
        canned::message_stop(),
    ];
    let model = std::sync::Arc::new(MockLlmClient::new(vec![
        budget_turn,
        canned::simple_text_turn("the cross-turn continuation runs past the exhausted budget"),
    ]));
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = Config::default();
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        subagents_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("finish within budget".to_string()),
        goal_token_budget: Some(10),
        ..EngineConfig::default()
    };
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "start budgeted goal".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(
                &config,
                codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL,
            ),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: Some("finish within budget".to_string()),
            goal_token_budget: Some(10),
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::ExternalUser,
            submission_id: None,
        }))
        .await
        .expect("send budgeted goal turn");

    let mut starts = 0;
    let mut completed_turns = 0;
    let mut saw_blocked_goal = false;
    while completed_turns < 2 || !saw_blocked_goal {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("budget goal event timeout")
        .expect("budget goal event");
        match event {
            Event::TurnStarted { .. } => starts += 1,
            Event::TurnComplete { status, error, .. } => {
                if status == TurnOutcomeStatus::Completed {
                    completed_turns += 1;
                } else {
                    // The fixture mock is exhausted after the continuation
                    // turns; that provider failure blocks the goal (a
                    // legitimate terminal) — it is not a budget pause.
                    assert!(
                        error
                            .as_deref()
                            .is_some_and(|e| e.contains("no canned turn queued")),
                        "unexpected non-completed turn: {status:?} {error:?}"
                    );
                }
            }
            Event::GoalUpdated { snapshot } if snapshot.status == "paused" => {
                panic!(
                    "budgets are telemetry-only in unbounded goal mode; the goal must not \
                     pause on budget (pause_reason={:?})",
                    snapshot.pause_reason
                );
            }
            Event::GoalUpdated { snapshot } if snapshot.status == "blocked" => {
                saw_blocked_goal = true;
            }
            _ => {}
        }
    }

    let snapshot = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(snapshot.status, "blocked");
    assert_eq!(
        snapshot.pause_reason, None,
        "budget must never be the pause reason in unbounded goal mode"
    );
    assert_eq!(snapshot.tokens_used, 11);
    assert_eq!(snapshot.token_budget, Some(10));
    assert!(
        starts >= 2,
        "budget exhaustion must not stop the cross-turn continuation (starts={starts})"
    );
    assert!(
        model.call_count() >= 2,
        "the continuation must issue a second provider call (calls={})",
        model.call_count()
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn current_turn_usage_does_not_stop_budgeted_goal_after_one_provider_call() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let objective = "stop before an intra-turn budget overspend";
    let budget_turn = vec![
        canned::message_start("mock_goal_current_turn_budget"),
        canned::text_block_start(0),
        canned::text_delta(0, "budget spent"),
        canned::block_stop(0),
        canned::message_delta(
            "end_turn",
            Some(Usage {
                input_tokens: 8,
                output_tokens: 3,
                ..Usage::default()
            }),
        ),
        canned::message_stop(),
    ];
    let model = std::sync::Arc::new(MockLlmClient::new(vec![
        budget_turn,
        canned::simple_text_turn("the continuation runs past the exhausted budget"),
    ]));
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = goal_custom_route_config();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            subagents_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some(objective.to_string()),
            goal_token_budget: Some(10),
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());

    handle
        .send(active_goal_message_op(
            &config,
            "start the budgeted goal",
            objective,
            Some(10),
        ))
        .await
        .expect("send budgeted goal turn");
    let mut starts = 0;
    let mut saw_blocked_goal = false;
    while !saw_blocked_goal {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("current-turn budget continuation did not settle")
        .expect("current-turn budget event");
        match event {
            Event::TurnStarted { .. } => starts += 1,
            Event::TurnComplete { status, error, .. } => {
                if status != TurnOutcomeStatus::Completed {
                    assert!(
                        error
                            .as_deref()
                            .is_some_and(|e| e.contains("no canned turn queued")),
                        "unexpected non-completed turn: {status:?} {error:?}"
                    );
                }
            }
            Event::GoalUpdated { snapshot } if snapshot.status == "paused" => {
                panic!(
                    "budgets are telemetry-only in unbounded goal mode; the goal must not \
                     pause on budget (pause_reason={:?})",
                    snapshot.pause_reason
                );
            }
            Event::GoalUpdated { snapshot } if snapshot.status == "blocked" => {
                saw_blocked_goal = true;
            }
            _ => {}
        }
    }
    assert!(
        model.call_count() >= 2,
        "current-turn usage must not stop additional provider calls (calls={})",
        model.call_count()
    );
    assert_eq!(model.remaining_turns(), 0);
    let goal = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(goal.status, "blocked");
    assert_eq!(
        goal.pause_reason, None,
        "budget must never be the pause reason in unbounded goal mode"
    );
    assert_eq!(goal.tokens_used, 11);
    assert_eq!(goal.token_budget, Some(10));
    assert!(
        starts >= 1,
        "the initial goal turn must start before its intra-turn continuation (starts={starts})"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn tool_response_crossing_goal_budget_issues_second_provider_request() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let objective = "stop after a budget-crossing goal tool response";
    let budget_tool_turn = vec![
        canned::message_start("mock_goal_tool_budget"),
        canned::tool_use_block_start(0, "call-get-goal", "get_goal"),
        canned::tool_input_delta(0, "{}"),
        canned::block_stop(0),
        canned::message_delta(
            "tool_use",
            Some(Usage {
                input_tokens: 8,
                output_tokens: 3,
                ..Usage::default()
            }),
        ),
        canned::message_stop(),
    ];
    let model = std::sync::Arc::new(MockLlmClient::new(vec![
        budget_tool_turn,
        canned::simple_text_turn(
            "this second provider response is issued past the exhausted budget",
        ),
    ]));
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = goal_custom_route_config();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            snapshots_enabled: false,
            subagents_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some(objective.to_string()),
            goal_token_budget: Some(10),
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());

    handle
        .send(active_goal_message_op(
            &config,
            "inspect the goal without overspending",
            objective,
            Some(10),
        ))
        .await
        .expect("send budgeted goal tool turn");

    let mut saw_get_goal = false;
    let mut saw_blocked_goal = false;
    while !saw_blocked_goal {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("budget-crossing goal tool turn did not settle")
        .expect("budget-crossing goal tool event");
        match event {
            Event::ToolCallComplete { name, result, .. } if name == "get_goal" => {
                assert!(result.expect("get_goal result").success);
                saw_get_goal = true;
            }
            Event::TurnComplete { status, error, .. } => {
                if status != TurnOutcomeStatus::Completed {
                    assert!(
                        error
                            .as_deref()
                            .is_some_and(|e| e.contains("no canned turn queued")),
                        "unexpected non-completed turn: {status:?} {error:?}"
                    );
                }
            }
            Event::GoalUpdated { snapshot } if snapshot.status == "paused" => {
                panic!(
                    "budgets are telemetry-only in unbounded goal mode; the goal must not \
                     pause on budget (pause_reason={:?})",
                    snapshot.pause_reason
                );
            }
            Event::GoalUpdated { snapshot } if snapshot.status == "blocked" => {
                saw_blocked_goal = true;
            }
            _ => {}
        }
    }

    assert!(saw_get_goal, "the first response's goal tool must execute");
    assert!(
        model.call_count() >= 2,
        "the provider-request boundary must not stop the second model call (calls={})",
        model.call_count()
    );
    assert_eq!(model.remaining_turns(), 0);
    let goal = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(goal.status, "blocked");
    assert_eq!(goal.tokens_used, 11, "usage must be durably recorded once");
    assert_eq!(goal.token_budget, Some(10));
    assert_eq!(
        goal.pause_reason, None,
        "budget must never be the pause reason in unbounded goal mode"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

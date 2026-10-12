

#[tokio::test]
async fn queued_goal_clear_refreshes_prompt_and_cancels_stale_continuation() {
    let config = Config::default();
    let engine_config = EngineConfig {
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("clear this goal".to_string()),
        goal_token_budget: Some(42_000),
        ..EngineConfig::default()
    };
    let (engine, handle) = Engine::new(engine_config, &config);
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());

    // Model the mailbox order produced when the user clears a goal while its
    // prior turn is still finishing: the control reaches the queue before the
    // synthetic continuation that TurnComplete schedules.
    handle
        .send(Op::SetGoalStatus {
            goal_id: None,
            status: crate::tools::goal::GoalStatus::Active,
            clear: true,
        })
        .await
        .expect("queue goal clear");
    handle
        .send(Op::ContinueGoal {
            dynamic_tools: Vec::new(),
            engine_schedule_id: None,
        })
        .await
        .expect("queue stale continuation");

    // This receipt sits behind both operations. Once it arrives, a stale
    // continuation has either incorrectly started a turn or been consumed.
    let session = handle
        .get_session_snapshot()
        .await
        .expect("post-clear session snapshot");
    let prompt = match session.system_prompt.expect("post-clear system prompt") {
        SystemPrompt::Text(text) => text,
        SystemPrompt::Blocks(blocks) => blocks
            .into_iter()
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n"),
    };
    assert!(
        !prompt.contains("<session_goal>"),
        "cleared config fallback must not restore the goal prompt: {prompt}"
    );
    let snapshot = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(snapshot.objective, None);
    assert_eq!(snapshot.status, "none");
    assert_eq!(snapshot.token_budget, None);

    let mut saw_clear_session = false;
    let mut saw_clear_goal = false;
    let mut saw_clear_status = false;
    {
        let mut events = handle.rx_event.write().await;
        while let Ok(event) = events.try_recv() {
            match event {
                Event::TurnStarted { .. } => {
                    panic!("queued clear must prevent a stale goal continuation")
                }
                Event::SessionUpdated { system_prompt, .. } => {
                    let prompt = match system_prompt.expect("clear SessionUpdated prompt") {
                        SystemPrompt::Text(text) => text,
                        SystemPrompt::Blocks(blocks) => blocks
                            .into_iter()
                            .map(|block| block.text)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    assert!(!prompt.contains("<session_goal>"), "{prompt}");
                    saw_clear_session = true;
                }
                Event::GoalUpdated { snapshot } => {
                    assert_eq!(snapshot.objective, None);
                    assert_eq!(snapshot.status, "none");
                    saw_clear_goal = true;
                }
                Event::Status { message } if message == "Goal cleared." => {
                    saw_clear_status = true;
                }
                _ => {}
            }
        }
    }
    assert!(
        saw_clear_session,
        "clear must refresh persisted prompt state"
    );
    assert!(saw_clear_goal, "clear must emit a canonical empty snapshot");
    assert!(saw_clear_status, "clear must remain user-visible");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

fn goal_custom_route_config() -> Config {
    let mut custom = HashMap::new();
    custom.insert(
        "custom-a".to_string(),
        crate::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18181/v1".to_string()),
            model: Some("local-model".to_string()),
            api_key: Some("local-test-key".to_string()),
            ..crate::config::ProviderConfig::default()
        },
    );
    Config {
        provider: Some("custom-a".to_string()),
        providers: Some(crate::config::ProvidersConfig {
            custom,
            ..crate::config::ProvidersConfig::default()
        }),
        ..Config::default()
    }
}

#[tokio::test]
async fn ordinary_prose_never_activates_a_goal() {
    let request_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_request = std::sync::Arc::new(tokio::sync::Notify::new());
    let model = std::sync::Arc::new(FirstRequestGatedGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        request_entered: std::sync::Arc::clone(&request_entered),
        release_request: std::sync::Arc::clone(&release_request),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            max_steps: 1,
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
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "hello - take over and make it your /goal to solve navier stokes".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&config, "local-model"),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
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
        .expect("send explicit natural goal turn");

    // #6290 rework: the natural-language `/goal` prose parser is gone. The
    // same wording that used to activate a goal is now an ordinary turn;
    // only the model (`create_goal`) or the `/goal` command creates one.
    let mut saw_goal = false;
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("prose goal event timeout")
        .expect("prose goal event");
        match event {
            Event::GoalUpdated { .. } => {
                saw_goal = true;
            }
            Event::TurnStarted { .. } => {
                assert!(
                    !saw_goal,
                    "ordinary prose must not publish a goal before provider work starts"
                );
                break;
            }
            _ => {}
        }
    }

    tokio::time::timeout(model_turn_event_timeout(), request_entered.notified())
        .await
        .expect("provider request was never entered");
    release_request.notify_one();
    let _ = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("turn did not settle")
        .expect("post-turn session snapshot");
    let snapshot = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(snapshot.objective.as_deref(), None);
    assert!(!snapshot.is_active());
    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

/// Drive one turn in `mode` and report what the goal path did before the
/// provider was called: the objective published by `GoalUpdated` (if any),
/// whether the engine goal state is active, and how many Operate contract
/// messages the session log holds afterwards.
async fn operate_goal_probe(mode: AppMode, prompt: &str) -> (Option<String>, bool, usize) {
    let request_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_request = std::sync::Arc::new(tokio::sync::Notify::new());
    let model = std::sync::Arc::new(FirstRequestGatedGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        request_entered: std::sync::Arc::clone(&request_entered),
        release_request: std::sync::Arc::clone(&release_request),
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            max_steps: 1,
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
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: prompt.to_string(),
            images: Vec::new(),
            mode,
            route: resolved_route_for_test(&config, "local-model"),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
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
        .expect("send probe turn");

    let mut published_objective = None;
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("probe event timeout")
        .expect("probe event");
        match event {
            Event::GoalUpdated { snapshot } if snapshot.status == "active" => {
                published_objective = snapshot.objective;
            }
            Event::TurnStarted { .. } => break,
            _ => {}
        }
    }
    tokio::time::timeout(model_turn_event_timeout(), request_entered.notified())
        .await
        .expect("provider request was never entered");
    let active = goal_state.lock().expect("goal lock").is_active();
    if active {
        // Stop autonomous continuation after the one provider call.
        handle
            .send(Op::SetGoalStatus {
                goal_id: None,
                status: crate::tools::goal::GoalStatus::Paused,
                clear: false,
            })
            .await
            .expect("queue goal pause");
    }
    release_request.notify_one();
    let snapshot = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("probe turn did not settle")
        .expect("probe session snapshot");
    let contracts = snapshot
        .messages
        .iter()
        .filter(|message| crate::runtime_handoff::is_operate_contract_message(message))
        .count();
    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    (published_objective, active, contracts)
}

#[tokio::test]
async fn operate_never_promotes_wording_to_a_goal() {
    let prompt =
        "Migrate the settings loader to the new config crate and keep the old keys readable";

    // The verb-list promotion is gone: an ordinary work prompt is an ordinary
    // turn in every mode, and the model decides goals through `create_goal`
    // (docs/design/TUI_DECONSTRUCTION.md, founder clarification 2026-09-09).
    let (objective, active, contracts) = operate_goal_probe(AppMode::Operate, prompt).await;
    assert_eq!(
        objective, None,
        "the host must not infer a goal from wording"
    );
    assert!(!active);
    assert_eq!(contracts, 1, "Operate appends its contract exactly once");

    let (objective, active, contracts) = operate_goal_probe(AppMode::Agent, prompt).await;
    assert_eq!(objective, None, "Work must not promote a prompt to a goal");
    assert!(!active);
    assert_eq!(contracts, 0, "Work never sees the Operate contract");

    // #6290 rework: even an explicit-looking declaration is ordinary
    // prose now — the host never parses it, and the model decides goals
    // through `create_goal`.
    let (objective, active, contracts) =
        operate_goal_probe(AppMode::Operate, "Please set /goal to ship the release").await;
    assert_eq!(
        objective, None,
        "prose asking for a goal must not create one host-side"
    );
    assert!(!active);
    assert_eq!(contracts, 1);
}

#[tokio::test]
async fn operate_leaves_followup_and_long_questions_as_ordinary_turns() {
    let report = "what about like rust or docker builds or something";
    let (objective, active, contracts) = operate_goal_probe(AppMode::Operate, report).await;
    assert_eq!(
        objective, None,
        "conversational followup must remain ordinary turn in Operate"
    );
    assert!(!active);
    assert_eq!(contracts, 1);

    let long_q =
        "why did the build fail on the last step when running under docker on macos with rust 1.80";
    let (objective, active, contracts) = operate_goal_probe(AppMode::Operate, long_q).await;
    assert_eq!(
        objective, None,
        "long question without punctuation must remain ordinary turn"
    );
    assert!(!active);
    assert_eq!(contracts, 1);

    let zh_followup = "那 rust 或者 docker 构建呢";
    let (objective, active, contracts) = operate_goal_probe(AppMode::Operate, zh_followup).await;
    assert_eq!(
        objective, None,
        "Chinese followup must remain ordinary turn in Operate"
    );
    assert!(!active);
    assert_eq!(contracts, 1);
}

#[tokio::test]
async fn operate_does_not_create_a_goal_when_the_work_request_declines_one() {
    let prompt = "Run one bounded cancellation check. Do not edit files, inspect other files, create a goal, spawn agents, or start any other tool.";
    let (objective, active, contracts) = operate_goal_probe(AppMode::Operate, prompt).await;
    assert_eq!(objective, None);
    assert!(!active);
    assert_eq!(
        contracts, 1,
        "the ordinary Operate turn still reaches the model"
    );
}

#[tokio::test]
async fn operate_contract_is_appended_once_and_an_existing_goal_is_never_replaced() {
    let first_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release_first = std::sync::Arc::new(tokio::sync::Notify::new());
    let model = std::sync::Arc::new(IndexedGatedGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        gates: HashMap::from([(
            1,
            (
                std::sync::Arc::clone(&first_entered),
                std::sync::Arc::clone(&release_first),
            ),
        )]),
        max_calls: 2,
    });
    let config = goal_custom_route_config();
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            max_steps: 1,
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());

    let send = |content: &str, goal_objective: Option<String>, goal_status| {
        Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: content.to_string(),
            images: Vec::new(),
            mode: AppMode::Operate,
            route: resolved_route_for_test(&config, "local-model"),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective,
            goal_token_budget: None,
            goal_status,
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
        })
    };

    let first_objective =
        "Migrate the settings loader to the new config crate and keep the old keys readable";
    // #6290 rework: prose no longer creates goals, so the unfinished goal
    // this test needs is seeded directly — the same `GoalState::create` path
    // the `/goal` command and the model's `create_goal` tool use.
    goal_state
        .lock()
        .expect("goal lock")
        .create(first_objective.to_string(), None)
        .expect("seed unfinished goal");
    handle
        .send(send(
            first_objective,
            Some(first_objective.to_string()),
            crate::tools::goal::GoalStatus::Active,
        ))
        .await
        .expect("send first Operate turn");
    tokio::time::timeout(model_turn_event_timeout(), first_entered.notified())
        .await
        .expect("first provider request was never entered");
    assert_eq!(
        goal_state.lock().expect("goal lock").objective(),
        Some(first_objective)
    );
    handle
        .send(Op::SetGoalStatus {
            goal_id: None,
            status: crate::tools::goal::GoalStatus::Paused,
            clear: false,
        })
        .await
        .expect("queue goal pause");
    release_first.notify_one();
    let _ = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("first turn did not settle")
        .expect("first session snapshot");

    // Second Operate prompt while the (paused) goal is still unfinished: the
    // host reports that goal, so the prompt is ordinary work under it.
    handle
        .send(send(
            "Refactor the provider table so it survives a config reload",
            Some(first_objective.to_string()),
            crate::tools::goal::GoalStatus::Paused,
        ))
        .await
        .expect("send second Operate turn");
    let snapshot = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("second turn did not settle")
        .expect("second session snapshot");
    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let contracts = snapshot
        .messages
        .iter()
        .filter(|message| crate::runtime_handoff::is_operate_contract_message(message))
        .count();
    assert_eq!(
        contracts, 1,
        "the contract must not repeat on later Operate turns"
    );
    let goal = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(goal.objective.as_deref(), Some(first_objective));
    assert_eq!(goal.status, "paused");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

fn without_named_custom_route(mut config: Config) -> Config {
    config
        .providers
        .as_mut()
        .expect("custom providers")
        .custom
        .clear();
    config
}

#[tokio::test]
async fn exhausted_goal_reaches_route_failure_without_budget_pause() {
    let config = goal_custom_route_config();
    let engine_config = EngineConfig {
        model: "local-model".to_string(),
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("stop at the budget".to_string()),
        goal_token_budget: Some(10),
        ..EngineConfig::default()
    };
    let (mut engine, handle) = Engine::new(engine_config, &config);
    let goal_state = engine.config.goal_state.clone();
    goal_state.lock().expect("goal lock").record_usage(11, 0, std::time::Instant::now());

    let invalid_route_config = without_named_custom_route(config);
    engine.authoritative_route_config =
        Some(Arc::new(parking_lot::RwLock::new(invalid_route_config)));
    assert!(
        engine.current_runtime_route().is_err(),
        "fixture must prove route resolution cannot succeed"
    );
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::ContinueGoal {
            dynamic_tools: Vec::new(),
            engine_schedule_id: None,
        })
        .await
        .expect("queue exhausted continuation");

    let mut saw_route_error = false;
    let mut saw_blocked_goal = false;
    while !(saw_route_error && saw_blocked_goal) {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("budget terminal event timeout")
        .expect("budget terminal event");
        match event {
            Event::TurnStarted { .. } => {
                panic!("an exhausted goal must not start a turn before route resolution")
            }
            Event::Error { envelope, .. } => {
                assert!(
                    envelope.message.contains("route is no longer valid"),
                    "the exhausted goal must reach the invalid-route failure, got: {envelope:?}"
                );
                saw_route_error = true;
            }
            Event::GoalUpdated { snapshot } if snapshot.status == "paused" => {
                panic!(
                    "budgets are telemetry-only in unbounded goal mode; the goal must not                      pause on budget (pause_reason={:?})",
                    snapshot.pause_reason
                );
            }
            Event::GoalUpdated { snapshot } if snapshot.status == "blocked" => {
                assert_eq!(snapshot.tokens_used, 11);
                assert_eq!(snapshot.token_budget, Some(10));
                assert_eq!(
                    snapshot.pause_reason, None,
                    "budget must never be the pause reason in unbounded goal mode"
                );
                saw_blocked_goal = true;
            }
            _ => {}
        }
    }

    let snapshot = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(snapshot.status, "blocked");
    assert_eq!(snapshot.tokens_used, 11);
    assert_eq!(snapshot.token_budget, Some(10));
    assert_eq!(snapshot.pause_reason, None);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn continuation_circuit_breaker_pauses_with_run_limit_reason() {
    // #5052: the backstop is configurable ([goal] max_continuations) and set
    // deliberately past the retired hardcoded cap of 10 to prove an operate
    // goal is no longer stopped there — only the configured backstop halts a
    // pathological loop that never emits a terminal signal.
    let backstop = 12u32;
    let config = Config::default();
    let (engine, handle) = Engine::new(
        EngineConfig {
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some("stop a runaway continuation loop".to_string()),
            goal_max_continuations: backstop,
            ..EngineConfig::default()
        },
        &config,
    );
    let goal_state = engine.config.goal_state.clone();
    {
        let mut goal = goal_state.lock().expect("goal lock");
        for _ in 0..backstop {
            goal.record_continuation();
        }
    }
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::ContinueGoal {
            dynamic_tools: Vec::new(),
            engine_schedule_id: None,
        })
        .await
        .expect("queue capped continuation");

    let mut saw_pause = false;
    let mut saw_reason = false;
    while !(saw_pause && saw_reason) {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("continuation cap event timeout")
        .expect("continuation cap event");
        match event {
            Event::TurnStarted { .. } => panic!("capped goal must not start another turn"),
            Event::GoalUpdated { snapshot } if snapshot.status == "paused" => {
                assert_eq!(
                    snapshot.pause_reason,
                    Some(crate::tools::goal::GoalPauseReason::Backoff)
                );
                saw_pause = true;
            }
            Event::Status { message } if message.contains("automatic continuations") => {
                assert!(message.contains(&backstop.to_string()), "{message}");
                assert!(message.contains("[goal] max_continuations"), "{message}");
                saw_reason = true;
            }
            _ => {}
        }
    }

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn goal_continues_past_legacy_ten_pass_cap_when_budget_remains() {
    // #5052 regression: 10 automatic continuations used to be a terminal stop.
    // With the default backstop and budget remaining, the loop must keep
    // dispatching toward the completion gate.
    let config = Config::default();
    let (engine, _handle) = Engine::new(
        EngineConfig {
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some("run to the completion gate, not a pass count".to_string()),
            goal_token_budget: Some(1_000_000),
            ..EngineConfig::default()
        },
        &config,
    );
    {
        let mut goal = engine.config.goal_state.lock().expect("goal lock");
        for _ in 0..10 {
            goal.record_continuation();
        }
    }

    match engine.goal_continuation_if_active() {
        GoalContinuationAction::Dispatch { snapshot, .. } => {
            assert_eq!(snapshot.continuation_count, 11);
        }
        other => panic!("goal must continue past 10 passes, got {other:?}"),
    }

    // A backstop of 0 means unlimited-with-budget-stops: even a pathological
    // pass count keeps continuing while budget remains.
    let (engine, _handle) = Engine::new(
        EngineConfig {
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            goal_objective: Some("unlimited backstop".to_string()),
            goal_token_budget: Some(1_000_000),
            goal_max_continuations: 0,
            ..EngineConfig::default()
        },
        &config,
    );
    {
        let mut goal = engine.config.goal_state.lock().expect("goal lock");
        for _ in 0..500 {
            goal.record_continuation();
        }
    }
    assert!(
        matches!(
            engine.goal_continuation_if_active(),
            GoalContinuationAction::Dispatch { .. }
        ),
        "backstop 0 must not stop an in-budget goal"
    );
}

/// T08-03: the cross-turn continuation gate stops at an *enforced* token
/// budget, the same stop the intra-turn gate and the host take, and keeps
/// the default advisory behavior when enforcement is off.
#[tokio::test]
async fn cross_turn_goal_continuation_stops_at_an_enforced_token_budget() {
    let config = Config::default();
    for enforce in [true, false] {
        let (engine, _handle) = Engine::new(
            EngineConfig {
                snapshots_enabled: false,
                terminal_chrome_enabled: false,
                goal_objective: Some("stop at the enforced budget".to_string()),
                goal_token_budget: Some(100),
                goal_enforce_token_budget: enforce,
                ..EngineConfig::default()
            },
            &config,
        );
        engine
            .config
            .goal_state
            .lock()
            .expect("goal lock")
            .record_usage(100, 0, std::time::Instant::now());
        let action = engine.goal_continuation_if_active();
        if enforce {
            assert!(
                matches!(
                    action,
                    GoalContinuationAction::Stopped {
                        reason: crate::tools::goal::GoalPauseReason::BudgetLimit,
                        ..
                    }
                ),
                "an exhausted enforced budget must not dispatch another turn: {action:?}"
            );
        } else {
            assert!(
                matches!(action, GoalContinuationAction::Dispatch { .. }),
                "an advisory budget keeps the goal running: {action:?}"
            );
        }
    }
}

#[tokio::test]
async fn invalid_route_blocks_active_goal_and_refreshes_projections() {
    let config = goal_custom_route_config();
    let engine_config = EngineConfig {
        model: "local-model".to_string(),
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("keep going across route drift".to_string()),
        ..EngineConfig::default()
    };
    let (mut engine, handle) = Engine::new(engine_config, &config);
    let goal_state = engine.config.goal_state.clone();
    engine.authoritative_route_config = Some(Arc::new(parking_lot::RwLock::new(
        without_named_custom_route(config),
    )));
    assert!(
        engine.current_runtime_route().is_err(),
        "fixture must fail before dispatch"
    );
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::ContinueGoal {
            dynamic_tools: Vec::new(),
            engine_schedule_id: None,
        })
        .await
        .expect("queue active continuation");
    let session = handle
        .get_session_snapshot()
        .await
        .expect("post-route-failure session snapshot");

    let prompt = match session.system_prompt.expect("blocked system prompt") {
        SystemPrompt::Text(text) => text,
        SystemPrompt::Blocks(blocks) => blocks
            .into_iter()
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n"),
    };
    assert!(!prompt.contains("<session_goal>"), "{prompt}");
    let snapshot = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(snapshot.status, "blocked");
    assert!(
        snapshot
            .blocker
            .as_deref()
            .is_some_and(|blocker| blocker.contains("provider route is no longer valid")),
        "{snapshot:?}"
    );

    let mut saw_route_error = false;
    let mut saw_blocked_session = false;
    let mut saw_blocked_goal = false;
    let mut saw_blocked_status = false;
    {
        let mut events = handle.rx_event.write().await;
        while let Ok(event) = events.try_recv() {
            match event {
                Event::TurnStarted { .. } => {
                    panic!("invalid route must not start a continuation turn")
                }
                Event::Error { envelope, .. } => {
                    assert!(format!("{envelope:?}").contains("provider route is no longer valid"));
                    saw_route_error = true;
                }
                Event::SessionUpdated {
                    system_prompt: Some(system_prompt),
                    ..
                } => {
                    let prompt = match system_prompt {
                        SystemPrompt::Text(text) => text,
                        SystemPrompt::Blocks(blocks) => blocks
                            .into_iter()
                            .map(|block| block.text)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    assert!(!prompt.contains("<session_goal>"), "{prompt}");
                    saw_blocked_session = true;
                }
                Event::GoalUpdated { snapshot } if snapshot.status == "blocked" => {
                    saw_blocked_goal = true;
                }
                Event::Status { message }
                    if message.contains("provider route is no longer valid") =>
                {
                    assert!(message.contains("resume the goal"), "{message}");
                    saw_blocked_status = true;
                }
                _ => {}
            }
        }
    }
    assert!(saw_route_error, "route failure must remain visible");
    assert!(
        saw_blocked_session,
        "session prompt projection must refresh"
    );
    assert!(saw_blocked_goal, "sidebar must receive blocked state");
    assert!(saw_blocked_status, "blocked reason must remain visible");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn rejected_continuation_dispatch_blocks_goal_after_failed_turn() {
    let config = goal_custom_route_config();
    let engine_config = EngineConfig {
        model: "local-model".to_string(),
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("keep going after dispatch".to_string()),
        ..EngineConfig::default()
    };
    let (mut engine, handle) = Engine::new(engine_config, &config);
    let goal_state = engine.config.goal_state.clone();
    assert!(engine.current_runtime_route().is_ok());
    // Exercise the continuation caller's `false` boundary deterministically:
    // the route installs, but the injected-model authority has no client with
    // which to start the request.
    engine.model_client_injected = true;
    engine.model_client = None;
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::ContinueGoal {
            dynamic_tools: Vec::new(),
            engine_schedule_id: None,
        })
        .await
        .expect("queue rejected continuation");
    let session = handle
        .get_session_snapshot()
        .await
        .expect("post-rejection session snapshot");

    let prompt = match session.system_prompt.expect("blocked system prompt") {
        SystemPrompt::Text(text) => text,
        SystemPrompt::Blocks(blocks) => blocks
            .into_iter()
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n"),
    };
    assert!(!prompt.contains("<session_goal>"), "{prompt}");
    let snapshot = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(snapshot.status, "blocked");
    assert!(
        snapshot
            .blocker
            .as_deref()
            .is_some_and(|blocker| blocker.contains("next model turn could not be started")),
        "{snapshot:?}"
    );

    let mut starts = 0;
    let mut saw_failed_turn = false;
    let mut saw_dispatch_error = false;
    let mut saw_blocked_session = false;
    let mut saw_blocked_goal = false;
    let mut saw_blocked_status = false;
    {
        let mut events = handle.rx_event.write().await;
        while let Ok(event) = events.try_recv() {
            match event {
                Event::TurnStarted { .. } => starts += 1,
                Event::TurnComplete { status, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Failed);
                    saw_failed_turn = true;
                }
                Event::Error { .. } => saw_dispatch_error = true,
                Event::SessionUpdated {
                    system_prompt: Some(system_prompt),
                    ..
                } => {
                    let prompt = match system_prompt {
                        SystemPrompt::Text(text) => text,
                        SystemPrompt::Blocks(blocks) => blocks
                            .into_iter()
                            .map(|block| block.text)
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    assert!(!prompt.contains("<session_goal>"), "{prompt}");
                    saw_blocked_session = true;
                }
                Event::GoalUpdated { snapshot } if snapshot.status == "blocked" => {
                    saw_blocked_goal = true;
                }
                Event::Status { message }
                    if message.contains("next model turn could not be started") =>
                {
                    assert!(message.contains("resume the goal"), "{message}");
                    saw_blocked_status = true;
                }
                _ => {}
            }
        }
    }
    assert_eq!(
        starts, 1,
        "dispatch reached exactly one engine turn boundary"
    );
    assert!(
        saw_failed_turn,
        "rejected dispatch must surface a failed turn"
    );
    assert!(
        saw_dispatch_error,
        "rejected dispatch must surface its error"
    );
    assert!(
        saw_blocked_session,
        "session prompt projection must refresh"
    );
    assert!(saw_blocked_goal, "sidebar must receive blocked state");
    assert!(saw_blocked_status, "blocked reason must remain visible");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn started_nonretryable_continuation_failure_blocks_goal_with_bounded_reason() {
    let failure_marker = "HTTP 400 Bad Request: deterministic continuation failure";
    let leaked_secret = "sk-goal-secret-sentinel-123456";
    let failure_message = format!(
        "{failure_marker}: {leaked_secret} {}",
        "provider detail ".repeat(80)
    );
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: failure_message.clone(),
    });
    let config = goal_custom_route_config();
    let engine_config = EngineConfig {
        model: "local-model".to_string(),
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("block a failed continuation truthfully".to_string()),
        ..EngineConfig::default()
    };
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    let goal_state = engine.config.goal_state.clone();
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::ContinueGoal {
            dynamic_tools: Vec::new(),
            engine_schedule_id: None,
        })
        .await
        .expect("queue failing continuation");
    let session = tokio::time::timeout(model_turn_event_timeout(), handle.get_session_snapshot())
        .await
        .expect("failed continuation did not terminalize")
        .expect("post-failure session snapshot");

    let prompt = match session.system_prompt.expect("blocked system prompt") {
        SystemPrompt::Text(text) => text,
        SystemPrompt::Blocks(blocks) => blocks
            .into_iter()
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n"),
    };
    assert!(!prompt.contains("<session_goal>"), "{prompt}");
    let snapshot = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(snapshot.status, "blocked");
    let blocker = snapshot.blocker.as_deref().expect("failure blocker");
    assert!(blocker.contains(failure_marker), "{blocker}");
    assert!(blocker.contains("resume the goal"), "{blocker}");
    assert!(!blocker.contains(leaked_secret), "{blocker}");
    assert!(
        blocker.contains(codewhale_config::persistence::REDACTED),
        "{blocker}"
    );
    assert!(
        blocker.len() <= GOAL_CONTINUATION_FAILURE_DETAIL_MAX_BYTES + 160,
        "failure reason must remain bounded: {} bytes",
        blocker.len()
    );
    assert!(
        blocker.len() < failure_message.len(),
        "long provider detail must be truncated"
    );
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a nonretryable started failure must not dispatch again"
    );

    let mut starts = 0;
    let mut saw_failed_turn = false;
    let mut saw_provider_error = false;
    let mut saw_blocked_goal = false;
    let mut saw_blocked_status = false;
    {
        let mut events = handle.rx_event.write().await;
        while let Ok(event) = events.try_recv() {
            match event {
                Event::TurnStarted { .. } => starts += 1,
                Event::TurnComplete { status, error, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Failed);
                    assert!(
                        error
                            .as_deref()
                            .is_some_and(|message| message.contains(failure_marker)),
                        "{error:?}"
                    );
                    saw_failed_turn = true;
                }
                Event::Error { envelope, .. } => {
                    if envelope.message.contains(failure_marker) {
                        saw_provider_error = true;
                    }
                }
                Event::GoalUpdated { snapshot } if snapshot.status == "blocked" => {
                    saw_blocked_goal = true;
                }
                Event::Status { message } if message.contains(failure_marker) => {
                    assert!(message.contains("resume the goal"), "{message}");
                    saw_blocked_status = true;
                }
                _ => {}
            }
        }
    }
    assert_eq!(starts, 1, "exactly one continuation turn must start");
    assert!(saw_failed_turn, "failed turn receipt must remain visible");
    assert!(
        saw_provider_error,
        "provider error event must remain visible"
    );
    assert!(saw_blocked_goal, "goal must publish its blocked snapshot");
    assert!(saw_blocked_status, "bounded failure must remain visible");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn headless_host_drains_existing_engine_completion_inbox_before_exit() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().unwrap();
    let config = goal_custom_route_config();
    let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "child evidence integrated by the existing Engine",
    )]));
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".into(),
            terminal_chrome_enabled: false,
            ..deterministic_engine_config(workspace.path())
        },
        &config,
        mock.clone(),
    );
    assert!(engine.subagent_settlement_snapshot().await.is_settled());
    // Reproduce the host boundary: the parent already ended, and a terminal
    // child's receipt is waiting for the Engine's normal idle fan-in path.
    engine
        .tx_event
        .send(Event::TurnComplete {
            usage: Usage::default(),
            parent_route_usage: Usage::default(),
            routed_usage_dropped_records: 0,
            status: TurnOutcomeStatus::Completed,
            error: None,
            tool_catalog: None,
            base_url: None,
        })
        .await
        .unwrap();
    engine
        .tx_subagent_completion
        .try_send(SubAgentCompletion {
            owner_session_id: engine.session.id.clone(),
            agent_id: "headless-settled-child".into(),
            payload: "bounded local fixture evidence".into(),
        })
        .unwrap();
    let pending = engine.subagent_settlement_snapshot().await;
    assert_eq!(pending.running_children, 0);
    assert_eq!(pending.pending_completions, 1);
    assert!(
        !pending.is_settled(),
        "terminal child alone cannot release the host"
    );

    let run = tokio::spawn(engine.run());
    let mut events = crate::exec_agent::ExecAgentEvents::new(
        handle.clone(),
        Instant::now() + model_turn_event_timeout(),
    );
    let mut content = String::new();
    let mut starts = 0;
    tokio::time::timeout(model_turn_event_timeout(), async {
        loop {
            match events.next().await.expect("host event") {
                Event::TurnStarted { .. } => starts += 1,
                Event::MessageDelta { content: delta, .. } => content.push_str(&delta),
                Event::TurnComplete { status, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Completed);
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("bounded headless settlement");
    assert_eq!(starts, 1, "fan-in uses exactly one existing Engine turn");
    assert_eq!(mock.call_count(), 1);
    assert!(content.contains("child evidence integrated"));
    assert!(!handle.is_cancelled());
    handle.send(Op::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), run)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn host_managed_engine_does_not_self_dispatch_goal_continuation() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let mut custom = HashMap::new();
    custom.insert(
        "custom-a".to_string(),
        crate::config::ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:18181/v1".to_string()),
            model: Some("local-model".to_string()),
            api_key: Some("local-test-key".to_string()),
            ..crate::config::ProviderConfig::default()
        },
    );
    let config = Config {
        provider: Some("custom-a".to_string()),
        providers: Some(crate::config::ProvidersConfig {
            custom,
            ..crate::config::ProvidersConfig::default()
        }),
        ..Config::default()
    };
    let runtime_services = crate::tools::spec::RuntimeToolServices {
        active_thread_id: Some("thr_host_managed".to_string()),
        ..crate::tools::spec::RuntimeToolServices::default()
    };
    let engine_config = EngineConfig {
        max_steps: 0,
        snapshots_enabled: false,
        terminal_chrome_enabled: false,
        goal_objective: Some("keep going".to_string()),
        runtime_services,
        ..EngineConfig::default()
    };
    let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "host-managed turn complete",
    )]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "one host-owned turn".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&config, "local-model"),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: Some("keep going".to_string()),
            goal_token_budget: None,
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
        .expect("send host-owned goal turn");

    let mut starts = 0;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("host engine event timeout")
        .expect("host engine event");
        match event {
            Event::TurnStarted { .. } => starts += 1,
            Event::TurnComplete { .. } => break,
            _ => {}
        }
    }
    assert_eq!(starts, 1);
    assert_eq!(
        mock.call_count(),
        1,
        "the host-owned turn runs exactly once"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .is_err(),
        "a hosted engine must wait for an explicit durable turn claim"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

#[tokio::test]
async fn cancellation_during_blocked_idle_handoff_survives_turn_admission() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    for child_completion in [true, false] {
        let workspace = tempdir().unwrap();
        let config = goal_custom_route_config();
        let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
            "must not dispatch after cancellation",
        )]));
        let (mut engine, mut handle) = Engine::new_with_model_client(
            EngineConfig {
                model: "local-model".into(),
                terminal_chrome_enabled: false,
                ..deterministic_engine_config(workspace.path())
            },
            &config,
            mock.clone(),
        );
        // Force the handoff to stop at its status send after its initial
        // cancellation check and before handle_send_message admits a turn.
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        engine.tx_event = tx;
        handle.rx_event = Arc::new(RwLock::new(rx));
        engine.tx_event.send(Event::status("full")).await.unwrap();
        let completion = SubAgentCompletion {
            owner_session_id: engine.session.id.clone(),
            agent_id: "cancel-race-child".into(),
            payload: "retained-after-cancel-race".into(),
        };
        let mut wake: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> =
            if child_completion {
                Box::pin(engine.handle_idle_subagent_completion(completion))
            } else {
                Box::pin(engine.handle_idle_shell_completion_wake())
            };
        assert!(
            tokio::time::timeout(Duration::from_millis(5), &mut wake)
                .await
                .is_err(),
            "the full event channel must hold the handoff before admission"
        );
        handle.cancel_with_reason(CancelReason::External);
        handle.rx_event.write().await.try_recv().unwrap();
        tokio::time::timeout(Duration::from_secs(2), &mut wake)
            .await
            .expect("cancelled handoff returns without a provider call");
        drop(wake);
        assert_eq!(mock.call_count(), 0);
        assert!(handle.is_cancelled());
        assert!(engine.delivered_subagent_completion_ids.is_empty());
        assert_eq!(
            engine.rx_subagent_completion.len(),
            usize::from(child_completion)
        );
        if child_completion {
            assert!(
                engine
                    .rx_subagent_completion
                    .try_recv()
                    .unwrap()
                    .payload
                    .contains("retained-after-cancel-race")
            );
        }
        // A new explicit user action still receives a fresh turn control.
        let _turn = engine.begin_turn_control();
        assert!(!handle.is_cancelled());
        let existing_child = engine.cancel_token.child_token();
        drop(_turn);
        let _automatic =
            engine.begin_turn_control_for_provenance(UserInputProvenance::SubAgentHandoff);
        handle.cancel();
        assert!(
            existing_child.is_cancelled(),
            "stopping an automatic continuation must also stop earlier request siblings"
        );
    }
}

#[tokio::test]
async fn cancelled_parent_defers_child_receipts_until_an_explicit_turn() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    for reason in [CancelReason::User, CancelReason::External] {
        let workspace = tempdir().unwrap();
        let config = Config::default();
        let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
            "Explicit continuation completed.",
        )]));
        let (mut engine, handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &config,
            mock.clone(),
        );
        handle.cancel_with_reason(reason);
        // Exercise a completion selected just before cancellation arrived.
        engine
            .handle_idle_subagent_completion(SubAgentCompletion {
                owner_session_id: engine.session.id.clone(),
                agent_id: "cancelled-worker".into(),
                payload: "parked-child-evidence".into(),
            })
            .await;
        assert_eq!(
            mock.call_count(),
            0,
            "cancellation must forbid a model wake"
        );
        assert!(engine.delivered_subagent_completion_ids.is_empty());
        assert_eq!(engine.rx_subagent_completion.len(), 1);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), engine.next_run_input(false))
                .await
                .is_err(),
            "the idle loop must leave the receipt queued without spinning"
        );

        let run = tokio::spawn(engine.run());
        handle
            .send(external_user_message_op(
                "Continue explicitly",
                AppMode::Agent,
                &config,
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(event) = handle.rx_event.write().await.recv().await {
                if matches!(event, Event::TurnComplete { .. }) {
                    break;
                }
            }
        })
        .await
        .expect("explicit turn completes");
        assert_eq!(mock.call_count(), 1);
        let snapshot = handle.get_session_snapshot().await.unwrap();
        assert!(
            serde_json::to_string(&snapshot.messages)
                .unwrap()
                .contains("parked-child-evidence"),
            "the next explicit turn must retain the completion receipt"
        );
        handle.send(Op::Shutdown).await.unwrap();
        run.await.unwrap();
    }
}
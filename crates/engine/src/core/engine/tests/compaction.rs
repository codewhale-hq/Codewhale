use super::*;

#[tokio::test]
async fn rejected_manual_compaction_route_closes_typed_lifecycle() {
    let _env_lock = lock_test_env();
    let _api_key = EnvVarGuard::remove("DEEPSEEK_API_KEY");
    let route_config = {
        let __cw_config_init_0_0 = Some("deepseek".to_string());
        let __cw_config_init_0_1 =
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
        let mut __cw_config_init_0_value: Config = Config::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.default_text_model = __cw_config_init_0_1;
        __cw_config_init_0_value
    }
    .with_legacy_root(Some(String::new()), None);
    let route = resolve_runtime_route(
        &route_config,
        ProviderKind::Deepseek,
        Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
    )
    .expect("structurally resolve route without credential");
    assert!(
        route.clone().validate().is_err(),
        "fixture must fail at engine route installation"
    );
    let (mut engine, handle) = Engine::new(EngineConfig::default(), &route_config);

    engine
        .handle_manual_compaction_op(
            "compact-route-invalid".to_string(),
            route,
            CompactionConfig::default(),
        )
        .await;

    let mut started_id = None;
    let mut failed_id = None;
    let mut order = Vec::new();
    let mut events = handle.rx_event.write().await;
    while let Ok(event) = events.try_recv() {
        match event {
            Event::CompactionStarted { id, auto, .. } => {
                assert!(!auto);
                started_id = Some(id);
                order.push("started");
            }
            Event::CompactionFailed { id, auto, message } => {
                assert!(!auto);
                assert!(message.contains("provider route is not ready"));
                failed_id = Some(id);
                order.push("failed");
            }
            Event::Error { .. } => order.push("error"),
            Event::TurnComplete {
                turn_id,
                submission_id,
                compaction_id,
                status,
                ..
            } => {
                assert!(turn_id.is_none());
                assert!(submission_id.is_none());
                assert_eq!(compaction_id.as_deref(), Some("compact-route-invalid"));
                assert_eq!(status, TurnOutcomeStatus::Failed);
                order.push("terminal");
            }
            _ => {}
        }
    }
    assert_eq!(order, ["started", "failed", "error", "terminal"]);
    assert_eq!(started_id, failed_id);
}

#[tokio::test]
async fn queued_manual_compaction_cancellation_is_idempotent_and_skips_route_activation() {
    let _env_lock = lock_test_env();
    let _api_key = EnvVarGuard::remove("DEEPSEEK_API_KEY");
    let route_config = {
        let __cw_config_init_1_0 = Some("deepseek".to_string());
        let __cw_config_init_1_1 =
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
        let mut __cw_config_init_1_value: Config = Config::default();
        __cw_config_init_1_value.provider = __cw_config_init_1_0;
        __cw_config_init_1_value.default_text_model = __cw_config_init_1_1;
        __cw_config_init_1_value
    }
    .with_legacy_root(Some(String::new()), None);
    let route = resolve_runtime_route(
        &route_config,
        ProviderKind::Deepseek,
        Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
    )
    .expect("structurally resolve route without credential");
    let (mut engine, handle) = Engine::new(EngineConfig::default(), &route_config);
    let id = "compact-cancel-before-start";

    handle.cancel_compaction(id).expect("first cancel accepted");
    handle
        .cancel_compaction(id)
        .expect("replayed cancel remains idempotent");
    engine
        .handle_manual_compaction_op(id.to_string(), route, CompactionConfig::default())
        .await;

    let mut events = handle.rx_event.write().await;
    let drained = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
    assert!(matches!(
        drained.as_slice(),
        [
            Event::CompactionStarted { id: started, auto: false, .. },
            Event::CompactionCancelled { id: cancelled, auto: false, .. },
            Event::TurnComplete { turn_id: None, submission_id: None,
                compaction_id: Some(terminal), status: TurnOutcomeStatus::Interrupted, .. }
        ] if started == id && cancelled == id && terminal == id
    ));
    assert!(
        !drained
            .iter()
            .any(|event| matches!(event, Event::Error { .. })),
        "pre-start cancellation must not activate or validate the provider route"
    );

    let retry = engine
        .claim_compaction(id)
        .expect("the same stable id can be retried after terminal settlement");
    assert!(!retry.is_cancelled());
    handle
        .cancel_compaction(id)
        .expect("running cancel accepted");
    assert!(
        retry.is_cancelled(),
        "running cancellation reaches its token"
    );
    engine.finish_compaction(id);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn terminal_correlation_preflight_failures_keep_each_submitted_owner() {
    let _env_lock = lock_test_env();
    let _api_key = EnvVarGuard::remove("DEEPSEEK_API_KEY");
    let workspace = tempdir().expect("isolated workspace");
    let route_config = {
        let __cw_config_init_2_0 = Some("deepseek".to_string());
        let __cw_config_init_2_1 =
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
        let mut __cw_config_init_2_value: Config = Config::default();
        __cw_config_init_2_value.provider = __cw_config_init_2_0;
        __cw_config_init_2_value.default_text_model = __cw_config_init_2_1;
        __cw_config_init_2_value
    }
    .with_legacy_root(Some(String::new()), None);
    let invalid_route = resolve_runtime_route(
        &route_config,
        ProviderKind::Deepseek,
        Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
    )
    .expect("structurally resolved route");
    assert!(
        invalid_route.clone().validate().is_err(),
        "fixture must fail at engine route installation"
    );
    let (engine, handle) =
        Engine::new(deterministic_engine_config(workspace.path()), &route_config);
    let task = tokio::spawn(engine.run());
    for submission in [None, Some("sub-route-rejected")] {
        let mut op = external_user_message_op("invalid route", AppMode::Agent, &Config::default());
        if let Op::SendMessage(TurnSpec {
            route,
            provenance,
            submission_id,
            ..
        }) = &mut op
        {
            **route = invalid_route.clone();
            *submission_id = submission.map(str::to_string);
            if submission.is_none() {
                *provenance = UserInputProvenance::Runtime;
            }
        }
        handle.send(op).await.expect("queue route rejection");
    }
    handle
        .send(Op::EditLastTurn {
            new_message: "rejected edit".into(),
            submission_id: Some("sub-edit-rejected".into()),
        })
        .await
        .expect("queue rejected edit");
    handle
        .send(Op::CompactContext {
            id: "compact-after-rejections".into(),
            route: Box::new(invalid_route),
            compaction: Box::new(CompactionConfig::default()),
        })
        .await
        .expect("queue compaction rejection");
    let expected = [
        (None, None),
        (Some("sub-route-rejected"), None),
        (Some("sub-edit-rejected"), None),
        (None, Some("compact-after-rejections")),
    ];
    let mut terminal_count = 0;
    {
        let mut rx = handle.rx_event.write().await;
        while terminal_count < expected.len() {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("every rejected operation must settle")
                .expect("engine event");
            match event {
                Event::TurnStarted { .. } => panic!("preflight rejections must not start turns"),
                Event::TurnComplete {
                    turn_id,
                    submission_id,
                    compaction_id,
                    status,
                    error,
                    ..
                } => {
                    assert!(turn_id.is_none());
                    assert_eq!(
                        (submission_id.as_deref(), compaction_id.as_deref()),
                        expected[terminal_count]
                    );
                    assert_eq!(status, TurnOutcomeStatus::Failed);
                    assert!(error.is_some());
                    terminal_count += 1;
                }
                _ => {}
            }
        }
    }
    handle.send(Op::Shutdown).await.expect("shutdown");
    task.await.expect("engine task");
    let mut rx = handle.rx_event.write().await;
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok()).any(|event| matches!(
            event,
            Event::TurnStarted { .. } | Event::TurnComplete { .. }
        )),
        "each rejected operation has exactly one terminal and no provider turn"
    );
}

#[tokio::test]
async fn terminal_correlation_started_failure_matches_its_original_submission() {
    let workspace = tempdir().expect("isolated workspace");
    let model = std::sync::Arc::new(FailingGoalModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        message: "deterministic terminal correlation failure".into(),
    });
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op_with_submission(
            "fail this turn",
            AppMode::Agent,
            &Config::default(),
            Some("sub-started-failure".into()),
        ))
        .await
        .expect("submit");
    let mut started = None;
    {
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("failed model turn must settle")
                .expect("engine event");
            match event {
                Event::TurnStarted {
                    turn_id,
                    submission_id,
                    ..
                } => {
                    assert!(started.is_none(), "exactly one start");
                    assert_eq!(submission_id.as_deref(), Some("sub-started-failure"));
                    started = Some(turn_id);
                }
                Event::TurnComplete {
                    turn_id,
                    submission_id,
                    compaction_id,
                    status,
                    error,
                    ..
                } => {
                    assert!(started.is_some());
                    assert_eq!(turn_id, started);
                    assert_eq!(submission_id.as_deref(), Some("sub-started-failure"));
                    assert!(compaction_id.is_none());
                    assert_eq!(status, TurnOutcomeStatus::Failed);
                    assert!(
                        error
                            .as_deref()
                            .is_some_and(|s| s.contains("terminal correlation failure"))
                    );
                    break;
                }
                _ => {}
            }
        }
    }
    handle.send(Op::Shutdown).await.expect("shutdown");
    task.await.expect("engine task");
    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let mut rx = handle.rx_event.write().await;
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok()).any(|event| matches!(
            event,
            Event::TurnStarted { .. } | Event::TurnComplete { .. }
        )),
        "a model failure must not produce a second terminal"
    );
}

struct BlockingEmergencyCompactionModelClient {
    entered: std::sync::Arc<tokio::sync::Notify>,
    request_dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[tokio::test]
async fn failed_emergency_compaction_preserves_history_instead_of_trimming() {
    use crate::llm_client::mock::MockLlmClient;
    let _env_lock = lock_test_env();
    let workspace = tempdir().unwrap();
    // Emergency compaction persists a checkpoint before calling the model.
    // Keep that prerequisite away from other tests' shared state fixtures so
    // this exercises a summary failure, rather than an unrelated write failure.
    let _home = EnvVarGuard::set("CODEWHALE_HOME", workspace.path());
    let (mut engine, handle) = Engine::new(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
    );
    engine.session.messages = (0..12)
        .map(|i| Message {
            role: if i % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: vec![ContentBlock::Text {
                text: format!(
                    "must preserve instruction and evidence {i}: {}",
                    "x".repeat(20_000)
                ),
                cache_control: None,
            }],
        })
        .collect::<Vec<_>>()
        .into();
    let before = engine.session.messages.clone();
    let summary = engine.session.compaction_summary_prompt.clone();
    let client = MockLlmClient::new(Vec::new());
    let tools = vec![catalog_tool("read")];
    let mut turn = TurnContext::new(1);
    assert!(
        !engine
            .recover_context_overflow(
                &client,
                Some(&tools),
                "provider rejection fixture",
                &mut turn
            )
            .await
    );
    assert_eq!(engine.session.messages.as_slice(), before.as_slice());
    assert_eq!(engine.session.compaction_summary_prompt, summary);
    let mut events = handle.rx_event.write().await;
    let drained = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
    assert_eq!(
        client.call_count(),
        1,
        "a deterministic summary failure is not retried unchanged: {drained:?}"
    );
    let requests = client.captured_requests();
    assert_eq!(requests[0].tools.as_deref(), Some(tools.as_slice()));
    assert_eq!(requests[0].tool_choice, Some(json!("none")));
    assert_eq!(requests[0].system, engine.session.system_prompt);
}

#[tokio::test]
async fn manual_compaction_accounts_accepted_and_rejected_responses_once() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _env_lock = lock_test_env();
    let _cost_scope = crate::cost_status::test_scope();
    let workspace = tempdir().expect("isolated compaction workspace");
    let _home = EnvVarGuard::set("CODEWHALE_HOME", workspace.path());
    for (finish_reason, expected_status) in [
        ("stop", TurnOutcomeStatus::Completed),
        ("length", TurnOutcomeStatus::Failed),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": format!("compaction-{finish_reason}"),
                "object": "chat.completion",
                "model": codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL,
                "choices": [{
                    "index": 0,
                    "message": { "role": "assistant", "content": "Primary request: preserve the session migration. Completed: inspected the existing store. Constraints: keep every user message and failing test. Next: finish the transactional migration and rerun session_store::roundtrip." },
                    "finish_reason": finish_reason,
                }],
                "usage": { "prompt_tokens": 41, "completion_tokens": 7, "total_tokens": 48 },
            })))
            .expect(1)
            .mount(&server)
            .await;
        let route_config = {
            let __cw_config_init_3_0 = Some("deepseek".to_string());
            let mut __cw_config_init_3_value: Config = Config::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value
        }
        .with_legacy_root(
            Some("fixture-key".to_string()),
            Some(format!("{}/v1", server.uri())),
        );
        let (mut engine, handle) = Engine::new(
            EngineConfig {
                workspace: workspace.path().to_path_buf(),
                snapshots_enabled: false,
                subagents_enabled: false,
                ..EngineConfig::default()
            },
            &route_config,
        );
        engine.session.messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Preserve the transactional session migration.".to_string(),
                cache_control: None,
            }],
        });
        engine.config.goal_state.lock().unwrap().replace(
            "Finish the session migration",
            Some(1000),
            None,
        );
        engine
            .handle_manual_compaction("compact-accounting".to_string(), CancellationToken::new())
            .await;

        assert_eq!(engine.session.total_usage.input_tokens, 41);
        assert_eq!(engine.session.total_usage.output_tokens, 7);
        assert_eq!(
            engine
                .config
                .goal_state
                .lock()
                .unwrap()
                .snapshot()
                .tokens_used,
            48
        );
        let mut events = handle.rx_event.write().await;
        let mut telemetry_count = 0;
        let mut terminal_count = 0;
        while let Ok(event) = events.try_recv() {
            match event {
                Event::RoutedTurnUsage { usage, .. } => {
                    telemetry_count += 1;
                    assert_eq!((usage.input_tokens, usage.output_tokens), (41, 7));
                }
                Event::TurnComplete {
                    usage,
                    parent_route_usage,
                    status,
                    ..
                } => {
                    terminal_count += 1;
                    assert_eq!((usage.input_tokens, usage.output_tokens), (41, 7));
                    assert_eq!(parent_route_usage, Usage::default());
                    assert_eq!(status, expected_status);
                }
                _ => {}
            }
        }
        assert_eq!((telemetry_count, terminal_count), (1, 1));
    }
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for BlockingEmergencyCompactionModelClient {
    fn provider_name(&self) -> &str {
        "deepseek"
    }

    fn model(&self) -> &str {
        codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        let _drop_signal = DropSignal(std::sync::Arc::clone(&self.request_dropped));
        self.entered.notify_one();
        std::future::pending().await
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        anyhow::bail!("emergency compaction uses the non-streaming model boundary")
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[tokio::test]
async fn emergency_compaction_cancellation_drops_provider_and_never_mutates_context() {
    let route_config = {
        let __cw_config_init_4_0 = Some("deepseek".to_string());
        let __cw_config_init_4_1 =
            Some(codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string());
        let mut __cw_config_init_4_value: Config = Config::default();
        __cw_config_init_4_value.provider = __cw_config_init_4_0;
        __cw_config_init_4_value.default_text_model = __cw_config_init_4_1;
        __cw_config_init_4_value
    };
    let (mut engine, handle) = Engine::new(EngineConfig::default(), &route_config);
    engine.session.messages = (0..8)
        .map(|index| Message {
            role: if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            },
            content: vec![ContentBlock::Text {
                text: format!("preserve emergency context item {index}"),
                cache_control: None,
            }],
        })
        .collect::<Vec<_>>()
        .into();
    let messages_before = engine.session.messages.clone();
    let checkpoint_before = engine.session.compaction_summary_prompt.clone();
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let request_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = std::sync::Arc::new(BlockingEmergencyCompactionModelClient {
        entered: std::sync::Arc::clone(&entered),
        request_dropped: std::sync::Arc::clone(&request_dropped),
    });

    let recovery = tokio::spawn(async move {
        let mut turn = TurnContext::new(1);
        let recovered = engine
            .recover_context_overflow(client.as_ref(), None, "cancellation regression", &mut turn)
            .await;
        (engine, recovered)
    });

    let started_id = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = handle
                .rx_event
                .write()
                .await
                .recv()
                .await
                .expect("emergency compaction start event");
            if let Event::CompactionStarted { id, auto: true, .. } = event {
                break id;
            }
        }
    })
    .await
    .expect("emergency compaction publishes its stable id");
    tokio::time::timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("emergency provider request starts");

    handle
        .cancel_compaction(started_id.clone())
        .expect("exact emergency cancellation accepted");
    let (engine, recovered) = tokio::time::timeout(Duration::from_secs(1), recovery)
        .await
        .expect("emergency cancellation settles promptly")
        .expect("recovery task");

    assert!(!recovered);
    assert_eq!(&*engine.session.messages, &*messages_before);
    assert_eq!(engine.session.compaction_summary_prompt, checkpoint_before);
    assert!(
        request_dropped.load(std::sync::atomic::Ordering::SeqCst),
        "cancellation must drop the in-flight provider future"
    );

    let mut events = handle.rx_event.write().await;
    let drained = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
    assert!(matches!(
        drained.as_slice(),
        [Event::CompactionCancelled { id, auto: true, .. }] if id == &started_id
    ));
    assert!(
        !drained.iter().any(|event| matches!(
            event,
            Event::CompactionCompleted { .. } | Event::CompactionFailed { .. }
        )),
        "a canceled emergency pass must have one canceled terminal event"
    );
}

/// Experience mark 2: a one-message conversation has nothing to summarize.
/// Emergency recovery must not start a pass (no spinner, no model call)
/// before the failure the caller reports anyway.
#[tokio::test]
async fn emergency_recovery_preserves_history_when_disabled_or_unnecessary() {
    use crate::llm_client::mock::MockLlmClient;
    let _env_lock = lock_test_env();
    let workspace = tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", workspace.path());
    for automatic in [true, false] {
        let mut config = deterministic_engine_config(workspace.path());
        config.compaction.enabled = automatic;
        let (mut engine, handle) = Engine::new(config, &Config::default());
        let count = if automatic { 1 } else { 8 };
        engine.session.messages = (0..count)
            .map(|index| Message {
                role: if index % 2 == 0 {
                    Role::User
                } else {
                    Role::Assistant
                },
                content: vec![ContentBlock::Text {
                    text: format!("Original conversation step {index}"),
                    cache_control: None,
                }],
            })
            .collect::<Vec<_>>()
            .into();
        assert_eq!(
            crate::compaction::has_compactable_history(&engine.session.messages),
            !automatic
        );
        let original = engine.session.messages.snapshot();
        let client = MockLlmClient::new(Vec::new());
        let mut turn = TurnContext::new(1);
        assert!(
            !engine
                .recover_context_overflow(&client, None, "preflight token budget", &mut turn)
                .await
        );
        assert_eq!(client.call_count(), 0);
        assert_eq!(turn.stop_diagnostics.emergency_compaction_attempts, 0);
        assert_eq!(engine.session.messages.snapshot(), original);
        if !automatic {
            assert!(
                turn.context_recovery_rejection
                    .as_ref()
                    .is_some_and(|error| matches!(
                        error.downcast_ref::<crate::llm_client::LlmError>(),
                        Some(crate::llm_client::LlmError::ContextLengthError(_))
                    ))
            );
            let message = engine
                .report_context_recovery_failure(&mut turn)
                .await
                .expect("disabled-setting explanation");
            assert!(
                message.contains("Automatic compaction is disabled"),
                "{message}"
            );
            assert!(
                engine
                    .report_context_recovery_failure(&mut turn)
                    .await
                    .is_none()
            );
        } else {
            assert!(turn.context_recovery_rejection.is_none());
        }
        let mut events = handle.rx_event.write().await;
        let drained = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            !drained.iter().any(|event| matches!(
                event,
                Event::CompactionStarted { .. } | Event::CompactionFailed { .. }
            )),
            "{drained:?}"
        );
        assert_eq!(
            drained
                .iter()
                .filter(|event| matches!(event, Event::Error { .. }))
                .count(),
            usize::from(!automatic)
        );
    }
}

#[test]
fn recovery_failures_from_the_provider_are_told_apart_from_budget_failures() {
    use super::super::compaction::is_provider_rejection;
    use crate::llm_client::LlmError;
    assert!(is_provider_rejection(&anyhow::Error::new(
        LlmError::ModelError("\"nomic-embed-text:latest\" does not support chat".to_string())
    )));
    assert!(is_provider_rejection(&anyhow::anyhow!(
        "connection refused while contacting http://localhost:11434"
    )));
    assert!(!is_provider_rejection(&anyhow::Error::new(
        LlmError::ContextLengthError("prompt is too long".to_string())
    )));
    assert!(!is_provider_rejection(&anyhow::anyhow!(
        "Making room did not shrink the context; the original conversation was preserved."
    )));
}

#[test]
fn a_request_that_cannot_fit_names_the_cause_and_one_next_step() {
    use super::super::context::context_does_not_fit_message;
    let embed =
        context_does_not_fit_message(true, true, "nomic-embed-text:latest", 5_200, 1_500, 5_100);
    assert_eq!(
        embed,
        "nomic-embed-text:latest can't chat. Pick a chat model: /model."
    );
    let window = context_does_not_fit_message(true, true, "qwen3:4b", 5_300, 3_000, 5_100);
    assert!(
        window.contains("qwen3:4b's context window (~3000 tokens usable)"),
        "{window}"
    );
    assert!(
        window.contains("working instructions (~5100 tokens)"),
        "{window}"
    );
    assert!(window.ends_with("raise num_ctx: /model."), "{window}");
    assert!(!window.contains("compaction"), "{window}");
    let message = context_does_not_fit_message(false, false, "small-model", 9_000, 6_000, 2_000);
    assert!(
        message.contains("there is not enough earlier conversation to summarize"),
        "{message}"
    );
    assert!(message.ends_with("choose a larger model."), "{message}");
    assert!(
        !message.contains("/model"),
        "headless has no command layer: {message}"
    );
}

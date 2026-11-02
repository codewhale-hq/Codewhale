const WORKING_SET_SUMMARY_MARKER: &str = "## Repo Working Set";

#[tokio::test]
async fn event_capacity_cancel_before_admission_has_no_started_turn_and_keeps_classifier_cost() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    let _cost = crate::cost_status::test_scope();
    let workspace = tempdir().unwrap();
    let config = Config::default();
    let mock = Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "must not run",
    )]));
    let mut engine_config = deterministic_engine_config(workspace.path());
    engine_config.features.disable(Feature::Mcp);
    let (mut engine, handle) = Engine::new_with_model_client(engine_config, &config, mock.clone());
    let (tx, rx) = mpsc::channel(1);
    engine.tx_event = tx;
    let mut handle = handle;
    handle.rx_event = Arc::new(RwLock::new(rx));
    engine
        .tx_event
        .try_send(Event::status("existing idle receipt"))
        .unwrap();
    let mut op = external_user_message_op("not admitted", AppMode::Agent, &config);
    let Op::SendMessage(spec) = &mut op else {
        unreachable!()
    };
    spec.initial_routed_usage
        .records
        .push(crate::cost_status::RuntimeUsageRecord {
            source_id: "event-capacity:classifier".into(),
            usage: crate::cost_status::EffectiveRouteUsage {
                route: crate::cost_status::EffectiveRouteEnvelope::capture(
                    None,
                    ProviderKind::Openai,
                    "openai",
                    "classifier",
                    None,
                    chrono::Utc::now(),
                ),
                usage: Usage {
                    input_tokens: 7,
                    output_tokens: 3,
                    ..Usage::default()
                },
            },
        });
    let controls = Arc::clone(&engine.turn_controls);
    handle.send(op).await.unwrap();
    let task = tokio::spawn(engine.run());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if controls.lock().unwrap().active.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued op enters the existing control scope");
    handle.cancel();
    // The oneshot snapshot can settle while the event receiver remains full.
    let snapshot = tokio::time::timeout(Duration::from_secs(2), handle.get_session_snapshot())
        .await
        .expect("cancel releases admission reservation")
        .unwrap();
    assert!(
        snapshot.messages.is_empty(),
        "no session mutation before admission"
    );
    assert_eq!(mock.call_count(), 0);
    assert!(controls.lock().unwrap().active.is_none());
    let cost = crate::cost_status::drain();
    assert!(
        cost.usage_source_fingerprints
            .contains(&crate::cost_status::usage_source_fingerprint(
                "event-capacity:classifier"
            ),),
        "already billed classifier work remains accounted"
    );
    let mut events = handle.rx_event.write().await;
    assert!(matches!(events.try_recv(), Ok(Event::Status { .. })));
    assert!(
        events.try_recv().is_err(),
        "unadmitted work has no fabricated terminal event"
    );
    drop(events);
    handle.send(Op::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn event_capacity_admitted_full_channel_settles_once_with_partial_usage_without_drain() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    let _cost = crate::cost_status::test_scope();
    let workspace = tempdir().unwrap();
    let config = Config::default();
    let mock = Arc::new(MockLlmClient::new(Vec::new()));
    let mut engine_config = deterministic_engine_config(workspace.path());
    engine_config.features.disable(Feature::Mcp);
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, mock.clone());
    let tx = engine.tx_event.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let filled = Arc::clone(&entered);
    mock.push_factory(move |_| {
        while tx.try_send(Event::status("backpressure receipt")).is_ok() {}
        filled.notify_one();
        vec![
            canned::message_start("billed-before-cancel"),
            canned::message_delta(
                "end_turn",
                Some(Usage {
                    input_tokens: 11,
                    output_tokens: 5,
                    ..Usage::default()
                }),
            ),
            canned::text_block_start(0),
            canned::text_delta(0, "must not render after cancellation"),
            canned::message_stop(),
        ]
    });
    let controls = Arc::clone(&engine.turn_controls);
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "admitted",
            AppMode::Agent,
            &config,
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    handle.cancel();
    let snapshot = tokio::time::timeout(Duration::from_secs(2), handle.get_session_snapshot())
        .await
        .expect("admitted cancellation settles before any event drain")
        .unwrap();
    assert_eq!(mock.call_count(), 1);
    assert_eq!(
        snapshot.total_tokens, 16,
        "known partial provider usage survives cancellation"
    );
    assert!(controls.lock().unwrap().active.is_none());
    let mut events = handle.rx_event.write().await;
    let mut started = 0;
    let mut completed = 0;
    let mut terminal_was_last = false;
    while let Ok(event) = events.try_recv() {
        assert!(
            !terminal_was_last,
            "completion stays after prior accepted observations"
        );
        match event {
            Event::TurnStarted { .. } => started += 1,
            Event::TurnComplete {
                status,
                usage,
                parent_route_usage,
                error,
                ..
            } => {
                completed += 1;
                terminal_was_last = true;
                assert_eq!(status, TurnOutcomeStatus::Interrupted);
                assert!(
                    error.is_none(),
                    "cancellation is not a provider failure: {error:?}"
                );
                assert_eq!((usage.input_tokens, usage.output_tokens), (11, 5));
                assert_eq!(usage, parent_route_usage);
            }
            Event::MessageDelta { content, .. } => assert!(!content.contains("must not render")),
            _ => {}
        }
    }
    assert_eq!((started, completed), (1, 1));
    assert!(terminal_was_last);
    drop(events);
    handle.send(Op::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn event_capacity_invalid_images_release_queued_control_and_keep_classifier_receipt() {
    use crate::llm_client::mock::MockLlmClient;
    for full in [false, true] {
        let _cost = crate::cost_status::test_scope();
        let workspace = tempdir().unwrap();
        let config = Config::default();
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        let mut engine_config = deterministic_engine_config(workspace.path());
        engine_config.features.disable(Feature::Mcp);
        let (engine, handle) = Engine::new_with_model_client(engine_config, &config, mock.clone());
        if full {
            while engine.tx_event.try_send(Event::status("occupied")).is_ok() {}
        }
        let controls = Arc::clone(&engine.turn_controls);
        let mut op = external_user_message_op("invalid attachment", AppMode::Agent, &config);
        let Op::SendMessage(spec) = &mut op else {
            unreachable!()
        };
        spec.images
            .push(codewhale_protocol::runtime::RuntimeImageInput {
                mime: "image/png".into(),
                data_base64: "invalid@base64".into(),
            });
        spec.initial_routed_usage
            .records
            .push(crate::cost_status::RuntimeUsageRecord {
                source_id: "event-capacity:invalid-image-classifier".into(),
                usage: crate::cost_status::EffectiveRouteUsage {
                    route: crate::cost_status::EffectiveRouteEnvelope::capture(
                        None,
                        ProviderKind::Openai,
                        "openai",
                        "classifier",
                        None,
                        chrono::Utc::now(),
                    ),
                    usage: Usage {
                        input_tokens: 7,
                        output_tokens: 3,
                        ..Usage::default()
                    },
                },
            });
        handle.send(op).await.unwrap();
        let task = tokio::spawn(engine.run());
        if full {
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if controls.lock().unwrap().active.is_some() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            handle.cancel();
        }
        let snapshot = tokio::time::timeout(Duration::from_secs(2), handle.get_session_snapshot())
            .await
            .expect("rejected input releases the current control")
            .unwrap();
        assert!(snapshot.messages.is_empty());
        assert_eq!(mock.call_count(), 0);
        assert!(controls.lock().unwrap().active.is_none());
        let cost = crate::cost_status::drain();
        assert!(cost.usage_source_fingerprints.contains(
            &crate::cost_status::usage_source_fingerprint(
                "event-capacity:invalid-image-classifier"
            ),
        ));
        let mut events = handle.rx_event.write().await;
        let mut invalid_errors = 0;
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(
                event,
                Event::TurnStarted { .. } | Event::TurnComplete { .. }
            ));
            if let Event::Error { envelope, .. } = event {
                assert_eq!(envelope.code, "image_input_invalid");
                invalid_errors += 1;
            }
        }
        assert_eq!(
            invalid_errors,
            usize::from(!full),
            "a cancelled full-channel rejection is not fabricated as delivered"
        );
        drop(events);
        handle.send(Op::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn event_capacity_releases_all_admitted_senders_and_keeps_idle_receipts_lossless() {
    let workspace = tempdir().unwrap();
    let (mut engine, _handle) = Engine::new(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
    );
    let (tx, mut rx) = mpsc::channel(1);
    engine.tx_event = tx;
    engine.tx_event.try_send(Event::status("occupied")).unwrap();
    let turn = engine.begin_turn_control();
    let mut senders =
        Box::pin(futures_util::future::join_all((0..8).map(|_| {
            engine.send_event(Event::status("admitted observation"))
        })));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut senders)
            .await
            .is_err()
    );
    engine.cancel_token.cancel();
    let outcomes = tokio::time::timeout(Duration::from_secs(1), &mut senders)
        .await
        .unwrap();
    assert_eq!(outcomes, vec![Err(streaming::EventSendError::Cancelled); 8]);
    drop(senders);
    assert!(matches!(rx.try_recv(), Ok(Event::Status { .. })));
    // A cancelled turn still delivers its usage/status receipts when they
    // fit immediately. The stream suffix remains strictly cancelled.
    engine
        .send_event(Event::status(
            "cancelled turn receipt with available capacity",
        ))
        .await
        .unwrap();
    assert!(
        matches!(rx.try_recv(), Ok(Event::Status { message, .. }) if message == "cancelled turn receipt with available capacity")
    );
    assert!(
        !engine
            .send_stream_event(Event::status("forbidden stream suffix"))
            .await
    );
    assert!(rx.try_recv().is_err());
    engine
        .tx_event
        .try_send(Event::status("occupied before idle receipt"))
        .unwrap();
    drop(turn);
    let mut idle = Box::pin(engine.send_event(Event::status("idle receipt after cancelled turn")));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut idle)
            .await
            .is_err()
    );
    assert!(matches!(rx.try_recv(), Ok(Event::Status { .. })));
    tokio::time::timeout(Duration::from_secs(1), &mut idle)
        .await
        .unwrap()
        .unwrap();
    drop(idle);
    assert!(
        matches!(rx.try_recv(), Ok(Event::Status { message, .. }) if message == "idle receipt after cancelled turn")
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn event_capacity_parallel_tool_fanout_is_not_capped_per_turn() {
    struct Counter(std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl codewhale_workflow_js::ToolInvoker for Counter {
        async fn invoke(
            &self,
            _: codewhale_workflow_js::ToolCallRequest,
        ) -> Result<codewhale_workflow_js::ToolCallResponse, codewhale_workflow_js::DriverError>
        {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::task::yield_now().await;
            Ok(codewhale_workflow_js::ToolCallResponse {
                ok: true,
                result: json!(null),
            })
        }
    }
    let invoker = Arc::new(Counter(std::sync::atomic::AtomicUsize::new(0)));
    for program in 1..=2 {
        let result = codewhale_workflow_js::WorkflowVm::new().run_tools_script(
            r#"const calls = await Promise.allSettled(Array.from({length:256}, () => tools.call('read', {})));
               return {ok:calls.filter(x=>x.status==='fulfilled').length,
                       rejected:calls.filter(x=>x.status==='rejected' && x.reason.kind==='admission').length};"#,
            json!(null),
            Arc::new(codewhale_workflow_js::testing::FakeDriver::new()),
            invoker.clone(), codewhale_workflow_js::WorkflowRunCancel::new(),
        ).await.unwrap();
        assert_eq!(result, json!({"ok":256,"rejected":0}));
        assert_eq!(
            invoker.0.load(std::sync::atomic::Ordering::SeqCst),
            256 * program
        );
    }
}

#[tokio::test]
async fn event_capacity_admitted_user_shell_cancels_before_side_effect_and_settles_without_drain() {
    let workspace = tempdir().unwrap();
    let marker = workspace.path().join("shell-must-not-start.txt");
    let mut engine_config = deterministic_engine_config(workspace.path());
    engine_config.features.disable(Feature::Mcp);
    let (engine, handle) = Engine::new(engine_config, &Config::default());
    // Leave precisely the two lifecycle slots free. TurnStarted fills one;
    // the reserved terminal owns the other. The next tool observation must
    // wait before the human-provenance command can reach its executor.
    for _ in 0..engine.tx_event.max_capacity() - 2 {
        engine
            .tx_event
            .try_send(Event::status("prior receipt"))
            .unwrap();
    }
    let controls = Arc::clone(&engine.turn_controls);
    handle
        .send(Op::RunShellCommand {
            command: format!("echo must-not-run > \"{}\"", marker.display()),
            mode: AppMode::Agent,
            allow_shell: true,
            trust_mode: true,
            auto_approve: true,
            approval_mode: ApprovalMode::Bypass,
        })
        .await
        .unwrap();
    let task = tokio::spawn(engine.run());
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if controls.lock().unwrap().active.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    handle.cancel();
    tokio::time::timeout(Duration::from_secs(2), handle.get_session_snapshot())
        .await
        .expect("shell cancellation settles before any event drain")
        .unwrap();
    assert!(
        !marker.exists(),
        "cancellation preserves the execution gate"
    );
    assert!(controls.lock().unwrap().active.is_none());
    let mut events = handle.rx_event.write().await;
    let mut started = 0;
    let mut completed = 0;
    let mut terminal_was_last = false;
    while let Ok(event) = events.try_recv() {
        assert!(!terminal_was_last);
        match event {
            Event::TurnStarted { .. } => started += 1,
            Event::TurnComplete { status, usage, .. } => {
                completed += 1;
                terminal_was_last = true;
                assert_eq!(status, TurnOutcomeStatus::Interrupted);
                assert_eq!(usage, Usage::default());
            }
            _ => {}
        }
    }
    assert_eq!((started, completed), (1, 1));
    assert!(terminal_was_last);
    drop(events);
    handle.send(Op::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn event_capacity_cancelled_repl_child_keeps_unknown_cost_and_discards_kernel() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    struct ReplClient {
        inner: MockLlmClient,
        stream_requests: std::sync::atomic::AtomicUsize,
        child_entered: Arc<tokio::sync::Notify>,
        child_dropped: Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait::async_trait]
    impl crate::core::model_client::ModelClient for ReplClient {
        fn provider_name(&self) -> &str {
            "backpressure-repl-fixture"
        }
        fn model(&self) -> &str {
            "mock-model"
        }
        async fn create_message(
            &self,
            _: codewhale_models::MessageRequest,
        ) -> anyhow::Result<codewhale_models::MessageResponse> {
            anyhow::bail!("fixture expects canonical streaming requests")
        }
        async fn create_message_stream(
            &self,
            request: codewhale_models::MessageRequest,
        ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
            if self
                .stream_requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                return crate::core::model_client::ModelClient::create_message_stream(
                    &self.inner,
                    request,
                )
                .await;
            }
            let _drop = DropSignal(Arc::clone(&self.child_dropped));
            self.child_entered.notify_one();
            std::future::pending().await
        }
        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }
    let _cost = crate::cost_status::test_scope();
    let workspace = tempdir().unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut response = canned::simple_text_turn(
        "```repl\nchild = sub_query('hang until cancelled')\nfinalize(child)\n```",
    );
    for event in &mut response {
        if let codewhale_models::StreamEvent::MessageDelta { usage, .. } = event {
            *usage = Some(Usage {
                input_tokens: 11,
                output_tokens: 5,
                ..Usage::default()
            });
        }
    }
    let client = Arc::new(ReplClient {
        inner: MockLlmClient::new(vec![response]),
        stream_requests: std::sync::atomic::AtomicUsize::new(0),
        child_entered: Arc::clone(&entered),
        child_dropped: Arc::clone(&dropped),
    });
    let api_config = rlm_host::fixture_config("mock-model");
    let (mut engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "mock-model".into(),
            ..deterministic_engine_config(workspace.path())
        },
        &api_config,
        client.clone(),
    );
    rlm_host::install_fixture_route(&mut engine);
    engine.session.auto_approve = true;
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "run cancellable REPL".into(),
            cache_control: None,
        }],
    });
    let turn_guard = engine.begin_turn_control();
    let mut turn = TurnContext::new(4);
    let registry = crate::tools::ToolRegistry::new(rlm_host::admitted_context(&engine, &turn.id));
    let policy = test_tool_surface(
        &engine,
        registry,
        Some(vec![catalog_tool(CODE_EXECUTION_TOOL_NAME)]),
        AppMode::Agent,
    );
    let tx = engine.tx_event.clone();
    let mut run = Box::pin(engine.run_turn(&mut turn, policy, None, None));
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            result = &mut run => panic!("REPL did not enter the child provider request: {result:?}"),
            () = entered.notified() => {},
        }
    }).await.expect("actual Python kernel dispatches the pending child request");
    while tx
        .try_send(Event::status("full during nested REPL"))
        .is_ok()
    {}
    handle.cancel();
    let (status, error) = tokio::time::timeout(Duration::from_secs(2), &mut run)
        .await
        .expect("cancel drops the pending REPL round even with a full queue");
    drop(run);
    assert_eq!(status, TurnOutcomeStatus::Interrupted, "{error:?}");
    assert!(error.is_none());
    assert!(
        dropped.load(std::sync::atomic::Ordering::SeqCst),
        "nested provider future is released"
    );
    assert!(
        engine.repl_kernel.is_none(),
        "a cancelled round cannot preserve an executing process"
    );
    assert_eq!((turn.usage.input_tokens, turn.usage.output_tokens), (11, 5));
    let cost = crate::cost_status::drain();
    // The child provider future never returned a response; its canonical
    // dispatch guard records an unknown outcome, not success without usage.
    assert!(
        cost.unpriced_reasons
            .contains(crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown.label()),
        "pending child usage stays unknown, never zero"
    );
    assert_eq!(cost.priced_turns, 0);
    assert_eq!(cost.unpriced_turns, 1);
    assert_eq!(cost.missing_usage_sources.len(), 1);
    assert!(cost.missing_usage_sources.values().all(|coverage| {
        coverage.reason == crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown
            && coverage.money_metered
    }));
    assert!(cost.resolved_missing_usage_sources.is_empty());
    assert_eq!(
        client.inner.call_count(),
        1,
        "no next root provider request after cancellation"
    );
    drop(turn_guard);
}
#[tokio::test]
async fn event_capacity_cancelled_parallel_tool_keeps_completed_span_and_call_when_available() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use crate::tools::spec::{
        ApprovalRequirement, PreparedToolCall, ResourceClaim, ToolCapability, ToolSpec,
    };
    use codewhale_protocol::engine_owner::OwnerOperationOutcome;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CompleteThenCancel {
        cancel: tokio_util::sync::CancellationToken,
        tx: mpsc::Sender<Event>,
        fill_queue: bool,
        executed: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl ToolSpec for CompleteThenCancel {
        // Registered under the canonical read identity so the engine's
        // central resource authority (not this fixture) grants the disjoint
        // ReadPath claims that form one real parallel chunk.
        fn name(&self) -> &str {
            "read_file"
        }
        fn description(&self) -> &str {
            "Finish an observed operation before firing its turn cancellation token."
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        fn supports_parallel(&self) -> bool {
            true
        }
        fn prepare(
            &self,
            input: Value,
            context: &ToolContext,
        ) -> Result<PreparedToolCall, ToolError> {
            let path = input["path"].as_str().expect("fixture path");
            Ok(PreparedToolCall {
                name: self.name().to_string(),
                description: self.description().to_string(),
                read_only: true,
                supports_parallel: true,
                starts_detached: false,
                approval: ApprovalRequirement::Auto,
                // Replaced by `registered_resource_claims`; kept honest anyway.
                resources: vec![ResourceClaim::ReadPath(context.workspace.join(path))],
                input,
            })
        }
        async fn execute(&self, _: Value, _: &ToolContext) -> Result<ToolResult, ToolError> {
            self.executed.fetch_add(1, Ordering::SeqCst);
            if self.fill_queue {
                while self
                    .tx
                    .try_send(Event::status("full at tool completion"))
                    .is_ok()
                {}
            }
            self.cancel.cancel();
            Ok(ToolResult::success("completed before cancellation"))
        }
    }

    for fill_queue in [false, true] {
        let workspace = tempdir().unwrap();
        for path in ["one", "two"] {
            std::fs::write(workspace.path().join(path), path).unwrap();
        }
        let mock = Arc::new(MockLlmClient::new(vec![
            tool_batch_turn(&[
                ("one", "read_file", r#"{"path":"one"}"#),
                ("two", "read_file", r#"{"path":"two"}"#),
            ]),
            canned::simple_text_turn("must not run after cancellation"),
        ]));
        let (mut engine, handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &Config::default(),
            mock.clone(),
        );
        let turn_guard = engine.begin_turn_control();
        let executed = Arc::new(AtomicUsize::new(0));
        let mut registry = crate::tools::ToolRegistry::new(ToolContext::new(workspace.path()));
        registry.register(Arc::new(CompleteThenCancel {
            cancel: engine.cancel_token.clone(),
            tx: engine.tx_event.clone(),
            fill_queue,
            executed: executed.clone(),
        }));
        let tools = Some(registry.to_api_tools_with_cache(true));
        let policy = test_tool_surface(&engine, registry, tools, AppMode::Agent);
        let mut turn = TurnContext::new(4);
        let (status, error) = tokio::time::timeout(
            Duration::from_secs(2),
            engine.run_turn(&mut turn, policy, None, None),
        )
        .await
        .expect("actual parallel tool completion cannot park on a full cancelled queue");
        assert_eq!(status, TurnOutcomeStatus::Interrupted, "{error:?}");
        assert!(error.is_none());
        assert_eq!(
            executed.load(Ordering::SeqCst),
            1,
            "the peer never executes"
        );
        assert_eq!(
            mock.call_count(),
            1,
            "no provider continuation after cancellation"
        );
        let mut rx = handle.rx_event.write().await;
        let events: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(events.iter().any(|event| matches!(event,
            Event::Status { message } if message == "Executing 2 read-only tools in 1 parallel chunk(s)"
        )), "fixture must exercise one actual two-tool parallel chunk");
        let starts: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::OperationActivityStarted {
                    span_id,
                    activity_kind,
                    ..
                } => Some((span_id, activity_kind)),
                _ => None,
            })
            .collect();
        assert_eq!(starts.len(), 1, "one actual tool activity was admitted");
        let completed: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::OperationActivityCompleted {
                    span_id,
                    activity_kind,
                    outcome,
                    ..
                } => Some((span_id, activity_kind, outcome)),
                _ => None,
            })
            .collect();
        let calls: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::ToolCallComplete {
                    id,
                    model_call,
                    result,
                    ..
                } => Some((id, model_call, result)),
                _ => None,
            })
            .collect();
        if fill_queue {
            assert_eq!(
                completed.len(),
                1,
                "the span reserved at admission keeps its completion on a full queue"
            );
            assert_eq!(completed[0].0, starts[0].0, "retain the span relationship");
            assert_eq!(completed[0].1, starts[0].1);
            assert_eq!(*completed[0].2, OwnerOperationOutcome::Succeeded);
            assert!(
                calls.is_empty(),
                "cancelled full waits do not invent tool-call delivery"
            );
        } else {
            assert_eq!(
                completed.len(),
                1,
                "preserve the completed activity after cancellation"
            );
            assert_eq!(completed[0].0, starts[0].0, "retain the span relationship");
            assert_eq!(completed[0].1, starts[0].1);
            assert_eq!(*completed[0].2, OwnerOperationOutcome::Succeeded);
            assert_eq!(
                calls.len(),
                2,
                "completed call and cancelled peer both settle exactly once"
            );
            let succeeded: Vec<_> = calls
                .iter()
                .filter(|(_, _, result)| result.as_ref().is_ok_and(|result| result.success))
                .collect();
            assert_eq!(succeeded.len(), 1);
            let cancelled_peer = calls
                .iter()
                .find(|(_, _, result)| !result.as_ref().is_ok_and(|result| result.success))
                .expect("cancelled peer has its own completion");
            let peer = cancelled_peer
                .2
                .as_ref()
                .expect("legacy cancelled peer result");
            assert_eq!(peer.metadata.as_ref().unwrap()["cancelled"], true);
            assert_eq!(peer.metadata.as_ref().unwrap()["cleanup_confirmed"], false);
            assert_eq!(
                succeeded[0].2.as_ref().unwrap().content,
                "completed before cancellation"
            );
            assert!(
                starts[0].0.starts_with(&format!("{}#", succeeded[0].0)),
                "span retains its completed execution identity"
            );
            let provider_ids: HashSet<_> = calls
                .iter()
                .map(|(_, model_call, _)| model_call.as_ref().unwrap().provider_id.as_str())
                .collect();
            assert_eq!(provider_ids, HashSet::from(["one", "two"]));
            let call_starts: HashSet<_> = events
                .iter()
                .filter_map(|event| match event {
                    Event::ToolCallStarted { id, .. } => Some(id),
                    _ => None,
                })
                .collect();
            assert!(calls.iter().all(|(id, _, _)| call_starts.contains(id)));
        }
        drop(rx);
        drop(turn_guard);
    }
}

const REPRESENTATIVE_FIXTURE_ID: &str = "representative-v1";
const REPRESENTATIVE_PROJECT_AUTHORITY: &str = "REPRESENTATIVE_PROJECT_AUTHORITY";
const REPRESENTATIVE_PROJECT_AUTHORITY_BODY: &str = concat!(
    "# Representative Project Authority\n\n",
    "REPRESENTATIVE_PROJECT_AUTHORITY\n\n",
    "- Keep all work local to the isolated fixture workspace.\n",
    "- Treat the checked-in repository instructions as the authority for edits.\n",
    "- Preserve unrelated files and report unsupported checks as unrun.\n",
    "- Prefer one owner for each runtime fact and delete duplicated derivations.\n",
    "- Use deterministic provider-free tests before claiming a behavior is verified.\n",
    "- Keep durable state atomic, recoverable, and explicit about unavailable facts.\n",
    "- Do not contact remotes, providers, registries, or production services.\n",
    "- Record exact measurements and distinguish source proof from installed proof.\n",
);

#[test]
fn snapshot_notice_precedes_first_provider_call_and_is_owned_by_session() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    let _env = lock_test_env();
    let root = tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _user_home = EnvVarGuard::set("HOME", root.path());
    let _user_profile = EnvVarGuard::set("USERPROFILE", root.path());
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("large.txt"), vec![b'x'; 4096]).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        // A resumed Engine for session-a must not warn again. Session-b in
        // the same process/workspace must receive its own first-turn notice.
        for (session_id, expected_notices) in [("session-a", 1), ("session-b", 1), ("session-a", 0)]
        {
            let config = Config::default();
            let client = std::sync::Arc::new(MockLlmClient::new(Vec::new()));
            let (engine, handle) = Engine::new_with_model_client(
                EngineConfig {
                    session_id: Some(session_id.into()),
                    snapshots_enabled: true,
                    snapshots_max_workspace_bytes: 1024,
                    ..deterministic_engine_config(&workspace)
                },
                &config,
                client.clone(),
            );
            let events = std::sync::Arc::clone(&handle.rx_event);
            let observations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let observed = std::sync::Arc::clone(&observations);
            client.push_factory(move |_| {
                let mut events = events.try_write().expect("fixture owns the event receiver");
                let mut notices = Vec::new();
                while let Ok(event) = events.try_recv() {
                    if let Event::SnapshotsDisabled { reason, .. } = event {
                        notices.push(reason);
                    }
                }
                observed.lock().unwrap().push(notices);
                canned::simple_text_turn("snapshot fixture done")
            });
            let run = tokio::spawn(engine.run());
            handle
                .send(external_user_message_op(
                    "check snapshots",
                    AppMode::Agent,
                    &config,
                ))
                .await
                .unwrap();
            let snapshot =
                tokio::time::timeout(Duration::from_secs(10), handle.get_session_snapshot())
                    .await
                    .unwrap()
                    .unwrap();
            // The Engine catches provider panics, so assertions inside the
            // factory are not a test oracle. Inspect its observations here.
            {
                let observed = observations.lock().unwrap();
                assert_eq!(
                    observed.len(),
                    1,
                    "factory must have recorded an observation"
                );
                assert_eq!(
                    observed[0].len(),
                    expected_notices,
                    "notice must precede provider dispatch for this session"
                );
                assert!(observed[0].iter().all(|reason| {
                    // One rendered line: consequence, cause, and the remedy
                    // that lifts this gate, each stated once.
                    reason.lines().count() == 1
                        && reason.contains("Snapshots and /undo are off")
                        && reason
                            .matches(crate::core::turn::SNAPSHOTS_CAP_CONFIG_KEY)
                            .count()
                            == 1
                }));
            }
            assert_eq!(client.call_count(), 1);
            assert!(
                serde_json::to_string(&snapshot.messages)
                    .unwrap()
                    .contains("snapshot fixture done")
            );
            handle.send(Op::Shutdown).await.unwrap();
            tokio::time::timeout(Duration::from_secs(10), run)
                .await
                .unwrap()
                .unwrap();
        }
    });
    // Await the owned blocking post-turn snapshots before restoring test home.
    drop(runtime);
}

/// A recording host (`record_restore_points`) receives every workspace
/// snapshot receipt of a turn before its `TurnComplete`: the pre-turn restore
/// point, a `tool` snapshot naming the file-mutating call and the paths it
/// declared, the `post_tool` snapshot closing it, and the post-turn state. A
/// read-only call takes none. The pre/post-turn trees bracket exactly the
/// turn's write, so a host derives the turn's workspace delta from them.
#[test]
fn recorded_snapshot_receipts_bracket_the_turn_and_its_file_writes() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_snapshot::WorkspaceSnapshotKind;
    let _env = lock_test_env();
    let root = tempdir().unwrap();
    let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _user_home = EnvVarGuard::set("HOME", root.path());
    let _user_profile = EnvVarGuard::set("USERPROFILE", root.path());
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("README.md"), "fixture\n").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let config = Config::default();
        let client = std::sync::Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn(
                "call-write",
                "File",
                r#"{"action":"write","path":"out.md","content":"out\n"}"#,
            ),
            canned::tool_call_turn(
                "call-read",
                "File",
                r#"{"action":"read","path":"README.md"}"#,
            ),
            canned::simple_text_turn("done"),
        ]));
        let (engine, handle) = Engine::new_with_model_client(
            EngineConfig {
                session_id: Some("session-restore".into()),
                snapshots_enabled: true,
                snapshots_max_workspace_bytes: 0,
                record_restore_points: true,
                ..deterministic_engine_config(&workspace)
            },
            &config,
            client,
        );
        let run = tokio::spawn(engine.run());
        let Op::SendMessage(mut spec) =
            external_user_message_op("write out.md", AppMode::Agent, &config)
        else {
            unreachable!("external_user_message_op builds a SendMessage");
        };
        spec.auto_approve = true;
        spec.trust_mode = true;
        spec.approval_mode = ApprovalMode::Bypass;
        handle.send(Op::SendMessage(spec)).await.unwrap();

        let mut completions = HashMap::new();
        let mut local_ids = HashMap::new();
        let mut receipts = Vec::new();
        let mut rx = handle.rx_event.write().await;
        while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("turn events")
        {
            match event {
                Event::ToolCallStarted {
                    id,
                    model_call: Some(model_call),
                    ..
                } => {
                    uuid::Uuid::parse_str(&id).expect("host execution id");
                    assert_ne!(id, model_call.provider_id);
                    assert!(local_ids.insert(model_call.provider_id, id).is_none());
                }
                Event::ToolCallComplete {
                    id,
                    result,
                    model_call: Some(model_call),
                    ..
                } => {
                    assert_eq!(local_ids.get(&model_call.provider_id), Some(&id));
                    completions.insert(model_call.provider_id, result.expect("tool result"));
                }
                Event::WorkspaceSnapshotTaken { snapshot } => receipts.push(snapshot),
                Event::TurnComplete { status, error, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                    break;
                }
                _ => {}
            }
        }
        drop(rx);

        assert!(completions.get("call-write").expect("write ran").success);
        assert!(completions.get("call-read").expect("read ran").success);
        assert_eq!(local_ids.len(), 2);
        assert_ne!(local_ids["call-write"], local_ids["call-read"]);
        // Every receipt, post-turn included, arrived before TurnComplete.
        assert_eq!(
            receipts
                .iter()
                .map(|receipt| (receipt.kind, receipt.tool_call_id.as_deref()))
                .collect::<Vec<_>>(),
            [
                (WorkspaceSnapshotKind::PreTurn, None),
                (
                    WorkspaceSnapshotKind::Tool,
                    Some(local_ids["call-write"].as_str())
                ),
                (
                    WorkspaceSnapshotKind::PostTool,
                    Some(local_ids["call-write"].as_str())
                ),
                (WorkspaceSnapshotKind::PostTurn, None),
            ],
            "a read-only call takes no restore point: {receipts:?}"
        );
        assert!(
            receipts
                .iter()
                .all(|receipt| receipt.session_id == "session-restore")
        );
        assert_eq!(
            receipts[1].write_paths.as_deref(),
            Some(&["out.md".to_string()][..])
        );
        assert_eq!(
            receipts[2].changed_paths.as_deref(),
            Some(&["out.md".to_string()][..])
        );

        let repo = codewhale_snapshot::SnapshotRepo::open_existing(&workspace)
            .unwrap()
            .expect("snapshot repo");
        let listed = repo.list(usize::MAX).unwrap();
        assert!(
            listed.iter().any(|snapshot| receipts[1].matches(snapshot)
                && snapshot.label == format!("tool:{}", local_ids["call-write"])),
            "the tool receipt names a live snapshot"
        );
        let delta = repo
            .diff_snapshots(
                &codewhale_snapshot::SnapshotId::parse(&receipts[0].tree_id).unwrap(),
                &codewhale_snapshot::SnapshotId::parse(&receipts[3].tree_id).unwrap(),
                100,
            )
            .unwrap();
        assert_eq!(
            delta
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["out.md"],
            "the pre/post-turn trees bracket exactly this turn's write"
        );

        handle.send(Op::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap();
    });
    // Await the owned blocking post-turn snapshots before restoring test home.
    drop(runtime);
}

#[test]
fn preview_request_error_preserves_non_semantic_context_chain() {
    let error = anyhow::Error::msg("root cause").context("request preparation failed");
    assert_eq!(
        preview_request_error_user_message("en", &error),
        "request preparation failed: root cause"
    );
    assert_eq!(
        initial_stream_error_user_message("en", &error),
        "request preparation failed: root cause"
    );
}

#[test]
fn initial_stream_failure_preserves_sanitized_context_and_typed_category() {
    let error = anyhow::Error::new(crate::llm_client::LlmError::InvalidRequest {
        status: 400,
        message: "image input is unsupported; api_key=fixture-credential-value".to_string(),
    })
    .context("Responses API request failed");
    let display = initial_stream_error_user_message("en", &error);
    assert!(
        display.contains("Responses API request failed"),
        "{display}"
    );
    assert!(display.contains("Invalid request (400)"), "{display}");
    assert!(display.contains("image input is unsupported"), "{display}");
    assert!(!display.contains("fixture-credential-value"), "{display}");
    assert!(display.contains("[redacted]"), "{display}");

    // The real boundary classifies the original error independently of its
    // expanded display text. Preserve the typed terminal invalid-input result.
    let policy_message = error.to_string();
    let mut envelope = crate::llm_error_envelope::envelope_for_llm_error(error, policy_message);
    envelope.message = display;
    assert_eq!(
        envelope.category,
        codewhale_error_taxonomy::ErrorCategory::InvalidInput
    );
    assert!(!envelope.recoverable);
    assert_eq!(envelope.code, "llm_invalid_request");
}
const REPRESENTATIVE_INLINE_INSTRUCTIONS: &str = "REPRESENTATIVE_INLINE_INSTRUCTIONS";
const REPRESENTATIVE_SKILL_DESCRIPTION: &str = "REPRESENTATIVE_SKILL_DESCRIPTION";
const REPRESENTATIVE_MEMORY_CHECKPOINT: &str = "REPRESENTATIVE_MEMORY_CHECKPOINT";
const REPRESENTATIVE_GOAL_OBJECTIVE: &str = "REPRESENTATIVE_GOAL_OBJECTIVE";

#[test]
fn cancellation_wins_at_the_terminal_child_settlement_seam() {
    assert_eq!(
        terminal_turn_status_at_settlement(TurnOutcomeStatus::Completed, true),
        TurnOutcomeStatus::Interrupted
    );
    assert_eq!(
        terminal_turn_status_at_settlement(TurnOutcomeStatus::Completed, false),
        TurnOutcomeStatus::Completed
    );
    assert_eq!(
        terminal_turn_status_at_settlement(TurnOutcomeStatus::Failed, true),
        TurnOutcomeStatus::Failed
    );
}

#[tokio::test]
async fn terminal_barrier_keeps_healthy_child_and_late_completion_alive() {
    use std::sync::atomic::Ordering;
    let turn_token = CancellationToken::new();
    let (mailbox, _receiver) = Mailbox::new(turn_token.clone());
    let children = Arc::new(ForegroundChildRegistry::new());
    let child_token = turn_token.child_token();
    let registration = children
        .register(child_token.clone(), "agent_healthy")
        .unwrap();
    let parking = registration.parking_signal();
    let (complete_tx, mut complete_rx) = tokio::sync::mpsc::unbounded_channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let child = tokio::spawn(async move {
        release_rx.await.unwrap();
        assert!(!child_token.is_cancelled());
        complete_tx.send("existing completion inbox").unwrap();
        drop(registration);
    });
    let (flush_tx, flush_rx) = tokio::sync::oneshot::channel();
    let drain_handle = tokio::spawn(async {
        let _ = flush_rx.await;
    });
    let barrier = TurnMailboxBarrier {
        mailbox,
        cancel_token: turn_token.clone(),
        foreground_children: Arc::clone(&children),
        flush_tx,
        drain_handle,
        settle_grace: Duration::from_secs(1),
    };
    tokio::time::timeout(Duration::from_secs(1), barrier.continue_and_flush())
        .await
        .unwrap();
    assert_eq!(children.active_count(), 1);
    assert!(!turn_token.is_cancelled());
    assert!(!parking.load(Ordering::Acquire));
    release_tx.send(()).unwrap();
    assert_eq!(complete_rx.recv().await, Some("existing completion inbox"));
    child.await.unwrap();
    assert_eq!(children.active_count(), 0);
}

#[tokio::test]
async fn terminal_barrier_explicit_cancel_still_joins_owned_child() {
    let turn_token = CancellationToken::new();
    let (mailbox, _receiver) = Mailbox::new(turn_token.clone());
    let children = Arc::new(ForegroundChildRegistry::new());
    let child_token = turn_token.child_token();
    let registration = children
        .register(child_token.clone(), "agent_owned")
        .unwrap();
    let child = tokio::spawn(async move {
        child_token.cancelled().await;
        drop(registration);
    });
    let (flush_tx, flush_rx) = tokio::sync::oneshot::channel();
    let drain_handle = tokio::spawn(async {
        let _ = flush_rx.await;
    });
    let barrier = TurnMailboxBarrier {
        mailbox,
        cancel_token: turn_token,
        foreground_children: Arc::clone(&children),
        flush_tx,
        drain_handle,
        settle_grace: Duration::from_secs(1),
    };
    let unsettled = tokio::time::timeout(Duration::from_secs(1), barrier.cancel_and_flush())
        .await
        .unwrap();
    assert!(unsettled.is_empty(), "cooperative children join cleanly");
    child.await.unwrap();
    assert_eq!(children.active_count(), 0);
}

/// Regression for #6184: a foreground child parked on an await that never
/// observes its cancel token must not withhold the terminal turn event. The
/// join gives up at `settle_grace` and names the child it left behind.
#[tokio::test]
async fn terminal_barrier_cancel_names_child_that_ignores_cancellation() {
    let turn_token = CancellationToken::new();
    let (mailbox, _receiver) = Mailbox::new(turn_token.clone());
    let children = Arc::new(ForegroundChildRegistry::new());
    let child_token = turn_token.child_token();
    // The fake child keeps its registration for the whole test — it never
    // observes the cancel token, like a task parked on a blocking await.
    let registration = children
        .register(child_token.clone(), "agent_stuck")
        .unwrap();
    let (flush_tx, flush_rx) = tokio::sync::oneshot::channel();
    let drain_handle = tokio::spawn(async {
        let _ = flush_rx.await;
    });
    let barrier = TurnMailboxBarrier {
        mailbox,
        cancel_token: turn_token.clone(),
        foreground_children: Arc::clone(&children),
        flush_tx,
        drain_handle,
        settle_grace: Duration::from_millis(50),
    };
    // Esc latches the turn token before the barrier runs, so the grace
    // window — not the token — is the bound under test.
    turn_token.cancel();
    let unsettled = tokio::time::timeout(Duration::from_secs(1), barrier.cancel_and_flush())
        .await
        .expect("the bounded join must not wait on a parked child");
    assert_eq!(unsettled, vec!["agent_stuck".to_string()]);
    assert!(
        child_token.is_cancelled(),
        "the bounded join still cancels the child's token"
    );
    assert_eq!(
        children.active_count(),
        1,
        "the stuck child is left registered — leaked, not awaited"
    );
    drop(registration);
    assert_eq!(children.active_count(), 0);
}

/// Regression for #6184: the mailbox drainer parked in an untimed
/// `tx_event.send().await` (event channel full, UI not draining) must not
/// withhold the terminal turn event. `flush` gives up at `settle_grace` and
/// aborts the drainer, so the turn settles instead of waiting hours.
#[tokio::test]
async fn terminal_barrier_flush_bounds_a_drainer_parked_on_a_full_event_channel() {
    let turn_token = CancellationToken::new();
    let (mailbox, _receiver) = Mailbox::new(turn_token.clone());
    let children = Arc::new(ForegroundChildRegistry::new());
    // The wedged shape from the report: a one-slot event channel, already
    // full, with nobody draining — so the drainer's forward parks in `send`.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<()>(1);
    event_tx.send(()).await.unwrap();
    let drain_handle = tokio::spawn(async move {
        // Parked forever: the channel is full and the receiver never drains.
        let _ = event_tx.send(()).await;
    });
    // Give the drainer a moment to park before the flush races it.
    tokio::task::yield_now().await;
    let (flush_tx, _flush_rx) = tokio::sync::oneshot::channel();
    let barrier = TurnMailboxBarrier {
        mailbox,
        cancel_token: turn_token,
        foreground_children: Arc::clone(&children),
        flush_tx,
        drain_handle,
        settle_grace: Duration::from_millis(50),
    };
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(2), barrier.continue_and_flush())
        .await
        .expect("flush must give up at its grace, not park with the drainer");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "flush returned in {:?}",
        started.elapsed()
    );
    // Abort receipt: an aborted drainer drops its sender half, so the
    // buffered item drains and the channel then reads closed.
    assert!(event_rx.recv().await.is_some());
    assert!(
        event_rx.recv().await.is_none(),
        "aborted drainer must release the event channel"
    );
}

/// A turn that fails without user cancellation runs the same barrier with a
/// live turn token: Esc during the join must still break it rather than sit
/// out the whole grace period (#6184).
#[tokio::test]
async fn terminal_barrier_cancel_join_breaks_on_fresh_esc() {
    let turn_token = CancellationToken::new();
    let (mailbox, _receiver) = Mailbox::new(turn_token.clone());
    let children = Arc::new(ForegroundChildRegistry::new());
    let registration = children
        .register(turn_token.child_token(), "agent_stuck")
        .unwrap();
    let (flush_tx, flush_rx) = tokio::sync::oneshot::channel();
    let drain_handle = tokio::spawn(async {
        let _ = flush_rx.await;
    });
    let barrier = TurnMailboxBarrier {
        mailbox,
        cancel_token: turn_token.clone(),
        foreground_children: Arc::clone(&children),
        flush_tx,
        drain_handle,
        settle_grace: Duration::from_secs(30),
    };
    let esc = turn_token.clone();
    let esc_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        esc.cancel();
    });
    let unsettled = tokio::time::timeout(Duration::from_secs(1), barrier.cancel_and_flush())
        .await
        .expect("a fresh Esc must break the join before its grace expires");
    assert_eq!(unsettled, vec!["agent_stuck".to_string()]);
    esc_task.await.unwrap();
    drop(registration);
}

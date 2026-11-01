/// A fence is admitted through the same planning as a `code_execution`
/// call, so an Auto-Review block rule on `code_execution` also stops the
/// fence, even under Full Access where no card would open.
#[tokio::test]
async fn repl_fence_obeys_auto_review_block_under_full_access() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Message};

    let workspace = tempdir().expect("tempdir");
    let marker = workspace.path().join("fence-ran");
    let fence = format!(
        "```repl\nopen({:?}, 'w').write('x')\nfinalize('done')\n```",
        marker.display().to_string()
    );
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn(&fence),
        canned::simple_text_turn("Done."),
    ]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let mut config = deterministic_engine_config(workspace.path());
    // Built through the config entry point, as production does.
    config.auto_review_policy =
        crate::core::authority::auto_review::AutoReviewPolicy::from_config(&{
            let __cw_config_init_0_0 = Some(codewhale_runtime::config::AutoReviewConfig {
                block: vec![{
                    let __cw_config_init_1_0 = Some("no-code".to_string());
                    let __cw_config_init_1_1 = Some(CODE_EXECUTION_TOOL_NAME.to_string());
                    let __cw_config_init_1_2 = Some("code is blocked here".to_string());
                    let mut __cw_config_init_1_value: codewhale_runtime::config::AutoReviewRuleConfig =
                        Default::default();
                    __cw_config_init_1_value.id = __cw_config_init_1_0;
                    __cw_config_init_1_value.tool = __cw_config_init_1_1;
                    __cw_config_init_1_value.reason = __cw_config_init_1_2;
                    __cw_config_init_1_value
                }],
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.auto_review = __cw_config_init_0_0;
            __cw_config_init_0_value
        });
    let (mut engine, handle) = Engine::new_with_model_client(config, &Config::default(), client);
    engine.session.auto_approve = true;
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Compute.".to_string(),
            cache_control: None,
        }],
    });
    let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
        workspace.path().to_path_buf(),
    ));
    let policy = test_tool_surface(
        &engine,
        registry,
        Some(vec![catalog_tool(CODE_EXECUTION_TOOL_NAME)]),
        AppMode::Agent,
    );
    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine.run_turn(&mut turn, policy, None, None).await;

    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert!(
        engine.repl_kernel.is_none(),
        "blocked fence starts no kernel"
    );
    assert!(!marker.exists(), "blocked fence must not run");
    let note = {
        let mut rx = handle.rx_event.write().await;
        std::iter::from_fn(|| rx.try_recv().ok()).any(|event| {
            matches!(event, Event::Status { message }
                if message.starts_with("REPL block not run:") && message.contains("code is blocked here"))
        })
    };
    assert!(note, "the block reason is shown");
}

/// Under Auto-Review a fence goes through the same review as a
/// `code_execution` call instead of an approval card that the posture can
/// only auto-deny: with the reviewer allowing it, the fence runs.
#[tokio::test]
async fn repl_fence_runs_through_auto_review_without_a_card() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Message};

    let workspace = tempdir().expect("tempdir");
    let marker = workspace.path().join("fence-ran");
    let fence = format!(
        "```repl\nopen({:?}, 'w').write('x')\nfinalize('done')\n```",
        marker.display().to_string()
    );
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn(&fence),
        canned::simple_text_turn("Done."),
    ]));
    mock.push_message_response(guardian_fixture_response(
        r#"{"risk_level":"low","decision":"allow","reason":"isolated fixture write"}"#,
    ));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    engine.session.auto_approve = false;
    engine.session.approval_mode = ApprovalMode::Auto;
    engine.session.add_message(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "Compute.".to_string(),
            cache_control: None,
        }],
    });
    let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
        workspace.path().to_path_buf(),
    ));
    let policy = test_tool_surface(
        &engine,
        registry,
        Some(vec![catalog_tool(CODE_EXECUTION_TOOL_NAME)]),
        AppMode::Agent,
    );
    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = tokio::time::timeout(
        Duration::from_secs(30),
        engine.run_turn(&mut turn, policy, None, None),
    )
    .await
    .expect("an Auto-Review fence must not wait on a card");

    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    let asked = {
        let mut rx = handle.rx_event.write().await;
        std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, Event::ApprovalRequired { .. }))
    };
    assert!(!asked, "Auto-Review opens no card for a reviewed fence");
    assert!(marker.exists(), "the reviewed fence runs");
}

async fn snapshot_for_catalog(
    workspace: &Path,
    catalog: Option<Vec<codewhale_models::Tool>>,
) -> crate::tool_inspection::ToolInspectionSnapshot {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("Done.")]));
    let client: crate::core::model_client::SharedModelClient = mock;
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace),
        &Config::default(),
        client,
    );
    let registry =
        crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(workspace.to_path_buf()));
    let surface = test_tool_surface(&engine, registry, catalog, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(2);
    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    let mut events = handle.rx_event.write().await;
    std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            Event::ToolRequestSnapshot { snapshot } => Some(snapshot),
            _ => None,
        })
        .expect("request snapshot")
}

#[tokio::test]
async fn request_selector_distinguishes_absent_tools_from_present_empty_tools() {
    let workspace = tempdir().expect("tempdir");
    let absent = snapshot_for_catalog(workspace.path(), None).await;
    let deferred_only = codewhale_models::Tool {
        tool_type: Some("function".to_string()),
        name: "deferred_fixture".to_string(),
        description: "Deferred fixture".to_string(),
        input_schema: json!({"type": "object"}),
        allowed_callers: None,
        defer_loading: Some(true),
        input_examples: None,
        strict: None,
        cache_control: None,
    };
    let selected = active_tools_for_request(&[deferred_only], &HashSet::new(), false);
    let present_empty = crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request(
        "turn",
        0,
        selected.as_deref(),
    );

    assert!(!absent.tools_field_present);
    assert_eq!(absent.tool_count, 0);
    assert!(present_empty.tools_field_present);
    assert_eq!(present_empty.tool_count, 0);
    assert_eq!(present_empty.payload_json_bytes, Some(2));
}

#[tokio::test]
async fn terminal_diagnostics_distinguish_narration_from_missing_protocol_tool_calls() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use crate::tool_inspection::TurnStopReason;

    for finish in ["end_turn", "tool_calls", "tool_use"] {
        let workspace = tempdir().expect("tempdir");
        let response = vec![
            canned::message_start("stop_diagnostic_fixture"),
            canned::text_block_start(0),
            canned::text_delta(0, "I will edit the file. 我现在修改文件。"),
            canned::block_stop(0),
            canned::message_delta(
                finish,
                Some(Usage {
                    input_tokens: 100,
                    output_tokens: 12,
                    reasoning_tokens: Some(4),
                    prompt_cache_hit_tokens: Some(80),
                    prompt_cache_miss_tokens: Some(20),
                    ..Usage::default()
                }),
            ),
            canned::message_stop(),
        ];
        let mock = std::sync::Arc::new(MockLlmClient::new(vec![response]));
        let client: crate::core::model_client::SharedModelClient = mock.clone();
        let (mut engine, handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &Config::default(),
            client,
        );
        let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
            workspace.path().to_path_buf(),
        ));
        let surface = test_tool_surface(&engine, registry, None, AppMode::Agent);
        let mut turn = crate::core::turn::TurnContext::new(4);
        let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
        let expected = if finish == "end_turn" {
            assert!(
                error.is_none(),
                "ordinary narration is not a protocol error"
            );
            TurnStopReason::ProviderNoToolCall
        } else {
            assert_eq!(status, TurnOutcomeStatus::Failed);
            assert!(
                error
                    .as_deref()
                    .is_some_and(|error| error.contains("supplied no tool call"))
            );
            TurnStopReason::ProviderToolCallMissing
        };
        let snapshot = turn
            .terminal_request_snapshot(status)
            .expect("terminal request snapshot");
        let terminal = snapshot.terminal.as_ref().expect("terminal facts");
        assert_eq!(terminal.reason, Some(expected));
        assert_eq!(terminal.model_requests_started, 1);
        assert_eq!(terminal.last_reported_input_tokens, Some(100));
        assert_eq!(terminal.last_response_tool_calls, Some(0));
        assert_eq!(terminal.last_response_tool_calls_suppressed, Some(0));
        assert_eq!(
            terminal.last_provider_finish_reason.as_ref().unwrap().value,
            finish
        );
        assert_eq!(
            mock.captured_requests().len(),
            1,
            "narration must not synthesize continuation"
        );
        assert_eq!(turn.usage.input_tokens, 100);
        assert_eq!(
            turn.usage.output_tokens, 12,
            "reasoning is an output subset, not additional output"
        );
        assert!(snapshot.render_text().contains("Terminal diagnostics"));
        let json = serde_json::to_value(&snapshot).expect("serialize snapshot");
        assert_eq!(json["terminal"]["model_requests_started"], 1);
        let mut events = handle.rx_event.write().await;
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, Event::ToolCallStarted { .. }))
        );
    }
}

#[tokio::test]
async fn terminal_diagnostics_merge_cumulative_usage_within_one_request() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let usage = Usage {
        input_tokens: 100,
        output_tokens: 12,
        reasoning_tokens: Some(4),
        prompt_cache_hit_tokens: Some(80),
        prompt_cache_miss_tokens: Some(20),
        ..Usage::default()
    };
    let mut start = canned::message_start("cumulative_usage_fixture");
    if let StreamEvent::MessageStart { message } = &mut start {
        message.usage = usage.clone();
    }
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![vec![
        start,
        canned::text_block_start(0),
        canned::text_delta(0, "Done."),
        canned::block_stop(0),
        canned::message_delta("end_turn", Some(usage.clone())),
        canned::message_delta("end_turn", Some(usage.clone())),
        canned::message_stop(),
    ]]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
        workspace.path().to_path_buf(),
    ));
    let surface = test_tool_surface(&engine, registry, None, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(mock.captured_requests().len(), 1);
    let mut expected_usage = usage.clone();
    expected_usage.request_usages = vec![usage.clone()];
    assert_eq!(
        turn.usage, expected_usage,
        "repeated cumulative receipts are counted once"
    );
    let mut events = handle.rx_event.write().await;
    let usages = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            Event::TurnUsage { usage, .. } => Some(usage),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(usages, vec![usage]);
}

#[tokio::test]
async fn request_snapshots_advance_to_the_latest_tool_step() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("README.md"), "fixture\n").expect("write fixture");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-read", "read_file", r#"{"path":"README.md"}"#),
        canned::simple_text_turn("Done."),
    ]));
    let client: crate::core::model_client::SharedModelClient = mock;
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let context = crate::tools::ToolContext::new(workspace.path().to_path_buf());
    let mut registry = crate::tools::ToolRegistry::new(context);
    registry.register(std::sync::Arc::new(crate::tools::file::ReadFileTool));
    let tools = Some(registry.to_api_tools_with_cache(true));
    let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    let mut events = handle.rx_event.write().await;
    let snapshots = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            Event::ToolRequestSnapshot { snapshot } => Some(snapshot),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].step, 0);
    assert_eq!(snapshots[1].step, 1);
    assert_eq!(snapshots[1].turn_id.value, turn.id);
}

#[tokio::test]
async fn tool_result_followed_by_terminal_empty_assistant_fails_turn() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("README.md"), "fixture\n").expect("write fixture");
    let empty_terminal_turn = vec![
        canned::message_start("mock_empty_after_tool"),
        canned::message_delta("stop", None),
        canned::message_stop(),
    ];
    // #6310: an answerless clean stop is retried (exact prefix, then nudged)
    // before the turn fails, so the fixture stays empty for every attempt.
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-read", "read_file", r#"{"path":"README.md"}"#),
        empty_terminal_turn.clone(),
        empty_terminal_turn.clone(),
        empty_terminal_turn,
    ]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let context = crate::tools::ToolContext::new(workspace.path().to_path_buf());
    let mut registry = crate::tools::ToolRegistry::new(context);
    registry.register(std::sync::Arc::new(crate::tools::file::ReadFileTool));
    let tools = Some(registry.to_api_tools_with_cache(true));
    let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert_eq!(
        mock.call_count(),
        4,
        "tool step, empty provider step, then exactly two bounded retries"
    );
    assert_eq!(turn.stop_diagnostics.empty_stop_retries, 2);
    assert!(
        error
            .as_deref()
            .is_some_and(|message| message.contains("terminal stop reason `stop`")
                && message.contains("after 2 retries")),
        "terminal empty response must produce a precise failure: {error:?}"
    );

    let mut events = handle.rx_event.write().await;
    let events = std::iter::from_fn(|| events.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::ToolCallComplete { model_call: Some(model_call), result, .. }
                if model_call.provider_id == "call-read" && result.is_ok()
        )),
        "the successful tool result must remain durable: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Error { .. })),
        "the empty provider response must be visible as an error: {events:?}"
    );
    assert!(
        engine
            .session
            .messages
            .iter()
            .all(|message| { message.role != Role::Assistant || !message.content.is_empty() }),
        "the engine must not fabricate an empty assistant message"
    );
}

fn empty_clean_stop_turn() -> Vec<StreamEvent> {
    use crate::llm_client::mock::canned;
    vec![
        canned::message_start("mock_empty_clean_stop"),
        canned::message_delta("stop", None),
        canned::message_stop(),
    ]
}

async fn run_empty_stop_fixture(
    turns: Vec<Vec<StreamEvent>>,
) -> (
    std::sync::Arc<crate::llm_client::mock::MockLlmClient>,
    Engine,
    crate::core::turn::TurnContext,
    TurnOutcomeStatus,
    Option<String>,
    Vec<Event>,
) {
    let workspace = tempdir().expect("tempdir");
    let mock = std::sync::Arc::new(crate::llm_client::mock::MockLlmClient::new(turns));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
        workspace.path().to_path_buf(),
    ));
    let surface = test_tool_surface(&engine, registry, None, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    let mut rx = handle.rx_event.write().await;
    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    (mock, engine, turn, status, error, events)
}

/// #6310: one clean `stop` with no text, reasoning or tool call is retried
/// with the identical request and the turn completes on the real answer.
#[tokio::test]
async fn empty_clean_stop_is_retried_once_and_the_turn_completes() {
    use crate::llm_client::mock::canned;

    let (mock, engine, turn, status, error, events) = run_empty_stop_fixture(vec![
        empty_clean_stop_turn(),
        canned::simple_text_turn("the recovered answer"),
    ])
    .await;

    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(mock.call_count(), 2, "exactly one retry");
    assert_eq!(turn.stop_diagnostics.empty_stop_retries, 1);
    let requests = mock.captured_requests();
    assert_eq!(
        requests[0].messages.len(),
        requests[1].messages.len(),
        "the first retry is an exact-prefix re-request"
    );
    let transcript =
        serde_json::to_string(&engine.session.messages.iter().collect::<Vec<_>>()).unwrap();
    assert_eq!(transcript.matches("the recovered answer").count(), 1);
    assert!(
        engine
            .session
            .messages
            .iter()
            .all(|message| message.role != Role::Assistant || !message.content.is_empty()),
        "the empty response must not be persisted"
    );
    let retry_receipts = events
        .iter()
        .filter_map(|event| match event {
            Event::Status { message } if message.starts_with("Retry attempt: empty-stop ") => {
                Some(message)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retry_receipts.len(), 1);
    for (index, receipt) in retry_receipts.iter().enumerate() {
        assert!(receipt.starts_with(&format!("Retry attempt: empty-stop {}/2;", index + 1)));
    }
    assert_eq!(events.iter().filter(|event| matches!(event, Event::Status { message } if message == "Retry recovery: per-kind counter empty-stop used 1/2 retries; turn completed")).count(), 1);
}

/// #6310: the second retry carries the request-scoped nudge, which never
/// joins the session; the retry after that budget is not attempted.
#[tokio::test]
async fn empty_clean_stop_second_retry_is_nudged_and_never_persisted() {
    use crate::llm_client::mock::canned;

    let (mock, engine, turn, status, error, events) = run_empty_stop_fixture(vec![
        empty_clean_stop_turn(),
        empty_clean_stop_turn(),
        canned::simple_text_turn("answer after nudge"),
    ])
    .await;

    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(mock.call_count(), 3);
    assert_eq!(turn.stop_diagnostics.empty_stop_retries, 2);
    let requests = mock.captured_requests();
    let nudge = codewhale_runtime::config::DEFAULT_REASONING_ONLY_REPROMPT_MESSAGE;
    let carries_nudge = |request: &codewhale_models::MessageRequest| {
        serde_json::to_string(&request.messages)
            .unwrap()
            .contains(nudge)
    };
    assert!(!carries_nudge(&requests[0]));
    assert!(!carries_nudge(&requests[1]), "first retry is exact-prefix");
    assert!(carries_nudge(&requests[2]), "second retry is nudged");
    assert_eq!(requests[2].messages.len(), requests[0].messages.len() + 1);
    assert!(
        !serde_json::to_string(&engine.session.messages.iter().collect::<Vec<_>>())
            .unwrap()
            .contains(nudge),
        "the nudge is request-scoped and never written to the session"
    );
    let retry_receipts = events
        .iter()
        .filter_map(|event| match event {
            Event::Status { message } if message.starts_with("Retry attempt: empty-stop ") => {
                Some(message)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retry_receipts.len(), 2);
    for (index, receipt) in retry_receipts.iter().enumerate() {
        assert!(receipt.starts_with(&format!("Retry attempt: empty-stop {}/2;", index + 1)));
    }
    assert_eq!(events.iter().filter(|event| matches!(event, Event::Status { message } if message == "Retry recovery: per-kind counter empty-stop used 2/2 retries; turn completed")).count(), 1);
}

/// #6310: an empty response on every attempt fails visibly once the budget
/// is spent, with the retries recorded in stop diagnostics.
#[tokio::test]
async fn empty_clean_stop_every_time_fails_after_the_retry_budget() {
    let (mock, _engine, turn, status, error, events) = run_empty_stop_fixture(vec![
        empty_clean_stop_turn(),
        empty_clean_stop_turn(),
        empty_clean_stop_turn(),
    ])
    .await;

    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert_eq!(
        mock.call_count(),
        1 + crate::core::engine::turn_loop::EMPTY_STOP_MAX_RETRIES as usize
    );
    assert_eq!(
        turn.stop_diagnostics.empty_stop_retries,
        crate::core::engine::turn_loop::EMPTY_STOP_MAX_RETRIES
    );
    assert!(
        error
            .as_deref()
            .is_some_and(|message| message.contains("terminal stop reason `stop`")
                && message.contains("after 2 retries")),
        "{error:?}"
    );
    let retry_receipts = events
        .iter()
        .filter_map(|event| match event {
            Event::Status { message } if message.starts_with("Retry attempt: empty-stop ") => {
                Some(message)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retry_receipts.len(), 2);
    for (index, receipt) in retry_receipts.iter().enumerate() {
        assert!(receipt.starts_with(&format!("Retry attempt: empty-stop {}/2;", index + 1)));
    }
    assert_eq!(events.iter().filter(|event| matches!(event, Event::Status { message } if message == "Retry stopped: per-kind counter empty-stop used 2/2 retries; turn failed")).count(), 1);
}

#[tokio::test]
async fn request_snapshot_reports_registry_provenance_for_the_transmitted_catalog() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::simple_text_turn("Done.")]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    // Pin `read_file` loaded so this step's request actually carries it; the
    // point of the test is what a *transmitted* tool is reported as.
    let mut engine_config = deterministic_engine_config(workspace.path());
    engine_config.tools_always_load = HashSet::from(["read_file".to_string()]);
    let (mut engine, handle) =
        Engine::new_with_model_client(engine_config, &Config::default(), client);
    let context = crate::tools::ToolContext::new(workspace.path().to_path_buf());
    let mut registry = crate::tools::ToolRegistry::new(context);
    registry.register(std::sync::Arc::new(crate::tools::file::ReadFileTool));
    // `read_file` is a hidden compatibility alias, so `to_api_tools` would hand
    // the turn an empty catalog and nothing would be transmitted. Hand the
    // engine an explicit catalog instead: the registry is still the source of
    // the *facts*, including the fact that this tool is not model-visible.
    let tools = Some(vec![codewhale_models::Tool {
        tool_type: None,
        name: "read_file".to_string(),
        description: "Read a file".to_string(),
        input_schema: json!({"type": "object"}),
        allowed_callers: Some(vec!["direct".to_string()]),
        defer_loading: Some(false),
        input_examples: None,
        strict: None,
        cache_control: None,
    }]);

    // The same surface context `handle_send_message` resolves for a real turn:
    // real registry facts, real (empty) MCP attribution, the engine's own
    // synthetic-name list, and the resolved model client's receipt.
    let synthetic_names = super::tool_catalog::default_synthetic_catalog_tool_names();
    let surface = crate::tool_inspection::ToolSurfaceContext {
        registry: registry.registry_facts(&HashSet::new()),
        mcp_servers: std::collections::BTreeMap::new(),
        synthetic_names: synthetic_names.clone(),
        provider: engine.tool_surface_provider_receipt(),
    };
    let policy = test_tool_surface(&engine, registry, tools, AppMode::Agent);

    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine
        .run_turn(&mut turn, policy, None, Some(surface))
        .await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");

    let mut events = handle.rx_event.write().await;
    let snapshot = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| match event {
            Event::ToolRequestSnapshot { snapshot } => Some(snapshot),
            _ => None,
        })
        .expect("request snapshot");

    let transmitted = mock
        .last_request()
        .expect("captured request")
        .tools
        .unwrap_or_default();
    assert!(!transmitted.is_empty(), "the turn must carry tools");

    // The digest is the request path's, over what was prepared.
    assert_eq!(
        snapshot.active_tool_catalog_sha256.as_deref(),
        Some(crate::core::engine::preview::active_tool_catalog_sha256(&transmitted).as_str())
    );

    // Provenance is registry-derived truth, not "unavailable".
    assert!(snapshot.registry_facts_present);
    assert!(snapshot.provider.is_available());
    assert_eq!(
        snapshot.unavailable_for_this_request,
        vec!["provider_wire_payload"]
    );

    let read_file = snapshot
        .tools
        .iter()
        .find(|entry| entry.name.value == "read_file")
        .expect("read_file projected");
    assert_eq!(
        read_file.provenance,
        crate::tool_inspection::Evidence::Known {
            value: crate::tool_inspection::ToolProvenance::Builtin
        }
    );
    assert!(matches!(
        read_file.approval,
        crate::tool_inspection::Evidence::Known { .. }
    ));
    // Registry truth, not an inference from the request: this alias is hidden
    // from the model catalog even though this catalog carried it explicitly.
    assert_eq!(
        read_file.model_visible,
        crate::tool_inspection::Evidence::Known { value: false }
    );
    assert!(read_file.visibility.in_request());

    // Anything the engine injected rather than registered is reported as
    // synthetic, from the engine's own list — never guessed from the name.
    for entry in &snapshot.tools {
        if synthetic_names.contains(&entry.name.value) {
            assert_eq!(
                entry.provenance,
                crate::tool_inspection::Evidence::Known {
                    value: crate::tool_inspection::ToolProvenance::Synthetic
                },
                "'{}' is engine-injected, not registry-backed",
                entry.name.value
            );
            // Not in the registry, so capabilities are unknown, not "none".
            assert!(matches!(
                entry.capabilities,
                crate::tool_inspection::Evidence::Unknown { .. }
            ));
        }
    }
}

fn deterministic_engine_config(workspace: &Path) -> EngineConfig {
    EngineConfig {
        workspace: workspace.to_path_buf(),
        snapshots_enabled: false,
        subagents_enabled: false,
        ..EngineConfig::default()
    }
}

/// The compaction budget is measured against the real system prompt, and the
/// skills block in that prompt is discovered from the developer's home as well
/// as from the workspace. Left ambient, this test counts whatever skills the
/// machine happens to have installed into its token budget: 39 of them trip a
/// seventh compaction pass on a developer box while CI, with an empty home,
/// sees six and passes. That is the #5359 leak class, and the isolated home is
/// what makes `deterministic_engine_config` actually deterministic here.
#[test]
fn automatic_compaction_continues_one_task_and_preserves_failed_history() {
    let _env = lock_test_env();
    let home = tempdir().unwrap();
    let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
    let _user_home = EnvVarGuard::set("HOME", home.path());
    let _user_profile = EnvVarGuard::set("USERPROFILE", home.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        use crate::llm_client::mock::{MockLlmClient, canned};
        for fail_summary in [false, true] {
            let workspace = tempdir().unwrap();
            fs::write(
                workspace.path().join("README.md"),
                "verified fixture evidence",
            )
            .unwrap();
            let mock = std::sync::Arc::new(MockLlmClient::new(Vec::new()));
            for step in 0..16 {
                mock.push_turn(vec![
                    canned::message_start(&format!("response-{step}")),
                    canned::text_block_start(0),
                    canned::text_delta(0, &format!("Step {step}: {}", "x".repeat(32_000))),
                    canned::block_stop(0),
                    canned::tool_use_block_start(1, &format!("read-{step}"), "File"),
                    canned::tool_input_delta(1, r#"{"action":"read","path":"README.md"}"#),
                    canned::block_stop(1),
                    canned::message_delta("tool_use", None),
                    canned::message_stop(),
                ]);
            }
            mock.push_turn(canned::simple_text_turn(
                "All sixteen reads verified; task complete.",
            ));
            for checkpoint in 0..8 {
                let content = if fail_summary {
                    json!([{"type":"tool_use","id":"unexpected","name":"File","input":{}}])
                } else {
                    json!([{"type":"text","text":format!("Current objective: complete all sixteen reads. Checkpoint {checkpoint}: earlier reads verified. Preserve the user's no-publication constraint. Continue the remaining File reads, then report the observed evidence.")}])
                };
                mock.push_message_response(serde_json::from_value(json!({
                    "id":format!("summary-{checkpoint}"), "type":"message", "role":"assistant",
                    "content":content, "model":"mock-model", "usage":{"input_tokens":0,"output_tokens":0}
                })).unwrap());
            }
            let config = Config::default();
            let (engine, handle) = Engine::new_with_model_client(
                deterministic_engine_config(workspace.path()),
                &config,
                mock.clone(),
            );
            let task = tokio::spawn(engine.run());
            let mut op = external_user_message_op(
                "Complete all sixteen reads; do not publish.",
                AppMode::Agent,
                &config,
            );
            if let Op::SendMessage(TurnSpec {
                compaction,
                auto_approve,
                ..
            }) = &mut op
            {
                compaction.token_threshold = 40_000;
                *auto_approve = true;
            }
            handle.send(op).await.unwrap();
            let mut completed = 0;
            let mut failed = 0;
            {
                let mut rx = handle.rx_event.write().await;
                loop {
                    match tokio::time::timeout(Duration::from_secs(30), rx.recv())
                        .await
                        .unwrap()
                        .unwrap()
                    {
                        Event::CompactionCompleted { auto: true, .. } => completed += 1,
                        Event::CompactionFailed { auto: true, .. } => failed += 1,
                        Event::TurnComplete { status, error, .. } => {
                            if fail_summary {
                                assert_eq!(status, TurnOutcomeStatus::Failed, "{error:?}");
                                assert!(error.as_deref().is_some_and(|message| {
                                    message.contains("tool call instead of a summary")
                                        && message.contains("original conversation was preserved")
                                }), "{error:?}");
                            } else {
                                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                            }
                            break;
                        }
                        _ => {}
                    }
                }
            }
            let requests = mock.captured_requests();
            let streaming = requests
                .iter()
                .filter(|r| r.stream == Some(true))
                .collect::<Vec<_>>();
            if fail_summary {
                assert!(
                    (1..17).contains(&streaming.len()),
                    "an unusable summary must stop before another model step"
                );
                assert_eq!(
                    (completed, failed),
                    (0, 1),
                    "failed compaction must not loop at every tool boundary"
                );
            } else {
                assert_eq!(
                    streaming.len(),
                    17,
                    "one user request must continue through all tool steps"
                );
                assert!(
                    (2..=6).contains(&completed),
                    "expected repeated useful compaction: {completed}"
                );
                assert_eq!(failed, 0);
            }
            for request in &requests {
                assert_eq!(
                    request.system, streaming[0].system,
                    "the stable system prefix must survive every pass"
                );
                assert_eq!(
                    request.tools, streaming[0].tools,
                    "summarizing must reuse the tool prefix"
                );
                if request.stream == Some(false) {
                    assert_eq!(request.tool_choice, Some(json!("none")));
                }
                let mut calls = HashSet::new();
                for message in &request.messages {
                    for block in &message.content {
                        match block {
                            ContentBlock::ToolUse { id, .. } => {
                                calls.insert(id);
                            }
                            ContentBlock::ToolResult { tool_use_id, .. } => assert!(
                                calls.contains(tool_use_id),
                                "orphan tool result after compaction"
                            ),
                            _ => {}
                        }
                    }
                }
            }
            let snapshot = handle.get_session_snapshot().await.unwrap();
            if fail_summary {
                assert!(!snapshot.messages.iter().any(crate::compaction::is_compaction_checkpoint_message));
                for retained in ["Complete all sixteen reads; do not publish.", "Step 0:"] {
                    assert!(snapshot.messages.iter().any(|message| message.content.iter().any(|block| matches!(block, ContentBlock::Text {text,..} if text.contains(retained)))), "lost original context: {retained}");
                }
                assert!(snapshot.messages.iter().any(|message| message.content.iter().any(|block| matches!(block, ContentBlock::ToolResult {..}))));
            } else {
                assert!(snapshot.messages.iter().any(|m| m.content.iter().any(|b| matches!(b, ContentBlock::Text {text,..} if text.contains("All sixteen reads verified")))));
            }
            handle.send(Op::Shutdown).await.unwrap();
            task.await.unwrap();
        }
    });
}

#[tokio::test]
async fn initial_routed_usage_is_total_only_emitted_once_and_keeps_parent_route_separate() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let _cost_scope = crate::cost_status::test_scope();
    let workspace = tempdir().expect("tempdir");
    let parent_usage = Usage {
        input_tokens: 11,
        output_tokens: 3,
        ..Usage::default()
    };
    let classifier_usage = Usage {
        input_tokens: 7,
        output_tokens: 5,
        ..Usage::default()
    };
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![vec![
        canned::message_start("parent-response"),
        canned::text_block_start(0),
        canned::text_delta(0, "done"),
        canned::block_stop(0),
        canned::message_delta("end_turn", Some(parent_usage.clone())),
        canned::message_stop(),
    ]]));
    let client: crate::core::model_client::SharedModelClient = mock;
    let api_config = Config::default();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &api_config,
        client,
    );
    let task = tokio::spawn(engine.run());

    let mut op = external_user_message_op("account for classifier", AppMode::Agent, &api_config);
    let Op::SendMessage(TurnSpec {
        initial_routed_usage,
        ..
    }) = &mut op
    else {
        unreachable!("external_user_message_op always builds SendMessage");
    };
    let mut missing_usage_route = crate::cost_status::EffectiveRouteEnvelope::capture(
        None,
        ProviderKind::Openai,
        "openai",
        "classifier-model",
        Some(ProviderKind::Openai.provider().default_base_url()),
        chrono::Utc::now(),
    );
    missing_usage_route.billing_mode = crate::cost_status::RouteBillingMode::Metered;
    **initial_routed_usage = crate::cost_status::RuntimeUsageBatch {
        decisions: Vec::new(),
        records: vec![crate::cost_status::RuntimeUsageRecord {
            source_id: "auto-router:engine-fixture".to_string(),
            usage: crate::cost_status::EffectiveRouteUsage {
                route: crate::cost_status::EffectiveRouteEnvelope::capture(
                    None,
                    ProviderKind::Openai,
                    "openai",
                    "classifier-model",
                    Some(ProviderKind::Openai.provider().default_base_url()),
                    chrono::Utc::now(),
                ),
                usage: classifier_usage.clone(),
            },
        }],
        drop_records: vec![crate::cost_status::RuntimeUsageDropRecord {
            reason: crate::cost_status::RuntimeUsageMissingReason::default(),
            source_id: "auto-router:engine-fixture:missing-usage".to_string(),
            route: missing_usage_route,
        }],
        // One exact route-aware missing receipt plus two residual gaps whose
        // route identity was truncated upstream.
        dropped_records: 3,
    };
    handle.send(op).await.expect("send routed-usage turn");

    let mut routed_events = Vec::new();
    let mut rx = handle.rx_event.write().await;
    let (total_usage, terminal_parent_usage, dropped_records) = loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("timed out waiting for routed-usage turn")
            .expect("engine event stream closed");
        match event {
            Event::RoutedTurnUsage { usage, .. } => routed_events.push(usage),
            Event::TurnComplete {
                usage,
                parent_route_usage,
                routed_usage_dropped_records,
                status,
                error,
                ..
            } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break (usage, parent_route_usage, routed_usage_dropped_records);
            }
            _ => {}
        }
    };
    drop(rx);

    assert_eq!(routed_events, vec![classifier_usage.clone()]);
    let mut expected_parent_usage = parent_usage.clone();
    expected_parent_usage.request_usages = vec![parent_usage.clone()];
    assert_eq!(terminal_parent_usage, expected_parent_usage);
    assert_eq!(total_usage.input_tokens, 18);
    assert_eq!(total_usage.output_tokens, 8);
    assert_eq!(dropped_records, 2);
    let initial_cost = crate::cost_status::drain();
    assert!(
        initial_cost.usage_source_fingerprints.contains(
            &crate::cost_status::usage_source_fingerprint(
                "auto-router:engine-fixture:missing-usage"
            )
        ),
        "exact missing-usage response identity was not settled"
    );
    assert!(
        initial_cost
            .unpriced_reasons
            .contains("provider_success_missing_usage"),
        "metered missing-usage route was not marked incomplete"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn isolated_runtime_chat_provider_request_contains_no_host_context_or_tools() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    const PROJECT_CANARY: &str = "PRIVATE_PROJECT_AGENTS_CANARY";
    const MEMORY_CANARY: &str = "PRIVATE_USER_MEMORY_CANARY";
    const SKILL_CANARY: &str = "PRIVATE_SKILL_CANARY";
    const MCP_CANARY: &str = "PRIVATE_MCP_CANARY";
    const INSTRUCTION_CANARY: &str = "PRIVATE_INSTRUCTION_CANARY";
    const ATTACHMENT_CANARY: &str = "PRIVATE_ATTACHMENT_BYTES_CANARY";

    let root = tempdir().expect("private Runtime Chat root");
    let workspace = root.path().join("private-host-workspace-canary");
    let skills = root.path().join("private-skills-canary");
    fs::create_dir_all(&workspace).expect("create private workspace");
    fs::create_dir_all(&skills).expect("create private skills");
    fs::write(workspace.join("AGENTS.md"), PROJECT_CANARY).expect("write AGENTS canary");
    let memory = root.path().join("private-memory.md");
    fs::write(&memory, MEMORY_CANARY).expect("write memory canary");
    fs::write(skills.join("SKILL.md"), SKILL_CANARY).expect("write skill canary");
    let mcp = root.path().join("private-mcp.json");
    fs::write(&mcp, format!(r#"{{"server":"{MCP_CANARY}"}}"#)).expect("write MCP canary");
    let instructions = root.path().join("private-instructions.md");
    fs::write(&instructions, INSTRUCTION_CANARY).expect("write instruction canary");
    let attachment = root.path().join("private-attachment.png");
    fs::write(&attachment, ATTACHMENT_CANARY).expect("write attachment canary");

    let api_config = {
        let __cw_config_init_2_0 = true;
        let mut __cw_config_init_2_value: Config = Config::default();
        __cw_config_init_2_value.runtime_chat_isolated = __cw_config_init_2_0;
        __cw_config_init_2_value
    };
    let mut engine_config = deterministic_engine_config(&workspace);
    engine_config.instructions = vec![instructions.into()];
    engine_config.project_context_pack_enabled = true;
    engine_config.memory_enabled = true;
    engine_config.memory_path = memory;
    engine_config.skills_dir = skills;
    engine_config.mcp_config_path = mcp;
    engine_config.subagents_enabled = true;
    engine_config.allowed_tools = None;

    let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::simple_text_turn(
        "Hello from isolated Chat.",
    )]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (engine, handle) = Engine::new_with_model_client(engine_config, &api_config, client);
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            &format!("Say hello.\n[Attached image: {}]", attachment.display()),
            AppMode::Agent,
            &api_config,
        ))
        .await
        .expect("send isolated Chat turn");

    let mut rx = handle.rx_event.write().await;
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("isolated Chat turn timed out")
            .expect("isolated Chat event stream closed");
        if let Event::TurnComplete { status, error, .. } = event {
            assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
            break;
        }
    }

    let request = mock.last_request().expect("captured provider request");
    assert!(
        request.tools.as_ref().is_none_or(Vec::is_empty),
        "isolated Chat must expose no provider tools"
    );
    // #6517: the engine and the Runtime Chat relay once carried two different
    // isolated-chat prompts. The engine sends the one shared constant; the
    // relay's own test pins `dedicated_chat_system_prompt(None)` to the same
    // constant, so core tests need no edge into the relay (runtime ratchet).
    let system = match request.system.as_ref() {
        Some(SystemPrompt::Text(text)) => text.clone(),
        other => panic!("isolated Chat should send one text system prompt: {other:?}"),
    };
    assert_eq!(system, ISOLATED_CHAT_SYSTEM_PROMPT);
    let serialized = serde_json::to_string(&request).expect("serialize captured request");
    assert!(serialized.contains("Say hello."), "{serialized}");
    assert!(serialized.contains("Attachment omitted"), "{serialized}");
    for forbidden in [
        workspace.to_string_lossy().as_ref(),
        root.path().to_string_lossy().as_ref(),
        PROJECT_CANARY,
        MEMORY_CANARY,
        SKILL_CANARY,
        MCP_CANARY,
        INSTRUCTION_CANARY,
        ATTACHMENT_CANARY,
        "<turn_meta>",
        "<user_memory>",
        "<available_skills>",
        "Git workspace:",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "leaked {forbidden}: {serialized}"
        );
    }

    task.abort();
}

#[tokio::test]
async fn injected_model_drives_real_engine_navigation_trajectory() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(
        workspace.path().join("README.md"),
        "navigation-seam-proof\n",
    )
    .expect("write fixture");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-read",
            "File",
            r#"{"action":"read","path":"README.md"}"#,
        ),
        canned::simple_text_turn("Navigation complete."),
    ]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Read README.md and report what it contains.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send deterministic navigation turn");

    let mut saw_read = false;
    let mut saw_answer = false;
    let mut saw_unreceipted_injected_route = false;
    let mut saw_unattributed_injected_completion = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for deterministic navigation")
    {
        match event {
            Event::TurnStarted { route, .. } => {
                let route = route.expect("injected model turn route");
                assert!(
                    route.receipt.is_none(),
                    "an auxiliary route client must not receipt injected model I/O"
                );
                saw_unreceipted_injected_route = true;
            }
            Event::ToolCallComplete { name, result, .. } if name == "File" => {
                let result = result.expect("File.read result");
                assert!(result.success, "{result:?}");
                assert!(result.content.contains("navigation-seam-proof"));
                saw_read = true;
            }
            Event::MessageDelta { content, .. } => {
                saw_answer |= content.contains("Navigation complete");
            }
            Event::TurnComplete {
                status,
                error,
                base_url,
                ..
            } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                assert!(
                    base_url.is_none(),
                    "an auxiliary route client must not attribute an injected completion"
                );
                saw_unattributed_injected_completion = true;
                break;
            }
            _ => {}
        }
    }
    drop(rx);
    assert!(
        saw_read,
        "real registry must execute the mock-requested read"
    );
    assert!(
        saw_answer,
        "real stream projection must emit the final answer"
    );
    assert!(saw_unreceipted_injected_route);
    assert!(saw_unattributed_injected_completion);
    assert_eq!(mock.call_count(), 2);
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn injected_model_sandbox_escalation_applies_only_after_exact_call_approval() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    const COMMAND: &str = "echo elevated > escalation.txt";
    for (tool, source) in [
        ("bash", json!({"command": COMMAND})),
        (
            CODE_EXECUTION_TOOL_NAME,
            json!({"code": "open('escalation.txt', 'w').write('approved')"}),
        ),
        (
            JS_EXECUTION_TOOL_NAME,
            json!({"code": "require('fs').writeFileSync('escalation.txt', 'approved')"}),
        ),
    ] {
        let mut input = source;
        input["sandbox_permissions"] = json!("workspace-write");
        input["justification"] = json!("the exact command writes the requested workspace proof");
        let workspace = tempdir().expect("tempdir");
        let mock = std::sync::Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn("call-escalated-bash", tool, &input.to_string()),
            canned::simple_text_turn("Escalated command complete."),
        ]));
        let client: crate::core::model_client::SharedModelClient = mock.clone();
        let config = {
            let __cw_config_init_3_0 = Some("read-only".to_string());
            let mut __cw_config_init_3_value: Config = Config::default();
            __cw_config_init_3_value.sandbox_mode = __cw_config_init_3_0;
            __cw_config_init_3_value
        };
        let mut engine_config = deterministic_engine_config(workspace.path());
        engine_config.exec_policy_engine = ask_rule_engine(COMMAND);
        let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
        let task = tokio::spawn(engine.run());
        handle
            .send(external_user_message_op(
                "Create the escalation proof after approval.",
                AppMode::Agent,
                &config,
            ))
            .await
            .expect("send escalation journey");

        let mut approved_result = None;
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("timed out waiting for escalation journey")
                .expect("engine event stream closed");
            match event {
                Event::ApprovalRequired {
                    id,
                    input,
                    description,
                    ..
                } => {
                    assert_eq!(input["sandbox_permissions"], "workspace-write");
                    assert!(
                        description
                            .contains("the exact command writes the requested workspace proof"),
                        "{description}"
                    );
                    if tool == "bash" {
                        assert!(
                            description.contains("Additional approval gate"),
                            "the sandbox grant must not hide the typed ask rule: {description}"
                        );
                        assert!(
                            description.contains("Typed ask rule"),
                            "the typed ask rule must not hide the sandbox grant: {description}"
                        );
                    }
                    assert!(
                        !workspace.path().join("escalation.txt").exists(),
                        "approval must happen before execution"
                    );
                    handle
                        .approve_tool_call(&id)
                        .await
                        .expect("approve exact escalated call");
                }
                Event::ToolCallComplete {
                    model_call: Some(model_call),
                    result,
                    ..
                } if model_call.provider_id == "call-escalated-bash" => {
                    approved_result = Some(result.expect("approved escalation result"));
                }
                Event::TurnComplete { status, error, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                    break;
                }
                _ => {}
            }
        }
        drop(rx);

        let result = approved_result.expect("paired escalated tool result");
        assert!(result.success, "{result:?}");
        assert!(
            result
                .content
                .contains("approved by the user with an adjusted execution policy"),
            "{}",
            result.content
        );
        assert!(workspace.path().join("escalation.txt").exists());
        assert_eq!(mock.call_count(), 2);
        handle.send(Op::Shutdown).await.expect("shutdown engine");
        task.await.expect("engine task");
    }
}

#[tokio::test]
async fn sandbox_escalation_fails_closed_when_the_posture_cannot_prompt() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    for (approval_mode, auto_approve, posture, expected_denial) in [
        (
            ApprovalMode::Auto,
            false,
            "Auto-Review",
            "Sandbox escalation requires a one-shot user approval",
        ),
        (
            ApprovalMode::Suggest,
            true,
            "Full Access",
            "requires a one-shot user approval",
        ),
    ] {
        for tool in ["bash", CODE_EXECUTION_TOOL_NAME, JS_EXECUTION_TOOL_NAME] {
            let mut input = if tool == CODE_EXECUTION_TOOL_NAME {
                json!({"code": "open('escalation.txt', 'w').write('denied')"})
            } else if tool == JS_EXECUTION_TOOL_NAME {
                json!({"code": "require('fs').writeFileSync('escalation.txt', 'denied')"})
            } else {
                json!({"command": "echo denied > escalation.txt"})
            };
            input["sandbox_permissions"] = json!("workspace-write");
            input["justification"] = json!("this execution needs workspace write access");
            let workspace = tempdir().expect("tempdir");
            let mock = std::sync::Arc::new(MockLlmClient::new(vec![
                canned::tool_call_turn("call-unattended-escalation", tool, &input.to_string()),
                canned::simple_text_turn("Escalation was unavailable."),
            ]));
            if matches!(approval_mode, ApprovalMode::Auto) {
                // Let Auto-Review's independent guardian approve the bounded
                // fixture call so this test reaches the separate rule under test:
                // unattended postures still cannot mint a sandbox escalation.
                mock.push_message_response(guardian_fixture_response(
                    r#"{"risk_level":"low","decision":"allow","reason":"isolated fixture write"}"#,
                ));
            }
            let client: crate::core::model_client::SharedModelClient = mock.clone();
            let config = {
                let __cw_config_init_4_0 = Some("read-only".to_string());
                let mut __cw_config_init_4_value: Config = Config::default();
                __cw_config_init_4_value.sandbox_mode = __cw_config_init_4_0;
                __cw_config_init_4_value
            };
            let (engine, handle) = Engine::new_with_model_client(
                deterministic_engine_config(workspace.path()),
                &config,
                client,
            );
            let task = tokio::spawn(engine.run());
            let mut op = external_user_message_op(
                "Do not pause for an unattended escalation.",
                AppMode::Agent,
                &config,
            );
            let Op::SendMessage(TurnSpec {
                approval_mode: op_approval_mode,
                auto_approve: op_auto_approve,
                ..
            }) = &mut op
            else {
                panic!("user message op")
            };
            *op_approval_mode = approval_mode;
            *op_auto_approve = auto_approve;
            handle.send(op).await.expect("send unattended escalation");

            let mut saw_denial = false;
            let mut rx = handle.rx_event.write().await;
            loop {
                let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                    .await
                    .expect("timed out waiting for unattended escalation")
                    .expect("engine event stream closed");
                match event {
                    Event::ApprovalRequired { .. } => {
                        panic!("{posture} must not open an escalation prompt")
                    }
                    Event::ToolCallComplete {
                        model_call: Some(model_call),
                        result,
                        ..
                    } if model_call.provider_id == "call-unattended-escalation" => {
                        let error = result.expect_err("unattended escalation must be denied");
                        assert!(error.to_string().contains(expected_denial), "{error}");
                        assert!(error.to_string().contains(posture), "{error}");
                        saw_denial = true;
                    }
                    Event::TurnComplete { status, error, .. } => {
                        assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                        break;
                    }
                    _ => {}
                }
            }
            drop(rx);

            assert!(saw_denial);
            assert!(!workspace.path().join("escalation.txt").exists());
            handle.send(Op::Shutdown).await.expect("shutdown engine");
            task.await.expect("engine task");
        }
    }
}

#[tokio::test]
async fn productive_tool_results_do_not_hit_no_user_input_backstop() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    const TOOL_ROUNDS: usize = 201;
    const FINAL_ANSWER: &str = "All productive tool rounds completed.";

    let workspace = tempdir().expect("tempdir");
    let mut turns = Vec::with_capacity(TOOL_ROUNDS + 1);
    for index in 1..=TOOL_ROUNDS {
        let fixture = format!("fixture-{index}.txt");
        fs::write(
            workspace.path().join(&fixture),
            format!("productive-round-{index}\n"),
        )
        .expect("write distinct read fixture");
        turns.push(canned::tool_call_turn(
            &format!("call-read-{index}"),
            "File",
            &format!(r#"{{"action":"read","path":"{fixture}"}}"#),
        ));
    }
    turns.push(canned::simple_text_turn(FINAL_ANSWER));

    let mock = std::sync::Arc::new(MockLlmClient::new(turns));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Read every distinct fixture, then report completion.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send productive tool trajectory");

    let mut successful_tool_ids = HashSet::new();
    let mut saw_final_answer = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for productive tool trajectory")
    {
        match event {
            Event::ToolCallComplete {
                id, name, result, ..
            } if name == "File" => {
                let result = result.expect("File.read result");
                assert!(result.success, "{id}: {result:?}");
                assert!(
                    successful_tool_ids.insert(id.clone()),
                    "tool id completed twice: {id}"
                );
            }
            Event::MessageDelta { content, .. } => {
                saw_final_answer |= content.contains(FINAL_ANSWER);
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                assert_eq!(
                    successful_tool_ids.len(),
                    TOOL_ROUNDS,
                    "turn completed before all productive tool rounds"
                );
                assert_eq!(
                    mock.call_count(),
                    TOOL_ROUNDS + 1,
                    "productive work must finish beyond the former 200-step default"
                );
                assert!(
                    saw_final_answer,
                    "turn completed before the final assistant text arrived"
                );
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    assert_eq!(mock.remaining_turns(), 0, "the final turn must be consumed");
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn eight_identical_sequential_reads_all_execute_before_final_answer() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    const READ_ROUNDS: usize = 8;
    const FINAL_ANSWER: &str = "Repeated observation complete.";

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("state.txt"), "stable-observation\n")
        .expect("write repeated-read fixture");
    let mut turns = Vec::with_capacity(READ_ROUNDS + 1);
    for index in 1..=READ_ROUNDS {
        turns.push(canned::tool_call_turn(
            &format!("call-identical-read-{index}"),
            "File",
            r#"{"action":"read","path":"state.txt"}"#,
        ));
    }
    turns.push(canned::simple_text_turn(FINAL_ANSWER));

    let mock = std::sync::Arc::new(MockLlmClient::new(turns));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Read state.txt until you have enough evidence, then report completion.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send repeated-read trajectory");

    let mut completed_ids = HashSet::new();
    let mut saw_final_answer = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for repeated-read trajectory")
    {
        match event {
            Event::ToolCallComplete {
                id, name, result, ..
            } if name == "File" => {
                let result = result.expect("File.read result");
                assert!(result.success, "{id}: {result:?}");
                assert!(result.content.contains("stable-observation"));
                assert!(completed_ids.insert(id), "tool id completed twice");
            }
            Event::MessageDelta { content, .. } => {
                saw_final_answer |= content.contains(FINAL_ANSWER);
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                assert_eq!(completed_ids.len(), READ_ROUNDS);
                assert_eq!(mock.call_count(), READ_ROUNDS + 1);
                assert!(saw_final_answer);
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    assert_eq!(mock.remaining_turns(), 0);
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

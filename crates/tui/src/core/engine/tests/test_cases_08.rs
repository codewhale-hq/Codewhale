#[tokio::test]
async fn max_steps_exhaustion_fails_as_budget_never_completed() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("state.txt"), "still-working\n").expect("write fixture");
    // The model keeps tool-calling past the 1-step budget; it never gets to
    // produce a final answer.
    let turns = vec![
        canned::tool_call_turn(
            "call-step-1",
            "File",
            r#"{"action":"read","path":"state.txt"}"#,
        ),
        canned::tool_call_turn(
            "call-step-2",
            "File",
            r#"{"action":"read","path":"state.txt"}"#,
        ),
    ];
    let mock = std::sync::Arc::new(MockLlmClient::new(turns));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let engine_config = EngineConfig {
        max_steps: 1,
        ..deterministic_engine_config(workspace.path())
    };
    let (engine, handle) = Engine::new_with_model_client(engine_config, &Config::default(), client);
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Keep reading until done.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send step-budget trajectory");

    let mut rx = handle.rx_event.write().await;
    let (status, error) = loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("timed out waiting for step-budget trajectory")
            .expect("engine event");
        if let Event::TurnComplete { status, error, .. } = event {
            break (status, error);
        }
    };
    drop(rx);

    assert_eq!(
        status,
        TurnOutcomeStatus::Failed,
        "step-budget exhaustion must never report Completed"
    );
    let error = error.expect("step-budget exhaustion must carry a terminal error");
    assert!(error.contains("Maximum model steps"), "{error}");

    // The terminal error reduces to BudgetExhausted for machine consumers —
    // this is the same reduction the headless exec receipt applies, and it is
    // what gates persistent-service release on Completed-only turns.
    let category = crate::error_taxonomy::classify_error_message(&error);
    assert_eq!(category, ErrorCategory::Budget);
    assert_eq!(
        crate::core::termination::classify_turn_termination(status, Some(category), false, false),
        crate::core::termination::RunTerminationReason::BudgetExhausted
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn goal_turn_uses_goal_step_allowance_and_pauses_budget_limit_after_final_report() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let config = goal_custom_route_config();
    // The model makes one tool step (consuming the 1-step goal allowance),
    // then writes its bounded final report when granted it.
    let turns = vec![
        canned::tool_call_turn(
            "call-step-1",
            "File",
            r#"{"action":"read","path":"state.txt"}"#,
        ),
        canned::simple_text_turn("final report: one step of progress made"),
    ];
    let mock = std::sync::Arc::new(MockLlmClient::new(turns));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("state.txt"), "still-working\n").expect("write fixture");
    let (engine, handle) = Engine::new_with_model_client(
        EngineConfig {
            model: "local-model".to_string(),
            workspace: workspace.path().to_path_buf(),
            max_steps: 200,
            goal_max_steps: Some(1),
            goal_objective: Some("finish the migration".to_string()),
            snapshots_enabled: false,
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &config,
        client,
    );
    let goal_state = engine.config.goal_state.clone();
    let task = tokio::spawn(engine.run());
    handle
        .send(active_goal_message_op(
            &config,
            "Work on the goal.",
            "finish the migration",
            None,
        ))
        .await
        .expect("send goal-budget trajectory");

    let mut rx = handle.rx_event.write().await;
    let (status, _) = loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("timed out waiting for goal-budget trajectory")
            .expect("engine event");
        if let Event::TurnComplete { status, error, .. } = event {
            break (status, error);
        }
    };
    drop(rx);

    // The final report closes the turn cleanly; the unfinished goal then
    // pauses BudgetLimit instead of re-arming another full goal turn (#5994).
    assert_eq!(status, TurnOutcomeStatus::Completed);
    let snapshot = goal_state.lock().expect("goal lock").snapshot();
    assert_eq!(snapshot.status, "paused");
    assert_eq!(
        snapshot.pause_reason,
        Some(crate::tools::goal::GoalPauseReason::BudgetLimit)
    );
    // The mock served exactly the tool step plus the final report; any
    // re-armed continuation would have needed a third provider turn.
    assert_eq!(mock.call_count(), 2);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn interactive_turn_keeps_ordinary_ceiling_when_goal_allowance_is_configured() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("state.txt"), "still-working\n").expect("write fixture");
    // No active goal: the [goal] allowance must never raise the ordinary
    // interactive ceiling.
    let turns = vec![
        canned::tool_call_turn(
            "call-step-1",
            "File",
            r#"{"action":"read","path":"state.txt"}"#,
        ),
        canned::tool_call_turn(
            "call-step-2",
            "File",
            r#"{"action":"read","path":"state.txt"}"#,
        ),
    ];
    let mock = std::sync::Arc::new(MockLlmClient::new(turns));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let engine_config = EngineConfig {
        max_steps: 1,
        goal_max_steps: Some(1_000),
        ..deterministic_engine_config(workspace.path())
    };
    let (engine, handle) = Engine::new_with_model_client(engine_config, &Config::default(), client);
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Keep reading until done.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send interactive trajectory");

    let mut rx = handle.rx_event.write().await;
    let (status, error) = loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("timed out waiting for interactive trajectory")
            .expect("engine event");
        if let Event::TurnComplete { status, error, .. } = event {
            break (status, error);
        }
    };
    drop(rx);

    assert_eq!(status, TurnOutcomeStatus::Failed);
    let error = error.expect("budget exhaustion carries a terminal error");
    assert!(error.contains("limit: 1"), "{error}");
    assert!(error.contains("max_steps"), "{error}");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[test]
fn synthetic_resume_paths_have_no_hidden_default_ceiling() {
    let turn_loop = include_str!("../turn_loop.rs");

    for legacy_marker in ["no_user_input_continues", "no-user-input resume backstop"] {
        assert!(
            !turn_loop.contains(legacy_marker),
            "turn_loop.rs reintroduced the hidden synthetic-resume ceiling marker {legacy_marker:?}"
        );
    }
}

#[tokio::test]
async fn injected_model_duplicate_reads_both_execute_and_close_both_tool_ids() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(workspace.path().join("README.md"), "duplicate-read-proof\n").expect("write fixture");
    let duplicate_read_turn = vec![
        canned::message_start("mock_msg_duplicate_read"),
        canned::tool_use_block_start(0, "call-read-1", "File"),
        canned::tool_input_delta(0, r#"{"action":"read","path":"README.md"}"#),
        canned::block_stop(0),
        canned::tool_use_block_start(1, "call-read-2", "File"),
        canned::tool_input_delta(1, r#"{"action":"read","path":"README.md"}"#),
        canned::block_stop(1),
        canned::message_delta("tool_use", None),
        canned::message_stop(),
    ];
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        duplicate_read_turn,
        canned::simple_text_turn("Duplicate read complete."),
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
            "Issue the duplicate read batch.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send duplicate-read trajectory");

    let mut results = HashMap::new();
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for duplicate-read trajectory")
    {
        match event {
            Event::ToolCallComplete {
                model_call: Some(model_call),
                name,
                result,
                ..
            } if name == "File" => {
                results.insert(model_call.provider_id, result.expect("read result"));
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    assert_eq!(results.len(), 2, "every tool ID needs one terminal result");
    assert!(
        results["call-read-1"]
            .content
            .contains("duplicate-read-proof")
    );
    assert!(
        results["call-read-2"]
            .content
            .contains("duplicate-read-proof")
    );
    assert!(
        results.values().all(|result| result
            .metadata
            .as_ref()
            .is_none_or(|metadata| { metadata.get("executed").is_none() })),
        "neither model-requested read may be replaced with a synthetic receipt"
    );

    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 2);
    let result_ids = requests[1]
        .messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(result_ids.contains(&"call-read-1"));
    assert!(result_ids.contains(&"call-read-2"));

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn native_navigation_tracks_only_returned_references_in_the_real_model_request() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    for expose in [false, true] {
        let workspace = tempdir().expect("tempdir");
        fs::write(
            workspace.path().join("nav.rs"),
            "fn wanted() {\n    let _ = \"PRIVATE_BODY\";\n}\n",
        )
        .unwrap();
        let selection = if expose {
            "return {selected: r.metadata.navigation ? r.metadata.navigation.symbols[0].read : null};"
        } else {
            "return {done: true};"
        };
        let code = format!(
            "const r = await tools.call('read', {{path: 'nav.rs', limit: 1}}); {selection}"
        );
        let mock = std::sync::Arc::new(MockLlmClient::new(vec![
            canned::tool_call_turn(
                "navigation-program",
                "execute_tools",
                &serde_json::json!({"code": code}).to_string(),
            ),
            canned::simple_text_turn("Navigation received."),
        ]));
        let client: crate::core::model_client::SharedModelClient = mock.clone();
        let (mut engine, _handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &Config::default(),
            client,
        );
        let context = crate::tools::ToolContext::new(workspace.path());
        let mut registry = crate::tools::ToolRegistry::new(context);
        registry.register(std::sync::Arc::new(crate::tools::file_tool::ReadTool));
        let mut tools = registry.to_api_tools_with_cache(true);
        let mut compose = crate::tools::codemode::execute_tools_tool_definition();
        compose.defer_loading = Some(false);
        tools.push(compose);
        let surface = test_tool_surface(&engine, registry, Some(tools), AppMode::Agent);
        let mut turn = crate::core::turn::TurnContext::new(8);
        let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
        assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
        let requests = mock.captured_requests();
        assert_eq!(requests.len(), 2);
        let delivered = requests[1]
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .find_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } if tool_use_id == "navigation-program" => Some(content),
                _ => None,
            })
            .expect("actual outer result in the second model request");
        let payload: serde_json::Value = serde_json::from_str(delivered).unwrap();
        assert!(payload["success"].as_bool().unwrap(), "{payload}");
        assert!(
            !delivered.contains("PRIVATE_BODY"),
            "an intermediate read is not model delivery"
        );
        let state = serde_json::to_value(&engine.session.working_set).unwrap();
        let refs = state["delivered_code_refs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if expose {
            let selected = &payload["body"]["return"]["selected"];
            assert_eq!(
                selected["path"], "nav.rs",
                "composition must return an actionable reference"
            );
            assert_eq!(
                refs,
                vec![selected.clone()],
                "record exactly what survived outbound projection"
            );
            let mut restored = crate::working_set::WorkingSet::default();
            restored.rebuild_from_messages(&engine.session.messages, workspace.path());
            assert_eq!(
                serde_json::to_value(restored).unwrap()["delivered_code_refs"],
                serde_json::json!(refs),
                "resume reconstructs delivered references from the logged conversation, not the physical cache"
            );
        } else {
            assert!(
                refs.is_empty(),
                "hidden program reads must not seed delivery state"
            );
        }
    }
}

#[tokio::test]
async fn duplicate_raw_read_errors_each_touch_the_working_set() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    async fn missing_read_touches(read_count: usize) -> u32 {
        let workspace = tempdir().expect("tempdir");
        let mut read_turn = vec![canned::message_start("mock_msg_missing_read")];
        for index in 0..read_count {
            let block_index = u32::try_from(index).expect("test read count fits u32");
            let tool_id = format!("call-missing-{}", index + 1);
            read_turn.push(canned::tool_use_block_start(
                block_index,
                &tool_id,
                "read_file",
            ));
            read_turn.push(canned::tool_input_delta(
                block_index,
                r#"{"path":"missing.rs"}"#,
            ));
            read_turn.push(canned::block_stop(block_index));
        }
        read_turn.push(canned::message_delta("tool_use", None));
        read_turn.push(canned::message_stop());

        let mock = std::sync::Arc::new(MockLlmClient::new(vec![
            read_turn,
            canned::simple_text_turn("Missing read handled."),
        ]));
        let client: crate::core::model_client::SharedModelClient = mock;
        let (mut engine, _handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &Config::default(),
            client,
        );
        let context = crate::tools::ToolContext::new(workspace.path().to_path_buf());
        let mut registry = crate::tools::ToolRegistry::new(context);
        registry.register(std::sync::Arc::new(crate::tools::file::ReadFileTool));
        let tools = Some(registry.to_api_tools_with_cache(true));
        let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
        let mut turn = crate::core::turn::TurnContext::new(8);

        let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;

        assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
        engine
            .session
            .working_set
            .entries
            .get("missing.rs")
            .expect("leader error should record the attempted path")
            .touches
    }

    let baseline_touches = missing_read_touches(1).await;
    let duplicate_touches = missing_read_touches(2).await;
    assert_eq!(
        duplicate_touches,
        baseline_touches.saturating_mul(2),
        "each model-requested read must execute and record its own observation"
    );
}

#[tokio::test]
async fn truncated_response_continues_turn() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let truncated_turn = vec![
        canned::message_start("mock_msg_truncated_continue"),
        canned::text_block_start(0),
        canned::text_delta(0, "Partial answer before the output budget ran out"),
        canned::block_stop(0),
        canned::message_delta(
            "max_output_tokens",
            Some(Usage {
                input_tokens: 41,
                output_tokens: 7,
                reasoning_tokens: Some(3),
                ..Default::default()
            }),
        ),
        canned::message_stop(),
    ];
    let followup_turn = canned::simple_text_turn("Continued after the truncation.");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![truncated_turn, followup_turn]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Answer the question.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send truncated-then-continue trajectory");

    let mut saw_turn_usage = false;
    let mut saw_truncation_observation = false;
    let mut last_session_messages = None;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for truncated-then-continue trajectory")
    {
        match event {
            Event::TurnUsage {
                usage: reported, ..
            } => {
                assert_eq!(reported.input_tokens, 41);
                assert_eq!(reported.output_tokens, 7);
                assert_eq!(reported.reasoning_tokens, Some(3));
                saw_turn_usage = true;
            }
            Event::SessionUpdated { messages, .. } => {
                saw_truncation_observation |= messages.iter().any(|message| {
                    message.content.iter().any(|block| {
                        matches!(
                            block,
                            ContentBlock::Text { text, .. }
                                if text.contains("output limit")
                                    && text.contains("Continue from where you left off")
                        )
                    })
                });
                last_session_messages = Some(messages);
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                assert!(error.is_none(), "no terminal error expected: {error:?}");
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    assert!(
        saw_turn_usage,
        "reported usage must be accounted before the turn continues"
    );
    assert!(
        saw_truncation_observation,
        "the truncation must be surfaced to the model as a bounded observation"
    );
    let messages = last_session_messages.expect("session updated after truncation");
    assert!(
        messages.iter().any(|message| {
            message.role == "assistant"
                && message.content.iter().any(|block| {
                    matches!(
                        block,
                        ContentBlock::Text { text, .. }
                            if text.contains("Partial answer before the output budget ran out")
                    )
                })
        }),
        "the partial text must be accepted as a completed assistant message"
    );

    let requests = mock.captured_requests();
    assert_eq!(
        requests.len(),
        2,
        "the loop must continue with a follow-up request"
    );
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn injected_chat_content_filter_never_becomes_a_completed_answer() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let filtered_turn = vec![
        canned::message_start("mock_msg_content_filter"),
        canned::text_block_start(0),
        canned::text_delta(0, "Provider returned only a partial fragment"),
        canned::block_stop(0),
        canned::message_delta(
            "content_filter",
            Some(Usage {
                input_tokens: 17,
                output_tokens: 4,
                ..Default::default()
            }),
        ),
        canned::message_stop(),
    ];
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![filtered_turn]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Answer the question.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send content-filter trajectory");

    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for content-filter trajectory")
    {
        match event {
            Event::MessageComplete { .. } => {
                panic!("content-filtered text must not be marked completed")
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Failed);
                let error = error.expect("content-filtered turn needs a terminal error");
                assert!(error.contains("Model response incomplete"), "{error}");
                assert!(error.contains("content_filter"), "{error}");
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    assert_eq!(mock.captured_requests().len(), 1);
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn injected_model_complete_tool_block_at_max_output_tokens_executes() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    fs::write(
        workspace.path().join("truncated-tool-ran.txt"),
        "truncated-tool-executed",
    )
    .expect("write fixture");
    let usage = Usage {
        input_tokens: 52,
        output_tokens: 23,
        ..Default::default()
    };
    let truncated_tool_turn = vec![
        canned::message_start("mock_msg_truncated_tool"),
        canned::tool_use_block_start(0, "call-truncated", "File"),
        canned::tool_input_delta(0, r#"{"action":"read","path":"truncated-tool-ran.txt"}"#),
        canned::block_stop(0),
        canned::message_delta("max_output_tokens", Some(usage.clone())),
        canned::message_stop(),
    ];
    let followup_turn = canned::simple_text_turn("Done.");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![truncated_tool_turn, followup_turn]));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Read the fixture file.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send truncated-tool trajectory");

    let mut saw_turn_usage = false;
    let mut saw_tool_start = false;
    let mut saw_tool_success = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for truncated-tool trajectory")
    {
        match event {
            Event::TurnUsage { .. } => saw_turn_usage = true,
            Event::ToolCallStarted {
                id,
                name,
                model_call,
                ..
            } => {
                assert_ne!(id, "call-truncated");
                assert_eq!(model_call.unwrap().provider_id, "call-truncated");
                assert_eq!(name, "File");
                saw_tool_start = true;
            }
            Event::ToolCallComplete {
                id,
                name,
                result,
                model_call,
            } => {
                assert_ne!(id, "call-truncated");
                assert_eq!(model_call.unwrap().provider_id, "call-truncated");
                assert_eq!(name, "File");
                let result = result.expect("complete tool call closes with a tool result");
                assert!(
                    result.success,
                    "complete tool call must be accepted and executed: {result:?}"
                );
                saw_tool_success = true;
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                assert!(error.is_none(), "no terminal error expected: {error:?}");
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    assert!(saw_turn_usage, "reported usage must be emitted");
    assert!(saw_tool_start, "the streamed tool lifecycle must open");
    assert!(
        saw_tool_success,
        "the complete tool call must be accepted and executed"
    );
    let requests = mock.captured_requests();
    assert_eq!(
        requests.len(),
        2,
        "the loop must continue with a follow-up request after the tool result"
    );
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn injected_model_receives_malformed_tool_feedback_and_recovers() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    let workspace = tempdir().expect("tempdir");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-bad-read", "File", r#"{"action":"read"}"#),
        canned::simple_text_turn("Recovered after validation feedback."),
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
            "Exercise malformed tool feedback.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send malformed trajectory");

    let mut validation_feedback = None;
    let mut recovered = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for malformed trajectory")
    {
        match event {
            Event::ToolCallComplete { name, result, .. } if name == "File" => {
                validation_feedback = Some(match result {
                    Ok(result) => result.content,
                    Err(error) => error.to_string(),
                });
            }
            Event::MessageDelta { content, .. } => {
                recovered |= content.contains("Recovered after validation feedback");
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    drop(rx);
    let feedback = validation_feedback.expect("validation feedback event");
    assert!(feedback.to_ascii_lowercase().contains("path"), "{feedback}");
    assert!(
        recovered,
        "model must get a follow-up turn after tool failure"
    );
    assert_eq!(mock.call_count(), 2);
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn engine_cancellation_drops_active_injected_model_request() {
    let workspace = tempdir().expect("tempdir");
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let request_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client: crate::core::model_client::SharedModelClient =
        std::sync::Arc::new(BlockingModelClient {
            entered: std::sync::Arc::clone(&entered),
            request_dropped: std::sync::Arc::clone(&request_dropped),
        });
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(external_user_message_op(
            "Block until explicitly cancelled.",
            AppMode::Agent,
            &Config::default(),
        ))
        .await
        .expect("send cancellation trajectory");
    tokio::time::timeout(model_turn_event_timeout(), entered.notified())
        .await
        .expect("model request was never entered");

    let mut rx = handle.rx_event.write().await;
    let pending_snapshot = loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("timed out waiting for pending request snapshot")
            .expect("engine event");
        if let Event::ToolRequestSnapshot { snapshot } = event {
            break snapshot;
        }
    };
    assert!(pending_snapshot.delivery_status.starts_with("unknown"));
    assert!(pending_snapshot.tools_field_present);
    drop(rx);
    handle.cancel();

    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for cancellation")
    {
        if let Event::TurnComplete { status, error, .. } = event {
            assert_eq!(status, TurnOutcomeStatus::Interrupted, "{error:?}");
            break;
        }
    }
    drop(rx);
    assert!(
        request_dropped.load(std::sync::atomic::Ordering::SeqCst),
        "cancellation must drop the active provider future"
    );
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

fn guardian_fixture_response(text: &str) -> codewhale_models::MessageResponse {
    codewhale_models::MessageResponse {
        id: "guardian-fixture".to_string(),
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
        model: "mock-model".to_string(),
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        container: None,
        usage: Usage {
            input_tokens: 17,
            output_tokens: 3,
            ..Usage::default()
        },
    }
}

fn guardian_tool_results<'a>(
    request: &'a codewhale_models::MessageRequest,
    call_id: &str,
) -> Vec<(&'a str, Option<bool>)> {
    request
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } if tool_use_id == call_id => Some((content.as_str(), *is_error)),
            _ => None,
        })
        .collect()
}

/// One transcript-visible gate receipt observed on the event stream.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GateReceipt {
    gate: crate::core::events::ToolGate,
    decision: crate::core::events::ToolGateVerdict,
    risk: Option<String>,
    reason: String,
}

async fn collect_guardian_journey_with_receipts(
    handle: &EngineHandle,
    call_id: &str,
) -> (
    Result<crate::tools::spec::ToolResult, crate::tools::spec::ToolError>,
    Vec<Usage>,
    Usage,
    Vec<GateReceipt>,
) {
    let mut completion = None;
    let mut execution_id = None;
    let mut usage_events = Vec::new();
    let mut receipts = Vec::new();
    let mut rx = handle.rx_event.write().await;
    let terminal_usage = loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
            .await
            .expect("timed out waiting for Auto-Review journey")
            .expect("engine event stream closed");
        match event {
            Event::ToolCallStarted {
                id,
                model_call: Some(model_call),
                ..
            } if model_call.provider_id == call_id => {
                execution_id = Some(id);
            }
            Event::ToolCallComplete {
                model_call: Some(model_call),
                result,
                ..
            } if model_call.provider_id == call_id => {
                assert!(
                    completion.replace(result).is_none(),
                    "duplicate tool result"
                );
            }
            // Guardian consults carry their own routed receipt; both the
            // parent-route and routed per-call telemetry count as reaching
            // the cost UI.
            Event::TurnUsage { usage, .. } | Event::RoutedTurnUsage { usage, .. } => {
                usage_events.push(usage);
            }
            Event::ToolGateDecision {
                tool_id,
                gate,
                decision,
                risk,
                reason,
                ..
            } if execution_id.as_deref() == Some(tool_id.as_str()) => receipts.push(GateReceipt {
                gate,
                decision,
                risk,
                reason,
            }),
            Event::TurnComplete {
                status,
                error,
                usage,
                ..
            } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break usage;
            }
            _ => {}
        }
    };
    drop(rx);
    (
        completion.expect("held call must have one paired result"),
        usage_events,
        terminal_usage,
        receipts,
    )
}

#[cfg(windows)]
#[test]
fn windows_auto_review_node_image_kill_is_denied_before_any_shell_effect() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    with_artifact_home(|home| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let workspace = home.join("windows-node-floor");
                fs::create_dir(&workspace).expect("fixture workspace");
                let sentinel = workspace.join("must-not-run.txt");
                // The scanner deliberately holds commands inside conditionals.
                // Keep the kill unreachable even if this regression breaks:
                // failing the guard may write the sentinel, but must never kill
                // the developer's other Node or Codewhale processes.
                let command = "powershell -NoProfile -Command \"Set-Content -LiteralPath 'must-not-run.txt' -Value 'ran'; if ($false) { taskkill /F /IM node.exe }\"";
                let arguments = json!({"command": command}).to_string();
                let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::tool_call_turn(
                    "windows-node-kill",
                    "bash",
                    &arguments,
                )]));
                mock.push_factory(|request| {
                    let results = guardian_tool_results(request, "windows-node-kill");
                    assert_eq!(results.len(), 1, "one provider ID gets one result");
                    assert_eq!(results[0].1, Some(true));
                    assert!(results[0].0.contains("Node launcher"), "{results:?}");
                    assert!(results[0].0.contains("do not work around"), "{results:?}");
                    canned::simple_text_turn("Use an owned server PID instead.")
                });
                let config = Config { allow_shell: Some(true), ..Config::default() };
                let (engine, handle) = Engine::new_with_model_client(
                    deterministic_engine_config(&workspace), &config, mock.clone(),
                );
                let op = auto_review_message_op("Attempt the supplied cleanup.", &config);
                let task = tokio::spawn(engine.run());
                handle.send(op).await.expect("send Auto-Review trajectory");
                let (completion, _, _, receipts) = collect_guardian_journey_with_receipts(&handle, "windows-node-kill").await;
                let error = completion.expect_err("runtime safety floor must deny before shell execution");
                assert!(matches!(error, crate::tools::spec::ToolError::PermissionDenied { .. }));
                assert!(error.to_string().contains("Node launcher"), "{error}");
                assert!(!sentinel.exists(), "no part of the composed shell call may run");
                assert_eq!(receipts.len(), 1, "{receipts:?}");
                assert_eq!(receipts[0].gate, crate::core::events::ToolGate::AutoReviewDeterministic);
                assert_eq!(receipts[0].decision, crate::core::events::ToolGateVerdict::Denied);
                assert!(receipts[0].reason.contains("Node launcher"));
                assert_eq!(mock.captured_requests().len(), 2, "main and paired-result follow-up only; no guardian");
                handle.send(Op::Shutdown).await.expect("shutdown engine");
                tokio::time::timeout(model_turn_event_timeout(), task).await.expect("bounded shutdown").expect("engine task");
            });
    });
}

#[tokio::test]
async fn auto_review_guardian_allow_executes_once_and_accounts_usage_without_prompt_leak() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    const REVIEW_REASON: &str = "bounded fixture write is reversible";
    let workspace = tempdir().expect("tempdir");
    fs::create_dir(workspace.path().join(".git")).expect("git marker");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::tool_call_turn(
        "call-guardian-allow",
        "File",
        r#"{"action":"write","path":".env","content":"assembled=true\n"}"#,
    )]));
    mock.push_factory(|request| {
        let tool_results = guardian_tool_results(request, "call-guardian-allow");
        assert_eq!(tool_results.len(), 1, "one call must produce one result");
        assert_ne!(tool_results[0].1, Some(true));
        let request_json = serde_json::to_string(request).expect("serialize follow-up request");
        assert!(!request_json.contains(REVIEW_REASON), "{request_json}");
        assert!(
            !request_json.contains("deterministic_observations"),
            "{request_json}"
        );
        assert!(!request_json.contains("hold_reason"), "{request_json}");
        canned::simple_text_turn("Guardian-approved write complete.")
    });
    mock.push_message_response(guardian_fixture_response(&format!(
        r#"{{"risk_level":"low","decision":"allow","reason":"{REVIEW_REASON}"}}"#
    )));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let config = Config::default();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(auto_review_message_op(
            "Write the isolated fixture.",
            &config,
        ))
        .await
        .expect("send Auto-Review allow journey");

    let (completion, usage_events, terminal_usage, receipts) =
        collect_guardian_journey_with_receipts(&handle, "call-guardian-allow").await;
    assert!(completion.expect("guardian-approved tool result").success);
    // The person never saw a prompt, so the transcript gets exactly one
    // receipt naming the guardian's verdict and risk tier.
    assert_eq!(receipts.len(), 1, "{receipts:?}");
    assert_eq!(
        receipts[0].gate,
        crate::core::events::ToolGate::AutoReviewGuardian
    );
    assert_eq!(
        receipts[0].decision,
        crate::core::events::ToolGateVerdict::Allowed
    );
    assert!(receipts[0].risk.is_some(), "{receipts:?}");
    assert!(!receipts[0].reason.contains('\n'));
    assert!(
        usage_events
            .iter()
            .any(|usage| usage.input_tokens == 17 && usage.output_tokens == 3),
        "guardian usage must reach the cost UI"
    );
    assert_eq!(terminal_usage.input_tokens, 17);
    assert_eq!(terminal_usage.output_tokens, 3);
    assert_eq!(
        fs::read_to_string(workspace.path().join(".env")).expect("written fixture"),
        "assembled=true\n"
    );
    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 3, "main, guardian, follow-up");
    assert_eq!(requests[1].stream, Some(false));
    assert!(requests[1].tools.is_none());
    let guardian_json = serde_json::to_string(&requests[1]).expect("guardian request JSON");
    assert!(guardian_json.contains("call") || guardian_json.contains("proposed_tool_call"));
    assert!(guardian_json.contains(".env"));

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn auto_review_guardian_deny_returns_one_paired_failed_result() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    const DENIAL: &str = "sensitive configuration must remain untouched";
    let workspace = tempdir().expect("tempdir");
    fs::create_dir(workspace.path().join(".git")).expect("git marker");
    let mock = std::sync::Arc::new(MockLlmClient::new(vec![canned::tool_call_turn(
        "call-guardian-deny",
        "File",
        r#"{"action":"write","path":".env","content":"must-not-run\n"}"#,
    )]));
    mock.push_factory(|request| {
        let tool_results = guardian_tool_results(request, "call-guardian-deny");
        assert_eq!(tool_results.len(), 1, "denied call must not be orphaned");
        assert_eq!(tool_results[0].1, Some(true));
        assert!(tool_results[0].0.contains(DENIAL), "{tool_results:?}");
        assert!(
            tool_results[0].0.contains("Do not work around this denial"),
            "{tool_results:?}"
        );
        let request_json = serde_json::to_string(request).expect("serialize follow-up request");
        assert!(
            !request_json.contains("deterministic_observations"),
            "{request_json}"
        );
        assert!(!request_json.contains("hold_reason"), "{request_json}");
        canned::simple_text_turn("Stopped after the guardian denial.")
    });
    mock.push_message_response(guardian_fixture_response(&format!(
        r#"{{"risk_level":"medium","decision":"deny","reason":"{DENIAL}"}}"#
    )));
    let client: crate::core::model_client::SharedModelClient = mock.clone();
    let config = Config::default();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(auto_review_message_op("Attempt the held write.", &config))
        .await
        .expect("send Auto-Review deny journey");

    let (completion, _, _, receipts) =
        collect_guardian_journey_with_receipts(&handle, "call-guardian-deny").await;
    let error = completion.expect_err("guardian denial must fail the tool call");
    assert!(error.to_string().contains(DENIAL), "{error}");
    assert_eq!(receipts.len(), 1, "{receipts:?}");
    assert_eq!(
        receipts[0].decision,
        crate::core::events::ToolGateVerdict::Denied
    );
    assert!(receipts[0].reason.contains(DENIAL), "{receipts:?}");
    assert!(!workspace.path().join(".env").exists());
    assert_eq!(mock.captured_requests().len(), 3);
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
async fn auto_review_guardian_parse_and_transport_failures_deny_closed() {
    use crate::llm_client::mock::{MockLlmClient, canned};

    for failure in ["parse", "transport"] {
        let workspace = tempdir().expect("tempdir");
        fs::create_dir(workspace.path().join(".git")).expect("git marker");
        let call_id = format!("call-guardian-{failure}");
        let initial = canned::tool_call_turn(
            &call_id,
            "File",
            r#"{"action":"write","path":".env","content":"must-not-run\n"}"#,
        );
        let follow_up_id = call_id.clone();
        let follow_up = move |request: &codewhale_models::MessageRequest| {
            let results = guardian_tool_results(request, &follow_up_id);
            assert_eq!(results.len(), 1, "reviewer failure must pair one result");
            let result = results[0];
            assert_eq!(result.1, Some(true));
            assert!(result.0.contains("denied (fail closed)"), "{result:?}");
            assert!(!result.0.contains("fixture guardian transport failure"));
            canned::simple_text_turn("Stopped after reviewer failure.")
        };

        let config = Config::default();
        let client: crate::core::model_client::SharedModelClient = if failure == "parse" {
            let mock = MockLlmClient::new(vec![initial]);
            mock.push_factory(follow_up);
            mock.push_message_response(guardian_fixture_response("not valid guardian JSON"));
            std::sync::Arc::new(mock)
        } else {
            let mock = MockLlmClient::new(vec![initial]);
            mock.push_factory(follow_up);
            std::sync::Arc::new(FailingGuardianModelClient { inner: mock })
        };
        let (engine, handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &config,
            client,
        );
        let task = tokio::spawn(engine.run());
        handle
            .send(auto_review_message_op("Attempt the held write.", &config))
            .await
            .expect("send reviewer failure journey");

        let (completion, _, _, receipts) =
            collect_guardian_journey_with_receipts(&handle, &call_id).await;
        let error = completion.expect_err("reviewer failure must deny");
        assert!(error.to_string().contains("fail closed"), "{error}");
        assert_eq!(receipts.len(), 1, "{receipts:?}");
        assert_eq!(
            receipts[0].decision,
            crate::core::events::ToolGateVerdict::Unavailable,
            "{receipts:?}"
        );
        assert!(receipts[0].risk.is_none());
        assert!(!workspace.path().join(".env").exists());
        handle.send(Op::Shutdown).await.expect("shutdown engine");
        task.await.expect("engine task");
    }
}

#[tokio::test]
async fn auto_review_cancellation_promptly_drops_the_guardian_request() {
    let workspace = tempdir().expect("tempdir");
    fs::create_dir(workspace.path().join(".git")).expect("git marker");
    let guardian_entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let guardian_dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let model = std::sync::Arc::new(BlockingGuardianModelClient {
        guardian_entered: std::sync::Arc::clone(&guardian_entered),
        guardian_dropped: std::sync::Arc::clone(&guardian_dropped),
        streaming_calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = Config::default();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        client,
    );
    let task = tokio::spawn(engine.run());
    handle
        .send(auto_review_message_op("Attempt the held write.", &config))
        .await
        .expect("send blocking guardian journey");
    tokio::time::timeout(model_turn_event_timeout(), guardian_entered.notified())
        .await
        .expect("guardian request was never entered");

    handle.cancel();
    let mut rx = handle.rx_event.write().await;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("guardian cancellation did not settle promptly")
            .expect("engine event stream closed");
        if let Event::TurnComplete { status, error, .. } = event {
            assert_eq!(status, TurnOutcomeStatus::Interrupted, "{error:?}");
            break;
        }
    }
    drop(rx);

    assert!(
        guardian_dropped.load(std::sync::atomic::Ordering::SeqCst),
        "cancellation must drop the guardian provider future"
    );
    assert_eq!(
        model
            .streaming_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert!(!workspace.path().join(".env").exists());
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    task.await.expect("engine task");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn operate_conversation_reaches_provider_when_workers_are_disabled() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-operate\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"I can still answer normally.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-operate\",\"choices\":[{\"index\":0,",
        "\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(done_sse),
        )
        .expect(1)
        .mount(&server)
        .await;

    let api_config = Config {
        ..Config::default()
    }
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let engine_config = EngineConfig {
        workspace: workspace.path().to_path_buf(),
        snapshots_enabled: false,
        subagents_enabled: false,
        ..EngineConfig::default()
    };
    let (operate_engine, operate_handle) = Engine::new(engine_config, &api_config);
    let operate_task = tokio::spawn(operate_engine.run());
    operate_handle
        .send(external_user_message_op(
            "what is a Rust worktree?",
            AppMode::Operate,
            &api_config,
        ))
        .await
        .expect("send Operate turn");

    let mut saw_operate_complete = false;
    let mut saw_operate_route = false;
    let mut operate_rx = operate_handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), operate_rx.recv())
        .await
        .expect("timed out waiting for Operate completion")
    {
        match event {
            Event::RouteDispatched { route, .. } => {
                assert_eq!(route.provider, ProviderKind::Deepseek);
                assert_eq!(route.model, crate::config::DEFAULT_TEXT_MODEL);
                assert!(!route.auto_model);
                saw_operate_route = true;
            }
            Event::Error { envelope, .. } => {
                panic!("ordinary Operate conversation emitted an error: {envelope:?}");
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                saw_operate_complete = true;
                break;
            }
            _ => {}
        }
    }
    drop(operate_rx);

    assert!(
        saw_operate_route,
        "model turns must publish route provenance"
    );
    assert!(
        saw_operate_complete,
        "Operate conversation must complete without worker readiness"
    );
    let requests = server
        .received_requests()
        .await
        .expect("recorded requests after Operate");
    assert_eq!(requests.len(), 1, "Operate must reach the provider");
    operate_handle
        .send(Op::Shutdown)
        .await
        .expect("shutdown Operate engine");
    operate_task.await.expect("Operate engine task");
}

fn auto_review_plan_decision(
    policy: &crate::core::authority::auto_review::AutoReviewPolicy,
    tool_name: &str,
    tool_input: &Value,
    run_origin: crate::core::authority::auto_review::RunOrigin,
    approval_mode: ApprovalMode,
    workspace_trusted: bool,
    workspace: Option<&Path>,
) -> (AutoReviewPlanDecision, Value) {
    let context = crate::core::authority::auto_review::AutoReviewContext::from_tool_call(
        tool_name,
        tool_input,
        run_origin,
        approval_mode,
        workspace_trusted,
        workspace,
    );
    auto_review_plan_decision_for_context(policy, &context)
}

#[test]
fn auto_review_scenario() {
    // Scenario consolidation of: auto_review_classifies_publish_and_holds_without_prompting, auto_review_classifier_allow_executes_without_prompting, auto_review_allows_ordinary_shell_probe_without_prompting, auto_review_routes_unknown_tool_to_reviewer_in_auto, auto_review_policy_blocks_publish_when_approval_is_never, auto_review_allows_ordinary_test_command_without_prompting, auto_review_allows_ordinary_workspace_write_without_prompting, auto_review_routes_unbounded_or_sensitive_workspace_writes_to_reviewer
    // from auto_review_classifies_publish_and_holds_without_prompting
    {
        let (decision, audit) = auto_review_plan_decision(
            &crate::core::authority::auto_review::AutoReviewPolicy::default(),
            "exec_shell",
            &json!({"command": "git push origin main"}),
            crate::core::authority::auto_review::RunOrigin::Interactive,
            ApprovalMode::Auto,
            true,
            None,
        );

        assert_eq!(
            decision,
            AutoReviewPlanDecision::Block(
                "Built-in safety gate requires approval: publish-like action requires durable review"
                    .to_string()
            )
        );
        assert_eq!(audit["action_kind"], "publish");
        assert_eq!(audit["decision"], "hold_for_review");
    }
    // from auto_review_classifier_allow_executes_without_prompting
    {
        let (decision, audit) = auto_review_plan_decision(
            &crate::core::authority::auto_review::AutoReviewPolicy::default(),
            "read_file",
            &json!({"path": "Cargo.toml"}),
            crate::core::authority::auto_review::RunOrigin::Interactive,
            ApprovalMode::Auto,
            true,
            None,
        );

        assert_eq!(decision, AutoReviewPlanDecision::Allow);
        assert_eq!(audit["decision"], "allow");
    }
    // from auto_review_allows_ordinary_shell_probe_without_prompting
    {
        let (decision, audit) = auto_review_plan_decision(
            &crate::core::authority::auto_review::AutoReviewPolicy::default(),
            "exec_shell",
            &json!({"command": "git remote -v && git rev-parse --show-toplevel && git branch --show-current && git rev-parse HEAD && git tag --list 'v0.8.65'"}),
            crate::core::authority::auto_review::RunOrigin::Interactive,
            ApprovalMode::Auto,
            true,
            None,
        );

        assert_eq!(decision, AutoReviewPlanDecision::Allow);
        assert_eq!(audit["decision"], "allow");
        assert_eq!(audit["action_kind"], "shell");
    }
    // from auto_review_routes_unknown_tool_to_reviewer_in_auto
    {
        let (decision, audit) = auto_review_plan_decision(
            &crate::core::authority::auto_review::AutoReviewPolicy::default(),
            "mystery_tool",
            &json!({"value": true}),
            crate::core::authority::auto_review::RunOrigin::Interactive,
            ApprovalMode::Auto,
            true,
            None,
        );

        assert_eq!(
            decision,
            AutoReviewPlanDecision::ConsultReviewer(
                "unknown tool category requires explicit review".to_string()
            )
        );
        assert_eq!(audit["decision"], "ask_user");
    }
    // from auto_review_policy_blocks_publish_when_approval_is_never
    {
        let (decision, audit) = auto_review_plan_decision(
            &crate::core::authority::auto_review::AutoReviewPolicy::default(),
            "github_publish_release",
            &json!({"tag": "v0.8.64"}),
            crate::core::authority::auto_review::RunOrigin::Interactive,
            ApprovalMode::Never,
            true,
            None,
        );

        assert_eq!(
            decision,
            AutoReviewPlanDecision::Block(
                "Built-in safety gate requires approval: publish-like action requires durable review"
                    .to_string()
            )
        );
        assert_eq!(audit["approval_mode"], "NEVER");
        assert_eq!(audit["decision"], "hold_for_review");
    }
    // from auto_review_allows_ordinary_test_command_without_prompting
    {
        let (decision, audit) = auto_review_plan_decision(
            &crate::core::authority::auto_review::AutoReviewPolicy::default(),
            "exec_shell",
            &json!({"command": "cargo test"}),
            crate::core::authority::auto_review::RunOrigin::Interactive,
            ApprovalMode::Auto,
            true,
            None,
        );

        assert_eq!(decision, AutoReviewPlanDecision::Allow);
        assert_eq!(audit["decision"], "allow");
        assert_eq!(audit["risk"], "destructive");
    }
    // from auto_review_allows_ordinary_workspace_write_without_prompting
    {
        let tmp = tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("git marker");
        std::fs::create_dir(tmp.path().join("src")).expect("source directory");
        let (decision, audit) = auto_review_plan_decision(
            &crate::core::authority::auto_review::AutoReviewPolicy::default(),
            "write_file",
            &json!({"path": "src/lib.rs", "content": "pub fn ready() {}\n"}),
            crate::core::authority::auto_review::RunOrigin::Interactive,
            ApprovalMode::Auto,
            true,
            Some(tmp.path()),
        );

        assert_eq!(decision, AutoReviewPlanDecision::Allow);
        assert_eq!(audit["decision"], "allow");
        assert_eq!(audit["action_kind"], "write");
    }
    // from auto_review_routes_unbounded_or_sensitive_workspace_writes_to_reviewer
    {
        let tmp = tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join(".git")).expect("git marker");
        for path in ["../outside.rs", "/etc/hostname", ".env", ".git/config"] {
            let (decision, audit) = auto_review_plan_decision(
                &crate::core::authority::auto_review::AutoReviewPolicy::default(),
                "write_file",
                &json!({"path": path, "content": "blocked"}),
                crate::core::authority::auto_review::RunOrigin::Interactive,
                ApprovalMode::Auto,
                true,
                Some(tmp.path()),
            );
            assert!(
                matches!(decision, AutoReviewPlanDecision::ConsultReviewer(_)),
                "Auto-Review must not auto-approve {path} without reviewer judgment"
            );
            assert_eq!(audit["decision"], "ask_user", "unexpected audit for {path}");
        }
    }
}

#[test]
fn repo_law_asks_only_in_ask_posture() {
    use ApprovalMode;

    assert!(!repo_law_must_block_without_prompt(
        ApprovalMode::Suggest,
        false
    ));
    for mode in [
        ApprovalMode::Auto,
        ApprovalMode::Never,
        ApprovalMode::Bypass,
    ] {
        assert!(
            repo_law_must_block_without_prompt(mode, false),
            "{} must not open a human repo-law approval",
            mode.permission_chip_label()
        );
    }
    assert!(repo_law_must_block_without_prompt(
        ApprovalMode::Suggest,
        true
    ));
}

#[test]
fn rlm_eval_required_approval_is_auto_approved_in_full_access() {
    assert!(!registered_tool_approval_required(
        "rlm_eval",
        ApprovalRequirement::Required,
        true
    ));
}

/// A remembered grant for a Computer Use consent must not answer a later,
/// identical call: the prompt is forced, so only a card decides it.
#[test]
fn computer_use_decisions_always_force_the_card() {
    let consent = serde_json::json!({"app": "Safari", "bundle_id": "com.apple.Safari"});
    for name in [
        "mcp_codewhale-cu_consent_allow",
        "mcp_codewhale-cu_consent_revoke",
    ] {
        assert!(
            call_forces_prompt(name, &consent, ApprovalRequirement::Required),
            "{name}"
        );
    }
    assert!(call_forces_prompt(
        "mcp_codewhale-cu_app_script",
        &serde_json::json!({"script": "tell application \"Finder\" to activate"}),
        ApprovalRequirement::Required,
    ));
    assert!(!call_forces_prompt(
        "mcp_codewhale-cu_consent_status",
        &serde_json::json!({}),
        ApprovalRequirement::Required,
    ));
    let ask = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        false,
        false,
        ApprovalMode::Suggest,
    );
    assert_eq!(
        crate::core::authority::resolve_approval_request_disposition(
            &ask, true, false, true, false
        ),
        crate::core::authority::ApprovalRequestDisposition::Prompt,
        "a session grant must not pre-answer a forced prompt"
    );
}

#[test]
fn non_bypassable_registered_tools_auto_approve_in_full_access() {
    // #3866 reversed (owner decision, 2026-08-10): Full Access already grants
    // everything these calls can do — shell included — so a hold that cannot
    // open its own approval modal auto-approves instead of stranding the
    // call. Ask, which can open the modal, still gates every one of these.
    // Registry launcher is host-constructed and cache-bound (no free-form
    // command), so Full Access auto-approves it: `--auto` automation must
    // be able to complete the discovery flow end to end. Ask still gates it.
    assert!(!registered_tool_approval_required(
        "start_registry_mcp_server",
        ApprovalRequirement::Required,
        true
    ));
    assert!(registered_tool_approval_required(
        "start_registry_mcp_server",
        ApprovalRequirement::Required,
        false
    ));
    assert!(!registered_tool_approval_required(
        "start_mcp_server",
        ApprovalRequirement::Required,
        true
    ));
    assert!(!registered_tool_approval_required(
        "rlm_eval",
        ApprovalRequirement::Required,
        true
    ));
    assert!(registered_tool_forces_prompt(
        "start_mcp_server",
        ApprovalRequirement::Required,
    ));
    assert!(!registered_tool_forces_prompt(
        "start_registry_mcp_server",
        ApprovalRequirement::Required,
    ));
    assert!(registered_tool_forces_prompt(
        "rlm_eval",
        ApprovalRequirement::Required,
    ));
    assert!(!registered_tool_approval_required(
        "exec_shell",
        ApprovalRequirement::Required,
        true
    ));
    assert!(
        registered_tool_approval_required("start_mcp_server", ApprovalRequirement::Required, false),
        "start_mcp_server must require approval when auto_approve is disabled"
    );
    // Sanity contrast: an ordinary Required tool is bypassable under auto-approve.
    assert!(!registered_tool_approval_required(
        "exec_shell",
        ApprovalRequirement::Required,
        true
    ));
}

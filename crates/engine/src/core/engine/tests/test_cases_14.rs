#[tokio::test]
async fn sync_session_projects_persisted_subagent_handoff_for_headless_restore() {
    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &Config::default());
    let payload = concat!(
        "Child result retained.\nCheckpoint: engine restore is covered.\n",
        "<codewhale:subagent.done>{\"agent_id\":\"agent_headless\",",
        "\"status\":\"completed\",\"summary_location\":\"previous_line\"}",
        "</codewhale:subagent.done>",
    );
    let messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Keep the original task".to_string(),
                cache_control: None,
            }],
        },
        crate::runtime_handoff::subagent_completion_runtime_message(payload),
    ];

    let run = tokio::spawn(engine.run());
    handle
        .send(Op::SyncSession {
            session_id: Some("headless-resume".to_string()),
            messages,
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: tmp.path().to_path_buf(),
            mode: AppMode::Agent,
        })
        .await
        .expect("sync session");

    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetSessionSnapshot {
            tx: std::sync::Arc::new(std::sync::Mutex::new(Some(tx))),
        })
        .await
        .expect("request snapshot");
    let snapshot = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("snapshot response")
        .expect("snapshot");

    assert_eq!(snapshot.messages.len(), 2);
    assert!(snapshot.messages[0].content.iter().any(
        |block| matches!(block, ContentBlock::Text { text, .. } if text == "Keep the original task")
    ));
    let restored =
        codewhale_models::turn_classify::restored_subagent_checkpoint_display(&snapshot.messages[1])
            .expect("projected headless checkpoint");
    assert!(restored.contains("agent_headless"));
    assert!(restored.contains("Checkpoint: engine restore is covered."));
    assert!(!restored.contains("runtime_event"));
    assert!(!restored.contains("subagent.done"));

    run.abort();
}

#[tokio::test]
async fn session_snapshot_records_the_literal_custom_table_id() {
    let tmp = tempdir().expect("tempdir");
    let api_config = codewhale_runtime::config::parse_config_base(
        r#"provider = "custom"
[providers.custom]
kind = "openai-compatible"
base_url = "http://127.0.0.1:18180/v1"
model = "legacy-root-model"
auth_mode = "none"
"#,
    )
    .expect("canonical literal custom table");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "legacy-root-model".to_string(),
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &api_config);

    let run = tokio::spawn(engine.run());
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetSessionSnapshot {
            tx: std::sync::Arc::new(std::sync::Mutex::new(Some(tx))),
        })
        .await
        .expect("request snapshot");
    let snapshot = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("snapshot response")
        .expect("snapshot");

    // The literal route is the `[providers.custom]` table since #6394.
    assert_eq!(snapshot.model_provider, "custom");
    assert_eq!(snapshot.model_provider_id.as_deref(), Some("custom"));
    run.abort();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn edit_last_turn_preserves_current_mode() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // EditLastTurn dispatches a real replacement turn. Pin that turn to a
    // local, completing SSE response instead of depending on whichever
    // provider configuration or network state the parallel test process has.
    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-edit-mode\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"Revised plan.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-edit-mode\",\"choices\":[{\"index\":0,",
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

    let api_config = Config::default()
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        snapshots_enabled: false,
        subagents_enabled: false,
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &api_config);

    let run = tokio::spawn(engine.run());
    let seeded_messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "draft the plan".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "initial response".to_string(),
                cache_control: None,
            }],
        },
    ];
    handle
        .send(Op::SyncSession {
            session_id: Some("edit-mode-test".to_string()),
            messages: seeded_messages,
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: tmp.path().to_path_buf(),
            mode: AppMode::Agent,
        })
        .await
        .expect("sync session");
    handle
        .send(Op::ChangeMode {
            mode: AppMode::Plan,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            configured_sandbox_mode: None,
        })
        .await
        .expect("send plan mode");
    handle
        .send(Op::EditLastTurn {
            new_message: "revise this in plan mode".to_string(),
            submission_id: Some("sub-edit-1".to_string()),
        })
        .await
        .expect("send edit");
    // The replacement turn the edit replays must echo the edit's own
    // correlation token, not the one of any earlier turn.
    {
        let mut rx = handle.rx_event.write().await;
        loop {
            let event = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
                .await
                .expect("timed out waiting for the edited turn")
                .expect("engine event");
            if let Event::TurnStarted { submission_id, .. } = event {
                assert_eq!(
                    submission_id.as_deref(),
                    Some("sub-edit-1"),
                    "the edit's replacement turn must echo its correlation token"
                );
                break;
            }
        }
    }

    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetSessionSnapshot {
            tx: std::sync::Arc::new(std::sync::Mutex::new(Some(tx))),
        })
        .await
        .expect("request snapshot");
    let snapshot = tokio::time::timeout(model_turn_event_timeout(), rx)
        .await
        .expect("snapshot response")
        .expect("snapshot");

    assert_eq!(snapshot.mode, "plan");

    let requests = server
        .received_requests()
        .await
        .expect("recorded replacement request");
    assert_eq!(
        requests.len(),
        1,
        "edit must dispatch exactly one replacement turn"
    );
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run.await.expect("engine task");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn edit_last_turn_cuts_at_user_prompt_before_tool_results() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Tool results persist with role "user"; the edit cut must land on the
    // last genuine user prompt, not on the trailing tool_result of the
    // previous turn.
    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-edit-cut\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"Revised answer.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-edit-cut\",\"choices\":[{\"index\":0,",
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

    let api_config = Config::default()
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        snapshots_enabled: false,
        subagents_enabled: false,
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &api_config);

    let run = tokio::spawn(engine.run());
    let seeded_messages = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "original prompt".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                execution_id: None,
                id: "call_1".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "printf hi"}),
                caller: None,
                thought_signature: None,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                execution_id: None,
                tool_use_id: "call_1".to_string(),
                content: "unique-tool-output-marker".to_string(),
                is_error: None,
                content_blocks: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "final answer".to_string(),
                cache_control: None,
            }],
        },
    ];
    handle
        .send(Op::SyncSession {
            session_id: Some("edit-cut-test".to_string()),
            messages: seeded_messages,
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: tmp.path().to_path_buf(),
            mode: AppMode::Agent,
        })
        .await
        .expect("sync session");
    handle
        .send(Op::EditLastTurn {
            new_message: "edited prompt".to_string(),
            submission_id: None,
        })
        .await
        .expect("send edit");

    // Ops are processed in order: once the snapshot arrives, the replacement
    // turn has completed.
    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetSessionSnapshot {
            tx: std::sync::Arc::new(std::sync::Mutex::new(Some(tx))),
        })
        .await
        .expect("request snapshot");
    let snapshot = tokio::time::timeout(model_turn_event_timeout(), rx)
        .await
        .expect("snapshot response")
        .expect("snapshot");

    assert_eq!(
        snapshot.messages.len(),
        2,
        "the whole previous turn (prompt, tool_use, tool_result, answer) must be cut: {:?}",
        snapshot.messages
    );
    assert!(
        snapshot.messages.iter().all(|message| !message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))),
        "no tool_result may survive the cut: {:?}",
        snapshot.messages
    );
    let replacement_text = message_text_of(&snapshot.messages[0]);
    assert!(
        replacement_text.contains("edited prompt"),
        "first surviving message is the edited prompt: {replacement_text}"
    );

    let requests = server
        .received_requests()
        .await
        .expect("recorded replacement request");
    assert_eq!(requests.len(), 1);
    let body = String::from_utf8(requests[0].body.clone()).expect("request body utf8");
    assert!(body.contains("edited prompt"));
    assert!(
        !body.contains("original prompt"),
        "old prompt must not leak into the replacement turn: {body}"
    );
    assert!(
        !body.contains("unique-tool-output-marker"),
        "tool round-trip must not leak into the replacement turn: {body}"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run.await.expect("engine task");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn edit_last_turn_without_user_prompt_errors_and_sends_nothing() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let tmp = tempdir().expect("tempdir");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let api_config = Config::default()
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        model: "deepseek-v4-pro".to_string(),
        snapshots_enabled: false,
        subagents_enabled: false,
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &api_config);

    let run = tokio::spawn(engine.run());
    // History without any genuine user prompt: nothing to edit. The engine
    // must surface an error instead of silently appending the message.
    handle
        .send(Op::SyncSession {
            session_id: Some("edit-no-user-test".to_string()),
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "assistant only".to_string(),
                    cache_control: None,
                }],
            }],
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: tmp.path().to_path_buf(),
            mode: AppMode::Agent,
        })
        .await
        .expect("sync session");
    handle
        .send(Op::EditLastTurn {
            new_message: "edited prompt".to_string(),
            submission_id: Some("sub-edit-rejected".to_string()),
        })
        .await
        .expect("send edit");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut saw_edit_error = false;
    let mut saw_failed_terminal = false;
    {
        let mut events = handle.rx_event.write().await;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
            match event {
                Event::TurnStarted { .. } => {
                    panic!("rejected edit must not start a turn");
                }
                Event::Error { envelope, .. } => {
                    assert_eq!(envelope.code, "edit_last_turn_no_user_prompt");
                    assert!(!envelope.recoverable);
                    assert!(
                        envelope.message.contains("no user message"),
                        "unexpected error: {}",
                        envelope.message
                    );
                    saw_edit_error = true;
                }
                Event::TurnComplete {
                    turn_id,
                    submission_id,
                    compaction_id,
                    status,
                    error,
                    ..
                } => {
                    assert!(turn_id.is_none());
                    assert_eq!(submission_id.as_deref(), Some("sub-edit-rejected"));
                    assert!(compaction_id.is_none());
                    assert_eq!(status, TurnOutcomeStatus::Failed);
                    assert!(
                        error
                            .as_deref()
                            .is_some_and(|message| message.contains("no user message")),
                        "failed edit terminal must carry the rejection: {error:?}"
                    );
                    saw_failed_terminal = true;
                    break;
                }
                _ => {}
            }
        }
    }
    assert!(saw_edit_error, "edit without a user prompt must error out");
    assert!(
        saw_failed_terminal,
        "edit rejection must complete the submitted host lifecycle"
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetSessionSnapshot {
            tx: std::sync::Arc::new(std::sync::Mutex::new(Some(tx))),
        })
        .await
        .expect("request snapshot");
    let snapshot = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("snapshot response")
        .expect("snapshot");
    assert_eq!(
        snapshot.messages.len(),
        1,
        "failed edit must not append the new message: {:?}",
        snapshot.messages
    );

    // An unsupported latest user turn is still a history boundary. It must
    // fail in place rather than falling through to the older text prompt and
    // deleting a larger portion of the conversation.
    let image_only_history = vec![
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "older editable prompt".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "older response".to_string(),
                cache_control: None,
            }],
        },
        Message {
            role: Role::User,
            content: vec![ContentBlock::ImageUrl {
                image_url: codewhale_models::ImageUrlContent {
                    url: "data:image/png;base64,AAAA".to_string(),
                },
            }],
        },
    ];
    handle
        .send(Op::SyncSession {
            session_id: Some("edit-unsupported-user-test".to_string()),
            messages: image_only_history.clone(),
            system_prompt: None,
            system_prompt_override: false,
            model: "deepseek-v4-pro".to_string(),
            workspace: tmp.path().to_path_buf(),
            mode: AppMode::Agent,
        })
        .await
        .expect("sync unsupported user session");
    handle
        .send(Op::EditLastTurn {
            new_message: "must not replace the older prompt".to_string(),
            submission_id: None,
        })
        .await
        .expect("send unsupported edit");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut saw_unsupported_error = false;
    let mut saw_unsupported_terminal = false;
    {
        let mut events = handle.rx_event.write().await;
        while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
            match event {
                Event::Error { envelope, .. } => {
                    assert_eq!(envelope.code, "edit_last_turn_unsupported_user_content");
                    assert!(!envelope.recoverable);
                    saw_unsupported_error = true;
                }
                Event::TurnComplete { status, error, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Failed);
                    assert!(
                        error
                            .as_deref()
                            .is_some_and(|message| message
                                .contains("latest user message has no editable text")),
                        "unsupported edit terminal must carry the rejection: {error:?}"
                    );
                    saw_unsupported_terminal = true;
                    break;
                }
                _ => {}
            }
        }
    }
    assert!(saw_unsupported_error);
    assert!(saw_unsupported_terminal);

    let (tx, rx) = tokio::sync::oneshot::channel();
    handle
        .send(Op::GetSessionSnapshot {
            tx: std::sync::Arc::new(std::sync::Mutex::new(Some(tx))),
        })
        .await
        .expect("request unsupported snapshot");
    let unsupported_snapshot = tokio::time::timeout(Duration::from_secs(2), rx)
        .await
        .expect("unsupported snapshot response")
        .expect("unsupported snapshot");
    assert_eq!(
        unsupported_snapshot.messages, image_only_history,
        "unsupported latest user content must leave the entire history unchanged"
    );

    let requests = server.received_requests().await.expect("recorded requests");
    assert!(
        requests.is_empty(),
        "failed edit must not dispatch a provider turn"
    );

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run.await.expect("engine task");
}

#[tokio::test]
async fn provider_runtime_status_reports_configured_zai_cap_without_client() {
    let (engine, handle) = {
        let _lock = lock_test_env();
        let _zai_key = EnvVarGuard::remove("ZAI_API_KEY");
        let _zai_alt_key = EnvVarGuard::remove("Z_AI_API_KEY");
        let api_config = {
            let __cw_config_init_3_0 = Some("zai".to_string());
            let mut __cw_config_init_3_value: Config = Config::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value
        };
        Engine::new(EngineConfig::default(), &api_config)
    };

    let run = tokio::spawn(engine.run());
    let status = tokio::time::timeout(Duration::from_secs(2), handle.get_provider_runtime_status())
        .await
        .expect("provider runtime status response")
        .expect("provider runtime status");

    assert_eq!(status.provider, ProviderKind::Zai);
    assert_eq!(
        status.request_concurrency_limit,
        Some(codewhale_runtime::config::DEFAULT_ZAI_PROVIDER_MAX_CONCURRENCY)
    );
    assert_eq!(status.active_provider_requests, 0);

    run.abort();
}

#[test]
fn detects_context_length_errors_from_provider_payloads() {
    let msg = r#"SSE stream request failed: HTTP 400 Bad Request: {"error":{"message":"This model's maximum context length is 131072 tokens. However, you requested 153056 tokens (148960 in the messages, 4096 in the completion).","type":"invalid_request_error"}}"#;
    assert!(is_context_length_error_message(msg));
    // llama.cpp's server wording (#6374): a genuine overflow on a local route
    // must enter the bounded recovery path too.
    assert!(is_context_length_error_message(
        r#"SSE stream request failed: HTTP 400 Bad Request: {"error":{"code":400,"message":"the request exceeds the available context size. try increasing the context size or enable context shift","type":"invalid_request_error"}}"#
    ));
    assert!(!is_context_length_error_message(
        "SSE stream request failed: HTTP 400 Bad Request: model not found"
    ));
}

/// #6374: the exhausted-recovery message must name levers the reader has.
#[test]
fn context_overflow_exhausted_message_names_levers_that_exist_in_the_mode() {
    let headless = super::context::context_overflow_exhausted_message(false, 2, 98_739, 97_280);
    assert!(
        !headless.contains("/compact") && !headless.contains("/clear"),
        "a headless host has no command layer: {headless}"
    );
    assert!(
        headless.contains("2 emergency compaction passes"),
        "{headless}"
    );
    assert!(
        headless.contains("CODEWHALE_MAX_OUTPUT_TOKENS"),
        "{headless}"
    );
    let interactive = super::context::context_overflow_exhausted_message(true, 1, 98_739, 97_280);
    assert!(
        interactive.contains("/compact") && interactive.contains("/clear"),
        "{interactive}"
    );
    assert!(
        interactive.contains("1 emergency compaction pass "),
        "{interactive}"
    );
}

#[test]
fn context_budget_scenario() {
    // Scenario consolidation of: context_budget_reserves_output_and_headroom, context_budget_uses_conservative_fallback_for_unknown_models, context_budget_uses_provider_effective_window_for_openai_codex
    // from context_budget_reserves_output_and_headroom
    {
        // Serialize with other tests that mutate DEEPSEEK_MAX_OUTPUT_TOKENS so
        // the internal effective_max_output_tokens() call sees a stable env.
        let _lock = lock_test_env();
        // Preflight reserves exactly the route-effective output request plus the
        // shared safety headroom, even on a 1M route.
        let budget = context_input_budget_for_provider(ProviderKind::Deepseek, "deepseek-v4-pro")
            .expect("deepseek-v4-pro should have a known context window");
        let v4_window: usize = 1_000_000;
        let expected = v4_window
            - effective_max_output_tokens_for_route(ProviderKind::Deepseek, "deepseek-v4-pro", None)
                as usize
            - 1_024usize;
        assert_eq!(budget, expected);
    }
    // from context_budget_uses_conservative_fallback_for_unknown_models
    {
        let _lock = lock_test_env();
        let budget = context_input_budget_for_provider(ProviderKind::Openai, "auto")
            .expect("unknown/auto model ids should still get a conservative hard preflight budget");
        let expected = 128_000usize
            - effective_max_output_tokens_for_route(ProviderKind::Openai, "auto", None) as usize
            - 1_024usize;
        assert_eq!(budget, expected);
    }
    // from context_budget_uses_provider_effective_window_for_openai_codex
    {
        let _lock = lock_test_env();
        let budget = context_input_budget_for_provider(ProviderKind::OpenaiCodex, "gpt-5.5")
            .expect("OpenAI Codex should use a conservative fallback without route metadata");
        let expected = usize::try_from(codewhale_runtime::config::OPENAI_CODEX_EFFECTIVE_CONTEXT_WINDOW_TOKENS)
            .expect("context window fits usize")
            - crate::provider_lake::provider_capability(ProviderKind::OpenaiCodex, "gpt-5.5")
                .max_output
                .expect("Codex route publishes a deliberate conservative output cap")
                as usize
            - 1_024usize;
        assert_eq!(budget, expected);
    }
}

#[test]
fn route_context_scenario() {
    // Scenario consolidation of: route_context_budget_uses_shared_budget_service, route_context_budget_prefers_resolved_route_limits
    // from route_context_budget_uses_shared_budget_service
    {
        let _lock = lock_test_env();
        let budget =
            route_context_budget_for_provider(ProviderKind::OpenaiCodex, "gpt-5.5", 380_000)
                .expect("OpenAI Codex should produce a route budget");

        assert_eq!(
            budget.window_tokens,
            u64::from(codewhale_runtime::config::OPENAI_CODEX_EFFECTIVE_CONTEXT_WINDOW_TOKENS)
        );
        assert_eq!(
            budget.output_cap_tokens,
            u64::from(
                crate::provider_lake::provider_capability(ProviderKind::OpenaiCodex, "gpt-5.5")
                    .max_output
                    .expect("Codex route publishes a deliberate conservative output cap")
            )
        );
        assert_eq!(
            budget.pressure,
            codewhale_runtime::context_budget::PressureLevel::Critical
        );
        assert!(!budget.fits_additional(1));
    }
    // from route_context_budget_prefers_resolved_route_limits
    {
        let _lock = lock_test_env();
        let limits = codewhale_config::route::RouteLimits {
            context_tokens: Some(128_000),
            input_tokens: None,
            output_tokens: Some(32_768),
            output_declared: false,
        };
        let budget = route_context_budget_for_route(
            ProviderKind::Openrouter,
            "deepseek/deepseek-v4-pro",
            Some(limits),
            60_000,
        )
        .expect("route limits should produce a budget");

        assert_eq!(budget.window_tokens, 128_000);
        assert_eq!(budget.output_cap_tokens, 32_768);
        assert_eq!(budget.available_input_tokens, 34_208);
    }
}

#[test]
fn route_input_limit_blocks_oversized_preflight_before_transport() {
    let _lock = lock_test_env();
    let limits = codewhale_config::route::RouteLimits {
        context_tokens: Some(1_000_000),
        input_tokens: Some(128_000),
        output_tokens: Some(64_000),
        output_declared: false,
    };
    let estimated_input = 200_000;
    let budget = route_context_budget_for_route(
        ProviderKind::Vllm,
        "DeepSeek-V4-Flash",
        Some(limits),
        estimated_input,
    )
    .expect("resolved route limits should produce the turn-loop preflight budget");

    assert_eq!(budget.window_tokens, 1_000_000);
    assert_eq!(budget.output_cap_tokens, 64_000);
    assert_eq!(budget.input_budget_ceiling, 128_000);
    assert_eq!(budget.available_input_tokens, 0);
    assert!(
        estimated_input > usize::try_from(budget.input_budget_ceiling).unwrap(),
        "the turn-loop preflight must recover before constructing a network request"
    );
}

/// #6374: the preflight guard measured a ×1.5-inflated estimate against the
/// honest input ceiling, so a route refused at two thirds of its budget with
/// the request never leaving the machine. The window here is calibrated so the
/// honest estimate sits below the ceiling and the inflated one above it; the
/// turn must reach the model with its history untouched.
#[tokio::test]
async fn preflight_guard_measures_honest_input_against_the_input_ceiling() {
    let _lock = lock_test_env();
    let _output_env = ScopedDeepSeekMaxOutputTokens::unset();
    let workspace = tempdir().expect("workspace");
    let _home = EnvVarGuard::set("CODEWHALE_HOME", workspace.path());
    let mock = std::sync::Arc::new(crate::llm_client::mock::MockLlmClient::new(vec![
        crate::llm_client::mock::canned::simple_text_turn("continuing"),
    ]));
    let (mut engine, _handle) = Engine::new_with_model_client(
        EngineConfig {
            terminal_chrome_enabled: false,
            ..deterministic_engine_config(workspace.path())
        },
        &Config::default(),
        mock.clone(),
    );
    // Only the preflight guard is under test; the auto-compaction gate stays out.
    engine.config.compaction.enabled = false;
    let history: Vec<Message> = [
        (Role::User, "x".repeat(120_000)),
        (Role::Assistant, "y".repeat(100_000)),
        (Role::User, "please continue".to_string()),
    ]
    .into_iter()
    .map(|(role, text)| Message {
        role,
        content: vec![ContentBlock::Text {
            text,
            cache_control: None,
        }],
    })
    .collect();
    for message in &history {
        engine.session.add_message(message.clone());
    }
    let system = engine.session.system_prompt.clone();
    let honest = crate::compaction::estimate_input_tokens_for_pressure(&history, system.as_ref());
    let inflated = crate::compaction::estimate_input_tokens_conservative(&history, system.as_ref());
    assert!(
        inflated > honest + 20_000,
        "fixture must separate the estimators: honest {honest}, inflated {inflated}"
    );
    let output_cap = 4_096u64;
    let target_ceiling = u64::try_from((honest + inflated) / 2).unwrap();
    engine.active_route_limits = Some(codewhale_config::route::RouteLimits {
        context_tokens: Some(
            target_ceiling + output_cap + codewhale_runtime::context_budget::CONTEXT_HEADROOM_TOKENS,
        ),
        input_tokens: None,
        output_tokens: Some(output_cap),
        output_declared: false,
    });
    let ceiling = route_context_budget_for_route(
        engine.api_provider,
        &engine.session.model,
        engine.active_route_limits,
        0,
    )
    .expect("route limits produce a budget")
    .input_budget_ceiling;
    let ceiling = usize::try_from(ceiling).unwrap();
    assert!(
        honest < ceiling && ceiling < inflated,
        "calibration: honest {honest} < ceiling {ceiling} < inflated {inflated}"
    );

    let registry =
        crate::tools::ToolRegistry::new(crate::tools::spec::ToolContext::new(workspace.path()));
    let catalog = registry.to_api_tools_with_cache(true);
    let surface = crate::core::engine::tool_catalog::ToolSurfacePolicy::new(
        registry,
        Some(catalog),
        codewhale_config::AppMode::Agent,
        &engine.config.tools_always_load,
        &[],
        false,
        None,
        None,
        Some(4),
        crate::core::engine::tool_catalog::ToolMode::Direct,
    );
    let (status, error) = engine
        .run_turn(
            &mut crate::core::turn::TurnContext::new(8),
            surface,
            None,
            None,
        )
        .await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(
        mock.call_count(),
        1,
        "the only model request is the turn itself, not an emergency compaction"
    );
    let request = mock.last_request().expect("the turn reached the model");
    assert_eq!(
        request.messages.len(),
        history.len(),
        "history reached the model without an emergency compaction pass"
    );
}

#[test]
fn kimi_catalog_output_ceiling_does_not_collapse_input_budget() {
    let _lock = lock_test_env();
    let _guard = ScopedDeepSeekMaxOutputTokens::unset();
    let documented =
        route_context_budget_for_route(ProviderKind::Moonshot, "kimi-k2.7-code", None, 0)
            .expect("bundled Kimi limits should produce a budget");
    assert_eq!(documented.window_tokens, 262_144);
    assert_eq!(documented.output_cap_tokens, 32_768);
    assert_eq!(documented.available_input_tokens, 228_352);

    // #4368/#4378: Models.dev may report Kimi's full 262K context as both its
    // context window and provider output ceiling. That ceiling must not be
    // reserved as though every normal turn requested 262K of output; the
    // integrated Kimi route cap is 32K.
    let limits = codewhale_config::route::RouteLimits {
        context_tokens: Some(262_144),
        input_tokens: None,
        output_tokens: Some(262_144),
        output_declared: false,
    };

    let budget =
        route_context_budget_for_route(ProviderKind::Moonshot, "kimi-k2.7-code", Some(limits), 0)
            .expect("Kimi route limits should produce a budget");

    assert_eq!(budget.window_tokens, 262_144);
    assert_eq!(budget.output_cap_tokens, 32_768);
    assert_eq!(budget.available_input_tokens, 228_352);
}

#[test]
fn effective_max_scenario() {
    // Scenario consolidation of: effective_max_output_tokens_for_route_caps_to_route_output_limit, effective_max_output_tokens_for_route_caps_to_context_window, effective_max_output_tokens_for_route_keeps_tiny_window_positive, effective_max_output_tokens_caps_api_request_for_large_window_models, effective_max_output_tokens_env_override_rejects_zero_and_invalid
    // from effective_max_output_tokens_for_route_caps_to_route_output_limit
    {
        let _lock = lock_test_env();
        let limits = codewhale_config::route::RouteLimits {
            context_tokens: Some(1_000_000),
            input_tokens: None,
            output_tokens: Some(8_192),
            output_declared: false,
        };

        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
                Some(limits),
            ),
            8_192
        );
    }
    // from effective_max_output_tokens_for_route_caps_to_context_window
    {
        let _lock = lock_test_env();
        let limits = codewhale_config::route::RouteLimits {
            context_tokens: Some(32_000),
            input_tokens: None,
            output_tokens: None,
            output_declared: false,
        };

        let cap = effective_max_output_tokens_for_route(
            ProviderKind::Deepseek,
            "deepseek-v4-pro",
            Some(limits),
        );

        assert!(cap < 32_000, "request cap must fit the configured window");
        assert!(
            cap > 0,
            "small configured windows should still allow output"
        );
    }
    // from effective_max_output_tokens_for_route_keeps_tiny_window_positive
    {
        let _lock = lock_test_env();
        let limits = codewhale_config::route::RouteLimits {
            context_tokens: Some(2_048),
            input_tokens: None,
            output_tokens: None,
            output_declared: false,
        };

        assert_eq!(
            effective_max_output_tokens_for_route(
                ProviderKind::Deepseek,
                "deepseek-v4-pro",
                Some(limits),
            ),
            1
        );
    }
    // from effective_max_output_tokens_caps_api_request_for_large_window_models
    {
        // Serialize with other tests that mutate DEEPSEEK_MAX_OUTPUT_TOKENS so
        // v4_cap and flash_cap below see the same env state.
        let _lock = lock_test_env();
        // Hosted V4 documents a 384K capability ceiling in the bundled catalogue,
        // but a ceiling is not a safe no-config request size. The operator can
        // still request a larger value explicitly; the automatic request starts
        // at the ordinary 64K cap (#5516/#5518).
        let v4_cap = effective_max_output_tokens("deepseek-v4-pro");
        assert_eq!(
            v4_cap, 65_536,
            "hosted V4 must not turn the 384K capability maximum into the default request, got {v4_cap}"
        );

        let flash_cap = effective_max_output_tokens("deepseek-v4-flash");
        assert_eq!(v4_cap, flash_cap);
    }
    // from effective_max_output_tokens_env_override_rejects_zero_and_invalid
    {
        let _lock = lock_test_env();
        // Establish the heuristic baseline with the env unset.
        let baseline = {
            let _guard = ScopedDeepSeekMaxOutputTokens::unset();
            effective_max_output_tokens("deepseek-v4-pro")
        };
        assert!(baseline > 0);

        // 0, non-numeric, and empty values must all fall through to the heuristic
        // rather than producing a zero/garbage cap that would silently break
        // request budgeting.
        for raw in ["0", "abc", "", "  ", "-1"] {
            let _guard = ScopedDeepSeekMaxOutputTokens::set(raw);
            assert_eq!(
                effective_max_output_tokens("deepseek-v4-pro"),
                baseline,
                "env={raw:?} should fall through to heuristic"
            );
        }
    }
}

#[test]
fn codex_route_without_output_metadata_uses_oauth_capability_floor() {
    let _lock = lock_test_env();
    let limits = codewhale_config::route::RouteLimits {
        context_tokens: Some(272_000),
        input_tokens: None,
        output_tokens: None,
        output_declared: false,
    };

    assert_eq!(
        effective_max_output_tokens_for_route(ProviderKind::OpenaiCodex, "gpt-5.5", Some(limits)),
        4_096
    );
    let budget =
        route_context_budget_for_route(ProviderKind::OpenaiCodex, "gpt-5.5", Some(limits), 0)
            .expect("Codex route budget");
    assert_eq!(budget.output_cap_tokens, 4_096);
}

#[test]
fn reasoning_max_does_not_add_a_second_deepseek_v4_output_reservation() {
    let _lock = lock_test_env();
    let _codewhale = EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
    let _deepseek = EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
    let limits = codewhale_config::route::RouteLimits {
        context_tokens: Some(327_680),
        input_tokens: None,
        output_tokens: None,
        output_declared: false,
    };
    let cap = effective_max_output_tokens_for_route(
        ProviderKind::Vllm,
        "DeepSeek-V4-Flash",
        Some(limits),
    );
    let request = codewhale_core::request::prepare_primary_turn_request(
        codewhale_core::request::PrimaryTurnRequest {
            model: "DeepSeek-V4-Flash".to_string(),
            messages: Vec::new(),
            max_tokens: cap,
            system: None,
            tools: None,
            tool_choice: None,
            reasoning_effort: Some("max".to_string()),
        },
    );
    let budget = route_context_budget_for_route(
        ProviderKind::Vllm,
        "DeepSeek-V4-Flash",
        Some(limits),
        105_000,
    )
    .expect("max-reasoning vLLM route budget");

    assert_eq!(request.reasoning_effort.as_deref(), Some("max"));
    assert_eq!(request.max_tokens, 65_536);
    assert_eq!(budget.output_cap_tokens, u64::from(request.max_tokens));
    assert_eq!(budget.input_budget_ceiling, 261_120);
    assert!(budget.available_input_tokens > 0);
}

struct ScopedDeepSeekMaxOutputTokens {
    previous: Option<OsString>,
}

impl ScopedDeepSeekMaxOutputTokens {
    fn set(value: &str) -> Self {
        let previous = std::env::var_os("DEEPSEEK_MAX_OUTPUT_TOKENS");
        // Safety: tests using this helper serialize with lock_test_env() and
        // restore the original value in Drop.
        unsafe {
            std::env::set_var("DEEPSEEK_MAX_OUTPUT_TOKENS", value);
        }
        Self { previous }
    }

    fn unset() -> Self {
        let previous = std::env::var_os("DEEPSEEK_MAX_OUTPUT_TOKENS");
        // Safety: see set().
        unsafe {
            std::env::remove_var("DEEPSEEK_MAX_OUTPUT_TOKENS");
        }
        Self { previous }
    }
}

impl Drop for ScopedDeepSeekMaxOutputTokens {
    fn drop(&mut self) {
        // Safety: tests using this helper serialize with lock_test_env().
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var("DEEPSEEK_MAX_OUTPUT_TOKENS", previous);
            } else {
                std::env::remove_var("DEEPSEEK_MAX_OUTPUT_TOKENS");
            }
        }
    }
}

#[test]
fn effective_max_output_tokens_env_override_returns_positive_value() {
    let _lock = lock_test_env();
    let _guard = ScopedDeepSeekMaxOutputTokens::set("16384");

    // Override applies regardless of model — V4 hosted, V4 flash, and
    // self-hosted routes all return the env value verbatim before route clamps.
    assert_eq!(effective_max_output_tokens("deepseek-v4-pro"), 16_384);
    assert_eq!(effective_max_output_tokens("deepseek-v4-flash"), 16_384);
    assert_eq!(effective_max_output_tokens("qwen3-32b-256k"), 16_384);
}

#[test]
fn internal_context_budget_uses_the_wire_cap_across_window_sizes() {
    // Serialize with other tests that mutate DEEPSEEK_MAX_OUTPUT_TOKENS so
    // both branches below see a stable env.
    let _lock = lock_test_env();
    // Large routes use the same effective output cap that reaches the wire.
    let internal_budget =
        context_input_budget_for_provider(ProviderKind::Deepseek, "deepseek-v4-pro")
            .expect("V4 should have a known context window");
    let v4_window: usize = 1_000_000;
    let expected_internal = v4_window
        - effective_max_output_tokens_for_route(ProviderKind::Deepseek, "deepseek-v4-pro", None)
            as usize
        - 1_024usize;
    assert_eq!(internal_budget, expected_internal);

    // A 256K self-hosted deployment uses the same rule and yields a usable
    // positive budget rather than silently disabling preflight/recovery.
    let small_window_budget =
        context_input_budget_for_provider(ProviderKind::Openai, "qwen3-32b-256k")
            .expect("a 256K-suffix model must yield Some budget via the effective-cap branch");
    let effective_output =
        effective_max_output_tokens_for_route(ProviderKind::Openai, "qwen3-32b-256k", None)
            as usize;
    let expected_small = 256_000 - effective_output - 1_024;
    assert_eq!(small_window_budget, expected_small);
}

const ROUTE_128K: &str = "deepseek-v3.2-128k";
const SESSION_6508: &str = "session-6508";

fn budget_128k() -> usize {
    crate::route_budget::route_inline_char_budget_for_route(
        ProviderKind::Deepseek,
        ROUTE_128K,
        None,
    )
}

fn view_128k(tool_name: &str, output: &ToolResult) -> super::context::ToolResultContextView {
    super::context::tool_result_context_view(
        ProviderKind::Deepseek,
        ROUTE_128K,
        None,
        tool_name,
        output,
    )
}

/// Run `f` with the spillover and session-artifact roots under a temp home.
fn with_artifact_home<R>(f: impl FnOnce(&Path) -> R) -> R {
    let _spill_guard = crate::tools::truncate::TEST_SPILLOVER_GUARD
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let home = tempdir().expect("tempdir");
    let path = home.path().to_path_buf();
    crate::tools::truncate::with_test_home(&path, || f(&path))
}

fn session_artifact_files(home: &Path) -> Vec<PathBuf> {
    let dir = home
        .join(".codewhale")
        .join("sessions")
        .join(SESSION_6508)
        .join("artifacts");
    fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default()
}

#[test]
fn under_budget_results_pass_through_whole_for_every_tool() {
    // #6508: one budget, sized by the route, decides what the model sees.
    // There is no per-tool-name soft limit: a web answer or a shell log that
    // fits the budget reaches the model byte for byte.
    // 3% of a 128K window is 15,360 characters (an operator opt-in can only
    // raise it; see route_budget's tests for the exact values).
    assert!(budget_128k() >= 15_360);
    for content in [
        String::new(),
        "\n \t\n".to_string(),
        format!("\n\t{} \n", "w".repeat(14_000)),
    ] {
        let output = ToolResult::success(content.clone());
        for tool_name in [
            "exec_shell",
            "web_search",
            "Web",
            "web.run",
            "fetch_url",
            "read",
            "read_file",
            "run_tests",
        ] {
            let view = view_128k(tool_name, &output);
            assert_eq!(view.text, content, "{tool_name} was cut under the budget");
            assert!(!view.needs_full_output_artifact);
        }
    }
}

#[test]
fn over_budget_result_without_a_saved_copy_says_so_and_asks_for_one() {
    // The view never writes. Without a saved copy it asks the engine for
    // one, and until then it promises no ref it cannot honour.
    let raw = "shell line\n".repeat(4_000);
    let output = ToolResult::success(raw.clone());
    let view = view_128k("exec_shell", &output);

    assert!(view.needs_full_output_artifact);
    assert!(view.text.chars().count() <= budget_128k());
    assert!(view.text.contains("the full output could not be saved"));
    assert!(view.text.contains("no tool call reaches this copy"));
    assert!(!view.text.contains("retrieve_tool_result"));
}

#[test]
fn oversized_tool_output_is_recoverable_before_serial_and_parallel_fanout() {
    use crate::llm_client::mock::{MockLlmClient, canned};
    use crate::tools::spec::{ToolCapability, ToolSpec};

    struct OutputTool {
        parallel: bool,
        content: String,
    }
    #[async_trait::async_trait]
    impl ToolSpec for OutputTool {
        fn name(&self) -> &str {
            "fixture_output"
        }
        fn description(&self) -> &str {
            "Return output below the spill threshold."
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        fn supports_parallel(&self) -> bool {
            self.parallel
        }
        async fn execute(&self, _: Value, _: &ToolContext) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::success(self.content.clone()))
        }
    }

    with_artifact_home(|home| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let raw = format!("{}MIDDLE{}", "h".repeat(5_000), "t".repeat(5_000));
                assert!(raw.len() < crate::tools::truncate::SPILLOVER_THRESHOLD_BYTES);
                for (parallel, count) in [(true, 1), (true, 2), (false, 1)] {
                    let calls = [
                        ("call-one", "fixture_output", "{}"),
                        ("call-two", "fixture_output", "{}"),
                    ];
                    let mock = Arc::new(MockLlmClient::new(vec![
                        tool_batch_turn(&calls[..count]),
                        canned::simple_text_turn("done"),
                    ]));
                    let (mut engine, handle) = Engine::new_with_model_client(
                        deterministic_engine_config(home),
                        &Config::default(),
                        mock.clone(),
                    );
                    engine.active_route_limits = Some(codewhale_config::route::RouteLimits {
                        context_tokens: Some(64_000),
                        input_tokens: None,
                        output_tokens: Some(4_096),
                        output_declared: false,
                    });
                    let mut registry = crate::tools::ToolRegistry::new(ToolContext::new(home));
                    registry.register(Arc::new(OutputTool {
                        parallel,
                        content: raw.clone(),
                    }));
                    let tools = Some(registry.to_api_tools_with_cache(true));
                    let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
                    let mut turn = crate::core::turn::TurnContext::new(4);
                    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
                    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                    let requests = mock.captured_requests();
                    assert_eq!(requests.len(), 2);
                    let mut events = handle.rx_event.write().await;
                    let mut completed = 0;
                    while let Ok(event) = events.try_recv() {
                        let Event::ToolCallComplete {
                            result, model_call, ..
                        } = event
                        else {
                            continue;
                        };
                        let output = result.expect("tool succeeded");
                        assert_eq!(output.content, raw, "UI keeps the complete output");
                        let metadata = output
                            .metadata
                            .expect("output must be preserved before fanout");
                        let path = metadata["artifact_path"].as_str().expect("artifact path");
                        assert_eq!(fs::read_to_string(path).unwrap(), raw);
                        let reference = metadata["artifact_id"].as_str().unwrap();
                        let results =
                            guardian_tool_results(&requests[1], &model_call.unwrap().provider_id);
                        assert_eq!(results.len(), 1);
                        let (text, _) = results[0];
                        assert!(text.len() <= 7_680, "64K route inline budget");
                        assert!(!text.contains("MIDDLE"));
                        assert!(text.contains("retrieve_tool_result"));
                        assert!(text.contains(reference), "model and UI share the artifact");
                        completed += 1;
                    }
                    assert_eq!(completed, count);
                }
            });
    });
}

#[test]
fn over_budget_result_writes_the_full_output_and_names_its_ref() {
    with_artifact_home(|home| {
        let raw = format!("FIRST LINE\n{}LAST LINE", "shell line\n".repeat(4_000));
        let mut output = ToolResult::success(raw.clone());
        assert!(view_128k("exec_shell", &output).needs_full_output_artifact);

        assert!(
            crate::tools::truncate::preserve_full_output_for_model_context(
                &mut output,
                "call-over",
                "exec_shell",
                SESSION_6508,
            )
        );
        // The UI cell keeps the whole result; only metadata changed.
        assert_eq!(output.content, raw);

        let view = view_128k("exec_shell", &output);
        assert!(!view.needs_full_output_artifact);
        assert!(view.text.chars().count() <= budget_128k());
        assert!(view.text.starts_with("FIRST LINE"));
        assert!(view.text.ends_with("LAST LINE"));
        assert!(view.text.contains("omitted range recovery:"));
        assert!(view.text.contains("ref=\"art_call-over\""));

        let files = session_artifact_files(home);
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read_to_string(&files[0]).expect("artifact"), raw);
    });
}

#[test]
fn spilled_preview_is_refit_not_recut() {
    // Spillover already saved the full output and left a ~40 KB preview. The
    // model view re-fits that preview to the budget with the same ref, and no
    // second artifact (which could only hold the preview) is written.
    with_artifact_home(|home| {
        let raw = format!(
            "\n    HEAD START\n{}TAIL END \n",
            "spilled output line\n".repeat(16_000)
        );
        let mut output = ToolResult::success(raw.clone());
        assert!(
            crate::tools::truncate::apply_spillover_with_artifact(
                &mut output,
                "call-spill",
                "exec_shell",
                SESSION_6508,
            )
            .is_some()
        );
        assert_eq!(session_artifact_files(home).len(), 1);

        let view = view_128k("exec_shell", &output);
        assert!(!view.needs_full_output_artifact);
        assert!(view.text.chars().count() <= budget_128k());
        assert!(view.text.starts_with("\n    HEAD START"));
        assert!(view.text.ends_with("TAIL END \n"));
        assert_eq!(
            view.text.matches("omitted range recovery:").count(),
            1,
            "exactly one footer: {}",
            &view.text[..200]
        );
        assert!(view.text.contains("ref=\"art_call-spill\""));
        assert_eq!(session_artifact_files(home).len(), 1);
    });
}

#[test]
fn legacy_spill_without_ref_says_no_tool_call_reaches_it() {
    let head = "h".repeat(32 * 1024);
    let tail = "t".repeat(8 * 1024);
    let preview = format!("{head}\n\n… footer …\n\n…\n{tail}");
    let output = ToolResult::success(preview).with_metadata(json!({
        "spillover_path": "/tmp/tool_outputs/call-legacy.txt",
        "retained_head_bytes": head.len(),
        "retained_tail_bytes": tail.len(),
        "original_byte_count": 300_000,
        "truncated": true
    }));

    let view = view_128k("exec_shell", &output);
    assert!(!view.needs_full_output_artifact);
    assert!(view.text.chars().count() <= budget_128k());
    assert!(view.text.contains("/tmp/tool_outputs/call-legacy.txt"));
    assert!(view.text.contains("no tool call reaches this copy"));
    assert!(!view.text.contains("retrieve_tool_result"));
}

#[test]
fn structured_run_tests_summary_is_recoverable_and_leads_with_failures() {
    with_artifact_home(|home| {
        let stdout = format!(
            "{}test result: FAILED. 1 failed",
            "test ok ... ok\n".repeat(3_000)
        );
        let raw = json!({
            "success": false,
            "exit_code": 101,
            "stdout": stdout,
            "stderr": "",
            "command": "(cd /repo && cargo test)"
        })
        .to_string();
        let mut output = ToolResult::success(raw.clone()).with_metadata(json!({
            "summary": "1 test failed: tools::git::tests::diff_keeps_the_last_file"
        }));
        assert!(view_128k("run_tests", &output).needs_full_output_artifact);
        assert!(
            crate::tools::truncate::preserve_full_output_for_model_context(
                &mut output,
                "call-tests",
                "run_tests",
                SESSION_6508,
            )
        );

        let view = view_128k("run_tests", &output);
        assert!(!view.needs_full_output_artifact);
        assert!(view.text.chars().count() <= budget_128k());
        let failures = view
            .text
            .find("failure summary: 1 test failed: tools::git::tests::diff_keeps_the_last_file")
            .expect("failure summary inline");
        assert!(failures < view.text.find("stdout:").expect("stdout"));
        assert!(view.text.contains("test result: FAILED"));
        assert!(view.text.contains("ref=\"art_call-tests\""));

        let files = session_artifact_files(home);
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read_to_string(&files[0]).expect("artifact"), raw);
    });
}

#[test]
fn display_compaction_never_writes_artifacts() {
    // The TUI builds its API-message copy with the same pure view.
    with_artifact_home(|home| {
        let output = ToolResult::success("x".repeat(60_000));
        let text = compact_tool_result_for_route(
            ProviderKind::Deepseek,
            ROUTE_128K,
            None,
            "exec_shell",
            &output,
        );
        assert!(text.chars().count() <= budget_128k());
        assert!(session_artifact_files(home).is_empty());
    });
}

#[test]
fn evidence_bounded_preview_is_not_recompacted() {
    // The adaptive evidence envelope already produced an honest bounded
    // preview (head + footer with the recovery path + tail). The context
    // compactor must pass it through untouched, even beyond the 12K hard
    // limit — re-compacting would strip the recovery contract.
    let content = format!(
        "\n\t{}\n\n… 19.0 KiB of output omitted (123 lines) — full output at /tmp/art_call.txt; read it back with the read_file tool or with sed line ranges\n\n…\n{} \n",
        "h".repeat(32 * 1024),
        "t".repeat(8 * 1024)
    );
    let output = ToolResult::success(content.clone()).with_metadata(json!({
        "evidence_available": true,
        "truncated": true,
        "spillover_path": "/tmp/art_call.txt"
    }));

    let context = compact_tool_result_for_context("deepseek-v3.2-128k", "Bash", &output);
    assert_eq!(context, content);
    assert!(context.contains("full output at /tmp/art_call.txt"));
}

#[tokio::test]
async fn budgeted_read_result_is_not_truncated_a_second_time_by_the_context_compactor() {
    use crate::tools::spec::ToolSpec as _;

    // C05: `read` bounds itself to an explicit per-call byte budget. The 12K
    // context hard limit used to re-truncate that bounded result into a 900-
    // char snippet, discarding both the content and the continuation footer.
    let content = format!(
        "{}\n\n[Showing lines 1-100 of 2000 (100000-byte output budget). Use offset=101 to continue.]",
        "r".repeat(90_000)
    );
    let budgeted = ToolResult::success(content.clone()).with_metadata(json!({
        "evidence_routing": "inline",
        "read_budget_bytes": 100_000
    }));
    let passed_through = compact_tool_result_for_context("deepseek-v3.2-128k", "read", &budgeted);
    assert_eq!(passed_through, content);
    assert!(passed_through.contains("Use offset=101 to continue"));

    // The same bytes without a declared budget still take the ordinary path,
    // which is what proves the metadata (not the tool name) did the work.
    let unbudgeted = ToolResult::success(content.clone());
    let compacted = compact_tool_result_for_context("deepseek-v3.2-128k", "read", &unbudgeted);
    assert!(compacted.contains(codewhale_tools::names::SPILLOVER_RECOVERY_HINT));
    assert!(compacted.len() < content.len());

    // A result that overran its own declared budget is not exempt.
    let overrun = ToolResult::success(content).with_metadata(json!({
        "read_budget_bytes": 1_000
    }));
    let compacted_overrun = compact_tool_result_for_context("deepseek-v3.2-128k", "read", &overrun);
    assert!(compacted_overrun.contains(codewhale_tools::names::SPILLOVER_RECOVERY_HINT));

    let workspace = tempdir().expect("tempdir");
    let line = format!("    {}\n", "x".repeat(95));
    let source = format!("\n\t{}", line.repeat(1_200));
    tokio::fs::write(workspace.path().join("budget.txt"), &source)
        .await
        .unwrap();
    let context = ToolContext::new(workspace.path());
    for max_bytes in [1, 90_000, 500_000] {
        let native = crate::tools::file_tool::ReadTool
            .execute(
                json!({"path": "budget.txt", "max_bytes": max_bytes}),
                &context,
            )
            .await
            .expect("native read");
        let metadata = native.metadata.as_ref().unwrap();
        assert!(native.content.len() as u64 <= metadata["read_budget_bytes"].as_u64().unwrap());
        let view = view_128k("read", &native);
        assert_eq!(view.text, native.content, "max_bytes={max_bytes}");
        assert!(!view.needs_full_output_artifact);
        let body = native.content.split("\n\n[").next().unwrap();
        assert!(body.len() <= (max_bytes as usize).max(100_000));
        if max_bytes == 90_000 {
            let mut source_budget_only = native.clone();
            source_budget_only.metadata.as_mut().unwrap()["read_budget_bytes"] = json!(max_bytes);
            let cut = view_128k("read", &source_budget_only);
            assert_ne!(cut.text, native.content);
            assert!(cut.needs_full_output_artifact);
        }
    }
}

#[test]
fn codex_tool_retention_uses_oauth_route_window_not_asmall_contract_model_window() {
    let content = "route-effective context\n".repeat(900);
    let output = ToolResult::success(content.clone());
    let limits = codewhale_config::route::RouteLimits {
        context_tokens: Some(272_000),
        input_tokens: None,
        output_tokens: None,
        output_declared: false,
    };

    // The budget follows the route's 272K window (3% of it, 32,640
    // characters), so this 21.6K result reaches the model whole.
    assert!(
        crate::route_budget::route_inline_char_budget_for_route(
            ProviderKind::OpenaiCodex,
            "gpt-5.5",
            Some(limits),
        ) >= 32_640
    );
    let context = compact_tool_result_for_route(
        ProviderKind::OpenaiCodex,
        "gpt-5.5",
        Some(limits),
        "read_file",
        &output,
    );

    assert_eq!(context, content);
}

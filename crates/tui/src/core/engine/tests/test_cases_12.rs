#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn auto_review_asks_the_user_and_returns_the_answer() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let arguments = json!({
        "questions": [{
            "header": "Choice",
            "id": "choice",
            "question": "Which path should I take?",
            "options": [
                {"label": "A", "description": "Take path A"},
                {"label": "B", "description": "Take path B"}
            ]
        }]
    })
    .to_string();
    let tool_delta = json!({
        "id": "chatcmpl-auto-review-question",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_auto_review_question",
                "type": "function",
                "function": {
                    "name": REQUEST_USER_INPUT_NAME,
                    "arguments": arguments,
                },
            }]},
            "finish_reason": serde_json::Value::Null,
        }],
    });
    let tool_finish = json!({
        "id": "chatcmpl-auto-review-question",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
    });
    let tool_call_sse = format!("data: {tool_delta}\n\ndata: {tool_finish}\n\ndata: [DONE]\n\n");
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("take-path-b"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(done_sse),
        )
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(tool_call_sse),
        )
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;

    let api_config = Config::default()
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let (engine, handle) = Engine::new(
        EngineConfig {
            model: codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string(),
            workspace: workspace.path().to_path_buf(),
            snapshots_enabled: false,
            subagents_enabled: false,
            ..EngineConfig::default()
        },
        &api_config,
    );
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "continue autonomously".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&api_config, codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: true,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Auto,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::ExternalUser,
            submission_id: None,
        }))
        .await
        .expect("send Auto-Review model turn");

    let mut saw_question = false;
    let mut saw_tool_result = false;
    let mut saw_turn_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for Auto-Review question event")
    {
        match event {
            Event::UserInputRequired { id, .. } => {
                // Auto-Review reviews tool approvals only; a question still
                // reaches the user.
                handle
                    .submit_user_input(
                        id,
                        crate::tools::user_input::UserInputResponse {
                            answers: vec![crate::tools::user_input::UserInputAnswer {
                                id: "choice".to_string(),
                                label: "B".to_string(),
                                value: "take-path-b".to_string(),
                            }],
                        },
                    )
                    .await
                    .expect("submit answer");
                saw_question = true;
            }
            Event::ApprovalRequired { .. } => {
                panic!("an Auto-Review question must not become an approval")
            }
            Event::ToolCallComplete { name, result, .. } if name == REQUEST_USER_INPUT_NAME => {
                let result = result.expect("the answered question succeeds");
                assert!(result.success, "{result:?}");
                assert!(result.content.contains("take-path-b"), "{result:?}");
                saw_tool_result = true;
            }
            Event::TurnComplete { status, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed);
                saw_turn_complete = true;
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    assert!(saw_question);
    assert!(saw_tool_result);
    assert!(saw_turn_complete);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn full_access_permission_allow_runs_background_destructive_shell_without_a_hold() {
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;
    let victim = workspace.path().join("must-survive");
    fs::write(&victim, "guarded\n").expect("write guarded fixture");
    // Full Access owns the call: neither a remembered workspace grant nor the
    // built-in hold may strand it. The fixture writes its own sentinel, so an
    // execution regression is visible without any destructive command running.
    let command = format!("echo \"rm -rf /\" > \"{}\"", victim.display());
    let allow_rule = codewhale_execpolicy::ToolAskRule::exec_shell(command.clone())
        .into_exact_workspace_allow(workspace.path().to_string_lossy().into_owned());
    let tool_input = json!({
        "action": "run",
        "command": command,
        "background": true,
    });
    let arguments = serde_json::to_string(&tool_input).expect("serialize tool arguments");

    let tool_delta = json!({
        "id": "chatcmpl-bg",
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [{
                "index": 0,
                "id": "call_bg",
                "type": "function",
                "function": {"name": "Bash", "arguments": arguments},
            }]},
            "finish_reason": serde_json::Value::Null,
        }],
    });
    let tool_finish = json!({
        "id": "chatcmpl-bg",
        "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
    });
    let tool_call_sse = format!("data: {tool_delta}\n\ndata: {tool_finish}\n\ndata: [DONE]\n\n");
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("call_bg"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(done_sse),
        )
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(tool_call_sse),
        )
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;

    let api_config = Config::default()
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let engine_config = EngineConfig {
        model: codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string(),
        workspace: workspace.path().to_path_buf(),
        snapshots_enabled: false,
        subagents_enabled: false,
        exec_policy_engine: codewhale_execpolicy::ExecPolicyEngine::with_rulesets(vec![
            codewhale_execpolicy::Ruleset::user(vec![], vec![]).with_ask_rules(vec![allow_rule]),
        ]),
        ..EngineConfig::default()
    };
    assert_eq!(
        exec_shell_ask_rule_decision(
            &engine_config,
            "exec_shell",
            &tool_input,
            workspace.path(),
            ApprovalMode::Bypass,
        ),
        Some(ToolAskRuleDecision::Allow),
        "precondition: the remembered grant matches and Full Access adds no hold"
    );
    let (engine, handle) = Engine::new(engine_config, &api_config);
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "please run a background shell".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&api_config, codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: true,
            trust_mode: true,
            auto_approve: true,
            approval_mode: ApprovalMode::Bypass,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::ExternalUser,
            submission_id: None,
        }))
        .await
        .expect("send model turn");

    let mut saw_tool_result = false;
    let mut saw_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for engine event")
    {
        match event {
            Event::ApprovalRequired { .. } => {
                panic!("Full Access must not prompt for a remembered grant")
            }
            Event::ToolCallComplete { name, result, .. } => {
                if name == "Bash" {
                    saw_tool_result = true;
                    let result = result.expect("Full Access runs the granted command");
                    assert!(result.success, "unexpected shell failure: {result:?}");
                }
            }
            Event::TurnComplete { status, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed);
                saw_complete = true;
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    let granted = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let text = crate::test_support::read_shell_sentinel(&victim);
            if !text.is_empty() && text != "guarded" {
                break text;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the granted command never wrote its sentinel");

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    assert!(saw_tool_result);
    assert!(saw_complete);
    assert_eq!(
        granted, "rm -rf /",
        "the granted command must run to completion"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn yolo_mode_does_not_prompt_for_background_shell() {
    // #3883: the durable-review floor keys on what the command does, not on
    // "not provably read-only". An ordinary background command in YOLO must
    // run without a prompt. Full Access skips the floor entirely; the
    // reviewing postures keep it (see the policy-level tests).
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;

    let tool_call_sse = concat!(
        "data: {\"id\":\"chatcmpl-bgok\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call_bgok\",\"type\":\"function\",\"function\":{\"name\":\"Bash\",",
        "\"arguments\":\"{\\\"action\\\":\\\"run\\\",\\\"command\\\":\\\"echo bg-yolo-no-prompt\\\",\\\"background\\\":true}\"}}",
        "]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-bgok\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("bg-yolo-no-prompt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(done_sse),
        )
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(tool_call_sse),
        )
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;

    let api_config = Config::default()
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let (engine, handle) = Engine::new(
        EngineConfig {
            model: codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string(),
            workspace: workspace.path().to_path_buf(),
            snapshots_enabled: false,
            subagents_enabled: false,
            ..EngineConfig::default()
        },
        &api_config,
    );
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "please run a background shell".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&api_config, codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: true,
            trust_mode: true,
            auto_approve: true,
            approval_mode: ApprovalMode::Auto,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::ExternalUser,
            submission_id: None,
        }))
        .await
        .expect("send model turn");

    let mut saw_tool_result = false;
    let mut saw_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for engine event")
    {
        match event {
            Event::ApprovalRequired { .. } => {
                panic!("YOLO mode must not prompt for an ordinary background shell command");
            }
            Event::ToolCallComplete { name, result, .. } => {
                if name == "Bash" {
                    saw_tool_result = true;
                    let result = result.expect("shell result");
                    assert!(result.success, "{result:?}");
                    assert!(
                        result.content.contains("Background task started"),
                        "expected a background start, got: {result:?}"
                    );
                }
            }
            Event::TurnComplete { status, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed);
                saw_complete = true;
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    assert!(saw_tool_result);
    assert!(saw_complete);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn yolo_mode_executes_publish_like_shell_without_prompt() {
    // #4595: Full Access (Bypass/YOLO) is truly full access — the publish
    // floor prompts only in Ask/Auto-Review postures. The regression guard is
    // the absence of ApprovalRequired; execution itself may fail (the tempdir
    // is not a git repo), which is fine.
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;

    let tool_call_sse = concat!(
        "data: {\"id\":\"chatcmpl-publish\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call_publish\",\"type\":\"function\",\"function\":{\"name\":\"Bash\",",
        "\"arguments\":\"{\\\"action\\\":\\\"run\\\",\\\"command\\\":\\\"git push origin main\\\",\\\"background\\\":true}\"}}",
        "]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-publish\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"ack\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("call_publish"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(done_sse),
        )
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(tool_call_sse),
        )
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;

    let api_config = Config::default()
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let (engine, handle) = Engine::new(
        EngineConfig {
            model: codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string(),
            workspace: workspace.path().to_path_buf(),
            snapshots_enabled: false,
            subagents_enabled: false,
            ..EngineConfig::default()
        },
        &api_config,
    );
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "please publish this crate".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&api_config, codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: true,
            trust_mode: true,
            auto_approve: true,
            approval_mode: ApprovalMode::Bypass,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::ExternalUser,
            submission_id: None,
        }))
        .await
        .expect("send model turn");

    let mut saw_tool_complete = false;
    let mut saw_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for engine event")
    {
        match event {
            Event::ApprovalRequired {
                tool_name,
                description,
                ..
            } => {
                panic!(
                    "Full Access must not prompt for publish-like shell \
                     (#4595); got prompt for {tool_name}: {description}"
                );
            }
            Event::ToolCallComplete { name, .. } if name == "Bash" => {
                // Execution outcome is irrelevant (the tempdir is not a git
                // repo); the contract is that it ran without a prompt.
                saw_tool_complete = true;
            }
            Event::TurnComplete { status, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed);
                saw_complete = true;
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    assert!(
        saw_tool_complete,
        "the publish-like shell should execute without a prompt under Full Access"
    );
    assert!(saw_complete, "the publish-like turn should complete");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn yolo_mode_does_not_prompt_for_mcp_action() {
    // #3790: MCP mutations are governed by the selected mode, just like shell.
    // YOLO must not emit an approval request for a non-read-only MCP tool; this
    // fixture has no GitHub MCP server, so execution may fail after the no-prompt
    // planning decision. The regression guard is the absence of ApprovalRequired.
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let _lock = lock_test_env();
    let workspace = tempdir().expect("tempdir");
    let server = MockServer::start().await;

    let tool_call_sse = concat!(
        "data: {\"id\":\"chatcmpl-mcp\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call_mcp\",\"type\":\"function\",\"function\":{\"name\":\"mcp_github_create_pull_request\",",
        "\"arguments\":\"{\\\"title\\\":\\\"test\\\",\\\"body\\\":\\\"body\\\"}\"}}",
        "]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-mcp\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let done_sse = concat!(
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"ack\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-done\",\"choices\":[{\"index\":0,\"delta\":{},",
        "\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("MCP tool failed"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(done_sse),
        )
        .expect(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(tool_call_sse),
        )
        .expect(1)
        .with_priority(2)
        .mount(&server)
        .await;

    let api_config = Config::default()
    .with_legacy_root(Some("test-key".to_string()), Some(server.uri()));
    let (engine, handle) = Engine::new(
        EngineConfig {
            model: codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL.to_string(),
            workspace: workspace.path().to_path_buf(),
            snapshots_enabled: false,
            subagents_enabled: false,
            ..EngineConfig::default()
        },
        &api_config,
    );
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "please open the PR".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&api_config, codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: true,
            trust_mode: true,
            auto_approve: true,
            approval_mode: ApprovalMode::Bypass,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::ExternalUser,
            submission_id: None,
        }))
        .await
        .expect("send model turn");

    let mut saw_mcp_result = false;
    let mut saw_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(model_turn_event_timeout(), rx.recv())
        .await
        .expect("timed out waiting for engine event")
    {
        match event {
            Event::ApprovalRequired { .. } => {
                panic!("YOLO mode must not prompt for an MCP action");
            }
            Event::ToolCallComplete { name, result, .. }
                if name == "mcp_github_create_pull_request" =>
            {
                saw_mcp_result = true;
                let err = result
                    .expect_err("unconfigured MCP server should fail after no-prompt planning");
                assert!(
                    err.to_string().contains("MCP tool failed"),
                    "unexpected MCP error: {err:?}"
                );
            }
            Event::TurnComplete { status, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed);
                saw_complete = true;
                break;
            }
            _ => {}
        }
    }
    drop(rx);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    assert!(
        saw_mcp_result,
        "the MCP tool should execute without an approval gate"
    );
    assert!(saw_complete, "the YOLO MCP turn should complete");
}

#[tokio::test]
async fn run_shell_command_op_preserves_plan_mode_shell_block() {
    let (mut engine, handle) = Engine::new(EngineConfig::default(), &Config::default());

    engine
        .handle_run_shell_command(
            "echo blocked".to_string(),
            AppMode::Plan,
            false,
            false,
            false,
            ApprovalMode::Suggest,
        )
        .await;

    let mut saw_complete = false;
    let mut saw_turn_complete = false;
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = rx.recv().await {
        match event {
            Event::ApprovalRequired { .. } => {
                panic!("Plan mode shell should be blocked before approval");
            }
            Event::ToolCallComplete { name, result, .. } => {
                saw_complete = true;
                assert_eq!(name, "Bash");
                let err = result.expect_err("plan shell should fail");
                assert!(
                    err.to_string()
                        .contains("Tool 'bash' is unavailable in Plan mode"),
                    "{err}"
                );
            }
            Event::TurnComplete { status, .. } => {
                saw_turn_complete = true;
                assert_eq!(status, TurnOutcomeStatus::Failed);
                break;
            }
            _ => {}
        }
    }

    assert!(saw_complete);
    assert!(saw_turn_complete);
}

#[test]
fn deferred_tool_preflight_skips_already_active_tools() {
    let mut tool = api_tool("deferred_tool");
    tool.defer_loading = Some(true);
    let catalog = vec![tool];
    let mut active = HashSet::from(["deferred_tool".to_string()]);

    assert!(
        preflight_requested_deferred_tool("deferred_tool", &json!({}), &catalog, &mut active,)
            .is_none(),
        "already active tools should execute normally"
    );
}

#[test]
fn turn_tool_registry_builder_keeps_plan_primitive_identity() {
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    let registry = engine
        .build_turn_tool_registry_builder(
            AppMode::Plan,
            engine.config.todos.clone(),
            engine.config.plan_state.clone(),
        )
        .build(engine.build_tool_context(AppMode::Plan, false));

    for primitive in ["read", "write", "edit", "bash"] {
        assert!(registry.contains(primitive), "missing {primitive}");
    }
    for hidden in ["File", "Bash", "read_file", "write_file", "edit_file"] {
        assert!(registry.contains(hidden), "missing hidden {hidden}");
    }
    assert!(registry.contains("list_dir"));
    let api_names = registry
        .to_api_tools()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<HashSet<_>>();
    for primitive in ["read", "write", "edit", "bash"] {
        assert!(api_names.contains(primitive), "missing visible {primitive}");
    }
    for hidden in ["File", "Bash", "read_file", "write_file", "edit_file"] {
        assert!(!api_names.contains(hidden), "visible hidden tool {hidden}");
    }
    assert!(!registry.contains("exec_shell"));
    assert!(!registry.contains("exec_shell_wait"));
    assert!(!registry.contains("exec_shell_interact"));
    assert!(!registry.contains("task_shell_start"));
    assert!(!registry.contains("task_create"));
    assert!(!registry.contains("task_gate_run"));
    assert!(!registry.contains("rlm"));
    assert!(!registry.contains("fim_edit"));
    assert!(registry.contains("update_plan"));
    assert!(registry.contains("create_goal"));
    assert!(registry.contains("get_goal"));
    assert!(registry.contains("update_goal"));
    assert!(registry.contains("tasks"));
    assert!(!registry.contains("task_list"));
    assert!(!registry.contains("task_read"));
    assert!(registry.contains("handle_read"));
    assert_eq!(
        registry.context().shell_policy,
        codewhale_agent_contract::worker_profile::ShellPolicy::None
    );
}

/// Plan mode toggle must not change the byte representation of the tool
/// catalog head. DeepSeek's KV prefix cache includes the tools array in
/// the immutable prefix; if toggling between Plan and Agent mode changes
/// the tool bytes, every mode switch forces a full re-prefill.
///
/// This test verifies two invariants:
/// 1. Building the catalog twice for the same mode produces identical bytes.
/// 2. The head of the catalog (non-deferred tools) preserves its order
///    when deferred tools are activated mid-session.
#[test]
fn plan_mode_toggle_preserves_catalog_byte_stability() {
    let always_load = HashSet::new();

    // Build catalog for Plan mode twice — must be byte-identical.
    let plan_native = vec![
        api_tool("read"),
        api_tool("write"),
        api_tool("edit"),
        api_tool("bash"),
        api_tool("agent"),
        api_tool("tool_search"),
        api_tool("list_dir"),
    ];
    let plan_mcp = vec![api_tool("mcp_search"), api_tool("mcp_write")];

    let catalog_a = build_model_tool_catalog(
        plan_native.clone(),
        plan_mcp.clone(),
        AppMode::Plan,
        &always_load,
    );
    let catalog_b = build_model_tool_catalog(
        plan_native.clone(),
        plan_mcp.clone(),
        AppMode::Plan,
        &always_load,
    );

    let json_a = serde_json::to_string(&catalog_a).unwrap();
    let json_b = serde_json::to_string(&catalog_b).unwrap();
    assert_eq!(
        json_a, json_b,
        "building the catalog twice for Plan mode must produce identical bytes"
    );

    // Build catalog for Agent mode twice — must be byte-identical.
    let agent_catalog_a = build_model_tool_catalog(
        plan_native.clone(),
        plan_mcp.clone(),
        AppMode::Agent,
        &always_load,
    );
    let agent_catalog_b = build_model_tool_catalog(
        plan_native.clone(),
        plan_mcp.clone(),
        AppMode::Agent,
        &always_load,
    );

    let agent_json_a = serde_json::to_string(&agent_catalog_a).unwrap();
    let agent_json_b = serde_json::to_string(&agent_catalog_b).unwrap();
    assert_eq!(
        agent_json_a, agent_json_b,
        "building the catalog twice for Agent mode must produce identical bytes"
    );

    // Modes keep the same primitive identities; central authority gates decide
    // whether an advertised write/edit/bash call may execute.
    let plan_names: Vec<&str> = catalog_a
        .iter()
        .filter(|t| !t.defer_loading.unwrap_or(false))
        .map(|t| t.name.as_str())
        .collect();
    let agent_names: Vec<&str> = agent_catalog_a
        .iter()
        .filter(|t| !t.defer_loading.unwrap_or(false))
        .map(|t| t.name.as_str())
        .collect();

    let expected_head = ["agent", "bash", "edit", "read", "tool_search", "write"];
    assert_eq!(plan_names, expected_head);
    assert_eq!(agent_names, expected_head);

    // Verify that activating a deferred tool mid-session appends to the
    // tail without reordering the head.
    let mut tools_with_deferred = plan_native.clone();
    tools_with_deferred.push({
        let mut t = api_tool("deferred_search");
        t.defer_loading = Some(true);
        t
    });
    let catalog_with_deferred = build_model_tool_catalog(
        tools_with_deferred,
        plan_mcp.clone(),
        AppMode::Agent,
        &always_load,
    );

    // Activate the deferred tool.
    let mut active: HashSet<String> = catalog_with_deferred
        .iter()
        .filter(|t| !t.defer_loading.unwrap_or(false))
        .map(|t| t.name.clone())
        .collect();
    active.insert("deferred_search".to_string());

    let listed = active_tools_for_step(&catalog_with_deferred, &active);
    let listed_names: Vec<&str> = listed.iter().map(|t| t.name.as_str()).collect();

    // The head (non-deferred tools) must still be in their original order.
    let head_names: Vec<&str> = catalog_with_deferred
        .iter()
        .filter(|t| !t.defer_loading.unwrap_or(false))
        .map(|t| t.name.as_str())
        .collect();
    assert!(
        listed_names.starts_with(&head_names),
        "activating a deferred tool must not reorder the catalog head: \
         expected {head_names:?} as prefix, got {listed_names:?}"
    );
    // The deferred tool must be at the tail.
    assert_eq!(
        listed_names.last(),
        Some(&"deferred_search"),
        "deferred tool must be appended at the tail"
    );
}

#[test]
fn parent_turn_registry_includes_goal_tools_for_all_modes() {
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());

    for mode in [AppMode::Plan, AppMode::Agent, AppMode::Operate] {
        let registry = engine
            .build_turn_tool_registry_builder(
                mode,
                engine.config.todos.clone(),
                engine.config.plan_state.clone(),
            )
            .build(engine.build_tool_context(mode, false));

        for name in ["create_goal", "get_goal", "update_goal"] {
            assert!(
                registry.contains(name),
                "parent {mode:?} registry should expose {name}"
            );
        }
    }
}

#[test]
fn plan_mode_registry_can_expose_agent_launcher_without_shell_tools() {
    let tmp = tempdir().expect("tempdir");
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    let context = engine.build_tool_context(AppMode::Plan, false);
    let client = CodewhaleClient::new(
        &Config::default()
        .with_legacy_root(Some("test-key".to_string()), None),
    )
    .expect("stub client");
    let manager = crate::tools::subagent::new_shared_subagent_manager(tmp.path().to_path_buf(), 4);
    let mut runtime = SubAgentRuntime::new(
        client,
        DEFAULT_TEXT_MODEL.to_string(),
        context.clone(),
        false,
        None,
        manager.clone(),
    )
    .with_agent_tool_surface_options(
        engine.agent_tool_surface_options(shell_policy_for_mode(AppMode::Plan, false)),
    );
    runtime.worker_profile = WorkerRuntimeProfile::for_role(FleetRole::Planner);

    let registry = engine
        .build_turn_tool_registry_builder(
            AppMode::Plan,
            engine.config.todos.clone(),
            engine.config.plan_state.clone(),
        )
        .with_subagent_tools(manager, runtime)
        .build(context);

    assert!(
        registry.contains("agent"),
        "Plan mode should be able to request focused read-only sub-agents"
    );
    assert!(
        !registry.contains("exec_shell"),
        "Plan mode must remain shell-free while exposing sub-agent delegation"
    );
}

#[test]
fn mode_invariant_matrix_covers_context_catalog_subagents_and_prompt_metadata() {
    use codewhale_sandbox::SandboxPolicy;
    use codewhale_agent_contract::worker_profile::ShellPolicy;
    use ApprovalMode;

    #[derive(Clone, Copy)]
    enum ExpectedSandbox {
        ReadOnly,
        WorkspaceWrite,
        DangerFullAccess,
    }

    struct ModeCase {
        name: &'static str,
        mode: AppMode,
        shell_policy: ShellPolicy,
        sandbox: ExpectedSandbox,
        trust_mode: bool,
        auto_approve: bool,
        approval_mode: ApprovalMode,
        plan_hint: bool,
    }

    let cases = [
        ModeCase {
            name: "plan",
            mode: AppMode::Plan,
            shell_policy: ShellPolicy::None,
            sandbox: ExpectedSandbox::ReadOnly,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            plan_hint: true,
        },
        ModeCase {
            name: "agent",
            mode: AppMode::Agent,
            shell_policy: ShellPolicy::Full,
            sandbox: ExpectedSandbox::WorkspaceWrite,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            plan_hint: false,
        },
        ModeCase {
            name: "agent-full-access",
            mode: AppMode::Agent,
            shell_policy: ShellPolicy::Full,
            sandbox: ExpectedSandbox::DangerFullAccess,
            trust_mode: true,
            auto_approve: true,
            approval_mode: ApprovalMode::Bypass,
            plan_hint: false,
        },
        ModeCase {
            name: "operate",
            mode: AppMode::Operate,
            shell_policy: ShellPolicy::Full,
            sandbox: ExpectedSandbox::WorkspaceWrite,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            plan_hint: false,
        },
    ];

    for case in cases {
        let tmp = tempdir().expect("tempdir");
        let config = EngineConfig {
            workspace: tmp.path().to_path_buf(),
            allow_shell: true,
            trust_mode: case.trust_mode,
            ..EngineConfig::default()
        };
        let (mut engine, _handle) = Engine::new(config, &Config::default());
        engine.current_mode = case.mode;
        engine.session.allow_shell = true;
        engine.session.trust_mode = case.trust_mode;
        engine.session.auto_approve = case.auto_approve;
        engine.session.approval_mode = case.approval_mode;

        let policy = effective_input_policy(
            UserInputProvenance::ExternalUser,
            case.mode,
            "continue",
            engine.session.allow_shell,
            engine.session.trust_mode,
            engine.session.auto_approve,
            engine.session.approval_mode,
        );
        assert_eq!(policy.mode, case.mode, "{}", case.name);
        assert_eq!(policy.trust_mode, case.trust_mode, "{}", case.name);
        assert_eq!(policy.auto_approve, case.auto_approve, "{}", case.name);
        assert_eq!(policy.approval_mode, case.approval_mode, "{}", case.name);
        assert!(policy.allow_shell, "{}", case.name);

        let context = engine.build_tool_context(case.mode, case.auto_approve);
        assert_eq!(context.shell_policy, case.shell_policy, "{}", case.name);
        assert_eq!(context.trust_mode, case.trust_mode, "{}", case.name);
        assert_eq!(context.auto_approve, case.auto_approve, "{}", case.name);
        assert_eq!(
            context.shell_network_denied_hint.is_some(),
            case.plan_hint,
            "{}",
            case.name
        );
        let sandbox = context
            .elevated_sandbox_policy
            .as_ref()
            .expect("mode context should always carry an elevated sandbox policy");
        match (case.sandbox, sandbox) {
            (ExpectedSandbox::ReadOnly, SandboxPolicy::ReadOnly { .. }) => {}
            (
                ExpectedSandbox::WorkspaceWrite,
                SandboxPolicy::WorkspaceWrite {
                    writable_roots,
                    network_access,
                    ..
                },
            ) => {
                assert_eq!(
                    writable_roots,
                    &vec![tmp.path().to_path_buf()],
                    "{}",
                    case.name
                );
                // Workspace-write grants writes, not egress. Network is a
                // separate, explicit grant in every mode that reaches here.
                assert!(!*network_access, "{}", case.name);
            }
            (ExpectedSandbox::DangerFullAccess, SandboxPolicy::DangerFullAccess) => {}
            _ => panic!("{}: unexpected sandbox policy {sandbox:?}", case.name),
        }

        let client = CodewhaleClient::new(
            &Config::default()
            .with_legacy_root(Some("test-key".to_string()), None),
        )
        .expect("stub client");
        let manager =
            crate::tools::subagent::new_shared_subagent_manager(tmp.path().to_path_buf(), 4);
        let mut runtime = SubAgentRuntime::new(
            client,
            DEFAULT_TEXT_MODEL.to_string(),
            context.clone(),
            false,
            None,
            manager.clone(),
        )
        .with_agent_tool_surface_options(
            engine.agent_tool_surface_options(shell_policy_for_mode(case.mode, true)),
        );
        runtime.worker_profile = WorkerRuntimeProfile::for_role(match case.mode {
            AppMode::Plan => FleetRole::Planner,
            _ => FleetRole::Worker,
        });

        let registry = engine
            .build_turn_tool_registry_builder(
                case.mode,
                engine.config.todos.clone(),
                engine.config.plan_state.clone(),
            )
            .with_subagent_tools(manager, runtime)
            .build(context);
        assert!(registry.contains("agent"), "{}", case.name);
        // Primitive identity is mode-stable: both the lowercase `bash` and
        // its legacy `Bash` transcript alias register in every mode, and the
        // mode gate lives at the execution/catalog boundary (Plan refuses
        // shell rather than unregistering the identity).
        assert!(registry.contains("bash"), "{}", case.name);
        assert!(registry.contains("Bash"), "{}", case.name);
        assert!(
            !registry.contains("exec_shell"),
            "{}: retired exec_shell must remain absent",
            case.name
        );

        let msg = engine.user_text_message_with_turn_metadata_for_route(
            "check current policy".to_string(),
            DEFAULT_TEXT_MODEL,
            false,
            None,
            false,
        );
        let metadata = msg.content.last().expect("turn metadata block");
        let ContentBlock::Text { text, .. } = metadata else {
            panic!("{}: expected text metadata block", case.name);
        };
        assert!(
            text.contains(&format!(
                "Current permission posture: {}",
                case.approval_mode.permission_chip_label()
            )),
            "{}: {text}",
            case.name
        );
        // Mode is enforced by runtime policy and the live tool catalog. The
        // turn block carries only the independently actionable permission posture.
        assert!(
            !text.contains("Current mode:"),
            "{}: turn metadata must not repeat the mode: {text}",
            case.name
        );
        let prefix = crate::prompts::system_prompt_flat_text(
            &crate::prompts::system_prompt_for_mode_with_context_skills_session_and_approval(
                &engine.config.workspace,
                None,
                None,
                None,
                crate::prompts::PromptSessionContext {
                    mode: case.mode,
                    ..Default::default()
                },
            ),
        );
        assert!(
            !prefix.contains("##### Mode:"),
            "{}: mode doctrine must not enter the shared prompt: {prefix}",
            case.name
        );
    }
}

#[test]
fn engine_context_honors_stricter_config_under_full_access() {
    use codewhale_sandbox::SandboxPolicy;
    use ApprovalMode;

    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..EngineConfig::default()
    };
    let api_config = {
        let __cw_config_init_7_0 = Some("workspace-write".to_string());
        let mut __cw_config_init_7_value: Config = Config::default();
        __cw_config_init_7_value.sandbox_mode = __cw_config_init_7_0;
        __cw_config_init_7_value
    };
    let (mut engine, _handle) = Engine::new(config, &api_config);
    engine.session.approval_mode = ApprovalMode::Bypass;
    engine.session.auto_approve = true;

    let context = engine.build_tool_context(AppMode::Agent, true);
    assert!(matches!(
        context.elevated_sandbox_policy.as_ref(),
        Some(SandboxPolicy::WorkspaceWrite { writable_roots, .. })
            if *writable_roots == vec![tmp.path().to_path_buf()]
    ));
}

#[test]
fn mode_invariant_matrix_covers_provenance_authority_narrowing() {
    use ApprovalMode;

    struct ProvenanceCase {
        name: &'static str,
        provenance: UserInputProvenance,
        expected_mode: AppMode,
        expected_trust: bool,
        expected_auto: bool,
        expected_approval: ApprovalMode,
        expect_status: bool,
    }

    let cases = [
        ProvenanceCase {
            name: "external user",
            provenance: UserInputProvenance::ExternalUser,
            expected_mode: AppMode::Agent,
            expected_trust: true,
            expected_auto: true,
            expected_approval: ApprovalMode::Bypass,
            expect_status: false,
        },
        ProvenanceCase {
            name: "runtime continuation",
            provenance: UserInputProvenance::Runtime,
            expected_mode: AppMode::Agent,
            expected_trust: true,
            expected_auto: true,
            expected_approval: ApprovalMode::Bypass,
            expect_status: false,
        },
        ProvenanceCase {
            name: "sub-agent handoff",
            provenance: UserInputProvenance::SubAgentHandoff,
            expected_mode: AppMode::Agent,
            expected_trust: true,
            expected_auto: true,
            expected_approval: ApprovalMode::Bypass,
            expect_status: false,
        },
        ProvenanceCase {
            name: "imported transcript",
            provenance: UserInputProvenance::ImportedTranscript,
            expected_mode: AppMode::Agent,
            expected_trust: false,
            expected_auto: false,
            expected_approval: ApprovalMode::Suggest,
            expect_status: true,
        },
        ProvenanceCase {
            name: "memory recall",
            provenance: UserInputProvenance::MemoryRecall,
            expected_mode: AppMode::Agent,
            expected_trust: false,
            expected_auto: false,
            expected_approval: ApprovalMode::Suggest,
            expect_status: true,
        },
        ProvenanceCase {
            name: "assistant generated",
            provenance: UserInputProvenance::AssistantGenerated,
            expected_mode: AppMode::Agent,
            expected_trust: false,
            expected_auto: false,
            expected_approval: ApprovalMode::Suggest,
            expect_status: true,
        },
    ];

    for case in cases {
        let policy = effective_input_policy(
            case.provenance,
            AppMode::Agent,
            "continue",
            true,
            true,
            true,
            ApprovalMode::Bypass,
        );
        assert_eq!(policy.mode, case.expected_mode, "{}", case.name);
        assert_eq!(policy.trust_mode, case.expected_trust, "{}", case.name);
        assert_eq!(policy.auto_approve, case.expected_auto, "{}", case.name);
        assert_eq!(
            policy.approval_mode, case.expected_approval,
            "{}",
            case.name
        );
        assert!(policy.allow_shell, "{}", case.name);
        assert_eq!(
            policy.status().is_some(),
            case.expect_status,
            "{}",
            case.name
        );
    }
}

#[test]
fn agent_mode_can_build_auto_approved_tool_context() {
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());

    assert!(
        !engine
            .build_tool_context(AppMode::Agent, false)
            .auto_approve
    );
    assert!(engine.build_tool_context(AppMode::Agent, true).auto_approve);
}

#[test]
fn build_tool_context_preserves_read_snapshots_across_turns() {
    let workspace = tempdir().expect("tempdir");
    let path = workspace.path().join("observed.txt");
    fs::write(&path, "before\n").expect("write fixture");
    let config = EngineConfig {
        workspace: workspace.path().to_path_buf(),
        ..EngineConfig::default()
    };
    let (engine, _handle) = Engine::new(config, &Config::default());

    let read_turn = engine.build_tool_context(AppMode::Agent, false);
    read_turn.note_file_read(&path);

    let later_turn = engine.build_tool_context(AppMode::Agent, false);
    later_turn
        .require_fresh_file_read(&path, "observed.txt")
        .expect("a later turn should retain the session's fresh read snapshot");

    fs::write(&path, "changed contents\n").expect("change fixture");
    let err = later_turn
        .require_fresh_file_read(&path, "observed.txt")
        .expect_err("a retained snapshot must still reject stale edits");
    // Names the tool the model can actually call. `read_file` is a retired name
    // and pointing a stale-read refusal at it sent the model to a tool that does
    // not exist — the guard-then-bad-advice chain this release set out to close.
    assert!(
        err.to_string()
            .contains("changed since the last File action=\"read\" call"),
        "stale-read refusal must name a live tool, got: {err}"
    );
}

#[test]
fn build_tool_context_uses_typed_shell_policy_per_mode() {
    let mut config = EngineConfig {
        allow_shell: true,
        ..EngineConfig::default()
    };
    let (engine, _handle) = Engine::new(config.clone(), &Config::default());

    // Plan mode is shell-free and exposes no shell tools.
    assert_eq!(
        engine.build_tool_context(AppMode::Plan, false).shell_policy,
        codewhale_agent_contract::worker_profile::ShellPolicy::None
    );
    assert_eq!(
        engine
            .build_tool_context(AppMode::Agent, false)
            .shell_policy,
        codewhale_agent_contract::worker_profile::ShellPolicy::Full
    );

    config.allow_shell = false;
    let (engine, _handle) = Engine::new(config, &Config::default());
    assert_eq!(
        engine
            .build_tool_context(AppMode::Agent, false)
            .shell_policy,
        codewhale_agent_contract::worker_profile::ShellPolicy::None
    );
}

#[test]
fn turn_tool_context_uses_planned_authority_and_route_not_installed_session() {
    let (mut engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    engine.session.allow_shell = false;
    engine.session.trust_mode = false;
    engine.session.model = "installed-old-model".to_string();

    let authority = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        true,
        true,
        ApprovalMode::Bypass,
    );
    let route = TurnRouteContext {
        provider: ProviderKind::Deepseek,
        model: "planned-next-model".to_string(),
        capabilities: codewhale_config::route::RouteCapabilities::default(),
        limits: Some(codewhale_config::route::RouteLimits {
            context_tokens: Some(123_456),
            input_tokens: None,
            output_tokens: Some(4_096),
            output_declared: false,
        }),
        client: None,
        api_config: Box::new(Config::default()),
        locale_tag: engine.config.locale_tag.clone(),
        role_models: engine.subagent_role_models(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
    };

    let context = engine.build_tool_context_for_turn(&authority, &route);
    assert_eq!(
        context.shell_policy,
        codewhale_agent_contract::worker_profile::ShellPolicy::Full
    );
    assert!(context.trust_mode);
    assert!(context.auto_approve);
    assert_eq!(
        context.approval_mode,
        ApprovalMode::Bypass,
        "the turn's posture travels with the context its tools see"
    );
    // Auto-Review is the case the legacy bit cannot express: folding `false`
    // alone would read as Ask, so this is what proves the posture itself is
    // carried rather than re-derived.
    let auto_review = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        false,
        false,
        ApprovalMode::Auto,
    );
    assert_eq!(
        engine
            .build_tool_context_for_turn(&auto_review, &route)
            .approval_mode,
        ApprovalMode::Auto
    );
    assert_eq!(context.route_context_window, Some(123_456));
    assert_eq!(context.route_capabilities, route.capabilities);
    assert_eq!(
        context
            .session_objects
            .as_ref()
            .expect("session object snapshot")
            .model,
        "planned-next-model"
    );
}

/// `/network allow <host>` edits `config.toml` and tells the operator to retry
/// the command. The engine holds its policy by value, so that retry only works
/// when the tool context re-reads the document — this is the regression guard
/// for a session that kept refusing a host the file already allowed.
#[test]
fn tool_context_network_policy_follows_the_config_document_mid_session() {
    use codewhale_net::network_policy::{Decision, DecisionToml, NetworkPolicy, NetworkPolicyDecider};

    let dir = tempdir().expect("temp dir");
    let config_path = dir.path().join("config.toml");
    fs::write(
        &config_path,
        "[network]\ndefault = \"prompt\"\nallow = [\"api.github.com\"]\n",
    )
    .expect("write config");

    let api_config = {
        let __cw_config_init_8_0 = Some(config_path);
        let mut __cw_config_init_8_value: Config = Config::default();
        __cw_config_init_8_value.loaded_config_path = __cw_config_init_8_0;
        __cw_config_init_8_value
    };

    // The snapshot the engine was spawned with does not allow the host yet.
    let engine_config = EngineConfig {
        network_policy: Some(NetworkPolicyDecider::new(
            NetworkPolicy {
                default: DecisionToml::Prompt,
                ..NetworkPolicy::default()
            },
            None,
        )),
        ..EngineConfig::default()
    };
    let (engine, _handle) = Engine::new(engine_config, &api_config);

    let authority = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        true,
        true,
        ApprovalMode::Bypass,
    );
    let route = TurnRouteContext {
        provider: ProviderKind::Deepseek,
        model: "network-refresh-model".to_string(),
        capabilities: codewhale_config::route::RouteCapabilities::default(),
        limits: None,
        client: None,
        api_config: Box::new(Config::default()),
        locale_tag: engine.config.locale_tag.clone(),
        role_models: engine.subagent_role_models(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
    };

    let context = engine.build_tool_context_for_turn(&authority, &route);
    let decider = context
        .network_policy
        .as_ref()
        .expect("the configured policy is injected");
    assert_eq!(
        decider.evaluate("api.github.com", "Bash"),
        Decision::Allow,
        "the document already allows the host; the retry must see it"
    );
    assert_eq!(
        decider.evaluate("unlisted.example.com", "Bash"),
        Decision::Prompt,
        "a host the document does not name still prompts"
    );
}

/// `/network deny <host>` writes the same `[network]` table `/network allow`
/// does. A session that started without one has to adopt it — otherwise
/// tightening the policy mid-session needs the restart this fix removes — and a
/// session that started gated must not lose its policy because the table went
/// away.
#[test]
fn tool_context_adopts_a_policy_written_mid_session_and_never_ungates_one() {
    use codewhale_net::network_policy::{Decision, DecisionToml, NetworkPolicy, NetworkPolicyDecider};

    let dir = tempdir().expect("temp dir");
    let config_path = dir.path().join("config.toml");
    fs::write(
        &config_path,
        "[network]\ndefault = \"prompt\"\ndeny = [\"evil.example.com\"]\n",
    )
    .expect("write config");

    let api_config = {
        let __cw_config_init_9_0 = Some(config_path.clone());
        let mut __cw_config_init_9_value: Config = Config::default();
        __cw_config_init_9_value.loaded_config_path = __cw_config_init_9_0;
        __cw_config_init_9_value
    };

    // A session that started with no `[network]` table at all adopts the deny.
    let (engine, _handle) = Engine::new(EngineConfig::default(), &api_config);
    let authority = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        true,
        true,
        ApprovalMode::Bypass,
    );
    let route = TurnRouteContext {
        provider: ProviderKind::Deepseek,
        model: "network-adopt-model".to_string(),
        capabilities: codewhale_config::route::RouteCapabilities::default(),
        limits: None,
        client: None,
        api_config: Box::new(Config::default()),
        locale_tag: engine.config.locale_tag.clone(),
        role_models: engine.subagent_role_models(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
    };
    let context = engine.build_tool_context_for_turn(&authority, &route);
    let decider = context
        .network_policy
        .as_ref()
        .expect("a table written mid-session is adopted");
    assert_eq!(
        decider.evaluate("evil.example.com", "Bash"),
        Decision::Deny,
        "the deny direction must land mid-session too"
    );

    // Removing the table must not ungate a session that started gated.
    fs::write(&config_path, "# `[network]` removed\n").expect("rewrite config");
    let engine_config = EngineConfig {
        network_policy: Some(NetworkPolicyDecider::new(
            NetworkPolicy {
                default: DecisionToml::Deny,
                ..NetworkPolicy::default()
            },
            None,
        )),
        ..EngineConfig::default()
    };
    let (gated, _handle) = Engine::new(engine_config, &api_config);
    let context = gated.build_tool_context_for_turn(&authority, &route);
    let decider = context
        .network_policy
        .as_ref()
        .expect("a gated session stays gated");
    assert_eq!(
        decider.evaluate("unlisted.example.com", "Bash"),
        Decision::Deny,
        "removing the table must not hand a gated session an open policy"
    );
}

/// Both continuation dispatchers test `is_active()` before re-arming. A goal the
/// model handed back must therefore land as `Inactive` at the gate the loop
/// actually consults — the unit tests pin `GoalState`, not this decision.
#[test]
fn a_yielded_goal_is_not_rearmed_by_the_continuation_gate() {
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    {
        let mut state = engine.config.goal_state.lock().expect("goal lock");
        state.replace("ship the milestone", None, Some("goal-1".to_string()));
        state.mark_yielded().expect("yield an active goal");
    }

    assert!(
        matches!(
            engine.goal_continuation_if_active(),
            GoalContinuationAction::Inactive
        ),
        "a handed-back goal must not be re-armed by the cross-turn gate"
    );
}

/// The same gate, against an active goal, still dispatches — so the test above
/// is measuring the yield and not a gate that is dead in test conditions.
#[test]
fn an_active_goal_is_still_rearmed_by_the_continuation_gate() {
    let (engine, _handle) = Engine::new(EngineConfig::default(), &Config::default());
    {
        let mut state = engine.config.goal_state.lock().expect("goal lock");
        state.replace("ship the milestone", None, Some("goal-1".to_string()));
    }

    assert!(
        matches!(
            engine.goal_continuation_if_active(),
            GoalContinuationAction::Dispatch { .. }
        ),
        "an active goal still continues; otherwise the yield test proves nothing"
    );
}
